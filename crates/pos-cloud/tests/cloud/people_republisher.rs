// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The people republisher (ADR-0158): a grant migration's stores, published again by the cloud.

use std::sync::Arc;

use pos_cloud::audit::{AuditEntry, AuditRecorder, AuditSink, AuditStore, NoopAuditRecorder};
use pos_cloud::auth::admin::{AdminRole, SYSTEM_ACTOR_EMAIL, SYSTEM_ACTOR_ID};
use pos_cloud::config_tree::{ConfigTreeState, ConfigTreeStore};
use pos_cloud::health::TaskHealthStore;
use pos_cloud::people::{
    AssignmentId, AssignmentScope, AssignmentStore, EmployeeId, NewAssignment,
};
use pos_cloud::people_republish;
use pos_cloud::people_republish::{
    DrainTally, InMemoryPeopleRepublishes, PEOPLE_REPUBLISHER, PeopleRepublish,
    PeopleRepublishStore,
};
use pos_cloud::version::UpdateOutcome;
use pos_proto::ids::StoreId;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use crate::{
    FakeAudit, FakeConfigTrees, FakePeople, FakeTaskHealth, NOW_MS, admin_cookie, clock,
    people_publish_app, people_republish_contract, provisioned_admin, publish_people,
    seed_a_cashier, staff_at, store_id, tenant, versions_at,
};

// --- The people republisher (ADR-0158): a grant migration's stores, published again by the cloud --

/// The in-memory queue holds the seam's rules, the ones `PostgresPeopleRepublishes` is held to in
/// `tests/people_republish_store_postgres.rs`: one row per store, read in tenant then store order
/// past the last row read, and cleared only while it carries the time it was read with.
#[tokio::test]
async fn the_in_memory_people_republish_queue_holds_the_contract() {
    people_republish_contract::holds(&InMemoryPeopleRepublishes::new(), 0).await;
}

/// The reason migration 0083 queues every store under.
const REPUBLISH_REASON: &str = "0083_people_republishes";

/// A second store of the tenant.
fn second_store() -> StoreId {
    StoreId::new(Ulid::from_u128(0x0ADB))
}

/// Writes a store's tree as a publish would have left it, its four layers as given and no history.
async fn seed_tree(config: &FakeConfigTrees, store: StoreId, layers: [serde_json::Value; 4]) {
    let state = ConfigTreeState {
        layers,
        history: Vec::new(),
        k: 8,
    };
    let saved = config
        .save(tenant(), store, &state, None)
        .await
        .expect("seed the tree");
    assert!(matches!(saved, UpdateOutcome::Updated(_)), "{saved:?}");
}

/// A tree whose Store layer is `store_layer` and whose other layers are empty.
fn store_layer_only(store_layer: serde_json::Value) -> [serde_json::Value; 4] {
    [
        serde_json::json!({}),
        serde_json::json!({}),
        store_layer,
        serde_json::json!({}),
    ]
}

/// A store's `permissions` node from before a grant: nobody on its roster.
fn people_before_the_grant(store: StoreId) -> serde_json::Value {
    serde_json::json!({ "store_id": store.to_string(), "staff": [] })
}

/// Queues a store as migration 0083 queues it, at `ms`.
async fn queue_store(queue: &InMemoryPeopleRepublishes, store: StoreId, ms: i64) {
    queue
        .enqueue(&PeopleRepublish {
            tenant_id: tenant(),
            store_id: store,
            reason: REPUBLISH_REASON.to_owned(),
            enqueued_time: Timestamp::from_milliseconds_since_epoch(ms).expect("a valid time"),
        })
        .await
        .expect("queue the store");
}

/// Every store still queued, in order.
async fn still_queued(queue: &InMemoryPeopleRepublishes) -> Vec<StoreId> {
    queue
        .pending(None, 1_000)
        .await
        .expect("read the queue")
        .into_iter()
        .map(|row| row.store_id)
        .collect()
}

/// The `permissions.publish` entries the trail holds, oldest first.
async fn people_publishes(audit: &FakeAudit) -> Vec<AuditEntry> {
    let mut entries: Vec<AuditEntry> = audit
        .list(None, 1_000)
        .await
        .expect("read the trail")
        .into_iter()
        .filter(|entry| entry.action == "permissions.publish")
        .collect();
    entries.reverse();
    entries
}

/// Drains the queue once, as the republisher's loop does, recording into `audit`.
async fn drain_once(
    queue: &InMemoryPeopleRepublishes,
    people: &FakePeople,
    config: &FakeConfigTrees,
    audit: &FakeAudit,
) -> DrainTally {
    let recorder: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(audit.clone()));
    people_republish::drain(queue, people, config, &recorder, &clock())
        .await
        .expect("drain the queue")
}

#[tokio::test]
async fn a_queued_store_is_published_again_under_the_system_actor_and_leaves_the_queue() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let queue = InMemoryPeopleRepublishes::new();
    let store = store_id();
    seed_a_cashier(&people, store).await;
    seed_tree(
        &config,
        store,
        store_layer_only(serde_json::json!({ "permissions": people_before_the_grant(store) })),
    )
    .await;
    queue_store(&queue, store, NOW_MS).await;

    let tally = drain_once(&queue, &people, &config, &audit).await;

    assert_eq!(
        tally,
        DrainTally {
            published: 1,
            ..DrainTally::default()
        }
    );
    let staff = staff_at(&config, store).await;
    assert_eq!(staff.len(), 1, "{staff:?}");
    assert_eq!(
        staff[0]["permissions"],
        serde_json::json!(["billing.discount.apply"]),
        "the node carries what the person's role grants now"
    );
    assert_eq!(versions_at(&config, store).await, 1, "one new version");
    assert!(
        still_queued(&queue).await.is_empty(),
        "a store published again leaves the queue"
    );

    // Recorded as the button's publish is, under the system actor, with the queue's reason.
    let entries = people_publishes(&audit).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.actor.admin_id, SYSTEM_ACTOR_ID);
    assert_eq!(entry.actor.email, SYSTEM_ACTOR_EMAIL);
    assert_eq!(entry.actor.role, AdminRole::Ops);
    assert_eq!(entry.tenant_id, Some(tenant()));
    assert_eq!(
        (entry.entity_type.as_str(), entry.entity_id.as_str()),
        ("store", store.to_string().as_str())
    );
    let version = config
        .load(tenant(), store)
        .await
        .expect("load the tree")
        .and_then(|tree| {
            tree.record
                .history
                .last()
                .map(|version| version.id.to_string())
        })
        .expect("a version");
    assert_eq!(
        entry.after,
        Some(serde_json::json!({
            "config_version_id": version,
            "staff_count": 1,
            "reason": REPUBLISH_REASON,
        })),
        "the version, the staff count and the reason: nothing about anyone"
    );
    assert_eq!(entry.before, None);
    let recorded = format!("{:?} {:?}", entry.after, entry.actor);
    for personal in ["Alice", "C01", "argon2"] {
        assert!(
            !recorded.contains(personal),
            "no name, code or PIN hash in the trail: {recorded}"
        );
    }
}

#[tokio::test]
async fn a_queued_store_whose_node_is_current_is_given_no_version() {
    let admin = provisioned_admin();
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let queue = InMemoryPeopleRepublishes::new();
    let store = store_id();
    seed_a_cashier(&people, store).await;
    // Published by the button: the node is what the store's people compile to.
    let router = people_publish_app(
        admin,
        people.clone(),
        config.clone(),
        Arc::new(AuditSink::new(audit.clone())),
    );
    let cookie = admin_cookie(&router).await;
    publish_people(&router, &cookie, store).await;
    let versions = versions_at(&config, store).await;
    queue_store(&queue, store, NOW_MS).await;

    let tally = drain_once(&queue, &people, &config, &audit).await;

    assert_eq!(
        tally,
        DrainTally {
            unchanged: 1,
            ..DrainTally::default()
        }
    );
    assert_eq!(
        versions_at(&config, store).await,
        versions,
        "a store whose node is current is given no new version"
    );
    assert!(
        still_queued(&queue).await.is_empty(),
        "and leaves the queue"
    );
    assert_eq!(
        people_publishes(&audit).await.len(),
        1,
        "only the button's publish is recorded"
    );
}

#[tokio::test]
async fn a_queued_store_whose_people_were_never_published_is_given_no_node() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let queue = InMemoryPeopleRepublishes::new();
    let (alice, cashier) = seed_a_cashier(&people, store_id()).await;
    AssignmentStore::assign(
        &people,
        &NewAssignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(2)),
            tenant_id: tenant(),
            employee_id: alice,
            scope: AssignmentScope::Store(second_store()),
            role_template_id: cashier,
        },
    )
    .await
    .expect("assign");
    // `store_id()` has no tree at all. `second_store()` has a tree whose only `permissions` node is
    // the switch the settings publish writes on the Tenant layer, and no people node.
    seed_tree(
        &config,
        second_store(),
        [
            serde_json::json!({ "permissions": { "enforced": true } }),
            serde_json::json!({}),
            serde_json::json!({}),
            serde_json::json!({}),
        ],
    )
    .await;
    queue_store(&queue, store_id(), NOW_MS).await;
    queue_store(&queue, second_store(), NOW_MS).await;

    let tally = drain_once(&queue, &people, &config, &audit).await;

    assert_eq!(
        tally,
        DrainTally {
            never_published: 2,
            ..DrainTally::default()
        }
    );
    assert!(
        config
            .load(tenant(), store_id())
            .await
            .expect("load the tree")
            .is_none(),
        "no tree is created for a store that had none"
    );
    let tree = config
        .load(tenant(), second_store())
        .await
        .expect("load the tree")
        .expect("the tree it had");
    assert!(
        tree.record.layers[2].get("permissions").is_none(),
        "no people node is created beside the switch"
    );
    assert!(tree.record.history.is_empty(), "and no version");
    assert!(
        still_queued(&queue).await.is_empty(),
        "both leave the queue"
    );
    assert!(people_publishes(&audit).await.is_empty());
}

/// The tree a rollback leaves ([`ConfigTree::restore_with`]): the restored version whole on the
/// Tenant layer, here a roster from before the grant beside the switch, and the other three empty.
fn rolled_back(store: StoreId) -> [serde_json::Value; 4] {
    let alice = EmployeeId::new(Ulid::from_u128(0xA11));
    [
        serde_json::json!({
            "permissions": {
                "store_id": store.to_string(),
                "enforced": true,
                "staff": [{
                    "id": alice.to_string(),
                    "code": "C01",
                    "name": "Alice",
                    "permissions": [],
                    "pin_phc": "argon2id$phc$alice",
                }],
            },
        }),
        serde_json::json!({}),
        serde_json::json!({}),
        serde_json::json!({}),
    ]
}

#[tokio::test]
async fn a_rolled_back_store_is_published_again_onto_its_store_layer() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let queue = InMemoryPeopleRepublishes::new();
    let store = store_id();
    seed_a_cashier(&people, store).await;
    seed_tree(&config, store, rolled_back(store)).await;
    queue_store(&queue, store, NOW_MS).await;

    let tally = drain_once(&queue, &people, &config, &audit).await;

    assert_eq!(
        tally,
        DrainTally {
            published: 1,
            ..DrainTally::default()
        },
        "a roster a rollback moved onto the Tenant layer is a published roster"
    );
    let tree = config
        .load(tenant(), store)
        .await
        .expect("load the tree")
        .expect("the tree")
        .record;
    let written = &tree.layers[2]["permissions"];
    assert_eq!(
        written["staff"][0]["permissions"],
        serde_json::json!(["billing.discount.apply"]),
        "the node its people compile to now, written on the Store layer"
    );
    let runs = tree.effective();
    assert_eq!(
        runs["permissions"]["staff"], written["staff"],
        "the store runs the compiled roster, not the one the rollback left"
    );
    assert_eq!(
        runs["permissions"]["enforced"],
        serde_json::json!(true),
        "and keeps the switch the rollback restored"
    );
    assert_eq!(tree.history.len(), 1, "one new version");
    assert!(still_queued(&queue).await.is_empty());
    assert_eq!(people_publishes(&audit).await.len(), 1);
}

#[tokio::test]
async fn a_store_whose_store_layer_holds_the_switch_alone_is_given_no_roster() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let queue = InMemoryPeopleRepublishes::new();
    let store = store_id();
    // People who would compile to a roster, and a `permissions` node typed on the Store layer
    // with the switch and no `staff` key: the edge reads that as no roster.
    seed_a_cashier(&people, store).await;
    let switch_alone = serde_json::json!({ "permissions": { "enforced": true } });
    seed_tree(&config, store, store_layer_only(switch_alone.clone())).await;
    queue_store(&queue, store, NOW_MS).await;

    let tally = drain_once(&queue, &people, &config, &audit).await;

    assert_eq!(
        tally,
        DrainTally {
            never_published: 1,
            ..DrainTally::default()
        }
    );
    let tree = config
        .load(tenant(), store)
        .await
        .expect("load the tree")
        .expect("the tree")
        .record;
    assert_eq!(
        tree.layers[2], switch_alone,
        "no roster is created beside the switch"
    );
    assert!(tree.history.is_empty(), "and no version");
    assert!(still_queued(&queue).await.is_empty(), "it leaves the queue");
    assert!(people_publishes(&audit).await.is_empty());
}

#[tokio::test]
async fn a_store_that_cannot_be_published_keeps_its_row_and_the_drain_reaches_every_other() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let audit = FakeAudit::default();
    let queue = InMemoryPeopleRepublishes::new();
    seed_a_cashier(&people, store_id()).await;
    // More stores than one read of the queue hands over. Every other one holds a stale people node
    // beside a rollout no publish can validate, so every publish to it is refused; the rest hold
    // no tree, so their people were never published.
    let count = people_republish::DRAIN_BATCH + 6;
    let mut refusing = Vec::new();
    for n in 0..count {
        let store = StoreId::new(Ulid::from_u128(0xB000 + u128::try_from(n).expect("small")));
        if n % 2 == 1 {
            seed_tree(
                &config,
                store,
                store_layer_only(serde_json::json!({
                    "permissions": people_before_the_grant(store_id()),
                    "fleet_update": "not a rollout",
                })),
            )
            .await;
            refusing.push(store);
        }
        queue_store(&queue, store, NOW_MS).await;
    }
    // And the store with people, published again past all of them.
    seed_tree(
        &config,
        store_id(),
        store_layer_only(serde_json::json!({ "permissions": people_before_the_grant(store_id()) })),
    )
    .await;
    queue_store(&queue, store_id(), NOW_MS).await;

    let tally = drain_once(&queue, &people, &config, &audit).await;

    assert_eq!(
        tally,
        DrainTally {
            published: 1,
            unchanged: 0,
            never_published: count - refusing.len(),
            failed: refusing.len(),
        }
    );
    assert_eq!(
        still_queued(&queue).await,
        refusing,
        "a store that cannot be published keeps its row for the next drain, and only such a store"
    );
    for store in &refusing {
        assert_eq!(
            versions_at(&config, *store).await,
            0,
            "a refused store is given no version"
        );
    }
    assert_eq!(staff_at(&config, store_id()).await.len(), 1);

    // The next drain tries them again, and keeps them again.
    let again = drain_once(&queue, &people, &config, &audit).await;
    assert_eq!(
        again,
        DrainTally {
            failed: refusing.len(),
            ..DrainTally::default()
        }
    );
    assert_eq!(still_queued(&queue).await, refusing);
}

#[tokio::test]
async fn the_people_republisher_drains_at_start_up_and_records_its_tick() {
    let people = FakePeople::default();
    let config = FakeConfigTrees::default();
    let queue = InMemoryPeopleRepublishes::new();
    let task_health = FakeTaskHealth::default();
    let store = store_id();
    seed_a_cashier(&people, store).await;
    seed_tree(
        &config,
        store,
        store_layer_only(serde_json::json!({ "permissions": people_before_the_grant(store) })),
    )
    .await;
    queue_store(&queue, store, NOW_MS).await;

    // Asked to stop before it ever waits: what runs is the drain at start-up.
    people_republish::run(
        queue.clone(),
        people.clone(),
        config.clone(),
        Arc::new(NoopAuditRecorder),
        task_health.clone(),
        clock(),
        people_republish::DRAIN_INTERVAL,
        async {},
    )
    .await;

    assert_eq!(
        staff_at(&config, store).await.len(),
        1,
        "published at start-up"
    );
    assert!(still_queued(&queue).await.is_empty());
    let ticks = task_health.list_health().await.expect("read the ticks");
    let tick = ticks
        .iter()
        .find(|tick| tick.task == PEOPLE_REPUBLISHER)
        .expect("the republisher recorded its tick");
    assert_eq!(
        tick.detail,
        serde_json::json!({
            "ok": true,
            "interval_secs": people_republish::DRAIN_INTERVAL.as_secs(),
            "published": 1,
            "unchanged": 0,
            "never_published": 0,
            "failed": 0,
        })
    );
}
