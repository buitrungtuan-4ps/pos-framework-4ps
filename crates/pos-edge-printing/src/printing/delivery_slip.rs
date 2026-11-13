// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A delivery order's slip: the one paper its recipient's name, phone number and address print on
//! ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 3).
//!
//! It prints once, by itself, when the store takes the order: as it opens, or when a member of
//! staff confirms it where its vendor's orders wait for one. It goes to the store's receipt
//! printer, travels with the order to the person it belongs to, and is never a kitchen ticket, a
//! pre-bill or a receipt. Its job id is the order's own, so taking the order again prints nothing
//! new. Nothing reprints it.

use pos_edge_app::app::Edge;
use pos_ports::event_store::EventStore;
use pos_ports::printer::{PrintBlock, PrintDocument, PrintJob, TextStyle};
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_proto::ids::{EventId, OrderId};
use pos_proto::store_profile::StoreProfile;

use crate::paper_labels::PaperLabels;
use crate::printing::blocks::short_reference;
use crate::printing::devices::receipt_printer;
use crate::printing::documents::{centred, seller_blocks};
use crate::printing::{PrintOutcome, Printers};

/// The record's fields a slip prints, in order.
const PRINTED_FIELDS: [&str; 3] = ["name", "phone", "address"];

/// A delivery slip: the seller, the heading, the order's short reference, who it goes to and when
/// it was printed.
#[must_use]
pub fn delivery_slip_document(
    profile: &StoreProfile,
    labels: &PaperLabels,
    reference: &str,
    recipient: &SubjectRecord,
    printed_at: &str,
) -> PrintDocument {
    let text = |line: String| PrintBlock::Text {
        line,
        style: TextStyle::default(),
    };
    let mut blocks = seller_blocks(profile);
    blocks.push(centred(labels.delivery, true));
    blocks.push(centred(format!("#{reference}"), true));
    blocks.push(text(format!("{}:", labels.deliver_to)));
    for field in PRINTED_FIELDS {
        let Some(value) = recipient.fields.get(field) else {
            continue;
        };
        blocks.push(text(if field == "phone" {
            format!("{}: {value}", labels.phone)
        } else {
            value.clone()
        }));
    }
    blocks.push(text(format!("{}: {printed_at}", labels.time)));
    blocks.push(PrintBlock::Cut);
    PrintDocument { blocks }
}

impl Printers {
    /// Prints `order_id`'s delivery slip on the store's receipt printer, when the order names a
    /// recipient this store still holds. `None` when there is nothing to print: no recipient, or
    /// one already masked. Never an error otherwise: the outcome says what came of the paper, and
    /// the order is taken whether or not it printed.
    pub async fn print_delivery_slip<S>(
        &self,
        edge: &Edge<S>,
        order_id: OrderId,
        printed_at: &str,
    ) -> Option<PrintOutcome>
    where
        S: EventStore + SubjectStore,
    {
        let recipient = match edge.delivery_recipient(order_id).await {
            Ok(recipient) => recipient?,
            Err(error) => {
                tracing::warn!(%order_id, %error, "a delivery slip's recipient could not be read");
                return Some(PrintOutcome::Unavailable);
            }
        };
        let session = edge.session();
        let Some(device) = receipt_printer(&session.devices) else {
            return Some(PrintOutcome::NoPrinter);
        };
        let document = delivery_slip_document(
            &session.profile,
            self.labels_for(&session),
            &short_reference(&order_id.to_string()),
            &recipient,
            printed_at,
        );
        Some(
            self.dispatch(
                &session,
                device,
                PrintJob {
                    // The order's own id: the order is taken once, and so is its slip.
                    job_id: EventId::new(order_id.as_ulid()),
                    store_id: edge.store_id(),
                    station_id: None,
                    document,
                },
            )
            .await,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pos_ports::printer::PrintBlock;
    use pos_ports::subject_store::SubjectRecord;
    use pos_proto::store_profile::StoreProfile;
    use pos_proto::time::Timestamp;

    use super::delivery_slip_document;
    use crate::paper_labels::ENGLISH;

    #[test]
    fn a_delivery_slip_prints_who_it_goes_to_under_its_heading() {
        let fields = BTreeMap::from([
            ("name".to_owned(), "Test Guest".to_owned()),
            ("phone".to_owned(), "000-0000".to_owned()),
            ("address".to_owned(), "1 Example Street".to_owned()),
        ]);
        let recipient = SubjectRecord::new(
            Timestamp::from_milliseconds_since_epoch(0).expect("an instant"),
            fields,
        );
        let document = delivery_slip_document(
            &StoreProfile::default(),
            &ENGLISH,
            "ABC123",
            &recipient,
            "12:00",
        );
        let lines: Vec<&str> = document
            .blocks
            .iter()
            .filter_map(|block| match block {
                PrintBlock::Text { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                "DELIVERY",
                "#ABC123",
                "Deliver to:",
                "Test Guest",
                "Phone: 000-0000",
                "1 Example Street",
                "Time: 12:00",
            ]
        );
        assert!(matches!(document.blocks.last(), Some(PrintBlock::Cut)));
    }
}
