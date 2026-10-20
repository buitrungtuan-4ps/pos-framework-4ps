// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask new screen --app dashboard <id>`: a console screen, wired everywhere a screen is
//! named ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md)
//! decision 5).
//!
//! It writes `dashboard/src/screens/<Component>.tsx`, a screen with its title and nothing else, and
//! `dashboard/tests/<kebab-id>.test.tsx`, a vitest test that it draws that title. It adds:
//! - its entry last in `SCREENS` (`dashboard/src/state/screens.ts`), with its scope and icon, and its
//!   id last in the items of its nav group in `NAV_GROUPS`;
//! - `App.tsx`: the component's `lazy()` block after the last one, and its `COMPONENTS` entry;
//! - every string catalogue under `dashboard/src/i18n/`, the key `nav.<id>`, as the till's generator
//!   does: `--title` for `en.json` and `--title-<locale>` for each other, required.
//!
//! The scope, the icon and the nav group are design choices, so each is a required flag and none
//! has a default. The icon must be one `ICON_NAMES` already holds, since a glyph is drawn geometry
//! and not an input, and a group already holding six screens, the ceiling, is refused.

use std::collections::BTreeMap;
use std::path::Path;

use super::screen::{
    Spec, check, insert_after_opening, insert_before, pascal, read_catalogues, strings,
};
use super::{Edit, Plan, render};
use crate::checks::Error;

const SCREEN: &str = include_str!("../../templates/screen/dashboard/Screen.tsx.tmpl");
const TEST: &str = include_str!("../../templates/screen/dashboard/screen.test.tsx.tmpl");

const SCREENS: &str = "dashboard/src/state/screens.ts";
const APP: &str = "dashboard/src/App.tsx";
const ICONS: &str = "dashboard/src/components/icons.tsx";
const I18N: &str = "dashboard/src/i18n";

/// The most screens a nav group holds, so it is readable at a glance (`NAV_GROUPS`).
const GROUP_CEILING: usize = 6;

/// The context a console screen needs before it can do anything useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Under `/t/<tenant>`, needing a tenant.
    Tenant,
    /// Under `/t/<tenant>`, needing a store as well.
    Store,
    /// Console-level, at a bare path, with no tenant to carry.
    Console,
}

/// The design choices a console screen needs beyond a till screen's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Design {
    /// `--scope`.
    pub scope: Scope,
    /// `--icon`, a name in `ICON_NAMES`.
    pub icon: String,
    /// `--nav-group`, the `<name>` of the group keyed `nav.group.<name>`.
    pub group: String,
}

/// Plans the screen `spec` describes with the design flags in `flags`, against the repository at
/// `root`.
///
/// # Errors
///
/// [`Error`] for a missing or malformed design flag, or anything [`plan`] refuses.
pub fn plan_from_args(
    root: &Path,
    spec: &Spec,
    mut flags: BTreeMap<String, String>,
) -> Result<Plan, Error> {
    let mut required = |flag: &str| {
        flags.remove(flag).ok_or_else(|| {
            format!("`--{flag}` is required for `--app dashboard`: it is a design choice")
        })
    };
    let scope = match required("scope")?.as_str() {
        "tenant" => Scope::Tenant,
        "store" => Scope::Store,
        "console" => Scope::Console,
        other => {
            return Err(format!("`--scope {other}` is not one of tenant, store or console").into());
        }
    };
    let design = Design {
        scope,
        icon: required("icon")?,
        group: required("nav-group")?,
    };
    let catalogues = read_catalogues(root, I18N)?;
    let read = |path: &str| std::fs::read_to_string(root.join(path));
    plan(
        spec,
        &design,
        &Sources {
            screens: &read(SCREENS)?,
            app: &read(APP)?,
            icons: &read(ICONS)?,
            catalogues: &catalogues,
        },
    )
}

/// The files a console screen is wired through, as they stand.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// `dashboard/src/state/screens.ts`.
    pub screens: &'a str,
    /// `dashboard/src/App.tsx`.
    pub app: &'a str,
    /// `dashboard/src/components/icons.tsx`, for `ICON_NAMES`.
    pub icons: &'a str,
    /// Every catalogue under `dashboard/src/i18n/`, as (locale, contents).
    pub catalogues: &'a [(String, String)],
}

/// The console screen `spec` describes, as `design` places it, wired through `sources`.
///
/// # Errors
///
/// [`Error`] if a value is malformed, the id, path or string already exists, the icon or the group
/// does not, the group is full, or a file lacks the place a line goes.
pub fn plan(spec: &Spec, design: &Design, sources: &Sources<'_>) -> Result<Plan, Error> {
    check(spec)?;
    let component = pascal(&spec.id);
    let key = format!("nav.{}", spec.id);
    if sources.screens.contains(&format!("\n  {}: {{", spec.id)) {
        return Err(format!("`{}` is already a screen", spec.id).into());
    }
    if sources
        .screens
        .contains(&format!("path: \"{}\"", spec.path))
    {
        return Err(format!("a screen already has the path `{}`", spec.path).into());
    }
    let mut edits = vec![
        screens_edit(spec, design, &key, sources)?,
        app_edit(sources.app, &spec.id, &component)?,
    ];
    edits.extend(strings(spec, I18N, sources.catalogues, &key)?);
    let values = [
        ("component", component.as_str()),
        ("key", key.as_str()),
        ("path", spec.path.as_str()),
        ("title", spec.title.as_str()),
    ];
    Ok(Plan {
        files: vec![
            (
                format!("dashboard/src/screens/{component}.tsx"),
                render(SCREEN, &values)?,
            ),
            (
                format!("dashboard/tests/{}.test.tsx", kebab(&spec.id)),
                render(TEST, &values)?,
            ),
        ],
        member: None,
        package: component,
        next: vec!["`pnpm build`, `pnpm lint` and `pnpm test` in `dashboard/` check it".to_owned()],
        edits,
    })
}

/// `screens.ts` with the screen's entry last in `SCREENS` and its id last in its nav group, once
/// the icon is known to be one the console draws.
fn screens_edit(
    spec: &Spec,
    design: &Design,
    key: &str,
    sources: &Sources<'_>,
) -> Result<Edit, Error> {
    let icons = icon_names(sources.icons)?;
    if !icons.contains(&design.icon.as_str()) {
        return Err(format!(
            "`--icon {}` is not in `ICON_NAMES` ({ICONS}); it has {}",
            design.icon,
            icons.join(", ")
        )
        .into());
    }
    let scope = match design.scope {
        Scope::Tenant => "scope: \"tenant\", tenantScoped: true",
        Scope::Store => "scope: \"store\", tenantScoped: true",
        Scope::Console => "tenantScoped: false",
    };
    let entry = format!(
        "  {}: {{ path: \"{}\", key: \"{key}\", {scope}, icon: \"{}\" }},",
        spec.id, spec.path, design.icon
    );
    let with_entry = insert_before(
        SCREENS,
        sources.screens,
        "} as const satisfies Record<string, Screen>;",
        std::slice::from_ref(&entry),
    )?;
    let (contents, item) = filed(&with_entry.contents, &design.group, &spec.id)?;
    Ok(Edit {
        path: SCREENS.to_owned(),
        contents,
        added: vec![entry, item],
    })
}

/// The names in `ICON_NAMES`, one quoted name a line between its opening and `] as const;`.
fn icon_names(icons: &str) -> Result<Vec<&str>, Error> {
    let start = icons
        .find("export const ICON_NAMES = [\n")
        .ok_or_else(|| format!("`{ICONS}` has no `ICON_NAMES` list"))?;
    let list = icons
        .get(start..)
        .and_then(|rest| rest.split_once("\n] as const;"))
        .map(|(list, _)| list)
        .ok_or_else(|| format!("`{ICONS}`'s `ICON_NAMES` list is not closed"))?;
    Ok(list
        .lines()
        .skip(1)
        .filter_map(|line| {
            line.trim()
                .strip_suffix(',')?
                .strip_prefix('"')?
                .strip_suffix('"')
        })
        .collect())
}

/// `screens` with `id` added last to the items of the nav group keyed `nav.group.<group>`, and the
/// line that now holds those items.
fn filed(screens: &str, group: &str, id: &str) -> Result<(String, String), Error> {
    let groups = screens
        .find("export const NAV_GROUPS")
        .ok_or_else(|| format!("`{SCREENS}` has no `NAV_GROUPS`"))?;
    let named = format!("key: \"nav.group.{group}\",");
    let at = screens
        .get(groups..)
        .and_then(|rest| rest.find(&named))
        .ok_or_else(|| {
            format!("`--nav-group {group}` is not a nav group: `{SCREENS}` has no `{named}`")
        })?
        + groups;
    let open = screens
        .get(at..)
        .and_then(|rest| rest.find("items: ["))
        .ok_or_else(|| format!("the `{group}` nav group has no `items: [`"))?
        + at
        + "items: [".len();
    let close = screens
        .get(open..)
        .and_then(|rest| rest.find(']'))
        .ok_or_else(|| format!("the `{group}` nav group's items are not closed"))?
        + open;
    let items = screens.get(open..close).unwrap_or_default();
    let count = items
        .split(',')
        .filter(|item| !item.trim().is_empty())
        .count();
    if count >= GROUP_CEILING {
        return Err(format!(
            "the `{group}` nav group is full: it holds {count} screens, and {GROUP_CEILING} is \
             the ceiling; choose another group or move a screen out first"
        )
        .into());
    }
    let added = if count == 0 {
        format!("\"{id}\"")
    } else {
        format!(", \"{id}\"")
    };
    let mut contents = screens.to_owned();
    contents.insert_str(close, &added);
    let line_start = contents
        .get(..open)
        .and_then(|before| before.rfind('\n'))
        .map_or(0, |at| at + 1);
    let line = contents
        .get(line_start..)
        .and_then(|rest| rest.lines().next())
        .unwrap_or_default()
        .to_owned();
    Ok((contents, line))
}

/// `App.tsx` with the screen's `lazy()` block after the last one, and its `COMPONENTS` entry last.
fn app_edit(text: &str, id: &str, component: &str) -> Result<Edit, Error> {
    let block = [
        format!("const {component} = lazy(() =>"),
        format!(
            "  import(\"./screens/{component}\").then((module) => ({{ default: module.{component} }})),"
        ),
        ");".to_owned(),
    ];
    let last = text
        .rfind(" = lazy(() =>\n")
        .ok_or_else(|| format!("`{APP}` has no `lazy()` screen to add one after"))?;
    let end = text
        .get(last..)
        .and_then(|rest| rest.find("\n);\n"))
        .ok_or_else(|| format!("`{APP}` ends inside a `lazy()` block"))?
        + last
        + "\n);\n".len();
    let mut contents = text.to_owned();
    contents.insert_str(end, &format!("{}\n", block.join("\n")));
    let entry = format!("  {id}: {component},");
    let with_entry = insert_after_opening(
        APP,
        &contents,
        "const COMPONENTS: Record<ScreenId, Component> = {",
        "};",
        &entry,
    )?;
    let mut added = block.to_vec();
    added.push(entry);
    Ok(Edit {
        path: APP.to_owned(),
        contents: with_entry.contents,
        added,
    })
}

/// `stockCount` → `stock-count`.
fn kebab(id: &str) -> String {
    let mut out = String::new();
    for c in id.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::LazyLock;

    use super::{Design, Scope, Sources, Spec, plan};

    /// The catalogues as they stand, Vietnamese listed first to show the order is the generator's.
    static CATALOGUES: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
        vec![
            (
                "vi".to_owned(),
                include_str!("../../../dashboard/src/i18n/vi.json").to_owned(),
            ),
            (
                "en".to_owned(),
                include_str!("../../../dashboard/src/i18n/en.json").to_owned(),
            ),
        ]
    });

    fn sources() -> Sources<'static> {
        Sources {
            screens: include_str!("../../../dashboard/src/state/screens.ts"),
            app: include_str!("../../../dashboard/src/App.tsx"),
            icons: include_str!("../../../dashboard/src/components/icons.tsx"),
            catalogues: &CATALOGUES,
        }
    }

    /// The golden input.
    fn spec() -> Spec {
        Spec {
            id: "stockCount".into(),
            path: "/stock".into(),
            title: "Stock count".into(),
            titles: BTreeMap::from([("vi".to_owned(), "Kiểm kho".to_owned())]),
        }
    }

    fn design() -> Design {
        Design {
            scope: Scope::Store,
            icon: "package".into(),
            group: "overview".into(),
        }
    }

    fn contents(planned: &super::Plan, path: &str) -> String {
        planned
            .edits
            .iter()
            .find(|edit| edit.path == path)
            .map(|edit| edit.contents.clone())
            .unwrap_or_default()
    }

    #[test]
    fn the_screen_its_test_and_its_lines_are_the_golden_ones() {
        let planned = plan(&spec(), &design(), &sources()).expect("planned");
        let files: Vec<(&str, &str)> = planned
            .files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        assert_eq!(
            files,
            [
                (
                    "dashboard/src/screens/StockCount.tsx",
                    include_str!("../../tests/golden/new-screen/dashboard/StockCount.tsx.golden"),
                ),
                (
                    "dashboard/tests/stock-count.test.tsx",
                    include_str!(
                        "../../tests/golden/new-screen/dashboard/stock-count.test.tsx.golden"
                    ),
                ),
            ]
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
        let golden: Vec<&str> =
            include_str!("../../tests/golden/new-screen/dashboard/edits.golden")
                .lines()
                .collect();
        assert_eq!(added, golden);
        assert!(planned.member.is_none());
    }

    #[test]
    fn each_line_lands_where_it_belongs() {
        let planned = plan(&spec(), &design(), &sources()).expect("planned");
        let screens = contents(&planned, "dashboard/src/state/screens.ts");
        assert!(screens.contains("icon: \"package\" },\n} as const satisfies"));
        let groups = screens.find("export const NAV_GROUPS").expect("groups");
        let item = screens
            .find("\"alerts\", \"stockCount\"] },")
            .expect("filed");
        assert!(groups < item);
        let app = contents(&planned, "dashboard/src/App.tsx");
        let block = app.find("const StockCount = lazy(() =>").expect("block");
        let webhooks = app.find("const Webhooks = lazy(() =>").expect("webhooks");
        let guarded = app.find("function Guarded").expect("guarded");
        assert!(webhooks < block && block < guarded);
        let table = app.find("const COMPONENTS").expect("table");
        let entry = app.find("  stockCount: StockCount,\n};").expect("entry");
        assert!(table < entry);
        for (path, title) in [
            ("dashboard/src/i18n/en.json", "Stock count"),
            ("dashboard/src/i18n/vi.json", "Kiểm kho"),
        ] {
            let parsed: serde_json::Value =
                serde_json::from_str(&contents(&planned, path)).expect("still JSON");
            assert_eq!(parsed["nav.stockCount"], title, "{path}");
        }
    }

    #[test]
    fn the_scope_is_written_as_the_registry_spells_it() {
        for (scope, written) in [
            (Scope::Tenant, "scope: \"tenant\", tenantScoped: true, icon"),
            (Scope::Store, "scope: \"store\", tenantScoped: true, icon"),
            (
                Scope::Console,
                "key: \"nav.stockCount\", tenantScoped: false, icon",
            ),
        ] {
            let planned =
                plan(&spec(), &Design { scope, ..design() }, &sources()).expect("planned");
            let entry = planned
                .edits
                .first()
                .and_then(|edit| edit.added.first())
                .cloned()
                .unwrap_or_default();
            assert!(entry.contains(written), "{scope:?}: {entry}");
        }
    }

    #[test]
    fn a_multi_line_group_is_filed_on_its_items_line() {
        let mut asked = design();
        asked.group = "compliance".into();
        let planned = plan(&spec(), &asked, &sources()).expect("planned");
        assert_eq!(
            planned
                .edits
                .first()
                .and_then(|edit| edit.added.get(1))
                .map(String::as_str),
            Some(
                "  { key: \"nav.group.compliance\", items: [\"audit\", \"subjects\", \
                 \"chainFindings\", \"stockCount\"] },"
            )
        );
        let wrapped = sources().screens.replace(
            "{ key: \"nav.group.account\", items: [\"mySessions\", \"mySecurity\"] },",
            "{\n    key: \"nav.group.account\",\n    items: [\"mySessions\"],\n  },",
        );
        let planned = plan(
            &spec(),
            &Design {
                group: "account".into(),
                ..design()
            },
            &Sources {
                screens: &wrapped,
                ..sources()
            },
        )
        .expect("planned");
        assert_eq!(
            planned
                .edits
                .first()
                .and_then(|edit| edit.added.get(1))
                .map(String::as_str),
            Some("    items: [\"mySessions\", \"stockCount\"],")
        );
    }

    #[test]
    fn a_full_group_is_refused() {
        for group in ["menu", "operations", "estate", "settings"] {
            let refused = plan(
                &spec(),
                &Design {
                    group: group.into(),
                    ..design()
                },
                &sources(),
            )
            .err()
            .map(|error| error.to_string());
            assert!(
                refused.as_deref().unwrap_or_default().contains("is full"),
                "{group}: {refused:?}"
            );
        }
        let five = sources().screens.replace(
            "items: [\"admins\", \"apiKeys\", \"webhooks\", \"integrations\"]",
            "items: [\"admins\", \"apiKeys\", \"webhooks\", \"integrations\", \"audit\"]",
        );
        let at_five = Sources {
            screens: &five,
            ..sources()
        };
        let access = Design {
            group: "access".into(),
            ..design()
        };
        assert!(
            plan(&spec(), &access, &at_five).is_ok(),
            "five has room for one"
        );
    }

    #[test]
    fn an_unknown_icon_or_group_is_refused() {
        for (icon, group) in [
            ("pizza", "overview"),
            ("package", "kitchen"),
            ("package", ""),
        ] {
            let asked = Design {
                icon: icon.into(),
                group: group.into(),
                ..design()
            };
            assert!(plan(&spec(), &asked, &sources()).is_err(), "{icon} {group}");
        }
    }

    #[test]
    fn what_already_exists_is_refused() {
        for (id, path, why) in [
            ("alerts", "/stock", "is already a screen"),
            ("stockCount", "/alerts", "already has the path"),
        ] {
            let mut asked = spec();
            asked.id = id.into();
            asked.path = path.into();
            let refused = plan(&asked, &design(), &sources())
                .err()
                .map(|error| error.to_string());
            assert!(
                refused.as_deref().unwrap_or_default().contains(why),
                "{id} {path}: {refused:?}"
            );
        }
        let taken = sources()
            .screens
            .replace("  storeHub: {", "  storeHubRenamed: {");
        let mut hub = spec();
        hub.id = "storeHub".into();
        hub.path = "/hub-again".into();
        let refused = plan(
            &hub,
            &design(),
            &Sources {
                screens: &taken,
                ..sources()
            },
        )
        .err()
        .map(|error| error.to_string());
        assert!(
            refused
                .as_deref()
                .unwrap_or_default()
                .contains("nav.storeHub"),
            "the string exists: {refused:?}"
        );
    }

    #[test]
    fn the_design_flags_are_read_and_each_is_required() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("root");
        let entry = |flags: &[(&str, &str)]| {
            let flags: BTreeMap<String, String> = flags
                .iter()
                .map(|(flag, value)| ((*flag).to_owned(), (*value).to_owned()))
                .collect();
            super::plan_from_args(root, &spec(), flags)
                .map(|planned| {
                    planned
                        .edits
                        .first()
                        .and_then(|edit| edit.added.first())
                        .cloned()
                        .unwrap_or_default()
                })
                .map_err(|error| error.to_string())
        };
        let all = [("icon", "package"), ("nav-group", "overview")];
        for (scope, written) in [
            ("tenant", "scope: \"tenant\""),
            ("store", "scope: \"store\""),
            ("console", "tenantScoped: false"),
        ] {
            let read = entry(&[all.as_slice(), &[("scope", scope)]].concat());
            assert!(
                read.as_deref().unwrap_or_default().contains(written),
                "{scope}: {read:?}"
            );
        }
        let refused = entry(&[all.as_slice(), &[("scope", "global")]].concat());
        assert!(refused.is_err(), "{refused:?}");
        for missing in ["scope", "icon", "nav-group"] {
            let given: Vec<(&str, &str)> = [all.as_slice(), &[("scope", "store")]]
                .concat()
                .into_iter()
                .filter(|(flag, _)| *flag != missing)
                .collect();
            let refused = entry(&given).err().unwrap_or_default();
            assert!(
                refused.contains(&format!("--{missing}")),
                "{missing}: {refused}"
            );
        }
    }

    #[test]
    fn every_catalogue_needs_its_title() {
        let mut missing = spec();
        missing.titles.clear();
        let refused = plan(&missing, &design(), &sources())
            .err()
            .map(|error| error.to_string());
        assert!(
            refused
                .as_deref()
                .unwrap_or_default()
                .contains("--title-vi"),
            "{refused:?}"
        );
    }
}
