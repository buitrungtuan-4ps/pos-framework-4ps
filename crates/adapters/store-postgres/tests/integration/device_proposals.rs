// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device-proposal onboarding queue (ADR-0041): propose, list by status,
//! and the one-way pending → approved/rejected resolve.

use pos_proto::devices::{DeviceConnection, PaperWidth};
use pos_proto::printing::{ReceiptLanguage, ReceiptSecondLanguage};

use super::{TENANT_A, block_on, port_err, prepared};

#[test]
fn propose_list_and_resolve_round_trip() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let devices = store.device_proposals();

        // Two proposals for store-1, one for store-2 — all pending.
        devices
            .create(
                "DEV1",
                TENANT_A,
                "store-1",
                "printer",
                "Kitchen 1",
                "10.0.0.1:9100",
            )
            .await
            .expect("propose 1");
        devices
            .create("DEV2", TENANT_A, "store-1", "kds", "Expo", "10.0.0.2")
            .await
            .expect("propose 2");
        devices
            .create(
                "DEV3",
                TENANT_A,
                "store-2",
                "printer",
                "Bar",
                "10.0.0.3:9100",
            )
            .await
            .expect("propose 3");

        // The store-scoped pending list sees only store-1's two; the tenant-wide queue sees all.
        let store_1_pending = devices
            .fetch(TENANT_A, Some("store-1"), "pending")
            .await
            .expect("store-1 pending");
        assert_eq!(store_1_pending.len(), 2, "only store-1's proposals");
        let all_pending = devices
            .fetch(TENANT_A, None, "pending")
            .await
            .expect("tenant pending");
        assert_eq!(all_pending.len(), 3, "the whole tenant's queue");

        // Approve one; it leaves the pending list and joins the approved one, carrying the two
        // facts approval adds (ADR-0100).
        assert!(
            devices
                .mark(
                    TENANT_A,
                    "DEV1",
                    "approved",
                    Some(DeviceConnection::Usb),
                    None
                )
                .await
                .expect("approve"),
            "a pending proposal is resolved"
        );
        let approved = devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("store-1 approved");
        assert_eq!(approved.len(), 1);
        let row = approved.first().expect("one row");
        assert_eq!(row.id, "DEV1");
        // Stored, and stored in the *short* spelling — asserted here rather than left to prose.
        // This is the check that was missing: the seam wrote the config node's prefixed
        // `DEVICE_CONNECTION_USB` into this column instead, `compile_devices` prefixed that a
        // second time, and the store received a token no build can read.
        assert_eq!(
            row.connection.as_deref(),
            Some(DeviceConnection::Usb.short_name()),
            "approval's connection is stored, in the spelling every reader parses"
        );
        assert_eq!(
            row.station_id, None,
            "a counter printer serves the bill, not a station"
        );
        assert_eq!(
            devices
                .fetch(TENANT_A, Some("store-1"), "pending")
                .await
                .expect("store-1 pending after approve")
                .len(),
            1,
            "one of store-1's two is still pending"
        );

        // Resolving again is a no-op: the row is no longer pending.
        assert!(
            !devices
                .mark(TENANT_A, "DEV1", "rejected", None, None)
                .await
                .expect("re-resolve"),
            "an already-resolved proposal does not transition again"
        );
        // And another tenant cannot resolve this tenant's proposal.
        assert!(
            !devices
                .mark(
                    "tenant-b",
                    "DEV2",
                    "approved",
                    Some(DeviceConnection::Network),
                    None
                )
                .await
                .expect("cross-tenant"),
            "the tenant scope stops one tenant resolving another's proposal"
        );
    });
}

/// A terminal is written approved, and an approved device points at it conditionally
/// (ADR-0112).
///
/// Three claims the fake cannot settle, because they are the database's: a terminal row is
/// created already `approved` (there is no proposal for anyone to resolve), the agent pointer
/// round-trips through the column the publish reads, and the conditional write is decided by
/// `xmin` — a real version the adapter does not mint itself.
#[test]
fn a_terminal_is_created_approved_and_an_agent_pick_is_conditional() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let devices = store.device_proposals();

        devices
            .create_terminal("TILL1", TENANT_A, "store-1", "Till 1")
            .await
            .expect("create the terminal");
        devices
            .create(
                "PRN1",
                TENANT_A,
                "store-1",
                "printer",
                "Counter",
                "/dev/usb/lp0",
            )
            .await
            .expect("propose the printer");
        devices
            .mark(
                TENANT_A,
                "PRN1",
                "approved",
                Some(DeviceConnection::Usb),
                None,
            )
            .await
            .expect("approve the printer");

        let approved = devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("read the approved devices");
        assert_eq!(
            approved.len(),
            2,
            "the terminal is approved on creation, with no resolve step of its own"
        );
        let terminal = approved
            .iter()
            .find(|row| row.id == "TILL1")
            .expect("the terminal");
        assert_eq!(terminal.kind, "terminal");
        assert_eq!(terminal.address, "", "nothing dials a terminal");
        assert_eq!(
            terminal.connection, None,
            "a terminal is not attached to the edge at all"
        );
        let printer = approved
            .iter()
            .find(|row| row.id == "PRN1")
            .expect("the printer");
        assert_eq!(
            printer.agent_device_id, None,
            "absent means the edge opens the address itself"
        );

        // A pick against the version the row is at succeeds and moves it on.
        let moved = devices
            .set_agent(TENANT_A, "PRN1", Some("TILL1"), &printer.version)
            .await
            .expect("the conditional pick")
            .expect("a matching version writes");
        assert_ne!(
            moved, printer.version,
            "the write moves the row to a new version"
        );

        // The same token replayed is stale, and the row is unchanged.
        assert!(
            devices
                .set_agent(TENANT_A, "PRN1", None, &printer.version)
                .await
                .expect("the stale pick")
                .is_none(),
            "a caller holding the old version does not silently overwrite the new one"
        );
        assert_eq!(
            devices
                .version_of(TENANT_A, "PRN1")
                .await
                .expect("the current version"),
            Some(moved.clone()),
            "and the row still holds what the first pick left"
        );
        let after = devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("re-read");
        assert_eq!(
            after
                .iter()
                .find(|row| row.id == "PRN1")
                .and_then(|row| row.agent_device_id.clone()),
            Some("TILL1".to_owned()),
            "the agent pointer round-trips through the column the publish reads"
        );
    });
}

/// Only an **approved** device in **this** tenant can be pointed at an agent (ADR-0112).
///
/// Two scopes on one statement, split out from the round trip above so each failure names
/// itself: a pending proposal has no place to print from yet, and one tenant must not reach
/// another's devices — the isolation every statement in this table carries.
#[test]
fn an_agent_pick_reaches_only_this_tenant_s_approved_devices() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let devices = store.device_proposals();

        devices
            .create_terminal("TILL1", TENANT_A, "store-1", "Till 1")
            .await
            .expect("create the terminal");
        devices
            .create(
                "PRN1",
                TENANT_A,
                "store-1",
                "printer",
                "Counter",
                "/dev/usb/lp0",
            )
            .await
            .expect("propose the printer");
        assert!(
            devices
                .version_of(TENANT_A, "PRN1")
                .await
                .expect("a pending row's version")
                .is_none(),
            "a pending proposal is not addressable yet: it has no version to write against"
        );

        devices
            .mark(
                TENANT_A,
                "PRN1",
                "approved",
                Some(DeviceConnection::Usb),
                None,
            )
            .await
            .expect("approve the printer");
        let version = devices
            .version_of(TENANT_A, "PRN1")
            .await
            .expect("the approved row's version")
            .expect("an approved row has one");
        assert!(
            devices
                .set_agent("tenant-b", "PRN1", Some("TILL1"), &version)
                .await
                .expect("cross-tenant pick")
                .is_none(),
            "the tenant scope stops one tenant picking another's print agent"
        );
    });
}

/// A printer's cash drawer is marked, cleared and read back through the column the publish reads
/// (ADR-0165 decision 4), under the same conditional write as the agent pick.
///
/// The default is the claim a fleet upgrade rests on: a row nobody marked reads `false`, so no
/// store's drawer starts opening because the column appeared.
#[test]
fn a_drawer_mark_round_trips_and_a_row_nobody_marked_has_none() {
    async fn read(
        devices: &store_postgres::PostgresDeviceProposals,
    ) -> store_postgres::DeviceProposalRow {
        devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("read the approved devices")
            .into_iter()
            .find(|row| row.id == "PRN1")
            .expect("the printer")
    }

    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let devices = store.device_proposals();
        devices
            .create(
                "PRN1",
                TENANT_A,
                "store-1",
                "printer",
                "Counter",
                "/dev/usb/lp0",
            )
            .await
            .expect("propose the printer");
        devices
            .mark(
                TENANT_A,
                "PRN1",
                "approved",
                Some(DeviceConnection::Usb),
                None,
            )
            .await
            .expect("approve the printer");
        let printer = read(&devices).await;
        assert!(
            !printer.drawer_attached,
            "a printer nobody marked has no drawer"
        );

        let moved = devices
            .set_drawer(TENANT_A, "PRN1", true, &printer.version)
            .await
            .expect("the conditional mark")
            .expect("a matching version writes");
        assert!(read(&devices).await.drawer_attached, "the mark round-trips");
        assert!(
            devices
                .set_drawer(TENANT_A, "PRN1", false, &printer.version)
                .await
                .expect("the stale clear")
                .is_none(),
            "a caller holding the old version does not silently undo the mark"
        );
        assert!(
            devices
                .set_drawer("tenant-b", "PRN1", false, &moved)
                .await
                .expect("cross-tenant clear")
                .is_none(),
            "the tenant scope stops one tenant clearing another's drawer"
        );
        devices
            .set_drawer(TENANT_A, "PRN1", false, &moved)
            .await
            .expect("the clear")
            .expect("the current version writes");
        assert!(
            !read(&devices).await.drawer_attached,
            "and the clear does too"
        );
    });
}

/// A printer's paper and cutter are written and read back through the columns the publish reads
/// (ADR-0160 decision 2), under the drawer mark's conditional write.
///
/// The default is the claim a fleet upgrade rests on: a row nobody set reads neither, which is
/// published as nothing, so no store's receipts change because the columns appeared.
#[test]
fn a_printers_paper_round_trips_as_its_token_and_a_row_nobody_set_has_none() {
    async fn read(
        devices: &store_postgres::PostgresDeviceProposals,
    ) -> store_postgres::DeviceProposalRow {
        devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("read the approved devices")
            .into_iter()
            .find(|row| row.id == "PRN1")
            .expect("the printer")
    }

    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let devices = store.device_proposals();
        devices
            .create(
                "PRN1",
                TENANT_A,
                "store-1",
                "printer",
                "Counter",
                "192.0.2.10:9100",
            )
            .await
            .expect("propose the printer");
        devices
            .mark(
                TENANT_A,
                "PRN1",
                "approved",
                Some(DeviceConnection::Network),
                None,
            )
            .await
            .expect("approve the printer");
        let printer = read(&devices).await;
        assert_eq!(
            (printer.paper_width.as_deref(), printer.cuts_paper),
            (None, None),
            "a printer nobody set says nothing about its paper"
        );

        let moved = devices
            .set_paper(
                TENANT_A,
                "PRN1",
                PaperWidth::Millimetres58,
                false,
                &printer.version,
            )
            .await
            .expect("the conditional write")
            .expect("a matching version writes");
        let written = read(&devices).await;
        assert_eq!(
            (written.paper_width.as_deref(), written.cuts_paper),
            (Some("PAPER_WIDTH_MILLIMETRES_58"), Some(false)),
            "the paper is stored as the token the node carries"
        );
        assert!(
            devices
                .set_paper(
                    TENANT_A,
                    "PRN1",
                    PaperWidth::Millimetres80,
                    true,
                    &printer.version
                )
                .await
                .expect("the stale write")
                .is_none(),
            "a caller holding the old version does not silently undo the paper"
        );
        assert!(
            devices
                .set_paper("tenant-b", "PRN1", PaperWidth::Millimetres80, true, &moved)
                .await
                .expect("cross-tenant write")
                .is_none(),
            "the tenant scope stops one tenant setting another's paper"
        );
    });
}

/// A till's receipt printer and languages are written and read back through the columns the
/// publish reads (ADR-0160 decision 4), under the paper's conditional write, and cleared with
/// nulls.
///
/// The default is the claim a fleet upgrade rests on: a terminal nobody set reads none of the
/// three, which is published as nothing, so no till's receipts change because the columns
/// appeared.
#[test]
fn a_tills_receipts_round_trip_as_tokens_and_a_terminal_nobody_set_has_none() {
    async fn read(
        devices: &store_postgres::PostgresDeviceProposals,
    ) -> store_postgres::DeviceProposalRow {
        devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("read the approved devices")
            .into_iter()
            .find(|row| row.id == "TILL1")
            .expect("the terminal")
    }
    fn receipts(row: &store_postgres::DeviceProposalRow) -> [Option<&str>; 3] {
        [
            row.receipt_printer_id.as_deref(),
            row.receipt_language.as_deref(),
            row.receipt_second_language.as_deref(),
        ]
    }

    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let devices = store.device_proposals();
        devices
            .create_terminal("TILL1", TENANT_A, "store-1", "Bar till")
            .await
            .expect("create the terminal");
        let till = read(&devices).await;
        assert_eq!(
            receipts(&till),
            [None, None, None],
            "a terminal nobody set says nothing about its receipts"
        );

        let moved = devices
            .set_receipt(
                TENANT_A,
                "TILL1",
                Some("PRN2"),
                Some(ReceiptLanguage::English),
                Some(ReceiptSecondLanguage::None),
                &till.version,
            )
            .await
            .expect("the conditional write")
            .expect("a matching version writes");
        assert_eq!(
            receipts(&read(&devices).await),
            [
                Some("PRN2"),
                Some("RECEIPT_LANGUAGE_EN"),
                Some("RECEIPT_SECOND_LANGUAGE_NONE")
            ],
            "each language is stored as the token the node carries"
        );
        assert!(
            devices
                .set_receipt(TENANT_A, "TILL1", None, None, None, &till.version)
                .await
                .expect("the stale write")
                .is_none(),
            "a caller holding the old version does not silently undo the receipts"
        );
        assert!(
            devices
                .set_receipt("tenant-b", "TILL1", None, None, None, &moved)
                .await
                .expect("cross-tenant write")
                .is_none(),
            "the tenant scope stops one tenant setting another's receipts"
        );
        devices
            .set_receipt(TENANT_A, "TILL1", None, None, None, &moved)
            .await
            .expect("the clearing write")
            .expect("a matching version clears");
        assert_eq!(
            receipts(&read(&devices).await),
            [None, None, None],
            "a till cleared is the store's again"
        );
    });
}

/// A till's own float is written and read back through the column migration 0085 adds, under
/// the receipts' conditional write, kept across the next boot, and cleared with a null
/// (ADR-0167 decision 4).
///
/// The default is the claim a fleet upgrade rests on: a terminal nobody set reads no float,
/// which is published as nothing, so no till's drawer opens differently because the column
/// appeared.
#[test]
fn a_tills_float_round_trips_through_the_next_boot_and_a_terminal_nobody_set_has_none() {
    async fn float(devices: &store_postgres::PostgresDeviceProposals) -> (Option<i64>, String) {
        let till = devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("read the approved devices")
            .into_iter()
            .find(|row| row.id == "TILL1")
            .expect("the terminal");
        (till.opening_float_minor, till.version)
    }

    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let devices = store.device_proposals();
        devices
            .create_terminal("TILL1", TENANT_A, "store-1", "Bar till")
            .await
            .expect("create the terminal");
        let (unset, version) = float(&devices).await;
        assert_eq!(unset, None, "a terminal nobody set has no float of its own");

        let moved = devices
            .set_float(TENANT_A, "TILL1", Some(250_000), &version)
            .await
            .expect("the conditional write")
            .expect("a matching version writes");
        assert!(
            devices
                .set_float(TENANT_A, "TILL1", None, &version)
                .await
                .expect("the stale write")
                .is_none(),
            "a caller holding the old version does not silently undo the float"
        );
        assert!(
            devices
                .set_float("tenant-b", "TILL1", None, &moved)
                .await
                .expect("cross-tenant write")
                .is_none(),
            "the tenant scope stops one tenant setting another's float"
        );

        store.migrate().await.expect("the next boot");
        let (kept, version) = float(&devices).await;
        assert_eq!(
            kept,
            Some(250_000),
            "the next boot applies 0085 again and keeps what was written"
        );
        devices
            .set_float(TENANT_A, "TILL1", Some(0), &version)
            .await
            .expect("the zero write")
            .expect("a matching version writes");
        let (zero, version) = float(&devices).await;
        assert_eq!(
            zero,
            Some(0),
            "a drawer started empty is a float, not the store's"
        );
        devices
            .set_float(TENANT_A, "TILL1", None, &version)
            .await
            .expect("the clearing write")
            .expect("a matching version clears");
        assert_eq!(
            float(&devices).await.0,
            None,
            "a till cleared is the store's again"
        );
    });
}

/// Migration 0055 repairs the rows a two-spelling seam already wrote.
///
/// Typing the argument stops the mistake being made again; it does nothing for the approvals
/// already in a production database. Those rows carry the config node's `DEVICE_CONNECTION_USB`
/// where the column's contract is `usb`, and until they are repaired every one of them still
/// publishes a token no build can read — a USB printer arriving at its store as a network
/// device, cash drawer disabled. So the repair runs at boot with the rest of the schema, and
/// this proves it on a row written the old way.
#[test]
fn the_migration_repairs_a_connection_written_in_the_node_s_spelling() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");

        // A row exactly as the old seam left it, alongside one already spelled correctly and one
        // carrying a value that is neither — the last is the case the repair must not touch.
        admin
            .batch_execute(
                "INSERT INTO device_proposals                        (id, tenant_id, store_id, kind, name, address, status, connection) VALUES                        ('OLD1', 'tenant-a', 'store-1', 'printer', 'Counter', '/dev/usb/lp0',                         'approved', 'DEVICE_CONNECTION_USB'),                        ('OK1', 'tenant-a', 'store-1', 'printer', 'Bar', '10.0.0.9:9100',                         'approved', 'network'),                        ('ODD1', 'tenant-a', 'store-1', 'printer', 'Label', '10.0.0.8:9100',                         'approved', 'infrared')",
            )
            .await
            .expect("seed rows written under the old behaviour");

        // Re-running the schema is what a boot does; the repair is idempotent by its own WHERE.
        store.migrate().await.map_err(port_err)?;
        store.migrate().await.map_err(port_err)?;

        let devices = store.device_proposals();
        let rows = devices
            .fetch(TENANT_A, Some("store-1"), "approved")
            .await
            .expect("read the repaired rows");
        let spelling = |id: &str| {
            rows.iter()
                .find(|row| row.id == id)
                .and_then(|row| row.connection.clone())
        };
        assert_eq!(
            spelling("OLD1").as_deref(),
            Some("usb"),
            "the node's spelling is repaired to the column's"
        );
        assert_eq!(
            spelling("OK1").as_deref(),
            Some("network"),
            "a row already correct is left alone"
        );
        assert_eq!(
            spelling("ODD1").as_deref(),
            Some("infrared"),
            "a value that is neither spelling is left exactly as it is, not guessed at"
        );
        Ok::<(), pos_contract_tests::harness::HarnessError>(())
    })
    .expect("the repair runs");
}
