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
//!
//! # Why the k8s lane is checked here too
//!
//! The `/internal` deny is the one security control both deployment lanes must carry, and for a
//! long time only one did: the Compose lane denied it in the shared Caddy block while `k8s/` routed
//! `/` as a single prefix and published all three routes, unauthenticated, to the internet. Nothing
//! caught it because nothing looked at both lanes at once. This check does, so the lanes cannot
//! drift apart again — which is the actual failure mode, not either lane being wrong on its own.

use std::collections::BTreeSet;

use super::{Error, repo_root};
use crate::Finding;

/// The line in `bootstrap.sh` that enumerates the accepted postures, e.g.
/// `    acme-http01 | acme-dns01 | byo-cert | external) return 0 ;;`
const ACCEPT_MARKER: &str = ") return 0 ;;";
const SHARED: &str = "site.caddy";
const IMPORT: &str = "import /etc/caddy/Caddyfile.d/site.caddy";
/// The `handle` block that keeps the cloud's trusted-network surface off the public one. Checked by
/// name, because the whole point is that every posture inherits it from the one shared file.
const INTERNAL_DENY: &str = "handle /internal/*";
/// The optional Kubernetes lane's manifest, which must carry the same deny by a different mechanism.
const K8S_MANIFEST: &str = "k8s/pos-cloud.yaml";
/// What the deny looks like there: an nginx `location` block returning 404 for the prefix. Matched
/// on the two halves rather than one exact string, so reformatting the snippet does not fail the
/// check while deleting it does.
const K8S_DENY_LOCATION: &str = "location ^~ /internal/";
const K8S_DENY_RETURN: &str = "return 404;";

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
            // `/internal/*` must be denied here, in the shared block, so all four postures inherit
            // it. Those handlers now require a shared secret (ADR-0097), and that is a second
            // control rather than a replacement for this one: the deny is what keeps the surface
            // off the internet at all, so dropping it would put event injection and falsifiable
            // fleet state one leaked key away — not the kind of regression a test run shows.
            if !text.contains(INTERNAL_DENY) {
                findings.push(
                    Finding::new(
                        "deploy/Caddyfile.d/site.caddy",
                        "tls-modes",
                        "the shared site block does not deny `/internal/*`".to_owned(),
                    )
                    .with_hint(
                        "the /internal routes require a shared secret (ADR-0097) *and* this \
                         deny; neither replaces the other, and removing the deny leaves event \
                         injection and falsifiable fleet state one leaked key from the internet. \
                         The deny belongs in the shared block where every posture inherits it",
                    ),
                );
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

    findings.extend(k8s_internal_deny(&root)?);
    Ok(findings)
}

/// The same `/internal` deny, in the optional Kubernetes lane.
///
/// A missing manifest is not a finding: `k8s/` is explicitly optional (`k8s/README.md`), and a fork
/// that deletes it has removed the exposure rather than created one. A manifest that exists and does
/// not deny the prefix is the finding, because that is a lane which serves the routes.
fn k8s_internal_deny(root: &std::path::Path) -> Result<Vec<Finding>, Error> {
    let path = root.join(K8S_MANIFEST);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    if text.contains(K8S_DENY_LOCATION) && text.contains(K8S_DENY_RETURN) {
        return Ok(Vec::new());
    }
    Ok(vec![
        Finding::new(
            K8S_MANIFEST,
            "tls-modes",
            "the Kubernetes Ingress does not deny `/internal/*`".to_owned(),
        )
        .with_hint(
            "the /internal routes require a shared secret (ADR-0097) *and* this deny; neither \
             replaces the other. The Compose lane denies them in deploy/Caddyfile.d/site.caddy \
             and this lane must too, or it publishes event injection and falsifiable fleet \
             state to the internet behind one shared key - see k8s/README.md, which also gives \
             the curl that verifies the deny actually took effect",
        ),
    ])
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
    fn the_k8s_deny_is_recognised_only_when_both_halves_are_present() {
        // Deleting either line re-opens the routes, so neither alone may satisfy the check.
        let denied = "  annotations:\n    nginx.ingress.kubernetes.io/server-snippet: |\n      \
                      location ^~ /internal/ {\n        return 404;\n      }\n";
        assert!(
            denied.contains(super::K8S_DENY_LOCATION) && denied.contains(super::K8S_DENY_RETURN)
        );

        let location_only = "location ^~ /internal/ {\n  proxy_pass http://pos-cloud;\n}\n";
        assert!(
            !(location_only.contains(super::K8S_DENY_LOCATION)
                && location_only.contains(super::K8S_DENY_RETURN)),
            "a location that proxies rather than denies must not pass"
        );

        let return_only = "location /health {\n  return 404;\n}\n";
        assert!(
            !(return_only.contains(super::K8S_DENY_LOCATION)
                && return_only.contains(super::K8S_DENY_RETURN)),
            "a 404 on some other path must not pass"
        );
    }

    #[test]
    fn a_script_without_the_arm_is_none_rather_than_an_empty_set() {
        // The distinction matters: an empty set would make every committed mode file look
        // unreachable and bury the real problem under four bogus findings.
        assert!(accepted_modes("case \"$1\" in\n  *) return 1 ;;\nesac\n").is_none());
    }
}
