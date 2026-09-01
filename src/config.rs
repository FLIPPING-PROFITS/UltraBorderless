// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Alpha-Leader

use crate::{
    profiles::{Profile, exe_key},
    window_manager::{Anchor, Placement, PlacementMode},
};
use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf};

/// Settings that survive a restart, stored as `key=value` lines in
/// `%APPDATA%\<package name>\config.txt`.
///
/// Deliberately hand-rolled: the whole format is a handful of integers, enums
/// and short strings, which is not worth a serialization dependency. A missing
/// or malformed file is never an error — anything unparseable falls back to the
/// default, and unknown keys are skipped, so files travel between versions in
/// both directions.
///
/// Profiles are numbered dotted keys (`profile.0.exe=game`); a block without an
/// `exe` is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config
{
    pub placement: Placement,
    pub display_index: usize,
    /// Monitor device name; preferred over `display_index`, which is fragile
    /// across monitor changes.
    pub display_device: String,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub hotkey_toggle: String,
    pub hotkey_restore_all: String,
    pub hotkey_cursor_lock: String,
    /// Auto-hide the taskbar while any managed window exists.
    pub taskbar_autohide: bool,
    /// Dim companion windows while a managed window has focus.
    pub dim_enabled: bool,
    /// Remaining brightness of a dimmed companion, in percent.
    pub dim_alpha_pct: u8,
    /// Executables (exe_key form) to dim while gaming — `companion.<n>.exe`.
    pub companions: Vec<String>,
    pub profiles: Vec<Profile>,
}

impl Default for Config
{
    fn default() -> Self
    {
        Self {
            placement: Placement::default(),
            display_index: 0,
            display_device: String::new(),
            close_to_tray: true,
            start_minimized: false,
            hotkey_toggle: "ctrl+alt+b".to_string(),
            hotkey_restore_all: "ctrl+alt+r".to_string(),
            hotkey_cursor_lock: "ctrl+alt+l".to_string(),
            taskbar_autohide: false,
            dim_enabled: false,
            dim_alpha_pct: 40,
            companions: Vec::new(),
            profiles: Vec::new(),
        }
    }
}

impl Config
{
    pub fn load() -> Self
    {
        let Some(path) = config_path() else {
            return Self::default();
        };

        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };

        Self::parse(&contents)
    }

    fn parse(contents: &str) -> Self
    {
        let mut config = Self::default();
        let mut builders: BTreeMap<usize, Profile> = BTreeMap::new();
        let mut companions: BTreeMap<usize, String> = BTreeMap::new();

        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let key = key.trim();
            let value = value.trim();

            if let Some(rest) = key.strip_prefix("profile.") {
                let Some((index, field)) = rest.split_once('.') else {
                    continue;
                };
                let Ok(index) = index.parse::<usize>() else {
                    continue;
                };

                let builder = builders.entry(index).or_insert_with(empty_profile);
                apply_profile_field(builder, field, value);
                continue;
            }

            if let Some(rest) = key.strip_prefix("companion.") {
                if let Some((index, "exe")) = rest.split_once('.')
                    && let Ok(index) = index.parse::<usize>()
                {
                    companions.insert(index, exe_key(value));
                }
                continue;
            }

            match key {
                "mode" => config.placement.mode = parse_mode(value),
                "anchor" => config.placement.anchor = parse_anchor(value),
                "width" => set_dimension(&mut config.placement.width, value),
                "height" => set_dimension(&mut config.placement.height, value),
                "custom_x" => set_offset(&mut config.placement.custom_x, value),
                "custom_y" => set_offset(&mut config.placement.custom_y, value),
                "always_on_top" => config.placement.always_on_top = value == "true",
                "display_index" => set_index(&mut config.display_index, value),
                "display_device" => config.display_device = value.to_string(),
                "close_to_tray" => config.close_to_tray = value == "true",
                "start_minimized" => config.start_minimized = value == "true",
                "hotkey_toggle" => config.hotkey_toggle = value.to_string(),
                "hotkey_restore_all" => config.hotkey_restore_all = value.to_string(),
                "hotkey_cursor_lock" => config.hotkey_cursor_lock = value.to_string(),
                "taskbar_autohide" => config.taskbar_autohide = value == "true",
                "dim_enabled" => config.dim_enabled = value == "true",
                "dim_alpha" => {
                    if let Ok(parsed) = value.parse::<u8>() {
                        config.dim_alpha_pct = parsed.min(100);
                    }
                },
                _ => {},
            }
        }

        config.profiles = builders.into_values().filter(|p| !p.exe.is_empty()).collect();
        config.companions = companions.into_values().filter(|exe| !exe.is_empty()).collect();

        config
    }

    /// Best effort: a failed write must never disrupt the app, so errors are
    /// dropped rather than surfaced.
    pub fn save(&self)
    {
        let Some(path) = config_path() else {
            return;
        };

        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }

        let _ = std::fs::write(&path, self.serialize());
    }

    fn serialize(&self) -> String
    {
        let mut contents = format!(
            "# {} settings\n\
             mode={}\n\
             width={}\n\
             height={}\n\
             anchor={}\n\
             custom_x={}\n\
             custom_y={}\n\
             always_on_top={}\n\
             display_index={}\n\
             display_device={}\n\
             close_to_tray={}\n\
             start_minimized={}\n\
             hotkey_toggle={}\n\
             hotkey_restore_all={}\n\
             hotkey_cursor_lock={}\n\
             taskbar_autohide={}\n\
             dim_enabled={}\n\
             dim_alpha={}\n",
            env!("CARGO_PKG_NAME"),
            mode_str(self.placement.mode),
            self.placement.width,
            self.placement.height,
            anchor_str(self.placement.anchor),
            self.placement.custom_x,
            self.placement.custom_y,
            self.placement.always_on_top,
            self.display_index,
            self.display_device,
            self.close_to_tray,
            self.start_minimized,
            self.hotkey_toggle,
            self.hotkey_restore_all,
            self.hotkey_cursor_lock,
            self.taskbar_autohide,
            self.dim_enabled,
            self.dim_alpha_pct,
        );

        for (index, companion) in self.companions.iter().enumerate() {
            let _ = write!(contents, "companion.{}.exe={}\n", index, companion);
        }

        for (index, profile) in self.profiles.iter().enumerate() {
            let _ = write!(
                contents,
                "profile.{i}.exe={}\n\
                 profile.{i}.mode={}\n\
                 profile.{i}.width={}\n\
                 profile.{i}.height={}\n\
                 profile.{i}.anchor={}\n\
                 profile.{i}.custom_x={}\n\
                 profile.{i}.custom_y={}\n\
                 profile.{i}.always_on_top={}\n\
                 profile.{i}.display_device={}\n\
                 profile.{i}.display_index={}\n\
                 profile.{i}.auto_apply={}\n\
                 profile.{i}.cursor_lock={}\n",
                profile.exe,
                mode_str(profile.placement.mode),
                profile.placement.width,
                profile.placement.height,
                anchor_str(profile.placement.anchor),
                profile.placement.custom_x,
                profile.placement.custom_y,
                profile.placement.always_on_top,
                profile.display_device,
                profile.display_index,
                profile.auto_apply,
                profile.cursor_lock,
                i = index,
            );
        }

        contents
    }
}

fn empty_profile() -> Profile
{
    Profile {
        exe: String::new(),
        placement: Placement::default(),
        display_device: String::new(),
        display_index: 0,
        auto_apply: true,
        cursor_lock: false,
    }
}

fn apply_profile_field(profile: &mut Profile, field: &str, value: &str)
{
    match field {
        "exe" => profile.exe = exe_key(value),
        "mode" => profile.placement.mode = parse_mode(value),
        "anchor" => profile.placement.anchor = parse_anchor(value),
        "width" => set_dimension(&mut profile.placement.width, value),
        "height" => set_dimension(&mut profile.placement.height, value),
        "custom_x" => set_offset(&mut profile.placement.custom_x, value),
        "custom_y" => set_offset(&mut profile.placement.custom_y, value),
        "always_on_top" => profile.placement.always_on_top = value == "true",
        "display_device" => profile.display_device = value.to_string(),
        "display_index" => set_index(&mut profile.display_index, value),
        "auto_apply" => profile.auto_apply = value == "true",
        "cursor_lock" => profile.cursor_lock = value == "true",
        _ => {},
    }
}

fn parse_mode(value: &str) -> PlacementMode
{
    match value {
        "leave_in_place" => PlacementMode::LeaveInPlace,
        "region" => PlacementMode::Region,
        _ => PlacementMode::FullDisplay,
    }
}

fn mode_str(mode: PlacementMode) -> &'static str
{
    match mode {
        PlacementMode::LeaveInPlace => "leave_in_place",
        PlacementMode::FullDisplay => "full_display",
        PlacementMode::Region => "region",
    }
}

fn parse_anchor(value: &str) -> Anchor
{
    match value {
        "left" => Anchor::Left,
        "right" => Anchor::Right,
        "custom" => Anchor::Custom,
        _ => Anchor::Centered,
    }
}

fn anchor_str(anchor: Anchor) -> &'static str
{
    match anchor {
        Anchor::Centered => "centered",
        Anchor::Left => "left",
        Anchor::Right => "right",
        Anchor::Custom => "custom",
    }
}

fn set_dimension(target: &mut i32, value: &str)
{
    if let Ok(parsed) = value.parse::<i32>()
        && parsed > 0
    {
        *target = parsed;
    }
}

fn set_offset(target: &mut i32, value: &str)
{
    if let Ok(parsed) = value.parse::<i32>() {
        *target = parsed;
    }
}

fn set_index(target: &mut usize, value: &str)
{
    if let Ok(parsed) = value.parse::<usize>() {
        *target = parsed;
    }
}

fn config_path() -> Option<PathBuf>
{
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(env!("CARGO_PKG_NAME")).join("config.txt"))
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn unknown_and_malformed_entries_fall_back_to_defaults()
    {
        // Exercised indirectly: the parser must never panic on junk.
        let mut config = Config::default();
        set_dimension(&mut config.placement.width, "not-a-number");
        set_dimension(&mut config.placement.height, "-5");
        assert_eq!(config.placement.width, Placement::default().width);
        assert_eq!(config.placement.height, Placement::default().height);

        set_dimension(&mut config.placement.width, "2560");
        assert_eq!(config.placement.width, 2560);

        // Negative offsets are legitimate (a region left of the display origin).
        set_offset(&mut config.placement.custom_x, "-100");
        assert_eq!(config.placement.custom_x, -100);
    }

    #[test]
    fn config_round_trips_through_serialize_and_parse()
    {
        let config = Config {
            placement: Placement {
                mode: PlacementMode::Region,
                width: 2560,
                height: 1440,
                anchor: Anchor::Custom,
                custom_x: -50,
                custom_y: 30,
                always_on_top: false,
            },
            display_index: 1,
            display_device: r"\\.\DISPLAY2".to_string(),
            close_to_tray: false,
            start_minimized: true,
            hotkey_toggle: "ctrl+shift+f12".to_string(),
            hotkey_restore_all: "ctrl+alt+r".to_string(),
            hotkey_cursor_lock: "ctrl+alt+k".to_string(),
            taskbar_autohide: true,
            dim_enabled: true,
            dim_alpha_pct: 55,
            companions: vec!["discord".to_string(), "firefox".to_string()],
            profiles: vec![
                Profile {
                    exe: "game".to_string(),
                    placement: Placement { width: 3840, height: 2160, ..Placement::default() },
                    display_device: r"\\.\DISPLAY1".to_string(),
                    display_index: 0,
                    auto_apply: true,
                    cursor_lock: true,
                },
                Profile {
                    exe: "notepad".to_string(),
                    placement: Placement {
                        mode: PlacementMode::LeaveInPlace,
                        ..Placement::default()
                    },
                    display_device: String::new(),
                    display_index: 1,
                    auto_apply: false,
                    cursor_lock: false,
                },
            ],
        };

        assert_eq!(Config::parse(&config.serialize()), config);
    }

    #[test]
    fn companions_parse_normalized_and_junk_is_dropped()
    {
        let parsed = Config::parse(
            "companion.0.exe=Discord.exe\n\
             companion.2.exe=\n\
             companion.bad.exe=x\n\
             companion.3.name=not-an-exe-field\n\
             dim_alpha=250\n",
        );

        assert_eq!(parsed.companions, vec!["discord".to_string()]);
        // Out-of-range percent clamps rather than being taken literally.
        assert_eq!(parsed.dim_alpha_pct, 100);
    }

    #[test]
    fn profile_blocks_without_an_exe_are_dropped()
    {
        let parsed = Config::parse(
            "profile.0.width=1920\n\
             profile.0.height=1080\n\
             profile.1.exe=Game.exe\n\
             profile.1.auto_apply=false\n\
             profile.junk.exe=bad-index\n",
        );

        assert_eq!(parsed.profiles.len(), 1);
        assert_eq!(parsed.profiles[0].exe, "game");
        assert!(!parsed.profiles[0].auto_apply);
    }

    #[test]
    fn old_config_files_still_parse()
    {
        // The exact shape written by versions before profiles existed.
        let parsed = Config::parse(
            "# ultraborderless settings\n\
             mode=region\n\
             width=3840\n\
             height=2160\n\
             anchor=centered\n\
             custom_x=0\n\
             custom_y=0\n\
             always_on_top=true\n\
             display_index=0\n",
        );

        assert_eq!(parsed.placement.mode, PlacementMode::Region);
        assert_eq!(parsed.placement.width, 3840);
        assert!(parsed.profiles.is_empty());
        // New fields keep their defaults.
        assert_eq!(parsed.hotkey_toggle, "ctrl+alt+b");
        assert!(parsed.close_to_tray);
    }
}
