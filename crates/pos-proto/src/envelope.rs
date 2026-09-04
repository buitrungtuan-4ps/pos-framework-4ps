// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The event envelope — identical on every channel — and the two-layer decode that
//! lets a node handle an event it does not fully understand.
//!
//! # Three version numbers, three jobs
//!
//! Conflating any two of these causes a bad afternoon, so they are named apart:
//!
//! | Number | Governs | Where |
//! |---|---|---|
//! | [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION) | the language the two tiers speak, including this envelope's shape | negotiated once per connection ([ADR-0024](../../../docs/adr/0024-protocol-version-negotiation.md)) |
//! | [`EventEnvelope::schema_version`] | the shape of **one event type's payload** | on every event |
//!
//! A third number used to be listed here: a `pos-api-version` header, "a minor-version pin for
//! external API callers". Nothing ever read it, and roadmap **Q5** removed it from the published
//! header table rather than leave a pin that pins nothing (`docs/naming-and-api.md` §4). There are
//! two numbers, not three.
//!
//! # Why the payload is preserved verbatim and the envelope is not
//!
//! An unrecognised **payload** field must survive: `schema_version` is per-event and
//! additive, so a cloud can add a field to one event without a protocol bump, and an
//! older edge must still be able to store, forward and reconcile that event. Hence
//! [`RawPayload`], which holds the bytes exactly as received.
//!
//! An unrecognised **envelope** field need not survive, because changing the envelope
//! *is* a protocol change, and the handshake means a node only ever sees envelopes of
//! a version it agreed to speak. That is why this struct does not try to round-trip
//! unknown top-level fields — the guarantee comes from the handshake instead, which is
//! a better place for it.
//!
//! Neither struct uses `deny_unknown_fields`: rejecting an unknown field would turn
//! every additive change into a break, which is the opposite of the intent.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::ids::{BrandId, DeviceId, EmployeeId, EventId, ShiftId, StoreId, TenantId};
use crate::time::{BusinessDate, Timestamp};

/// The envelope carried by every event, on every channel.
///
/// `D` is the payload: [`RawPayload`] on receipt, a typed value after decoding.
///
/// `PartialEq` is derived rather than hand-written, so comparing two envelopes compares
/// their payloads by whatever rule `D` defines. For [`RawPayload`] that is a textual
/// comparison of the JSON — see that type's implementation, which explains why it is not
/// semantic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EventEnvelope<D> {
    /// A ULID, which **doubles as the receiver's idempotency key**.
    ///
    /// A retry must reuse it. If a command minted a fresh identifier on each attempt,
    /// at-least-once delivery would become at-least-once *effect*, and a bill could be
    /// settled twice. That is why a command carries a caller-supplied request
    /// identifier which becomes this value.
    pub event_id: EventId,

    /// `domain.resource.action`, action in the past tense.
    pub event_type: EventTypeRef,

    /// When it happened. RFC 3339, UTC.
    pub event_time: Timestamp,

    /// The trading day it belongs to.
    ///
    /// Not derivable from `event_time` alone: it needs the store's timezone and
    /// cut-off hour, both of which are configuration and both of which change. So it
    /// is computed **once, at capture, on the device** and carried here. Recomputing
    /// it downstream would silently rewrite history the next time a store's cut-off
    /// was edited.
    pub business_date: BusinessDate,

    /// The payload contract version for this `event_type`.
    ///
    /// Increases only when a break is unavoidable. Adding an optional field does not
    /// bump it — that is what "additive" means.
    pub schema_version: u16,

    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Owning brand.
    pub brand_id: BrandId,
    /// Originating store.
    pub store_id: StoreId,
    /// Originating device.
    pub device_id: DeviceId,

    /// The member of staff responsible, when there is one.
    ///
    /// Absent for system-originated events: a published configuration version, a
    /// completed device activation, a fleet rollout. The envelope's *shape* is still
    /// identical on every channel; this field is simply optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<EmployeeId>,

    /// The cash shift, when the event occurred inside one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift_id: Option<ShiftId>,

    /// The payload.
    pub data: D,
}

impl<D> EventEnvelope<D> {
    /// Replaces the payload, keeping every context field.
    ///
    /// Used by the decode step, and by anything that needs to map a payload without
    /// restating eleven fields.
    pub fn map_data<T>(self, transform: impl FnOnce(D) -> T) -> EventEnvelope<T> {
        EventEnvelope {
            event_id: self.event_id,
            event_type: self.event_type,
            event_time: self.event_time,
            business_date: self.business_date,
            schema_version: self.schema_version,
            tenant_id: self.tenant_id,
            brand_id: self.brand_id,
            store_id: self.store_id,
            device_id: self.device_id,
            employee_id: self.employee_id,
            shift_id: self.shift_id,
            data: transform(self.data),
        }
    }
}

/// An event payload exactly as it arrived, still undecoded.
///
/// Holding the original bytes is what lets a node forward, store, checksum and
/// reconcile an event whose payload contains fields it has never heard of. Decoding
/// eagerly and re-encoding would silently drop them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawPayload(Box<RawValue>);

impl PartialEq for RawPayload {
    /// Compares the JSON text as received.
    ///
    /// Textual rather than semantic: `{"a":1}` and `{ "a" : 1 }` are unequal here even
    /// though they mean the same thing. That is the right choice for this type, whose
    /// whole purpose is byte-preservation — an equality that ignored formatting would
    /// quietly contradict the guarantee.
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl Eq for RawPayload {}

impl RawPayload {
    /// The payload as JSON text.
    #[must_use]
    pub fn as_json(&self) -> &str {
        self.0.get()
    }

    /// Decodes into a typed payload.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] when the payload does not match the shape `T` expects.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, DecodeError> {
        serde_json::from_str(self.0.get()).map_err(|source| DecodeError {
            message: source.to_string(),
        })
    }

    /// Builds a raw payload by serialising a typed one.
    ///
    /// # Errors
    ///
    /// [`DecodeError`] if the value cannot be represented as JSON.
    pub fn encode<T: Serialize>(value: &T) -> Result<Self, DecodeError> {
        let text = serde_json::to_string(value).map_err(|source| DecodeError {
            message: source.to_string(),
        })?;
        RawValue::from_string(text)
            .map(Self)
            .map_err(|source| DecodeError {
                message: source.to_string(),
            })
    }
}

/// An event type as it appeared on the wire.
///
/// Keeps the token verbatim so re-serialisation is byte-identical, and reports
/// separately whether this build recognises it. A `None` from [`EventTypeRef::known`]
/// is not an error — it is an event from a newer sender, which must still be storable
/// and forwardable.
#[derive(Clone, PartialEq, Eq)]
pub struct EventTypeRef {
    known: Option<crate::events::EventType>,
    raw: std::borrow::Cow<'static, str>,
}

impl EventTypeRef {
    /// Wraps a type this build understands. Borrows its static token, so no
    /// allocation.
    #[must_use]
    pub const fn from_known(event_type: crate::events::EventType) -> Self {
        Self {
            known: Some(event_type),
            raw: std::borrow::Cow::Borrowed(event_type.as_str()),
        }
    }

    /// Parses a token, retaining it whether or not it is recognised.
    #[must_use]
    pub fn parse(token: &str) -> Self {
        match crate::events::EventType::parse(token) {
            Some(event_type) => Self::from_known(event_type),
            None => Self {
                known: None,
                raw: std::borrow::Cow::Owned(token.to_owned()),
            },
        }
    }

    /// The recognised type, or `None` for an event from a newer vocabulary.
    #[must_use]
    pub const fn known(&self) -> Option<crate::events::EventType> {
        self.known
    }

    /// The token as it will be written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl From<crate::events::EventType> for EventTypeRef {
    fn from(value: crate::events::EventType) -> Self {
        Self::from_known(value)
    }
}

impl core::fmt::Display for EventTypeRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl core::fmt::Debug for EventTypeRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.known.is_some() {
            write!(f, "EventTypeRef({})", self.raw)
        } else {
            write!(f, "EventTypeRef(unrecognised {:?})", self.raw)
        }
    }
}

impl Serialize for EventTypeRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventTypeRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = <&str>::deserialize(deserializer)?;
        Ok(Self::parse(token))
    }
}

/// A payload could not be decoded into the shape its event type declares.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("could not decode event payload: {message}")]
pub struct DecodeError {
    /// What the deserialiser objected to.
    pub message: String,
}

/// Implemented by every payload in the catalogue.
///
/// Supplied by the `event_catalogue!` macro rather than by hand, so a payload cannot
/// declare one event type and be registered under another.
pub trait EventPayload: Serialize + serde::de::DeserializeOwned {
    /// The event type this payload belongs to.
    const EVENT_TYPE: crate::events::EventType;

    /// The payload contract version.
    const SCHEMA_VERSION: u16;

    /// Every field name, for the personal-data name check and the snapshot.
    const FIELD_NAMES: &'static [&'static str];
}
