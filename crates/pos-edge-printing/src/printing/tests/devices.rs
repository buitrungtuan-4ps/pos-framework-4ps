// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which device prints what: the receipt printer, a station's printer and its backup, a KDS, a
//! connection this build does not know, and a directly attached printer's channel.

use pos_ports::printer::PrinterConnection;
use pos_proto::devices::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::floor::{KitchenStation, StationPlan};
use pos_proto::text::DisplayName;
use pos_proto::wire_enum::Open;

use crate::printing::tests::{device, station};
use crate::printing::{
    TcpTransports, TransportFactory, connection_of, receipt_printer, station_printer,
};

#[test]
fn the_receipt_printer_is_the_one_serving_no_station() {
    let devices = PublishedDevices::new(vec![
        device(1, DeviceKind::Printer, Some(station(9))),
        device(2, DeviceKind::Printer, None),
    ]);
    let chosen = receipt_printer(&devices).expect("a receipt printer");
    assert_eq!(chosen.address, "192.0.2.2:9100");
}

#[test]
fn a_store_with_only_kitchen_printers_has_no_receipt_printer_and_says_so() {
    // `None`, not "fall back to the oven". A guest's bill printing in the kitchen is worse than
    // not printing: the guest waits for paper that went somewhere they cannot see.
    let devices = PublishedDevices::new(vec![device(1, DeviceKind::Printer, Some(station(9)))]);
    assert!(receipt_printer(&devices).is_none());
}

#[test]
fn a_kds_is_not_something_to_send_escpos_to() {
    let devices = PublishedDevices::new(vec![device(1, DeviceKind::Kds, None)]);
    assert!(
        receipt_printer(&devices).is_none(),
        "a kitchen display is a screen, not a printer"
    );
}

#[test]
fn a_station_with_no_printer_falls_back_to_the_plans_declared_backup() {
    let oven = station(1);
    let grill = station(2);
    let plan = StationPlan::from_parts(
        vec![
            KitchenStation {
                station_id: oven,
                name: DisplayName::new("Oven"),
                backup_station_id: Some(grill),
                late_after_seconds: None,
                ticket_language: Open::default(),
            },
            KitchenStation {
                station_id: grill,
                name: DisplayName::new("Grill"),
                backup_station_id: None,
                late_after_seconds: None,
                ticket_language: Open::default(),
            },
        ],
        Vec::new(),
        None,
    );
    // Only the grill has a printer.
    let devices = PublishedDevices::new(vec![device(2, DeviceKind::Printer, Some(grill))]);
    let (printing, chosen) =
        station_printer(&devices, &plan, oven).expect("the backup station's printer");
    assert_eq!(chosen.address, "192.0.2.2:9100");
    assert_eq!(
        printing, grill,
        "the grill's printer, so the grill's cooks read it"
    );
    let (printing, _) = station_printer(&devices, &plan, grill).expect("its own printer");
    assert_eq!(printing, grill);
}

#[test]
fn the_backup_is_followed_once_and_not_chased_around_a_loop() {
    // Two stations naming each other. `pos_core::floor` rejects a plan like this, but a plan
    // reaching here unvalidated must not hang the printer thread — one hop, then give up.
    let left = station(1);
    let right = station(2);
    let plan = StationPlan::from_parts(
        vec![
            KitchenStation {
                station_id: left,
                name: DisplayName::new("Left"),
                backup_station_id: Some(right),
                late_after_seconds: None,
                ticket_language: Open::default(),
            },
            KitchenStation {
                station_id: right,
                name: DisplayName::new("Right"),
                backup_station_id: Some(left),
                late_after_seconds: None,
                ticket_language: Open::default(),
            },
        ],
        Vec::new(),
        None,
    );
    let devices = PublishedDevices::new(Vec::new());
    assert!(station_printer(&devices, &plan, left).is_none());
}

#[test]
fn a_connection_this_build_does_not_know_degrades_to_the_posture_that_authorises_least() {
    let mut unknown = device(1, DeviceKind::Printer, None);
    unknown.connection = Open::parse("DEVICE_CONNECTION_INFRARED");
    assert_eq!(connection_of(&unknown), PrinterConnection::Network);
    assert!(
        !connection_of(&unknown).may_open_a_drawer(),
        "an unrecognised connection never authorises a cash drawer"
    );
}

#[test]
fn a_directly_attached_printer_gets_a_channel_rather_than_a_refusal() {
    // Before ADR-0103 this arm returned "this build talks to network printers only". A store
    // with a USB printer had no way to print at all, and a cash drawer — which may only ever
    // open over USB (architecture.md §5) — had no transport to open over.
    let usb = PublishedDevice {
        connection: DeviceConnection::Usb.into(),
        address: "/dev/usb/lp0".to_owned(),
        ..device(1, DeviceKind::Printer, None)
    };
    assert!(
        TcpTransports.open(&usb).is_ok(),
        "a USB printer should get a device channel"
    );

    let serial = PublishedDevice {
        connection: DeviceConnection::Serial.into(),
        address: "/dev/ttyUSB0".to_owned(),
        ..device(2, DeviceKind::Printer, None)
    };
    assert!(TcpTransports.open(&serial).is_ok(), "and so should serial");

    // Opening the channel is not reaching the printer: nothing is touched until the first write,
    // so a device path that does not exist fails where every other unreachable printer does.
    let network = device(3, DeviceKind::Printer, None);
    assert!(
        TcpTransports.open(&network).is_ok(),
        "and the LAN still works"
    );
}
