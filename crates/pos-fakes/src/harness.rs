// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Harness implementations, so `tests/contract.rs` can run all eighteen suites.
//!
//! Each one is thin, and that is the point: the destructive operations a suite needs — losing power,
//! severing a link, emptying a paper roll, staging an ambiguous card result — are methods on the
//! *fakes*, reachable only through these harnesses. None of them is on a port, so no production
//! adapter carries a "corrupt yourself now" entry point
//! ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §6).
//!
//! These are also worth reading as a worked example: an adapter author writing `store-sqlite` needs
//! exactly this file, with `fresh` creating a temporary database and `lose_power` reopening it
//! without checkpointing.

use pos_contract_tests::harness::{
    BlobStoreHarness, ClockSourceHarness, CloudSyncHarness, ConfigStoreHarness,
    DeliveryVendorHarness, DeviceRegistryHarness, ErpSinkHarness, EventStoreHarness,
    FiscalizationHarness, HarnessError, IdGeneratorHarness, IntakeLedgerHarness, KeyVaultHarness,
    MessageLinkHarness, MetricsSinkHarness, OrderInHarness, PaymentTerminalHarness,
    PrinterDriverHarness, Setup, ShippingDispatchHarness, SignerHarness,
};
use pos_ports::{
    AccountCode, BusyMode, CourierJobRef, MetricSample, PrinterCapabilities, PublicKey, Signature,
    UpdateReport, VendorOrderRef,
};
use pos_proto::{
    DeviceId, MenuItemId, Money, PaymentOutcome, ReleaseTag, StoreId, Timestamp, Ulid,
};

use crate::determinism::{FakeClock, FakeIdGenerator};
use crate::devices::{FakePaymentTerminal, FakePrinter};
use crate::infra::{
    FakeBlobStore, FakeCloudSync, FakeDeviceRegistry, FakeKeyVault, FakeLink, FakeMetricsSink,
    FakeSigner,
};
use crate::store::FakeStore;
use crate::vendors::{
    FakeDeliveryVendor, FakeErp, FakeFiscal, FakeIntake, FakeShipping, known_menu_item,
    unknown_menu_item,
};

/// The store every fake harness reports.
///
/// Fixed rather than random: a suite failure should be reproducible, and none of these fakes cares
/// which store it is.
#[must_use]
pub fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

/// Harness for [`FakeStore`] as an `EventStore`.
#[derive(Debug, Default)]
pub struct StoreHarness;

impl EventStoreHarness for StoreHarness {
    type Store = FakeStore;

    async fn fresh(&self) -> Setup<Self::Store> {
        Ok(FakeStore::new())
    }

    async fn lose_power(&self, store: Self::Store) -> Setup<Self::Store> {
        // Nothing is flushed and nothing is committed. Uncommitted writes live in the transaction
        // handle, so they are already gone by the time this runs — which is exactly what an unflushed
        // SQLite transaction does after a pulled cable.
        Ok(store.reopen())
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

impl ConfigStoreHarness for StoreHarness {
    type Store = FakeStore;

    async fn fresh(&self) -> Setup<Self::Store> {
        Ok(FakeStore::new())
    }

    async fn lose_power(&self, store: Self::Store) -> Setup<Self::Store> {
        Ok(store.reopen())
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

impl IntakeLedgerHarness for StoreHarness {
    type Ledger = FakeStore;

    async fn fresh(&self) -> Setup<Self::Ledger> {
        Ok(FakeStore::new())
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Harness for [`FakeLink`].
#[derive(Debug, Default)]
pub struct LinkHarness;

impl MessageLinkHarness for LinkHarness {
    type Link = FakeLink;

    async fn fresh(&self) -> Setup<Self::Link> {
        Ok(FakeLink::new())
    }

    async fn sever(&self, link: &Self::Link) -> Setup<()> {
        link.sever();
        Ok(())
    }

    async fn fill(&self, link: &Self::Link) -> Setup<()> {
        link.fill();
        Ok(())
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Harness for [`FakeBlobStore`].
#[derive(Debug, Default)]
pub struct BlobHarness;

impl BlobStoreHarness for BlobHarness {
    type Store = FakeBlobStore;

    async fn fresh(&self) -> Setup<Self::Store> {
        Ok(FakeBlobStore::new())
    }
}

/// Harness for [`FakeMetricsSink`].
#[derive(Debug, Default)]
pub struct MetricsHarness;

impl MetricsSinkHarness for MetricsHarness {
    type Sink = FakeMetricsSink;

    async fn fresh(&self) -> Setup<Self::Sink> {
        Ok(FakeMetricsSink::new())
    }

    async fn recorded(&self, sink: &Self::Sink) -> Setup<Vec<MetricSample>> {
        Ok(sink.recorded())
    }

    async fn saturate(&self, sink: &Self::Sink) -> Setup<()> {
        sink.saturate();
        Ok(())
    }
}

/// Harness for [`FakeSigner`].
#[derive(Debug, Default)]
pub struct SignerFixture;

impl SignerHarness for SignerFixture {
    type Signer = FakeSigner;

    async fn fresh(&self) -> Setup<Self::Signer> {
        Ok(FakeSigner::new())
    }

    fn valid_triple(&self) -> Setup<(Vec<u8>, Signature, PublicKey)> {
        let artifact = b"pos_edge binary contents".to_vec();
        let key = FakeSigner::key(1);
        let signature = FakeSigner::sign(&artifact, &key);
        Ok((artifact, signature, key))
    }

    fn other_key(&self) -> Setup<PublicKey> {
        // A different key id as well as different bytes, which is what makes "wrong key" reportable
        // as such rather than as a failed verification.
        Ok(FakeSigner::key(2))
    }
}

/// Harness for [`FakeKeyVault`].
#[derive(Debug, Default)]
pub struct VaultHarness;

impl KeyVaultHarness for VaultHarness {
    type Vault = FakeKeyVault;

    async fn fresh(&self) -> Setup<Self::Vault> {
        Ok(FakeKeyVault::new())
    }
}

/// Harness for [`FakeDeviceRegistry`].
#[derive(Debug, Default)]
pub struct RegistryHarness;

impl DeviceRegistryHarness for RegistryHarness {
    type Registry = FakeDeviceRegistry;

    async fn fresh(&self) -> Setup<Self::Registry> {
        Ok(FakeDeviceRegistry::new())
    }
}

/// Harness for [`FakeCloudSync`].
#[derive(Debug, Default)]
pub struct CloudHarness;

impl CloudSyncHarness for CloudHarness {
    type Channel = FakeCloudSync;

    async fn fresh(&self) -> Setup<Self::Channel> {
        Ok(FakeCloudSync::new())
    }

    fn valid_code(&self) -> String {
        FakeCloudSync::VALID_CODE.to_owned()
    }

    fn granted_device(&self) -> DeviceId {
        FakeCloudSync::granted_device()
    }

    fn known_release(&self) -> ReleaseTag {
        ReleaseTag::new(FakeCloudSync::KNOWN_RELEASE)
    }

    fn update_bytes(&self) -> Vec<u8> {
        FakeCloudSync::artifact_bytes()
    }

    fn sample_report(&self) -> UpdateReport {
        FakeCloudSync::sample_report()
    }
}

/// Harness for [`FakePrinter`].
#[derive(Debug, Default)]
pub struct PrinterHarness;

impl PrinterDriverHarness for PrinterHarness {
    type Printer = FakePrinter;

    async fn fresh(&self, capabilities: PrinterCapabilities) -> Setup<Self::Printer> {
        Ok(FakePrinter::new(capabilities))
    }

    async fn take_offline(&self, printer: &Self::Printer) -> Setup<()> {
        printer.take_offline();
        Ok(())
    }

    async fn empty_paper(&self, printer: &Self::Printer) -> Setup<()> {
        printer.empty_paper();
        Ok(())
    }

    async fn tickets_printed(&self, printer: &Self::Printer) -> Setup<u32> {
        Ok(printer.tickets_printed())
    }

    async fn drawer_opened(&self, printer: &Self::Printer) -> Setup<bool> {
        Ok(printer.drawer_opened())
    }
}

/// Harness for [`FakePaymentTerminal`].
#[derive(Debug, Default)]
pub struct TerminalHarness;

impl PaymentTerminalHarness for TerminalHarness {
    type Terminal = FakePaymentTerminal;

    async fn fresh(&self) -> Setup<Self::Terminal> {
        Ok(FakePaymentTerminal::new())
    }

    async fn stage_outcome(&self, terminal: &Self::Terminal, outcome: PaymentOutcome) -> Setup<()> {
        terminal.stage_outcome(outcome);
        Ok(())
    }

    async fn stage_unknown(&self, terminal: &Self::Terminal) -> Setup<()> {
        // The branch that always exists. A real terminal reaches it by being unplugged mid-tap; a
        // fake reaches it because a suite asked, which is the only way to test it at all.
        terminal.stage_outcome(PaymentOutcome::Unknown);
        Ok(())
    }

    async fn authorisation_count(&self, terminal: &Self::Terminal) -> Setup<u32> {
        Ok(terminal.authorisation_count())
    }
}

/// Harness for [`FakeFiscal`].
#[derive(Debug, Default)]
pub struct FiscalHarness;

impl FiscalizationHarness for FiscalHarness {
    type Fiscal = FakeFiscal;

    async fn fresh(&self) -> Setup<Self::Fiscal> {
        Ok(FakeFiscal::new())
    }

    async fn disconnect(&self, fiscal: &Self::Fiscal) -> Setup<()> {
        fiscal.disconnect();
        Ok(())
    }

    async fn reconnect(&self, fiscal: &Self::Fiscal) -> Setup<()> {
        fiscal.reconnect();
        Ok(())
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Harness for [`FakeDeliveryVendor`].
#[derive(Debug, Default)]
pub struct DeliveryHarness;

impl DeliveryVendorHarness for DeliveryHarness {
    type Vendor = FakeDeliveryVendor;

    async fn fresh(&self) -> Setup<Self::Vendor> {
        Ok(FakeDeliveryVendor::new())
    }

    async fn stage_order(&self, vendor: &Self::Vendor) -> Setup<VendorOrderRef> {
        Ok(vendor.stage_order())
    }

    async fn stage_expired_order(&self, vendor: &Self::Vendor) -> Setup<VendorOrderRef> {
        Ok(vendor.stage_expired_order())
    }

    async fn busy_mode(&self, vendor: &Self::Vendor) -> Setup<BusyMode> {
        Ok(vendor.busy_mode())
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Harness for [`FakeShipping`].
#[derive(Debug, Default)]
pub struct ShippingHarness;

impl ShippingDispatchHarness for ShippingHarness {
    type Courier = FakeShipping;

    async fn fresh(&self) -> Setup<Self::Courier> {
        Ok(FakeShipping::new())
    }

    async fn complete(&self, courier: &Self::Courier, job: &CourierJobRef) -> Setup<()> {
        courier.complete(job);
        Ok(())
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Harness for [`FakeErp`].
#[derive(Debug, Default)]
pub struct ErpHarness;

impl ErpSinkHarness for ErpHarness {
    type Erp = FakeErp;

    async fn fresh(&self) -> Setup<Self::Erp> {
        Ok(FakeErp::new())
    }

    fn known_account(&self) -> AccountCode {
        FakeErp::known_account()
    }

    fn unknown_account(&self) -> AccountCode {
        FakeErp::unknown_account()
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Harness for [`FakeIntake`].
#[derive(Debug, Default)]
pub struct IntakeHarness;

impl OrderInHarness for IntakeHarness {
    type Intake = FakeIntake;

    async fn fresh(&self) -> Setup<Self::Intake> {
        Ok(FakeIntake::new())
    }

    fn known_menu_item(&self) -> (MenuItemId, Money) {
        known_menu_item()
    }

    fn unknown_menu_item(&self) -> MenuItemId {
        unknown_menu_item()
    }

    fn store_id(&self) -> StoreId {
        store_id()
    }
}

/// Harness for [`FakeClock`].
#[derive(Debug, Default)]
pub struct ClockHarness;

impl ClockSourceHarness for ClockHarness {
    type Clock = FakeClock;

    fn fresh(&self, at: Timestamp) -> Setup<Self::Clock> {
        Ok(FakeClock::new(at))
    }

    fn set(&self, clock: &Self::Clock, at: Timestamp) -> Setup<()> {
        clock.set(at);
        Ok(())
    }
}

/// Harness for [`FakeIdGenerator`].
#[derive(Debug, Default)]
pub struct IdHarness;

impl IdGeneratorHarness for IdHarness {
    type Generator = FakeIdGenerator;

    fn fresh(&self, at: Timestamp) -> Setup<Self::Generator> {
        Ok(FakeIdGenerator::new(FakeClock::new(at)))
    }

    fn set(&self, generator: &Self::Generator, at: Timestamp) -> Setup<()> {
        generator.clock().set(at);
        Ok(())
    }
}

/// Kept so the unused import of [`HarnessError`] has a purpose, and so that a harness failure has a
/// worked example somewhere in the tree.
///
/// A real `store-sqlite` harness returns this when a temporary directory cannot be created — which is
/// a broken machine, not a broken adapter, and the suite says so rather than reporting a port failure.
#[must_use]
pub fn example_setup_failure() -> HarnessError {
    HarnessError::new("no writable temporary directory")
}
