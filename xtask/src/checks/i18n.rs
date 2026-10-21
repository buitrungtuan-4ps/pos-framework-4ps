// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every supported UI language is complete in both front ends, and the settings have their words
//! in each (P11 item 2; [ADR-0020](../../../docs/adr/0020-i18n-runtime.md)).
//!
//! `packages/shared/i18n/languages.json` is the register of supported languages. For `ui/` and
//! `dashboard/`, this fails when:
//!
//! - a supported language has no catalogue, or a catalogue ships for a language the register does
//!   not list;
//! - `LOCALES` in the front end's `src/i18n/index.ts` is not the register;
//! - a catalogue lacks a key `en.json` has, or carries one it does not;
//! - in the console, a supported catalogue lacks a setting's words: the title and help of every key
//!   in `docs/snapshots/settings.txt`, the label of every token value, and the notice of every value
//!   that needs a confirmation (backlog D8). The Settings screen otherwise shows the raw key.
//!
//! A language a country module declares as its default (`default_language` in `countries/*`) that
//! the register does not list is reported, not failed: *declared by a country, not yet
//! supported*. It enters the register only with a complete, reviewed catalogue. Until then its
//! stores see `en`, which every catalogue falls back to (docs/languages.md).

use std::collections::BTreeSet;
use std::path::Path;

use super::{Error, repo_root};
use crate::Finding;

const RULE: &str = "i18n";

/// The register.
pub const REGISTER: &str = "packages/shared/i18n/languages.json";

/// The front ends, each with a catalogue directory.
const FRONT_ENDS: [&str; 2] = ["ui", "dashboard"];

/// The settings register snapshot, which names every setting the console labels.
const SETTINGS: &str = "docs/snapshots/settings.txt";

/// A catalogue: its keys.
type Catalogue = BTreeSet<String>;

/// The languages the register supports.
///
/// # Errors
///
/// When the register cannot be read or has no `supported` list.
pub fn supported(text: &str) -> Result<Vec<String>, Error> {
    let json: serde_json::Value = serde_json::from_str(text)?;
    let list = json["supported"]
        .as_array()
        .ok_or("the register has no `supported` list")?;
    Ok(list
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect())
}

/// The languages in `LOCALES` in a front end's `index.ts`, in order.
#[must_use]
pub fn locales(index: &str) -> Option<Vec<String>> {
    let after = index.split_once("export const LOCALES")?.1;
    // After the `=`, so the `[]` of `readonly Locale[]` is not taken for the list.
    let list = after
        .split_once('=')?
        .1
        .split_once('[')?
        .1
        .split_once(']')?
        .0;
    Some(
        list.split(',')
            .map(|item| item.trim().trim_matches('"').to_owned())
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

/// The default language each country module declares, by its directory.
#[must_use]
pub fn country_languages(sources: &[(String, String)]) -> Vec<(String, String)> {
    let marker = "default_language: TranslationKey::new(\"";
    sources
        .iter()
        .filter_map(|(country, source)| {
            let after = source.split_once(marker)?.1;
            let language = after.split_once('"')?.0;
            Some((country.clone(), language.to_owned()))
        })
        .collect()
}

/// The keys the console needs for each setting, from the settings snapshot.
#[must_use]
pub fn settings_keys(snapshot: &str) -> Catalogue {
    let mut keys = Catalogue::new();
    for line in snapshot
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        match line.split_once('\t') {
            None => {
                keys.insert(format!("settings.item.{line}.title"));
                keys.insert(format!("settings.item.{line}.help"));
            }
            Some((setting, field)) => {
                if field.starts_with("confirm=") {
                    keys.insert(format!("settings.item.{setting}.confirm"));
                }
                // A switch's `true` and `false` read as On and Off, which the console words itself.
                if let Some(token) = field.strip_prefix("value=")
                    && token != "true"
                    && token != "false"
                {
                    keys.insert(format!("settings.value.{token}"));
                }
            }
        }
    }
    keys
}

/// Reads a catalogue's keys.
fn catalogue(path: &Path) -> Result<Catalogue, Error> {
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(json
        .as_object()
        .ok_or("a catalogue is not a JSON object")?
        .keys()
        .cloned()
        .collect())
}

/// The findings for one front end's catalogues, given the register and the console's settings keys.
#[must_use]
pub fn front_end_findings(
    front_end: &str,
    shipped: &[(String, Catalogue)],
    declared: Option<&[String]>,
    supported: &[String],
    settings: Option<&Catalogue>,
) -> Vec<Finding> {
    let directory = format!("{front_end}/src/i18n");
    let mut findings = Vec::new();
    let names: BTreeSet<&str> = shipped.iter().map(|(name, _)| name.as_str()).collect();
    for language in supported {
        if !names.contains(language.as_str()) {
            findings.push(
                Finding::new(
                    &directory,
                    RULE,
                    format!("has no catalogue for `{language}`, which the register supports"),
                )
                .with_hint(format!(
                    "add {directory}/{language}.json, or take `{language}` out of {REGISTER}"
                )),
            );
        }
    }
    for name in &names {
        if !supported.iter().any(|language| language == name) {
            findings.push(
                Finding::new(format!("{directory}/{name}.json"), RULE, format!("ships `{name}`, which the register does not support"))
                    .with_hint(format!("a language ships only once it is complete and reviewed: add it to {REGISTER} then, or remove the file")),
            );
        }
    }
    if declared != Some(supported) {
        findings.push(
            Finding::new(
                format!("{directory}/index.ts"),
                RULE,
                format!("`LOCALES` is {declared:?}, but the register supports {supported:?}"),
            )
            .with_hint("make `LOCALES` list the register's languages, in its order"),
        );
    }
    let empty = Catalogue::new();
    let floor = shipped
        .iter()
        .find(|(name, _)| name == "en")
        .map_or(&empty, |(_, keys)| keys);
    for (name, keys) in shipped.iter().filter(|(name, _)| name != "en") {
        let file = format!("{directory}/{name}.json");
        let missing: Vec<&String> = floor.difference(keys).collect();
        let extra: Vec<&String> = keys.difference(floor).collect();
        if !missing.is_empty() {
            findings.push(Finding::new(
                &file,
                RULE,
                format!(
                    "lacks {} key(s) en.json has: {}",
                    missing.len(),
                    sample(&missing)
                ),
            ));
        }
        if !extra.is_empty() {
            findings.push(Finding::new(
                &file,
                RULE,
                format!(
                    "has {} key(s) en.json does not: {}",
                    extra.len(),
                    sample(&extra)
                ),
            ));
        }
    }
    if let Some(settings) = settings {
        for (name, keys) in shipped {
            let missing: Vec<&String> = settings.difference(keys).collect();
            if !missing.is_empty() {
                findings.push(
                    Finding::new(format!("{directory}/{name}.json"), RULE, format!("lacks the words for {} setting key(s): {}", missing.len(), sample(&missing)))
                        .with_hint("every setting in docs/snapshots/settings.txt needs a title, a help line, its token values' labels and any confirmation notice, in every console language"),
                );
            }
        }
    }
    findings
}

/// Up to five keys, for a finding.
fn sample(keys: &[&String]) -> String {
    let shown: Vec<&str> = keys.iter().take(5).map(|key| key.as_str()).collect();
    if keys.len() > shown.len() {
        format!("{}, …", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// Runs the check, and names on standard error each language a country declares that the register
/// does not support yet.
///
/// # Errors
///
/// When the register, a catalogue, an index or the settings snapshot cannot be read.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let supported = supported(&std::fs::read_to_string(root.join(REGISTER))?)?;
    let settings = settings_keys(&std::fs::read_to_string(root.join(SETTINGS))?);
    let mut findings = Vec::new();
    for front_end in FRONT_ENDS {
        let directory = root.join(front_end).join("src/i18n");
        let mut shipped = Vec::new();
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                shipped.push((name, catalogue(&path)?));
            }
        }
        shipped.sort();
        let index = std::fs::read_to_string(directory.join("index.ts"))?;
        let declared = locales(&index);
        let words = (front_end == "dashboard").then_some(&settings);
        findings.extend(front_end_findings(
            front_end,
            &shipped,
            declared.as_deref(),
            &supported,
            words,
        ));
    }
    let mut countries = Vec::new();
    for entry in std::fs::read_dir(root.join("countries"))? {
        let path = entry?.path();
        if let Ok(source) = std::fs::read_to_string(path.join("src/lib.rs")) {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            countries.push((name, source));
        }
    }
    countries.sort();
    for (country, language) in country_languages(&countries) {
        if !supported.contains(&language) {
            eprintln!(
                "xtask i18n: `{language}` (countries/{country}) is declared by a country, not yet \
                 supported; its stores see `en` until a reviewed catalogue enters {REGISTER}"
            );
        }
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::{Catalogue, country_languages, front_end_findings, locales, settings_keys};

    fn keys(list: &[&str]) -> Catalogue {
        list.iter().map(|key| (*key).to_owned()).collect()
    }

    fn languages(list: &[&str]) -> Vec<String> {
        list.iter().map(|language| (*language).to_owned()).collect()
    }

    #[test]
    fn reads_locales_from_an_index() {
        let index = "export const LOCALES: readonly Locale[] = [\"en\", \"vi\"];\n";
        assert_eq!(locales(index), Some(languages(&["en", "vi"])));
        assert_eq!(locales("const OTHER = [];"), None);
    }

    #[test]
    fn reads_each_countrys_default_language() {
        let sources = vec![
            (
                "jp".to_owned(),
                "default_language: TranslationKey::new(\"ja\"),".to_owned(),
            ),
            (
                "in".to_owned(),
                "default_language: TranslationKey::new(\"en\"),".to_owned(),
            ),
        ];
        assert_eq!(
            country_languages(&sources),
            [
                ("jp".to_owned(), "ja".to_owned()),
                ("in".to_owned(), "en".to_owned())
            ]
        );
    }

    #[test]
    fn a_setting_needs_its_title_help_token_labels_and_notice() {
        let snapshot = "# comment\nshift.drawer_access\nshift.drawer_access\tconfirm=DRAWER_ACCESS_ASSIGNED\n\
                        shift.drawer_access\tvalue=DRAWER_ACCESS_SHARED\npermissions.enforced\n\
                        permissions.enforced\tvalue=true\n";
        assert_eq!(
            settings_keys(snapshot),
            keys(&[
                "settings.item.permissions.enforced.help",
                "settings.item.permissions.enforced.title",
                "settings.item.shift.drawer_access.confirm",
                "settings.item.shift.drawer_access.help",
                "settings.item.shift.drawer_access.title",
                "settings.value.DRAWER_ACCESS_SHARED",
            ])
        );
    }

    #[test]
    fn complete_catalogues_pass() {
        let shipped = vec![
            ("en".to_owned(), keys(&["a", "b"])),
            ("vi".to_owned(), keys(&["a", "b"])),
        ];
        let supported = languages(&["en", "vi"]);
        let findings = front_end_findings("ui", &shipped, Some(&supported), &supported, None);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn each_gap_is_a_finding() {
        let supported = languages(&["en", "vi"]);
        let shipped = vec![
            ("en".to_owned(), keys(&["a", "b", "settings.item.x.title"])),
            ("ja".to_owned(), keys(&["a", "b", "settings.item.x.title"])),
        ];
        let settings = keys(&["settings.item.x.title", "settings.item.x.help"]);
        let declared = languages(&["en"]);
        let messages: Vec<String> = front_end_findings(
            "dashboard",
            &shipped,
            Some(&declared),
            &supported,
            Some(&settings),
        )
        .into_iter()
        .map(|finding| finding.message)
        .collect();
        for expected in [
            "has no catalogue for `vi`",
            "ships `ja`, which the register does not support",
            "`LOCALES` is",
            "lacks the words for 1 setting key(s)",
        ] {
            assert!(
                messages.iter().any(|message| message.contains(expected)),
                "missing {expected:?} in {messages:?}"
            );
        }
        let partial = vec![
            ("en".to_owned(), keys(&["a", "b"])),
            ("vi".to_owned(), keys(&["a", "c"])),
        ];
        let messages: Vec<String> =
            front_end_findings("ui", &partial, Some(&supported), &supported, None)
                .into_iter()
                .map(|finding| finding.message)
                .collect();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("lacks 1 key(s) en.json has: b"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("has 1 key(s) en.json does not: c"))
        );
    }
}
