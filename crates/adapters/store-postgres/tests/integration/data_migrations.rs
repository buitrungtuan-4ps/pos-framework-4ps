// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The record of the one-time changes the cloud makes through its own code (`data_migrations`):
//! a change is unrecorded until it is recorded, recording it twice keeps the first row, and a name
//! recorded by a migration's own SQL reads as made too, since the two share the table.

use super::{block_on, prepared};

const CHANGE: &str = "qr_ordering_one_switch";

#[test]
fn a_change_reads_as_made_at_its_time_once_recorded_and_recording_it_again_keeps_the_first() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let markers = store.data_migrations();

        assert_eq!(
            markers
                .recorded_time(CHANGE)
                .await
                .expect("read the record"),
            None,
            "nothing has recorded the change yet"
        );
        markers
            .record(CHANGE, 1_790_000_000_123)
            .await
            .expect("record the change");
        assert_eq!(
            markers
                .recorded_time(CHANGE)
                .await
                .expect("read the record"),
            Some(1_790_000_000_123),
            "the time comes back to the millisecond"
        );

        markers
            .record(CHANGE, 1_790_000_999_000)
            .await
            .expect("record the change again");
        let rows = admin
            .query(
                "SELECT name FROM data_migrations WHERE name = $1",
                &[&CHANGE],
            )
            .await
            .expect("read the rows");
        assert_eq!(rows.len(), 1, "one row per change");
        assert_eq!(
            markers
                .recorded_time(CHANGE)
                .await
                .expect("read the record"),
            Some(1_790_000_000_123),
            "the second record keeps the first row, and its time"
        );

        // A name a migration's SQL claimed is in the same table, so it reads as made as well.
        admin
            .execute(
                "INSERT INTO data_migrations (name) VALUES ('0073_roles_keep_every_till_action')",
                &[],
            )
            .await
            .expect("claim a SQL migration's row");
        assert!(
            markers
                .recorded_time("0073_roles_keep_every_till_action")
                .await
                .expect("read the record")
                .is_some()
        );
    });
}
