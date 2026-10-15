// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The activation-code and device-credential store (ADR-0050).

use super::{block_on, prepared};

const TENANT: &str = "TENANT000000000000000000AA";
const STORE: &str = "STORE0000000000000000000BB";

/// Issue reads back as `issued`; the exchange consumes it and mints a credential atomically; a
/// replay is refused (single-use); and revoke cancels a slot's still-issued code.
#[test]
fn issue_redeem_replay_and_revoke_round_trip() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let codes = store.activation_codes();
        let hash: &[u8] = &[7_u8; 32];
        let device = "DEVICE000000000000000000CC";

        codes
            .issue(hash, TENANT, STORE, device)
            .await
            .expect("issue the code");

        let row = codes
            .lookup(hash)
            .await
            .expect("lookup")
            .expect("the issued code is present");
        assert_eq!(row.status, "issued");
        assert_eq!(row.tenant_id, TENANT);
        assert_eq!(row.device_id, device);

        // The first redemption wins: the code is consumed and the credential provisioned together.
        let secret_hash: &[u8] = &[9_u8; 32];
        assert!(
            codes
                .consume_and_provision(hash, "CRED00000000000000000000DD", secret_hash)
                .await
                .expect("consume"),
            "the issued code is redeemed"
        );

        // It now reads redeemed, and a second attempt changes nothing — single-use.
        let row = codes
            .lookup(hash)
            .await
            .expect("lookup")
            .expect("still present");
        assert_eq!(row.status, "redeemed");
        assert!(
            !codes
                .consume_and_provision(hash, "CRED00000000000000000000EE", secret_hash)
                .await
                .expect("second consume"),
            "a spent code cannot be redeemed twice"
        );

        // The credential landed for the code's slot, exactly once (the atomic mint, not two).
        let minted: i64 = admin
            .query_one(
                "SELECT count(*) FROM device_credentials WHERE tenant_id = $1 AND device_id = $2",
                &[&TENANT, &device],
            )
            .await
            .expect("count credentials")
            .get(0);
        assert_eq!(minted, 1, "exactly one credential was minted");

        // Revoke cancels a still-issued code for a slot and is idempotent.
        let other_device = "DEVICE000000000000000000FF";
        codes
            .issue(&[1_u8; 32], TENANT, STORE, other_device)
            .await
            .expect("issue a second code");
        assert_eq!(
            codes
                .revoke_slot(TENANT, STORE, other_device)
                .await
                .expect("revoke"),
            1,
            "the one issued code is cancelled"
        );
        assert_eq!(
            codes
                .revoke_slot(TENANT, STORE, other_device)
                .await
                .expect("revoke again"),
            0,
            "nothing is left to revoke"
        );
        let revoked = codes
            .lookup(&[1_u8; 32])
            .await
            .expect("lookup")
            .expect("present");
        assert_eq!(revoked.status, "revoked");
    });
}

/// A minted credential reads back for its slot; archiving its device marks it archived, which
/// the cloud refuses (ADR-0143); a credential with no registry row is not archived.
#[test]
fn a_device_credential_reads_back_and_its_device_s_archive_ends_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let codes = store.activation_codes();
        let keys = store.api_keys();
        let device = "DEVICE000000000000000000C1";
        let credential = "CRED00000000000000000000C1";
        codes
            .issue(&[3_u8; 32], TENANT, STORE, device)
            .await
            .expect("issue the code");
        assert!(
            codes
                .consume_and_provision(&[3_u8; 32], credential, &[4_u8; 32])
                .await
                .expect("consume")
        );

        let row = keys
            .fetch_device_credential(credential)
            .await
            .expect("fetch")
            .expect("the minted credential is present");
        assert_eq!(row.tenant_id, TENANT);
        assert_eq!(row.store_id, STORE);
        assert_eq!(row.secret_hash, vec![4_u8; 32]);
        assert!(
            !row.archived,
            "no registry row: activation does not need one"
        );

        let registry = store.registry();
        let version = registry
            .insert_device(device, TENANT, STORE, "Till 1", "edge")
            .await
            .expect("register the device");
        let live = keys
            .fetch_device_credential(credential)
            .await
            .expect("fetch")
            .expect("present");
        assert!(!live.archived, "an active device's credential is live");

        registry
            .set_device(TENANT, device, "Till 1", "edge", "archived", &version)
            .await
            .expect("archive the device");
        let archived = keys
            .fetch_device_credential(credential)
            .await
            .expect("fetch")
            .expect("still present");
        assert!(
            archived.archived,
            "archiving the device ends its credential"
        );

        assert!(
            keys.fetch_device_credential("CRED0000000000000000000000")
                .await
                .expect("fetch")
                .is_none(),
            "an unknown credential is absent"
        );
    });
}
