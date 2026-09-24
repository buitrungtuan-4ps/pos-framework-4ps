// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A small stderr sink for the `log` facade Tauri already logs through.
//!
//! `POS_STATION_LOG` sets this app's level (`error`, `warn`, `info` — the default — `debug`, `off`);
//! everything else (Tauri, wry, the webview) is shown from `warn` up. Lines carry no timestamp: the
//! service manager or terminal that captured stderr adds one.
//!
//! **Nothing here writes a token, a pairing code or a person.** The modules that log name edges by
//! origin and processes by pid; the types that hold a credential redact it in `Debug`.

use std::io::Write as _;

use log::{LevelFilter, Log, Metadata, Record};

/// This crate's module prefix, which gets the configured level.
const OWN_TARGET: &str = "pos_station";

struct StderrLog {
    own: LevelFilter,
}

impl Log for StderrLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let limit = if metadata.target().starts_with(OWN_TARGET) {
            self.own
        } else {
            LevelFilter::Warn
        };
        metadata.level() <= limit
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ignored = writeln!(
                std::io::stderr().lock(),
                "pos-station {} {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        let _ignored = std::io::stderr().lock().flush();
    }
}

/// Installs the sink, once. A second call is ignored.
pub(crate) fn init() {
    let own = std::env::var("POS_STATION_LOG")
        .ok()
        .and_then(|value| value.trim().parse::<LevelFilter>().ok())
        .unwrap_or(LevelFilter::Info);
    if log::set_boxed_logger(Box::new(StderrLog { own })).is_ok() {
        log::set_max_level(own.max(LevelFilter::Warn));
    }
}
