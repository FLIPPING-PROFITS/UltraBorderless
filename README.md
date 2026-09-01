## UltraBorderless <img src="assets/icon.png" alt="UltraBorderless icon" width="48" height="48" align="left">
![Windows Only](https://img.shields.io/badge/platform-Windows-blue?logo=windows)
[![License](https://img.shields.io/badge/license-GPL--3.0-green)](LICENSE)
[![Downloads](https://img.shields.io/github/downloads/Alpha-Leader/UltraBorderless/total)](https://github.com/Alpha-Leader/UltraBorderless/releases)
[![Issues](https://img.shields.io/github/issues/Alpha-Leader/UltraBorderless)](https://github.com/Alpha-Leader/UltraBorderless/issues)

A lightweight Windows utility that strips the borders from any window and places it into a
**region** of your monitor — sized and anchored how you want, rather than only stretching it across
the whole display.

Built for ultrawides. On a 7680×2160 monitor you can run a game as a 3840×2160 borderless window
dead center, covering the taskbar, with 1920 px of desktop still usable on each side.

<p align="center">
  <img src="assets/screenshot.png" alt="UltraBorderless placing a 3840x2160 region centered on a 7680x2160 display" width="380">
</p>

### Credit

This project is a fork of [**ihateborders** by Z1xus](https://github.com/Z1xus/ihateborders),
forked at commit `b5bb27e` (v1.1.1, 2026-02-16). All of the original window-manipulation work is
theirs — this fork adds region placement, always-on-top, persisted settings, and a number of
correctness fixes.

See [NOTICE](NOTICE) for the full list of modifications, and go star
[the original](https://github.com/Z1xus/ihateborders).

### Installation

#### Portable

Download the latest release from the [Releases](https://github.com/Alpha-Leader/UltraBorderless/releases)
page. It's a single self-contained `.exe` — no installer, no dependencies. Verify it against the
`SHA256SUMS.txt` published alongside it if you like.

The binary is unsigned, so Windows SmartScreen will likely warn on first run — choose
**More info → Run anyway**, or build it yourself from source (see below).

#### Scoop

```bash
scoop install https://raw.githubusercontent.com/Alpha-Leader/UltraBorderless/main/scoop/ultraborderless.json
```

### Usage

1. Run the executable.
2. Pick a window from the dropdown.
3. Choose a **Placement** mode.
4. Click **Make Borderless**. Click **Restore Borders** to put it back exactly as it was.

Or set it up once and forget it: save a **profile** for the game and let auto-apply fix the
window every time it appears, with the app sitting in the tray.

#### Placement modes

| Mode | Behavior |
| --- | --- |
| **Region** | Places the window into a sub-rectangle of the display. Set a size, pick an anchor. |
| **Full display** | Fills the entire monitor. |
| **Leave in place** | Strips borders without moving or resizing. |

In Region mode you get size fields with `4K` / `1440p` / `1080p` presets, and an anchor of
**Centered**, **Left**, **Right**, or **Custom** (explicit x/y offset). A live preview shows exactly
where the window will land before you commit — e.g. `→ 3840x2160 at (1920, 0)` with
`sides free: 1920px | 1920px`. It turns amber if the region runs past the display edge.

#### Always on top

**This is what makes taskbar coverage work.** Windows only auto-hides the taskbar for a window that
fills an *entire* monitor, so a narrower region can never cover it by position alone. Enabling this
puts the window in the topmost band instead. It's on by default; restoring borders returns the
window to its original z-order.

#### Profiles and auto-apply

Select a window and click **Save profile for \<exe\>** in the **Profiles** section to remember the
current placement and display for that application. With the profile's **auto** checkbox on, the
window is fixed automatically within a few seconds of appearing — launch the game and it lands in
its region, no clicking. Because some games re-take their own size shortly after launch, the
placement is asserted once more two seconds after an auto-apply.

Auto-apply fires once per window: restoring a window by hand is respected, not fought. Profiles
remember the monitor by device name, so they survive monitors being plugged in or removed.

#### System tray and hotkeys

The app lives in the system tray. By default the close button hides the window there —
auto-apply and hotkeys keep working — and the tray menu has **Show / Hide**,
**Restore all windows**, and **Quit**. Left-click the tray icon to bring the window back.
**Start minimized to tray** (in **Options**) makes it launch straight to the tray.

Two global hotkeys work from anywhere, no alt-tabbing to the app:

- `Ctrl+Alt+B` — toggle borderless on the **focused** window, using its profile if it has one and
  the current UI placement otherwise
- `Ctrl+Alt+R` — restore every window this app stripped

Bindings are editable in the config file (`hotkey_toggle`, `hotkey_restore_all`,
`hotkey_cursor_lock`) as `modifier+key`, e.g. `ctrl+shift+f12`. Modifiers: `ctrl`, `alt`, `shift`,
`win`; keys: letters, digits, `f1`–`f12`. If a binding is taken by another app, a status message
says so and everything else keeps working.

#### Cursor lock

A profile's 🖱 toggle confines the mouse to that window's rectangle while it has focus — the fix
for edge-scroll or a stray click sliding the pointer into the desktop strip beside the game.
`Ctrl+Alt+L` toggles the lock for whatever window is focused right now, profile or not, as a
session-only override. It releases the moment focus moves elsewhere and is off by default: opt in
per profile, since a mouse that unexpectedly can't leave a window is a worse surprise than not
locking it.

#### Focus & topmost watchdog

Every window this app has placed is watched continuously. If something knocks it out of the
topmost band or nudges it off its region — a driver overlay, a game re-asserting its own window,
Windows' own z-order shuffling — it's put back automatically, usually within a quarter of a
second while it has focus and at worst by the next 5-second scan. A short debounce keeps it from
fighting a placement you're deliberately adjusting by hand.

#### Taskbar auto-hide and companion dimming

Two more options for a fully immersive region, both in **Options**:

- **Auto-hide taskbar while gaming** hides the taskbar for as long as any window is managed by
  this app, so a notification flash can't draw over the game. It's restored exactly as it was
  when the last managed window closes or the app exits — and if the app is ever killed outright
  while this is on, the next launch notices and restores it rather than leaving your taskbar
  stuck.
- **Dim companion windows while gaming** — mark a window (Discord, a browser, a stats page) as a
  **Companion** in its own section, and it dims to a set brightness the moment a managed game
  gains focus, so it's visible in your peripheral vision without competing for attention. Hover a
  dimmed companion to bring it back to full brightness without touching the game's focus; move
  away and it dims again.

### Interface

- **[B]** — borderless window
- **[W]** — windowed (has borders)
- System windows are filtered out, and the list refreshes every 5 seconds
- The display dropdown only appears if you have more than one monitor

### Keyboard shortcuts

In the app:

- `F5` — refresh the window list
- `Esc` — clear the current selection

Global (work everywhere): `Ctrl+Alt+B` toggle focused window, `Ctrl+Alt+R` restore all,
`Ctrl+Alt+L` toggle cursor lock on the focused window — see
[System tray and hotkeys](#system-tray-and-hotkeys).

### Settings

Persisted to `%APPDATA%\ultraborderless\config.txt` as plain `key=value` text — placement,
display, tray behavior, hotkey bindings, gaming-session options, and one numbered block each for
profiles (`profile.0.exe=game`, `profile.0.width=3840`, …) and companions (`companion.0.exe=discord`).
Saved on exit and after each action; delete the file to reset to defaults. Unknown keys are
ignored, so files travel between versions.

Original window frames are persisted separately to `frames.txt` in the same folder, so
**Restore Borders** still works after the app restarts — even after a crash: on relaunch a saved
frame is re-attached to a window matching the same executable and window class. Entries expire
after seven days.

### Using it with games

- Set the game to **Windowed** mode first, not its own borderless or exclusive-fullscreen mode —
  exclusive fullscreen has no border to strip.
- Set the game's internal render resolution to match the region size.
- Some games re-assert their own window size after placement or on a focus/resolution change
  (auto-apply re-asserts the placement once, two seconds in, which handles most of them), and a
  few ignore external placement entirely.

### Requirements

- Windows 10/11
- To modify an elevated window (Task Manager, an admin terminal), run this as administrator too —
  Windows blocks a normal-privilege process from restyling an elevated one.

### Building

```bash
git clone https://github.com/Alpha-Leader/UltraBorderless
cd UltraBorderless
cargo build --release
```

The binary lands in `target/release/ultraborderless.exe`, and is portable — no `target-cpu=native`,
so a build from one machine runs on any x86-64 CPU.

Run the tests with `cargo test`.

### License

GPL-3.0-only — see [LICENSE](LICENSE). As a derivative of ihateborders, which is GPL-3.0, this
project remains under the same license.
