// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_country::CountryModule;
use pos_proto::BusinessType;
use pos_proto::settings::{ValueRefusal, register};
use pos_proto::wire_enum::WireEnum;

use super::{TemplateError, directory, read, template, templates};

/// Every shipped country pack (ADR-0200 decision 6: `vn`, `jp`, `in` and the neutral `zz`).
fn packs() -> Vec<Box<dyn CountryModule>> {
    vec![
        Box::new(pos_country_vn::Vietnam),
        Box::new(pos_country_jp::Japan),
        Box::new(pos_country_in::India),
        Box::new(pos_country_zz::Zz),
    ]
}

#[test]
fn every_shipped_template_reads_and_checks() {
    let shipped = templates();
    assert!(shipped.is_ok(), "{shipped:?}");
    assert_eq!(shipped.map(|all| all.len()).ok(), Some(6));
}

#[test]
fn every_business_type_has_exactly_one_template_and_none_has_none() {
    for business_type in BusinessType::ALL.iter().copied() {
        let found = template(business_type);
        if business_type == BusinessType::Unspecified {
            assert_eq!(found, Ok(None));
            assert_eq!(directory(business_type), None);
            continue;
        }
        let found = found.ok().flatten();
        assert_eq!(
            found.as_ref().map(|template| template.business_type),
            Some(business_type)
        );
        assert!(
            found.and_then(|template| template.capabilities()).is_some(),
            "{business_type:?} has no preset"
        );
    }
}

#[test]
fn the_directory_on_disk_holds_the_six_templates_and_nothing_else() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../business-types");
    let mut on_disk: Vec<String> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let mut compiled: Vec<String> = BusinessType::ALL
        .iter()
        .filter_map(|business_type| directory(*business_type))
        .map(str::to_owned)
        .collect();
    compiled.sort();
    assert_eq!(on_disk, compiled, "a template directory is not compiled in");
}

#[test]
fn no_template_sets_a_key_a_country_pack_sets() {
    let shipped = templates().unwrap_or_default();
    assert!(!shipped.is_empty());
    for pack in packs() {
        for seeded in pack.setting_defaults() {
            for template in &shipped {
                assert!(
                    template
                        .recommended_settings
                        .iter()
                        .all(|recommended| recommended.setting_key != seeded.setting_key),
                    "business-types/{} sets {}, which the {} pack sets: the country states law and \
                     money, the template a way of working (ADR-0200 decision 4)",
                    template.directory,
                    seeded.setting_key,
                    pack.country_code(),
                );
            }
        }
    }
}

/// Reads `body` as the café template, against the real register.
fn cafe(body: &str) -> Result<super::Template, TemplateError> {
    let source = format!(r#"{{"business_type": "BUSINESS_TYPE_CAFE", {body}}}"#);
    read(BusinessType::Cafe, "cafe", &source, &register())
}

#[test]
fn a_template_naming_another_business_type_is_refused() {
    let refused = read(
        BusinessType::Cafe,
        "cafe",
        r#"{"business_type": "BUSINESS_TYPE_BAR", "recommended_settings": []}"#,
        &register(),
    );
    assert!(matches!(
        refused,
        Err(TemplateError::WrongBusinessType { named, .. }) if named == "BUSINESS_TYPE_BAR"
    ));
}

#[test]
fn a_file_of_another_shape_is_refused() {
    assert!(matches!(
        cafe(r#""recommended_settings": [], "screens": []"#),
        Err(TemplateError::Malformed { .. })
    ));
}

#[test]
fn an_unknown_key_is_refused() {
    assert!(matches!(
        cafe(r#""recommended_settings": [{"setting_key": "shift.no_such_thing", "value": 1}]"#),
        Err(TemplateError::UnknownSetting { setting_key, .. }) if setting_key == "shift.no_such_thing"
    ));
}

#[test]
fn a_package_key_is_refused_because_a_template_pins_its_own_copy() {
    assert!(matches!(
        cafe(
            r#""recommended_settings": [{"setting_key": "package.mode", "value": "PACKAGE_MODE_LOCKED"}]"#
        ),
        Err(TemplateError::PackageSetting { .. })
    ));
}

#[test]
fn a_value_the_setting_does_not_take_is_refused() {
    assert!(matches!(
        cafe(
            r#""recommended_settings": [{"setting_key": "tender_keys.split_ways_max", "value": 99}]"#
        ),
        Err(TemplateError::Refused {
            refusal: ValueRefusal::OutOfRange,
            ..
        })
    ));
}

#[test]
fn a_key_named_twice_is_refused() {
    let twice = r#""recommended_settings": [
        {"setting_key": "tender_keys.split_ways_max", "value": 4},
        {"setting_key": "tender_keys.split_ways_max", "value": 5}
    ]"#;
    assert!(matches!(cafe(twice), Err(TemplateError::Duplicate { .. })));
}

#[test]
fn every_demo_sells_in_every_shipped_country_s_currency() {
    for template in templates().unwrap_or_default() {
        for pack in packs() {
            let currency = pack.locale_pack().currency_code;
            let menu = template.demo.menu(template.business_type, currency);
            assert!(
                menu.is_some(),
                "business-types/{} has an item with no {} price",
                template.directory,
                currency.as_str()
            );
        }
    }
}

#[test]
fn a_template_that_serves_at_tables_has_a_floor_and_a_counter_has_none() {
    use pos_core::capability::Capability;
    for template in templates().unwrap_or_default() {
        let tables = template
            .capabilities()
            .is_some_and(|preset| preset.enabled(Capability::Tables));
        let floor = template.demo.floor_plan(template.business_type);
        assert_eq!(
            floor.is_some_and(|plan| plan.areas().iter().any(|area| !area.tables.is_empty())),
            tables,
            "business-types/{}",
            template.directory
        );
    }
}

#[test]
fn no_two_templates_publish_the_same_item_id() {
    let mut ids = Vec::new();
    for template in templates().unwrap_or_default() {
        let menu = template
            .demo
            .menu(template.business_type, pos_proto::money::CurrencyCode::VND);
        for entry in menu
            .iter()
            .flat_map(|book| book.catalog_for(pos_proto::SalesChannel::DineIn).items())
        {
            assert!(!ids.contains(&entry.menu_item_id), "{}", template.directory);
            ids.push(entry.menu_item_id);
        }
    }
    assert!(!ids.is_empty());
}

#[test]
fn a_demo_with_a_real_looking_name_a_shared_key_or_no_price_is_refused() {
    use super::demo::{Demo, DemoFault};
    let demo = |items: &str| serde_json::from_str::<Demo>(&format!(r#"{{"items": {items}}}"#));
    let item = |key: u16, name: &str, price: i64| {
        format!(
            r#"{{"key": {key}, "display_name": "{name}", "tax_class": "food", "prices": {{"VND": {price}}}}}"#
        )
    };
    let check = |items: String| demo(&items).map(|demo| demo.check());
    assert_eq!(
        check(format!("[{}]", item(1, "Demo A", 10))).ok(),
        Some(Ok(()))
    );
    assert_eq!(check("[]".to_owned()).ok(), Some(Err(DemoFault::Size)));
    assert_eq!(
        check(format!("[{}]", item(1, "Margherita", 10))).ok(),
        Some(Err(DemoFault::NotObviouslyFake))
    );
    assert_eq!(
        check(format!(
            "[{}, {}]",
            item(1, "Demo A", 10),
            item(1, "Demo B", 10)
        ))
        .ok(),
        Some(Err(DemoFault::DuplicateKey))
    );
    assert_eq!(
        check(format!("[{}]", item(1, "Demo A", 0))).ok(),
        Some(Err(DemoFault::BadPrice))
    );
}

#[test]
fn a_template_with_a_kitchen_display_has_a_station_and_one_without_has_none() {
    use pos_core::capability::Capability;
    for template in templates().unwrap_or_default() {
        let kitchen = template
            .capabilities()
            .is_some_and(|preset| preset.enabled(Capability::Kds));
        assert_eq!(
            template.demo.station_plan(template.business_type).is_some(),
            kitchen,
            "business-types/{}",
            template.directory
        );
    }
}
