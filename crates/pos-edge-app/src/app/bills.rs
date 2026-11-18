// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The bill: opening it, reading what it owes, voiding, discounting, comping a line of and waiving a fee on it, splitting
//! and merging it, settling it, and the receipts, takings and buyers a settle leaves behind.

mod checks;
mod comp;

pub use checks::CheckPrinted;
pub use payments::PaymentTaken;
pub use void::VoidedBill;
mod open;
mod pay_first;
mod payments;
mod reads;
mod receipts;
mod reductions;
mod regroup;
mod settle;
mod void;
mod void_settled;

#[cfg(test)]
mod tests;
