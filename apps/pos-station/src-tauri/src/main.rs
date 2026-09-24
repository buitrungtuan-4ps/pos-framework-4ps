// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! POS Station: a Tauri v2 shell around the edge's own till
//! ([ADR-0147](../../../../docs/adr/0147-pos-station-is-a-tauri-shell-over-the-edge.md)).
//!
//! The window loads the till from the edge's URL, so it is same-origin and always the version that
//! edge serves; the app bundles only its connect and status pages. Pairing is native — the token goes
//! to the OS credential store and reaches the till through an initialization script — and remote
//! pages get no commands. On the store PC (the edge on `127.0.0.1:8080`) it adds a tray and
//! notifications; on another machine it runs the print agent as a sidecar. See
//! `docs/guides/pos-station.md` for building, signing and the spike's measurements.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

mod address;
mod commands;
mod config;
mod edge;
mod health;
mod i18n;
mod init_script;
mod logging;
mod monitor;
mod sidecar;
mod station;
mod vault;

fn main() -> ExitCode {
    logging::init();
    let built = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            commands::set_language,
            commands::connect_context,
            commands::pair,
            commands::status,
        ])
        .setup(|app| {
            station::start(app.handle());
            Ok(())
        })
        .build(context());
    match built {
        Ok(app) => {
            app.run(|app, event| station::on_run_event(app, &event));
            ExitCode::SUCCESS
        }
        Err(error) => {
            log::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// The context compiled from `tauri.conf.json`, the capability file and `ui/`.
///
/// Tauri's macro builds it on a helper thread and, should that thread panic, prints one line and
/// exits. That generated code is the only print and the only `exit` in this crate, and it is not
/// ours to change, so the bans are lifted here and nowhere else.
#[expect(
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::exit,
    reason = "tauri::generate_context! expands to an eprintln! and a process::exit this crate cannot change"
)]
fn context() -> tauri::Context<tauri::Wry> {
    tauri::generate_context!()
}
