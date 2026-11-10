// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Whether every translation key a package version names has an English text
//! ([ADR-0190](../../../../docs/adr/0190-a-screen-is-drawn-from-the-package.md) decision 6).
//!
//! A package holds no user-visible text: a time slot's name, a screen's label and title properties,
//! and a data part's field, unit, consent purpose, object type, list and list entry labels are keys
//! of the tenant's translation grid. A till shows a key's text in its language and falls back to
//! English, so a key without an `en` value would draw nothing. The grid is the cloud's, so the
//! cloud runs [`check_keys`] at publish, answering for each key from the grid it holds; the edge
//! does not, as the grid it holds may be older than the version.

use pos_proto::package::{CustomField, PackageContent};
use pos_proto::screen::{Component, PropertyKind, PropertyValue, ScreenNode};

/// Where a key without an `en` value is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAt {
    /// The name of the time slot at this position in `time_slots`.
    TimeSlot(usize),
    /// A translation-key property of a screen's node: the variant's position in `screens`, and the
    /// node's in its tree's pre-order, the root being 0.
    Screen {
        /// The variant's position in `screens`.
        screen: usize,
        /// The node's position in the tree's pre-order.
        node: usize,
    },
    /// A key of the field at this position in `data.custom_fields`.
    CustomField(usize),
    /// A key of the object type at this position in `data.object_types`, or of one of its fields.
    ObjectType(usize),
    /// A key of the list at this position in `data.lists`, or of one of its entries.
    List(usize),
}

/// A translation key the version names that has no `en` value in the tenant's grid, and where.
/// A key is a label's name, never its text, so it can be shown and logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRefusal {
    /// Where the version names it.
    pub at: KeyAt,
    /// The key.
    pub key: String,
}

impl KeyRefusal {
    /// The request field at fault, in the shape the console's field errors use.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        match self.at {
            KeyAt::TimeSlot(_) => "time_slots.label_key",
            KeyAt::Screen { .. } => "screens",
            KeyAt::CustomField(_) => "data.custom_fields",
            KeyAt::ObjectType(_) => "data.object_types",
            KeyAt::List(_) => "data.lists",
        }
    }
}

impl core::fmt::Display for KeyRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let key = &self.key;
        match self.at {
            KeyAt::TimeSlot(index) => write!(f, "time slot {index}: ")?,
            KeyAt::Screen { screen, node } => write!(f, "screen {screen}: node {node}: ")?,
            KeyAt::CustomField(index) => write!(f, "custom field {index}: ")?,
            KeyAt::ObjectType(index) => write!(f, "object type {index}: ")?,
            KeyAt::List(index) => write!(f, "list {index}: ")?,
        }
        write!(
            f,
            "the key `{key}` has no English text in the tenant's translations"
        )
    }
}

impl core::error::Error for KeyRefusal {}

/// Checks that every translation key `content` names has an `en` value, as `has_en` answers from
/// the tenant's grid. A property is read as a key only where the catalogue says it holds one, so a
/// token or an act's id is never mistaken for one.
///
/// # Errors
///
/// The first [`KeyRefusal`], in field order: time slots, screens (each tree in pre-order, a node's
/// properties by name), then the data part's fields, object types and lists.
pub fn check_keys(
    content: &PackageContent,
    has_en: impl Fn(&str) -> bool,
) -> Result<(), KeyRefusal> {
    let check = |at: KeyAt, key: &str| {
        if has_en(key) {
            Ok(())
        } else {
            Err(KeyRefusal {
                at,
                key: key.to_owned(),
            })
        }
    };
    for (index, slot) in content.time_slots.iter().enumerate() {
        check(KeyAt::TimeSlot(index), slot.label_key.as_str())?;
    }
    for (screen, variant) in content.screens.iter().enumerate() {
        let mut pending: Vec<&ScreenNode> = vec![&variant.body.root];
        let mut node = 0;
        while let Some(current) = pending.pop() {
            for key in node_keys(current) {
                check(KeyAt::Screen { screen, node }, key)?;
            }
            pending.extend(current.children.iter().rev());
            node += 1;
        }
    }
    let data = &content.data;
    for (index, field) in data.custom_fields.iter().enumerate() {
        for key in field_keys(field) {
            check(KeyAt::CustomField(index), key)?;
        }
    }
    for (index, object_type) in data.object_types.iter().enumerate() {
        let at = KeyAt::ObjectType(index);
        check(at, object_type.label_key.as_str())?;
        for key in object_type.fields.iter().flat_map(field_keys) {
            check(at, key)?;
        }
    }
    for (index, list) in data.lists.iter().enumerate() {
        let at = KeyAt::List(index);
        check(at, list.label_key.as_str())?;
        for entry in &list.entries {
            check(at, entry.label_key.as_str())?;
        }
    }
    Ok(())
}

/// The keys a node gives its component's translation-key properties, by property name. A node
/// whose component the release lacks names none: [`super::check_screens`] refuses it first.
fn node_keys(node: &ScreenNode) -> impl Iterator<Item = &str> {
    let component = Component::find(&node.component_id);
    node.properties.iter().filter_map(move |(name, value)| {
        let property = component?.property(name)?;
        match (property.kind, value) {
            (PropertyKind::TranslationKey, PropertyValue::Text(key)) => Some(key.as_str()),
            _ => None,
        }
    })
}

/// A field's label key, then its unit's and its consent purpose's when it names them.
fn field_keys(field: &CustomField) -> impl Iterator<Item = &str> {
    core::iter::once(&field.label_key)
        .chain(field.unit_key.as_ref())
        .chain(field.consent_purpose_key.as_ref())
        .map(pos_proto::text::TranslationKey::as_str)
}

#[cfg(test)]
mod tests {
    use super::{KeyAt, KeyRefusal, check_keys};
    use pos_proto::package::PackageContent;

    /// A version naming a key in each place a version can: a slot's name, a screen's title, a
    /// field's label and unit, an object type and its personal field's consent purpose, a list and
    /// its entry.
    const CONTENT: &str = r#"{
        "package_id":"01J00000000000000000000P01","package_version_id":"01J00000000000000000000V01",
        "version_label":"March","min_edge_version":"0.15.0",
        "time_slots":[{"time_slot_id":"lunch","label_key":"slot.lunch","schedules":[{"days":31,"start_minute":660,"end_minute":840}]}],
        "screens":[{"body":{"screen_id":"welcome","root":{"component_id":"screen_frame","children":[
            {"component_id":"page_header","properties":{"title_key":"welcome.title"}}]}}}],
        "data":{
            "custom_fields":[{"custom_field_id":"01J0000000000000000000CF01","object":"DATA_OBJECT_ORDER","label_key":"data.weight","field_type":"CUSTOM_FIELD_TYPE_QUANTITY","personal":false,"unit_key":"data.unit.kg"}],
            "object_types":[{"object_type_id":"01J00000000000000000000B01","label_key":"data.reservation","fields":[
                {"custom_field_id":"01J0000000000000000000CF03","label_key":"data.reservation.contact","field_type":"CUSTOM_FIELD_TYPE_TEXT","personal":true,"retention_days":30,"consent_purpose_key":"data.consent.reservation"}]}],
            "lists":[{"custom_list_id":"01J0000000000000000000CA01","label_key":"data.list.occasion","entries":[
                {"list_entry_id":"01J0000000000000000000EE01","label_key":"data.occasion.birthday"}]}]
        }
    }"#;

    const EVERY_KEY: [&str; 9] = [
        "slot.lunch",
        "welcome.title",
        "data.weight",
        "data.unit.kg",
        "data.reservation",
        "data.reservation.contact",
        "data.consent.reservation",
        "data.list.occasion",
        "data.occasion.birthday",
    ];

    fn content() -> PackageContent {
        serde_json::from_str(CONTENT).expect("the test content parses")
    }

    #[test]
    fn a_version_whose_every_key_has_english_passes() {
        assert_eq!(
            check_keys(&content(), |key| EVERY_KEY.contains(&key)),
            Ok(())
        );
    }

    #[test]
    fn each_key_without_english_is_refused_where_it_is_named() {
        let expected = [
            (KeyAt::TimeSlot(0), "time_slots.label_key"),
            (KeyAt::Screen { screen: 0, node: 1 }, "screens"),
            (KeyAt::CustomField(0), "data.custom_fields"),
            (KeyAt::CustomField(0), "data.custom_fields"),
            (KeyAt::ObjectType(0), "data.object_types"),
            (KeyAt::ObjectType(0), "data.object_types"),
            (KeyAt::ObjectType(0), "data.object_types"),
            (KeyAt::List(0), "data.lists"),
            (KeyAt::List(0), "data.lists"),
        ];
        for (missing, (at, field)) in EVERY_KEY.iter().zip(expected) {
            let refused = check_keys(&content(), |key| key != *missing);
            let want = KeyRefusal {
                at,
                key: (*missing).to_owned(),
            };
            assert_eq!(refused.as_ref(), Err(&want), "{missing}");
            assert_eq!(want.field(), field, "{missing}");
        }
    }

    #[test]
    fn the_first_key_in_field_order_is_the_one_refused() {
        let refused = check_keys(&content(), |_| false);
        assert_eq!(
            refused.map_err(|refusal| refusal.to_string()),
            Err(
                "time slot 0: the key `slot.lunch` has no English text in the tenant's \
                 translations"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_token_or_an_act_is_never_read_as_a_key() {
        let mut content = content();
        content.time_slots.clear();
        content.data = pos_proto::package::PackageData::default();
        content.screens = serde_json::from_str(
            r#"[{"body":{"screen_id":"pay","root":{"component_id":"screen_frame","children":[
                {"component_id":"action_button","properties":{"label_key":"pay.go","variant":"primary","action":{"action_id":"ui.screen.open","params":{"screen_id":"tender"}}}}]}}}]"#,
        )
        .expect("the test screens parse");
        let named = core::cell::RefCell::new(Vec::new());
        let checked = check_keys(&content, |key| {
            named.borrow_mut().push(key.to_owned());
            true
        });
        assert_eq!(checked, Ok(()));
        assert_eq!(named.into_inner(), vec!["pay.go".to_owned()]);
    }
}
