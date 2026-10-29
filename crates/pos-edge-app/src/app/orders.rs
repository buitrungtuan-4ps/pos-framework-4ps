// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The order a store takes: inbound orders and the staff who confirm them, sold-out items, the
//! lines an order is made of, the counter, and changing a line once taken.

mod amend;
mod confirmation;
mod counter;
mod inbound;
mod lines;
mod sold_out;
