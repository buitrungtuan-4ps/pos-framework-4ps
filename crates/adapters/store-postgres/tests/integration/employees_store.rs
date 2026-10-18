// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The employee store: tenant-scoped, RLS-isolated, PIN held only as its hash (ADR-0070).

use core::fmt::Write as _;

use pos_proto::error::ErrorStatus;
use store_postgres::EmployeeOrder;

use super::{TENANT_A, block_on, prepared};
use store_postgres::RowUpdate;

/// Employees insert and read back tenant-scoped and newest-first; the code is unique within a
/// tenant but free across tenants; `has_pin` reflects `set_pin` without ever exposing the hash on
/// a read (only `pin_phc` reads it back); an update renames/archives; and the grant is
/// SELECT/INSERT/UPDATE only — no DELETE (an employee is archived, never removed) ([ADR-0070]).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario over the real table: insert across two tenants, the \
              unique-code constraint, scoped + newest-first reads, the PIN hash round-trip and \
              has_pin flag, an update, and the append-only-ish grant — splitting it would \
              duplicate the multi-tenant setup"
)]
fn insert_scoped_pin_round_trip_and_grant() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        // Two tenants, both using the staff code "A01" — unique per tenant, not globally.
        people
            .insert("01EMP0000000000000000000A1", "tenant-a", "A01", "Alice")
            .await
            .expect("insert alice");
        people
            .insert("01EMP0000000000000000000A2", "tenant-a", "A02", "Anh")
            .await
            .expect("insert anh");
        people
            .insert("01EMP0000000000000000000B1", "tenant-b", "A01", "Bao")
            .await
            .expect("a duplicate code is fine under a different tenant");

        // The unique index refuses a second "A01" within tenant-a, as a conflict rather than an
        // outage, and the refusal names the staff code without quoting it: a code identifies a
        // person.
        let refused = people
            .insert("01EMP0000000000000000000A3", "tenant-a", "A01", "Clone")
            .await
            .expect_err("staff codes are unique within a tenant");
        assert_eq!(refused.status(), ErrorStatus::AlreadyExists, "{refused}");
        assert!(
            refused.message().contains("staff code") && !refused.message().contains("A01"),
            "{refused}"
        );
        assert!(
            core::error::Error::source(&refused).is_none(),
            "the database's own error, which quotes the key, does not travel with it"
        );

        // Tenant-scoped, newest-first, and no PIN yet; the refused one wrote nothing.
        let scoped = people.fetch("tenant-a").await.expect("fetch tenant-a");
        assert_eq!(scoped.len(), 2, "only tenant-a's own employees");
        assert_eq!(
            scoped
                .iter()
                .find(|row| row.code == "A01")
                .map(|row| row.name.as_str()),
            Some("Alice"),
            "the code still belongs to the first"
        );
        assert_eq!(scoped.first().expect("a row").name, "Anh", "newest first");
        assert!(scoped.iter().all(|row| !row.has_pin), "no PIN set yet");

        // A different tenant sees only its own.
        let other = people.fetch("tenant-b").await.expect("fetch tenant-b");
        assert_eq!(other.len(), 1);
        assert_eq!(other.first().expect("a row").code, "A01");

        // Set a PIN hash: has_pin flips, and the hash round-trips only via pin_phc — fetch never
        // carries it.
        assert!(
            people
                .set_pin(
                    "tenant-a",
                    "01EMP0000000000000000000A1",
                    "argon2id$phc$alice"
                )
                .await
                .expect("set pin"),
            "the row was found"
        );
        let alice = people
            .fetch_one("tenant-a", "01EMP0000000000000000000A1")
            .await
            .expect("fetch one")
            .expect("present");
        assert!(alice.has_pin, "has_pin reflects the set PIN");
        assert_eq!(
            people
                .pin_phc("tenant-a", "01EMP0000000000000000000A1")
                .await
                .expect("pin_phc"),
            Some("argon2id$phc$alice".to_owned()),
            "the trusted path reads the stored hash back"
        );

        // A PIN set scoped to the wrong tenant matches no row.
        assert!(
            !people
                .set_pin("tenant-b", "01EMP0000000000000000000A1", "x")
                .await
                .expect("cross-tenant set"),
            "set_pin is tenant-scoped"
        );

        // Rename + archive, at the version the read handed out (ADR-0094).
        assert!(
            matches!(
                people
                    .set(
                        "tenant-a",
                        "01EMP0000000000000000000A1",
                        "Alice Nguyen",
                        "archived",
                        &alice.version,
                    )
                    .await
                    .expect("update"),
                RowUpdate::Updated(_)
            ),
            "the row changed"
        );
        let archived = people
            .fetch_one("tenant-a", "01EMP0000000000000000000A1")
            .await
            .expect("fetch one")
            .expect("present");
        assert_eq!(archived.name, "Alice Nguyen");
        assert_eq!(archived.status, "archived");

        // Append-only-ish grant: app_tenant may SELECT/INSERT/UPDATE but never DELETE.
        let can_delete: bool = admin
            .query_one(
                "SELECT has_table_privilege('app_tenant', 'employees', 'DELETE')",
                &[],
            )
            .await
            .expect("privilege check")
            .get(0);
        let can_update: bool = admin
            .query_one(
                "SELECT has_table_privilege('app_tenant', 'employees', 'UPDATE')",
                &[],
            )
            .await
            .expect("privilege check")
            .get(0);
        assert!(!can_delete, "employees is never DELETEd — archived instead");
        assert!(
            can_update,
            "app_tenant may UPDATE (rename / archive / set PIN)"
        );
    });
}

/// A page is a window onto the sequence the unpaged roster read returns: the pages partition it,
/// every page reports the tenant's headcount rather than its own size, a page past the end is
/// empty and still counts, and another tenant's staff are in neither ([ADR-0098](../../../../../docs/adr/0098-paged-admin-reads.md)).
///
/// The whole roster is inserted in one transaction on purpose. `created_at` defaults to `now()`,
/// which is *transaction* time, so those rows share one timestamp exactly — the case decision 9
/// is about, and the case a `created_at DESC` order without the primary-key tiebreaker cannot
/// partition. A CSV staff import is exactly this shape.
#[test]
fn the_roster_pages_partition_the_unpaged_read_even_when_a_batch_shares_one_timestamp() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        // Six of ours and one elsewhere, all in one transaction.
        let mut inserts = String::new();
        for index in 0..6 {
            write!(
                inserts,
                "INSERT INTO employees (id, tenant_id, code, name) \
                 VALUES ('01EMPPAGE000000000000000{index:02}', '{TENANT_A}', 'P{index:02}', \
                 'Person {index}');"
            )
            .expect("writing to a String cannot fail");
        }
        inserts.push_str(
            "INSERT INTO employees (id, tenant_id, code, name) \
             VALUES ('01EMPPAGE000000000000000FF', 'tenant-b', 'P00', 'Elsewhere');",
        );
        admin
            .batch_execute(&format!("BEGIN; {inserts} COMMIT;"))
            .await
            .expect("write the batch in one transaction");

        let distinct: i64 = admin
            .query_one(
                "SELECT count(DISTINCT created_at) FROM employees WHERE tenant_id = $1",
                &[&TENANT_A],
            )
            .await
            .expect("count the distinct timestamps")
            .get(0);
        assert_eq!(
            distinct, 1,
            "six rows in one transaction carry one created_at, not six close ones — \
             which is why the read's ORDER BY needs id as a tiebreaker"
        );

        let roster = people.fetch(TENANT_A).await.expect("the unpaged roster");
        assert_eq!(roster.len(), 6, "the unpaged read is unchanged by paging");

        let mut stitched = Vec::new();
        for offset in [0, 2, 4] {
            let (page, total) = people
                .fetch_page(TENANT_A, None, EmployeeOrder::Newest, false, 2, offset)
                .await
                .expect("page over the tied batch");
            assert_eq!(
                total, 6,
                "every page reports the tenant's headcount, not the window's size"
            );
            assert!(
                page.iter().all(|row| !row.has_pin),
                "a page carries whether a PIN exists, never the hash (ADR-0070)"
            );
            stitched.extend(page.into_iter().map(|row| row.id));
        }
        assert_eq!(
            stitched,
            roster.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
            "three pages of two partition the roster in the unpaged read's own order"
        );

        // A page past the end is empty and still counts the roster — not an error, not zero.
        let (beyond, beyond_total) = people
            .fetch_page(TENANT_A, None, EmployeeOrder::Newest, false, 10, 50)
            .await
            .expect("a page past the end still reads");
        assert!(beyond.is_empty());
        assert_eq!(beyond_total, 6);

        // The other tenant's page sees only its own row, and counts only its own.
        let (theirs, their_total) = people
            .fetch_page("tenant-b", None, EmployeeOrder::Newest, false, 10, 0)
            .await
            .expect("the other tenant's page");
        assert_eq!(their_total, 1, "a headcount is per tenant");
        assert_eq!(
            theirs.first().expect("a row").name,
            "Elsewhere",
            "and the page is too"
        );
    });
}

/// `?q=` narrows the roster page on the person's **name or staff code**, and narrows `total`
/// with it.
///
/// Both columns, because those are the two handles an operator has on someone: a name they were
/// told, or the code on a badge. The two matches here are deliberately *different people*, so a
/// predicate that dropped either column would fail rather than pass on the other's row.
///
/// This is the read an assign picker sits on, and having it is what lets the People screen's
/// table be paged at all ([ADR-0098](../../../../../docs/adr/0098-paged-admin-reads.md), B3-4).
#[test]
fn the_roster_search_narrows_the_page_and_its_total_on_name_or_code() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        for (id, code, name) in [
            ("01EMPFIND00000000000000A1", "C01", "Mai Anh"),
            ("01EMPFIND00000000000000A2", "C02", "Bao"),
            ("01EMPFIND00000000000000A3", "MAI99", "Linh"),
        ] {
            people
                .insert(id, TENANT_A, code, name)
                .await
                .expect("insert the employee");
        }

        let (matched, matched_total) = people
            .fetch_page(TENANT_A, Some("mai"), EmployeeOrder::Newest, false, 10, 0)
            .await
            .expect("the searched page");
        assert_eq!(
            matched_total, 2,
            "the total counts what matched, not the roster"
        );
        let mut codes: Vec<String> = matched.into_iter().map(|row| row.code).collect();
        codes.sort();
        assert_eq!(
            codes,
            vec!["C01".to_owned(), "MAI99".to_owned()],
            "one matched on name, the other on code, both case-insensitively"
        );

        // No match is an empty page and a zero total — and this zero is the true one, because
        // the empty-window fallback carries the same predicate the page did.
        let (none, none_total) = people
            .fetch_page(
                TENANT_A,
                Some("nobody"),
                EmployeeOrder::Newest,
                false,
                10,
                0,
            )
            .await
            .expect("a search that matches nothing");
        assert!(none.is_empty());
        assert_eq!(
            none_total, 0,
            "the fallback count is filtered, not the whole roster"
        );

        // `%` is a character in the needle, not a wildcard: the read uses `position`, not
        // `ILIKE`, so an operator searching for a literal percent gets a literal search rather
        // than every row.
        let (wild, wild_total) = people
            .fetch_page(TENANT_A, Some("%"), EmployeeOrder::Newest, false, 10, 0)
            .await
            .expect("a literal percent");
        assert!(wild.is_empty(), "no name or code contains a percent sign");
        assert_eq!(wild_total, 0);

        // An unsearched page is still the whole roster.
        let (all, all_total) = people
            .fetch_page(TENANT_A, None, EmployeeOrder::Newest, false, 10, 0)
            .await
            .expect("the unsearched page");
        assert_eq!(all.len(), 3);
        assert_eq!(all_total, 3);
        assert!(
            all.iter().all(|row| !row.has_pin),
            "a searched or unsearched page carries whether a PIN exists, never the hash"
        );

        drop(admin);
    });
}

/// `?sort=` reorders the page, every order is total, and `?order=desc` is the exact reverse.
///
/// The three orders are asserted against data whose three sequences differ, so no two can pass
/// for one another: a name order that quietly fell back to `created_at` would fail rather than
/// coincide. The `desc` case is the reverse of the `asc` one *element by element*, which is what
/// the flipped tiebreaker in [`employee_order`] buys — without it, reversing only the leading
/// column would give a different total order that happens to share a first row.
#[test]
fn the_roster_page_orders_by_name_or_code_and_reverses_exactly() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        // Inserted in an order matching neither sort, so insertion order cannot pass for either.
        // Two people share the name "Bao" — the ordinary case a staff code exists to
        // disambiguate — so the `id` tiebreaker actually fires and the reversal has something to
        // get wrong.
        for (id, code, name) in [
            ("01EMPSORT00000000000000A1", "C03", "Bao"),
            ("01EMPSORT00000000000000A2", "C01", "Mai"),
            ("01EMPSORT00000000000000A3", "C02", "An"),
            ("01EMPSORT00000000000000A4", "C04", "Bao"),
        ] {
            people
                .insert(id, TENANT_A, code, name)
                .await
                .expect("insert the employee");
        }

        // Codes rather than names, because two rows share a name: only the code distinguishes
        // them, so only a code-level assertion can see the tiebreaker do its job.
        let codes = |rows: Vec<store_postgres::EmployeeRow>| {
            rows.into_iter().map(|row| row.code).collect::<Vec<_>>()
        };

        // "Bao"/C03 before "Bao"/C04, because the tiebreaker is the id and A1 precedes A4.
        let ascending = vec![
            "C02".to_owned(),
            "C03".to_owned(),
            "C04".to_owned(),
            "C01".to_owned(),
        ];
        let (by_name, _) = people
            .fetch_page(TENANT_A, None, EmployeeOrder::Name, false, 10, 0)
            .await
            .expect("by name");
        assert_eq!(codes(by_name), ascending);

        let (by_name_desc, _) = people
            .fetch_page(TENANT_A, None, EmployeeOrder::Name, true, 10, 0)
            .await
            .expect("by name, reversed");
        let mut reversed = ascending.clone();
        reversed.reverse();
        assert_eq!(
            codes(by_name_desc),
            reversed,
            "reversed is the exact reverse, element by element — which needs the tiebreaker to \
             flip with the direction, not only the leading column"
        );

        let (by_code, _) = people
            .fetch_page(TENANT_A, None, EmployeeOrder::Code, false, 10, 0)
            .await
            .expect("by code");
        assert_eq!(
            codes(by_code),
            vec![
                "C01".to_owned(),
                "C02".to_owned(),
                "C03".to_owned(),
                "C04".to_owned()
            ],
            "a different sequence from the name order, so one cannot pass for the other"
        );

        // A sorted page still partitions the set: four pages of one, no repeat and no gap —
        // across the shared name, which is where a non-total order would drop or double a row.
        let mut stitched = Vec::new();
        for offset in [0, 1, 2, 3] {
            let (page, total) = people
                .fetch_page(TENANT_A, None, EmployeeOrder::Name, false, 1, offset)
                .await
                .expect("a page of the name order");
            assert_eq!(total, 4, "the total is the set, whatever the order");
            stitched.extend(codes(page));
        }
        assert_eq!(
            stitched, ascending,
            "windows over the name order stitch back into it"
        );

        // The search composes with the order rather than replacing it.
        let (searched, searched_total) = people
            .fetch_page(TENANT_A, Some("bao"), EmployeeOrder::Name, false, 10, 0)
            .await
            .expect("searched and sorted");
        assert_eq!(searched_total, 2, "both people named Bao");
        assert_eq!(
            codes(searched),
            vec!["C03".to_owned(), "C04".to_owned()],
            "and the tiebreaker orders them inside the search too"
        );

        drop(admin);
    });
}

/// The name order rides `employees_by_tenant_name` (migration 0046) rather than sorting.
///
/// The same reasoning as the default order's plan test, applied to the order a query parameter
/// introduced: `?sort=name` that fell back to a sort of the tenant's whole roster would
/// reintroduce, through a parameter, exactly the cost migration 0045 exists to avoid.
#[test]
fn the_name_ordered_roster_page_is_served_by_its_own_index() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();
        for index in 0..30_i32 {
            people
                .insert(
                    &format!("01EMPNAME00000000000000{index:03}"),
                    TENANT_A,
                    &format!("Q{index:03}"),
                    &format!("Name {index:03}"),
                )
                .await
                .expect("insert");
        }
        admin
            .batch_execute("ANALYZE employees")
            .await
            .expect("analyze");

        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT id FROM employees WHERE tenant_id = $1 \
                     ORDER BY name ASC, id ASC LIMIT $2 OFFSET $3",
                    &[&TENANT_A, &10_i64, &0_i64],
                )
                .await
                .expect("explain");
            admin
                .batch_execute("RESET enable_seqscan")
                .await
                .expect("restore");
            rows.iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            plan.contains("employees_by_tenant_name"),
            "the name order walks its own index: {plan}"
        );
        assert!(
            !plan.contains("Sort"),
            "and walking it *is* the sort, so no Sort node sits above the scan: {plan}"
        );

        drop(admin);
    });
}

/// The index migration 0045 added covers this query's `ORDER BY`, so `LIMIT` stops the scan
/// instead of truncating a completed sort of the tenant's whole roster.
///
/// Asserted through `EXPLAIN` rather than by timing: on 30 rows a timing test would pass either
/// way. What this catches is the migration being dropped, or `EMPLOYEE_ORDER` drifting away from
/// the index built for it — including the `id` tiebreaker, whose absence from the index is what
/// would put a `Sort` node back above the scan.
#[test]
fn the_paged_roster_query_is_served_by_the_index_and_not_by_a_sort() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();
        for index in 0..30 {
            people
                .insert(
                    &format!("01EMPSCAN00000000000000{index:03}"),
                    TENANT_A,
                    &format!("Q{index:03}"),
                    "Planned",
                )
                .await
                .expect("insert");
        }
        // Without statistics the planner picks a sequential scan on a table this small whatever
        // indexes exist, so the assertion would be about row count rather than about the index.
        admin
            .batch_execute("ANALYZE employees")
            .await
            .expect("analyze");

        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT id FROM employees WHERE tenant_id = $1 \
                     ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3",
                    &[&TENANT_A, &10_i64, &0_i64],
                )
                .await
                .expect("explain");
            admin
                .batch_execute("RESET enable_seqscan")
                .await
                .expect("restore");
            rows.iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            plan.contains("employees_by_tenant_newest"),
            "the page should be served by the sort-carrying index; plan was:\n{plan}"
        );
        assert!(
            !plan.contains("Sort Key"),
            "an index that carries the order needs no sort step; plan was:\n{plan}"
        );
    });
}
