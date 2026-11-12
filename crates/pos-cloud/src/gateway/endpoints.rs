// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's gateway endpoints
//! ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
//! decisions 2, 3 and 5).
//!
//! An endpoint is what a store's lookup names by id: an `https` URL (vetted by the webhook guard
//! when it is written and again before every call), a method, the fields a request is built from by
//! name and the fields a response is read into, each with a kind, a deadline, whether a retry is
//! safe, and the headers that carry its credentials.
//!
//! # Secret headers are sealed the moment they arrive
//!
//! A secret header's value is sealed with the connections' key
//! ([`crate::connections::seal`]), bound to its tenant, this endpoint's id and the header's name,
//! before the record reaches the store. No read returns it: a view says only which headers are set.
//!
//! # No personal data yet
//!
//! ADR-0198 decision 9 lets a request carry a personal field only to an endpoint declared as a
//! processor, with a data processing agreement and the country it sends data to. That declaration
//! is not built, so a field here cannot be marked personal, and a lookup must not send one.

use core::fmt;
use core::future::Future;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use pos_proto::ids::TenantId;

use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

/// The deadline an endpoint gets when it states none (decision 5).
pub const DEFAULT_DEADLINE_MS: u32 = 3_000;
/// The longest deadline an endpoint may state (decision 5).
pub const MAX_DEADLINE_MS: u32 = 10_000;
/// The shortest deadline an endpoint may state: below it no `https` round trip completes.
pub const MIN_DEADLINE_MS: u32 = 100;
/// The most fields a request, or a response, may declare.
pub const MAX_FIELDS: usize = 32;
/// The most secret headers an endpoint may hold.
pub const MAX_SECRET_HEADERS: usize = 8;
/// The longest name the console may give an endpoint, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// The longest field name, in bytes.
const MAX_FIELD_NAME: usize = 64;
/// The longest header name, in bytes.
const MAX_HEADER_NAME: usize = 64;
/// The longest secret header value, in bytes.
const MAX_HEADER_VALUE: usize = 4_096;

/// Headers the gateway sets itself, or that would change how the request is framed or routed, so
/// no endpoint may set them. Compared case-insensitively.
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "te",
    "trailer",
    "idempotency-key",
    "proxy-authorization",
    "proxy-connection",
];

/// How the gateway calls an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum GatewayMethod {
    /// `GET`, with the request's fields as the query string.
    #[serde(rename = "GATEWAY_METHOD_GET")]
    Get,
    /// `POST`, with the request's fields as a JSON object.
    #[serde(rename = "GATEWAY_METHOD_POST")]
    Post,
}

/// The kind of one field's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum GatewayFieldKind {
    /// Text.
    #[serde(rename = "GATEWAY_FIELD_KIND_STRING")]
    String,
    /// A whole number. Never an amount of money: a lookup's answer is read, not charged.
    #[serde(rename = "GATEWAY_FIELD_KIND_INTEGER")]
    Integer,
    /// True or false.
    #[serde(rename = "GATEWAY_FIELD_KIND_BOOLEAN")]
    Boolean,
}

/// One field a request is built from, or a response is read into, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GatewayField {
    /// Its name: lower-case letters, digits and `_`, starting with a letter, at most 64 bytes.
    pub name: String,
    /// The kind its value must be.
    pub kind: GatewayFieldKind,
    /// Whether a request must carry it, or a response must answer it.
    #[serde(default)]
    pub required: bool,
}

/// One endpoint as stored, with its secret headers sealed.
///
/// Never serialized to a response. [`GatewayEndpointView`] is what the API answers, and it has no
/// field a sealed value could travel in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayEndpoint {
    /// The endpoint's id, a server-minted ULID.
    pub gateway_endpoint_id: String,
    /// What the console calls it.
    pub display_name: String,
    /// The `https` URL, as the webhook guard normalised it.
    pub url: String,
    /// How it is called.
    pub method: GatewayMethod,
    /// The fields a request is built from.
    #[serde(default)]
    pub request_fields: Vec<GatewayField>,
    /// The fields a response is read into; anything else it answers is dropped.
    #[serde(default)]
    pub response_fields: Vec<GatewayField>,
    /// How long a call may take, in milliseconds.
    pub deadline_ms: u32,
    /// Whether a call may be retried on a transport failure or a `5xx` (decision 6).
    #[serde(default)]
    pub idempotent: bool,
    /// Whether stores may call it. A disabled endpoint keeps its secrets.
    pub enabled: bool,
    /// Each secret header's sealed value, by header name in lower case.
    #[serde(default)]
    pub sealed_headers: BTreeMap<String, String>,
}

impl GatewayEndpoint {
    /// The secret headers this endpoint holds a value for.
    #[must_use]
    pub fn secret_headers_set(&self) -> BTreeSet<String> {
        self.sealed_headers.keys().cloned().collect()
    }
}

/// An endpoint as the API answers it: everything but its sealed values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct GatewayEndpointView {
    /// The endpoint's id.
    pub gateway_endpoint_id: String,
    /// What the console calls it.
    pub display_name: String,
    /// The `https` URL.
    pub url: String,
    /// How it is called.
    pub method: GatewayMethod,
    /// The fields a request is built from.
    pub request_fields: Vec<GatewayField>,
    /// The fields a response is read into.
    pub response_fields: Vec<GatewayField>,
    /// How long a call may take, in milliseconds.
    pub deadline_ms: u32,
    /// Whether a call may be retried.
    pub idempotent: bool,
    /// Whether stores may call it.
    pub enabled: bool,
    /// The secret headers that hold a value. Never the value.
    pub secret_headers_set: Vec<String>,
}

impl GatewayEndpointView {
    /// The view of `endpoint`.
    #[must_use]
    pub fn of(endpoint: &GatewayEndpoint) -> Self {
        Self {
            gateway_endpoint_id: endpoint.gateway_endpoint_id.clone(),
            display_name: endpoint.display_name.clone(),
            url: endpoint.url.clone(),
            method: endpoint.method,
            request_fields: endpoint.request_fields.clone(),
            response_fields: endpoint.response_fields.clone(),
            deadline_ms: endpoint.deadline_ms,
            idempotent: endpoint.idempotent,
            enabled: endpoint.enabled,
            secret_headers_set: endpoint.secret_headers_set().into_iter().collect(),
        }
    }
}

/// Why a declared endpoint is refused: the field and the reason the API names, and its prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointRefusal {
    /// The request field at fault.
    pub field: &'static str,
    /// The machine-readable reason.
    pub reason: &'static str,
    /// The sentence the API answers.
    pub message: &'static str,
}

impl EndpointRefusal {
    const fn new(field: &'static str, reason: &'static str, message: &'static str) -> Self {
        Self {
            field,
            reason,
            message,
        }
    }
}

/// The display name, trimmed, or the refusal of a blank or long one.
///
/// # Errors
///
/// An [`EndpointRefusal`] on `display_name`.
pub fn checked_name(name: &str) -> Result<String, EndpointRefusal> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(EndpointRefusal::new(
            "display_name",
            "REQUIRED",
            "an endpoint needs a name the console can show",
        ));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(EndpointRefusal::new(
            "display_name",
            "TOO_LONG",
            "an endpoint's name is at most 80 characters",
        ));
    }
    Ok(trimmed.to_owned())
}

/// The deadline an endpoint states, or the default, within the bounds of decision 5.
///
/// # Errors
///
/// An [`EndpointRefusal`] on `deadline_ms` outside [`MIN_DEADLINE_MS`]..=[`MAX_DEADLINE_MS`].
pub fn checked_deadline(deadline_ms: Option<u32>) -> Result<u32, EndpointRefusal> {
    let deadline = deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS);
    if (MIN_DEADLINE_MS..=MAX_DEADLINE_MS).contains(&deadline) {
        Ok(deadline)
    } else {
        Err(EndpointRefusal::new(
            "deadline_ms",
            "OUT_OF_RANGE",
            "a deadline is between 100 and 10000 milliseconds",
        ))
    }
}

/// Whether `name` is a field name: a lower-case letter, then lower-case letters, digits and `_`.
fn is_field_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= MAX_FIELD_NAME
        && bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Checks one side's fields: at most [`MAX_FIELDS`], each named well and once.
///
/// # Errors
///
/// An [`EndpointRefusal`] on `field`.
pub fn check_fields(fields: &[GatewayField], field: &'static str) -> Result<(), EndpointRefusal> {
    if fields.len() > MAX_FIELDS {
        return Err(EndpointRefusal::new(
            field,
            "TOO_MANY",
            "an endpoint declares at most 32 fields on each side",
        ));
    }
    let mut seen = BTreeSet::new();
    for declared in fields {
        if !is_field_name(&declared.name) {
            return Err(EndpointRefusal::new(
                field,
                "INVALID_FORMAT",
                "a field name is lower-case letters, digits and _, starting with a letter",
            ));
        }
        if !seen.insert(declared.name.as_str()) {
            return Err(EndpointRefusal::new(
                field,
                "DUPLICATE",
                "a field is declared once",
            ));
        }
    }
    Ok(())
}

/// A secret header's name in lower case, or the refusal of one the gateway owns or that is not a
/// header token.
///
/// # Errors
///
/// An [`EndpointRefusal`] on `secret_headers`.
pub fn checked_header_name(name: &str) -> Result<String, EndpointRefusal> {
    let lower = name.trim().to_ascii_lowercase();
    let token = !lower.is_empty()
        && lower.len() <= MAX_HEADER_NAME
        && lower
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if !token {
        return Err(EndpointRefusal::new(
            "secret_headers",
            "INVALID_FORMAT",
            "a header name is letters, digits and -, at most 64 of them",
        ));
    }
    if RESERVED_HEADERS.contains(&lower.as_str()) {
        return Err(EndpointRefusal::new(
            "secret_headers",
            "RESERVED",
            "the gateway sets that header itself",
        ));
    }
    Ok(lower)
}

/// Checks a secret header's value: present, bounded, and on one line.
///
/// # Errors
///
/// An [`EndpointRefusal`] on `secret_headers`.
pub fn check_header_value(value: &str) -> Result<(), EndpointRefusal> {
    if value.is_empty()
        || value.len() > MAX_HEADER_VALUE
        || value
            .bytes()
            .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
    {
        return Err(EndpointRefusal::new(
            "secret_headers",
            "INVALID_FORMAT",
            "a header value is one line of at most 4096 bytes",
        ));
    }
    Ok(())
}

/// Persists and reads a tenant's gateway endpoints: tenant-scoped, a create that refuses a taken
/// id, an update only at the version the caller read, and a delete that is not an error when the
/// row is already gone, as [`crate::connections::ConnectionStore`].
pub trait GatewayEndpointStore {
    /// Every endpoint a tenant has, in id order.
    fn list(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError>> + Send;

    /// One endpoint by id, or `None`.
    fn get(
        &self,
        tenant_id: TenantId,
        gateway_endpoint_id: &str,
    ) -> impl Future<Output = Result<Option<Versioned<GatewayEndpoint>>, GatewayEndpointStoreError>> + Send;

    /// Inserts an endpoint, refusing if one already holds its id.
    fn create(
        &self,
        tenant_id: TenantId,
        endpoint: &GatewayEndpoint,
    ) -> impl Future<Output = Result<CreateOutcome, GatewayEndpointStoreError>> + Send;

    /// Replaces an endpoint, only at the version the caller read it at.
    fn update(
        &self,
        tenant_id: TenantId,
        endpoint: &GatewayEndpoint,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, GatewayEndpointStoreError>> + Send;

    /// Removes an endpoint and its sealed headers. Removing one that does not exist is not an
    /// error.
    fn delete(
        &self,
        tenant_id: TenantId,
        gateway_endpoint_id: &str,
    ) -> impl Future<Output = Result<(), GatewayEndpointStoreError>> + Send;
}

/// A failure of the endpoint store itself: the database is unreachable, or a stored row could not
/// be decoded. Never carries a secret value: the store only holds sealed ones.
#[derive(Debug, thiserror::Error)]
pub struct GatewayEndpointStoreError(String);

impl GatewayEndpointStoreError {
    /// Wraps a message, for the server's log.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for GatewayEndpointStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the gateway endpoint store failed: {}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GatewayEndpoint, GatewayEndpointView, GatewayField, GatewayFieldKind, GatewayMethod,
        check_fields, check_header_value, checked_deadline, checked_header_name, checked_name,
    };

    fn field(name: &str) -> GatewayField {
        GatewayField {
            name: name.to_owned(),
            kind: GatewayFieldKind::String,
            required: false,
        }
    }

    #[test]
    fn a_deadline_defaults_to_three_seconds_and_stays_within_ten() {
        assert_eq!(checked_deadline(None), Ok(3_000));
        assert_eq!(checked_deadline(Some(10_000)), Ok(10_000));
        assert_eq!(checked_deadline(Some(100)), Ok(100));
        assert_eq!(
            checked_deadline(Some(10_001)).map_err(|refusal| refusal.reason),
            Err("OUT_OF_RANGE")
        );
        assert!(checked_deadline(Some(99)).is_err());
    }

    #[test]
    fn fields_are_named_well_once_and_at_most_thirty_two() {
        assert!(check_fields(&[field("code"), field("store_2")], "request_fields").is_ok());
        let reason = |fields: &[GatewayField]| {
            check_fields(fields, "request_fields").map_err(|refusal| refusal.reason)
        };
        assert_eq!(reason(&[field("Code")]), Err("INVALID_FORMAT"));
        assert_eq!(reason(&[field("2code")]), Err("INVALID_FORMAT"));
        assert_eq!(reason(&[field("")]), Err("INVALID_FORMAT"));
        assert_eq!(reason(&[field("a"), field("a")]), Err("DUPLICATE"));
        let many: Vec<GatewayField> = (0..33).map(|n| field(&format!("f{n}"))).collect();
        assert_eq!(reason(&many), Err("TOO_MANY"));
    }

    #[test]
    fn a_secret_header_is_a_token_the_gateway_does_not_set_itself() {
        assert_eq!(
            checked_header_name(" X-Api-Key "),
            Ok("x-api-key".to_owned())
        );
        for reserved in [
            "Host",
            "content-length",
            "Idempotency-Key",
            "proxy-authorization",
        ] {
            assert_eq!(
                checked_header_name(reserved).map_err(|refusal| refusal.reason),
                Err("RESERVED"),
                "{reserved}"
            );
        }
        assert!(checked_header_name("x api key").is_err());
        assert!(checked_header_name("").is_err());
        assert!(check_header_value("token").is_ok());
        assert!(check_header_value("a\r\nX-Injected: 1").is_err());
        assert!(check_header_value("").is_err());
    }

    #[test]
    fn a_name_is_trimmed_and_bounded() {
        assert_eq!(checked_name("  Loyalty  "), Ok("Loyalty".to_owned()));
        assert!(checked_name("   ").is_err());
        assert!(checked_name(&"x".repeat(81)).is_err());
    }

    #[test]
    fn a_view_names_the_secret_headers_set_and_never_their_values() {
        let endpoint = GatewayEndpoint {
            gateway_endpoint_id: "01J00000000000000000000G01".to_owned(),
            display_name: "Loyalty".to_owned(),
            url: "https://93.184.216.34/balance".to_owned(),
            method: GatewayMethod::Post,
            request_fields: vec![field("code")],
            response_fields: vec![field("balance")],
            deadline_ms: 3_000,
            idempotent: true,
            enabled: true,
            sealed_headers: [("x-api-key".to_owned(), "sealed-value".to_owned())].into(),
        };
        let view = serde_json::to_string(&GatewayEndpointView::of(&endpoint)).expect("serialise");
        assert!(view.contains(r#""secret_headers_set":["x-api-key"]"#));
        assert!(!view.contains("sealed-value"));
        assert!(view.contains(r#""method":"GATEWAY_METHOD_POST""#));
    }
}
