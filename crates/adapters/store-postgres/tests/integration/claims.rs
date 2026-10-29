// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The device-claim store (ADR-0148).

use super::{block_on, prepared};
use store_postgres::{ClaimBindRow, ClaimCollectRow, ClaimSlotRow};

/// 2026-01-01T00:00:00Z, the instant every case reasons about.
const NOW: i64 = 1_767_225_600_000;
const HOUR: i64 = 3_600_000;

fn slot() -> ClaimSlotRow {
    ClaimSlotRow {
        tenant_id: "TENANT000000000000000000AA".to_owned(),
        store_id: "STORE0000000000000000000BB".to_owned(),
        device_id: "DEVICE000000000000000000D1".to_owned(),
    }
}

/// A claim waits, binds once, and is collected once with its own secret, which writes the device
/// credential for the bound slot in the same transaction.
#[test]
fn a_claim_binds_once_and_collects_once_with_its_secret() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let claims = store.claims();
        let claim = "CLAIM0000000000000000000A1";
        let credential = "CRED00000000000000000000A1";
        claims
            .open(claim, &[1_u8; 32], &[2_u8; 32], NOW + HOUR)
            .await
            .expect("open");

        let early = claims
            .collect(claim, &[2_u8; 32], credential, &[3_u8; 32], NOW)
            .await
            .expect("collect");
        assert_eq!(early, ClaimCollectRow::Pending, "nobody has bound it yet");
        let forged = claims
            .collect(claim, &[9_u8; 32], credential, &[3_u8; 32], NOW)
            .await
            .expect("collect");
        assert_eq!(
            forged,
            ClaimCollectRow::Refused,
            "a wrong secret is refused"
        );

        assert_eq!(
            claims.bind(&[7_u8; 32], &slot(), NOW).await.expect("bind"),
            ClaimBindRow::Unknown
        );
        assert_eq!(
            claims.bind(&[1_u8; 32], &slot(), NOW).await.expect("bind"),
            ClaimBindRow::Bound
        );
        assert_eq!(
            claims.bind(&[1_u8; 32], &slot(), NOW).await.expect("bind"),
            ClaimBindRow::AlreadyBound,
            "a code binds once"
        );

        let collected = claims
            .collect(claim, &[2_u8; 32], credential, &[3_u8; 32], NOW)
            .await
            .expect("collect");
        assert_eq!(collected, ClaimCollectRow::Collected(slot()));
        let minted = admin
            .query_one(
                "SELECT tenant_id, store_id, device_id, secret_hash FROM device_credentials \
                 WHERE id = $1",
                &[&credential],
            )
            .await
            .expect("the credential row");
        assert_eq!(minted.get::<_, String>(0), slot().tenant_id);
        assert_eq!(minted.get::<_, String>(1), slot().store_id);
        assert_eq!(minted.get::<_, String>(2), slot().device_id);
        assert_eq!(minted.get::<_, Vec<u8>>(3), vec![3_u8; 32]);

        let again = claims
            .collect(
                claim,
                &[2_u8; 32],
                "CRED00000000000000000000A2",
                &[4_u8; 32],
                NOW,
            )
            .await
            .expect("collect");
        assert_eq!(again, ClaimCollectRow::Refused, "collected once");
        let credentials: i64 = admin
            .query_one("SELECT count(*) FROM device_credentials", &[])
            .await
            .expect("count")
            .get(0);
        assert_eq!(credentials, 1, "the second collection minted nothing");
    });
}

/// Past its hour a claim can be neither bound nor collected, even one bound in time.
#[test]
fn an_expired_claim_is_neither_bound_nor_collected() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let claims = store.claims();
        claims
            .open(
                "CLAIM0000000000000000000B1",
                &[4_u8; 32],
                &[5_u8; 32],
                NOW + 1_000,
            )
            .await
            .expect("open");
        assert_eq!(
            claims
                .bind(&[4_u8; 32], &slot(), NOW + 1_000)
                .await
                .expect("bind"),
            ClaimBindRow::Expired
        );
        assert_eq!(
            claims
                .collect(
                    "CLAIM0000000000000000000B1",
                    &[5_u8; 32],
                    "CRED0000000000000000000B1",
                    &[1_u8; 32],
                    NOW + 1_000
                )
                .await
                .expect("collect"),
            ClaimCollectRow::Expired
        );

        claims
            .open(
                "CLAIM0000000000000000000B2",
                &[6_u8; 32],
                &[8_u8; 32],
                NOW + 1_000,
            )
            .await
            .expect("open");
        assert_eq!(
            claims.bind(&[6_u8; 32], &slot(), NOW).await.expect("bind"),
            ClaimBindRow::Bound
        );
        assert_eq!(
            claims
                .collect(
                    "CLAIM0000000000000000000B2",
                    &[8_u8; 32],
                    "CRED0000000000000000000B2",
                    &[1_u8; 32],
                    NOW + 2_000
                )
                .await
                .expect("collect"),
            ClaimCollectRow::Expired,
            "bound in time, collected too late"
        );
    });
}

/// One code names one claim: a second claim drawing the same code fails, and the box asks again.
#[test]
fn a_code_names_one_claim() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let claims = store.claims();
        claims
            .open(
                "CLAIM0000000000000000000C1",
                &[1_u8; 32],
                &[2_u8; 32],
                NOW + HOUR,
            )
            .await
            .expect("open");
        assert!(
            claims
                .open(
                    "CLAIM0000000000000000000C2",
                    &[1_u8; 32],
                    &[3_u8; 32],
                    NOW + HOUR
                )
                .await
                .is_err()
        );
    });
}
