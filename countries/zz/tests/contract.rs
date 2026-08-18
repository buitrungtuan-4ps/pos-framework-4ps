// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `Fiscalization` contract suite, against the reference country module.
//!
//! This is what makes `countries/zz` a reference rather than a template. A copied module keeps this
//! file, replaces the implementation, and finds out immediately whether its provider fits the port —
//! rather than finding out in P10 with a tax authority waiting.
//!
//! The harness supplies the destructive operation the suite needs: `disconnect`, so
//! `issues_with_no_network` can check the obligation that pre-allocated ranges exist for. Without a
//! way to cut the authority off, that case would pass against a reachable one and prove nothing.

use pos_contract_tests::harness::{FiscalizationHarness, Setup};
use pos_country_zz::ZzFiscalization;
use pos_fakes::executor::run_ready;
use pos_proto::{StoreId, Ulid};

/// Supplies a fresh [`ZzFiscalization`].
///
/// `disconnect` and `reconnect` are no-ops here, and that is honest rather than lazy: this
/// implementation never contacts an authority, so it is *always* in the disconnected state the suite
/// wants to test. A real country module's harness flips a flag on its HTTP client, and the same suite
/// then checks the same obligation against a provider that can be unreachable.
#[derive(Default)]
struct ZzHarness;

impl FiscalizationHarness for ZzHarness {
    type Fiscal = ZzFiscalization;

    async fn fresh(&self) -> Setup<Self::Fiscal> {
        Ok(ZzFiscalization::new())
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

pos_contract_tests::fiscalization_suite!(ZzHarness, run_ready);
