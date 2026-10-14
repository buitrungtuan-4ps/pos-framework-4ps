// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Whether the store's state can be read by anyone but the store server
//! ([ADR-0174](../../../docs/adr/0174-a-windows-store-keeps-its-state-to-its-own-service.md)
//! decision 5).
//!
//! The directory that holds the database is checked: on Windows its access-control list, which may
//! grant reading to SYSTEM, the Administrators group and the account the store server runs as, and
//! nobody else (`windows-protect`); on Linux and macOS its mode, which may grant nothing to group or
//! others. The answer is logged once at start, and `GET /api/activation` carries it so the setup
//! screen can say so. It names no file and no account: it is only useful to someone already at the
//! box.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Whether the store's state is closed to other local accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StateAccess {
    /// Only the store server, and on Windows SYSTEM and Administrators, can read it.
    #[serde(rename = "STATE_ACCESS_OK")]
    Ok,
    /// Some other local account can read it.
    #[serde(rename = "STATE_ACCESS_TOO_OPEN")]
    TooOpen,
    /// It could not be checked: the directory could not be read, or this platform has no check.
    #[serde(rename = "STATE_ACCESS_UNCHECKED")]
    Unchecked,
}

/// The directory that holds the database at `store_path`.
#[must_use]
pub fn state_dir(store_path: &Path) -> PathBuf {
    match store_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        // A bare `store.sqlite` lives in the working directory.
        _ => PathBuf::from("."),
    }
}

/// Checks `dir`. File-system calls: run it off the async workers.
#[must_use]
pub fn check(dir: &Path) -> StateAccess {
    #[cfg(windows)]
    {
        match windows_protect::dir_readers(dir) {
            Ok(windows_protect::access::Readers::OwnOnly) => StateAccess::Ok,
            Ok(windows_protect::access::Readers::Others) => StateAccess::TooOpen,
            Err(_unreadable) => StateAccess::Unchecked,
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(dir) {
            // The low six bits are the group's and others' read, write and search: none may be set.
            Ok(meta) if meta.permissions().mode().trailing_zeros() >= 6 => StateAccess::Ok,
            Ok(_) => StateAccess::TooOpen,
            Err(_unreadable) => StateAccess::Unchecked,
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = dir;
        StateAccess::Unchecked
    }
}

/// Checks `dir` on the blocking pool.
pub async fn check_off_the_workers(dir: PathBuf) -> StateAccess {
    tokio::task::spawn_blocking(move || check(&dir))
        .await
        .unwrap_or(StateAccess::Unchecked)
}

/// Says once, at start, when the state is open to other accounts, and what fixes it.
pub async fn log_at_start(dir: PathBuf) {
    match check_off_the_workers(dir).await {
        StateAccess::Ok => {}
        StateAccess::TooOpen => tracing::warn!(
            "other accounts on this computer can read the store's data: run the installer again, \
             as an administrator, to close it (ADR-0174)"
        ),
        StateAccess::Unchecked => tracing::info!(
            "could not check who can read the store's data on this computer (ADR-0174)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{StateAccess, state_dir};

    #[test]
    fn the_state_directory_is_the_database_s_own() {
        assert_eq!(
            state_dir(Path::new("/var/lib/pos-edge/store.sqlite")),
            PathBuf::from("/var/lib/pos-edge")
        );
        assert_eq!(state_dir(Path::new("store.sqlite")), PathBuf::from("."));
    }

    #[test]
    fn the_wire_names_are_the_records_own() {
        let names: Vec<String> = [
            StateAccess::Ok,
            StateAccess::TooOpen,
            StateAccess::Unchecked,
        ]
        .iter()
        .map(|access| serde_json::to_string(access).expect("json"))
        .collect();
        assert_eq!(
            names,
            [
                "\"STATE_ACCESS_OK\"",
                "\"STATE_ACCESS_TOO_OPEN\"",
                "\"STATE_ACCESS_UNCHECKED\""
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_others_can_read_is_too_open_and_one_they_cannot_is_ok() {
        use std::os::unix::fs::PermissionsExt;

        use super::check;

        let dir = tempfile::tempdir().expect("dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).expect("755");
        assert_eq!(check(dir.path()), StateAccess::TooOpen);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("700");
        assert_eq!(check(dir.path()), StateAccess::Ok);
        assert_eq!(check(&dir.path().join("missing")), StateAccess::Unchecked);
    }
}
