use pos_proto::locale::CountryCode;
use serde_json::{Value, json};

use super::{ANY_COUNTRY, Directory, MAX_DIRECTORY_BYTES};
use crate::cell::Cell;

/// This cell: `vn1` at its own address.
pub(crate) fn vn1() -> Cell {
    Cell {
        cell_id: "vn1".to_owned(),
        country_alias: Some("vn".to_owned()),
        home_country: Some(CountryCode::VN),
        base_url: Some("https://vn1.example.com".to_owned()),
        runs_in: Some(CountryCode::VN),
        configured: true,
    }
}

/// A two-cell directory at `version`.
pub(crate) fn document(version: u64) -> Value {
    json!({
        "directory_version": version,
        "cells": { "vn1": "https://vn1.example.com", "jp1": "https://jp1.example.com" },
        "countries": { "VN": "vn1", "JP": "jp1", "*": "vn1" },
        "tenants": { "pho-hanoi": "vn1", "ramen-osaka": "jp1" },
    })
}

fn parse(document: &Value) -> Result<Directory, String> {
    Directory::parse(document.to_string().as_bytes(), &vn1())
}

fn refusal(document: &Value) -> String {
    parse(document).expect_err("the document is refused")
}

#[test]
fn a_well_formed_directory_answers_its_three_maps() {
    let directory = parse(&document(7)).expect("parses");
    assert_eq!(directory.directory_version, 7);
    assert_eq!(directory.tenant_cell("ramen-osaka"), Some("jp1"));
    assert_eq!(directory.tenant_cell("pho-hanoi"), Some("vn1"));
    assert_eq!(directory.tenant_cell("unknown"), None);
    assert_eq!(directory.country_cell(CountryCode::JP), Some("jp1"));
    assert_eq!(
        directory.country_cell(CountryCode::IN),
        Some("vn1"),
        "a country with no entry falls back to *"
    );
    assert_eq!(directory.cell_url("jp1"), Some("https://jp1.example.com"));
    assert_eq!(directory.cell_url("sg1"), None);
}

#[test]
fn unknown_fields_are_ignored_and_tenants_may_be_absent() {
    let mut doc = document(1);
    doc["signed_by"] = json!("someone");
    doc.as_object_mut().map(|object| object.remove("tenants"));
    let directory = parse(&doc).expect("a newer field does not stop an older cell");
    assert_eq!(directory.tenant_cell("pho-hanoi"), None);
}

#[test]
fn malformed_documents_are_refused() {
    assert!(Directory::parse(b"not json", &vn1()).is_err());
    let mut doc = document(1);
    doc.as_object_mut().map(|object| object.remove("cells"));
    assert!(refusal(&doc).contains("not a directory document"));
}

#[test]
fn every_cell_is_checked() {
    let mut doc = document(1);
    doc["cells"]["VN2"] = json!("https://vn2.example.com");
    assert!(refusal(&doc).contains("cell_id"));
    let mut doc = document(1);
    doc["cells"]["vn2"] = json!("http://vn2.example.com");
    assert!(refusal(&doc).contains("base_url"));
    let mut doc = document(1);
    doc["cells"]["vn2"] = json!("https://vn2.example.com/");
    assert!(
        refusal(&doc).contains("must be written as https://vn2.example.com"),
        "one spelling per URL, so two cells cannot disagree about one"
    );
}

#[test]
fn every_country_key_is_an_upper_case_code_or_star() {
    for key in ["vn", "VNM", "V1", "**"] {
        let mut doc = document(1);
        doc["countries"][key] = json!("vn1");
        assert!(refusal(&doc).contains("countries map has the key"), "{key}");
    }
}

#[test]
fn the_star_fallback_is_required() {
    let mut doc = document(1);
    doc["countries"]
        .as_object_mut()
        .map(|object| object.remove(ANY_COUNTRY));
    assert!(refusal(&doc).contains("no \"*\" fallback"));
}

#[test]
fn every_named_cell_must_be_listed() {
    let mut doc = document(1);
    doc["countries"]["SG"] = json!("sg1");
    assert!(refusal(&doc).contains("countries map names the cell \"sg1\""));
    let mut doc = document(1);
    doc["tenants"]["kopi"] = json!("sg1");
    assert!(refusal(&doc).contains("tenants map names the cell \"sg1\""));
}

#[test]
fn tenant_slugs_are_dns_labels() {
    for good in ["a", "pho-hanoi", "a1", &"a".repeat(63)] {
        let mut doc = document(1);
        doc["tenants"][good] = json!("vn1");
        assert!(parse(&doc).is_ok(), "{good:?} is a slug");
    }
    for bad in [
        "",
        "-pho",
        "pho-",
        "Pho",
        "pho_hanoi",
        "phở",
        &"a".repeat(64),
    ] {
        let mut doc = document(1);
        doc["tenants"][bad] = json!("vn1");
        assert!(
            refusal(&doc).contains("not a tenant slug"),
            "{bad:?} is not a slug"
        );
    }
}

#[test]
fn the_directory_must_list_this_cell_at_its_own_address() {
    let mut doc = document(1);
    doc["cells"]
        .as_object_mut()
        .map(|object| object.remove("vn1"));
    doc["countries"]["VN"] = json!("jp1");
    doc["countries"]["*"] = json!("jp1");
    doc["tenants"]["pho-hanoi"] = json!("jp1");
    assert!(refusal(&doc).contains("does not list this cell, vn1"));

    let mut doc = document(1);
    doc["cells"]["vn1"] = json!("https://elsewhere.example.com");
    assert!(refusal(&doc).contains("but its [cell] base_url is https://vn1.example.com"));

    let mut unconfigured = vn1();
    unconfigured.base_url = None;
    assert!(
        Directory::parse(document(1).to_string().as_bytes(), &unconfigured).is_ok(),
        "with no base URL of its own there is nothing to disagree with"
    );
}

#[test]
fn sizes_are_bounded() {
    let oversized = vec![b' '; MAX_DIRECTORY_BYTES + 1];
    assert!(
        Directory::parse(&oversized, &vn1())
            .expect_err("too large")
            .contains("over the")
    );
    // The document already names two cells, so 1,022 more is exactly the limit.
    let mut doc = document(1);
    for n in 0..1_022 {
        doc["cells"][format!("c{n:04}")] = json!(format!("https://c{n:04}.example.com"));
    }
    assert!(parse(&doc).is_ok(), "1024 cells is the limit, and it holds");
    doc["cells"]["c9999"] = json!("https://c9999.example.com");
    assert!(refusal(&doc).contains("cells map has 1025 entries, over the 1024"));
}
