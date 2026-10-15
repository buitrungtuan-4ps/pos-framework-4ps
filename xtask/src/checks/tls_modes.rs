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
/// The script that holds the accept-list, relative to the repository root.
const BOOTSTRAP: &str = "deploy/bootstrap.sh";
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
    let script = std::fs::read_to_string(root.join(BOOTSTRAP))?;

    let Some(accepted) = accepted_modes(&script) else {
        return Ok(vec![
            Finding::new(
                BOOTSTRAP,
                "tls-modes",
                format!(
                    "could not find the TLS_MODE accept-list (a line ending `{ACCEPT_MARKER}`)"
                ),
            )
            .with_hint(
                "this check reads the accepted postures out of tls_mode_is_valid(); keep that \
                 case arm on one line, or teach this check the new shape",
            ),
        ]);
    };

    let caddyfiles = read_caddyfiles(&root.join("deploy/Caddyfile.d"))?;
    let k8s_path = root.join(K8S_MANIFEST);
    let k8s = if k8s_path.exists() {
        Some(std::fs::read_to_string(&k8s_path)?)
    } else {
        None
    };
    Ok(check(&accepted, &caddyfiles, k8s.as_deref()))
}

/// Every `*.caddy` file in `dir`, by file name, in directory order.
fn read_caddyfiles(dir: &std::path::Path) -> Result<Vec<(String, String)>, Error> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".caddy") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        files.push((name.to_owned(), text));
    }
    Ok(files)
}

/// The findings for the accepted modes, the Caddyfiles and the Kubernetes manifest, if any: each
/// file in the order it was read, then the accept-list against the files, then the k8s lane.
fn check(
    accepted: &BTreeSet<String>,
    caddyfiles: &[(String, String)],
    k8s: Option<&str>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut present = BTreeSet::new();
    for (name, text) in caddyfiles {
        let Some(stem) = name.strip_suffix(".caddy") else {
            continue;
        };
        if name == SHARED {
            findings.extend(shared_block_findings(text));
            continue;
        }
        present.insert(stem.to_owned());
        findings.extend(missing_import(name, text));
    }
    findings.extend(accept_list_mismatches(accepted, &present));
    findings.extend(k8s_internal_deny(k8s));
    findings
}

/// What the shared block may not hold, a `tls` directive, and the deny it must.
fn shared_block_findings(text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    // The shared block is imported *inside* a site block, so a site address or a `tls`
    // directive in it would either fail to parse or silently apply to every posture —
    // including the two that must not hold a certificate at all.
    let declares_tls = ["tls ", "tls {"].iter().any(|forbidden| {
        text.lines()
            .any(|line| line.trim_start().starts_with(forbidden))
    });
    if declares_tls {
        findings.push(
            Finding::new(
                "deploy/Caddyfile.d/site.caddy",
                "tls-modes",
                "the shared site block declares a `tls` directive".to_owned(),
            )
            .with_hint("TLS is what a posture differs in; it belongs in the per-mode file"),
        );
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
    findings
}

/// A mode file that does not import the shared block.
fn missing_import(name: &str, text: &str) -> Option<Finding> {
    if text.contains(IMPORT) {
        return None;
    }
    Some(
        Finding::new(
            format!("deploy/Caddyfile.d/{name}"),
            "tls-modes",
            format!("does not `{IMPORT}`"),
        )
        .with_hint(
            "every posture serves the same proxy configuration; importing it is what \
             keeps it from being copied four times and drifting in the one nobody runs",
        ),
    )
}

/// Accepted modes without a file, then files without an accepted mode.
fn accept_list_mismatches(accepted: &BTreeSet<String>, present: &BTreeSet<String>) -> Vec<Finding> {
    let missing = accepted.difference(present).map(|mode| {
        Finding::new(
            BOOTSTRAP,
            "tls-modes",
            format!("TLS_MODE `{mode}` is accepted but deploy/Caddyfile.d/{mode}.caddy is missing"),
        )
        .with_hint("bootstrap.sh selects the Caddyfile by this exact name, on the box")
    });
    let unreachable = present.difference(accepted).map(|mode| {
        Finding::new(
            format!("deploy/Caddyfile.d/{mode}.caddy"),
            "tls-modes",
            format!("no TLS_MODE `{mode}` is accepted, so this file can never be selected"),
        )
        .with_hint("add it to tls_mode_is_valid() in deploy/bootstrap.sh, or remove the file")
    });
    missing.chain(unreachable).collect()
}

/// The same `/internal` deny, in the optional Kubernetes lane.
///
/// A missing manifest is not a finding: `k8s/` is explicitly optional (`k8s/README.md`), and a fork
/// that deletes it has removed the exposure rather than created one. A manifest that exists and does
/// not deny the prefix is the finding, because that is a lane which serves the routes.
fn k8s_internal_deny(manifest: Option<&str>) -> Option<Finding> {
    let text = manifest?;
    if text.contains(K8S_DENY_LOCATION) && text.contains(K8S_DENY_RETURN) {
        return None;
    }
    Some(
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
    )
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
    use std::collections::BTreeSet;

    use super::{accepted_modes, check};

    const SHARED_OK: &str = "handle /internal/* {\n  respond 404\n}\n";
    const MODE_OK: &str = "example.com {\n  import /etc/caddy/Caddyfile.d/site.caddy\n}\n";
    const K8S_OK: &str = "location ^~ /internal/ {\n  return 404;\n}\n";

    fn modes(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn files(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
            .collect()
    }

    /// Each finding as `file: message`, in the order the check reported them.
    fn reported(accepted: &[&str], caddyfiles: &[(&str, &str)], k8s: Option<&str>) -> Vec<String> {
        check(&modes(accepted), &files(caddyfiles), k8s)
            .iter()
            .map(|finding| format!("{}: {}", finding.file, finding.message))
            .collect()
    }

    #[test]
    fn modes_that_agree_with_their_files_have_no_finding() {
        let caddyfiles = [("site.caddy", SHARED_OK), ("acme.caddy", MODE_OK)];
        assert!(reported(&["acme"], &caddyfiles, Some(K8S_OK)).is_empty());
        assert!(
            reported(&["acme"], &caddyfiles, None).is_empty(),
            "a lane without k8s/ is not a finding"
        );
    }

    #[test]
    fn the_shared_block_may_hold_no_tls_directive_and_must_deny_internal() {
        let tls_block = [("site.caddy", "tls {\n  internal\n}\n")];
        assert_eq!(
            reported(&[], &tls_block, None),
            [
                "deploy/Caddyfile.d/site.caddy: the shared site block declares a `tls` directive",
                "deploy/Caddyfile.d/site.caddy: the shared site block does not deny `/internal/*`",
            ]
        );

        // Both spellings at once are still one finding.
        let both = format!("  tls internal\ntls {{\n}}\n{SHARED_OK}");
        assert_eq!(
            reported(&[], &[("site.caddy", both.as_str())], None),
            ["deploy/Caddyfile.d/site.caddy: the shared site block declares a `tls` directive"]
        );
    }

    #[test]
    fn the_shared_block_is_not_a_mode() {
        // `site` is neither missing from the accept-list nor required to import itself.
        assert!(reported(&[], &[("site.caddy", SHARED_OK)], None).is_empty());
    }

    #[test]
    fn a_mode_file_must_import_the_shared_block() {
        let caddyfiles = [
            ("site.caddy", SHARED_OK),
            ("byo.caddy", "example.com {\n}\n"),
        ];
        assert_eq!(
            reported(&["byo"], &caddyfiles, None),
            ["deploy/Caddyfile.d/byo.caddy: does not `import /etc/caddy/Caddyfile.d/site.caddy`"]
        );
    }

    #[test]
    fn the_accept_list_and_the_files_are_compared_both_ways_after_the_files() {
        let caddyfiles = [
            ("stray.caddy", "x {\n}\n"),
            ("site.caddy", SHARED_OK),
            ("acme.caddy", MODE_OK),
        ];
        assert_eq!(
            reported(&["acme", "ghost"], &caddyfiles, Some("location /x {}\n")),
            [
                "deploy/Caddyfile.d/stray.caddy: does not `import /etc/caddy/Caddyfile.d/site.caddy`",
                "deploy/bootstrap.sh: TLS_MODE `ghost` is accepted but \
                 deploy/Caddyfile.d/ghost.caddy is missing",
                "deploy/Caddyfile.d/stray.caddy: no TLS_MODE `stray` is accepted, so this file can \
                 never be selected",
                "k8s/pos-cloud.yaml: the Kubernetes Ingress does not deny `/internal/*`",
            ]
        );
    }

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
    fn the_k8s_lane_passes_only_with_both_halves_of_the_deny() {
        assert!(super::k8s_internal_deny(Some(K8S_OK)).is_none());
        assert!(super::k8s_internal_deny(None).is_none());
        for half in [
            "location ^~ /internal/ {\n  proxy_pass http://pos-cloud;\n}\n",
            "location /health {\n  return 404;\n}\n",
        ] {
            assert!(
                super::k8s_internal_deny(Some(half)).is_some(),
                "half a deny passed: {half}"
            );
        }
    }

    #[test]
    fn a_script_without_the_arm_is_none_rather_than_an_empty_set() {
        // The distinction matters: an empty set would make every committed mode file look
        // unreachable and bury the real problem under four bogus findings.
        assert!(accepted_modes("case \"$1\" in\n  *) return 1 ;;\nesac\n").is_none());
    }
}
