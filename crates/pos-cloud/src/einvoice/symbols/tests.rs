// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::CountryCode;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use super::{
    AllocateError, InMemorySymbolLedger, SymbolLedger, SymbolRecord, SymbolSource, allocate,
};

fn tenant(n: u128) -> TenantId {
    TenantId::new(Ulid::from_u128(n))
}

fn store(n: u128) -> StoreId {
    StoreId::new(Ulid::from_u128(1_000 + n))
}

fn now() -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(1_790_000_000_000).expect("an instant")
}

fn by_hand(symbol: &str, holder: StoreId) -> SymbolRecord {
    SymbolRecord {
        symbol: symbol.to_owned(),
        store_id: Some(holder.to_string()),
        lease_generation: None,
        source: SymbolSource::Manual,
        recorded_at_ms: 1,
    }
}

#[cfg(feature = "country-vn")]
#[tokio::test]
async fn each_store_is_allocated_a_symbol_no_other_store_of_the_tenant_held() {
    let registry = crate::countries::registry();
    let vietnam = registry.get(CountryCode::parse("VN").expect("VN"));
    let ledger = InMemorySymbolLedger::default();
    // A symbol typed by hand earlier is skipped, and so is a replaced box's.
    ledger
        .record(tenant(1), by_hand("1C26MAB", store(9)))
        .await
        .expect("record");
    let mut given = Vec::new();
    for (n, generation) in [(1, Some(0)), (2, None), (1, Some(1))] {
        let record = allocate(
            &ledger,
            vietnam,
            tenant(1),
            Some(store(n)),
            generation,
            2026,
            now(),
        )
        .await
        .expect("allocated");
        assert!(record.is_held_by(store(n)));
        assert_eq!(record.source, SymbolSource::Allocated);
        given.push(record.symbol);
    }
    assert_eq!(given, ["1C26MAA", "1C26MAC", "1C26MAD"]);
    // Another tenant is another seller, and starts again.
    let other = allocate(
        &ledger,
        vietnam,
        tenant(2),
        Some(store(5)),
        None,
        2026,
        now(),
    )
    .await
    .expect("allocated");
    assert_eq!(other.symbol, "1C26MAA");
    // The correction symbol comes from the same ledger, with no store.
    let correction = allocate(&ledger, vietnam, tenant(1), None, None, 2026, now())
        .await
        .expect("allocated");
    assert_eq!(
        (correction.symbol.as_str(), correction.store_id),
        ("1C26MAE", None)
    );
}

#[tokio::test]
async fn a_country_with_no_rule_allocates_nothing() {
    let ledger = InMemorySymbolLedger::default();
    let registry = crate::countries::registry();
    for code in ["JP", "IN"] {
        let module = CountryCode::parse(code)
            .ok()
            .and_then(|code| registry.get(code));
        assert_eq!(
            allocate(
                &ledger,
                module,
                tenant(1),
                Some(store(1)),
                None,
                2026,
                now()
            )
            .await,
            Err(AllocateError::NoRule),
            "{code}"
        );
    }
    assert_eq!(
        allocate(&ledger, None, tenant(1), Some(store(1)), None, 2026, now()).await,
        Err(AllocateError::NoRule)
    );
}

#[tokio::test]
async fn a_symbol_stays_with_its_first_holder() {
    let ledger = InMemorySymbolLedger::default();
    let first = by_hand("1C26MAA", store(1));
    ledger
        .record(tenant(1), first.clone())
        .await
        .expect("record");
    let held = ledger
        .record(tenant(1), by_hand("1C26MAA", store(2)))
        .await
        .expect("record");
    assert_eq!(held, first);
    assert!(!held.is_held_by(store(2)));
    assert_eq!(ledger.list(tenant(1)).await.expect("list"), vec![first]);
}
