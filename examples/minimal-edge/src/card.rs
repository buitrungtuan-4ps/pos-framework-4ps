// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The card terminal beside this example's till (ADR-0230): the fake. A real store's terminal is an
//! adapter (B10.3), whose vendor is still the owner's to choose.
//!
//! Every card is captured, unless `POS_DEMO_CARD` scripts the next ones: a comma-separated list of
//! `DECLINED`, `UNKNOWN` or `CAPTURED`, one per card in turn (at most 16), so a browser test can
//! drive the decline and the unknown result. An unknown card is settled as captured by the acquirer
//! by the time anyone looks it up, as one whose connection dropped would be.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use pos_fakes::FakePaymentTerminal;
use pos_ports::PortError;
use pos_ports::payment::{PaymentAttempt, PaymentReference, PaymentRequest, PaymentTerminal};
use pos_proto::PaymentOutcome;

/// The most outcomes a script may hold.
const SCRIPT_MAX: usize = 16;

/// The demo store's terminal.
#[derive(Debug, Clone, Default)]
pub struct DemoTerminal {
    fake: FakePaymentTerminal,
    script: Arc<Mutex<VecDeque<PaymentOutcome>>>,
}

impl DemoTerminal {
    /// The terminal, scripted by `POS_DEMO_CARD` where it is set.
    pub fn from_env() -> Self {
        let script = std::env::var("POS_DEMO_CARD")
            .unwrap_or_default()
            .split(',')
            .filter_map(|token| match token.trim() {
                "DECLINED" => Some(PaymentOutcome::Declined),
                "UNKNOWN" => Some(PaymentOutcome::Unknown),
                "CAPTURED" => Some(PaymentOutcome::Captured),
                _ => None,
            })
            .take(SCRIPT_MAX)
            .collect();
        Self {
            fake: FakePaymentTerminal::new(),
            script: Arc::new(Mutex::new(script)),
        }
    }
}

impl PaymentTerminal for DemoTerminal {
    async fn authorize(&self, request: &PaymentRequest) -> Result<PaymentAttempt, PortError> {
        let next = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        if let Some(outcome) = next {
            self.fake.stage_outcome(outcome);
        }
        self.fake.authorize(request).await
    }

    async fn look_up(&self, reference: &PaymentReference) -> Result<PaymentAttempt, PortError> {
        let attempt = self.fake.look_up(reference).await?;
        if attempt.outcome.known() == PaymentOutcome::Unknown {
            self.fake
                .conclude(attempt.payment_id, PaymentOutcome::Captured);
            return self.fake.look_up(reference).await;
        }
        Ok(attempt)
    }

    async fn void(&self, reference: &PaymentReference) -> Result<PaymentAttempt, PortError> {
        self.fake.void(reference).await
    }
}
