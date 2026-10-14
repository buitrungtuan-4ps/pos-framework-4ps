// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask changelog`: change files in `changes/`, folded into `CHANGELOG.md` before a release
//! ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 5).
//!
//! Every pull request used to edit `[Unreleased]` in one 1.2 MB file, so any two open pull requests
//! conflicted there. Now each adds one file of its own, `changes/<topic>.md`:
//!
//! ```markdown
//! ---
//! kind: fixed
//! upgrade_note: Optional. One paragraph, on this line.
//! ---
//! - **A split bill reconciles to the original total.** The remainder goes to the last split. (#231)
//! ```
//!
//! - `cargo xtask changelog --check` validates every change file, which the docs gate runs.
//! - `cargo xtask changelog --exempt [--base <ref>]` prints `true` when the docs gate asks for no
//!   change file: a pull request Dependabot opened (`PR_AUTHOR`) that changes only manifests and
//!   lockfiles. A version bump there is recorded by the lockfile, not by a hand-written entry.
//! - `cargo xtask changelog` folds them in, by hand, before a release is tagged. Each entry goes to
//!   the top of its kind's heading under `[Unreleased]`, created if absent, in file-name order. The
//!   upgrade note is appended as the entry's own paragraph. The files are then deleted.

use std::fs;
use std::path::Path;

use super::{Error, repo_root};
use crate::Finding;

/// The directory change files live in.
const DIR: &str = "changes";

/// The kinds a change may be, as Keep a Changelog names them, and the heading each folds under.
const KINDS: [(&str, &str); 6] = [
    ("added", "Added"),
    ("changed", "Changed"),
    ("deprecated", "Deprecated"),
    ("removed", "Removed"),
    ("fixed", "Fixed"),
    ("security", "Security"),
];

/// The pull-request author whose manifest-only bumps need no change file.
const DEPENDABOT: &str = "dependabot[bot]";

/// The file names a dependency bump touches, at any depth: the Rust and pnpm manifests and
/// lockfiles.
const MANIFESTS: [&str; 4] = ["Cargo.toml", "Cargo.lock", "package.json", "pnpm-lock.yaml"];

/// One change file, read and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The file's name, which orders the entries.
    pub file: String,
    /// The heading it folds under, such as `Fixed`.
    pub heading: &'static str,
    /// The entry, as the file's body wrote it.
    pub body: String,
    /// The upgrade note, if the change has one.
    pub upgrade_note: Option<String>,
}

/// Validates the change files with `--check`; otherwise folds them into `CHANGELOG.md`.
///
/// # Errors
///
/// When `changes/` or `CHANGELOG.md` cannot be read or written.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let (changes, findings) = read_all(&root)?;
    if !findings.is_empty() || args.iter().any(|argument| argument == "--check") {
        return Ok(findings);
    }
    if changes.is_empty() {
        eprintln!("xtask changelog: no change files to fold");
        return Ok(Vec::new());
    }
    let path = root.join("CHANGELOG.md");
    match fold(&fs::read_to_string(&path)?, &changes) {
        Ok(folded) => fs::write(&path, folded)?,
        Err(message) => return Ok(vec![Finding::new("CHANGELOG.md", "changelog", message)]),
    }
    for change in &changes {
        fs::remove_file(root.join(DIR).join(&change.file))?;
    }
    eprintln!("xtask changelog: folded {} change file(s)", changes.len());
    Ok(Vec::new())
}

/// Whether the docs gate exempts the pull request from a change file, for `--exempt`: the author
/// from `PR_AUTHOR`, the changed files since the merge base with `--base` (default `origin/main`).
///
/// # Errors
///
/// When git cannot list the changed files.
pub fn run_exempt(args: &[String]) -> Result<bool, Error> {
    let author = std::env::var("PR_AUTHOR").unwrap_or_default();
    let changed = crate::affected::changed_files(&super::base_ref(args))?;
    Ok(exempt(&author, &changed))
}

/// Whether a pull request by `author` changing `changed` needs no change file: Dependabot opened it
/// and every file it changes is a manifest or a lockfile. Anyone else, or one other file (a source
/// edit pushed onto Dependabot's branch, say), and the gate applies; so it does to an empty list.
pub fn exempt(author: &str, changed: &[String]) -> bool {
    author == DEPENDABOT
        && !changed.is_empty()
        && changed.iter().all(|path| {
            let name = path.rsplit('/').next().unwrap_or(path.as_str());
            MANIFESTS.contains(&name)
        })
}

/// Every change file under `changes/`, with a finding for each that does not parse.
fn read_all(root: &Path) -> Result<(Vec<Change>, Vec<Finding>), Error> {
    let dir = root.join(DIR);
    let mut names: Vec<String> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".md") && name != "README.md")
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    let mut changes = Vec::new();
    let mut findings = Vec::new();
    for name in names {
        match parse(&name, &fs::read_to_string(dir.join(&name))?) {
            Ok(change) => changes.push(change),
            Err(message) => findings.push(
                Finding::new(format!("{DIR}/{name}"), "changelog", message).with_hint(
                    "start with ---, a `kind:` line and an optional `upgrade_note:` line, then ---, \
                     then the entry as a `- ` bullet (changes/README.md)",
                ),
            ),
        }
    }
    Ok((changes, findings))
}

/// Reads one change file.
///
/// # Errors
///
/// What is wrong with it, for the finding.
pub fn parse(file: &str, text: &str) -> Result<Change, String> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or("the file does not open with `---` front matter")?;
    let (front, body) = rest
        .split_once("\n---\n")
        .ok_or("the front matter is not closed with `---`")?;
    let mut kind = None;
    let mut upgrade_note = None;
    for line in front.lines().filter(|line| !line.trim().is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("`{line}` is not a `key: value` line"))?;
        let value = value.trim();
        match key.trim() {
            "kind" => kind = Some(value.to_owned()),
            "upgrade_note" if value.is_empty() => return Err("`upgrade_note` is empty".into()),
            "upgrade_note" => upgrade_note = Some(value.to_owned()),
            other => {
                return Err(format!(
                    "unknown key `{other}`; only `kind` and `upgrade_note`"
                ));
            }
        }
    }
    let kind = kind.ok_or("there is no `kind:`")?;
    let heading = KINDS
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, heading)| *heading)
        .ok_or_else(|| {
            format!(
                "`kind: {kind}` is none of added, changed, deprecated, removed, fixed, security"
            )
        })?;
    let body = body.trim().to_owned();
    if !body.starts_with("- ") {
        return Err("the entry is empty or is not a `- ` bullet".into());
    }
    Ok(Change {
        file: file.to_owned(),
        heading,
        body,
        upgrade_note,
    })
}

/// `changelog` with each change at the top of its heading under `[Unreleased]`, in the order given.
///
/// # Errors
///
/// When the changelog has no `## [Unreleased]` section.
pub fn fold(changelog: &str, changes: &[Change]) -> Result<String, String> {
    let mut lines: Vec<String> = changelog.lines().map(str::to_owned).collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with("## [Unreleased]"))
        .ok_or("CHANGELOG.md has no `## [Unreleased]` section")?;
    for (_, heading) in KINDS {
        let entries: Vec<&Change> = changes.iter().filter(|c| c.heading == heading).collect();
        if entries.is_empty() {
            continue;
        }
        let end = section_end(&lines, start);
        let title = format!("### {heading}");
        let at = match lines[start..end].iter().position(|line| *line == title) {
            Some(offset) => start + offset + 1,
            None => {
                lines.splice(end..end, [String::new(), title]);
                end + 2
            }
        };
        let mut block = vec![String::new()];
        for change in entries {
            block.extend(entry(change).lines().map(str::to_owned));
            block.push(String::new());
        }
        // The heading's existing first entry is already preceded by one blank line.
        if lines.get(at).is_some_and(|line| line.is_empty()) {
            block.pop();
        }
        lines.splice(at..at, block);
    }
    let mut out = lines.join("\n");
    // The file is otherwise left byte for byte as it was, its last line included.
    if changelog.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Where the `[Unreleased]` section starting at `start` ends: the next release, or the file's end.
/// A trailing blank or `---` separator stays below the end.
fn section_end(lines: &[String], start: usize) -> usize {
    let mut end = lines
        .iter()
        .skip(start + 1)
        .position(|line| line.starts_with("## ["))
        .map_or(lines.len(), |offset| start + 1 + offset);
    while end > start + 1 && (lines[end - 1].is_empty() || lines[end - 1] == "---") {
        end -= 1;
    }
    end
}

/// The entry as it folds: its body, then its upgrade note as a paragraph of its own.
fn entry(change: &Change) -> String {
    match &change.upgrade_note {
        Some(note) => format!("{}\n\n  **Upgrade note:** {note}", change.body),
        None => change.body.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{exempt, fold, parse};

    const LOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- Old fix.\n\n## [0.1.0] — 2026-01-01\n\n### Added\n\n- First.\n";

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn dependabot_touching_only_manifests_and_lockfiles_is_exempt() {
        let bump = paths(&[
            "Cargo.lock",
            "crates/pos-render/Cargo.toml",
            "dashboard/package.json",
            "dashboard/pnpm-lock.yaml",
        ]);
        assert!(exempt("dependabot[bot]", &bump));
    }

    #[test]
    fn everyone_else_and_every_other_file_keeps_the_gate() {
        let bump = paths(&["crates/pos-render/Cargo.toml", "Cargo.lock"]);
        // Someone else's bump, even of the same files.
        assert!(!exempt("octocat", &bump));
        assert!(!exempt("dependabot", &bump));
        assert!(!exempt("", &bump));
        // Dependabot's branch with one other file on it.
        let mixed = paths(&["Cargo.lock", "crates/pos-render/src/shape.rs"]);
        assert!(!exempt("dependabot[bot]", &mixed));
        // A name that only ends like a manifest.
        assert!(!exempt(
            "dependabot[bot]",
            &paths(&["crates/x/NotCargo.toml"])
        ));
        // Nothing changed is not a bump.
        assert!(!exempt("dependabot[bot]", &[]));
    }

    #[test]
    fn a_change_file_is_its_kind_its_entry_and_an_optional_upgrade_note() {
        let change = parse(
            "a.md",
            "---\nkind: fixed\nupgrade_note: Run the migration.\n---\n- **A fix.** Details.\n",
        )
        .expect("parses");
        assert_eq!(change.heading, "Fixed");
        assert_eq!(change.body, "- **A fix.** Details.");
        assert_eq!(change.upgrade_note.as_deref(), Some("Run the migration."));
    }

    #[test]
    fn a_change_file_without_front_matter_a_known_kind_or_a_bullet_is_refused() {
        for (text, why) in [
            ("- **No front matter.**\n", "open"),
            ("---\nkind: fixed\n- never closed\n", "closed"),
            ("---\nkind: tweaked\n---\n- x\n", "none of"),
            ("---\nupgrade_note: x\n---\n- x\n", "no `kind:`"),
            ("---\nkind: fixed\nauthor: me\n---\n- x\n", "unknown key"),
            ("---\nkind: fixed\n---\nNot a bullet.\n", "bullet"),
            ("---\nkind: fixed\nupgrade_note:\n---\n- x\n", "empty"),
        ] {
            let error = parse("a.md", text).expect_err(text);
            assert!(error.contains(why), "{text:?}: {error}");
        }
    }

    #[test]
    fn changes_fold_to_the_top_of_their_heading_in_file_order() {
        let changes = [
            parse("a.md", "---\nkind: fixed\n---\n- New fix A.\n").expect("a"),
            parse("b.md", "---\nkind: fixed\n---\n- New fix B.\n").expect("b"),
        ];
        let folded = fold(LOG, &changes).expect("folds");
        assert_eq!(
            folded,
            "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- New fix A.\n\n- New fix B.\n\n- Old fix.\n\n## [0.1.0] — 2026-01-01\n\n### Added\n\n- First.\n"
        );
    }

    #[test]
    fn a_missing_heading_is_created_at_the_end_of_unreleased_with_the_upgrade_note() {
        let changes = [parse(
            "c.md",
            "---\nkind: security\nupgrade_note: Rotate the key.\n---\n- **Safer.**\n",
        )
        .expect("c")];
        let folded = fold(LOG, &changes).expect("folds");
        assert_eq!(
            folded,
            "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- Old fix.\n\n### Security\n\n- **Safer.**\n\n  **Upgrade note:** Rotate the key.\n\n## [0.1.0] — 2026-01-01\n\n### Added\n\n- First.\n"
        );
    }

    #[test]
    fn a_changelog_without_unreleased_is_refused() {
        let changes = [parse("a.md", "---\nkind: added\n---\n- x\n").expect("a")];
        assert!(fold("# Changelog\n", &changes).is_err());
    }
}
