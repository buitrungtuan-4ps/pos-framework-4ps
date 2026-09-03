// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voucher batch generation (Track M3, [ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
//!
//! A voucher-kind campaign discounts a bill when the guest presents a **code**. This seam mints and
//! stores a batch of those codes — one voucher instance ([`pos_proto::ids::VoucherId`]) per code, tied
//! to the campaign it redeems against — and lists them for distribution. Redemption itself is the
//! engine's existing online check-and-mark (the `PromotionVoucher*` events); minting and handing out
//! the codes is what this adds.
//!
//! A voucher code is distributable — it is printed on a flyer or e-mailed to a guest — so, unlike an
//! API key (ADR-0037), it is stored in clear text rather than hashed: the operator must be able to
//! read it back to hand it out. It is still sensitive (it carries redeemable value), so listing codes
//! sits behind the same `console.campaigns.manage` permission that mints them, never plain `Read`, and
//! the mint audit records only the count, never the codes.

use core::future::Future;

use pos_proto::ids::{CampaignId, TenantId, VoucherId};

use crate::paging::{Page, PageRequest};

/// A voucher's lifecycle status. Minting creates it [`Active`](VoucherStatus::Active); the engine's
/// online redemption (a later, runtime concern) moves it to [`Redeemed`](VoucherStatus::Redeemed), and
/// an operator may [`Void`](VoucherStatus::Void) one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoucherStatus {
    /// Minted and not yet redeemed.
    Active,
    /// Redeemed against a bill (set by the runtime redemption path).
    Redeemed,
    /// Cancelled by an operator; never redeemable.
    Void,
}

impl VoucherStatus {
    /// The stored token.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Redeemed => "REDEEMED",
            Self::Void => "VOID",
        }
    }

    /// Parses a stored token, defaulting an unknown one to [`Active`](VoucherStatus::Active) — a stored
    /// value this cloud wrote is always one of the three, so the default is unreachable in practice.
    #[must_use]
    pub fn from_wire(token: &str) -> Self {
        match token {
            "REDEEMED" => Self::Redeemed,
            "VOID" => Self::Void,
            _ => Self::Active,
        }
    }
}

/// A voucher to mint: its id, the campaign it redeems against, and its distributable code.
#[derive(Debug, Clone)]
pub struct NewVoucher {
    /// The voucher instance's id.
    pub voucher_id: VoucherId,
    /// The campaign this code redeems against (must be a voucher-kind campaign).
    pub campaign_id: CampaignId,
    /// The distributable code, unique within the tenant.
    pub code: String,
}

/// A stored voucher instance, as listed for distribution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VoucherRecord {
    /// The voucher instance's id (a ULID string).
    pub voucher_id: String,
    /// The campaign it redeems against.
    pub campaign_id: String,
    /// The distributable code.
    pub code: String,
    /// Its lifecycle status.
    pub status: VoucherStatus,
    /// When it was minted, Unix milliseconds.
    pub created_at_ms: i64,
}

/// Persists and reads a tenant's voucher instances.
///
/// `insert_batch` mints a batch atomically (all or nothing — a code collision fails the whole batch
/// rather than half-minting); `list_by_campaign` reads the codes for one campaign, newest first. Both
/// are tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every other cloud table.
pub trait VoucherStore {
    /// Mints a batch of vouchers for a tenant, atomically.
    fn insert_batch(
        &self,
        tenant_id: TenantId,
        vouchers: &[NewVoucher],
    ) -> impl Future<Output = Result<(), VoucherStoreError>> + Send;

    /// Lists a campaign's voucher instances, newest first.
    ///
    /// Every code, unpaged. Kept as it is and not deprecated
    /// ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)).
    ///
    /// Unlike the other lists ADR-0098 pages, this one has no in-process reader to protect — the
    /// route is its only caller. What it serves is the wire read: an operator distributing a
    /// promotion needs every code in the batch to print or mail, and "page four of the flyer run"
    /// is not a thing they can ask for. So the unpaged form stays reachable by omitting `?limit=`,
    /// and the console's *table* is what pages.
    fn list_by_campaign(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
    ) -> impl Future<Output = Result<Vec<VoucherRecord>, VoucherStoreError>> + Send;

    /// One page of a campaign's voucher instances, newest first, with the size of the whole set.
    ///
    /// Beside [`list_by_campaign`](Self::list_by_campaign) rather than replacing it, which is the
    /// shape ADR-0098 chose for every paged list: a caller that wants all the codes and a caller
    /// that wants twenty-five of them are asking different questions, and the second one arriving
    /// must not change the answer to the first.
    ///
    /// This is the seam's acute case. `MAX_VOUCHER_BATCH` is 10 000 codes per mint and batches
    /// accumulate against a campaign, so a promotion with three drops holds 30 000 rows that the
    /// console fetches and renders in one go.
    fn list_by_campaign_page(
        &self,
        tenant_id: TenantId,
        campaign_id: CampaignId,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<VoucherRecord>, VoucherStoreError>> + Send;
}

/// The alphabet voucher codes draw from: Crockford base32 — digits and upper-case letters with the
/// ambiguous `I`, `L`, `O`, `U` removed, so a code read off a printed flyer is unambiguous.
const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The length of a minted voucher code. Twelve Crockford-base32 characters is ~60 bits of entropy —
/// far more than enough that a batch of thousands sees no collision, while staying short enough to
/// print and type.
const CODE_LENGTH: usize = 12;

/// Mints one random voucher code from OS entropy, or `None` if the entropy source is unavailable.
///
/// Each character is one CSPRNG byte reduced into [`CODE_ALPHABET`]. The reduction's modulo bias is
/// immaterial for a distribution code (this is not a cryptographic key), and the store's uniqueness
/// constraint is the real guard against a collision.
#[must_use]
pub fn generate_code() -> Option<String> {
    let mut bytes = [0_u8; CODE_LENGTH];
    getrandom::fill(&mut bytes).ok()?;
    let code = bytes
        .iter()
        .map(|byte| char::from(CODE_ALPHABET[usize::from(*byte) % CODE_ALPHABET.len()]))
        .collect();
    Some(code)
}

/// A failure of the voucher store itself — the database is unreachable, or a code collided.
#[derive(Debug, thiserror::Error)]
#[error("the voucher store failed: {0}")]
pub struct VoucherStoreError(String);

impl VoucherStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::ids::{CampaignId, TenantId, VoucherId};
    use pos_proto::ulid::Ulid;

    use super::{
        CODE_ALPHABET, CODE_LENGTH, NewVoucher, Page, PageRequest, VoucherRecord, VoucherStatus,
        VoucherStore, VoucherStoreError, generate_code,
    };

    #[derive(Default)]
    struct FakeVouchers {
        rows: Mutex<Vec<(TenantId, VoucherRecord)>>,
    }

    impl VoucherStore for FakeVouchers {
        async fn insert_batch(
            &self,
            tenant_id: TenantId,
            vouchers: &[NewVoucher],
        ) -> Result<(), VoucherStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            for voucher in vouchers {
                if rows
                    .iter()
                    .any(|(owner, existing)| *owner == tenant_id && existing.code == voucher.code)
                {
                    return Err(VoucherStoreError::new("a voucher code collided"));
                }
                rows.push((
                    tenant_id,
                    VoucherRecord {
                        voucher_id: voucher.voucher_id.to_string(),
                        campaign_id: voucher.campaign_id.to_string(),
                        code: voucher.code.clone(),
                        status: VoucherStatus::Active,
                        created_at_ms: 0,
                    },
                ));
            }
            Ok(())
        }

        async fn list_by_campaign(
            &self,
            tenant_id: TenantId,
            campaign_id: CampaignId,
        ) -> Result<Vec<VoucherRecord>, VoucherStoreError> {
            Ok(self.matching(tenant_id, campaign_id))
        }

        async fn list_by_campaign_page(
            &self,
            tenant_id: TenantId,
            campaign_id: CampaignId,
            page: PageRequest,
        ) -> Result<Page<VoucherRecord>, VoucherStoreError> {
            // The whole matching set, then the window — which is what a fake is for. The
            // store-postgres impl does the same thing in one statement (`LIMIT`/`OFFSET` beside a
            // `COUNT(*) OVER()`), and the point of both is that `total` counts the *set* while
            // `items` carries the *page*, so a caller cannot read one for the other.
            let matching = self.matching(tenant_id, campaign_id);
            let total = u32::try_from(matching.len()).unwrap_or(u32::MAX);
            let items = matching
                .into_iter()
                .skip(page.offset() as usize)
                .take(page.limit() as usize)
                .collect();
            Ok(Page::new(items, total))
        }
    }

    impl FakeVouchers {
        /// The tenant's vouchers for one campaign, in insertion order.
        ///
        /// Shared by both reads so the paged one cannot drift from the unpaged one: a page that
        /// filtered differently would be a bug no test comparing only lengths would catch.
        fn matching(&self, tenant_id: TenantId, campaign_id: CampaignId) -> Vec<VoucherRecord> {
            let campaign = campaign_id.to_string();
            self.rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|(owner, record)| *owner == tenant_id && record.campaign_id == campaign)
                .map(|(_owner, record)| record.clone())
                .collect()
        }
    }

    #[test]
    fn a_generated_code_is_the_right_length_and_alphabet() {
        let code = generate_code().expect("entropy");
        assert_eq!(code.len(), CODE_LENGTH);
        assert!(
            code.bytes().all(|byte| CODE_ALPHABET.contains(&byte)),
            "every character is drawn from the unambiguous alphabet: {code}"
        );
    }

    #[tokio::test]
    async fn a_batch_mints_and_lists_tenant_scoped() {
        let store = FakeVouchers::default();
        let tenant = TenantId::new(Ulid::from_u128(1));
        let other = TenantId::new(Ulid::from_u128(2));
        let campaign = CampaignId::new(Ulid::from_u128(10));

        let batch: Vec<NewVoucher> = (0..3)
            .map(|n| NewVoucher {
                voucher_id: VoucherId::new(Ulid::from_u128(100 + n)),
                campaign_id: campaign,
                code: format!("CODE{n}"),
            })
            .collect();
        store.insert_batch(tenant, &batch).await.expect("mint");
        // A neighbour minting the same code is fine — codes are unique per tenant, not globally.
        store
            .insert_batch(
                other,
                &[NewVoucher {
                    voucher_id: VoucherId::new(Ulid::from_u128(200)),
                    campaign_id: campaign,
                    code: "CODE0".to_owned(),
                }],
            )
            .await
            .expect("neighbour mint");

        let listed = store
            .list_by_campaign(tenant, campaign)
            .await
            .expect("list");
        assert_eq!(listed.len(), 3);
        assert!(listed.iter().all(|v| v.status == VoucherStatus::Active));

        // A collision within the tenant fails the batch.
        let collision = store
            .insert_batch(
                tenant,
                &[NewVoucher {
                    voucher_id: VoucherId::new(Ulid::from_u128(300)),
                    campaign_id: campaign,
                    code: "CODE0".to_owned(),
                }],
            )
            .await;
        assert!(collision.is_err(), "a duplicate code is rejected");
    }

    /// Mints `count` codes for one campaign, so the paging tests are about paging.
    async fn minted(store: &FakeVouchers, tenant: TenantId, campaign: CampaignId, count: u32) {
        let batch: Vec<NewVoucher> = (0..count)
            .map(|n| NewVoucher {
                voucher_id: VoucherId::new(Ulid::from_u128(u128::from(1000 + n))),
                campaign_id: campaign,
                code: format!("CODE{n:04}"),
            })
            .collect();
        store.insert_batch(tenant, &batch).await.expect("mint");
    }

    #[tokio::test]
    async fn a_page_carries_its_own_rows_and_the_size_of_the_whole_set() {
        // The property the pager renders, and the one a page is for: `items` is the window,
        // `total` is the set. Reporting the page's own length as `total` would make every pager
        // read "1–10 of 10" and hide the other fifteen codes with no error anywhere.
        let store = FakeVouchers::default();
        let tenant = TenantId::new(Ulid::from_u128(1));
        let campaign = CampaignId::new(Ulid::from_u128(10));
        minted(&store, tenant, campaign, 25).await;

        let page = store
            .list_by_campaign_page(tenant, campaign, PageRequest::new(10, 0).expect("in range"))
            .await
            .expect("page");
        assert_eq!(page.items.len(), 10, "the window is the limit");
        assert_eq!(page.total, 25, "the total is the set, not the window");
    }

    #[tokio::test]
    async fn consecutive_pages_partition_the_set_without_overlap_or_gaps() {
        let store = FakeVouchers::default();
        let tenant = TenantId::new(Ulid::from_u128(1));
        let campaign = CampaignId::new(Ulid::from_u128(10));
        minted(&store, tenant, campaign, 25).await;

        let mut seen = Vec::new();
        for offset in [0, 10, 20] {
            let page = store
                .list_by_campaign_page(
                    tenant,
                    campaign,
                    PageRequest::new(10, offset).expect("in range"),
                )
                .await
                .expect("page");
            assert_eq!(page.total, 25, "the total does not change as pages advance");
            seen.extend(page.items.into_iter().map(|record| record.code));
        }
        assert_eq!(seen.len(), 25, "three pages of ten cover twenty-five rows");
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 25, "no code appears on two pages");
    }

    #[tokio::test]
    async fn a_page_past_the_end_is_empty_and_not_an_error() {
        // An operator on page four of a batch that just shrank gets an empty page, not a `500`.
        let store = FakeVouchers::default();
        let tenant = TenantId::new(Ulid::from_u128(1));
        let campaign = CampaignId::new(Ulid::from_u128(10));
        minted(&store, tenant, campaign, 5).await;

        let page = store
            .list_by_campaign_page(
                tenant,
                campaign,
                PageRequest::new(10, 100).expect("in range"),
            )
            .await
            .expect("a page past the end still reads");
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn the_paged_read_is_tenant_scoped_exactly_like_the_unpaged_one() {
        // The filter has to be identical on both reads. A page that leaked a neighbour's codes
        // would be the worst possible bug on this particular table.
        let store = FakeVouchers::default();
        let tenant = TenantId::new(Ulid::from_u128(1));
        let neighbour = TenantId::new(Ulid::from_u128(2));
        let campaign = CampaignId::new(Ulid::from_u128(10));
        minted(&store, tenant, campaign, 3).await;
        minted(&store, neighbour, campaign, 7).await;

        let page = store
            .list_by_campaign_page(
                tenant,
                campaign,
                PageRequest::new(100, 0).expect("in range"),
            )
            .await
            .expect("page");
        assert_eq!(page.total, 3, "only this tenant's codes are counted");
        assert_eq!(page.items.len(), 3);
        let unpaged = store
            .list_by_campaign(tenant, campaign)
            .await
            .expect("list");
        assert_eq!(unpaged.len(), 3, "and the unpaged read agrees");
    }
}
