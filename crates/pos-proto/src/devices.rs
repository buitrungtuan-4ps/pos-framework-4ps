// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `devices` config node ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)):
//! the printers and kitchen displays a store may actually address.
//!
//! Discovery, proposal and approval are the cloud's, exactly where
//! [ADR-0041](../../../docs/adr/0041-device-onboarding.md) put them. This node is the last hop only:
//! the approved set, compiled into the store's configuration so the edge learns it through the
//! config-pull it already runs — and so a box that reboots with its broadband down still knows where
//! its printers are, because the config tree is persisted locally and restored at boot. A second
//! sync loop would come back knowing nothing, and a receipt is a legal artefact in Vietnam rather
//! than a convenience. ADR-0100 records the alternative (`GET /sync/stores/{id}/devices`, which is
//! built and keeps its job as the console's read) and why it is not this.
//!
//! **Never-blank, opt-in semantics**, as for `channels`/`tender`: an *absent* node means the store
//! has no addressable device and prints nothing — an ordinary state for a LAN-only box or a shop
//! with no printer. A *present* node is authoritative.
//!
//! # A drawer is not addressable over the network
//!
//! `docs/architecture.md` §5: port 9100 has no authentication of any kind, and the drawer-kick
//! command rides the same unauthenticated channel as everything else. So [`DeviceConnection`] is part
//! of a device's identity here, and a node naming a network printer with a drawer is *accepted* while
//! the drawer command is not sent — the refusal is `pos_ports::PrinterConnection::may_open_a_drawer`,
//! and this node cannot override it. Publishing an address changes nothing about that.

use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, StationId};
use crate::text::DisplayName;
use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// What kind of device this is.
    DeviceKind, prefix = "DEVICE_KIND";
    /// An ESC/POS receipt or kitchen printer.
    Printer = "PRINTER",
    /// A kitchen-display station.
    Kds = "KDS",
}

wire_enum! {
    /// How a device is attached — part of its identity, because a cash drawer may be opened only
    /// over USB (`docs/architecture.md` §5).
    DeviceConnection, prefix = "DEVICE_CONNECTION";
    /// Directly attached. The only connection over which a cash drawer may be opened.
    Usb = "USB",
    /// On the network, typically raw TCP on port 9100 — which has no authentication.
    Network = "NETWORK",
    /// Serial or parallel, on older hardware.
    Serial = "SERIAL",
}

/// One approved device the store may address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedDevice {
    /// The device's identity, as the cloud's approval assigned it.
    pub device_id: DeviceId,
    /// What it is.
    pub kind: Open<DeviceKind>,
    /// How it is attached.
    pub connection: Open<DeviceConnection>,
    /// Where to reach it: `host:port` for a network printer, an OS device path for USB or serial.
    ///
    /// Opaque here on purpose. What a valid address looks like is the adapter's question, and a node
    /// that encoded one shape would have to change every time a fork attached a printer a new way.
    pub address: String,
    /// The name to show an operator when this device is the one that failed.
    pub name: DisplayName,
    /// The kitchen station this device serves, if any.
    ///
    /// `None` is the receipt printer at the counter — it serves the *bill*, not a station, and a
    /// fired line never routes to it. A station printer names its station, and the station plan's
    /// `backup_station_id` is what decides where a ticket goes when this one is unreachable
    /// ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub station_id: Option<StationId>,
}

/// The `devices` config node: the store's approved, addressable devices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedDevices {
    /// The approved devices. An empty list published means the store addresses none.
    #[serde(default)]
    devices: Vec<PublishedDevice>,
}

impl PublishedDevices {
    /// A node listing exactly `devices`.
    #[must_use]
    pub fn new(devices: Vec<PublishedDevice>) -> Self {
        Self { devices }
    }

    /// Every device the node lists, in publication order.
    #[must_use]
    pub fn devices(&self) -> &[PublishedDevice] {
        &self.devices
    }

    /// Whether the node lists no device at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{DeviceConnection, DeviceKind, PublishedDevice, PublishedDevices};
    use crate::ids::{DeviceId, StationId};
    use crate::text::DisplayName;
    use crate::ulid::Ulid;

    fn device(station: Option<StationId>) -> PublishedDevice {
        PublishedDevice {
            device_id: DeviceId::new(Ulid::from_u128(1)),
            kind: DeviceKind::Printer.into(),
            connection: DeviceConnection::Network.into(),
            address: "192.0.2.10:9100".to_owned(),
            name: DisplayName::new("Counter"),
            station_id: station,
        }
    }

    #[test]
    fn a_counter_printer_serves_no_station_and_says_so_by_omission() {
        // `station_id` is skipped when absent rather than written as null: the receipt printer is
        // the common case, and a node full of explicit nulls makes a publish diff noisier than the
        // change it carries.
        let json = serde_json::to_string(&PublishedDevices::new(vec![device(None)])).expect("json");
        assert!(
            !json.contains("station_id"),
            "an absent station is omitted, not written as null: {json}"
        );
        let back: PublishedDevices = serde_json::from_str(&json).expect("round trip");
        let only = back.devices().first().expect("one device");
        assert_eq!(only.station_id, None);
    }

    #[test]
    fn a_kind_this_build_does_not_know_keeps_its_token_instead_of_failing_the_node() {
        // The forward-compatibility rule the whole config tree follows: a newer cloud naming a label
        // printer must not cost this store its receipt printer. `Open` retains the token, and the
        // dispatcher simply addresses nothing it cannot interpret.
        let raw = r#"{"devices":[
            {"device_id":"00000000000000000000000001","kind":"DEVICE_KIND_LABEL",
             "connection":"DEVICE_CONNECTION_NETWORK","address":"192.0.2.11:9100","name":"Labels"},
            {"device_id":"00000000000000000000000002","kind":"DEVICE_KIND_PRINTER",
             "connection":"DEVICE_CONNECTION_USB","address":"/dev/usb/lp0","name":"Counter"}
        ]}"#;
        let node: PublishedDevices =
            serde_json::from_str(raw).expect("the whole node still parses");
        assert_eq!(node.devices().len(), 2);
        let mut listed = node.devices().iter();
        let unknown = listed.next().expect("the label printer");
        let printer = listed.next().expect("the receipt printer");
        assert!(
            unknown.kind.is_unspecified(),
            "an unknown kind is not guessed at"
        );
        assert_eq!(printer.kind.known(), DeviceKind::Printer);
        assert_eq!(
            serde_json::to_value(&unknown.kind).expect("re-serialize"),
            serde_json::json!("DEVICE_KIND_LABEL"),
            "the original token survives a round trip through a build that predates it"
        );
    }

    #[test]
    fn an_absent_node_and_an_empty_one_are_both_readable_and_both_address_nothing() {
        let absent: PublishedDevices = serde_json::from_str("{}").expect("an absent list defaults");
        assert!(absent.is_empty());
        let empty: PublishedDevices = serde_json::from_str(r#"{"devices":[]}"#).expect("empty");
        assert!(empty.is_empty());
    }
}
