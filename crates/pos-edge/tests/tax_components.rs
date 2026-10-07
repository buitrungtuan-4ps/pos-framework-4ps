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

use pos_core::billing::{Payment, TaxComponentLine};
use pos_core::decision::Actor;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::app::PreBill;
use pos_edge::app::ShiftReport;
use pos_edge::printing::{PrintOutcome, Printers, TillPrinting, TransportFactory};
use pos_edge::{
    CombinedReport, Edge, EdgeSession, InMemoryReceipts, LineDraft, StaffAuth, StaffRoster,
    StoreIdentity, TillScope,
};
use pos_fakes::FakeStore;
use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::chain::ChainLink;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::{BillTaxComponent, BillingBillSettled, EventType};
use pos_proto::fees::PublishedFees;
use pos_proto::ids::{
    BillId, BrandId, DeviceId, EmployeeId, EventId, MenuItemId, ShiftId, StoreId, TableId,
    TaxClassId, TenantId,
};
use pos_proto::locale::{NumberFormat, TaxComponent, TaxRate, TaxRateTable};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money, Ratio};
use pos_proto::printing::PublishedPrinting;
use pos_proto::quantity::Quantity;
use pos_proto::shift::{CloseReport, DrawerModel};
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
    /// The report its one drawer prints at a close after two such bills, and the slip two tills'
    /// drawers print closed together after one each, captured from main's code (a34f8a1) before
    /// the report listed tax by component (ADR-0168 decision 5).
    shift_report: &'static str,
    combined_slip: &'static str,
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
    shift_report: "3fa83a3a7e23c528daec0c4524dfc4e875ef2d78531626904152f25babc2d932",
    combined_slip: "af0c5df4440faf61d8e8b34fcb3c80def4a97f9d748f2b304c79da467dd1c489",
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
    shift_report: "381d5389690ce6bbebcde9a3c6b7dfe85118b846ec7e398911d52b0c97edb126",
    combined_slip: "034173a6d1a22ecd77c2ad9473a8793166e7d2acd8f91337957c31605d70823e",
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

/// `session`, with its `printing.receipt_tax_components` turned off (ADR-0168 decision 4).
fn components_off(session: EdgeSession) -> EdgeSession {
    EdgeSession {
        printing: PublishedPrinting {
            receipt_tax_components: Some(false),
            ..PublishedPrinting::default()
        },
        ..session
    }
}

/// **Vietnam and Japan write and print what they did before**, with the store's switch for tax
/// components on, as it is by default, and off. Neither publishes components, so the settle records
/// none, and every byte of the event, the guest's paper and the cash-up's is main's either way: the
/// shift report and the combined slip list no tax by component.
#[tokio::test]
async fn vietnam_and_japan_write_and_print_the_bytes_they_did_before() {
    for off in [false, true] {
        let turned = |session| {
            if off {
                components_off(session)
            } else {
                session
            }
        };
        for (country, (session, lines), before) in
            [("Vietnam", vietnam(), &VIETNAM), ("Japan", japan(), &JAPAN)]
        {
            let session = turned(session);
            check(country, &sell(session.clone(), lines.clone()).await, before);
            let (_, report) =
                shift_report_of(session.clone(), &[lines.clone(), lines.clone()]).await;
            assert_eq!(
                digest(&report),
                before.shift_report,
                "{country}'s shift report:\n{:?}",
                lines_of(&report)
            );
            let (_, slip) = combined_slip_of(session, &lines, &lines).await;
            assert_eq!(
                digest(&slip),
                before.combined_slip,
                "{country}'s combined slip:\n{:?}",
                lines_of(&slip)
            );
        }
    }
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

/// The text lines a printer was sent, its commands taken out. `printer-escpos` sends each line as
/// `ESC a`, `ESC E` and `GS !` with one argument each, the text and a newline, and resets them after;
/// a document starts with `ESC @` and ends with `GS V 0`.
fn lines_of(bytes: &[u8]) -> Vec<String> {
    let mut text = Vec::new();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            0x1B if bytes.get(at + 1) == Some(&0x40) => at += 2,
            0x1B | 0x1D => at += 3,
            _ => {
                text.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&text)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// What an Indian store's pre-bill, receipt and copy print with
/// `printing.receipt_tax_components` on, as it is by default: each tax line's components under its
/// rate (ADR-0104, ADR-0168 decision 4).
const WITH_COMPONENTS: [&[&str]; 3] = [
    &[
        "PRE-BILL",
        "T41",
        "Paneer tikka",
        "  1 x INR 103.00  INR 103.00",
        "Subtotal  INR 103.00",
        "Service charge  INR 10.30",
        "Tax 5.00%  INR 5.67",
        "  CGST 2.50%  INR 2.83",
        "  SGST 2.50%  INR 2.84",
        "INR 118.97",
        "Not a receipt",
    ],
    &[
        "#1",
        "Paneer tikka",
        "  1 x INR 103.00  INR 103.00",
        "Subtotal  INR 103.00",
        "Service charge  INR 10.30",
        "Tax 5.00%  INR 5.67",
        "  CGST 2.50%  INR 2.83",
        "  SGST 2.50%  INR 2.84",
        "INR 118.97",
    ],
    &[
        "#1",
        "COPY",
        "Reprint 1 - 2026-01-01 00:00",
        "Paneer tikka",
        "  1 x INR 103.00  INR 103.00",
        "Subtotal  INR 103.00",
        "Service charge  INR 10.30",
        "Tax 5.00%  INR 5.67",
        "  CGST 2.50%  INR 2.83",
        "  SGST 2.50%  INR 2.84",
        "INR 118.97",
    ],
];

/// The same paper with the switch off: each tax line alone.
const WITHOUT_COMPONENTS: [&[&str]; 3] = [
    &[
        "PRE-BILL",
        "T41",
        "Paneer tikka",
        "  1 x INR 103.00  INR 103.00",
        "Subtotal  INR 103.00",
        "Service charge  INR 10.30",
        "Tax 5.00%  INR 5.67",
        "INR 118.97",
        "Not a receipt",
    ],
    &[
        "#1",
        "Paneer tikka",
        "  1 x INR 103.00  INR 103.00",
        "Subtotal  INR 103.00",
        "Service charge  INR 10.30",
        "Tax 5.00%  INR 5.67",
        "INR 118.97",
    ],
    &[
        "#1",
        "COPY",
        "Reprint 1 - 2026-01-01 00:00",
        "Paneer tikka",
        "  1 x INR 103.00  INR 103.00",
        "Subtotal  INR 103.00",
        "Service charge  INR 10.30",
        "Tax 5.00%  INR 5.67",
        "INR 118.97",
    ],
];

/// **An Indian store prints CGST and SGST unless it turns them off** (ADR-0168 decision 4): on, its
/// pre-bill, receipt and copy print each tax line's components under its rate; off, the line alone.
/// The settle records them either way, because the switch is about paper.
#[tokio::test]
async fn an_indian_store_prints_its_tax_components_unless_it_turns_them_off() {
    for (off, goldens) in [(false, WITH_COMPONENTS), (true, WITHOUT_COMPONENTS)] {
        let (session, lines) = india(["CGST", "SGST"]);
        let session = if off {
            components_off(session)
        } else {
            session
        };
        let sale = sell(session, lines).await;
        for ((document, bytes), golden) in ["pre-bill", "receipt", "copy"]
            .iter()
            .zip(&sale.paper)
            .zip(goldens)
        {
            assert_eq!(lines_of(bytes), golden, "the {document}, switch off: {off}");
        }
        let settled = sale.settled();
        let recorded = settled
            .tax_lines
            .first()
            .map(|line| parts(&line.components))
            .unwrap_or_default();
        assert_eq!(
            recorded,
            [("CGST", 250, 283), ("SGST", 250, 284)],
            "switch off: {off}"
        );
    }
}

// --- The shift close report (ADR-0168 decision 5) ------------------------------------------------

/// The bar's till and the counter's, the two drawers a slip closes together.
const BAR: u128 = 0x7111;
const COUNTER: u128 = 0x7112;

/// A `TERMINAL` entry: a till, whose drawer is its own where the store keeps one per till.
fn terminal(seed: u128, name: &str) -> PublishedDevice {
    PublishedDevice {
        device_id: DeviceId::new(Ulid::from_u128(seed)),
        kind: DeviceKind::Terminal.into(),
        connection: Open::default(),
        address: String::new(),
        name: DisplayName::new(name),
        ..counter_printer()
    }
}

/// The server, holding what a sale and a drawer's shift take, and closing another till's drawer.
fn till_staff() -> StaffRoster {
    let mut staff = StaffRoster::new();
    staff.insert(
        "SRV-1",
        StaffAuth {
            employee_id: Some(server().employee_id),
            permissions: [
                Permission::OpenShift,
                Permission::CloseShift,
                Permission::ManageTables,
                Permission::AddLine,
                Permission::OpenBill,
                Permission::TakePayment,
                Permission::ManageOtherTill,
            ]
            .into_iter()
            .fold(PermissionSet::EMPTY, PermissionSet::with),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: None,
        },
    );
    staff
}

/// A request from `till`'s device, naming no other drawer.
fn at(till: u128) -> TillScope {
    TillScope {
        own: Some(DeviceId::new(Ulid::from_u128(till))),
        named: None,
    }
}

/// The shift id a report prints here, in place of the one the edge minted, so its bytes are the
/// same on every run.
fn printed_shift(n: u128) -> ShiftId {
    ShiftId::new(Ulid::from_u128(0x5_1F70 + n))
}

/// Seats `table`, rings `lines`, and settles the bill in cash at `scope`'s till: what it took.
async fn cash_sale(
    edge: &Edge<FakeStore>,
    scope: TillScope,
    table: u128,
    lines: &[LineDraft],
) -> Money {
    let table = TableId::new(Ulid::from_u128(table));
    edge.seat_table(server(), table, None).await.expect("seats");
    for line in lines {
        edge.add_line(server(), table, line.clone())
            .await
            .expect("rings");
    }
    let bill_id = edge
        .open_bill(server(), table)
        .await
        .expect("opens")
        .bill_id;
    let due = edge.check_totals(table).expect("the check reads").total_due;
    edge.settle_bill_at(
        server(),
        scope,
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
    due
}

/// The paper a cash-up sends the store's receipt printer, as it received it.
fn printed_by(paper: &Paper) -> Vec<u8> {
    paper
        .0
        .lock()
        .expect("the paper")
        .last()
        .cloned()
        .expect("something printed")
}

/// The report the store's one drawer prints when its shift closes, after a bill of each of
/// `bills`, all paid in cash: the report, under a fixed shift id, and its bytes.
async fn shift_report_of(session: EdgeSession, bills: &[Vec<LineDraft>]) -> (ShiftReport, Vec<u8>) {
    let paper = Arc::new(Paper::default());
    let printers = Printers::over(Arc::new(PaperRolls(Arc::clone(&paper))));
    let float = Money::new(session.currency, 100_000);
    let edge = Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(store_id()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    let shift = edge
        .open_shift(server(), float)
        .await
        .expect("opens")
        .shift_id;
    let mut taken = float;
    for (table, lines) in (41..).zip(bills) {
        let due = cash_sale(&edge, TillScope::default(), table, lines).await;
        taken = taken.checked_add(due).expect("in range");
    }
    edge.count_shift(server(), shift, taken.amount_minor)
        .await
        .expect("counts");
    let closed = edge.close_shift(server(), shift).await.expect("closes");
    let mut report = closed.report.expect("the close's report");
    report.shift_id = printed_shift(1);
    let printed = printers
        .print_shift_report(
            &edge.session(),
            &TillPrinting::STORE,
            store_id(),
            job(9),
            &report,
        )
        .await;
    assert_eq!(printed, PrintOutcome::Printed, "the shift report");
    (report, printed_by(&paper))
}

/// The slip the bar's and the counter's drawers print when they close together, after a bill of
/// `bar` at the bar and one of `counter` at the counter, each paid in cash at its own till.
async fn combined_slip_of(
    session: EdgeSession,
    bar: &[LineDraft],
    counter: &[LineDraft],
) -> (CombinedReport, Vec<u8>) {
    let paper = Arc::new(Paper::default());
    let printers = Printers::over(Arc::new(PaperRolls(Arc::clone(&paper))));
    let float = Money::new(session.currency, 100_000);
    let mut session = session;
    session.devices = PublishedDevices::new(vec![
        counter_printer(),
        terminal(BAR, "Bar"),
        terminal(COUNTER, "Counter"),
    ]);
    session.shift.drawer_model = Open::from_known(DrawerModel::PerTerminal);
    session.shift.close_report = Open::from_known(CloseReport::Combined);
    // The server holds what closing another till's drawer takes, so the bar closes both.
    session.staff = till_staff();
    session.permissions_enforced = true;
    let edge = Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(store_id()),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seed the id generator");
    let mut shifts = Vec::new();
    for (till, table, lines) in [(BAR, 41, bar), (COUNTER, 42, counter)] {
        let shift = edge
            .open_shift_at(server(), at(till), float, None)
            .await
            .expect("opens")
            .shift_id;
        let due = cash_sale(&edge, at(till), table, lines).await;
        edge.count_shift_at(
            server(),
            at(till),
            shift,
            float.checked_add(due).expect("in range").amount_minor,
            None,
        )
        .await
        .expect("counts");
        shifts.push(shift);
    }
    let closed = edge
        .close_shifts_at(server(), at(BAR), &shifts, None, &[])
        .await
        .expect("both close");
    let mut combined = closed.combined.expect("one slip");
    for ((_, report), n) in combined.drawers.iter_mut().zip(2..) {
        report.shift_id = printed_shift(n);
    }
    let printed = printers
        .print_combined_report(
            &edge.session(),
            &TillPrinting::STORE,
            store_id(),
            job(10),
            &combined,
        )
        .await;
    assert_eq!(printed, PrintOutcome::Printed, "the combined slip");
    (combined, printed_by(&paper))
}

/// Each summed component's name, rate and tax, for one assertion.
fn summed(components: &[TaxComponentLine]) -> Vec<(&str, u32, i64)> {
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

/// Two bills of the Indian store's dish: one of one dish, one of two, each with its service charge.
/// ₹113.30 of base is taxed 5.67, in halves of 2.83 and 2.84; ₹226.60 is taxed 11.33, in halves
/// of 5.66 and 5.67. The fee's share of each is inside those figures, not beside them.
fn two_indian_bills() -> [Vec<LineDraft>; 2] {
    let (_, lines) = india(["CGST", "SGST"]);
    let two = lines.iter().chain(&lines).cloned().collect();
    [lines, two]
}

/// **A shift's report lists its bills' tax by component**, summed by name and rate out of the
/// components their tax lines recorded (ADR-0168 decision 5): CGST and SGST at 2.5 % each, adding
/// up to the bills' tax. Each bill pays a service charge, whose tax is inside its tax line, so the
/// fee's own components are not counted again.
#[tokio::test]
async fn a_shift_report_lists_its_bills_tax_by_component() {
    let (session, _) = india(["CGST", "SGST"]);
    let (report, printed) = shift_report_of(session, &two_indian_bills()).await;
    assert_eq!(
        summed(&report.tax_components),
        [("CGST", 250, 283 + 566), ("SGST", 250, 284 + 567)]
    );
    let total: i64 = report
        .tax_components
        .iter()
        .map(|part| part.tax.amount_minor)
        .sum();
    assert_eq!(total, 567 + 1_133, "the two bills' tax_total, no more");
    assert_eq!(
        lines_of(&printed),
        [
            "SHIFT REPORT",
            "00A7VH",
            "Opening float  INR 1,000.00",
            "Cash taken  INR 356.90",
            "Expected in drawer  INR 1,356.90",
            "Counted  INR 1,356.90",
            "Variance  INR 0.00",
            "Balanced",
            "TAX BY COMPONENT",
            "CGST 2.50%  INR 8.49",
            "SGST 2.50%  INR 8.51",
        ]
    );
}

/// **A row named in free text adds nothing**: its bills recorded no components, so the report has
/// no section rather than a section claiming their tax (ADR-0168 decision 2).
#[tokio::test]
async fn a_row_named_in_free_text_adds_nothing_to_the_shifts_tax_by_component() {
    let (session, lines) = india(["Central GST", "SGST"]);
    let (report, printed) = shift_report_of(session, &[lines]).await;
    assert!(report.tax_components.is_empty());
    assert!(
        !lines_of(&printed)
            .iter()
            .any(|line| line.contains("TAX BY COMPONENT"))
    );
}

/// **Each till's drawer lists its own bills, and the slip's store totals are the sum** (ADR-0167
/// decision 10, ADR-0168 decision 5): one dish at the bar and two at the counter.
#[tokio::test]
async fn each_drawer_lists_its_own_bills_and_the_slip_sums_them() {
    let (session, _) = india(["CGST", "SGST"]);
    let [bar, counter] = two_indian_bills();
    let (combined, printed) = combined_slip_of(session, &bar, &counter).await;
    let drawers: Vec<Vec<(&str, u32, i64)>> = combined
        .drawers
        .iter()
        .map(|(_, report)| summed(&report.tax_components))
        .collect();
    assert_eq!(
        drawers,
        [
            vec![("CGST", 250, 283), ("SGST", 250, 284)],
            vec![("CGST", 250, 566), ("SGST", 250, 567)],
        ]
    );
    assert_eq!(
        summed(&combined.tax_components),
        [("CGST", 250, 849), ("SGST", 250, 851)]
    );
    let lines = lines_of(&printed);
    let sections: Vec<&str> = lines
        .iter()
        .filter(|line| line.starts_with("CGST") || line.starts_with("SGST"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        sections,
        [
            "CGST 2.50%  INR 8.49",
            "SGST 2.50%  INR 8.51",
            "CGST 2.50%  INR 2.83",
            "SGST 2.50%  INR 2.84",
            "CGST 2.50%  INR 5.66",
            "SGST 2.50%  INR 5.67",
        ],
        "the store's, then the bar's, then the counter's: {lines:?}"
    );
}

/// **A restart rebuilds a shift's tax by component from the log**, as it rebuilds its cash: each
/// settle names its shift, and the replay adds its components there again.
#[tokio::test]
async fn a_restart_rebuilds_the_shifts_tax_by_component() {
    let (session, lines) = india(["CGST", "SGST"]);
    let store = FakeStore::default();
    let edge_of = |session| {
        Edge::new(
            store.clone(),
            StoreIdentity::for_store(store_id()),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator")
    };
    let float = inr(100_000);
    let edge = edge_of(session.clone());
    let shift = edge
        .open_shift(server(), float)
        .await
        .expect("opens")
        .shift_id;
    let due = cash_sale(&edge, TillScope::default(), 41, &lines).await;

    let restarted = edge_of(session);
    restarted.rebuild().await.expect("replays the log");
    restarted
        .count_shift(
            server(),
            shift,
            float.checked_add(due).expect("in range").amount_minor,
        )
        .await
        .expect("counts after the restart");
    let closed = restarted
        .close_shift(server(), shift)
        .await
        .expect("closes after the restart");
    let report = closed.report.expect("the close's report");
    assert_eq!(
        summed(&report.tax_components),
        [("CGST", 250, 283), ("SGST", 250, 284)]
    );
}
