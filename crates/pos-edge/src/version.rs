// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What version of itself this binary reports (roadmap v3 slice **R1b**).
//!
//! # The problem this closes
//!
//! `crates/pos-edge/Cargo.toml` is `version = "0.0.0"` and nothing wrote the release tag at build
//! time, so **every artifact reported the same version**. The OTA progress model
//! ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)) is built on each store telling the
//! cloud what it is running; with one constant answer the console could not tell one release from
//! another, a rollout's progress bar was meaningless, and `decide_rollout`'s "already current" gate
//! never fired.
//!
//! # Where the value comes from
//!
//! `option_env!("POS_EDGE_RELEASE_VERSION")`, set by the release workflow from the tag it already
//! validates, falling back to `CARGO_PKG_VERSION`. Three properties come out of that choice:
//!
//! * **The tag is the single source of truth.** The workflow refuses a tag that is not `vX.Y.Z` and
//!   builds from that tag, so the stamp and the artifact name cannot disagree. Bumping the manifest
//!   per release was the alternative and it can drift — the tag says `v1.2.0`, the committed
//!   manifest still says `1.1.0`, and nothing notices.
//! * **A hand-built binary says `0.0.0`, which is true.** It is not a release. That is more honest
//!   than a build script guessing from `git describe`, which also needs `.git` in the build context
//!   — and the Docker build does not have it.
//! * **No build script and no new dependency.**
//!
//! # The `v` is stripped, and that is load-bearing
//!
//! The tag is `v1.2.0`; this reports `1.2.0`. Not cosmetic:
//! [`ReleaseVersion::parse`](pos_core::ota::ReleaseVersion::parse) splits on `.` and parses each
//! component as a `u16`, so a leading `v` makes it return `None` — and the cloud publishes
//! `target_version` in that same bare form. A stamped `v1.2.0` would report a value nothing could
//! compare, which is the "written but never wired" failure this slice exists to fix, one level down.
//! [`released`] is what enforces it, and [`version_parses`] is the test that fails the build rather
//! than the fleet if a fork changes the workflow's expression.

use pos_core::ota::ReleaseVersion;
use pos_proto::text::ReleaseTag;

/// This binary's version, as the release tag with its `v` removed — `1.2.0`, never `v1.2.0`.
///
/// `0.0.0` when the binary was not built by the release workflow, which is the honest answer for a
/// developer build: it is not a release, and `0.0.0` sorts below every published version.
pub const VERSION: &str = match option_env!("POS_EDGE_RELEASE_VERSION") {
    Some(stamped) => stamped,
    None => env!("CARGO_PKG_VERSION"),
};

/// This binary's version as the [`ReleaseTag`] the cloud is told about
/// ([`CloudSync::report`](pos_ports::CloudSync::report), ADR-0078).
#[must_use]
pub fn tag() -> ReleaseTag {
    ReleaseTag::new(VERSION)
}

/// This binary's version as the [`ReleaseVersion`] a rollout decision compares
/// ([`decide_rollout`](pos_core::ota::decide_rollout)).
///
/// `None` only if [`VERSION`] is not `X.Y.Z`, which [`version_parses`] makes a build failure rather
/// than a field failure. Callers still handle `None` rather than unwrapping: the edge refusing to
/// weigh an update is recoverable, and a panic on a till is not.
#[must_use]
pub fn released() -> Option<ReleaseVersion> {
    ReleaseVersion::parse(VERSION)
}

/// The target triple this binary was compiled for, stamped by `build.rs` from Cargo's own `TARGET`.
///
/// What the OTA artifact fetch sends as `arch` ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)
/// Correction 2): R1's workflow cross-compiles two targets, so a request without one cannot say which
/// binary it means, and guessing hands an `aarch64` box an `x86_64` executable that fails its
/// self-test *after* the install.
///
/// Not composed from `std::env::consts` — see `build.rs` for why that is silently wrong for a musl
/// fork.
#[must_use]
pub const fn target() -> &'static str {
    env!("POS_EDGE_TARGET")
}

#[cfg(test)]
mod tests {
    use super::{VERSION, released, tag};

    /// The invariant the whole slice rests on: whatever the release workflow stamped, the edge can
    /// still turn it into a version a rollout can compare.
    ///
    /// This runs against the *actual* compiled-in value, so a fork that changes the workflow's
    /// expression — leaving the `v` on, appending a build number, using a four-part version — fails
    /// here instead of shipping a fleet that silently never updates.
    #[test]
    fn version_parses() {
        assert!(
            released().is_some(),
            "POS_EDGE_RELEASE_VERSION must be X.Y.Z with no leading `v` — the cloud publishes \
             target_version in that form and ReleaseVersion::parse rejects anything else. Got {VERSION:?}"
        );
    }

    #[test]
    fn the_tag_and_the_version_agree() {
        // Two accessors, one value: a mismatch would mean the console showed one version and the
        // rollout weighed another.
        assert_eq!(tag().as_str(), VERSION);
        let parsed = released().expect("parses");
        assert_eq!(
            format!("{}.{}.{}", parsed.major, parsed.minor, parsed.patch),
            VERSION,
            "the parse round-trips, so nothing is being silently truncated"
        );
    }

    #[test]
    fn an_unstamped_build_reports_the_manifest_version() {
        // Documents what a developer build says, and pins that it is parseable too — `0.0.0` sorts
        // below every published release, so such a box is eligible for any update rather than
        // wrongly believing it is current.
        if option_env!("POS_EDGE_RELEASE_VERSION").is_none() {
            assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
            let parsed = released().expect("the manifest version parses");
            assert_eq!(parsed, pos_core::ota::ReleaseVersion::new(0, 0, 0));
        }
    }
}
