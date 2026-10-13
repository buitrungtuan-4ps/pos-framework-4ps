// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask impact [--base <ref>]`: what a change touches and what CI runs for it, as Markdown
//! for one pull-request comment ([ADR-0171](../../docs/adr/0171-ci-runs-what-a-change-affects.md)
//! decision 2).
//!
//! The report has four parts:
//! - the path classes, from [`crate::affected::classify`];
//! - the affected crates, from [`crate::affected::affected_packages`];
//! - the till and console screens whose files changed, and whether code every screen uses did;
//! - the snapshots and generated documents the change touches or the affected crates check;
//! - every job in `.github/workflows/pr.yml`, and whether it runs or skips on a pull request.
//!
//! [`JOBS`] restates each job's `if:` in Rust. A test reads `pr.yml` and fails when a job is added,
//! removed or renamed, or its condition names other classes, so the report cannot drift from what CI
//! does.
//!
//! The report starts with [`MARKER`], so the workflow can find its earlier comment and edit it rather
//! than add one per push.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::Finding;
use crate::affected::{Packages, READ_BY, affected_packages, changed_files, classify};
use crate::checks::crates::{Crate, load};
use crate::checks::{Error, base_ref};

/// The first line of the report: how the workflow recognises its own comment.
pub const MARKER: &str = "<!-- xtask-impact -->";

/// When a job in `pr.yml` runs on a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Every pull request: the job has no class in its `if:`.
    Always,
    /// When any of these classes is true, or `changes` failed.
    Any(&'static [&'static str]),
}

/// A step inside a job that the job's own `env:` switches on, by class.
#[derive(Debug)]
pub struct Step {
    /// The `env:` variable the step's `if:` reads. Only the test that holds [`JOBS`] to `pr.yml`
    /// reads it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub env: &'static str,
    /// The classes that variable is true for.
    pub classes: &'static [&'static str],
    /// What the step does, for the report.
    pub what: &'static str,
}

/// A job in `pr.yml`.
#[derive(Debug)]
pub struct Job {
    /// The job id, which is also the check name.
    pub name: &'static str,
    /// When it runs.
    pub when: When,
    /// How many checks it reports: a matrix's size, else 1.
    pub count: usize,
    /// Steps it runs or skips by class.
    pub steps: &'static [Step],
    /// A note for the report.
    pub note: &'static str,
}

/// The two front ends: the name the report gives each, and its directory.
const FRONT_ENDS: [(&str, &str); 2] = [("till", "ui/"), ("console", "dashboard/")];

const RUST: &[&str] = &["rust"];
const UI: &[&str] = &["ui", "shared", "rust"];

/// Every job in `pr.yml`, in file order. A test holds this to the workflow.
pub const JOBS: [Job; 19] = [
    job("changes", When::Always, 1, &[], "computes the classes"),
    job("rules", When::Always, 1, &[], ""),
    job(
        "lints",
        When::Any(&["rust", "deploy"]),
        1,
        &[Step {
            env: "RUN_RUST",
            classes: RUST,
            what: "fmt and clippy",
        }],
        "",
    ),
    job("test-archive", When::Any(RUST), 1, &[], ""),
    job("test-shard", When::Any(RUST), 3, &[], "after test-archive"),
    job("test-doc", When::Any(RUST), 1, &[], ""),
    job(
        "test",
        When::Always,
        1,
        &[],
        "required; passes when the test jobs passed or had nothing to run",
    ),
    job(
        "build",
        When::Always,
        2,
        &[
            Step {
                env: "RUN_RUST",
                classes: RUST,
                what: "cargo build",
            },
            Step {
                env: "RUN_INSTALLERS",
                classes: &["rust", "deploy", "dashboard"],
                what: "Windows installer checks",
            },
        ],
        "",
    ),
    job("ui", When::Any(UI), 1, &[], ""),
    job("replay-build", When::Any(UI), 1, &[], ""),
    job("replay-shard", When::Any(UI), 3, &[], "after replay-build"),
    job(
        "replay",
        When::Always,
        1,
        &[],
        "required; passes when the replay jobs passed or had nothing to run",
    ),
    job(
        "console-replay",
        When::Any(&["dashboard", "shared", "rust"]),
        1,
        &[],
        "",
    ),
    job(
        "dashboard",
        When::Any(&["dashboard", "shared", "rust", "deploy"]),
        1,
        &[],
        "",
    ),
    job("deny", When::Any(RUST), 1, &[], ""),
    job("secrets", When::Always, 1, &[], ""),
    job("postgres", When::Any(RUST), 1, &[], ""),
    job("docs-gate", When::Always, 1, &[], ""),
    job("impact", When::Always, 1, &[], "this comment"),
];

const fn job(
    name: &'static str,
    when: When,
    count: usize,
    steps: &'static [Step],
    note: &'static str,
) -> Job {
    Job {
        name,
        when,
        count,
        steps,
        note,
    }
}

/// True when any of `wanted` is among `classes`.
fn any(classes: &BTreeSet<&str>, wanted: &[&str]) -> bool {
    wanted.iter().any(|class| classes.contains(class))
}

/// `a`, `b` as inline code, or `fallback` when there are none.
fn code_list<'a>(items: impl IntoIterator<Item = &'a str>, fallback: &str) -> String {
    let list: Vec<String> = items
        .into_iter()
        .map(|item| format!("`{}`", item.replace('`', "'")))
        .collect();
    if list.is_empty() {
        fallback.to_owned()
    } else {
        list.join(", ")
    }
}

/// The snapshot files: those [`READ_BY`] maps to the crate that renders and checks them.
fn snapshots() -> impl Iterator<Item = (&'static str, &'static str)> {
    READ_BY
        .iter()
        .copied()
        .filter(|(path, _)| path.starts_with("docs/snapshots/") || path.starts_with("docs/openapi"))
}

/// Everything the report says, worked out from the changed files.
pub struct Impact<'a> {
    /// The ref the change was compared with.
    pub base: &'a str,
    /// The changed files.
    pub files: &'a [String],
    /// The workspace.
    pub crates: &'a [Crate],
    /// Each generated document, and whether the committed file is what the tree renders.
    pub generated: &'a [(String, bool)],
}

impl Impact<'_> {
    /// The Markdown report.
    #[must_use]
    pub fn render(&self) -> String {
        let classes = classify(self.files);
        let packages = affected_packages(self.files, self.crates);
        let mut out = String::new();
        let _ = writeln!(out, "{MARKER}\n### What this change affects\n");
        let _ = writeln!(
            out,
            "{} changed file(s) since the merge base with {}. \
             From `cargo xtask impact`; this comment is updated on every push.\n",
            self.files.len(),
            code_list([self.base], "")
        );
        let _ = writeln!(
            out,
            "**Path classes:** {}\n",
            code_list(classes.iter().copied(), "none")
        );
        self.render_crates(&mut out, &packages);
        self.render_screens(&mut out);
        self.render_snapshots(&mut out, &packages);
        self.render_generated(&mut out);
        render_jobs(&mut out, &classes);
        out
    }

    fn render_crates(&self, out: &mut String, packages: &Packages) {
        let text = match packages {
            Packages::Workspace => {
                "the whole workspace: a run-everything path changed, or a Rust file outside every crate"
                    .to_owned()
            }
            Packages::Some(names) => format!(
                "{} of {}: {}",
                names.len(),
                self.crates.len(),
                code_list(names.iter().map(String::as_str), "none")
            ),
        };
        let _ = writeln!(
            out,
            "**Affected crates** (what `test`, `lints` and `build` work on): {text}\n"
        );
    }

    fn render_screens(&self, out: &mut String) {
        let mut parts = Vec::new();
        for (app, root) in FRONT_ENDS {
            let screens_dir = format!("{root}src/screens/");
            let screens: BTreeSet<&str> = self
                .files
                .iter()
                .filter_map(|file| file.strip_prefix(&screens_dir))
                .filter_map(|rest| rest.split('/').next())
                .map(|name| name.split('.').next().unwrap_or(name))
                .collect();
            if !screens.is_empty() {
                parts.push(format!("{app}: {}", code_list(screens.iter().copied(), "")));
            }
            let common = self.files.iter().any(|file| {
                (file.starts_with(&format!("{root}src/")) && !file.starts_with(&screens_dir))
                    || file.starts_with("packages/shared/")
            });
            if common {
                parts.push(format!(
                    "{app}: code every screen can use (`{root}src/` outside `screens/`, or `packages/shared/`)"
                ));
            }
        }
        let _ = writeln!(
            out,
            "**Screens:** {}\n",
            if parts.is_empty() {
                "none".to_owned()
            } else {
                parts.join("; ")
            }
        );
    }

    fn render_snapshots(&self, out: &mut String, packages: &Packages) {
        let mut lines = Vec::new();
        for (path, owner) in snapshots() {
            let changed = self.files.iter().any(|file| file == path);
            let checked = match packages {
                Packages::Workspace => true,
                Packages::Some(names) => names.contains(owner),
            };
            let what = match (changed, checked) {
                (true, _) => "changed here; `xtask snapshot` refuses a removed line",
                (false, true) => {
                    "unchanged; the affected crate's test fails if the code no longer matches it"
                }
                (false, false) => continue,
            };
            lines.push(format!("- `{path}` (`{owner}`): {what}"));
        }
        let _ = writeln!(out, "**Snapshots:**");
        if lines.is_empty() {
            let _ = writeln!(out, "none touched or checked\n");
        } else {
            let _ = writeln!(out, "{}\n", lines.join("\n"));
        }
    }

    fn render_generated(&self, out: &mut String) {
        let _ = writeln!(out, "**Generated docs:**");
        for (path, current) in self.generated {
            let changed = self.files.iter().any(|file| file == path);
            let what = match (changed, current) {
                (true, true) => "changed here, and matches the tree",
                (false, true) => "unchanged, and matches the tree",
                (_, false) => "out of date: run `cargo xtask gen-docs` and commit the result",
            };
            let _ = writeln!(out, "- `{path}`: {what}");
        }
        let _ = writeln!(out);
    }
}

/// The jobs table: each job, whether it runs, and why.
fn render_jobs(out: &mut String, classes: &BTreeSet<&str>) {
    let mut run = 0;
    let mut total = 0;
    let mut rows = String::new();
    for job in &JOBS {
        let (runs, why) = match job.when {
            When::Always => (true, "every pull request".to_owned()),
            When::Any(wanted) if any(classes, wanted) => (
                true,
                code_list(
                    wanted
                        .iter()
                        .copied()
                        .filter(|class| classes.contains(class)),
                    "",
                ),
            ),
            When::Any(wanted) => (
                false,
                format!(
                    "needs {}",
                    code_list(wanted.iter().copied(), "").replace(", ", " or ")
                ),
            ),
        };
        total += job.count;
        if runs {
            run += job.count;
        }
        let mut detail: Vec<String> = Vec::new();
        if runs && !job.steps.is_empty() {
            let active: Vec<&str> = job
                .steps
                .iter()
                .filter(|step| any(classes, step.classes))
                .map(|step| step.what)
                .collect();
            detail.push(if active.is_empty() {
                "its steps have nothing to do".to_owned()
            } else {
                active.join(", ")
            });
        }
        if !job.note.is_empty() {
            detail.push(job.note.to_owned());
        }
        let name = if job.count > 1 {
            format!("`{}` ×{}", job.name, job.count)
        } else {
            format!("`{}`", job.name)
        };
        let _ = writeln!(
            rows,
            "| {name} | {} | {why} | {} |",
            if runs { "runs" } else { "skips" },
            detail.join("; ")
        );
    }
    let _ = writeln!(
        out,
        "**Jobs:** {run} of {total} checks run; a skipped job reports success.\n"
    );
    let _ = writeln!(out, "| Job | Runs | Because | Note |\n|---|---|---|---|");
    out.push_str(&rows);
}

/// Prints the report for the changes since `--base <ref>` (default `origin/main`).
///
/// # Errors
///
/// [`Error`] for an unknown argument, or when git, `cargo metadata` or the tree cannot be read.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    if let Some(unknown) = args
        .iter()
        .enumerate()
        .find(|(at, argument)| *argument != "--base" && (*at == 0 || args[at - 1] != "--base"))
    {
        return Err(format!(
            "unknown argument {}; usage: cargo xtask impact [--base <ref>]",
            unknown.1
        )
        .into());
    }
    let base = base_ref(args);
    let files = changed_files(&base)?;
    let crates = load(&crate::checks::repo_root())?;
    let generated = crate::gen_docs::up_to_date(&crates)?;
    print!(
        "{}",
        Impact {
            base: &base,
            files: &files,
            crates: &crates,
            generated: &generated,
        }
        .render()
    );
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Impact, JOBS, MARKER, When};
    use crate::checks::crates::Crate;

    fn krate(name: &str, dir: &str, normal: &[&str]) -> Crate {
        Crate {
            name: name.into(),
            dir: dir.into(),
            normal: normal.iter().map(|n| (*n).to_owned()).collect(),
            ..Crate::default()
        }
    }

    fn workspace() -> Vec<Crate> {
        vec![
            krate("pos-proto", "crates/pos-proto", &[]),
            krate("pos-core", "crates/pos-core", &["pos-proto"]),
            krate("pos-edge", "crates/pos-edge", &["pos-core"]),
            krate(
                "store-sqlite",
                "crates/adapters/store-sqlite",
                &["pos-proto"],
            ),
            krate("pos-cloud", "crates/pos-cloud", &["pos-core"]),
        ]
    }

    fn report(files: &[&str], generated: &[(String, bool)]) -> String {
        let files: Vec<String> = files.iter().map(|f| (*f).to_owned()).collect();
        let crates = workspace();
        Impact {
            base: "origin/main",
            files: &files,
            crates: &crates,
            generated,
        }
        .render()
    }

    fn row<'a>(text: &'a str, job: &str) -> &'a str {
        let needle = format!("| `{job}`");
        text.lines()
            .find(|line| line.starts_with(&needle))
            .unwrap_or_else(|| panic!("no row for {job} in\n{text}"))
    }

    fn up_to_date() -> Vec<(String, bool)> {
        vec![
            ("docs/generated/crate-graph.md".into(), true),
            ("docs/generated/adr-index.md".into(), true),
        ]
    }

    #[test]
    fn a_docs_only_change_runs_only_the_always_jobs() {
        let text = report(&["docs/pos-spec.md"], &up_to_date());
        assert!(text.starts_with(MARKER));
        assert!(text.contains("**Path classes:** `docs_only`"));
        assert!(text.contains(
            "**Affected crates** (what `test`, `lints` and `build` work on): 0 of 5: none"
        ));
        assert!(text.contains("**Snapshots:**\nnone touched or checked"));
        assert!(row(&text, "test-archive").contains("| skips | needs `rust` |"));
        assert!(row(&text, "replay-shard").starts_with("| `replay-shard` ×3 | skips"));
        assert!(row(&text, "test").contains("| runs | every pull request |"));
        assert!(row(&text, "build").contains("its steps have nothing to do"));
        assert!(text.contains("**Jobs:** 9 of 24 checks run"));
    }

    #[test]
    fn a_leaf_change_names_its_crates_and_the_jobs_it_runs() {
        let text = report(&["crates/pos-edge/src/http.rs"], &up_to_date());
        assert!(text.contains("**Path classes:** `rust`"));
        assert!(text.contains("1 of 5: `pos-edge`"));
        assert!(text.contains("- `docs/snapshots/routes.txt` (`pos-edge`): unchanged;"));
        assert!(!text.contains("events.txt"), "pos-proto is not affected");
        assert!(row(&text, "test-shard").contains("| runs | `rust` |"));
        assert!(row(&text, "build").contains("cargo build, Windows installer checks"));
        assert!(row(&text, "lints").contains("fmt and clippy"));
    }

    #[test]
    fn a_core_change_is_the_workspace_and_checks_every_snapshot() {
        let text = report(
            &[
                "crates/pos-core/src/permission.rs",
                "docs/snapshots/permissions.txt",
            ],
            &up_to_date(),
        );
        assert!(text.contains(
            "**Affected crates** (what `test`, `lints` and `build` work on): the whole workspace"
        ));
        assert!(text.contains("- `docs/snapshots/permissions.txt` (`pos-core`): changed here;"));
        assert!(text.contains("- `docs/snapshots/events.txt` (`pos-proto`): unchanged;"));
        assert!(text.contains("**Jobs:** 24 of 24 checks run"));
    }

    #[test]
    fn a_deploy_change_runs_lints_without_its_rust_steps() {
        let text = report(&["deploy/compose.yml"], &up_to_date());
        assert!(row(&text, "lints").contains("| runs | `deploy` | its steps have nothing to do"));
        assert!(row(&text, "build").contains("| Windows installer checks"));
        assert!(row(&text, "deny").contains("| skips |"));
    }

    #[test]
    fn screens_are_named_and_shared_front_end_code_is_called_out() {
        let text = report(
            &[
                "ui/src/screens/Order.tsx",
                "ui/src/screens/Pay.tsx",
                "dashboard/src/screens/catalog/Items.tsx",
            ],
            &up_to_date(),
        );
        assert!(text.contains("**Screens:** till: `Order`, `Pay`; console: `catalog`\n"));
        let text = report(&["packages/shared/styles/tokens.css"], &up_to_date());
        assert!(text.contains("till: code every screen can use"));
        assert!(text.contains("console: code every screen can use"));
        assert!(report(&["docs/x.md"], &up_to_date()).contains("**Screens:** none\n"));
    }

    #[test]
    fn a_stale_generated_document_says_how_to_fix_it() {
        let stale = vec![("docs/generated/crate-graph.md".into(), false)];
        let text = report(&["crates/pos-edge/Cargo.toml"], &stale);
        assert!(text.contains(
            "- `docs/generated/crate-graph.md`: out of date: run `cargo xtask gen-docs`"
        ));
    }

    // ---- JOBS against .github/workflows/pr.yml ----------------------------------------------

    /// One job of the workflow as the file says it: its id, its `if:`, its matrix size and its
    /// `env:` lines.
    struct Parsed {
        name: String,
        condition: String,
        count: usize,
        env: Vec<String>,
    }

    /// The jobs in `pr.yml`, read line by line: a job is a two-space-indented key under `jobs:`.
    fn parse(text: &str) -> Vec<Parsed> {
        let mut jobs: Vec<Parsed> = Vec::new();
        let mut in_jobs = false;
        for line in text.lines() {
            if line == "jobs:" {
                in_jobs = true;
                continue;
            }
            if !in_jobs || line.trim_start().starts_with('#') {
                continue;
            }
            if let Some(name) = line
                .strip_prefix("  ")
                .filter(|rest| !rest.starts_with(' '))
                .and_then(|rest| rest.strip_suffix(':'))
            {
                jobs.push(Parsed {
                    name: name.to_owned(),
                    condition: String::new(),
                    count: 1,
                    env: Vec::new(),
                });
                continue;
            }
            let Some(job) = jobs.last_mut() else { continue };
            if let Some(condition) = line.strip_prefix("    if: ") {
                job.condition = condition.to_owned();
            } else if let Some(list) = line
                .trim()
                .strip_prefix("shard: [")
                .or_else(|| line.trim().strip_prefix("os: ["))
            {
                job.count = list.split(',').count();
            } else if line.starts_with("      RUN_") {
                job.env.push(line.trim().to_owned());
            }
        }
        jobs
    }

    /// The classes an expression names as `needs.changes.outputs.<class> == 'true'`.
    fn classes_in(expression: &str) -> BTreeSet<String> {
        expression
            .split("needs.changes.outputs.")
            .skip(1)
            .filter_map(|rest| {
                rest.split_once(" == 'true'")
                    .map(|(class, _)| class.to_owned())
            })
            .collect()
    }

    fn set(classes: &[&str]) -> BTreeSet<String> {
        classes.iter().map(|class| (*class).to_owned()).collect()
    }

    #[test]
    fn jobs_is_what_pr_yml_says() {
        let root = crate::checks::repo_root();
        let text = std::fs::read_to_string(root.join(".github/workflows/pr.yml")).expect("pr.yml");
        let parsed = parse(&text);
        let names: Vec<&str> = parsed.iter().map(|job| job.name.as_str()).collect();
        let ours: Vec<&str> = JOBS.iter().map(|job| job.name).collect();
        assert_eq!(names, ours, "add, remove or rename the job in JOBS too");
        for (file, job) in parsed.iter().zip(&JOBS) {
            // A shard job follows the job it needs, whose condition is checked on its own row.
            let followed = file
                .condition
                .split("needs.")
                .nth(1)
                .and_then(|rest| rest.split_once(".result == 'success'"))
                .map(|(name, _)| name.to_owned());
            let condition = match &followed {
                Some(name) if name != "changes" => parsed
                    .iter()
                    .find(|other| &other.name == name)
                    .map_or(String::new(), |other| other.condition.clone()),
                _ => file.condition.clone(),
            };
            let named = classes_in(&condition);
            match job.when {
                When::Always => assert!(
                    named.is_empty(),
                    "{} names classes {named:?} in its `if:`, so it is not When::Always",
                    job.name
                ),
                When::Any(classes) => {
                    assert_eq!(
                        named,
                        set(classes),
                        "{}'s `if:` names other classes",
                        job.name
                    );
                    assert!(
                        condition.contains("needs.changes.result != 'success'"),
                        "{} must run when `changes` fails",
                        job.name
                    );
                }
            }
            assert_eq!(file.count, job.count, "{}'s matrix size", job.name);
            for step in job.steps {
                let line = file
                    .env
                    .iter()
                    .find(|line| line.starts_with(&format!("{}:", step.env)))
                    .unwrap_or_else(|| panic!("{} has no env {}", job.name, step.env));
                assert_eq!(
                    classes_in(line),
                    set(step.classes),
                    "{} env {}",
                    job.name,
                    step.env
                );
            }
        }
    }
}
