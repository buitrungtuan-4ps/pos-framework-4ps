// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The gateway link an activated store's lookups go out on
//! ([ADR-0214](../../../../docs/adr/0214-a-tills-external-lookup-is-one-catalogue-act.md)
//! decision 3).

use std::sync::Arc;

use pos_ports::key_vault::{KeyVault, SecretName};
use pos_proto::ids::StoreId;
use tokio_rustls::rustls::pki_types::CertificateDer;

use super::vault_text;
use crate::cloud_http::{CloudHttpClient, GatewayHttpTransport};
use crate::http::gateway::Gateway;

/// Layers the gateway link on `app`, over the box's device credential.
///
/// The device credential rather than the `/sync` loops' key, for the event rail's reason: only the
/// device credential is certain to carry `call_gateway`, and a store key issued before that scope
/// existed does not. Without a credential, or a client, `app` is returned as it was and every lookup
/// answers `GATEWAY_OFFLINE`; the store trades either way.
pub(super) async fn with_gateway<V: KeyVault>(
    app: axum::Router,
    store_id: StoreId,
    (cloud_url, roots): (&url::Url, &[CertificateDer<'static>]),
    vault: &V,
) -> axum::Router {
    let Some(credential) = vault_text(vault, SecretName::DeviceCredential).await else {
        tracing::warn!("no device credential could be read; the till's lookups answer offline");
        return app;
    };
    match CloudHttpClient::trusting(cloud_url, credential, roots) {
        Ok(client) => app.layer(axum::Extension(Gateway::new(Arc::new(
            GatewayHttpTransport::new(client, store_id),
        )))),
        Err(error) => {
            tracing::error!(%error, "the gateway link could not be built; the till's lookups answer offline");
            app
        }
    }
}
