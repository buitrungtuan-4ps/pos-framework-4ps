// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `Fiscalization` contract suite, against Japan's module.
//!
//! Every pack runs it. The obligations are the port's rather than the country's — offline issuance,
//! never-reuse across ranges, one number per bill, a refusal on exhaustion — and they are satisfied
//! by [`pos_country::offline`], which this pack constructs with Japan's own number format
//! ([ADR-0105](../../../docs/adr/0105-a-country-pack-is-values.md)).
//!
//! Running it here anyway is the point: when a provider replaces the body, this file is what says
//! immediately whether the provider fits the port — rather than a tax authority saying so later.

use pos_contract_tests::harness::{FiscalizationHarness, Setup};
use pos_country::offline::OfflineFiscalization;
use pos_fakes::executor::run_ready;
use pos_proto::{StoreId, Ulid};

/// Supplies a fresh fiscalization for Japan.
///
/// `disconnect` and `reconnect` are no-ops, and that is honest rather than lazy: nothing here
/// contacts an authority, so it is *always* in the disconnected state the suite wants to test. A
/// provider-backed module flips a flag on its HTTP client instead, and the same suite then checks the
/// same obligation against something that can genuinely be unreachable.
#[derive(Default)]
struct JpHarness;

impl FiscalizationHarness for JpHarness {
    type Fiscal = OfflineFiscalization;

    async fn fresh(&self) -> Setup<Self::Fiscal> {
        Ok(pos_country_jp::fiscalization())
    }

    async fn disconnect(&self, _fiscal: &Self::Fiscal) -> Setup<()> {
        Ok(())
    }

    async fn reconnect(&self, _fiscal: &Self::Fiscal) -> Setup<()> {
        Ok(())
    }

    fn store_id(&self) -> StoreId {
        StoreId::new(Ulid::from_u128(1))
    }
}

pos_contract_tests::fiscalization_suite!(JpHarness, run_ready);
