// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What an adapter's test code supplies so a suite can run against it.
//!
//! One harness trait per port. Each provides a fresh instance, and — for the ports whose
//! contract includes surviving abrupt loss — the destructive operation that causes it.
//!
//! # Why the destructive operations are here
//!
//! Because they must not be anywhere else. `EventStore`'s contract includes surviving a crash
//! mid-transaction, so a suite has to be able to cause one; putting
//! [`EventStoreHarness::lose_power`] on `EventStore` itself would ship a "corrupt yourself now"
//! method in every production adapter. See
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §6.
//!
//! # What `lose_power` must actually do
//!
//! Not a clean shutdown. The point is to reproduce what a pulled mains cable does: for a
//! file-backed adapter, drop the handle **without** checkpointing or flushing and reopen the
//! same file; for an in-memory fake, discard everything an open transaction was holding while
//! keeping everything committed. A harness that quietly commits first turns the whole obligation
//! into a no-op, and the suite would pass while the guarantee it checks does not hold — which is
//! worse than not having the case at all.
//!
//! # Every method returns [`Setup`], and only some carry an `# Errors` section
//!
//! Not an oversight. `clippy::missing_errors_doc` cannot see a `Result` behind
//! `-> impl Future<Output = …>`, so it fires on the synchronous harness methods and not on the
//! asynchronous ones. They all mean the same thing — the fixture could not be prepared — and
//! the sections that exist are there because the lint asked, not because those six are special.
//!
//! # Fresh means fresh
//!
//! [`EventStoreHarness::fresh`] and its siblings must return an instance with **no state from a
//! previous case**. Cases run in whatever order and whatever parallelism the test runner
//! chooses, so a shared instance produces failures that depend on scheduling — the least
//! debuggable failure available.

use core::fmt;
use core::future::Future;

use pos_ports::{
    BlobStore, CloudSync, ConfigStore, DeliveryVendor, ErpSink, EventStore, Fiscalization,
    KeyVault, MessageLink, MetricsSink, OrderIn, PaymentTerminal, PrinterDriver, ShippingDispatch,
    Signer,
};
use pos_proto::{ClockSource, DeviceId, IdGenerator, ReleaseTag, StoreId};

/// The test fixture itself failed.
///
/// Distinct from a [`crate::CaseFailure`] so an adapter author is not sent to debug a port when
/// the problem was a temporary directory that could not be created.
#[derive(Debug)]
pub struct HarnessError(String);

impl HarnessError {
    /// Describes what the harness could not do.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for HarnessError {}

/// Shorthand for what a harness method returns.
pub type Setup<T> = Result<T, HarnessError>;

/// Supplies a fresh [`EventStore`], and can take its power away.
pub trait EventStoreHarness: Send + Sync {
    /// The implementation under test.
    type Store: EventStore;

    /// A store with no events in it.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Store>> + Send;

    /// Reopens the same underlying storage after abrupt loss.
    ///
    /// Must **not** flush, checkpoint, or commit anything first. See this module's
    /// documentation for why a polite implementation makes the obligation vacuous.
    fn lose_power(&self, store: Self::Store) -> impl Future<Output = Setup<Self::Store>> + Send;

    /// A store identifier the cases may use.
    ///
    /// Supplied by the harness rather than invented by the suite, because a real adapter may
    /// require the store to exist — a row in the cloud, row-level security applying to it — and
    /// only the harness knows how to arrange that.
    fn store_id(&self) -> StoreId;
}

/// Supplies a fresh [`ConfigStore`].
pub trait ConfigStoreHarness: Send + Sync {
    /// The implementation under test.
    type Store: ConfigStore;

    /// A store with no configuration in it.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Store>> + Send;

    /// Reopens the same underlying storage after abrupt loss, without flushing first.
    fn lose_power(&self, store: Self::Store) -> impl Future<Output = Setup<Self::Store>> + Send;

    /// A store identifier the cases may use.
    fn store_id(&self) -> StoreId;
}

/// Supplies a fresh [`MessageLink`], and can sever it.
pub trait MessageLinkHarness: Send + Sync {
    /// The implementation under test.
    type Link: MessageLink;

    /// A link that has completed no handshake.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Link>> + Send;

    /// Makes the far side unreachable, so the "never at-most-once" obligation can be checked.
    ///
    /// A real adapter drops the connection; a fake flips a flag. Either way subsequent calls
    /// must fail with a retryable status rather than silently discarding events.
    fn sever(&self, link: &Self::Link) -> impl Future<Output = Setup<()>> + Send;

    /// Fills the far side to its limit, so back-pressure can be checked.
    fn fill(&self, link: &Self::Link) -> impl Future<Output = Setup<()>> + Send;

    /// A store identifier the cases may use.
    fn store_id(&self) -> StoreId;
}

/// Supplies a fresh [`BlobStore`].
pub trait BlobStoreHarness: Send + Sync {
    /// The implementation under test.
    type Store: BlobStore;

    /// A store with no objects in it.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Store>> + Send;
}

/// Supplies a fresh [`MetricsSink`].
pub trait MetricsSinkHarness: Send + Sync {
    /// The implementation under test.
    type Sink: MetricsSink;

    /// A sink with nothing recorded.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Sink>> + Send;

    /// Everything the sink has been given, in the order it arrived.
    ///
    /// The one harness method that reads back rather than sets up. A metrics sink has no read
    /// side in its port — telemetry goes one way — so without this the suite could only check
    /// that recording does not error, which is not a contract worth having.
    fn recorded(
        &self,
        sink: &Self::Sink,
    ) -> impl Future<Output = Setup<Vec<pos_ports::MetricSample>>> + Send;

    /// Saturates the sink, so the back-pressure obligation can be checked.
    fn saturate(&self, sink: &Self::Sink) -> impl Future<Output = Setup<()>> + Send;
}

/// Supplies a [`Signer`] together with material it will accept and reject.
///
/// The harness has to provide the artifact, signature and key, because only it knows the
/// algorithm — and a suite that generated its own would be testing its own arithmetic rather
/// than the adapter's.
pub trait SignerHarness: Send + Sync {
    /// The implementation under test.
    type Signer: Signer;

    /// The signer.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Signer>> + Send;

    /// An artifact, a signature over it, and the key that verifies it.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the fixture cannot be prepared.
    fn valid_triple(&self) -> Setup<(Vec<u8>, pos_ports::Signature, pos_ports::PublicKey)>;

    /// A second key, which the signature above must **not** verify against.
    ///
    /// This is what separates "wrong key" from "bad signature", and the port's contract says
    /// they are different statuses.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the fixture cannot be prepared.
    fn other_key(&self) -> Setup<pos_ports::PublicKey>;
}

/// Supplies a fresh [`KeyVault`].
pub trait KeyVaultHarness: Send + Sync {
    /// The implementation under test.
    type Vault: KeyVault;

    /// A vault holding nothing.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Vault>> + Send;
}

/// Supplies a fresh [`CloudSync`] seeded with one recognised activation and one published release.
///
/// A transport has no state of its own to reset, so the fixtures are values the harness both seeds
/// the channel with and hands the suite to assert against — the suite cannot know the right answer
/// otherwise.
pub trait CloudSyncHarness: Send + Sync {
    /// The implementation under test.
    type Channel: CloudSync;

    /// A channel that recognises [`Self::valid_code`] and publishes [`Self::known_release`].
    fn fresh(&self) -> impl Future<Output = Setup<Self::Channel>> + Send;

    /// The one activation code the channel accepts.
    fn valid_code(&self) -> String;

    /// The device the accepted code grants.
    fn granted_device(&self) -> DeviceId;

    /// A release the channel can fetch.
    fn known_release(&self) -> ReleaseTag;

    /// The artifact bytes that release returns.
    fn update_bytes(&self) -> Vec<u8>;
}

/// Supplies a fresh [`PrinterDriver`], and can break the printer.
pub trait PrinterDriverHarness: Send + Sync {
    /// The implementation under test.
    type Printer: PrinterDriver;

    /// A printer that is ready, with the given capabilities.
    ///
    /// Taking capabilities as a parameter is what lets one suite cover a USB drawer-kicking
    /// thermal printer and a network one that cannot open a drawer — the two configurations
    /// whose difference is a security boundary rather than a feature.
    fn fresh(
        &self,
        capabilities: pos_ports::PrinterCapabilities,
    ) -> impl Future<Output = Setup<Self::Printer>> + Send;

    /// Takes the printer offline, so the "return unavailable and let the caller re-queue"
    /// obligation can be checked.
    fn take_offline(&self, printer: &Self::Printer) -> impl Future<Output = Setup<()>> + Send;

    /// Empties the paper roll.
    fn empty_paper(&self, printer: &Self::Printer) -> impl Future<Output = Setup<()>> + Send;

    /// How many physical tickets came out.
    ///
    /// Required rather than optional: idempotency by job identifier is unobservable through the
    /// port itself, since a deduplicated second call and a real second print both return `Ok`.
    /// Without this the suite cannot tell them apart, and telling them apart is the obligation.
    fn tickets_printed(&self, printer: &Self::Printer) -> impl Future<Output = Setup<u32>> + Send;

    /// Whether the drawer was opened.
    fn drawer_opened(&self, printer: &Self::Printer) -> impl Future<Output = Setup<bool>> + Send;
}

/// Supplies a [`PaymentTerminal`] whose next outcome the suite can choose.
///
/// The unknown-result branch is the whole reason this port exists, and it cannot be provoked by
/// asking politely — a real terminal decides. So the harness stages the outcome.
pub trait PaymentTerminalHarness: Send + Sync {
    /// The implementation under test.
    type Terminal: PaymentTerminal;

    /// A terminal with no attempts on it.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Terminal>> + Send;

    /// Makes the next authorisation conclude with `outcome`.
    fn stage_outcome(
        &self,
        terminal: &Self::Terminal,
        outcome: pos_proto::PaymentOutcome,
    ) -> impl Future<Output = Setup<()>> + Send;

    /// Makes the next authorisation return an ambiguous result — the terminal was reached and
    /// could not say.
    fn stage_unknown(&self, terminal: &Self::Terminal) -> impl Future<Output = Setup<()>> + Send;

    /// How many times the terminal was actually asked to move money.
    ///
    /// The idempotency obligation is invisible from the port: a deduplicated retry and a second
    /// charge both return an attempt. This is what distinguishes them, and on this port the
    /// difference is a customer being charged twice.
    fn authorisation_count(
        &self,
        terminal: &Self::Terminal,
    ) -> impl Future<Output = Setup<u32>> + Send;
}

/// Supplies a [`Fiscalization`] implementation.
pub trait FiscalizationHarness: Send + Sync {
    /// The implementation under test.
    type Fiscal: Fiscalization;

    /// A country module with no ranges allocated.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Fiscal>> + Send;

    /// Cuts the authority off, so the offline-issuance obligation can be checked.
    ///
    /// The most important harness method in this file. "An invoice issues with no network"
    /// cannot be tested against a reachable authority, and if it is never tested, the first
    /// place it gets tested is a store with no internet and a customer waiting.
    fn disconnect(&self, fiscal: &Self::Fiscal) -> impl Future<Output = Setup<()>> + Send;

    /// Restores the connection.
    fn reconnect(&self, fiscal: &Self::Fiscal) -> impl Future<Output = Setup<()>> + Send;

    /// A store identifier the cases may use.
    fn store_id(&self) -> StoreId;
}

/// Supplies a [`DeliveryVendor`] whose inbound orders the suite can stage.
pub trait DeliveryVendorHarness: Send + Sync {
    /// The implementation under test.
    type Vendor: DeliveryVendor;

    /// A vendor with no pending orders.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Vendor>> + Send;

    /// Stages an order awaiting a decision, returning the vendor's reference for it.
    fn stage_order(
        &self,
        vendor: &Self::Vendor,
    ) -> impl Future<Output = Setup<pos_ports::VendorOrderRef>> + Send;

    /// Stages an order whose decision window has already closed.
    fn stage_expired_order(
        &self,
        vendor: &Self::Vendor,
    ) -> impl Future<Output = Setup<pos_ports::VendorOrderRef>> + Send;

    /// What the vendor currently believes about whether the store is open.
    fn busy_mode(
        &self,
        vendor: &Self::Vendor,
    ) -> impl Future<Output = Setup<pos_ports::BusyMode>> + Send;

    /// A store identifier the cases may use.
    fn store_id(&self) -> StoreId;
}

/// Supplies a [`ShippingDispatch`] implementation.
pub trait ShippingDispatchHarness: Send + Sync {
    /// The implementation under test.
    type Courier: ShippingDispatch;

    /// A courier with no jobs.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Courier>> + Send;

    /// Moves a job to a terminal status, so the "cannot cancel a completed job" obligation can
    /// be checked.
    fn complete(
        &self,
        courier: &Self::Courier,
        job: &pos_ports::CourierJobRef,
    ) -> impl Future<Output = Setup<()>> + Send;

    /// A store identifier the cases may use.
    fn store_id(&self) -> StoreId;
}

/// Supplies an [`ErpSink`] implementation.
pub trait ErpSinkHarness: Send + Sync {
    /// The implementation under test.
    type Erp: ErpSink;

    /// An ERP with nothing posted.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Erp>> + Send;

    /// An account code the ERP accepts.
    fn known_account(&self) -> pos_ports::AccountCode;

    /// An account code the ERP rejects, so the validation obligation can be checked.
    fn unknown_account(&self) -> pos_ports::AccountCode;

    /// A store identifier the cases may use.
    fn store_id(&self) -> StoreId;
}

/// Supplies an [`OrderIn`] implementation.
///
/// The unusual one: this port is implemented by the framework rather than by a vendor, so this
/// harness is supplied by `pos_edge` and `pos_cloud` and the suite is the specification of what
/// a caller may rely on.
pub trait OrderInHarness: Send + Sync {
    /// The implementation under test.
    type Intake: OrderIn;

    /// An intake with no orders.
    fn fresh(&self) -> impl Future<Output = Setup<Self::Intake>> + Send;

    /// A menu item the store sells, and the price it sells it at.
    fn known_menu_item(&self) -> (pos_proto::MenuItemId, pos_proto::Money);

    /// A menu item the store does not sell.
    fn unknown_menu_item(&self) -> pos_proto::MenuItemId;

    /// A store identifier the cases may use.
    fn store_id(&self) -> StoreId;
}

/// Supplies a [`ClockSource`] the suite can move.
pub trait ClockSourceHarness: Send + Sync {
    /// The implementation under test.
    type Clock: ClockSource;

    /// A clock reading `at`.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the fixture cannot be prepared.
    fn fresh(&self, at: pos_proto::Timestamp) -> Setup<Self::Clock>;

    /// Moves the clock to `at`, including backwards — which is what an NTP step does, and the
    /// case an implementation is most likely to have got wrong.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the fixture cannot be prepared.
    fn set(&self, clock: &Self::Clock, at: pos_proto::Timestamp) -> Setup<()>;
}

/// Supplies an [`IdGenerator`].
pub trait IdGeneratorHarness: Send + Sync {
    /// The implementation under test.
    type Generator: IdGenerator;

    /// A generator whose clock reads `at`.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the fixture cannot be prepared.
    fn fresh(&self, at: pos_proto::Timestamp) -> Setup<Self::Generator>;

    /// Moves the generator's clock, including backwards.
    ///
    /// The monotonicity obligation is only interesting when time goes backwards, and time does
    /// go backwards: `docs/architecture.md` §8 has stores correcting drift over SNTP.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] if the fixture cannot be prepared.
    fn set(&self, generator: &Self::Generator, at: pos_proto::Timestamp) -> Setup<()>;
}
