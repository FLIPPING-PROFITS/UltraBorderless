// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2025-2026 Z1xus
// Copyright (C) 2026 Alpha-Leader
//
// Modified from ihateborders <https://github.com/Z1xus/ihateborders>.
// See NOTICE for the list of changes.

#![windows_subsystem = "windows"]

mod app;
mod config;
mod frames;
mod hotkeys;
mod profiles;
mod service;
mod session;
mod ui;
mod window_manager;

use app::{BorderlessApp, create_app_options};
use config::Config;

fn main() -> Result<(), eframe::Error>
{
    // Loaded before the window exists: start_minimized decides whether the
    // window is created visible at all.
    let config = Config::load();
    let options = create_app_options(&config);

    eframe::run_native(
        env!("CARGO_PKG_NAME"),
        options,
        Box::new(|cc| Ok(Box::new(BorderlessApp::new(cc, config)))),
    )
}
