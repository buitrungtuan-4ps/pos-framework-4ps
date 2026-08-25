// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The seventeen boundaries between the framework and the outside world.
//!
//! Every external system — database, broker, printer, terminal, marketplace, tax
//! authority — is one implementation of one trait defined here. The list is fixed
//! by `docs/adr/0021-corrected-port-list.md`; adding a seventeenth needs an ADR
//! merged first.
//!
//! # Shape
//!
//! Ports are small and role-shaped. An adapter that has to write
//! `unimplemented!()` means the port is wrong, not the adapter
//! (`docs/design-principles.md`, interface segregation).
//!
//! Two of the seventeen are synchronous and are re-exported from `pos-proto`
//! rather than defined here, so that there is exactly one definition of each:
//! `ClockSource` and `IdGenerator`. The other fifteen are asynchronous,
//! declared with native `async fn` in trait — no procedural macro, no boxing on
//! the happy path. Where a family needs runtime selection between several
//! compiled-in adapters, this crate also carries a hand-written object-safe
//! mirror of that trait. See `docs/adr/0013-async-strategy.md`.
//!
//! # `pos-core` does not depend on this crate
//!
//! That is deliberate and it is what makes "the domain performs no I/O" a
//! property of the dependency graph rather than a lint. Do not add the edge.
//!
//! # Contract tests
//!
//! Each port ships a shared test suite that every implementation must pass; that
//! is what makes "swappable" a verified fact rather than a claim. The suites live
//! in `pos-contract-tests`, which is not subject to this crate's dependency
//! allow-list because it needs an executor.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

pub mod blob_store;
pub mod cloud_sync;
pub mod config_store;
pub mod delivery;
pub mod dynamic;
pub mod erp;
pub mod error;
pub mod event_store;
pub mod fiscalization;
pub mod intake_ledger;
pub mod key_vault;
pub mod message_link;
pub mod metrics_sink;
pub mod order_in;
pub mod payment;
pub mod printer;
pub mod shipping;
pub mod signer;
pub mod tx;

pub use blob_store::{BlobKey, BlobKeyError, BlobStore};
pub use cloud_sync::{ActivationGrant, CloudSync};
pub use config_store::{ConfigDelta, ConfigDocument, ConfigSnapshot, ConfigStore, ConfigUpdate};
pub use delivery::{BusyMode, DeliveryVendor, PendingDecision, PrepTime, VendorOrderRef};
pub use dynamic::{
    BoxFuture, DynDeliveryVendor, DynErpSink, DynFiscalization, DynPaymentTerminal,
    DynPrinterDriver,
};
pub use erp::{AccountCode, ErpBatch, ErpLine, ErpPostingRef, ErpSink};
pub use error::{PortError, PortName};
pub use event_store::{AppendOutcome, EventQuery, EventStore, OutboxPosition, OutboxRecord};
pub use fiscalization::{
    Fiscalization, InvoiceBuyer, InvoiceLine, InvoiceNumber, InvoiceRange, InvoiceRequest,
    IssuedInvoice, ReconciliationReport,
};
pub use intake_ledger::{IntakeLedger, IntakeRecord};
pub use key_vault::{KeyVault, Secret, SecretName};
pub use message_link::{LinkCapacity, MessageLink, PublishOutcome};
pub use metrics_sink::{
    MetricLabel, MetricLabelValue, MetricName, MetricNameError, MetricSample, MetricUnit,
    MetricsSink,
};
pub use order_in::{ExternalReference, InboundOrder, InboundOrderLine, OrderAcceptance, OrderIn};
pub use payment::{PaymentAttempt, PaymentReference, PaymentRequest, PaymentTerminal};
pub use printer::{
    CodePage, PrintBlock, PrintDocument, PrintJob, PrinterCapabilities, PrinterConnection,
    PrinterDriver, PrinterStatus, TextStyle,
};
pub use shipping::{
    CourierJobRef, DeliveryContact, DeliveryRequest, Shipment, ShipmentUpdate, ShippingDispatch,
};
pub use signer::{KeyId, PublicKey, Signature, Signer};
pub use tx::{Transactional, TxContext};

/// The two synchronous ports, re-exported so the seventeen-port list has one definition of
/// each rather than two that can drift.
///
/// They are defined in `pos-proto` because `pos-core` needs them and must not depend on this
/// crate — see [ADR-0013](../../../docs/adr/0013-async-strategy.md).
pub use pos_proto::determinism::{ClockSource, IdGenerator};
