// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a stopping edge says about its last drain, on its own volume
//! ([ADR-0113](../../../docs/adr/0113-the-host-agent.md), "Drain before stop").
//!
//! A hosted store's container and volume go together, so an event still in the outbox when the
//! process exits is an event the cloud may never see. The host agent that stopped the container
//! has to know whether that happened, and ADR-0113 refuses it a log line, an `exec` into the
//! container or a debug port. It gets two things it already owns instead: the exit code
//! ([`ServeOutcome::exit_code`](crate::ServeOutcome::exit_code)) and this file, one small JSON object
//! beside the store's database.
//!
//! The file names a store, a count, two durations and a time. No personal data: the outbox's events
//! stay where they are.

use std::path::{Path, PathBuf};

use serde::Serialize;

use pos_proto::ids::StoreId;
use pos_proto::time::Timestamp;

/// The file's name, in the directory that holds the store's database.
pub const DRAIN_STATUS_FILE: &str = "drain-status.json";

/// How the last drain ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DrainStatus {
    /// The store that stopped.
    pub store_id: StoreId,
    /// Events still in the outbox after the drain. `None` when the depth could not be read, which
    /// a reader must treat as not drained: nobody can say the outbox is empty.
    pub outbox_depth: Option<u64>,
    /// How long the drain was allowed, in milliseconds.
    pub budget_ms: u64,
    /// How long it took, in milliseconds.
    pub spent_ms: u64,
    /// When the drain finished.
    pub finish_time: Timestamp,
}

impl DrainStatus {
    /// Whether the outbox is known to be empty.
    #[must_use]
    pub const fn drained(&self) -> bool {
        matches!(self.outbox_depth, Some(0))
    }
}

/// Where the status is written for a store whose database is at `store_path`: the same directory,
/// which on a hosted placement is the store's own volume.
#[must_use]
pub fn path_beside(store_path: &Path) -> PathBuf {
    match store_path.parent() {
        Some(directory) if !directory.as_os_str().is_empty() => directory.join(DRAIN_STATUS_FILE),
        _ => PathBuf::from(DRAIN_STATUS_FILE),
    }
}

/// Overwrites the status at `path`: written beside it and renamed over it, so a reader never sees
/// half a file.
///
/// Blocking file I/O. An async caller runs it under `spawn_blocking`.
///
/// # Errors
///
/// The I/O error if the file cannot be written or renamed into place.
pub fn write(path: &Path, status: &DrainStatus) -> std::io::Result<()> {
    let body = serde_json::to_vec(status).map_err(std::io::Error::other)?;
    let staging = path.with_extension("json.partial");
    std::fs::write(&staging, body)?;
    std::fs::rename(&staging, path)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use pos_proto::ids::StoreId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use super::{DRAIN_STATUS_FILE, DrainStatus, path_beside, write};

    fn status(depth: Option<u64>) -> DrainStatus {
        DrainStatus {
            store_id: StoreId::new(Ulid::from_u128(7)),
            outbox_depth: depth,
            budget_ms: 15_000,
            spent_ms: 1_250,
            finish_time: Timestamp::from_milliseconds_since_epoch(1_790_000_000_000)
                .expect("a valid time"),
        }
    }

    #[test]
    fn only_a_known_empty_outbox_is_drained() {
        assert!(status(Some(0)).drained());
        assert!(!status(Some(3)).drained());
        assert!(
            !status(None).drained(),
            "an unread depth is not an empty one"
        );
    }

    #[test]
    fn the_status_sits_beside_the_database() {
        assert_eq!(
            path_beside(Path::new("/var/lib/pos-edge/store.sqlite")),
            PathBuf::from("/var/lib/pos-edge").join(DRAIN_STATUS_FILE)
        );
        assert_eq!(
            path_beside(Path::new("store.sqlite")),
            PathBuf::from(DRAIN_STATUS_FILE)
        );
    }

    #[test]
    fn the_file_is_one_json_object_a_host_agent_can_read() {
        let directory = tempfile::tempdir().expect("a temp dir");
        let path = directory.path().join(DRAIN_STATUS_FILE);
        write(&path, &status(Some(3))).expect("writes");
        write(&path, &status(Some(0))).expect("overwrites");

        let read: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("reads")).expect("json");
        assert_eq!(read.pointer("/outbox_depth"), Some(&serde_json::json!(0)));
        assert_eq!(read.pointer("/budget_ms"), Some(&serde_json::json!(15_000)));
        assert_eq!(
            read.pointer("/store_id")
                .and_then(serde_json::Value::as_str),
            Some(StoreId::new(Ulid::from_u128(7)).to_string().as_str())
        );
        assert!(read.pointer("/finish_time").is_some());
        assert!(
            !directory.path().join("drain-status.json.partial").exists(),
            "the staging file is renamed away"
        );
    }
}
