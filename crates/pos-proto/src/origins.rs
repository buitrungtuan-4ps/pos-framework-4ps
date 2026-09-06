// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `origins` config node: which other origins may address a store's edge
//! ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
//!
//! The edge has always assumed one origin — the browser is served by the box it talks to — so
//! `fetch(path)` works on a shop LAN with no configuration. A native shell, a hosted edge placement
//! reached by hostname, and a second front-end are each a *different* origin, and each is refused
//! until a store publishes this node.
//!
//! # Why the rule lives here and not on the edge
//!
//! The cloud has to refuse a bad origin at authoring time, and the edge has to refuse one at
//! apply time — a store can be pulling a node published by an older or newer cloud, and the never-blank
//! contract means the edge cannot trust the document it is handed. Two copies of "what is a valid
//! origin" would be two rules that drift, and the shape they would drift into is an edge quietly
//! dropping what the console said it saved. So [`validate_origin`] is the one rule, and both sides
//! call it.
//!
//! # Never-blank, opt-in semantics
//!
//! An *absent* node means same-origin only, which is exactly how every store behaved before ADR-0111.
//! An *empty* node is a person deliberately withdrawing every second origin, and is distinct from
//! absent. A *malformed* node is refused whole, leaving whatever the edge already held — a list half
//! applied is a list nobody authored.

use serde::{Deserialize, Serialize};

/// The most origins a store may publish.
///
/// [`AGENTS.md`](../../../AGENTS.md) §2 forbids an unbounded in-memory structure, and the edge reads
/// this list on the front of every request. Eight is one origin per shipped shell with headroom; a
/// fleet that needs more has a different problem than a bigger array.
pub const MAX_ORIGINS: usize = 8;

/// Why a published origin list was refused whole.
///
/// Refused *whole*, never per-entry: a list half applied is a list nobody authored, and the operator
/// who published it would have no way to tell which half took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginsError {
    /// More than [`MAX_ORIGINS`] entries.
    TooMany {
        /// How many the document carried.
        count: usize,
    },
    /// An entry that is not an origin an edge will compare by equality.
    Invalid {
        /// The offending entry, which is a published configuration value and never a secret.
        entry: String,
        /// Why it was refused, for the log line or the `400` an operator reads.
        reason: &'static str,
    },
}

impl core::fmt::Display for OriginsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooMany { count } => write!(
                formatter,
                "the origins node carries {count} entries and the limit is {MAX_ORIGINS}"
            ),
            Self::Invalid { entry, reason } => {
                write!(formatter, "the origin `{entry}` is refused: {reason}")
            }
        }
    }
}

impl core::error::Error for OriginsError {}

/// Validates one entry into an origin an edge will compare by string equality.
///
/// Four refusals, each closing a specific hole:
///
/// - **No wildcards.** `https://*.example.com` is refused. An exact origin is a string comparison; a
///   wildcard makes it a parser, and a parser is where `https://evil.test#.example.com` gets in.
/// - **`null` is refused.** A sandboxed iframe, a `file://` page and several redirect shapes all send
///   `Origin: null`. Allow-listing it allow-lists all of them at once and names none of them.
/// - **`http` or `https` only.** Any other scheme is not something a browser sends as an `Origin` to
///   an edge, so allow-listing one can only ever be a mistake nobody sees.
/// - **Scheme and authority only.** An origin is `scheme://host[:port]` and nothing else: a trailing
///   path, a query or a fragment means the publisher wrote a URL, and a URL compared against an
///   `Origin` header never matches — a silent no-op is worse than a refusal they can see.
///
/// # Errors
///
/// [`OriginsError::Invalid`] naming the entry and the rule it broke.
pub fn validate_origin(entry: &str) -> Result<String, OriginsError> {
    let refuse = |reason| {
        Err(OriginsError::Invalid {
            entry: entry.to_owned(),
            reason,
        })
    };
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return refuse("it is empty");
    }
    if trimmed.eq_ignore_ascii_case("null") {
        return refuse(
            "`null` is what a sandboxed iframe, a file:// page and several redirects all send, so \
             allow-listing it allow-lists all of them at once",
        );
    }
    if trimmed.contains('*') {
        return refuse("wildcards are not compared, they are parsed, and a parser is a way in");
    }
    let Some((scheme, authority)) = trimmed.split_once("://") else {
        return refuse("an origin is `scheme://host[:port]`");
    };
    if !matches!(scheme, "http" | "https") {
        return refuse("only http and https origins are compared");
    }
    if authority.is_empty() {
        return refuse("an origin is `scheme://host[:port]`");
    }
    if authority.contains('/') || authority.contains('?') || authority.contains('#') {
        return refuse(
            "an origin carries no path, query or fragment — a URL never matches an Origin header",
        );
    }
    Ok(trimmed.to_owned())
}

/// Validates a whole published list, or refuses it whole.
///
/// The bound is checked before any entry, and every entry is validated before any is kept, so a
/// caller that refuses on `Err` never has to undo a partial application.
///
/// # Errors
///
/// [`OriginsError`] if the list is over [`MAX_ORIGINS`] or any entry is refused.
pub fn validate_origins<I, S>(published: I) -> Result<Vec<String>, OriginsError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let entries: Vec<S> = published.into_iter().collect();
    if entries.len() > MAX_ORIGINS {
        return Err(OriginsError::TooMany {
            count: entries.len(),
        });
    }
    let mut validated = Vec::with_capacity(entries.len());
    for entry in &entries {
        validated.push(validate_origin(entry.as_ref())?);
    }
    Ok(validated)
}

/// The `origins` config node: the origins a store's edge answers beside the one that served the
/// request.
///
/// The serving origin is never in this list and never needs to be — the edge compares it against the
/// request's own `Host`, so a store that has published nothing keeps serving its own UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedOrigins {
    /// The allowed origins, each `scheme://host[:port]`.
    #[serde(default)]
    allowed: Vec<String>,
}

impl PublishedOrigins {
    /// A node allowing exactly `allowed`, refusing the list whole if any entry is not an origin.
    ///
    /// # Errors
    ///
    /// [`OriginsError`] if the list is over [`MAX_ORIGINS`] or any entry is refused.
    pub fn new<I, S>(allowed: I) -> Result<Self, OriginsError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Ok(Self {
            allowed: validate_origins(allowed)?,
        })
    }

    /// The allowed origins, in the order the node lists them.
    #[must_use]
    pub fn allowed(&self) -> &[String] {
        &self.allowed
    }

    /// Whether the node lists no origin at all — a person withdrawing every second origin.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_ORIGINS, OriginsError, PublishedOrigins, validate_origins};

    #[test]
    fn a_valid_list_round_trips_through_the_wire_shape() {
        let node = PublishedOrigins::new(["https://till.example.com", "http://localhost:1420"])
            .expect("valid origins");
        let json = serde_json::to_value(&node).expect("serialises");
        assert_eq!(
            json,
            serde_json::json!({
                "allowed": ["https://till.example.com", "http://localhost:1420"]
            })
        );
        let back: PublishedOrigins = serde_json::from_value(json).expect("deserialises");
        assert_eq!(back, node);
    }

    #[test]
    fn an_absent_list_is_an_empty_node_not_an_error() {
        // `{}` is what an older cloud's node looks like; it must read as "no second origin", never
        // as a parse failure that would take the whole document down with it.
        let node: PublishedOrigins = serde_json::from_value(serde_json::json!({}))
            .expect("an absent allowed list deserialises");
        assert!(node.is_empty());
    }

    #[test]
    fn the_refusals_each_name_what_they_refused() {
        for entry in [
            "https://*.example.com",
            "null",
            "NULL",
            "https://till.example.com/app",
            "https://till.example.com?x=1",
            "https://till.example.com#f",
            "ftp://till.example.com",
            "till.example.com",
            "https://",
            "   ",
        ] {
            let refused = PublishedOrigins::new([entry]);
            assert!(
                matches!(refused, Err(OriginsError::Invalid { .. })),
                "`{entry}` should be refused, got {refused:?}"
            );
        }
    }

    #[test]
    fn the_bound_is_checked_before_any_entry() {
        let over: Vec<String> = (0..=MAX_ORIGINS)
            .map(|index| format!("https://shell-{index}.example.com"))
            .collect();
        assert_eq!(
            validate_origins(&over),
            Err(OriginsError::TooMany {
                count: MAX_ORIGINS + 1
            })
        );
    }

    #[test]
    fn an_entry_is_trimmed_and_compared_exactly() {
        let node = PublishedOrigins::new(["  https://till.example.com  "]).expect("valid");
        assert_eq!(node.allowed(), ["https://till.example.com"]);
    }
}
