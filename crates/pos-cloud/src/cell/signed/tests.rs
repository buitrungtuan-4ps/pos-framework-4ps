use super::{CellDirectory, DirectoryMode, Offer, parse_public_key};
use crate::cell::directory::tests::{document, vn1};
use crate::cell::testkit::{forged_minisig, minisig, public_key_line};

fn bytes(version: u64) -> Vec<u8> {
    document(version).to_string().into_bytes()
}

fn trusting() -> CellDirectory {
    CellDirectory::trusting(
        vn1(),
        parse_public_key(&public_key_line()).expect("the key parses"),
    )
}

#[test]
fn no_key_is_self_mode_and_every_tenant_is_here() {
    let directory = CellDirectory::single(vn1());
    assert_eq!(directory.mode(), DirectoryMode::SelfOnly);
    assert_eq!(directory.tenant_cell("anyone").as_deref(), Some("vn1"));
    assert!(directory.held().is_none());
    let refusal = directory
        .offer(&bytes(1), &minisig(&bytes(1)))
        .expect_err("a cell with no key trusts no directory");
    assert!(refusal.contains("no [cell] directory_public_key"));
}

#[test]
fn a_key_with_nothing_accepted_answers_for_no_tenant() {
    let directory = trusting();
    assert_eq!(directory.mode(), DirectoryMode::Missing);
    assert_eq!(directory.tenant_cell("pho-hanoi"), None);
    assert_eq!(directory.last_refusal(), None);
}

#[test]
fn a_signed_directory_is_accepted_and_answers() {
    let directory = trusting();
    assert_eq!(
        directory.offer(&bytes(3), &minisig(&bytes(3))),
        Ok(Offer::Accepted(3))
    );
    assert_eq!(directory.mode(), DirectoryMode::Signed);
    assert_eq!(directory.tenant_cell("ramen-osaka").as_deref(), Some("jp1"));
    assert_eq!(directory.tenant_cell("unknown"), None);
    assert_eq!(directory.held().map(|held| held.directory_version), Some(3));
}

#[test]
fn a_bad_signature_is_refused_and_the_last_good_directory_kept() {
    let directory = trusting();
    assert!(directory.offer(&bytes(3), &minisig(&bytes(3))).is_ok());

    let refusal = directory
        .offer(&bytes(4), &forged_minisig(&bytes(4)))
        .expect_err("a forged signature is refused");
    assert!(refusal.contains("does not verify"), "{refusal}");
    assert_eq!(directory.last_refusal(), Some(refusal));

    let mut tampered = bytes(4);
    let signature = minisig(&tampered);
    tampered.push(b' ');
    assert!(directory.offer(&tampered, &signature).is_err());

    assert!(
        directory.offer(&bytes(4), b"not a signature file").is_err(),
        "a malformed signature file is refused"
    );
    assert_eq!(
        directory.held().map(|held| held.directory_version),
        Some(3),
        "every refusal keeps the directory held"
    );
    assert_eq!(directory.mode(), DirectoryMode::Signed);
}

#[test]
fn a_well_signed_but_invalid_directory_is_refused() {
    let directory = trusting();
    let mut doc = document(5);
    doc["countries"]
        .as_object_mut()
        .map(|object| object.remove("*"));
    let invalid = doc.to_string().into_bytes();
    let refusal = directory
        .offer(&invalid, &minisig(&invalid))
        .expect_err("content is checked after the signature");
    assert!(refusal.contains("fallback"));
    assert_eq!(directory.mode(), DirectoryMode::Missing);
}

#[test]
fn only_a_newer_version_replaces_the_one_held() {
    let directory = trusting();
    assert!(directory.offer(&bytes(5), &minisig(&bytes(5))).is_ok());
    assert_eq!(
        directory.offer(&bytes(5), &minisig(&bytes(5))),
        Ok(Offer::Unchanged(5))
    );
    let refusal = directory
        .offer(&bytes(4), &minisig(&bytes(4)))
        .expect_err("a rollback is refused");
    assert!(refusal.contains("older than the 5"), "{refusal}");
    assert_eq!(
        directory.offer(&bytes(6), &minisig(&bytes(6))),
        Ok(Offer::Accepted(6))
    );
    assert_eq!(
        directory.last_refusal(),
        None,
        "an accepted offer clears the refusal"
    );
}

#[test]
fn public_keys_are_checked() {
    assert!(parse_public_key(&public_key_line()).is_ok());
    assert!(
        parse_public_key(&format!("  {}\n", public_key_line())).is_ok(),
        "surrounding space is ignored"
    );
    for (bad, why) in [
        ("not base64!", "not valid base64"),
        ("RWQ=", "42 bytes"),
        (
            "RUQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "algorithm",
        ),
    ] {
        let error = parse_public_key(bad).expect_err("refused");
        assert!(error.contains(why), "{bad}: {error}");
        assert!(error.contains("directory_public_key"));
    }
}

#[test]
fn a_well_signed_directory_naming_this_cell_elsewhere_is_refused() {
    let directory = trusting();
    let mut doc = document(2);
    doc["cells"]["vn1"] = serde_json::json!("https://elsewhere.example.com");
    let elsewhere = doc.to_string().into_bytes();
    let refusal = directory
        .offer(&elsewhere, &minisig(&elsewhere))
        .expect_err("a directory that misplaces this cell is refused");
    assert!(refusal.contains("base_url"), "{refusal}");
    assert_eq!(directory.mode(), DirectoryMode::Missing);
}
