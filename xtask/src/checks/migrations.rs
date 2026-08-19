// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Enforces that migrations are additive-only ([ADR-0017](../../../docs/adr/0017-migrations.md)).
//!
//! Two rules, both for the same reason: an edge that has been offline for weeks, and a cloud
//! replaying old events, still expect the columns a shipped migration created.
//!
//! 1. **A shipped migration is immutable.** A file that exists on the base branch may not change —
//!    the same removal-gate principle [`snapshot`](super::snapshot) uses for the catalogues. A
//!    schema change is a *new* numbered file, never an edit to an old one.
//! 2. **A migration adds, it does not take away.** `DROP TABLE`, `DROP COLUMN` and `RENAME` are a
//!    break dressed as a change. A column is retired by deprecation over two releases, not dropped.
//!    A genuinely necessary destructive change is possible but deliberately awkward: the file must
//!    carry the reviewed marker line, so it shows up in the diff as an explicit decision.

use super::{Error, base_ref, read_at_ref, repo_root};
use crate::Finding;

/// Directories holding numbered migration files. The cloud tier (P7) adds its own directory here.
const MIGRATION_DIRS: &[&str] = &["crates/adapters/store-sqlite/migrations"];

/// The exact line a migration must carry to opt a reviewed destructive change past the gate.
const ESCAPE_HATCH: &str = "-- migrations:allow-destructive";

/// Statements that drop or rename schema — the changes an offline edge cannot survive.
const DESTRUCTIVE: &[&str] = &["DROP TABLE", "DROP COLUMN", "RENAME TO", "RENAME COLUMN"];

/// Checks every migration against the two rules.
pub fn run(args: &[String]) -> Result<Vec<Finding>, Error> {
    let base = base_ref(args);
    let root = repo_root();
    let mut findings = Vec::new();

    for dir in MIGRATION_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            // No such directory yet — a tier whose migrations have not landed.
            continue;
        };
        let mut files: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".sql"))
            .collect();
        files.sort();

        for file in &files {
            let relative = format!("{dir}/{file}");
            let current = std::fs::read_to_string(root.join(&relative)).unwrap_or_default();

            // Rule 1: immutability.
            if let Some(previous) = read_at_ref(&root, &base, &relative)?
                && previous != current
            {
                findings.push(
                    Finding::new(&relative, "migrations", "a shipped migration was edited".to_owned())
                        .with_hint(
                            "a migration is immutable once merged; add a new numbered migration \
                             rather than editing this one — an offline edge already ran the old form",
                        ),
                );
            }

            // Rule 2: additive-only.
            for pattern in destructive_hits(&current) {
                findings.push(
                    Finding::new(
                        &relative,
                        "migrations",
                        format!("destructive statement `{pattern}`"),
                    )
                    .with_hint(
                        "retire a column or table by deprecating it over two releases, not by \
                         dropping it; if this is genuinely necessary add the reviewed \
                         `-- migrations:allow-destructive` marker to the file",
                    ),
                );
            }
        }
    }
    Ok(findings)
}

/// The destructive statements a migration contains, ignoring `--` comments and honouring the
/// escape-hatch marker. Returns each matched pattern once.
fn destructive_hits(sql: &str) -> Vec<&'static str> {
    if sql.lines().any(|line| line.trim() == ESCAPE_HATCH) {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for line in sql.lines() {
        // Only the code before an inline `--` comment counts, so a comment mentioning DROP is fine.
        let code = line.split("--").next().unwrap_or("").to_uppercase();
        for &pattern in DESTRUCTIVE {
            if code.contains(pattern) && !hits.contains(&pattern) {
                hits.push(pattern);
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::destructive_hits;

    #[test]
    fn a_dropped_table_is_flagged() {
        assert_eq!(destructive_hits("DROP TABLE events;"), vec!["DROP TABLE"]);
    }

    #[test]
    fn a_renamed_column_is_flagged() {
        assert_eq!(
            destructive_hits("ALTER TABLE bills RENAME COLUMN a TO b;"),
            vec!["RENAME COLUMN"]
        );
    }

    #[test]
    fn an_additive_migration_is_clean() {
        let sql = "CREATE TABLE t (a INTEGER);\nALTER TABLE t ADD COLUMN b TEXT;";
        assert!(destructive_hits(sql).is_empty());
    }

    #[test]
    fn a_comment_mentioning_drop_is_ignored() {
        let sql = "-- we never DROP TABLE here\nCREATE TABLE t (a INTEGER);";
        assert!(destructive_hits(sql).is_empty());
    }

    #[test]
    fn the_escape_hatch_allows_a_reviewed_drop() {
        let sql = "-- migrations:allow-destructive\nDROP COLUMN legacy;";
        assert!(destructive_hits(sql).is_empty());
    }
}
