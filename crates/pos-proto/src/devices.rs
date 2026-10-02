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
use crate::wire_enum::{Open, WireEnum};

wire_enum! {
    /// What kind of device this is.
    DeviceKind, prefix = "DEVICE_KIND";
    /// An ESC/POS receipt or kitchen printer.
    Printer = "PRINTER",
    /// A kitchen-display station.
    Kds = "KDS",
    /// A fixed, mains-powered POS terminal that may hold a printer's transport on the edge's behalf
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// Nothing dials a terminal, so its [`PublishedDevice::address`] is empty and its
    /// [`PublishedDevice::connection`] is `DEVICE_CONNECTION_UNSPECIFIED` — the agent opens a
    /// connection to the edge, never the other way round. It is also the one kind no store can
    /// *discover*: nothing on a LAN announces itself as a till, so an operator creates the entry in
    /// the console and the named admin who created it is the human gate ADR-0041 was protecting.
    Terminal = "TERMINAL",
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

impl DeviceConnection {
    /// The short, lowercase spelling used in the console and in the cloud's `device_proposals`
    /// column — `usb`, `network`, `serial`.
    ///
    /// Distinct from [`crate::wire_enum::WireEnum::as_wire`], which produces the prefixed token the
    /// *config node* carries (`DEVICE_CONNECTION_USB`). Two spellings because they answer to two
    /// different rules: the node's is forward-compatible and prefixed so an older store can retain a
    /// token it does not know, while a database column and a `<select>` want the plain word. The
    /// mapping lives here, once, rather than in whichever layer needed it first.
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::Usb => "usb",
            Self::Network => "network",
            Self::Serial => "serial",
        }
    }

    /// Parses a short name, or `None` for one this build does not know.
    ///
    /// `unspecified` deliberately does not parse: it is the wire's degradation value, never a thing
    /// an operator picks or a column should hold.
    #[must_use]
    pub fn from_short_name(name: &str) -> Option<Self> {
        match name {
            "usb" => Some(Self::Usb),
            "network" => Some(Self::Network),
            "serial" => Some(Self::Serial),
            _ => None,
        }
    }
}

wire_enum! {
    /// The paper a printer takes, which decides how many characters a line holds and how wide a
    /// raster it accepts
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 2). Nothing a printer reports says so, so an operator says so in the console. Each
    /// is at 203 dpi, which almost every thermal printer runs at.
    PaperWidth, prefix = "PAPER_WIDTH";
    /// 80 mm paper, 42 characters a line and 576 dots: what every printer was taken to be before
    /// the field existed.
    Millimetres80 = "MILLIMETRES_80",
    /// 80 mm paper laid out at 48 characters a line, as some 80 mm printers are set up, and 576
    /// dots.
    Millimetres80Columns48 = "MILLIMETRES_80_COLUMNS_48",
    /// 58 mm paper, 32 characters a line and 384 dots.
    Millimetres58 = "MILLIMETRES_58",
}

impl PaperWidth {
    /// Characters a line holds in the printer's standard font. `UNSPECIFIED` is 80 mm's.
    #[must_use]
    pub const fn columns(self) -> u16 {
        match self {
            Self::Unspecified | Self::Millimetres80 => 42,
            Self::Millimetres80Columns48 => 48,
            Self::Millimetres58 => 32,
        }
    }

    /// Printable dots a line, the widest raster the printer takes. `UNSPECIFIED` is 80 mm's.
    #[must_use]
    pub const fn dots_per_line(self) -> u16 {
        match self {
            Self::Unspecified | Self::Millimetres80 | Self::Millimetres80Columns48 => 576,
            Self::Millimetres58 => 384,
        }
    }
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
    /// The device whose transport reaches this printer
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// **Absent means the edge is the agent**, which is what the edge has always done and what every
    /// in-store placement keeps doing: the box opens the address itself. Present, it names a
    /// [`DeviceKind::Terminal`] in this same node, and the edge renders the document and hands the
    /// bytes to that terminal's agent instead of opening a transport.
    ///
    /// It lives on the printer because a printer's agent is a fact about that printer, in the same
    /// category as its connection and its address — both of which are already here. A separate node
    /// would be two records that can disagree about one device, and the config tree's never-blank
    /// rule ([ADR-0033](../../../docs/adr/0033-config-tree.md)) makes disagreement durable: one node
    /// updates, the other keeps its previous value, and the store prints somewhere nobody chose.
    ///
    /// An older edge that does not know this field ignores it and opens the address, which is the
    /// safe direction: in-store that is the behaviour it already had, and on a hosted edge it opens
    /// a device path that is not there and reports a named refusal rather than failing silently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_device_id: Option<DeviceId>,
    /// Whether a cash drawer is wired to this printer's kick port
    /// ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)
    /// decision 4).
    ///
    /// An operator says so in the console, because nothing a printer reports does
    /// ([ADR-0103](../../../docs/adr/0103-directly-attached-printers.md)). `false`, which is also
    /// what an absent field reads as, means no drawer: what every edge assumed before this field
    /// existed. It says a drawer is there, not that it may be opened from anywhere. A drawer still
    /// opens only over USB, and only when the edge writes the printer's bytes itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drawer_attached: bool,
    /// The paper the printer takes. Read it through [`PublishedDevice::paper_width`], which reads an
    /// absent or unknown value as 80 mm. Left off the wire while absent, as `agent_device_id` is, so
    /// a node that sets none is written as before.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub paper_width: Open<PaperWidth>,
    /// Whether the printer cuts its paper. Read it through [`PublishedDevice::cuts_paper`]: absent is
    /// `true`, which every printer was taken to do. `false` is a printer with no cutter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuts_paper: Option<bool>,
}

/// Whether a field holds no value at all. A token from a newer release is a value, and goes back
/// out as it came.
fn is_absent<E: WireEnum>(value: &Open<E>) -> bool {
    *value == Open::default()
}

impl PublishedDevice {
    /// The paper the printer takes.
    ///
    /// An absent value, `PAPER_WIDTH_UNSPECIFIED` and a value this release does not know all read as
    /// [`PaperWidth::Millimetres80`], which is what every printer was taken to be before the console
    /// could say otherwise.
    #[must_use]
    pub fn paper_width(&self) -> PaperWidth {
        match self.paper_width.known() {
            PaperWidth::Unspecified | PaperWidth::Millimetres80 => PaperWidth::Millimetres80,
            PaperWidth::Millimetres80Columns48 => PaperWidth::Millimetres80Columns48,
            PaperWidth::Millimetres58 => PaperWidth::Millimetres58,
        }
    }

    /// Whether the printer cuts its paper: `true` unless the console says it has no cutter.
    #[must_use]
    pub fn cuts_paper(&self) -> bool {
        self.cuts_paper.unwrap_or(true)
    }
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
    use super::{DeviceConnection, DeviceKind, PaperWidth, PublishedDevice, PublishedDevices};
    use crate::ids::{DeviceId, StationId};
    use crate::text::DisplayName;
    use crate::ulid::Ulid;
    use crate::wire_enum::Open;

    fn device(station: Option<StationId>) -> PublishedDevice {
        PublishedDevice {
            device_id: DeviceId::new(Ulid::from_u128(1)),
            kind: DeviceKind::Printer.into(),
            connection: DeviceConnection::Network.into(),
            address: "192.0.2.10:9100".to_owned(),
            name: DisplayName::new("Counter"),
            station_id: station,
            agent_device_id: None,
            drawer_attached: false,
            paper_width: Open::default(),
            cuts_paper: None,
        }
    }

    #[test]
    fn the_two_spellings_of_a_connection_agree_and_unspecified_does_not_parse() {
        for connection in [
            DeviceConnection::Usb,
            DeviceConnection::Network,
            DeviceConnection::Serial,
        ] {
            assert_eq!(
                DeviceConnection::from_short_name(connection.short_name()),
                Some(connection)
            );
        }
        assert_eq!(
            DeviceConnection::from_short_name("unspecified"),
            None,
            "the wire's degradation value is not something an operator picks"
        );
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
    fn a_printer_that_names_no_agent_says_so_by_omission_and_the_edge_opens_the_address() {
        // The whole compatibility claim of ADR-0112 in one assertion: a fleet takes this release and
        // prints tomorrow as it printed today, because for every store that configures nothing the
        // field is not on the wire at all. A node full of explicit nulls would also work, and would
        // make every publish diff carry a change nobody made.
        let json = serde_json::to_string(&PublishedDevices::new(vec![device(None)])).expect("json");
        assert!(
            !json.contains("agent_device_id"),
            "a printer the edge opens itself names no agent: {json}"
        );
        let back: PublishedDevices = serde_json::from_str(&json).expect("round trip");
        assert_eq!(
            back.devices().first().expect("one device").agent_device_id,
            None
        );
    }

    #[test]
    fn a_printer_may_name_the_terminal_whose_transport_reaches_it() {
        let terminal = DeviceId::new(Ulid::from_u128(7));
        let raw = r#"{"devices":[
            {"device_id":"00000000000000000000000007","kind":"DEVICE_KIND_TERMINAL",
             "connection":"DEVICE_CONNECTION_UNSPECIFIED","address":"","name":"Till 1"},
            {"device_id":"00000000000000000000000001","kind":"DEVICE_KIND_PRINTER",
             "connection":"DEVICE_CONNECTION_USB","address":"/dev/usb/lp0","name":"Counter",
             "agent_device_id":"00000000000000000000000007"}
        ]}"#;
        let node: PublishedDevices = serde_json::from_str(raw).expect("the node parses");
        let mut listed = node.devices().iter();
        let till = listed.next().expect("the terminal");
        let printer = listed.next().expect("the printer");
        assert_eq!(till.kind.known(), DeviceKind::Terminal);
        assert!(
            till.address.is_empty(),
            "nothing dials a terminal, so it publishes no address"
        );
        assert_eq!(
            till.connection.known(),
            DeviceConnection::Unspecified,
            "a terminal is not attached to the edge at all: the agent connects outbound"
        );
        assert_eq!(
            printer.agent_device_id,
            Some(terminal),
            "the printer names the terminal that holds its transport"
        );
    }

    #[test]
    fn a_printer_with_no_drawer_says_so_by_omission_and_an_attached_one_round_trips() {
        // The ADR-0112 rule again: a fleet that marks nothing publishes exactly the node it did
        // before, so an edge that predates the field reads the same bytes and opens no drawer.
        let json = serde_json::to_string(&PublishedDevices::new(vec![device(None)])).expect("json");
        assert!(
            !json.contains("drawer_attached"),
            "a printer with no drawer writes no field: {json}"
        );
        let back: PublishedDevices = serde_json::from_str(&json).expect("round trip");
        assert!(!back.devices().first().expect("one device").drawer_attached);

        let marked = PublishedDevice {
            connection: DeviceConnection::Usb.into(),
            drawer_attached: true,
            ..device(None)
        };
        let json = serde_json::to_string(&PublishedDevices::new(vec![marked])).expect("json");
        assert!(json.contains(r#""drawer_attached":true"#), "{json}");
        let back: PublishedDevices = serde_json::from_str(&json).expect("round trip");
        assert!(back.devices().first().expect("one device").drawer_attached);
    }

    #[test]
    fn a_printer_nobody_set_paper_for_is_an_80_mm_printer_that_cuts_and_writes_neither_field() {
        // The ADR-0112 rule once more: a fleet that sets nothing publishes the node it did before,
        // and every printer reads as the one this framework always took it to be.
        let json = serde_json::to_string(&PublishedDevices::new(vec![device(None)])).expect("json");
        assert!(!json.contains("paper_width"), "{json}");
        assert!(!json.contains("cuts_paper"), "{json}");
        let back: PublishedDevices = serde_json::from_str(&json).expect("round trip");
        let printer = back.devices().first().expect("one device");
        assert_eq!(printer.paper_width(), PaperWidth::Millimetres80);
        assert_eq!(
            (
                printer.paper_width().columns(),
                printer.paper_width().dots_per_line()
            ),
            (42, 576)
        );
        assert!(printer.cuts_paper());
    }

    #[test]
    fn a_paper_and_a_printer_with_no_cutter_round_trip_and_a_newer_paper_keeps_its_token() {
        let raw = r#"{"devices":[
            {"device_id":"00000000000000000000000001","kind":"DEVICE_KIND_PRINTER",
             "connection":"DEVICE_CONNECTION_USB","address":"/dev/usb/lp0","name":"Counter",
             "paper_width":"PAPER_WIDTH_MILLIMETRES_58","cuts_paper":false},
            {"device_id":"00000000000000000000000002","kind":"DEVICE_KIND_PRINTER",
             "connection":"DEVICE_CONNECTION_NETWORK","address":"192.0.2.12:9100","name":"Bar",
             "paper_width":"PAPER_WIDTH_MILLIMETRES_112"}
        ]}"#;
        let node: PublishedDevices = serde_json::from_str(raw).expect("the node parses");
        let [narrow, newer] = node.devices() else {
            panic!("two printers: {node:?}");
        };
        assert_eq!(narrow.paper_width(), PaperWidth::Millimetres58);
        assert_eq!(
            (
                narrow.paper_width().columns(),
                narrow.paper_width().dots_per_line()
            ),
            (32, 384)
        );
        assert!(!narrow.cuts_paper());
        assert_eq!(
            PaperWidth::Millimetres80Columns48.columns(),
            48,
            "80 mm paper laid out at 48 a line"
        );
        // A paper from a newer release prints as an 80 mm printer, as before the field existed,
        // and goes back out as it came.
        assert_eq!(newer.paper_width(), PaperWidth::Millimetres80);
        assert!(newer.cuts_paper());
        let json = serde_json::to_string(&node).expect("json");
        assert!(
            json.contains(r#""paper_width":"PAPER_WIDTH_MILLIMETRES_58","cuts_paper":false"#),
            "{json}"
        );
        assert!(
            json.contains(r#""paper_width":"PAPER_WIDTH_MILLIMETRES_112""#),
            "{json}"
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
