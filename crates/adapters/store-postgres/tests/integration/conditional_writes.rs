// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Conditional writes over the `xmin` system column (ADR-0094).

use super::{block_on, prepared};
use store_postgres::RowUpdate;

/// The compare-and-swap itself, against the real planner.
///
/// Three things can only be shown here. That `xmin` **moves on every `UPDATE`** — the whole
/// scheme rests on it, and no fake can prove Postgres does it. That a stale version is refused
/// as a `VersionMismatch` rather than silently applying. And that a **garbled** tag is a
/// mismatch too, not a database error: the comparison is on `xmin::text`, because casting
/// caller-supplied text to `xid` raises `invalid input syntax for type xid` and would turn a
/// client's stale tag into a `500` instead of the `412` it has earned.
#[test]
fn a_stale_or_garbled_version_is_refused_and_a_current_one_moves_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let registry = store.registry();
        let tenant = "0000000000TENANTXMINAAAAAA";

        let first = registry
            .insert_tenant(tenant, "Placeholder")
            .await
            .expect("insert the tenant");

        // A garbled tag: not a transaction id at all. It must simply not match.
        assert_eq!(
            registry
                .set_tenant(tenant, "Nope", "active", "not-a-transaction-id")
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch
        );

        // The current version applies, and hands back a different one.
        let second = match registry
            .set_tenant(tenant, "Pizza 4P's", "active", &first)
            .await
            .expect("the update")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the update to apply, got {other:?}")
            }
        };
        assert_ne!(
            second, first,
            "xmin must move on every UPDATE, or the next write would be unguarded"
        );

        // Replaying the first version is the lost update, refused.
        assert_eq!(
            registry
                .set_tenant(tenant, "Stale Overwrite", "archived", &first)
                .await
                .expect("the update"),
            RowUpdate::VersionMismatch
        );

        // And the refused write changed nothing.
        let rows = registry.fetch_tenants().await.expect("list the tenants");
        // By id, not by position: the shared truncation between cases does not clear `tenants`.
        let row = rows
            .iter()
            .find(|row| row.tenant_id == tenant)
            .expect("the tenant is there");
        assert_eq!(row.name, "Pizza 4P's");
        assert_eq!(row.status, "active");
        assert_eq!(row.version, second);
    });
}

/// The floor family's own SQL, which the registry case cannot cover.
///
/// Two things are specific to these tables. Each `SELECT` had `xmin::text` appended to a
/// hand-written column list whose mapper reads by **position**, so a column added in the wrong
/// place would hand back a name where a version belongs — only a real read proves the order.
/// And `set_station` writes a self-referencing `backup_station_id`, the most intricate of the
/// three conditional statements.
#[test]
fn a_floor_row_carries_the_version_it_was_read_at_and_a_stale_write_is_refused() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let floor = store.floor();
        let tenant = "0000000000TENANTFLOORXMINA";
        let store_id = "00000000000000STOREFLOORXA";
        let area = "0000000000000000AREAXMINAA";
        let station = "000000000000000STATIONXMIN";

        let area_version = floor
            .insert_area(area, tenant, store_id, "Terrace")
            .await
            .expect("insert the area");
        let station_version = floor
            .insert_station(station, tenant, store_id, "Oven", None, true, None, None)
            .await
            .expect("insert the station");

        // The read hands back the same token the insert minted, from the right column.
        let areas = floor
            .fetch_areas(tenant, store_id)
            .await
            .expect("list the areas");
        let row = areas.first().expect("the area is there");
        assert_eq!(
            row.name, "Terrace",
            "the column order survived the addition"
        );
        assert_eq!(row.version, area_version);

        // A stale tag is refused; the current one applies and moves the row.
        assert_eq!(
            floor
                .set_station(
                    tenant, station, "Nope", None, false, None, None, "active", "1"
                )
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch
        );
        let moved = match floor
            .set_station(
                tenant,
                station,
                "Pizza oven",
                Some(station),
                false,
                None,
                None,
                "active",
                &station_version,
            )
            .await
            .expect("the update")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the update to apply, got {other:?}")
            }
        };
        assert_ne!(moved, station_version, "xmin moves on every UPDATE");

        let stations = floor
            .fetch_stations(tenant, store_id)
            .await
            .expect("list the stations");
        let row = stations.first().expect("the station is there");
        assert_eq!(row.name, "Pizza oven");
        assert_eq!(row.backup_station_id.as_deref(), Some(station));
        assert_eq!(row.version, moved);

        // The other half of the same probe, and the half that had no assertion: when the UPDATE
        // matches nothing, the probe decides whether the caller sent a stale version (412) or
        // named a station that does not exist (404). Only the stale branch was covered, so the
        // probe could — and did — query a table that was not there while still looking correct.
        assert_eq!(
            floor
                .set_station(
                    tenant,
                    "00000000000000NOSUCHSTATIO",
                    "Ghost",
                    None,
                    false,
                    None,
                    None,
                    "active",
                    &moved,
                )
                .await
                .expect("the probe must not raise"),
            RowUpdate::NotFound
        );
    });
}

/// A station's late threshold (migration 0080, ADR-0160 decision 2) is written on create and on
/// update and read back from the column the mapper expects; a station nobody has set reads
/// `None`, which the cloud publishes as nothing, and an update that clears it writes null again.
#[test]
fn a_stations_late_threshold_round_trips_and_a_row_nobody_set_has_none() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let floor = store.floor();
        let tenant = "0000000000TENANTSTATIONLATE";
        let store_id = "000000000000STORESTATIONLA";
        let oven = "0000000000000STATIONOVENLA";
        let bar = "00000000000000STATIONBARLA";

        let oven_version = floor
            .insert_station(oven, tenant, store_id, "Oven", None, true, Some(240), None)
            .await
            .expect("insert the oven");
        floor
            .insert_station(bar, tenant, store_id, "Bar", None, false, None, None)
            .await
            .expect("insert the bar");
        let read = |rows: &[store_postgres::StationRow], id: &str| {
            rows.iter()
                .find(|row| row.id == id)
                .map(|row| (row.name.clone(), row.late_after_seconds))
                .expect("the station is listed")
        };
        let rows = floor
            .fetch_stations(tenant, store_id)
            .await
            .expect("list the stations");
        assert_eq!(read(&rows, oven), ("Oven".to_owned(), Some(240)));
        assert_eq!(read(&rows, bar), ("Bar".to_owned(), None));

        let moved = match floor
            .set_station(
                tenant,
                oven,
                "Oven",
                None,
                true,
                Some(900),
                None,
                "active",
                &oven_version,
            )
            .await
            .expect("the update")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the update to apply, got {other:?}")
            }
        };
        let row = floor
            .fetch_station(tenant, oven)
            .await
            .expect("read the oven")
            .expect("the oven is there");
        assert_eq!(row.late_after_seconds, Some(900));
        assert_eq!(
            row.version, moved,
            "the version still comes from its own column"
        );

        floor
            .set_station(
                tenant, oven, "Oven", None, true, None, None, "active", &moved,
            )
            .await
            .expect("the update");
        let row = floor
            .fetch_station(tenant, oven)
            .await
            .expect("read the oven")
            .expect("the oven is there");
        assert_eq!(
            row.late_after_seconds, None,
            "cleared, it is nobody's again"
        );
    });
}

/// A station's ticket language (migration 0081, ADR-0160 decision 2) is written on create and on
/// update as the token the cloud validated, and read back from the column the mapper expects; a
/// station nobody has set reads `None`, which the cloud publishes as nothing, and an update that
/// clears it writes null again.
#[test]
fn a_stations_ticket_language_round_trips_and_a_row_nobody_set_has_none() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let floor = store.floor();
        let tenant = "000000000TENANTSTATIONLANG";
        let store_id = "000000000000STORESTATIONLG";
        let oven = "0000000000000STATIONOVENLG";
        let bar = "00000000000000STATIONBARLG";
        let english = Some("RECEIPT_LANGUAGE_EN");

        let oven_version = floor
            .insert_station(oven, tenant, store_id, "Oven", None, true, None, english)
            .await
            .expect("insert the oven");
        floor
            .insert_station(bar, tenant, store_id, "Bar", None, false, Some(300), None)
            .await
            .expect("insert the bar");
        let rows = floor
            .fetch_stations(tenant, store_id)
            .await
            .expect("list the stations");
        let read = |id: &str| {
            rows.iter()
                .find(|row| row.id == id)
                .map(|row| (row.ticket_language.clone(), row.late_after_seconds))
                .expect("the station is listed")
        };
        assert_eq!(read(oven), (english.map(str::to_owned), None));
        assert_eq!(read(bar), (None, Some(300)), "each column in its own place");

        let vietnamese = Some("RECEIPT_LANGUAGE_VI");
        let moved = match floor
            .set_station(
                tenant,
                oven,
                "Oven",
                None,
                true,
                None,
                vietnamese,
                "active",
                &oven_version,
            )
            .await
            .expect("the update")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the update to apply, got {other:?}")
            }
        };
        let row = floor
            .fetch_station(tenant, oven)
            .await
            .expect("read the oven")
            .expect("the oven is there");
        assert_eq!(row.ticket_language.as_deref(), vietnamese);
        assert_eq!(
            row.version, moved,
            "the version still comes from its own column"
        );

        floor
            .set_station(
                tenant, oven, "Oven", None, true, None, None, "active", &moved,
            )
            .await
            .expect("the update");
        let row = floor
            .fetch_station(tenant, oven)
            .await
            .expect("read the oven")
            .expect("the oven is there");
        assert_eq!(row.ticket_language, None, "cleared, it is nobody's again");
    });
}

/// The people family's own SQL. `role_templates` stores its permissions as `jsonb`, so its
/// column list is the one most likely to have taken `xmin::text` in the wrong position.
#[test]
fn a_people_row_carries_the_version_it_was_read_at_and_a_stale_write_is_refused() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let people = store.people();
        let tenant = "000000000TENANTPEOPLEXMINA";
        let role = "00000000000000000ROLEXMINA";

        let first = people
            .insert_role_template(
                role,
                tenant,
                "Cashier",
                "[\"sales.item.open\"]",
                "[\"billing.bill.void\"]",
                None,
            )
            .await
            .expect("insert the role template");

        let rows = people
            .fetch_role_templates(tenant)
            .await
            .expect("list the role templates");
        let row = rows.first().expect("the role is there");
        assert_eq!(
            row.name, "Cashier",
            "the column order survived the addition"
        );
        assert_eq!(row.permissions_json, "[\"sales.item.open\"]");
        assert_eq!(
            row.permissions_with_approval_json,
            "[\"billing.bill.void\"]"
        );
        assert_eq!(row.version, first);

        assert_eq!(
            people
                .set_role_template(
                    tenant,
                    role,
                    "Nope",
                    "[]",
                    "[]",
                    "active",
                    None,
                    "not-a-transaction-id",
                )
                .await
                .expect("the comparison must not raise"),
            RowUpdate::VersionMismatch
        );
        let second = match people
            .set_role_template(
                tenant, role, "Cashier", "[]", "[]", "archived", None, &first,
            )
            .await
            .expect("the update")
        {
            RowUpdate::Updated(version) => version,
            other @ (RowUpdate::VersionMismatch | RowUpdate::NotFound) => {
                panic!("expected the update to apply, got {other:?}")
            }
        };
        assert_ne!(second, first, "xmin moves on every UPDATE");

        let rows = people
            .fetch_role_templates(tenant)
            .await
            .expect("list the role templates");
        let row = rows.first().expect("the role is there");
        assert_eq!(row.status, "archived");
        assert_eq!(row.version, second);
    });
}

/// An absent row is `NotFound`, not `VersionMismatch` — the probe on the failure path is what
/// separates a `404` from a `412`, and zero rows alone cannot.
#[test]
fn an_absent_row_is_not_found_rather_than_a_version_mismatch() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let registry = store.registry();
        assert_eq!(
            registry
                .set_tenant("0000000000TENANTGONEAAAAAA", "Nope", "active", "1")
                .await
                .expect("the update"),
            RowUpdate::NotFound
        );
    });
}
