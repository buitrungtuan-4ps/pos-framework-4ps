// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A kiosk signs in as itself, and a person leaves it with a PIN
//! ([ADR-0224](../../../../docs/adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) decision 4).
//!
//! A kiosk is a till on a `TERMINAL` the console made `DEVICE_CLASS_KIOSK`, and the cloud publishes
//! a staff record of its own for that terminal, with no PIN. `POST /api/session/kiosk` signs a
//! device in as that record, and only a device bound to such a terminal: the device's token and the
//! console's binding are the credential, and anything else is refused `403`. The record then acts on
//! the fixed kiosk set alone (`pos_core::permission::KIOSK_GRANTS`).
//!
//! Leaving the kiosk screen is a person signing in on the kiosk with their code and PIN, through the
//! ordinary sign-in and its lockout. On a kiosk that sign-in is refused `403`
//! `KIOSK_EXIT_NOT_ALLOWED` unless the person holds `admin.kiosk.exit` directly, and the kiosk stays
//! signed in as itself.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use pos_core::permission::Permission;
use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::devices::{DeviceClass, PublishedDevices};
use pos_proto::ids::{DeviceId, EmployeeId};

use super::auth::{LOG_TARGET, SignInDeps, SignedIn};
use crate::clock::SystemClock;
use crate::printing::{Printers, published_terminals};

/// The kiosk terminal `device` is bound to, if it is bound to one the store's `devices` node names
/// `DEVICE_CLASS_KIOSK`. A binding that cannot be read is no kiosk: the device is then refused,
/// never signed in on a guess.
async fn kiosk_terminal_of<S>(
    deps: &SignInDeps<S>,
    printers: Option<&Arc<Printers>>,
    device: DeviceId,
) -> Option<DeviceId> {
    kiosk_terminal(|| deps.edge.session().devices.clone(), printers?, device)
        .await
        .ok()
        .flatten()
}

/// The kiosk terminal `device` is bound to, reading the store's `devices` node from `devices`.
/// Shared by the sign-in and by `/ws`, which a kiosk may not open. A store whose node names no
/// kiosk terminal has no kiosk, and its bindings are not read.
///
/// # Errors
///
/// The binding could not be read, on a store that has a kiosk terminal: whether `device` is a kiosk
/// is then unknown, and each caller refuses what it guards rather than guess (phase 9 security
/// review, finding K1).
pub(crate) async fn kiosk_terminal(
    devices: impl FnOnce() -> PublishedDevices,
    printers: &Printers,
    device: DeviceId,
) -> Result<Option<DeviceId>, PortError> {
    let kiosks: Vec<DeviceId> = published_terminals(&devices())
        .filter(|entry| entry.device_class() == Some(DeviceClass::Kiosk))
        .map(|entry| entry.device_id)
        .collect();
    if kiosks.is_empty() {
        return Ok(None);
    }
    let terminal = match printers.terminal_of(device).await {
        Ok(terminal) => terminal,
        Err(error) => {
            tracing::warn!(target: LOG_TARGET, %device, %error, "a kiosk's binding could not be read");
            return Err(error);
        }
    };
    Ok(terminal.filter(|terminal| kiosks.contains(terminal)))
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/session/kiosk",
    security(("till_session" = [])),
    responses(
        (status = 200, body = SignedIn, description = "Signed in as the kiosk's own staff record: the body carries its `employee_id`"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "This device is not bound to a kiosk terminal, or the store publishes no staff record for it"),
        (status = 503, description = "The sign-in could not be recorded"),
    ),
    tag = "session",
))]
/// `POST /api/session/kiosk` — sign this device in as its kiosk's own staff record, with no PIN
/// (ADR-0224 decision 4). The till calls it when it starts on a kiosk.
pub(crate) async fn kiosk_sign_in<S>(
    State(deps): State<SignInDeps<S>>,
    Extension(device_id): Extension<DeviceId>,
    printers: Option<Extension<Arc<Printers>>>,
) -> Response
where
    S: EventStore + Send + Sync + 'static,
{
    let printers = printers.map(|Extension(printers)| printers);
    let record = match kiosk_terminal_of(&deps, printers.as_ref(), device_id).await {
        Some(terminal) => deps.edge.session().staff.kiosk_record_of(terminal),
        None => None,
    };
    let Some(employee_id) = record else {
        tracing::warn!(target: LOG_TARGET, %device_id, "kiosk sign-in refused: not a kiosk");
        return (
            StatusCode::FORBIDDEN,
            "this device is not a kiosk the store publishes",
        )
            .into_response();
    };
    if let Err(error) = deps
        .sessions
        .sign_in(device_id, employee_id, SystemClock.now())
        .await
    {
        tracing::error!(target: LOG_TARGET, error = %error, %employee_id, "could not record a kiosk sign-in");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "could not record the sign-in",
        )
            .into_response();
    }
    // The audit trail of a sign-in, as for a person: the record's id, which is no one's name.
    tracing::info!(target: LOG_TARGET, %employee_id, %device_id, "kiosk signed in as its own record");
    (
        StatusCode::OK,
        axum::Json(SignedIn {
            employee_id: employee_id.to_string(),
        }),
    )
        .into_response()
}

/// The refusal for a person whose PIN verified on a kiosk but who may not leave it, or `None` when
/// the sign-in may go on: the device is no kiosk, or the person holds `admin.kiosk.exit` directly.
pub(super) async fn refuse_kiosk_exit<S>(
    deps: &SignInDeps<S>,
    printers: Option<&Arc<Printers>>,
    device: DeviceId,
    employee_id: EmployeeId,
) -> Option<Response> {
    kiosk_terminal_of(deps, printers, device).await?;
    if deps
        .edge
        .session()
        .staff
        .grants(employee_id, Permission::ExitKiosk)
    {
        tracing::info!(target: LOG_TARGET, %employee_id, device_id = %device, "kiosk left with a PIN");
        return None;
    }
    tracing::warn!(target: LOG_TARGET, %employee_id, "leaving a kiosk refused: not permitted");
    Some(super::refusal(
        StatusCode::FORBIDDEN,
        "KIOSK_EXIT_NOT_ALLOWED",
        "this person may not leave the kiosk".to_owned(),
    ))
}
