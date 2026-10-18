// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! ADR-0112: a printer whose transport belongs to another device, and the drawer kick an agent
//! carries.

use std::sync::Arc;

use pos_ports::printer::PrintJob;
use pos_proto::ClockSource;
use pos_proto::devices::{DeviceKind, PublishedDevice, PublishedDevices};
use pos_proto::ids::DeviceId;
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::ulid::Ulid;

use crate::printing::tests::{
    Recorder, STORE, device, device_via_agent, drawer_device, event_id, lines_of, recorder,
    session_with, store_id, totals_of,
};
use crate::printing::{
    AgentLane, DrawerOutcome, KICKING_AGENTS, KICKS_AWAITED, PrintOutcome, Printers,
};

// ---------------------------------------------------------------------
// ADR-0112: a printer whose transport belongs to another device.
// ---------------------------------------------------------------------

/// The three seams the agent lane needs, in memory, plus the agent's own id.
struct Lane {
    queue: Arc<crate::print_queue::InMemoryPrintQueue>,
    agent: DeviceId,
}

/// A dispatcher whose printers are reached through an agent bound `heard_from_ms` ago, or that
/// nobody holds at all when `heard_from_ms` is `None`.
async fn with_agent_lane(heard_from_ms: Option<i64>) -> (Printers, Arc<Recorder>, Lane) {
    use crate::print_agent::PrintAgents as _;

    let (printers, recorder) = recorder(true);
    let agents = Arc::new(crate::print_agent::InMemoryPrintAgents::new());
    let queue = Arc::new(crate::print_queue::InMemoryPrintQueue::new());
    let agent = DeviceId::new(Ulid::from_u128(0xA6E7));
    if let Some(ago) = heard_from_ms {
        let now = crate::clock::SystemClock
            .now()
            .as_milliseconds_since_epoch();
        agents
            .claim(
                agent,
                DeviceId::new(Ulid::from_u128(0x00DE_71CE)),
                now - ago,
            )
            .await
            .expect("the binding is recorded");
    }
    let printers = printers.with_agents(Arc::new(AgentLane::new(
        Arc::clone(&agents),
        Arc::clone(&queue),
        Arc::new(crate::print_wake::SharedPrintWake::new()),
    )));
    (printers, recorder, Lane { queue, agent })
}

/// What the agent would be handed if it asked right now.
async fn claimable(lane: &Lane) -> Vec<crate::print_queue::ClaimedJob> {
    use crate::print_queue::PrintQueue as _;
    let now = crate::clock::SystemClock
        .now()
        .as_milliseconds_since_epoch();
    lane.queue
        .claim(lane.agent, now, now + 30_000)
        .await
        .expect("the queue answers")
}

/// A drawer printer behind `agent`: marked, on USB, serving the bill.
fn drawer_via_agent(agent: DeviceId) -> PublishedDevice {
    PublishedDevice {
        agent_device_id: Some(agent),
        ..drawer_device(2)
    }
}

/// An agent that takes the first kick it finds on its queue within [`KICK_WAIT`], writes it
/// and acknowledges it, as `pos_print_agent` does through the routes, and hands it back.
async fn an_agent_kicks(
    printers: &Printers,
    lane: &Lane,
) -> Option<crate::print_queue::ClaimedJob> {
    use crate::print_queue::PrintQueue as _;
    let found = tokio::time::timeout(super::KICK_WAIT, async {
        loop {
            if let Some(kick) = claimable(lane)
                .await
                .into_iter()
                .find(|claimed| claimed.job.document.is_kick())
            {
                return kick;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .ok()?;
    let job = found.job.job_id;
    assert!(lane.queue.acknowledge(job, lane.agent).await.expect("ack"));
    printers.acknowledged(job);
    Some(found)
}

#[tokio::test]
async fn a_drawer_behind_an_agent_that_carries_the_kick_opens_once_it_acknowledges() {
    // ADR-0167 decision 6: the kick travels inside a job, to the agent that owns the printer,
    // and the till hears `OPENED` when the agent says it wrote it.
    let (printers, recorder, lane) = with_agent_lane(Some(0)).await;
    printers.heard_from_agent(lane.agent, true);
    let session = session_with(PublishedDevices::new(vec![drawer_via_agent(lane.agent)]));

    let (outcome, kick) = tokio::join!(
        printers.open_drawer(&session, store_id(), event_id(0xD0), None),
        an_agent_kicks(&printers, &lane)
    );
    assert_eq!(outcome, DrawerOutcome::Opened);
    let kick = kick.expect("the agent was sent a kick");
    assert_eq!(kick.printer, drawer_via_agent(lane.agent).device_id);
    assert_eq!(kick.job.job_id, event_id(0xD0), "under the id it was given");
    assert!(kick.job.document.is_kick());
    assert!(
        recorder.written.lock().expect("the recorder").is_empty(),
        "the edge never dials a printer an agent owns"
    );
    assert!(
        claimable(&lane).await.is_empty(),
        "one kick, and it is acknowledged"
    );
}

#[tokio::test]
async fn an_agent_that_does_not_carry_the_kick_is_sent_none_and_the_till_uses_the_key() {
    // An agent built before kicks claims without saying it carries one: ADR-0165's answer for a
    // printer the edge may not open a drawer through, and nothing on its queue.
    let (printers, _recorder, lane) = with_agent_lane(Some(0)).await;
    printers.heard_from_agent(lane.agent, false);
    let session = session_with(PublishedDevices::new(vec![drawer_via_agent(lane.agent)]));
    assert_eq!(
        printers
            .open_drawer(&session, store_id(), event_id(0xD0), None)
            .await,
        DrawerOutcome::NoDrawer
    );
    assert!(claimable(&lane).await.is_empty());

    // One that said so and then said otherwise, an agent rolled back, is the same.
    printers.heard_from_agent(lane.agent, true);
    printers.heard_from_agent(lane.agent, false);
    assert_eq!(
        printers
            .open_drawer(&session, store_id(), event_id(0xD0), None)
            .await,
        DrawerOutcome::NoDrawer
    );
    assert!(claimable(&lane).await.is_empty());
}

#[tokio::test]
async fn a_kicking_agent_that_is_silent_or_unbound_leaves_the_drawer_unavailable() {
    // The drawer printer was chosen and its agent cannot take the kick: queue nothing, and say
    // so at once rather than after a wait.
    let (printers, _recorder, lane) = with_agent_lane(None).await;
    printers.heard_from_agent(lane.agent, true);
    let session = session_with(PublishedDevices::new(vec![drawer_via_agent(lane.agent)]));
    assert_eq!(
        printers
            .open_drawer(&session, store_id(), event_id(0xD0), None)
            .await,
        DrawerOutcome::Unavailable
    );
    assert!(
        claimable(&lane).await.is_empty(),
        "nothing builds behind a box that is not there"
    );
}

#[tokio::test]
async fn a_kick_is_heard_only_while_the_till_waits_and_the_waiters_are_bounded() {
    let (printers, _recorder, _lane) = with_agent_lane(Some(0)).await;
    let first = event_id(0x0001);
    let waiting = printers.await_kick(first).expect("room to wait");
    printers.acknowledged(first);
    assert!(waiting.within(std::time::Duration::from_secs(1)).await);

    let unheard = printers.await_kick(event_id(0x0002)).expect("room to wait");
    assert!(!unheard.within(std::time::Duration::from_millis(10)).await);
    assert!(
        printers.awaited.lock().expect("lock").is_empty(),
        "a till that stopped waiting leaves nothing behind"
    );

    let held: Vec<_> = (0..KICKS_AWAITED)
        .map(|n| printers.await_kick(event_id(0x1000 + n as u128)))
        .collect();
    assert!(held.iter().all(Option::is_some));
    assert!(
        printers.await_kick(event_id(0x2000)).is_none(),
        "past the bound a kick is refused"
    );
}

#[test]
fn the_agents_said_to_carry_a_kick_are_bounded() {
    let (printers, _recorder) = recorder(true);
    for n in 0..=KICKING_AGENTS {
        printers.heard_from_agent(DeviceId::new(Ulid::from_u128(0x5000 + n as u128)), true);
    }
    let last = DeviceId::new(Ulid::from_u128(0x5000 + KICKING_AGENTS as u128));
    assert_eq!(printers.kicking.lock().expect("lock").len(), KICKING_AGENTS);
    assert!(
        printers.carries_kick(last),
        "the newest is remembered, and one older forgotten"
    );
}

#[tokio::test]
async fn a_printer_that_names_a_live_agent_is_queued_rather_than_dialled() {
    // The last hop moves and nothing above it does: the document is rendered here, and what
    // reaches the queue is the finished job.
    let (printers, recorder, lane) = with_agent_lane(Some(0)).await;
    let session = session_with(PublishedDevices::new(vec![device_via_agent(
        2, None, lane.agent,
    )]));
    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(7),
            41,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 90_000)),
            None,
        )
        .await;

    assert_eq!(outcome, PrintOutcome::QueuedToAgent);
    assert_eq!(outcome.as_wire(), "QUEUED_TO_AGENT");
    assert!(
        !outcome.printed(),
        "no paper has come out; the till must not claim it has"
    );
    assert!(
        recorder.written.lock().expect("lock").is_empty(),
        "the edge must not also dial a printer it handed to an agent"
    );

    let queued = claimable(&lane).await;
    assert_eq!(queued.len(), 1, "one job, for one printer");
    assert_eq!(queued[0].job.job_id, event_id(7));
    assert!(
        lines_of(&queued[0].job.document)
            .iter()
            .any(|line| line.contains("41")),
        "the agent receives the rendered receipt, not the ingredients for one"
    );
}

#[tokio::test]
async fn a_printer_with_no_agent_still_opens_its_own_transport() {
    // The absent field is the whole compatibility story: a lane being composed changes nothing
    // for a store that configured no agent.
    let (printers, recorder, _lane) = with_agent_lane(Some(0)).await;
    let session = session_with(PublishedDevices::new(vec![device(
        2,
        DeviceKind::Printer,
        None,
    )]));
    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(8),
            42,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 10_000)),
            None,
        )
        .await;
    assert_eq!(outcome, PrintOutcome::Printed);
    assert!(!recorder.written.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn an_unbound_or_silent_agent_is_refused_before_the_queue_is_touched() {
    // ADR-0112's ordering: a queue must not start building behind a box that is not there.
    for heard_from in [
        None,
        Some(i64::try_from(crate::print_agent::AGENT_SILENCE.as_millis()).expect("fits") + 1),
    ] {
        let (printers, recorder, lane) = with_agent_lane(heard_from).await;
        let session = session_with(PublishedDevices::new(vec![device_via_agent(
            2, None, lane.agent,
        )]));
        let outcome = printers
            .print_receipt(
                &session,
                &STORE,
                store_id(),
                event_id(9),
                43,
                &[],
                &totals_of(Money::new(CurrencyCode::VND, 10_000)),
                None,
            )
            .await;
        assert_eq!(outcome, PrintOutcome::AgentUnavailable);
        assert_eq!(outcome.as_wire(), "PRINT_AGENT_UNAVAILABLE");
        assert!(
            claimable(&lane).await.is_empty(),
            "nothing was written: the refusal comes before the table"
        );
        assert!(
            recorder.written.lock().expect("lock").is_empty(),
            "and the edge does not fall back to dialling the printer itself"
        );
    }
}

#[tokio::test]
async fn a_printer_whose_agent_is_alive_but_not_consuming_fills_and_then_refuses() {
    // The state the cap exists for: a *live* agent whose printer is not consuming. Adding one
    // more job is promising paper that is not coming, so the enqueue refuses and the operator
    // learns it at the till rather than from the absence of a ticket.
    use crate::print_queue::PrintQueue as _;

    let (printers, _recorder, lane) = with_agent_lane(Some(0)).await;
    let printer = device_via_agent(2, None, lane.agent);
    let now = crate::clock::SystemClock
        .now()
        .as_milliseconds_since_epoch();
    for filler in 0..crate::print_agent::MAX_QUEUED_PER_PRINTER {
        lane.queue
            .enqueue(
                lane.agent,
                printer.device_id,
                PrintJob {
                    job_id: event_id(u128::from(filler) + 1_000),
                    store_id: store_id(),
                    station_id: None,
                    document: pos_ports::printer::PrintDocument { blocks: Vec::new() },
                },
                now,
                now + 600_000,
                crate::print_agent::MAX_QUEUED_PER_PRINTER,
            )
            .await
            .expect("the queue fills");
    }

    let session = session_with(PublishedDevices::new(vec![printer]));
    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(10),
            44,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 10_000)),
            None,
        )
        .await;
    assert_eq!(outcome, PrintOutcome::QueueFull);
    assert_eq!(outcome.as_wire(), "PRINT_QUEUE_FULL");
}

#[tokio::test]
async fn a_named_agent_with_no_queue_composed_refuses_rather_than_dialling() {
    // A route test or the fakes-backed example. Opening the address anyway is the wrong
    // direction: in a hosted edge placement it is a device path that is not on this machine.
    let (printers, recorder) = recorder(true);
    let session = session_with(PublishedDevices::new(vec![device_via_agent(
        2,
        None,
        DeviceId::new(Ulid::from_u128(0xA6E7)),
    )]));
    let outcome = printers
        .print_receipt(
            &session,
            &STORE,
            store_id(),
            event_id(11),
            45,
            &[],
            &totals_of(Money::new(CurrencyCode::VND, 10_000)),
            None,
        )
        .await;
    assert_eq!(outcome, PrintOutcome::AgentUnavailable);
    assert!(recorder.written.lock().expect("lock").is_empty());
}
