// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Repository checks, run by `just preflight` and directly by CI.
//!
//! CI invokes these subcommands rather than `just`, so the pipeline does not
//! depend on `just` being installed on the runner.

use std::process::ExitCode;

mod checks;
mod finding;

pub use finding::Finding;

const USAGE: &str = "\
cargo xtask <check>

  deps-rule       pos-core / pos-ports / pos-proto import only allow-listed crates
  lint-config     every per-crate clippy.toml restates the baseline keys
  naming          snake_case, no created_at, no bare id, enums have *_UNSPECIFIED
  snapshot        nothing has been removed from a committed snapshot
  migrations      migrations within a release only add
  docs-gate       a code change touches CHANGELOG.md, or says why not
  links           internal documentation links resolve
  todos           TODO markers are not older than one release
  actions-pinned  every GitHub action is pinned to a commit SHA
  countries       every country module is named, wired into the workspace, and selectable
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(check) = args.next() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest: Vec<String> = args.collect();

    let findings = match check.as_str() {
        "deps-rule" => checks::deps_rule::run(&rest),
        "lint-config" => checks::lint_config::run(&rest),
        "actions-pinned" => checks::actions_pinned::run(&rest),
        "countries" => checks::countries::run(&rest),
        "links" => checks::links::run(&rest),
        "snapshot" => checks::snapshot::run(&rest),
        "migrations" => checks::migrations::run(&rest),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("unknown check: {other}\n");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match findings {
        Err(error) => {
            eprintln!("xtask {check}: {error}");
            ExitCode::from(3)
        }
        Ok(findings) if findings.is_empty() => {
            eprintln!("xtask {check}: ok");
            ExitCode::SUCCESS
        }
        Ok(findings) => {
            for finding in &findings {
                // GitHub renders this as an annotation on the pull-request diff,
                // so a failure lands next to the offending line instead of at the
                // bottom of a log nobody scrolls.
                println!("{}", finding.as_github_annotation());
            }
            eprintln!("xtask {check}: {} finding(s)", findings.len());
            ExitCode::FAILURE
        }
    }
}
