// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The queries and request bodies the people routes take, and the PIN and error helpers they share.

use axum::response::Response;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use serde::Deserialize;

use crate::auth::password::hash_password;

/// The tenant a people read/write is scoped to (the super-admin is global, ADR-0060).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct PeopleTenantQuery {
    /// The tenant to act within (a 26-character ULID).
    pub(super) tenant_id: String,
}

/// `GET /admin/people/employees`: the tenant, plus the optional paging bounds.
///
/// Separate from [`PeopleTenantQuery`] rather than adding the two fields to it, because that struct
/// is shared by five routes and only this one reads a page. A `?limit=` on the other four would be
/// accepted and ignored — a query string that looks honoured and is not.
///
/// The `limit`/`offset` pair is repeated per route for the reason [`VoucherListQuery`] gives.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct EmployeeListQuery {
    /// The tenant whose employees to list (a 26-character ULID).
    pub(super) tenant_id: String,
    /// How many employees to return. **Absent means unpaged**: the whole roster, as an array.
    #[serde(default)]
    pub(super) limit: Option<String>,
    /// How many employees to skip. Only meaningful with `limit`.
    #[serde(default)]
    pub(super) offset: Option<String>,
    /// A case-insensitive substring the person's name or staff code must contain.
    ///
    /// Only meaningful with `limit`: on the unpaged read the route refuses rather than quietly
    /// returning the whole roster, the same as every other search on this surface.
    ///
    #[serde(default)]
    pub(super) q: Option<String>,
    /// Which order to return the page in — one of [`EmployeeSort::tokens`]. Only with `limit`.
    #[serde(default)]
    pub(super) sort: Option<String>,
    /// `asc` or `desc`, inverting `sort`'s natural direction. Only with `limit`.
    #[serde(default)]
    pub(super) order: Option<String>,
}

/// The tenant plus which assignments to list: exactly one of `store_id` (everyone who works at a
/// store, whatever reaches it), `employee_id` (everything a person holds), `store_group_id` (what
/// names a group) or `scope_kind=ASSIGNMENT_SCOPE_TENANT` (what reaches every store).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct AssignmentListQuery {
    pub(super) tenant_id: String,
    #[serde(default)]
    pub(super) store_id: Option<String>,
    #[serde(default)]
    pub(super) employee_id: Option<String>,
    #[serde(default)]
    pub(super) store_group_id: Option<String>,
    #[serde(default)]
    pub(super) scope_kind: Option<String>,
}

/// Which listing an [`AssignmentListQuery`] asks for, by the one selector it carries.
#[derive(Debug, Clone, Copy)]
pub(super) enum AssignmentListing {
    /// Everyone who works at a store.
    Store,
    /// Everything a person holds.
    Employee,
    /// What names a group.
    Group,
    /// What reaches every store.
    TenantWide,
}

/// Create an employee — identity only; the PIN is set separately.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct CreateEmployeeRequest {
    pub(super) tenant_id: String,
    pub(super) code: String,
    pub(super) name: String,
}

/// Rename an employee and/or set their status (`active`/`archived`).
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateEmployeeRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    pub(super) status: String,
}

/// Set or reset an employee's sign-in PIN. The digits are hashed here and never stored or logged in
/// the clear (ADR-0070); the request body is the only place they appear, and it is never audited.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct SetPinRequest {
    pub(super) tenant_id: String,
    pub(super) pin: String,
}

/// Create a role template — a named subset of the `pos-core` permission catalogue (§9).
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct CreateRoleRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    /// What the role grants directly, stored as sent.
    pub(super) permissions: Vec<String>,
    /// What the role grants only with approval (ADR-0158 decision 4), checked by
    /// [`granted_with_approval`]. Absent is none: every permission in `permissions` is granted
    /// directly, as before the list existed.
    ///
    /// The catalogue's PIN flag, the default a new role starts from, is the console's to apply: its
    /// role editor starts a newly ticked PIN-flagged permission with approval and always sends both
    /// lists. Applied here, it would give a role created by a script or an old console tab holders
    /// who can no longer approve, since only a direct holder approves, and a `permissions` that
    /// reads back without the ids it was sent.
    #[serde(default)]
    pub(super) permissions_with_approval: Vec<String>,
    /// How much this role may discount before it needs a manager, in the currency's minor unit.
    /// Absent leaves the role with no configured ceiling, which the edge reads as zero — every
    /// discount then needs a manager, exactly as it does today.
    #[serde(default)]
    pub(super) discount_ceiling_minor: Option<i64>,
}

/// Update a role template's name, permission sets, ceiling and status.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateRoleRequest {
    pub(super) tenant_id: String,
    pub(super) name: String,
    /// What the role grants directly.
    pub(super) permissions: Vec<String>,
    /// What the role grants only with approval (ADR-0158 decision 4). Absent, the role keeps what
    /// it grants with approval, less anything `permissions` now grants directly
    /// ([`granted_with_approval`]).
    #[serde(default)]
    pub(super) permissions_with_approval: Option<Vec<String>>,
    /// How much this role may discount before it needs a manager, in the currency's minor unit.
    /// Absent leaves the role with no configured ceiling, which the edge reads as zero — every
    /// discount then needs a manager, exactly as it does today.
    #[serde(default)]
    pub(super) discount_ceiling_minor: Option<i64>,
    pub(super) status: String,
}

/// Assign an employee with a role, at one store, a store group or every store of the tenant
/// (ADR-0158 decision 3) — see [`requested_scope`] for how the three fields name one.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct CreateAssignmentRequest {
    pub(super) tenant_id: String,
    pub(super) employee_id: String,
    /// The store, for an assignment to one store.
    #[serde(default)]
    pub(super) store_id: Option<String>,
    /// The group, for an assignment to every store it holds.
    #[serde(default)]
    pub(super) store_group_id: Option<String>,
    /// An [`AssignmentScopeKind`] token. Absent, or `ASSIGNMENT_SCOPE_UNSPECIFIED`, the scope is
    /// the one the id sent names.
    #[serde(default)]
    pub(super) scope_kind: Option<String>,
    pub(super) role_template_id: String,
}

/// The most digits a PIN may have.
pub(super) const PIN_MAX_DIGITS: u32 = 8;

/// A PIN must be digits, from `fewest` to [`PIN_MAX_DIGITS`] of them: four to eight until the tenant
/// sets a minimum of its own (`session.pin_min_length`, ADR-0160). Its defence is the Argon2id cost
/// plus the edge's attempt lockout, not its length (ADR-0030/0070); the minimum is the tenant's
/// policy, and this otherwise only rejects the obviously wrong.
pub(super) fn pin_is_well_formed(pin: &str, fewest: u32) -> bool {
    u32::try_from(pin.len()).is_ok_and(|digits| (fewest..=PIN_MAX_DIGITS).contains(&digits))
        && pin.bytes().all(|byte| byte.is_ascii_digit())
}

/// Hashes a PIN with Argon2id under a fresh 16-byte CSPRNG salt — the same primitive and shape as
/// [`mint_credential`]. `None` if OS entropy is unavailable or hashing fails, so the caller fails
/// closed rather than store a weak or empty hash.
pub(super) fn hash_pin(pin: &str) -> Option<String> {
    let mut salt_bytes = [0_u8; 16];
    getrandom::fill(&mut salt_bytes).ok()?;
    hash_password(pin, &salt_bytes).ok()
}

/// Maps any people-store failure to a retryable `503`, logging the detail rather than leaking it.
pub fn people_error_response(error: &impl std::fmt::Display) -> Response {
    tracing::error!(%error, "a people & access store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the people service is unavailable",
    )
}

/// `503` when OS entropy is unavailable to mint an id or hash a PIN — the request cannot proceed
/// safely, and it is transient.
pub(super) fn people_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy for a people & access write");
    api_error(
        ErrorStatus::Unavailable,
        "the people service is unavailable",
    )
}
