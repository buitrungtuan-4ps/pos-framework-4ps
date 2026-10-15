// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One store as the fleet console sees it.

use pos_core::lease::LeaseGeneration;

use super::{FLEET_ONLINE_THRESHOLD_MS, FleetPrintAgent, RegionAcknowledgementView};
use crate::fleet::FleetRow;
use crate::lease::{HandoverFacts, HandoverState, handover_state};
use crate::registry::EntityStatus;

/// One store as the fleet console sees it: the seam's raw facts plus the two verdicts derived at read
/// time — `online` (from `last_seen_at` against [`FLEET_ONLINE_THRESHOLD_MS`]) and `config_current`
/// (the held version equals the published one). Timestamps are Unix milliseconds, the shape the
/// dashboard reads; the store id is a string so the console shows a name and hides the ULID.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct FleetStoreView {
    store_id: String,
    name: String,
    status: EntityStatus,
    online: bool,
    last_seen_at_ms: Option<i64>,
    last_config_pull_at_ms: Option<i64>,
    config_version_held: Option<String>,
    config_version_published: Option<String>,
    config_current: bool,
    relay_backlog: u64,
    relay_oldest_pending_at_ms: Option<i64>,
    /// The binary version the store last reported running (ADR-0078), or `null` if it has never
    /// reported.
    installed_version: Option<String>,
    /// Whether the store's last post-install self-test passed, or `null`.
    self_test_ok: Option<bool>,
    /// Unix ms of the store's most recent OTA report, or `null`.
    reported_at_ms: Option<i64>,
    /// How many events the store had committed and not yet published as of its last heartbeat, or
    /// `null` if it has never reported one. The mirror of `relay_backlog`: that counts orders held
    /// *for* the store, this counts sales held *at* it. `null` is not zero — a store that never said
    /// is not a store that is caught up — so the console renders the two differently.
    outbox_depth: Option<u64>,
    /// Unix ms of the heartbeat that reported `outbox_depth`, or `null`.
    outbox_reported_at_ms: Option<i64>,
    /// The print agents the store last reported, or `null` if it never has
    /// ([ADR-0112](../../../../../docs/adr/0112-print-agents.md)).
    ///
    /// `null` and `[]` are different answers and the console renders them differently: `null` is a
    /// store that has said nothing about agents — every store whose edge runs in the shop, and every
    /// edge on a binary older than this field — while `[]` is a store that looked and has none.
    print_agents: Option<Vec<FleetPrintAgent>>,
    /// Unix ms of the heartbeat that reported them, or `null`. A standing is only as current as the
    /// beat that carried it, so the instant travels with the list.
    print_agents_reported_at_ms: Option<i64>,
    /// The lease generation the box last reported holding
    /// ([ADR-0108](../../../../../docs/adr/0108-the-lease-generation-is-authority.md)), or `null` if it
    /// has never said.
    lease_generation_held: Option<u64>,
    /// Unix ms of the heartbeat that reported it, or `null`.
    lease_reported_at_ms: Option<i64>,
    /// The store's authoritative lease generation, or `null` if the cloud has never issued it one.
    lease_generation_authoritative: Option<u64>,
    /// Whether this box has been **superseded**: it holds a generation the store has moved past, so
    /// it refuses over-the-air updates and is no longer the machine the store runs on.
    ///
    /// Derived here rather than stored, exactly as `online` and `config_current` are: it is a
    /// comparison of two numbers the row already carries, and deriving it in one place keeps the
    /// console from re-implementing `lease_standing` in TypeScript. `false` when either number is
    /// absent — a store with no lease in force, or a box that has not reported one, is not
    /// *superseded*; it is simply not under the lease yet, and saying otherwise would put a red
    /// badge on every store the day this shipped.
    lease_superseded: bool,
    /// Where the machine holding the authoritative generation runs — the `EDGE_PLACEMENT_*` token,
    /// or `null` ([ADR-0110](../../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// `null` covers two different things on purpose, and the console must not read it as a mode:
    /// the cloud has never bumped this store, or the stored token is one this build cannot decode.
    /// Neither is `EDGE_PLACEMENT_UNSPECIFIED` — on the wire that token means *this message did not
    /// say*, and a server that emitted it would be saying something. A field that is simply absent
    /// is the honest shape for both.
    ///
    /// What the two absences mean for *urgency* is deliberately not on this view, because the
    /// console does not decide it: the alert engine reads the same `FleetRow` and scores an
    /// undecodable token with the hosted case rather than the in-store one.
    edge_placement: Option<&'static str>,
    /// Where in the world that machine is — its ISO 3166-1 alpha-2 country — or `null`
    /// ([ADR-0114](../../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// `null` for a store nobody has bumped, for one whose edge placement is in-store (the machine
    /// is in the shop and has no region), and for a row written before migration `0058`. All three
    /// mean the same thing to a reader — there is nothing to show — which is why this is a plain
    /// `Option` and the placement beside it is not.
    region_country: Option<String>,
    /// The place a person recognises — `ap-southeast-1`, `Ho Chi Minh City`, `the rack in Sakai` —
    /// or `null`. Present exactly when [`region_country`](Self::region_country) is.
    ///
    /// Free text an admin typed, carried verbatim and never parsed. It is on this read because the
    /// question it answers ("where has this store been running?") is asked while somebody has the
    /// fleet page open, not afterwards.
    region_label: Option<String>,
    /// The country this store publishes in its `locale` node
    /// ([ADR-0114](../../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// **Present only on the single-store read**, which is the one route that loads a store's
    /// configuration; the listing omits the key entirely rather than sending `null`, so a console
    /// can tell "not compared here" from "compared, and there is no country". Absence must never
    /// render as agreement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) profile_country: Option<String>,
    /// Whether the region agrees with that country — `agrees`, `differs`, `country-not-recorded` —
    /// or absent when there is nothing to compare
    /// ([ADR-0114](../../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Derived by [`region_agreement`] rather than in the console, for the reason `handover` beside
    /// it is: the third case is the one that is easy to get wrong, and getting it wrong means a
    /// store with no recorded country renders as agreeing.
    ///
    /// Omitted on the listing, exactly as [`profile_country`](Self::profile_country) is — a
    /// comparison there would be one config-tree load per row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) region_agreement: Option<&'static str>,
    /// The answer somebody recorded for **this** difference, or absent
    /// ([ADR-0114](../../../../../docs/adr/0114-region-is-required-recorded-visible.md)).
    ///
    /// Present only when a stored acknowledgement names the same two country codes the store has
    /// right now. An answer written about a difference the store no longer has is not returned —
    /// see [`standing_acknowledgement`] — so a console that renders "acknowledged" whenever this key
    /// is present is correct without knowing the rule.
    ///
    /// Omitted on the listing with the two fields above it, for the same reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) region_acknowledgement: Option<RegionAcknowledgementView>,
    /// The generation the last bump displaced and nothing has yet proved drained, or `null`
    /// ([ADR-0110](../../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// Shown beside [`handover`](Self::handover) rather than in place of it: the state says *what*
    /// is happening and this says *which machine* it is about, which is the number an operator needs
    /// to type into a settle.
    lease_superseded_generation: Option<u64>,
    /// Which state this store's handover is in — `taking-over`, `settled`, `retired` — or `null`
    /// when it has never had one
    /// ([ADR-0110](../../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// Derived here, not stored, exactly as `online` and `config_current` are, and by
    /// [`handover_state`] so the console never re-implements the rule — in particular the same-beat
    /// guard, which is the half that is easy to get wrong and expensive to get wrong.
    ///
    /// `null` is a real answer and the commonest one: a store on generation `0` has never been
    /// replaced, and a store with no lease row has never been issued one. Neither is a handover in
    /// any state, so the console renders no badge rather than a reassuring one.
    handover: Option<&'static str>,
    /// When a person decided this handover's outgoing machine is no longer needed, or `null`.
    retired_at_ms: Option<i64>,
    /// The deciding admin's id, or `null`.
    ///
    /// An id, and the console resolves it to a name through the admin roster if it wants one. The
    /// email lives in the audit trail, which is read under a permission this view is not behind.
    retired_by: Option<String>,
}

impl FleetStoreView {
    /// Builds the view from a seam row, deriving `online` against `now_ms` and `config_current` from
    /// the held-vs-published comparison (a store that has published nothing, or holds nothing, is not
    /// "current" — there is a gap to close either way).
    pub(super) fn from_row(row: FleetRow, now_ms: i64) -> Self {
        let last_seen_at_ms = row
            .last_seen_at
            .map(pos_proto::Timestamp::as_milliseconds_since_epoch);
        let online = last_seen_at_ms
            .is_some_and(|seen| now_ms.saturating_sub(seen) <= FLEET_ONLINE_THRESHOLD_MS);
        let config_current = match (&row.config_version_held, &row.config_version_published) {
            (Some(held), Some(published)) => held == published,
            _ => false,
        };
        // `lease_standing` is the domain's, and this is the console's read of it: a box behind its
        // store's authority has been replaced. A box *ahead* of it (`Invalid`) also refuses, and is
        // deliberately not shown as superseded — that is corruption or a restored backup, which a
        // "replaced" badge would mislabel; it surfaces as the two numbers disagreeing.
        let lease_superseded = matches!(
            (row.lease_generation_held, row.lease_generation_authoritative),
            (Some(held), Some(authoritative)) if held < authoritative
        );
        Self {
            store_id: row.store_id.to_string(),
            name: row.name,
            status: row.status,
            online,
            last_seen_at_ms,
            last_config_pull_at_ms: row
                .last_config_pull_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            config_version_held: row.config_version_held,
            config_version_published: row.config_version_published,
            config_current,
            relay_backlog: row.relay_backlog,
            relay_oldest_pending_at_ms: row
                .relay_oldest_pending_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            installed_version: row.installed_version,
            self_test_ok: row.self_test_ok,
            reported_at_ms: row
                .reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            outbox_depth: row.outbox_depth,
            outbox_reported_at_ms: row
                .outbox_reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            print_agents: row.print_agents.clone().map(|agents| {
                agents
                    .into_iter()
                    .map(|agent| FleetPrintAgent {
                        agent_device_id: agent.agent_device_id,
                        paired_device_id: agent.paired_device_id,
                        oldest_unacknowledged_secs: agent.oldest_unacknowledged_secs,
                    })
                    .collect()
            }),
            print_agents_reported_at_ms: row
                .print_agents_reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            lease_generation_held: row.lease_generation_held,
            lease_reported_at_ms: row
                .lease_reported_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            lease_generation_authoritative: row.lease_generation_authoritative,
            lease_superseded,
            edge_placement: row.edge_placement.as_wire(),
            region_country: row
                .region
                .as_ref()
                .map(|region| region.country_str().to_owned()),
            region_label: row.region.as_ref().map(|region| region.label().to_owned()),
            // Filled in by the single-store route after this builds; the listing leaves all three
            // absent, because it does not perform the comparison and so has nothing an
            // acknowledgement could be measured against.
            profile_country: None,
            region_agreement: None,
            region_acknowledgement: None,
            lease_superseded_generation: row.superseded_generation,
            handover: handover_state(HandoverFacts {
                authoritative: row.lease_generation_authoritative.map(LeaseGeneration::new),
                superseded: row.superseded_generation.map(LeaseGeneration::new),
                retired_at: row.retired_at,
                held: row.lease_generation_held.map(LeaseGeneration::new),
                outbox_depth: row.outbox_depth,
                outbox_reported_at: row.outbox_reported_at,
                lease_reported_at: row.lease_reported_at,
            })
            .map(HandoverState::as_wire),
            retired_at_ms: row
                .retired_at
                .map(pos_proto::Timestamp::as_milliseconds_since_epoch),
            retired_by: row.retired_by,
        }
    }
}
