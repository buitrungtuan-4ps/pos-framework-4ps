// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The hash chain that makes the event log tamper-evident
//! ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md)).
//!
//! # What this module owns, and what it deliberately does not
//!
//! It owns the **preimage**: the exact bytes a chain hash is taken over, in the exact
//! order. That is the part that must never drift, because a store computing one preimage
//! and a cloud computing another would disagree about a log neither has tampered with.
//!
//! It does **not** own the digest. `pos-proto` is a backbone crate, and
//! `tools/backbone-allowlist.toml` admits only allow-listed pure-computation crates —
//! adding `sha2` there needs an architecture reviewer and an ADR naming it, which
//! [ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) does not. So the tiers that
//! already link `sha2` (`pos-edge`, `pos-cloud`) call [`ChainHash::of`] with a digest they
//! compute, and what they hash is decided here. Splitting it this way keeps one definition
//! of *what* is hashed without forcing a new dependency into the backbone; the alternative
//! — allow-listing `sha2` so the whole computation lives here — is a reasonable different
//! answer that needs its own ADR.
//!
//! # Why the preimage covers the link and not just the payload
//!
//! [ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 2. A hash over the
//! payload alone would let two records swap places with their hashes still valid, because
//! nothing in either hash would say where it sat. Covering `prev_hash` and `seq` is what
//! turns a set of hashes into a chain.
//!
//! # What a chain catches, and what it does not
//!
//! It catches a record edited in place and a record removed from the middle. It does
//! **not** catch either of these on its own:
//!
//! * **Truncation.** Drop the last records and what survives is still perfectly linked.
//! * **Recomputation.** This algorithm is in the source. Edit a record, re-derive every
//!   later link, and the result verifies.
//!
//! Both are closed by publishing the head to the cloud, which a store cannot rewrite —
//! the other half of ADR-0131, and the reason nothing here may be described as
//! tamper-*proof*. It is tamper-**evident**, and only as far back as the last anchor.

use core::fmt;

use serde::{Deserialize, Serialize};

/// A chain hash, as 64 lowercase hexadecimal characters.
///
/// A string rather than `[u8; 32]` because it crosses the wire inside the envelope and is
/// read by humans in a diagnostic; the cost is 32 bytes a record, and the benefit is that
/// a chain break can be pasted into a bug report.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChainHash(String);

/// The link a store's very first record chains to.
///
/// Sixty-four zeros: a value no digest produces in practice, so "this is the beginning of
/// the log" cannot be confused with "this follows a record whose hash happened to be
/// zero".
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

impl ChainHash {
    /// The genesis link.
    #[must_use]
    pub fn genesis() -> Self {
        Self(GENESIS.to_owned())
    }

    /// Wraps a hash already in hexadecimal — one read back from storage, or off the wire.
    ///
    /// Deliberately not validating: a malformed value read from a tampered database must compare
    /// **unequal** to the hash recomputed over the record, which is a chain break and exactly what
    /// verification exists to report. Refusing to construct it here would turn that break into a
    /// read error and lose which record it was.
    #[must_use]
    pub fn from_hex(hex: &str) -> Self {
        Self(hex.to_owned())
    }

    /// Wraps a digest the caller computed over [`preimage`].
    ///
    /// Takes the raw 32 bytes rather than a string, so a caller cannot pass the *preimage*
    /// where the *digest* belongs — the one confusion that would silently produce a chain
    /// of plaintext that still verifies against itself.
    #[must_use]
    pub fn of(digest: [u8; 32]) -> Self {
        let mut hex = String::with_capacity(64);
        for byte in digest {
            // `from_digit` answers `None` only for a radix above 36 or a digit at or above the
            // radix. A nibble is 0..=15 against radix 16, so neither fallback is reachable — and
            // it is a fallback rather than an `expect` because this crate may not panic.
            // `every_byte_value_round_trips_through_the_hex_encoder` is what proves the fallback
            // is never taken, rather than this comment.
            hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        Self(hex)
    }

    /// The hexadecimal form, for storage and comparison.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the genesis link rather than a real digest.
    #[must_use]
    pub fn is_genesis(&self) -> bool {
        self.0 == GENESIS
    }
}

impl fmt::Display for ChainHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Where a record sits in its store's chain.
///
/// `seq` is a per-store counter starting at one, with no gaps — and it, not `event_id`, is
/// what orders the chain ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md)
/// decision 6). Events sort by ULID elsewhere, and
/// [ADR-0026](../../../docs/adr/0026-port-shapes.md) §3 already records why that is unsafe
/// as a cursor once writers are concurrent: a chain built on ULID order would re-link
/// itself the first time two transactions interleaved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChainLink {
    /// This record's position in its store's chain. The first record is one.
    pub seq: u64,
    /// The hash of the record before it, or [`ChainHash::genesis`] for the first.
    pub prev_hash: ChainHash,
}

impl ChainLink {
    /// The link for a store's first record.
    #[must_use]
    pub fn first() -> Self {
        Self {
            seq: 1,
            prev_hash: ChainHash::genesis(),
        }
    }

    /// The link that follows a record with this hash at this position.
    #[must_use]
    pub fn following(previous_seq: u64, previous_hash: ChainHash) -> Self {
        Self {
            seq: previous_seq.saturating_add(1),
            prev_hash: previous_hash,
        }
    }
}

/// Why a chain does not verify.
///
/// Three reasons rather than one boolean, because they mean different things to whoever reads the
/// report: a missing record and an edited one are different incidents, and telling them apart is
/// most of the value of looking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainBreak {
    /// The sequence skips. A record was removed from the middle of the log.
    MissingRecord,
    /// A record does not link to the one before it.
    LinkMismatch,
    /// A record links correctly but no longer hashes to the hash stored beside it: its content
    /// was edited after it was written.
    ContentEdited,
}

/// What a walk of a store's chain found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ChainStatus {
    /// Every chained record links to the one before it and hashes to its stored hash.
    ///
    /// **This is not a statement that nothing was tampered with.** A chain verifies happily after
    /// its tail has been cut off, and after an edit whose later links were all re-derived — the
    /// two cases [ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) rejects a bare chain
    /// for. Only comparing `head` against an anchor the store cannot reach closes them.
    Intact {
        /// How many chained records were walked.
        checked: u64,
        /// How many records carry no chain because they predate it. Not a fault.
        unchained: u64,
        /// The hash of the last record, or `None` when nothing is chained yet. This is the value
        /// that is published to the cloud.
        head: Option<ChainHash>,
    },
    /// The walk stopped at a record that does not verify.
    Broken {
        /// The position the break was found at.
        at_seq: u64,
        /// What kind of break it is.
        reason: ChainBreak,
    },
}

impl ChainStatus {
    /// Whether the walk found a break.
    #[must_use]
    pub const fn is_broken(&self) -> bool {
        matches!(self, Self::Broken { .. })
    }
}

/// The exact bytes a chain hash is taken over.
///
/// `prev_hash`, `seq` and the serialized record, newline-separated. The separator is what
/// stops two different records sharing a preimage: without it, `seq` 1 followed by a body
/// starting `23` and `seq` 12 followed by a body starting `3` are the same byte string.
///
/// `body` is the record **as stored**, with its own chain fields absent — a record cannot
/// contain its own hash, and a preimage that included a partially-filled link would depend
/// on which half had been filled in.
#[must_use]
pub fn preimage(link: &ChainLink, body: &str) -> String {
    format!("{}\n{}\n{}", link.prev_hash.as_str(), link.seq, body)
}

#[cfg(test)]
mod tests {
    use super::{ChainHash, ChainLink, preimage};

    #[test]
    fn genesis_is_sixty_four_zeros_and_knows_it() {
        let genesis = ChainHash::genesis();
        assert_eq!(genesis.as_str().len(), 64);
        assert!(genesis.as_str().chars().all(|c| c == '0'));
        assert!(genesis.is_genesis());
        assert!(!ChainHash::of([0xff; 32]).is_genesis());
    }

    #[test]
    fn a_digest_renders_as_sixty_four_lowercase_hex_characters() {
        let hash = ChainHash::of(
            [0xde, 0xad, 0xbe, 0xef]
                .iter()
                .copied()
                .cycle()
                .take(32)
                .collect::<Vec<_>>()
                .try_into()
                .expect("32 bytes"),
        );
        assert_eq!(hash.as_str().len(), 64);
        assert!(hash.as_str().starts_with("deadbeef"));
        assert!(
            hash.as_str()
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "lowercase hex only: {hash}"
        );
    }

    #[test]
    fn every_byte_value_round_trips_through_the_hex_encoder() {
        // The hand-rolled encoder is the one place a nibble can be dropped, and a chain
        // whose hex is subtly wrong still verifies against itself — so it would only show
        // up when a *second* implementation disagreed.
        for chunk in 0_u16..8 {
            let mut digest = [0_u8; 32];
            for (offset, slot) in digest.iter_mut().enumerate() {
                let value = (chunk * 32).wrapping_add(u16::try_from(offset).unwrap_or(0)) % 256;
                *slot = u8::try_from(value).unwrap_or(0);
            }
            let hex = ChainHash::of(digest);
            let decoded: Vec<u8> = hex
                .as_str()
                .as_bytes()
                .chunks(2)
                .filter_map(|pair| {
                    let text = core::str::from_utf8(pair).ok()?;
                    u8::from_str_radix(text, 16).ok()
                })
                .collect();
            assert_eq!(decoded, digest, "chunk {chunk}");
        }
    }

    #[test]
    fn the_first_link_is_seq_one_chained_to_genesis() {
        let first = ChainLink::first();
        assert_eq!(first.seq, 1);
        assert!(first.prev_hash.is_genesis());
    }

    #[test]
    fn a_following_link_advances_the_sequence_and_carries_the_previous_hash() {
        let previous = ChainHash::of([7; 32]);
        let next = ChainLink::following(41, previous.clone());
        assert_eq!(next.seq, 42);
        assert_eq!(next.prev_hash, previous);
    }

    /// The separator is not decoration. Without it these two records share a preimage, so
    /// one could be swapped for the other with every hash in the chain still valid.
    #[test]
    fn the_preimage_separator_stops_two_records_colliding() {
        let link = |seq| ChainLink {
            seq,
            prev_hash: ChainHash::genesis(),
        };
        assert_ne!(
            preimage(&link(1), "23{\"a\":1}"),
            preimage(&link(12), "3{\"a\":1}"),
            "a numeric boundary that only the separator distinguishes"
        );
    }

    #[test]
    fn the_preimage_covers_the_link_and_not_only_the_body() {
        let body = r#"{"event_id":"01J","amount_minor":500000}"#;
        let at_one = preimage(&ChainLink::first(), body);
        let at_two = preimage(
            &ChainLink {
                seq: 2,
                prev_hash: ChainHash::genesis(),
            },
            body,
        );
        assert_ne!(
            at_one, at_two,
            "same body at a different position must hash differently, or two records swap places freely"
        );

        let after_other = preimage(
            &ChainLink {
                seq: 1,
                prev_hash: ChainHash::of([9; 32]),
            },
            body,
        );
        assert_ne!(
            at_one, after_other,
            "same body after a different record must hash differently, or the chain is not a chain"
        );
    }

    #[test]
    fn a_chain_hash_round_trips_through_json() {
        let hash = ChainHash::of([3; 32]);
        let json = serde_json::to_string(&hash).expect("serialise");
        assert_eq!(
            json,
            format!("\"{hash}\""),
            "transparent, so it is a bare string"
        );
        assert_eq!(
            serde_json::from_str::<ChainHash>(&json).expect("deserialise"),
            hash
        );
    }
}
