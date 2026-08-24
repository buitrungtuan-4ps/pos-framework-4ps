// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos-simulator` entry point: run the scenarios and print the reports (`just simulate`).
//!
//! Everything of substance is the library ([`pos_simulator`]); this only renders it to stdout, so the
//! model and the scenarios stay unit-tested and the one place `print` is allowed is here.

use pos_simulator::report::{capacity_report, reconciliation_report};

#[expect(
    clippy::disallowed_macros,
    reason = "the simulator's entry point reports its scenario results to stdout — that is its \
              output, and this one binary is the only place the tree's no-print rule is lifted"
)]
fn main() {
    println!("{}", capacity_report());
    println!();
    println!("{}", reconciliation_report());
}
