// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask new screen --app ui <id>`: a till screen, wired everywhere a screen is named
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 5).
//!
//! It writes `ui/src/screens/<Component>.tsx`, a screen with its title and nothing else, and adds one
//! line each to:
//! - the registry, `SCREENS` in `ui/src/state/screens.ts`;
//! - `App.tsx`: the component's import and its `COMPONENTS` entry;
//! - every string catalogue under `ui/src/i18n/`, as the last key: `en.json` gets `--title`, and
//!   each other catalogue `<locale>.json` gets `--title-<locale>`, which is required for every
//!   catalogue present. The generator never writes a string nobody gave it, so a third locale makes
//!   its flag required without a code change;
//! - the replay test's route walk, so the new route is checked to open its screen.
//!
//! The console's screens (`--app dashboard`) also need an icon and a nav group, which are design
//! choices rather than inputs, so that app is not generated yet.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Edit, Plan, render};
use crate::checks::Error;

const TEMPLATE: &str = include_str!("../../templates/screen/ui/Screen.tsx.tmpl");

const SCREENS: &str = "ui/src/state/screens.ts";
const APP: &str = "ui/src/App.tsx";
const I18N: &str = "ui/src/i18n";
const REPLAY: &str = "ui/tests/replay.spec.mjs";

/// What `cargo xtask new screen` is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// The screen's id in `SCREENS`, in `camelCase`.
    pub id: String,
    /// Its route path.
    pub path: String,
    /// Its title in English, for `en.json`.
    pub title: String,
    /// Its title in each other locale, by locale (`vi` → the Vietnamese title).
    pub titles: BTreeMap<String, String>,
}

/// Reads the arguments after `new screen`, then plans against the repository at `root`.
///
/// # Errors
///
/// [`Error`] for a missing, repeated or unknown argument, an app other than `ui`, or anything
/// [`plan`] refuses.
pub fn plan_from_args(root: &Path, args: &[&String]) -> Result<Plan, Error> {
    let spec = spec_from_args(args)?;
    let mut catalogues = Vec::new();
    for entry in std::fs::read_dir(root.join(I18N))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(locale) = name.strip_suffix(".json") {
            let text = std::fs::read_to_string(root.join(I18N).join(&name))?;
            catalogues.push((locale.to_owned(), text));
        }
    }
    let read = |path: &str| std::fs::read_to_string(root.join(path));
    plan(
        &spec,
        &Sources {
            screens: &read(SCREENS)?,
            app: &read(APP)?,
            catalogues: &catalogues,
            replay: &read(REPLAY)?,
        },
    )
}

fn spec_from_args(args: &[&String]) -> Result<Spec, Error> {
    let mut flags: BTreeMap<String, String> = BTreeMap::new();
    let mut id = None;
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        let Some(flag) = argument.strip_prefix("--") else {
            if id.replace((*argument).clone()).is_some() {
                return Err(format!("unexpected argument `{argument}`; {}", super::USAGE).into());
            }
            continue;
        };
        let known = matches!(flag, "app" | "path" | "title") || flag.starts_with("title-");
        if !known {
            return Err(format!("unexpected argument `{argument}`; {}", super::USAGE).into());
        }
        let value = iter
            .next()
            .ok_or_else(|| format!("`{argument}` needs a value"))?;
        if flags.insert(flag.to_owned(), (*value).clone()).is_some() {
            return Err(format!("`{argument}` is given twice").into());
        }
    }
    match flags.remove("app").as_deref() {
        Some("ui") => {}
        Some("dashboard") => {
            return Err(
                "`--app dashboard` is not generated yet: a console screen also needs an icon \
                 and a nav group, which are design choices"
                    .into(),
            );
        }
        _ => return Err("`--app ui` is required".into()),
    }
    let path = flags.remove("path").ok_or("`--path` is required")?;
    let title = flags.remove("title").ok_or("`--title` is required")?;
    let titles = flags
        .into_iter()
        .filter_map(|(flag, value)| {
            flag.strip_prefix("title-")
                .map(|locale| (locale.to_owned(), value))
        })
        .collect();
    Ok(Spec {
        id: id.ok_or(super::USAGE)?,
        path,
        title,
        titles,
    })
}

/// The files a screen is wired through, as they stand.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// `ui/src/state/screens.ts`.
    pub screens: &'a str,
    /// `ui/src/App.tsx`.
    pub app: &'a str,
    /// Every catalogue under `ui/src/i18n/`, as (locale, contents).
    pub catalogues: &'a [(String, String)],
    /// `ui/tests/replay.spec.mjs`.
    pub replay: &'a str,
}

/// The screen `spec` describes, wired through `sources`.
///
/// # Errors
///
/// [`Error`] if a value is malformed, the id, path or string already exists, or a file lacks the
/// place a line goes.
pub fn plan(spec: &Spec, sources: &Sources<'_>) -> Result<Plan, Error> {
    check(spec)?;
    let component = pascal(&spec.id);
    let key = format!("{}.title", snake(&spec.id));
    if sources.screens.contains(&format!("\n  {}: {{", spec.id)) {
        return Err(format!("`{}` is already a screen", spec.id).into());
    }
    if sources
        .screens
        .contains(&format!("path: \"{}\"", spec.path))
    {
        return Err(format!("a screen already has the path `{}`", spec.path).into());
    }
    let strings = strings(spec, sources.catalogues, &key)?;
    let walked = spec
        .path
        .split('/')
        .map(|part| if part.starts_with(':') { "x" } else { part })
        .collect::<Vec<_>>()
        .join("/");
    let mut edits = vec![
        insert_before(
            SCREENS,
            sources.screens,
            "} as const satisfies Record<string, Screen>;",
            &[format!(
                "  {}: {{ path: \"{}\", key: \"{key}\" }},",
                spec.id, spec.path
            )],
        )?,
        app_edit(sources.app, &spec.id, &component)?,
    ];
    edits.extend(strings);
    edits.push(insert_after_opening(
        REPLAY,
        sources.replay,
        "const ROUTED_SCREENS = [",
        "];",
        &format!("  {{ path: \"{walked}\", title: \"{key}\" }},"),
    )?);
    let values = [
        ("component", component.as_str()),
        ("key", key.as_str()),
        ("path", spec.path.as_str()),
        ("title", spec.title.as_str()),
    ];
    let next = vec![
        "`pnpm build` and `pnpm lint` in `ui/` check it; `pnpm replay` opens its route".to_owned(),
    ];
    Ok(Plan {
        files: vec![(
            format!("ui/src/screens/{component}.tsx"),
            render(TEMPLATE, &values)?,
        )],
        member: None,
        package: component,
        next,
        edits,
    })
}

/// Refuses a value the files could not hold as written.
fn check(spec: &Spec) -> Result<(), Error> {
    let camel = spec.id.starts_with(|c: char| c.is_ascii_lowercase())
        && spec.id.len() <= 40
        && spec.id.chars().all(|c| c.is_ascii_alphanumeric());
    if !camel {
        return Err(format!(
            "`{}` is not a camelCase screen id, as in `stockCount`",
            spec.id
        )
        .into());
    }
    let path = spec.path.starts_with('/')
        && !spec.path.contains("//")
        && spec
            .path
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '/' | '-' | ':'));
    if !path {
        return Err(format!(
            "`{}` is not a route path, as in `/stock` or `/stock/:id`",
            spec.path
        )
        .into());
    }
    let plain = |text: &str| {
        !text.trim().is_empty()
            && text.trim() == text
            && !text.contains(['\n', '"', '\\', '{', '}', '<', '>'])
    };
    if !plain(&spec.title) || spec.titles.values().any(|title| !plain(title)) {
        return Err(
            "a title must be one plain line, without quotes, backslashes, braces or angle \
                    brackets"
                .into(),
        );
    }
    Ok(())
}

/// The title's line in each catalogue: English first, then the other locales in order. Every
/// catalogue but English needs its `--title-<locale>`, and a flag for a locale with no catalogue is
/// refused, so no string is written that nobody gave.
fn strings(spec: &Spec, catalogues: &[(String, String)], key: &str) -> Result<Vec<Edit>, Error> {
    let mut ordered: Vec<&(String, String)> = catalogues.iter().collect();
    ordered.sort_by_key(|(locale, _)| (locale != "en", locale.clone()));
    if ordered.first().is_none_or(|(locale, _)| locale != "en") {
        return Err(format!(
            "`{I18N}` has no en.json, the catalogue every other is checked against"
        )
        .into());
    }
    if let Some(stray) = spec
        .titles
        .keys()
        .find(|locale| !catalogues.iter().any(|(have, _)| have == *locale))
    {
        return Err(
            format!("`--title-{stray}` names a locale `{I18N}` has no catalogue for").into(),
        );
    }
    let mut edits = Vec::new();
    for (locale, text) in ordered {
        if text.contains(&format!("\"{key}\":")) {
            return Err(format!("the string `{key}` already exists in {locale}.json").into());
        }
        let title = if locale == "en" {
            &spec.title
        } else {
            spec.titles.get(locale).ok_or_else(|| {
                format!(
                    "`--title-{locale}` is required: the generator never invents a {locale} string"
                )
            })?
        };
        edits.push(catalogue(
            &format!("{I18N}/{locale}.json"),
            text,
            key,
            title,
        )?);
    }
    Ok(edits)
}

/// `stockCount` → `StockCount`.
fn pascal(id: &str) -> String {
    let mut chars = id.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

/// `stockCount` → `stock_count`.
fn snake(id: &str) -> String {
    let mut out = String::new();
    for c in id.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` with `lines` inserted before the one line that is exactly `anchor`.
fn insert_before(path: &str, text: &str, anchor: &str, lines: &[String]) -> Result<Edit, Error> {
    let at = text
        .find(&format!("\n{anchor}"))
        .ok_or_else(|| format!("`{path}` has no line `{anchor}` to add before"))?
        + 1;
    let mut contents = text.to_owned();
    contents.insert_str(at, &format!("{}\n", lines.join("\n")));
    Ok(Edit {
        path: path.to_owned(),
        contents,
        added: lines.to_vec(),
    })
}

/// `text` with `line` added as the last entry of the list that opens at `opening` and closes at the
/// first `closing` line after it.
fn insert_after_opening(
    path: &str,
    text: &str,
    opening: &str,
    closing: &str,
    line: &str,
) -> Result<Edit, Error> {
    let start = text
        .find(opening)
        .ok_or_else(|| format!("`{path}` has no `{opening}`"))?;
    let end = text
        .get(start..)
        .and_then(|rest| rest.find(&format!("\n{closing}")))
        .ok_or_else(|| format!("`{path}`'s `{opening}` list is not closed"))?
        + start
        + 1;
    let mut contents = text.to_owned();
    contents.insert_str(end, &format!("{line}\n"));
    Ok(Edit {
        path: path.to_owned(),
        contents,
        added: vec![line.to_owned()],
    })
}

/// `App.tsx` with the screen's import after the last screen import, and its `COMPONENTS` entry last.
fn app_edit(text: &str, id: &str, component: &str) -> Result<Edit, Error> {
    let import = format!("import {{ {component} }} from \"./screens/{component}\";");
    let last_import = text
        .match_indices("\nimport { ")
        .filter(|(at, _)| {
            text.get(*at..)
                .and_then(|rest| rest.get(1..).and_then(|line| line.split('\n').next()))
                .is_some_and(|line| line.contains("from \"./screens/"))
        })
        .last()
        .map(|(at, _)| at)
        .ok_or("`ui/src/App.tsx` imports no screen to add one after")?;
    let line_end = text
        .get(last_import + 1..)
        .and_then(|rest| rest.find('\n'))
        .ok_or("`ui/src/App.tsx` ends inside an import")?
        + last_import
        + 2;
    let mut contents = text.to_owned();
    contents.insert_str(line_end, &format!("{import}\n"));
    let entry = format!("  {id}: {component},");
    let with_entry = insert_after_opening(
        APP,
        &contents,
        "const COMPONENTS: Record<ScreenId, Component> = {",
        "};",
        &entry,
    )?;
    Ok(Edit {
        path: APP.to_owned(),
        contents: with_entry.contents,
        added: vec![import, entry],
    })
}

/// A flat catalogue with `"key": "value"` added as its last entry.
fn catalogue(path: &str, text: &str, key: &str, value: &str) -> Result<Edit, Error> {
    let close = text
        .rfind("\"\n}")
        .ok_or_else(|| format!("`{path}` does not end in a flat string entry"))?
        + 1;
    let line = format!("  \"{key}\": \"{value}\"");
    let mut contents = text.to_owned();
    contents.insert_str(close, &format!(",\n{line}"));
    Ok(Edit {
        path: path.to_owned(),
        contents,
        added: vec![line],
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::LazyLock;

    use super::{Sources, Spec, plan, spec_from_args};

    /// The catalogues as they stand, Vietnamese listed before English to show the order is the
    /// generator's own.
    static CATALOGUES: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
        vec![
            (
                "vi".to_owned(),
                include_str!("../../../ui/src/i18n/vi.json").to_owned(),
            ),
            (
                "en".to_owned(),
                include_str!("../../../ui/src/i18n/en.json").to_owned(),
            ),
        ]
    });

    fn sources() -> Sources<'static> {
        Sources {
            screens: include_str!("../../../ui/src/state/screens.ts"),
            app: include_str!("../../../ui/src/App.tsx"),
            catalogues: &CATALOGUES,
            replay: include_str!("../../../ui/tests/replay.spec.mjs"),
        }
    }

    /// The golden input: every catalogue's title given.
    fn spec() -> Spec {
        Spec {
            id: "stockCount".into(),
            path: "/stock".into(),
            title: "Stock count".into(),
            titles: BTreeMap::from([("vi".to_owned(), "Kiểm kho".to_owned())]),
        }
    }

    #[test]
    fn the_screen_and_its_lines_are_the_golden_ones() {
        let planned = plan(&spec(), &sources()).expect("planned");
        let files: Vec<(&str, &str)> = planned
            .files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        assert_eq!(
            files,
            [(
                "ui/src/screens/StockCount.tsx",
                include_str!("../../tests/golden/new-screen/ui/StockCount.tsx.golden"),
            )]
        );
        let added: Vec<String> = planned
            .edits
            .iter()
            .flat_map(|edit| {
                edit.added
                    .iter()
                    .map(move |line| format!("{}: {line}", edit.path))
            })
            .collect();
        let golden: Vec<&str> = include_str!("../../tests/golden/new-screen/ui/edits.golden")
            .lines()
            .collect();
        assert_eq!(added, golden);
        assert!(planned.member.is_none());
    }

    #[test]
    fn each_catalogue_still_parses_and_holds_the_title_once() {
        let planned = plan(&spec(), &sources()).expect("planned");
        for (path, title) in [
            ("ui/src/i18n/en.json", "Stock count"),
            ("ui/src/i18n/vi.json", "Kiểm kho"),
        ] {
            let edit = planned
                .edits
                .iter()
                .find(|edit| edit.path == path)
                .expect(path);
            let parsed: serde_json::Value =
                serde_json::from_str(&edit.contents).expect("still JSON");
            assert_eq!(parsed["stock_count.title"], title, "{path}");
        }
    }

    #[test]
    fn each_line_lands_inside_its_list() {
        let planned = plan(&spec(), &sources()).expect("planned");
        let contents = |path: &str| {
            planned
                .edits
                .iter()
                .find(|edit| edit.path == path)
                .map(|edit| edit.contents.clone())
                .unwrap_or_default()
        };
        let screens = contents("ui/src/state/screens.ts");
        assert!(screens.contains(
            "  stockCount: { path: \"/stock\", key: \"stock_count.title\" },\n} as const satisfies"
        ));
        let app = contents("ui/src/App.tsx");
        let table = app.find("const COMPONENTS").expect("table");
        let entry = app.find("  stockCount: StockCount,").expect("entry");
        let close = app
            .get(table..)
            .and_then(|rest| rest.find("\n};"))
            .expect("close")
            + table;
        assert!(table < entry && entry < close);
        let replay = contents("ui/tests/replay.spec.mjs");
        let walk = replay.find("const ROUTED_SCREENS = [").expect("walk");
        let line = replay
            .find("  { path: \"/stock\", title: \"stock_count.title\" },\n];")
            .expect("line");
        assert!(walk < line);
    }

    #[test]
    fn a_parameter_is_walked_with_a_placeholder() {
        let mut asked = spec();
        asked.path = "/stock/:id".into();
        let planned = plan(&asked, &sources()).expect("planned");
        assert!(planned.edits.iter().any(|edit| {
            edit.added
                .iter()
                .any(|line| line.contains("path: \"/stock/x\""))
        }));
    }

    #[test]
    fn what_already_exists_is_refused() {
        for (id, path) in [("floor", "/stock"), ("stockCount", "/kds")] {
            let mut asked = spec();
            asked.id = id.into();
            asked.path = path.into();
            assert!(plan(&asked, &sources()).is_err(), "{id} {path}");
        }
        let mut today = spec();
        today.id = "today".into();
        today.path = "/today-again".into();
        assert!(plan(&today, &sources()).is_err(), "today.title exists");
    }

    #[test]
    fn every_catalogue_needs_its_title_and_no_title_is_written_for_a_locale_with_none() {
        let mut missing = spec();
        missing.titles.clear();
        let refused = plan(&missing, &sources())
            .err()
            .map(|error| error.to_string());
        assert!(
            refused
                .as_deref()
                .unwrap_or_default()
                .contains("--title-vi"),
            "{refused:?}"
        );
        let mut stray = spec();
        stray
            .titles
            .insert("fr".to_owned(), "Inventaire".to_owned());
        assert!(plan(&stray, &sources()).is_err(), "no fr.json");
        let third = [
            CATALOGUES.clone(),
            vec![("ja".to_owned(), "{\n  \"a\": \"b\"\n}\n".to_owned())],
        ]
        .concat();
        let with_ja = Sources {
            catalogues: &third,
            ..sources()
        };
        let refused = plan(&spec(), &with_ja).err().map(|error| error.to_string());
        assert!(
            refused
                .as_deref()
                .unwrap_or_default()
                .contains("--title-ja"),
            "{refused:?}"
        );
    }

    #[test]
    fn malformed_values_are_refused() {
        for (id, path, title) in [
            ("Stock", "/stock", "Stock"),
            ("stock_count", "/stock", "Stock"),
            ("stockCount", "stock", "Stock"),
            ("stockCount", "/Stock", "Stock"),
            ("stockCount", "/stock", "a \"quoted\" title"),
            ("stockCount", "/stock", ""),
        ] {
            let asked = Spec {
                id: id.into(),
                path: path.into(),
                title: title.into(),
                titles: BTreeMap::new(),
            };
            assert!(plan(&asked, &sources()).is_err(), "{id} {path} {title}");
        }
    }

    #[test]
    fn only_the_till_is_generated_and_every_flag_is_read() {
        let read = |list: &[&str]| {
            let owned: Vec<String> = list.iter().map(|a| (*a).to_owned()).collect();
            let refs: Vec<&String> = owned.iter().collect();
            spec_from_args(&refs).map(|spec| spec.titles.get("vi").cloned())
        };
        assert!(read(&["s", "--app", "dashboard", "--path", "/s", "--title", "S"]).is_err());
        assert!(read(&["s", "--path", "/s", "--title", "S"]).is_err());
        assert_eq!(
            read(&[
                "s",
                "--app",
                "ui",
                "--path",
                "/s",
                "--title",
                "S",
                "--title-vi",
                "V"
            ])
            .ok(),
            Some(Some("V".to_owned()))
        );
        assert!(
            read(&[
                "s", "--app", "ui", "--path", "/s", "--title", "S", "--colour", "x"
            ])
            .is_err()
        );
    }
}
