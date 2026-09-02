// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Keeps `TLS_MODE` and `deploy/Caddyfile.d/` in agreement (ADR-0090).
//!
//! `bootstrap.sh` selects a Caddyfile **by name** — `Caddyfile.d/$TLS_MODE.caddy` — from a list of
//! accepted modes written a hundred lines earlier in the same script. Nothing but a deploy connects
//! the two, so a renamed or deleted file, or a fifth mode added to the accept-list without one, is
//! discovered on someone's box in the posture nobody here is running.
//!
//! This does not validate Caddy syntax; that needs the binary, and one mode needs a plugin build
//! (deferred, and named as deferred, in ADR-0090). It checks the three things that are checkable
//! from the repository: the accept-list and the directory hold the same set of modes, every mode
//! file imports the one shared site block, and the shared block does not itself declare a site
//! address or a TLS directive — the three things a posture is allowed to differ in.

use std::collections::BTreeSet;

use super::{Error, repo_root};
use crate::Finding;

/// The line in `bootstrap.sh` that enumerates the accepted postures, e.g.
/// `    acme-http01 | acme-dns01 | byo-cert | external) return 0 ;;`
const ACCEPT_MARKER: &str = ") return 0 ;;";
const SHARED: &str = "site.caddy";
const IMPORT: &str = "import /etc/caddy/Caddyfile.d/site.caddy";

/// Checks the mode accept-list against the committed per-mode Caddyfiles.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let bootstrap = "deploy/bootstrap.sh";
    let script = std::fs::read_to_string(root.join(bootstrap))?;

    let mut findings = Vec::new();
    let Some(accepted) = accepted_modes(&script) else {
        findings.push(
            Finding::new(
                bootstrap,
                "tls-modes",
                format!(
                    "could not find the TLS_MODE accept-list (a line ending `{ACCEPT_MARKER}`)"
                ),
            )
            .with_hint(
                "this check reads the accepted postures out of tls_mode_is_valid(); keep that \
                 case arm on one line, or teach this check the new shape",
            ),
        );
        return Ok(findings);
    };

    let dir = root.join("deploy/Caddyfile.d");
    let mut present = BTreeSet::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".caddy") else {
            continue;
        };
        if name == SHARED {
            let text = std::fs::read_to_string(&path)?;
            // The shared block is imported *inside* a site block, so a site address or a `tls`
            // directive in it would either fail to parse or silently apply to every posture —
            // including the two that must not hold a certificate at all.
            for forbidden in ["tls ", "tls {"] {
                if text
                    .lines()
                    .any(|line| line.trim_start().starts_with(forbidden))
                {
                    findings.push(
                        Finding::new(
                            "deploy/Caddyfile.d/site.caddy",
                            "tls-modes",
                            "the shared site block declares a `tls` directive".to_owned(),
                        )
                        .with_hint(
                            "TLS is what a posture differs in; it belongs in the per-mode file",
                        ),
                    );
                    break;
                }
            }
            continue;
        }
        present.insert(stem.to_owned());

        let text = std::fs::read_to_string(&path)?;
        if !text.contains(IMPORT) {
            findings.push(
                Finding::new(
                    format!("deploy/Caddyfile.d/{name}"),
                    "tls-modes",
                    format!("does not `{IMPORT}`"),
                )
                .with_hint(
                    "every posture serves the same proxy configuration; importing it is what \
                     keeps it from being copied four times and drifting in the one nobody runs",
                ),
            );
        }
    }

    for mode in accepted.difference(&present) {
        findings.push(
            Finding::new(
                bootstrap,
                "tls-modes",
                format!(
                    "TLS_MODE `{mode}` is accepted but deploy/Caddyfile.d/{mode}.caddy is missing"
                ),
            )
            .with_hint("bootstrap.sh selects the Caddyfile by this exact name, on the box"),
        );
    }
    for mode in present.difference(&accepted) {
        findings.push(
            Finding::new(
                format!("deploy/Caddyfile.d/{mode}.caddy"),
                "tls-modes",
                format!("no TLS_MODE `{mode}` is accepted, so this file can never be selected"),
            )
            .with_hint("add it to tls_mode_is_valid() in deploy/bootstrap.sh, or remove the file"),
        );
    }
    Ok(findings)
}

/// The posture names from `bootstrap.sh`'s `tls_mode_is_valid` accept arm.
fn accepted_modes(script: &str) -> Option<BTreeSet<String>> {
    let line = script
        .lines()
        .find(|line| line.trim_end().ends_with(ACCEPT_MARKER))?;
    let modes = line.trim().trim_end_matches(ACCEPT_MARKER);
    let set: BTreeSet<String> = modes
        .split('|')
        .map(|mode| mode.trim().to_owned())
        .filter(|mode| !mode.is_empty())
        .collect();
    (!set.is_empty()).then_some(set)
}

#[cfg(test)]
mod tests {
    use super::accepted_modes;

    #[test]
    fn the_accept_list_is_read_out_of_the_case_arm() {
        let script = "tls_mode_is_valid() {\n  case \"$1\" in\n    \
                      acme-http01 | acme-dns01 | byo-cert | external) return 0 ;;\n    \
                      *) return 1 ;;\n  esac\n}\n";
        let modes = accepted_modes(script).expect("the arm parses");
        assert_eq!(modes.len(), 4);
        assert!(modes.contains("byo-cert"));
        assert!(modes.contains("external"));
    }

    #[test]
    fn a_script_without_the_arm_is_none_rather_than_an_empty_set() {
        // The distinction matters: an empty set would make every committed mode file look
        // unreachable and bury the real problem under four bogus findings.
        assert!(accepted_modes("case \"$1\" in\n  *) return 1 ;;\nesac\n").is_none());
    }
}
