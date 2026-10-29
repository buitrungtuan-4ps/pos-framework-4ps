// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's printing
//! ([ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md),
//! [ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4,
//! [ADR-0175](../../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md)).
//!
//! - [`printing`]: `Printers`, which device prints what, the receipt, ticket and report documents,
//!   the cash drawer, and handing a job to a printer directly or to a print agent.
//! - [`print_agent`], [`print_queue`] and [`print_wake`]: the print agents a store binds
//!   (ADR-0112), the queue they claim jobs from, and the wake that tells them to look, with their
//!   SQLite impls.
//! - [`paper_labels`]: the words paper prints, in each language.
//! - [`config`]: where the value in force of a setting comes from, and the log line that says so
//!   (ADR-0160 decision 6).
//!
//! It stands on `pos-edge-app` (`Edge` and its authorities). `pos-edge` re-exports every module
//! under its old path (`pos_edge::printing`, `pos_edge::print_agent`, …) and `ValueSource` and
//! `log_in_force` in `pos_edge::config`. Each module that logs keeps its old `pos_edge::…` `tracing`
//! target through a `LOG_TARGET` constant, so log filters see the same targets.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

// Moved out of pos-edge with their `crate::` paths (ADR-0170 decision 4); these imports keep them
// resolving.
use pos_edge_app::{app, clock, line_notes};

pub mod config;
pub mod paper_labels;
pub mod print_agent;
pub mod print_queue;
pub mod print_wake;
pub mod printing;
