// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reason-code authoring seam
//! ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md), roadmap **B2.2** slice 2).
//!
//! Where a tenant's managed reason codes live between edits. `docs/pos-spec.md` §11 item 2 makes
//! reasons from a cloud-managed list one of the six fraud controls, and eleven event fields declare
//! a `reason_code_id` that comes from it; ADR-0115 decided the shape, and this is where the cloud
//! keeps the entries an operator authors before a publish assembles them into the `reason_codes`
//! node.
//!
//! Authored per **tenant**, like inventory and campaigns: "waste", "staff error" and "customer
//! changed their mind" mean the same thing in every store a brand runs, and a per-store list would
//! make the per-employee void-rate comparison §11 item 3 asks for meaningless across stores.
//!
//! The authored record *is* the wire type ([`PublishedReasonCode`]) — the fields an operator sets
//! are exactly the fields the edge reads — so there is no separate cloud-domain shape to keep in
//! sync. One row per entry keyed by `(tenant, id)`; CRUD is per-record rather than the wholesale
//! replace tax rates use, because an operator edits one reason at a time.
//!
//! # Retiring, not deleting
//!
//! [`ReasonCodeStore::delete`] exists for the entry created by mistake and never cited. The way an
//! operator takes a reason out of service is [`PublishedReasonCode::active`], because a historic
//! `sales.order_line.voided` still names the id: a deleted row makes that event unresolvable, and a
//! fraud control whose audit trail decays into unreadable ULIDs has stopped being one. Slice 3's
//! console leads with retire and puts delete behind a typed-name confirmation for exactly that
//! reason.

use core::future::Future;

use pos_proto::ids::{ReasonCodeId, TenantId};
use pos_proto::reason_codes::{PublishedReasonCode, PublishedReasonCodes};

use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

/// Persists and reads a tenant's authored reason codes.
///
/// Every method is tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every
/// other cloud table. [`create`](Self::create) inserts one and refuses a taken id;
/// [`update`](Self::update) replaces one only at the version the caller read it at
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md));
/// [`delete`](Self::delete) removes one, and removing an absent record is not an error.
pub trait ReasonCodeStore {
    /// Every reason code a tenant has authored, id order (a ULID, so creation order — stable for a
    /// diff), including retired ones.
    ///
    /// Retired entries are in the list because the console has to show them: an operator needs to
    /// see what is out of service in order to bring it back, and a publish needs them too, so that
    /// a till which still holds a stale node resolves the same ids the audit trail names.
    ///
    /// Each row carries the version it was read at: the console edits an entry from this list, and
    /// that token is what [`update`](Self::update) demands back.
    fn list(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<PublishedReasonCode>>, ReasonCodeStoreError>> + Send;

    /// One reason code by id and the version it was read at, or `None` if the tenant has none with
    /// that id. Answered from the table's primary key rather than by scanning [`list`](Self::list).
    fn get(
        &self,
        tenant_id: TenantId,
        reason_code_id: ReasonCodeId,
    ) -> impl Future<Output = Result<Option<Versioned<PublishedReasonCode>>, ReasonCodeStoreError>> + Send;

    /// Inserts a reason code, refusing if one already holds its id.
    fn create(
        &self,
        tenant_id: TenantId,
        reason_code: &PublishedReasonCode,
    ) -> impl Future<Output = Result<CreateOutcome, ReasonCodeStoreError>> + Send;

    /// Replaces a reason code, only at the version the caller read it at.
    ///
    /// This is also how an entry is retired and restored: `active` is a field on the record, not a
    /// separate lifecycle route, so retiring goes through the same optimistic-concurrency check as
    /// any other edit and two managers cannot silently undo one another.
    fn update(
        &self,
        tenant_id: TenantId,
        reason_code: &PublishedReasonCode,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, ReasonCodeStoreError>> + Send;

    /// Removes a reason code by id. Removing one that does not exist is not an error.
    ///
    /// For the entry created by mistake. Anything an event may already cite is *retired* instead —
    /// see the module documentation.
    fn delete(
        &self,
        tenant_id: TenantId,
        reason_code_id: ReasonCodeId,
    ) -> impl Future<Output = Result<(), ReasonCodeStoreError>> + Send;
}

/// Assembles a tenant's authored reason codes into the wire [`PublishedReasonCodes`] node a publish
/// writes — the same "assemble authored rows into the node" shape every other node uses (e.g.
/// `inventory::to_node`).
///
/// Note what this does **not** do: it does not fold in
/// [`PublishedReasonCodes::framework_default`]. ADR-0115 decided that a published node **replaces**
/// the framework set wholesale rather than merging with it, so that an operator can remove a
/// framework reason they judge wrong for their business. Merging here would quietly resurrect every
/// one of them.
#[must_use]
pub fn to_node(codes: Vec<PublishedReasonCode>) -> PublishedReasonCodes {
    PublishedReasonCodes::from_parts(codes)
}

/// A failure of the reason-code store itself — the database is unreachable, or a stored value could
/// not be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the reason code store failed: {0}")]
pub struct ReasonCodeStoreError(String);

impl ReasonCodeStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future;
    use std::sync::Mutex;

    use pos_proto::ids::{ReasonCodeId, TenantId};
    use pos_proto::reason_codes::{PublishedReasonCode, ReasonAction, ReasonCode};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    use super::{ReasonCodeStore, ReasonCodeStoreError, to_node};
    use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

    /// An in-memory `ReasonCodeStore`, standing in for the `store-postgres` one.
    ///
    /// The point of the fake is not to be a database: it is to let these cases pin the *seam's*
    /// contract — what a create on a taken id answers, what an update at a stale version answers,
    /// what a delete of an absent row answers, and that one tenant never sees another's list — so
    /// that the real adapter has a written specification to satisfy on real Postgres in CI.
    #[derive(Default)]
    struct FakeReasonCodes {
        rows: Mutex<Vec<(TenantId, PublishedReasonCode, Version)>>,
    }

    impl ReasonCodeStore for FakeReasonCodes {
        async fn list(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<PublishedReasonCode>>, ReasonCodeStoreError> {
            let rows = self.rows.lock().expect("lock");
            let mut mine: Vec<(PublishedReasonCode, Version)> = rows
                .iter()
                .filter(|(owner, _, _)| *owner == tenant_id)
                .map(|(_, code, at)| (code.clone(), at.clone()))
                .collect();
            mine.sort_by_key(|(code, _)| code.id.as_ulid().to_u128());
            Ok(mine
                .into_iter()
                .map(|(code, at)| Versioned::new(code, at))
                .collect())
        }

        async fn get(
            &self,
            tenant_id: TenantId,
            reason_code_id: ReasonCodeId,
        ) -> Result<Option<Versioned<PublishedReasonCode>>, ReasonCodeStoreError> {
            let rows = self.rows.lock().expect("lock");
            Ok(rows
                .iter()
                .find(|(owner, code, _)| *owner == tenant_id && code.id == reason_code_id)
                .map(|(_, code, at)| Versioned::new(code.clone(), at.clone())))
        }

        async fn create(
            &self,
            tenant_id: TenantId,
            reason_code: &PublishedReasonCode,
        ) -> Result<CreateOutcome, ReasonCodeStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if rows
                .iter()
                .any(|(owner, code, _)| *owner == tenant_id && code.id == reason_code.id)
            {
                return Ok(CreateOutcome::AlreadyExists);
            }
            let version = Version::new("1");
            rows.push((tenant_id, reason_code.clone(), version.clone()));
            Ok(CreateOutcome::Created(version))
        }

        async fn update(
            &self,
            tenant_id: TenantId,
            reason_code: &PublishedReasonCode,
            expected: &Version,
        ) -> Result<UpdateOutcome, ReasonCodeStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            let Some(row) = rows
                .iter_mut()
                .find(|(owner, code, _)| *owner == tenant_id && code.id == reason_code.id)
            else {
                return Ok(UpdateOutcome::NotFound);
            };
            if row.2 != *expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            let version = Version::new("2");
            row.1 = reason_code.clone();
            row.2 = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn delete(
            &self,
            tenant_id: TenantId,
            reason_code_id: ReasonCodeId,
        ) -> Result<(), ReasonCodeStoreError> {
            self.rows
                .lock()
                .expect("lock")
                .retain(|(owner, code, _)| !(*owner == tenant_id && code.id == reason_code_id));
            Ok(())
        }
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn other_tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(2))
    }

    fn waste(n: u128) -> PublishedReasonCode {
        PublishedReasonCode::new(
            ReasonCodeId::new(Ulid::from_u128(n)),
            ReasonCode::new("WASTE"),
            DisplayName::new("Waste or spoilage"),
            vec![ReasonAction::VoidLine, ReasonAction::StockWaste],
        )
    }

    fn run<T>(future: impl Future<Output = T>) -> T {
        pos_fakes::executor::run_ready(future)
    }

    #[test]
    fn a_created_code_reads_back_at_the_version_it_was_created_at() {
        run(async {
            let store = FakeReasonCodes::default();
            let code = waste(10);

            let CreateOutcome::Created(version) =
                store.create(tenant(), &code).await.expect("create")
            else {
                panic!("a fresh id is created, not refused");
            };

            let read = store
                .get(tenant(), code.id)
                .await
                .expect("get")
                .expect("the code is there");
            assert_eq!(read.record, code);
            assert_eq!(
                read.etag, version,
                "the read's version is the one the create returned, so a console can edit \
                 straight from a create without re-reading"
            );
        });
    }

    #[test]
    fn a_taken_id_is_refused_rather_than_overwriting() {
        run(async {
            let store = FakeReasonCodes::default();
            store.create(tenant(), &waste(10)).await.expect("create");

            let mut clash = waste(10);
            clash.display_name = DisplayName::new("Something else entirely");
            let outcome = store.create(tenant(), &clash).await.expect("create");

            assert!(
                matches!(outcome, CreateOutcome::AlreadyExists),
                "a create must never silently replace an entry historic events cite"
            );
            let kept = store
                .get(tenant(), clash.id)
                .await
                .expect("get")
                .expect("still there");
            assert_eq!(
                kept.record.display_name.as_str(),
                "Waste or spoilage",
                "and the original is untouched"
            );
        });
    }

    #[test]
    fn an_update_at_a_stale_version_is_refused() {
        run(async {
            let store = FakeReasonCodes::default();
            let code = waste(10);
            let CreateOutcome::Created(first) =
                store.create(tenant(), &code).await.expect("create")
            else {
                panic!("created");
            };

            // One manager retires it.
            let retired = code.clone().retired();
            store
                .update(tenant(), &retired, &first)
                .await
                .expect("the first edit lands");

            // A second manager, holding the version from before that edit, renames it.
            let mut renamed = code.clone();
            renamed.display_name = DisplayName::new("Wastage");
            let outcome = store
                .update(tenant(), &renamed, &first)
                .await
                .expect("update");

            assert!(
                matches!(outcome, UpdateOutcome::VersionMismatch),
                "ADR-0095: the second edit is refused rather than silently un-retiring the entry"
            );
            let kept = store
                .get(tenant(), code.id)
                .await
                .expect("get")
                .expect("still there");
            assert!(!kept.record.active, "and it is still retired");
        });
    }

    #[test]
    fn updating_a_code_that_is_not_there_is_not_found() {
        run(async {
            let store = FakeReasonCodes::default();
            let outcome = store
                .update(tenant(), &waste(10), &Version::new("1"))
                .await
                .expect("update");
            assert!(matches!(outcome, UpdateOutcome::NotFound));
        });
    }

    #[test]
    fn retiring_keeps_the_entry_resolvable_while_taking_it_off_the_picker() {
        run(async {
            // The distinction the module documentation turns on: a historic
            // `sales.order_line.voided` names the id forever, so an entry leaves service by going
            // inactive, not by leaving the table.
            let store = FakeReasonCodes::default();
            let code = waste(10);
            let CreateOutcome::Created(at) = store.create(tenant(), &code).await.expect("create")
            else {
                panic!("created");
            };
            store
                .update(tenant(), &code.clone().retired(), &at)
                .await
                .expect("retire");

            let listed = store.list(tenant()).await.expect("list");
            assert_eq!(listed.len(), 1, "a retired entry is still in the list");
            assert!(!listed[0].record.active);

            let node = to_node(listed.into_iter().map(|row| row.record).collect());
            assert!(
                node.find(code.id).is_some(),
                "and the published node still resolves it, so a till reading an old event's \
                 reason gets a name rather than a bare ULID"
            );
            assert_eq!(
                node.for_action(ReasonAction::VoidLine).count(),
                0,
                "while offering it to nobody"
            );
        });
    }

    #[test]
    fn deleting_is_idempotent_and_scoped_to_one_tenant() {
        run(async {
            let store = FakeReasonCodes::default();
            store.create(tenant(), &waste(10)).await.expect("create");
            store
                .create(other_tenant(), &waste(10))
                .await
                .expect("the other tenant may hold the same id");

            store.delete(tenant(), waste(10).id).await.expect("delete");
            store
                .delete(tenant(), waste(10).id)
                .await
                .expect("deleting an absent row is not an error");

            assert!(store.list(tenant()).await.expect("list").is_empty());
            assert_eq!(
                store.list(other_tenant()).await.expect("list").len(),
                1,
                "one tenant's delete must not reach another's list"
            );
        });
    }

    #[test]
    fn the_node_a_publish_writes_is_the_authored_list_and_not_the_framework_set() {
        run(async {
            // ADR-0115: a published node REPLACES the framework default rather than merging with
            // it, so that an operator can remove a framework reason they judge wrong. Folding the
            // default in here would quietly resurrect all eight.
            let store = FakeReasonCodes::default();
            store.create(tenant(), &waste(10)).await.expect("create");

            let node = to_node(
                store
                    .list(tenant())
                    .await
                    .expect("list")
                    .into_iter()
                    .map(|row| row.record)
                    .collect(),
            );

            assert_eq!(node.codes().len(), 1, "only what the tenant authored");
            assert_eq!(
                node.for_action(ReasonAction::DrawerOpen).count(),
                0,
                "the framework's MAKING_CHANGE is not folded in behind the operator's back"
            );
        });
    }
}
