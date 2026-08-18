// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The vendor fakes: fiscalization, delivery, shipping, ERP, and order intake.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pos_ports::delivery::{BusyMode, DeliveryVendor, PendingDecision, PrepTime, VendorOrderRef};
use pos_ports::erp::{AccountCode, ErpBatch, ErpPostingRef, ErpSink};
use pos_ports::fiscalization::{
    Fiscalization, InvoiceNumber, InvoiceRange, InvoiceRequest, IssuedInvoice, ReconciliationReport,
};
use pos_ports::order_in::{ExternalReference, InboundOrder, OrderAcceptance, OrderIn};
use pos_ports::shipping::{CourierJobRef, DeliveryRequest, Shipment, ShippingDispatch};
use pos_ports::{PortError, PortName};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::wire_enum::Open;
use pos_proto::{
    BillId, BusinessDate, CalendarDate, MenuItemId, OrderId, ReasonCodeId, SalesChannel,
    ShipmentId, ShipmentStatus, StoreId, Timestamp, Ulid,
};

use crate::lock;

// -----------------------------------------------------------------------------------------------
// Fiscalization
// -----------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct FiscalState {
    /// Numbers allocated and not yet consumed, in issue order.
    available: Vec<InvoiceNumber>,
    /// Issued invoices by bill, which is how the fake deduplicates.
    issued: BTreeMap<BillId, IssuedInvoice>,
    connected: bool,
    next_range: u32,
}

/// An in-memory `Fiscalization`.
///
/// The important behaviour: [`Self::issue`] consumes from `available` and never talks to the
/// authority, so it works with `connected == false`. That is what makes offline issuance testable at
/// all — and the case that checks it is the one standing between the offline-first promise and a
/// store with no internet and a customer waiting.
#[derive(Debug, Clone)]
pub struct FakeFiscal {
    state: Arc<Mutex<FiscalState>>,
}

impl Default for FakeFiscal {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(FiscalState {
                connected: true,
                ..FiscalState::default()
            })),
        }
    }
}

impl FakeFiscal {
    /// A country module with no ranges allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cuts the authority off.
    pub fn disconnect(&self) {
        lock(&self.state).connected = false;
    }

    /// Restores the connection, and marks queued invoices submitted.
    pub fn reconnect(&self) {
        let mut state = lock(&self.state);
        state.connected = true;
        for invoice in state.issued.values_mut() {
            invoice.submitted = true;
            invoice.authority_reference = Some("fake-authority-ref".into());
        }
    }
}

impl Fiscalization for FakeFiscal {
    async fn allocate_range(
        &self,
        store_id: StoreId,
        count: core::num::NonZeroU32,
    ) -> Result<InvoiceRange, PortError> {
        let mut state = lock(&self.state);
        if !state.connected {
            return Err(PortError::unavailable(
                PortName::Fiscalization,
                "the authority cannot be reached",
            ));
        }
        let series = state.next_range;
        state.next_range = state.next_range.saturating_add(1);
        let numbers: Vec<InvoiceNumber> = (0..count.get())
            .map(|index| InvoiceNumber::new(format!("1C26TAA/{series:03}{index:06}")))
            .collect();
        state.available.clone_from(&numbers);
        Ok(InvoiceRange {
            store_id,
            range_id: format!("range-{series}").into(),
            numbers,
            issued: 0,
        })
    }

    async fn issue(&self, request: &InvoiceRequest) -> Result<IssuedInvoice, PortError> {
        let mut state = lock(&self.state);
        if let Some(existing) = state.issued.get(&request.bill_id) {
            // One bill, one number. Two numbers for one bill is a compliance incident.
            return Ok(existing.clone());
        }
        if state.available.is_empty() {
            return Err(PortError::resource_exhausted(
                PortName::Fiscalization,
                "no invoice numbers remain in the allocated range",
            ));
        }
        let invoice_number = state.available.remove(0);
        let connected = state.connected;
        let invoice = IssuedInvoice {
            bill_id: request.bill_id,
            invoice_number,
            issued_at: Timestamp::EPOCH,
            submitted: connected,
            authority_reference: connected.then(|| "fake-authority-ref".into()),
        };
        state.issued.insert(request.bill_id, invoice.clone());
        Ok(invoice)
    }

    async fn look_up(
        &self,
        invoice_number: &InvoiceNumber,
    ) -> Result<Option<IssuedInvoice>, PortError> {
        let state = lock(&self.state);
        Ok(state
            .issued
            .values()
            .find(|invoice| &invoice.invoice_number == invoice_number)
            .cloned())
    }

    async fn reconcile(
        &self,
        _store_id: StoreId,
        _on: CalendarDate,
    ) -> Result<ReconciliationReport, PortError> {
        let state = lock(&self.state);
        if !state.connected {
            return Err(PortError::unavailable(
                PortName::Fiscalization,
                "the authority cannot be reached",
            ));
        }
        Ok(ReconciliationReport {
            unsubmitted: state
                .issued
                .values()
                .filter(|invoice| !invoice.submitted)
                .map(|invoice| invoice.invoice_number.clone())
                .collect(),
            // A fake authority holds exactly what the fake store issued, so this direction is always
            // empty here. Which is honest: the case checks that a store's own invoice is not reported
            // as unknown to it, and a real adapter's answer comes from the authority.
            unknown_locally: Vec::new(),
        })
    }
}

// -----------------------------------------------------------------------------------------------
// DeliveryVendor
// -----------------------------------------------------------------------------------------------

/// What the fake vendor believes about one order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VendorDecision {
    Pending { expired: bool },
    Accepted,
    Rejected,
    Ready,
}

#[derive(Debug, Default)]
struct VendorState {
    orders: BTreeMap<VendorOrderRef, VendorDecision>,
    next: u32,
    busy: Option<BusyMode>,
}

/// An in-memory `DeliveryVendor`.
#[derive(Debug, Clone, Default)]
pub struct FakeDeliveryVendor {
    state: Arc<Mutex<VendorState>>,
}

impl FakeDeliveryVendor {
    /// A vendor with no pending orders.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stages an order awaiting a decision.
    #[must_use]
    pub fn stage_order(&self) -> VendorOrderRef {
        self.stage(false)
    }

    /// Stages an order whose window has already closed.
    #[must_use]
    pub fn stage_expired_order(&self) -> VendorOrderRef {
        self.stage(true)
    }

    fn stage(&self, expired: bool) -> VendorOrderRef {
        let mut state = lock(&self.state);
        state.next = state.next.saturating_add(1);
        let reference = VendorOrderRef::new(format!("GF-{}", state.next));
        state
            .orders
            .insert(reference.clone(), VendorDecision::Pending { expired });
        reference
    }

    /// What the vendor currently believes about whether the store is open.
    ///
    /// `Open` until told otherwise, which is what a marketplace assumes about a restaurant that has
    /// never said anything.
    #[must_use]
    pub fn busy_mode(&self) -> BusyMode {
        lock(&self.state).busy.unwrap_or(BusyMode::Open)
    }

    fn decide(
        state: &mut VendorState,
        reference: &VendorOrderRef,
        to: VendorDecision,
    ) -> Result<(), PortError> {
        let Some(current) = state.orders.get(reference).copied() else {
            return Err(PortError::not_found(
                PortName::DeliveryVendor,
                "the vendor does not recognise this order",
            ));
        };
        match (current, to) {
            // Repeating a decision is one decision — marketplace APIs retry constantly and none
            // promise exactly-once.
            (from, wanted) if from == wanted => Ok(()),
            (VendorDecision::Pending { expired: true }, _) => Err(PortError::failed_precondition(
                PortName::DeliveryVendor,
                "the decision window has closed",
            )),
            (VendorDecision::Pending { expired: false }, VendorDecision::Ready) => {
                Err(PortError::failed_precondition(
                    PortName::DeliveryVendor,
                    "the order was never accepted",
                ))
            }
            (VendorDecision::Pending { expired: false }, wanted) => {
                state.orders.insert(reference.clone(), wanted);
                Ok(())
            }
            (VendorDecision::Accepted, VendorDecision::Ready) => {
                state
                    .orders
                    .insert(reference.clone(), VendorDecision::Ready);
                Ok(())
            }
            _ => Err(PortError::failed_precondition(
                PortName::DeliveryVendor,
                "that would contradict a decision already made",
            )),
        }
    }
}

impl DeliveryVendor for FakeDeliveryVendor {
    fn vendor_name(&self) -> &'static str {
        "fake"
    }

    async fn accept(
        &self,
        vendor_order_ref: &VendorOrderRef,
        _prep_time: PrepTime,
    ) -> Result<(), PortError> {
        let mut state = lock(&self.state);
        Self::decide(&mut state, vendor_order_ref, VendorDecision::Accepted)
    }

    async fn reject(
        &self,
        vendor_order_ref: &VendorOrderRef,
        _reason_code_id: ReasonCodeId,
    ) -> Result<(), PortError> {
        let mut state = lock(&self.state);
        Self::decide(&mut state, vendor_order_ref, VendorDecision::Rejected)
    }

    async fn mark_ready(&self, vendor_order_ref: &VendorOrderRef) -> Result<(), PortError> {
        let mut state = lock(&self.state);
        Self::decide(&mut state, vendor_order_ref, VendorDecision::Ready)
    }

    async fn set_busy(&self, _store_id: StoreId, mode: BusyMode) -> Result<(), PortError> {
        // Idempotent, because the store re-asserts on reconnect rather than tracking whether it
        // already did.
        lock(&self.state).busy = Some(mode);
        Ok(())
    }

    async fn pending_decisions(
        &self,
        _store_id: StoreId,
    ) -> Result<Vec<PendingDecision>, PortError> {
        let state = lock(&self.state);
        Ok(state
            .orders
            .iter()
            .filter(|(_, decision)| matches!(decision, VendorDecision::Pending { .. }))
            .map(|(reference, _)| PendingDecision {
                vendor_order_ref: reference.clone(),
                order_id: OrderId::new(Ulid::from_u128(1)),
                decide_by: Timestamp::EPOCH,
            })
            .collect())
    }
}

// -----------------------------------------------------------------------------------------------
// ShippingDispatch
// -----------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct CourierState {
    jobs: BTreeMap<CourierJobRef, Shipment>,
    by_shipment: BTreeMap<ShipmentId, CourierJobRef>,
    next: u32,
}

/// An in-memory `ShippingDispatch`.
#[derive(Debug, Clone, Default)]
pub struct FakeShipping {
    state: Arc<Mutex<CourierState>>,
}

impl FakeShipping {
    /// A courier with no jobs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves a job to delivered.
    pub fn complete(&self, job: &CourierJobRef) {
        if let Some(shipment) = lock(&self.state).jobs.get_mut(job) {
            shipment.status = Open::from_known(ShipmentStatus::Completed);
        }
    }
}

impl ShippingDispatch for FakeShipping {
    fn courier_name(&self) -> &'static str {
        "fake"
    }

    async fn create_delivery(&self, request: &DeliveryRequest) -> Result<Shipment, PortError> {
        let mut state = lock(&self.state);
        if let Some(existing) = state
            .by_shipment
            .get(&request.shipment_id)
            .and_then(|reference| state.jobs.get(reference))
        {
            // One shipment identifier, one rider. A retry after a timeout must not put two on the
            // same order, which the store then pays for twice.
            return Ok(existing.clone());
        }
        state.next = state.next.saturating_add(1);
        let courier_job_ref = CourierJobRef::new(format!("AHA-{}", state.next));
        let shipment = Shipment {
            shipment_id: request.shipment_id,
            courier_job_ref: courier_job_ref.clone(),
            status: Open::from_known(ShipmentStatus::Accepted),
            fee: Some(Money::new(CurrencyCode::VND, 25_000)),
            updated_at: Timestamp::EPOCH,
        };
        state
            .by_shipment
            .insert(request.shipment_id, courier_job_ref.clone());
        state.jobs.insert(courier_job_ref, shipment.clone());
        Ok(shipment)
    }

    async fn cancel(&self, courier_job_ref: &CourierJobRef) -> Result<Shipment, PortError> {
        let mut state = lock(&self.state);
        let Some(shipment) = state.jobs.get_mut(courier_job_ref) else {
            return Err(PortError::not_found(
                PortName::ShippingDispatch,
                "the courier does not recognise this job",
            ));
        };
        match shipment.status.known() {
            // A courier who has delivered cannot un-deliver, and a cancel that looks successful
            // leaves the store expecting a refund it will not get.
            ShipmentStatus::Completed => Err(PortError::failed_precondition(
                PortName::ShippingDispatch,
                "the job has already completed",
            )),
            // Cancellation is retried, so a repeat succeeds.
            ShipmentStatus::Cancelled => Ok(shipment.clone()),
            // Enumerated rather than wildcarded, because `wildcard_enum_match_arm` is denied for a
            // reason: a courier adding SHIPMENT_STATUS_RETURNED_TO_SENDER would otherwise be
            // silently treated as cancellable, and this is where that decision belongs.
            ShipmentStatus::Unspecified | ShipmentStatus::Accepted | ShipmentStatus::InTransit => {
                shipment.status = Open::from_known(ShipmentStatus::Cancelled);
                Ok(shipment.clone())
            }
        }
    }

    async fn track(&self, courier_job_ref: &CourierJobRef) -> Result<Shipment, PortError> {
        let state = lock(&self.state);
        state.jobs.get(courier_job_ref).cloned().ok_or_else(|| {
            PortError::not_found(
                PortName::ShippingDispatch,
                "the courier does not recognise this job",
            )
        })
    }
}

// -----------------------------------------------------------------------------------------------
// ErpSink
// -----------------------------------------------------------------------------------------------

/// The account code a fake ERP accepts.
pub const KNOWN_ACCOUNT: &str = "511";

#[derive(Debug, Default)]
struct ErpState {
    /// One entry per (store, trading day). Replaced by a higher revision rather than appended to,
    /// which is the obligation with the worst failure attached.
    posted: BTreeMap<(StoreId, BusinessDate), ErpPostingRef>,
}

/// An in-memory `ErpSink`.
#[derive(Debug, Clone, Default)]
pub struct FakeErp {
    state: Arc<Mutex<ErpState>>,
}

impl FakeErp {
    /// An ERP with nothing posted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An account code this ERP accepts.
    #[must_use]
    pub fn known_account() -> AccountCode {
        AccountCode::new(KNOWN_ACCOUNT)
    }

    /// An account code it does not.
    #[must_use]
    pub fn unknown_account() -> AccountCode {
        AccountCode::new("999-not-in-the-chart")
    }
}

impl ErpSink for FakeErp {
    fn erp_name(&self) -> &'static str {
        "fake"
    }

    async fn post(&self, batch: &ErpBatch) -> Result<ErpPostingRef, PortError> {
        // Validated before anything is written: a batch is posted whole or not at all, and half a
        // day's revenue in an accounting period is worse than none because none is visibly missing.
        for line in &batch.lines {
            if line.account_code().as_str() != KNOWN_ACCOUNT {
                return Err(PortError::invalid_argument(
                    PortName::ErpSink,
                    "the ERP does not know that account code",
                ));
            }
        }

        let mut state = lock(&self.state);
        let key = (batch.store_id, batch.business_date);
        if let Some(existing) = state.posted.get(&key) {
            if existing.revision == batch.revision {
                // The same revision again. Success with the same document, so a retried nightly job
                // is harmless.
                return Ok(existing.clone());
            }
            if existing.revision > batch.revision {
                return Err(PortError::already_exists(
                    PortName::ErpSink,
                    "a later revision of this day has already posted",
                ));
            }
        }
        let posting = ErpPostingRef {
            document_ref: format!("DOC-{}", batch.idempotency_key()).into(),
            revision: batch.revision,
        };
        // Insert replaces, which is the supersession rule. Appending here is what would double-count
        // a reposted day.
        state.posted.insert(key, posting.clone());
        Ok(posting)
    }

    async fn posted(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
    ) -> Result<Option<ErpPostingRef>, PortError> {
        let state = lock(&self.state);
        Ok(state.posted.get(&(store_id, business_date)).cloned())
    }
}

// -----------------------------------------------------------------------------------------------
// OrderIn
// -----------------------------------------------------------------------------------------------

/// The menu item a fake intake sells, and its price.
#[must_use]
pub fn known_menu_item() -> (MenuItemId, Money) {
    (
        MenuItemId::new(Ulid::from_u128(1)),
        Money::new(CurrencyCode::VND, 120_000),
    )
}

/// A menu item it does not.
#[must_use]
pub fn unknown_menu_item() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(u128::MAX))
}

#[derive(Debug, Default)]
struct IntakeState {
    /// Keyed by channel token **and** reference. Two channels using the same reference are two
    /// orders — nothing stops a marketplace and a till both numbering an order 1001.
    accepted: BTreeMap<(String, ExternalReference), OrderAcceptance>,
    next: u128,
}

/// An in-memory `OrderIn`.
///
/// The one fake standing in for *our* code rather than a vendor's, since `OrderIn` is a driving port.
#[derive(Debug, Clone, Default)]
pub struct FakeIntake {
    state: Arc<Mutex<IntakeState>>,
}

impl FakeIntake {
    /// An intake with no orders.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn key(
        channel: &Open<SalesChannel>,
        reference: &ExternalReference,
    ) -> (String, ExternalReference) {
        (channel.as_wire().to_owned(), reference.clone())
    }
}

impl OrderIn for FakeIntake {
    async fn submit(&self, order: &InboundOrder) -> Result<OrderAcceptance, PortError> {
        if order.lines.is_empty() {
            return Err(PortError::invalid_argument(
                PortName::OrderIn,
                "an order must have at least one line",
            ));
        }
        let (known, price) = known_menu_item();
        for line in &order.lines {
            if line.menu_item_id != known {
                // Refused, never substituted. Guessing the closest match is how a kitchen makes the
                // wrong thing and a customer is charged for it.
                return Err(PortError::invalid_argument(
                    PortName::OrderIn,
                    "the store does not sell that menu item",
                ));
            }
        }

        let key = Self::key(&order.sales_channel, &order.external_reference);
        let mut state = lock(&self.state);
        if let Some(existing) = state.accepted.get(&key) {
            let mut replay = existing.clone();
            replay.created = false;
            return Ok(replay);
        }

        state.next = state.next.saturating_add(1);
        // The store's price wins, and a quote that differs is reported rather than honoured or
        // refused: honouring loses margin on every order until somebody notices, refusing loses a
        // sale the store wanted.
        let repriced = order
            .lines
            .iter()
            .any(|line| line.quoted_unit_price.is_some_and(|quoted| quoted != price));
        let acceptance = OrderAcceptance {
            order_id: OrderId::new(Ulid::from_u128(state.next)),
            created: true,
            queue_number: None,
            total: price,
            repriced,
            awaiting_staff_confirmation: order.table_id.is_some(),
        };
        state.accepted.insert(key, acceptance.clone());
        Ok(acceptance)
    }

    async fn look_up(
        &self,
        _store_id: StoreId,
        sales_channel: Open<SalesChannel>,
        external_reference: &ExternalReference,
    ) -> Result<Option<OrderAcceptance>, PortError> {
        let state = lock(&self.state);
        Ok(state
            .accepted
            .get(&Self::key(&sales_channel, external_reference))
            .cloned())
    }
}
