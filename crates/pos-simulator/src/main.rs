// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `pos-simulator` entry point: run the scenarios and print the reports (`just simulate`), or
//! the template-by-country matrix (`pos-simulator matrix [--template <dir>] [--country <cc>]`).
//!
//! Everything of substance is the library ([`pos_simulator`]); this only renders it to stdout, so the
//! model and the scenarios stay unit-tested and the one place `print` is allowed is here.

use std::process::ExitCode;

use pos_simulator::matrix;
use pos_simulator::report::{capacity_report, reconciliation_report};

#[expect(
    clippy::disallowed_macros,
    reason = "the simulator's entry point reports its scenario results to stdout — that is its \
              output, and this one binary is the only place the tree's no-print rule is lifted"
)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("matrix") {
        println!("{}", capacity_report());
        println!();
        println!("{}", reconciliation_report());
        return ExitCode::SUCCESS;
    }
    let ran = matrix::Filter::from_args(args.get(1..).unwrap_or_default()).and_then(|filter| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("no runtime: {error}"))?
            .block_on(matrix::run(&filter))
    });
    match ran {
        Ok(results) => {
            let (report, passed) = matrix::render(&results);
            println!("{report}");
            if passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(refusal) => {
            eprintln!("pos-simulator matrix: {refusal}");
            ExitCode::from(2)
        }
    }
}
