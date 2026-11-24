// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `einvoice.sandbox`: a deterministic e-invoice provider with no vendor behind it
//! ([ADR-0153](../../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decision 6,
//! ADR-0225 decision 8).
//!
//! It accepts, refuses or times out on command, so the submitter's retry and its alerts are driven
//! end to end without a vendor account. A submission (a request carrying the store's own number) is
//! what the mode applies to. A request without one is numbered from the sandbox's own range, as every
//! country's offline allocator numbers it, so the sandbox also passes the whole `Fiscalization`
//! contract.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use pos_country::offline::OfflineFiscalization;
use pos_ports::fiscalization::{
    Fiscalization, InvoiceNumber, InvoiceRange, InvoiceRequest, IssuedInvoice, ReconciliationReport,
};
use pos_ports::{PortError, PortName};
use pos_proto::{CalendarDate, StoreId};
use pos_providers::{Family, ProviderDescriptor};

/// The sandbox in the cloud's provider catalogue: every country, no settings, no secrets.
pub static PROVIDER: ProviderDescriptor = ProviderDescriptor {
    provider_id: "einvoice.sandbox",
    family: Family::EInvoice,
    name_key: "provider.einvoice.sandbox",
    countries: &[],
    fields: &[],
    capabilities: &["adjust", "replace"],
    sandbox: true,
};

/// What the sandbox does with the next submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// Accepts it, and the invoice reads as submitted.
    #[default]
    Accept,
    /// Refuses it, as a provider refuses an invoice it will not take: invalid, never retried.
    Refuse,
    /// Does not answer: unavailable, which the submitter retries.
    TimeOut,
}

#[derive(Debug, Default)]
struct Accepted {
    mode: SandboxMode,
    numbers: BTreeSet<InvoiceNumber>,
}

/// The sandbox provider. Cloning shares its state.
#[derive(Debug, Clone)]
pub struct SandboxEInvoice {
    numbering: OfflineFiscalization,
    accepted: Arc<Mutex<Accepted>>,
}

impl Default for SandboxEInvoice {
    fn default() -> Self {
        Self {
            numbering: OfflineFiscalization::new("sandbox", |series, index| {
                format!("SANDBOX{series:04}/{index:08}")
            }),
            accepted: Arc::default(),
        }
    }
}

impl SandboxEInvoice {
    /// A sandbox that accepts every submission.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets what the next submissions meet.
    pub fn set_mode(&self, mode: SandboxMode) {
        self.lock().mode = mode;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Accepted> {
        self.accepted.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn marked(&self, mut invoice: IssuedInvoice) -> IssuedInvoice {
        if self.lock().numbers.contains(&invoice.invoice_number) {
            invoice.submitted = true;
            invoice.authority_reference =
                Some(format!("SANDBOX-{}", invoice.invoice_number).into());
        }
        invoice
    }
}

impl Fiscalization for SandboxEInvoice {
    async fn allocate_range(
        &self,
        store_id: StoreId,
        count: core::num::NonZeroU32,
    ) -> Result<InvoiceRange, PortError> {
        self.numbering.allocate_range(store_id, count).await
    }

    async fn issue(&self, request: &InvoiceRequest) -> Result<IssuedInvoice, PortError> {
        if request.issued_number.is_some() {
            match self.lock().mode {
                SandboxMode::Accept => {}
                SandboxMode::Refuse => {
                    return Err(PortError::invalid_argument(
                        PortName::Fiscalization,
                        "the sandbox refused the invoice, as instructed",
                    ));
                }
                SandboxMode::TimeOut => {
                    return Err(PortError::unavailable(
                        PortName::Fiscalization,
                        "the sandbox did not answer, as instructed",
                    ));
                }
            }
        }
        let invoice = self.numbering.issue(request).await?;
        if request.issued_number.is_some() {
            self.lock().numbers.insert(invoice.invoice_number.clone());
        }
        Ok(self.marked(invoice))
    }

    async fn look_up(
        &self,
        invoice_number: &InvoiceNumber,
    ) -> Result<Option<IssuedInvoice>, PortError> {
        Ok(self
            .numbering
            .look_up(invoice_number)
            .await?
            .map(|invoice| self.marked(invoice)))
    }

    async fn reconcile(
        &self,
        store_id: StoreId,
        on: CalendarDate,
    ) -> Result<ReconciliationReport, PortError> {
        let mut report = self.numbering.reconcile(store_id, on).await?;
        let accepted = self.lock();
        report
            .unsubmitted
            .retain(|number| !accepted.numbers.contains(number));
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use pos_contract_tests::harness::{FiscalizationHarness, Setup};
    use pos_fakes::executor::run_ready;
    use pos_ports::fiscalization::{Fiscalization, InvoiceNumber, InvoiceRequest};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::{BillId, CalendarDate, ErrorStatus, StoreId, Ulid};

    use super::{SandboxEInvoice, SandboxMode};

    /// The sandbox under the shared suite. `disconnect` makes submissions time out, which numbering
    /// from a range never notices: that is the offline obligation.
    struct SandboxHarness;

    impl FiscalizationHarness for SandboxHarness {
        type Fiscal = SandboxEInvoice;

        async fn fresh(&self) -> Setup<Self::Fiscal> {
            Ok(SandboxEInvoice::new())
        }

        async fn disconnect(&self, fiscal: &Self::Fiscal) -> Setup<()> {
            fiscal.set_mode(SandboxMode::TimeOut);
            Ok(())
        }

        async fn reconnect(&self, fiscal: &Self::Fiscal) -> Setup<()> {
            fiscal.set_mode(SandboxMode::Accept);
            Ok(())
        }

        fn store_id(&self) -> StoreId {
            StoreId::new(Ulid::from_u128(1))
        }
    }

    pos_contract_tests::fiscalization_suite!(SandboxHarness, run_ready);

    fn submission(seed: u128, number: &str) -> InvoiceRequest {
        InvoiceRequest {
            bill_id: BillId::new(Ulid::from_u128(seed)),
            store_id: StoreId::new(Ulid::from_u128(1)),
            issued_on: CalendarDate::from_ymd(2026, 11, 24).expect("a date"),
            buyer: None,
            lines: Vec::new(),
            total: Money::new(CurrencyCode::VND, 0),
            issued_number: Some(InvoiceNumber::new(number)),
            correction: None,
        }
    }

    #[test]
    fn it_accepts_refuses_and_times_out_on_command() {
        let sandbox = SandboxEInvoice::new();
        let accepted = run_ready(sandbox.issue(&submission(1, "1C26MAA/00000001"))).expect("taken");
        assert!(
            accepted.submitted,
            "an accepted submission reads as submitted"
        );
        assert_eq!(
            accepted.authority_reference.as_deref(),
            Some("SANDBOX-1C26MAA/00000001")
        );

        sandbox.set_mode(SandboxMode::Refuse);
        let refused = run_ready(sandbox.issue(&submission(2, "1C26MAA/00000002")));
        assert_eq!(
            refused.map_err(|error| error.status()).err(),
            Some(ErrorStatus::InvalidArgument),
            "a refusal is not retried"
        );

        sandbox.set_mode(SandboxMode::TimeOut);
        let timed_out = run_ready(sandbox.issue(&submission(2, "1C26MAA/00000002")));
        assert_eq!(
            timed_out.map_err(|error| error.status()).err(),
            Some(ErrorStatus::Unavailable),
            "a timeout is retried"
        );

        sandbox.set_mode(SandboxMode::Accept);
        let retried = run_ready(sandbox.issue(&submission(2, "1C26MAA/00000002"))).expect("taken");
        assert!(retried.submitted, "the retry after a timeout goes through");
        let clean = run_ready(sandbox.reconcile(
            StoreId::new(Ulid::from_u128(1)),
            CalendarDate::from_ymd(2026, 11, 24).expect("a date"),
        ))
        .expect("reconciled");
        assert!(clean.is_clean(), "{clean:?}");
    }

    #[test]
    fn it_is_in_the_catalogue_as_a_sandbox_for_every_country() {
        let registry = crate::providers::registry().expect("a valid catalogue");
        let found = registry.get("einvoice.sandbox").expect("listed");
        assert!(found.sandbox);
        assert!(found.countries.is_empty());
    }
}
