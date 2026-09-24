// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The app's own settings file, `station.json` in the app's configuration directory.
//!
//! It holds where the edge is and two preferences — never the token, which is in the OS credential
//! store. Every field has a default, so a missing file, a missing field or an unreadable file all
//! give a working app; an unreadable file is logged and replaced on the next save.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    /// The edge this device paired with, as an origin (`http://192.168.1.10:8080`). `None` until the
    /// first pairing.
    pub(crate) edge_origin: Option<String>,
    /// Whether the till opens full screen. It opens when the app starts, which the installer arranges
    /// at login; on by default, off for a machine that also does other work.
    pub(crate) till_full_screen: bool,
    /// The language the bundled pages last reported (`en`, `vi`), for the tray and notifications.
    pub(crate) language: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            edge_origin: None,
            till_full_screen: true,
            language: None,
        }
    }
}

/// Reads the settings, falling back to the defaults when the file is absent or unreadable.
pub(crate) fn load(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).unwrap_or_else(|error| {
            log::warn!("{} is unreadable ({error}); using defaults", path.display());
            Settings::default()
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Settings::default(),
        Err(error) => {
            log::warn!(
                "{} could not be read ({error}); using defaults",
                path.display()
            );
            Settings::default()
        }
    }
}

fn parse(text: &str) -> Result<Settings, serde_json::Error> {
    serde_json::from_str(text)
}

/// Writes the settings: to a temporary file first, then renamed over the old one, so a crash mid-write
/// leaves the previous file rather than half of a new one.
pub(crate) fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::{Settings, parse};

    #[test]
    fn an_empty_file_is_the_defaults_and_the_till_is_full_screen_by_default() {
        let settings = parse("{}").unwrap();
        assert_eq!(settings, Settings::default());
        assert!(settings.till_full_screen);
        assert_eq!(settings.edge_origin, None);
    }

    #[test]
    fn the_settings_round_trip_in_snake_case() {
        let settings = Settings {
            edge_origin: Some("http://192.168.1.10:8080".to_owned()),
            till_full_screen: false,
            language: Some("vi".to_owned()),
        };
        let text = serde_json::to_string(&settings).unwrap();
        assert!(text.contains("\"edge_origin\""));
        assert!(text.contains("\"till_full_screen\":false"));
        assert_eq!(parse(&text).unwrap(), settings);
    }

    #[test]
    fn an_unknown_field_from_a_newer_build_is_ignored() {
        let settings = parse(r#"{"till_full_screen": false, "something_new": 1}"#).unwrap();
        assert!(!settings.till_full_screen);
    }
}
