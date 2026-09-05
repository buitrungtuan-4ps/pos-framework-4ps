// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Version negotiation between a store and the cloud.
//!
//! Implements [ADR-0024](../../../docs/adr/0024-protocol-version-negotiation.md).
//! Several edge versions are always connected to one cloud at once — updates roll out
//! in rings and a store may be offline for days — so that is the normal state, not an
//! error.
//!
//! # The part that matters most
//!
//! A version mismatch degrades to **not syncing**, never to **not selling**. A store
//! that cannot talk to the cloud keeps trading, keeps everything in its outbox, backs
//! off with jitter, and raises the condition where somebody will see it. The same
//! property [ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md) buys
//! against a network outage is applied here to our own mistakes: shipping a botched
//! protocol change costs synchronisation latency and an alert, not revenue.
//!
//! Negotiation happens **once per connection**. Nothing renegotiates mid-session, and
//! nothing checks compatibility per event — the alternative discovers the same mismatch
//! forever, once per message.

use serde::{Deserialize, Serialize};

use crate::PROTOCOL_VERSION;
use crate::ids::StoreId;
use crate::text::ReleaseTag;

/// The oldest protocol version this build speaks.
///
/// The floor `docs/naming-and-api.md` §11 requires: the cloud must understand at least
/// the two most recent versions, because edges update in rings. A CI test asserts it, so
/// dropping support for a version still in the fleet fails the build rather than the
/// rollout.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = if PROTOCOL_VERSION > 1 {
    PROTOCOL_VERSION - 1
} else {
    1
};

// The floor, checked at compile time rather than in a test: a test asserting a property
// of two constants proves only that they were compiled, and this is a rule worth
// enforcing before anything runs. Widening the window past two versions now fails the
// build, which is where a decision about fleet compatibility should surface.
const _: () = assert!(
    PROTOCOL_VERSION <= MIN_SUPPORTED_PROTOCOL_VERSION + 1,
    "the cloud must support at most two protocol versions without a deliberate decision"
);
const _: () = assert!(
    MIN_SUPPORTED_PROTOCOL_VERSION <= PROTOCOL_VERSION,
    "the supported window cannot start above the current version"
);

/// An opaque proof that a machine is the single active server for its store.
///
/// Carried on the handshake so single-active enforcement and version negotiation share
/// one round trip ([ADR-0003](../../../docs/adr/0003-cattle-not-pets.md)).
///
/// `Debug` redacts the value. A lease token in a log line is a credential in a log line,
/// and logs travel to the cloud.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseToken(String);

impl LeaseToken {
    /// Wraps a token.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The token, for handing to the transport. Deliberately not `Display`.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for LeaseToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("LeaseToken(redacted)")
    }
}

/// The first frame a store sends on connecting.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Hello {
    /// The oldest protocol version this edge can speak.
    pub protocol_version_min: u32,
    /// The newest protocol version this edge can speak.
    pub protocol_version_max: u32,
    /// The edge's product release.
    ///
    /// **It does not reach the fleet view on the shipped path** (production-readiness **R2**). The
    /// only production [`MessageLink`](pos_ports::message_link::MessageLink) is `link-nats`, which is
    /// outbound-only by design ([ADR-0089](../../../docs/adr/0089-edge-event-bus-transport.md)): there is no
    /// cloud responder, so its `handshake` runs [`negotiate`] against its *own* compiled constants
    /// and this field is never transmitted. The console learns which binary a store runs from
    /// [`CloudSync::report`](pos_ports::cloud_sync::CloudSync::report) over `/sync`
    /// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)), which is a different rail.
    ///
    /// The field stays because the frame is the protocol's, not one transport's: the deferred
    /// bidirectional link (roadmap-v3 #89b, whose ADR is not written) reads it, and removing a
    /// member from a wire type is a `PROTOCOL_VERSION` change made for no gain.
    pub product_version: ReleaseTag,
    /// Which store is calling.
    pub store_id: StoreId,
    /// The lease this machine holds, when it has one.
    ///
    /// Absent during activation, before a lease has been issued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_token: Option<LeaseToken>,
}

impl Hello {
    /// A hello advertising this build's range.
    #[must_use]
    pub fn current(store_id: StoreId, product_version: ReleaseTag) -> Self {
        Self {
            protocol_version_min: MIN_SUPPORTED_PROTOCOL_VERSION,
            protocol_version_max: PROTOCOL_VERSION,
            product_version,
            store_id,
            lease_token: None,
        }
    }

    /// Attaches a lease token.
    #[must_use]
    pub fn with_lease(mut self, lease_token: LeaseToken) -> Self {
        self.lease_token = Some(lease_token);
        self
    }
}

/// What the cloud answers.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum HelloOutcome {
    /// Agreed. Both sides speak this version for the life of the connection.
    Accepted {
        /// The negotiated version: the highest both sides support.
        protocol_version: u32,
    },
    /// No overlap.
    ///
    /// The edge keeps selling, keeps its outbox, backs off, and raises the condition.
    /// It does **not** retry in a tight loop, and it does not stop trading.
    Refused {
        /// The oldest version the cloud speaks, so the edge can report what it needs.
        minimum_supported: u32,
        /// The newest version the cloud speaks.
        maximum_supported: u32,
    },
}

/// Chooses the version for a connection.
///
/// Pure, so the compatibility matrix is testable without a socket.
#[must_use]
pub fn negotiate(hello: &Hello, cloud_min: u32, cloud_max: u32) -> HelloOutcome {
    let lowest_common = hello.protocol_version_min.max(cloud_min);
    let highest_common = hello.protocol_version_max.min(cloud_max);
    if lowest_common > highest_common {
        return HelloOutcome::Refused {
            minimum_supported: cloud_min,
            maximum_supported: cloud_max,
        };
    }
    HelloOutcome::Accepted {
        protocol_version: highest_common,
    }
}

#[cfg(test)]
mod tests {
    use super::{Hello, HelloOutcome, LeaseToken, MIN_SUPPORTED_PROTOCOL_VERSION, negotiate};
    use crate::PROTOCOL_VERSION;
    use crate::ids::StoreId;
    use crate::text::ReleaseTag;
    use crate::ulid::Ulid;

    fn hello(min: u32, max: u32) -> Hello {
        Hello {
            protocol_version_min: min,
            protocol_version_max: max,
            product_version: ReleaseTag::new("v1.0.0"),
            store_id: StoreId::new(Ulid::NIL),
            lease_token: None,
        }
    }

    #[test]
    fn the_supported_window_tracks_the_current_version() {
        // The width of the window is asserted at compile time above; this checks that
        // the floor actually follows PROTOCOL_VERSION rather than being pinned by hand,
        // which is the mistake that would silently drop a version still in the fleet.
        if PROTOCOL_VERSION > 1 {
            assert_eq!(MIN_SUPPORTED_PROTOCOL_VERSION, PROTOCOL_VERSION - 1);
        } else {
            assert_eq!(MIN_SUPPORTED_PROTOCOL_VERSION, 1);
        }
    }

    #[test]
    fn a_build_accepts_both_versions_in_its_window() {
        // The rule `docs/naming-and-api.md` §11 states, exercised through the actual
        // negotiation rather than asserted about constants.
        for edge_version in [MIN_SUPPORTED_PROTOCOL_VERSION, PROTOCOL_VERSION] {
            let outcome = negotiate(
                &hello(edge_version, edge_version),
                MIN_SUPPORTED_PROTOCOL_VERSION,
                PROTOCOL_VERSION,
            );
            assert_eq!(
                outcome,
                HelloOutcome::Accepted {
                    protocol_version: edge_version
                },
                "an edge on protocol {edge_version} must be accepted"
            );
        }
    }

    #[test]
    fn negotiation_picks_the_highest_version_both_sides_speak() {
        assert_eq!(
            negotiate(&hello(1, 3), 2, 4),
            HelloOutcome::Accepted {
                protocol_version: 3
            }
        );
        assert_eq!(
            negotiate(&hello(2, 5), 1, 3),
            HelloOutcome::Accepted {
                protocol_version: 3
            }
        );
    }

    #[test]
    fn an_exact_match_is_accepted() {
        assert_eq!(
            negotiate(&hello(2, 2), 2, 2),
            HelloOutcome::Accepted {
                protocol_version: 2
            }
        );
    }

    #[test]
    fn an_edge_too_old_is_refused_with_the_cloud_range() {
        // The refusal names what the cloud needs, so the alert says something useful
        // rather than just "incompatible".
        assert_eq!(
            negotiate(&hello(1, 1), 3, 4),
            HelloOutcome::Refused {
                minimum_supported: 3,
                maximum_supported: 4
            }
        );
    }

    #[test]
    fn an_edge_newer_than_the_cloud_downgrades_when_it_can() {
        // Should not happen — the cloud is upgraded before edge rings roll — but if it
        // does, the edge speaks down rather than refusing.
        assert_eq!(
            negotiate(&hello(2, 9), 1, 2),
            HelloOutcome::Accepted {
                protocol_version: 2
            }
        );
        // And is refused only when there is genuinely no overlap.
        assert_eq!(
            negotiate(&hello(5, 9), 1, 2),
            HelloOutcome::Refused {
                minimum_supported: 1,
                maximum_supported: 2
            }
        );
    }

    #[test]
    fn a_current_build_negotiates_with_itself() {
        let outcome = negotiate(
            &Hello::current(StoreId::new(Ulid::NIL), ReleaseTag::new("v1.0.0")),
            MIN_SUPPORTED_PROTOCOL_VERSION,
            PROTOCOL_VERSION,
        );
        assert_eq!(
            outcome,
            HelloOutcome::Accepted {
                protocol_version: PROTOCOL_VERSION
            }
        );
    }

    #[test]
    fn a_lease_token_never_appears_in_debug_output() {
        // Logs travel to the cloud, so a credential rendered by accident is a
        // credential leaked.
        let token = LeaseToken::new("super-secret-lease-value");
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("super-secret"), "got {rendered}");
        assert_eq!(rendered, "LeaseToken(redacted)");

        // And the same inside the frame that carries it.
        let frame = Hello::current(StoreId::new(Ulid::NIL), ReleaseTag::new("v1.0.0"))
            .with_lease(LeaseToken::new("super-secret-lease-value"));
        assert!(!format!("{frame:?}").contains("super-secret"));
    }

    #[test]
    fn a_lease_token_still_travels_on_the_wire() {
        // Redacting Debug must not redact serialisation, or the handshake would fail.
        let frame = Hello::current(StoreId::new(Ulid::NIL), ReleaseTag::new("v1.0.0"))
            .with_lease(LeaseToken::new("lease-abc"));
        let json = serde_json::to_string(&frame).expect("serialise");
        assert!(json.contains("lease-abc"));
        let back: Hello = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, frame);
    }

    #[test]
    fn an_absent_lease_is_omitted() {
        // A machine mid-activation has no lease yet.
        let frame = Hello::current(StoreId::new(Ulid::NIL), ReleaseTag::new("v1.0.0"));
        let json = serde_json::to_string(&frame).expect("serialise");
        assert!(!json.contains("lease_token"), "got {json}");
    }

    #[test]
    fn the_outcome_is_tagged_so_a_client_can_branch_before_reading_fields() {
        let json = serde_json::to_string(&HelloOutcome::Accepted {
            protocol_version: 2,
        })
        .expect("serialise");
        assert_eq!(json, r#"{"outcome":"accepted","protocol_version":2}"#);
    }
}
