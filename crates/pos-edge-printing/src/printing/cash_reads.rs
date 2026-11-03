// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cash papers that close nothing: the handover slip
//! ([ADR-0187](../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
//! decision 3).

use pos_ports::printer::{PrintBlock, PrintDocument};
use pos_proto::store_profile::StoreProfile;

use crate::app::HandoverSlip;
use crate::paper_labels::PaperLabels;
use crate::printing::blocks::{MoneyStyle, amount_line, short_reference};
use crate::printing::documents::{centred, count_blocks, verdict};

/// A handover on paper: the store and the till, the time, what the outgoing shift counted out with
/// its notes, what the incoming shift counted in with its notes, and the difference with its verdict
/// in words. Names no person, as the void slip names none (ADR-0184, amendment).
#[must_use]
pub fn handover_slip_document(
    profile: &StoreProfile,
    money: &MoneyStyle,
    labels: &PaperLabels,
    slip: &HandoverSlip,
) -> PrintDocument {
    let mut blocks = Vec::new();
    if let Some(name) = profile.display_name() {
        blocks.push(centred(name, true));
    }
    blocks.push(centred(labels.handover, true));
    if let Some(till) = &slip.till_name {
        blocks.push(centred(till.as_str(), true));
    }
    if let Some(at) = &slip.handed_over_at {
        blocks.push(centred(format!("{}: {at}", labels.time), false));
    }
    blocks.push(centred(
        short_reference(&slip.outgoing_shift_id.to_string()),
        false,
    ));
    blocks.push(amount_line(labels.counted_out, slip.outgoing_count, money));
    count_blocks(&mut blocks, money, labels, &slip.outgoing_breakdown);
    blocks.push(centred(
        short_reference(&slip.incoming_shift_id.to_string()),
        false,
    ));
    blocks.push(amount_line(labels.counted_in, slip.incoming_count, money));
    count_blocks(&mut blocks, money, labels, &slip.incoming_breakdown);
    blocks.push(amount_line(
        labels.handover_difference,
        slip.difference,
        money,
    ));
    blocks.push(centred(verdict(labels, slip.difference), true));
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}
