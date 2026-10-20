// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask new`: generators that write a complete, checked starting point instead of a copy to
//! edit by hand ([ADR-0176](../../../docs/adr/0176-the-adapter-and-country-standard.md)).
//!
//! A generator only plans: it returns the files to create and the workspace member to add, as a
//! [`Plan`], from its input and the repository's own files. [`apply`] then refuses the plan if any of
//! its files already exists or the member is already listed, and otherwise writes it, or with
//! `--dry-run` prints it and writes nothing. Planning is pure, so the tests hold each generator to
//! its golden files under `xtask/tests/golden/` without touching the tree.
//!
//! Templates live in `xtask/templates/`, are embedded with `include_str!`, and are filled in by
//! [`render`]: plain `{{placeholder}}` substitution, where a placeholder left unfilled is an error.

mod adapter;
mod console;
mod country;
mod krate;
mod screen;

use std::path::Path;

use crate::Finding;
use crate::checks::{Error, repo_root};

/// The usage line, for a missing or unknown generator.
const USAGE: &str = "usage: cargo xtask new crate <name> --layer <layer> --purpose <one line> \
                     [--adr <number>]... [--dry-run] | cargo xtask new adapter --kind \
                     printer|storage|card|fiscal|courier|erp|delivery [--dry-run] | cargo xtask new country <cc> --display-name <name> --currency \
                     <code> --exponent <n> --prices-include-tax <true|false> --cash-rounding \
                     <none|minor units> --tax-rate <class[@channel]=basis points>... \
                     --retention-days <n> --language <tag> [--dry-run] | cargo xtask new screen \
                     --app ui|dashboard <id> --path <path> --title <English> --title-<locale> \
                     <title>... [--scope tenant|store|console --icon <name> --nav-group <name>] \
                     [--dry-run]";

/// What a generator writes: new files, and the line it adds to the workspace `members`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Each new file, by its path relative to the repository root, with its whole contents.
    pub files: Vec<(String, String)>,
    /// The directory to add to the root `Cargo.toml`'s `members`, for a plan that makes a crate.
    pub member: Option<String>,
    /// The package the plan creates.
    pub package: String,
    /// What to run once the files are written, one line each.
    pub next: Vec<String>,
    /// Existing files the plan adds lines to, each with its new contents.
    pub edits: Vec<Edit>,
}

/// Lines a generator adds to a file that already exists: a country's wiring into the binaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// The file, relative to the repository root.
    pub path: String,
    /// Its whole contents with the lines added.
    pub contents: String,
    /// The lines added, as they read in the file.
    pub added: Vec<String>,
}

/// Runs a generator: `cargo xtask new <kind> …`.
///
/// # Errors
///
/// [`Error`] for an unknown generator or argument, an input the generator refuses, a plan that would
/// overwrite something, or a file that cannot be read or written.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    let (dry_run, args): (Vec<&String>, Vec<&String>) =
        args.iter().partition(|argument| *argument == "--dry-run");
    let root = repo_root();
    let plan = match args.split_first() {
        Some((kind, rest)) if kind.as_str() == "crate" => krate::plan_from_args(&root, rest)?,
        Some((kind, rest)) if kind.as_str() == "adapter" => adapter::plan_from_args(&root, rest)?,
        Some((kind, rest)) if kind.as_str() == "country" => country::plan_from_args(&root, rest)?,
        Some((kind, rest)) if kind.as_str() == "screen" => screen::plan_from_args(&root, rest)?,
        _ => return Err(USAGE.into()),
    };
    apply(&root, &plan, !dry_run.is_empty())?;
    Ok(Vec::new())
}

/// Fills each `{{key}}` in `template` with its value.
///
/// # Errors
///
/// [`Error`] if a `{{` is left once every value is in: a placeholder this generator does not fill.
pub fn render(template: &str, values: &[(&str, &str)]) -> Result<String, Error> {
    let mut text = template.to_owned();
    for (key, value) in values {
        text = text.replace(&format!("{{{{{key}}}}}"), value);
    }
    match text.find("{{") {
        Some(at) => {
            let rest = text.get(at..).unwrap_or_default();
            let end = rest.find("}}").map_or(rest.len(), |end| end + 2);
            Err(format!(
                "template placeholder {} has no value",
                rest.get(..end).unwrap_or(rest)
            )
            .into())
        }
        None => Ok(text),
    }
}

/// `manifest` with `member` added to its `members` list, after the last member in the same parent
/// directory, or last when it has no sibling there.
///
/// # Errors
///
/// [`Error`] if the manifest has no `members = [` list, or `member` is already in it.
pub fn with_member(manifest: &str, member: &str) -> Result<String, Error> {
    let start = manifest
        .find("members = [\n")
        .ok_or("the root Cargo.toml has no `members = [` list")?;
    let list_start = start + "members = [\n".len();
    let list_len = manifest
        .get(list_start..)
        .and_then(|rest| rest.find("\n]"))
        .ok_or("the root Cargo.toml's `members` list is not closed")?;
    let list = manifest
        .get(list_start..list_start + list_len)
        .unwrap_or_default();
    if list
        .lines()
        .any(|line| line.trim() == format!("\"{member}\","))
    {
        return Err(format!("`{member}` is already a workspace member").into());
    }
    let parent = member.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut lines: Vec<&str> = list.lines().collect();
    let sibling = lines.iter().rposition(|line| {
        let entry = line.trim().trim_end_matches(',').trim_matches('"');
        entry.rsplit_once('/').map_or("", |(dir, _)| dir) == parent
    });
    let new_line = format!("    \"{member}\",");
    lines.insert(sibling.map_or(lines.len(), |at| at + 1), &new_line);
    Ok(format!(
        "{}{}{}",
        manifest.get(..list_start).unwrap_or_default(),
        lines.join("\n"),
        manifest.get(list_start + list_len..).unwrap_or_default()
    ))
}

/// Writes `plan` under `root`, or with `dry_run` prints it and writes nothing.
///
/// Nothing is written unless all of it can be: a file that already exists, or a member already
/// listed, refuses the whole plan before the first write.
///
/// # Errors
///
/// [`Error`] if a planned file exists, the member is listed, or a file cannot be read or written.
pub fn apply(root: &Path, plan: &Plan, dry_run: bool) -> Result<(), Error> {
    if let Some((path, _)) = plan.files.iter().find(|(path, _)| root.join(path).exists()) {
        return Err(format!("`{path}` already exists; `cargo xtask new` never overwrites").into());
    }
    let manifest_path = root.join("Cargo.toml");
    let manifest = match &plan.member {
        Some(member) => Some(with_member(
            &std::fs::read_to_string(&manifest_path)?,
            member,
        )?),
        None => None,
    };
    if dry_run {
        for (path, contents) in &plan.files {
            println!("--- {path}\n{contents}");
        }
        if let Some(member) = &plan.member {
            println!("--- Cargo.toml: members gains \"{member}\"");
        }
        for edit in &plan.edits {
            println!("--- {} gains:\n{}", edit.path, edit.added.join("\n"));
        }
        return Ok(());
    }
    for (path, contents) in &plan.files {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, contents)?;
        println!("created {path}");
    }
    if let (Some(manifest), Some(member)) = (manifest, &plan.member) {
        std::fs::write(&manifest_path, manifest)?;
        println!("Cargo.toml: members gains \"{member}\"");
    }
    for edit in &plan.edits {
        std::fs::write(root.join(&edit.path), &edit.contents)?;
        println!("{} gains {}", edit.path, edit.added.join(" · "));
    }
    for step in &plan.next {
        println!("next: {step}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Plan, apply, render, with_member};

    const MANIFEST: &str = "[workspace]\nmembers = [\n    \"crates/a\",\n    \"crates/adapters/x\",\n    \
                            \"crates/b\",\n    \"xtask\",\n]\n\n[workspace.package]\n";

    #[test]
    fn render_fills_every_placeholder() {
        let text = render("{{a}} and {{b}}, {{a}}", &[("a", "1"), ("b", "2")]);
        assert_eq!(text.ok().as_deref(), Some("1 and 2, 1"));
    }

    #[test]
    fn render_refuses_a_placeholder_it_has_no_value_for() {
        let error = render("{{a}} and {{typo}}", &[("a", "1")])
            .err()
            .map(|e| e.to_string());
        assert_eq!(
            error.as_deref(),
            Some("template placeholder {{typo}} has no value")
        );
    }

    #[test]
    fn a_member_goes_after_its_last_sibling() {
        let manifest = with_member(MANIFEST, "crates/c").unwrap_or_default();
        assert!(manifest.contains("    \"crates/b\",\n    \"crates/c\",\n    \"xtask\",\n]"));
        let adapter = with_member(MANIFEST, "crates/adapters/y").unwrap_or_default();
        assert!(
            adapter.contains(
                "    \"crates/adapters/x\",\n    \"crates/adapters/y\",\n    \"crates/b\","
            )
        );
    }

    #[test]
    fn a_member_with_no_sibling_goes_last() {
        let manifest = with_member(MANIFEST, "tools/z").unwrap_or_default();
        assert!(manifest.contains("    \"xtask\",\n    \"tools/z\",\n]\n\n[workspace.package]\n"));
    }

    #[test]
    fn a_member_already_listed_is_refused() {
        assert!(with_member(MANIFEST, "crates/b").is_err());
    }

    /// A scratch repository root under the system temporary directory, removed on drop.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("xtask-new-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch directory");
            std::fs::write(dir.join("Cargo.toml"), MANIFEST).expect("scratch manifest");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn plan() -> Plan {
        Plan {
            files: vec![
                ("crates/c/Cargo.toml".into(), "manifest".into()),
                ("crates/c/src/lib.rs".into(), "//! c".into()),
            ],
            member: Some("crates/c".into()),
            package: "c".into(),
            next: Vec::new(),
            edits: Vec::new(),
        }
    }

    #[test]
    fn apply_writes_the_files_and_the_member() {
        let scratch = Scratch::new("writes");
        apply(&scratch.0, &plan(), false).expect("applied");
        let lib = std::fs::read_to_string(scratch.0.join("crates/c/src/lib.rs")).expect("lib");
        assert_eq!(lib, "//! c");
        let manifest = std::fs::read_to_string(scratch.0.join("Cargo.toml")).expect("manifest");
        assert!(manifest.contains("\"crates/c\","));
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        let scratch = Scratch::new("dry");
        apply(&scratch.0, &plan(), true).expect("printed");
        assert!(!scratch.0.join("crates/c").exists());
        let manifest = std::fs::read_to_string(scratch.0.join("Cargo.toml")).expect("manifest");
        assert_eq!(manifest, MANIFEST);
    }

    #[test]
    fn apply_overwrites_nothing_and_writes_nothing_when_one_file_exists() {
        let scratch = Scratch::new("exists");
        std::fs::create_dir_all(scratch.0.join("crates/c/src")).expect("dir");
        std::fs::write(scratch.0.join("crates/c/src/lib.rs"), "mine").expect("existing file");
        assert!(apply(&scratch.0, &plan(), false).is_err());
        assert!(!scratch.0.join("crates/c/Cargo.toml").exists());
        let lib = std::fs::read_to_string(scratch.0.join("crates/c/src/lib.rs")).expect("lib");
        assert_eq!(lib, "mine");
        let manifest = std::fs::read_to_string(scratch.0.join("Cargo.toml")).expect("manifest");
        assert_eq!(manifest, MANIFEST);
    }
}
