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

use crate::chain::{ChainHash, ChainLink, preimage};
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

    /// Where this record sits in its store's hash chain, or `None` for one written before
    /// the chain existed ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md)).
    ///
    /// Additive, and `None` is a real answer rather than a defect: a store's chain begins
    /// at its first record after the migration, and everything earlier verifies as
    /// **unchained** rather than as broken. Backfilling would compute a chain over history
    /// nobody can vouch for, which is the false confidence the record rejects.
    ///
    /// It rides on the envelope rather than in a store's own columns so that it crosses
    /// every channel the envelope does — a cloud reader verifies the same bytes the store
    /// wrote, instead of taking the store's word for its own integrity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<ChainLink>,

    /// The payload.
    pub data: D,
}

impl<D> EventEnvelope<D> {
    /// Replaces the payload, keeping every context field.
    ///
    /// Used by the decode step, and by anything that needs to map a payload without
    /// restating twelve fields.
    ///
    /// **Every field is restated here, which is a hazard**: a field added to the struct
    /// and forgotten here is silently dropped by every caller, and nothing fails to
    /// compile because the struct literal is still complete. `MenuCatalog::localized`
    /// lost its modifier groups that exact way. `chain` in particular must survive —
    /// a decode step that dropped it would leave every decoded event looking unchained.
    /// `keeps_every_field_including_the_chain` below is the guard.
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
            chain: self.chain,
            data: transform(self.data),
        }
    }
}

impl<D: Serialize> EventEnvelope<D> {
    /// The bytes this record's chain hash is taken over, for a proposed link.
    ///
    /// The record is serialized **with its own chain field cleared**, because a record
    /// cannot contain its own hash and a preimage over a half-filled link would depend on
    /// which half was filled. The link is then prepended by
    /// [`chain::preimage`](crate::chain::preimage), which is where the exact format lives.
    ///
    /// The caller digests the result — see that module for why `pos-proto` does not.
    ///
    /// # Errors
    ///
    /// The payload failed to serialize, which for a stored envelope means it was built
    /// from a type whose `Serialize` can fail.
    pub fn chain_preimage(&self, link: &ChainLink) -> Result<String, serde_json::Error> {
        let mut body = serde_json::to_value(self)?;
        if let Some(object) = body.as_object_mut() {
            object.remove("chain");
        }
        Ok(preimage(link, &serde_json::to_string(&body)?))
    }

    /// This record's chain hash at `link`: the caller's `digest` over the preimage above.
    ///
    /// # Why the digest is an argument
    ///
    /// [ADR-0133](../../../docs/adr/0133-the-backbone-defines-what-is-hashed-not-how.md). This crate
    /// owns *what* is hashed and the shape of the answer; it does not own the hash function, because
    /// `sha2` in the backbone costs eight crates — nine on `aarch64`, where `cpufeatures` pulls
    /// `libc` — and one of them reads the host, which is the thing these three crates are checked
    /// for not doing.
    ///
    /// What the argument buys is that the **sequence** has one definition. Before this, three
    /// adapters each wrote *build the preimage, digest it, wrap it*, and a fourth wrote it again in
    /// a test; a difference between any two of them would have been a chain break nobody could
    /// account for. Now they each write the one line naming the digest they already link:
    ///
    /// ```ignore
    /// let hash = envelope.chain_hash(&link, |bytes| Sha256::digest(bytes).into())?;
    /// ```
    ///
    /// Nothing here stops a caller passing a digest that returns a constant. The contract suite is
    /// what catches that ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 5): a
    /// store whose hashes do not chain fails its obligations.
    ///
    /// # Errors
    ///
    /// As [`chain_preimage`](Self::chain_preimage) — the payload failed to serialize.
    pub fn chain_hash(
        &self,
        link: &ChainLink,
        digest: impl FnOnce(&[u8]) -> [u8; 32],
    ) -> Result<ChainHash, serde_json::Error> {
        Ok(ChainHash::of(digest(self.chain_preimage(link)?.as_bytes())))
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

#[cfg(test)]
mod chain_tests {
    use super::{EventEnvelope, EventTypeRef, RawPayload};
    use crate::chain::{ChainHash, ChainLink};
    use crate::events::EventType;
    use crate::ids::{BrandId, DeviceId, EmployeeId, EventId, ShiftId, StoreId, TenantId};
    use crate::time::{BusinessDate, Timestamp};
    use crate::ulid::Ulid;

    fn envelope() -> EventEnvelope<RawPayload> {
        EventEnvelope {
            event_id: EventId::new(Ulid::from_parts(1_767_225_600_000, 1)),
            event_type: EventTypeRef::from_known(EventType::BillingBillSettled),
            event_time: Timestamp::from_milliseconds_since_epoch(1_767_225_600_000)
                .expect("instant"),
            business_date: BusinessDate::from_ymd(2026, 8, 13).expect("date"),
            schema_version: 1,
            tenant_id: TenantId::new(Ulid::from_parts(1, 1)),
            brand_id: BrandId::new(Ulid::from_parts(1, 2)),
            store_id: StoreId::new(Ulid::from_parts(1, 3)),
            device_id: DeviceId::new(Ulid::from_parts(1, 4)),
            employee_id: Some(EmployeeId::new(Ulid::from_parts(1, 5))),
            shift_id: Some(ShiftId::new(Ulid::from_parts(1, 6))),
            chain: None,
            data: RawPayload::encode(&serde_json::json!({"amount_minor": 500_000}))
                .expect("payload"),
        }
    }

    /// `map_data` restates every field by hand, so a new field is dropped silently and
    /// nothing fails to compile. `MenuCatalog::localized` lost its modifier groups that
    /// exact way, and a decode step that dropped `chain` would leave every decoded event
    /// looking unchained — which reads as "written before the chain existed", not as
    /// tampering, so nothing would ever report it.
    #[test]
    fn map_data_keeps_every_field_including_the_chain() {
        let link = ChainLink::following(41, ChainHash::of([7; 32]));
        let original = EventEnvelope {
            chain: Some(link.clone()),
            ..envelope()
        };

        let mapped = original.clone().map_data(|payload| payload);

        assert_eq!(mapped.chain, Some(link), "the chain survived the map");
        // And everything else, compared as a whole rather than field by field — a
        // per-field list here would have the same blind spot as the function it guards.
        assert_eq!(mapped, original);
    }

    #[test]
    fn the_preimage_excludes_the_records_own_chain_field() {
        let link = ChainLink::first();
        let unstamped = envelope();
        let stamped = EventEnvelope {
            chain: Some(link.clone()),
            ..envelope()
        };

        assert_eq!(
            unstamped.chain_preimage(&link).expect("preimage"),
            stamped.chain_preimage(&link).expect("preimage"),
            "a record cannot contain its own hash, so stamping must not change its preimage"
        );
        assert!(
            !stamped
                .chain_preimage(&link)
                .expect("preimage")
                .contains("\"chain\""),
            "the chain field is cleared out of the body"
        );
    }

    #[test]
    fn editing_any_part_of_the_record_changes_its_preimage() {
        let link = ChainLink::first();
        let original = envelope().chain_preimage(&link).expect("preimage");

        let edited_payload = EventEnvelope {
            data: RawPayload::encode(&serde_json::json!({"amount_minor": 1})).expect("payload"),
            ..envelope()
        };
        assert_ne!(
            edited_payload.chain_preimage(&link).expect("preimage"),
            original,
            "an edited amount must change the preimage — this is the whole point"
        );

        let edited_context = EventEnvelope {
            employee_id: Some(EmployeeId::new(Ulid::from_parts(9, 9))),
            ..envelope()
        };
        assert_ne!(
            edited_context.chain_preimage(&link).expect("preimage"),
            original,
            "context is covered too: who rang the sale is part of the record"
        );
    }

    #[test]
    fn a_record_moved_to_another_position_gets_a_different_preimage() {
        let record = envelope();
        let at_one = record
            .chain_preimage(&ChainLink::first())
            .expect("preimage");
        let at_two = record
            .chain_preimage(&ChainLink::following(1, ChainHash::genesis()))
            .expect("preimage");
        assert_ne!(at_one, at_two, "or two records could swap places freely");
    }

    #[test]
    fn an_envelope_written_before_the_chain_existed_still_loads() {
        // Round-tripped through text rather than `serde_json::Value`, because the envelope
        // borrows on the way in (`BusinessDate` deserializes from a borrowed string) and
        // `from_value` cannot hand out borrows. That is a property of the existing type,
        // not of this change.
        let older = serde_json::to_string(&envelope()).expect("serialise");
        assert!(
            !older.contains("\"chain\""),
            "an unstamped envelope carries no chain field on the wire: {older}"
        );
        let loaded: EventEnvelope<RawPayload> =
            serde_json::from_str(&older).expect("an envelope without the field still loads");
        assert_eq!(loaded.chain, None, "and reads as unchained, not as broken");
    }

    #[test]
    fn a_stamped_envelope_round_trips_through_json() {
        let stamped = EventEnvelope {
            chain: Some(ChainLink::following(7, ChainHash::of([2; 32]))),
            ..envelope()
        };
        let json = serde_json::to_string(&stamped).expect("serialise");
        assert_eq!(
            serde_json::from_str::<EventEnvelope<RawPayload>>(&json).expect("deserialise"),
            stamped
        );
    }
}
