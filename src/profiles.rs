// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Alpha-Leader

use crate::window_manager::Placement;

/// A remembered placement for one application, matched by executable name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile
{
    /// Normalized via [`exe_key`], so it matches however the name was captured.
    pub exe: String,
    pub placement: Placement,
    /// Monitor device name (`\\.\DISPLAY1`); empty means unset.
    pub display_device: String,
    /// Fallback when the device name no longer resolves.
    pub display_index: usize,
    /// Apply automatically when a matching window appears.
    pub auto_apply: bool,
    /// Confine the cursor to the window while it has focus. Opt-in: a mouse
    /// that unexpectedly cannot leave a window is the worse surprise.
    pub cursor_lock: bool,
}

/// Canonical form of an executable name: the enumeration stores the stem
/// (`game`), config files and users may write `Game.exe` — both must match.
pub fn exe_key(name: &str) -> String
{
    let name = name.trim();
    let stem = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name);
    stem.to_ascii_lowercase()
}

pub fn find_profile<'a>(profiles: &'a [Profile], process_name: &str) -> Option<&'a Profile>
{
    let key = exe_key(process_name);
    profiles.iter().find(|profile| profile.exe == key)
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn exe_key_normalizes_case_and_extension()
    {
        assert_eq!(exe_key("Game.exe"), "game");
        assert_eq!(exe_key("game"), "game");
        assert_eq!(exe_key("GAME.EXE"), "game");
        assert_eq!(exe_key("  Notepad.exe "), "notepad");
        // Only the exe extension is special; other dots belong to the name.
        assert_eq!(exe_key("some.game.exe"), "some.game");
    }

    #[test]
    fn find_profile_matches_regardless_of_captured_form()
    {
        let profiles = vec![Profile {
            exe: exe_key("Game.exe"),
            placement: Placement::default(),
            display_device: String::new(),
            display_index: 0,
            auto_apply: true,
            cursor_lock: false,
        }];

        assert!(find_profile(&profiles, "game").is_some());
        assert!(find_profile(&profiles, "GAME.exe").is_some());
        assert!(find_profile(&profiles, "othergame").is_none());
    }
}
