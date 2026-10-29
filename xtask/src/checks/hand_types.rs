// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The front ends' hand-mirrored wire types only go away
//! ([ADR-0177](../../../docs/adr/0177-openapi-is-the-route-map.md) decision 3).
//!
//! Each front end names the OpenAPI document's schemas in `src/api/schemas.ts`. What is still
//! written by hand is an exported `interface` or `type` in its `src/api/types.ts`, and
//! `tools/hand-types-baseline.toml` holds how many each may have. A count above its baseline fails:
//! a new wire type is an alias in `schemas.ts`, not another hand mirror. A count below it fails too,
//! until `--write-baseline` lowers the number, so the baseline follows every type that moves.
//! `--write-baseline` refuses to raise a number.

use std::collections::BTreeMap;

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "hand-types";

/// The baseline file, from the repository root.
pub const BASELINE: &str = "tools/hand-types-baseline.toml";

/// Each front end, and the file that holds its hand-mirrored wire types.
pub const FRONT_ENDS: &[(&str, &str)] = &[
    ("dashboard", "dashboard/src/api/types.ts"),
    ("ui", "ui/src/api/types.ts"),
];

/// The names of the exported `interface`s and `type`s in `source`, in the order they appear.
///
/// A declaration starts a line, as every one in these files does; an `export const`, an
/// `export type { … } from` re-export and an `import type` are not hand types.
#[must_use]
pub fn hand_types(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let rest = line
                .strip_prefix("export interface ")
                .or_else(|| line.strip_prefix("export type "))?;
            let name: String = rest
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The counts a baseline file holds, by front end.
///
/// # Errors
///
/// The text is not TOML, or has no `[hand_types]` table of whole numbers.
pub fn parse_baseline(text: &str) -> Result<BTreeMap<String, usize>, Error> {
    let value: toml::Table = text.parse()?;
    let table = value
        .get("hand_types")
        .and_then(toml::Value::as_table)
        .ok_or("no [hand_types] table")?;
    table
        .iter()
        .map(|(front_end, count)| {
            let count = count
                .as_integer()
                .and_then(|count| usize::try_from(count).ok())
                .ok_or_else(|| format!("{front_end}: not a whole number"))?;
            Ok((front_end.clone(), count))
        })
        .collect()
}

/// The baseline file for `counts`.
#[must_use]
pub fn render_baseline(counts: &BTreeMap<String, usize>) -> String {
    let mut text = String::from(
        "# How many wire types each front end still mirrors by hand in src/api/types.ts, rather than\n\
         # naming the OpenAPI document's schema in src/api/schemas.ts (ADR-0177 decision 3). Each\n\
         # number only falls: `cargo xtask hand-types` fails above it, and below it until\n\
         # `cargo xtask hand-types --write-baseline` lowers it, which refuses to raise one.\n\
         \n\
         [hand_types]\n",
    );
    for (front_end, count) in counts {
        text.push_str(&format!("{front_end} = {count}\n"));
    }
    text
}

/// The findings for one front end that has `names` while its baseline is `allowed`.
#[must_use]
pub fn compare(
    front_end: &str,
    file: &str,
    names: &[String],
    allowed: Option<usize>,
) -> Option<Finding> {
    let count = names.len();
    let message = match allowed {
        None => format!("{front_end} has no entry in {BASELINE}"),
        Some(allowed) if count > allowed => format!(
            "{count} hand-mirrored wire types, above the baseline's {allowed}: name the document's \
             schema in src/api/schemas.ts instead of adding to {file} (the last ones are {})",
            names[allowed.min(count)..].join(", ")
        ),
        Some(allowed) if count < allowed => format!(
            "{count} hand-mirrored wire types, below the baseline's {allowed}: run `cargo xtask \
             hand-types --write-baseline` and commit {BASELINE}"
        ),
        Some(_) => return None,
    };
    Some(Finding::new(file, RULE, message))
}

/// The baseline `counts` would write over `current`, or why it may not: a number may only fall.
///
/// # Errors
///
/// A count is above its current baseline.
pub fn lowered(
    current: &BTreeMap<String, usize>,
    counts: &BTreeMap<String, usize>,
) -> Result<BTreeMap<String, usize>, String> {
    for (front_end, count) in counts {
        if let Some(allowed) = current.get(front_end)
            && count > allowed
        {
            return Err(format!(
                "{front_end} has {count} hand types, above its baseline of {allowed}; the baseline \
                 only falls, so move the new ones to src/api/schemas.ts"
            ));
        }
    }
    Ok(counts.clone())
}

/// Counts each front end's hand types and holds them to the baseline, or with `--write-baseline`
/// lowers the baseline to them.
///
/// # Errors
///
/// A front end's `types.ts` or the baseline cannot be read, or `--write-baseline` would raise a
/// number.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut names = BTreeMap::new();
    for (front_end, file) in FRONT_ENDS {
        let source = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        names.insert(*front_end, hand_types(&source));
    }
    let counts: BTreeMap<String, usize> = names
        .iter()
        .map(|(front_end, names)| ((*front_end).to_owned(), names.len()))
        .collect();
    let baseline_path = root.join(BASELINE);
    let current = match std::fs::read_to_string(&baseline_path) {
        Ok(text) => parse_baseline(&text)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => return Err(error.into()),
    };
    if args.iter().any(|argument| argument == "--write-baseline") {
        let next = lowered(&current, &counts)?;
        std::fs::write(&baseline_path, render_baseline(&next))?;
        return Ok(Vec::new());
    }
    Ok(FRONT_ENDS
        .iter()
        .filter_map(|(front_end, file)| {
            compare(
                front_end,
                file,
                &names[front_end],
                current.get(*front_end).copied(),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{compare, hand_types, lowered, parse_baseline, render_baseline};

    fn counts(dashboard: usize, ui: usize) -> BTreeMap<String, usize> {
        BTreeMap::from([("dashboard".to_owned(), dashboard), ("ui".to_owned(), ui)])
    }

    #[test]
    fn counts_exported_interfaces_and_types_only() {
        let source = "\
import type { Money } from \"./schemas\";
export interface Store {
  readonly id: string;
}
export type Level = \"tenant\" | \"store\";
export const LEVELS: readonly Level[] = [\"tenant\", \"store\"];
export type { Brand } from \"./schemas\";
  export interface Indented {}
interface Local {}
";
        assert_eq!(hand_types(source), ["Store", "Level"]);
    }

    #[test]
    fn the_baseline_reads_back_what_it_wrote() {
        let written = counts(68, 27);
        assert_eq!(
            parse_baseline(&render_baseline(&written)).expect("parse"),
            written
        );
        assert!(parse_baseline("[other]\nui = 1\n").is_err());
        assert!(parse_baseline("[hand_types]\nui = -1\n").is_err());
    }

    #[test]
    fn a_count_must_equal_its_baseline() {
        let names: Vec<String> = ["A", "B", "C"].map(str::to_owned).to_vec();
        assert!(compare("ui", "ui/src/api/types.ts", &names, Some(3)).is_none());
        let above = compare("ui", "ui/src/api/types.ts", &names, Some(2)).expect("above");
        assert!(above.message.contains("above") && above.message.contains('C'));
        let below = compare("ui", "ui/src/api/types.ts", &names, Some(4)).expect("below");
        assert!(below.message.contains("--write-baseline"));
        assert!(compare("ui", "ui/src/api/types.ts", &names, None).is_some());
    }

    #[test]
    fn writing_the_baseline_only_lowers_it() {
        assert_eq!(
            lowered(&counts(68, 27), &counts(60, 27)),
            Ok(counts(60, 27))
        );
        assert!(lowered(&counts(68, 27), &counts(69, 27)).is_err());
        assert_eq!(
            lowered(&BTreeMap::new(), &counts(68, 27)),
            Ok(counts(68, 27))
        );
    }
}
