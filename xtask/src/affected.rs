// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask affected --classes`: which kinds of thing a change touches, so CI runs only the
//! jobs that can be affected ([ADR-0171](../../docs/adr/0171-ci-runs-what-a-change-affects.md)
//! decision 2).
//!
//! The changed files are `git diff --name-only <merge base>..HEAD`, the merge base being that of
//! `--base <ref>` (default `origin/main`) and `HEAD`. Each file falls in one class, and the run
//! prints one `class=true|false` line per class, the format `$GITHUB_OUTPUT` takes:
//!
//! | Class | Files |
//! |---|---|
//! | `rust_all` | `crates/pos-proto`, `pos-core`, `pos-ports`, `Cargo.lock`, the root `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.cargo/`, `.github/`, `xtask/`, `tools/` |
//! | `rust` | any other file under `crates/`, `countries/`, `examples/` or `apps/`, any `*.rs` or `Cargo.toml`, the `justfile`; and `rust_all` |
//! | `ui`, `dashboard`, `shared` | `ui/`, `dashboard/`, `packages/shared/` |
//! | `deploy` | `deploy/`, `k8s/`, `.dockerignore` |
//! | `workflows` | `.github/` |
//! | `docs_only` | true when every file is documentation: `docs/`, `changes/`, `.jules/`, any `*.md` outside `.github/`, `LICENSE`, `CODEOWNERS`, `.gitignore`, `.gitattributes`, `.editorconfig` |
//!
//! **When unsure, run.** A file no rule names counts as `rust_all`, `ui` and `dashboard` all at
//! once, and so does `--all`, which CI passes on a push to `main` and on a schedule. The only way a
//! class is false is that every changed file was positively placed elsewhere.

use std::collections::BTreeSet;

use crate::Finding;
use crate::checks::{Error, base_ref, repo_root};

/// Every class, in the order they are printed.
pub const CLASSES: [&str; 8] = [
    "rust",
    "rust_all",
    "ui",
    "dashboard",
    "shared",
    "docs_only",
    "workflows",
    "deploy",
];

/// Paths whose change can affect every Rust crate, or how CI runs: they run everything.
const RUST_ALL: [&str; 13] = [
    "crates/pos-proto/",
    "crates/pos-core/",
    "crates/pos-ports/",
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    "rustfmt.toml",
    "clippy.toml",
    "deny.toml",
    ".cargo/",
    ".github/",
    "xtask/",
    "tools/",
];

/// Files that change no build and no test.
const DOCS: [&str; 9] = [
    "docs/",
    "changes/",
    ".jules/",
    "LICENSE",
    "CODEOWNERS",
    ".gitignore",
    ".gitattributes",
    ".editorconfig",
    "README.md",
];

/// True when `path` is `prefix`, or sits under it when `prefix` ends in `/`.
fn under(path: &str, prefix: &str) -> bool {
    if prefix.ends_with('/') {
        path.starts_with(prefix)
    } else {
        path == prefix
    }
}

/// The classes the changed `paths` fall in.
#[must_use]
pub fn classify<S: AsRef<str>>(paths: &[S]) -> BTreeSet<&'static str> {
    let mut classes = BTreeSet::new();
    let mut only_docs = true;
    for path in paths.iter().map(AsRef::as_ref) {
        let mut add = |class| {
            classes.insert(class);
        };
        if path.starts_with(".github/") {
            add("workflows");
        }
        if RUST_ALL.iter().any(|prefix| under(path, prefix)) {
            add("rust_all");
        } else if DOCS.iter().any(|prefix| under(path, prefix))
            || (path.ends_with(".md") && !path.starts_with(".github/"))
        {
            continue; // documentation: the only branch that leaves `only_docs` alone
        } else if ["crates/", "countries/", "examples/", "apps/"]
            .iter()
            .any(|prefix| path.starts_with(prefix))
            || path.ends_with(".rs")
            || path.ends_with("/Cargo.toml")
            || path == "justfile"
        {
            add("rust");
        } else if path.starts_with("ui/") {
            add("ui");
        } else if path.starts_with("dashboard/") {
            add("dashboard");
        } else if path.starts_with("packages/shared/") {
            add("shared");
        } else if path.starts_with("deploy/") || path.starts_with("k8s/") || path == ".dockerignore"
        {
            add("deploy");
        } else {
            // Unknown: run everything that could be affected.
            add("rust_all");
            add("ui");
            add("dashboard");
        }
        only_docs = false;
    }
    if classes.contains("rust_all") {
        classes.insert("rust");
    }
    if only_docs {
        classes.insert("docs_only");
    }
    classes
}

/// Every class at once: what a push to `main`, a schedule, or `--all` runs.
#[must_use]
pub fn all() -> BTreeSet<&'static str> {
    CLASSES
        .iter()
        .copied()
        .filter(|class| *class != "docs_only")
        .collect()
}

/// `class=true|false`, one per line, in [`CLASSES`] order.
#[must_use]
pub fn render(classes: &BTreeSet<&str>) -> String {
    CLASSES
        .iter()
        .map(|class| format!("{class}={}\n", classes.contains(class)))
        .collect()
}

/// The files changed between the merge base of `base` and `HEAD`, and `HEAD`.
fn changed_files(base: &str) -> Result<Vec<String>, Error> {
    let root = repo_root();
    let git = |args: &[&str]| -> Result<String, Error> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let merge_base = git(&["merge-base", base, "HEAD"])?;
    let range = format!("{}..HEAD", merge_base.trim());
    Ok(git(&["diff", "--name-only", &range])?
        .lines()
        .map(str::to_owned)
        .collect())
}

/// `--classes [--base <ref>] [--all]`.
///
/// # Errors
///
/// [`Error`] for an unknown argument, or when git cannot name the changed files. CI treats a
/// failure here as "run everything".
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    if !args.iter().any(|argument| argument == "--classes") {
        return Err("usage: cargo xtask affected --classes [--base <ref>] [--all]".into());
    }
    let classes = if args.iter().any(|argument| argument == "--all") {
        all()
    } else {
        let files = changed_files(&base_ref(args))?;
        eprintln!("xtask affected: {} changed file(s)", files.len());
        classify(&files)
    };
    print!("{}", render(&classes));
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{all, classify, render};

    fn classes(paths: &[&str]) -> Vec<&'static str> {
        classify(paths).into_iter().collect()
    }

    #[test]
    fn documentation_alone_is_docs_only() {
        assert_eq!(
            classes(&[
                "docs/pos-spec.md",
                "changes/x.md",
                "crates/pos-edge/README.md",
                "CODEOWNERS"
            ]),
            ["docs_only"]
        );
        assert_eq!(
            classes(&[]),
            ["docs_only"],
            "nothing changed runs nothing but the always jobs"
        );
    }

    #[test]
    fn a_leaf_crate_is_rust_and_a_core_crate_is_everything_rust() {
        assert_eq!(
            classes(&["crates/adapters/store-sqlite/src/store.rs"]),
            ["rust"]
        );
        assert_eq!(classes(&["countries/vn/src/lib.rs", "docs/x.md"]), ["rust"]);
        assert_eq!(
            classes(&["crates/pos-core/src/billing.rs"]),
            ["rust", "rust_all"]
        );
        assert_eq!(classes(&["Cargo.lock"]), ["rust", "rust_all"]);
        assert_eq!(classes(&["tools/tool-versions.toml"]), ["rust", "rust_all"]);
    }

    #[test]
    fn a_workflow_runs_everything_rust_and_says_so() {
        assert_eq!(
            classes(&[".github/workflows/pr.yml"]),
            ["rust", "rust_all", "workflows"]
        );
        assert_eq!(
            classes(&[".github/PULL_REQUEST_TEMPLATE.md"]),
            ["rust", "rust_all", "workflows"],
            "a Markdown file under .github/ is not documentation"
        );
    }

    #[test]
    fn the_front_ends_and_deploy_are_their_own_classes() {
        assert_eq!(classes(&["ui/src/screens/Order.tsx"]), ["ui"]);
        assert_eq!(
            classes(&["dashboard/src/App.tsx", "packages/shared/styles/tokens.css"]),
            ["dashboard", "shared"]
        );
        assert_eq!(
            classes(&["deploy/compose.yml", "k8s/pos-cloud.yaml"]),
            ["deploy"]
        );
    }

    #[test]
    fn an_unknown_file_runs_everything() {
        assert_eq!(
            classes(&["Makefile"]),
            ["dashboard", "rust", "rust_all", "ui"]
        );
    }

    #[test]
    fn all_is_every_class_but_docs_only_and_renders_one_line_each() {
        let every = all();
        assert!(!every.contains("docs_only"));
        assert_eq!(every.len(), 7);
        let text = render(&BTreeSet::from(["ui"]));
        assert_eq!(
            text,
            "rust=false\nrust_all=false\nui=true\ndashboard=false\nshared=false\ndocs_only=false\nworkflows=false\ndeploy=false\n"
        );
    }
}
