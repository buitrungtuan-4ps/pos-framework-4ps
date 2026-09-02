// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every contract suite, against the fakes.
//!
//! `docs/roadmap.md` P2's exit criterion is *"every port has a contract suite; the fakes crate passes
//! all of them"*. This file is the second half.
//!
//! It matters more than a fakes test usually would. `pos-core`'s suite runs against these fakes, so a
//! fake that disagrees with the real store makes every domain test a test of the wrong thing — the
//! suites are the only thing closing that gap, and exempting the fakes from them would leave the
//! whole domain suite resting on an unchecked assumption.
//!
//! # No runtime
//!
//! Every suite is invoked with `run_ready`, which polls a future exactly once. Nothing here awaits
//! anything, so a `Pending` would be a fake that had grown a suspension point — see
//! `pos_fakes::executor`. This is what makes the suite finish in milliseconds: not fast fakes, but no
//! scheduler at all.

use pos_fakes::executor::run_ready;
use pos_fakes::harness::{
    BlobHarness, ClockHarness, CloudHarness, DeliveryHarness, ErpHarness, FiscalHarness, IdHarness,
    IntakeHarness, LinkHarness, MetricsHarness, PrinterHarness, RegistryHarness, ShippingHarness,
    SignerFixture, StoreHarness, TerminalHarness, VaultHarness,
};

/// One module per port, so a failing test names its port in the path.
mod event_store {
    use super::{StoreHarness, run_ready};
    pos_contract_tests::event_store_suite!(StoreHarness, run_ready);
}

mod config_store {
    use super::{StoreHarness, run_ready};
    pos_contract_tests::config_store_suite!(StoreHarness, run_ready);
}

mod message_link {
    use super::{LinkHarness, run_ready};
    pos_contract_tests::message_link_suite!(LinkHarness, run_ready);
}

mod blob_store {
    use super::{BlobHarness, run_ready};
    pos_contract_tests::blob_store_suite!(BlobHarness, run_ready);
}

mod metrics_sink {
    use super::{MetricsHarness, run_ready};
    pos_contract_tests::metrics_sink_suite!(MetricsHarness, run_ready);
}

mod signer {
    use super::SignerFixture;
    pos_contract_tests::signer_suite!(SignerFixture, super::run_ready);
}

mod key_vault {
    use super::{VaultHarness, run_ready};
    pos_contract_tests::key_vault_suite!(VaultHarness, run_ready);
}

mod cloud_sync {
    use super::{CloudHarness, run_ready};
    pos_contract_tests::cloud_sync_suite!(CloudHarness, run_ready);
}

mod clock_source {
    use super::ClockHarness;
    pos_contract_tests::clock_source_suite!(ClockHarness);
}

mod id_generator {
    use super::IdHarness;
    pos_contract_tests::id_generator_suite!(IdHarness);
}

mod printer_driver {
    use super::{PrinterHarness, run_ready};
    pos_contract_tests::printer_driver_suite!(PrinterHarness, run_ready);
}

mod payment_terminal {
    use super::{TerminalHarness, run_ready};
    pos_contract_tests::payment_terminal_suite!(TerminalHarness, run_ready);
}

mod fiscalization {
    use super::{FiscalHarness, run_ready};
    pos_contract_tests::fiscalization_suite!(FiscalHarness, run_ready);
}

mod delivery_vendor {
    use super::{DeliveryHarness, run_ready};
    pos_contract_tests::delivery_vendor_suite!(DeliveryHarness, run_ready);
}

mod shipping_dispatch {
    use super::{ShippingHarness, run_ready};
    pos_contract_tests::shipping_dispatch_suite!(ShippingHarness, run_ready);
}

mod erp_sink {
    use super::{ErpHarness, run_ready};
    pos_contract_tests::erp_sink_suite!(ErpHarness, run_ready);
}

mod order_in {
    use super::{IntakeHarness, run_ready};
    pos_contract_tests::order_in_suite!(IntakeHarness, run_ready);
}

mod device_registry {
    use super::{RegistryHarness, run_ready};
    pos_contract_tests::device_registry_suite!(RegistryHarness, run_ready);
}

/// Eighteen suites are invoked above, matching `PortName::ALL`.
///
/// `pos_contract_tests` asserts that every port *has* a suite; this asserts that this crate *runs*
/// every one. Without it a suite could be added and quietly never invoked here, which would look
/// exactly like passing.
#[test]
fn every_suite_is_invoked_here() {
    let invoked = [
        "event_store",
        "config_store",
        "message_link",
        "blob_store",
        "metrics_sink",
        "signer",
        "key_vault",
        "clock_source",
        "id_generator",
        "printer_driver",
        "payment_terminal",
        "fiscalization",
        "delivery_vendor",
        "shipping_dispatch",
        "erp_sink",
        "order_in",
        "cloud_sync",
        "device_registry",
    ];
    assert_eq!(invoked.len(), pos_contract_tests::SUITES.len());
    for (port, _) in pos_contract_tests::SUITES {
        assert!(
            invoked.contains(&port.as_label()),
            "{port}'s suite exists but is not invoked in this file"
        );
    }
}
