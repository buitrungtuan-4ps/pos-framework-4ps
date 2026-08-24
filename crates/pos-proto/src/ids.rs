// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One newtype per resource identifier.
//!
//! Every one of these wraps a [`Ulid`], so they are all structurally identical — and
//! that is exactly why they are separate types. A `StoreId` where a `TenantId`
//! belongs is the kind of mistake that produces a query returning somebody else's
//! data, and in a multi-tenant system that is the worst failure available. Making
//! each one distinct turns it into a compile error.
//!
//! Naming follows `docs/naming-and-api.md` §2: the primary key carries the full
//! resource name (`order_id`, never a bare `id`), and a foreign key keeps the
//! referenced key's name, so `order_lines.order_id` and `orders.order_id` are the
//! same word in the API, the database, the events and the logs. There is no mapping
//! layer because there is nothing to map.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ulid::{Ulid, UlidError};

/// Declares a resource identifier newtype over [`Ulid`].
macro_rules! resource_id {
    (
        $(#[$meta:meta])*
        $name:ident
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(Ulid);

        impl $name {
            /// Wraps an identifier.
            #[must_use]
            pub const fn new(value: Ulid) -> Self {
                Self(value)
            }

            /// The underlying identifier.
            #[must_use]
            pub const fn as_ulid(self) -> Ulid {
                self.0
            }
        }

        impl From<Ulid> for $name {
            fn from(value: Ulid) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl FromStr for $name {
            type Err = UlidError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                text.parse().map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Ulid::deserialize(deserializer).map(Self)
            }
        }
    };
}

resource_id! {
    /// A customer of the platform. The top of the configuration tree.
    TenantId
}
resource_id! {
    /// A brand within a tenant. Menus and pricing usually live here.
    BrandId
}
resource_id! {
    /// A single trading location.
    StoreId
}
resource_id! {
    /// A paired device: a terminal, tablet, phone, kitchen display, printer, card
    /// terminal, or customer-facing display.
    DeviceId
}
resource_id! {
    /// A member of staff. Employees belong to a tenant; roles are granted per store,
    /// so working at two stores means two grants against one `EmployeeId`.
    EmployeeId
}
resource_id! {
    /// A cash shift.
    ShiftId
}
resource_id! {
    /// An order.
    OrderId
}
resource_id! {
    /// A line on an order.
    OrderLineId
}
resource_id! {
    /// A bill. Distinct from an order: one order may be split across several bills,
    /// and several orders may be merged into one.
    BillId
}
resource_id! {
    /// One payment against a bill. Several may combine to settle a single bill.
    PaymentId
}
resource_id! {
    /// A table on a floor plan.
    TableId
}
resource_id! {
    /// An item on a menu.
    MenuItemId
}
resource_id! {
    /// An ingredient held in stock.
    IngredientId
}
resource_id! {
    /// One entry in the stock ledger.
    StockLedgerEntryId
}
resource_id! {
    /// A pricing or promotion campaign.
    CampaignId
}
resource_id! {
    /// A single event. Doubles as the receiver's idempotency key, which is why a
    /// retry must reuse it rather than mint a fresh one.
    EventId
}
resource_id! {
    /// A published configuration version.
    ConfigVersionId
}
resource_id! {
    /// One guest's session with a table's QR code. Time-limited and bound to a signed
    /// `table_id`, because a printed code can be photographed and used from outside.
    QrSessionId
}
resource_id! {
    /// A tax class, such as food, drink or alcohol.
    ///
    /// The rate is resolved from the store's locale pack keyed by this and by sales
    /// channel, so the same item can be taxed differently takeaway and dine-in.
    TaxClassId
}
resource_id! {
    /// A course: starter, main, dessert. Configured per brand, so it is data rather
    /// than an enumeration.
    CourseId
}
resource_id! {
    /// A kitchen station a line routes to.
    StationId
}
resource_id! {
    /// A reason drawn from the cloud-managed list that a void, discount, comp, refund
    /// or out-of-sale drawer opening must cite.
    ReasonCodeId
}
resource_id! {
    /// A voucher instance, whose redemption is an atomic check-and-mark.
    VoucherId
}
resource_id! {
    /// One delivery job with a courier.
    ShipmentId
}
resource_id! {
    /// A reference to a record in the separate personal-data store.
    ///
    /// This is the one identifier that exists to keep something **out** of the event
    /// log. The log is immutable, so personal data never goes inside it
    /// (`docs/pos-spec.md` §15); events carry only this reference, and anonymising a
    /// person is deleting one row rather than rewriting history and every backup.
    SubjectId
}

#[cfg(test)]
mod tests {
    use super::{OrderId, StoreId, TenantId};
    use crate::ulid::Ulid;

    #[test]
    fn an_identifier_round_trips_through_its_text_form() {
        let order = OrderId::new(Ulid::from_parts(1_767_225_600_000, 42));
        let text = order.to_string();
        assert_eq!(text.parse::<OrderId>().expect("round trip"), order);
    }

    #[test]
    fn identifiers_serialise_as_bare_ulid_strings() {
        // No wrapper object: `{"store_id": "01J..."}`, not
        // `{"store_id": {"ulid": "01J..."}}`.
        let store = StoreId::new(Ulid::from_parts(1_767_225_600_000, 7));
        let json = serde_json::to_string(&store).expect("serialise");
        assert_eq!(json, format!("\"{store}\""));
        assert_eq!(
            serde_json::from_str::<StoreId>(&json).expect("deserialise"),
            store
        );
    }

    #[test]
    fn different_resources_are_different_types() {
        // Both wrap a Ulid and both serialise identically, yet neither can be passed
        // where the other is expected. Uncommenting this must fail to compile:
        //   let _: TenantId = StoreId::new(Ulid::NIL);
        let tenant = TenantId::new(Ulid::NIL);
        let store = StoreId::new(Ulid::NIL);
        assert_eq!(tenant.to_string(), store.to_string());
    }

    #[test]
    fn ordering_follows_the_underlying_ulid() {
        // Which means ordering is chronological, so a feed can page by identifier.
        let earlier = OrderId::new(Ulid::from_parts(1_000, u128::MAX));
        let later = OrderId::new(Ulid::from_parts(1_001, 0));
        assert!(earlier < later);
    }

    #[test]
    fn a_malformed_identifier_is_rejected() {
        assert!("not-a-ulid".parse::<OrderId>().is_err());
        // The four excluded Crockford letters are rejected here too.
        assert!("I0000000000000000000000000".parse::<OrderId>().is_err());
    }
}
