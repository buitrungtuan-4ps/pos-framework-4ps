use super::{CellDirectory, DirectoryMode, Offer, TenantHome, parse_public_key};
use crate::cell::directory::tests::vn1;
use crate::cell::testkit::{
    V2, V3, V4, V5_FORGED, V6_NO_FALLBACK, V7_ELSEWHERE, public_key_line, tampered,
};

fn trusting() -> CellDirectory {
    CellDirectory::trusting(
        vn1(),
        parse_public_key(public_key_line()).expect("the key parses"),
    )
}

fn held_version(directory: &CellDirectory) -> Option<u64> {
    directory.held().map(|held| held.directory_version)
}

#[test]
fn no_key_is_self_mode_and_every_tenant_is_here() {
    let directory = CellDirectory::single(vn1());
    assert_eq!(directory.mode(), DirectoryMode::SelfOnly);
    assert_eq!(directory.tenant_cell("anyone").as_deref(), Some("vn1"));
    assert!(directory.held().is_none());
    let refusal = directory
        .offer(V3.document, V3.minisig)
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
        directory.offer(V3.document, V3.minisig),
        Ok(Offer::Accepted(3))
    );
    assert_eq!(directory.mode(), DirectoryMode::Signed);
    assert_eq!(directory.tenant_cell("ramen-osaka").as_deref(), Some("jp1"));
    assert_eq!(directory.tenant_cell("pho-hanoi").as_deref(), Some("vn1"));
    assert_eq!(directory.tenant_cell("unknown"), None);
    assert_eq!(held_version(&directory), Some(3));
}

#[test]
fn a_forged_signature_is_refused_and_the_last_good_directory_kept() {
    let directory = trusting();
    assert!(directory.offer(V3.document, V3.minisig).is_ok());
    let refusal = directory
        .offer(V5_FORGED.document, V5_FORGED.minisig)
        .expect_err("a signature by another key under this key's id is refused");
    assert!(refusal.contains("does not verify"), "{refusal}");
    assert_eq!(directory.last_refusal(), Some(refusal));
    assert_eq!(
        held_version(&directory),
        Some(3),
        "the directory held is kept"
    );
    assert_eq!(directory.mode(), DirectoryMode::Signed);
}

#[test]
fn a_tampered_document_is_refused() {
    let directory = trusting();
    assert!(directory.offer(V3.document, V3.minisig).is_ok());
    let changed = tampered(V4.document);
    assert_ne!(changed.as_slice(), V4.document, "one byte is flipped");
    let refusal = directory
        .offer(&changed, V4.minisig)
        .expect_err("a byte changed after signing is refused");
    assert!(refusal.contains("does not verify"), "{refusal}");
    assert_eq!(held_version(&directory), Some(3));
}

#[test]
fn a_malformed_signature_file_or_another_document_is_refused() {
    let directory = trusting();
    let refusal = directory
        .offer(V3.document, b"not a signature file")
        .expect_err("refused");
    assert!(refusal.contains("signature file"), "{refusal}");
    assert!(
        directory.offer(V3.document, V4.minisig).is_err(),
        "a signature over another document is refused"
    );
    assert_eq!(directory.mode(), DirectoryMode::Missing);
}

#[test]
fn a_well_signed_but_invalid_directory_is_refused() {
    let directory = trusting();
    let refusal = directory
        .offer(V6_NO_FALLBACK.document, V6_NO_FALLBACK.minisig)
        .expect_err("content is checked after the signature");
    assert!(refusal.contains("fallback"), "{refusal}");
    let refusal = directory
        .offer(V7_ELSEWHERE.document, V7_ELSEWHERE.minisig)
        .expect_err("a directory that misplaces this cell is refused");
    assert!(refusal.contains("base_url"), "{refusal}");
    assert_eq!(directory.mode(), DirectoryMode::Missing);
}

#[test]
fn only_a_newer_version_replaces_the_one_held() {
    let directory = trusting();
    assert_eq!(
        directory.offer(V3.document, V3.minisig),
        Ok(Offer::Accepted(3))
    );
    assert_eq!(
        directory.offer(V3.document, V3.minisig),
        Ok(Offer::Unchanged(3))
    );
    let refusal = directory
        .offer(V2.document, V2.minisig)
        .expect_err("a rollback is refused");
    assert!(refusal.contains("older than the 3"), "{refusal}");
    assert_eq!(held_version(&directory), Some(3));
    assert_eq!(
        directory.offer(V4.document, V4.minisig),
        Ok(Offer::Accepted(4))
    );
    assert_eq!(
        directory.last_refusal(),
        None,
        "an accepted offer clears the refusal"
    );
}

#[test]
fn a_recorded_refusal_is_kept_until_an_offer_is_accepted() {
    let directory = trusting();
    directory.record_refusal("cannot read /etc/pos/cells.json".to_owned());
    assert_eq!(
        directory.last_refusal().as_deref(),
        Some("cannot read /etc/pos/cells.json")
    );
    assert_eq!(directory.mode(), DirectoryMode::Missing);
    assert!(directory.offer(V3.document, V3.minisig).is_ok());
    assert_eq!(directory.last_refusal(), None);
}

#[test]
fn public_keys_are_checked() {
    assert!(parse_public_key(public_key_line()).is_ok());
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
fn a_tenant_home_carries_its_cell_and_origin() {
    let single = CellDirectory::single(vn1());
    assert_eq!(
        single.tenant_home("anyone"),
        Some(TenantHome {
            cell_id: "vn1".to_owned(),
            base_url: Some("https://vn1.example.com".to_owned()),
            here: true,
        })
    );

    let directory = trusting();
    assert_eq!(
        directory.tenant_home("ramen-osaka"),
        None,
        "nothing held yet"
    );
    assert!(directory.offer(V3.document, V3.minisig).is_ok());
    assert_eq!(
        directory.tenant_home("ramen-osaka"),
        Some(TenantHome {
            cell_id: "jp1".to_owned(),
            base_url: Some("https://jp1.example.com".to_owned()),
            here: false,
        })
    );
    assert_eq!(
        directory.tenant_home("pho-hanoi"),
        Some(TenantHome {
            cell_id: "vn1".to_owned(),
            base_url: Some("https://vn1.example.com".to_owned()),
            here: true,
        })
    );
    assert_eq!(directory.tenant_home("unknown"), None);
}
