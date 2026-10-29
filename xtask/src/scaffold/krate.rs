// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask new crate`: a workspace crate that `xtask crates` accepts as written — its manifest
//! with a complete contract card, a README with the six headings and the lines the check holds, and
//! a crate root.
//!
//! The owners are not an input. They are the team `CODEOWNERS` routes the crate's directory to, or
//! `@maintainers-architecture` where no line routes it, which is the rule every card already follows
//! (standardisation plan, 2026-10-13). An owner typed by hand could only disagree with that.

use std::collections::BTreeSet;
use std::path::Path;

use super::{Plan, render};
use crate::checks::Error;
use crate::checks::crates::{LAYERS, adr_numbers, codeowner_lines, route_for};

const MANIFEST: &str = include_str!("../../templates/crate/Cargo.toml.tmpl");
pub(super) const README: &str = include_str!("../../templates/crate/README.md.tmpl");
const LIB: &str = include_str!("../../templates/crate/lib.rs.tmpl");

/// The owner of a crate whose directory no `CODEOWNERS` line routes.
const DEFAULT_OWNER: &str = "@maintainers-architecture";

/// What `cargo xtask new crate` is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// The package name, in `kebab-case`.
    pub name: String,
    /// One of [`LAYERS`], except `country`.
    pub layer: String,
    /// The card's one-line purpose, also the manifest's description and the crate's doc line.
    pub purpose: String,
    /// The ADRs the card cites, each one under `docs/adr/`.
    pub adrs: Vec<u32>,
}

/// Reads the arguments after `new crate`, then plans against the repository at `root`.
///
/// # Errors
///
/// [`Error`] for a missing, repeated or unknown argument, or anything [`plan`] refuses.
pub fn plan_from_args(root: &Path, args: &[&String]) -> Result<Plan, Error> {
    let spec = spec_from_args(args)?;
    let codeowners = std::fs::read_to_string(root.join("CODEOWNERS"))?;
    let adrs = adr_numbers(&root.join("docs/adr"))?;
    plan(&spec, &codeowners, &adrs)
}

/// The [`Spec`] the arguments name.
fn spec_from_args(args: &[&String]) -> Result<Spec, Error> {
    let mut name = None;
    let mut layer = None;
    let mut purpose = None;
    let mut adrs = Vec::new();
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        let mut value = || {
            iter.next()
                .map(|value| (*value).clone())
                .ok_or_else(|| format!("`{argument}` needs a value"))
        };
        match argument.as_str() {
            "--layer" if layer.is_none() => layer = Some(value()?),
            "--purpose" if purpose.is_none() => purpose = Some(value()?),
            "--adr" => adrs.push(
                value()?
                    .trim_start_matches("ADR-")
                    .parse()
                    .map_err(|_| "`--adr` takes an ADR number, as in `--adr 176`")?,
            ),
            other if !other.starts_with("--") && name.is_none() => name = Some(other.to_owned()),
            other => return Err(format!("unexpected argument `{other}`; {}", super::USAGE).into()),
        }
    }
    Ok(Spec {
        name: name.ok_or(super::USAGE)?,
        layer: layer.ok_or("`--layer` is required")?,
        purpose: purpose.ok_or("`--purpose` is required")?,
        adrs,
    })
}

/// What a generator built on this one adds to the plain crate.
#[derive(Debug, Clone, Default)]
pub struct Parts {
    /// TOML lines after `[dependencies]`, each ending in a newline; empty for none.
    pub dependencies: String,
    /// The README's Depends on listing, as `xtask crates` words it.
    pub depends: String,
    /// The README's Owns section.
    pub owns: String,
    /// The README's Entry points section.
    pub entry_points: String,
    /// The template for `README.md`.
    pub readme: &'static str,
    /// The template for `src/lib.rs`.
    pub lib: &'static str,
    /// Where the crate goes, when not where its layer puts it: a country under `countries/`.
    pub dir: Option<String>,
    /// More files, each a path relative to the crate and its template.
    pub extra: Vec<(&'static str, &'static str)>,
    /// Values for the placeholders only these templates use.
    pub values: Vec<(&'static str, String)>,
    /// What to run once the files are written, after building the crate.
    pub next: Vec<String>,
}

impl Parts {
    /// A crate with no dependency and nothing in it yet.
    #[must_use]
    pub fn plain() -> Self {
        Self {
            depends: "no workspace crate".into(),
            owns: "- Nothing yet. List what this crate is the one place for.".into(),
            entry_points: "- [`src/lib.rs`](src/lib.rs)".into(),
            readme: README,
            lib: LIB,
            ..Self::default()
        }
    }
}

/// The plain crate `spec` describes; see [`plan_with`].
///
/// # Errors
///
/// As [`plan_with`].
pub fn plan(spec: &Spec, codeowners: &str, adrs: &BTreeSet<u32>) -> Result<Plan, Error> {
    plan_with(spec, codeowners, adrs, &Parts::plain())
}

/// The crate `spec` describes with `parts`, routed by `codeowners` and checked against the ADR
/// numbers in `adrs`.
///
/// # Errors
///
/// [`Error`] if the name is not `kebab-case`, the layer is unknown or is `country`, the purpose is
/// not one plain line, an ADR does not exist, or a template has a placeholder with no value.
pub fn plan_with(
    spec: &Spec,
    codeowners: &str,
    adrs: &BTreeSet<u32>,
    parts: &Parts,
) -> Result<Plan, Error> {
    check_name(&spec.name)?;
    let dir = match &parts.dir {
        Some(dir) => dir.clone(),
        None => directory(&spec.layer, &spec.name)?,
    };
    check_purpose(&spec.purpose)?;
    if let Some(missing) = spec.adrs.iter().find(|adr| !adrs.contains(adr)) {
        return Err(format!("ADR {missing} is not under docs/adr/").into());
    }
    let [adr_list, owners_toml, owners_line, to_root] = derived(spec, codeowners, &dir);
    let mut values = vec![
        ("name", spec.name.as_str()),
        ("layer", spec.layer.as_str()),
        ("purpose", spec.purpose.as_str()),
        ("adrs", &adr_list),
        ("owners_toml", &owners_toml),
        ("owners", &owners_line),
        ("to_root", &to_root),
        ("dependencies", &parts.dependencies),
        ("depends", &parts.depends),
        ("owns", &parts.owns),
        ("entry_points", &parts.entry_points),
    ];
    values.extend(
        parts
            .values
            .iter()
            .map(|(key, value)| (*key, value.as_str())),
    );
    let mut files = vec![
        (format!("{dir}/Cargo.toml"), render(MANIFEST, &values)?),
        (format!("{dir}/README.md"), render(parts.readme, &values)?),
        (format!("{dir}/src/lib.rs"), render(parts.lib, &values)?),
    ];
    for (path, template) in &parts.extra {
        files.push((format!("{dir}/{path}"), render(template, &values)?));
    }
    let mut next = vec![format!(
        "`cargo check -p {}` records it in Cargo.lock, and `cargo xtask gen-docs` adds it to the \
         crate graph",
        spec.name
    )];
    next.extend(parts.next.iter().cloned());
    Ok(Plan {
        files,
        member: Some(dir),
        package: spec.name.clone(),
        next,
        edits: Vec::new(),
    })
}

/// The values a crate's templates derive rather than take: its ADR list, its owners as TOML and as
/// a README line (the team `CODEOWNERS` routes `dir` to, else [`DEFAULT_OWNER`]), and the path from
/// `dir` back to the repository root.
fn derived(spec: &Spec, codeowners: &str, dir: &str) -> [String; 4] {
    let routes = codeowner_lines(codeowners);
    let owners: Vec<&str> =
        route_for(&routes, dir).map_or_else(|| vec![DEFAULT_OWNER], |(_, owners)| owners.to_vec());
    let adr_list = spec.adrs.iter().map(u32::to_string).collect::<Vec<_>>();
    let owners_toml = owners
        .iter()
        .map(|o| format!("\"{o}\""))
        .collect::<Vec<_>>();
    [
        adr_list.join(", "),
        owners_toml.join(", "),
        owners.join(", "),
        "../".repeat(dir.matches('/').count() + 1),
    ]
}

/// Where a crate of `layer` lives: adapters under `crates/adapters/`, the rest under `crates/`.
fn directory(layer: &str, name: &str) -> Result<String, Error> {
    match layer {
        "country" => Err(
            "a country module lives under countries/ and has its own generator, \
                          `cargo xtask new country`"
                .into(),
        ),
        "adapter" => Ok(format!("crates/adapters/{name}")),
        known if LAYERS.contains(&known) => Ok(format!("crates/{name}")),
        other => Err(format!("layer `{other}` is not one of {}", LAYERS.join(", ")).into()),
    }
}

/// A package name: lower-case ASCII letters, digits and single hyphens, starting with a letter.
fn check_name(name: &str) -> Result<(), Error> {
    let shaped = name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('-')
        && !name.contains("--")
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if shaped {
        Ok(())
    } else {
        Err(format!("`{name}` is not a kebab-case package name, as in `pos-cloud-floor`").into())
    }
}

/// A purpose the card, the manifest and a doc comment can all hold as written.
fn check_purpose(purpose: &str) -> Result<(), Error> {
    let plain = !purpose.trim().is_empty()
        && purpose.trim() == purpose
        && !purpose.contains(['\n', '\r', '"', '\\', '{', '}']);
    if plain {
        Ok(())
    } else {
        Err("`--purpose` must be one line, without quotes, backslashes or braces".into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Spec, plan, spec_from_args};

    const CODEOWNERS: &str = include_str!("../../../CODEOWNERS");

    fn adrs() -> BTreeSet<u32> {
        [170, 176].into_iter().collect()
    }

    fn spec(name: &str, layer: &str) -> Spec {
        Spec {
            name: name.into(),
            layer: layer.into(),
            purpose: "An example crate the generator's golden test renders.".into(),
            adrs: vec![170, 176],
        }
    }

    #[test]
    fn the_crate_it_writes_is_the_golden_one() {
        let plan = plan(&spec("pos-example", "tool"), CODEOWNERS, &adrs()).expect("planned");
        let golden = [
            (
                "crates/pos-example/Cargo.toml",
                include_str!("../../tests/golden/new-crate/Cargo.toml.golden"),
            ),
            (
                "crates/pos-example/README.md",
                include_str!("../../tests/golden/new-crate/README.md.golden"),
            ),
            (
                "crates/pos-example/src/lib.rs",
                include_str!("../../tests/golden/new-crate/lib.rs.golden"),
            ),
        ];
        let files: Vec<(&str, &str)> = plan
            .files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        assert_eq!(files, golden);
        assert_eq!(plan.member.as_deref(), Some("crates/pos-example"));
        assert_eq!(plan.package, "pos-example");
    }

    #[test]
    fn the_member_line_is_the_golden_one() {
        let workspace = "members = [\n    \"crates/pos-cloud\",\n    \"crates/pos-simulator\",\n    \
                         \"examples/minimal-edge\",\n    \"xtask\",\n]\n";
        let manifest = super::super::with_member(workspace, "crates/pos-example").expect("added");
        assert!(manifest.contains(include_str!("../../tests/golden/new-crate/members.golden")));
    }

    #[test]
    fn an_adapter_lives_under_adapters_and_links_one_level_further_up() {
        let plan = plan(&spec("printer-sandbox", "adapter"), CODEOWNERS, &adrs()).expect("planned");
        assert_eq!(
            plan.member.as_deref(),
            Some("crates/adapters/printer-sandbox")
        );
        let readme = &plan.files.get(1).expect("readme").1;
        assert!(readme.contains("[`AGENTS.md`](../../../AGENTS.md)"));
    }

    #[test]
    fn the_owners_are_the_team_codeowners_routes_the_directory_to() {
        let plan = plan(
            &spec("pos-cloud-example", "app"),
            "/crates/pos-cloud-example/ @team-x\n",
            &adrs(),
        )
        .expect("planned");
        let manifest = &plan.files.first().expect("manifest").1;
        assert!(manifest.contains("owners = [\"@team-x\"]"));
        let readme = &plan.files.get(1).expect("readme").1;
        assert!(readme.contains("**Layer** `app` · **Owners** @team-x"));
    }

    #[test]
    fn a_name_that_is_not_kebab_case_is_refused() {
        for name in ["Pos-x", "pos_x", "pos--x", "pos-x-", "1pos", ""] {
            assert!(
                plan(&spec(name, "tool"), CODEOWNERS, &adrs()).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_country_or_unknown_layer_is_refused() {
        for layer in ["country", "domain"] {
            assert!(
                plan(&spec("pos-x", layer), CODEOWNERS, &adrs()).is_err(),
                "{layer}"
            );
        }
    }

    #[test]
    fn a_purpose_a_manifest_cannot_hold_as_written_is_refused() {
        for purpose in ["", " padded", "two\nlines", "a \"quote\"", "a {{brace}}"] {
            let mut asked = spec("pos-x", "tool");
            asked.purpose = purpose.into();
            assert!(plan(&asked, CODEOWNERS, &adrs()).is_err(), "{purpose:?}");
        }
    }

    #[test]
    fn an_adr_that_does_not_exist_is_refused() {
        let mut asked = spec("pos-x", "tool");
        asked.adrs = vec![9999];
        assert!(plan(&asked, CODEOWNERS, &adrs()).is_err());
    }

    #[test]
    fn the_arguments_name_the_spec() {
        let args: Vec<String> = [
            "pos-x",
            "--layer",
            "tool",
            "--purpose",
            "One line.",
            "--adr",
            "ADR-0176",
        ]
        .map(str::to_owned)
        .to_vec();
        let refs: Vec<&String> = args.iter().collect();
        let asked = spec_from_args(&refs).expect("read");
        assert_eq!(asked.name, "pos-x");
        assert_eq!(asked.adrs, vec![176]);
        let missing: Vec<&String> = refs.get(..3).expect("three").to_vec();
        assert!(spec_from_args(&missing).is_err(), "no --purpose");
    }
}
