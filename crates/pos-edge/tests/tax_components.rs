// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A settle records its tax components, and a copy prints the ones it recorded
//! ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3,
//! [ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)
//! decision 5).
//!
//! Vietnam and Japan publish no components, so for them nothing changes, byte for byte: the settle
//! event and the chain hash over it, and the pre-bill, the receipt and its copy as the printer
//! receives them. Those bytes are held against the ones main wrote and printed, captured from its
//! code before this change.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use pos_core::billing::Payment;
use pos_core::decision::Actor;
use pos_edge::app::PreBill;
use pos_edge::printing::{PrintOutcome, Printers, TillPrinting, TransportFactory};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, LineDraft, StoreIdentity};
use pos_fakes::FakeStore;
use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::chain::ChainLink;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::{BillTaxComponent, BillingBillSettled, EventType};
use pos_proto::fees::PublishedFees;
use pos_proto::ids::{
    BillId, BrandId, DeviceId, EmployeeId, EventId, MenuItemId, StoreId, TableId, TaxClassId,
    TenantId,
};
use pos_proto::locale::{NumberFormat, TaxComponent, TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::{Open, PaymentMethod, SalesChannel};
use printer_escpos::{Transport, TransportStatus, Unreachable};
use serde_json::json;
use sha2::{Digest, Sha256};

fn server() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(1))
}

fn table() -> TableId {
    TableId::new(Ulid::from_u128(41))
}

fn standard() -> TaxClassId {
    EdgeSession::standard_tax_class()
}

/// A second class, for the drinks Vietnam taxes at its reduced rate.
fn reduced() -> TaxClassId {
    TaxClassId::new(Ulid::from_u128(0x2008))
}

fn pizza() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn tea() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(501))
}

/// The instant a copy is printed at, fixed so the copy's heading is the same on every run.
fn copy_time() -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("an instant")
}

/// The store's receipt printer: no station, so a guest's paper goes to it.
fn counter_printer() -> PublishedDevice {
    PublishedDevice {
        device_id: DeviceId::new(Ulid::from_u128(0x9100)),
        kind: DeviceKind::Printer.into(),
        connection: DeviceConnection::Network.into(),
        address: "192.0.2.10:9100".to_owned(),
        name: DisplayName::new("Counter"),
        station_id: None,
        agent_device_id: None,
        drawer_attached: false,
        paper_width: Open::default(),
        cuts_paper: None,
        receipt_printer_id: None,
        receipt_language: Open::default(),
        receipt_second_language: Open::default(),
        opening_float_minor: None,
    }
}

/// A service charge of `percent`, taxed the way its lines are, parsed as the edge parses a
/// published `fees` node.
fn service(percent: i64) -> PublishedFees {
    let rule = json!({
        "fee_id": Ulid::from_u128(0xFEE1).to_string(),
        "code": "SERVICE",
        "display_name": "Service charge",
        "kind": "FEE_KIND_PERCENT",
        "rate": { "numerator": percent, "denominator": 100 },
    });
    serde_json::from_str(&json!({ "fees": [rule] }).to_string()).expect("a fees node")
}

/// A line as the till drafts one: one of `item`, at `price`, in `class` at `basis_points`.
fn draft(
    item: MenuItemId,
    name: &str,
    price: Money,
    class: TaxClassId,
    basis_points: i64,
) -> LineDraft {
    LineDraft {
        menu_item_id: item,
        display_name: DisplayName::new(name),
        quantity: Quantity::ONE,
        unit_price: price,
        line_total: price,
        tax_class_id: class,
        tax_rate: Ratio::basis_points(basis_points).expect("a valid rate"),
        seat: None,
        course_id: None,
        modifier_menu_item_ids: Vec::new(),
        note_present: false,
    }
}

/// A Vietnamese store as its country pack sets one up: đồng, `1.234.567`, totals rounded to the
/// 1,000 đồng note, prices quoted before tax, food at 10 % and drinks at 8 %, and a 5 % service
/// charge. The names are plain ASCII so the paper prints without a rasteriser.
fn vietnam() -> (EdgeSession, Vec<LineDraft>) {
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let menu = MenuCatalog::new()
        .with(MenuEntry::new(
            pizza(),
            DisplayName::new("Margherita"),
            vnd(185_000),
            standard(),
        ))
        .with(MenuEntry::new(
            tea(),
            DisplayName::new("Peach tea"),
            vnd(45_000),
            reduced(),
        ));
    let rates = TaxRateTable::new()
        .with(standard(), SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(reduced(), SalesChannel::DineIn, TaxRate::from_percent(8));
    let session = EdgeSession {
        number_format: NumberFormat {
            decimal_separator: ',',
            group_separator: '.',
            digits_per_group: 3,
        },
        cash_rounding_increment: Some(1_000),
        devices: PublishedDevices::new(vec![counter_printer()]),
        fees: service(5),
        ..EdgeSession::bootstrap()
            .with_menu(menu)
            .with_tax_rates(rates)
    };
    let lines = vec![
        draft(pizza(), "Margherita", vnd(185_000), standard(), 1_000),
        draft(tea(), "Peach tea", vnd(45_000), reduced(), 800),
    ];
    (session, lines)
}

/// A Japanese store: yen, prices quoting their tax (税込), dine-in at 10 % and takeaway at 8 %, and
/// a 10 % service charge.
fn japan() -> (EdgeSession, Vec<LineDraft>) {
    let jpy = |minor| Money::new(CurrencyCode::JPY, minor);
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Margherita"),
        jpy(1_980),
        standard(),
    ));
    let rates = TaxRateTable::new()
        .with(standard(), SalesChannel::DineIn, TaxRate::from_percent(10))
        .with(standard(), SalesChannel::Takeaway, TaxRate::from_percent(8));
    let session = EdgeSession {
        currency: CurrencyCode::JPY,
        prices_include_tax: true,
        devices: PublishedDevices::new(vec![counter_printer()]),
        fees: service(10),
        ..EdgeSession::bootstrap()
            .with_menu(menu)
            .with_tax_rates(rates)
    };
    let lines = vec![
        draft(pizza(), "Margherita", jpy(1_980), standard(), 1_000),
        draft(pizza(), "Margherita", jpy(1_980), standard(), 1_000),
    ];
    (session, lines)
}

/// A transport that keeps what the receipt printer was sent, in order.
#[derive(Debug, Default)]
struct Paper(Mutex<Vec<Vec<u8>>>);

#[derive(Debug)]
struct PaperRoll(Arc<Paper>);

impl Transport for PaperRoll {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        self.0
            .0
            .lock()
            .map_err(|_| Unreachable)?
            .push(bytes.to_vec());
        Ok(())
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        Ok(TransportStatus::default())
    }
}

#[derive(Debug)]
struct PaperRolls(Arc<Paper>);

impl TransportFactory for PaperRolls {
    fn open(&self, _device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
        Ok(Box::new(PaperRoll(Arc::clone(&self.0))))
    }
}

/// What a store wrote and printed for one bill.
struct Sale {
    /// The bill, whose id the log carries.
    bill_id: BillId,
    /// Every event the store wrote, as the log holds it.
    log: Vec<EventEnvelope<RawPayload>>,
    /// The pre-bill, the receipt and its first copy, as the receipt printer received them.
    paper: Vec<Vec<u8>>,
    /// The edge, for what a test does next.
    edge: Edge<FakeStore>,
    /// The store's printers, and what they have been sent so far.
    printers: Printers,
    printed: Arc<Paper>,
}

impl Sale {
    /// The settle event, as the log holds it.
    fn settled(&self) -> BillingBillSettled {
        self.log
            .iter()
            .find(|envelope| envelope.event_type.known() == Some(EventType::BillingBillSettled))
            .expect("a settle in the log")
            .data
            .decode()
            .expect("the settle decodes")
    }

    /// Prints a further copy of the receipt, and what the printer received for it, as text.
    async fn copy_printed(&self) -> String {
        let copy = self
            .edge
            .reprint_receipt(server(), self.bill_id)
            .await
            .expect("a copy");
        let session = self.edge.session();
        let printed = self
            .printers
            .print_receipt_copy(&session, &TillPrinting::STORE, store_id(), &copy, None)
            .await;
        assert_eq!(printed, PrintOutcome::Printed, "the copy");
        let paper = self.printed.0.lock().expect("the paper");
        String::from_utf8_lossy(paper.last().expect("a copy printed")).into_owned()
    }
}

fn job(n: u128) -> EventId {
    EventId::new(Ulid::from_u128(0xB0B0 + n))
}

/// Sells `lines` at one table of a store running `session`, settles the bill in cash, and prints
/// its pre-bill, its receipt and a copy at the store's receipt printer.
async fn sell(session: EdgeSession, lines: Vec<LineDraft>) -> Sale {
    let store = FakeStore::default();
    let paper = Arc::new(Paper::default());
    let printers = Printers::over(Arc::new(PaperRolls(Arc::clone(&paper))));
    let edge = Edge::new(
        store.clone(),
        StoreIdentity::for_store(store_id()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    edge.seat_table(server(), table(), None)
        .await
        .expect("seats");
    for line in lines {
        edge.add_line(server(), table(), line).await.expect("rings");
    }
    let bill_id = edge
        .open_bill(server(), table())
        .await
        .expect("opens")
        .bill_id;
    let session = edge.session();
    let pre_bill: PreBill = edge.pre_bill_for_bill(bill_id).expect("a pre-bill");
    let printed = printers
        .print_pre_bill(
            &session,
            &TillPrinting::STORE,
            store_id(),
            job(1),
            "T41",
            &pre_bill,
        )
        .await;
    assert_eq!(printed, PrintOutcome::Printed, "the pre-bill");
    let due = pre_bill.totals.total_due;
    let settled = edge
        .settle_bill(
            server(),
            bill_id,
            vec![Payment {
                method: PaymentMethod::Cash,
                tendered: due,
                applied_to_bill: due,
                tip: Money::zero(due.currency_code),
            }],
            None,
        )
        .await
        .expect("settles");
    let printed = printers
        .print_receipt(
            &session,
            &TillPrinting::STORE,
            store_id(),
            job(2),
            settled.receipt_number.expect("a number"),
            &settled.lines,
            settled.totals.as_ref().expect("the totals"),
            None,
        )
        .await;
    assert_eq!(printed, PrintOutcome::Printed, "the receipt");
    let mut copy = edge
        .reprint_receipt(server(), bill_id)
        .await
        .expect("a copy");
    copy.reprinted_time = copy_time();
    let printed = printers
        .print_receipt_copy(&session, &TillPrinting::STORE, store_id(), &copy, None)
        .await;
    assert_eq!(printed, PrintOutcome::Printed, "the copy");

    let query = EventQuery::first(store_id(), NonZeroU32::new(200).expect("a positive limit"));
    let log = store.read(&query).await.expect("read the log");
    let printed = Arc::clone(&paper);
    let paper = paper.0.lock().expect("the paper").clone();
    Sale {
        bill_id,
        log,
        paper,
        edge,
        printers,
        printed,
    }
}

/// The bill id every run's settle event is written with here, in place of the one the edge minted.
const BILL: &str = "01JQ00000000000000000000B1";

/// The settle's payload, with the bill's id fixed, and the hash a store's chain gives it as its
/// first record: what the event writes, minted ids aside.
fn settle_event(sale: &Sale) -> (String, String) {
    let settled = sale
        .log
        .iter()
        .find(|envelope| envelope.event_type.known() == Some(EventType::BillingBillSettled))
        .expect("a settle in the log");
    let payload = settled
        .data
        .as_json()
        .replace(&sale.bill_id.to_string(), BILL);
    let envelope = EventEnvelope {
        event_id: EventId::new(Ulid::from_parts(1_767_225_600_000, 1)),
        event_type: EventTypeRef::from_known(EventType::BillingBillSettled),
        event_time: copy_time(),
        business_date: BusinessDate::from_ymd(2026, 1, 1).expect("a date"),
        schema_version: 1,
        tenant_id: TenantId::new(Ulid::from_u128(2)),
        brand_id: BrandId::new(Ulid::from_u128(3)),
        store_id: store_id(),
        device_id: server().device_id,
        employee_id: Some(server().employee_id),
        shift_id: None,
        chain: None,
        data: serde_json::from_str::<RawPayload>(&payload).expect("the payload reads back"),
    };
    let hash = envelope
        .chain_hash(&ChainLink::first(), |bytes| Sha256::digest(bytes).into())
        .expect("hashes");
    (payload, hash.as_str().to_owned())
}

/// A printed document's SHA-256, in hex: what is compared, so the test does not carry kilobytes of
/// printer commands.
fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("a String takes any text");
            hex
        })
}

/// Vietnam's and Japan's settle event, its chain hash, and their pre-bill, receipt and copy, as
/// main wrote and printed them before tax components were recorded.
///
/// Captured from main's code (bcdb297) before this change, by running [`sell`] there. They are the
/// standard the change is held to, so they are never regenerated from a later build: a difference
/// is a change to Vietnamese or Japanese bytes, which ADR-0168 rules out.
struct Before {
    settled: &'static str,
    chain_hash: &'static str,
    paper: [&'static str; 3],
}

const VIETNAM: Before = Before {
    settled: concat!(
        r#"{"bill_id":"01JQ00000000000000000000B1","receipt_number":1,"#,
        r#""subtotal":{"currency_code":"VND","amount_minor":230000},"#,
        r#""reduction_total":{"currency_code":"VND","amount_minor":0},"#,
        r#""service_charge":{"currency_code":"VND","amount_minor":11500},"#,
        r#""tax_total":{"currency_code":"VND","amount_minor":23205},"#,
        r#""rounding_adjustment":{"currency_code":"VND","amount_minor":295},"#,
        r#""total_due":{"currency_code":"VND","amount_minor":265000},"buyer_subject_id":null,"#,
        r#""fee_lines":[{"fee_id":"00000000000000000000001ZQ1","code":"SERVICE","#,
        r#""display_name":"Service charge","amount":{"currency_code":"VND","#,
        r#""amount_minor":11500},"tax":{"currency_code":"VND","amount_minor":1105}}],"#,
        r#""tax_lines":[{"tax_class_id":"00000000000000000000000001","#,
        r#""taxable_base":{"currency_code":"VND","amount_minor":194250},"#,
        r#""rate_basis_points":1000,"tax":{"currency_code":"VND","amount_minor":19425}},"#,
        r#"{"tax_class_id":"00000000000000000000000808","taxable_base":{"currency_code":"VND","#,
        r#""amount_minor":47250},"rate_basis_points":800,"tax":{"currency_code":"VND","#,
        r#""amount_minor":3780}}]}"#,
    ),
    chain_hash: "dd4481e5d93aa7c8f22b2b8a0076293f080f064d25fffcf6bbbe874ab0303246",
    paper: [
        "9bbf645b95253428f75e10483b2ff4c1a73112a8e795e9774f58127a33166d80",
        "d3f0c1b9c786c70c8f520ab300c26af2a01da1ed6626d9ed028a6a380cd50601",
        "d4eb806c5c8f4ec96ecd3db8b804f08169ecef7847ae61206279364f3e08c6c7",
    ],
};

const JAPAN: Before = Before {
    settled: concat!(
        r#"{"bill_id":"01JQ00000000000000000000B1","receipt_number":1,"#,
        r#""subtotal":{"currency_code":"JPY","amount_minor":3567},"#,
        r#""reduction_total":{"currency_code":"JPY","amount_minor":0},"#,
        r#""service_charge":{"currency_code":"JPY","amount_minor":360},"#,
        r#""tax_total":{"currency_code":"JPY","amount_minor":393},"#,
        r#""rounding_adjustment":{"currency_code":"JPY","amount_minor":0},"#,
        r#""total_due":{"currency_code":"JPY","amount_minor":4320},"buyer_subject_id":null,"#,
        r#""fee_lines":[{"fee_id":"00000000000000000000001ZQ1","code":"SERVICE","#,
        r#""display_name":"Service charge","amount":{"currency_code":"JPY","amount_minor":360},"#,
        r#""tax":{"currency_code":"JPY","amount_minor":32}}],"#,
        r#""tax_lines":[{"tax_class_id":"00000000000000000000000001","#,
        r#""taxable_base":{"currency_code":"JPY","amount_minor":3927},"rate_basis_points":1000,"#,
        r#""tax":{"currency_code":"JPY","amount_minor":393}}]}"#,
    ),
    chain_hash: "53fee4e43eb97d2a27e2466576fbc750ebf745cfd602e08183b37c9c9bd1a09b",
    paper: [
        "aad3382708e6d3b2b5c5dabb0761cee65e7fce40fef516ae92052efd89b777fd",
        "38fef9fb4ef655d5b28fbd75ba9976c6abdb457b8544ad5978496ed8316def01",
        "819a408c2132613ca4d2923ad48bacddcaf06b3a9c2b2662c09849b7ed8c22dd",
    ],
};

fn check(country: &str, sale: &Sale, before: &Before) {
    let (settled, chain_hash) = settle_event(sale);
    let paper: Vec<String> = sale.paper.iter().map(|bytes| digest(bytes)).collect();
    assert_eq!(settled, before.settled, "{country}'s settle event");
    assert_eq!(chain_hash, before.chain_hash, "{country}'s chain hash");
    for ((printed, bytes), (expected, document)) in paper
        .iter()
        .zip(&sale.paper)
        .zip(before.paper.iter().zip(["pre-bill", "receipt", "copy"]))
    {
        assert_eq!(
            printed,
            expected,
            "{country}'s {document}, as the printer received it:\n{}",
            String::from_utf8_lossy(bytes)
        );
    }
    assert_eq!(
        sale.paper.len(),
        3,
        "{country}: a pre-bill, a receipt and a copy"
    );
}

/// **Vietnam and Japan write and print what they did before.** Neither publishes components, so
/// the settle records none, and every byte of the event and the paper is main's.
#[tokio::test]
async fn vietnam_and_japan_write_and_print_the_bytes_they_did_before() {
    let (session, lines) = vietnam();
    check("Vietnam", &sell(session, lines).await, &VIETNAM);
    let (session, lines) = japan();
    check("Japan", &sell(session, lines).await, &JAPAN);
}

/// An Indian store: rupees in paise, prices before tax, food at 5 % GST printed as two halves named
/// `names`, and a 10 % service charge taxed as its lines are. One dish at ₹103: its 10.30 service
/// charge joins its class's base, 113.30 at 5 % is 5.665, rounded once to 5.67, and the halves are
/// allocated out of that, the odd paisa on the last.
fn india(names: [&str; 2]) -> (EdgeSession, Vec<LineDraft>) {
    let menu = MenuCatalog::new().with(MenuEntry::new(
        pizza(),
        DisplayName::new("Paneer tikka"),
        inr(10_300),
        standard(),
    ));
    let rates = TaxRateTable::new().with_components(
        standard(),
        SalesChannel::DineIn,
        TaxRate::from_percent(5),
        names
            .iter()
            .map(|name| TaxComponent::new(*name, TaxRate::from_basis_points(250)))
            .collect(),
    );
    let session = EdgeSession {
        currency: CurrencyCode::INR,
        currency_exponent: 2,
        devices: PublishedDevices::new(vec![counter_printer()]),
        fees: service(10),
        ..EdgeSession::bootstrap()
            .with_menu(menu)
            .with_tax_rates(rates)
    };
    let lines = vec![draft(pizza(), "Paneer tikka", inr(10_300), standard(), 500)];
    (session, lines)
}

fn inr(paise: i64) -> Money {
    Money::new(CurrencyCode::INR, paise)
}

/// Each component's name, rate and tax, for one assertion.
fn parts(components: &[BillTaxComponent]) -> Vec<(&str, u32, i64)> {
    components
        .iter()
        .map(|part| {
            (
                part.name.as_str(),
                part.rate_basis_points,
                part.tax.amount_minor,
            )
        })
        .collect()
}

/// **An Indian settle records CGST and SGST on its tax line**, allocated out of the rounded tax so
/// they sum to it exactly, with the odd paisa on the last (ADR-0104, ADR-0168 decision 3).
#[tokio::test]
async fn an_indian_settle_records_each_component_summing_to_its_tax() {
    let (session, lines) = india(["CGST", "SGST"]);
    let settled = sell(session, lines).await.settled();
    let [line] = settled.tax_lines.as_slice() else {
        panic!("one class, one line: {:?}", settled.tax_lines);
    };
    assert_eq!(line.tax, inr(567));
    assert_eq!(
        parts(&line.components),
        [("CGST", 250, 283), ("SGST", 250, 284)]
    );
    let split: i64 = line
        .components
        .iter()
        .map(|part| part.tax.amount_minor)
        .sum();
    assert_eq!(split, line.tax.amount_minor, "the parts are the tax");
}

/// **A fee taxed at that class records its share of the tax by component**, summing exactly to the
/// fee's tax (ADR-0159 decision 4, ADR-0168 decision 3): the fee's part of the class's 5.67 is
/// 0.51, split 0.25 and 0.26.
#[tokio::test]
async fn a_fee_records_its_tax_by_component_summing_to_its_tax() {
    let (session, lines) = india(["CGST", "SGST"]);
    let settled = sell(session, lines).await.settled();
    let [fee] = settled.fee_lines.as_slice() else {
        panic!("one fee: {:?}", settled.fee_lines);
    };
    assert_eq!((fee.amount, fee.tax), (inr(1_030), inr(51)));
    assert_eq!(
        parts(&fee.tax_components),
        [("CGST", 250, 25), ("SGST", 250, 26)]
    );
}

/// **A row named in free text is charged at its rate and records no breakdown** (ADR-0168
/// decision 2): the money is what the tokened table charges, and neither list is written at all.
#[tokio::test]
async fn a_row_named_in_free_text_is_charged_at_its_rate_and_records_no_breakdown() {
    let (session, lines) = india(["Central GST", "SGST"]);
    let sale = sell(session, lines).await;
    let settled = sale.settled();
    let [line] = settled.tax_lines.as_slice() else {
        panic!("one class, one line: {:?}", settled.tax_lines);
    };
    assert_eq!(line.tax, inr(567), "the rate is charged");
    assert!(line.components.is_empty());
    let [fee] = settled.fee_lines.as_slice() else {
        panic!("one fee: {:?}", settled.fee_lines);
    };
    assert_eq!(fee.tax, inr(51));
    assert!(fee.tax_components.is_empty());
    assert_eq!(settled.total_due, inr(10_300 + 1_030 + 567));
    let written = sale
        .log
        .iter()
        .find(|envelope| envelope.event_type.known() == Some(EventType::BillingBillSettled))
        .expect("a settle in the log")
        .data
        .as_json()
        .to_owned();
    assert!(!written.contains("components"), "{written}");
}

/// **A copy prints the components its settle recorded, not today's table's** (ADR-0164 decision 5,
/// ADR-0168 decision 4). The table now names UTGST where it named SGST, at the same rate, so the
/// tax computed again is the same and only the names tell the two apart.
#[tokio::test]
async fn a_copy_prints_the_components_its_settle_recorded_after_the_table_changes() {
    let (session, lines) = india(["CGST", "SGST"]);
    let sale = sell(session, lines).await;
    let (changed, _) = india(["CGST", "UTGST"]);
    sale.edge.apply_session(changed);

    let copy = sale.copy_printed().await;
    assert!(copy.contains("SGST 2.50%"), "{copy}");
    assert!(!copy.contains("UTGST"), "{copy}");
}
