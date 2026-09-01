// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2025-2026 Z1xus
// Copyright (C) 2026 Alpha-Leader

use crate::{
    config::Config,
    hotkeys::{parse_binding, to_hotkey},
    profiles::{Profile, exe_key, find_profile},
    service::{self, Command, Shared},
    ui::{self, IconCacheInterface, ProfileAction},
    window_manager::{self, resolve_display},
};
use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    time::{Duration, Instant},
};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem},
};

/// How long an action's success/failure message stays on screen.
const STATUS_TTL: Duration = Duration::from_secs(4);

struct IconCache
{
    cache: HashMap<String, (egui::TextureHandle, Instant)>,
    max_size: usize,
    ttl: Duration,
}

impl IconCache
{
    fn new() -> Self
    {
        Self { cache: HashMap::new(), max_size: 100, ttl: Duration::from_secs(300) }
    }

    fn cleanup_expired(&mut self)
    {
        let now = Instant::now();
        self.cache.retain(|_, (_, last_used)| now.duration_since(*last_used) < self.ttl);
    }

    fn remove_oldest(&mut self)
    {
        if let Some(oldest_key) = self
            .cache
            .iter()
            .min_by_key(|(_, (_, last_used))| *last_used)
            .map(|(key, _)| key.clone())
        {
            self.cache.remove(&oldest_key);
        }
    }
}

impl IconCacheInterface for IconCache
{
    fn get(&mut self, key: &str) -> Option<&egui::TextureHandle>
    {
        let now = Instant::now();

        let fresh = match self.cache.get(key) {
            Some((_, last_used)) => now.duration_since(*last_used) < self.ttl,
            None => return None,
        };

        if !fresh {
            self.cache.remove(key);
            return None;
        }

        let (texture, last_used) = self.cache.get_mut(key)?;
        *last_used = now;
        Some(texture)
    }

    fn insert(&mut self, key: String, texture: egui::TextureHandle)
    {
        self.cleanup_expired();

        if self.cache.len() >= self.max_size {
            self.remove_oldest();
        }

        self.cache.insert(key, (texture, Instant::now()));
    }

    fn contains_key(&self, key: &str) -> bool
    {
        if let Some((_, last_used)) = self.cache.get(key) {
            last_used.elapsed() < self.ttl
        } else {
            false
        }
    }
}

pub struct BorderlessApp
{
    shared: Arc<Shared>,
    service_tx: Sender<Command>,
    /// The selected window is tracked by handle, never by list index: the list is
    /// rebuilt and re-sorted every few seconds, so an index can silently come to
    /// mean a different application.
    selected_hwnd: Option<isize>,
    icon_cache: IconCache,
    config: Config,
    selected_display: Option<usize>,
    displays: Vec<window_manager::DisplayInfo>,
    dark_titlebar_applied: bool,
    /// Set by the tray Quit item so the close-to-tray interception stands down.
    really_quit: Arc<AtomicBool>,
    /// Kept alive for the life of the app; dropping them unregisters everything.
    _hotkey_manager: Option<GlobalHotKeyManager>,
    _tray_icon: Option<TrayIcon>,
}

impl BorderlessApp
{
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self
    {
        ui::setup_dark_theme(&cc.egui_ctx);

        let shared = Arc::new(Shared::new());

        let displays = shared.manager.lock().map(|m| m.get_displays()).unwrap_or_default();

        if let Ok(mut manager) = shared.manager.lock() {
            manager.enable_frame_persistence();
        }

        let selected_display = if displays.is_empty() {
            None
        } else {
            // Prefer the saved device name: an index can silently come to mean
            // a different monitor after one is plugged in or removed.
            let by_device = if config.display_device.is_empty() {
                None
            } else {
                displays.iter().position(|d| d.device_name == config.display_device)
            };
            Some(by_device.unwrap_or(config.display_index.min(displays.len() - 1)))
        };

        if let Ok(mut state) = shared.state.lock() {
            state.profiles = config.profiles.clone();
            state.displays = displays.clone();
            state.ui_placement = config.placement;
            state.ui_display = selected_display.and_then(|index| displays.get(index)).cloned();
            // These must land before the watcher thread starts below: it runs
            // a startup safety check (recovering a taskbar left auto-hidden by
            // a crash) synchronously at thread start, using whatever is in
            // `state` at that instant — `sync_shared_state`'s per-frame push
            // happens on the main thread's first `update()`, too late to be
            // visible to that check.
            state.companions = config.companions.clone();
            state.dim_enabled = config.dim_enabled;
            state.dim_alpha_pct = config.dim_alpha_pct;
            state.taskbar_autohide = config.taskbar_autohide;
        }

        let (service_tx, _watcher) = service::spawn_watcher(shared.clone(), cc.egui_ctx.clone());

        let really_quit = Arc::new(AtomicBool::new(false));

        let (hotkey_manager, hotkey_warning) =
            setup_hotkeys(&config, &shared, &cc.egui_ctx, service_tx.clone());
        let tray_icon = setup_tray(&shared, &cc.egui_ctx, &really_quit);

        if let Some(warning) = hotkey_warning {
            shared.set_status(warning, true);
        }

        Self {
            shared,
            service_tx,
            selected_hwnd: None,
            icon_cache: IconCache::new(),
            config,
            selected_display,
            displays,
            dark_titlebar_applied: false,
            really_quit,
            _hotkey_manager: hotkey_manager,
            _tray_icon: tray_icon,
        }
    }

    fn handle_keyboard_input(&mut self, ctx: &egui::Context)
    {
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            let _ = self.service_tx.send(Command::Rescan);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selected_hwnd = None;
        }
    }

    fn handle_window_action(&mut self, hwnd: isize)
    {
        let display = self.selected_display.and_then(|index| self.displays.get(index)).cloned();

        let (title, restoring, result) = {
            let Ok(mut manager) = self.shared.manager.lock() else {
                return;
            };

            let title = manager
                .get_windows()
                .iter()
                .find(|w| w.hwnd == hwnd)
                .map(|w| w.title.clone())
                .unwrap_or_else(|| "selected window".to_string());

            let restoring = manager.has_saved_frame(hwnd);
            let result = manager.toggle_borderless(hwnd, &self.config.placement, display.as_ref());
            (title, restoring, result)
        };

        match result {
            Ok(()) => {
                let action = if restoring { "Restored borders on" } else { "Made borderless:" };
                self.shared.set_status(format!("{} {}", action, title), false);

                // The user acted on this window; auto-apply must not fight a
                // later manual restore of it.
                if let Ok(mut state) = self.shared.state.lock() {
                    state.auto_applied.insert(hwnd);
                }

                if restoring {
                    self.shared.unmark_managed(hwnd);
                } else {
                    self.shared.mark_managed(hwnd, self.config.placement, display, false);
                }

                self.save_config();
                let _ = self.service_tx.send(Command::Rescan);
            },
            Err(error) => {
                self.shared.set_status(format!("{}: {}", title, error), true);
            },
        }
    }

    fn handle_profile_action(&mut self, action: ProfileAction)
    {
        match action {
            ProfileAction::None => return,
            ProfileAction::Changed => {},
            ProfileAction::Save => {
                let Some(exe) = self.selected_exe() else {
                    return;
                };

                let display = self.selected_display.and_then(|index| self.displays.get(index));
                let key = exe_key(&exe);

                // Re-saving an existing profile must not silently clear its
                // cursor-lock flag or auto-apply choice.
                let existing = self.config.profiles.iter().find(|p| p.exe == key);
                let auto_apply = existing.map(|p| p.auto_apply).unwrap_or(true);
                let cursor_lock = existing.map(|p| p.cursor_lock).unwrap_or(false);

                let profile = Profile {
                    exe: key,
                    placement: self.config.placement,
                    display_device: display.map(|d| d.device_name.clone()).unwrap_or_default(),
                    display_index: self.selected_display.unwrap_or(0),
                    auto_apply,
                    cursor_lock,
                };

                match self.config.profiles.iter_mut().find(|p| p.exe == profile.exe) {
                    Some(existing) => *existing = profile,
                    None => self.config.profiles.push(profile),
                }
                self.shared.set_status(format!("Saved profile for {}", exe), false);
            },
            ProfileAction::Apply(index) => {
                self.apply_profile(index);
            },
            ProfileAction::Delete(index) => {
                if index < self.config.profiles.len() {
                    let removed = self.config.profiles.remove(index);
                    self.shared.set_status(format!("Deleted profile for {}", removed.exe), false);
                }
            },
        }

        self.save_config();
    }

    fn handle_companion_action(&mut self, action: ui::CompanionAction)
    {
        match action {
            ui::CompanionAction::None => return,
            ui::CompanionAction::Add => {
                let Some(exe) = self.selected_exe() else {
                    return;
                };
                let key = exe_key(&exe);
                if !self.config.companions.contains(&key) {
                    self.config.companions.push(key);
                    self.shared.set_status(format!("Added companion: {}", exe), false);
                }
            },
            ui::CompanionAction::Remove(index) => {
                if index < self.config.companions.len() {
                    let removed = self.config.companions.remove(index);
                    self.shared.set_status(format!("Removed companion: {}", removed), false);
                }
            },
        }

        self.save_config();
    }

    /// Applies a profile to the selected window by hand. An already-stripped
    /// window is re-placed rather than toggled back.
    fn apply_profile(&mut self, index: usize)
    {
        let (Some(profile), Some(hwnd)) =
            (self.config.profiles.get(index).cloned(), self.selected_hwnd)
        else {
            self.shared.set_status("Select a window to apply the profile to".to_string(), true);
            return;
        };

        let display =
            resolve_display(&self.displays, &profile.display_device, profile.display_index)
                .cloned();

        let result = {
            let Ok(mut manager) = self.shared.manager.lock() else {
                return;
            };

            if manager.has_saved_frame(hwnd) {
                manager.reapply_placement(hwnd, &profile.placement, display.as_ref())
            } else {
                manager.toggle_borderless(hwnd, &profile.placement, display.as_ref())
            }
        };

        match result {
            Ok(()) => {
                if let Ok(mut state) = self.shared.state.lock() {
                    state.auto_applied.insert(hwnd);
                }
                self.shared.mark_managed(hwnd, profile.placement, display, profile.cursor_lock);
                self.shared.set_status(format!("Applied profile for {}", profile.exe), false);
                let _ = self.service_tx.send(Command::Rescan);
            },
            Err(error) => self.shared.set_status(format!("{}: {}", profile.exe, error), true),
        }
    }

    fn selected_exe(&self) -> Option<String>
    {
        let hwnd = self.selected_hwnd?;
        let manager = self.shared.manager.lock().ok()?;
        manager.get_windows().iter().find(|w| w.hwnd == hwnd).map(|w| w.process_name.clone())
    }

    fn save_config(&mut self)
    {
        if let Some(index) = self.selected_display {
            self.config.display_index = index;
            self.config.display_device =
                self.displays.get(index).map(|d| d.device_name.clone()).unwrap_or_default();
        }
        self.config.save();
    }

    /// Pushes what the background half needs to know about the UI's current
    /// settings. Runs every frame; the data is small.
    fn sync_shared_state(&self)
    {
        if let Ok(mut state) = self.shared.state.lock() {
            if state.profiles != self.config.profiles {
                state.profiles = self.config.profiles.clone();
            }
            state.ui_placement = self.config.placement;
            state.ui_display =
                self.selected_display.and_then(|index| self.displays.get(index)).cloned();
            state.companions = self.config.companions.clone();
            state.dim_enabled = self.config.dim_enabled;
            state.dim_alpha_pct = self.config.dim_alpha_pct;
            state.taskbar_autohide = self.config.taskbar_autohide;
        }
    }

    fn current_status(&self) -> Option<(String, bool)>
    {
        let mut state = self.shared.state.lock().ok()?;

        let expired = match &state.status {
            Some((_, _, shown_at)) => shown_at.elapsed() >= STATUS_TTL,
            None => return None,
        };

        if expired {
            state.status = None;
            return None;
        }

        state.status.as_ref().map(|(message, is_error, _)| (message.clone(), *is_error))
    }
}

impl eframe::App for BorderlessApp
{
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame)
    {
        // Deferred to the first frame: the native window is guaranteed to exist
        // by now, which it is not during BorderlessApp::new.
        if !self.dark_titlebar_applied {
            self.dark_titlebar_applied = crate::window_manager::use_dark_titlebar_for_own_window();
        }

        // Without a tray icon there is no way back to a hidden window, so the
        // close button must actually close.
        if ctx.input(|i| i.viewport().close_requested())
            && self.config.close_to_tray
            && self._tray_icon.is_some()
            && !self.really_quit.load(Ordering::Relaxed)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            // Through Win32, not winit: nothing here depends on eframe still
            // producing frames afterwards.
            window_manager::set_own_window_visible(false);
        }

        self.handle_keyboard_input(ctx);

        self.icon_cache.cleanup_expired();

        // Read before the manager lock below: status lives under the state
        // lock, and the two are never held together.
        let status = self.current_status();

        let mut window_action = None;
        let mut profile_action = ProfileAction::None;
        let mut companion_action = ui::CompanionAction::None;
        let mut options_changed = false;

        {
            let Ok(manager) = self.shared.manager.lock() else {
                return;
            };
            let windows = manager.get_windows();

            if let Some(hwnd) = self.selected_hwnd
                && !windows.iter().any(|w| w.hwnd == hwnd)
            {
                self.selected_hwnd = None;
            }

            let can_restore = self.selected_hwnd.is_some_and(|hwnd| manager.has_saved_frame(hwnd));
            let selected_exe = self
                .selected_hwnd
                .and_then(|hwnd| windows.iter().find(|w| w.hwnd == hwnd))
                .map(|w| w.process_name.clone());

            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui::render_header(ui, windows.len());

                    ui::render_window_selector(
                        ui,
                        windows,
                        &mut self.selected_hwnd,
                        &mut self.icon_cache,
                    );

                    if self.displays.len() > 1 {
                        ui::render_display_selector(ui, &self.displays, &mut self.selected_display);
                    }

                    let display = self.selected_display.and_then(|index| self.displays.get(index));
                    ui::render_placement_controls(ui, &mut self.config.placement, display);

                    profile_action =
                        ui::render_profiles(ui, &mut self.config.profiles, selected_exe.as_deref());

                    companion_action =
                        ui::render_companions(ui, &self.config.companions, selected_exe.as_deref());

                    options_changed = ui::render_options(
                        ui,
                        ui::OptionsUi {
                            close_to_tray: &mut self.config.close_to_tray,
                            start_minimized: &mut self.config.start_minimized,
                            taskbar_autohide: &mut self.config.taskbar_autohide,
                            dim_enabled: &mut self.config.dim_enabled,
                            dim_alpha_pct: &mut self.config.dim_alpha_pct,
                            hotkey_toggle: &self.config.hotkey_toggle,
                            hotkey_restore_all: &self.config.hotkey_restore_all,
                            hotkey_cursor_lock: &self.config.hotkey_cursor_lock,
                        },
                    );

                    window_action =
                        ui::render_action_button(ui, windows, self.selected_hwnd, can_restore);

                    if let Some((message, is_error)) = &status {
                        ui::render_status(ui, message, *is_error);
                    }

                    ui::render_footer(ui);
                });
            });
        }

        if let Some(hwnd) = window_action {
            self.handle_window_action(hwnd);
        }

        if profile_action != ProfileAction::None {
            self.handle_profile_action(profile_action);
        }

        if companion_action != ui::CompanionAction::None {
            self.handle_companion_action(companion_action);
        }

        if options_changed {
            self.save_config();
        }

        self.sync_shared_state();

        // The watcher wakes the UI after every scan; this cadence only covers
        // the status line's expiry between scans.
        if status.is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        } else {
            ctx.request_repaint_after(service::SCAN_INTERVAL);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>)
    {
        self.save_config();
        let _ = self.service_tx.send(Command::Quit);
        // The watcher thread also runs this on its own shutdown path; calling
        // it here too covers a slow-to-notice Quit and is idempotent either way.
        service::end_session_effects(&self.shared);
    }
}

/// Registers the configured global hotkeys and installs their handler. Returns
/// the manager (which must stay alive) and a warning for anything that failed —
/// a conflicting registration must not stop the app.
fn setup_hotkeys(
    config: &Config,
    shared: &Arc<Shared>,
    ctx: &egui::Context,
    service_tx: Sender<Command>,
) -> (Option<GlobalHotKeyManager>, Option<String>)
{
    let Ok(manager) = GlobalHotKeyManager::new() else {
        return (None, Some("global hotkeys unavailable".to_string()));
    };

    let mut warnings = Vec::new();

    let mut register = |text: &str| -> Option<u32> {
        let Some(binding) = parse_binding(text) else {
            warnings.push(format!("invalid hotkey \"{}\"", text));
            return None;
        };
        let hotkey = to_hotkey(&binding)?;
        match manager.register(hotkey) {
            Ok(()) => Some(hotkey.id()),
            Err(_) => {
                warnings.push(format!("hotkey {} is taken by another app", binding));
                None
            },
        }
    };

    let toggle_id = register(&config.hotkey_toggle);
    let restore_id = register(&config.hotkey_restore_all);
    let cursor_lock_id = register(&config.hotkey_cursor_lock);

    let handler_shared = Arc::clone(shared);
    let handler_ctx = ctx.clone();

    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state() != HotKeyState::Pressed {
            return;
        }

        if Some(event.id()) == toggle_id {
            hotkey_toggle_foreground(&handler_shared, &handler_ctx);
        } else if Some(event.id()) == restore_id {
            restore_all(&handler_shared, &handler_ctx);
        } else if Some(event.id()) == cursor_lock_id {
            hotkey_toggle_cursor_lock(&handler_shared, &handler_ctx, &service_tx);
        }
    }));

    let warning = if warnings.is_empty() { None } else { Some(warnings.join("; ")) };
    (Some(manager), warning)
}

/// Toggles the cursor-lock override for the focused window: a session-only
/// choice that beats whatever its profile says, working on unprofiled windows
/// too. Applies or releases the clip immediately for a responsive feel, ahead
/// of the watcher's own heartbeat.
fn hotkey_toggle_cursor_lock(shared: &Shared, ctx: &egui::Context, service_tx: &Sender<Command>)
{
    let Some(hwnd) = window_manager::foreground_window() else {
        return;
    };

    let now_locked = shared.toggle_cursor_lock_override(hwnd);

    if now_locked {
        window_manager::clip_cursor_to_window(hwnd);
        if let Ok(mut state) = shared.state.lock() {
            state.clipped = Some(hwnd);
        }
        shared.set_status("Cursor locked to focused window".to_string(), false);
    } else {
        window_manager::release_cursor_clip();
        if let Ok(mut state) = shared.state.lock() {
            state.clipped = None;
        }
        shared.set_status("Cursor lock released".to_string(), false);
    }

    let _ = service_tx.send(Command::Poke);
    ctx.request_repaint();
}

/// Toggles borderless on the focused window: its profile's placement if it has
/// one, the UI's current placement otherwise.
fn hotkey_toggle_foreground(shared: &Shared, ctx: &egui::Context)
{
    let Some(hwnd) = window_manager::foreground_window() else {
        return;
    };

    let (title, process_name) = shared
        .manager
        .lock()
        .ok()
        .and_then(|manager| {
            manager
                .get_windows()
                .iter()
                .find(|w| w.hwnd == hwnd)
                .map(|w| (w.title.clone(), w.process_name.clone()))
        })
        .unwrap_or_else(|| ("focused window".to_string(), String::new()));

    let (placement, display, cursor_lock) = {
        let Ok(state) = shared.state.lock() else {
            return;
        };

        match find_profile(&state.profiles, &process_name) {
            Some(profile) => (
                profile.placement,
                resolve_display(&state.displays, &profile.display_device, profile.display_index)
                    .cloned(),
                profile.cursor_lock,
            ),
            None => (state.ui_placement, state.ui_display.clone(), false),
        }
    };

    let (restoring, result) = {
        let Ok(mut manager) = shared.manager.lock() else {
            return;
        };
        let restoring = manager.has_saved_frame(hwnd);
        (restoring, manager.toggle_borderless(hwnd, &placement, display.as_ref()))
    };

    if let Ok(mut state) = shared.state.lock() {
        state.auto_applied.insert(hwnd);
    }

    match result {
        Ok(()) => {
            let action = if restoring { "Restored borders on" } else { "Made borderless:" };
            shared.set_status(format!("{} {}", action, title), false);

            if restoring {
                shared.unmark_managed(hwnd);
            } else {
                shared.mark_managed(hwnd, placement, display, cursor_lock);
            }
        },
        Err(error) => shared.set_status(format!("{}: {}", title, error), true),
    }

    ctx.request_repaint();
}

fn restore_all(shared: &Shared, ctx: &egui::Context)
{
    let restored = match shared.manager.lock() {
        Ok(mut manager) => manager.restore_all(),
        Err(_) => return,
    };

    // Nothing is managed any more; the heartbeat's next tick releases the
    // cursor clip and undims companions on its own since it sees an empty
    // managed set, but clearing it here means that tick has nothing to fight.
    shared.clear_all_managed();

    let noun = if restored == 1 { "window" } else { "windows" };
    shared.set_status(format!("Restored {} {}", restored, noun), false);
    ctx.request_repaint();
}

/// Builds the tray icon and installs its handlers. Show/Hide and Quit act
/// through Win32 rather than winit viewport commands, because those commands
/// are only processed while eframe is producing frames — which it is not while
/// the window is hidden, the exact state the tray exists to serve.
fn setup_tray(
    shared: &Arc<Shared>,
    ctx: &egui::Context,
    really_quit: &Arc<AtomicBool>,
) -> Option<TrayIcon>
{
    let (rgba, width, height) = icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, width, height).ok()?;

    let menu = Menu::new();
    let show_item = MenuItem::new("Show / Hide", true, None);
    let restore_item = MenuItem::new("Restore all windows", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append_items(&[&show_item, &restore_item, &quit_item]).ok()?;

    let show_id = show_item.id().clone();
    let restore_id = restore_item.id().clone();
    let quit_id = quit_item.id().clone();

    let handler_shared = Arc::clone(shared);
    let handler_ctx = ctx.clone();
    let handler_quit = Arc::clone(really_quit);

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let id = event.id();

        if *id == show_id {
            let visible = window_manager::own_window_visible();
            window_manager::set_own_window_visible(!visible);
            if !visible {
                handler_ctx.request_repaint();
            }
        } else if *id == restore_id {
            restore_all(&handler_shared, &handler_ctx);
        } else if *id == quit_id {
            handler_quit.store(true, Ordering::Relaxed);
            // Shown first so eframe resumes frames and processes the close.
            window_manager::set_own_window_visible(true);
            handler_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            handler_ctx.request_repaint();
        }
    }));

    let click_ctx = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            window_manager::set_own_window_visible(true);
            click_ctx.request_repaint();
        }
    }));

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(env!("CARGO_PKG_NAME"))
        .with_icon(icon)
        .build()
        .ok()
}

pub fn create_app_options(config: &Config) -> eframe::NativeOptions
{
    let icon_data = load_icon();

    // Resizable since the profiles list can grow; content scrolls when the
    // window is smaller than what it holds.
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(env!("CARGO_PKG_NAME"))
            .with_inner_size([380.0, 560.0])
            .with_min_inner_size([380.0, 450.0])
            .with_maximize_button(false)
            .with_visible(!config.start_minimized)
            .with_icon(icon_data),
        ..Default::default()
    }
}

fn icon_rgba() -> (Vec<u8>, u32, u32)
{
    let icon_bytes = include_bytes!("../assets/icon.ico");

    let image = image::load_from_memory(icon_bytes).expect("Failed to load icon").into_rgba8();

    let (width, height) = image.dimensions();
    (image.into_raw(), width, height)
}

fn load_icon() -> egui::IconData
{
    let (rgba, width, height) = icon_rgba();
    egui::IconData { rgba, width, height }
}
