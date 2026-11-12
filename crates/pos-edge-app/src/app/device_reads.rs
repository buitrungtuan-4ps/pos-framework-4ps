// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which package version each paired till last read (roadmap-v3 B7.3, per-device drift).
//!
//! A till reads its package on `GET /api/package` at boot and again on every `config_applied`. A till
//! that missed one keeps drawing the version it read before, while the store runs a newer one. The
//! store's own heartbeat says which version it runs; this record says which one each till last read
//! and when, so the cloud can show the tills that are behind.
//!
//! It holds at most [`DEVICE_READS_MAX`] tills, one entry each, the oldest read dropped first, and it
//! is held in memory only: after a restart every till reads again on its next boot or
//! `config_applied`, and a till that does not is reported again once it does. A device id names a
//! paired device, never a person, so the record holds nothing personal.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use pos_proto::ids::DeviceId;

/// The most tills one record holds: more than a store pairs, so only a store past it loses the
/// oldest read.
pub const DEVICE_READS_MAX: usize = 64;

/// What one till last read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRead {
    /// The till.
    pub device_id: DeviceId,
    /// The package version it was served, or `None` for its release's default package.
    pub package_version_id: Option<String>,
    /// When it read it, in Unix milliseconds.
    pub read_at_ms: i64,
}

/// The last read of each paired till.
#[derive(Debug, Default)]
pub struct DeviceReads {
    reads: Mutex<BTreeMap<DeviceId, (Option<String>, i64)>>,
}

impl DeviceReads {
    /// Records that `device_id` read `package_version_id` at `read_at_ms`, replacing its earlier
    /// read. When the record is full and the till is new to it, the till whose read is oldest is
    /// dropped first.
    pub fn record(&self, device_id: DeviceId, package_version_id: Option<String>, read_at_ms: i64) {
        let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
        if !reads.contains_key(&device_id) && reads.len() >= DEVICE_READS_MAX {
            let oldest = reads
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(device, _)| *device);
            if let Some(oldest) = oldest {
                reads.remove(&oldest);
            }
        }
        reads.insert(device_id, (package_version_id, read_at_ms));
    }

    /// Every till's last read, by device id.
    #[must_use]
    pub fn snapshot(&self) -> Vec<DeviceRead> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(device_id, (package_version_id, read_at_ms))| DeviceRead {
                device_id: *device_id,
                package_version_id: package_version_id.clone(),
                read_at_ms: *read_at_ms,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::DeviceId;
    use pos_proto::ulid::Ulid;

    use super::{DEVICE_READS_MAX, DeviceRead, DeviceReads};

    fn device(n: u128) -> DeviceId {
        DeviceId::new(Ulid::from_u128(n))
    }

    #[test]
    fn a_till_keeps_only_its_latest_read() {
        let reads = DeviceReads::default();
        reads.record(device(1), Some("v1".to_owned()), 10);
        reads.record(device(2), None, 11);
        reads.record(device(1), Some("v2".to_owned()), 12);
        assert_eq!(
            reads.snapshot(),
            vec![
                DeviceRead {
                    device_id: device(1),
                    package_version_id: Some("v2".to_owned()),
                    read_at_ms: 12,
                },
                DeviceRead {
                    device_id: device(2),
                    package_version_id: None,
                    read_at_ms: 11,
                },
            ]
        );
    }

    #[test]
    fn a_full_record_drops_the_oldest_read_for_a_new_till() {
        let reads = DeviceReads::default();
        let count = u128::try_from(DEVICE_READS_MAX).expect("fits");
        for n in 0..count {
            // Device 0 reads last, so device 1's read is the oldest.
            let at = if n == 0 {
                1_000
            } else {
                i64::try_from(n).expect("fits")
            };
            reads.record(device(n), None, at);
        }
        reads.record(device(count), None, 2_000);
        let held = reads.snapshot();
        assert_eq!(held.len(), DEVICE_READS_MAX);
        assert!(held.iter().all(|read| read.device_id != device(1)));
        assert!(held.iter().any(|read| read.device_id == device(0)));
        assert!(held.iter().any(|read| read.device_id == device(count)));
        // A till already held replaces its own read and drops nobody.
        reads.record(device(0), None, 3_000);
        assert_eq!(reads.snapshot().len(), DEVICE_READS_MAX);
    }
}
