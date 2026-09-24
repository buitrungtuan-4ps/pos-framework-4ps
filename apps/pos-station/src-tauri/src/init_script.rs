// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The script that hands the till its token before the till boots (ADR-0147, ADR-0111's seam).
//!
//! The till reads its credentials through `ui/src/api/credentials.ts`: the token from
//! `localStorage["pos-edge.device-token"]` and the edge's base from `localStorage["pos-edge.base-url"]`,
//! where an absent base means "the origin that served me". The app loads the till from the edge's own
//! URL, so the base must be absent — requests stay root-relative and same-origin, which is what keeps
//! ADR-0111's allow-list and `/ws` origin check out of the picture entirely. So the script writes the
//! token and **removes** the base, and it runs as a webview initialization script: before any of the
//! page's own code, on every load, so the till's first `deviceToken()` read already sees it.
//!
//! # The origin guard
//!
//! An initialization script runs on every page the window loads, whatever its origin. The window's
//! navigation handler already refuses to leave the edge's origin, and the script refuses too: it
//! writes nothing unless `window.location.origin` is exactly the edge the token was issued by. A token
//! is a bearer credential; the only page that should ever hold it is the edge that minted it.
//!
//! # Escaping
//!
//! Both values are embedded as JSON string literals, which are valid JavaScript string literals, so no
//! value — however hostile an edge's response — can close the string and become code.

use crate::address::EdgeOrigin;

/// The key the till reads its bearer token from. Must match `TOKEN_KEY` in `credentials.ts`.
pub(crate) const TOKEN_KEY: &str = "pos-edge.device-token";

/// The key the till reads a non-default edge base from. Must match `BASE_KEY` in `credentials.ts`.
pub(crate) const BASE_KEY: &str = "pos-edge.base-url";

/// Builds the initialization script for a till served by `origin`, paired with `token`.
pub(crate) fn build(origin: &EdgeOrigin, token: &str) -> String {
    let origin = js_string(&origin.to_string());
    let token = js_string(token);
    let token_key = js_string(TOKEN_KEY);
    let base_key = js_string(BASE_KEY);
    format!(
        "(function () {{\n\
         \x20 if (window.location.origin !== {origin}) {{ return; }}\n\
         \x20 try {{\n\
         \x20   window.localStorage.setItem({token_key}, {token});\n\
         \x20   window.localStorage.removeItem({base_key});\n\
         \x20 }} catch (_) {{\n\
         \x20   // Storage refused (a locked-down profile): the till shows its pairing screen instead.\n\
         \x20 }}\n\
         }})();\n"
    )
}

/// A JavaScript string literal for `value`: a JSON string, which JavaScript accepts verbatim.
fn js_string(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::{BASE_KEY, TOKEN_KEY, build};
    use crate::address::parse;

    fn origin() -> crate::address::EdgeOrigin {
        parse("192.168.1.10:8080").unwrap().origin
    }

    #[test]
    fn it_writes_the_keys_the_till_reads() {
        // The two literals are the contract with ui/src/api/credentials.ts; a rename there that is
        // not made here leaves every Station till on its pairing screen.
        assert_eq!(TOKEN_KEY, "pos-edge.device-token");
        assert_eq!(BASE_KEY, "pos-edge.base-url");
        let script = build(&origin(), "not-a-real-token");
        assert!(script.contains(
            r#"window.localStorage.setItem("pos-edge.device-token", "not-a-real-token");"#
        ));
        assert!(script.contains(r#"window.localStorage.removeItem("pos-edge.base-url");"#));
    }

    #[test]
    fn it_writes_nothing_on_any_other_origin() {
        let script = build(&origin(), "not-a-real-token");
        let guard = script
            .find(r#"if (window.location.origin !== "http://192.168.1.10:8080") { return; }"#)
            .expect("the origin guard");
        let write = script.find("setItem").expect("the write");
        assert!(
            guard < write,
            "the guard must run before the write:\n{script}"
        );
    }

    #[test]
    fn the_token_is_written_before_the_base_is_cleared_and_both_inside_the_try() {
        let script = build(&origin(), "t");
        let open = script.find("try {").unwrap();
        let set = script.find("setItem").unwrap();
        let remove = script.find("removeItem").unwrap();
        let catch = script.find("catch").unwrap();
        assert!(open < set && set < remove && remove < catch);
    }

    #[test]
    fn a_hostile_value_stays_inside_its_string() {
        let script = build(&origin(), "\"); alert(1); (\"\\</script>\u{2028}");
        // Every quote the value carried is escaped, so none of them can end the literal.
        let breakouts: Vec<usize> = script
            .match_indices(r#""); alert(1)"#)
            .map(|(at, _)| at)
            .collect();
        assert!(!breakouts.is_empty());
        for at in breakouts {
            assert_eq!(
                script.get(at.saturating_sub(1)..at),
                Some("\\"),
                "an unescaped quote at {at}:\n{script}"
            );
        }
        assert!(script.contains(r#""\"); alert(1); (\"\\</script>"#));
    }

    #[test]
    fn it_is_one_self_contained_expression() {
        let script = build(&origin(), "t");
        assert!(script.starts_with("(function () {"));
        assert!(script.trim_end().ends_with("})();"));
        assert_eq!(script.matches('{').count(), script.matches('}').count());
    }
}
