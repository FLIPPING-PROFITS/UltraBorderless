// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Alpha-Leader

//! Best-effort persistence of original window frames, so "Restore Borders"
//! still works after this app restarts or crashes while windows were stripped.
//!
//! Window handles do not survive a restart, so frames are keyed by executable
//! and window class instead, and re-attached ("adopted") when a live borderless
//! window matching both appears. Kept in its own file — `frames.txt`, next to
//! `config.txt` — because it is state, not settings.

use std::path::PathBuf;

/// A stripped window's original frame, in a restart-safe form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedFrame
{
    /// Normalized via [`crate::profiles::exe_key`].
    pub exe: String,
    pub class: String,
    pub style: u32,
    /// left, top, right, bottom.
    pub rect: (i32, i32, i32, i32),
    pub topmost: bool,
    /// Unix seconds when the frame was saved.
    pub saved_at: u64,
}

/// An entry this old is dropped rather than adopted: the odds that it still
/// describes a live window are gone.
pub const MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// Cap on stored entries so the file cannot grow without bound.
pub const MAX_FRAMES: usize = 32;

pub fn unix_now() -> u64
{
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parses the numbered `frame.<n>.<field>=value` format. Entries missing an
/// exe or class are unusable for adoption and dropped; junk never errors.
pub fn parse(contents: &str) -> Vec<PersistedFrame>
{
    let mut builders: std::collections::BTreeMap<usize, PersistedFrame> = Default::default();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let Some(rest) = key.trim().strip_prefix("frame.") else {
            continue;
        };
        let Some((index, field)) = rest.split_once('.') else {
            continue;
        };
        let Ok(index) = index.parse::<usize>() else {
            continue;
        };

        let builder = builders.entry(index).or_insert_with(|| PersistedFrame {
            exe: String::new(),
            class: String::new(),
            style: 0,
            rect: (0, 0, 0, 0),
            topmost: false,
            saved_at: 0,
        });

        let value = value.trim();
        match field {
            "exe" => builder.exe = value.to_string(),
            "class" => builder.class = value.to_string(),
            "style" => {
                if let Ok(parsed) = value.parse::<u32>() {
                    builder.style = parsed;
                }
            },
            "rect" => {
                let parts: Vec<i32> =
                    value.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if let [left, top, right, bottom] = parts[..] {
                    builder.rect = (left, top, right, bottom);
                }
            },
            "topmost" => builder.topmost = value == "true",
            "saved_at" => {
                if let Ok(parsed) = value.parse::<u64>() {
                    builder.saved_at = parsed;
                }
            },
            _ => {},
        }
    }

    builders
        .into_values()
        .filter(|f| !f.exe.is_empty() && !f.class.is_empty() && f.style != 0)
        .collect()
}

pub fn serialize(frames: &[PersistedFrame]) -> String
{
    use std::fmt::Write as _;

    let mut contents = format!("# {} saved window frames\n", env!("CARGO_PKG_NAME"));

    for (index, frame) in frames.iter().take(MAX_FRAMES).enumerate() {
        let _ = write!(
            contents,
            "frame.{i}.exe={}\n\
             frame.{i}.class={}\n\
             frame.{i}.style={}\n\
             frame.{i}.rect={},{},{},{}\n\
             frame.{i}.topmost={}\n\
             frame.{i}.saved_at={}\n",
            frame.exe,
            frame.class,
            frame.style,
            frame.rect.0,
            frame.rect.1,
            frame.rect.2,
            frame.rect.3,
            frame.topmost,
            frame.saved_at,
            i = index,
        );
    }

    contents
}

pub fn is_stale(frame: &PersistedFrame, now_secs: u64) -> bool
{
    now_secs.saturating_sub(frame.saved_at) > MAX_AGE_SECS
}

/// Loads the persisted frames, already filtered for staleness.
pub fn load() -> Vec<PersistedFrame>
{
    let Some(path) = frames_path() else {
        return Vec::new();
    };

    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    let now = unix_now();
    parse(&contents).into_iter().filter(|f| !is_stale(f, now)).collect()
}

/// Best effort, like the config: a failed write never disrupts the app.
pub fn save(frames: &[PersistedFrame])
{
    let Some(path) = frames_path() else {
        return;
    };

    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }

    let _ = std::fs::write(&path, serialize(frames));
}

fn frames_path() -> Option<PathBuf>
{
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(env!("CARGO_PKG_NAME")).join("frames.txt"))
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn frame(exe: &str, saved_at: u64) -> PersistedFrame
    {
        PersistedFrame {
            exe: exe.to_string(),
            class: "TestClass".to_string(),
            style: 0x00CF_0000,
            rect: (100, -50, 1820, 1030),
            topmost: true,
            saved_at,
        }
    }

    #[test]
    fn frames_round_trip_through_serialize_and_parse()
    {
        let frames = vec![frame("game", 1_725_000_000), frame("notepad", 1_725_000_100)];
        assert_eq!(parse(&serialize(&frames)), frames);
    }

    #[test]
    fn unusable_entries_are_dropped()
    {
        let parsed = parse(
            "frame.0.class=NoExe\n\
             frame.0.style=100\n\
             frame.1.exe=no-class\n\
             frame.1.style=100\n\
             frame.2.exe=no-style\n\
             frame.2.class=C\n\
             junk line\n\
             frame.bad.exe=x\n",
        );
        assert!(parsed.is_empty());
    }

    #[test]
    fn staleness_is_a_week()
    {
        let now = 10_000_000;
        assert!(!is_stale(&frame("game", now - MAX_AGE_SECS), now));
        assert!(is_stale(&frame("game", now - MAX_AGE_SECS - 1), now));
        // A clock that went backwards must not mark everything stale.
        assert!(!is_stale(&frame("game", now + 100), now));
    }

    #[test]
    fn serialize_caps_the_entry_count()
    {
        let frames: Vec<PersistedFrame> =
            (0..MAX_FRAMES + 10).map(|i| frame(&format!("exe{}", i), 1)).collect();
        assert_eq!(parse(&serialize(&frames)).len(), MAX_FRAMES);
    }
}
