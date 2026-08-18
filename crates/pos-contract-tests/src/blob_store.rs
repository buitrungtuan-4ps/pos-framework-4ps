// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `BlobStore` suite.
//!
//! Short, because the port is deliberately thin and scheduled for deletion once WAL shipping is
//! in-house ([ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md)). The one case worth
//! more than its length is [`lists_within_a_prefix_only`]: `stores/1` must not return `stores/10`,
//! and in a multi-tenant system a prefix that leaks into a sibling is the worst failure available.

use pos_ports::PortName;
use pos_ports::blob_store::{BlobKey, BlobStore};

use crate::harness::BlobStoreHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `BlobStore` case as a `#[test]`.
#[macro_export]
macro_rules! blob_store_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_BLOB_STORE,
            module = blob_store,
            cases = [
                round_trips_an_object,
                reports_an_absent_object_as_none,
                overwrites_on_a_second_put,
                deletes_idempotently,
                lists_within_a_prefix_only,
            ]
        }
    };
}

fn obligation() -> Obligation {
    Obligation::new(PortName::BlobStore, "whole objects, prefix-scoped listing")
}

/// A key, or a failure naming the bad literal.
fn key(text: &str) -> Result<BlobKey, CaseFailure> {
    BlobKey::parse(text).map_err(|error| CaseFailure::new(format!("fixture key `{text}`: {error}")))
}

/// What goes in comes out.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn round_trips_an_object<H: BlobStoreHarness>(harness: &H) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let key = key("stores/1/backup.sqlite")?;
    // Not a text payload: a backup is bytes, and an adapter that assumes UTF-8 corrupts one
    // silently rather than failing.
    let body: Vec<u8> = (0..=255_u8).collect();
    store.put(&key, &body).await?;
    let read = store.get(&key).await?;
    obligation().require_eq(
        &read.as_deref(),
        &Some(body.as_slice()),
        "the object round-trips",
    )
}

/// A missing object is a fact, not an exception.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_an_absent_object_as_none<H: BlobStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let read = store.get(&key("stores/1/never-written")?).await?;
    obligation().require(
        read.is_none(),
        "an absent object is Ok(None). The restore drill in docs/roadmap.md P8 asks whether a \
         backup exists, and it should not have to handle an error to find out",
    )
}

/// The second write wins.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn overwrites_on_a_second_put<H: BlobStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let key = key("releases/v1.0.0/pos_edge")?;
    store.put(&key, b"first").await?;
    store.put(&key, b"second").await?;
    let read = store.get(&key).await?;
    obligation().require_eq(
        &read.as_deref(),
        &Some(b"second".as_slice()),
        "a repeated put replaces rather than erroring — a backup job re-running is not a conflict",
    )
}

/// Deleting twice is fine, because cleanup runs more than once.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn deletes_idempotently<H: BlobStoreHarness>(harness: &H) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let key = key("stores/1/old-backup")?;
    store.put(&key, b"body").await?;
    store.delete(&key).await?;
    store.delete(&key).await?;
    let read = store.get(&key).await?;
    obligation().require(
        read.is_none(),
        "a deleted object is gone, and deleting it again is fine",
    )
}

/// A prefix stops at a segment boundary.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn lists_within_a_prefix_only<H: BlobStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    for path in [
        "stores/1/a",
        "stores/1/b",
        "stores/10/c",
        "stores/1x/d",
        "other/e",
    ] {
        store.put(&key(path)?, b"body").await?;
    }

    let listed = store.list(&key("stores/1")?).await?;
    let obligation = obligation();
    let mut names: Vec<&str> = listed.iter().map(BlobKey::as_str).collect();
    names.sort_unstable();
    obligation.require_eq(
        &names.as_slice(),
        &["stores/1/a", "stores/1/b"].as_slice(),
        "listing `stores/1` returns its own objects and nothing else. `stores/10` and `stores/1x` \
         both start with the same characters, and a plain starts_with would return one tenant's \
         backups under another's prefix",
    )?;
    // Owned strings rather than borrows: `require_ascending`'s key must outlive the borrow of the
    // item it came from, and a listing is small.
    obligation.require_ascending(&listed, ToString::to_string, "the listing")
}
