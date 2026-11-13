// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A delivery order's recipient, kept apart as a personal record
//! ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 3,
//! [ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)).
//!
//! When a vendor's order names who it goes to, the connector route keeps the name, phone number and
//! address in the cloud's `subjects` table, never in the relay's queue, an event or a log, and the
//! order carries only the `subject_id`:
//!
//! - the id is derived from the tenant, the order's idempotency key and when it was placed
//!   ([`recipient_subject_id`]), so a vendor's retry names the same subject and the first record
//!   stands rather than a second copy of the same person being kept;
//! - the record names the store it was collected for, and only that store's credential reads it,
//!   through `GET /sync/stores/{store_id}/subjects/{subject_id}` ([`recipient_sync_router`]);
//! - it expires on the store's own retention, the `default_retention_days` its locale publishes,
//!   counted from when the cloud kept it ([`retain_until`]). A store that publishes none leaves the
//!   record to the cloud's own period (ADR-0035): no period is guessed here.
//!
//! The subject-request tooling (ADR-0076) reads, exports and erases it like any cloud subject.

use core::future::Future;
use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::response::{api_error, parse_ulid_fields};
use pos_ports::order_in::ExternalReference;
use pos_ports::{PortError, PortName};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, SubjectId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use store_postgres::{PostgresSubjects, StoreSubjectRow};

use super::VendorRecipient;
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};

/// The field a recipient's name is kept under.
pub const NAME: &str = "name";
/// The field a recipient's phone number is kept under.
pub const PHONE: &str = "phone";
/// The field a recipient's address is kept under.
pub const ADDRESS: &str = "address";
/// The longest name kept, in characters.
pub const MAX_NAME_CHARS: usize = 120;
/// The longest phone number kept, in characters.
pub const MAX_PHONE_CHARS: usize = 32;
/// The longest address kept, in characters.
pub const MAX_ADDRESS_CHARS: usize = 500;

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// A recipient as the cloud keeps it: one subject record, for one store. Its `Debug` prints no
/// value.
#[derive(Clone, PartialEq, Eq)]
pub struct KeptRecipient {
    /// The subject the order names.
    pub subject_id: SubjectId,
    /// When the cloud kept it.
    pub collected_at: Timestamp,
    /// When it is masked, or `None` for the cloud's own period.
    pub retain_until: Option<Timestamp>,
    /// `name`, `phone` and `address`.
    pub fields: BTreeMap<String, String>,
}

impl core::fmt::Debug for KeptRecipient {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("KeptRecipient")
            .field("subject_id", &self.subject_id)
            .field("collected_at", &self.collected_at)
            .field("retain_until", &self.retain_until)
            .field("fields", &self.fields.len())
            .finish()
    }
}

/// The recipient's fields, each trimmed, present and within its limit.
///
/// # Errors
///
/// A short reason naming the field, never its value.
pub fn recipient_fields(
    recipient: &VendorRecipient,
) -> Result<BTreeMap<String, String>, &'static str> {
    let mut fields = BTreeMap::new();
    for (key, value, max, reason) in [
        (
            NAME,
            &recipient.name,
            MAX_NAME_CHARS,
            "the recipient's name is empty or too long",
        ),
        (
            PHONE,
            &recipient.phone,
            MAX_PHONE_CHARS,
            "the recipient's phone is empty or too long",
        ),
        (
            ADDRESS,
            &recipient.address,
            MAX_ADDRESS_CHARS,
            "the recipient's address is empty or too long",
        ),
    ] {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > max {
            return Err(reason);
        }
        fields.insert(key.to_owned(), value.to_owned());
    }
    Ok(fields)
}

/// The subject a delivery order's recipient is kept under: the same for the same tenant, order key
/// and placing time, so a vendor's retry names the subject its first attempt kept. A ULID whose time
/// is when the order was placed and whose random part is a hash, not the person: nothing personal
/// goes into it.
#[must_use]
pub fn recipient_subject_id(
    tenant: TenantId,
    reference: &ExternalReference,
    placed_at: Timestamp,
) -> SubjectId {
    let digest = Sha256::new()
        .chain_update(b"delivery-recipient\0")
        .chain_update(tenant.to_string().as_bytes())
        .chain_update(b"\0")
        .chain_update(reference.as_str().as_bytes())
        .finalize();
    let mut random = [0u8; 16];
    random.copy_from_slice(&digest[..16]);
    let ms = u64::try_from(placed_at.as_milliseconds_since_epoch().max(0)).unwrap_or(0);
    SubjectId::new(Ulid::from_parts(ms, u128::from_le_bytes(random)))
}

/// When a record kept at `collected_at` is masked under a store retention of `days`, or `None`
/// when the store publishes none (or zero, which would mask it at once and is not a period).
#[must_use]
pub fn retain_until(collected_at: Timestamp, days: Option<u16>) -> Option<Timestamp> {
    let days = days.filter(|days| *days > 0)?;
    let until = collected_at
        .as_milliseconds_since_epoch()
        .saturating_add(i64::from(days).saturating_mul(DAY_MS));
    Timestamp::from_milliseconds_since_epoch(until).ok()
}

/// Where recipients are kept, and the store retention they are kept under.
pub trait RecipientStore: Send + Sync {
    /// The store's personal-data retention in days, from its locale's `default_retention_days`, or
    /// `None` when it publishes none.
    fn retention_days(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<u16>, PortError>> + Send;

    /// Keeps `recipient` for `store_id`. A subject already kept stays as it is.
    fn keep(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        recipient: &KeptRecipient,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// The unmasked recipient kept for `store_id` under `subject_id`, or `None`.
    fn fetch(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        subject_id: SubjectId,
    ) -> impl Future<Output = Result<Option<KeptRecipient>, PortError>> + Send;
}

/// Keeps a vendor order's recipient for `store_id` under the store's retention, as of `now`.
///
/// # Errors
///
/// [`PortError::invalid_argument`] for a field that is empty or too long, or the store's error.
pub async fn keep_recipient<R: RecipientStore>(
    recipients: &R,
    (tenant, store_id): (TenantId, StoreId),
    subject_id: SubjectId,
    recipient: &VendorRecipient,
    now: Timestamp,
) -> Result<(), PortError> {
    let fields = recipient_fields(recipient)
        .map_err(|reason| PortError::invalid_argument(PortName::OrderIn, reason))?;
    let days = recipients.retention_days(tenant, store_id).await?;
    let kept = KeptRecipient {
        subject_id,
        collected_at: now,
        retain_until: retain_until(now, days),
        fields,
    };
    recipients.keep(tenant, store_id, &kept).await
}

/// The cloud's recipients: the `subjects` table, and each store's retention from its config tree.
#[derive(Clone, Debug)]
pub struct CloudRecipients<T> {
    subjects: PostgresSubjects,
    trees: T,
}

impl<T> CloudRecipients<T> {
    /// Recipients over the subject store and the config trees.
    #[must_use]
    pub const fn new(subjects: PostgresSubjects, trees: T) -> Self {
        Self { subjects, trees }
    }
}

fn store_error(error: &impl core::fmt::Display) -> PortError {
    PortError::unavailable(
        PortName::OrderIn,
        format!("the subject store failed: {error}"),
    )
}

impl<T: ConfigTreeStore + Send + Sync> RecipientStore for CloudRecipients<T> {
    async fn retention_days(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> Result<Option<u16>, PortError> {
        let Some(state) = self
            .trees
            .load(tenant, store_id)
            .await
            .map(crate::http::strip_tree_version)
            .map_err(|error| store_error(&error))?
        else {
            return Ok(None);
        };
        let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
        Ok(tree
            .current_effective()
            .and_then(|effective| {
                effective
                    .get("locale")
                    .and_then(|locale| locale.get("default_retention_days"))
                    .and_then(serde_json::Value::as_u64)
            })
            .and_then(|days| u16::try_from(days).ok()))
    }

    async fn keep(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        recipient: &KeptRecipient,
    ) -> Result<(), PortError> {
        let fields_json = serde_json::to_string(&recipient.fields).map_err(|_| {
            PortError::internal(PortName::OrderIn, "a recipient could not be encoded")
        })?;
        let (subject_id, tenant_id, store) = (
            recipient.subject_id.to_string(),
            tenant.to_string(),
            store_id.to_string(),
        );
        self.subjects
            .keep_for_store(&StoreSubjectRow {
                subject_id: &subject_id,
                tenant_id: &tenant_id,
                store_id: &store,
                collected_at_ms: recipient.collected_at.as_milliseconds_since_epoch(),
                retain_until_ms: recipient
                    .retain_until
                    .map(Timestamp::as_milliseconds_since_epoch),
                fields_json: &fields_json,
            })
            .await
            .map(|_inserted| ())
    }

    async fn fetch(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        subject_id: SubjectId,
    ) -> Result<Option<KeptRecipient>, PortError> {
        let Some((row, retain_until_ms)) = self
            .subjects
            .fetch_for_store(
                &tenant.to_string(),
                &store_id.to_string(),
                &subject_id.to_string(),
            )
            .await?
        else {
            return Ok(None);
        };
        let corrupt = || PortError::internal(PortName::OrderIn, "a kept recipient is corrupt");
        let at = |ms: i64| Timestamp::from_milliseconds_since_epoch(ms).map_err(|_| corrupt());
        Ok(Some(KeptRecipient {
            subject_id,
            collected_at: at(row.collected_at_ms)?,
            retain_until: retain_until_ms.map(at).transpose()?,
            fields: serde_json::from_str(&row.fields_json).map_err(|_| corrupt())?,
        }))
    }
}

/// What the store reads: the one subject record, and when it expires.
#[derive(Serialize, utoipa::ToSchema)]
struct SubjectRecordDto {
    /// The subject, a ULID.
    subject_id: String,
    /// When the cloud kept it, in Unix milliseconds.
    collected_time: i64,
    /// When it is masked, in Unix milliseconds; absent when the store publishes no retention.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    retain_until_time: Option<i64>,
    /// The personal fields: `name`, `phone` and `address`.
    fields: BTreeMap<String, String>,
}

/// What the store-facing read shares.
pub(crate) struct ReadState<R, K, C> {
    recipients: R,
    keys: K,
    clock: C,
}

impl<R: Clone, K: Clone, C: Clone> Clone for ReadState<R, K, C> {
    fn clone(&self) -> Self {
        Self {
            recipients: self.recipients.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
        }
    }
}

/// The store-facing read of a recipient: `GET /sync/stores/{store_id}/subjects/{subject_id}`, scope
/// `relay_orders`, the credential's own store only.
pub fn recipient_sync_router<R, K, C>(recipients: R, keys: K, clock: C) -> Router
where
    R: RecipientStore + Clone + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/subjects/{subject_id}",
            get(read_recipient::<R, K, C>),
        )
        .with_state(ReadState {
            recipients,
            keys,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/sync/stores/{store_id}/subjects/{subject_id}",
    security(("device_credential" = ["relay_orders"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
        ("subject_id" = String, Path, description = "The subject a relayed order names, a ULID"),
    ),
    responses(
        (status = 200, body = SubjectRecordDto, description = "The one subject record kept for this store: a \
                                      delivery order's recipient (ADR-0215). No list, no search"),
        (status = 400, description = "store_id or subject_id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `relay_orders` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "No unmasked record is kept for this store under that id: unknown, \
                                      another store's, or already masked", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store or the subject store is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "order relay",
)]
/// `GET /sync/stores/{store_id}/subjects/{subject_id}` — a store reads the recipient a relayed
/// order names. The log names the store and the subject, never a value.
pub(crate) async fn read_recipient<R, K, C>(
    State(state): State<ReadState<R, K, C>>,
    headers: HeaderMap,
    Path((store_id, subject_id)): Path<(String, String)>,
) -> Response
where
    R: RecipientStore + Clone + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::RelayOrders) {
        return forbidden.into_response();
    }
    let (store_id, subject_id) =
        match parse_ulid_fields([("store_id", &store_id), ("subject_id", &subject_id)]) {
            Ok([store_id, subject_id]) => (StoreId::new(store_id), SubjectId::new(subject_id)),
            Err(refusal) => return refusal,
        };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    match state
        .recipients
        .fetch(grant.tenant(), store_id, subject_id)
        .await
    {
        Ok(Some(kept)) => {
            tracing::info!(%store_id, %subject_id, "a store read the recipient its order names");
            (
                StatusCode::OK,
                Json(SubjectRecordDto {
                    subject_id: kept.subject_id.to_string(),
                    collected_time: kept.collected_at.as_milliseconds_since_epoch(),
                    retain_until_time: kept
                        .retain_until
                        .map(Timestamp::as_milliseconds_since_epoch),
                    fields: kept.fields,
                }),
            )
                .into_response()
        }
        Ok(None) => api_error(ErrorStatus::NotFound, "no such subject for this store"),
        Err(error) => {
            tracing::error!(%store_id, status = ?error.status(), "the subject store failed");
            api_error(ErrorStatus::Unavailable, "the subject store is unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use pos_ports::order_in::ExternalReference;
    use pos_proto::ids::TenantId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use super::{
        ADDRESS, DAY_MS, KeptRecipient, MAX_PHONE_CHARS, recipient_fields, recipient_subject_id,
        retain_until,
    };
    use crate::connectors::VendorRecipient;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("an instant")
    }

    fn fake() -> VendorRecipient {
        VendorRecipient {
            name: " Test Guest ".to_owned(),
            phone: "000-0000".to_owned(),
            address: "1 Example Street".to_owned(),
        }
    }

    #[test]
    fn a_recipient_keeps_three_trimmed_fields_each_within_its_limit() {
        let fields = recipient_fields(&fake()).expect("good");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields.get("name").map(String::as_str), Some("Test Guest"));
        assert_eq!(
            fields.get(ADDRESS).map(String::as_str),
            Some("1 Example Street")
        );
        let long = VendorRecipient {
            phone: "0".repeat(MAX_PHONE_CHARS + 1),
            ..fake()
        };
        let empty = VendorRecipient {
            address: "  ".to_owned(),
            ..fake()
        };
        for bad in [long, empty] {
            let reason = recipient_fields(&bad).expect_err("refused");
            assert!(!reason.contains("0000"), "the reason names no value");
        }
    }

    #[test]
    fn a_retry_names_the_same_subject_and_another_order_another() {
        let tenant = TenantId::new(Ulid::from_u128(1));
        let reference = ExternalReference::parse("CONN:D-1").expect("a reference");
        let other = ExternalReference::parse("CONN:D-2").expect("a reference");
        let first = recipient_subject_id(tenant, &reference, at(1_800_000_000_000));
        assert_eq!(
            first,
            recipient_subject_id(tenant, &reference, at(1_800_000_000_000))
        );
        assert_ne!(
            first,
            recipient_subject_id(tenant, &other, at(1_800_000_000_000))
        );
        assert_ne!(
            first,
            recipient_subject_id(
                TenantId::new(Ulid::from_u128(2)),
                &reference,
                at(1_800_000_000_000)
            )
        );
    }

    #[test]
    fn a_record_expires_on_the_store_s_retention_and_never_on_a_guess() {
        assert_eq!(retain_until(at(0), Some(30)), Some(at(30 * DAY_MS)));
        assert_eq!(retain_until(at(0), None), None, "no period is guessed");
        assert_eq!(retain_until(at(0), Some(0)), None, "zero is not a period");
    }

    #[test]
    fn a_kept_recipient_prints_no_value() {
        let kept = KeptRecipient {
            subject_id: pos_proto::ids::SubjectId::new(Ulid::from_u128(5)),
            collected_at: at(0),
            retain_until: None,
            fields: recipient_fields(&fake()).expect("good"),
        };
        let printed = format!("{kept:?}");
        assert!(!printed.contains("Example") && !printed.contains("Guest"));
    }
}
