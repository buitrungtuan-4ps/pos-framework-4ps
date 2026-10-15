// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till a device is, and opening its drawer: directly, or as a kick through its print agent
//! that waits for the agent's acknowledgement.

use pos_ports::PortError;
use pos_ports::printer::{PrintDocument, PrintJob};
use pos_proto::ids::{DeviceId, EventId, StoreId};
use tokio::sync::oneshot;

use crate::app::EdgeSession;
use crate::print_agent::KICK_WAIT;
use crate::printing::agents::AwaitedKick;
use crate::printing::devices::{
    TillPrinting, a_till_prints_its_own, drawer_printer, published_terminals,
};
use crate::printing::transports::assumed_capabilities;
use crate::printing::{DrawerOutcome, KICKING_AGENTS, KICKS_AWAITED, PrintOutcome, Printers};

impl Printers {
    /// The paper choices of the till `device` is
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 4): what the `TERMINAL` entry it is bound to names, through the print-agent binding
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
    ///
    /// [`TillPrinting::STORE`] when no terminal in the store's `devices` node names anything of its
    /// own, without reading the binding at all, so a store that names nothing prints as before
    /// whatever the binding record holds. The same when `device` is bound to no terminal, when its
    /// terminal is not in the node, and in a composition with no binding record. A binding that
    /// cannot be read while a terminal does name its own is a till nobody can tell apart, whose
    /// paper is refused: printed at the store's receipt printer, the bar's bill would come out at
    /// the counter.
    pub async fn till_for(&self, session: &EdgeSession, device: DeviceId) -> TillPrinting {
        if !a_till_prints_its_own(&session.devices) {
            return TillPrinting::STORE;
        }
        match self.terminal_of(device).await {
            Ok(terminal) => terminal
                .and_then(|terminal| {
                    published_terminals(&session.devices).find(|entry| entry.device_id == terminal)
                })
                .map_or(TillPrinting::STORE, TillPrinting::of),
            Err(error) => {
                tracing::warn!(%device, %error, "the terminal a device is bound to could not be read");
                TillPrinting::UNKNOWN
            }
        }
    }

    /// The terminal `device` is bound to through the print-agent binding
    /// ([ADR-0112](../../../docs/adr/0112-print-agents.md)), which is the till it is: `None` for one
    /// bound to none, and in a composition with no binding record.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the binding cannot be read.
    pub async fn terminal_of(&self, device: DeviceId) -> Result<Option<DeviceId>, PortError> {
        match self.agents.as_ref() {
            Some(lane) => lane.terminal_of(device).await,
            None => Ok(None),
        }
    }

    /// Opens a cash drawer through the printer [`drawer_printer`] picks for it: the store's one
    /// drawer where `till` is `None`, and that till's otherwise
    /// ([ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 4,
    /// [ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6).
    ///
    /// The caller asks only after the act the drawer is for has committed: a cash payment, a paid in
    /// or out, a no-sale opening. Never an error, for the reason a receipt is never one: the money is
    /// already recorded, and a drawer that did not spring is opened with its key. The outcome says
    /// which, and the till tells the cashier.
    ///
    /// Behind a print agent that carries the kick, the kick is a job on its queue under `kick_id`,
    /// and the till waits up to [`KICK_WAIT`] for the agent to say it wrote it
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 6). A fresh
    /// id, as a test page's is: a cash payment's receipt already prints under the payment's.
    pub async fn open_drawer(
        &self,
        session: &EdgeSession,
        store_id: StoreId,
        kick_id: EventId,
        till: Option<DeviceId>,
    ) -> DrawerOutcome {
        let Some(device) = drawer_printer(&session.devices, till, |agent| self.carries_kick(agent))
        else {
            if let Some(till) = till {
                tracing::info!(
                    %till,
                    "this till names no receipt printer with a cash drawer this edge may open"
                );
            } else if session
                .devices
                .devices()
                .iter()
                .any(|device| device.drawer_attached)
            {
                tracing::warn!(
                    "a printer is marked as having a cash drawer, but not one this edge may open: it \
                     must serve the bill, be on USB, and print without an agent or through one that \
                     carries the kick"
                );
            }
            return DrawerOutcome::NoDrawer;
        };
        if let Some(agent) = device.agent_device_id {
            return self
                .kick_through(agent, device.device_id, store_id, kick_id)
                .await;
        }
        let capabilities = assumed_capabilities(device, self.can_rasterise());
        let printer = match self.printer_for(device, capabilities) {
            Ok(printer) => printer,
            Err(error) => {
                tracing::warn!(device = %device.device_id, %error, "no channel to the drawer's printer");
                return DrawerOutcome::Unavailable;
            }
        };
        // On a blocking thread for the reason a receipt is: an unplugged printer blocks on its write.
        match tokio::task::spawn_blocking(move || printer.open_drawer_blocking()).await {
            Ok(Ok(())) => {
                tracing::info!(device = %device.device_id, "cash drawer kicked");
                DrawerOutcome::Opened
            }
            Ok(Err(error)) => {
                tracing::warn!(device = %device.device_id, %error, "the drawer's printer refused the kick");
                DrawerOutcome::Unavailable
            }
            Err(_) => {
                tracing::warn!(device = %device.device_id, "the drawer thread did not finish");
                DrawerOutcome::Unavailable
            }
        }
    }

    /// Opens the drawer through the print agent that owns its printer: a kick on the agent's
    /// queue, answered `Opened` once the agent acknowledges it within [`KICK_WAIT`].
    async fn kick_through(
        &self,
        agent: DeviceId,
        printer: DeviceId,
        store_id: StoreId,
        job_id: EventId,
    ) -> DrawerOutcome {
        let Some(lane) = self.agents.as_ref() else {
            tracing::warn!(%printer, %agent, "the drawer's printer names an agent but no print queue is composed");
            return DrawerOutcome::Unavailable;
        };
        // Waited on before it is offered, so an acknowledgement that beats the offer is heard.
        let Some(acknowledged) = self.await_kick(job_id) else {
            tracing::warn!(%printer, %agent, "too many kicks are waiting on print agents; this one is not sent");
            return DrawerOutcome::Unavailable;
        };
        let kick = PrintJob {
            job_id,
            store_id,
            station_id: None,
            document: PrintDocument::kick(),
        };
        if lane.kick(agent, printer, kick).await != PrintOutcome::QueuedToAgent {
            return DrawerOutcome::Unavailable;
        }
        if acknowledged.within(KICK_WAIT).await {
            tracing::info!(%job_id, %printer, %agent, "cash drawer kicked through its print agent");
            DrawerOutcome::Opened
        } else {
            tracing::warn!(%job_id, %printer, %agent, "the print agent did not acknowledge the kick in time");
            DrawerOutcome::Unavailable
        }
    }

    /// Records what a print agent's claim said about it: whether it carries a kick. The claim
    /// route calls this once the binding has said which terminal the caller is.
    pub(crate) fn heard_from_agent(&self, agent: DeviceId, carries_kick: bool) {
        let mut kicking = self
            .kicking
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !carries_kick {
            kicking.remove(&agent);
            return;
        }
        if !kicking.contains(&agent)
            && kicking.len() >= KICKING_AGENTS
            && let Some(forgotten) = kicking.iter().next().copied()
        {
            kicking.remove(&forgotten);
        }
        kicking.insert(agent);
    }

    /// Whether `agent`'s last claim said it carries a kick.
    pub(super) fn carries_kick(&self, agent: DeviceId) -> bool {
        self.kicking
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&agent)
    }

    /// An agent acknowledged `job`: a till waiting on it as a kick hears so. The acknowledgement
    /// route calls this for a job it deleted from the queue.
    pub(crate) fn acknowledged(&self, job: EventId) {
        let waiting = self
            .awaited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&job);
        if let Some(waiting) = waiting {
            // The till may have stopped waiting a moment ago, and nothing else is listening.
            let _ = waiting.send(());
        }
    }

    /// Starts waiting on `job`'s acknowledgement, or `None` when [`KICKS_AWAITED`] are already.
    pub(super) fn await_kick(&self, job: EventId) -> Option<AwaitedKick<'_>> {
        let mut awaited = self
            .awaited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if awaited.len() >= KICKS_AWAITED {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        awaited.insert(job, sender);
        Some(AwaitedKick {
            awaited: &self.awaited,
            job,
            receiver,
        })
    }
}
