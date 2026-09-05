// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The one failure type every port returns.
//!
//! One type rather than seventeen, because the framework asks the same three questions
//! of every adapter failure — retry or not, park it or not, what to tell the operator
//! — and per-port enums would each need that classification bolted on separately. See
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §1 for the alternatives that were
//! rejected.
//!
//! The status vocabulary is [`ErrorStatus`], reused from `pos-proto` rather than
//! invented here, so an adapter failure already speaks the same language as the HTTP
//! surface in `docs/naming-and-api.md` and needs no translation table.

use core::fmt;

use pos_proto::error::ErrorStatus;
use pos_proto::wire_enum::WireEnum;

/// Which port produced a failure.
///
/// An enum rather than a string so metrics, the error mailbox, and the per-adapter
/// latency charts required by `docs/roadmap.md` P11 can partition by port without
/// matching text. The list is fixed by
/// [ADR-0021](../../../docs/adr/0021-corrected-port-list.md) as amended by
/// [ADR-0053](../../../docs/adr/0053-cloud-sync-port.md) (`CloudSync`, the seventeenth) and
/// [ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md) (`DeviceRegistry`, the
/// eighteenth), [ADR-0064](../../../docs/adr/0064-edge-order-in.md) (`IntakeLedger`, the
/// nineteenth) and [ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md) (`SubjectStore`, the
/// twentieth); a twenty-first variant needs an ADR first.
///
/// `IntakeLedger` is the odd one: ADR-0064 called it a port when it landed, but it was given no
/// variant here — so [`crate::IntakeLedger`] had no suite and no row in `docs/architecture.md` §5,
/// and *nothing noticed*, because the guard that enforces "every port has a suite" iterates
/// [`Self::ALL`] and can only see what is registered here. Registering it is what puts it back
/// under that guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PortName {
    /// [`crate::EventStore`].
    EventStore,
    /// [`crate::ConfigStore`].
    ConfigStore,
    /// [`crate::MessageLink`].
    MessageLink,
    /// [`crate::BlobStore`].
    BlobStore,
    /// [`crate::MetricsSink`].
    MetricsSink,
    /// [`crate::Signer`].
    Signer,
    /// [`crate::KeyVault`].
    KeyVault,
    /// [`pos_proto::ClockSource`].
    ClockSource,
    /// [`pos_proto::IdGenerator`].
    IdGenerator,
    /// [`crate::PrinterDriver`].
    PrinterDriver,
    /// [`crate::PaymentTerminal`].
    PaymentTerminal,
    /// [`crate::Fiscalization`].
    Fiscalization,
    /// [`crate::DeliveryVendor`].
    DeliveryVendor,
    /// [`crate::ShippingDispatch`].
    ShippingDispatch,
    /// [`crate::ErpSink`].
    ErpSink,
    /// [`crate::OrderIn`].
    OrderIn,
    /// [`crate::CloudSync`].
    CloudSync,
    /// [`crate::DeviceRegistry`].
    DeviceRegistry,
    /// [`crate::IntakeLedger`].
    IntakeLedger,
    /// [`crate::SubjectStore`].
    SubjectStore,
}

impl PortName {
    /// Every port, in the order [ADR-0021](../../../docs/adr/0021-corrected-port-list.md)
    /// tabulates them.
    ///
    /// Used by the contract-test matrix to assert that no port ships without a suite.
    pub const ALL: &'static [Self] = &[
        Self::EventStore,
        Self::ConfigStore,
        Self::MessageLink,
        Self::BlobStore,
        Self::MetricsSink,
        Self::Signer,
        Self::KeyVault,
        Self::ClockSource,
        Self::IdGenerator,
        Self::PrinterDriver,
        Self::PaymentTerminal,
        Self::Fiscalization,
        Self::DeliveryVendor,
        Self::ShippingDispatch,
        Self::ErpSink,
        Self::OrderIn,
        Self::CloudSync,
        Self::DeviceRegistry,
        Self::IntakeLedger,
        Self::SubjectStore,
    ];

    /// The port's name in `snake_case`, for metric labels and log fields.
    ///
    /// `snake_case` because `docs/adr/0010-naming-standard.md` applies at every
    /// boundary, and a metric label is a boundary.
    #[must_use]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::EventStore => "event_store",
            Self::ConfigStore => "config_store",
            Self::MessageLink => "message_link",
            Self::BlobStore => "blob_store",
            Self::MetricsSink => "metrics_sink",
            Self::Signer => "signer",
            Self::KeyVault => "key_vault",
            Self::ClockSource => "clock_source",
            Self::IdGenerator => "id_generator",
            Self::PrinterDriver => "printer_driver",
            Self::PaymentTerminal => "payment_terminal",
            Self::Fiscalization => "fiscalization",
            Self::DeliveryVendor => "delivery_vendor",
            Self::ShippingDispatch => "shipping_dispatch",
            Self::ErpSink => "erp_sink",
            Self::OrderIn => "order_in",
            Self::CloudSync => "cloud_sync",
            Self::DeviceRegistry => "device_registry",
            Self::IntakeLedger => "intake_ledger",
            Self::SubjectStore => "subject_store",
        }
    }
}

impl fmt::Display for PortName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_label())
    }
}

/// A port call failed.
///
/// # What belongs in `message`
///
/// Enough for an operator to act: which resource, which vendor reference, which
/// limit. **Never** anything from `docs/pos-spec.md`'s personal-data set — no guest
/// name, phone, address, invoice buyer, or card detail. Port errors are logged, and
/// `AGENTS.md` §2 forbids personal data in logs. `pos_proto::pii::NoPii` cannot help
/// here because the message is a formatted string by the time it arrives; this is one
/// of the few rules in the framework that stays a review rule, and it is called out
/// so nobody assumes otherwise.
pub struct PortError {
    status: ErrorStatus,
    port: PortName,
    message: Box<str>,
    source: Option<Box<dyn core::error::Error + Send + Sync + 'static>>,
}

impl PortError {
    /// A failure with a status, a port, and a message.
    #[must_use]
    pub fn new(port: PortName, status: ErrorStatus, message: impl Into<Box<str>>) -> Self {
        Self {
            status,
            port,
            message: message.into(),
            source: None,
        }
    }

    /// Attaches the underlying cause.
    ///
    /// Kept separate from [`Self::new`] so an adapter must decide what the framework
    /// should conclude, rather than passing a vendor error up and leaving the
    /// classification to a caller who cannot see the protocol.
    #[must_use]
    pub fn with_source(mut self, source: impl core::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// The system is there but cannot serve the request now. Retryable.
    #[must_use]
    pub fn unavailable(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::Unavailable, message)
    }

    /// A bounded queue, range, or quota is full. Retryable, and the framework's
    /// standard back-pressure signal.
    ///
    /// Returning this is how a full queue stays bounded. The alternatives — growing,
    /// blocking the caller, or dropping silently — are the three ways
    /// `docs/capacity-and-reliability.md`'s guarantees get lost.
    #[must_use]
    pub fn resource_exhausted(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::ResourceExhausted, message)
    }

    /// The caller asked for something that does not exist. Not retryable.
    #[must_use]
    pub fn not_found(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::NotFound, message)
    }

    /// The request is malformed. Not retryable, and retrying it is how a bad request
    /// becomes an outage.
    #[must_use]
    pub fn invalid_argument(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::InvalidArgument, message)
    }

    /// The system is in the wrong state for this call — a closed shift, a settled
    /// bill, an exhausted invoice range. Not retryable without changing something.
    #[must_use]
    pub fn failed_precondition(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::FailedPrecondition, message)
    }

    /// The caller is authenticated but not allowed, or a signature did not verify. Not
    /// retryable, and never retried automatically — a rejected update signature that gets
    /// retried is an attacker's best case.
    #[must_use]
    pub fn permission_denied(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::PermissionDenied, message)
    }

    /// The thing being created is already there, and the existing one is not equivalent.
    ///
    /// Distinct from an idempotent replay, which succeeds. Use this only when the collision
    /// means the caller asked for something contradictory.
    #[must_use]
    pub fn already_exists(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::AlreadyExists, message)
    }

    /// The adapter is broken, or the vendor returned something the adapter does not
    /// understand. Not retryable.
    #[must_use]
    pub fn internal(port: PortName, message: impl Into<Box<str>>) -> Self {
        Self::new(port, ErrorStatus::Internal, message)
    }

    /// The AIP-193 status.
    #[must_use]
    pub const fn status(&self) -> ErrorStatus {
        self.status
    }

    /// Which port failed.
    #[must_use]
    pub const fn port(&self) -> PortName {
        self.port
    }

    /// The operator-facing message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether a caller should try again.
    ///
    /// Delegates to the status rather than living at the call site, because seventeen
    /// call sites would eventually disagree about whether `FailedPrecondition` is
    /// worth retrying. It is not.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.status.is_retryable()
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} ({})",
            self.port,
            self.message,
            self.status.as_wire()
        )
    }
}

/// Deliberately hand-written rather than derived.
///
/// A derived `Debug` prints `source` in full, and an adapter's error chain is exactly
/// where a connection string with a password ends up. This prints the chain's
/// *presence*, not its contents.
impl fmt::Debug for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortError")
            .field("port", &self.port)
            .field("status", &self.status)
            .field("message", &self.message)
            .field("source", &self.source.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl core::error::Error for PortError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|boxed| boxed.as_ref() as &(dyn core::error::Error + 'static))
    }
}

/// A port failure, converted for the HTTP surface.
///
/// The conversion exists so a handler never has to invent a status for an adapter
/// failure. `message` crosses into the response body because `docs/naming-and-api.md`
/// requires an actionable message; the `source` chain does not.
impl From<PortError> for pos_proto::error::ErrorResponse {
    fn from(value: PortError) -> Self {
        Self::new(value.status, String::from(value.message))
    }
}

#[cfg(test)]
mod tests {
    use super::{PortError, PortName};
    use pos_proto::error::ErrorStatus;
    use pos_proto::wire_enum::WireEnum;

    #[test]
    fn every_port_has_a_distinct_snake_case_label() {
        // A duplicate label would silently merge two ports' metrics, which is the kind
        // of mistake that is invisible until somebody debugs the wrong adapter.
        let mut labels: Vec<&str> = PortName::ALL.iter().map(|port| port.as_label()).collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count, "two ports share a label");

        for port in PortName::ALL {
            let label = port.as_label();
            assert!(
                label.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{label} is not snake_case"
            );
        }
    }

    #[test]
    fn the_port_list_is_the_twenty_adr_0021_and_its_amendments_name() {
        // Sixteen from ADR-0021, plus `CloudSync` (ADR-0053), `DeviceRegistry` (ADR-0091),
        // `IntakeLedger` (ADR-0064 — a port from the start, registered late) and `SubjectStore`
        // (ADR-0107). The number is asserted rather than described so that adding a port without its
        // ADR, its suite and its row in `docs/architecture.md` §5 fails here first.
        assert_eq!(PortName::ALL.len(), 20);
    }

    #[test]
    fn every_port_has_a_distinct_label() {
        // The labels partition metrics and log fields, so two ports sharing one would silently
        // merge two adapters' latency charts. Cheap to check, and it also catches a copy-paste in
        // `as_label` when a variant is added.
        let mut labels: Vec<&str> = PortName::ALL.iter().map(|port| port.as_label()).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count, "two ports share a label");
    }

    #[test]
    fn retryability_comes_from_the_status_not_the_call_site() {
        let full = PortError::resource_exhausted(PortName::MessageLink, "stream at capacity");
        assert!(full.is_retryable());

        let closed = PortError::failed_precondition(PortName::EventStore, "shift is closed");
        assert!(
            !closed.is_retryable(),
            "retrying a precondition failure turns a bad request into an outage"
        );
    }

    #[test]
    fn debug_does_not_print_the_source_chain() {
        // Regression guard for the reason this impl is hand-written: a derived Debug
        // would print a vendor error verbatim, and that is where connection strings
        // live.
        #[derive(Debug)]
        struct Secretive;
        impl core::fmt::Display for Secretive {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("postgres://user:hunter2@host/db")
            }
        }
        impl core::error::Error for Secretive {}

        let error =
            PortError::unavailable(PortName::ConfigStore, "connect failed").with_source(Secretive);
        let rendered = format!("{error:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("hunter2"));
    }

    #[test]
    fn display_names_the_port_and_the_status() {
        let error = PortError::not_found(PortName::BlobStore, "no such key");
        assert_eq!(format!("{error}"), "blob_store: no such key (NOT_FOUND)");
    }

    #[test]
    fn converting_to_a_response_keeps_the_status_and_drops_the_chain() {
        let error = PortError::invalid_argument(PortName::OrderIn, "menu_item_id is unknown");
        let response: pos_proto::error::ErrorResponse = error.into();
        assert_eq!(
            response.error.status.as_wire(),
            ErrorStatus::InvalidArgument.as_wire()
        );
        assert_eq!(response.error.message, "menu_item_id is unknown");
    }
}
