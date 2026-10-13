// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask code-stats --check`: the size ratchet
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 3).
//!
//! New code meets the size rules. Code that already breaks them is listed in
//! [`BASELINE`] with its figure, and the list only shrinks:
//!
//! - A Rust file outside `tests/` over 1,000 lines, or one under `tests/` over 1,500, fails.
//! - A function outside test code over 100 lines, or with more than 8 parameters, fails.
//! - Either way, unless the baseline lists it. A listed item may grow by at most [`SLACK_LINES`]
//!   lines past its recorded figure, and its parameters not at all.
//! - A listed item that is back within the rules, or no longer exists, fails too: remove it from the
//!   baseline. Only a pull request that touched that item can cause this, so its author is the
//!   person to tidy the entry, in the same change.
//!
//! A function is keyed by its file and its qualified name (`Edge::open_bill`), not its line, so an
//! edit above it does not churn the baseline.
//!
//! It also prints, as warnings that never fail, items in the files changed since the base branch
//! that pass the rules but exceed the warn levels: a file over 800 lines (1,200 under `tests/`), a
//! function over 60 lines or with more than 7 parameters.
//!
//! `--write-baseline` writes the baseline from the tree. It records current figures, so a raised
//! figure shows in the pull request's diff, and `CODEOWNERS` routes that file to the architecture
//! maintainers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;

use super::{FileStat, FunctionStat, Stats, collect};
use crate::Finding;
use crate::checks::{Error, base_ref, read_at_ref, repo_root};

/// The baseline file, from the repository root.
pub const BASELINE: &str = "tools/code-size-baseline.toml";

/// How far a listed item may grow past its recorded figure, in lines.
///
/// Ten lines lets a fix or a small field land in a file that is already too long, so that a
/// one-line bug fix in `http.rs` is never blocked. It is not enough to add a feature: anything
/// larger has to go in a new module, or shrink something first. Parameters get no slack.
pub const SLACK_LINES: usize = 10;

const RULE: &str = "code-size";

/// The limits for one kind of item: (warn above, fail above).
struct Limits {
    file: (usize, usize),
    test_file: (usize, usize),
    function_lines: (usize, usize),
    params: (usize, usize),
}

/// ADR-0170 decision 3's table.
const LIMITS: Limits = Limits {
    file: (800, 1_000),
    test_file: (1_200, 1_500),
    function_lines: (60, 100),
    params: (7, 8),
};

/// What `tools/code-size-baseline.toml` records.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Baseline {
    /// File path → lines.
    #[serde(default)]
    pub files: BTreeMap<String, usize>,
    /// `path::qualified name` → figures.
    #[serde(default)]
    pub functions: BTreeMap<String, Recorded>,
}

/// A listed function's figures.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
pub struct Recorded {
    /// Lines.
    pub lines: usize,
    /// Parameters.
    pub params: usize,
}

/// `--check`, `--write-baseline` and `--base <ref>`.
///
/// # Errors
///
/// [`Error`] if the tree or the baseline cannot be read or written.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let stats = collect(&root)?;
    if args.iter().any(|argument| argument == "--write-baseline") {
        std::fs::write(root.join(BASELINE), render(&stats))?;
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(root.join(BASELINE)).unwrap_or_default();
    let baseline: Baseline = toml::from_str(&text)?;
    let changed = changed_files(&root, &base_ref(args));
    for warning in warnings(&stats, &baseline, &changed) {
        println!("{warning}");
    }
    let mut findings = check(&stats, &baseline);
    if args.iter().any(|argument| argument == "--base") {
        findings.extend(baseline_growth(&root, args, &baseline)?);
    }
    Ok(findings)
}

/// The environment variable `pr.yml` passes the pull request's body in.
const PR_BODY: &str = "POS_PR_BODY";

/// Compares the baseline with the one at the merge base of `--base` and `HEAD`. Growth fails,
/// unless the pull request's body says `size-baseline: grow <reason>`. With
/// `--baseline-growth warn` (a push to `main`, compared with its parent) it only warns.
fn baseline_growth(root: &Path, args: &[String], now: &Baseline) -> Result<Vec<Finding>, Error> {
    let base = base_ref(args);
    let output = std::process::Command::new("git")
        .args(["merge-base", &base, "HEAD"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!("no merge base between `{base}` and HEAD; fetch it first").into());
    }
    let merge_base = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let Some(text) = read_at_ref(root, &merge_base, BASELINE)? else {
        return Ok(Vec::new()); // the pull request that introduces the baseline
    };
    let grown = grown_entries(&toml::from_str(&text)?, now);
    if grown.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(reason) = std::env::var(PR_BODY)
        .ok()
        .as_deref()
        .and_then(growth_allowed)
    {
        eprintln!("code-size: the baseline grows, allowed by the pull request: {reason}");
        return Ok(Vec::new());
    }
    let warn_only = args
        .windows(2)
        .any(|pair| pair[0] == "--baseline-growth" && pair[1] == "warn");
    if warn_only {
        for finding in &grown {
            println!(
                "::warning file={BASELINE},title={RULE}::{}",
                finding.message
            );
        }
        return Ok(Vec::new());
    }
    Ok(grown)
}

/// The reason in a `size-baseline: grow <reason>` line of a pull request's body, if it has one.
#[must_use]
pub fn growth_allowed(body: &str) -> Option<&str> {
    body.lines().find_map(|line| {
        let (key, rest) = line.trim_start().split_once(':')?;
        let rest = rest.trim_start();
        let reason = rest.get(4..)?;
        (key.eq_ignore_ascii_case("size-baseline")
            && rest.get(..4)?.eq_ignore_ascii_case("grow")
            && reason.starts_with(char::is_whitespace)
            && !reason.trim().is_empty())
        .then(|| reason.trim())
    })
}

/// One finding per baseline entry that is new since `base`, or whose figure grew.
#[must_use]
pub fn grown_entries(base: &Baseline, now: &Baseline) -> Vec<Finding> {
    let hint = "the baseline may only shrink: split the code instead, or write \
                `size-baseline: grow <reason>` in the pull-request body for the owner to see";
    let mut findings = Vec::new();
    for (path, lines) in &now.files {
        match base.files.get(path) {
            None => findings.push(Finding::new(
                BASELINE,
                RULE,
                format!("adds `{path}` ({lines} lines)"),
            )),
            Some(was) if lines > was => findings.push(Finding::new(
                BASELINE,
                RULE,
                format!("raises `{path}` from {was} to {lines} lines"),
            )),
            _ => {}
        }
    }
    for (key, figures) in &now.functions {
        match base.functions.get(key) {
            None => findings.push(Finding::new(
                BASELINE,
                RULE,
                format!(
                    "adds `{key}` ({} lines, {} parameters)",
                    figures.lines, figures.params
                ),
            )),
            Some(was) if figures.lines > was.lines || figures.params > was.params => {
                findings.push(Finding::new(
                    BASELINE,
                    RULE,
                    format!(
                        "raises `{key}` from {} lines, {} parameters to {}, {}",
                        was.lines, was.params, figures.lines, figures.params
                    ),
                ));
            }
            _ => {}
        }
    }
    findings
        .into_iter()
        .map(|finding| finding.with_hint(hint))
        .collect()
}

fn file_limits(file: &FileStat) -> (usize, usize) {
    if file.test {
        LIMITS.test_file
    } else {
        LIMITS.file
    }
}

fn breaks(function: &FunctionStat) -> bool {
    !function.test
        && (function.lines > LIMITS.function_lines.1 || function.params > LIMITS.params.1)
}

fn key(function: &FunctionStat) -> String {
    format!("{}::{}", function.path, function.qualified)
}

const HINT_SPLIT: &str = "split it by meaning (ADR-0170 decision 3); only code the baseline already lists may break the rule";
const HINT_GREW: &str =
    "move the new code to its own module or function instead; the baseline only shrinks";
const HINT_STALE: &str = "remove its entry from tools/code-size-baseline.toml, or regenerate it with `cargo xtask code-stats --write-baseline`";

/// Every rule violation. Pure, so the tests drive it with fixtures.
#[must_use]
pub fn check(stats: &Stats, baseline: &Baseline) -> Vec<Finding> {
    let mut findings = Vec::new();
    check_files(stats, baseline, &mut findings);
    check_functions(stats, baseline, &mut findings);
    findings
}

/// The file rules, for [`check`].
fn check_files(stats: &Stats, baseline: &Baseline, findings: &mut Vec<Finding>) {
    let mut seen_files = BTreeSet::new();
    for file in &stats.files {
        let (_, limit) = file_limits(file);
        seen_files.insert(file.path.as_str());
        match (file.lines > limit, baseline.files.get(&file.path)) {
            (true, None) => findings.push(
                Finding::new(
                    &file.path,
                    RULE,
                    format!("{} lines, over the limit of {limit}", file.lines),
                )
                .with_hint(HINT_SPLIT),
            ),
            (true, Some(&recorded)) if file.lines > recorded + SLACK_LINES => findings.push(
                Finding::new(
                    &file.path,
                    RULE,
                    format!(
                        "grew to {} lines from the baseline's {recorded} (slack {SLACK_LINES})",
                        file.lines
                    ),
                )
                .with_hint(HINT_GREW),
            ),
            (false, Some(_)) => findings.push(
                Finding::new(
                    &file.path,
                    RULE,
                    format!("is {} lines, within the limit of {limit}", file.lines),
                )
                .with_hint(HINT_STALE),
            ),
            _ => {}
        }
    }
    for path in baseline
        .files
        .keys()
        .filter(|path| !seen_files.contains(path.as_str()))
    {
        findings.push(
            Finding::new(path, RULE, "is in the baseline but no longer exists")
                .with_hint(HINT_STALE),
        );
    }
}

/// The function rules, for [`check`].
fn check_functions(stats: &Stats, baseline: &Baseline, findings: &mut Vec<Finding>) {
    let mut seen_functions = BTreeSet::new();
    for function in &stats.functions {
        let key = key(function);
        let listed = baseline.functions.get(&key);
        let at = u32::try_from(function.line).unwrap_or(u32::MAX);
        let figures = format!("{} lines, {} parameters", function.lines, function.params);
        match (breaks(function), listed) {
            (true, None) => findings.push(
                Finding::new(
                    &function.path,
                    RULE,
                    format!(
                        "`{}` is {figures}; the limits are {} lines and {} parameters",
                        function.qualified, LIMITS.function_lines.1, LIMITS.params.1
                    ),
                )
                .at_line(at)
                .with_hint(HINT_SPLIT),
            ),
            (true, Some(recorded))
                if function.lines > recorded.lines + SLACK_LINES
                    || function.params > recorded.params =>
            {
                findings.push(
                    Finding::new(
                        &function.path,
                        RULE,
                        format!(
                            "`{}` grew to {figures} from the baseline's {} lines, {} parameters \
                             (slack {SLACK_LINES} lines, no parameters)",
                            function.qualified, recorded.lines, recorded.params
                        ),
                    )
                    .at_line(at)
                    .with_hint(HINT_GREW),
                );
            }
            (false, Some(_)) => findings.push(
                Finding::new(
                    &function.path,
                    RULE,
                    format!("`{}` is {figures}, within the limits", function.qualified),
                )
                .at_line(at)
                .with_hint(HINT_STALE),
            ),
            _ => {}
        }
        if listed.is_some() {
            seen_functions.insert(key);
        }
    }
    for key in baseline
        .functions
        .keys()
        .filter(|key| !seen_functions.contains(*key))
    {
        let path = key.split("::").next().unwrap_or(key);
        findings.push(
            Finding::new(
                path,
                RULE,
                format!("`{key}` is in the baseline but no longer exists"),
            )
            .with_hint(HINT_STALE),
        );
    }
}

/// GitHub warning annotations for items in `changed` files that pass the rules but exceed the warn
/// levels. They never fail the check.
#[must_use]
pub fn warnings(stats: &Stats, baseline: &Baseline, changed: &BTreeSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for file in stats
        .files
        .iter()
        .filter(|file| changed.contains(&file.path))
    {
        let (warn, limit) = file_limits(file);
        if file.lines > warn && file.lines <= limit && !baseline.files.contains_key(&file.path) {
            out.push(format!(
                "::warning file={},title={RULE}::{} lines, over the warn level of {warn} (the limit is {limit})",
                file.path, file.lines
            ));
        }
    }
    for function in stats
        .functions
        .iter()
        .filter(|f| !f.test && changed.contains(&f.path))
    {
        let long = function.lines > LIMITS.function_lines.0;
        let wide = function.params > LIMITS.params.0;
        if (long || wide) && !breaks(function) {
            out.push(format!(
                "::warning file={},line={},title={RULE}::`{}` is {} lines with {} parameters, over the warn \
                 level of {} lines or {} parameters",
                function.path,
                function.line,
                function.qualified,
                function.lines,
                function.params,
                LIMITS.function_lines.0,
                LIMITS.params.0
            ));
        }
    }
    out
}

/// The files changed between `base` and the working tree, or none when git cannot say.
fn changed_files(root: &Path, base: &str) -> BTreeSet<String> {
    std::process::Command::new("git")
        .args(["diff", "--name-only", base])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A TOML basic string.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The baseline for `stats`: every item that breaks a rule, sorted, one per line.
#[must_use]
pub fn render(stats: &Stats) -> String {
    let mut out = String::from(
        "# Code that broke ADR-0170's size rules when the ratchet began. Each item may grow by at most\n\
         # 10 lines (no parameters), and is removed once it is within the rules. Generated by\n\
         # `cargo xtask code-stats --write-baseline`; `cargo xtask code-stats --check` holds the tree to it.\n\
         \n[files]\n",
    );
    let mut files: Vec<&FileStat> = stats
        .files
        .iter()
        .filter(|file| file.lines > file_limits(file).1)
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for file in files {
        let _ = writeln!(out, "{} = {}", quoted(&file.path), file.lines);
    }
    out.push_str("\n[functions]\n");
    let mut functions: Vec<&FunctionStat> = stats.functions.iter().filter(|f| breaks(f)).collect();
    functions.sort_by_key(|function| key(function));
    for function in functions {
        let _ = writeln!(
            out,
            "{} = {{ lines = {}, params = {} }}",
            quoted(&key(function)),
            function.lines,
            function.params
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{Baseline, Recorded, check, grown_entries, growth_allowed, render, warnings};
    use crate::code_stats::{FileStat, FunctionStat, Stats};

    fn file(path: &str, lines: usize) -> FileStat {
        FileStat {
            path: path.into(),
            lines,
            test: path.contains("/tests/"),
        }
    }

    fn function(path: &str, qualified: &str, lines: usize, params: usize) -> FunctionStat {
        FunctionStat {
            path: path.into(),
            name: qualified.rsplit("::").next().unwrap_or(qualified).into(),
            qualified: qualified.into(),
            line: 10,
            lines,
            params,
            test: false,
        }
    }

    fn stats(files: Vec<FileStat>, functions: Vec<FunctionStat>) -> Stats {
        Stats {
            files,
            functions,
            crates: Vec::new(),
            frontends: Vec::new(),
        }
    }

    fn messages(stats: &Stats, baseline: &Baseline) -> Vec<String> {
        check(stats, baseline)
            .into_iter()
            .map(|f| format!("{}: {}", f.file, f.message))
            .collect()
    }

    #[test]
    fn the_rules_pass_at_their_limits_and_fail_one_past_them() {
        let at = stats(
            vec![file("a.rs", 1_000), file("x/tests/t.rs", 1_500)],
            vec![function("a.rs", "f", 100, 8)],
        );
        assert_eq!(messages(&at, &Baseline::default()), Vec::<String>::new());
        let past = stats(
            vec![file("a.rs", 1_001), file("x/tests/t.rs", 1_501)],
            vec![
                function("a.rs", "Edge::f", 101, 8),
                function("a.rs", "g", 5, 9),
            ],
        );
        let found = messages(&past, &Baseline::default());
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(
            found.iter().any(|m| m.contains("`Edge::f` is 101 lines")),
            "{found:?}"
        );
    }

    #[test]
    fn a_listed_item_may_grow_by_the_slack_and_no_further() {
        let baseline = Baseline {
            files: BTreeMap::from([("big.rs".into(), 2_000)]),
            functions: BTreeMap::from([(
                "big.rs::Edge::f".into(),
                Recorded {
                    lines: 150,
                    params: 9,
                },
            )]),
        };
        let within = stats(
            vec![file("big.rs", 2_010)],
            vec![function("big.rs", "Edge::f", 160, 9)],
        );
        assert_eq!(messages(&within, &baseline), Vec::<String>::new());
        let grown = stats(
            vec![file("big.rs", 2_011)],
            vec![function("big.rs", "Edge::f", 161, 10)],
        );
        let found = messages(&grown, &baseline);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|m| m.contains("grew")), "{found:?}");
    }

    #[test]
    fn an_entry_that_is_fixed_or_gone_must_be_removed() {
        let baseline = Baseline {
            files: BTreeMap::from([("fixed.rs".into(), 2_000), ("gone.rs".into(), 1_200)]),
            functions: BTreeMap::from([
                (
                    "fixed.rs::f".into(),
                    Recorded {
                        lines: 150,
                        params: 3,
                    },
                ),
                (
                    "fixed.rs::gone".into(),
                    Recorded {
                        lines: 150,
                        params: 3,
                    },
                ),
            ]),
        };
        let now = stats(
            vec![file("fixed.rs", 900)],
            vec![function("fixed.rs", "f", 40, 3)],
        );
        let found = messages(&now, &baseline);
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(
            found
                .iter()
                .any(|m| m.starts_with("gone.rs: is in the baseline but no longer exists"))
        );
        assert!(
            found
                .iter()
                .any(|m| m.contains("`fixed.rs::gone` is in the baseline"))
        );
    }

    #[test]
    fn test_code_is_measured_against_its_own_limits() {
        let mut test_fn = function("a.rs", "tests::long", 400, 12);
        test_fn.test = true;
        assert_eq!(
            messages(&stats(vec![], vec![test_fn]), &Baseline::default()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_baseline_renders_what_breaks_and_reads_back() {
        let now = stats(
            vec![file("b.rs", 1_200), file("a.rs", 1_001), file("ok.rs", 999)],
            vec![
                function("a.rs", "<X as Y>::f", 120, 2),
                function("a.rs", "small", 10, 1),
            ],
        );
        let text = render(&now);
        assert!(
            text.contains("[files]\n\"a.rs\" = 1001\n\"b.rs\" = 1200\n"),
            "{text}"
        );
        assert!(
            text.contains("\"a.rs::<X as Y>::f\" = { lines = 120, params = 2 }"),
            "{text}"
        );
        let read: Baseline = toml::from_str(&text).expect("the rendered baseline parses");
        assert_eq!(
            messages(&now, &read),
            Vec::<String>::new(),
            "a fresh baseline passes"
        );
    }

    #[test]
    fn the_baseline_may_only_shrink_against_its_base() {
        let base = Baseline {
            files: BTreeMap::from([("a.rs".into(), 2_000), ("b.rs".into(), 1_500)]),
            functions: BTreeMap::from([(
                "a.rs::f".into(),
                Recorded {
                    lines: 150,
                    params: 3,
                },
            )]),
        };
        // Lower, removed or unchanged: fine.
        let shrunk = Baseline {
            files: BTreeMap::from([("a.rs".into(), 1_900)]),
            functions: BTreeMap::from([(
                "a.rs::f".into(),
                Recorded {
                    lines: 150,
                    params: 3,
                },
            )]),
        };
        assert!(grown_entries(&base, &shrunk).is_empty());
        let grown = Baseline {
            files: BTreeMap::from([("a.rs".into(), 2_001), ("c.rs".into(), 1_100)]),
            functions: BTreeMap::from([
                (
                    "a.rs::f".into(),
                    Recorded {
                        lines: 150,
                        params: 4,
                    },
                ),
                (
                    "a.rs::g".into(),
                    Recorded {
                        lines: 120,
                        params: 1,
                    },
                ),
            ]),
        };
        let found: Vec<String> = grown_entries(&base, &grown)
            .into_iter()
            .map(|f| f.message)
            .collect();
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(
            found
                .iter()
                .any(|m| m == "raises `a.rs` from 2000 to 2001 lines"),
            "{found:?}"
        );
        assert!(
            found.iter().any(|m| m.starts_with("adds `a.rs::g`")),
            "{found:?}"
        );
    }

    #[test]
    fn growth_is_allowed_only_by_a_grow_line_with_a_reason() {
        assert_eq!(
            growth_allowed("What\n\n  Size-Baseline: GROW  generated parser\n"),
            Some("generated parser")
        );
        assert_eq!(growth_allowed("size-baseline: grow"), None);
        assert_eq!(growth_allowed("size-baseline: grow   "), None);
        assert_eq!(growth_allowed("size-baseline: growth spurt"), None);
        assert_eq!(growth_allowed("changelog: none docs only"), None);
    }

    #[test]
    fn warnings_cover_changed_files_only_and_never_listed_or_failing_items() {
        let now = stats(
            vec![file("a.rs", 900), file("b.rs", 900)],
            vec![
                function("a.rs", "f", 70, 2),
                function("a.rs", "g", 10, 8),
                function("b.rs", "h", 70, 2),
            ],
        );
        let changed = BTreeSet::from(["a.rs".to_owned()]);
        let found = warnings(&now, &Baseline::default(), &changed);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(
            found.iter().all(|w| w.starts_with("::warning file=a.rs")),
            "{found:?}"
        );
    }
}
