// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::package::PackageData;

use super::{EvolutionRefusal, check_evolution};

/// A list of two entries, a choice on an order, and a reservation with a party size and a personal
/// contact. Fake labels only.
const EARLIER: &str = r#"{"custom_fields":[{"custom_field_id":"01J0000000000000000000CF01","object":"DATA_OBJECT_ORDER","label_key":"data.order.occasion","field_type":"CUSTOM_FIELD_TYPE_CHOICE","personal":false,"custom_list_id":"01J0000000000000000000CA01"}],"object_types":[{"object_type_id":"01J00000000000000000000B01","label_key":"data.reservation","fields":[{"custom_field_id":"01J0000000000000000000CF02","label_key":"data.reservation.party","field_type":"CUSTOM_FIELD_TYPE_INT","personal":false},{"custom_field_id":"01J0000000000000000000CF03","label_key":"data.reservation.contact","field_type":"CUSTOM_FIELD_TYPE_TEXT","personal":true,"retention_days":30,"consent_purpose_key":"data.consent.reservation"}]}],"lists":[{"custom_list_id":"01J0000000000000000000CA01","label_key":"data.list.occasion","entries":[{"list_entry_id":"01J0000000000000000000EE01","label_key":"data.occasion.birthday"},{"list_entry_id":"01J0000000000000000000EE02","label_key":"data.occasion.anniversary"}]}]}"#;

fn earlier() -> PackageData {
    serde_json::from_str(EARLIER).expect("data parses")
}

fn id<T: core::str::FromStr>(text: &str) -> T
where
    T::Err: core::fmt::Debug,
{
    text.parse().expect("an id")
}

#[test]
fn the_same_part_or_one_that_only_adds_or_deprecates_follows() {
    assert_eq!(check_evolution(&earlier(), &earlier()), Ok(()));
    assert_eq!(check_evolution(&PackageData::default(), &earlier()), Ok(()));

    let mut next = earlier();
    let mut added = next.custom_fields.first().cloned().expect("a field");
    added.custom_field_id = id("01J0000000000000000000CF09");
    next.custom_fields.push(added);
    for field in &mut next.custom_fields {
        field.deprecated = true;
        field.label_key = pos_proto::text::TranslationKey::new("data.order.renamed_label");
    }
    for kind in &mut next.object_types {
        kind.deprecated = true;
    }
    for list in &mut next.lists {
        for entry in &mut list.entries {
            entry.deprecated = true;
        }
    }
    assert_eq!(check_evolution(&earlier(), &next), Ok(()));
}

/// One change that breaks an earlier version, and what it is refused for.
#[test]
fn removing_moving_retyping_or_unmarking_personal_is_refused() {
    let order_field = || EvolutionRefusal::Field(id("01J0000000000000000000CF01"));
    let contact = || EvolutionRefusal::Field(id("01J0000000000000000000CF03"));
    let list = || EvolutionRefusal::List(id("01J0000000000000000000CA01"));
    let cases: [(Change, EvolutionRefusal); 7] = [
        (|d| d.custom_fields.clear(), order_field()),
        (
            |d| {
                d.custom_fields
                    .iter_mut()
                    .for_each(|f| f.object = pos_proto::wire_enum::Open::default());
            },
            order_field(),
        ),
        (
            |d| {
                d.custom_fields
                    .iter_mut()
                    .for_each(|f| f.custom_list_id = None);
            },
            order_field(),
        ),
        (
            |d| {
                d.object_types
                    .iter_mut()
                    .for_each(|t| t.fields.iter_mut().for_each(|f| f.personal = Some(false)));
            },
            contact(),
        ),
        (
            |d| {
                let moved = d
                    .object_types
                    .iter_mut()
                    .flat_map(|t| t.fields.drain(..))
                    .collect::<Vec<_>>();
                d.custom_fields.extend(moved);
            },
            EvolutionRefusal::Field(id("01J0000000000000000000CF02")),
        ),
        (
            |d| d.object_types.clear(),
            EvolutionRefusal::ObjectType(id("01J00000000000000000000B01")),
        ),
        (
            |d| d.lists.iter_mut().for_each(|l| l.entries.truncate(1)),
            list(),
        ),
    ];
    for (change, refusal) in cases {
        let mut next = earlier();
        change(&mut next);
        assert_eq!(
            check_evolution(&earlier(), &next),
            Err(refusal.clone()),
            "{refusal}"
        );
    }
}

/// A field retyped keeps its id at its peril: the new type is a new field.
#[test]
fn a_retyped_field_is_refused() {
    let mut next = earlier();
    let text = serde_json::to_string(&next.object_types).expect("serialise");
    next.object_types =
        serde_json::from_str(&text.replace("CUSTOM_FIELD_TYPE_INT", "CUSTOM_FIELD_TYPE_QUANTITY"))
            .expect("parses");
    assert_eq!(
        check_evolution(&earlier(), &next),
        Err(EvolutionRefusal::Field(id("01J0000000000000000000CF02")))
    );
}

/// One change made to a data part.
type Change = fn(&mut PackageData);
