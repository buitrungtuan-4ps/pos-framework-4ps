// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The subject store: retention / PII masking (ADR-0035).

use super::{block_on, prepared};

/// A due row is fetched, masking scrubs it and stamps `masked_at`, and a masked row is neither
/// re-fetched nor re-masked (the sweep's idempotence at the database).
#[test]
fn fetch_due_then_mask_is_idempotent() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let subjects = store.subjects();

        // Seed one unmasked subject collected at t=1000ms with obviously-fake placeholder PII.
        let id: &str = "SUBJECT0000000000000000AA";
        let tenant: &str = "TENANT000000000000000000AA";
        let collected: i64 = 1000;
        let fields: &str = r#"{"name":"name-placeholder","phone":"phone-placeholder"}"#;
        admin
            .execute(
                // `$4::text::jsonb`, not `$4::jsonb`: the inner `::text` pins the bound parameter's
                // inferred type to `text` (which `&str` serialises to) before the database casts it
                // to `jsonb`. `$4::jsonb` alone makes Postgres infer the parameter itself as `jsonb`,
                // which tokio-postgres rejects for a `&str` (WrongType). Same pattern the production
                // writers use — subjects.rs, config_trees.rs, translations.rs, rollups.rs.
                "INSERT INTO subjects (subject_id, tenant_id, collected_at, fields) \
                 VALUES ($1, $2, $3, $4::text::jsonb)",
                &[&id, &tenant, &collected, &fields],
            )
            .await
            .expect("seed a subject");

        // Not yet due before its collection instant; due at or after it.
        assert!(
            subjects
                .fetch_due(999, 100)
                .await
                .expect("fetch")
                .is_empty(),
            "a record is not due before its collection instant"
        );
        let due = subjects.fetch_due(2000, 100).await.expect("fetch");
        assert_eq!(due.len(), 1);
        assert_eq!(due.first().expect("one row").subject_id, id);

        // Mask it: redact the values in place and stamp masked_at.
        let redacted: &str = r#"{"name":"[REDACTED]","phone":"[REDACTED]"}"#;
        assert!(
            subjects.mask(id, redacted, 5000).await.expect("mask"),
            "an unmasked row is masked"
        );

        // A masked row is neither returned by a sweep nor masked a second time.
        assert!(
            subjects
                .fetch_due(9999, 100)
                .await
                .expect("fetch")
                .is_empty(),
            "masked rows are excluded, which is what makes the sweep idempotent"
        );
        assert!(
            !subjects.mask(id, redacted, 6000).await.expect("re-mask"),
            "an already-masked row is not re-masked"
        );
    });
}

/// `fetch_one` is the subject-request tooling's read (ADR-0076): it returns one row scoped to its
/// tenant, reports the real `masked_at`, and returns `None` for a wrong tenant or unknown id.
#[test]
fn fetch_one_is_tenant_scoped_and_reports_masked_at() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let subjects = store.subjects();

        let id: &str = "SUBJECT0000000000000000BB";
        let tenant: &str = "TENANT000000000000000000AA";
        let other_tenant: &str = "TENANT000000000000000000BB";
        let fields: &str = r#"{"name":"name-placeholder"}"#;
        admin
            .execute(
                "INSERT INTO subjects (subject_id, tenant_id, collected_at, fields) \
                 VALUES ($1, $2, $3, $4::text::jsonb)",
                &[&id, &tenant, &1000_i64, &fields],
            )
            .await
            .expect("seed a subject");

        // The owning tenant sees it, unmasked (masked_at is None).
        let row = subjects
            .fetch_one(id, tenant)
            .await
            .expect("fetch")
            .expect("the subject exists for its tenant");
        assert_eq!(row.subject_id, id);
        assert_eq!(row.masked_at_ms, None);

        // A different tenant cannot reach it, and an unknown id is None.
        assert!(
            subjects
                .fetch_one(id, other_tenant)
                .await
                .expect("fetch")
                .is_none(),
            "a subject is not visible to another tenant"
        );
        assert!(
            subjects
                .fetch_one("SUBJECT0000000000000000ZZ", tenant)
                .await
                .expect("fetch")
                .is_none()
        );

        // After masking, fetch_one reports the masked_at stamp.
        let redacted: &str = r#"{"name":"[REDACTED]"}"#;
        assert!(subjects.mask(id, redacted, 7000).await.expect("mask"));
        let masked = subjects
            .fetch_one(id, tenant)
            .await
            .expect("fetch")
            .expect("still present after masking");
        assert_eq!(masked.masked_at_ms, Some(7000));
    });
}
