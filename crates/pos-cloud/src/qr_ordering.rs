// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! QR ordering is one switch
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 5).
//!
//! Three things used to decide whether a store took a guest's QR order, and they could disagree:
//!
//! 1. the capability flag `qr_ordering_enabled`, which nothing read;
//! 2. `qr.enabled` on the `qr` node, which only the cloud's guest intake read, and which counted as
//!    on when it was absent;
//! 3. the QR channel in a published `channels` list, without which the edge refused the order.
//!
//! The flag is the switch now ([`SWITCH`]). The cloud's guest intake ([`crate::qr_http`]) reads it,
//! and the edge refuses a QR order where it is published off, with `qr.enabled` off beside it,
//! which tells it from the flag as it stood before the switch; where a store's configuration does
//! not carry the switch, the edge decides by the channel list, as before. The other two follow the
//! switch, so that an edge on an earlier release, which reads only the channel list, and anything
//! still reading `qr.enabled` agree with it:
//!
//! - **`qr.enabled`** is set to the switch whenever the switch is written, and on every write of
//!   the `qr` node. A `qr` write that names `enabled` is a write of the switch, so an older
//!   console's checkbox and a cohort copy of a store's `qr` node still do what they did.
//! - **The QR channel** is added to a published `channels` list while the switch is on, and taken
//!   out while it is off, on every write of the switch or of the list ([`compose_channels`]). What
//!   a list means does not change. An absent list is every channel, so nothing is published for
//!   it; an empty list is no channel, so it stays empty; and a list holding only the QR channel
//!   keeps it when the switch goes off, since an empty list would refuse every order.
//!
//! [`align`] does both for a write to the Store layer, inside the write's conditional-write retry,
//! so it composes on what the tree holds at that moment.
//!
//! # A rollback to a version from before the switch
//!
//! Such a version carries the flag as it was while it did nothing. Restored as it is, it would
//! switch QR ordering by a value nobody chose, so a rollback settles it ([`settle`]): it is given
//! the switch it ran. The version's id says when it was published, and the switch's one-time
//! setting records when it began ([`OneSwitch::since`], [`predates`]). A version published since is
//! restored as it is.
//!
//! # The switch every store had already
//!
//! An absent flag reads as off, while an absent `qr.enabled` read as on. So a store taking QR orders
//! may carry no flag at all, or a `false` that the console's capability form wrote while the flag
//! did nothing. [`run_once`] sets each store's switch from what the store did
//! ([`taken_before_the_switch`]), by publishing a new configuration version its edge pulls, never by
//! writing the tree's rows. What a store did is read from the version it runs, which is what its
//! edge holds and what the guest intake read. A store whose three already agree ([`aligned`]) runs
//! as it did and is left alone. The setting runs once, behind the [`MIGRATION`] marker in
//! `data_migrations`, before the cloud serves, and a second run changes nothing.
//!
//! Nothing here is personal data: switches, channel tokens and store ids.

use serde_json::{Map, Value};

use pos_core::capability::{Capability, CapabilityContext};
use pos_proto::SalesChannel;
use pos_proto::channels::PublishedChannels;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::WireEnum;

use crate::config_tree::merge::merge_layers;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::data_migrations::{DataMigrationError, DataMigrations};
use crate::registry::{RegistryStore, RegistryStoreError};

/// The switch: the capability flag a store's document carries it under.
pub const SWITCH: &str = Capability::QrOrdering.meta().key;

/// The name the switch's one-time setting is recorded under in `data_migrations`.
pub const MIGRATION: &str = "qr_ordering_one_switch";

/// The node whose `enabled` field follows the switch.
const QR_NODE: &str = "qr";
/// The field of the `qr` node the cloud's guest intake read before the switch.
const QR_ENABLED: &str = "enabled";
/// The node whose QR channel follows the switch.
const CHANNELS_NODE: &str = "channels";
/// The list a `channels` node carries.
const CHANNELS_ENABLED: &str = "enabled";

/// The QR channel's wire token, as a `channels` list names it.
fn qr_channel() -> &'static str {
    SalesChannel::Qr.as_wire()
}

/// Whether QR ordering is on in a store's document: its switch, or the switch's declared default
/// (off) when the document does not set it. Read through the one capability reader, as the edge
/// reads it.
#[must_use]
pub fn switch_of(document: &Value) -> bool {
    CapabilityContext::from_flags(|key| document.get(key).and_then(Value::as_bool))
        .enabled(Capability::QrOrdering)
}

/// Whether `qr.enabled` reads as on, the way the cloud's guest intake read it before the switch: on
/// unless it is `false`, so an absent or unreadable value is on.
fn qr_enabled_reads_on(document: &Value) -> bool {
    document
        .get(QR_NODE)
        .and_then(|qr| qr.get(QR_ENABLED))
        .and_then(Value::as_bool)
        != Some(false)
}

/// A `channels` node's list as the edge reads it, every token as written: `None` for a node the
/// edge cannot read, which restricts no channel.
fn listed(node: &Value) -> Option<Vec<String>> {
    // Through the JSON text, as the edge parses every node.
    let text = serde_json::to_string(node).ok()?;
    let published: PublishedChannels = serde_json::from_str(&text).ok()?;
    Some(
        published
            .enabled()
            .iter()
            .map(|channel| channel.as_wire().to_owned())
            .collect(),
    )
}

/// A document's published channel list, or `None` when it has none, or one the edge cannot read:
/// either restricts no channel.
fn channel_list(document: &Value) -> Option<Vec<String>> {
    document.get(CHANNELS_NODE).and_then(listed)
}

/// Whether a store took a guest's QR order before the switch, as the code decided it then: the
/// cloud's guest intake unless `qr.enabled` was `false`, and the edge unless a published `channels`
/// list left the QR channel out. An absent or unreadable list restricted nothing.
#[must_use]
pub fn taken_before_the_switch(document: &Value) -> bool {
    let intake = qr_enabled_reads_on(document);
    let edge =
        channel_list(document).is_none_or(|list| list.iter().any(|token| token == qr_channel()));
    intake && edge
}

/// Whether a document's switch, `qr.enabled` and channel list agree: `qr.enabled` reads as the
/// switch does, and a published list holds the QR channel as [`compose_channels`] leaves it for the
/// switch.
///
/// A version from before the switch that agrees ran as it runs under the switch: the guest intake
/// read `qr.enabled`, which says what the switch says, and the edge reads the same list. That
/// covers a dead flag left on beside an empty list, where the guest intake took the order and the
/// edge refused it, as both do now. Every write of the switch, the `qr` node or the list leaves a
/// version that agrees ([`align`]).
#[must_use]
pub fn aligned(document: &Value) -> bool {
    let on = switch_of(document);
    qr_enabled_reads_on(document) == on
        && channel_list(document).is_none_or(|list| compose_channels(&list, on) == list)
}

/// The switch a document runs under: its own where it agrees with `qr.enabled` and the channel
/// list ([`aligned`]), and otherwise what the store did before the switch.
fn settled_switch(document: &Value) -> bool {
    if aligned(document) {
        switch_of(document)
    } else {
        taken_before_the_switch(document)
    }
}

/// A published channel list with its QR channel following the switch at `on`, without changing
/// what the list means: an empty list stays empty, because it is a store that takes no channel,
/// and a list holding only the QR channel keeps it when the switch is off, because taking it out
/// would empty the list and refuse every order. Every other token is kept, in its order.
#[must_use]
pub fn compose_channels(list: &[String], on: bool) -> Vec<String> {
    let qr = qr_channel();
    let has_qr = list.iter().any(|token| token == qr);
    if list.is_empty() || on == has_qr {
        return list.to_vec();
    }
    if on {
        let mut composed = list.to_vec();
        composed.push(qr.to_owned());
        return composed;
    }
    let rest: Vec<String> = list.iter().filter(|token| *token != qr).cloned().collect();
    if rest.is_empty() { list.to_vec() } else { rest }
}

/// A channel list as the node carries it.
fn list_value(list: Vec<String>) -> Value {
    Value::Array(list.into_iter().map(Value::String).collect())
}

/// The object a node holds on the Store layer, or an empty one: the base a following write of that
/// node keeps the other fields of.
fn store_layer_object(before: Option<&ConfigTreeState>, node: &str) -> Map<String, Value> {
    before
        .and_then(|state| state.layer(ConfigLevel::Store).get(node))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Every layer of a tree merged: what the next version is composed from.
fn effective_of(state: &ConfigTreeState) -> Value {
    let [tenant, brand, store, device] = &state.layers;
    merge_layers(&[tenant, brand, store, device])
}

/// The document of the version a store runs: what its edge holds, and what the guest intake reads.
/// `None` before the first publish, when nothing has reached the edge.
fn running(state: &ConfigTreeState) -> Option<&Value> {
    state.history.last().map(|version| &version.effective)
}

/// The nodes a write to a store's Store layer sets once the switch, `qr.enabled` and the QR
/// channel agree.
///
/// `nodes` is what the write sets; `before` is the store's tree as the write read it, `None` for a
/// store never published to. A write that touches none of the three comes back as it went in.
/// Otherwise:
///
/// - the switch is the one the write sets, or, for a `qr` write that names `enabled`, that value,
///   which is then written as the switch too; failing both, it is the store's own;
/// - a `qr` node the write sets is given `enabled` equal to the switch, and a store whose
///   `qr.enabled` reads otherwise gets its Store-layer `qr` node with `enabled` set, and its other
///   fields kept;
/// - a `channels` list the write sets, or else the store's published list, has its QR channel
///   composed by [`compose_channels`], and a store-held list that changes is written back with the
///   node's other fields kept.
#[must_use]
pub fn align(
    before: Option<&ConfigTreeState>,
    mut nodes: Vec<(String, Value)>,
) -> Vec<(String, Value)> {
    let sets = |key: &str| nodes.iter().any(|(node, _)| node == key);
    let (sets_switch, sets_qr, sets_channels) = (sets(SWITCH), sets(QR_NODE), sets(CHANNELS_NODE));
    if !(sets_switch || sets_qr || sets_channels) {
        return nodes;
    }
    let document = before.map_or_else(|| Value::Object(Map::new()), effective_of);
    let value_of = |nodes: &[(String, Value)], key: &str| {
        nodes
            .iter()
            .find(|(node, _)| node == key)
            .map(|(_, value)| value.clone())
    };
    let written_switch = value_of(&nodes, SWITCH).and_then(|value| value.as_bool());
    let named_by_qr =
        value_of(&nodes, QR_NODE).and_then(|qr| qr.get(QR_ENABLED).and_then(Value::as_bool));
    let on = written_switch
        .or(named_by_qr)
        .unwrap_or_else(|| switch_of(&document));
    if written_switch.is_none() && named_by_qr.is_some() {
        nodes.push((SWITCH.to_owned(), Value::Bool(on)));
    }

    // `qr.enabled`.
    if sets_qr {
        for (node, value) in &mut nodes {
            if node == QR_NODE
                && let Value::Object(qr) = value
            {
                qr.insert(QR_ENABLED.to_owned(), Value::Bool(on));
            }
        }
    } else if qr_enabled_reads_on(&document) != on {
        let mut qr = store_layer_object(before, QR_NODE);
        qr.insert(QR_ENABLED.to_owned(), Value::Bool(on));
        nodes.push((QR_NODE.to_owned(), Value::Object(qr)));
    }

    // The QR channel.
    if sets_channels {
        for (node, value) in &mut nodes {
            if node != CHANNELS_NODE {
                continue;
            }
            if let Some(list) = listed(value)
                && let Value::Object(channels) = value
            {
                channels.insert(
                    CHANNELS_ENABLED.to_owned(),
                    list_value(compose_channels(&list, on)),
                );
            }
        }
    } else if let Some(list) = channel_list(&document) {
        let composed = compose_channels(&list, on);
        if composed != list {
            let mut channels = store_layer_object(before, CHANNELS_NODE);
            channels.insert(CHANNELS_ENABLED.to_owned(), list_value(composed));
            nodes.push((CHANNELS_NODE.to_owned(), Value::Object(channels)));
        }
    }
    nodes
}

/// Settles a whole document from before the switch ([`predates`]) — a version a rollback restores —
/// whose switch, `qr.enabled` and channel list do not agree ([`aligned`]). Its switch is set to what
/// the store then did ([`taken_before_the_switch`]), and the two follow it. A document that agrees
/// ran as it would under the switch, and is left as it is.
///
/// Only for a version from before the switch: one published since says what it says, and a store
/// that never set its switch and never published `qr.enabled` reads, to this, as one taking QR
/// orders.
pub fn settle(document: &mut Value) {
    if aligned(document) {
        return;
    }
    let on = taken_before_the_switch(document);
    let reads_on = qr_enabled_reads_on(document);
    let list = channel_list(document);
    let Value::Object(map) = document else {
        return;
    };
    map.insert(SWITCH.to_owned(), Value::Bool(on));
    if reads_on != on {
        let qr = map
            .entry(QR_NODE.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !qr.is_object() {
            *qr = Value::Object(Map::new());
        }
        if let Value::Object(qr) = qr {
            qr.insert(QR_ENABLED.to_owned(), Value::Bool(on));
        }
    }
    if let Some(list) = list {
        let composed = compose_channels(&list, on);
        if composed != list
            && let Some(Value::Object(channels)) = map.get_mut(CHANNELS_NODE)
        {
            channels.insert(CHANNELS_ENABLED.to_owned(), list_value(composed));
        }
    }
}

/// What a run of the switch's one-time setting did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Stores whose switch was published, with what follows it.
    pub published: usize,
    /// Stores whose switch, `qr.enabled` and channel list already agreed ([`aligned`]).
    pub unchanged: usize,
    /// Stores with no published configuration. Nothing has reached their edge, and their first
    /// publish starts them with the switch off, as a new store starts.
    pub unconfigured: usize,
    /// Stores whose configuration could not be read or whose publish was refused, by tenant and
    /// store, each logged as it failed. Their switch is not set: an operator sets it in the
    /// console.
    pub failed: Vec<(TenantId, StoreId)>,
}

/// Why the one-time setting could not run.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// The registry, which lists the stores, could not be read.
    #[error(transparent)]
    Registry(#[from] RegistryStoreError),
    /// The record of data changes could not be read or written.
    #[error(transparent)]
    Record(#[from] DataMigrationError),
}

/// When QR ordering got one switch, and what this boot did about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OneSwitch {
    /// When the switch's one-time setting began: a configuration version published before then is
    /// from before the switch, and a rollback to it is settled ([`settle`], [`predates`]).
    pub since: Timestamp,
    /// What this run did, or `None` when an earlier boot had made the change.
    pub report: Option<MigrationReport>,
}

/// Whether a configuration version was published before QR ordering had one switch, by the time
/// its id was minted at. `None` is a cloud with no record of the change, as one built without its
/// database is, which treats every version as published since.
#[must_use]
pub fn predates(version_id: ConfigVersionId, since: Option<Timestamp>) -> bool {
    since.is_some_and(|since| {
        i64::try_from(version_id.as_ulid().timestamp_ms())
            .is_ok_and(|minted| minted < since.as_milliseconds_since_epoch())
    })
}

/// Sets every store's switch from what the store did before the switch, once.
///
/// Returns when the change was made, with no report when [`MIGRATION`] is already recorded.
/// Otherwise every store of every tenant, archived ones included, is looked at, and a store whose
/// switch, `qr.enabled` and channel list do not agree ([`aligned`]) is published a new
/// configuration version: the switch it ran ([`taken_before_the_switch`]), and what [`align`] adds
/// to make the other two follow it. A store that agrees is left alone, so a second run publishes
/// nothing.
///
/// The run is recorded once every store has been looked at, including when some failed. Run again,
/// it would read a store published to since as one from before the switch: a new store sets
/// neither its switch nor `qr.enabled`, which before the switch was a store taking QR orders. So it
/// runs once, at boot, before the cloud serves, and a store that failed is left to an operator,
/// whom the log names it to.
///
/// # Errors
///
/// [`MigrationError`] if the registry or the record of data changes cannot be read, or the record
/// cannot be written. The registry is read whole before any store is published to. A store whose
/// configuration cannot be read or published is reported in [`MigrationReport::failed`] instead,
/// so one bad store does not stop the others.
pub async fn run_once<R, Cfg, M, C>(
    registry: &R,
    config_trees: &Cfg,
    record: &M,
    clock: &C,
) -> Result<OneSwitch, MigrationError>
where
    R: RegistryStore,
    Cfg: ConfigTreeStore,
    M: DataMigrations,
    C: ClockSource,
{
    if let Some(since) = record.recorded_time(MIGRATION).await? {
        return Ok(OneSwitch {
            since,
            report: None,
        });
    }
    let since = clock.now();
    let mut stores: Vec<(TenantId, StoreId)> = Vec::new();
    for tenant in registry.list_tenants().await? {
        let tenant_id = tenant.record.tenant_id;
        for store in registry.list_stores(tenant_id).await? {
            stores.push((tenant_id, store.record.store_id));
        }
    }
    let mut report = MigrationReport::default();
    for (tenant_id, store_id) in stores {
        match settle_store(config_trees, clock, tenant_id, store_id).await {
            Ok(StoreOutcome::Published) => report.published += 1,
            Ok(StoreOutcome::Unchanged) => report.unchanged += 1,
            Ok(StoreOutcome::Unconfigured) => report.unconfigured += 1,
            Err(()) => report.failed.push((tenant_id, store_id)),
        }
    }
    record.record(MIGRATION, since).await?;
    Ok(OneSwitch {
        since,
        report: Some(report),
    })
}

/// What setting one store's switch did.
enum StoreOutcome {
    Published,
    Unchanged,
    Unconfigured,
}

/// Sets one store's switch from what it did, unless its three agree already. A failure is logged
/// here, by id, and reported as `Err(())` for the run to count.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is the axum Response every node publish's closure \
              carries, and this closure never takes the Err branch"
)]
async fn settle_store<Cfg, C>(
    config_trees: &Cfg,
    clock: &C,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<StoreOutcome, ()>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let loaded = config_trees
        .load(tenant_id, store_id)
        .await
        .map_err(|error| {
            tracing::error!(
                %tenant_id,
                %store_id,
                %error,
                "could not read a store's configuration to set its QR ordering switch; set the \
                 switch in Channels & payments"
            );
        })?;
    let Some(document) = loaded.as_ref().and_then(|loaded| running(&loaded.record)) else {
        return Ok(StoreOutcome::Unconfigured);
    };
    if aligned(document) {
        return Ok(StoreOutcome::Unchanged);
    }
    // Read again inside the publish's retry, from the version the store runs at that moment.
    crate::http::publish_config_nodes_with(
        config_trees,
        clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        |state| {
            let on = state.and_then(running).is_some_and(settled_switch);
            Ok(vec![(SWITCH.to_owned(), Value::Bool(on))])
        },
    )
    .await
    .map(|_version| StoreOutcome::Published)
    .map_err(|refusal| {
        tracing::error!(
            %tenant_id,
            %store_id,
            status = %refusal.status(),
            "could not publish a store's QR ordering switch; set the switch in Channels & payments"
        );
    })
}

#[cfg(test)]
mod tests {
    use super::{
        SWITCH, align, aligned, compose_channels, predates, settle, switch_of,
        taken_before_the_switch,
    };
    use crate::config_tree::ConfigTreeState;
    use pos_proto::ids::ConfigVersionId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;
    use serde_json::{Value, json};

    fn tokens(list: &[&str]) -> Vec<String> {
        list.iter().map(|token| (*token).to_owned()).collect()
    }

    /// A tree whose Store layer is `store` and whose other layers are empty.
    fn tree(store: Value) -> ConfigTreeState {
        ConfigTreeState {
            layers: [json!({}), json!({}), store, json!({})],
            history: Vec::new(),
            k: 8,
        }
    }

    fn node<'a>(nodes: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
        nodes
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn the_switch_is_off_until_a_document_sets_it() {
        assert!(!switch_of(&json!({})));
        assert!(!switch_of(&json!({ "qr_ordering_enabled": false })));
        assert!(switch_of(&json!({ "qr_ordering_enabled": true })));
    }

    #[test]
    fn what_a_store_did_is_the_guest_intake_and_the_edge_together() {
        // Nothing published: the intake read `qr.enabled` as on, and no list restricted the edge.
        assert!(taken_before_the_switch(&json!({})));
        assert!(taken_before_the_switch(
            &json!({ "qr": { "enabled": true } })
        ));
        // The intake refused it.
        assert!(!taken_before_the_switch(
            &json!({ "qr": { "enabled": false } })
        ));
        // The edge refused it: a published list without the QR channel, or an empty one.
        assert!(!taken_before_the_switch(
            &json!({ "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] } })
        ));
        assert!(!taken_before_the_switch(
            &json!({ "channels": { "enabled": [] } })
        ));
        assert!(taken_before_the_switch(
            &json!({ "channels": { "enabled": ["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"] } })
        ));
        // A list the edge cannot read restricted nothing, and neither did an unreadable `enabled`.
        assert!(taken_before_the_switch(
            &json!({ "channels": { "enabled": [1] } })
        ));
        assert!(taken_before_the_switch(
            &json!({ "qr": { "enabled": "no" } })
        ));
        // The dead flag decided nothing.
        assert!(taken_before_the_switch(
            &json!({ "qr_ordering_enabled": false })
        ));
    }

    #[test]
    fn a_document_agrees_when_qr_enabled_reads_as_the_switch_and_the_list_holds_qr_as_it_should() {
        // Off, said three ways that agree.
        assert!(aligned(&json!({ "qr": { "enabled": false } })));
        assert!(aligned(&json!({
            "qr_ordering_enabled": false,
            "qr": { "enabled": false },
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
        })));
        // On, with no list, or with QR in it.
        assert!(aligned(&json!({ "qr_ordering_enabled": true })));
        assert!(aligned(&json!({
            "qr_ordering_enabled": true,
            "channels": { "enabled": ["SALES_CHANNEL_QR"] },
        })));
        // An empty list stays empty whatever the switch says, so it agrees with either.
        assert!(aligned(
            &json!({ "qr_ordering_enabled": true, "channels": { "enabled": [] } })
        ));

        // The switch unset reads off, while `qr.enabled` unset reads on.
        assert!(!aligned(&json!({})));
        // The QR channel missing from the list of a store that has the switch on.
        assert!(!aligned(&json!({
            "qr_ordering_enabled": true,
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
        })));
        // Still listed beside another channel while the switch is off.
        assert!(!aligned(&json!({
            "qr": { "enabled": false },
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"] },
        })));
    }

    #[test]
    fn a_version_predates_the_switch_by_the_instant_its_id_was_minted_at() {
        let at = |ms: u64| ConfigVersionId::new(Ulid::from_parts(ms, 1));
        let since = Timestamp::from_milliseconds_since_epoch(10_000).ok();
        assert!(predates(at(9_999), since));
        assert!(!predates(at(10_000), since), "minted as the change began");
        assert!(!predates(at(10_001), since));
        assert!(
            !predates(at(1), None),
            "a cloud with no record of the change restores every version as it is"
        );
    }

    #[test]
    fn the_qr_channel_follows_the_switch_without_changing_what_a_list_means() {
        let dine_in = tokens(&["SALES_CHANNEL_DINE_IN"]);
        let with_qr = tokens(&["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"]);
        assert_eq!(compose_channels(&dine_in, true), with_qr);
        assert_eq!(compose_channels(&with_qr, false), dine_in);
        assert_eq!(compose_channels(&with_qr, true), with_qr);
        assert_eq!(compose_channels(&dine_in, false), dine_in);
        // An empty list takes no channel, and stays so.
        assert_eq!(compose_channels(&[], true), Vec::<String>::new());
        // A list of the QR channel alone keeps it: an empty list would refuse every order.
        let only_qr = tokens(&["SALES_CHANNEL_QR"]);
        assert_eq!(compose_channels(&only_qr, false), only_qr);
        // A token from a newer cloud is kept, in its place.
        let newer = tokens(&["SALES_CHANNEL_KIOSK", "SALES_CHANNEL_QR"]);
        assert_eq!(
            compose_channels(&newer, false),
            tokens(&["SALES_CHANNEL_KIOSK"])
        );
    }

    #[test]
    fn a_write_that_touches_none_of_the_three_is_left_as_it_is() {
        let before = tree(json!({ "qr": { "enabled": false } }));
        let nodes = vec![("menu".to_owned(), json!({ "channels": [] }))];
        assert_eq!(align(Some(&before), nodes.clone()), nodes);
    }

    #[test]
    fn turning_the_switch_off_turns_qr_enabled_off_and_takes_the_channel_out() {
        let before = tree(json!({
            "qr": { "staff_confirmation_required": false },
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"] },
        }));
        let nodes = align(Some(&before), vec![(SWITCH.to_owned(), json!(false))]);
        assert_eq!(node(&nodes, SWITCH), Some(&json!(false)));
        assert_eq!(
            node(&nodes, "qr"),
            Some(&json!({ "staff_confirmation_required": false, "enabled": false })),
            "the guardrails are kept"
        );
        assert_eq!(
            node(&nodes, "channels"),
            Some(&json!({ "enabled": ["SALES_CHANNEL_DINE_IN"] }))
        );
    }

    #[test]
    fn turning_it_on_adds_the_channel_and_leaves_an_unset_qr_enabled_alone() {
        let before = tree(json!({ "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] } }));
        let nodes = align(Some(&before), vec![(SWITCH.to_owned(), json!(true))]);
        assert_eq!(
            node(&nodes, "qr"),
            None,
            "an absent `qr.enabled` already reads as on"
        );
        assert_eq!(
            node(&nodes, "channels"),
            Some(&json!({ "enabled": ["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"] }))
        );
        // A store with no list takes every channel already, and is given none.
        let nodes = align(
            Some(&tree(json!({}))),
            vec![(SWITCH.to_owned(), json!(true))],
        );
        assert_eq!(nodes, vec![(SWITCH.to_owned(), json!(true))]);
    }

    #[test]
    fn a_qr_write_naming_enabled_writes_the_switch_and_one_that_does_not_takes_it() {
        let before = tree(json!({ "qr_ordering_enabled": true }));
        let named = align(
            Some(&before),
            vec![(
                "qr".to_owned(),
                json!({ "enabled": false, "per_table_limit": 5 }),
            )],
        );
        assert_eq!(node(&named, SWITCH), Some(&json!(false)));
        assert_eq!(
            node(&named, "qr"),
            Some(&json!({ "enabled": false, "per_table_limit": 5 }))
        );

        let unnamed = align(
            Some(&before),
            vec![("qr".to_owned(), json!({ "per_table_limit": 5 }))],
        );
        assert_eq!(node(&unnamed, SWITCH), None, "the switch is not written");
        assert_eq!(
            node(&unnamed, "qr"),
            Some(&json!({ "per_table_limit": 5, "enabled": true }))
        );
    }

    #[test]
    fn a_channel_list_written_follows_the_switch_the_store_has() {
        let before = tree(json!({ "qr_ordering_enabled": false }));
        let nodes = align(
            Some(&before),
            vec![(
                "channels".to_owned(),
                json!({ "enabled": ["SALES_CHANNEL_QR", "SALES_CHANNEL_TAKEAWAY"] }),
            )],
        );
        assert_eq!(
            node(&nodes, "channels"),
            Some(&json!({ "enabled": ["SALES_CHANNEL_TAKEAWAY"] }))
        );
        assert_eq!(node(&nodes, SWITCH), None);
        // `qr.enabled` reads as on while the switch is off, so it is set to follow.
        assert_eq!(node(&nodes, "qr"), Some(&json!({ "enabled": false })));
    }

    #[test]
    fn a_restored_version_from_before_the_switch_gets_the_switch_it_ran() {
        // The capability form wrote `false` into the flag while nothing read it; the store took
        // QR orders, by `qr.enabled` and an unrestricted channel list.
        let mut document = json!({ "qr_ordering_enabled": false, "qr": { "per_table_limit": 3 } });
        settle(&mut document);
        assert_eq!(
            document,
            json!({ "qr_ordering_enabled": true, "qr": { "per_table_limit": 3 } })
        );

        // A store whose guest page was off, and whose list still named QR beside dine-in.
        let mut document = json!({
            "qr": { "enabled": false },
            "channels": { "enabled": ["SALES_CHANNEL_QR", "SALES_CHANNEL_DINE_IN"] },
        });
        settle(&mut document);
        assert_eq!(
            document,
            json!({
                "qr_ordering_enabled": false,
                "qr": { "enabled": false },
                "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
            })
        );

        // A dead flag left on beside an empty list agrees: the intake took the order and the edge
        // refused it, as both do under the switch. It is restored as it was.
        let refused_by_the_edge = json!({
            "qr_ordering_enabled": true,
            "channels": { "enabled": [] },
        });
        let mut document = refused_by_the_edge.clone();
        settle(&mut document);
        assert_eq!(document, refused_by_the_edge);

        // A version published since the switch already agrees, and is restored as it was.
        let agreed = json!({
            "qr_ordering_enabled": false,
            "qr": { "enabled": false },
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
        });
        let mut document = agreed.clone();
        settle(&mut document);
        assert_eq!(document, agreed);
    }
}
