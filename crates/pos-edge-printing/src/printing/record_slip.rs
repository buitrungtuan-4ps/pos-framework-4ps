// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A custom record's slip on paper
//! ([ADR-0207](../../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)
//! decisions 4 and 5): what the store's receipt template prints for a record, framed by the seller,
//! the record's type, its short reference and the time.
//!
//! A record has no sales channel, so its slip takes the base template. A store with no template
//! prints no slip: nothing printed one before, and the template is what says which fields it holds.

use pos_core::decision::Actor;
use pos_edge_app::app::{Edge, PrintedObjects, RecordSlipCopy};
use pos_ports::event_store::EventStore;
use pos_ports::printer::{PrintBlock, PrintDocument, PrintJob, TextStyle};
use pos_ports::subject_store::SubjectStore;
use pos_proto::ids::CustomRecordId;
use pos_proto::receipt::PersonCopy;
use pos_proto::store_profile::StoreProfile;

use crate::paper_labels::PaperLabels;
use crate::printing::blocks::short_reference;
use crate::printing::devices::guest_printer;
use crate::printing::documents::{centred, seller_blocks};
use crate::printing::template::TemplatePrint;
use crate::printing::{PrintOutcome, Printers};

/// The body of a record's slip: the seller, the record's type as its title where it has one, its
/// short reference, and when it was printed. The template's lines and fields frame it.
#[must_use]
pub fn record_slip_document(
    profile: &StoreProfile,
    labels: &PaperLabels,
    title: Option<&str>,
    reference: &str,
    printed_at: &str,
) -> PrintDocument {
    let mut blocks = seller_blocks(profile);
    if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
        blocks.push(centred(title, true));
    }
    blocks.push(centred(format!("#{reference}"), true));
    blocks.push(PrintBlock::Text {
        line: format!("{}: {printed_at}", labels.time),
        style: TextStyle::default(),
    });
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

impl Printers {
    /// Prints `custom_record_id`'s slip, as `slip` decides it for `actor`, on the receipt printer of
    /// the till `actor` asks at: the paper handed to the person the record is about. A personal
    /// value prints only where the template marks the slip as the person's copy (ADR-0207 decision
    /// 5), and as `slip` allows.
    ///
    /// `None` when the store has no receipt template, so there is no slip to print. Never an error
    /// otherwise: the outcome says what came of the paper.
    pub async fn print_record_slip<S>(
        &self,
        edge: &Edge<S>,
        actor: Actor,
        custom_record_id: CustomRecordId,
        slip: &RecordSlipCopy,
        printed_at: &str,
    ) -> Option<PrintOutcome>
    where
        S: EventStore + SubjectStore,
    {
        let session = edge.session();
        let till = self.till_for(&session, actor.device_id).await;
        let template = self
            .template_for(
                edge,
                &session,
                &till,
                PrintedObjects::Record(custom_record_id),
                Some((PersonCopy::RecordSlip, slip.personal)),
            )
            .await?;
        let device = match guest_printer(&session.devices, &till) {
            Ok(device) => device,
            Err(outcome) => return Some(outcome),
        };
        let words = self.receipt_labels_for(&session, &till);
        let grid = edge.label_grid();
        let title = slip
            .object_label_key
            .as_ref()
            .and_then(|key| grid.label(key.as_str(), words.language));
        let document = record_slip_document(
            &session.profile,
            words,
            title,
            &short_reference(&custom_record_id.to_string()),
            printed_at,
        );
        let document = TemplatePrint::applied(Some(&template), document);
        Some(
            self.dispatch(
                &session,
                device,
                PrintJob {
                    // A job of its own each time: a slip asked for twice prints twice.
                    job_id: edge.print_job_id(),
                    store_id: edge.store_id(),
                    station_id: None,
                    document,
                },
            )
            .await,
        )
    }
}
