// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The committed record of the event catalogue.
//!
//! `docs/naming-and-api.md` §13 requires a snapshot of the event schema so that any
//! change to it is a visible diff in a pull request and a removal can be refused. This
//! module renders it; `cargo xtask snapshot` compares it against the base branch and
//! rejects removals.
//!
//! # Why one line per field
//!
//! The obvious format — one line per event type listing its fields — cannot express the
//! difference between *adding* a field and *replacing* an event type, because adding a
//! field rewrites the line. A line-based set difference would report the rewrite as a
//! removal and refuse a change that is entirely legal.
//!
//! So the snapshot is one line per fact. Adding a field adds a line, and nothing else
//! moves. Removing one removes a line, and that is exactly what must be refused.
//!
//! # A payload's parts
//!
//! A field may hold a part: a struct the payload nests, declared outside the catalogue, such as a
//! settled bill's tax lines. Each of a part's fields is a line of its own under the path to it, a
//! list written `[]`: `field=tax_lines[].components[].name` on `billing.bill.settled` is the name
//! of each component of each tax line. A field added inside a part then adds a line as a payload's
//! own does. A value type such as `Money` is not a part: it has one wire form wherever it appears.
//!
//! Created while the catalogue is still young, so the file grows by reviewable diffs
//! from here rather than arriving fully formed with no history.

use crate::envelope::{EventPayload, PayloadPart};
use crate::events::{BillFeeLine, BillTaxComponent, BillTaxLine, EventType};
use crate::fees::FrozenFee;

/// Where the rendered snapshot is committed, relative to the repository root.
pub const SNAPSHOT_PATH: &str = "docs/snapshots/events.txt";

/// Each part of a payload: the event it is in, its path there, and its fields.
///
/// Placed by hand, because the catalogue knows its fields' names and not their types; a part's own
/// field names come from its declaration ([`PayloadPart`]). A part admitted to the personal-data
/// barrier ([`crate::pii`]) is placed here too, and a test checks that each path runs through
/// fields of the payload.
const PARTS: &[(EventType, &str, &[&str])] = &[
    (
        EventType::BillingBillOpened,
        "fee_rules[]",
        FrozenFee::FIELD_NAMES,
    ),
    (
        EventType::BillingBillSettled,
        "fee_lines[]",
        BillFeeLine::FIELD_NAMES,
    ),
    (
        EventType::BillingBillSettled,
        "fee_lines[].tax_components[]",
        BillTaxComponent::FIELD_NAMES,
    ),
    (
        EventType::BillingBillSettled,
        "tax_lines[]",
        BillTaxLine::FIELD_NAMES,
    ),
    (
        EventType::BillingBillSettled,
        "tax_lines[].components[]",
        BillTaxComponent::FIELD_NAMES,
    ),
];

/// Renders the catalogue in the committed format.
///
/// Sorted throughout, so the output depends on the catalogue's content and not on its
/// declaration order — otherwise reordering two events would look like a change.
#[must_use]
pub fn render() -> String {
    let mut lines: Vec<String> = Vec::new();
    for event_type in EventType::ALL {
        let token = event_type.as_str();
        lines.push(format!(
            "{token}\tschema_version={}",
            event_type.schema_version()
        ));
        let mut fields: Vec<&str> = event_type.field_names().to_vec();
        fields.sort_unstable();
        for field in fields {
            lines.push(format!("{token}\tfield={field}"));
        }
    }
    for (event_type, path, fields) in PARTS {
        let token = event_type.as_str();
        for field in *fields {
            lines.push(format!("{token}\tfield={path}.{field}"));
        }
    }
    lines.sort();

    let mut out = String::from(
        "# Event catalogue snapshot. Generated — do not hand-edit.\n\
         #\n\
         # Regenerate with:  just snapshot\n\
         # A REMOVED line fails CI: a published event type or payload field is a\n\
         # contract and may only be added to, never renamed or removed.\n\
         # `docs/naming-and-api.md` §13.\n",
    );
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Asserts that a payload's declared metadata agrees with the enumeration.
///
/// Called once per payload from the test below, which is what makes the snapshot a
/// record of the *types* rather than of a separate list somebody has to maintain.
///
/// # Panics
///
/// When a payload's `SCHEMA_VERSION` or `FIELD_NAMES` disagree with what
/// [`EventType`] reports for the same type. The macro generates both from one
/// declaration, so this can only fire if that macro is changed incorrectly — which is
/// exactly when a loud failure is wanted.
pub fn assert_registered<P: EventPayload>() {
    let event_type = P::EVENT_TYPE;
    assert_eq!(
        P::SCHEMA_VERSION,
        event_type.schema_version(),
        "{event_type} declares two different schema versions"
    );
    assert_eq!(
        P::FIELD_NAMES,
        event_type.field_names(),
        "{event_type} declares two different field lists"
    );
}

#[cfg(test)]
mod tests {
    use super::{PARTS, SNAPSHOT_PATH, render};
    use crate::pii::check_field_name;

    fn snapshot_file() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(SNAPSHOT_PATH)
    }

    #[test]
    fn the_committed_snapshot_matches_the_catalogue() {
        let rendered = render();
        let path = snapshot_file();

        // Set POS_UPDATE_SNAPSHOTS=1 to rewrite. Deliberately opt-in: a check that
        // silently fixes itself is not a check.
        if std::env::var("POS_UPDATE_SNAPSHOTS").is_ok() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create the snapshot directory");
            }
            std::fs::write(&path, &rendered).expect("write the snapshot");
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{SNAPSHOT_PATH} is missing. Generate it with:\n\
                 \n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot\n"
            )
        });

        if committed != rendered {
            let committed_lines: std::collections::BTreeSet<&str> = committed.lines().collect();
            let rendered_lines: std::collections::BTreeSet<&str> = rendered.lines().collect();
            let removed: Vec<&&str> = committed_lines.difference(&rendered_lines).collect();
            let added: Vec<&&str> = rendered_lines.difference(&committed_lines).collect();
            panic!(
                "the event catalogue no longer matches {SNAPSHOT_PATH}.\n\
                 \nRemoved (each of these is a broken contract):\n  {}\n\
                 \nAdded (additive changes are fine):\n  {}\n\
                 \nRegenerate with:\n    POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot\n",
                removed
                    .iter()
                    .map(|line| (**line).to_owned())
                    .collect::<Vec<_>>()
                    .join("\n  "),
                added
                    .iter()
                    .map(|line| (**line).to_owned())
                    .collect::<Vec<_>>()
                    .join("\n  "),
            );
        }
    }

    #[test]
    fn the_rendering_is_deterministic() {
        // Otherwise the snapshot would churn between runs and stop being reviewable.
        assert_eq!(render(), render());
    }

    #[test]
    fn every_event_type_appears_with_a_version_and_at_least_the_lines_for_its_fields() {
        let rendered = render();
        for event_type in crate::events::EventType::ALL {
            let token = event_type.as_str();
            assert!(
                rendered.contains(&format!("{token}\tschema_version=")),
                "{token} has no version line"
            );
            for field in event_type.field_names() {
                assert!(
                    rendered.contains(&format!("{token}\tfield={field}")),
                    "{token}.{field} is missing from the snapshot"
                );
            }
        }
        for (event_type, path, fields) in PARTS {
            for field in *fields {
                assert!(
                    rendered.contains(&format!("{event_type}\tfield={path}.{field}\n")),
                    "{event_type} {path}.{field} is missing from the snapshot"
                );
            }
        }
    }

    #[test]
    fn every_part_sits_in_a_field_of_what_holds_it() {
        // `tax_lines[].components[]` is a field of the tax line, which is a field of the payload:
        // a path that names no field would list a part the wire never carries there.
        for (event_type, path, _) in PARTS {
            let place = path.strip_suffix("[]").unwrap_or(*path);
            let (fields, field) = match place.rsplit_once('.') {
                None => (event_type.field_names(), place),
                Some((holder, field)) => (
                    PARTS
                        .iter()
                        .find(|(other, held, _)| other == event_type && *held == holder)
                        .map(|(_, _, fields)| *fields)
                        .unwrap_or_default(),
                    field,
                ),
            };
            assert!(fields.contains(&field), "{event_type} has no {path}");
        }
    }

    #[test]
    fn no_field_of_a_part_suggests_personal_data() {
        // The name net over the parts, which the payloads' own field lists cannot see.
        for (event_type, path, fields) in PARTS {
            for field in *fields {
                assert_eq!(
                    check_field_name(field),
                    Ok(()),
                    "{event_type} {path}.{field}"
                );
                assert!(
                    field.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                    "{event_type} {path}.{field} is not snake_case"
                );
            }
        }
    }
}
