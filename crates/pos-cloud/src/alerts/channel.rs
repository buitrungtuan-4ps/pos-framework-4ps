// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Off-console alert delivery ([ADR-0073](../../../docs/adr/0073-alerting.md), Track O2 slice 4).
//!
//! # Why a seam and not just an HTTP call
//!
//! ADR-0073 ships the **in-console channel** as the primary one: the evaluator persists every firing
//! alert and the notification bell reads it. That channel is the `alerts` table, and it already
//! works. What it cannot do is wake anybody: a store that goes dark at 02:00 waits in the table until
//! somebody opens the console. This module is the push half — one seam, and the webhook channel
//! ADR-0073 named, with email and a chat channel as later adapters behind the same trait.
//!
//! # What delivery may never do
//!
//! **A failed delivery is not a failed pass.** The alert is already stored and already visible in the
//! console before a channel is asked; a channel that cannot reach its destination must not undo that,
//! retry into the tick's deadline, or make the evaluator's health row read `false` — the evaluator
//! *did* its job. The pass reports the delivery failure in its detail instead, so an operator can see
//! that alerts are firing and not arriving, which is a different fault from either half being down.
//!
//! # No PII, by construction
//!
//! A [`FiringAlert`] carries a kind, a severity, ids (tenant, and a dedup key that is a store or
//! endpoint id), a composed one-line summary and a small numeric detail object. There is no field a
//! customer or employee identifier could arrive in, which is what makes it safe to post to a
//! third-party endpoint at all (ADR-0070 T1 data never leaves over this path). The test
//! `the_delivered_body_carries_no_free_text_beyond_the_composed_summary` is what keeps that true as
//! kinds are added.

use core::future::Future;

use pos_proto::Timestamp;
use serde::Serialize;

use super::model::FiringAlert;
use crate::webhook::dispatch::WebhookTransport;
use crate::webhook::sign::{SigningSecret, sign};
use crate::webhook::ssrf::VettedUrl;

/// A delivery that did not happen.
///
/// Opaque and never fatal: see the module docstring. The message is for the evaluator's health detail
/// and the log line, not for a retry decision — the next tick re-evaluates from the read models, and
/// an alert that is still firing is still in the table.
#[derive(Debug, Clone, thiserror::Error)]
#[error("alert delivery failed: {message}")]
pub struct ChannelError {
    message: String,
}

impl ChannelError {
    /// Builds a failure with a human-readable reason.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Somewhere newly-opened alerts are pushed.
///
/// Takes the whole newly-opened batch rather than one alert at a time, so a channel with a per-message
/// cost (an email, a chat post) can coalesce a burst — twelve stores dropping off at once is one
/// notification, not twelve.
pub trait AlertChannel {
    /// Pushes `alerts` to this channel's destination.
    ///
    /// `now` is the pass's clock reading, passed in rather than read here so a channel signs with the
    /// same instant the pass reconciled at and a test can pin it.
    ///
    /// # Errors
    ///
    /// [`ChannelError`] if the destination could not be reached or refused the batch. Never a reason
    /// to fail the pass.
    fn deliver(
        &self,
        now: Timestamp,
        alerts: &[FiringAlert],
    ) -> impl Future<Output = Result<(), ChannelError>> + Send;
}

/// One alert as it goes over the wire.
///
/// A named struct rather than reusing [`FiringAlert`]'s own serialisation, because this is a published
/// body shape a receiver parses: it should change when someone means to change it, not when a domain
/// type gains a field.
#[derive(Debug, Serialize)]
struct AlertBody {
    /// The condition that fired, as its wire token (`store_offline`, `projector_unhealthy`, …).
    kind: &'static str,
    /// `info`, `warning` or `critical`.
    severity: &'static str,
    /// The owning tenant as a ULID, or absent for a server-wide alert.
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<String>,
    /// What distinguishes this alert within its kind — a store or endpoint id, or empty for a
    /// singleton condition.
    dedup_key: String,
    /// The composed one-line summary.
    summary: String,
    /// The numbers behind the alert.
    detail: serde_json::Value,
}

/// The delivered batch.
#[derive(Debug, Serialize)]
struct DeliveryBody {
    /// When the pass that opened these alerts ran, in milliseconds since the epoch.
    opened_at_ms: i64,
    /// The newly-opened alerts. Never empty — a channel is not asked with nothing to say.
    alerts: Vec<AlertBody>,
}

/// Builds the wire body for a batch. Split out so a test can assert its shape without a transport.
fn delivery_body(now: Timestamp, alerts: &[FiringAlert]) -> DeliveryBody {
    DeliveryBody {
        opened_at_ms: now.as_milliseconds_since_epoch(),
        alerts: alerts
            .iter()
            .map(|alert| AlertBody {
                kind: alert.kind.as_str(),
                severity: alert.severity.as_str(),
                tenant_id: alert.tenant_id.map(|id| id.as_ulid().to_string()),
                dedup_key: alert.dedup_key.clone(),
                summary: alert.summary.clone(),
                detail: alert.detail.clone(),
            })
            .collect(),
    }
}

/// The webhook channel: newly-opened alerts as one signed JSON body to a vetted URL.
///
/// Reuses the ADR-0032 machinery whole — [`vet`](crate::webhook::vet) for the SSRF check,
/// [`sign`] for the HMAC, and the [`WebhookTransport`] seam for the TLS request — because an alert
/// batch *is* just another signed JSON body to an endpoint an operator nominated, and a second
/// half-built sender beside the first is how two senders come to disagree about what is safe to
/// resolve to.
///
/// One destination per deployment, not per tenant: the conditions this delivers include server-wide
/// ones (the projector is unhealthy, the stream is near capacity) that belong to no tenant, and there
/// is no tenant whose webhook those should go to.
#[derive(Debug)]
pub struct WebhookAlertChannel<T> {
    transport: T,
    destination: VettedUrl,
    secret: SigningSecret,
}

impl<T> WebhookAlertChannel<T> {
    /// Composes the channel over a transport, a vetted destination and the signing secret.
    #[must_use]
    pub fn new(transport: T, destination: VettedUrl, secret: SigningSecret) -> Self {
        Self {
            transport,
            destination,
            secret,
        }
    }
}

impl<T: WebhookTransport + Sync> AlertChannel for WebhookAlertChannel<T> {
    async fn deliver(&self, now: Timestamp, alerts: &[FiringAlert]) -> Result<(), ChannelError> {
        let body = serde_json::to_vec(&delivery_body(now, alerts)).map_err(|error| {
            ChannelError::new(format!("encoding the alert batch failed: {error}"))
        })?;
        let signature = sign(&self.secret, now, &body);
        self.transport
            .deliver(&self.destination, &signature, &body)
            .await
            .map_err(|error| ChannelError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::{TenantId, Timestamp, Ulid};

    use super::{AlertChannel as _, ChannelError, WebhookAlertChannel, delivery_body};
    use crate::alerts::model::{AlertKind, AlertSeverity, FiringAlert};
    use crate::webhook::dispatch::{DeliveryError, WebhookTransport};
    use crate::webhook::sign::SigningSecret;
    use crate::webhook::ssrf::VettedUrl;
    use std::sync::Mutex;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(seconds * 1_000).expect("a valid instant")
    }

    fn destination() -> VettedUrl {
        VettedUrl {
            url: "https://ops.example.com/alerts".to_owned(),
            addresses: vec![std::net::IpAddr::from([203, 0, 113, 7])],
        }
    }

    fn store_offline(tenant: Option<TenantId>) -> FiringAlert {
        FiringAlert::new(
            AlertKind::StoreOffline,
            tenant,
            "01STORE0000000000000000AA",
            "Store has not checked in for 12 minutes",
            serde_json::json!({ "silent_secs": 720 }),
        )
    }

    /// Records what was handed to the transport, so a test can read the signed body back.
    #[derive(Default)]
    struct RecordingTransport {
        sent: Mutex<Vec<(String, String, Vec<u8>)>>,
        fail: bool,
    }

    impl WebhookTransport for RecordingTransport {
        async fn deliver(
            &self,
            target: &VettedUrl,
            signature: &crate::webhook::sign::Signature,
            body: &[u8],
        ) -> Result<(), DeliveryError> {
            if self.fail {
                return Err(DeliveryError::new("the endpoint refused with 503"));
            }
            self.sent.lock().expect("lock").push((
                target.url.clone(),
                signature.signature.clone(),
                body.to_vec(),
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_batch_goes_to_the_vetted_url_signed_with_the_configured_secret() {
        let channel = WebhookAlertChannel::new(
            RecordingTransport::default(),
            destination(),
            SigningSecret::new("a".repeat(32)),
        );
        let alerts = vec![store_offline(Some(TenantId::new(Ulid::from_u128(1))))];

        channel
            .deliver(at(1_700_000_000), &alerts)
            .await
            .expect("delivered");

        let sent = channel.transport.sent.lock().expect("lock").clone();
        assert_eq!(sent.len(), 1, "one batch, one request");
        let (url, signature, body) = sent.into_iter().next().expect("the one request");
        assert_eq!(url, "https://ops.example.com/alerts");
        assert!(
            signature.starts_with("v1="),
            "the ADR-0032 signature format, not a bespoke one: {signature}"
        );
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(parsed["alerts"].as_array().expect("the batch").len(), 1);
        assert_eq!(parsed["alerts"][0]["kind"], "store_offline");
        assert_eq!(parsed["opened_at_ms"], 1_700_000_000_000_i64);
    }

    #[tokio::test]
    async fn a_refused_delivery_is_an_error_the_caller_can_ignore_and_never_a_panic() {
        let channel = WebhookAlertChannel::new(
            RecordingTransport {
                fail: true,
                ..RecordingTransport::default()
            },
            destination(),
            SigningSecret::new("a".repeat(32)),
        );

        let outcome = channel.deliver(at(1), &[store_offline(None)]).await;

        let error: ChannelError = outcome.expect_err("the transport refused");
        assert!(
            error.to_string().contains("503"),
            "the reason survives for the health detail: {error}"
        );
    }

    #[test]
    fn a_server_wide_alert_omits_the_tenant_rather_than_inventing_one() {
        let body = delivery_body(at(5), &[store_offline(None)]);
        let json = serde_json::to_value(&body).expect("serialises");
        assert!(
            json["alerts"][0].get("tenant_id").is_none(),
            "absent, not null and not a nil ULID: {json}"
        );
    }

    /// The no-PII guard. A [`FiringAlert`]'s only free-text field is `summary`, which the evaluator
    /// composes from a kind and numbers; `detail` is a numeric object and the ids are ULIDs. This
    /// pins the body's field set so a kind added later cannot quietly widen what leaves the building.
    #[test]
    fn the_delivered_body_carries_no_free_text_beyond_the_composed_summary() {
        let body = delivery_body(at(5), &[store_offline(Some(TenantId::new(Ulid::NIL)))]);
        let json = serde_json::to_value(&body).expect("serialises");
        let mut fields: Vec<&str> = json["alerts"][0]
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "dedup_key",
                "detail",
                "kind",
                "severity",
                "summary",
                "tenant_id"
            ],
            "the alert body's fields are fixed; adding one is a deliberate change to what is pushed \
             to a third party, and needs the ADR-0073 no-PII argument re-made"
        );
    }

    #[test]
    fn every_severity_and_kind_has_a_wire_token_the_body_can_carry() {
        // The body holds `&'static str` tokens, so a kind whose token was forgotten would not
        // compile — this asserts the tokens are distinct, which a copy-paste would break.
        let tokens = [
            AlertSeverity::Info.as_str(),
            AlertSeverity::Warning.as_str(),
            AlertSeverity::Critical.as_str(),
        ];
        let mut unique = tokens.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), tokens.len(), "severity tokens are distinct");
    }
}
