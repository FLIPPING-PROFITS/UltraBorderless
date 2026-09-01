// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Alpha-Leader

//! The decision logic for gaming-session effects — cursor lock, companion
//! dimming, and taskbar auto-hide — kept pure so the whole matrix is
//! unit-testable. The watcher thread gathers inputs, calls [`plan_heartbeat`],
//! and executes the returned actions through `window_manager`'s effect
//! functions.

use crate::window_manager::{DisplayInfo, Placement};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};
use windows::Win32::Foundation::RECT;

/// A window this app applied a placement to, and what was applied — the
/// watchdog re-asserts this when it decays.
#[derive(Debug, Clone)]
pub struct ManagedWindow
{
    pub placement: Placement,
    pub display: Option<DisplayInfo>,
    /// From the profile that applied it; false for manual/UI applies.
    pub cursor_lock: bool,
    /// Last time the watchdog re-asserted the placement, for debouncing.
    pub last_reassert: Option<Instant>,
}

/// What the heartbeat should do right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeartbeatPlan
{
    /// Confine the cursor to this window's rectangle, or release the clip.
    pub clip: Option<isize>,
    pub dim: Vec<isize>,
    pub undim: Vec<isize>,
    pub taskbar_should_hide: bool,
}

pub struct PlanInputs<'a>
{
    pub foreground: Option<isize>,
    /// Managed windows, as hwnd → cursor_lock flag. Doubles as the managed set.
    pub managed_lock_flags: &'a HashMap<isize, bool>,
    /// Session overrides from the cursor-lock hotkey: hwnd → forced on/off.
    pub lock_overrides: &'a HashMap<isize, bool>,
    /// Visible top-level windows of companion exes, managed windows excluded.
    pub companion_hwnds: &'a [isize],
    /// Windows this app currently has dimmed.
    pub dimmed: &'a HashSet<isize>,
    /// Top-level window under the mouse, for hover-to-undim.
    pub cursor_root: Option<isize>,
    pub dim_enabled: bool,
    pub taskbar_autohide: bool,
}

/// A hotkey override beats the profile flag; without either there is no lock.
pub fn effective_cursor_lock(override_flag: Option<bool>, profile_flag: Option<bool>) -> bool
{
    override_flag.unwrap_or_else(|| profile_flag.unwrap_or(false))
}

/// The alpha byte for a dim percentage, where the percent is how much
/// *brightness* remains (40 → dim to 40%). Clamped to a floor so a
/// misconfigured 0 cannot make a window fully invisible and unclickable-looking.
pub fn alpha_from_percent(percent: u8) -> u8
{
    let percent = percent.clamp(10, 100) as u32;
    (percent * 255 / 100) as u8
}

/// Whether a window sits where it was placed. DWM and apps adjust rectangles
/// by a pixel or two, so exact equality would make the watchdog fight noise.
pub fn rect_matches(expected: &RECT, actual: &RECT) -> bool
{
    const TOLERANCE: i32 = 2;

    (expected.left - actual.left).abs() <= TOLERANCE
        && (expected.top - actual.top).abs() <= TOLERANCE
        && (expected.right - actual.right).abs() <= TOLERANCE
        && (expected.bottom - actual.bottom).abs() <= TOLERANCE
}

pub fn plan_heartbeat(inputs: &PlanInputs<'_>) -> HeartbeatPlan
{
    let clip = inputs.foreground.filter(|fg| {
        effective_cursor_lock(
            inputs.lock_overrides.get(fg).copied(),
            inputs.managed_lock_flags.get(fg).copied(),
        )
    });

    let game_focused =
        inputs.foreground.is_some_and(|fg| inputs.managed_lock_flags.contains_key(&fg));

    let mut dim = Vec::new();
    let mut undim = Vec::new();

    if inputs.dim_enabled && game_focused {
        for &hwnd in inputs.companion_hwnds {
            let hovered = inputs.cursor_root == Some(hwnd);
            let is_dimmed = inputs.dimmed.contains(&hwnd);

            if hovered && is_dimmed {
                undim.push(hwnd);
            } else if !hovered && !is_dimmed {
                dim.push(hwnd);
            }
        }

        // A window we dimmed that stopped being a companion (config edit,
        // window replaced) must not stay dark.
        for &hwnd in inputs.dimmed {
            if !inputs.companion_hwnds.contains(&hwnd) {
                undim.push(hwnd);
            }
        }
    } else {
        // No focused game: everything we dimmed comes back.
        undim.extend(inputs.dimmed.iter().copied());
    }

    let taskbar_should_hide = inputs.taskbar_autohide && !inputs.managed_lock_flags.is_empty();

    HeartbeatPlan { clip, dim, undim, taskbar_should_hide }
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn inputs<'a>(
        foreground: Option<isize>,
        managed: &'a HashMap<isize, bool>,
        overrides: &'a HashMap<isize, bool>,
        companions: &'a [isize],
        dimmed: &'a HashSet<isize>,
    ) -> PlanInputs<'a>
    {
        PlanInputs {
            foreground,
            managed_lock_flags: managed,
            lock_overrides: overrides,
            companion_hwnds: companions,
            dimmed,
            cursor_root: None,
            dim_enabled: true,
            taskbar_autohide: true,
        }
    }

    #[test]
    fn clip_follows_profile_flag_and_hotkey_override()
    {
        let managed = HashMap::from([(10, true), (20, false)]);
        let mut overrides = HashMap::new();
        let dimmed = HashSet::new();

        // Profile flag on → clip while focused.
        assert_eq!(
            plan_heartbeat(&inputs(Some(10), &managed, &overrides, &[], &dimmed)).clip,
            Some(10)
        );
        // Profile flag off → no clip.
        assert_eq!(
            plan_heartbeat(&inputs(Some(20), &managed, &overrides, &[], &dimmed)).clip,
            None
        );
        // Focus elsewhere → no clip even with lockable windows around.
        assert_eq!(
            plan_heartbeat(&inputs(Some(99), &managed, &overrides, &[], &dimmed)).clip,
            None
        );
        assert_eq!(plan_heartbeat(&inputs(None, &managed, &overrides, &[], &dimmed)).clip, None);

        // Hotkey override wins in both directions, and works on unmanaged windows.
        overrides.insert(10, false);
        overrides.insert(99, true);
        assert_eq!(
            plan_heartbeat(&inputs(Some(10), &managed, &overrides, &[], &dimmed)).clip,
            None
        );
        assert_eq!(
            plan_heartbeat(&inputs(Some(99), &managed, &overrides, &[], &dimmed)).clip,
            Some(99)
        );
    }

    #[test]
    fn dims_companions_only_while_a_managed_window_is_focused()
    {
        let managed = HashMap::from([(10, false)]);
        let overrides = HashMap::new();
        let companions = [30, 31];
        let dimmed = HashSet::new();

        let focused = plan_heartbeat(&inputs(Some(10), &managed, &overrides, &companions, &dimmed));
        assert_eq!(focused.dim, vec![30, 31]);
        assert!(focused.undim.is_empty());

        // Game loses focus → nothing new dims, everything dimmed comes back.
        let dimmed_now = HashSet::from([30, 31]);
        let unfocused =
            plan_heartbeat(&inputs(Some(99), &managed, &overrides, &companions, &dimmed_now));
        assert!(unfocused.dim.is_empty());
        let mut undim = unfocused.undim.clone();
        undim.sort_unstable();
        assert_eq!(undim, vec![30, 31]);
    }

    #[test]
    fn hovering_a_dimmed_companion_undims_exactly_that_one()
    {
        let managed = HashMap::from([(10, false)]);
        let overrides = HashMap::new();
        let companions = [30, 31];
        let dimmed = HashSet::from([30, 31]);

        let mut hover = inputs(Some(10), &managed, &overrides, &companions, &dimmed);
        hover.cursor_root = Some(30);

        let plan = plan_heartbeat(&hover);
        assert_eq!(plan.undim, vec![30]);
        assert!(plan.dim.is_empty());

        // Cursor moves back over the game → the hovered one re-dims.
        let dimmed_after = HashSet::from([31]);
        let mut back = inputs(Some(10), &managed, &overrides, &companions, &dimmed_after);
        back.cursor_root = Some(10);
        assert_eq!(plan_heartbeat(&back).dim, vec![30]);
    }

    #[test]
    fn dim_disabled_still_restores_previously_dimmed_windows()
    {
        let managed = HashMap::from([(10, false)]);
        let overrides = HashMap::new();
        let dimmed = HashSet::from([30]);

        let mut plan_inputs = inputs(Some(10), &managed, &overrides, &[30], &dimmed);
        plan_inputs.dim_enabled = false;

        assert_eq!(plan_heartbeat(&plan_inputs).undim, vec![30]);
    }

    #[test]
    fn taskbar_hides_only_with_managed_windows_and_the_option_on()
    {
        let empty = HashMap::new();
        let managed = HashMap::from([(10, false)]);
        let overrides = HashMap::new();
        let dimmed = HashSet::new();

        assert!(
            plan_heartbeat(&inputs(None, &managed, &overrides, &[], &dimmed)).taskbar_should_hide
        );
        assert!(
            !plan_heartbeat(&inputs(None, &empty, &overrides, &[], &dimmed)).taskbar_should_hide
        );

        let mut off = inputs(None, &managed, &overrides, &[], &dimmed);
        off.taskbar_autohide = false;
        assert!(!plan_heartbeat(&off).taskbar_should_hide);
    }

    #[test]
    fn alpha_from_percent_maps_and_clamps()
    {
        assert_eq!(alpha_from_percent(100), 255);
        assert_eq!(alpha_from_percent(40), 102);
        // The floor keeps a window from vanishing entirely.
        assert_eq!(alpha_from_percent(0), alpha_from_percent(10));
        assert_eq!(alpha_from_percent(200), 255);
    }

    #[test]
    fn rect_matches_tolerates_dwm_noise()
    {
        let expected = RECT { left: 100, top: 50, right: 900, bottom: 650 };
        let nudged = RECT { left: 102, top: 48, right: 902, bottom: 652 };
        let drifted = RECT { left: 150, top: 50, right: 950, bottom: 650 };

        assert!(rect_matches(&expected, &expected));
        assert!(rect_matches(&expected, &nudged));
        assert!(!rect_matches(&expected, &drifted));
    }
}
