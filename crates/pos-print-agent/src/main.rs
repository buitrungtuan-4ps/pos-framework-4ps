// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos_print_agent` entry point.
//!
//! Deliberately tiny, and for the same reason [`pos_edge`'s](../../pos-edge/src/main.rs) is: read
//! the configuration, build the client, build the printers, run the loop. Everything testable lives
//! in the library. The configuration path comes from `POS_PRINT_AGENT_CONFIG`, defaulting to
//! `print-agent.toml` beside the binary.
//!
//! One flag: `--self-test`, which answers *can these bytes run on this box, and can they read this
//! machine's configuration* without touching a printer or the edge. The installer runs it after a
//! swap, exactly as the edge's does ([ADR-0055](../../../docs/adr/0055-edge-ota-updater.md)
//! Amendment 1); opening a printer here would be the wrong test, because a printer that is off is
//! an ordinary state and not a bad binary.

use std::path::PathBuf;

use pos_print_agent::printers::EscPosPrinters;
use pos_print_agent::wire::HttpEdge;
use pos_print_agent::{AgentError, Config, LastWritten, VERSION};

/// The flag the installer runs a staged binary with.
const SELF_TEST_FLAG: &str = "--self-test";

fn main() -> Result<(), AgentError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_unset| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let path = std::env::var_os("POS_PRINT_AGENT_CONFIG")
        .map_or_else(|| PathBuf::from("print-agent.toml"), PathBuf::from);
    let config = Config::load(&path)?;

    if std::env::args().any(|argument| argument == SELF_TEST_FLAG) {
        // Reaching here means the loader accepted the executable and the configuration parses.
        // Deliberately no printer and no edge: both are ordinary states of the world rather than
        // facts about these bytes.
        tracing::info!(version = VERSION, "self-test passed");
        return Ok(());
    }

    let edge = HttpEdge::new(&config.edge_url, &config.device_token)?;
    let written = LastWritten::load(&config.state_path);
    tracing::info!(
        version = VERSION,
        edge = %config.edge_url,
        state = %config.state_path.display(),
        "claiming print jobs"
    );

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| AgentError::Config(format!("the runtime could not be built: {error}")))?
        .block_on(pos_print_agent::run(
            edge,
            EscPosPrinters::new(),
            written,
            shutdown(),
        ));
    Ok(())
}

/// Resolves on Ctrl-C, or on `SIGTERM` where there is one.
///
/// A stop mid-job leaves that job unacknowledged, so the queue hands it back at the lease and the
/// next agent — or this one after a restart — prints it. There is nothing to drain.
async fn shutdown() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    tracing::warn!(%error, "SIGTERM could not be watched; Ctrl-C still stops this");
                    let _ignored = interrupt.await;
                    return;
                }
            };
        tokio::select! {
            _ignored = interrupt => {}
            _ignored = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ignored = interrupt.await;
    }
}
