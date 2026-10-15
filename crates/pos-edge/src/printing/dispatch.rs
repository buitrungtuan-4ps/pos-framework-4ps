// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Preparing a document for its printer and sending it, directly or through the agent, and the
//! printer each job goes to.

use std::sync::Arc;

use pos_ports::printer::{PrintBlock, PrintDocument, PrintJob, PrinterCapabilities};
use pos_ports::{PortError, PortName};
use pos_proto::devices::PublishedDevice;
use printer_escpos::EscPosPrinter;

use crate::app::EdgeSession;
use crate::printing::transports::assumed_capabilities;
use crate::printing::{Held, HeldPrinter, MISSING_GLYPHS_REPORTED, PrintOutcome, Printers};

impl Printers {
    /// Turns every line the printer's own character set cannot carry into a raster, as wide as its
    /// paper, and leaves out the cut where the printer has no cutter.
    ///
    /// The framework's half of the port's contract (ADR-0026 §5): the adapter sends `Text` as text,
    /// so deciding a line is sendable is the caller's job, and getting it wrong prints a row of
    /// question marks in front of a customer. Before ADR-0102 the only answer available here was to
    /// refuse the whole document; now the line is drawn, at the size in force for `session`
    /// ([`Self::font_size`]), so a change to it applies from the next print.
    ///
    /// # Errors
    ///
    /// The count of characters that could not be rendered at all — no code page, and no font loaded
    /// to draw them with. The caller reports [`PrintOutcome::Unprintable`], which is still better
    /// than sending bytes the printer will mangle.
    fn prepare(
        &self,
        session: &EdgeSession,
        capabilities: &PrinterCapabilities,
        document: PrintDocument,
    ) -> Result<PrintDocument, usize> {
        let size = self
            .renderer
            .as_ref()
            .map(|renderer| self.font_size_now(session, renderer));
        let mut blocks = Vec::with_capacity(document.blocks.len());
        let mut refused = 0_usize;
        for block in document.blocks {
            // A printer with no cutter is sent no cut (ADR-0160): its operator tears the paper.
            if matches!(block, PrintBlock::Cut) && !capabilities.cuts_paper {
                continue;
            }
            let PrintBlock::Text { line, style } = &block else {
                blocks.push(block);
                continue;
            };
            // The printer's own font covers it: text is a fraction of the bytes and comes out of the
            // head faster, so an ASCII receipt still goes as text after this change.
            if capabilities.needs_bitmap(line) == Ok(false) {
                blocks.push(block);
                continue;
            }
            let (Some(renderer), Some(size)) = (self.renderer.as_ref(), size) else {
                refused = refused.saturating_add(line.chars().count());
                continue;
            };
            match renderer.render_at(line, *style, capabilities.dots_per_line, size) {
                Ok(drawn) => {
                    if !drawn.substituted.is_empty() {
                        // The characters, not the line: which glyphs are missing is what an operator
                        // needs to install a font for.
                        //
                        // Truncated at `MISSING_GLYPHS_REPORTED`, because the *whole* set is not the
                        // harmless thing the sentence above assumed. A receipt block carries a
                        // buyer's name (`pos_ports::printer`); on a box with no CJK or Vietnamese
                        // face, every character of that name substitutes, and a set of them in a
                        // durable log reconstructs most of it (ADR-0117 decision 7). A handful names
                        // the script and the font to install, which is the whole operational use.
                        let missing = &drawn.substituted
                            [..drawn.substituted.len().min(MISSING_GLYPHS_REPORTED)];
                        tracing::warn!(
                            missing = ?missing,
                            missing_count = drawn.substituted.len(),
                            "no installed font covers these characters; they printed as boxes"
                        );
                    }
                    blocks.push(drawn.bitmap.into_block());
                }
                Err(error) => {
                    tracing::warn!(%error, "a line could not be rendered");
                    refused = refused.saturating_add(line.chars().count());
                }
            }
        }
        if refused > 0 {
            return Err(refused);
        }
        Ok(PrintDocument { blocks })
    }

    /// Sends one job to one device — down the wire itself, or onto the queue of the agent that
    /// owns that device's transport (ADR-0112).
    pub(super) async fn dispatch(
        &self,
        session: &EdgeSession,
        device: &PublishedDevice,
        mut job: PrintJob,
    ) -> PrintOutcome {
        let capabilities = assumed_capabilities(device, self.can_rasterise());
        // Rendered **before** the branch, deliberately. The agent receives a finished document and
        // decides nothing about it, so the rasterising, the code-page decision, the width and the
        // font size all happen here whichever way the bytes leave. Moving any of it across would
        // mean a store with three terminals printing three different tickets from one order.
        match self.prepare(session, &capabilities, job.document) {
            Ok(document) => job.document = document,
            Err(line) => {
                // Never the line's text: a document may carry a buyer's name and tax code
                // (`pos_ports::printer`).
                tracing::warn!(
                    device = %device.device_id,
                    characters = line,
                    "the document needs characters this printer cannot render as text and no font \
                     is loaded to rasterise them"
                );
                return PrintOutcome::Unprintable;
            }
        }
        // A printer that names an agent is reached through the queue and never through a socket
        // this process opens. Absent — every printer in every store that has not configured one —
        // falls through to exactly the path ADR-0103 shipped.
        if let Some(agent) = device.agent_device_id {
            let Some(lane) = self.agents.as_ref() else {
                tracing::warn!(
                    device = %device.device_id,
                    %agent,
                    "this printer names an agent but no print queue is composed"
                );
                return PrintOutcome::AgentUnavailable;
            };
            return lane.enqueue(agent, device.device_id, job).await;
        }
        let printer = match self.printer_for(device, capabilities) {
            Ok(printer) => printer,
            Err(error) => {
                tracing::warn!(device = %device.device_id, %error, "no channel to that printer");
                return PrintOutcome::Unavailable;
            }
        };
        let job_id = job.job_id;
        // A printer that has been unplugged blocks on a socket timeout, so the write goes on a
        // blocking thread rather than an async worker: the till must stay responsive while the
        // receipt fails.
        let outcome = tokio::task::spawn_blocking(move || printer.print_blocking(&job)).await;
        match outcome {
            Ok(Ok(())) => {
                // The job's identifier and its outcome, never its content (`pos_ports::printer`).
                tracing::info!(%job_id, device = %device.device_id, "printed");
                PrintOutcome::Printed
            }
            Ok(Err(error)) => {
                tracing::warn!(%job_id, device = %device.device_id, %error, "the printer refused the job");
                PrintOutcome::Unavailable
            }
            Err(_) => {
                tracing::warn!(%job_id, device = %device.device_id, "the print thread did not finish");
                PrintOutcome::Unavailable
            }
        }
    }

    /// The held printer for `device`, opening a channel the first time and again whenever the
    /// device's address or its assumed capabilities have changed since ([`Held`]).
    pub(super) fn printer_for(
        &self,
        device: &PublishedDevice,
        capabilities: PrinterCapabilities,
    ) -> Result<HeldPrinter, PortError> {
        let mut open = self.open.lock().map_err(|_| {
            PortError::internal(
                PortName::PrinterDriver,
                "the printer registry lock was poisoned",
            )
        })?;
        if let Some(held) = open.get(&device.device_id)
            && held.address == device.address
            && held.capabilities == capabilities
        {
            return Ok(Arc::clone(&held.printer));
        }
        let transport = self.transports.open(device)?;
        let printer = Arc::new(EscPosPrinter::new(capabilities.clone(), transport));
        open.insert(
            device.device_id,
            Held {
                printer: Arc::clone(&printer),
                address: device.address.clone(),
                capabilities,
            },
        );
        Ok(printer)
    }
}
