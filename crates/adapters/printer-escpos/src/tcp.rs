// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The raw-TCP transport: ESC/POS bytes at port 9100
//! ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)).
//!
//! Port 9100 is the whole protocol — a socket, and whatever you write comes out of the printer.
//! There is no handshake, no acknowledgement and, most importantly, **no authentication**, which is
//! why [`PrinterConnection::Network`](pos_ports::printer::PrinterConnection::Network) never
//! authorises a cash drawer (`docs/architecture.md` §5). This transport carries bytes; it grants
//! nothing.
//!
//! # One connection, reopened rather than reused blindly
//!
//! Most thermal printers accept a single concurrent connection and drop an idle one without warning.
//! So the socket is held and reused — opening one per receipt is slow enough to be felt at a till —
//! and a failed write reconnects once and tries again. A printer that dropped an idle connection is
//! the ordinary case, not a fault, and treating it as one would put a "printer offline" toast in
//! front of a cashier every quiet afternoon.
//!
//! # The sensors are not read here
//!
//! [`TcpTransport::probe`] reports both sensors as `None` — "this model cannot tell" — rather than
//! guessing. Real-time status over 9100 is `DLE EOT n`, a *read* whose reply format and timing vary
//! by model and which some firmware answers only mid-job; a wrong answer here reads as "out of
//! paper" and refuses to print on a printer that is fine.
//! [`PrinterStatus::is_ready`](pos_ports::printer::PrinterStatus::is_ready) treats an absent sensor
//! as healthy for exactly this reason. Reachability *is* checked: a probe that cannot open the socket
//! is [`Unreachable`], which is the signal the dispatcher acts on. Reading real sensors is part of the
//! hardware gate (`docs/gate-register.md` §6).

use std::io::Write as _;
use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::Mutex;
use std::time::Duration;

use crate::{Transport, TransportStatus, Unreachable};

/// The port every ESC/POS network printer listens on. Not configurable per device: it is the
/// convention the whole class of hardware follows, and an address may still carry an explicit port.
pub const RAW_PRINTING_PORT: u16 = 9100;

/// How long to wait for the socket to open. Short on purpose: a printer that has been unplugged must
/// not hold a settle open, and the caller re-queues rather than blocking a cashier.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a single read or write may take once connected.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// A byte channel to a printer on the store's LAN.
#[derive(Debug)]
pub struct TcpTransport {
    address: String,
    stream: Mutex<Option<TcpStream>>,
}

impl TcpTransport {
    /// A transport to `address`, which may be `host` or `host:port`. A bare host gets
    /// [`RAW_PRINTING_PORT`].
    ///
    /// Nothing is connected here: a printer that is off when the store boots must not stop the store
    /// booting, so the socket opens on the first job.
    #[must_use]
    pub fn new(address: &str) -> Self {
        Self {
            address: with_default_port(address),
            stream: Mutex::new(None),
        }
    }

    /// The address this transport dials, port included — what a diagnostic prints.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Opens a socket to the printer, replacing whatever was held.
    fn connect(&self) -> Result<TcpStream, Unreachable> {
        let target = self
            .address
            .to_socket_addrs()
            .map_err(|_| Unreachable)?
            .next()
            .ok_or(Unreachable)?;
        let stream =
            TcpStream::connect_timeout(&target, CONNECT_TIMEOUT).map_err(|_| Unreachable)?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|_| Unreachable)?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|_| Unreachable)?;
        // A receipt is a few hundred bytes written in one go; Nagle would add a round trip's delay to
        // the end of every job for no benefit.
        stream.set_nodelay(true).map_err(|_| Unreachable)?;
        Ok(stream)
    }
}

impl Transport for TcpTransport {
    fn write(&self, bytes: &[u8]) -> Result<(), Unreachable> {
        let mut held = self.stream.lock().map_err(|_| Unreachable)?;
        // The held socket first. A printer that closed an idle connection fails here, which is
        // ordinary, so the second attempt is on a fresh socket rather than an error.
        if let Some(stream) = held.as_mut()
            && send(stream, bytes).is_ok()
        {
            return Ok(());
        }
        let mut fresh = self.connect()?;
        let outcome = send(&mut fresh, bytes);
        // Held whether or not this write succeeded: a socket that failed once is dropped on the next
        // attempt anyway, and holding it costs one file descriptor.
        *held = Some(fresh);
        outcome
    }

    fn probe(&self) -> Result<TransportStatus, Unreachable> {
        let mut held = self.stream.lock().map_err(|_| Unreachable)?;
        if held.is_none() {
            *held = Some(self.connect()?);
        }
        // Both sensors unknown, deliberately — see the module docs. Reachability was just proven.
        Ok(TransportStatus::default())
    }
}

fn send(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), Unreachable> {
    stream.write_all(bytes).map_err(|_| Unreachable)?;
    stream.flush().map_err(|_| Unreachable)
}

/// Appends the raw-printing port to an address that names only a host.
///
/// The three shapes an operator actually types: `192.168.1.50`, `192.168.1.50:9101`, and — rarely,
/// but a link-local printer is a real thing — an IPv6 literal. A bare IPv6 literal is wrapped in
/// brackets before the port is appended, because `::1:9100` would otherwise parse as another address
/// entirely and dial something nobody asked for.
fn with_default_port(address: &str) -> String {
    let address = address.trim();
    if let Some(rest) = address.strip_prefix('[') {
        // `[::1]:9101` already names a port; `[::1]` does not.
        return match rest.split_once(']') {
            Some((_, tail)) if tail.starts_with(':') => address.to_owned(),
            _ => format!("{address}:{RAW_PRINTING_PORT}"),
        };
    }
    match address.match_indices(':').count() {
        // A host or an IPv4 address with no port.
        0 => format!("{address}:{RAW_PRINTING_PORT}"),
        // Exactly one colon is `host:port`.
        1 => address.to_owned(),
        // More than one is a bare IPv6 literal, which needs brackets before it can carry a port.
        _ => format!("[{address}]:{RAW_PRINTING_PORT}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{RAW_PRINTING_PORT, TcpTransport, with_default_port};
    use crate::Transport as _;
    use std::io::Read as _;
    use std::net::TcpListener;

    #[test]
    fn a_bare_host_gets_the_raw_printing_port() {
        assert_eq!(with_default_port("192.168.1.50"), "192.168.1.50:9100");
        assert_eq!(with_default_port(" printer.local "), "printer.local:9100");
        assert_eq!(RAW_PRINTING_PORT, 9100);
    }

    #[test]
    fn an_address_that_already_names_a_port_keeps_it() {
        // An operator who typed a port meant it — a printer behind a port-forward is a real setup.
        assert_eq!(with_default_port("192.168.1.50:9101"), "192.168.1.50:9101");
        assert_eq!(with_default_port("[::1]:9101"), "[::1]:9101");
        assert_eq!(with_default_port("[::1]"), "[::1]:9100");
        assert_eq!(with_default_port("fe80::1"), "[fe80::1]:9100");
    }

    #[test]
    fn bytes_reach_the_printer() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let address = listener.local_addr().expect("an address").to_string();
        let accepted = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a connection");
            let mut received = Vec::new();
            // The transport holds the socket open, so read until the test drops it.
            let _ = stream.read_to_end(&mut received);
            received
        });

        let transport = TcpTransport::new(&address);
        transport.probe().expect("the printer answers");
        transport.write(b"ESC/POS").expect("the bytes go out");
        drop(transport);

        assert_eq!(accepted.join().expect("the listener thread"), b"ESC/POS");
    }

    #[test]
    fn a_printer_that_is_not_there_is_unreachable_rather_than_a_hang() {
        // Bound then dropped: nothing is listening on that port, which is what an unplugged printer
        // looks like. The caller re-queues; it does not block the till.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
            listener.local_addr().expect("an address").port()
        };
        let transport = TcpTransport::new(&format!("127.0.0.1:{port}"));
        assert!(transport.probe().is_err());
        assert!(transport.write(b"ESC/POS").is_err());
    }
}
