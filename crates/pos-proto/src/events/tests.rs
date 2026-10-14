// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The event payloads' tests: their names, their bytes, and what each reads back as.

use std::collections::BTreeSet;

use super::{EventType, SalesOrderOpened, TypedPayload};
use crate::enums::SalesChannel;
use crate::envelope::{EventEnvelope, EventPayload, EventTypeRef, RawPayload};
use crate::ids::{BrandId, DeviceId, EventId, OrderId, StoreId, TableId, TenantId};
use crate::pii::check_field_name;
use crate::time::{BusinessDate, Timestamp};
use crate::ulid::Ulid;
use crate::wire_enum::Open;

/// The thirty-eight types `docs/pos-spec.md` §18 declares.
///
/// Held here as a literal list so that dropping one is a test failure rather than a
/// silent protocol break. A published event name is a contract: it may be added to,
/// never renamed or removed.
const SPECIFIED: &[&str] = &[
    "sales.order.opened",
    "sales.order_line.added",
    "sales.order_line.updated",
    "sales.order_line.voided",
    "sales.order_line.fired",
    "sales.table.transferred",
    "sales.table.merged",
    "sales.qr_session.started",
    "sales.order.submitted_by_guest",
    "sales.order.confirmed_by_staff",
    "sales.order.rejected_by_staff",
    "kitchen.ticket.bumped",
    "kitchen.ticket.recalled",
    "billing.bill.split",
    "billing.bill.merged",
    "billing.bill.settled",
    "billing.bill.voided",
    "billing.discount.applied",
    "billing.comp.applied",
    "billing.payment.captured",
    "billing.tip.adjusted",
    "billing.refund.issued",
    "cash.shift.opened",
    "cash.shift.closed",
    "cash.drawer.opened",
    "cash.drawer.paid_in",
    "cash.drawer.paid_out",
    "inventory.item.sold_out",
    "inventory.item.restored",
    "inventory.stock.consumed",
    "inventory.stock.adjusted",
    "inventory.stock.counted",
    "promotion.voucher.redeemed",
    "delivery.shipment.created",
    "delivery.shipment.status_changed",
    "config.version.published",
    "device.activation.completed",
    "fleet.update.rolled_out",
];

fn sample_envelope<D>(event_type: EventType, data: D) -> EventEnvelope<D> {
    EventEnvelope {
        event_id: EventId::new(Ulid::from_parts(1_767_225_600_000, 1)),
        event_type: EventTypeRef::from_known(event_type),
        event_time: Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("instant"),
        business_date: BusinessDate::from_ymd(2026, 8, 13).expect("date"),
        schema_version: event_type.schema_version(),
        tenant_id: TenantId::new(Ulid::from_parts(1, 1)),
        brand_id: BrandId::new(Ulid::from_parts(1, 2)),
        store_id: StoreId::new(Ulid::from_parts(1, 3)),
        device_id: DeviceId::new(Ulid::from_parts(1, 4)),
        employee_id: None,
        shift_id: None,
        chain: None,
        data,
    }
}

#[test]
fn every_specified_event_type_is_present() {
    let declared: BTreeSet<&str> = EventType::ALL.iter().map(|t| t.as_str()).collect();
    for specified in SPECIFIED {
        assert!(
            declared.contains(specified),
            "{specified} is in pos-spec.md §18 but not in the catalogue — a \
             published event name may never be removed"
        );
    }
    assert_eq!(SPECIFIED.len(), 38, "the specification declares 38 types");
}

#[test]
fn the_catalogue_extends_the_specification_rather_than_replacing_it() {
    let specified: BTreeSet<&str> = SPECIFIED.iter().copied().collect();
    let extra: Vec<&str> = EventType::ALL
        .iter()
        .map(|t| t.as_str())
        .filter(|token| !specified.contains(token))
        .collect();
    // Each addition is justified in issue #13 and in this module's documentation.
    assert_eq!(
        extra.len(),
        EventType::ALL.len() - 38,
        "every type is either specified or a documented addition"
    );
    assert!(
        extra.contains(&"security.permission.overridden"),
        "the manager-override audit event is the most important addition"
    );
}

#[test]
fn every_token_is_unique() {
    let unique: BTreeSet<&str> = EventType::ALL.iter().map(|t| t.as_str()).collect();
    assert_eq!(
        unique.len(),
        EventType::ALL.len(),
        "two event types share a token"
    );
}

#[test]
fn every_token_follows_domain_resource_action() {
    for event_type in EventType::ALL {
        let token = event_type.as_str();
        let segments: Vec<&str> = token.split('.').collect();
        assert_eq!(segments.len(), 3, "{token} is not domain.resource.action");
        for segment in &segments {
            assert!(!segment.is_empty(), "{token} has an empty segment");
            assert!(
                segment
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()),
                "{token} is not snake_case throughout"
            );
        }
    }
}

#[test]
fn every_token_round_trips() {
    for event_type in EventType::ALL {
        assert_eq!(
            EventType::parse(event_type.as_str()),
            Some(*event_type),
            "{event_type} does not parse back to itself"
        );
    }
}

#[test]
fn no_payload_field_name_suggests_personal_data() {
    // The second layer of the personal-data barrier: the type system cannot tell a
    // translation key from a guest's name, so the field names are checked too.
    for event_type in EventType::ALL {
        for field in event_type.field_names() {
            assert_eq!(
                check_field_name(field),
                Ok(()),
                "{event_type} has field `{field}`, which suggests personal data — \
                 carry a subject_id instead"
            );
        }
    }
}

#[test]
fn every_payload_field_name_is_snake_case() {
    // Which is also what makes the wire name equal the Rust name equal the database
    // column name, so no mapping layer exists anywhere.
    for event_type in EventType::ALL {
        for field in event_type.field_names() {
            assert!(
                field
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()),
                "{event_type}.{field} is not snake_case"
            );
            assert_ne!(*field, "id", "{event_type} has a bare `id` field");
            assert!(
                !field.ends_with("_at"),
                "{event_type}.{field} ends in _at; timestamps end in _time"
            );
        }
    }
}

#[test]
fn every_payload_declares_a_positive_schema_version() {
    for event_type in EventType::ALL {
        assert!(
            event_type.schema_version() >= 1,
            "{event_type} has schema version 0, which collides with 'absent'"
        );
    }
}

#[test]
fn an_envelope_round_trips_through_json() {
    let payload = SalesOrderOpened {
        order_id: OrderId::new(Ulid::from_parts(1_767_225_600_000, 9)),
        channel: Open::from_known(SalesChannel::DineIn),
        table_id: Some(TableId::new(Ulid::from_parts(1, 12))),
        guest_count: Some(4),
    };
    let envelope = sample_envelope(EventType::SalesOrderOpened, payload.clone());
    let json = serde_json::to_string(&envelope).expect("serialise");

    // The documented envelope field names, verbatim.
    for field in [
        "event_id",
        "event_type",
        "event_time",
        "business_date",
        "schema_version",
        "tenant_id",
        "brand_id",
        "store_id",
        "device_id",
        "data",
    ] {
        assert!(json.contains(&format!("\"{field}\"")), "{field} is missing");
    }

    let back: EventEnvelope<SalesOrderOpened> = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back.data, payload);
    assert_eq!(back.event_type.as_str(), "sales.order.opened");
    assert_eq!(back.business_date.to_string(), "2026-08-13");
}

#[test]
fn business_date_is_carried_and_is_not_derived_from_the_event_time() {
    // `pos-spec.md` §14.1: a bill rung at 01:30 belongs to the previous evening, so
    // the business date needs the store's timezone and cut-off hour and cannot be
    // recovered from the instant alone. The archive's envelope omitted the field
    // entirely; the English set is right to carry it.
    //
    // The fixture deliberately sets a business date in August against an instant in
    // January. Nothing in the envelope reconciles them, and nothing should: the
    // device computed the business date at capture, and recomputing it downstream
    // would rewrite history the next time a store's cut-off was edited.
    let envelope = sample_envelope(EventType::SalesOrderOpened, ());
    let json = serde_json::to_string(&envelope).expect("serialise");

    assert!(
        json.contains(r#""business_date":"2026-08-13""#),
        "got {json}"
    );
    assert_eq!(
        envelope.event_time.to_string(),
        "2026-01-01T00:00:00Z",
        "the instant is unrelated to the business date"
    );
    assert!(
        json.contains(&format!(r#""event_time":"{}""#, envelope.event_time)),
        "got {json}"
    );
}

#[test]
fn an_absent_employee_is_omitted_rather_than_null() {
    // System-originated events — a published config version, a fleet rollout — have
    // no employee. The envelope's shape is still identical on every channel.
    let envelope = sample_envelope(EventType::ConfigVersionPublished, ());
    let json = serde_json::to_string(&envelope).expect("serialise");
    assert!(
        !json.contains("employee_id"),
        "expected omission, got {json}"
    );
    assert!(!json.contains("shift_id"));
}

#[test]
fn an_unrecognised_event_type_decodes_losslessly() {
    // The property that lets an older edge store, forward and checksum an event
    // from a newer cloud instead of dropping it.
    let json = r#"{
        "event_id": "01JQ0000000000000000000001",
        "event_type": "sales.order.teleported",
        "event_time": "2026-08-14T03:12:45.123Z",
        "business_date": "2026-08-13",
        "schema_version": 7,
        "tenant_id": "01JQ0000000000000000000002",
        "brand_id": "01JQ0000000000000000000003",
        "store_id": "01JQ0000000000000000000004",
        "device_id": "01JQ0000000000000000000005",
        "data": {"destination": "mars", "crew": 3}
    }"#;
    let received: EventEnvelope<RawPayload> =
        serde_json::from_str(json).expect("the envelope still parses");
    assert!(received.event_type.known().is_none());

    let typed = received
        .into_typed()
        .expect("an unknown type is not an error");
    // A let-else rather than a match: a wildcard arm over forty-nine variants
    // would also silently swallow any type added later, which is the whole
    // reason `clippy::wildcard_enum_match_arm` is denied.
    let TypedPayload::Unrecognised { event_type, data } = typed.data else {
        panic!("expected Unrecognised for an unknown event type");
    };
    assert_eq!(event_type, "sales.order.teleported");
    // Bytes intact, including the fields this build has never heard of.
    assert!(data.as_json().contains("\"destination\""));
    assert!(data.as_json().contains("\"crew\""));
}

#[test]
fn a_recognised_type_with_a_malformed_payload_is_an_error() {
    // Tolerance is for vocabulary, not for corruption. An event whose type we know
    // and whose payload we cannot read is a real fault worth surfacing.
    let json = r#"{
        "event_id": "01JQ0000000000000000000001",
        "event_type": "sales.order.opened",
        "event_time": "2026-08-14T03:12:45.123Z",
        "business_date": "2026-08-13",
        "schema_version": 1,
        "tenant_id": "01JQ0000000000000000000002",
        "brand_id": "01JQ0000000000000000000003",
        "store_id": "01JQ0000000000000000000004",
        "device_id": "01JQ0000000000000000000005",
        "data": {"order_id": "not-a-ulid"}
    }"#;
    let received: EventEnvelope<RawPayload> = serde_json::from_str(json).expect("parses");
    assert!(received.into_typed().is_err());
}

#[test]
fn an_added_payload_field_does_not_break_an_older_reader() {
    // This is what "protocol changes are additive" has to mean in practice: a cloud
    // adds a field to one event's payload, without a PROTOCOL_VERSION bump, and an
    // edge running last month's build still reads it.
    let json = r#"{
        "order_id": "01JQ0000000000000000000009",
        "channel": "SALES_CHANNEL_DINE_IN",
        "table_id": null,
        "guest_count": 4,
        "loyalty_tier_added_next_release": "GOLD"
    }"#;
    let payload: SalesOrderOpened =
        serde_json::from_str(json).expect("an unknown field must not break the read");
    assert_eq!(payload.guest_count, Some(4));
}

#[test]
fn a_line_recorded_before_modifier_names_existed_still_reads() {
    // ADR-0144. Every line an upgrading store replays at start-up predates the field, so it
    // must read, with no names: its receipt then prints no modifiers, and the box boots.
    use super::SalesOrderLineAdded;
    use crate::ids::{MenuItemId, OrderLineId, TaxClassId};
    use crate::money::{CurrencyCode, Money, Ratio};
    use crate::quantity::Quantity;
    use crate::text::DisplayName;

    let line = SalesOrderLineAdded {
        order_id: OrderId::new(Ulid::from_parts(1, 9)),
        order_line_id: OrderLineId::new(Ulid::from_parts(1, 10)),
        menu_item_id: MenuItemId::new(Ulid::from_parts(1, 11)),
        display_name: DisplayName::new("Margherita"),
        quantity: Quantity::ONE,
        unit_price: Money::new(CurrencyCode::VND, 190_000),
        line_total: Money::new(CurrencyCode::VND, 190_000),
        tax_class_id: TaxClassId::new(Ulid::from_parts(1, 12)),
        tax_rate: Ratio::basis_points(1_000).expect("a rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: vec![MenuItemId::new(Ulid::from_parts(1, 13))],
        modifier_display_names: vec![DisplayName::new("30 cm")],
        note_present: false,
    };
    let mut json = serde_json::to_value(&line).expect("serialise");
    let removed = json
        .as_object_mut()
        .and_then(|fields| fields.remove("modifier_display_names"));
    assert!(removed.is_some(), "a new line writes the names");

    // From text, as a replay reads it: the currency code deserialises borrowed.
    let text = serde_json::to_string(&json).expect("re-serialise");
    let older: SalesOrderLineAdded =
        serde_json::from_str(&text).expect("a line from before the field still reads");
    assert!(older.modifier_display_names.is_empty());
    assert_eq!(older.modifier_menu_item_ids, line.modifier_menu_item_ids);
}

#[test]
fn the_payload_trait_agrees_with_the_enumeration() {
    // The macro generates both from one declaration, so this asserts the macro
    // rather than the data — which is the point: they cannot drift.
    assert_eq!(SalesOrderOpened::EVENT_TYPE, EventType::SalesOrderOpened);
    assert_eq!(
        SalesOrderOpened::SCHEMA_VERSION,
        EventType::SalesOrderOpened.schema_version()
    );
    assert_eq!(
        SalesOrderOpened::FIELD_NAMES,
        EventType::SalesOrderOpened.field_names()
    );
}

fn vnd(amount: i64) -> crate::money::Money {
    crate::money::Money::new(crate::money::CurrencyCode::VND, amount)
}

/// A bill settled for 115,500: 100,000 of food and a 5,000 service charge, at 10 %.
fn settled(
    fee_lines: Vec<super::BillFeeLine>,
    tax_lines: Vec<super::BillTaxLine>,
) -> super::BillingBillSettled {
    super::BillingBillSettled {
        bill_id: crate::ids::BillId::new(Ulid::from_parts(1, 1)),
        receipt_number: 7,
        subtotal: vnd(100_000),
        reduction_total: vnd(0),
        service_charge: vnd(5_000),
        tax_total: vnd(10_500),
        rounding_adjustment: vnd(0),
        total_due: vnd(115_500),
        buyer_subject_id: None,
        fee_lines,
        tax_lines,
    }
}

fn opened(fee_rules: Vec<crate::fees::FrozenFee>) -> super::BillingBillOpened {
    super::BillingBillOpened {
        bill_id: crate::ids::BillId::new(Ulid::from_parts(1, 1)),
        order_id: OrderId::new(Ulid::from_parts(1, 2)),
        order_line_ids: Vec::new(),
        fee_rules,
    }
}

fn service_fee() -> crate::fees::FrozenFee {
    crate::fees::FrozenFee {
        fee_id: crate::ids::FeeId::new(Ulid::from_parts(1, 3)),
        code: crate::fees::FeeCode::new("SERVICE"),
        display_name: crate::text::DisplayName::new("Service charge"),
        kind: crate::fees::FeeKind::Percent.into(),
        rate: crate::money::Ratio::percent(5).ok(),
        amount: None,
        item_scope: Open::default(),
        menu_item_ids: Vec::new(),
        base_discounted: true,
        base_tax_inclusive: false,
        tax: Open::default(),
        tax_class_id: None,
        waivable: true,
    }
}

fn records() -> (Vec<super::BillFeeLine>, Vec<super::BillTaxLine>) {
    let fee = super::BillFeeLine {
        fee_id: crate::ids::FeeId::new(Ulid::from_parts(1, 3)),
        code: crate::fees::FeeCode::new("SERVICE"),
        display_name: crate::text::DisplayName::new("Service charge"),
        amount: vnd(5_000),
        tax: vnd(500),
        tax_components: Vec::new(),
    };
    let tax = super::BillTaxLine {
        tax_class_id: crate::ids::TaxClassId::new(Ulid::from_parts(1, 4)),
        taxable_base: vnd(105_000),
        rate_basis_points: 1_000,
        tax: vnd(10_500),
        components: Vec::new(),
    };
    (vec![fee], vec![tax])
}

/// Two components at 5 % each, splitting `cgst + sgst`.
fn halves(cgst: i64, sgst: i64) -> Vec<super::BillTaxComponent> {
    let part = |name: &str, tax: i64| super::BillTaxComponent {
        name: crate::locale::TaxComponentName::new(name).expect("a token"),
        rate_basis_points: 500,
        tax: vnd(tax),
    };
    vec![part("CGST", cgst), part("SGST", sgst)]
}

#[test]
fn a_bill_recorded_before_fees_existed_still_reads() {
    // ADR-0159: every bill an upgrading store replays predates the fields, so each reads as
    // empty — no rules, no fee lines, no tax lines recorded.
    let opened: super::BillingBillOpened = serde_json::from_str(
        r#"{"bill_id":"01JQ0000000000000000000001","order_id":"01JQ0000000000000000000002"}"#,
    )
    .expect("an opened bill without fee rules reads");
    assert!(opened.fee_rules.is_empty());

    let mut old = serde_json::to_value(settled(Vec::new(), Vec::new())).expect("serialise");
    if let Some(fields) = old.as_object_mut() {
        fields.remove("fee_lines");
        fields.remove("tax_lines");
    }
    let settled: super::BillingBillSettled =
        serde_json::from_str(&old.to_string()).expect("a settled bill without fee lines reads");
    assert!(settled.fee_lines.is_empty() && settled.tax_lines.is_empty());
}

#[test]
fn an_empty_record_leaves_the_event_as_it_was() {
    // Empty is left out, so a bill with no fee and no tax lines recorded writes exactly the
    // bytes it wrote before the fields existed.
    let opened = serde_json::to_string(&opened(Vec::new())).expect("serialise");
    assert!(!opened.contains("fee_rules"), "{opened}");
    let settled = serde_json::to_string(&settled(Vec::new(), Vec::new())).expect("serialise");
    assert!(
        !settled.contains("fee_lines") && !settled.contains("tax_lines"),
        "{settled}"
    );
}

#[test]
fn a_bill_without_components_writes_the_bytes_it_wrote_before_them() {
    // ADR-0168: every Vietnamese and Japanese bill. A settle with a fee line and a tax line
    // wrote exactly these bytes before either line had components, so its chain hash is the
    // same too.
    let (fee_lines, tax_lines) = records();
    let written = RawPayload::encode(&settled(fee_lines, tax_lines)).expect("encodes");
    assert_eq!(
        written.as_json(),
        concat!(
            r#"{"bill_id":"00000000010000000000000001","receipt_number":7,"#,
            r#""subtotal":{"currency_code":"VND","amount_minor":100000},"#,
            r#""reduction_total":{"currency_code":"VND","amount_minor":0},"#,
            r#""service_charge":{"currency_code":"VND","amount_minor":5000},"#,
            r#""tax_total":{"currency_code":"VND","amount_minor":10500},"#,
            r#""rounding_adjustment":{"currency_code":"VND","amount_minor":0},"#,
            r#""total_due":{"currency_code":"VND","amount_minor":115500},"#,
            r#""buyer_subject_id":null,"#,
            r#""fee_lines":[{"fee_id":"00000000010000000000000003","code":"SERVICE","#,
            r#""display_name":"Service charge","amount":{"currency_code":"VND","#,
            r#""amount_minor":5000},"tax":{"currency_code":"VND","amount_minor":500}}],"#,
            r#""tax_lines":[{"tax_class_id":"00000000010000000000000004","#,
            r#""taxable_base":{"currency_code":"VND","amount_minor":105000},"#,
            r#""rate_basis_points":1000,"tax":{"currency_code":"VND","#,
            r#""amount_minor":10500}}]}"#,
        )
    );
}

/// [`records`], split: the tax line's 10,500 and the fee's 500, each in two halves at 5 %.
fn split_bill() -> super::BillingBillSettled {
    let (mut fee_lines, mut tax_lines) = records();
    for line in &mut fee_lines {
        line.tax_components = halves(250, 250);
    }
    for line in &mut tax_lines {
        line.components = halves(5_250, 5_250);
    }
    settled(fee_lines, tax_lines)
}

/// `bill` as JSON, with the component name at `pointer` replaced by one that is not a token.
fn with_a_name_it_cannot_read(
    bill: &super::BillingBillSettled,
    pointer: &str,
) -> serde_json::Value {
    let mut json = serde_json::to_value(bill).expect("serialise");
    let name = json.pointer_mut(pointer).expect("a component's name");
    *name = serde_json::Value::from("Central GST");
    json
}

#[test]
fn a_bill_records_its_tax_components_and_reads_them_back() {
    // ADR-0168 decision 3: the class's tax and the fee's, each split under a token.
    let settled_bill = split_bill();
    let text = serde_json::to_string(&settled_bill).expect("serialise");
    assert!(
        text.contains(concat!(
            r#""tax_components":[{"name":"CGST","rate_basis_points":500,"#,
            r#""tax":{"currency_code":"VND","amount_minor":250}},"#,
        )),
        "{text}"
    );
    assert!(
        text.contains(concat!(
            r#""components":[{"name":"CGST","rate_basis_points":500,"#,
            r#""tax":{"currency_code":"VND","amount_minor":5250}},"#,
        )),
        "{text}"
    );
    let back: super::BillingBillSettled = serde_json::from_str(&text).expect("deserialise");
    assert_eq!(back, settled_bill);
}

#[test]
fn a_tax_line_with_a_name_it_cannot_read_is_not_split_and_keeps_its_tax() {
    // A later release may widen the token. An older reader still decodes the bill, as the
    // cloud's rollup does, and the line keeps its tax: its components read as none, never as
    // the half it could read.
    let bill = split_bill();
    let json = with_a_name_it_cannot_read(&bill, "/tax_lines/0/components/1/name");
    let read: super::BillingBillSettled = RawPayload::encode(&json)
        .expect("encodes")
        .decode()
        .expect("the bill still reads");
    let line = read.tax_lines.first().expect("its tax line");
    assert_eq!(line.tax, vnd(10_500));
    assert!(line.components.is_empty(), "{:?}", line.components);
    assert_eq!(
        read.fee_lines, bill.fee_lines,
        "the fee's own list still reads"
    );
    assert_eq!(read.tax_total, bill.tax_total);

    // Only the list is lenient: a component read on its own still refuses the name.
    let component = concat!(
        r#"{"name":"Central GST","rate_basis_points":500,"#,
        r#""tax":{"currency_code":"VND","amount_minor":5250}}"#,
    );
    assert!(serde_json::from_str::<super::BillTaxComponent>(component).is_err());
}

#[test]
fn a_fee_line_with_a_name_it_cannot_read_is_not_split_and_keeps_its_tax() {
    // As the tax line, read here from a value, which owns its strings.
    let bill = split_bill();
    let json = with_a_name_it_cannot_read(&bill, "/fee_lines/0/tax_components/0/name");
    let read: super::BillingBillSettled =
        serde_json::from_value(json).expect("the bill still reads");
    let fee = read.fee_lines.first().expect("its fee line");
    assert_eq!(fee.tax, vnd(500));
    assert!(fee.tax_components.is_empty(), "{:?}", fee.tax_components);
    assert_eq!(
        read.tax_lines, bill.tax_lines,
        "the tax line's own list still reads"
    );
}

#[test]
fn a_bill_records_its_rules_its_fees_and_its_tax() {
    let opened_bill = opened(vec![service_fee()]);
    let text = serde_json::to_string(&opened_bill).expect("serialise");
    let back: super::BillingBillOpened = serde_json::from_str(&text).expect("deserialise");
    assert_eq!(back, opened_bill);

    let (fee_lines, tax_lines) = records();
    let settled_bill = settled(fee_lines, tax_lines);
    let text = serde_json::to_string(&settled_bill).expect("serialise");
    let back: super::BillingBillSettled = serde_json::from_str(&text).expect("deserialise");
    assert_eq!(back, settled_bill);

    // The name net reaches inside the parts too, which the field list cannot see.
    let (fee_lines, tax_lines) = records();
    let parts = [
        serde_json::to_value(&fee_lines).expect("serialise"),
        serde_json::to_value(&tax_lines).expect("serialise"),
    ];
    for part in parts
        .iter()
        .flat_map(|list| list.as_array().cloned().unwrap_or_default())
    {
        for key in part
            .as_object()
            .map(|fields| fields.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
        {
            assert_eq!(check_field_name(&key), Ok(()), "{key}");
        }
    }
}

/// Writes `payload` as a store with one drawer does and `named` as a till with its own will,
/// and reads both back: the first is `before` exactly, and the till only adds its own field.
fn names_its_till_only_when_it_has_one<P>(payload: &P, named: &P, till: DeviceId, before: &str)
where
    P: serde::Serialize + serde::de::DeserializeOwned + PartialEq + core::fmt::Debug,
{
    let written = RawPayload::encode(payload).expect("encodes");
    assert_eq!(
        written.as_json(),
        before,
        "the store's drawer writes today's bytes"
    );
    assert_eq!(
        &written.decode::<P>().expect("today's bytes decode"),
        payload
    );

    let written = RawPayload::encode(named).expect("encodes");
    let object = before.strip_suffix('}').expect("a payload is an object");
    assert_eq!(
        written.as_json(),
        format!(r#"{object},"terminal_device_id":"{till}"}}"#)
    );
    assert_eq!(&written.decode::<P>().expect("decodes"), named);
}

#[test]
fn a_shift_event_that_names_no_till_writes_the_bytes_it_always_did() {
    // ADR-0167 decision 5. Each literal is what this crate wrote before the field existed,
    // captured from the release before it, so a store with one drawer keeps writing the same
    // log, the same chain and the same checksums.
    use super::{CashShiftClosed, CashShiftOpened};
    use crate::ids::ShiftId;
    use crate::money::{CurrencyCode, Money};

    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let shift = ShiftId::new(Ulid::from_u128(7));
    let till = DeviceId::new(Ulid::from_u128(0x7111));

    let opened = CashShiftOpened {
        opened_shift_id: shift,
        opening_float: vnd(500_000),
        terminal_device_id: None,
        assigned_employee_id: None,
    };
    names_its_till_only_when_it_has_one(
        &opened,
        &CashShiftOpened {
            terminal_device_id: Some(till),
            ..opened.clone()
        },
        till,
        r#"{"opened_shift_id":"00000000000000000000000007","opening_float":{"currency_code":"VND","amount_minor":500000}}"#,
    );

    let closed = CashShiftClosed {
        closed_shift_id: shift,
        expected_amount: vnd(1_650_000),
        counted_amount: vnd(1_600_000),
        variance: vnd(-50_000),
        terminal_device_id: None,
        reason_code_id: None,
    };
    names_its_till_only_when_it_has_one(
        &closed,
        &CashShiftClosed {
            terminal_device_id: Some(till),
            ..closed.clone()
        },
        till,
        r#"{"closed_shift_id":"00000000000000000000000007","expected_amount":{"currency_code":"VND","amount_minor":1650000},"counted_amount":{"currency_code":"VND","amount_minor":1600000},"variance":{"currency_code":"VND","amount_minor":-50000}}"#,
    );
}

#[test]
fn a_drawer_event_that_names_no_till_writes_the_bytes_it_always_did() {
    // As the shift's events above: each literal is what this crate wrote before the field.
    use super::{CashDrawerOpened, CashDrawerPaidIn, CashDrawerPaidOut};
    use crate::ids::ReasonCodeId;
    use crate::money::{CurrencyCode, Money};

    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let reason = ReasonCodeId::new(Ulid::from_u128(9));
    let till = DeviceId::new(Ulid::from_u128(0x7111));

    for (opening, before) in [
        (
            CashDrawerOpened {
                standalone: true,
                reason_code_id: Some(reason),
                terminal_device_id: None,
            },
            r#"{"standalone":true,"reason_code_id":"00000000000000000000000009"}"#,
        ),
        (
            CashDrawerOpened {
                standalone: false,
                reason_code_id: None,
                terminal_device_id: None,
            },
            r#"{"standalone":false,"reason_code_id":null}"#,
        ),
    ] {
        names_its_till_only_when_it_has_one(
            &opening,
            &CashDrawerOpened {
                terminal_device_id: Some(till),
                ..opening.clone()
            },
            till,
            before,
        );
    }

    let paid_in = CashDrawerPaidIn {
        amount: vnd(200_000),
        reason_code_id: reason,
        terminal_device_id: None,
    };
    names_its_till_only_when_it_has_one(
        &paid_in,
        &CashDrawerPaidIn {
            terminal_device_id: Some(till),
            ..paid_in.clone()
        },
        till,
        r#"{"amount":{"currency_code":"VND","amount_minor":200000},"reason_code_id":"00000000000000000000000009"}"#,
    );

    let paid_out = CashDrawerPaidOut {
        amount: vnd(50_000),
        reason_code_id: reason,
        terminal_device_id: None,
    };
    names_its_till_only_when_it_has_one(
        &paid_out,
        &CashDrawerPaidOut {
            terminal_device_id: Some(till),
            ..paid_out.clone()
        },
        till,
        r#"{"amount":{"currency_code":"VND","amount_minor":50000},"reason_code_id":"00000000000000000000000009"}"#,
    );
}

#[test]
fn a_close_names_its_reason_only_where_its_variance_owed_one() {
    // ADR-0167 decision 12: a close that owes no reason writes the bytes the test above holds,
    // and one that does adds its reason after every field before it, the till's included.
    use super::CashShiftClosed;
    use crate::ids::{ReasonCodeId, ShiftId};
    use crate::money::{CurrencyCode, Money};

    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let reason = ReasonCodeId::new(Ulid::from_u128(9));
    let till = DeviceId::new(Ulid::from_u128(0x7111));
    let explained = CashShiftClosed {
        closed_shift_id: ShiftId::new(Ulid::from_u128(7)),
        expected_amount: vnd(1_650_000),
        counted_amount: vnd(1_600_000),
        variance: vnd(-50_000),
        terminal_device_id: None,
        reason_code_id: Some(reason),
    };
    let figures = r#""closed_shift_id":"00000000000000000000000007","expected_amount":{"currency_code":"VND","amount_minor":1650000},"counted_amount":{"currency_code":"VND","amount_minor":1600000},"variance":{"currency_code":"VND","amount_minor":-50000}"#;
    let at_a_till = CashShiftClosed {
        terminal_device_id: Some(till),
        ..explained.clone()
    };
    for (closed, written) in [
        (
            &explained,
            format!(r#"{{{figures},"reason_code_id":"{reason}"}}"#),
        ),
        (
            &at_a_till,
            format!(r#"{{{figures},"terminal_device_id":"{till}","reason_code_id":"{reason}"}}"#),
        ),
    ] {
        let encoded = RawPayload::encode(closed).expect("encodes");
        assert_eq!(encoded.as_json(), written);
        assert_eq!(
            &encoded.decode::<CashShiftClosed>().expect("decodes"),
            closed
        );
    }
}

#[test]
fn a_shift_opened_names_its_person_only_where_its_drawer_is_assigned() {
    // ADR-0167 decision 9: an id, never a name, and nothing at all where drawers are shared, so
    // the bytes above stay what a store sharing its drawers writes.
    use super::CashShiftOpened;
    use crate::ids::{EmployeeId, ShiftId};
    use crate::money::{CurrencyCode, Money};

    let person = EmployeeId::new(Ulid::from_u128(0xE1));
    let assigned = CashShiftOpened {
        opened_shift_id: ShiftId::new(Ulid::from_u128(7)),
        opening_float: Money::new(CurrencyCode::VND, 500_000),
        terminal_device_id: None,
        assigned_employee_id: Some(person),
    };
    let written = RawPayload::encode(&assigned).expect("encodes");
    assert_eq!(
        written.as_json(),
        format!(
            r#"{{"opened_shift_id":"00000000000000000000000007","opening_float":{{"currency_code":"VND","amount_minor":500000}},"assigned_employee_id":"{person}"}}"#
        )
    );
    assert_eq!(
        written.decode::<CashShiftOpened>().expect("decodes"),
        assigned
    );
    assert_eq!(check_field_name("assigned_employee_id"), Ok(()));
}
