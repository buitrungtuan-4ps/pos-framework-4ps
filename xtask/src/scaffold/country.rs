// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask new country <cc>`: a country module under `countries/<cc>`, built on
//! `cargo xtask new crate`, wired into both binaries and passing the `Fiscalization` contract suite
//! from its first build ([ADR-0176](../../../docs/adr/0176-the-adapter-and-country-standard.md)).
//!
//! **A country's law is an input, never a default.** The currency, its exponent, whether prices
//! include tax, the cash rounding, the tax rates, the default retention and the language are each a
//! required flag, and the command refuses to run without one. The currency is read with
//! `CurrencyCode::parse` in the generated module, so `pos-proto` gains no constant.
//!
//! **Wiring, and nothing else.** Beyond the new directory and its `members` line it adds exactly the
//! lines ADR-0176 decision 4 names: a `country-<cc>` feature and an optional dependency in
//! `pos-edge`'s and `pos-cloud`'s manifests, and a `country_registry!` arm in each binary's
//! `countries.rs`. No default feature gains it. The README lines of the crates it depends on, and of
//! the two binaries that now depend on it, are left to the person; `xtask crates` prints each.

use std::collections::BTreeSet;
use std::path::Path;

use super::krate::{Parts, Spec, plan_with};
use super::{Edit, Plan};
use crate::checks::Error;
use crate::checks::crates::adr_numbers;

const README: &str = include_str!("../../templates/country/README.md.tmpl");
const LIB: &str = include_str!("../../templates/country/lib.rs.tmpl");
const CONTRACT: &str = include_str!("../../templates/country/contract.rs.tmpl");

/// The files each binary is wired through, by binary.
const BINARIES: [&str; 2] = ["crates/pos-edge", "crates/pos-cloud"];

/// The tax classes every country states, by their `pos_country::tax_class` function.
const CLASSES: [&str; 3] = ["food", "alcohol", "exempt"];

/// The sales channels, by flag name and `SalesChannel` variant.
const CHANNELS: [(&str, &str); 5] = [
    ("dine-in", "DineIn"),
    ("takeaway", "Takeaway"),
    ("delivery", "Delivery"),
    ("qr", "Qr"),
    ("api", "Api"),
];

/// What `cargo xtask new country` is asked for, every field from a flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    /// The ISO 3166-1 alpha-2 code, lower-case.
    pub cc: String,
    /// The name the console shows.
    pub display_name: String,
    /// The ISO 4217 code.
    pub currency: String,
    /// Decimal places of the currency's minor unit.
    pub exponent: u8,
    /// Whether menu prices include tax.
    pub prices_include_tax: bool,
    /// Cash rounding in minor units, or `None`.
    pub cash_rounding: Option<i64>,
    /// Basis points by (class, channel), every pair present.
    pub rates: Vec<(&'static str, &'static str, u32)>,
    /// The default retention in days.
    pub retention_days: u16,
    /// The default language tag.
    pub language: String,
}

/// Reads the arguments after `new country`, then plans against the repository at `root`.
///
/// # Errors
///
/// [`Error`] for a missing, repeated or malformed flag, or anything [`plan`] refuses.
pub fn plan_from_args(root: &Path, args: &[&String]) -> Result<Plan, Error> {
    let country = from_args(args)?;
    let codeowners = std::fs::read_to_string(root.join("CODEOWNERS"))?;
    let adrs = adr_numbers(&root.join("docs/adr"))?;
    let mut wiring = Vec::new();
    for binary in BINARIES {
        for file in ["Cargo.toml", "src/countries.rs"] {
            let path = format!("{binary}/{file}");
            wiring.push((path.clone(), std::fs::read_to_string(root.join(&path))?));
        }
    }
    plan(&country, &codeowners, &adrs, &wiring)
}

/// The flags, read into a [`Country`].
fn from_args(args: &[&String]) -> Result<Country, Error> {
    let mut cc = None;
    let mut flags: Vec<(&str, &str)> = Vec::new();
    let mut iter = args.iter();
    while let Some(argument) = iter.next() {
        if let Some(flag) = argument.strip_prefix("--") {
            let value = iter
                .next()
                .ok_or_else(|| format!("`--{flag}` needs a value"))?;
            flags.push((flag, value.as_str()));
        } else if cc.is_none() {
            cc = Some((*argument).clone());
        } else {
            return Err(format!("unexpected argument `{argument}`; {}", super::USAGE).into());
        }
    }
    let one = |name: &str| -> Result<&str, Error> {
        let mut found = flags.iter().filter(|(flag, _)| *flag == name);
        match (found.next(), found.next()) {
            (Some((_, value)), None) => Ok(value),
            (None, _) => {
                Err(format!("`--{name}` is required: the generator invents no value").into())
            }
            (Some(_), Some(_)) => Err(format!("`--{name}` is given twice").into()),
        }
    };
    if let Some((flag, _)) = flags.iter().find(|(flag, _)| !KNOWN.contains(flag)) {
        return Err(format!("unknown flag `--{flag}`; {}", super::USAGE).into());
    }
    let rate_flags: Vec<&str> = flags
        .iter()
        .filter(|(flag, _)| *flag == "tax-rate")
        .map(|(_, value)| *value)
        .collect();
    Ok(Country {
        cc: cc.ok_or(super::USAGE)?,
        display_name: one("display-name")?.to_owned(),
        currency: one("currency")?.to_owned(),
        exponent: number(one("exponent")?, "--exponent")?,
        prices_include_tax: boolean(one("prices-include-tax")?)?,
        cash_rounding: match one("cash-rounding")? {
            "none" => None,
            amount => Some(number(amount, "--cash-rounding")?),
        },
        rates: rates(&rate_flags)?,
        retention_days: number(one("retention-days")?, "--retention-days")?,
        language: one("language")?.to_owned(),
    })
}

/// The flags `new country` reads.
const KNOWN: [&str; 9] = [
    "display-name",
    "currency",
    "exponent",
    "prices-include-tax",
    "cash-rounding",
    "tax-rate",
    "retention-days",
    "language",
    "dry-run",
];

fn number<T: std::str::FromStr>(text: &str, flag: &str) -> Result<T, Error> {
    text.parse()
        .map_err(|_| format!("`{flag}` takes a whole number, not `{text}`").into())
}

fn boolean(text: &str) -> Result<bool, Error> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("`--prices-include-tax` takes true or false, not `{other}`").into()),
    }
}

/// `--tax-rate <class>=<bp>` for every channel, then `--tax-rate <class>@<channel>=<bp>` overrides.
/// Every class needs its every-channel rate, so no (class, channel) pair is left without one.
fn rates(flags: &[&str]) -> Result<Vec<(&'static str, &'static str, u32)>, Error> {
    let mut every = Vec::new();
    let mut overrides = Vec::new();
    for flag in flags {
        let (target, points) = flag.split_once('=').ok_or_else(|| {
            format!("`--tax-rate {flag}` is not `<class>[@<channel>]=<basis points>`")
        })?;
        let points: u32 = number(points, "--tax-rate")?;
        let (class, channel) = match target.split_once('@') {
            Some((class, channel)) => (class, Some(channel)),
            None => (target, None),
        };
        let class = *CLASSES
            .iter()
            .find(|known| **known == class)
            .ok_or_else(|| {
                format!(
                    "no tax class `{class}`; the classes are {}",
                    CLASSES.join(", ")
                )
            })?;
        match channel {
            None => every.push((class, points)),
            Some(channel) => {
                let (_, variant) = CHANNELS.iter().find(|(name, _)| *name == channel).ok_or_else(
                    || format!("no channel `{channel}`; the channels are dine-in, takeaway, delivery, qr, api"),
                )?;
                overrides.push((class, *variant, points));
            }
        }
    }
    let mut table = Vec::new();
    for class in CLASSES {
        let found: Vec<u32> = every
            .iter()
            .filter(|(c, _)| *c == class)
            .map(|(_, p)| *p)
            .collect();
        let [points] = found.as_slice() else {
            return Err(format!(
                "give `--tax-rate {class}=<basis points>` exactly once: every class needs a rate on \
                 every channel"
            )
            .into());
        };
        for (_, variant) in CHANNELS {
            let set = overrides
                .iter()
                .rev()
                .find(|(c, v, _)| *c == class && *v == variant);
            table.push((class, variant, set.map_or(*points, |(_, _, p)| *p)));
        }
    }
    Ok(table)
}

/// The module `country` describes, wired into the binaries whose files `wiring` holds.
///
/// # Errors
///
/// [`Error`] if a value is malformed, or a wiring file lacks the lines a country is added after or
/// already names this one.
pub fn plan(
    country: &Country,
    codeowners: &str,
    adrs: &BTreeSet<u32>,
    wiring: &[(String, String)],
) -> Result<Plan, Error> {
    check(country)?;
    let cc = country.cc.as_str();
    let name = format!("pos-country-{cc}");
    let spec = Spec {
        name: name.clone(),
        layer: "country".into(),
        purpose: format!("The country module for {}.", country.display_name),
        adrs: vec![27, 105, 176],
    };
    let mut plan = plan_with(&spec, codeowners, adrs, &parts(country, &name))?;
    for (path, text) in wiring {
        plan.edits.push(wire(path, text, cc)?);
    }
    Ok(plan)
}

/// Refuses a value the module could not hold as written.
fn check(country: &Country) -> Result<(), Error> {
    let lower =
        |text: &str, len: usize| text.len() == len && text.bytes().all(|b| b.is_ascii_lowercase());
    let upper =
        |text: &str, len: usize| text.len() == len && text.bytes().all(|b| b.is_ascii_uppercase());
    if !lower(&country.cc, 2) {
        return Err(format!(
            "`{}` is not a lower-case ISO 3166-1 alpha-2 code",
            country.cc
        )
        .into());
    }
    if !upper(&country.currency, 3) {
        return Err(format!(
            "`{}` is not an ISO 4217 code, three capitals",
            country.currency
        )
        .into());
    }
    let plain = |text: &str| {
        !text.trim().is_empty()
            && text.trim() == text
            && !text.contains(['\n', '"', '\\', '{', '}', '|'])
    };
    if !plain(&country.display_name) {
        return Err("`--display-name` must be one plain line".into());
    }
    let tag = !country.language.is_empty()
        && country.language.len() <= 35
        && country
            .language
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !tag {
        return Err(format!("`{}` is not a language tag", country.language).into());
    }
    if country.exponent > 4
        || country.cash_rounding.is_some_and(|step| step <= 0)
        || country.retention_days == 0
    {
        return Err(
            "`--exponent` is 0 to 4, `--cash-rounding` is above zero, and `--retention-days` \
                    is at least one"
                .into(),
        );
    }
    Ok(())
}

/// What a country adds to the plain crate.
fn parts(country: &Country, name: &str) -> Parts {
    let cc = country.cc.as_str();
    let rows: Vec<String> = country
        .rates
        .iter()
        .map(|(class, variant, points)| {
            format!(
                "        .with(\n            tax_class::{class}(),\n            SalesChannel::{variant},\n            \
                 TaxRate::from_basis_points({points}),\n        )"
            )
        })
        .collect();
    let summary: Vec<String> = country
        .rates
        .iter()
        .map(|(class, variant, points)| format!("{class} {variant} {points} bp"))
        .collect();
    let mut type_name = cc.to_owned();
    if let Some(first) = type_name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let dependency =
        |crate_name: &str| format!("{crate_name} = {{ path = \"../../crates/{crate_name}\" }}\n");
    let mut dependencies: String = ["pos-country", "pos-ports", "pos-proto"]
        .iter()
        .map(|d| dependency(d))
        .collect();
    dependencies.push_str("\n[dev-dependencies]\n");
    dependencies.extend(
        ["pos-contract-tests", "pos-fakes"]
            .iter()
            .map(|d| dependency(d)),
    );
    Parts {
        dependencies,
        depends:
            "`pos-country`, `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`, `pos-fakes`"
                .into(),
        readme: README,
        lib: LIB,
        dir: Some(format!("countries/{cc}")),
        extra: vec![("tests/contract.rs", CONTRACT)],
        values: vec![
            ("cc", cc.to_owned()),
            ("CC", cc.to_ascii_uppercase()),
            ("type_name", type_name),
            ("crate_ident", name.replace('-', "_")),
            ("display_name", country.display_name.clone()),
            ("currency", country.currency.clone()),
            ("exponent", country.exponent.to_string()),
            ("prices_include_tax", country.prices_include_tax.to_string()),
            (
                "prices_quoted",
                if country.prices_include_tax {
                    "inclusive of tax"
                } else {
                    "exclusive of tax"
                }
                .into(),
            ),
            (
                "cash_rounding",
                country
                    .cash_rounding
                    .map_or("None".into(), |step| format!("Some({step})")),
            ),
            (
                "cash_rounding_summary",
                country
                    .cash_rounding
                    .map_or("none".into(), |step| format!("to {step} minor units")),
            ),
            ("rate_rows", rows.join("\n")),
            ("rates_summary", summary.join(" · ")),
            ("retention_days", country.retention_days.to_string()),
            ("language", country.language.clone()),
        ],
        next: vec![
            format!("`cargo test -p {name}` runs the `Fiscalization` contract suite against it"),
            format!(
                "`cargo xtask crates` prints the Depends on line `pos-edge`'s and `pos-cloud`'s READMEs need, and the \
                 Used by line of `pos-country`, `pos-ports`, `pos-proto`, `pos-contract-tests` and \
                 `pos-fakes`; the generator \
                 leaves those files to you"
            ),
            format!(
                "`cargo xtask countries` checks the wiring; build a binary with `--features country-{cc}`"
            ),
        ],
        ..Parts::default()
    }
}

/// `text` with this country added after the last country already wired into it.
fn wire(path: &str, text: &str, cc: &str) -> Result<Edit, Error> {
    let lines: Vec<(&str, String)> = if path.ends_with("Cargo.toml") {
        vec![
            (
                "country-",
                format!("country-{cc} = [\"dep:pos-country-{cc}\"]"),
            ),
            (
                "pos-country-",
                format!(
                    "pos-country-{cc} = {{ path = \"../../countries/{cc}\", optional = true }}"
                ),
            ),
        ]
    } else {
        let type_name = format!(
            "{}{}",
            cc.get(..1).unwrap_or_default().to_ascii_uppercase(),
            cc.get(1..).unwrap_or_default()
        );
        vec![(
            "#[cfg(feature = \"country-",
            format!("#[cfg(feature = \"country-{cc}\")] pos_country_{cc}::{type_name},"),
        )]
    };
    let mut contents = text.to_owned();
    let mut added = Vec::new();
    for (prefix, line) in lines {
        if contents.lines().any(|existing| existing.trim() == line) {
            return Err(format!("`{path}` already wires country `{cc}`").into());
        }
        contents = insert_after_last(&contents, prefix, &line).ok_or_else(|| {
            format!("`{path}` has no line starting `{prefix}` to add country `{cc}` after")
        })?;
        added.push(line);
    }
    Ok(Edit {
        path: path.to_owned(),
        contents,
        added,
    })
}

/// `text` with `line` inserted after the last line whose trimmed start is `prefix`, at that line's
/// indent, skipping `default = …` lines; `None` if no line starts so.
fn insert_after_last(text: &str, prefix: &str, line: &str) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let at = lines.iter().rposition(|existing| {
        existing.trim_start().starts_with(prefix) && existing.contains('=')
    })?;
    let previous = lines.get(at)?;
    let indent = previous
        .get(..previous.len() - previous.trim_start().len())
        .unwrap_or_default();
    let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    out.insert(at + 1, format!("{indent}{line}"));
    Some(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{from_args, insert_after_last, plan, wire};

    const CODEOWNERS: &str = include_str!("../../../CODEOWNERS");

    /// The golden input, as its command line.
    const GOLDEN_ARGS: &[&str] = &[
        "xa",
        "--display-name",
        "Example country",
        "--currency",
        "XTS",
        "--exponent",
        "2",
        "--prices-include-tax",
        "false",
        "--cash-rounding",
        "5",
        "--tax-rate",
        "food=1000",
        "--tax-rate",
        "alcohol=1000",
        "--tax-rate",
        "exempt=0",
        "--tax-rate",
        "food@takeaway=800",
        "--retention-days",
        "365",
        "--language",
        "en",
    ];

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    fn country(list: &[&str]) -> Result<super::Country, super::Error> {
        let owned = args(list);
        let refs: Vec<&String> = owned.iter().collect();
        from_args(&refs)
    }

    /// The binaries' wiring files as they stand in the repository.
    fn wiring() -> Vec<(String, String)> {
        [
            (
                "crates/pos-edge/Cargo.toml",
                include_str!("../../../crates/pos-edge/Cargo.toml"),
            ),
            (
                "crates/pos-edge/src/countries.rs",
                include_str!("../../../crates/pos-edge/src/countries.rs"),
            ),
            (
                "crates/pos-cloud/Cargo.toml",
                include_str!("../../../crates/pos-cloud/Cargo.toml"),
            ),
            (
                "crates/pos-cloud/src/countries.rs",
                include_str!("../../../crates/pos-cloud/src/countries.rs"),
            ),
        ]
        .map(|(path, text)| (path.to_owned(), text.to_owned()))
        .to_vec()
    }

    fn adrs() -> BTreeSet<u32> {
        [27, 105, 176].into_iter().collect()
    }

    #[test]
    fn the_module_it_writes_is_the_golden_one() {
        let asked = country(GOLDEN_ARGS).expect("read");
        let planned = plan(&asked, CODEOWNERS, &adrs(), &wiring()).expect("planned");
        let golden = [
            (
                "countries/xa/Cargo.toml",
                include_str!("../../tests/golden/new-country/Cargo.toml.golden"),
            ),
            (
                "countries/xa/README.md",
                include_str!("../../tests/golden/new-country/README.md.golden"),
            ),
            (
                "countries/xa/src/lib.rs",
                include_str!("../../tests/golden/new-country/lib.rs.golden"),
            ),
            (
                "countries/xa/tests/contract.rs",
                include_str!("../../tests/golden/new-country/contract.rs.golden"),
            ),
        ];
        let files: Vec<(&str, &str)> = planned
            .files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        assert_eq!(files, golden);
        assert_eq!(planned.member.as_deref(), Some("countries/xa"));
        let added: Vec<String> = planned
            .edits
            .iter()
            .flat_map(|edit| {
                edit.added
                    .iter()
                    .map(move |line| format!("{}: {line}", edit.path))
            })
            .collect();
        let expected: Vec<&str> = include_str!("../../tests/golden/new-country/wiring.golden")
            .lines()
            .collect();
        assert_eq!(added, expected);
    }

    #[test]
    fn no_default_feature_gains_the_country() {
        let asked = country(GOLDEN_ARGS).expect("read");
        let planned = plan(&asked, CODEOWNERS, &adrs(), &wiring()).expect("planned");
        for edit in &planned.edits {
            for line in edit
                .contents
                .lines()
                .filter(|line| line.starts_with("default"))
            {
                assert!(!line.contains("country-xa"), "{}: {line}", edit.path);
            }
        }
    }

    #[test]
    fn each_line_goes_after_the_last_country_at_its_indent() {
        let manifest = "[features]\ndefault = [\"country-aa\"]\ncountry-aa = [\"dep:pos-country-aa\"]\n\n[dependencies]\n";
        let wired = insert_after_last(
            manifest,
            "country-",
            "country-xa = [\"dep:pos-country-xa\"]",
        )
        .expect("inserted");
        assert!(wired.contains(
            "country-aa = [\"dep:pos-country-aa\"]\ncountry-xa = [\"dep:pos-country-xa\"]\n\n"
        ));
        let registry =
            "    country_registry! {\n        #[cfg(feature = \"country-aa\")] a::A,\n    }\n";
        let edit = wire("x/src/countries.rs", registry, "xa").expect("wired");
        assert!(edit.contents.contains(
            "        #[cfg(feature = \"country-aa\")] a::A,\n        #[cfg(feature = \"country-xa\")] pos_country_xa::Xa,\n    }"
        ));
    }

    #[test]
    fn in_the_real_manifests_each_line_lands_in_its_section() {
        let asked = country(GOLDEN_ARGS).expect("read");
        let planned = plan(&asked, CODEOWNERS, &adrs(), &wiring()).expect("planned");
        for edit in planned
            .edits
            .iter()
            .filter(|edit| edit.path.ends_with("Cargo.toml"))
        {
            let at = |needle: &str| edit.contents.find(needle).expect(needle);
            let features = at("[features]");
            let dependencies = at("\n[dependencies]");
            let feature = at("country-xa = [");
            let dependency = at("pos-country-xa = {");
            assert!(
                features < feature && feature < dependencies,
                "{}: feature",
                edit.path
            );
            assert!(dependencies < dependency, "{}: dependency", edit.path);
            let previous = edit.contents.get(..dependency).unwrap_or_default();
            let last = previous.trim_end().lines().last().unwrap_or_default();
            assert!(
                last.starts_with("pos-country-"),
                "{}: after {last}",
                edit.path
            );
        }
    }

    #[test]
    fn a_country_already_wired_or_a_file_with_no_country_is_refused() {
        let wired = "country-xa = [\"dep:pos-country-xa\"]\npos-country-xa = { path = \"../../countries/xa\", optional = true }\n";
        assert!(wire("Cargo.toml", wired, "xa").is_err());
        assert!(wire("Cargo.toml", "[features]\n", "xa").is_err());
    }

    #[test]
    fn every_legal_value_is_required() {
        for flag in [
            "--display-name",
            "--currency",
            "--exponent",
            "--prices-include-tax",
            "--cash-rounding",
            "--retention-days",
            "--language",
        ] {
            let at = GOLDEN_ARGS
                .iter()
                .position(|a| *a == flag)
                .expect("in the golden input");
            let mut without = GOLDEN_ARGS.to_vec();
            without.drain(at..at + 2);
            assert!(country(&without).is_err(), "{flag} missing");
        }
    }

    #[test]
    fn every_class_needs_its_rate_once() {
        let without_exempt: Vec<&str> = GOLDEN_ARGS
            .iter()
            .copied()
            .filter(|a| *a != "exempt=0")
            .collect();
        let mut without = without_exempt.clone();
        let at = without
            .iter()
            .position(|a| *a == "food@takeaway=800")
            .expect("override")
            - 2;
        without.remove(at);
        assert!(country(&without).is_err(), "no exempt rate");
        let mut twice = GOLDEN_ARGS.to_vec();
        twice.extend(["--tax-rate", "food=900"]);
        assert!(country(&twice).is_err(), "food twice");
    }

    #[test]
    fn an_override_changes_its_channel_only() {
        let asked = country(GOLDEN_ARGS).expect("read");
        let food: Vec<(&str, u32)> = asked
            .rates
            .iter()
            .filter(|(class, _, _)| *class == "food")
            .map(|(_, channel, points)| (*channel, *points))
            .collect();
        assert_eq!(
            food,
            [
                ("DineIn", 1000),
                ("Takeaway", 800),
                ("Delivery", 1000),
                ("Qr", 1000),
                ("Api", 1000)
            ]
        );
        assert_eq!(asked.rates.len(), 15);
    }

    #[test]
    fn malformed_values_are_refused() {
        for (flag, value) in [
            ("--currency", "xts"),
            ("--currency", "XT"),
            ("--exponent", "5"),
            ("--exponent", "two"),
            ("--prices-include-tax", "yes"),
            ("--cash-rounding", "0"),
            ("--cash-rounding", "0.5"),
            ("--retention-days", "0"),
            ("--display-name", "a \"quoted\" name"),
            ("--language", "en GB"),
        ] {
            let mut asked = GOLDEN_ARGS.to_vec();
            let at = asked
                .iter()
                .position(|a| *a == flag)
                .expect("in the golden input");
            if let Some(slot) = asked.get_mut(at + 1) {
                *slot = value;
            }
            let refused = country(&asked)
                .and_then(|c| plan(&c, CODEOWNERS, &adrs(), &wiring()).map(|_| ()))
                .is_err();
            assert!(refused, "{flag} {value}");
        }
        let mut upper = GOLDEN_ARGS.to_vec();
        if let Some(cc) = upper.first_mut() {
            *cc = "XA";
        }
        let refused = country(&upper)
            .and_then(|c| plan(&c, CODEOWNERS, &adrs(), &wiring()).map(|_| ()))
            .is_err();
        assert!(refused, "an upper-case code");
    }
}
