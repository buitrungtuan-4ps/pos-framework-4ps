// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Discovery: advertising the edge on the store LAN (ADR-0030).
//!
//! mDNS (`pos.local`) is a **convenience**, not a foundation: it does not resolve on Android Chrome
//! and dies with one access point, so the always-works discovery path is the raw-IP pairing URL
//! ([`crate::pairing`]), which needs no advertiser at all. mDNS therefore lives behind this trait,
//! and its real multicast implementation lands with hardware bring-up (roadmap A5) — exactly as the
//! printer's real transports do — because multicast cannot be exercised in CI.

/// Advertises the edge's presence so a device can find it by name.
///
/// Discovery failing must never stop the store selling, so the method cannot fail: an advertiser
/// that cannot start simply advertises nothing, and the raw-IP path still works.
pub trait Advertiser: Send + Sync + core::fmt::Debug {
    /// Begin advertising an instance named `instance` on `port`.
    fn advertise(&self, instance: &str, port: u16);
}

/// The default advertiser: advertises nothing.
///
/// The framework ships this and the raw-IP fallback; the real mDNS advertiser arrives with hardware.
/// Choosing it is not a degraded mode — the pairing URL never depended on mDNS.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopAdvertiser;

impl Advertiser for NoopAdvertiser {
    fn advertise(&self, _instance: &str, _port: u16) {
        // Nothing to do: discovery is the raw-IP URL in this build.
    }
}
