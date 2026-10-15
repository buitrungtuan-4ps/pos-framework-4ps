// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The kitchen's paper: one ticket per fired line, with its note, at its station.

use pos_ports::printer::{PrintBlock, PrintDocument, TextStyle};
use pos_proto::ids::StationId;
use pos_proto::quantity::Quantity;

use crate::app::{EdgeSession, FiredLine};
use crate::line_notes::NoteText;
use crate::paper_labels::PaperLabels;
use crate::printing::language::{item_name, ticket_language};

/// What a kitchen ticket says about a line's guest note
/// ([ADR-0157](../../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TicketNote {
    /// No note was written.
    #[default]
    None,
    /// The note, as the server or the guest wrote it.
    Text(String),
    /// A note was written and the edge no longer holds it — it restarted since. The ticket says so,
    /// because a cook who reads nothing makes the dish as the menu describes it.
    Lost,
}

/// One line of a kitchen ticket: what to make, how many, and what was changed about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketLine {
    /// The item's name as the store's published menu spells it.
    pub item: String,
    /// How many, already rendered — [`Quantity`](pos_proto::quantity::Quantity) supports halves for a
    /// split item, and the kitchen reads the same figure the till showed.
    pub quantity: String,
    /// The chosen modifiers, in the order they were added.
    pub modifiers: Vec<String>,
    /// The guest's note, printed last and emphasised.
    pub note: TicketNote,
}

/// The ticket a station's printer produces when a line fires.
///
/// `reference` is what the kitchen calls back across the pass — a table's name, or the tail of the
/// order's id when there is no table. Double-size, because it is read from a metre away over a hot
/// counter; the item is emphasised; modifiers follow indented so a cook scanning the ticket sees
/// "what" before "how". A guest note comes last and emphasised, after a `!`, because it is the part
/// of a ticket that is most often about somebody's health.
#[must_use]
pub fn ticket_document(reference: &str, line: &TicketLine, labels: &PaperLabels) -> PrintDocument {
    let mut blocks = vec![
        PrintBlock::Text {
            line: reference.to_owned(),
            style: TextStyle {
                emphasised: true,
                double_size: true,
                centred: false,
            },
        },
        PrintBlock::Text {
            line: format!("{} x {}", line.quantity, line.item),
            style: TextStyle {
                emphasised: true,
                double_size: false,
                centred: false,
            },
        },
    ];
    blocks.extend(line.modifiers.iter().map(|modifier| PrintBlock::Text {
        line: format!("  + {modifier}"),
        style: TextStyle::default(),
    }));
    let note = match &line.note {
        TicketNote::None => None,
        TicketNote::Text(note) => Some(note.as_str()),
        TicketNote::Lost => Some(labels.note_lost),
    };
    if let Some(note) = note {
        blocks.push(PrintBlock::Text {
            line: format!("  ! {note}"),
            style: TextStyle {
                emphasised: true,
                double_size: false,
                centred: false,
            },
        });
    }
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

/// The ticket a fired line prints, with every id resolved against the store's published menu.
///
/// An item the menu does not name falls back to its identifier rather than to a blank: a cook can
/// still match a ticket to a screen, and a silently empty line is how the wrong dish gets made. The
/// same rule applies to each modifier.
///
/// `language` is the ticket's ([`ticket_language`]). In a language other than the display language
/// each item and modifier prints the menu's translation for it, and the name the menu publishes
/// where it has none; in the display language, or with none, the names are the ones every ticket
/// printed before a station could choose.
///
/// `note` is the guest note the edge holds for the line, if it still holds one (ADR-0157).
#[must_use]
pub fn ticket_line(
    session: &EdgeSession,
    fired: &FiredLine,
    note: Option<&NoteText>,
    language: Option<&str>,
) -> TicketLine {
    TicketLine {
        item: item_name(session, fired.menu_item_id, language),
        quantity: format_quantity(fired.quantity),
        modifiers: fired
            .modifier_menu_item_ids
            .iter()
            .map(|modifier| item_name(session, *modifier, language))
            .collect(),
        note: match note {
            Some(note) => TicketNote::Text(note.as_str().to_owned()),
            None if fired.note_present => TicketNote::Lost,
            None => TicketNote::None,
        },
    }
}

/// The ticket `fired` prints at `printing`, the station whose printer prints it
/// ([`station_printer`]): its labels and its names in that station's language ([`ticket_language`],
/// ADR-0160 decision 2).
///
/// The labels fall back to English on a box that cannot draw the language's, as all of a store's
/// paper does. A station that sets no language prints in the display language, so its ticket is the
/// one every station printed before a station could choose.
#[must_use]
pub fn ticket_at(
    session: &EdgeSession,
    can_rasterise: bool,
    printing: StationId,
    reference: &str,
    fired: &FiredLine,
    note: Option<&NoteText>,
) -> PrintDocument {
    let language = ticket_language(session, printing);
    ticket_document(
        reference,
        &ticket_line(session, fired, note, language.as_deref()),
        PaperLabels::for_store(language.as_deref(), can_rasterise),
    )
}

/// Renders a quantity the way a ticket reads it: `2`, or `0.5` for one half of a split item.
///
/// [`Quantity`] counts thousandths, so the raw figure is `2000`. A kitchen ticket saying "2000 x
/// Margherita" is not a rounding bug an operator would ever guess at, which is why this exists
/// rather than a `Display` on the wire type — the wire keeps its integer.
pub(super) fn format_quantity(quantity: Quantity) -> String {
    let milli = quantity.as_milli();
    let sign = if milli < 0 { "-" } else { "" };
    let magnitude = milli.unsigned_abs();
    let scale = Quantity::SCALE.unsigned_abs();
    let whole = magnitude / scale;
    let fraction = magnitude % scale;
    if fraction == 0 {
        format!("{sign}{whole}")
    } else {
        let decimals = format!("{fraction:03}");
        format!("{sign}{whole}.{}", decimals.trim_end_matches('0'))
    }
}
