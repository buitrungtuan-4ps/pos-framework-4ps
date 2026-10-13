// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask code-stats`: what the repository's code measures against ADR-0170's size rules
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md)).
//!
//! A report, not a gate: it always succeeds. [`collect`] gathers the figures and [`markdown`]
//! renders them, so the rules gate a later phase adds reads the same [`Stats`] this prints. The
//! numbers it reports are recorded in `docs/metrics/baseline-2026-10.md`.
//!
//! - every `.rs` file's line count, a test file being one under a `tests/` or `benches/` directory;
//! - every function with a body: its length and parameters ([`scan`]), and whether it is test code;
//! - each workspace crate's lines and whether it has a `README.md`;
//! - each front-end root's files and lines.

pub mod ratchet;
pub mod scan;

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::checks::{Error, repo_root};

/// Directories never walked: build output, dependencies, and other people's code.
const SKIPPED_DIRS: [&str; 7] = [
    ".git",
    "target",
    "node_modules",
    "dist",
    "vendor",
    "test-results",
    ".claude",
];

/// The front-end build roots, each measured on its own.
const FRONTEND_ROOTS: [&str; 3] = ["ui", "dashboard", "apps/pos-station/ui"];

/// The source extensions a front-end root is measured by.
const FRONTEND_EXTENSIONS: [&str; 6] = ["ts", "tsx", "mjs", "js", "css", "html"];

/// Everything one run measured.
#[derive(Debug, Serialize)]
pub struct Stats {
    /// Every Rust file, largest first.
    pub files: Vec<FileStat>,
    /// Every Rust function with a body, longest first.
    pub functions: Vec<FunctionStat>,
    /// Every workspace crate, by path.
    pub crates: Vec<CrateStat>,
    /// Every front-end root.
    pub frontends: Vec<FrontendStat>,
}

/// One Rust source file.
#[derive(Debug, Serialize)]
pub struct FileStat {
    /// Path from the repository root, with `/` separators.
    pub path: String,
    /// Lines in the file.
    pub lines: usize,
    /// Whether it sits under a `tests/` or `benches/` directory.
    pub test: bool,
}

/// One Rust function.
#[derive(Debug, Serialize)]
pub struct FunctionStat {
    /// The file it is in.
    pub path: String,
    /// Its name.
    pub name: String,
    /// Its name inside what encloses it ([`scan::FunctionSpan::qualified`]), with `#2`, `#3` …
    /// after a name the file already used, so it is unique within the file.
    pub qualified: String,
    /// The line its `fn` is on.
    pub line: usize,
    /// Its length in lines.
    pub lines: usize,
    /// Its parameters, `self` included.
    pub params: usize,
    /// Whether it is test code: in a test file, or below the file's first `#[cfg(test)]`.
    pub test: bool,
}

/// One workspace crate.
#[derive(Debug, Serialize)]
pub struct CrateStat {
    /// The package name.
    pub name: String,
    /// Its directory from the repository root.
    pub path: String,
    /// Lines of Rust under it.
    pub lines: usize,
    /// Whether it has a `README.md`.
    pub readme: bool,
}

/// One front-end root.
#[derive(Debug, Serialize)]
pub struct FrontendStat {
    /// The root, such as `dashboard`.
    pub root: String,
    /// Its source files, largest first, as (path, lines).
    pub files: Vec<(String, usize)>,
}

/// Runs the report: Markdown by default, JSON with `--json`, over this repository or the checkout
/// `--root <dir>` names (a merge base checked out elsewhere, say).
///
/// # Errors
///
/// When the tree or `cargo metadata` cannot be read.
pub fn run(args: &[String]) -> Result<(), Error> {
    let root = args
        .iter()
        .position(|argument| argument == "--root")
        .and_then(|at| args.get(at + 1))
        .map_or_else(repo_root, PathBuf::from);
    let stats = collect(&root)?;
    if args.iter().any(|argument| argument == "--json") {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        print!("{}", markdown(&stats));
    }
    Ok(())
}

/// Measures the repository at `root`.
///
/// # Errors
///
/// When a file cannot be read or `cargo metadata` fails.
pub fn collect(root: &Path) -> Result<Stats, Error> {
    let mut rust = Vec::new();
    walk(root, &["rs"], &mut rust)?;
    let mut files = Vec::new();
    let mut functions = Vec::new();
    for path in &rust {
        let source = fs::read_to_string(path)?;
        let relative = relative(root, path);
        let test = is_test_path(&relative);
        let test_from = source
            .lines()
            .position(|line| line.trim() == "#[cfg(test)]")
            .map_or(usize::MAX, |index| index + 1);
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for span in scan::functions(&source) {
            let count = seen.entry(span.qualified.clone()).or_default();
            *count += 1;
            let qualified = if *count == 1 {
                span.qualified
            } else {
                format!("{}#{count}", span.qualified)
            };
            functions.push(FunctionStat {
                path: relative.clone(),
                test: test || span.line > test_from,
                qualified,
                name: span.name,
                line: span.line,
                lines: span.lines,
                params: span.params,
            });
        }
        files.push(FileStat {
            path: relative,
            lines: source.lines().count(),
            test,
        });
    }
    files.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.path.cmp(&b.path)));
    functions.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.path.cmp(&b.path)));
    let crates = crates(root, &files)?;
    let frontends = frontends(root)?;
    Ok(Stats {
        files,
        functions,
        crates,
        frontends,
    })
}

/// Collects every file under `dir` with one of `extensions`, skipping [`SKIPPED_DIRS`].
fn walk(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if entry.file_type()?.is_dir() {
            if !SKIPPED_DIRS.iter().any(|skip| name == *skip) {
                walk(&path, extensions, out)?;
            }
        } else if path
            .extension()
            .is_some_and(|extension| extensions.iter().any(|wanted| extension == *wanted))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_test_path(relative: &str) -> bool {
    relative
        .split('/')
        .any(|part| part == "tests" || part == "benches")
}

/// The workspace crates from `cargo metadata`, with their lines and README.
fn crates(root: &Path, files: &[FileStat]) -> Result<Vec<CrateStat>, Error> {
    let output = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"].as_array().cloned().unwrap_or_default();
    let mut crates = Vec::new();
    for package in packages {
        let name = package["name"].as_str().unwrap_or_default().to_owned();
        let manifest = PathBuf::from(package["manifest_path"].as_str().unwrap_or_default());
        let dir = manifest.parent().unwrap_or(root);
        let path = relative(root, dir);
        let prefix = format!("{path}/");
        let lines = files
            .iter()
            .filter(|file| file.path.starts_with(&prefix))
            .map(|file| file.lines)
            .sum();
        crates.push(CrateStat {
            readme: dir.join("README.md").is_file(),
            name,
            path,
            lines,
        });
    }
    crates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(crates)
}

fn frontends(root: &Path) -> Result<Vec<FrontendStat>, Error> {
    let mut out = Vec::new();
    for name in FRONTEND_ROOTS {
        let dir = root.join(name);
        if !dir.is_dir() {
            continue;
        }
        let mut paths = Vec::new();
        walk(&dir, &FRONTEND_EXTENSIONS, &mut paths)?;
        let mut files = Vec::new();
        for path in paths {
            let lines = fs::read_to_string(&path)?.lines().count();
            files.push((relative(root, &path), lines));
        }
        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.push(FrontendStat {
            root: name.to_owned(),
            files,
        });
    }
    Ok(out)
}

/// The report as Markdown, for a person and for `docs/metrics/`.
#[must_use]
pub fn markdown(stats: &Stats) -> String {
    let mut out = String::new();
    out.push_str(&rust_files(stats));
    out.push_str(&rust_functions(stats));
    out.push_str(&crate_table(stats));
    out.push_str(&frontend_table(stats));
    out
}

fn count<T>(items: &[T], keep: impl Fn(&T) -> bool) -> usize {
    items.iter().filter(|item| keep(item)).count()
}

fn rust_files(stats: &Stats) -> String {
    let (tests, code): (Vec<&FileStat>, Vec<&FileStat>) =
        stats.files.iter().partition(|file| file.test);
    let lines = |files: &[&FileStat]| files.iter().map(|file| file.lines).sum::<usize>();
    let over = |files: &[&FileStat], limit: usize| count(files, |file| file.lines > limit);
    let mut out = String::from("## Rust files\n\n");
    out.push_str("| | Files | Lines | > 500 | > 800 | > 1,000 | > 1,200 | > 1,500 |\n");
    out.push_str("|---|---|---|---|---|---|---|---|\n");
    for (label, files) in [("Source", &code), ("Test (`tests/`, `benches/`)", &tests)] {
        out.push_str(&format!(
            "| {label} | {} | {} | {} | {} | {} | {} | {} |\n",
            files.len(),
            lines(files),
            over(files, 500),
            over(files, 800),
            over(files, 1_000),
            over(files, 1_200),
            over(files, 1_500),
        ));
    }
    out.push_str("\nThe largest ten:\n\n| File | Lines |\n|---|---|\n");
    for file in stats.files.iter().take(10) {
        out.push_str(&format!("| `{}` | {} |\n", file.path, file.lines));
    }
    out.push('\n');
    out
}

fn rust_functions(stats: &Stats) -> String {
    let all: Vec<&FunctionStat> = stats.functions.iter().collect();
    let code: Vec<&FunctionStat> = all.iter().copied().filter(|f| !f.test).collect();
    let mut out = String::from("## Rust functions\n\n");
    out.push_str("| | Functions | > 40 lines | > 60 | > 100 | Longest | > 5 params | > 7 |\n");
    out.push_str("|---|---|---|---|---|---|---|---|\n");
    for (label, functions) in [("All", &all), ("Outside test code", &code)] {
        let longest = functions.iter().map(|f| f.lines).max().unwrap_or(0);
        out.push_str(&format!(
            "| {label} | {} | {} | {} | {} | {longest} | {} | {} |\n",
            functions.len(),
            count(functions, |f| f.lines > 40),
            count(functions, |f| f.lines > 60),
            count(functions, |f| f.lines > 100),
            count(functions, |f| f.params > 5),
            count(functions, |f| f.params > 7),
        ));
    }
    out.push_str("\nThe longest ten:\n\n| Function | Lines | Params |\n|---|---|---|\n");
    for function in stats.functions.iter().take(10) {
        out.push_str(&format!(
            "| `{}` in `{}:{}` | {} | {} |\n",
            function.name, function.path, function.line, function.lines, function.params
        ));
    }
    out.push('\n');
    out
}

fn crate_table(stats: &Stats) -> String {
    let with = count(&stats.crates, |c| c.readme);
    let mut out = format!(
        "## Workspace crates\n\n{with} of {} have a `README.md`.\n\n| Crate | Path | Rust lines | README |\n|---|---|---|---|\n",
        stats.crates.len()
    );
    for krate in &stats.crates {
        let readme = if krate.readme { "yes" } else { "no" };
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {readme} |\n",
            krate.name, krate.path, krate.lines
        ));
    }
    out.push('\n');
    out
}

fn frontend_table(stats: &Stats) -> String {
    let mut out = String::from("## Front-end roots\n\n");
    out.push_str("| Root | Files | Lines | > 300 | > 500 | > 1,000 | Largest |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    for root in &stats.frontends {
        let lines: usize = root.files.iter().map(|file| file.1).sum();
        let largest = root
            .files
            .first()
            .map_or_else(String::new, |(path, n)| format!("`{path}` ({n})"));
        out.push_str(&format!(
            "| `{}` | {} | {lines} | {} | {} | {} | {largest} |\n",
            root.root,
            root.files.len(),
            count(&root.files, |file| file.1 > 300),
            count(&root.files, |file| file.1 > 500),
            count(&root.files, |file| file.1 > 1_000),
        ));
    }
    out.push('\n');
    out
}
