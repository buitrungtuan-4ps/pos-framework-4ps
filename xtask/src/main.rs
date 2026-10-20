// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Repository checks, run by `just preflight` and directly by CI.
//!
//! CI invokes these subcommands rather than `just`, so the pipeline does not
//! depend on `just` being installed on the runner.

use std::process::ExitCode;

mod affected;
mod checks;
mod code_stats;
mod finding;
mod gen_docs;
mod impact;
mod scaffold;

pub use finding::Finding;

const USAGE: &str = "\
cargo xtask <check>

  deps-rule       pos-core / pos-ports / pos-proto import only allow-listed crates
  lint-config     every per-crate clippy.toml restates the baseline keys
  naming          snake_case, no created_at, no bare id, enums have *_UNSPECIFIED
  snapshot        nothing has been removed from a committed snapshot
  migrations      migrations within a release only add
  docs-gate       a code change touches CHANGELOG.md, or says why not
  changelog       fold changes/*.md into CHANGELOG.md [Unreleased]; --check validates them;
                  --exempt [--base <ref>] prints whether the docs gate exempts the pull request
                  (PR_AUTHOR is dependabot[bot] and it changes only manifests and lockfiles)
  links           internal documentation links resolve
  todos           TODO markers are not older than one release
  actions-pinned  every GitHub action is pinned to a commit SHA
  countries       every country module is named, wired into the workspace, and selectable
  crates          every workspace crate has a README and a contract card, and both agree with cargo metadata
  guides          every block a guide in docs/guides/ marks is still true to the tree; --run also
                  does each guide's `cargo xtask new` commands and verify steps in a scratch worktree
  mirrored-files  the files ui/ and dashboard/ share live once, in packages/shared, and nowhere else
  print-agent-deps  pos-print-agent links only the three workspace crates ADR-0112 allows
  tls-modes       every accepted TLS_MODE has its Caddyfile, and it imports the shared block
  vendor-neutral-core  no vendor brand name appears in pos-core / pos-proto production code

cargo xtask gen-docs [--check | --adr-citations [ADR-NNNN]]

  write docs/generated/ (crate graph, CODEOWNERS lines from the cards, ADR index) from the tree;
  --check fails where it differs, where a crate's own CODEOWNERS line names other owners than its
  card, or where an ADR that does not exist is cited; --adr-citations prints which files cite each ADR, uncommitted

cargo xtask new crate <name> --layer <layer> --purpose <one line> [--adr <number>]... [--dry-run]

  write a workspace crate that `xtask crates` accepts: manifest and contract card, README, crate
  root, and its `members` line; owners come from CODEOWNERS; never overwrites (ADR-0176)

cargo xtask new adapter --kind printer|storage|card|fiscal|courier|erp|delivery
    [--dry-run]

  write crates/adapters/<kind>-sandbox: a port implementation over a transport seam, and the port's
  contract suite wired in tests/contract.rs; it names no vendor, and every kind but printer and
  storage carries its `<family>.sandbox` descriptor, registered nowhere (ADR-0176)

cargo xtask new country <cc> --display-name <name> --currency <ISO 4217> --exponent <0-4>
    --prices-include-tax <true|false> --cash-rounding <none|minor units>
    --tax-rate <food|alcohol|exempt>=<basis points>... [--tax-rate <class>@<channel>=<bp>]...
    --retention-days <n> --language <tag> [--dry-run]

  write countries/<cc> from the values its law sets, every one a required flag, passing the
  Fiscalization suite; wire it into pos-edge and pos-cloud as a feature in no default (ADR-0176)

cargo xtask new screen --app ui <id> --path <path> --title <English> --title-vi <Vietnamese>
    [--dry-run]

  write a till screen and its lines in the registry, App.tsx, every string catalogue and the replay
  route walk; each catalogue other than en needs its --title-<locale>, so nothing is left untranslated

cargo xtask new screen --app dashboard <id> --path <path> --title <English> --title-vi <Vietnamese>
    --scope <tenant|store|console> --icon <name in ICON_NAMES> --nav-group <group> [--dry-run]

  write a console screen and its vitest test, and its lines in the registry and its nav group,
  App.tsx and every string catalogue; a nav group already holding six screens is refused

cargo xtask affected --classes [--base <ref>] [--all]

  which path classes a change touches (rust, rust_all, ui, dashboard, shared, docs_only,
  workflows, deploy), one `class=true|false` line each, for $GITHUB_OUTPUT (ADR-0171)

cargo xtask impact [--base <ref>]

  a report, not a check: the path classes, affected crates, snapshots, generated docs, and which
  pull-request jobs run or skip, as Markdown for the pull-request comment (ADR-0171)

cargo xtask code-stats [--json] [--root <dir>]

  a report, not a check: file, function, parameter, crate README and front-end figures (ADR-0170)

cargo xtask code-stats --check [--base <ref>] | --write-baseline

  the size ratchet: files, functions and parameters over ADR-0170's limits fail unless
  tools/code-size-baseline.toml lists them, and a listed item may not grow; warns in changed files
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(check) = args.next() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest: Vec<String> = args.collect();

    let gate = rest
        .iter()
        .any(|argument| argument == "--check" || argument == "--write-baseline");
    if check == "code-stats" && !gate {
        return match code_stats::run(&rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("xtask code-stats: {error}");
                ExitCode::from(3)
            }
        };
    }

    if check == "changelog" && rest.iter().any(|argument| argument == "--exempt") {
        return match checks::changelog::run_exempt(&rest) {
            Ok(exempt) => {
                println!("{exempt}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("xtask changelog: {error}");
                ExitCode::from(3)
            }
        };
    }

    let findings = match check.as_str() {
        "affected" => affected::run(&rest),
        "deps-rule" => checks::deps_rule::run(&rest),
        "lint-config" => checks::lint_config::run(&rest),
        "actions-pinned" => checks::actions_pinned::run(&rest),
        "changelog" => checks::changelog::run(&rest),
        "code-stats" => code_stats::ratchet::run(&rest),
        "countries" => checks::countries::run(&rest),
        "crates" => checks::crates::run(&rest),
        "gen-docs" => gen_docs::run(&rest),
        "guides" => checks::guides::run(&rest),
        "impact" => impact::run(&rest),
        "links" => checks::links::run(&rest),
        "snapshot" => checks::snapshot::run(&rest),
        "migrations" => checks::migrations::run(&rest),
        "mirrored-files" => checks::mirrored_files::run(&rest),
        "new" => scaffold::run(&rest),
        "print-agent-deps" => checks::print_agent_deps::run(&rest),
        "tls-modes" => checks::tls_modes::run(&rest),
        "vendor-neutral-core" => checks::vendor_neutral_core::run(&rest),
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
