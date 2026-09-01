// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2025-2026 Z1xus
// Copyright (C) 2026 Alpha-Leader

use crate::{
    frames::{self, PersistedFrame},
    profiles::exe_key,
};
use std::collections::HashMap;
use windows::Win32::{
    Foundation::{COLORREF, CloseHandle, HWND, LPARAM, POINT, RECT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE, DwmSetWindowAttribute},
        Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC, GetDIBits,
            GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO, MONITORINFOEXW,
            ReleaseDC, SelectObject,
        },
    },
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    },
    UI::{
        Shell::{
            ABM_GETSTATE, ABM_SETSTATE, ABS_ALWAYSONTOP, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage,
        },
        WindowsAndMessaging::{
            ClipCursor, DrawIconEx, EnumWindows, GA_ROOT, GCLP_HICON, GWL_EXSTYLE, GWL_STYLE,
            GetAncestor, GetClassLongPtrW, GetClassNameW, GetCursorPos, GetForegroundWindow,
            GetWindowLongW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
            HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, ICON_SMALL, IsWindow, IsWindowVisible,
            LWA_ALPHA, MONITORINFOF_PRIMARY, SET_WINDOW_POS_FLAGS, SMTO_ABORTIFHUNG, SW_HIDE,
            SW_RESTORE, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
            SendMessageTimeoutW, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongW,
            SetWindowPos, ShowWindow, WM_GETICON, WS_BORDER, WS_CAPTION, WS_DLGFRAME,
            WS_EX_LAYERED, WS_EX_TOPMOST, WS_THICKFRAME, WindowFromPoint,
        },
    },
};

/// Bits cleared when making a window borderless.
const BORDER_STYLES: u32 = WS_BORDER.0 | WS_CAPTION.0 | WS_THICKFRAME.0 | WS_DLGFRAME.0;

/// How long to wait on a window's message loop when asking it for an icon.
/// Bounded so a hung application cannot stall the whole enumeration.
const ICON_QUERY_TIMEOUT_MS: u32 = 100;

const ICON_SIZE: i32 = 16;

#[derive(Debug, Clone)]
pub struct WindowInfo
{
    pub hwnd: isize,
    pub title: String,
    pub process_name: String,
    pub class_name: String,
    pub is_borderless: bool,
    pub icon_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct DisplayInfo
{
    pub name: String,
    /// The adapter device name (`\\.\DISPLAY1`), which survives monitors being
    /// added or removed, unlike a position in the enumeration order.
    pub device_name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub is_primary: bool,
}

/// Finds the display a saved setting referred to: by device name first, falling
/// back to the saved index (clamped), and finally to the primary display.
pub fn resolve_display<'a>(
    displays: &'a [DisplayInfo],
    device_name: &str,
    index_fallback: usize,
) -> Option<&'a DisplayInfo>
{
    if displays.is_empty() {
        return None;
    }

    if !device_name.is_empty()
        && let Some(display) = displays.iter().find(|d| d.device_name == device_name)
    {
        return Some(display);
    }

    displays
        .get(index_fallback.min(displays.len() - 1))
        .or_else(|| displays.iter().find(|d| d.is_primary))
}

/// A window's frame as it was before we stripped it, so it can be put back exactly.
#[derive(Debug, Clone, Copy)]
struct OriginalFrame
{
    style: u32,
    rect: RECT,
    was_topmost: bool,
}

/// An original frame plus the identity needed to persist it across a restart
/// of this app, when the window handle it was keyed by is meaningless.
#[derive(Debug, Clone)]
struct SavedFrame
{
    frame: OriginalFrame,
    /// Normalized exe stem; empty when the window was not in the last scan.
    exe: String,
    class: String,
    saved_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementMode
{
    /// Strip the borders but leave the window where it is.
    LeaveInPlace,
    /// Fill the whole monitor.
    FullDisplay,
    /// Fill a sub-rectangle of the monitor.
    Region,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor
{
    Centered,
    Left,
    Right,
    Custom,
}

/// Where a stripped window should be put. Deliberately free of Win32 calls so the
/// geometry can be unit-tested on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement
{
    pub mode: PlacementMode,
    pub width: i32,
    pub height: i32,
    pub anchor: Anchor,
    pub custom_x: i32,
    pub custom_y: i32,
    pub always_on_top: bool,
}

impl Default for Placement
{
    fn default() -> Self
    {
        Self {
            // Defaults to a centered 4K region: the common case is fitting a game
            // into part of an ultrawide, not stretching it across the whole panel.
            mode: PlacementMode::Region,
            width: 3840,
            height: 2160,
            anchor: Anchor::Centered,
            custom_x: 0,
            custom_y: 0,
            always_on_top: true,
        }
    }
}

impl Placement
{
    /// The target rectangle in desktop coordinates, or `None` to leave the window
    /// where it is. Offsets are relative to the display, so the result stays
    /// correct if the monitor is moved or its mode changes.
    pub fn resolve(&self, display: &DisplayInfo) -> Option<RECT>
    {
        let (x, y, width, height) = match self.mode {
            PlacementMode::LeaveInPlace => return None,
            PlacementMode::FullDisplay => (display.x, display.y, display.width, display.height),
            PlacementMode::Region => {
                let width = self.width.max(1);
                let height = self.height.max(1);

                let x = match self.anchor {
                    Anchor::Centered => display.x + (display.width - width) / 2,
                    Anchor::Left => display.x,
                    Anchor::Right => display.x + display.width - width,
                    Anchor::Custom => display.x + self.custom_x,
                };

                let y = match self.anchor {
                    Anchor::Custom => display.y + self.custom_y,
                    _ => display.y + (display.height - height) / 2,
                };

                (x, y, width, height)
            },
        };

        Some(RECT { left: x, top: y, right: x + width, bottom: y + height })
    }

    /// True when the resolved rectangle sticks out past the display, which is
    /// allowed but worth surfacing in the UI.
    pub fn overflows(&self, display: &DisplayInfo) -> bool
    {
        match self.resolve(display) {
            Some(rect) => {
                rect.left < display.x
                    || rect.top < display.y
                    || rect.right > display.x + display.width
                    || rect.bottom > display.y + display.height
            },
            None => false,
        }
    }
}

/// Payload threaded through `EnumWindows`. The process map is built once per
/// refresh rather than re-snapshotted for every window.
struct EnumContext
{
    windows: Vec<WindowInfo>,
    processes: HashMap<u32, String>,
}

impl WindowInfo
{
    pub fn display_text(&self) -> String
    {
        let max_title_len = 30;
        let max_process_len = 15;

        let truncated_title = if self.title.chars().count() > max_title_len {
            let truncated: String = self.title.chars().take(max_title_len - 3).collect();
            format!("{}...", truncated)
        } else {
            self.title.clone()
        };

        let truncated_process = if self.process_name.chars().count() > max_process_len {
            let truncated: String = self.process_name.chars().take(max_process_len - 3).collect();
            format!("{}...", truncated)
        } else {
            self.process_name.clone()
        };

        format!("{} ({})", truncated_title, truncated_process)
    }
}

impl DisplayInfo
{
    pub fn display_text(&self) -> String
    {
        let primary_indicator = if self.is_primary { " (Primary)" } else { "" };
        format!("{} - {}x{}{}", self.name, self.width, self.height, primary_indicator)
    }
}

pub struct WindowManager
{
    windows: Vec<WindowInfo>,
    original_frames: HashMap<isize, SavedFrame>,
    /// Frames loaded from disk that have not been matched to a live window
    /// yet. Adopted during scans; never used to invent a restore for a window
    /// that does not match.
    orphaned_frames: Vec<PersistedFrame>,
    /// Off in tests; on in the running app once persistence is enabled.
    persist: bool,
}

/// Enumerates the current top-level windows. Safe to call from any thread; the
/// watcher thread owns the cadence.
pub fn scan_windows() -> Vec<WindowInfo>
{
    let mut context = EnumContext { windows: Vec::new(), processes: build_process_map() };

    unsafe {
        if EnumWindows(Some(enum_windows_proc), LPARAM(&mut context as *mut EnumContext as isize))
            .is_ok()
        {
            context.windows.sort_by(|a: &WindowInfo, b: &WindowInfo| a.title.cmp(&b.title));
        }
    }

    context.windows
}

impl WindowManager
{
    pub fn new() -> Self
    {
        Self {
            windows: Vec::new(),
            original_frames: HashMap::new(),
            orphaned_frames: Vec::new(),
            persist: false,
        }
    }

    /// Turns on frame persistence and loads any frames a previous run left
    /// behind. Not part of `new` so tests never touch the real file.
    pub fn enable_frame_persistence(&mut self)
    {
        self.persist = true;
        self.orphaned_frames = frames::load();
    }

    pub fn get_windows(&self) -> &[WindowInfo]
    {
        &self.windows
    }

    pub fn set_windows(&mut self, windows: Vec<WindowInfo>)
    {
        self.windows = windows;

        // Drop saved frames for windows that no longer exist, so the map cannot
        // grow without bound over a long session. A window seen dying while we
        // run needs no crash recovery, so its persisted entry goes too.
        let before = self.original_frames.len();
        self.original_frames.retain(|hwnd, _| unsafe { IsWindow(Some(as_hwnd(*hwnd))).as_bool() });
        if self.original_frames.len() != before {
            self.persist_frames();
        }

        self.adopt_orphaned_frames();
    }

    /// Re-attaches frames persisted by a previous run to live windows that are
    /// currently borderless and match on executable and window class — the
    /// post-crash "Restore Borders still works" path.
    fn adopt_orphaned_frames(&mut self)
    {
        if self.orphaned_frames.is_empty() {
            return;
        }

        let candidates: Vec<(isize, String, String)> = self
            .windows
            .iter()
            .filter(|w| w.is_borderless && !self.original_frames.contains_key(&w.hwnd))
            .map(|w| (w.hwnd, exe_key(&w.process_name), w.class_name.clone()))
            .collect();

        let mut adopted = false;

        for (hwnd, exe, class) in candidates {
            let Some(position) = self
                .orphaned_frames
                .iter()
                .position(|orphan| orphan.exe == exe && orphan.class == class)
            else {
                continue;
            };

            let orphan = self.orphaned_frames.remove(position);
            self.original_frames.insert(
                hwnd,
                SavedFrame {
                    frame: OriginalFrame {
                        style: orphan.style,
                        rect: RECT {
                            left: orphan.rect.0,
                            top: orphan.rect.1,
                            right: orphan.rect.2,
                            bottom: orphan.rect.3,
                        },
                        was_topmost: orphan.topmost,
                    },
                    exe: orphan.exe,
                    class: orphan.class,
                    saved_at: orphan.saved_at,
                },
            );
            adopted = true;
        }

        if adopted {
            self.persist_frames();
        }
    }

    /// Writes the current saved frames (and still-unmatched orphans) to disk.
    /// Best effort and cheap; called whenever the frame map changes.
    fn persist_frames(&self)
    {
        if !self.persist {
            return;
        }

        let mut records: Vec<PersistedFrame> = self
            .original_frames
            .values()
            .filter(|saved| !saved.exe.is_empty() && !saved.class.is_empty())
            .map(|saved| PersistedFrame {
                exe: saved.exe.clone(),
                class: saved.class.clone(),
                style: saved.frame.style,
                rect: (
                    saved.frame.rect.left,
                    saved.frame.rect.top,
                    saved.frame.rect.right,
                    saved.frame.rect.bottom,
                ),
                topmost: saved.frame.was_topmost,
                saved_at: saved.saved_at,
            })
            .collect();

        records.extend(self.orphaned_frames.iter().cloned());

        // Newest first, so the size cap drops the oldest entries.
        records.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));

        frames::save(&records);
    }

    /// Whether this app stripped the given window and can put it back exactly.
    pub fn has_saved_frame(&self, hwnd: isize) -> bool
    {
        self.original_frames.contains_key(&hwnd)
    }

    pub fn get_displays(&self) -> Vec<DisplayInfo>
    {
        let mut displays = Vec::new();

        unsafe {
            let _ = EnumDisplayMonitors(
                Some(HDC::default()),
                None,
                Some(enum_monitors_proc),
                LPARAM(&mut displays as *mut Vec<DisplayInfo> as isize),
            );
        }

        displays.sort_by(|a: &DisplayInfo, b: &DisplayInfo| {
            if a.is_primary && !b.is_primary {
                std::cmp::Ordering::Less
            } else if !a.is_primary && b.is_primary {
                std::cmp::Ordering::Greater
            } else {
                a.name.cmp(&b.name)
            }
        });

        displays
    }

    /// Strips a window's borders and places it per `placement`, or restores the
    /// exact frame we saved when stripping it. Never invents a frame for a window
    /// it did not modify.
    pub fn toggle_borderless(
        &mut self,
        hwnd: isize,
        placement: &Placement,
        selected_display: Option<&DisplayInfo>,
    ) -> anyhow::Result<()>
    {
        let handle = as_hwnd(hwnd);

        unsafe {
            // Handles are recycled by Windows, so a stale entry from the last
            // snapshot could otherwise land on an unrelated window.
            if !IsWindow(Some(handle)).as_bool() {
                anyhow::bail!("window no longer exists");
            }

            let current_style = GetWindowLongW(handle, GWL_STYLE) as u32;
            if current_style == 0 {
                anyhow::bail!("could not read window style");
            }

            if let Some(saved) = self.original_frames.remove(&hwnd) {
                self.persist_frames();

                let original = saved.frame;
                SetWindowLongW(handle, GWL_STYLE, original.style as i32);

                // Put the window back in its original band. SWP_NOZORDER must be
                // absent for the insert-after handle to take effect.
                let restore_z = if original.was_topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };

                SetWindowPos(
                    handle,
                    Some(restore_z),
                    original.rect.left,
                    original.rect.top,
                    original.rect.right - original.rect.left,
                    original.rect.bottom - original.rect.top,
                    SWP_FRAMECHANGED,
                )?;

                return Ok(());
            }

            if (current_style & BORDER_STYLES) == 0 {
                anyhow::bail!("window has no borders to remove");
            }

            let mut rect = RECT::default();
            GetWindowRect(handle, &mut rect)?;

            let was_topmost = (GetWindowLongW(handle, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0) != 0;

            // Identity for persistence; empty when the window was not in the
            // last scan (persisting is then skipped for it).
            let (exe, class) = self
                .windows
                .iter()
                .find(|w| w.hwnd == hwnd)
                .map(|w| (exe_key(&w.process_name), w.class_name.clone()))
                .unwrap_or_default();

            self.original_frames.insert(
                hwnd,
                SavedFrame {
                    frame: OriginalFrame { style: current_style, rect, was_topmost },
                    exe,
                    class,
                    saved_at: frames::unix_now(),
                },
            );
            self.persist_frames();

            SetWindowLongW(handle, GWL_STYLE, (current_style & !BORDER_STYLES) as i32);

            // A window narrower than the monitor never triggers the shell's
            // fullscreen detection, so covering the taskbar requires topmost.
            let (insert_after, z_flag) = if placement.always_on_top {
                (HWND_TOPMOST, SET_WINDOW_POS_FLAGS(0))
            } else {
                (HWND_TOP, SWP_NOZORDER)
            };

            let target = selected_display.and_then(|display| placement.resolve(display));

            match target {
                Some(rect) => SetWindowPos(
                    handle,
                    Some(insert_after),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_FRAMECHANGED | z_flag,
                )?,
                // No display to resolve against, or LeaveInPlace: restyle only.
                None => SetWindowPos(
                    handle,
                    Some(insert_after),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | z_flag,
                )?,
            }
        }

        Ok(())
    }

    /// Re-asserts a placement on a window this app already stripped, without
    /// touching the saved frame. Some games re-take their own size shortly
    /// after launch; this puts them back where the profile says.
    pub fn reapply_placement(
        &self,
        hwnd: isize,
        placement: &Placement,
        selected_display: Option<&DisplayInfo>,
    ) -> anyhow::Result<()>
    {
        let handle = as_hwnd(hwnd);

        unsafe {
            if !IsWindow(Some(handle)).as_bool() {
                anyhow::bail!("window no longer exists");
            }

            // The game may have re-added its frame along with its size.
            let current_style = GetWindowLongW(handle, GWL_STYLE) as u32;
            if (current_style & BORDER_STYLES) != 0 {
                SetWindowLongW(handle, GWL_STYLE, (current_style & !BORDER_STYLES) as i32);
            }

            let (insert_after, z_flag) = if placement.always_on_top {
                (HWND_TOPMOST, SET_WINDOW_POS_FLAGS(0))
            } else {
                (HWND_TOP, SWP_NOZORDER)
            };

            match selected_display.and_then(|display| placement.resolve(display)) {
                Some(rect) => SetWindowPos(
                    handle,
                    Some(insert_after),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_FRAMECHANGED | z_flag,
                )?,
                None => SetWindowPos(
                    handle,
                    Some(insert_after),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | z_flag,
                )?,
            }
        }

        Ok(())
    }

    /// Restores every window this app stripped. Returns how many were restored.
    pub fn restore_all(&mut self) -> usize
    {
        let saved: Vec<isize> = self.original_frames.keys().copied().collect();
        let mut restored = 0;

        for hwnd in saved {
            // The restore path ignores placement and display entirely.
            if self.toggle_borderless(hwnd, &Placement::default(), None).is_ok() {
                restored += 1;
            }
        }

        restored
    }
}

/// The window that currently has focus, unless it belongs to this process —
/// a hotkey pressed while our own UI is focused must not strip our own window.
pub fn foreground_window() -> Option<isize>
{
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }

        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        if process_id == std::process::id() {
            return None;
        }

        Some(hwnd.0 as isize)
    }
}

fn as_hwnd(hwnd: isize) -> HWND
{
    HWND(hwnd as *mut std::ffi::c_void)
}

/// A window's current rectangle in desktop coordinates.
pub fn window_rect(hwnd: isize) -> Option<RECT>
{
    unsafe {
        let handle = as_hwnd(hwnd);
        if !IsWindow(Some(handle)).as_bool() {
            return None;
        }

        let mut rect = RECT::default();
        GetWindowRect(handle, &mut rect).ok()?;
        Some(rect)
    }
}

/// Confines the pointer to the window's current rectangle. Best effort; the
/// caller re-asserts on a heartbeat because Windows silently clears clips.
pub fn clip_cursor_to_window(hwnd: isize) -> bool
{
    let Some(rect) = window_rect(hwnd) else {
        return false;
    };

    unsafe { ClipCursor(Some(&rect)).is_ok() }
}

pub fn release_cursor_clip()
{
    unsafe {
        let _ = ClipCursor(None);
    }
}

pub fn is_topmost(hwnd: isize) -> bool
{
    unsafe { (GetWindowLongW(as_hwnd(hwnd), GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0) != 0 }
}

/// Dims a window by making it layered with the given alpha. Refuses windows
/// that are already layered — those manage their own transparency, and this
/// app must not clobber it. Returns whether the window is now dimmed *by us*.
pub fn dim_window(hwnd: isize, alpha: u8) -> bool
{
    unsafe {
        let handle = as_hwnd(hwnd);
        if !IsWindow(Some(handle)).as_bool() {
            return false;
        }

        let exstyle = GetWindowLongW(handle, GWL_EXSTYLE) as u32;
        if (exstyle & WS_EX_LAYERED.0) != 0 {
            return false;
        }

        SetWindowLongW(handle, GWL_EXSTYLE, (exstyle | WS_EX_LAYERED.0) as i32);

        if SetLayeredWindowAttributes(handle, COLORREF(0), alpha, LWA_ALPHA).is_err() {
            // Leave no half-applied state behind.
            SetWindowLongW(handle, GWL_EXSTYLE, exstyle as i32);
            return false;
        }

        true
    }
}

/// Undoes [`dim_window`]: full opacity, then the layered bit comes back off.
pub fn undim_window(hwnd: isize)
{
    unsafe {
        let handle = as_hwnd(hwnd);
        if !IsWindow(Some(handle)).as_bool() {
            return;
        }

        let _ = SetLayeredWindowAttributes(handle, COLORREF(0), 255, LWA_ALPHA);

        let exstyle = GetWindowLongW(handle, GWL_EXSTYLE) as u32;
        SetWindowLongW(handle, GWL_EXSTYLE, (exstyle & !WS_EX_LAYERED.0) as i32);
    }
}

/// Whether the taskbar is currently set to auto-hide.
pub fn taskbar_autohide_state() -> Option<bool>
{
    unsafe {
        let mut data =
            APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
        let state = SHAppBarMessage(ABM_GETSTATE, &mut data) as u32;
        Some((state & ABS_AUTOHIDE) != 0)
    }
}

pub fn set_taskbar_autohide(enabled: bool)
{
    unsafe {
        let mut data = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            lParam: LPARAM(if enabled { ABS_AUTOHIDE } else { ABS_ALWAYSONTOP } as isize),
            ..Default::default()
        };
        let _ = SHAppBarMessage(ABM_SETSTATE, &mut data);
    }
}

/// The top-level window under the mouse, for hover detection.
pub fn cursor_root_window() -> Option<isize>
{
    unsafe {
        let mut point = POINT::default();
        GetCursorPos(&mut point).ok()?;

        let hwnd = WindowFromPoint(point);
        if hwnd.is_invalid() {
            return None;
        }

        let root = GetAncestor(hwnd, GA_ROOT);
        if root.is_invalid() { Some(hwnd.0 as isize) } else { Some(root.0 as isize) }
    }
}

/// This app's own top-level window. Used for operations that must bypass
/// winit: showing and hiding from the tray has to work even when eframe is not
/// producing frames, which is exactly when the window is hidden.
///
/// Matched by process id and title, not title alone — `FindWindowW` by title
/// could hand back some other window that merely shares the name (say, an
/// Explorer folder called "ultraborderless"), and hiding that would be bad.
fn own_window_handle() -> Option<HWND>
{
    struct FindContext
    {
        pid: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn find_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL
    {
        unsafe {
            let context = &mut *(lparam.0 as *mut FindContext);

            let mut process_id = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
            if process_id != context.pid {
                return true.into();
            }

            let mut title_buffer = [0u16; 64];
            let title_len = GetWindowTextW(hwnd, &mut title_buffer);
            if title_len > 0
                && String::from_utf16_lossy(&title_buffer[..title_len as usize])
                    == env!("CARGO_PKG_NAME")
            {
                context.found = Some(hwnd);
                return false.into();
            }

            true.into()
        }
    }

    let mut context = FindContext { pid: std::process::id(), found: None };

    unsafe {
        // Returns Err when the callback stops the enumeration early — which is
        // the success case here.
        let _ = EnumWindows(Some(find_proc), LPARAM(&mut context as *mut FindContext as isize));
    }

    context.found
}

/// Shows or hides this app's own window directly through Win32.
pub fn set_own_window_visible(visible: bool)
{
    let Some(hwnd) = own_window_handle() else {
        return;
    };

    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_RESTORE } else { SW_HIDE });
        if visible {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

pub fn own_window_visible() -> bool
{
    match own_window_handle() {
        Some(hwnd) => unsafe { IsWindowVisible(hwnd).as_bool() },
        None => false,
    }
}

/// Switches this app's own title bar to the dark variant.
///
/// The title bar is drawn by Windows, not by egui, so it follows the *system*
/// theme regardless of the app's own styling. Without this a machine set to
/// light mode gets a white title bar above a dark window.
pub fn use_dark_titlebar_for_own_window() -> bool
{
    unsafe {
        let Some(hwnd) = own_window_handle() else {
            return false;
        };

        // The attribute takes a Win32 BOOL, which is a 32-bit int.
        let enabled: i32 = 1;
        let size = std::mem::size_of::<i32>() as u32;

        // Attribute 20 on Windows 10 2004+ and Windows 11; 19 on the older
        // Windows 10 builds that supported it first.
        let applied = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &enabled as *const i32 as *const std::ffi::c_void,
            size,
        )
        .is_ok();

        if applied {
            return true;
        }

        DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(19),
            &enabled as *const i32 as *const std::ffi::c_void,
            size,
        )
        .is_ok()
    }
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL
{
    unsafe {
        let context = &mut *(lparam.0 as *mut EnumContext);

        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }

        let mut title_buffer = [0u16; 256];
        let title_len = GetWindowTextW(hwnd, &mut title_buffer);
        if title_len <= 0 {
            return true.into();
        }

        let title = String::from_utf16_lossy(&title_buffer[..title_len as usize]);

        // Derived from Cargo.toml so a rename cannot leave the app listing itself.
        if title.trim().is_empty()
            || title.starts_with("Program Manager")
            || title == env!("CARGO_PKG_NAME")
        {
            return true.into();
        }

        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));

        let process_name =
            context.processes.get(&process_id).cloned().unwrap_or_else(|| "Unknown".to_string());

        if process_name.eq_ignore_ascii_case(env!("CARGO_PKG_NAME")) {
            return true.into();
        }

        let current_style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let is_borderless = (current_style & BORDER_STYLES) == 0;

        let mut class_buffer = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buffer);
        let class_name = if class_len > 0 {
            String::from_utf16_lossy(&class_buffer[..class_len as usize])
        } else {
            String::new()
        };

        let icon_data = extract_window_icon(hwnd);

        context.windows.push(WindowInfo {
            hwnd: hwnd.0 as isize,
            title,
            process_name,
            class_name,
            is_borderless,
            icon_data,
        });

        true.into()
    }
}

/// Snapshots every running process once, keyed by pid.
fn build_process_map() -> HashMap<u32, String>
{
    let mut processes = HashMap::new();

    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return processes;
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let raw = String::from_utf16_lossy(&entry.szExeFile);
                let name = raw.trim_end_matches('\0');
                let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
                processes.insert(entry.th32ProcessID, stem.to_string());

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }

    processes
}

unsafe extern "system" fn enum_monitors_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> windows::core::BOOL
{
    unsafe {
        let displays = &mut *(lparam.0 as *mut Vec<DisplayInfo>);

        // The EXW variant additionally carries the adapter device name, which is
        // the only stable identity a monitor has across replug/re-enumeration.
        let mut monitor_info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        if GetMonitorInfoW(hmonitor, &mut monitor_info.monitorInfo).as_bool() {
            let info = &monitor_info.monitorInfo;
            let width = info.rcMonitor.right - info.rcMonitor.left;
            let height = info.rcMonitor.bottom - info.rcMonitor.top;
            let is_primary = (info.dwFlags & MONITORINFOF_PRIMARY) != 0;

            let device_len = monitor_info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(monitor_info.szDevice.len());
            let device_name = String::from_utf16_lossy(&monitor_info.szDevice[..device_len]);

            let name = format!("Display {}", displays.len() + 1);

            displays.push(DisplayInfo {
                name,
                device_name,
                x: info.rcMonitor.left,
                y: info.rcMonitor.top,
                width,
                height,
                is_primary,
            });
        }

        true.into()
    }
}

struct GdiResources
{
    hdc_screen: HDC,
    hdc_mem: HDC,
    hbitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
}

impl GdiResources
{
    fn new(size: i32) -> Option<Self>
    {
        unsafe {
            let hdc_screen = GetDC(Some(HWND::default()));
            if hdc_screen.is_invalid() {
                return None;
            }

            let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
            if hdc_mem.is_invalid() {
                ReleaseDC(Some(HWND::default()), hdc_screen);
                return None;
            }

            let hbitmap = CreateCompatibleBitmap(hdc_screen, size, size);
            if hbitmap.is_invalid() {
                let _ = DeleteDC(hdc_mem);
                ReleaseDC(Some(HWND::default()), hdc_screen);
                return None;
            }

            let old_bitmap = SelectObject(hdc_mem, hbitmap.into());

            Some(Self { hdc_screen, hdc_mem, hbitmap, old_bitmap })
        }
    }

    fn draw_icon(
        &self,
        icon_handle: windows::Win32::UI::WindowsAndMessaging::HICON,
        size: i32,
    ) -> windows::core::Result<()>
    {
        unsafe {
            DrawIconEx(
                self.hdc_mem,
                0,
                0,
                icon_handle,
                size,
                size,
                0,
                Some(windows::Win32::Graphics::Gdi::HBRUSH::default()),
                windows::Win32::UI::WindowsAndMessaging::DI_NORMAL,
            )
        }
    }

    fn get_bitmap_data(&self, size: i32) -> Option<Vec<u8>>
    {
        unsafe {
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size,
                    biHeight: -size,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default(); 1],
            };

            let mut rgba_data = vec![0u8; (size * size * 4) as usize];
            let result = GetDIBits(
                self.hdc_mem,
                self.hbitmap,
                0,
                size as u32,
                Some(rgba_data.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            if result == 0 {
                return None;
            }

            for chunk in rgba_data.chunks_exact_mut(4) {
                chunk.swap(0, 2);
            }

            Some(rgba_data)
        }
    }
}

impl Drop for GdiResources
{
    fn drop(&mut self)
    {
        unsafe {
            SelectObject(self.hdc_mem, self.old_bitmap);
            let _ = DeleteObject(self.hbitmap.into());
            let _ = DeleteDC(self.hdc_mem);
            ReleaseDC(Some(HWND::default()), self.hdc_screen);
        }
    }
}

fn extract_window_icon(hwnd: HWND) -> Option<Vec<u8>>
{
    unsafe {
        // WM_GETICON is handled by the target window's message loop, so this must
        // be time-bounded: a plain SendMessageW to a hung app never returns and
        // would strand the refresh thread permanently.
        let mut icon_result = 0usize;
        let sent = SendMessageTimeoutW(
            hwnd,
            WM_GETICON,
            WPARAM(ICON_SMALL as usize),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            ICON_QUERY_TIMEOUT_MS,
            Some(&mut icon_result),
        );

        let icon_handle = if sent.0 != 0 && icon_result != 0 {
            windows::Win32::UI::WindowsAndMessaging::HICON(icon_result as *mut std::ffi::c_void)
        } else {
            let class_icon = GetClassLongPtrW(hwnd, GCLP_HICON);
            if class_icon != 0 {
                windows::Win32::UI::WindowsAndMessaging::HICON(class_icon as *mut std::ffi::c_void)
            } else {
                return None;
            }
        };

        let gdi_resources = GdiResources::new(ICON_SIZE)?;

        if gdi_resources.draw_icon(icon_handle, ICON_SIZE).is_err() {
            return None;
        }

        gdi_resources.get_bitmap_data(ICON_SIZE)
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        },
        core::w,
    };

    /// The 7680x2160 ultrawide this feature was built for.
    fn ultrawide() -> DisplayInfo
    {
        DisplayInfo {
            name: "Display 1".to_string(),
            device_name: r"\\.\DISPLAY1".to_string(),
            x: 0,
            y: 0,
            width: 7680,
            height: 2160,
            is_primary: true,
        }
    }

    fn region(width: i32, height: i32, anchor: Anchor) -> Placement
    {
        Placement { mode: PlacementMode::Region, width, height, anchor, ..Placement::default() }
    }

    /// The headline case: a 4K game centered on the ultrawide, leaving 1920px
    /// of desktop free on each side.
    #[test]
    fn centers_a_4k_region_on_the_ultrawide()
    {
        let rect = region(3840, 2160, Anchor::Centered).resolve(&ultrawide()).unwrap();

        assert_eq!((rect.left, rect.top), (1920, 0));
        assert_eq!((rect.right - rect.left, rect.bottom - rect.top), (3840, 2160));
        assert_eq!(rect.right, 5760);
    }

    #[test]
    fn anchors_pin_to_the_correct_edge()
    {
        let display = ultrawide();

        assert_eq!(region(3840, 2160, Anchor::Left).resolve(&display).unwrap().left, 0);
        assert_eq!(region(3840, 2160, Anchor::Right).resolve(&display).unwrap().left, 3840);

        let custom =
            Placement { custom_x: 100, custom_y: 50, ..region(1920, 1080, Anchor::Custom) };
        let rect = custom.resolve(&display).unwrap();
        assert_eq!((rect.left, rect.top), (100, 50));
    }

    /// A shorter region centers vertically rather than pinning to the top.
    #[test]
    fn a_shorter_region_is_centered_vertically()
    {
        let rect = region(3840, 1080, Anchor::Centered).resolve(&ultrawide()).unwrap();
        assert_eq!(rect.top, 540);
        assert_eq!(rect.bottom, 1620);
    }

    #[test]
    fn full_display_and_leave_in_place_are_unchanged()
    {
        let display = ultrawide();

        let full = Placement { mode: PlacementMode::FullDisplay, ..Placement::default() };
        let rect = full.resolve(&display).unwrap();
        assert_eq!((rect.left, rect.top, rect.right, rect.bottom), (0, 0, 7680, 2160));
        assert!(!full.overflows(&display));

        let leave = Placement { mode: PlacementMode::LeaveInPlace, ..Placement::default() };
        assert!(leave.resolve(&display).is_none());
    }

    #[test]
    fn oversized_regions_are_reported_as_overflowing()
    {
        let display = ultrawide();

        assert!(!region(3840, 2160, Anchor::Centered).overflows(&display));
        assert!(region(3840, 4320, Anchor::Centered).overflows(&display));
        assert!(region(8000, 2160, Anchor::Centered).overflows(&display));
    }

    /// Offsets are display-relative, so a monitor that is not at the desktop
    /// origin still gets the region placed inside it.
    #[test]
    fn regions_follow_a_display_that_is_not_at_the_origin()
    {
        let secondary = DisplayInfo { x: -1920, y: 200, width: 1920, height: 1080, ..ultrawide() };

        let rect = region(1280, 720, Anchor::Centered).resolve(&secondary).unwrap();
        assert_eq!((rect.left, rect.top), (-1600, 380));
    }

    /// The bug this guards: restoring used to OR in `WS_CAPTION | WS_THICKFRAME`
    /// rather than replaying the window's real style, so frames came back wrong.
    #[test]
    fn restores_the_original_style_exactly()
    {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("ultraborderless-test"),
                // Visible, like every window the app actually enumerates: a
                // never-shown window does not pick up WS_EX_TOPMOST.
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                400,
                300,
                None,
                None,
                None,
                None,
            )
            .expect("create test window");

            let handle = hwnd.0 as isize;
            let original = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            let original_ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
            assert_ne!(original & BORDER_STYLES, 0, "fixture should start with borders");
            assert_eq!(original_ex & WS_EX_TOPMOST.0, 0, "fixture should not start topmost");

            let mut manager = WindowManager::new();
            let placement = Placement {
                mode: PlacementMode::LeaveInPlace,
                always_on_top: true,
                ..Placement::default()
            };

            manager.toggle_borderless(handle, &placement, None).expect("strip borders");
            assert_eq!(
                GetWindowLongW(hwnd, GWL_STYLE) as u32 & BORDER_STYLES,
                0,
                "border bits should be cleared"
            );
            assert_ne!(
                GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                0,
                "always_on_top should have made the window topmost"
            );
            assert!(manager.has_saved_frame(handle));

            manager.toggle_borderless(handle, &placement, None).expect("restore borders");
            assert_eq!(
                GetWindowLongW(hwnd, GWL_STYLE) as u32,
                original,
                "style must round-trip exactly"
            );
            assert_eq!(
                GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0,
                0,
                "topmost must be dropped again on restore"
            );
            assert!(!manager.has_saved_frame(handle));

            let _ = DestroyWindow(hwnd);
        }
    }

    /// Dimming must round-trip cleanly and never touch a window that manages
    /// its own transparency.
    #[test]
    fn dim_round_trips_and_refuses_already_layered_windows()
    {
        use windows::Win32::UI::WindowsAndMessaging::GetLayeredWindowAttributes;

        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("ultraborderless-dim-test"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                200,
                150,
                None,
                None,
                None,
                None,
            )
            .expect("create test window");

            let handle = hwnd.0 as isize;

            assert!(dim_window(handle, 102), "dim should succeed on a plain window");
            assert_ne!(
                GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0,
                0,
                "dim should set the layered bit"
            );

            let mut alpha = 0u8;
            GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), None)
                .expect("read alpha back");
            assert_eq!(alpha, 102);

            // Already layered (by us, but indistinguishable from an app's own
            // layering) — a second dim must refuse.
            assert!(!dim_window(handle, 50));

            undim_window(handle);
            assert_eq!(
                GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0,
                0,
                "undim should clear the layered bit"
            );

            let _ = DestroyWindow(hwnd);
        }
    }

    /// Handles are recycled, so acting on a dead one could hit an unrelated window.
    #[test]
    fn rejects_a_handle_that_is_not_a_window()
    {
        let mut manager = WindowManager::new();
        let placement = Placement::default();
        assert!(manager.toggle_borderless(0, &placement, None).is_err());
        assert!(manager.toggle_borderless(0xDEAD_BEEF, &placement, None).is_err());
    }

    /// Device name wins, a stale index clamps, and an empty list yields nothing.
    #[test]
    fn resolve_display_prefers_device_name_over_index()
    {
        let displays = vec![
            ultrawide(),
            DisplayInfo {
                name: "Display 2".to_string(),
                device_name: r"\\.\DISPLAY2".to_string(),
                is_primary: false,
                ..ultrawide()
            },
        ];

        let by_device = resolve_display(&displays, r"\\.\DISPLAY2", 0).unwrap();
        assert_eq!(by_device.device_name, r"\\.\DISPLAY2");

        // Unknown device falls back to the index.
        let by_index = resolve_display(&displays, r"\\.\DISPLAY9", 1).unwrap();
        assert_eq!(by_index.device_name, r"\\.\DISPLAY2");

        // A stale index clamps instead of failing.
        let clamped = resolve_display(&displays, "", 7).unwrap();
        assert_eq!(clamped.device_name, r"\\.\DISPLAY2");

        assert!(resolve_display(&[], r"\\.\DISPLAY1", 0).is_none());
    }
}
