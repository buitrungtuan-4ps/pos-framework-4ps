// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A receipt template on paper (ADR-0207): its lines and fields from the grid in the paper's
//! language, where they sit, and the sections it leaves out.

use std::borrow::Cow;
use std::collections::BTreeMap;

use pos_edge_app::app::{PrintedField, PrintedValue};
use pos_edge_app::labels::LabelGrid;
use pos_ports::printer::{PrintBlock, PrintDocument, TextStyle};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::quantity::Quantity;
use pos_proto::receipt::ReceiptTemplate;
use pos_proto::text::{DisplayName, TranslationKey};

use crate::paper_labels::PaperLabels;
use crate::printing::template::TemplatePrint;

fn grid() -> LabelGrid {
    let entries: BTreeMap<String, BTreeMap<String, String>> = [
        (
            "receipt.header.welcome",
            vec![("en", "Welcome"), ("vi", "Xin chào")],
        ),
        ("receipt.footer.thanks", vec![("en", "Thank you")]),
        (
            "data.order.covers",
            vec![("en", "Covers"), ("vi", "Số khách")],
        ),
        ("data.reservation.contact", vec![("en", "Contact")]),
        ("data.reservation.vip", vec![("en", "VIP")]),
        ("data.unit.kg", vec![("en", "kg")]),
    ]
    .into_iter()
    .map(|(key, texts)| {
        let texts = texts
            .into_iter()
            .map(|(locale, text)| (locale.to_owned(), text.to_owned()))
            .collect();
        (key.to_owned(), texts)
    })
    .collect();
    LabelGrid::bounded(entries).expect("a small grid")
}

fn template(value: serde_json::Value) -> ReceiptTemplate {
    serde_json::from_value(value).expect("a template")
}

fn field(label: &str, value: PrintedValue) -> PrintedField {
    PrintedField {
        label_key: TranslationKey::new(label),
        value,
    }
}

fn vietnamese() -> &'static PaperLabels {
    PaperLabels::in_language("vi").expect("Vietnamese labels")
}

#[test]
fn keys_and_labels_print_in_the_papers_language_then_english_and_a_missing_one_is_left_out() {
    let print = TemplatePrint::of(
        &template(serde_json::json!({
            "header_keys": ["receipt.header.welcome", "receipt.header.gone"],
            "footer_keys": ["receipt.footer.thanks"]
        })),
        &grid(),
        vietnamese(),
        &[
            field("data.order.covers", PrintedValue::Integer(3)),
            field("data.reservation.contact", PrintedValue::Withheld),
            field("data.reservation.vip", PrintedValue::Boolean(true)),
            field(
                "data.order.weight",
                PrintedValue::Quantity(Quantity::from_milli(1500), None),
            ),
            field(
                "data.order.covers",
                PrintedValue::Quantity(
                    Quantity::from_milli(1500),
                    Some(TranslationKey::new("data.unit.kg")),
                ),
            ),
        ],
    );
    assert_eq!(print.header, vec!["Xin chào".to_owned()]);
    assert_eq!(
        print.footer,
        vec!["Thank you".to_owned()],
        "English stands in"
    );
    assert_eq!(
        print.fields,
        vec![
            ("Số khách".to_owned(), Some("3".to_owned())),
            ("Contact".to_owned(), None),
            ("VIP".to_owned(), Some("Có".to_owned())),
            ("Số khách".to_owned(), Some("1.5 kg".to_owned())),
        ],
        "a personal value withheld prints its label alone; a field with no label is left out"
    );
    assert!(!print.without_modifiers && !print.without_second_language);
}

#[test]
fn the_template_frames_the_document_and_leaves_the_legal_body_alone() {
    let print = TemplatePrint {
        header: vec!["Welcome".to_owned()],
        fields: vec![("Contact".to_owned(), None)],
        footer: vec!["Thank you".to_owned()],
        ..TemplatePrint::default()
    };
    let body = PrintBlock::Text {
        line: "#42".to_owned(),
        style: TextStyle::default(),
    };
    let document = PrintDocument {
        blocks: vec![body.clone(), PrintBlock::Cut],
    };
    let framed = TemplatePrint::applied(Some(&print), document.clone());
    let lines: Vec<String> = framed
        .blocks
        .iter()
        .map(|block| match block {
            PrintBlock::Text { line, .. } => line.clone(),
            PrintBlock::Cut => "<cut>".to_owned(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(lines, ["Welcome", "Contact:", "#42", "Thank you", "<cut>"]);
    assert_eq!(
        TemplatePrint::applied(None, document.clone()),
        document,
        "no template, no change"
    );
}

#[test]
fn a_template_may_leave_out_the_modifiers_and_the_second_language() {
    let print = TemplatePrint::of(
        &template(serde_json::json!({
            "sections": { "modifiers": false, "second_language": false, "payments": false }
        })),
        &grid(),
        vietnamese(),
        &[],
    );
    assert!(print.without_modifiers && print.without_second_language);
    assert!(!TemplatePrint::second_language(Some(&print)));
    assert!(TemplatePrint::second_language(None));

    let mut line = super::receipts::sold(
        "Pho",
        1000,
        Money::new(CurrencyCode::VND, 50_000),
        Money::new(CurrencyCode::VND, 50_000),
    );
    line.modifier_display_names = vec![DisplayName::new("Extra herbs")];
    let rows = TemplatePrint::rows(Some(&print), Cow::Owned(vec![line.clone()]));
    assert!(rows.iter().all(|row| row.modifier_display_names.is_empty()));
    let kept = TemplatePrint::rows(None, Cow::Owned(vec![line.clone()]));
    assert_eq!(kept.first(), Some(&line));
}
