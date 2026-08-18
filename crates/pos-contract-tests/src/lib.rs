// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The shared suites every implementation of every port must pass.
//!
//! `docs/architecture.md` §5: *"This is what makes 'swappable' a verified fact rather than a
//! claim."* A port list is a claim. A suite that `store-sqlite`, `store-postgres` and the
//! in-memory fake all pass is a fact, and it is the only reason anybody can believe the
//! second adapter behaves like the first.
//!
//! # How a suite is run
//!
//! Three pieces, and the separation between them is the design:
//!
//! 1. **A harness** ([`harness`]) that the adapter's test code implements. It creates a fresh
//!    instance and — for the ports whose contract includes surviving a crash — performs the
//!    destructive operations.
//! 2. **Cases**, one `async fn` per obligation, in this crate. They take `&H` and return
//!    [`Result<(), CaseFailure>`], so a suite never panics on its own account.
//! 3. **A macro** that turns the cases into `#[test]` functions, taking the adapter's own
//!    `block_on`. That is why this crate has no runtime dependency: the executor arrives from
//!    the caller, so `store-sqlite` uses tokio and `pos-fakes` uses a twenty-line poller.
//!
//! ```ignore
//! // In an adapter's tests/contract.rs:
//! pos_contract_tests::event_store_suite!(MyHarness::new(), tokio_block_on);
//! ```
//!
//! # Why fault injection lives on the harness
//!
//! `EventStore`'s contract includes *survival of a crash mid-transaction*, so something has to
//! cause the crash. The obvious place — a `simulate_crash` method on `EventStore` — would ship
//! a "corrupt yourself now" entry point in every production adapter, reachable from anywhere
//! holding the trait. A harness is test-only by construction. See
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §6.
//!
//! # Why cases return `Result` instead of asserting
//!
//! A suite's whole value is telling an adapter author *which obligation* they broke.
//! `assert_eq!` says `3 != 2`. [`Obligation::require_eq`] says which port, which numbered
//! obligation, what was expected and what happened — and the difference between those two
//! messages is the difference between a suite people use and a suite people disable.
//!
//! There is exactly one `panic!` in this crate, in [`report`], because panicking is the only
//! way to tell a test harness that a test failed.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use core::fmt;

use pos_ports::{PortError, PortName};

pub mod blob_store;
pub mod config_store;
pub mod delivery;
pub mod determinism;
pub mod erp;
pub mod event_store;
pub mod fiscalization;
pub mod fixtures;
pub mod harness;
pub mod key_vault;
pub mod message_link;
pub mod metrics_sink;
pub mod order_in;
pub mod payment;
pub mod printer;
pub mod shipping;
pub mod signer;

/// Re-exported for the suite macros, which expand in the adapter's crate and so cannot name
/// `pos_ports` unless the adapter happens to depend on it.
#[doc(hidden)]
pub const __PORT_BLOB_STORE: PortName = PortName::BlobStore;
#[doc(hidden)]
pub const __PORT_CLOCK_SOURCE: PortName = PortName::ClockSource;
#[doc(hidden)]
pub const __PORT_CONFIG_STORE: PortName = PortName::ConfigStore;
#[doc(hidden)]
pub const __PORT_DELIVERY_VENDOR: PortName = PortName::DeliveryVendor;
#[doc(hidden)]
pub const __PORT_ERP_SINK: PortName = PortName::ErpSink;
#[doc(hidden)]
pub const __PORT_EVENT_STORE: PortName = PortName::EventStore;
#[doc(hidden)]
pub const __PORT_FISCALIZATION: PortName = PortName::Fiscalization;
#[doc(hidden)]
pub const __PORT_MESSAGE_LINK: PortName = PortName::MessageLink;
#[doc(hidden)]
pub const __PORT_ID_GENERATOR: PortName = PortName::IdGenerator;
#[doc(hidden)]
pub const __PORT_KEY_VAULT: PortName = PortName::KeyVault;
#[doc(hidden)]
pub const __PORT_METRICS_SINK: PortName = PortName::MetricsSink;
#[doc(hidden)]
pub const __PORT_ORDER_IN: PortName = PortName::OrderIn;
#[doc(hidden)]
pub const __PORT_PAYMENT_TERMINAL: PortName = PortName::PaymentTerminal;
#[doc(hidden)]
pub const __PORT_PRINTER_DRIVER: PortName = PortName::PrinterDriver;
#[doc(hidden)]
pub const __PORT_SHIPPING_DISPATCH: PortName = PortName::ShippingDispatch;
#[doc(hidden)]
pub const __PORT_SIGNER: PortName = PortName::Signer;

/// A contract obligation was not met.
///
/// Deliberately carries the obligation's name rather than only a message, so the failure names
/// the rule rather than the symptom.
pub struct CaseFailure {
    obligation: Option<&'static str>,
    detail: String,
    source: Option<PortError>,
}

impl CaseFailure {
    /// A failure with no obligation attached, for something that went wrong before any
    /// obligation could be tested.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            obligation: None,
            detail: detail.into(),
            source: None,
        }
    }
}

impl fmt::Display for CaseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.obligation {
            Some(obligation) => write!(f, "[{obligation}] {}", self.detail)?,
            None => write!(f, "{}", self.detail)?,
        }
        if let Some(source) = &self.source {
            write!(f, " (port reported: {source})")?;
        }
        Ok(())
    }
}

impl fmt::Debug for CaseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

/// An unexpected port error mid-case is a failure, so `?` works on every port call.
impl From<PortError> for CaseFailure {
    fn from(value: PortError) -> Self {
        Self {
            obligation: None,
            detail: "the port returned an error where the contract requires success".to_owned(),
            source: Some(value),
        }
    }
}

/// A harness failure is a failure of the *harness*, and says so, so an adapter author does not
/// spend an afternoon debugging a port over a test-fixture problem.
impl From<harness::HarnessError> for CaseFailure {
    fn from(value: harness::HarnessError) -> Self {
        Self {
            obligation: None,
            detail: format!("the test harness itself failed: {value}"),
            source: None,
        }
    }
}

/// One numbered obligation from a port's documented contract.
///
/// Constructed at the top of each case, so the failure message names the rule the case exists
/// to check rather than the assertion that happened to catch it.
#[derive(Debug, Clone, Copy)]
pub struct Obligation {
    port: PortName,
    name: &'static str,
}

impl Obligation {
    /// Names an obligation. `name` should be the words the port's own documentation uses, so a
    /// reader can find it.
    #[must_use]
    pub const fn new(port: PortName, name: &'static str) -> Self {
        Self { port, name }
    }

    /// Which port.
    #[must_use]
    pub const fn port(&self) -> PortName {
        self.port
    }

    /// Requires a condition to hold.
    ///
    /// # Errors
    ///
    /// [`CaseFailure`] naming this obligation, when `holds` is false.
    pub fn require(&self, holds: bool, detail: impl Into<String>) -> Result<(), CaseFailure> {
        if holds {
            return Ok(());
        }
        Err(CaseFailure {
            obligation: Some(self.name),
            detail: detail.into(),
            source: None,
        })
    }

    /// Requires two values to be equal, reporting both when they are not.
    ///
    /// # Errors
    ///
    /// [`CaseFailure`] naming this obligation, with the observed and expected values.
    pub fn require_eq<T>(&self, observed: &T, expected: &T, what: &str) -> Result<(), CaseFailure>
    where
        T: PartialEq + fmt::Debug + ?Sized,
    {
        self.require(
            observed == expected,
            format!("{what}: expected {expected:?}, observed {observed:?}"),
        )
    }

    /// Requires a collection to have exactly `expected` items, and returns them.
    ///
    /// The length check comes first on purpose: a case that goes on to look at item three of a
    /// two-item result should report "expected three events, got two", not "index out of
    /// range". `clippy::indexing_slicing` is denied across this workspace, so there is no
    /// shorter way to write it anyway — which is the lint doing its job.
    ///
    /// # Errors
    ///
    /// [`CaseFailure`] naming this obligation, when the count differs.
    pub fn require_len<'a, T>(
        &self,
        items: &'a [T],
        expected: usize,
        what: &str,
    ) -> Result<&'a [T], CaseFailure> {
        self.require(
            items.len() == expected,
            format!(
                "{what}: expected {expected} items, observed {}",
                items.len()
            ),
        )?;
        Ok(items)
    }

    /// Returns item `index`, or fails naming what was being looked for.
    ///
    /// # Errors
    ///
    /// [`CaseFailure`] naming this obligation, when the item is not there.
    pub fn require_nth<'a, T>(
        &self,
        items: &'a [T],
        index: usize,
        what: &str,
    ) -> Result<&'a T, CaseFailure> {
        match items.get(index) {
            Some(item) => Ok(item),
            None => Err(CaseFailure {
                obligation: Some(self.name),
                detail: format!(
                    "{what}: wanted item {index} but only {} were returned",
                    items.len()
                ),
                source: None,
            }),
        }
    }

    /// Requires a sequence to be strictly ascending under `key`.
    ///
    /// A helper rather than a loop in each case, because "ordered read-back" is an obligation of
    /// four different ports and a per-case loop would report it four different ways.
    ///
    /// # Errors
    ///
    /// [`CaseFailure`] naming the first pair that is out of order.
    pub fn require_ascending<T, K, F>(
        &self,
        items: &[T],
        key: F,
        what: &str,
    ) -> Result<(), CaseFailure>
    where
        F: Fn(&T) -> K,
        K: Ord + fmt::Debug,
    {
        // `Ord` rather than `PartialOrd`, so that "not ascending" is a total answer. With
        // `PartialOrd`, two incomparable keys are neither ascending nor descending, and a
        // suite that reported them as out of order would be guessing.
        for (position, (left, right)) in items.iter().zip(items.iter().skip(1)).enumerate() {
            let (left, right) = (key(left), key(right));
            if left >= right {
                return self.require(
                    false,
                    format!(
                        "{what}: items {position} and {} are out of order — {left:?} does not                          precede {right:?}",
                        position.saturating_add(1)
                    ),
                );
            }
        }
        Ok(())
    }

    /// Requires a call to have failed, and to have failed for the stated reason.
    ///
    /// Separate from [`Self::require`] because "it errored" is not the obligation — *which*
    /// error matters. An adapter returning `Internal` where the contract says
    /// `FailedPrecondition` will be retried forever by a caller that trusts the classification.
    ///
    /// # Errors
    ///
    /// [`CaseFailure`] if the call succeeded, or failed with a different status.
    pub fn require_error<T>(
        &self,
        outcome: Result<T, PortError>,
        expected: pos_proto::ErrorStatus,
        what: &str,
    ) -> Result<(), CaseFailure> {
        use pos_proto::wire_enum::WireEnum;
        match outcome {
            Ok(_) => self.require(false, format!("{what}: the call succeeded, and must not")),
            Err(error) => self.require(
                error.status() == expected,
                format!(
                    "{what}: expected status {}, observed {} — a caller branches on this, so a \
                     wrong status is a wrong retry policy",
                    expected.as_wire(),
                    error.status().as_wire()
                ),
            ),
        }
    }
}

/// Reports a case's outcome to the test harness.
///
/// # Panics
///
/// When the case failed. That is the point: panicking is how a `#[test]` reports failure, and
/// confining it to this one function is what lets every case be an ordinary `Result`.
#[expect(
    clippy::panic,
    reason = "a contract suite has to fail a test, and a test harness listens for exactly one \
              signal. Confining it here keeps every case a Result, which is what lets the \
              failure message name the broken obligation instead of a line number."
)]
pub fn report(port: PortName, case: &'static str, outcome: Result<(), CaseFailure>) {
    if let Err(failure) = outcome {
        panic!("{port} contract case `{case}` failed: {failure}");
    }
}

/// Every port that has a suite in this crate, with the macro that emits it.
///
/// The list exists so [`tests::every_port_has_a_suite`] can fail when a port is added without one.
/// `docs/roadmap.md` P2's exit criterion is *"every port has a contract suite"*, and a criterion
/// nothing checks is a criterion that stops being true the first time somebody is in a hurry.
///
/// Sixteen entries, matching `PortName::ALL`.
pub const SUITES: &[(PortName, &str)] = &[
    (PortName::EventStore, "event_store_suite"),
    (PortName::ConfigStore, "config_store_suite"),
    (PortName::MessageLink, "message_link_suite"),
    (PortName::BlobStore, "blob_store_suite"),
    (PortName::MetricsSink, "metrics_sink_suite"),
    (PortName::Signer, "signer_suite"),
    (PortName::KeyVault, "key_vault_suite"),
    (PortName::ClockSource, "clock_source_suite"),
    (PortName::IdGenerator, "id_generator_suite"),
    (PortName::PrinterDriver, "printer_driver_suite"),
    (PortName::PaymentTerminal, "payment_terminal_suite"),
    (PortName::Fiscalization, "fiscalization_suite"),
    (PortName::DeliveryVendor, "delivery_vendor_suite"),
    (PortName::ShippingDispatch, "shipping_dispatch_suite"),
    (PortName::ErpSink, "erp_sink_suite"),
    (PortName::OrderIn, "order_in_suite"),
];

/// Turns a list of *synchronous* case functions into `#[test]` functions.
///
/// [`ClockSource`](pos_proto::ClockSource), [`IdGenerator`](pos_proto::IdGenerator) and
/// [`Signer`](pos_ports::Signer) are synchronous ports, so their cases cannot await and asking
/// their callers for a `block_on` would mean inventing an executor for a function that has no need
/// of one.
#[macro_export]
macro_rules! contract_cases_sync {
    (
        harness = $harness:expr,
        port = $port:expr,
        module = $module:ident,
        cases = [$($case:ident),+ $(,)?]
    ) => {
        $(
            #[test]
            fn $case() {
                let harness = $harness;
                $crate::report($port, stringify!($case), $crate::$module::$case(&harness));
            }
        )+
    };
}

/// Turns a list of case functions into `#[test]` functions.
///
/// `module` is the *bare module name*, not a path, and it is an `ident` fragment for a reason: a
/// `path` fragment is an opaque AST node, so `$module::$case` does not parse — the compiler reports
/// "expected one of `)`, `,` ... found `::`" from inside the expansion, which is an hour to diagnose
/// the first time. An `ident` can be followed by `::`, and every case module is a direct child of
/// this crate, so `$crate::$module::$case` is enough.
///
/// Used by the per-port suite macros below rather than directly. `$block_on` is the adapter's
/// own executor: `tokio::runtime::Runtime::block_on` for a real adapter, and for `pos-fakes` a
/// poller that drives a future exactly once — see that crate, where a future that yields is a
/// bug rather than a wait.
#[macro_export]
macro_rules! contract_cases {
    (
        harness = $harness:expr,
        block_on = $block_on:path,
        port = $port:expr,
        module = $module:ident,
        cases = [$($case:ident),+ $(,)?]
    ) => {
        $(
            #[test]
            fn $case() {
                let harness = $harness;
                let outcome = $block_on($crate::$module::$case(&harness));
                $crate::report($port, stringify!($case), outcome);
            }
        )+
    };
}

#[cfg(test)]
mod tests {
    use super::{CaseFailure, Obligation, SUITES, report};
    use pos_ports::{PortError, PortName};

    #[test]
    fn every_port_has_a_suite() {
        // P2's exit criterion, checked rather than asserted in prose. A seventeenth port needs an
        // ADR (ADR-0021), and this is what makes it also need a suite.
        for port in PortName::ALL {
            assert!(
                SUITES.iter().any(|(named, _)| named == port),
                "{port} has no contract suite. A port without one is a port whose                  implementations are not known to agree"
            );
        }
        assert_eq!(
            SUITES.len(),
            PortName::ALL.len(),
            "a suite exists for a port that does not"
        );
    }

    #[test]
    fn a_failure_names_the_obligation_it_broke() {
        // The whole reason cases return `Result` rather than asserting. `3 != 2` tells an adapter
        // author nothing; this tells them which rule and where to read it.
        let obligation = Obligation::new(PortName::EventStore, "idempotency by ULID");
        let failure = obligation
            .require_eq(&0_u32, &3_u32, "a replayed append reports duplicates")
            .expect_err("must fail");
        let rendered = failure.to_string();
        assert!(rendered.contains("idempotency by ULID"), "got {rendered}");
        assert!(rendered.contains("expected 3"), "got {rendered}");
        assert!(rendered.contains("observed 0"), "got {rendered}");
    }

    #[test]
    fn a_port_error_mid_case_is_reported_as_such() {
        // So an adapter author is not sent looking for a broken assertion when the port simply
        // returned an error where the contract required success.
        let failure: CaseFailure =
            PortError::unavailable(PortName::MessageLink, "connection refused").into();
        let rendered = failure.to_string();
        assert!(
            rendered.contains("the port returned an error"),
            "got {rendered}"
        );
        assert!(rendered.contains("connection refused"), "got {rendered}");
    }

    #[test]
    fn a_harness_failure_says_it_was_the_harness() {
        let failure: CaseFailure =
            crate::harness::HarnessError::new("no writable temporary directory").into();
        let rendered = failure.to_string();
        assert!(
            rendered.contains("test harness itself failed"),
            "got {rendered}"
        );
    }

    #[test]
    fn require_error_rejects_the_wrong_status() {
        // An adapter returning Internal where the contract says FailedPrecondition will be retried
        // forever by a caller that trusts the classification, so "it errored" is not enough.
        let obligation = Obligation::new(PortName::ConfigStore, "a delta applies in order");
        let wrong: Result<(), PortError> = Err(PortError::internal(PortName::ConfigStore, "boom"));
        let failure = obligation
            .require_error(
                wrong,
                pos_proto::ErrorStatus::FailedPrecondition,
                "a stale delta",
            )
            .expect_err("must fail");
        assert!(failure.to_string().contains("FAILED_PRECONDITION"));

        let right: Result<(), PortError> = Err(PortError::failed_precondition(
            PortName::ConfigStore,
            "wrong version",
        ));
        obligation
            .require_error(
                right,
                pos_proto::ErrorStatus::FailedPrecondition,
                "a stale delta",
            )
            .expect("the right status passes");
    }

    #[test]
    fn require_ascending_names_the_offending_pair() {
        let obligation = Obligation::new(PortName::EventStore, "ordered read-back");
        obligation
            .require_ascending(&[1_u32, 2, 3], |value| *value, "a sorted page")
            .expect("ascending passes");

        let failure = obligation
            .require_ascending(&[1_u32, 3, 2], |value| *value, "a page")
            .expect_err("must fail");
        let rendered = failure.to_string();
        assert!(rendered.contains("items 1 and 2"), "got {rendered}");

        // Equal neighbours are not ascending either — a duplicate in a supposedly ordered feed is
        // exactly the bug this catches.
        obligation
            .require_ascending(&[1_u32, 1], |value| *value, "a page")
            .expect_err("equal values are not ascending");
    }

    #[test]
    fn report_passes_a_successful_case_through() {
        report(PortName::EventStore, "a_passing_case", Ok(()));
    }

    #[test]
    #[should_panic(expected = "contract case `a_failing_case` failed")]
    fn report_panics_on_a_failure_because_that_is_how_a_test_fails() {
        report(
            PortName::EventStore,
            "a_failing_case",
            Err(CaseFailure::new("something broke")),
        );
    }
}
