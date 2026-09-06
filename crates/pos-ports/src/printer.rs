// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! ESC/POS printing, and the queue behind it.
//!
//! # The port reports its code page; it does not decide about bitmaps
//!
//! `docs/pos-spec.md` §13 requires any line containing characters outside the printer's
//! code page to be **rendered as a bitmap** — without it, Vietnamese diacritics and CJK
//! print as garbage on most thermal printers. That decision lives in the framework, not in
//! the adapter: duplicating it per vendor would let two printers disagree about the same
//! receipt, and `docs/architecture.md` §6.1 already says an adapter carries the vendor's
//! protocol and nothing else. So this port exposes [`PrinterCapabilities`] and the framework
//! renders. See [ADR-0026](../../../docs/adr/0026-port-shapes.md) §5.
//!
//! # A drawer-kick printer must be USB
//!
//! `docs/architecture.md` §5 is blunt about why: port 9100 has no authentication of any
//! kind, and the drawer-kick command is on the same unauthenticated channel as everything
//! else. So [`PrinterConnection`] is part of a printer's identity and
//! [`PrinterCapabilities::kicks_drawer`] is meaningless without it — a caller that opens a
//! drawer over the network is making a decision the network cannot secure.
//!
//! # A print document may contain personal data
//!
//! A corporate invoice carries the buyer's name and tax code (`docs/roadmap.md` P10), so
//! [`PrintDocument`] is deliberately **not** covered by any no-personal-data marker. The
//! rule that applies instead: a document is never logged, and `tracing` records the job's
//! identifier and outcome, never its content.
//!
//! # A rendered job is serialisable, because it may have to be stored and handed on
//!
//! [ADR-0112](../../../docs/adr/0112-print-agents.md) lets a printer name another device as the
//! one whose transport reaches it. The edge still renders every byte — that record spends four
//! paragraphs on why a device that rendered any of them would be wrong — so what crosses to that
//! device is a *finished* [`PrintJob`], parked on a durable queue until the agent claims it. That
//! is the only reason these types carry `serde` derives, and it is why they carry them **here**
//! rather than in a second definition somewhere else: two shapes for one document is two
//! renderings of a legal document that can disagree.
//!
//! [`PrintBlock::Bitmap`]'s raster goes as lowercase hex rather than as a JSON array of numbers.
//! An array spends three to four characters on every byte and a 576-dot line is 72 bytes a row, so
//! a Vietnamese ticket's rasters would be the bulk of a queue row; hex is two characters flat, and
//! it keeps a raster one field instead of a thousand.
//!
//! [`PrinterCapabilities`] is serialisable for the same reason and no other. The agent's contract is
//! *open this address, write these bytes* — so the address, the connection and the capabilities the
//! **edge** assumed all travel with the job. An agent that computed its own capabilities would be a
//! second opinion about a printer's column width, and a store with two terminals would produce two
//! shapes of receipt from one order.

use core::fmt;
use core::future::Future;
use core::num::NonZeroU16;

use serde::{Deserialize, Serialize};

use pos_proto::ids::{StationId, StoreId};

use crate::error::PortError;

/// [`PrintBlock::Bitmap`]'s raster, as lowercase hex.
///
/// Hand-rolled rather than pulled from a crate: `pos-ports` is a backbone crate, so every
/// dependency it declares needs an architecture reviewer and an entry in
/// `tools/backbone-allowlist.toml`, and this is twenty lines with no decisions in it.
mod hex_bits {
    use serde::{Deserialize as _, Deserializer, Serializer};

    /// Writes the bytes as `2 * len` lowercase hex characters.
    pub(super) fn serialize<S: Serializer>(bits: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let mut hex = String::with_capacity(bits.len().saturating_mul(2));
        for byte in bits {
            // Two nibbles, most significant first, so the string reads in memory order.
            for nibble in [byte >> 4, byte & 0x0f] {
                hex.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
            }
        }
        serializer.serialize_str(&hex)
    }

    /// Reads them back, refusing an odd length or a character that is not hex.
    ///
    /// Refusing is the only safe direction: a raster whose byte count is wrong shears on the print
    /// head, and half a row of pixels is not a partial success.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        // `String` rather than `&str`: a borrowed form cannot be read back by a streaming
        // deserializer, and one allocation per raster is not worth the constraint.
        let hex = String::deserialize(deserializer)?;
        if hex.len() % 2 != 0 {
            return Err(serde::de::Error::custom(
                "a raster needs an even number of hex characters",
            ));
        }
        let bytes = hex.as_bytes();
        let mut bits = Vec::with_capacity(hex.len() / 2);
        for pair in bytes.chunks_exact(2) {
            // Destructured rather than indexed: `chunks_exact(2)` guarantees the length, and
            // saying so in the pattern is what keeps the guarantee visible where it is relied on.
            let [high, low] = pair else {
                return Err(serde::de::Error::custom("a raster must be hex"));
            };
            let nibble = |byte: &u8| {
                char::from(*byte)
                    .to_digit(16)
                    .ok_or_else(|| serde::de::Error::custom("a raster must be hex"))
            };
            // Both digits are < 16, so the shift and the `or` are exact in a byte.
            bits.push(u8::try_from(nibble(high)? << 4 | nibble(low)?).unwrap_or(0));
        }
        Ok(bits)
    }
}

/// How a printer is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PrinterConnection {
    /// Directly attached. The only connection over which a cash drawer may be opened.
    Usb,
    /// On the network, typically raw TCP on port 9100 — which has no authentication.
    Network,
    /// Serial or parallel, on older hardware.
    Serial,
}

impl PrinterConnection {
    /// Whether a cash drawer attached to this printer may be opened.
    ///
    /// Only over USB. `docs/architecture.md` §5 gives the alternative — every POS device on
    /// a separate VLAN — but a framework cannot verify a VLAN, and defaulting to "no" is the
    /// only safe direction when the check cannot be made.
    #[must_use]
    pub const fn may_open_a_drawer(self) -> bool {
        matches!(self, Self::Usb)
    }
}

/// The character set a printer's firmware can render directly.
///
/// Anything outside it becomes a bitmap. The list is short because it only needs to cover
/// what the fleet actually contains; an unrecognised page is
/// [`CodePage::Unsupported`], which forces bitmap rendering for everything and is therefore
/// always safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CodePage {
    /// 7-bit ASCII only.
    Ascii,
    /// Code page 1258, which covers Vietnamese.
    Vietnamese1258,
    /// Code page 932, Shift-JIS.
    Japanese932,
    /// Nothing is known about this printer's character set, so nothing is sent as text.
    Unsupported,
}

impl CodePage {
    /// Whether every character in `line` can be sent as text rather than as a bitmap.
    ///
    /// # What this build actually knows
    ///
    /// ASCII, and nothing else. The extended pages answer the same as [`Self::Ascii`] today,
    /// deliberately: this framework does not carry the CP1258 or Shift-JIS repertoire tables,
    /// and a bitmap is correct on every model. Half a table would be worse than none, because
    /// the characters it got wrong would print as question marks on a customer's receipt while
    /// the tests passed.
    ///
    /// The variants are still worth distinguishing. An adapter reports what its firmware
    /// claims, which shows up in diagnostics, and widening coverage for one page later is a
    /// change to this method alone rather than to the port.
    ///
    /// Conservative in the one direction that matters: [`Self::Unsupported`] answers `false`
    /// for everything, plain ASCII included. A wrong `true` prints a receipt full of question
    /// marks in front of a customer; a wrong `false` costs a few milliseconds.
    #[must_use]
    pub fn covers(self, line: &str) -> bool {
        match self {
            Self::Unsupported => false,
            Self::Ascii | Self::Vietnamese1258 | Self::Japanese932 => line.is_ascii(),
        }
    }
}

/// What a printer can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterCapabilities {
    /// How it is attached.
    pub connection: PrinterConnection,
    /// The character set its firmware renders directly.
    pub code_page: CodePage,
    /// Characters per line in the standard font. Receipt layout is computed from this, so
    /// a wrong value produces wrapped totals.
    pub columns: NonZeroU16,
    /// Printable dots per line — the width of a raster this printer will accept.
    ///
    /// Separate from [`columns`](Self::columns) because the two answer different questions and
    /// neither derives from the other: `columns` is how much *text* fits, which depends on the
    /// firmware's font, while this is how wide a *bitmap* may be, which is a property of the print
    /// head. The common values are 384 for 58 mm paper and 576 for 80 mm, both at 203 dpi.
    ///
    /// A raster wider than this is not clipped by the printer — the excess wraps onto the next
    /// line and shears the image — so [ADR-0102](../../../docs/adr/0102-printing-any-script.md)
    /// makes the renderer take this as its width rather than guess.
    pub dots_per_line: NonZeroU16,
    /// Whether it can print raster images, which is what bitmap fallback needs.
    pub prints_bitmaps: bool,
    /// Whether it can cut paper.
    pub cuts_paper: bool,
    /// Whether a cash drawer is wired to it.
    pub kicks_drawer: bool,
}

impl PrinterCapabilities {
    /// Whether this printer may be asked to open a drawer.
    ///
    /// Both conditions, and the connection is the one that is not negotiable.
    #[must_use]
    pub const fn may_open_a_drawer(&self) -> bool {
        self.kicks_drawer && self.connection.may_open_a_drawer()
    }

    /// Whether `line` must be sent as a bitmap rather than as text.
    ///
    /// # Errors
    ///
    /// Returns `Err(())` when the line needs a bitmap and the printer cannot print one —
    /// the one combination that has no correct output, and therefore must be a decision the
    /// caller makes rather than a silent substitution of question marks.
    #[expect(
        clippy::result_unit_err,
        reason = "there is exactly one failure and its name is the method's name; an error \
                  enum with one variant would add a type without adding information"
    )]
    pub fn needs_bitmap(&self, line: &str) -> Result<bool, ()> {
        if self.code_page.covers(line) {
            return Ok(false);
        }
        if self.prints_bitmaps {
            Ok(true)
        } else {
            Err(())
        }
    }
}

/// Text styling. Deliberately minimal — a receipt is a document, not a design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextStyle {
    /// Bold.
    pub emphasised: bool,
    /// Double height and width, for a total or a queue number.
    pub double_size: bool,
    /// Centred rather than left-aligned.
    pub centred: bool,
}

/// One element of a printed document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PrintBlock {
    /// A line of text.
    ///
    /// The framework decides between this and [`Self::Bitmap`] using
    /// [`PrinterCapabilities::needs_bitmap`]; an adapter receiving `Text` sends it as text.
    Text {
        /// The line.
        line: String,
        /// How to render it.
        style: TextStyle,
    },
    /// A pre-rendered raster image: one bit per pixel, rows padded to whole bytes.
    ///
    /// Carries the width so an adapter need not infer it, because inferring it from the
    /// byte count requires knowing the height, and getting that wrong shears the image.
    Bitmap {
        /// Pixels per row.
        width: NonZeroU16,
        /// Row-major bits, `width.div_ceil(8)` bytes per row.
        #[serde(with = "hex_bits")]
        bits: Vec<u8>,
    },
    /// A one-dimensional barcode.
    Barcode {
        /// The encoded value.
        value: String,
    },
    /// A QR code — a table link for guest ordering, or an invoice lookup code.
    QrCode {
        /// The encoded value.
        value: String,
    },
    /// Blank lines.
    Feed {
        /// How many.
        lines: u16,
    },
    /// Cut the paper. Ignored by a printer that cannot.
    Cut,
}

/// A document to print.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintDocument {
    /// The blocks, in order.
    pub blocks: Vec<PrintBlock>,
}

/// A print job.
///
/// `job_id` is the idempotency key. Reprinting is a deliberate act with its own permission
/// and its own audit trail (`docs/pos-spec.md` §12 counts reprint rates per employee), so a
/// retry after an ambiguous failure must not silently produce a second ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintJob {
    /// Idempotency key. A retry reuses it; a reprint mints a new one.
    pub job_id: pos_proto::ids::EventId,
    /// Which store.
    pub store_id: StoreId,
    /// Which station's printer, or `None` for a job that no station made — the guest's receipt,
    /// which comes off the counter's printer when the bill settles
    /// ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md)).
    ///
    /// Optional rather than a nil id: a receipt genuinely has no station, and a placeholder would
    /// have to be recognised as "not really a station" by everything that reads this.
    pub station_id: Option<StationId>,
    /// What to print.
    pub document: PrintDocument,
}

/// What a printer is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrinterStatus {
    /// Whether the printer answered at all.
    pub online: bool,
    /// Whether it has paper. `None` when the model cannot report it — which many cannot,
    /// and pretending otherwise would make a missing sensor look like a full roll.
    pub has_paper: Option<bool>,
    /// Whether the cover is closed. `None` when the model cannot report it.
    pub cover_closed: Option<bool>,
    /// Jobs the adapter is still holding.
    pub queue_depth: u32,
}

impl PrinterStatus {
    /// Whether a job sent now is likely to come out.
    ///
    /// A `None` sensor counts as healthy, because the alternative — treating an absent
    /// sensor as a fault — would make every printer that cannot report paper permanently
    /// unusable.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.online
            && !matches!(self.has_paper, Some(false))
            && !matches!(self.cover_closed, Some(false))
    }
}

impl fmt::Display for PrinterStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_ready() {
            write!(f, "ready, {} queued", self.queue_depth)
        } else if !self.online {
            f.write_str("offline")
        } else if matches!(self.has_paper, Some(false)) {
            f.write_str("out of paper")
        } else {
            f.write_str("cover open")
        }
    }
}

/// Drives one printer.
///
/// # Contract
///
/// 1. **`print` is idempotent by [`PrintJob::job_id`].** Sending the same job twice produces
///    one ticket. This is what makes retrying after an ambiguous failure safe, and without
///    it a flaky USB cable produces duplicate kitchen tickets.
/// 2. **A queued job survives the adapter being busy.** `docs/pos-spec.md` §2 puts printing
///    behind a queue with retry, so a printer that is briefly unreachable returns
///    [`PortError::unavailable`] and the caller re-queues; it must not block.
/// 3. **`open_drawer` refuses over the network.** An adapter whose connection is not
///    [`PrinterConnection::Usb`] returns [`PortError::failed_precondition`], whatever the
///    firmware would have accepted.
/// 4. **`capabilities` does not lie by omission.** A model whose code page is unknown
///    reports [`CodePage::Unsupported`], which costs a bitmap and prints correctly, rather
///    than [`CodePage::Ascii`], which prints question marks in front of a customer.
pub trait PrinterDriver: Send + Sync {
    /// What this printer can do. Synchronous: it is configuration, not a question for the
    /// hardware.
    #[must_use]
    fn capabilities(&self) -> PrinterCapabilities;

    /// Prints a document.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the printer cannot be reached — the ordinary case, and
    /// the caller re-queues; [`PortError::failed_precondition`] if it is out of paper or its
    /// cover is open; [`PortError::invalid_argument`] if the document asks for something the
    /// printer cannot do, such as a bitmap on a text-only model.
    fn print(&self, job: &PrintJob) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Asks the printer how it is.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the printer cannot be reached.
    fn status(&self) -> impl Future<Output = Result<PrinterStatus, PortError>> + Send;

    /// Opens the attached cash drawer.
    ///
    /// A permissioned, audited action: `docs/pos-spec.md` §6 requires a permission and a log
    /// entry for opening a drawer outside a sale. This port performs it; the permission check
    /// belongs to the domain.
    ///
    /// # Errors
    ///
    /// [`PortError::failed_precondition`] if no drawer is attached **or** the printer is not
    /// USB-attached, and [`PortError::unavailable`] if the printer cannot be reached.
    fn open_drawer(&self) -> impl Future<Output = Result<(), PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{
        CodePage, PrintBlock, PrintDocument, PrintJob, PrinterCapabilities, PrinterConnection,
        PrinterStatus, TextStyle,
    };
    use core::num::NonZeroU16;

    fn capabilities(connection: PrinterConnection, code_page: CodePage) -> PrinterCapabilities {
        PrinterCapabilities {
            connection,
            code_page,
            columns: NonZeroU16::new(42).expect("positive"),
            dots_per_line: NonZeroU16::new(576).expect("positive"),
            prints_bitmaps: true,
            cuts_paper: true,
            kicks_drawer: true,
        }
    }

    #[test]
    fn a_drawer_never_opens_over_the_network() {
        // Port 9100 has no authentication, and the drawer-kick command rides the same
        // channel as everything else. This is the check that makes the rule in
        // architecture.md §5 mechanical.
        assert!(capabilities(PrinterConnection::Usb, CodePage::Ascii).may_open_a_drawer());
        assert!(!capabilities(PrinterConnection::Network, CodePage::Ascii).may_open_a_drawer());
        assert!(!capabilities(PrinterConnection::Serial, CodePage::Ascii).may_open_a_drawer());

        let no_drawer = PrinterCapabilities {
            kicks_drawer: false,
            ..capabilities(PrinterConnection::Usb, CodePage::Ascii)
        };
        assert!(!no_drawer.may_open_a_drawer());
    }

    #[test]
    fn vietnamese_goes_out_as_a_bitmap() {
        // The failure this prevents is the one pos-spec.md §13 names: diacritics printing as
        // garbage on a thermal printer with no Unicode font.
        let printer = capabilities(PrinterConnection::Usb, CodePage::Ascii);
        assert_eq!(printer.needs_bitmap("Total: 120,000"), Ok(false));
        assert_eq!(printer.needs_bitmap("Bún chả"), Ok(true));
        assert_eq!(printer.needs_bitmap("寿司"), Ok(true));
    }

    #[test]
    fn an_unknown_code_page_sends_everything_as_a_bitmap() {
        // Including plain ASCII. Conservative on purpose: a wrong `true` costs
        // milliseconds, a wrong `false` costs a receipt full of question marks.
        let printer = capabilities(PrinterConnection::Usb, CodePage::Unsupported);
        assert_eq!(printer.needs_bitmap("Total"), Ok(true));
    }

    #[test]
    fn a_line_that_needs_a_bitmap_on_a_text_only_printer_has_no_right_answer() {
        // So the caller decides, rather than the adapter silently substituting question
        // marks — which is what "print anyway" would mean.
        let printer = PrinterCapabilities {
            prints_bitmaps: false,
            ..capabilities(PrinterConnection::Usb, CodePage::Ascii)
        };
        assert_eq!(printer.needs_bitmap("Total"), Ok(false));
        assert_eq!(printer.needs_bitmap("Phở"), Err(()));
    }

    #[test]
    fn an_absent_sensor_is_not_a_fault() {
        // Many thermal models cannot report paper. Treating that as "no paper" would make
        // them permanently unusable.
        let unknown = PrinterStatus {
            online: true,
            has_paper: None,
            cover_closed: None,
            queue_depth: 0,
        };
        assert!(unknown.is_ready());
        assert_eq!(unknown.to_string(), "ready, 0 queued");

        let empty = PrinterStatus {
            has_paper: Some(false),
            ..unknown
        };
        assert!(!empty.is_ready());
        assert_eq!(empty.to_string(), "out of paper");

        let offline = PrinterStatus {
            online: false,
            ..unknown
        };
        assert_eq!(offline.to_string(), "offline");
    }

    #[test]
    fn a_rendered_job_survives_the_round_trip_through_json() {
        // The queue ADR-0112 parks a job on stores the rendered document as JSON, so a job that
        // does not come back byte-identical is a ticket that prints differently on the agent's
        // printer than it would have on the edge's.
        let job = PrintJob {
            job_id: pos_proto::ids::EventId::new(pos_proto::ulid::Ulid::from_u128(7)),
            store_id: pos_proto::ids::StoreId::new(pos_proto::ulid::Ulid::from_u128(3)),
            station_id: Some(pos_proto::ids::StationId::new(
                pos_proto::ulid::Ulid::from_u128(9),
            )),
            document: PrintDocument {
                blocks: vec![
                    PrintBlock::Text {
                        line: "Phở bò tái".to_owned(),
                        style: TextStyle {
                            emphasised: true,
                            double_size: false,
                            centred: true,
                        },
                    },
                    PrintBlock::Bitmap {
                        width: NonZeroU16::new(576).expect("positive"),
                        bits: vec![0x00, 0x0f, 0xf0, 0xff],
                    },
                    PrintBlock::Barcode {
                        value: "4901234567894".to_owned(),
                    },
                    PrintBlock::QrCode {
                        value: "https://example.test/i/7".to_owned(),
                    },
                    PrintBlock::Feed { lines: 3 },
                    PrintBlock::Cut,
                ],
            },
        };
        let json = serde_json::to_string(&job).expect("a rendered job serialises");
        assert!(
            json.contains("\"000ff0ff\""),
            "the raster goes as hex, not as an array of numbers: {json}"
        );
        let back: PrintJob = serde_json::from_str(&json).expect("and comes back");
        assert_eq!(back, job);
    }

    #[test]
    fn a_raster_that_is_not_whole_bytes_is_refused_rather_than_truncated() {
        // Half a row of pixels is not a partial success — it shears on the print head.
        let odd = r#"{"Bitmap":{"width":576,"bits":"000f0"}}"#;
        let error = serde_json::from_str::<PrintBlock>(odd).expect_err("an odd length is refused");
        assert!(error.to_string().contains("even number"), "{error}");

        let not_hex = r#"{"Bitmap":{"width":576,"bits":"00zz"}}"#;
        let error =
            serde_json::from_str::<PrintBlock>(not_hex).expect_err("a non-hex digit is refused");
        assert!(error.to_string().contains("must be hex"), "{error}");
    }
}
