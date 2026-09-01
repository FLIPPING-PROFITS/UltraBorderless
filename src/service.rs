// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Alpha-Leader

//! The background half of the app: a watcher thread that scans windows,
//! auto-applies profiles, and runs the gaming-session heartbeat (cursor lock,
//! focus/topmost watchdog, taskbar auto-hide, companion dimming).
//!
//! This deliberately does not run inside the egui frame loop. eframe stops
//! producing frames while its window is hidden, and the whole point of
//! auto-apply and the tray is to keep working with the UI closed. Anything
//! that must stay alive in the tray lives here or in a Win32 message handler,
//! never in `update()`.

use crate::{
    profiles::{Profile, exe_key, find_profile},
    session::{self, ManagedWindow, PlanInputs},
    window_manager::{self, DisplayInfo, Placement, WindowManager, resolve_display, scan_windows},
};
use eframe::egui;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    time::{Duration, Instant},
};

/// How often the watcher re-enumerates windows.
pub const SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// How long after an auto-apply the placement is asserted once more. Some games
/// re-take their own size shortly after their window first appears.
const REAPPLY_DELAY: Duration = Duration::from_secs(2);

/// The session tick while any managed window, override, or effect is live:
/// re-asserts cursor clips (Windows silently clears them), tracks focus for
/// dimming, and checks hover. Idle (folded into the 5s scan) otherwise.
const HEARTBEAT: Duration = Duration::from_millis(250);

/// Minimum spacing between watchdog re-assertions of one window, so a
/// deliberate user drag or resize is never fought tick by tick.
const REASSERT_DEBOUNCE: Duration = Duration::from_secs(2);

/// State shared between the UI thread, the watcher thread, and the hotkey and
/// tray handlers. Lock discipline: never hold `manager` and `state` at once —
/// take what you need from one, release it, then lock the other.
pub struct Shared
{
    pub manager: Mutex<WindowManager>,
    pub state: Mutex<State>,
}

pub struct State
{
    pub profiles: Vec<Profile>,
    pub displays: Vec<DisplayInfo>,
    /// The placement currently shown in the UI; what a hotkey falls back to
    /// when the focused window has no profile.
    pub ui_placement: Placement,
    pub ui_display: Option<DisplayInfo>,
    /// Message for the status line, shared so background actions can report.
    pub status: Option<(String, bool, Instant)>,
    /// Windows a profile has already been applied to (or that the user acted
    /// on directly), so auto-apply fires once per window: a manual restore
    /// must not be fought, and a failing window must not be retried every
    /// scan. A recreated window gets a new handle and becomes eligible again.
    pub auto_applied: HashSet<isize>,

    /// Windows this app has applied a placement to (auto-apply, manual apply,
    /// or hotkey toggle) and the placement it applied — the watchdog re-asserts
    /// this when it decays, and its keys are "what counts as a managed game"
    /// for dimming and taskbar auto-hide.
    pub managed: HashMap<isize, ManagedWindow>,
    /// Session overrides from the cursor-lock hotkey: hwnd -> forced on/off,
    /// beating whatever a profile says. Works on unprofiled windows too.
    pub lock_overrides: HashMap<isize, bool>,
    /// The window currently holding the cursor clip, if any.
    pub clipped: Option<isize>,
    /// Windows this app has dimmed (made layered). Never includes a window
    /// that came in already layered — that one is left alone.
    pub dimmed: HashSet<isize>,
    /// Taskbar auto-hide state before this app changed it, so it can be put
    /// back exactly. `None` means this app has not touched it.
    pub taskbar_prev: Option<bool>,

    // Mirrors of config the UI pushes in every frame via `sync_shared_state`.
    pub companions: Vec<String>,
    pub dim_enabled: bool,
    pub dim_alpha_pct: u8,
    pub taskbar_autohide: bool,
}

impl Shared
{
    pub fn new() -> Self
    {
        Self {
            manager: Mutex::new(WindowManager::new()),
            state: Mutex::new(State {
                profiles: Vec::new(),
                displays: Vec::new(),
                ui_placement: Placement::default(),
                ui_display: None,
                status: None,
                auto_applied: HashSet::new(),
                managed: HashMap::new(),
                lock_overrides: HashMap::new(),
                clipped: None,
                dimmed: HashSet::new(),
                taskbar_prev: None,
                companions: Vec::new(),
                dim_enabled: false,
                dim_alpha_pct: 40,
                taskbar_autohide: false,
            }),
        }
    }

    pub fn set_status(&self, message: String, is_error: bool)
    {
        if let Ok(mut state) = self.state.lock() {
            state.status = Some((message, is_error, Instant::now()));
        }
    }

    /// Marks a window as one this app placed, for the watchdog and for
    /// dimming/taskbar purposes. Call after every successful strip/apply.
    pub fn mark_managed(
        &self,
        hwnd: isize,
        placement: Placement,
        display: Option<DisplayInfo>,
        cursor_lock: bool,
    )
    {
        if let Ok(mut state) = self.state.lock() {
            state.managed.insert(
                hwnd,
                ManagedWindow { placement, display, cursor_lock, last_reassert: None },
            );
        }
    }

    /// Un-marks a window (on manual restore), so the watchdog, dimming, and
    /// taskbar auto-hide stop treating it as a managed game.
    pub fn unmark_managed(&self, hwnd: isize)
    {
        if let Ok(mut state) = self.state.lock() {
            state.managed.remove(&hwnd);
        }
    }

    pub fn clear_all_managed(&self)
    {
        if let Ok(mut state) = self.state.lock() {
            state.managed.clear();
        }
    }

    pub fn toggle_cursor_lock_override(&self, hwnd: isize) -> bool
    {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };

        let profile_flag = state.managed.get(&hwnd).map(|m| m.cursor_lock);
        let current =
            session::effective_cursor_lock(state.lock_overrides.get(&hwnd).copied(), profile_flag);
        let new_value = !current;
        state.lock_overrides.insert(hwnd, new_value);
        new_value
    }
}

pub enum Command
{
    /// Scan now instead of waiting out the interval (the F5 path).
    Rescan,
    /// Wake the heartbeat immediately without forcing a full scan — used after
    /// a hotkey changes session state so the effect lands without delay.
    Poke,
    Quit,
}

/// An auto-applied placement waiting to be asserted a second time.
struct PendingReapply
{
    hwnd: isize,
    due: Instant,
    placement: Placement,
    display: Option<DisplayInfo>,
}

pub fn spawn_watcher(
    shared: Arc<Shared>,
    ctx: egui::Context,
) -> (Sender<Command>, std::thread::JoinHandle<()>)
{
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || watcher_loop(&shared, &ctx, &receiver));
    (sender, handle)
}

fn watcher_loop(shared: &Shared, ctx: &egui::Context, receiver: &Receiver<Command>)
{
    let mut pending_reapply: Vec<PendingReapply> = Vec::new();
    let mut next_scan = Instant::now();

    // Recovers from a hard crash (release builds are `panic = "abort"`): if a
    // previous run left the taskbar auto-hidden with nothing left to manage,
    // put it back before anything else runs.
    if session_would_autohide_only_because_of_a_stale_state(shared) {
        window_manager::set_taskbar_autohide(false);
    }

    loop {
        let now = Instant::now();
        let session_active = has_active_session(shared);

        let mut next_due = pending_reapply
            .iter()
            .map(|p| p.due)
            .chain(std::iter::once(next_scan))
            .min()
            .unwrap_or(next_scan);
        if session_active {
            next_due = next_due.min(now + HEARTBEAT);
        }

        match receiver.recv_timeout(next_due.saturating_duration_since(now)) {
            Ok(Command::Rescan) => next_scan = Instant::now(),
            Ok(Command::Poke) => {},
            Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => {
                end_session_effects(shared);
                return;
            },
            Err(RecvTimeoutError::Timeout) => {},
        }

        let now = Instant::now();

        fire_due_reapplies(shared, &mut pending_reapply, now);

        if now >= next_scan {
            next_scan = now + SCAN_INTERVAL;

            let windows = scan_windows();
            if let Ok(mut manager) = shared.manager.lock() {
                manager.set_windows(windows);
            }

            prune_dead_session_state(shared);
            sync_managed_from_saved_frames(shared);
            auto_apply_pass(shared, &mut pending_reapply);
            run_watchdog(shared, now);

            // Harmless while the window is hidden; refreshes the list when not.
            ctx.request_repaint();
        }

        if session_active || has_active_session(shared) {
            heartbeat_pass(shared);
        }
    }
}

fn session_would_autohide_only_because_of_a_stale_state(shared: &Shared) -> bool
{
    let Ok(state) = shared.state.lock() else {
        return false;
    };
    state.taskbar_autohide
        && state.managed.is_empty()
        && window_manager::taskbar_autohide_state() == Some(true)
}

fn has_active_session(shared: &Shared) -> bool
{
    let Ok(state) = shared.state.lock() else {
        return false;
    };
    !state.managed.is_empty()
        || !state.lock_overrides.is_empty()
        || state.clipped.is_some()
        || !state.dimmed.is_empty()
        || state.taskbar_prev.is_some()
}

/// Registers windows this app already owns (a saved frame exists) but that
/// `managed` does not yet know about — the gap left by a window adopted from
/// `frames.txt` after a restart, or one stripped by an app instance before
/// this one's session-tracking existed. Without this, cursor lock, the
/// watchdog, dimming, and taskbar auto-hide would silently skip a game that
/// was already borderless when the app (re)started managing it.
fn sync_managed_from_saved_frames(shared: &Shared)
{
    let candidates: Vec<(isize, String)> = {
        let Ok(manager) = shared.manager.lock() else {
            return;
        };
        manager
            .get_windows()
            .iter()
            .filter(|w| w.is_borderless && manager.has_saved_frame(w.hwnd))
            .map(|w| (w.hwnd, w.process_name.clone()))
            .collect()
    };

    let Ok(mut state) = shared.state.lock() else {
        return;
    };

    for (hwnd, process_name) in candidates {
        if state.managed.contains_key(&hwnd) {
            continue;
        }

        let (placement, display, cursor_lock) = match find_profile(&state.profiles, &process_name) {
            Some(profile) => (
                profile.placement,
                resolve_display(&state.displays, &profile.display_device, profile.display_index)
                    .cloned(),
                profile.cursor_lock,
            ),
            None => (state.ui_placement, state.ui_display.clone(), false),
        };

        state
            .managed
            .insert(hwnd, ManagedWindow { placement, display, cursor_lock, last_reassert: None });
    }
}

/// Drops session bookkeeping for windows that no longer exist, releasing any
/// live effect that was pinned to them.
fn prune_dead_session_state(shared: &Shared)
{
    let live: HashSet<isize> = {
        let Ok(manager) = shared.manager.lock() else {
            return;
        };
        manager.get_windows().iter().map(|w| w.hwnd).collect()
    };

    let Ok(mut state) = shared.state.lock() else {
        return;
    };

    state.managed.retain(|hwnd, _| live.contains(hwnd));
    state.lock_overrides.retain(|hwnd, _| live.contains(hwnd));

    if let Some(clipped) = state.clipped
        && !live.contains(&clipped)
    {
        window_manager::release_cursor_clip();
        state.clipped = None;
    }

    // A dimmed window that died needs no undim call — it is gone — just drop
    // the bookkeeping so it is not mistaken for a live dim later.
    state.dimmed.retain(|hwnd| live.contains(hwnd));
}

/// Ends every live session effect: releases the cursor clip, restores every
/// window this app dimmed, and puts the taskbar back. Idempotent and
/// poison-tolerant so it is safe to call from both the watcher's shutdown
/// paths and `on_exit`.
pub fn end_session_effects(shared: &Shared)
{
    window_manager::release_cursor_clip();

    let (dimmed, taskbar_prev) = {
        let state = match shared.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        (state.dimmed.clone(), state.taskbar_prev)
    };

    for hwnd in dimmed {
        window_manager::undim_window(hwnd);
    }

    if let Some(previous) = taskbar_prev {
        window_manager::set_taskbar_autohide(previous);
    }

    if let Ok(mut state) = shared.state.lock() {
        state.clipped = None;
        state.dimmed.clear();
        state.taskbar_prev = None;
    }
}

fn fire_due_reapplies(shared: &Shared, pending: &mut Vec<PendingReapply>, now: Instant)
{
    let mut kept = Vec::new();

    for entry in pending.drain(..) {
        if entry.due > now {
            kept.push(entry);
            continue;
        }

        if let Ok(manager) = shared.manager.lock() {
            // A manual restore in the meantime wins: never re-strip a window
            // the user just put back.
            if manager.has_saved_frame(entry.hwnd) {
                let _ =
                    manager.reapply_placement(entry.hwnd, &entry.placement, entry.display.as_ref());
            }
        }
    }

    *pending = kept;
}

fn auto_apply_pass(shared: &Shared, pending_reapply: &mut Vec<PendingReapply>)
{
    // Snapshot the window list, then decide under the state lock, then act
    // under the manager lock — never both locks at once.
    let windows: Vec<(isize, String, String, bool)> = {
        let Ok(manager) = shared.manager.lock() else {
            return;
        };
        manager
            .get_windows()
            .iter()
            .map(|w| (w.hwnd, w.title.clone(), w.process_name.clone(), w.is_borderless))
            .collect()
    };

    let candidates: Vec<(isize, String, Profile, Option<DisplayInfo>)> = {
        let Ok(mut state) = shared.state.lock() else {
            return;
        };

        let live: HashSet<isize> = windows.iter().map(|(hwnd, ..)| *hwnd).collect();
        state.auto_applied.retain(|hwnd| live.contains(hwnd));
        pending_reapply.retain(|pending| live.contains(&pending.hwnd));

        let mut candidates = Vec::new();
        for (hwnd, title, process_name, is_borderless) in &windows {
            if *is_borderless || state.auto_applied.contains(hwnd) {
                continue;
            }

            let Some(profile) =
                find_profile(&state.profiles, process_name).filter(|p| p.auto_apply).cloned()
            else {
                continue;
            };

            // Marked before the attempt: a window that fails (say, elevated)
            // must not be retried every scan.
            state.auto_applied.insert(*hwnd);

            let display =
                resolve_display(&state.displays, &profile.display_device, profile.display_index)
                    .cloned();

            candidates.push((*hwnd, title.clone(), profile, display));
        }
        candidates
    };

    for (hwnd, title, profile, display) in candidates {
        let result = {
            let Ok(mut manager) = shared.manager.lock() else {
                return;
            };

            if manager.has_saved_frame(hwnd) {
                // Already stripped by hand; leave it alone.
                continue;
            }

            manager.toggle_borderless(hwnd, &profile.placement, display.as_ref())
        };

        match result {
            Ok(()) => {
                shared.set_status(format!("Auto-applied profile: {}", title), false);
                shared.mark_managed(hwnd, profile.placement, display.clone(), profile.cursor_lock);
                pending_reapply.push(PendingReapply {
                    hwnd,
                    due: Instant::now() + REAPPLY_DELAY,
                    placement: profile.placement,
                    display,
                });
            },
            Err(error) => {
                shared.set_status(format!("Auto-apply {}: {}", title, error), true);
            },
        }
    }
}

/// Re-asserts topmost and placement for managed windows whose state has
/// decayed. Runs every scan (cheap: one rect/style read per managed window)
/// and, for the focused window only, every heartbeat tick for a snappier feel.
fn run_watchdog(shared: &Shared, now: Instant)
{
    let targets: Vec<(isize, Placement, Option<DisplayInfo>, bool)> = {
        let Ok(mut state) = shared.state.lock() else {
            return;
        };

        state
            .managed
            .iter_mut()
            .filter_map(|(&hwnd, managed)| {
                let due = managed
                    .last_reassert
                    .is_none_or(|t| now.duration_since(t) >= REASSERT_DEBOUNCE);
                due.then(|| {
                    (
                        hwnd,
                        managed.placement,
                        managed.display.clone(),
                        managed.placement.always_on_top,
                    )
                })
            })
            .collect()
    };

    let mut reasserted = Vec::new();

    for (hwnd, placement, display, always_on_top) in targets {
        let topmost_decayed = always_on_top && !window_manager::is_topmost(hwnd);

        let rect_decayed = display
            .as_ref()
            .and_then(|d| placement.resolve(d))
            .zip(window_manager::window_rect(hwnd))
            .is_some_and(|(expected, actual)| !session::rect_matches(&expected, &actual));

        if !topmost_decayed && !rect_decayed {
            continue;
        }

        let Ok(manager) = shared.manager.lock() else {
            continue;
        };
        if manager.has_saved_frame(hwnd)
            && manager.reapply_placement(hwnd, &placement, display.as_ref()).is_ok()
        {
            reasserted.push(hwnd);
        }
    }

    if !reasserted.is_empty()
        && let Ok(mut state) = shared.state.lock()
    {
        for hwnd in reasserted {
            if let Some(managed) = state.managed.get_mut(&hwnd) {
                managed.last_reassert = Some(now);
            }
        }
    }
}

/// The gaming-session tick: cursor lock, companion dimming with hover-undim,
/// and taskbar auto-hide. All decision logic lives in `session::plan_heartbeat`
/// so it is unit-tested; this function only gathers inputs and executes.
fn heartbeat_pass(shared: &Shared)
{
    let companion_windows: Vec<(isize, String)> = {
        let Ok(manager) = shared.manager.lock() else {
            return;
        };
        manager.get_windows().iter().map(|w| (w.hwnd, exe_key(&w.process_name))).collect()
    };

    let foreground = window_manager::foreground_window();
    let cursor_root = window_manager::cursor_root_window();

    let plan = {
        let Ok(state) = shared.state.lock() else {
            return;
        };

        let managed_lock_flags: HashMap<isize, bool> =
            state.managed.iter().map(|(&hwnd, m)| (hwnd, m.cursor_lock)).collect();

        let companion_hwnds: Vec<isize> = companion_windows
            .iter()
            .filter(|(hwnd, exe)| {
                !state.managed.contains_key(hwnd) && state.companions.contains(exe)
            })
            .map(|(hwnd, _)| *hwnd)
            .collect();

        session::plan_heartbeat(&PlanInputs {
            foreground,
            managed_lock_flags: &managed_lock_flags,
            lock_overrides: &state.lock_overrides,
            companion_hwnds: &companion_hwnds,
            dimmed: &state.dimmed,
            cursor_root,
            dim_enabled: state.dim_enabled,
            taskbar_autohide: state.taskbar_autohide,
        })
    };

    apply_heartbeat_plan(shared, plan);
}

fn apply_heartbeat_plan(shared: &Shared, plan: session::HeartbeatPlan)
{
    let currently_clipped = shared.state.lock().ok().and_then(|s| s.clipped);

    if plan.clip != currently_clipped {
        match plan.clip {
            Some(hwnd) => {
                if window_manager::clip_cursor_to_window(hwnd) {
                    if let Ok(mut state) = shared.state.lock() {
                        state.clipped = Some(hwnd);
                    }
                }
            },
            None => {
                window_manager::release_cursor_clip();
                if let Ok(mut state) = shared.state.lock() {
                    state.clipped = None;
                }
            },
        }
    } else if let Some(hwnd) = plan.clip {
        // Windows silently clears clips (focus churn, Win+L, resolution
        // changes); re-assert even when our own bookkeeping did not change.
        window_manager::clip_cursor_to_window(hwnd);
    }

    let alpha = {
        let Ok(state) = shared.state.lock() else {
            return;
        };
        session::alpha_from_percent(state.dim_alpha_pct)
    };

    for hwnd in plan.undim {
        window_manager::undim_window(hwnd);
        if let Ok(mut state) = shared.state.lock() {
            state.dimmed.remove(&hwnd);
        }
    }

    for hwnd in plan.dim {
        if window_manager::dim_window(hwnd, alpha)
            && let Ok(mut state) = shared.state.lock()
        {
            state.dimmed.insert(hwnd);
        }
    }

    let taskbar_was_touched = shared.state.lock().is_ok_and(|s| s.taskbar_prev.is_some());

    if plan.taskbar_should_hide && !taskbar_was_touched {
        let previous = window_manager::taskbar_autohide_state().unwrap_or(false);
        window_manager::set_taskbar_autohide(true);
        if let Ok(mut state) = shared.state.lock() {
            state.taskbar_prev = Some(previous);
        }
    } else if !plan.taskbar_should_hide && taskbar_was_touched {
        let previous = shared.state.lock().ok().and_then(|s| s.taskbar_prev);
        if let Some(previous) = previous {
            window_manager::set_taskbar_autohide(previous);
        }
        if let Ok(mut state) = shared.state.lock() {
            state.taskbar_prev = None;
        }
    }
}
