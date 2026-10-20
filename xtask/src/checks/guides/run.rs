// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `cargo xtask guides --run`: does what a guide tells its reader to, and fails where it stops
//! working.
//!
//! `--check` proves a guide's commands are ones the generators accept; it cannot prove that what
//! they write still builds, which is the rot the golden files under `xtask/tests/golden/` miss when
//! a port changes under a template. So for each guide with a `guide:command` or `guide:verify`
//! block, this makes a scratch `git worktree` of `HEAD`, runs the guide's steps there in the order
//! the guide gives them, and removes the worktree, whatever happened.
//!
//! Only a guide `--check` passes is run, and only two kinds of its lines, never through a shell: a
//! `cargo xtask new …` line from a command block, and a verify line that is a `cargo test`, `cargo
//! check` or `cargo xtask` command, by the same rule `--check` holds verify blocks to. Any other
//! line a command block shows the reader — a `git` cleanup, say — is the reader's to run, not this
//! check's. The worktree is of the last commit, so commit a guide before running it; it shares the
//! checkout's target directory, so only what changed rebuilds.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Block, Kind, RULE, commands, is_verify_command, parse, words};
use crate::Finding;
use crate::checks::Error;

/// The steps of a guide this runs, in order: each block's marker line and the words of one line.
fn steps(blocks: &[Block]) -> Vec<(u32, Vec<String>)> {
    let mut steps = Vec::new();
    for block in blocks {
        for command in commands(&block.body) {
            let words = words(&command);
            let runs = match block.kind {
                Kind::Command => {
                    words.get(..3) == Some(&["cargo", "xtask", "new"].map(String::from)[..])
                }
                Kind::Verify => is_verify_command(&command),
                Kind::Excerpt(_) => false,
            };
            if runs {
                steps.push((block.line, words));
            }
        }
    }
    steps
}

/// Runs the steps of the guide `file`, whose text is `text`, in a scratch worktree of `root`, and
/// reports the first that fails. A guide with no step to run is not touched.
///
/// # Errors
///
/// [`Error`] if the worktree cannot be made.
pub(super) fn run_guide(root: &Path, file: &str, text: &str) -> Result<Vec<Finding>, Error> {
    let (blocks, _) = parse(file, text);
    let steps = steps(&blocks);
    if steps.is_empty() {
        return Ok(Vec::new());
    }
    let scratch = std::env::temp_dir().join(format!("pos-guide-{}", std::process::id()));
    git(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            &scratch.display().to_string(),
            "HEAD",
        ],
    )?;
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let mut findings = Vec::new();
    for (line, words) in &steps {
        let shown = words.join(" ");
        eprintln!("xtask guides: {file}: {shown}");
        let Some((program, args)) = words.split_first() else {
            continue;
        };
        let status = Command::new(program)
            .args(args)
            .current_dir(&scratch)
            .env("CARGO_TARGET_DIR", &target)
            .status();
        let failure = match status {
            Ok(status) if status.success() => continue,
            Ok(status) => format!("`{shown}` failed ({status}) where the guide says it works"),
            Err(error) => format!("`{shown}` could not start: {error}"),
        };
        findings.push(Finding::new(file, RULE, failure).at_line(*line));
        break;
    }
    git(
        root,
        &[
            "worktree",
            "remove",
            "--force",
            &scratch.display().to_string(),
        ],
    )?;
    Ok(findings)
}

/// Runs `git` in `root`.
fn git(root: &Path, args: &[&str]) -> Result<(), Error> {
    let status = Command::new("git").args(args).current_dir(root).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`git {}` failed ({status})", args.join(" ")).into())
    }
}

#[cfg(test)]
mod tests {
    use super::steps;
    use crate::checks::guides::parse;

    #[test]
    fn only_generator_commands_and_verify_steps_run_in_the_order_the_guide_gives_them() {
        let text = "<!-- guide:command -->\n```bash\ncargo xtask new crate pos-x \\\n  --purpose \"A crate\"\n\
                    git clean -fd\n```\n<!-- guide:excerpt src/lib.rs -->\n```rust\ncargo test\n```\n\
                    <!-- guide:verify -->\n```bash\ncargo test -p pos-x\ncargo install x\n```\n";
        let (blocks, _) = parse("g.md", text);
        let run: Vec<String> = steps(&blocks)
            .iter()
            .map(|(line, words)| format!("{line}: {}", words.join("|")))
            .collect();
        assert_eq!(
            run,
            [
                "1: cargo|xtask|new|crate|pos-x|--purpose|A crate",
                "11: cargo|test|-p|pos-x"
            ]
        );
    }
}
