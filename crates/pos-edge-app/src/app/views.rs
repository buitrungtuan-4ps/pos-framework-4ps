// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the edge's commands and reads answer with: the views of orders, bills and shifts.

mod bills;
mod orders;
mod shifts;

pub use bills::{
    BillCheckView, BillView, BuyerDetails, MAX_SETTLED_LISTED, PreBill, ReceiptCopy, ReceiptLine,
    SettledBillView, SettledVoidView, Takings,
};
pub use orders::{
    BumpView, CounterOrderLine, CounterOrderOpened, CounterOrderView, FiredLine, LineDraft,
    LineView, LiveOrderLine, LiveOrderView, OrderLineChoice, TableView, TransferView, WaitingLine,
};
pub use shifts::{
    CashMovement, ClosedDrawers, CombinedReport, DrawerFigures, DrawerView, DrawersView,
    HandoverSlip, ShiftHeader, ShiftReport, ShiftView, TillScope,
};
