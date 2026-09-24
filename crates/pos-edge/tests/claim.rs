// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A box installed with no store claims itself
//! ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)).
//!
//! The claim loop runs against a cloud scripted to answer as the real routes do: a first code that
//! expires unused, a second that waits and is then bound. What must come out is exactly one thing:
//! the credential in the vault and a `config.toml` that loads as the claimed store.

use core::time::Duration;
use std::sync::Mutex;

use cloud_sync_http::{HttpClaim, HttpResponse, HttpTransport, TransportError};
use pos_edge::EdgeConfig;
use pos_edge::claim::{ClaimStatus, claim};
use pos_fakes::FakeKeyVault;
use pos_ports::key_vault::{KeyVault, SecretName};

const STORE: &str = "01J9ZQ3M6V4Q1ZB2Y7H8K5N0P2";

/// A cloud that answers from a script and remembers the paths it was asked.
struct ScriptedCloud {
    answers: Mutex<Vec<(u16, serde_json::Value)>>,
    asked: Mutex<Vec<String>>,
}

impl HttpTransport for ScriptedCloud {
    async fn post_json(&self, path: &str, _body: Vec<u8>) -> Result<HttpResponse, TransportError> {
        self.asked.lock().expect("lock").push(path.to_owned());
        let mut answers = self.answers.lock().expect("lock");
        if answers.is_empty() {
            return Err(TransportError::new("the script ran out"));
        }
        let (status, body) = answers.remove(0);
        Ok(HttpResponse {
            status,
            body: serde_json::to_vec(&body).expect("encode"),
            ..HttpResponse::default()
        })
    }

    async fn post_bytes(
        &self,
        _path: &str,
        _content_type: &str,
        _body: Vec<u8>,
    ) -> Result<HttpResponse, TransportError> {
        Err(TransportError::new("not used"))
    }
}

fn opened(claim_id: &str, code: &str) -> (u16, serde_json::Value) {
    (
        201,
        serde_json::json!({
            "claim_id": claim_id,
            "user_code": code,
            "secret": "cd".repeat(32),
            "expires_in_secs": 3600,
            "poll_interval_secs": 0,
        }),
    )
}

#[tokio::test]
async fn an_expired_code_is_replaced_and_a_bound_one_makes_the_box_its_store() {
    let cloud = ScriptedCloud {
        answers: Mutex::new(vec![
            opened("01J9ZQ3M6V4Q1ZB2Y7H8K5N0A1", "AAAA-AAAA"),
            (409, serde_json::json!({})),
            opened("01J9ZQ3M6V4Q1ZB2Y7H8K5N0A2", "BBBB-BBBB"),
            (202, serde_json::json!({ "status": "PENDING" })),
            (
                200,
                serde_json::json!({
                    "tenant_id": "01J9ZQ3M6V4Q1ZB2Y7H8K5N0P1",
                    "store_id": STORE,
                    "device_id": "01J9ZQ3M6V4Q1ZB2Y7H8K5N0P3",
                    "credential": "posdev_claimed_secret",
                }),
            ),
        ]),
        asked: Mutex::new(Vec::new()),
    };
    let claims = HttpClaim::new(cloud);
    let vault = FakeKeyVault::new();
    let dir = tempfile::tempdir().expect("a directory");
    let config_path = dir.path().join("config.toml");
    let cloud_url = url::Url::parse("https://pos.example.vn").expect("a URL");
    let (status, page) = tokio::sync::watch::channel(ClaimStatus::Connecting {
        cloud: "https://pos.example.vn".to_owned(),
    });

    let claimed = claim(
        &claims,
        &vault,
        &cloud_url,
        &config_path,
        &status,
        Duration::ZERO,
    )
    .await
    .expect("the box is claimed");

    assert_eq!(claimed.store_id.to_string(), STORE);
    let credential = vault
        .load(SecretName::DeviceCredential)
        .await
        .expect("the vault reads")
        .expect("the credential is kept");
    assert_eq!(credential.expose(), b"posdev_claimed_secret");
    let config = EdgeConfig::load(&config_path).expect("the configuration loads");
    assert_eq!(config.store_id.to_string(), STORE);
    assert_eq!(
        config.cloud_url.as_ref().map(url::Url::as_str),
        Some("https://pos.example.vn/")
    );
    assert_eq!(
        *page.borrow(),
        ClaimStatus::Claimed {
            store_id: STORE.to_owned()
        },
        "the page says so"
    );
    assert!(
        !dir.path().join("config.toml.claiming").exists(),
        "no half-written file is left behind"
    );
}
