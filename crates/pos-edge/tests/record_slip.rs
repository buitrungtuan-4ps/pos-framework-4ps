// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A custom record's slip
//! ([ADR-0207](../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)
//! decisions 4 and 5, [ADR-0206](../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decision 9): what the store's template prints for a record, framed by the seller, the record's
//! type and its reference, and a personal value only on the person's own copy, as the save's own
//! slip or for someone holding `data.personal.read`.
//!
//! Every value below is a demo value.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pos_core::decision::Actor;
use pos_core::package::{EnteredValue, Entry, PersonalText};
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::labels::LabelGrid;
use pos_edge::printing::{PrintOutcome, Printers, TransportFactory};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_ports::PortError;
use pos_proto::Open;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::{CustomRecordId, DeviceId, EmployeeId, ObjectTypeId, StoreId};
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::{DisplayName, TranslationKey};
use pos_proto::ulid::Ulid;
use printer_escpos::{Transport, TransportStatus, Unreachable};
use serde_json::json;

const PARTY: &str = "01J0000000000000000000CF02";
const CONTACT: &str = "01J0000000000000000000CF03";
const RESERVATION: &str = "01J00000000000000000000B01";

fn cashier() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(10)),
        device_id: DeviceId::new(Ulid::from_u128(20)),
    }
}

/// The store's receipt printer.
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
        device_class: Open::default(),
    }
}

/// What the printer received, one entry per job.
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

/// A reservation type with a party size and a personal contact note, and a receipt template whose
/// base prints both and marks `person_copy` as the template says.
fn session(person_copy: &[&str], template: bool) -> EdgeSession {
    let mut session = EdgeSession::bootstrap();
    session.package = Some(Arc::new(
        serde_json::from_value(json!({
            "package_id": "01J00000000000000000000P01",
            "package_version_id": "01J00000000000000000000V01",
            "version_label": "Reservations",
            "min_edge_version": "0.0.0",
            "data": { "object_types": [{ "object_type_id": RESERVATION, "label_key": "data.reservation",
                "fields": [
                    {"custom_field_id": PARTY, "label_key": "data.reservation.party",
                     "field_type": "CUSTOM_FIELD_TYPE_INT", "required": true, "personal": false},
                    {"custom_field_id": CONTACT, "label_key": "data.reservation.contact",
                     "field_type": "CUSTOM_FIELD_TYPE_TEXT", "personal": true, "retention_days": 30,
                     "consent_purpose_key": "data.consent.reservation"}
                ]}]}
        }))
        .expect("the package parses"),
    ));
    if template {
        session.receipt = serde_json::from_value(json!({ "templates": { "base": {
            "header_keys": ["receipt.header.welcome"],
            "custom_field_ids": [PARTY, CONTACT],
            "person_copy": person_copy
        }}}))
        .expect("the receipt node parses");
    }
    session.devices = PublishedDevices::new(vec![counter_printer()]);
    session.profile = StoreProfile {
        trading_name: Some("Demo Store".to_owned()),
        ..StoreProfile::default()
    };
    session
}

fn grid() -> LabelGrid {
    let entries: BTreeMap<String, BTreeMap<String, String>> = [
        ("receipt.header.welcome", "Welcome"),
        ("data.reservation", "Reservation"),
        ("data.reservation.party", "Party"),
        ("data.reservation.contact", "Contact"),
    ]
    .into_iter()
    .map(|(key, text)| {
        (
            key.to_owned(),
            BTreeMap::from([("en".to_owned(), text.to_owned())]),
        )
    })
    .collect();
    LabelGrid::bounded(entries).expect("a small grid")
}

struct Store {
    edge: Edge<FakeStore>,
    printers: Printers,
    paper: Arc<Paper>,
}

fn store(session: EdgeSession) -> Store {
    let edge = Edge::new(
        FakeStore::default(),
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(1))),
        session,
        Arc::new(InMemoryReceipts::new()),
    )
    .expect("seeds");
    edge.apply_grid(grid());
    let paper = Arc::new(Paper::default());
    Store {
        edge,
        printers: Printers::over(Arc::new(PaperRolls(Arc::clone(&paper)))),
        paper,
    }
}

impl Store {
    /// A reservation of four, with a demo note.
    async fn a_reservation(&self) -> CustomRecordId {
        let field = |id: &str| id.parse().expect("a field id");
        let reservation: ObjectTypeId = RESERVATION.parse().expect("a type id");
        self.edge
            .save_custom_record(
                cashier(),
                reservation,
                None,
                &[
                    Entry {
                        custom_field_id: field(PARTY),
                        value: EnteredValue::Integer(4),
                    },
                    Entry {
                        custom_field_id: field(CONTACT),
                        value: EnteredValue::Text(PersonalText::new("demo note")),
                    },
                ],
                &[TranslationKey::new("data.consent.reservation")],
            )
            .await
            .expect("saves")
            .custom_record_id
            .expect("a record")
    }

    /// Prints `record`'s slip for `actor`, and the text the printer received.
    async fn slip(&self, actor: Actor, record: CustomRecordId) -> Option<String> {
        let slip = self.edge.record_slip(actor, record).await.expect("a slip");
        let outcome = self
            .printers
            .print_record_slip(&self.edge, actor, record, &slip, "2026-11-07 12:00")
            .await?;
        assert_eq!(outcome, PrintOutcome::Printed);
        let paper = self.paper.0.lock().expect("the paper");
        Some(String::from_utf8_lossy(paper.last().expect("a slip printed")).into_owned())
    }
}

#[tokio::test]
async fn the_saves_own_slip_prints_the_personal_value_on_the_persons_copy() {
    let store = store(session(&["PERSON_COPY_RECORD_SLIP"], true));
    let record = store.a_reservation().await;
    let paper = store.slip(cashier(), record).await.expect("a slip");
    for printed in [
        "Welcome",
        "Party: 4",
        "Contact: demo note",
        "Demo Store",
        "Reservation",
        "Time: 2026-11-07 12:00",
    ] {
        assert!(paper.contains(printed), "{printed} on {paper:?}");
    }
    let reference: String = {
        let id = record.to_string();
        id.chars().skip(id.len() - 6).collect()
    };
    assert!(paper.contains(&format!("#{reference}")));
}

#[tokio::test]
async fn a_reprint_prints_it_only_for_someone_holding_data_personal_read() {
    let store = store(session(&["PERSON_COPY_RECORD_SLIP"], true));
    let record = store.a_reservation().await;
    store.slip(cashier(), record).await.expect("the first slip");
    assert!(
        store
            .slip(cashier(), record)
            .await
            .expect("a reprint")
            .contains("Contact: demo note"),
        "the bootstrap roster grants data.personal.read"
    );

    let mut session = (*store.edge.session()).clone();
    session.granted = PermissionSet::EMPTY.with(Permission::WriteDataRecord);
    store.edge.apply_session(session);
    let paper = store.slip(cashier(), record).await.expect("a reprint");
    assert!(
        paper.contains("Contact:") && !paper.contains("demo"),
        "{paper:?}"
    );
}

#[tokio::test]
async fn a_slip_not_marked_as_the_persons_copy_prints_the_label_alone() {
    let store = store(session(&[], true));
    let record = store.a_reservation().await;
    let paper = store.slip(cashier(), record).await.expect("a slip");
    assert!(paper.contains("Party: 4") && paper.contains("Contact:"));
    assert!(!paper.contains("demo"), "{paper:?}");
}

#[tokio::test]
async fn a_store_with_no_template_has_no_slip_to_print() {
    let store = store(session(&[], false));
    let record = store.a_reservation().await;
    assert_eq!(store.slip(cashier(), record).await, None);
    assert!(store.paper.0.lock().expect("the paper").is_empty());
}
