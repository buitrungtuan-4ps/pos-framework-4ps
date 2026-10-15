// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! ADR-0165: a cash drawer opens through the printer the console marked.

use pos_proto::devices::{DeviceConnection, PublishedDevice, PublishedDevices};
use pos_proto::ids::{DeviceId, StationId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::ulid::Ulid;

use crate::printing::tests::tills::{paired, till};
use crate::printing::tests::{
    STORE, drawer_device, event_id, recorder, session_with, store_id, totals_of,
};
use crate::printing::{DrawerOutcome, PrintOutcome, assumed_capabilities, drawer_printer};

// ---------------------------------------------------------------------
// ADR-0165: a cash drawer opens through the printer the console marked.

#[test]
fn a_drawer_outcome_names_itself_with_a_stable_token() {
    assert_eq!(DrawerOutcome::Opened.as_wire(), "OPENED");
    assert_eq!(DrawerOutcome::NoDrawer.as_wire(), "NO_DRAWER");
    assert_eq!(DrawerOutcome::Unavailable.as_wire(), "DRAWER_UNAVAILABLE");
}

#[test]
fn only_a_marked_usb_counter_printer_written_here_or_by_a_kicking_agent_is_a_drawer_printer() {
    let marked = drawer_device(1);
    let devices = PublishedDevices::new(vec![marked.clone()]);
    assert_eq!(drawer_printer(&devices, None, |_| false), Some(&marked));
    assert!(
        assumed_capabilities(&marked, false).may_open_a_drawer(),
        "the mark is what the adapter's own check reads"
    );

    let station = StationId::new(Ulid::from_u128(9));
    for (why, device) in [
        (
            "not marked",
            PublishedDevice {
                drawer_attached: false,
                ..drawer_device(2)
            },
        ),
        (
            "on the network, where port 9100 has no authentication",
            PublishedDevice {
                connection: DeviceConnection::Network.into(),
                ..drawer_device(3)
            },
        ),
        (
            "a kitchen station's printer",
            PublishedDevice {
                station_id: Some(station),
                ..drawer_device(4)
            },
        ),
        (
            "written through a print agent that does not carry the kick",
            PublishedDevice {
                agent_device_id: Some(DeviceId::new(Ulid::from_u128(5))),
                ..drawer_device(6)
            },
        ),
    ] {
        assert_eq!(
            drawer_printer(&PublishedDevices::new(vec![device]), None, |_| false),
            None,
            "{why}"
        );
    }

    // Behind an agent that carries the kick, the same printer is the drawer's (ADR-0167).
    let agent = DeviceId::new(Ulid::from_u128(5));
    let behind = PublishedDevice {
        agent_device_id: Some(agent),
        ..drawer_device(7)
    };
    let devices = PublishedDevices::new(vec![behind.clone()]);
    assert_eq!(
        drawer_printer(&devices, None, |kicking| kicking == agent),
        Some(&behind)
    );
}

#[test]
fn a_tills_drawer_opens_only_through_the_receipt_printer_its_terminal_names() {
    let store_drawer = drawer_device(2);
    let bar_drawer = PublishedDevice {
        address: "/dev/usb/lp1".to_owned(),
        ..drawer_device(3)
    };
    let devices = PublishedDevices::new(vec![
        store_drawer.clone(),
        bar_drawer.clone(),
        PublishedDevice {
            drawer_attached: false,
            ..drawer_device(4)
        },
        till(7, Some(3), None),
        till(8, Some(4), None),
        till(9, None, None),
    ]);
    assert_eq!(
        drawer_printer(&devices, None, |_| false),
        Some(&store_drawer),
        "the store's one drawer, as before"
    );
    assert_eq!(
        drawer_printer(&devices, Some(paired(7)), |_| false),
        Some(&bar_drawer)
    );
    for (why, till) in [
        ("names a printer with no drawer", 8),
        ("names no printer", 9),
        ("is not in the node", 10),
    ] {
        assert_eq!(
            drawer_printer(&devices, Some(paired(till)), |_| false),
            None,
            "a till that {why} has no drawer, and the store's does not open for it"
        );
    }
}

#[tokio::test]
async fn a_drawer_is_kicked_through_its_printer_and_nothing_else_is_written() {
    let (printers, written) = recorder(true);
    let outcome = printers
        .open_drawer(
            &session_with(PublishedDevices::new(vec![drawer_device(2)])),
            store_id(),
            event_id(0xD0),
            None,
        )
        .await;
    assert_eq!(outcome, DrawerOutcome::Opened);
    assert_eq!(
        *written.written.lock().expect("the recorder"),
        vec![printer_escpos::escpos::DRAWER_KICK.to_vec()],
        "one pulse on the kick pin, and no document"
    );

    let (printers, written) = recorder(true);
    let unmarked = PublishedDevice {
        drawer_attached: false,
        ..drawer_device(2)
    };
    let outcome = printers
        .open_drawer(
            &session_with(PublishedDevices::new(vec![unmarked])),
            store_id(),
            event_id(0xD0),
            None,
        )
        .await;
    assert_eq!(outcome, DrawerOutcome::NoDrawer);
    assert!(
        written.written.lock().expect("the recorder").is_empty(),
        "a store with no drawer sends nothing to its printer"
    );
}

#[tokio::test]
async fn a_drawer_printer_that_does_not_answer_is_unavailable() {
    let (printers, _written) = recorder(false);
    let outcome = printers
        .open_drawer(
            &session_with(PublishedDevices::new(vec![drawer_device(2)])),
            store_id(),
            event_id(0xD0),
            None,
        )
        .await;
    assert_eq!(outcome, DrawerOutcome::Unavailable);
}

#[tokio::test]
async fn a_drawer_marked_after_its_printer_was_opened_opens_without_a_restart() {
    // The printer is opened for a receipt before anybody ticks the box, so it is held with no
    // drawer. The next config pull marks it, and the mark must not wait for a restart.
    let (printers, written) = recorder(true);
    let unmarked = PublishedDevice {
        drawer_attached: false,
        ..drawer_device(2)
    };
    let receipt = printers
        .print_receipt(
            &session_with(PublishedDevices::new(vec![unmarked])),
            &STORE,
            store_id(),
            event_id(1),
            1,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 10_000)),
            None,
        )
        .await;
    assert_eq!(receipt, PrintOutcome::Printed);

    let outcome = printers
        .open_drawer(
            &session_with(PublishedDevices::new(vec![drawer_device(2)])),
            store_id(),
            event_id(0xD0),
            None,
        )
        .await;
    assert_eq!(outcome, DrawerOutcome::Opened);
    assert_eq!(
        written.written.lock().expect("the recorder").last(),
        Some(&printer_escpos::escpos::DRAWER_KICK.to_vec()),
        "the kick follows the receipt on the same printer"
    );
}
