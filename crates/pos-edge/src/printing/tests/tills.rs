// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! ADR-0160 decision 4: a till's own receipt printer and languages.

use std::pin::Pin;
use std::sync::{Arc, Mutex};

use pos_ports::printer::PrintJob;
use pos_ports::{PortError, PortName};
use pos_proto::ClockSource;
use pos_proto::devices::{DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::{DeviceId, MenuItemId};
use pos_proto::menu::{MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::printing::{PublishedPrinting, ReceiptLanguage, ReceiptSecondLanguage};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use printer_escpos::{Transport, TransportStatus, Unreachable};

use crate::app::{EdgeSession, ReceiptLine};
use crate::paper_labels::{ENGLISH, VIETNAMESE};
use crate::printing::tests::language::a_bilingual_bill;
use crate::printing::tests::receipts::sold;
use crate::printing::tests::{device, event_id, session_with, station, store_id, totals_of};
use crate::printing::{
    AgentLane, PrintOutcome, Printers, SecondLanguage, TillPrinting, TransportFactory,
    receipt_language,
};

// --- A till's own receipt printer and languages (ADR-0160 decision 4) ----------------------

/// What each printer was sent, by its address, so a test can say at which counter a document
/// came out. The printer at `down`, if any, does not answer.
#[derive(Debug, Default)]
struct Counters {
    sent: Mutex<Vec<(String, Vec<u8>)>>,
    down: Option<&'static str>,
}

impl Counters {
    /// The addresses written to, in order.
    fn at(&self) -> Vec<String> {
        let sent = self.sent.lock().expect("the counters");
        sent.iter().map(|(address, _)| address.clone()).collect()
    }

    /// Everything the printer at `address` was sent, in one.
    fn bytes_at(&self, address: &str) -> Vec<u8> {
        let sent = self.sent.lock().expect("the counters");
        sent.iter()
            .filter(|(at, _)| at == address)
            .flat_map(|(_, bytes)| bytes.clone())
            .collect()
    }
}

#[derive(Debug)]
struct CountersFactory(Arc<Counters>);

#[derive(Debug)]
struct CounterTransport {
    counters: Arc<Counters>,
    address: String,
}

impl Transport for CounterTransport {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        if self.counters.down == Some(self.address.as_str()) {
            return Err(Unreachable);
        }
        self.counters
            .sent
            .lock()
            .map_err(|_| Unreachable)?
            .push((self.address.clone(), bytes.to_vec()));
        Ok(())
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        if self.counters.down == Some(self.address.as_str()) {
            Err(Unreachable)
        } else {
            Ok(TransportStatus::default())
        }
    }
}

impl TransportFactory for CountersFactory {
    fn open(&self, device: &PublishedDevice) -> Result<Box<dyn Transport>, PortError> {
        Ok(Box::new(CounterTransport {
            counters: Arc::clone(&self.0),
            address: device.address.clone(),
        }))
    }
}

const COUNTER: &str = "192.0.2.2:9100";
const BAR: &str = "192.0.2.3:9100";

pub(super) fn paired(seed: u128) -> DeviceId {
    DeviceId::new(Ulid::from_u128(seed))
}

/// A terminal `seed` naming `printer` as its receipt printer and `language` as its language.
pub(super) fn till(
    seed: u128,
    printer: Option<u128>,
    language: Option<ReceiptLanguage>,
) -> PublishedDevice {
    PublishedDevice {
        receipt_printer_id: printer.map(paired),
        receipt_language: language.map_or_else(Open::default, Open::from_known),
        ..device(seed, DeviceKind::Terminal, None)
    }
}

/// The store's devices: its receipt printer at the counter, a printer at the bar, a kitchen's,
/// and four tills. The bar till (7) names the bar's printer; the door till (8) names nothing;
/// the terrace till (9) names the kitchen's, and the patio till (10) a printer the node lacks.
fn the_tills() -> PublishedDevices {
    PublishedDevices::new(vec![
        device(2, DeviceKind::Printer, None),
        device(3, DeviceKind::Printer, None),
        device(4, DeviceKind::Printer, Some(station(9))),
        till(7, Some(3), None),
        till(8, None, None),
        till(9, Some(4), None),
        till(10, Some(5), None),
    ])
}

/// A dispatcher over `counters` whose binding record holds each till for one paired device:
/// the bar till for `0xBA2`, the door till for `0xD002`, the terrace till for `0x7E2` and the
/// patio till for `0xFA7`.
async fn with_tills(counters: &Arc<Counters>) -> Printers {
    use crate::print_agent::PrintAgents as _;
    let agents = Arc::new(crate::print_agent::InMemoryPrintAgents::new());
    let now = crate::clock::SystemClock
        .now()
        .as_milliseconds_since_epoch();
    for (terminal, device) in [(7, 0xBA2), (8, 0xD002), (9, 0x7E2), (10, 0xFA7)] {
        agents
            .claim(paired(terminal), paired(device), now)
            .await
            .expect("the binding is recorded");
    }
    Printers::over(Arc::new(CountersFactory(Arc::clone(counters)))).with_agents(Arc::new(
        AgentLane::new(
            agents,
            Arc::new(crate::print_queue::InMemoryPrintQueue::new()),
            Arc::new(crate::print_wake::SharedPrintWake::new()),
        ),
    ))
}

/// Prints a receipt for the till `device` is, and answers what came of it.
async fn receipt_from(printers: &Printers, session: &EdgeSession, device: u128) -> PrintOutcome {
    let till = printers.till_for(session, paired(device)).await;
    printers
        .print_receipt(
            session,
            &till,
            store_id(),
            event_id(device),
            42,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 99_000)),
            None,
        )
        .await
}

#[tokio::test]
async fn a_till_bound_to_a_terminal_with_its_own_printer_prints_there_and_the_rest_at_the_stores() {
    let counters = Arc::new(Counters::default());
    let printers = with_tills(&counters).await;
    let session = session_with(the_tills());
    for (device, expected, why) in [
        (0xBA2, BAR, "the bar till prints at the bar"),
        (
            0xD002,
            COUNTER,
            "a till whose terminal names nothing prints at the store's",
        ),
        (
            0x0FF,
            COUNTER,
            "a device bound to no terminal prints at the store's",
        ),
        (
            0x7E2,
            COUNTER,
            "a kitchen's printer is not a receipt printer",
        ),
        (
            0xFA7,
            COUNTER,
            "a printer the node does not list is the store's case",
        ),
    ] {
        assert_eq!(
            receipt_from(&printers, &session, device).await,
            PrintOutcome::Printed,
            "{why}"
        );
        assert_eq!(
            counters.at().last().map(String::as_str),
            Some(expected),
            "{why}"
        );
    }
    assert_eq!(
        counters.at().len(),
        5,
        "one receipt each, and no other paper"
    );
}

#[tokio::test]
async fn a_tills_printer_that_does_not_answer_is_reported_and_nothing_prints_at_the_counter() {
    // No fallback: the bar's bill at the counter is worse than a failure the cashier reads.
    let counters = Arc::new(Counters {
        down: Some(BAR),
        ..Counters::default()
    });
    let printers = with_tills(&counters).await;
    let session = session_with(the_tills());
    assert_eq!(
        receipt_from(&printers, &session, 0xBA2).await,
        PrintOutcome::Unavailable
    );
    assert!(
        counters.at().is_empty(),
        "nothing came out anywhere: {:?}",
        counters.at()
    );
}

/// A binding record that cannot be read.
struct Unreadable;

impl super::AgentDispatch for Unreadable {
    fn enqueue<'a>(
        &'a self,
        _agent: DeviceId,
        _printer: DeviceId,
        _job: PrintJob,
    ) -> Pin<Box<dyn Future<Output = PrintOutcome> + Send + 'a>> {
        Box::pin(async { PrintOutcome::AgentUnavailable })
    }

    fn terminal_of<'a>(
        &'a self,
        _device: DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<DeviceId>, PortError>> + Send + 'a>> {
        Box::pin(async {
            Err(PortError::unavailable(
                PortName::EventStore,
                "the binding cannot be read",
            ))
        })
    }
}

#[tokio::test]
async fn a_till_that_cannot_be_told_is_refused_only_where_a_till_prints_its_own() {
    let counters = Arc::new(Counters::default());
    let printers = Printers::over(Arc::new(CountersFactory(Arc::clone(&counters))))
        .with_agents(Arc::new(Unreadable));
    // A terminal names its own printer, so which till this is decides the counter.
    let session = session_with(the_tills());
    assert_eq!(
        receipt_from(&printers, &session, 0xBA2).await,
        PrintOutcome::Unavailable
    );
    assert!(counters.at().is_empty(), "not guessed at the counter");
    // No terminal names anything: the binding is not read, and the store prints as before.
    let session = session_with(PublishedDevices::new(vec![
        device(2, DeviceKind::Printer, None),
        till(8, None, None),
    ]));
    assert_eq!(
        receipt_from(&printers, &session, 0xBA2).await,
        PrintOutcome::Printed
    );
    assert_eq!(counters.at(), [COUNTER]);
}

#[tokio::test]
async fn a_tills_language_prints_its_receipt_in_that_language_and_the_others_in_the_stores() {
    // An English store whose menu names the pho in Vietnamese, ASCII here so that a box with no
    // fonts prints it, and a bar till that prints its receipts in Vietnamese.
    let item = MenuItemId::new(Ulid::from_u128(1));
    let session = EdgeSession {
        display_language: Some("en".to_owned()),
        menu: MenuCatalog::new().with(
            MenuEntry::new(
                item,
                DisplayName::new("Beef pho"),
                Money::new(CurrencyCode::VND, 65_000),
                EdgeSession::standard_tax_class(),
            )
            .with_name_translations([("vi".to_owned(), DisplayName::new("Pho bo"))].into()),
        ),
        ..session_with(PublishedDevices::new(vec![
            device(2, DeviceKind::Printer, None),
            till(7, None, Some(ReceiptLanguage::Vietnamese)),
        ]))
    };
    let price = Money::new(CurrencyCode::VND, 65_000);
    let lines = [ReceiptLine {
        menu_item_id: item,
        ..sold("Beef pho", 1_000, price, price)
    }];
    let counters = Arc::new(Counters::default());
    let printers = with_tills(&counters).await;
    for (device, name) in [(0xBA2, "Pho bo"), (0xD002, "Beef pho")] {
        let till = printers.till_for(&session, paired(device)).await;
        let outcome = printers
            .print_receipt(
                &session,
                &till,
                store_id(),
                event_id(device),
                42,
                &lines,
                &totals_of(price),
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::Printed);
        let bytes = counters.bytes_at(COUNTER);
        assert!(
            bytes
                .windows(name.len())
                .any(|window| window == name.as_bytes()),
            "{name}: the till's language decides the names, at the store's printer"
        );
        counters.sent.lock().expect("the counters").clear();
    }
    assert_eq!(
        receipt_language(
            &session,
            &TillPrinting::of(&till(7, None, Some(ReceiptLanguage::Country)))
        )
        .as_deref(),
        Some("en"),
        "resolved as the store's own choice is: a country with no language on the node"
    );
}

#[test]
fn a_tills_second_language_replaces_the_stores_and_none_is_a_choice() {
    let (lines, totals) = a_bilingual_bill();
    let session = EdgeSession {
        display_language: Some("vi".to_owned()),
        printing: PublishedPrinting {
            receipt_second_language: Open::from_known(ReceiptSecondLanguage::English),
            ..PublishedPrinting::default()
        },
        ..EdgeSession::bootstrap()
    };
    let second = |till: &PublishedDevice| {
        SecondLanguage::for_receipt(
            &session,
            &TillPrinting::of(till),
            &VIETNAMESE,
            true,
            42,
            &lines,
            &lines,
            &totals,
        )
        .map(|second| second.labels)
    };
    let one_language = PublishedDevice {
        receipt_second_language: Open::from_known(ReceiptSecondLanguage::None),
        ..till(7, None, None)
    };
    assert_eq!(second(&till(8, None, None)), Some(&ENGLISH), "the store's");
    assert_eq!(second(&one_language), None, "this till prints one language");
}
