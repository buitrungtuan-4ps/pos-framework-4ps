// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The field [`GatewayCalls`]: posts a till's lookup to the store's own
//! `POST /sync/stores/{store_id}/gateway/calls`
//! ([ADR-0214](../../../../docs/adr/0214-a-tills-external-lookup-is-one-catalogue-act.md)
//! decision 3). The body is passed through unread, so nothing it carries reaches a log here.

use pos_proto::ids::StoreId;

use super::CloudHttpClient;
use crate::http::gateway::{GATEWAY_WAIT, GatewayCalls, GatewayFuture};

/// The gateway transport over one keyed client, for one store.
#[derive(Debug, Clone)]
pub struct GatewayHttpTransport {
    client: CloudHttpClient,
    store_id: StoreId,
}

impl GatewayHttpTransport {
    /// Builds the transport over `client` for `store_id`. The client's own timeout is set past
    /// [`GATEWAY_WAIT`], so the route's wait is the one that ends a slow call and says so.
    #[must_use]
    pub fn new(client: CloudHttpClient, store_id: StoreId) -> Self {
        Self {
            client: client.with_timeout(GATEWAY_WAIT + core::time::Duration::from_secs(1)),
            store_id,
        }
    }
}

/// The gateway route for a store. Pure, so the path is checked without a socket.
fn gateway_path(store_id: StoreId) -> String {
    format!("/sync/stores/{store_id}/gateway/calls")
}

impl GatewayCalls for GatewayHttpTransport {
    fn call(&self, body: Vec<u8>) -> GatewayFuture<'_> {
        Box::pin(async move {
            self.client
                .request(
                    &hyper::Method::POST,
                    &gateway_path(self.store_id),
                    None,
                    body,
                )
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    use super::{GATEWAY_WAIT, GatewayHttpTransport, gateway_path};
    use crate::cloud_http::CloudHttpClient;

    #[test]
    fn a_call_goes_to_the_stores_own_gateway_route_and_outlasts_the_wait() {
        let store_id = StoreId::new(Ulid::from_u128(7));
        assert_eq!(
            gateway_path(store_id),
            format!("/sync/stores/{store_id}/gateway/calls")
        );
        let base = url::Url::parse("https://cloud.example.test").expect("a URL");
        let client = CloudHttpClient::new(&base, "posdev_fake").expect("a client");
        let transport = GatewayHttpTransport::new(client, store_id);
        assert!(transport.client.timeout > GATEWAY_WAIT);
    }
}
