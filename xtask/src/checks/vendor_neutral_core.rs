// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The vendor-neutrality gate: no external vendor's brand name may appear in the
//! production source of `pos-core` or `pos-proto`.
//!
//! This is the automated half of the integration doctrine ([ADR-0083](../../../docs/adr/0083-integration-doctrine.md)).
//! The dependency rule ([ADR-0013](../../../docs/adr/0013-async-strategy.md), enforced by
//! `deps_rule`) already keeps adapter *crates* out of the core's link graph, which is the
//! structural guarantee. But a vendor name can leak into core *logic or data* — an enum
//! variant, a match arm, a string literal compared against — without adding a single
//! dependency edge, and that is exactly the coupling the doctrine forbids: a `pos-core` is a
//! point of sale for anyone, so a Grab, a Shopee, an Ingenico must live in an adapter, an
//! `Open<T>` wire value, or a free-text field — never in a branch here.
//!
//! # Scope, and what is deliberately excluded
//!
//! Only `pos-core` and `pos-proto` are scanned — the layers that must be free even of a
//! vendor *instance*. `pos-ports` legitimately names the *category* it abstracts
//! (`PaymentTerminal`, `DeliveryVendor`) and is governed by the dependency rule and review.
//!
//! Within a scanned file, comments and `#[cfg(test)]` code are excluded, because example
//! data and documentation legitimately name a vendor (a round-trip test authoring a
//! `"GrabFood"` policy, a doc comment describing "a Grab feed"). What remains is production
//! code, and a brand token there is a violation.
//!
//! # A tripwire, not a proof
//!
//! The comment stripper does not parse string literals, so a `//` or `/*` inside a string
//! can truncate a line early. That can only *remove* text, so the failure mode is a missed
//! occurrence (a false negative), never a wrongly-broken clean build (a false positive) —
//! the safe direction for a tripwire guarding a currently-clean codebase. The denylist is
//! curated and narrow: unambiguous brand tokens only, never a domain word a vendor also uses
//! (`pax` is covers, `upi` is a payment rail). Extending it is a one-line change.

use std::collections::BTreeSet;
use std::path::Path;

use super::{Error, repo_root};
use crate::Finding;

/// The crates whose production source must not name a vendor.
const SCANNED: [&str; 2] = ["pos-core", "pos-proto"];

/// Brand tokens that may not appear in scanned production code. Lowercase; matched against
/// lowercased identifier words (so `Grab`, `GRAB`, `grab_food`'s `grab` all hit, and
/// `grabbed` — a different word — does not). Compound brands are listed in both their split
/// and joined forms because tokenisation splits on non-alphanumerics but not on case.
const DENYLIST: &[&str] = &[
    // Delivery / food marketplaces.
    "grab",
    "grabfood",
    "grabexpress",
    "grabkitchen",
    "shopee",
    "shopeefood",
    "ahamove",
    "capichi",
    "foodpanda",
    "gojek",
    "gofood",
    "baemin",
    "doordash",
    "ubereats",
    "deliveroo",
    "lalamove",
    // Payment acquirers / wallets (unambiguous brands only).
    "grabpay",
    "momo",
    "zalopay",
    "vnpay",
    "shopeepay",
    "paytm",
    "phonepe",
    "razorpay",
    "ingenico",
    "verifone",
    "adyen",
    "sumup",
    "alipay",
    // Fiscal / e-invoice / ERP vendors.
    "viettel",
    "vnpt",
    "misa",
    "easyinvoice",
];

/// One offending occurrence.
#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    /// One-based line the token was found on.
    pub line: u32,
    /// The brand token, as listed in the denylist.
    pub token: String,
}

/// Scans one file's contents, returning every denylisted token in production code.
///
/// Pure: no filesystem, no clock. Comments and `#[cfg(test)]` blocks are removed first.
#[must_use]
pub fn scan(contents: &str) -> Vec<Hit> {
    let denylist: BTreeSet<&str> = DENYLIST.iter().copied().collect();
    let mut hits = Vec::new();

    let mut in_block_comment = false;
    // Skipping the body of a `#[cfg(test)]` item by brace depth. `pending` means the
    // attribute has been seen and we are waiting for the opening brace of its item.
    let mut pending_test = false;
    let mut skipping = false;
    let mut depth: i32 = 0;

    for (index, raw) in contents.lines().enumerate() {
        let line = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let code = strip_comments(raw, &mut in_block_comment);

        if skipping {
            depth += brace_delta(&code);
            if depth <= 0 {
                skipping = false;
            }
            continue;
        }

        if !pending_test && code.contains("#[cfg(test)]") {
            pending_test = true;
        }
        if pending_test {
            if let Some(open) = code.find('{') {
                depth = brace_delta(&code[open..]);
                pending_test = false;
                skipping = depth > 0;
            }
            // Never scan an attribute or item-declaration line of a test block.
            continue;
        }

        for token in code.split(|c: char| !c.is_ascii_alphanumeric()) {
            if token.is_empty() {
                continue;
            }
            let lowered = token.to_ascii_lowercase();
            if denylist.contains(lowered.as_str()) {
                hits.push(Hit {
                    line,
                    token: lowered,
                });
            }
        }
    }

    hits
}

/// The net change in brace depth on a line: `{` is `+1`, `}` is `-1`.
fn brace_delta(code: &str) -> i32 {
    code.chars().fold(0, |acc, c| match c {
        '{' => acc + 1,
        '}' => acc - 1,
        _ => acc,
    })
}

/// Removes block (`/* */`) and line (`//`) comments from one line, carrying the block-comment
/// state across lines. Does not understand string literals — see the module note on why the
/// resulting bias toward false negatives is the safe one.
fn strip_comments(line: &str, in_block: &mut bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if *in_block {
            if c == '*' && next == Some('/') {
                *in_block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            *in_block = true;
            i += 2;
            continue;
        }
        if c == '/' && next == Some('/') {
            break;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Evaluates the gate against already-read files: `(repository-relative path, contents)`.
///
/// Pure, so it can be tested against literal fixtures.
#[must_use]
pub fn check(files: &[(String, String)]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, contents) in files {
        for hit in scan(contents) {
            findings.push(
                Finding::new(
                    path,
                    "vendor-neutral-core",
                    format!(
                        "names the vendor `{}` in production code — the core is vendor-neutral",
                        hit.token
                    ),
                )
                .at_line(hit.line)
                .with_hint(
                    "move the vendor behind a port/adapter, an `Open<T>` wire value, or a \
                     free-text field (ADR-0083); if this is example data or documentation, it \
                     belongs in a `#[cfg(test)]` block or a comment",
                ),
            );
        }
    }
    findings.sort_by(|a, b| (&a.file, &a.line, &a.message).cmp(&(&b.file, &b.line, &b.message)));
    findings
}

/// Reads the scanned crates' source and evaluates the gate.
pub fn run(_args: &[String]) -> Result<Vec<Finding>, Error> {
    let root = repo_root();
    let mut files = Vec::new();
    for crate_name in SCANNED {
        let src = root.join("crates").join(crate_name).join("src");
        if src.is_dir() {
            collect_rs(&src, &root, &mut files)?;
        }
    }
    files.sort();
    Ok(check(&files))
}

/// Recursively collects `.rs` files under `dir`, keyed by their path relative to `root`.
fn collect_rs(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rs(&path, root, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            out.push((rel, std::fs::read_to_string(&path)?));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests. A check nobody has watched fail is not a check.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{check, scan};

    #[test]
    fn clean_production_code_passes() {
        let code = "pub struct Order { channel: Open<SalesChannel> }\nfn total() -> Money { Money::ZERO }\n";
        assert!(
            scan(code).is_empty(),
            "clean code was flagged: {:?}",
            scan(code)
        );
    }

    #[test]
    fn a_vendor_in_a_line_comment_is_ignored() {
        let code = "let x = 1; // a Grab feed and a Shopee feed each get a plan\n";
        assert!(
            scan(code).is_empty(),
            "a comment was scanned: {:?}",
            scan(code)
        );
    }

    #[test]
    fn a_vendor_in_a_doc_comment_is_ignored() {
        let code =
            "/// The same menu is shown to a Grab feed and a POS terminal alike.\npub fn f() {}\n";
        assert!(scan(code).is_empty());
    }

    #[test]
    fn a_vendor_in_a_block_comment_is_ignored() {
        let code = "/* authored as\n   a Grab policy */\npub fn f() {}\n";
        assert!(scan(code).is_empty());
    }

    #[test]
    fn a_vendor_in_a_cfg_test_module_is_ignored() {
        let code = "\
pub fn f() {}
#[cfg(test)]
mod tests {
    fn a() {
        let node = policy(DisplayName::new(\"GrabFood\"));
    }
}
";
        assert!(
            scan(code).is_empty(),
            "test code was scanned: {:?}",
            scan(code)
        );
    }

    #[test]
    fn a_vendor_in_a_cfg_test_module_on_one_line_is_ignored() {
        let code = "pub fn f() {}\n#[cfg(test)] mod tests { fn a() { let _ = \"grab\"; } }\npub fn g() {}\n";
        assert!(
            scan(code).is_empty(),
            "inline test module was scanned: {:?}",
            scan(code)
        );
    }

    #[test]
    fn a_vendor_in_a_match_arm_is_caught() {
        let code = "match channel {\n    \"Grab\" => dispatch(),\n    _ => {}\n}\n";
        let hits = scan(code);
        assert_eq!(hits.len(), 1, "expected one hit: {hits:?}");
        assert_eq!(hits[0].token, "grab");
        assert_eq!(hits[0].line, 2);
    }

    #[test]
    fn a_vendor_in_an_identifier_is_caught() {
        let code = "fn grab_fee() -> Money { Money::ZERO }\n";
        let hits = scan(code);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].token, "grab");
    }

    #[test]
    fn a_camel_case_compound_brand_is_caught() {
        let code = "let v = GrabExpress::new();\n";
        let hits = scan(code);
        assert_eq!(hits.len(), 1, "compound brand missed: {hits:?}");
        assert_eq!(hits[0].token, "grabexpress");
    }

    #[test]
    fn resuming_after_a_test_module_scans_again() {
        let code = "\
#[cfg(test)]
mod tests {
    fn a() { let _ = \"grab\"; }
}
fn prod() { let _ = misa(); }
";
        let hits = scan(code);
        assert_eq!(
            hits.len(),
            1,
            "did not resume scanning after the test module: {hits:?}"
        );
        assert_eq!(hits[0].token, "misa");
    }

    #[test]
    fn a_domain_word_that_is_not_a_brand_passes() {
        // `pax` (covers) and `upi` (a payment rail) are deliberately not on the denylist.
        let code = "struct Seat { pax: u16 }\nfn upi_qr() {}\n";
        assert!(
            scan(code).is_empty(),
            "a domain word was flagged: {:?}",
            scan(code)
        );
    }

    #[test]
    fn check_reports_the_file_and_line() {
        let files = vec![(
            "crates/pos-core/src/x.rs".to_owned(),
            "fn f() { let _ = shopee(); }\n".to_owned(),
        )];
        let findings = check(&files);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "crates/pos-core/src/x.rs");
        assert_eq!(findings[0].line, Some(1));
    }
}
