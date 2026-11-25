// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The e-invoice symbol ledger (ADR-0240 decision 1, migration 0105).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::EInvoiceSymbolRow;

fn row(symbol: &str, store: Option<&str>, at: i64) -> EInvoiceSymbolRow {
    EInvoiceSymbolRow {
        symbol: symbol.to_owned(),
        store_id: store.map(str::to_owned),
        lease_generation: Some(0),
        source: "ALLOCATED".to_owned(),
        recorded_at_ms: at,
    }
}

/// A symbol is recorded once per tenant and the first holder keeps it; another tenant may hold the
/// same symbol, and reads none of the first tenant's.
#[test]
fn a_symbol_is_held_by_its_first_store_for_good() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let ledger = store.einvoice_symbols();
        let first = row("1C26MAA", Some("01J0000000000000000000STR1"), 1);
        assert_eq!(
            ledger.record(TENANT_A, &first).await.expect("record"),
            first
        );
        let second = row("1C26MAA", Some("01J0000000000000000000STR2"), 2);
        assert_eq!(
            ledger.record(TENANT_A, &second).await.expect("record"),
            first,
            "the first store keeps the symbol"
        );
        assert_eq!(
            ledger.record(TENANT_B, &second).await.expect("record"),
            second
        );
        let correction = row("1C26MAB", None, 3);
        ledger.record(TENANT_A, &correction).await.expect("record");
        assert_eq!(
            ledger.list(TENANT_A).await.expect("list"),
            vec![first, correction]
        );
    });
}
