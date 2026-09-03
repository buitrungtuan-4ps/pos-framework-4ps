// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The campaign authoring seam (Track M3, [ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
//!
//! Where an operator's promotions live between edits. A campaign is authored per **tenant** — the same
//! promotion runs across a brand's stores — and a publish (a later slice) assembles the tenant's
//! campaigns with [`to_node`] into the `campaigns` config node a store applies to its pricing engine.
//!
//! The authored record *is* the wire [`PublishedCampaign`]: the fields an operator sets are exactly
//! the fields the edge evaluates (plus a display name), so there is no separate cloud-domain shape to
//! keep in sync. The store holds each campaign as its own row keyed by `(tenant, campaign_id)` — CRUD
//! is per-campaign, not the wholesale replace tax rates use, because a tenant edits one promotion at a
//! time and may run many at once. The `store-postgres` impl holds each as `jsonb`, tenant-scoped and
//! RLS-isolated like the rest of the config data.

use core::future::Future;

use pos_proto::campaign::{PublishedCampaign, PublishedCampaigns};
use pos_proto::ids::{CampaignId, TenantId};

use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

/// Persists and reads a tenant's authored campaigns.
///
/// All methods are tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every
/// other cloud table. `create_campaign` inserts one and refuses a taken id; `update_campaign`
/// replaces one only at the version the caller read ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md));
/// `delete_campaign` removes one.
pub trait CampaignStore {
    /// Every campaign a tenant has authored, oldest first (campaign ids are ULIDs, so id order is
    /// creation order — a stable read for a diff).
    ///
    /// Each row carries the version it was read at, because that is the only place an editor can get
    /// the token [`update_campaign`](Self::update_campaign) demands: a campaign is edited from the
    /// list, and a header can carry one version for the whole response but not one per row.
    fn list_campaigns(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<PublishedCampaign>>, CampaignStoreError>> + Send;

    /// One campaign by id and the version it was read at, or `None` if the tenant has none with that id.
    fn get_campaign(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
    ) -> impl Future<Output = Result<Option<Versioned<PublishedCampaign>>, CampaignStoreError>> + Send;

    /// Inserts a campaign, refusing if one already holds its id.
    fn create_campaign(
        &self,
        tenant_id: TenantId,
        campaign: &PublishedCampaign,
    ) -> impl Future<Output = Result<CreateOutcome, CampaignStoreError>> + Send;

    /// Replaces a campaign, only at the version the caller read it at.
    ///
    /// This is the half the old `upsert_campaign` could not express. The update route reads the
    /// campaign, builds the new one, then wrote unconditionally — so a second admin's save between
    /// those two steps was silently overwritten, and a delete between them silently resurrected the
    /// row. The prior `get` proved the campaign existed a moment ago, never that it had not changed.
    fn update_campaign(
        &self,
        tenant_id: TenantId,
        campaign: &PublishedCampaign,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, CampaignStoreError>> + Send;

    /// Removes a campaign by id. Removing one that does not exist is not an error.
    fn delete_campaign(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
    ) -> impl Future<Output = Result<(), CampaignStoreError>> + Send;
}

/// Assembles a tenant's authored campaigns into the wire [`PublishedCampaigns`] node a publish writes.
///
/// A thin wrapper — the authored record is already the wire campaign — kept as a named helper so the
/// publish path reads the same "assemble authored rows into the node" shape every other node uses
/// (e.g. `tax::to_table`).
#[must_use]
pub fn to_node(campaigns: &[PublishedCampaign]) -> PublishedCampaigns {
    PublishedCampaigns::from_campaigns(campaigns.to_vec())
}

/// A failure of the campaign store itself — the database is unreachable, or a stored value could not
/// be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the campaign store failed: {0}")]
pub struct CampaignStoreError(String);

impl CampaignStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::campaign::{PublishedAction, PublishedCampaign, PublishedCampaignKind};
    use pos_proto::ids::{CampaignId, TenantId};
    use pos_proto::money::Ratio;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    use super::{CampaignStore, CampaignStoreError, to_node};
    use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

    /// An in-memory `CampaignStore` for the seam's tests. Tenant-scoped exactly like the real thing;
    /// an upsert touches one campaign for one tenant and leaves other tenants alone, so a test can
    /// prove isolation.
    #[derive(Default)]
    struct FakeCampaigns {
        rows: Mutex<Vec<(TenantId, PublishedCampaign, Version)>>,
        next_version: Mutex<u64>,
    }

    impl FakeCampaigns {
        /// The fake's stand-in for `xmin` (ADR-0094): a token that changes on every successful
        /// write, which is the only property the seam contract needs.
        fn mint(&self) -> Version {
            let mut next = self.next_version.lock().expect("lock");
            *next += 1;
            Version::new(next.to_string())
        }
    }

    impl CampaignStore for FakeCampaigns {
        async fn list_campaigns(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<PublishedCampaign>>, CampaignStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _campaign, _at)| *owner == tenant_id)
                .map(|(_owner, campaign, at)| Versioned::new(campaign.clone(), at.clone()))
                .collect())
        }

        async fn get_campaign(
            &self,
            tenant_id: TenantId,
            campaign_id: CampaignId,
        ) -> Result<Option<Versioned<PublishedCampaign>>, CampaignStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|(owner, campaign, _at)| *owner == tenant_id && campaign.id == campaign_id)
                .map(|(_owner, campaign, at)| Versioned::new(campaign.clone(), at.clone())))
        }

        async fn create_campaign(
            &self,
            tenant_id: TenantId,
            campaign: &PublishedCampaign,
        ) -> Result<CreateOutcome, CampaignStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            if rows
                .iter()
                .any(|(owner, existing, _at)| *owner == tenant_id && existing.id == campaign.id)
            {
                return Ok(CreateOutcome::AlreadyExists);
            }
            let version = self.mint();
            rows.push((tenant_id, campaign.clone(), version.clone()));
            Ok(CreateOutcome::Created(version))
        }

        async fn update_campaign(
            &self,
            tenant_id: TenantId,
            campaign: &PublishedCampaign,
            expected: &Version,
        ) -> Result<UpdateOutcome, CampaignStoreError> {
            let version = self.mint();
            let mut rows = self.rows.lock().expect("lock");
            let Some(row) = rows
                .iter_mut()
                .find(|(owner, existing, _at)| *owner == tenant_id && existing.id == campaign.id)
            else {
                return Ok(UpdateOutcome::NotFound);
            };
            if &row.2 != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            row.1 = campaign.clone();
            row.2 = version.clone();
            Ok(UpdateOutcome::Updated(version))
        }

        async fn delete_campaign(
            &self,
            tenant_id: TenantId,
            campaign_id: CampaignId,
        ) -> Result<(), CampaignStoreError> {
            self.rows
                .lock()
                .expect("lock")
                .retain(|(owner, campaign, _at)| {
                    !(*owner == tenant_id && campaign.id == campaign_id)
                });
            Ok(())
        }
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn other_tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(2))
    }

    fn campaign(n: u128, name: &str) -> PublishedCampaign {
        PublishedCampaign {
            id: CampaignId::new(Ulid::from_u128(n)),
            name: DisplayName::new(name),
            kind: PublishedCampaignKind::BillLevel,
            priority: 0,
            exclusion_group: None,
            action: PublishedAction::Percentage {
                rate: Ratio::percent(10).expect("percent"),
            },
            conditions: pos_proto::campaign::PublishedConditions::default(),
            quota_remaining: None,
        }
    }

    #[tokio::test]
    async fn create_refuses_a_taken_id_and_update_needs_the_version_read() {
        let store = FakeCampaigns::default();

        // A neighbour's campaign must survive our writes.
        store
            .create_campaign(other_tenant(), &campaign(99, "Neighbour"))
            .await
            .expect("neighbour");

        let first = match store
            .create_campaign(tenant(), &campaign(10, "Lunch"))
            .await
            .expect("create")
        {
            CreateOutcome::Created(version) => version,
            CreateOutcome::AlreadyExists => panic!("the id was free"),
        };
        store
            .create_campaign(tenant(), &campaign(11, "Dinner"))
            .await
            .expect("create 2");
        assert_eq!(store.list_campaigns(tenant()).await.expect("list").len(), 2);

        // A second create at a taken id is refused, and changes nothing. Before ADR-0095 split the
        // seam this was an upsert, so the rename below arrived through the *create* path and
        // silently replaced the row — the case this assertion now forbids.
        assert_eq!(
            store
                .create_campaign(tenant(), &campaign(10, "Lunch (renamed)"))
                .await
                .expect("the comparison must not raise"),
            CreateOutcome::AlreadyExists
        );
        let fetched = store
            .get_campaign(tenant(), CampaignId::new(Ulid::from_u128(10)))
            .await
            .expect("get")
            .expect("present");
        assert_eq!(
            fetched.record.name,
            DisplayName::new("Lunch"),
            "a refused create leaves the row it refused to overwrite alone"
        );
        // A read carries the version, and it is the one the create minted. Without this an editor
        // that loaded the list has no token to send back, so `update_campaign` would be unreachable
        // from anywhere but the response to a create it made itself.
        assert_eq!(
            fetched.etag, first,
            "a read hands back the version the row is at"
        );
        let listed = store.list_campaigns(tenant()).await.expect("list");
        assert_eq!(
            listed
                .iter()
                .find(|row| row.record.id == CampaignId::new(Ulid::from_u128(10)))
                .map(|row| row.etag.clone()),
            Some(first.clone()),
            "and so does every row of a list, which is where the console edits from"
        );

        // The rename goes through update, at the version the create minted.
        let renamed = match store
            .update_campaign(tenant(), &campaign(10, "Lunch (renamed)"), &first)
            .await
            .expect("the update")
        {
            UpdateOutcome::Updated(version) => version,
            other => panic!("expected the update to apply, got {other:?}"),
        };
        assert_ne!(renamed, first, "the version moves on every write");
        let listed = store.list_campaigns(tenant()).await.expect("list again");
        assert_eq!(listed.len(), 2, "an update does not add a row");
        let fetched = store
            .get_campaign(tenant(), CampaignId::new(Ulid::from_u128(10)))
            .await
            .expect("get")
            .expect("present");
        assert_eq!(fetched.record.name, DisplayName::new("Lunch (renamed)"));
        assert_eq!(
            fetched.etag, renamed,
            "and the version a read carries moves with the write"
        );

        // And the version the caller already used is now stale.
        assert_eq!(
            store
                .update_campaign(tenant(), &campaign(10, "Lunch (again)"), &first)
                .await
                .expect("the comparison must not raise"),
            UpdateOutcome::VersionMismatch
        );

        store
            .delete_campaign(tenant(), CampaignId::new(Ulid::from_u128(10)))
            .await
            .expect("delete");
        assert_eq!(store.list_campaigns(tenant()).await.expect("list").len(), 1);

        // The neighbour is untouched throughout.
        assert_eq!(
            store
                .list_campaigns(other_tenant())
                .await
                .expect("list")
                .len(),
            1
        );
    }

    #[test]
    fn to_node_wraps_the_authored_campaigns() {
        let node = to_node(&[campaign(1, "A"), campaign(2, "B")]);
        assert_eq!(node.len(), 2);
    }
}
