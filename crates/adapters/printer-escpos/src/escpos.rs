// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The ESC/POS byte encoder.
//!
//! Turns a [`PrintDocument`] into the command bytes a thermal printer understands. This is the
//! vendor protocol and nothing else ([`architecture.md`](../../../docs/architecture.md) §6.1): the
//! decision of whether a line goes out as text or as a bitmap was already made by the framework
//! ([ADR-0026](../../../docs/adr/0026-port-shapes.md) §5), so a [`PrintBlock::Text`] is sent as text
//! and a [`PrintBlock::Bitmap`] as a raster image, here, without re-deciding.
//!
//! The exact bytes are validated against real hardware in P4's pilot (roadmap A5); the contract
//! suite checks the observable behaviour — one ticket per job, a drawer only over USB — by
//! recognising a document by its initialise prefix ([`INIT`]) and a drawer pulse by its command
//! ([`DRAWER_KICK`]).

use core::num::NonZeroU16;

use pos_ports::printer::{PrintBlock, PrintDocument, TextStyle};

/// `ESC @` — initialise. Every document begins with it, which is how a recorder tells a document
/// write from a drawer pulse.
pub const INIT: [u8; 2] = [0x1B, 0x40];

/// `ESC p 0 25 250` — pulse the drawer-kick pin (25 ms on, 250 ms off), the standard cash-drawer
/// command. A document never begins with this, so the two are distinguishable on the wire.
pub const DRAWER_KICK: [u8; 5] = [0x1B, 0x70, 0x00, 0x19, 0xFA];

/// Low byte of a 16-bit little-endian value.
fn lo(value: u16) -> u8 {
    u8::try_from(value & 0x00FF).unwrap_or(0)
}

/// High byte of a 16-bit little-endian value.
fn hi(value: u16) -> u8 {
    u8::try_from(value >> 8).unwrap_or(0)
}

/// Encodes a whole document, initialise command first.
#[must_use]
pub fn encode(document: &PrintDocument) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&INIT);
    for block in &document.blocks {
        encode_block(&mut out, block);
    }
    out
}

fn encode_block(out: &mut Vec<u8>, block: &PrintBlock) {
    match block {
        PrintBlock::Text { line, style } => encode_text(out, line, *style),
        PrintBlock::Bitmap { width, bits } => encode_bitmap(out, *width, bits),
        PrintBlock::Barcode { value } => encode_barcode(out, value),
        PrintBlock::QrCode { value } => encode_qr(out, value),
        PrintBlock::Feed { lines } => {
            // ESC d n — print and feed n lines.
            out.extend_from_slice(&[0x1B, 0x64, u8::try_from(*lines).unwrap_or(u8::MAX)]);
        }
        PrintBlock::Cut => {
            // GS V 0 — full cut.
            out.extend_from_slice(&[0x1D, 0x56, 0x00]);
        }
        // `PrintBlock` is `#[non_exhaustive]`. A block type added after this adapter was built is
        // omitted rather than turned into bytes that might mean something else on the wire; a
        // rebuild against the newer `pos-ports` adds real handling.
        _ => {}
    }
}

fn encode_text(out: &mut Vec<u8>, line: &str, style: TextStyle) {
    // ESC a n — alignment (0 left, 1 centre).
    out.extend_from_slice(&[0x1B, 0x61, u8::from(style.centred)]);
    // ESC E n — emphasis.
    out.extend_from_slice(&[0x1B, 0x45, u8::from(style.emphasised)]);
    // GS ! n — character size; 0x11 doubles width and height.
    out.extend_from_slice(&[0x1D, 0x21, if style.double_size { 0x11 } else { 0x00 }]);
    // The framework only sends Text for lines the code page covers, so the bytes go out as-is.
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
    // Reset the transient styles so they do not bleed into the next line.
    out.extend_from_slice(&[0x1B, 0x45, 0x00, 0x1D, 0x21, 0x00, 0x1B, 0x61, 0x00]);
}

fn encode_bitmap(out: &mut Vec<u8>, width: NonZeroU16, bits: &[u8]) {
    let width_bytes = usize::from(width.get()).div_ceil(8);
    let height = if width_bytes == 0 {
        0
    } else {
        u16::try_from(bits.len() / width_bytes).unwrap_or(u16::MAX)
    };
    let width_bytes_u16 = u16::try_from(width_bytes).unwrap_or(u16::MAX);
    // GS v 0 m xL xH yL yH [data] — raster bit image, m=0 (normal).
    out.extend_from_slice(&[
        0x1D,
        0x76,
        0x30,
        0x00,
        lo(width_bytes_u16),
        hi(width_bytes_u16),
        lo(height),
        hi(height),
    ]);
    out.extend_from_slice(bits);
}

fn encode_barcode(out: &mut Vec<u8>, value: &str) {
    // GS k 4 <data> NUL — CODE39, NUL-terminated. A conservative default; the fleet's barcode
    // symbology is configuration the pilot pins (roadmap A5).
    out.extend_from_slice(&[0x1D, 0x6B, 0x04]);
    out.extend_from_slice(value.as_bytes());
    out.push(0x00);
}

fn encode_qr(out: &mut Vec<u8>, value: &str) {
    let data = value.as_bytes();
    // GS ( k — model 2, module size 8, error-correction level M, then store and print.
    out.extend_from_slice(&[0x1D, 0x28, 0x6B, 0x04, 0x00, 0x31, 0x41, 0x32, 0x00]);
    out.extend_from_slice(&[0x1D, 0x28, 0x6B, 0x03, 0x00, 0x31, 0x43, 0x08]);
    out.extend_from_slice(&[0x1D, 0x28, 0x6B, 0x03, 0x00, 0x31, 0x45, 0x31]);
    // Store: the length covers the three header bytes (49, 80, 48) plus the data.
    let length = u16::try_from(data.len().saturating_add(3)).unwrap_or(u16::MAX);
    out.extend_from_slice(&[0x1D, 0x28, 0x6B, lo(length), hi(length), 0x31, 0x50, 0x30]);
    out.extend_from_slice(data);
    // Print.
    out.extend_from_slice(&[0x1D, 0x28, 0x6B, 0x03, 0x00, 0x31, 0x51, 0x30]);
}

#[cfg(test)]
mod tests {
    use super::{DRAWER_KICK, INIT, encode};
    use core::num::NonZeroU16;
    use pos_ports::printer::{PrintBlock, PrintDocument, TextStyle};

    #[test]
    fn a_document_starts_with_the_initialise_command() {
        let document = PrintDocument {
            blocks: vec![PrintBlock::Text {
                line: "TOTAL".to_owned(),
                style: TextStyle::default(),
            }],
        };
        let bytes = encode(&document);
        assert!(bytes.starts_with(&INIT));
        assert!(!bytes.starts_with(&DRAWER_KICK));
    }

    #[test]
    fn a_cut_emits_the_cut_command() {
        let bytes = encode(&PrintDocument {
            blocks: vec![PrintBlock::Cut],
        });
        // GS V 0 appears after the INIT prefix.
        assert!(bytes.windows(3).any(|window| window == [0x1D, 0x56, 0x00]));
    }

    #[test]
    fn a_bitmap_carries_its_dimensions() {
        // 16 px wide (2 bytes) × 2 rows.
        let bits = vec![0xFF, 0x00, 0x0F, 0xF0];
        let bytes = encode(&PrintDocument {
            blocks: vec![PrintBlock::Bitmap {
                width: NonZeroU16::new(16).expect("positive"),
                bits: bits.clone(),
            }],
        });
        // GS v 0 0 xL xH yL yH: width 2 bytes, height 2 rows.
        let header = [0x1D, 0x76, 0x30, 0x00, 0x02, 0x00, 0x02, 0x00];
        assert!(bytes.windows(header.len()).any(|window| window == header));
        assert!(bytes.ends_with(&bits));
    }
}
