use pos_proto::locale::CountryCode;

use std::path::PathBuf;

use super::{Cell, CellSection, DEFAULT_CELL_ID, directory_source, resolve};
use crate::cell::testkit::public_key_line;

fn section() -> CellSection {
    CellSection {
        cell_id: "vn1".to_owned(),
        country_alias: Some("VN".to_owned()),
        home_country: "vn".to_owned(),
        base_url: "https://vn1.example.com/".to_owned(),
        runs_in: "VN".to_owned(),
        directory_path: None,
        directory_public_key: None,
    }
}

#[test]
fn no_section_is_the_single_default_cell() {
    let cell = resolve(None).expect("absent is valid");
    assert_eq!(cell, Cell::single());
    assert_eq!(cell.cell_id, DEFAULT_CELL_ID);
    assert_eq!(
        DEFAULT_CELL_ID, "default",
        "the default id is on the wire and in the docs"
    );
    assert!(!cell.configured);
    assert!(!cell.runs_abroad(), "nothing known is nothing abroad");
}

#[test]
fn a_full_section_resolves_normalised() {
    let cell = resolve(Some(&section())).expect("a well-formed section resolves");
    assert_eq!(cell.cell_id, "vn1");
    assert_eq!(
        cell.country_alias.as_deref(),
        Some("vn"),
        "the alias is lower-case"
    );
    assert_eq!(
        cell.home_country,
        Some(CountryCode::VN),
        "the country is upper-case"
    );
    assert_eq!(cell.runs_in, Some(CountryCode::VN));
    assert_eq!(
        cell.base_url.as_deref(),
        Some("https://vn1.example.com"),
        "the base URL loses its trailing slash"
    );
    assert!(cell.configured);
    assert!(!cell.runs_abroad());
}

#[test]
fn the_alias_is_optional() {
    let mut raw = section();
    raw.country_alias = None;
    let cell = raw.resolve().expect("no alias is valid");
    assert_eq!(cell.country_alias, None);
}

#[test]
fn servers_in_another_country_are_abroad_and_still_accepted() {
    let mut raw = section();
    raw.runs_in = "SG".to_owned();
    let cell = raw.resolve().expect("running abroad is never a refusal");
    assert!(cell.runs_abroad());
}

#[test]
fn cell_ids_are_checked() {
    for good in ["vn1", "jp-east-2", "abc", &"a".repeat(32)] {
        let mut raw = section();
        raw.cell_id = good.to_owned();
        assert!(raw.resolve().is_ok(), "{good:?} is a cell id");
    }
    for bad in [
        "vn",
        "",
        "1vn",
        "Vn1",
        "vn1-",
        "vn_1",
        "vn 1",
        "-vn1",
        &"a".repeat(33),
    ] {
        let mut raw = section();
        raw.cell_id = bad.to_owned();
        let error = raw.resolve().expect_err("a malformed cell id is refused");
        assert!(error.contains("cell_id"), "{bad:?}: {error}");
    }
}

#[test]
fn aliases_are_two_letters() {
    for bad in ["v", "vnm", "v1", ""] {
        let mut raw = section();
        raw.country_alias = Some(bad.to_owned());
        let error = raw.resolve().expect_err("a malformed alias is refused");
        assert!(error.contains("country_alias"), "{bad:?}: {error}");
    }
}

#[test]
fn countries_are_checked_by_key() {
    let mut raw = section();
    raw.home_country = "Vietnam".to_owned();
    assert!(raw.resolve().expect_err("refused").contains("home_country"));
    let mut raw = section();
    raw.runs_in = "V1".to_owned();
    assert!(raw.resolve().expect_err("refused").contains("runs_in"));
}

#[test]
fn base_urls_are_origins() {
    for good in [
        "https://vn1.example.com",
        "https://vn1.example.com:8443/",
        "http://localhost:8080",
        "http://127.0.0.1:8080",
        "http://[::1]:8080",
    ] {
        let mut raw = section();
        raw.base_url = good.to_owned();
        assert!(raw.resolve().is_ok(), "{good:?} is an origin");
    }
    for bad in [
        "vn1.example.com",
        "http://vn1.example.com",
        "http://10.0.0.1",
        "ftp://vn1.example.com",
        "https://vn1.example.com/console",
        "https://vn1.example.com/?a=1",
        "https://vn1.example.com/#top",
        "https://admin:pw@vn1.example.com",
        "https://admin@vn1.example.com",
        "https://:pw@vn1.example.com",
    ] {
        let mut raw = section();
        raw.base_url = bad.to_owned();
        let error = raw.resolve().expect_err("not an origin is refused");
        assert!(error.contains("base_url"), "{bad:?}: {error}");
    }
}

#[test]
fn the_section_parses_from_toml_and_refuses_unknown_keys() {
    let parsed: CellSection = toml::from_str(
        "cell_id = \"vn1\"\nhome_country = \"VN\"\nbase_url = \"https://vn1.example.com\"\nruns_in = \"VN\"\n",
    )
    .expect("parses");
    assert_eq!(parsed.country_alias, None);
    let typo = toml::from_str::<CellSection>(
        "cell_id = \"vn1\"\nhome_country = \"VN\"\nbase_ulr = \"https://vn1.example.com\"\nruns_in = \"VN\"\n",
    );
    assert!(typo.is_err(), "a misspelt key is refused, not ignored");
}

#[test]
fn the_directory_pair_is_both_or_neither() {
    assert!(directory_source(None).expect("no section").is_none());
    assert!(
        directory_source(Some(&section()))
            .expect("neither")
            .is_none()
    );

    let mut raw = section();
    raw.directory_path = Some(PathBuf::from("/etc/pos/cells.json"));
    raw.directory_public_key = Some(public_key_line().to_owned());
    let source = directory_source(Some(&raw))
        .expect("both")
        .expect("a source");
    assert_eq!(
        source.signature_path(),
        PathBuf::from("/etc/pos/cells.json.minisig"),
        "the signature sits beside the file, as minisign writes it"
    );

    let mut only_path = section();
    only_path.directory_path = Some(PathBuf::from("/etc/pos/cells.json"));
    let mut only_key = section();
    only_key.directory_public_key = Some(public_key_line().to_owned());
    for raw in [only_path, only_key] {
        let error = directory_source(Some(&raw)).expect_err("one without the other");
        assert!(error.contains("set together"), "{error}");
    }

    let mut bad_key = section();
    bad_key.directory_path = Some(PathBuf::from("/etc/pos/cells.json"));
    bad_key.directory_public_key = Some("RWQ=".to_owned());
    assert!(
        directory_source(Some(&bad_key))
            .expect_err("a malformed key")
            .contains("directory_public_key")
    );
}
