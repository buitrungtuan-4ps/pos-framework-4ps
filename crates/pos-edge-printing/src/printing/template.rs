// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A receipt template as paper prints it
//! ([ADR-0207](../../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)):
//! its header and footer keys and its custom fields' labels in the paper's language from the
//! tenant's grid, falling back to English, and the sections it leaves out.
//!
//! The legal body is not here: the seller, the receipt number, the lines, the totals, the tax lines
//! and a captured buyer always print. The receipt prints no payments and no change line, so a
//! template that turns those off changes nothing.

use std::borrow::Cow;

use pos_core::decision::Actor;
use pos_edge_app::app::{
    BuyerDetails, Edge, EdgeSession, PersonalOnPaper, PrintedField, PrintedObjects, PrintedValue,
    ReceiptCopy, ReceiptLine,
};
use pos_edge_app::labels::LabelGrid;
use pos_ports::event_store::EventStore;
use pos_ports::printer::{PrintBlock, PrintDocument, TextStyle};
use pos_ports::subject_store::SubjectStore;
use pos_proto::ids::BillId;
use pos_proto::receipt::{PersonCopy, ReceiptSection, ReceiptTemplate};

use crate::paper_labels::PaperLabels;
use crate::printing::documents::centred;
use crate::printing::tickets::format_quantity;
use crate::printing::{PrintOutcome, Printers, TillPrinting};

/// What a template adds to a document and leaves out of it, resolved for one piece of paper.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplatePrint {
    /// The lines above the legal body, each a header key's text.
    pub header: Vec<String>,
    /// The custom fields under the receipt's heading: each label and its value, or no value for a
    /// personal one this paper does not print.
    pub fields: Vec<(String, Option<String>)>,
    /// The lines at the foot, each a footer key's text.
    pub footer: Vec<String>,
    /// Whether the item rows' modifiers are left out.
    pub without_modifiers: bool,
    /// Whether the second language is left out.
    pub without_second_language: bool,
}

impl TemplatePrint {
    /// `template` for paper whose labels are `words`, in their language (English on a box with no
    /// fonts, so nothing the template adds is a line the printer cannot draw), with `fields` read
    /// for the document and `grid` the tenant's. A key the grid has no text for in that language or
    /// in English is left out, as is a field whose label has none: a template never stops a
    /// document printing.
    #[must_use]
    pub fn of(
        template: &ReceiptTemplate,
        grid: &LabelGrid,
        words: &PaperLabels,
        fields: &[PrintedField],
    ) -> Self {
        let text = |key: &str| grid.label(key, words.language).map(str::to_owned);
        Self {
            header: template
                .header_keys
                .iter()
                .filter_map(|key| text(key.as_str()))
                .collect(),
            fields: fields
                .iter()
                .filter_map(|field| {
                    let label = text(field.label_key.as_str())?;
                    let value = match &field.value {
                        PrintedValue::Withheld => None,
                        PrintedValue::Integer(number) => Some(number.to_string()),
                        PrintedValue::Boolean(yes) => {
                            Some((if *yes { words.yes } else { words.no }).to_owned())
                        }
                        PrintedValue::Entry(key) => Some(text(key.as_str())?),
                        PrintedValue::Date(date) => Some(date.to_string()),
                        PrintedValue::Quantity(quantity, unit) => Some(
                            [
                                Some(format_quantity(*quantity)),
                                unit.as_ref().and_then(|unit| text(unit.as_str())),
                            ]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" "),
                        ),
                        PrintedValue::Personal(value) => Some(value.clone()),
                    };
                    Some((label, value))
                })
                .collect(),
            footer: template
                .footer_keys
                .iter()
                .filter_map(|key| text(key.as_str()))
                .collect(),
            without_modifiers: !template.sections.is_on(ReceiptSection::Modifiers),
            without_second_language: !template.sections.is_on(ReceiptSection::SecondLanguage),
        }
    }

    /// The rows a document prints: without their modifiers where `template` leaves them out.
    pub(super) fn rows<'a>(
        template: Option<&Self>,
        lines: Cow<'a, [ReceiptLine]>,
    ) -> Cow<'a, [ReceiptLine]> {
        if !template.is_some_and(|template| template.without_modifiers) {
            return lines;
        }
        let mut lines = lines.into_owned();
        for line in &mut lines {
            line.modifier_display_names.clear();
        }
        Cow::Owned(lines)
    }

    /// Whether a document prints its second language: unless `template` leaves it out.
    pub(super) fn second_language(template: Option<&Self>) -> bool {
        !template.is_some_and(|template| template.without_second_language)
    }

    /// `document` with `template`'s header lines and fields above it and its footer lines at the
    /// foot, before the cut. No template, the document as it was.
    pub(super) fn applied(template: Option<&Self>, document: PrintDocument) -> PrintDocument {
        let Some(template) = template else {
            return document;
        };
        let mut body = document.blocks;
        let cut = matches!(body.last(), Some(PrintBlock::Cut))
            .then(|| body.pop())
            .flatten();
        let blocks = template
            .header_blocks()
            .chain(template.field_blocks())
            .chain(body)
            .chain(template.footer_blocks())
            .chain(cut)
            .collect();
        PrintDocument { blocks }
    }

    /// The header lines, centred.
    fn header_blocks(&self) -> impl Iterator<Item = PrintBlock> + '_ {
        self.header.iter().map(|line| centred(line.clone(), false))
    }

    /// The fields, each `label: value`, or the label alone.
    fn field_blocks(&self) -> impl Iterator<Item = PrintBlock> + '_ {
        self.fields.iter().map(|(label, value)| PrintBlock::Text {
            line: match value {
                Some(value) => format!("{label}: {value}"),
                None => format!("{label}:"),
            },
            style: TextStyle::default(),
        })
    }

    /// The footer lines, centred.
    fn footer_blocks(&self) -> impl Iterator<Item = PrintBlock> + '_ {
        self.footer.iter().map(|line| centred(line.clone(), false))
    }
}

impl Printers {
    /// The template a document about `objects` prints with at `till`, resolved: the template of its
    /// order's channel, then the base (ADR-0207 decision 2), or `None` to print as before.
    ///
    /// `copy` names which person's copy this document would be and how it is printed; a personal
    /// value prints only when the template marks that document as the person's copy (decision 5),
    /// and otherwise its label prints alone.
    pub async fn template_for<S>(
        &self,
        edge: &Edge<S>,
        session: &EdgeSession,
        till: &TillPrinting,
        objects: PrintedObjects,
        copy: Option<(PersonCopy, PersonalOnPaper)>,
    ) -> Option<TemplatePrint>
    where
        S: EventStore + SubjectStore,
    {
        let template = session
            .receipt
            .template_for(edge.printed_channel(objects))?;
        let personal = copy
            .filter(|(document, _)| template.is_person_copy(*document))
            .map_or(PersonalOnPaper::Withheld, |(_, personal)| personal);
        let fields = edge
            .printed_fields(&template.custom_field_ids, objects, personal)
            .await;
        Some(TemplatePrint::of(
            template,
            &edge.label_grid(),
            self.receipt_labels_for(session, till),
            &fields,
        ))
    }

    /// [`Self::template_for`] a settled bill's receipt (`reprinted_by` `None`) or its copy: a
    /// receipt with a captured buyer is the buyer's copy of a tax invoice, which the template may
    /// mark as the person's own; a reprint prints a personal value only for someone holding
    /// `data.personal.read` (ADR-0207 decision 5).
    pub async fn bill_template<S>(
        &self,
        edge: &Edge<S>,
        (session, till): (&EdgeSession, &TillPrinting),
        bill_id: BillId,
        has_buyer: bool,
        reprinted_by: Option<Actor>,
    ) -> Option<TemplatePrint>
    where
        S: EventStore + SubjectStore,
    {
        let personal = reprinted_by.map_or(PersonalOnPaper::Shown, PersonalOnPaper::Reprinted);
        self.template_for(
            edge,
            session,
            till,
            PrintedObjects::Bill(bill_id),
            has_buyer.then_some((PersonCopy::TaxInvoiceBuyer, personal)),
        )
        .await
    }

    /// Prints `copy` at the printer of the till `actor` asked at, with [`Self::bill_template`]
    /// for a reprint by `actor`.
    pub async fn print_bill_copy<S>(
        &self,
        edge: &Edge<S>,
        actor: Actor,
        copy: &ReceiptCopy,
        buyer: Option<&BuyerDetails>,
    ) -> PrintOutcome
    where
        S: EventStore + SubjectStore,
    {
        let session = edge.session();
        let till = self.till_for(&session, actor.device_id).await;
        let template = self
            .bill_template(
                edge,
                (&session, &till),
                copy.bill_id,
                buyer.is_some(),
                Some(actor),
            )
            .await;
        self.print_receipt_copy(
            &session,
            &till,
            edge.store_id(),
            copy,
            buyer,
            template.as_ref(),
        )
        .await
    }
}
