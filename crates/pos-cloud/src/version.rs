// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Optimistic concurrency for the console
//! ([ADR-0094](../../../docs/adr/0094-console-optimistic-concurrency.md)).
//!
//! Without it, every master-data edit is last-write-wins: a `PATCH` body carries the whole
//! record, so an admin saving a form they loaded a minute ago writes their stale copy of
//! every *other* field back over whoever edited in between. Both admins see success and the
//! audit trail shows two ordinary updates.
//!
//! The fix is one predicate. A read hands back the record and the version it was read at; a
//! write hands that version back, and applies only if it is still the version stored. The
//! store-postgres adapter spells that as `AND xmin = $n` in the `UPDATE` itself, so the
//! compare and the swap are one statement and nothing can slip between them.
//!
//! # The token is opaque
//!
//! [`Version`] is a string this crate never reads into. It is not a counter, not a
//! timestamp, and not ordered: `Version("847") < Version("91")` is a question with no
//! meaning here, which is why the type offers no way to ask it. Only the adapter that minted
//! a token knows what it is made of, and that is the whole point — it is what lets a fork on
//! another engine mint its own (a `version bigint` bumped in the same statement, a row hash)
//! without a single change above the seam. Above the seam the rule is: echo it back, compare
//! it for equality, never construct one.

use serde::{Deserialize, Serialize};

/// The version a record was read at, as an opaque token.
///
/// Minted by the adapter, echoed by the client, compared only for equality.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(String);

impl Version {
    /// Wraps a token an adapter minted.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token, for putting on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A record together with the version it was read at.
///
/// Serialises as the record's own fields plus an `etag` — additive, so no existing field
/// moves or is renamed, and a list can carry a version per row where a header cannot.
///
/// Serialize only. This shape is something the cloud *writes*; the console reads it in TypeScript,
/// and no Rust caller parses one. Deriving `Deserialize` as well would pull in serde's flatten
/// buffer, whose `Content` enum carries `f32`/`f64` variants that `clippy.toml` bans outright —
/// a real cost (`docs/adr/0013`: money is never a float) for a capability nothing uses.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Versioned<T> {
    /// The record itself, flattened so the wire shape is unchanged but for `etag`.
    #[serde(flatten)]
    pub record: T,
    /// The version [`record`](Self::record) was read at. Byte-identical to the `ETag` header
    /// a single-resource read would carry, so a client never reformats it.
    pub etag: Version,
}

impl<T> Versioned<T> {
    /// Pairs a record with its version.
    #[must_use]
    pub fn new(record: T, etag: Version) -> Self {
        Self { record, etag }
    }
}

/// Drops the per-row versions from a list read, keeping the records in order.
///
/// A `list_*` carries a [`Version`] per row because an editor needs one to write back
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)). Not every caller is an
/// editor: a publish reads a tenant's whole authored set and assembles a config node from it, and a
/// compiler reads items, menus and placements to build a price book. For those the token is noise —
/// there is no row to write back to — so they take the records and leave the versions here.
#[must_use]
pub fn records<T>(rows: Vec<Versioned<T>>) -> Vec<T> {
    rows.into_iter().map(|row| row.record).collect()
}

/// What a conditional write did.
///
/// The three answers are distinct because the caller must do three different things: carry
/// on, re-read and show the reader what changed, or stop because the row is gone. Collapsing
/// the last two — which a bare `bool` forces — would send an admin looking for a conflict
/// that does not exist, or hide a deletion behind a retry that can never succeed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UpdateOutcome {
    /// The write applied. Carries the version it produced, so the caller can chain another
    /// edit without re-reading.
    Updated(Version),
    /// The row exists, at a different version than the caller expected. Answers `412`.
    VersionMismatch,
    /// No such row. Answers `404`.
    NotFound,
}

/// What a create did.
///
/// The counterpart to [`UpdateOutcome`], for the six seams that were one `upsert_*` until
/// [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md) split them. An upsert
/// answers the same way whether it inserted or overwrote, so a caller asking to *add* something
/// could silently replace what was already at that key — a recipe for an item that already had one,
/// a layout button already placed on that channel — and neither the response nor the audit trail
/// said so.
///
/// `AlreadyExists` is a separate variant rather than a store error because a duplicate key is the
/// **caller's** fault and is recoverable by editing instead: it answers `409 ALREADY_EXISTS`, where
/// the error channel these seams share collapses everything to `503 the service is unavailable`.
/// That distinction is not cosmetic — a caller told `503` retries the same losing request, which is
/// exactly the trap [#152] fixed on `set_station`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CreateOutcome {
    /// The row was inserted. Carries the version it was created at, for the `ETag` on the `201`.
    Created(Version),
    /// A row already holds that key. Answers `409`; the caller wants `update_*` instead.
    AlreadyExists,
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::{UpdateOutcome, Version, Versioned};

    #[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
    struct Store {
        store_id: String,
        name: String,
    }

    fn a_store() -> Store {
        Store {
            store_id: "01JAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
            name: "Ben Thanh".to_owned(),
        }
    }

    #[test]
    fn a_versioned_record_adds_etag_and_moves_nothing() {
        // The wire shape is the contract the console already reads. `etag` is additive or it
        // is a breaking change, and a nested `{"record": …}` would be exactly that.
        let json = serde_json::to_string(&Versioned::new(a_store(), Version::new("1847302")))
            .expect("serialise");
        assert_eq!(
            json,
            r#"{"store_id":"01JAAAAAAAAAAAAAAAAAAAAAAA","name":"Ben Thanh","etag":"1847302"}"#
        );
    }

    #[test]
    fn a_versioned_record_still_parses_as_the_record_it_wraps() {
        // The flattening is what makes `etag` additive rather than a reshape: a reader that knows
        // nothing about versions — an older console, a fork's own client — must still read every
        // field it read before, with the unknown one ignored.
        let json = serde_json::to_string(&Versioned::new(a_store(), Version::new("1847302")))
            .expect("serialise");
        let parsed: Store = serde_json::from_str(&json).expect("parse as the bare record");
        assert_eq!(parsed, a_store());
    }

    #[test]
    fn a_token_an_adapter_minted_survives_the_round_trip_byte_for_byte() {
        // A fork's adapter may mint anything: a counter, a hash, a compound. Reformatting one
        // in transit would silently break every conditional write on that fork.
        for token in ["1847302", "0", "sha256:9f86d0818", "v2/7", ""] {
            let json = serde_json::to_string(&Version::new(token)).expect("serialise");
            let parsed: Version = serde_json::from_str(&json).expect("parse");
            assert_eq!(parsed.as_str(), token);
        }
    }

    #[test]
    fn the_three_outcomes_stay_three() {
        // The reason this is not a `bool`: a conflict and an absence need different answers
        // from the caller, and the two are indistinguishable once collapsed.
        assert_ne!(
            UpdateOutcome::VersionMismatch,
            UpdateOutcome::NotFound,
            "a stale write and a deleted row are not the same failure"
        );
        assert_ne!(
            UpdateOutcome::Updated(Version::new("2")),
            UpdateOutcome::Updated(Version::new("3")),
            "the version an update produced is part of what it returned"
        );
    }
}
