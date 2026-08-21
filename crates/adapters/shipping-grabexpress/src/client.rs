// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`ShippingDispatch`] adapter over a [`CourierTransport`]
//! ([ADR-0058](../../../docs/adr/0058-shipping-adapters.md)).
//!
//! The same shape as the Ahamove adapter — the port's contract is the same, so the status mapping is
//! the same load-bearing part: a cancel of a finished job is
//! [`failed_precondition`](PortError::failed_precondition), an unknown job
//! [`not_found`](PortError::not_found), and no rider available
//! [`resource_exhausted`](PortError::resource_exhausted). What differs is only the courier's own wire:
//! Grab Express keys a booking by `merchant_order_id`, posts to a `deliveries` collection, and reports
//! its own status vocabulary (`ALLOCATING`, `PICKING_UP`, `IN_DELIVERY`, `COMPLETED`, `CANCELED`),
//! which this adapter maps onto [`ShipmentStatus`].
//!
//! # The delivery contact is personal data, and it goes no further than the courier
//!
//! Same posture as the sibling courier adapter: the booking transmits the recipient's name, phone, and
//! address to the courier (a data processor, lawful basis contract performance, PDPD); nothing here
//! logs it, and the tracked [`Shipment`] never carries it back. See ADR-0058.

use pos_ports::shipping::{CourierJobRef, DeliveryRequest, Shipment, ShippingDispatch};
use pos_ports::{PortError, PortName};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::wire_enum::Open;
use pos_proto::{ShipmentId, ShipmentStatus, Timestamp};

use crate::wire::{CourierTransport, HttpResponse, Method};

/// The port this adapter serves, for every [`PortError`] it raises.
const PORT: PortName = PortName::ShippingDispatch;

/// The collection a booking creates.
const DELIVERIES_PATH: &str = "/v1/deliveries";

/// The Grab Express [`ShippingDispatch`] adapter: book, cancel, and track a courier job over a
/// [`CourierTransport`].
#[derive(Debug, Clone)]
pub struct HttpGrabExpress<T> {
    transport: T,
}

impl<T> HttpGrabExpress<T> {
    /// Wraps a transport as a Grab Express courier channel.
    #[must_use]
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }
}

/// The recipient and destination, as Grab Express's booking wire names them.
///
/// Personal data. Present on the request that goes to the courier and nowhere else.
#[derive(serde::Serialize)]
struct RecipientBody<'a> {
    name: &'a str,
    phone: &'a str,
    address: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

/// The booking request body.
///
/// `merchant_order_id` is our [`ShipmentId`]; sending it again returns the same job rather than a
/// second rider, which is the port's first contractual obligation.
#[derive(serde::Serialize)]
struct CreateBody<'a> {
    merchant_order_id: String,
    store_id: String,
    order_id: String,
    recipient: RecipientBody<'a>,
    ready_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    quoted_fee_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quoted_fee_currency: Option<String>,
}

/// A courier job as Grab Express reports it, on booking, cancel, or track.
///
/// `merchant_order_id` is our own identifier echoed back, so tracking by the courier's `delivery_id`
/// still resolves to the shipment we asked about.
#[derive(serde::Deserialize)]
struct DeliveryBody {
    merchant_order_id: String,
    delivery_id: String,
    status: String,
    #[serde(default)]
    fee_minor: Option<i64>,
    #[serde(default)]
    fee_currency: Option<String>,
    updated_at_ms: i64,
}

impl<T: CourierTransport> ShippingDispatch for HttpGrabExpress<T> {
    fn courier_name(&self) -> &'static str {
        "grabexpress"
    }

    async fn create_delivery(&self, request: &DeliveryRequest) -> Result<Shipment, PortError> {
        let (quoted_fee_minor, quoted_fee_currency) = match request.quoted_fee {
            Some(fee) => (
                Some(fee.amount_minor),
                Some(fee.currency_code.as_str().to_owned()),
            ),
            None => (None, None),
        };
        let body = serde_json::to_vec(&CreateBody {
            merchant_order_id: request.shipment_id.to_string(),
            store_id: request.store_id.to_string(),
            order_id: request.order_id.to_string(),
            recipient: RecipientBody {
                name: &request.contact.recipient_name,
                phone: &request.contact.recipient_phone,
                address: &request.contact.delivery_address,
                note: request.contact.delivery_note.as_deref(),
            },
            ready_at_ms: request.ready_at.as_milliseconds_since_epoch(),
            quoted_fee_minor,
            quoted_fee_currency,
        })
        .map_err(|error| {
            PortError::internal(
                PORT,
                format!("encoding the booking request failed: {error}"),
            )
        })?;
        let response = self
            .transport
            .request(Method::Post, DELIVERIES_PATH, Some(body))
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_create(&response)
    }

    async fn cancel(&self, courier_job_ref: &CourierJobRef) -> Result<Shipment, PortError> {
        let path = format!("{DELIVERIES_PATH}/{}/cancel", courier_job_ref.as_str());
        let response = self
            .transport
            .request(Method::Post, &path, None)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_cancel(&response)
    }

    async fn track(&self, courier_job_ref: &CourierJobRef) -> Result<Shipment, PortError> {
        let path = format!("{DELIVERIES_PATH}/{}", courier_job_ref.as_str());
        let response = self
            .transport
            .request(Method::Get, &path, None)
            .await
            .map_err(|error| PortError::unavailable(PORT, error.to_string()))?;
        parse_track(&response)
    }
}

/// Maps a booking response to a shipment, or the right refusal.
///
/// `2xx` is the booked job; `400` is an address the courier could not resolve
/// ([`invalid_argument`](PortError::invalid_argument)); `409` is no rider available — a business
/// outcome the caller surfaces rather than a fault to retry blindly
/// ([`resource_exhausted`](PortError::resource_exhausted)); anything else is retryable
/// ([`unavailable`](PortError::unavailable)).
fn parse_create(response: &HttpResponse) -> Result<Shipment, PortError> {
    match response.status {
        200..=299 => parse_shipment(&decode(response)?),
        400 => Err(PortError::invalid_argument(
            PORT,
            "the courier could not resolve the delivery address",
        )),
        409 => Err(PortError::resource_exhausted(
            PORT,
            "the courier has no rider available for this booking",
        )),
        other => Err(PortError::unavailable(
            PORT,
            format!("the courier returned HTTP {other} for a booking"),
        )),
    }
}

/// Maps a cancel response to a shipment, or the right refusal.
///
/// `2xx` is the cancelled (or already-cancelled) job; `404` is an unknown reference
/// ([`not_found`](PortError::not_found)); `409` is a job that has already completed and cannot be
/// un-delivered ([`failed_precondition`](PortError::failed_precondition), because a successful-looking
/// cancel would leave the store expecting a refund it will not get); anything else is retryable.
fn parse_cancel(response: &HttpResponse) -> Result<Shipment, PortError> {
    match response.status {
        200..=299 => parse_shipment(&decode(response)?),
        404 => Err(PortError::not_found(
            PORT,
            "the courier does not recognise this job",
        )),
        409 => Err(PortError::failed_precondition(
            PORT,
            "the job has already completed and cannot be cancelled",
        )),
        other => Err(PortError::unavailable(
            PORT,
            format!("the courier returned HTTP {other} for a cancellation"),
        )),
    }
}

/// Maps a track response to a shipment, or the right absence.
///
/// `2xx` is the courier's current view, even for a finished job — which is how a missed callback is
/// reconciled; `404` is an unknown reference ([`not_found`](PortError::not_found)); anything else is
/// retryable.
fn parse_track(response: &HttpResponse) -> Result<Shipment, PortError> {
    match response.status {
        200..=299 => parse_shipment(&decode(response)?),
        404 => Err(PortError::not_found(
            PORT,
            "the courier does not recognise this job",
        )),
        other => Err(PortError::unavailable(
            PORT,
            format!("the courier returned HTTP {other} for a track"),
        )),
    }
}

/// Deserialises a courier body, treating a body that does not parse as the courier breaking its own
/// contract ([`internal`](PortError::internal)).
fn decode(response: &HttpResponse) -> Result<DeliveryBody, PortError> {
    serde_json::from_slice(&response.body).map_err(|error| {
        PortError::internal(
            PORT,
            format!("the courier's delivery response did not parse: {error}"),
        )
    })
}

/// Turns a courier body into a [`Shipment`], failing [`internal`](PortError::internal) if the courier
/// echoed something structurally impossible — a non-ULID identifier, an unknown currency, or a
/// timestamp out of range.
fn parse_shipment(body: &DeliveryBody) -> Result<Shipment, PortError> {
    let shipment_id = body
        .merchant_order_id
        .parse::<ShipmentId>()
        .map_err(|_ignored| {
            PortError::internal(
                PORT,
                "the courier returned a merchant_order_id that is not a ULID",
            )
        })?;
    let updated_at =
        Timestamp::from_milliseconds_since_epoch(body.updated_at_ms).map_err(|_err| {
            PortError::internal(PORT, "the courier returned an out-of-range updated_at")
        })?;
    let fee = match (body.fee_minor, body.fee_currency.as_deref()) {
        (Some(amount_minor), Some(currency)) => {
            let currency_code = CurrencyCode::parse(currency).map_err(|_err| {
                PortError::internal(PORT, "the courier returned an unknown currency")
            })?;
            Some(Money::new(currency_code, amount_minor))
        }
        // A fee not yet known (or a currency the courier omitted) is simply absent — the port models
        // that with `None`, not a zero that would read as free delivery.
        _no_fee => None,
    };
    Ok(Shipment {
        shipment_id,
        courier_job_ref: CourierJobRef::new(body.delivery_id.clone()),
        status: map_status(&body.status),
        fee,
        updated_at,
    })
}

/// Maps Grab Express's status vocabulary onto [`ShipmentStatus`].
///
/// A status this adapter does not recognise is preserved as an *unrecognised* [`Open`] value rather
/// than forced into a known one — [`ShipmentUpdate::is_terminal`](pos_ports::shipping::ShipmentUpdate::is_terminal)
/// then reports it non-terminal, the safe direction: a job wrongly believed live costs one more poll,
/// whereas one wrongly believed finished stops being tracked.
fn map_status(courier_status: &str) -> Open<ShipmentStatus> {
    match courier_status {
        "PENDING" | "ALLOCATING" | "ASSIGNED" => Open::from_known(ShipmentStatus::Accepted),
        "PICKING_UP" | "IN_DELIVERY" | "IN_TRANSIT" => Open::from_known(ShipmentStatus::InTransit),
        "COMPLETED" | "DELIVERED" => Open::from_known(ShipmentStatus::Completed),
        "CANCELED" | "CANCELLED" | "FAILED" | "RETURNED" => {
            Open::from_known(ShipmentStatus::Cancelled)
        }
        // Preserved verbatim behind the SHIPMENT_STATUS prefix so it deserialises as unrecognised,
        // not as any known variant, and shows in logs as what the courier actually said.
        other => Open::parse(&format!("SHIPMENT_STATUS_{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpResponse, map_status, parse_cancel, parse_create, parse_track};
    use pos_proto::ShipmentStatus;

    fn body(status: &str) -> Vec<u8> {
        format!(
            r#"{{"merchant_order_id":"00000000000000000000000001","delivery_id":"GX-1",
                 "status":"{status}","fee_minor":30000,"fee_currency":"VND","updated_at_ms":0}}"#
        )
        .into_bytes()
    }

    fn response(status: u16, body: Vec<u8>) -> HttpResponse {
        HttpResponse { status, body }
    }

    #[test]
    fn a_booking_parses_the_job_and_its_fee() {
        let shipment = parse_create(&response(201, body("ALLOCATING"))).expect("a booked shipment");
        assert_eq!(shipment.courier_job_ref.as_str(), "GX-1");
        assert_eq!(shipment.status.known(), ShipmentStatus::Accepted);
        assert_eq!(shipment.fee.expect("a fee").amount_minor, 30000);
    }

    #[test]
    fn a_400_booking_is_invalid_argument() {
        let error = parse_create(&response(400, b"bad address".to_vec())).expect_err("rejected");
        assert_eq!(error.status(), pos_proto::ErrorStatus::InvalidArgument);
    }

    #[test]
    fn a_409_booking_is_resource_exhausted_not_a_blind_retry() {
        let error = parse_create(&response(409, b"no rider".to_vec())).expect_err("no rider");
        assert_eq!(error.status(), pos_proto::ErrorStatus::ResourceExhausted);
    }

    #[test]
    fn a_409_cancel_of_a_finished_job_is_failed_precondition() {
        let error = parse_cancel(&response(409, b"already delivered".to_vec()))
            .expect_err("cannot un-deliver");
        assert_eq!(error.status(), pos_proto::ErrorStatus::FailedPrecondition);
    }

    #[test]
    fn a_404_cancel_is_not_found() {
        let error = parse_cancel(&response(404, b"unknown".to_vec())).expect_err("unknown");
        assert_eq!(error.status(), pos_proto::ErrorStatus::NotFound);
    }

    #[test]
    fn a_track_of_a_finished_job_answers_completed() {
        let shipment = parse_track(&response(200, body("COMPLETED"))).expect("a finished job");
        assert_eq!(shipment.status.known(), ShipmentStatus::Completed);
    }

    #[test]
    fn a_404_track_is_not_found() {
        let error = parse_track(&response(404, b"unknown".to_vec())).expect_err("unknown");
        assert_eq!(error.status(), pos_proto::ErrorStatus::NotFound);
    }

    #[test]
    fn an_unmapped_courier_status_stays_unrecognised_and_non_terminal() {
        let status = map_status("REROUTING");
        assert!(
            status.is_unrecognised(),
            "an unknown courier status must not masquerade as a known one"
        );
        assert_eq!(
            status.known(),
            ShipmentStatus::Unspecified,
            "and it degrades to the zero value, which is not terminal"
        );
    }

    #[test]
    fn a_body_that_does_not_parse_is_an_internal_contract_breach() {
        let error = parse_track(&response(200, b"not json".to_vec())).expect_err("garbage body");
        assert_eq!(error.status(), pos_proto::ErrorStatus::Internal);
    }
}
