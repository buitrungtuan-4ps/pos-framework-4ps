// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Object-safe mirrors for the four families that need runtime selection.
//!
//! # Why these exist
//!
//! Native `async fn` in a trait — the desugared `-> impl Future` form every port here uses — is
//! not `dyn`-compatible: the future's type depends on the implementation, so there is nothing for
//! a vtable to name. That is the price
//! [ADR-0013](../../../docs/adr/0013-async-strategy.md) accepted in exchange for having no
//! procedural macro inside the crate the dependency allow-list exists to protect.
//!
//! Four families genuinely need runtime selection, because which one is in use is a
//! configuration value rather than a compile-time fact: a store may print to three printers
//! from different manufacturers, a chain may run two acquirers, a tenant may sell on Grab and
//! ShopeeFood at once, and a deployment adds a country module without recompiling the domain.
//!
//! # How they work, and what they cost
//!
//! Each mirror declares the same methods returning
//! `Pin<Box<dyn Future<Output = …> + Send + '_>>`, and a blanket implementation bridges from the
//! ergonomic trait. **An adapter implements only the plain trait**; the mirror comes for free
//! and cannot drift, because a mirror method that stopped matching would fail to compile the
//! blanket impl rather than silently diverge.
//!
//! The cost is one `Box::pin` per call, on a path that is about to talk to a printer over USB or
//! an acquirer over the network. Measured against a syscall it is not measurable.
//!
//! # The eleven other ports deliberately have no mirror
//!
//! `EventStore`, `ConfigStore`, `MessageLink`, `BlobStore`, `MetricsSink`, `KeyVault`, `Signer`
//! and `OrderIn` are chosen once when a binary starts, by which adapter was compiled in, so
//! static dispatch is not a limitation. Adding a mirror for them would be paying `Box::pin` for
//! flexibility nothing uses. `EventStore` and `ConfigStore` could not have one anyway: an
//! associated `Tx` type has no meaning behind a trait object.

use core::future::Future;
use core::pin::Pin;

use pos_proto::wire_enum::Open;
use pos_proto::{BusinessDate, CalendarDate, PaymentOutcome, ReasonCodeId, StoreId};

use crate::delivery::{BusyMode, DeliveryVendor, PendingDecision, PrepTime, VendorOrderRef};
use crate::erp::{ErpBatch, ErpPostingRef, ErpSink};
use crate::error::PortError;
use crate::fiscalization::{
    Fiscalization, InvoiceNumber, InvoiceRange, InvoiceRequest, IssuedInvoice, ReconciliationReport,
};
use crate::payment::{PaymentAttempt, PaymentReference, PaymentRequest, PaymentTerminal};
use crate::printer::{PrintJob, PrinterCapabilities, PrinterDriver, PrinterStatus};

/// A boxed future, as a trait object must return.
///
/// Aliased so the signatures below read as signatures rather than as punctuation.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// [`PrinterDriver`] behind a trait object.
///
/// A store routes tickets to several printers of different makes, chosen by the configuration
/// tree, so the set is not known until a config version arrives.
pub trait DynPrinterDriver: Send + Sync {
    /// See [`PrinterDriver::capabilities`].
    fn capabilities(&self) -> PrinterCapabilities;
    /// See [`PrinterDriver::print`].
    fn print<'a>(&'a self, job: &'a PrintJob) -> BoxFuture<'a, Result<(), PortError>>;
    /// See [`PrinterDriver::status`].
    fn status(&self) -> BoxFuture<'_, Result<PrinterStatus, PortError>>;
    /// See [`PrinterDriver::open_drawer`].
    fn open_drawer(&self) -> BoxFuture<'_, Result<(), PortError>>;
}

impl<T: PrinterDriver> DynPrinterDriver for T {
    fn capabilities(&self) -> PrinterCapabilities {
        PrinterDriver::capabilities(self)
    }

    fn print<'a>(&'a self, job: &'a PrintJob) -> BoxFuture<'a, Result<(), PortError>> {
        Box::pin(PrinterDriver::print(self, job))
    }

    fn status(&self) -> BoxFuture<'_, Result<PrinterStatus, PortError>> {
        Box::pin(PrinterDriver::status(self))
    }

    fn open_drawer(&self) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(PrinterDriver::open_drawer(self))
    }
}

/// [`PaymentTerminal`] behind a trait object.
///
/// A chain may run more than one acquirer, and which one a store uses is configuration.
pub trait DynPaymentTerminal: Send + Sync {
    /// See [`PaymentTerminal::authorize`].
    fn authorize<'a>(
        &'a self,
        request: &'a PaymentRequest,
    ) -> BoxFuture<'a, Result<PaymentAttempt, PortError>>;
    /// See [`PaymentTerminal::look_up`].
    fn look_up<'a>(
        &'a self,
        reference: &'a PaymentReference,
    ) -> BoxFuture<'a, Result<PaymentAttempt, PortError>>;
    /// See [`PaymentTerminal::void`].
    fn void<'a>(
        &'a self,
        reference: &'a PaymentReference,
    ) -> BoxFuture<'a, Result<PaymentAttempt, PortError>>;
}

impl<T: PaymentTerminal> DynPaymentTerminal for T {
    fn authorize<'a>(
        &'a self,
        request: &'a PaymentRequest,
    ) -> BoxFuture<'a, Result<PaymentAttempt, PortError>> {
        Box::pin(PaymentTerminal::authorize(self, request))
    }

    fn look_up<'a>(
        &'a self,
        reference: &'a PaymentReference,
    ) -> BoxFuture<'a, Result<PaymentAttempt, PortError>> {
        Box::pin(PaymentTerminal::look_up(self, reference))
    }

    fn void<'a>(
        &'a self,
        reference: &'a PaymentReference,
    ) -> BoxFuture<'a, Result<PaymentAttempt, PortError>> {
        Box::pin(PaymentTerminal::void(self, reference))
    }
}

/// [`DeliveryVendor`] behind a trait object.
///
/// A tenant sells on several marketplaces at once, so this one is a collection rather than a
/// choice.
pub trait DynDeliveryVendor: Send + Sync {
    /// See [`DeliveryVendor::vendor_name`].
    fn vendor_name(&self) -> &'static str;
    /// See [`DeliveryVendor::accept`].
    fn accept<'a>(
        &'a self,
        vendor_order_ref: &'a VendorOrderRef,
        prep_time: PrepTime,
    ) -> BoxFuture<'a, Result<(), PortError>>;
    /// See [`DeliveryVendor::reject`].
    fn reject<'a>(
        &'a self,
        vendor_order_ref: &'a VendorOrderRef,
        reason_code_id: ReasonCodeId,
    ) -> BoxFuture<'a, Result<(), PortError>>;
    /// See [`DeliveryVendor::mark_ready`].
    fn mark_ready<'a>(
        &'a self,
        vendor_order_ref: &'a VendorOrderRef,
    ) -> BoxFuture<'a, Result<(), PortError>>;
    /// See [`DeliveryVendor::set_busy`].
    fn set_busy(&self, store_id: StoreId, mode: BusyMode) -> BoxFuture<'_, Result<(), PortError>>;
    /// See [`DeliveryVendor::pending_decisions`].
    fn pending_decisions(
        &self,
        store_id: StoreId,
    ) -> BoxFuture<'_, Result<Vec<PendingDecision>, PortError>>;
}

impl<T: DeliveryVendor> DynDeliveryVendor for T {
    fn vendor_name(&self) -> &'static str {
        DeliveryVendor::vendor_name(self)
    }

    fn accept<'a>(
        &'a self,
        vendor_order_ref: &'a VendorOrderRef,
        prep_time: PrepTime,
    ) -> BoxFuture<'a, Result<(), PortError>> {
        Box::pin(DeliveryVendor::accept(self, vendor_order_ref, prep_time))
    }

    fn reject<'a>(
        &'a self,
        vendor_order_ref: &'a VendorOrderRef,
        reason_code_id: ReasonCodeId,
    ) -> BoxFuture<'a, Result<(), PortError>> {
        Box::pin(DeliveryVendor::reject(
            self,
            vendor_order_ref,
            reason_code_id,
        ))
    }

    fn mark_ready<'a>(
        &'a self,
        vendor_order_ref: &'a VendorOrderRef,
    ) -> BoxFuture<'a, Result<(), PortError>> {
        Box::pin(DeliveryVendor::mark_ready(self, vendor_order_ref))
    }

    fn set_busy(&self, store_id: StoreId, mode: BusyMode) -> BoxFuture<'_, Result<(), PortError>> {
        Box::pin(DeliveryVendor::set_busy(self, store_id, mode))
    }

    fn pending_decisions(
        &self,
        store_id: StoreId,
    ) -> BoxFuture<'_, Result<Vec<PendingDecision>, PortError>> {
        Box::pin(DeliveryVendor::pending_decisions(self, store_id))
    }
}

/// [`Fiscalization`] behind a trait object.
///
/// A cell serves one country, but a deployment adds a country module without recompiling the
/// domain, and `examples/fiscal-skeleton` exists so somebody outside this repository can write
/// the next one.
pub trait DynFiscalization: Send + Sync {
    /// See [`Fiscalization::allocate_range`].
    fn allocate_range(
        &self,
        store_id: StoreId,
        count: core::num::NonZeroU32,
    ) -> BoxFuture<'_, Result<InvoiceRange, PortError>>;
    /// See [`Fiscalization::issue`].
    fn issue<'a>(
        &'a self,
        request: &'a InvoiceRequest,
    ) -> BoxFuture<'a, Result<IssuedInvoice, PortError>>;
    /// See [`Fiscalization::look_up`].
    fn look_up<'a>(
        &'a self,
        invoice_number: &'a InvoiceNumber,
    ) -> BoxFuture<'a, Result<Option<IssuedInvoice>, PortError>>;
    /// See [`Fiscalization::reconcile`].
    fn reconcile(
        &self,
        store_id: StoreId,
        on: CalendarDate,
    ) -> BoxFuture<'_, Result<ReconciliationReport, PortError>>;
}

impl<T: Fiscalization> DynFiscalization for T {
    fn allocate_range(
        &self,
        store_id: StoreId,
        count: core::num::NonZeroU32,
    ) -> BoxFuture<'_, Result<InvoiceRange, PortError>> {
        Box::pin(Fiscalization::allocate_range(self, store_id, count))
    }

    fn issue<'a>(
        &'a self,
        request: &'a InvoiceRequest,
    ) -> BoxFuture<'a, Result<IssuedInvoice, PortError>> {
        Box::pin(Fiscalization::issue(self, request))
    }

    fn look_up<'a>(
        &'a self,
        invoice_number: &'a InvoiceNumber,
    ) -> BoxFuture<'a, Result<Option<IssuedInvoice>, PortError>> {
        Box::pin(Fiscalization::look_up(self, invoice_number))
    }

    fn reconcile(
        &self,
        store_id: StoreId,
        on: CalendarDate,
    ) -> BoxFuture<'_, Result<ReconciliationReport, PortError>> {
        Box::pin(Fiscalization::reconcile(self, store_id, on))
    }
}

/// [`ErpSink`] behind a trait object.
///
/// Not one of the four families ADR-0013 names, and included anyway for one reason: a chain
/// posting to two ERPs during a migration is an ordinary state of affairs, and it lasts months.
/// The cost is a `Box::pin` on a nightly job.
pub trait DynErpSink: Send + Sync {
    /// See [`ErpSink::erp_name`].
    fn erp_name(&self) -> &'static str;
    /// See [`ErpSink::post`].
    fn post<'a>(&'a self, batch: &'a ErpBatch) -> BoxFuture<'a, Result<ErpPostingRef, PortError>>;
    /// See [`ErpSink::posted`].
    fn posted(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
    ) -> BoxFuture<'_, Result<Option<ErpPostingRef>, PortError>>;
}

impl<T: ErpSink> DynErpSink for T {
    fn erp_name(&self) -> &'static str {
        ErpSink::erp_name(self)
    }

    fn post<'a>(&'a self, batch: &'a ErpBatch) -> BoxFuture<'a, Result<ErpPostingRef, PortError>> {
        Box::pin(ErpSink::post(self, batch))
    }

    fn posted(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
    ) -> BoxFuture<'_, Result<Option<ErpPostingRef>, PortError>> {
        Box::pin(ErpSink::posted(self, store_id, business_date))
    }
}

/// Keeps the compiler honest about what these mirrors are for.
///
/// A trait can compile perfectly and still not be usable behind `dyn` — an added method taking
/// `impl Trait`, or returning a bare `impl Future`, breaks object safety without touching any
/// call site. These assertions fail at compile time if that happens, which is the only way this
/// module's entire purpose stays true.
const _: () = {
    const fn assert_dyn_compatible<T: ?Sized>() {}
    let _ = assert_dyn_compatible::<dyn DynPrinterDriver>;
    let _ = assert_dyn_compatible::<dyn DynPaymentTerminal>;
    let _ = assert_dyn_compatible::<dyn DynDeliveryVendor>;
    let _ = assert_dyn_compatible::<dyn DynFiscalization>;
    let _ = assert_dyn_compatible::<dyn DynErpSink>;
};

/// A convenience for the unrecognised-outcome check the payment mirror's callers all need.
///
/// Here rather than on [`PaymentAttempt`] because it is about a *token*, not about an attempt: a
/// registry dispatching by vendor sees the token before it has an attempt to hang it on.
#[must_use]
pub fn outcome_needs_reconciliation(outcome: &Open<PaymentOutcome>) -> bool {
    !matches!(
        outcome.known(),
        PaymentOutcome::Captured | PaymentOutcome::Declined
    )
}
