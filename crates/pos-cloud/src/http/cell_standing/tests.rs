use pos_proto::locale::CountryCode;

use super::{CellAcknowledgementView, checked_reason, live_difference, standing};
use crate::cell::Cell;
use crate::lease::RegionReasonRefusal;

fn vn_cell() -> Cell {
    Cell {
        cell_id: "vn1".to_owned(),
        country_alias: None,
        home_country: Some(CountryCode::VN),
        base_url: Some("https://vn1.example.com".to_owned()),
        runs_in: Some(CountryCode::VN),
        configured: true,
    }
}

fn answer(profile: &str, cell: &str) -> CellAcknowledgementView {
    CellAcknowledgementView {
        profile_country: profile.to_owned(),
        cell_country: cell.to_owned(),
        reason: "Transfer covered by the group DTA.".to_owned(),
        acknowledged_at_ms: 1_793_000_000_000,
    }
}

#[test]
fn a_store_in_its_cells_country_agrees() {
    let view = standing(&vn_cell(), Some(CountryCode::VN), None);
    assert!(!view.differs);
    assert_eq!(view.cell_country.as_deref(), Some("VN"));
    assert_eq!(view.profile_country.as_deref(), Some("VN"));
}

#[test]
fn a_store_in_another_country_differs_until_answered() {
    let view = standing(&vn_cell(), Some(CountryCode::JP), None);
    assert!(view.differs);
    assert_eq!(view.acknowledgement, None);

    let view = standing(&vn_cell(), Some(CountryCode::JP), Some(answer("JP", "VN")));
    assert!(
        view.differs,
        "an answer explains the difference, it does not erase it"
    );
    assert_eq!(view.acknowledgement, Some(answer("JP", "VN")));
}

#[test]
fn an_answer_about_another_pair_does_not_stand() {
    for stale in [answer("IN", "VN"), answer("JP", "ZZ")] {
        let view = standing(&vn_cell(), Some(CountryCode::JP), Some(stale));
        assert_eq!(view.acknowledgement, None);
    }
    let view = standing(&vn_cell(), Some(CountryCode::VN), Some(answer("VN", "VN")));
    assert_eq!(
        view.acknowledgement, None,
        "no difference, nothing standing"
    );
}

#[test]
fn nothing_known_is_no_difference() {
    let view = standing(&Cell::single(), Some(CountryCode::JP), None);
    assert!(
        !view.differs,
        "a cell with no home country compares with nothing"
    );
    let view = standing(&vn_cell(), None, None);
    assert!(
        !view.differs,
        "a store with no country compares with nothing"
    );
    assert_eq!(view.profile_country, None);
}

#[test]
fn a_write_must_name_the_live_difference() {
    let (vn, jp, sg) = (CountryCode::VN, CountryCode::JP, CountryCode::ZZ);
    assert_eq!(live_difference(Some(vn), Some(jp), (jp, vn)), Ok(()));
    assert!(live_difference(None, Some(jp), (jp, vn)).is_err());
    assert!(live_difference(Some(vn), None, (jp, vn)).is_err());
    assert!(
        live_difference(Some(vn), Some(jp), (sg, vn))
            .expect_err("moved")
            .contains("moved")
    );
    assert!(
        live_difference(Some(vn), Some(jp), (jp, sg))
            .expect_err("moved")
            .contains("moved")
    );
    assert!(
        live_difference(Some(vn), Some(vn), (vn, vn))
            .expect_err("agrees")
            .contains("nothing to acknowledge")
    );
}

#[test]
fn a_reason_is_one_sentence() {
    assert_eq!(
        checked_reason("  Covered by the DTA. "),
        Ok("Covered by the DTA.")
    );
    assert_eq!(checked_reason("   "), Err(RegionReasonRefusal::Empty));
    assert!(checked_reason(&"a".repeat(280)).is_ok());
    assert_eq!(
        checked_reason(&"ả".repeat(281)),
        Err(RegionReasonRefusal::TooLong),
        "counted in characters, not bytes"
    );
}
