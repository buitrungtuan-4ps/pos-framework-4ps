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

/// Persists and reads a tenant's authored campaigns.
///
/// All four methods are tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every
/// other cloud table. `upsert_campaign` creates or replaces one campaign by its id; `delete_campaign`
/// removes one.
pub trait CampaignStore {
    /// Every campaign a tenant has authored, oldest first (campaign ids are ULIDs, so id order is
    /// creation order — a stable read for a diff).
    fn list_campaigns(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<PublishedCampaign>, CampaignStoreError>> + Send;

    /// One campaign by id, or `None` if the tenant has none with that id.
    fn get_campaign(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
    ) -> impl Future<Output = Result<Option<PublishedCampaign>, CampaignStoreError>> + Send;

    /// Creates a campaign, or replaces the one that already has its id.
    fn upsert_campaign(
        &self,
        tenant_id: TenantId,
        campaign: &PublishedCampaign,
    ) -> impl Future<Output = Result<(), CampaignStoreError>> + Send;

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

    /// An in-memory `CampaignStore` for the seam's tests. Tenant-scoped exactly like the real thing;
    /// an upsert touches one campaign for one tenant and leaves other tenants alone, so a test can
    /// prove isolation.
    #[derive(Default)]
    struct FakeCampaigns {
        rows: Mutex<Vec<(TenantId, PublishedCampaign)>>,
    }

    impl CampaignStore for FakeCampaigns {
        async fn list_campaigns(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<PublishedCampaign>, CampaignStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, _campaign)| *owner == tenant_id)
                .map(|(_owner, campaign)| campaign.clone())
                .collect())
        }

        async fn get_campaign(
            &self,
            tenant_id: TenantId,
            campaign_id: CampaignId,
        ) -> Result<Option<PublishedCampaign>, CampaignStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|(owner, campaign)| *owner == tenant_id && campaign.id == campaign_id)
                .map(|(_owner, campaign)| campaign.clone()))
        }

        async fn upsert_campaign(
            &self,
            tenant_id: TenantId,
            campaign: &PublishedCampaign,
        ) -> Result<(), CampaignStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            rows.retain(|(owner, existing)| !(*owner == tenant_id && existing.id == campaign.id));
            rows.push((tenant_id, campaign.clone()));
            Ok(())
        }

        async fn delete_campaign(
            &self,
            tenant_id: TenantId,
            campaign_id: CampaignId,
        ) -> Result<(), CampaignStoreError> {
            self.rows
                .lock()
                .expect("lock")
                .retain(|(owner, campaign)| !(*owner == tenant_id && campaign.id == campaign_id));
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
    async fn upsert_get_delete_stay_tenant_scoped() {
        let store = FakeCampaigns::default();

        // A neighbour's campaign must survive our writes.
        store
            .upsert_campaign(other_tenant(), &campaign(99, "Neighbour"))
            .await
            .expect("neighbour");

        store
            .upsert_campaign(tenant(), &campaign(10, "Lunch"))
            .await
            .expect("create");
        store
            .upsert_campaign(tenant(), &campaign(11, "Dinner"))
            .await
            .expect("create 2");
        assert_eq!(store.list_campaigns(tenant()).await.expect("list").len(), 2);

        // Upsert replaces by id rather than appending.
        store
            .upsert_campaign(tenant(), &campaign(10, "Lunch (renamed)"))
            .await
            .expect("update");
        let listed = store.list_campaigns(tenant()).await.expect("list again");
        assert_eq!(
            listed.len(),
            2,
            "upsert of an existing id does not add a row"
        );

        let fetched = store
            .get_campaign(tenant(), CampaignId::new(Ulid::from_u128(10)))
            .await
            .expect("get")
            .expect("present");
        assert_eq!(fetched.name, DisplayName::new("Lunch (renamed)"));

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
