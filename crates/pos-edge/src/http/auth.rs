// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Device authentication on the domain routes ([ADR-0084](../../../docs/adr/0084-device-authentication.md)).
//!
//! Every request to a domain route must present the bearer token a device was issued when it paired
//! ([`crate::pairing`]). A request that does not is refused with `401` before it reaches a handler,
//! which is what closes "any host on the store LAN commands the edge": the pairing code an operator
//! reads off the console is the gate, so an unpaired tablet — or a laptop someone plugged into the
//! store switch — cannot seat a table, settle a bill, or read the floor.
//!
//! The check is a [`require_paired_device`] middleware rather than a per-handler extractor so it
//! guards the whole [`domain_router`](super::domain_router) in one place, reads included: an unpaired
//! device has no more business reading the store's tables than commanding them. On success the
//! resolved [`DeviceId`] rides in the request's extensions, and the command handlers read it to build
//! their [`Actor`].
//!
//! # Device authenticated, person pending
//!
//! This slice authenticates the *device*. The *employee* a command runs as is still a placeholder
//! ([`device_actor`]) until PIN sign-in resolves a real one (a later slice); every command is now
//! attributable to a paired device, and gains a real person once sign-in lands. That is a deliberate
//! decomposition, not a gap left open: the LAN-command hole is closed here, and per-person
//! attribution and permissions follow on an authenticated foundation.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use pos_core::decision::Actor;
use pos_proto::ids::{DeviceId, EmployeeId};
use pos_proto::ulid::Ulid;

use crate::pairing::{DeviceToken, Pairing};

/// The placeholder employee a command runs as until PIN sign-in resolves a real one. Device
/// authenticated, person pending (see the module note); a later slice replaces it.
const UNASSIGNED_EMPLOYEE: u128 = 1;

/// The actor a request from the paired `device_id` runs as.
#[must_use]
pub(crate) fn device_actor(device_id: DeviceId) -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(UNASSIGNED_EMPLOYEE)),
        device_id,
    }
}

/// Refuses a request that does not carry a valid device token, and resolves the paired device for
/// the ones that do — placing its [`DeviceId`] in the request extensions for the handler.
///
/// Absent, malformed, and unknown tokens all get the same `401`, so a probe learns nothing about
/// which of the three it hit.
pub(crate) async fn require_paired_device(
    State(pairing): State<Arc<Pairing>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(device_id) = bearer(&request).and_then(|token| pairing.device_for(&token)) else {
        return (
            StatusCode::UNAUTHORIZED,
            "pair this device to reach the edge",
        )
            .into_response();
    };
    request.extensions_mut().insert(device_id);
    next.run(request).await
}

/// The device token from an `Authorization: Bearer <token>` header, if present and well-formed.
fn bearer(request: &Request) -> Option<DeviceToken> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| DeviceToken::parse(token.trim()))
}

#[cfg(test)]
mod tests {
    use super::device_actor;
    use pos_proto::ids::DeviceId;
    use pos_proto::ulid::Ulid;

    #[test]
    fn the_actor_carries_the_paired_device() {
        let device = DeviceId::new(Ulid::from_u128(0xD));
        assert_eq!(device_actor(device).device_id, device);
    }
}
