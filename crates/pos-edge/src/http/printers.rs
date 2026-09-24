// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's printers, and a test page on one (the setup plan's step 1.3).
//!
//! A new store's printers arrive in the published `devices` node, and until now the only way to
//! learn whether one was wired right was to sell something and watch for paper. `GET /api/printers`
//! lists what was published; `POST /api/printers/{device_id}/test` prints a page that names the
//! printer, through the same dispatch a receipt takes — direct, or through the device that owns its
//! transport (ADR-0112) — so a page that prints proves the path the guest's receipt will take.
//!
//! The test page needs [`Permission::ManageDevices`], like binding a print agent: it is paper and
//! noise at the counter, and on an agent-routed printer it is a job on a queue.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use pos_core::business_date::{StoreTimeZone, local_time};
use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::ids::DeviceId;
use pos_proto::time::Timestamp;

use crate::app::Edge;
use crate::clock::SystemClock;
use crate::http::{bad_request, parse_ulid};
use crate::printing::{Printers, published_printers};

/// One published printer, as the Devices screen lists it.
#[derive(Debug, Serialize)]
pub(crate) struct PrinterResponse {
    device_id: String,
    name: String,
    /// The station it serves, absent for the receipt printer.
    #[serde(skip_serializing_if = "Option::is_none")]
    station_id: Option<String>,
}

/// What came of a test page: the same token a receipt's print reports (`PRINTED`, `NO_PRINTER`,
/// `PRINTER_UNAVAILABLE`, …).
#[derive(Debug, Serialize)]
pub(crate) struct TestPrintResponse {
    print: &'static str,
}

/// `GET /api/printers` — the printers this store published, in publication order.
pub(crate) async fn list<S>(State(edge): State<Arc<Edge<S>>>) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let session = edge.session();
    let printers: Vec<PrinterResponse> = published_printers(&session.devices)
        .into_iter()
        .map(|device| PrinterResponse {
            device_id: device.device_id.to_string(),
            name: device.name.as_str().to_owned(),
            station_id: device.station_id.map(|station| station.to_string()),
        })
        .collect();
    Json(printers).into_response()
}

/// `POST /api/printers/{device_id}/test` — print a page that names the printer.
pub(crate) async fn test<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    printers: Option<Extension<Arc<Printers>>>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let Some(device_id) = parse_ulid(&id).map(DeviceId::new) else {
        return bad_request("a printer id is a ULID");
    };
    let session = edge.session();
    let may_manage = session
        .staff
        .permissions_for(actor.employee_id)
        .is_some_and(|granted| granted.contains(Permission::ManageDevices));
    if !may_manage {
        return (
            StatusCode::FORBIDDEN,
            [(crate::http::ERROR_REASON_HEADER, "PERMISSION_DENIED")],
            "printing a test page needs a manager signed in on this device",
        )
            .into_response();
    }
    // A composition with no dispatcher layered in (a route test, the fakes example) has nothing to
    // print on, which is the truth to report rather than a silent success.
    let outcome = match printers {
        Some(Extension(printers)) => {
            printers
                .print_test_page(
                    &session,
                    edge.store_id(),
                    edge.print_job_id(),
                    device_id,
                    &printed_at(SystemClock.now(), &session.timezone),
                )
                .await
        }
        None => crate::printing::PrintOutcome::NoPrinter,
    };
    Json(TestPrintResponse {
        print: outcome.as_wire(),
    })
    .into_response()
}

/// When a test page printed, as the shop's wall clock reads it: the store's timezone, named.
///
/// A store PC's clock usually keeps UTC, so the instant's own text told somebody in Ho Chi Minh City
/// that a page printed at lunchtime came out at five in the morning.
fn printed_at(now: Timestamp, zone: &StoreTimeZone) -> String {
    match (local_time(now, zone), zone.iana_name()) {
        (Ok(local), Some(name)) => format!("{local} ({name})"),
        (Ok(local), None) => local.to_string(),
        // Unreachable for a clock reading. The instant is still the truth, so print that.
        (Err(_), _) => now.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use pos_core::business_date::StoreTimeZone;

    use super::printed_at;

    #[test]
    fn a_test_page_prints_the_shop_s_time_and_names_its_zone() {
        let saigon = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("a real zone");
        let now = "2026-09-24T05:40:00Z".parse().expect("an instant");
        assert_eq!(
            printed_at(now, &saigon),
            "2026-09-24 12:40 (Asia/Ho_Chi_Minh)"
        );
    }
}
