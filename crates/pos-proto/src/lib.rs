// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Wire types shared by `pos_edge` and `pos_cloud`.
//!
//! This crate owns everything that crosses a boundary: the event envelope and
//! catalogue, money and quantity value types, identifiers, wire enums, the error
//! envelope, and [`PROTOCOL_VERSION`]. It also owns the two determinism traits
//! (`ClockSource`, `IdGenerator`), because they are total functions over this
//! crate's own value types and involve no I/O — see `docs/adr/0013-async-strategy.md`
//! for why they do not live in `pos-ports`.
//!
//! # What this crate must never contain
//!
//! No I/O, no runtime, no procedural macros, and nothing outside
//! `tools/backbone-allowlist.toml`. Names published here are contracts: they may be
//! added to, never renamed or removed (`docs/naming-and-api.md` §1).
//!
//! Changing anything in this crate requires an ADR merged first (`AGENTS.md` §7),
//! and a change to the envelope requires considering [`PROTOCOL_VERSION`].

// The backbone escalates the workspace-level `deny` to `forbid`: unlike an
// adapter, no part of this crate has a legitimate reason to use `unsafe`, and
// `forbid` cannot be lifted by an `expect` attribute the way `deny` can.
#![forbid(unsafe_code)]
// Crate-level lints do not reach doctests, so `docs/engineering-guide.md`'s rule
// that doc examples compile under the same rules has to be stated separately.
#![doc(test(attr(deny(warnings))))]

pub mod campaign;
pub mod channels;
pub mod determinism;
pub mod display;
pub mod enums;
pub mod envelope;
pub mod error;
pub mod events;
pub mod floor;
pub mod ids;
pub mod inventory;
pub mod locale;
pub mod menu;
pub mod money;
pub mod pii;
pub mod protocol;
pub mod quantity;
pub mod snapshot;
pub mod text;
pub mod time;
pub mod ulid;
pub mod wire_enum;

pub use determinism::{ClockSource, IdGenerator};
pub use display::{
    ChannelLayout, DisplayButton, DisplayCategory, DisplayPlan, DisplaySubcategory, GridPosition,
    LayoutBook,
};
pub use enums::{
    BillState, OrderLineState, OrderState, PaymentMethod, PaymentOutcome, ReductionKind,
    SalesChannel, ShiftState, ShipmentStatus, StockLedgerEntryKind, TableState, UnitOfMeasure,
};
pub use envelope::{DecodeError, EventEnvelope, EventPayload, EventTypeRef, RawPayload};
pub use error::{ErrorBody, ErrorDetail, ErrorResponse, ErrorStatus};
pub use events::{EventType, TypedPayload};
pub use floor::{FloorArea, FloorPlan, FloorTable, KitchenStation, RoutingRule, StationPlan};
pub use ids::{
    AreaId, BillId, BrandId, CampaignId, ConfigVersionId, CourseId, DeviceId, DisplayCategoryId,
    DisplaySubcategoryId, EmployeeId, EventId, IngredientId, MenuItemId, OrderId, OrderLineId,
    PaymentId, QrSessionId, ReasonCodeId, ShiftId, ShipmentId, StationId, StockLedgerEntryId,
    StoreId, SubjectId, SupplierId, TableId, TaxClassId, TenantId, VoucherId,
};
pub use locale::{
    CountryCode, CountryCodeError, LocalePack, NumberFormat, TaxRate, TaxRateRow, TaxRateTable,
};
pub use menu::{ChannelCatalog, MenuBook, MenuCatalog, MenuEntry};
pub use money::{CurrencyCode, Money, MoneyError, Ratio, Rounding, div_round};
pub use pii::NoPii;
pub use protocol::{Hello, HelloOutcome, LeaseToken, MIN_SUPPORTED_PROTOCOL_VERSION, negotiate};
pub use quantity::Quantity;
pub use text::{DisplayName, GuestNote, PermissionKey, ReleaseTag, TranslationKey};
pub use time::{BusinessDate, CalendarDate, TimeError, Timestamp};
pub use ulid::Ulid;
pub use wire_enum::{Open, UnknownEnumValue, WireEnum};

/// The cloud↔edge wire contract version.
///
/// A separate axis from the product's SemVer, and from the per-event
/// `schema_version` on the envelope: this number describes the *language* the two
/// tiers speak. The cloud must understand at least `PROTOCOL_VERSION` and
/// `PROTOCOL_VERSION - 1`, because edges update in rings and may be offline for
/// days — see `docs/adr/0024-protocol-version-negotiation.md`.
///
/// Protocol changes are additive. Incrementing this is a breaking change and
/// requires both versions to run in parallel for at least two releases.
pub const PROTOCOL_VERSION: u32 = 1;

// Zero is reserved: it would collide with the `*_UNSPECIFIED` convention that lets
// a receiver treat an unknown wire value as "absent" rather than failing. Checked
// at compile time, because a test asserting a property of a constant proves only
// that the constant was compiled.
const _: () = assert!(PROTOCOL_VERSION >= 1);
