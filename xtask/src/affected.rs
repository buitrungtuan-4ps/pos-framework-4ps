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
//! | `rust_all` | `crates/pos-proto`, `pos-core`, `pos-ports`, `Cargo.lock`, the root `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`, `.cargo/`, `.config/` (nextest), `.github/`, `xtask/`, `tools/` |
//! | `rust` | any other file under `crates/`, `countries/`, `examples/` or `apps/`, any `*.rs` or `Cargo.toml`, the `justfile`; and `rust_all` |
//! | `ui`, `dashboard`, `shared` | `ui/`, `dashboard/`, `packages/shared/` |
//! | `deploy` | `deploy/`, `k8s/`, `.dockerignore` |
//! | `workflows` | `.github/` |
//! | `docs_only` | true when every file is documentation: `docs/`, `changes/`, `.jules/`, any `*.md` outside `.github/`, `LICENSE`, `CODEOWNERS`, `.gitignore`, `.gitattributes`, `.editorconfig`, `.git-blame-ignore-revs` |
//!
//! **When unsure, run.** A file no rule names counts as `rust_all`, `ui` and `dashboard` all at
//! once, and so does `--all`, which CI passes on a push to `main` and on a schedule. The only way a
//! class is false is that every changed file was positively placed elsewhere.

use std::collections::BTreeSet;

use crate::Finding;
use crate::checks::crates::{Crate, dependents, load};
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
const RUST_ALL: [&str; 14] = [
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
    ".config/",
    ".github/",
    "xtask/",
    "tools/",
];

/// Files that change no build and no test.
const DOCS: [&str; 10] = [
    "docs/",
    "changes/",
    ".jules/",
    "LICENSE",
    "CODEOWNERS",
    ".gitignore",
    ".gitattributes",
    ".editorconfig",
    // Which commits `git blame` looks through: read by git and GitHub, never by a build or a test.
    ".git-blame-ignore-revs",
    "README.md",
];

/// Files outside a crate that the crate compiles in, or its tests read, with that crate. A change
/// to one is a change to the crate: it sets `rust` beside its own class, and puts the crate in the
/// affected set. A test in this module scans the crates for such reads and fails on one missing
/// here.
pub const READ_BY: [(&str, &str); 14] = [
    ("docs/snapshots/events.txt", "pos-proto"),
    ("docs/snapshots/settings.txt", "pos-proto"),
    ("docs/configuration.md", "pos-proto"),
    ("docs/snapshots/permissions.txt", "pos-core"),
    ("docs/snapshots/capabilities.txt", "pos-core"),
    ("docs/permissions.md", "pos-core"),
    ("docs/state-machines.md", "pos-core"),
    ("docs/snapshots/routes.txt", "pos-edge"),
    ("docs/openapi-edge.json", "pos-edge"),
    ("docs/guides/add-a-setting.md", "pos-edge"),
    ("deploy/edge/install-pos-edge.ps1", "pos-edge"),
    ("docs/openapi.json", "pos-cloud"),
    ("docs/openapi-admin.json", "pos-cloud"),
    ("dashboard/src/i18n/", "pos-cloud"),
];

/// The crates in [`READ_BY`] that read `path`.
fn readers(path: &str) -> impl Iterator<Item = &'static str> + '_ {
    READ_BY
        .iter()
        .filter(move |(file, _)| under(path, file))
        .map(|(_, krate)| *krate)
}

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
        if readers(path).next().is_some() {
            add("rust"); // and its own class below, unless it is documentation
            only_docs = false;
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
///
/// # Errors
///
/// [`Error`] when git cannot find the merge base or list the diff.
pub fn changed_files(base: &str) -> Result<Vec<String>, Error> {
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

/// The crates a change can affect.
#[derive(Debug, PartialEq, Eq)]
pub enum Packages {
    /// Every crate: a run-everything path changed, or a Rust file belongs to no crate.
    Workspace,
    /// These crates, the changed ones and every crate that depends on one, tests included.
    Some(BTreeSet<String>),
}

impl Packages {
    /// The cargo arguments: `--workspace`, `-p a -p b`, or nothing.
    #[must_use]
    pub fn args(&self) -> String {
        match self {
            Self::Workspace => "--workspace".to_owned(),
            Self::Some(names) => names
                .iter()
                .map(|name| format!("-p {name}"))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// The arguments for `cargo test --doc`: only crates with a library, since `--doc` on a
    /// binary-only crate is an error; `none` when no affected crate has one.
    #[must_use]
    pub fn doc_args(&self, crates: &[Crate]) -> String {
        match self {
            Self::Workspace => "--workspace".to_owned(),
            Self::Some(names) => {
                let with_lib: BTreeSet<String> = names
                    .iter()
                    .filter(|name| crates.iter().any(|krate| &krate.name == *name && krate.lib))
                    .cloned()
                    .collect();
                if with_lib.is_empty() {
                    "none".to_owned()
                } else {
                    Self::Some(with_lib).args()
                }
            }
        }
    }
}

/// The crate whose directory holds `path`: the deepest one, since crates nest.
fn owner<'a>(path: &str, crates: &'a [Crate]) -> Option<&'a Crate> {
    crates
        .iter()
        .filter(|krate| path.starts_with(&format!("{}/", krate.dir)))
        .max_by_key(|krate| krate.dir.len())
}

/// The crates the changed `paths` belong to or are read by, and every crate that depends on one
/// of them, through normal, build or dev dependencies, transitively.
#[must_use]
pub fn affected_packages<S: AsRef<str>>(paths: &[S], crates: &[Crate]) -> Packages {
    if classify(paths).contains("rust_all") {
        return Packages::Workspace;
    }
    let mut affected = BTreeSet::new();
    for path in paths.iter().map(AsRef::as_ref) {
        let read_by: Vec<&str> = readers(path).collect();
        if !read_by.is_empty() {
            affected.extend(read_by.into_iter().map(str::to_owned));
        } else if classify(&[path]).contains("rust") {
            match owner(path, crates) {
                Some(krate) => {
                    affected.insert(krate.name.clone());
                }
                None => return Packages::Workspace, // unsure: a Rust file outside every crate
            }
        }
    }
    let mut frontier: Vec<String> = affected.iter().cloned().collect();
    while let Some(name) = frontier.pop() {
        let (normal, dev) = dependents(crates, &name);
        for dependent in normal.into_iter().chain(dev) {
            if affected.insert(dependent.clone()) {
                frontier.push(dependent);
            }
        }
    }
    Packages::Some(affected)
}

/// `--classes` and/or `--crates`, with `[--base <ref>] [--all]`.
///
/// # Errors
///
/// [`Error`] for an unknown argument, or when git or `cargo metadata` cannot be read. CI treats a
/// failure here as "run everything".
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    let want_classes = args.iter().any(|argument| argument == "--classes");
    let want_crates = args.iter().any(|argument| argument == "--crates");
    if !want_classes && !want_crates {
        return Err("usage: cargo xtask affected --classes|--crates [--base <ref>] [--all]".into());
    }
    let everything = args.iter().any(|argument| argument == "--all");
    let files = if everything {
        Vec::new()
    } else {
        changed_files(&base_ref(args))?
    };
    if !everything {
        eprintln!("xtask affected: {} changed file(s)", files.len());
    }
    if want_classes {
        print!(
            "{}",
            render(&if everything { all() } else { classify(&files) })
        );
    }
    if want_crates {
        let crates = load(&repo_root())?;
        let packages = if everything {
            Packages::Workspace
        } else {
            affected_packages(&files, &crates)
        };
        println!("packages={}", packages.args());
        println!("doc_packages={}", packages.doc_args(&crates));
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Packages, affected_packages, all, classify, render};
    use crate::checks::crates::Crate;

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
        assert_eq!(classes(&[".config/nextest.toml"]), ["rust", "rust_all"]);
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
    fn a_file_a_crate_reads_is_rust_too() {
        assert_eq!(classes(&["docs/snapshots/routes.txt"]), ["rust"]);
        assert_eq!(
            classes(&["dashboard/src/i18n/vi.json"]),
            ["dashboard", "rust"]
        );
        assert_eq!(
            classes(&["deploy/edge/install-pos-edge.ps1"]),
            ["deploy", "rust"]
        );
        assert_eq!(classes(&["docs/pos-spec.md"]), ["docs_only"]);
    }

    fn krate(name: &str, dir: &str, normal: &[&str], dev: &[&str]) -> Crate {
        Crate {
            name: name.into(),
            dir: dir.into(),
            normal: normal.iter().map(|n| (*n).to_owned()).collect(),
            dev: dev.iter().map(|n| (*n).to_owned()).collect(),
            ..Crate::default()
        }
    }

    /// pos-proto ← pos-core ← pos-edge ← minimal-edge; store-sqlite ← pos-edge; fakes tests store-sqlite.
    fn workspace() -> Vec<Crate> {
        vec![
            krate("pos-proto", "crates/pos-proto", &[], &[]),
            krate("pos-core", "crates/pos-core", &["pos-proto"], &[]),
            krate(
                "store-sqlite",
                "crates/adapters/store-sqlite",
                &["pos-proto"],
                &["pos-fakes"],
            ),
            krate("pos-fakes", "crates/pos-fakes", &["pos-proto"], &[]),
            krate(
                "pos-edge",
                "crates/pos-edge",
                &["pos-core", "store-sqlite"],
                &["pos-fakes"],
            ),
            krate("minimal-edge", "examples/minimal-edge", &["pos-edge"], &[]),
            krate("pos-cloud", "crates/pos-cloud", &["pos-core"], &[]),
        ]
    }

    fn names(packages: Packages) -> Vec<String> {
        match packages {
            Packages::Workspace => vec!["--workspace".into()],
            Packages::Some(set) => set.into_iter().collect(),
        }
    }

    #[test]
    fn a_leaf_change_affects_it_and_its_dependents_tests_included() {
        let ws = workspace();
        assert_eq!(
            names(affected_packages(
                &["crates/adapters/store-sqlite/src/store.rs"],
                &ws
            )),
            ["minimal-edge", "pos-edge", "store-sqlite"]
        );
        assert_eq!(
            names(affected_packages(&["crates/pos-fakes/src/lib.rs"], &ws)),
            ["minimal-edge", "pos-edge", "pos-fakes", "store-sqlite"],
            "a dev-dependency's change reaches the crates whose tests use it"
        );
        assert_eq!(
            names(affected_packages(
                &["examples/minimal-edge/src/main.rs"],
                &ws
            )),
            ["minimal-edge"]
        );
    }

    #[test]
    fn a_core_change_is_the_workspace_and_a_non_rust_change_is_nothing() {
        let ws = workspace();
        assert_eq!(
            names(affected_packages(&["crates/pos-proto/src/lib.rs"], &ws)),
            ["--workspace"]
        );
        assert_eq!(
            names(affected_packages(&["Cargo.lock"], &ws)),
            ["--workspace"]
        );
        assert!(
            names(affected_packages(
                &["ui/src/App.tsx", "docs/pos-spec.md"],
                &ws
            ))
            .is_empty()
        );
        assert_eq!(
            names(affected_packages(&["justfile"], &ws)),
            ["--workspace"],
            "unsure runs everything"
        );
        assert_eq!(
            names(affected_packages(&["dashboard/src/i18n/en.json"], &ws)),
            ["pos-cloud"],
            "a file a crate reads affects that crate"
        );
        assert_eq!(
            Packages::Some(["a".to_owned(), "b".to_owned()].into()).args(),
            "-p a -p b"
        );
        let mut ws = ws;
        ws[0].lib = true; // pos-proto has a library; minimal-edge does not
        let binary_only = Packages::Some(["minimal-edge".to_owned()].into());
        assert_eq!(binary_only.doc_args(&ws), "none");
        let mixed = Packages::Some(["minimal-edge".to_owned(), "pos-proto".to_owned()].into());
        assert_eq!(mixed.doc_args(&ws), "-p pos-proto");
    }

    /// Every path a crate compiles in, or a test reads, from outside its own directory is in
    /// [`READ_BY`] against that crate. Reads `include_str!`/`include_bytes!` paths and `"docs/…"`
    /// literals in the real tree.
    #[test]
    fn every_file_a_crate_reads_from_outside_is_mapped() {
        let root = crate::checks::repo_root();
        let crates = crate::checks::crates::load(&root).expect("cargo metadata");
        let mut missing = Vec::new();
        // xtask reads the whole tree by design, and any change to it runs everything anyway.
        for krate in crates.iter().filter(|k| k.name != "xtask") {
            for file in rust_files(&root.join(&krate.dir)) {
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                let dir = file.parent().unwrap_or(&root).to_path_buf();
                for (target, from_manifest) in outside_reads(&text) {
                    let resolved = if target.starts_with("docs/") {
                        target.clone()
                    } else {
                        // `include_str!` is relative to the file, `CARGO_MANIFEST_DIR.join` to the crate.
                        let base = if from_manifest {
                            root.join(&krate.dir)
                        } else {
                            dir.clone()
                        };
                        let joined = normalise(&base.join(&target));
                        match joined.strip_prefix(&root) {
                            Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
                            Err(_) => continue,
                        }
                    };
                    // Only real files: a directory, a built output or a formatted path is not one.
                    if !root.join(&resolved).is_file() || resolved.contains("/dist/") {
                        continue;
                    }
                    let covered = match affected_packages(&[resolved.as_str()], &crates) {
                        Packages::Workspace => true,
                        Packages::Some(names) => names.contains(&krate.name),
                    };
                    if !covered {
                        missing.push(format!("{} reads {resolved}", krate.name));
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "add these to READ_BY: {missing:#?}");
    }

    fn rust_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(rust_files(&path));
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
        out
    }

    /// `include_str!`/`include_bytes!` arguments, `.join("…")` arguments that climb with `../`
    /// (marked as relative to the crate), and paths that start `docs/` in string literals,
    /// outside comments.
    fn outside_reads(text: &str) -> Vec<(String, bool)> {
        let mut found = Vec::new();
        for line in text
            .lines()
            .map(str::trim_start)
            .filter(|l| !l.starts_with("//"))
        {
            for marker in [
                "include_str!(\"",
                "include_bytes!(\"",
                ".join(\"../",
                "\"docs/",
            ] {
                let mut rest = line;
                while let Some(at) = rest.find(marker) {
                    let start = at + marker.len();
                    let tail = &rest[start..];
                    let end = tail
                        .find(|c: char| !(c.is_ascii_alphanumeric() || "_./-".contains(c)))
                        .unwrap_or(tail.len());
                    let (literal, from_manifest) = match marker {
                        ".join(\"../" => (format!("../{}", &tail[..end]), true),
                        "\"docs/" => (format!("docs/{}", &tail[..end]), false),
                        _ => (tail[..end].to_owned(), false),
                    };
                    if literal.starts_with("../") || literal.starts_with("docs/") {
                        found.push((literal, from_manifest));
                    }
                    rest = &tail[end..];
                }
            }
        }
        found
    }

    fn normalise(path: &std::path::Path) -> std::path::PathBuf {
        let mut out = std::path::PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other),
            }
        }
        out
    }

    #[test]
    fn the_blame_ignore_list_is_documentation() {
        assert_eq!(classes(&[".git-blame-ignore-revs"]), ["docs_only"]);
        assert_eq!(
            classes(&[".git-blame-ignore-revs", "docs/metrics/pilot-2026-10.md"]),
            ["docs_only"]
        );
        assert_eq!(
            classes(&[".git-blame-ignore-revs", "crates/pos-cloud/src/http.rs"]),
            ["rust"],
            "it adds nothing to a change that has code"
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
