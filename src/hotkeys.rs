// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Alpha-Leader

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use std::fmt;

/// A parsed hotkey binding like `ctrl+alt+b`. Kept separate from the
/// `global_hotkey` types so parsing and formatting stay pure and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding
{
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Lowercased key token: a letter, a digit, or `f1`..`f24`.
    pub key: String,
}

impl fmt::Display for Binding
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        if self.ctrl {
            write!(f, "ctrl+")?;
        }
        if self.alt {
            write!(f, "alt+")?;
        }
        if self.shift {
            write!(f, "shift+")?;
        }
        if self.win {
            write!(f, "win+")?;
        }
        write!(f, "{}", self.key)
    }
}

/// Parses `mod+mod+key`. At least one modifier is required — a bare global
/// hotkey would swallow ordinary typing system-wide. Returns `None` on
/// anything malformed; the caller falls back to its default.
pub fn parse_binding(text: &str) -> Option<Binding>
{
    let mut binding =
        Binding { ctrl: false, alt: false, shift: false, win: false, key: String::new() };

    let tokens: Vec<String> = text.split('+').map(|t| t.trim().to_ascii_lowercase()).collect();

    if tokens.len() < 2 {
        return None;
    }

    let (key, modifiers) = tokens.split_last()?;

    for modifier in modifiers {
        match modifier.as_str() {
            "ctrl" | "control" => binding.ctrl = true,
            "alt" => binding.alt = true,
            "shift" => binding.shift = true,
            "win" | "super" | "meta" | "cmd" | "command" => binding.win = true,
            _ => return None,
        }
    }

    if key_code(key).is_none() {
        return None;
    }
    binding.key = key.clone();

    Some(binding)
}

/// The `Code` for a key token, or `None` for anything unsupported.
fn key_code(key: &str) -> Option<Code>
{
    let code = match key {
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        _ => return None,
    };

    Some(code)
}

/// Converts a binding into the registrable form. Built explicitly rather than
/// through the crate's string parser so what we validate is what we register.
pub fn to_hotkey(binding: &Binding) -> Option<HotKey>
{
    let mut modifiers = Modifiers::empty();
    if binding.ctrl {
        modifiers |= Modifiers::CONTROL;
    }
    if binding.alt {
        modifiers |= Modifiers::ALT;
    }
    if binding.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if binding.win {
        modifiers |= Modifiers::SUPER;
    }

    Some(HotKey::new(Some(modifiers), key_code(&binding.key)?))
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn parses_and_round_trips_valid_bindings()
    {
        let binding = parse_binding("ctrl+alt+b").unwrap();
        assert!(binding.ctrl && binding.alt && !binding.shift && !binding.win);
        assert_eq!(binding.key, "b");
        assert_eq!(binding.to_string(), "ctrl+alt+b");

        // Case, whitespace, and modifier aliases normalize away.
        let messy = parse_binding(" Ctrl + Shift + F12 ").unwrap();
        assert_eq!(messy.to_string(), "ctrl+shift+f12");

        let win = parse_binding("super+9").unwrap();
        assert!(win.win);
        assert_eq!(win.to_string(), "win+9");
    }

    #[test]
    fn rejects_junk()
    {
        // No modifier: would swallow plain typing system-wide.
        assert!(parse_binding("b").is_none());
        assert!(parse_binding("").is_none());
        // Unknown modifier or key.
        assert!(parse_binding("hyper+b").is_none());
        assert!(parse_binding("ctrl+banana").is_none());
        // A modifier alone is not a binding.
        assert!(parse_binding("ctrl+").is_none());
    }

    #[test]
    fn duplicate_modifiers_collapse()
    {
        let binding = parse_binding("ctrl+control+ctrl+b").unwrap();
        assert_eq!(binding.to_string(), "ctrl+b");
    }

    #[test]
    fn valid_bindings_convert_to_hotkeys()
    {
        let hotkey = to_hotkey(&parse_binding("ctrl+alt+b").unwrap()).unwrap();
        assert_eq!(hotkey.key, Code::KeyB);
        assert!(hotkey.mods.contains(Modifiers::CONTROL));
        assert!(hotkey.mods.contains(Modifiers::ALT));
    }
}
