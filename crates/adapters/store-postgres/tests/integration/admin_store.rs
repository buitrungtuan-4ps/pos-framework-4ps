// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The super-admin credential and session store (ADR-0034).

use super::{block_on, prepared};

/// Seeds the single super-admin row with obviously-fake credentials — never real key material.
async fn seed_admin(admin: &tokio_postgres::Client) {
    let phc: &str = "$argon2id$v=19$m=19456,t=2,p=1$ZmFrZXNhbHQ$ZmFrZWhhc2h2YWx1ZQ";
    let secret: &[u8] = b"fake-totp-seed-not-real";
    admin
        .execute(
            "INSERT INTO super_admin (id, password_phc, totp_secret, last_used_totp_step) \
             VALUES (true, $1, $2, NULL)",
            &[&phc, &secret],
        )
        .await
        .expect("seed the super-admin row");
}

/// The credential reads back exactly as stored, and the TOTP step advances only forward.
#[test]
fn the_credential_round_trips_and_the_step_never_moves_backwards() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let credentials = store.admin();
        assert!(
            credentials
                .fetch_credential()
                .await
                .expect("fetch")
                .is_none(),
            "no credential before provisioning"
        );

        seed_admin(&admin).await;
        let row = credentials
            .fetch_credential()
            .await
            .expect("fetch")
            .expect("provisioned");
        assert_eq!(
            row.password_phc,
            "$argon2id$v=19$m=19456,t=2,p=1$ZmFrZXNhbHQ$ZmFrZWhhc2h2YWx1ZQ"
        );
        assert_eq!(row.totp_secret, b"fake-totp-seed-not-real");
        assert_eq!(
            row.last_used_totp_step, None,
            "unused until the first login"
        );

        credentials.advance_totp_step(100).await.expect("advance");
        // A lower step must not move it backwards — the guard is a no-op here.
        credentials
            .advance_totp_step(50)
            .await
            .expect("attempt lower");
        let row = credentials
            .fetch_credential()
            .await
            .expect("fetch")
            .expect("provisioned");
        assert_eq!(
            row.last_used_totp_step,
            Some(100),
            "the step advanced to 100 and a lower write did not move it back"
        );
    });
}

/// A session is live only up to its expiry, and revoking it removes it.
#[test]
fn a_session_is_live_until_its_expiry_and_gone_after_revoke() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let sessions = store.admin();
        let hash = [7_u8; 32];
        // A legacy fixed-TTL session (no absolute cap / idle window): it does not slide.
        sessions
            .insert_session(store_postgres::NewSessionRow {
                token_hash: &hash,
                created_at_ms: 1000,
                expires_at_ms: 2000,
                absolute_expires_at_ms: None,
                idle_ttl_ms: None,
                admin_id: None,
                ip: None,
                user_agent: None,
            })
            .await
            .expect("insert the session");

        assert!(
            sessions.session_valid(&hash, 1999).await.expect("query"),
            "live one millisecond before expiry"
        );
        assert!(
            !sessions.session_valid(&hash, 2000).await.expect("query"),
            "not live at the expiry instant (the check is strictly greater-than)"
        );
        assert!(
            !sessions
                .session_valid(&[9_u8; 32], 1999)
                .await
                .expect("query"),
            "an unknown token names no session"
        );

        sessions
            .delete_session(&hash)
            .await
            .expect("revoke the session");
        assert!(
            !sessions.session_valid(&hash, 1999).await.expect("query"),
            "a revoked session is gone even before its expiry"
        );
    });
}

/// A modern session slides its idle TTL forward on a real request, but never past its absolute
/// cap ([ADR-0067] slice 4).
#[test]
fn a_modern_session_slides_up_to_its_absolute_cap() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let sessions = store.admin();
        sessions
            .insert_admin_user(
                "adm-slide",
                "slide@x.test",
                "S",
                "owner",
                "active",
                "$phc",
                b"s",
            )
            .await
            .expect("seed admin");
        let hash = [11_u8; 32];
        // Idle window 60_000 ms; absolute cap at 10_000_000.
        sessions
            .insert_session(store_postgres::NewSessionRow {
                token_hash: &hash,
                created_at_ms: 0,
                expires_at_ms: 60_000,
                absolute_expires_at_ms: Some(10_000_000),
                idle_ttl_ms: Some(60_000),
                admin_id: Some("adm-slide"),
                ip: None,
                user_agent: None,
            })
            .await
            .expect("insert the session");

        // A real request at 50_000 ms slides the expiry to 50_000 + 60_000 = 110_000.
        assert_eq!(
            sessions
                .fetch_session_admin(&hash, 50_000)
                .await
                .expect("query"),
            Some(Some("adm-slide".to_owned())),
        );
        assert!(
            sessions.session_valid(&hash, 100_000).await.expect("query"),
            "the slid session outlives its original 60_000 ms boundary"
        );

        // Sliding right before the cap cannot push the expiry past it.
        sessions
            .fetch_session_admin(&hash, 9_995_000)
            .await
            .expect("query");
        assert!(
            !sessions
                .session_valid(&hash, 10_000_001)
                .await
                .expect("query"),
            "no amount of sliding lets a session outlive its absolute cap"
        );
    });
}

/// An admin lists and revokes only their own sessions; "revoke others" keeps the current one.
#[test]
fn admin_sessions_list_and_revoke_are_scoped_to_the_owner() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let sessions = store.admin();
        for (id, email) in [("own-1", "o1@x.test"), ("own-2", "o2@x.test")] {
            sessions
                .insert_admin_user(id, email, "N", "admin", "active", "$phc", b"s")
                .await
                .expect("seed admin");
        }
        let insert = |token: &'static [u8; 32], created: i64, admin: &'static str| {
            let sessions = &sessions;
            async move {
                sessions
                    .insert_session(store_postgres::NewSessionRow {
                        token_hash: token,
                        created_at_ms: created,
                        expires_at_ms: 1_000_000,
                        absolute_expires_at_ms: Some(9_000_000),
                        idle_ttl_ms: Some(1_000_000),
                        admin_id: Some(admin),
                        ip: None,
                        user_agent: None,
                    })
                    .await
                    .expect("insert session");
            }
        };
        let (current, other, theirs) = (&[21_u8; 32], &[22_u8; 32], &[23_u8; 32]);
        insert(current, 10, "own-1").await;
        insert(other, 20, "own-1").await;
        insert(theirs, 30, "own-2").await;

        // Listing is scoped and newest-first, and created_at round-trips to ms.
        let mine = sessions
            .list_admin_sessions("own-1", 100)
            .await
            .expect("list");
        assert_eq!(mine.len(), 2, "own-1 sees only their own sessions");
        assert_eq!(
            mine.first().expect("a first session").created_at_ms,
            20,
            "newest first"
        );
        assert_eq!(mine.get(1).expect("a second session").created_at_ms, 10);

        // Revocation is scoped: own-1 cannot revoke own-2's session.
        assert!(
            !sessions
                .delete_admin_session("own-1", theirs)
                .await
                .expect("revoke"),
            "an admin cannot revoke another's session"
        );
        // "Revoke others" keeps the current session and drops own-1's other one.
        assert_eq!(
            sessions
                .delete_other_admin_sessions("own-1", current)
                .await
                .expect("revoke others"),
            1,
        );
        let remaining = sessions
            .list_admin_sessions("own-1", 100)
            .await
            .expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining.first().expect("one remaining").token_hash,
            current.to_vec(),
            "the current session survives"
        );
        assert_eq!(
            sessions
                .list_admin_sessions("own-2", 100)
                .await
                .expect("list")
                .len(),
            1,
            "the other admin's session is untouched"
        );
    });
}

/// TOTP re-enrolment replaces the secret and resets the last-used step ([ADR-0067] slice 6).
#[test]
fn totp_re_enrolment_replaces_the_secret_and_resets_the_step() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let admin = store.admin();
        admin
            .insert_credential("$argon2id$phc", b"old-secret-value")
            .await
            .expect("provision");
        admin.advance_totp_step(42).await.expect("advance the step");

        admin
            .rotate_totp_secret(b"a-freshly-enrolled-secret")
            .await
            .expect("rotate");
        let row = admin
            .fetch_credential()
            .await
            .expect("fetch")
            .expect("a credential is present");
        assert_eq!(row.totp_secret, b"a-freshly-enrolled-secret".to_vec());
        assert_eq!(
            row.last_used_totp_step, None,
            "a fresh secret resets the last-used step"
        );
    });
}

/// Seeds the two admins the [ADR-0119] tests read: an active owner whose address is
/// deliberately mixed-case, and a suspended ops admin with a different credential.
///
/// [ADR-0119]: ../../../../docs/adr/0119-each-admin-signs-in-as-themselves.md
async fn seed_two_logins(admin: &store_postgres::PostgresAdmin) {
    admin
        .insert_admin_user(
            "log-1",
            "First@Example.test",
            "First",
            "owner",
            "active",
            "$argon2id$first",
            b"first-secret",
        )
        .await
        .expect("seed the first admin");
    admin
        .insert_admin_user(
            "log-2",
            "second@example.test",
            "Second",
            "ops",
            "suspended",
            "$argon2id$second",
            b"second-secret",
        )
        .await
        .expect("seed the second admin");
}

/// Sign-in reads *this* admin's own credential, by email
/// ([ADR-0119](../../../../../docs/adr/0119-each-admin-signs-in-as-themselves.md)).
///
/// Against the real database deliberately: `password_phc`, `totp_secret` and
/// `last_used_totp_step` have existed on `admin_users` since migration 0018 and nothing has
/// ever read them, so the first read of those columns happens here rather than in production.
#[test]
fn a_login_reads_one_admins_own_credential_by_email() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let admin = store.admin();
        seed_two_logins(&admin).await;

        // Case-insensitive, as the `lower(email)` unique index enforces — an operator typing
        // their address capitalised must not be refused.
        let found = admin
            .fetch_admin_login_by_email("first@EXAMPLE.TEST")
            .await
            .expect("fetch")
            .expect("the admin is present");
        assert_eq!(found.id, "log-1");
        assert_eq!(
            found.email, "First@Example.test",
            "the stored casing is returned"
        );
        assert_eq!(found.role, "owner");
        assert_eq!(found.status, "active");
        assert_eq!(found.password_phc, "$argon2id$first");
        assert_eq!(found.totp_secret, b"first-secret".to_vec());
        assert_eq!(
            found.last_used_totp_step, None,
            "an admin who has never signed in has no step"
        );

        // A suspended admin still reads — refusing them is the caller's decision, so the read
        // reports the status rather than hiding the row.
        let suspended = admin
            .fetch_admin_login_by_email("second@example.test")
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(suspended.status, "suspended");
        assert_eq!(suspended.password_phc, "$argon2id$second");

        assert!(
            admin
                .fetch_admin_login_by_email("nobody@example.test")
                .await
                .expect("fetch")
                .is_none(),
            "an absent address is Ok(None), not an error"
        );
    });
}

/// The TOTP step is per admin and moves only forward
/// ([ADR-0119](../../../../../docs/adr/0119-each-admin-signs-in-as-themselves.md) §5).
///
/// Burning it globally — which is what the single-row `super_admin` column did — would refuse a
/// second admin's perfectly valid code in the same 30-second window as a replay.
#[test]
fn the_totp_step_is_per_admin_and_moves_only_forward() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let admin = store.admin();
        seed_two_logins(&admin).await;

        admin
            .advance_admin_totp_step("log-1", 57_000_000)
            .await
            .expect("advance");
        admin
            .advance_admin_totp_step("log-1", 56_999_999)
            .await
            .expect("a backwards step is a no-op, not an error");
        assert_eq!(
            admin
                .fetch_admin_login_by_email("first@example.test")
                .await
                .expect("fetch")
                .expect("present")
                .last_used_totp_step,
            Some(57_000_000),
            "the step only moves forward"
        );
        assert_eq!(
            admin
                .fetch_admin_login_by_email("second@example.test")
                .await
                .expect("fetch")
                .expect("present")
                .last_used_totp_step,
            None,
            "the other admin's step is untouched"
        );
        // The write addresses a row by id, so an unknown id is a silent no-op rather than an
        // error — the same posture as every other `WHERE id = $1` write on this seam.
        admin
            .advance_admin_totp_step("no-such-admin", 1)
            .await
            .expect("an unknown admin is a no-op");
    });
}

/// Re-enrolment replaces one admin's secret and resets their step, and nobody else's
/// ([ADR-0119](../../../../../docs/adr/0119-each-admin-signs-in-as-themselves.md) §6).
#[test]
fn re_enrolment_replaces_one_admins_secret_and_nobody_elses() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let admin = store.admin();
        seed_two_logins(&admin).await;
        admin
            .advance_admin_totp_step("log-1", 57_000_000)
            .await
            .expect("advance");

        admin
            .rotate_admin_totp_secret("log-1", b"re-enrolled-secret")
            .await
            .expect("rotate");
        let rotated = admin
            .fetch_admin_login_by_email("first@example.test")
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(rotated.totp_secret, b"re-enrolled-secret".to_vec());
        assert_eq!(
            rotated.last_used_totp_step, None,
            "a fresh secret verifies from step zero"
        );
        assert_eq!(
            admin
                .fetch_admin_login_by_email("second@example.test")
                .await
                .expect("fetch")
                .expect("present")
                .totp_secret,
            b"second-secret".to_vec(),
            "the other admin's secret is untouched"
        );
        admin
            .rotate_admin_totp_secret("no-such-admin", b"x")
            .await
            .expect("an unknown admin is a no-op");
    });
}

/// Recovery codes store, count, burn single-use, and regenerate — scoped to the admin
/// ([ADR-0067] slice 6).
#[test]
fn recovery_codes_store_consume_count_and_regenerate() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let admin = store.admin();
        admin
            .insert_admin_user("rec-1", "rec@x.test", "R", "owner", "active", "$phc", b"s")
            .await
            .expect("seed admin");
        let (h1, h2, h3) = (vec![1_u8; 32], vec![2_u8; 32], vec![3_u8; 32]);
        admin
            .replace_recovery_codes(
                "rec-1",
                &[("c1".to_owned(), h1.clone()), ("c2".to_owned(), h2.clone())],
            )
            .await
            .expect("store");
        assert_eq!(admin.count_recovery_codes("rec-1").await.expect("count"), 2);

        // Single-use: the first match spends the code, a replay matches nothing.
        assert!(
            admin
                .consume_recovery_code("rec-1", &h1, 1000)
                .await
                .expect("consume")
        );
        assert!(
            !admin
                .consume_recovery_code("rec-1", &h1, 1000)
                .await
                .expect("consume"),
            "a spent code cannot be reused"
        );
        assert_eq!(admin.count_recovery_codes("rec-1").await.expect("count"), 1);

        // Regenerating replaces the set: the previous unused code is gone, the new one works.
        admin
            .replace_recovery_codes("rec-1", &[("c3".to_owned(), h3.clone())])
            .await
            .expect("regenerate");
        assert_eq!(admin.count_recovery_codes("rec-1").await.expect("count"), 1);
        assert!(
            !admin
                .consume_recovery_code("rec-1", &h2, 1000)
                .await
                .expect("consume"),
            "a code from the replaced set no longer matches"
        );
        assert!(
            admin
                .consume_recovery_code("rec-1", &h3, 1000)
                .await
                .expect("consume")
        );
    });
}
