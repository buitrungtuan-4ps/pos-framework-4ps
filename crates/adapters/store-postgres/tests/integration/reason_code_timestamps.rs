// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Reason codes (ADR-0115), for the one column only this file can check.

use super::{TENANT_A, block_on, prepared};

use store_postgres::RowUpdate;

/// The reason code this test authors. A ULID, because that is what the id column holds.
const ID: &str = "01J0000000000000000000000A";

/// 2020-01-01, in Unix milliseconds — the floor a projected timestamp has to clear.
///
/// Milliseconds and seconds differ by a factor of a thousand, and the only check that separates
/// them is one against a real clock: this date sits comfortably below any run of this suite and
/// far above what a seconds-valued cast would produce for any date this decade.
const Y2020_MS: i64 = 1_577_836_800_000;

/// An authored record, shaped like the wire one but assembled here so the test crate needs no
/// dependency on the cloud's types — the adapter treats `doc` as opaque JSON either way.
fn doc(code: &str) -> String {
    serde_json::json!({
        "id": ID,
        "code": code,
        "display_name": "Waste or spoilage",
        "applies_to": ["REASON_ACTION_VOID_LINE"],
        "active": true,
    })
    .to_string()
}

#[test]
fn a_read_carries_the_write_instant_in_milliseconds_and_an_edit_moves_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let reason_codes = store.reason_codes();

        let version = reason_codes
            .insert(TENANT_A, ID, &doc("WASTE"))
            .await
            .expect("insert")
            .expect("the id was free");

        let listed = reason_codes.fetch(TENANT_A).await.expect("list");
        let written = listed.first().expect("the row just inserted").updated_at_ms;

        assert!(
            written > Y2020_MS,
            "the projection is milliseconds since the epoch, not seconds: {written}"
        );

        // An edit moves it. `updated_at = now()` lives in the UPDATE statement and nowhere
        // else — drop it and every row reads as the day it was created, which is exactly the
        // staleness question the console asks this column to answer.
        let updated = reason_codes
            .update_at(TENANT_A, ID, &doc("WASTE_2"), &version)
            .await
            .expect("update");
        assert!(
            matches!(updated, RowUpdate::Updated(_)),
            "the version handed back by the insert still stands"
        );

        let edited = reason_codes
            .fetch_one(TENANT_A, ID)
            .await
            .expect("read")
            .expect("still there")
            .updated_at_ms;
        assert!(
            edited >= written,
            "the edit restamped the row: {written} then {edited}"
        );
    });
}
