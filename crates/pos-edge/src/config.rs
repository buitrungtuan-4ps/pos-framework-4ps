// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's configuration.
//!
//! A store server is configured by activation and by the cloud config tree
//! ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)), not by an operator editing
//! files. What lives here is the *bootstrap* configuration the binary needs before it can talk to
//! anything: where to listen, and which store it is. The hot-reloadable cloud configuration, with
//! last-known-good retention ([`docs/roadmap.md`](../../../docs/roadmap.md) P5), layers on top of
//! this in a later slice.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use pos_proto::ids::StoreId;
use serde::Deserialize;

use crate::error::EdgeError;

/// The default listen address: every interface, so any device on the store LAN can reach it, on a
/// memorable high port. Pairing (a later slice) is what makes reaching it safe.
const DEFAULT_BIND: &str = "0.0.0.0:8787";

/// The bootstrap configuration a `pos_edge` process needs to start.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeConfig {
    /// The address the HTTP server binds. Defaults to [`DEFAULT_BIND`].
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    /// Which store this machine is. Assigned at activation ([ADR-0003](../../../docs/adr/0003-cattle-not-pets.md));
    /// it identifies the event log this edge owns and is not itself PII.
    pub store_id: StoreId,
    /// The LAN IP to put in the pairing URL ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)),
    /// pinned by a DHCP reservation. `None` falls back to the bind address when it is a concrete
    /// interface, and otherwise the operator reads the IP off the console.
    #[serde(default)]
    pub advertised_ip: Option<IpAddr>,
    /// The base URL of this store's cloud ([ADR-0085](../../../docs/adr/0085-edge-cloud-sync-transport.md)),
    /// written into `config.toml` at provisioning ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
    /// When set, the edge runs its config-pull and heartbeat loops against it (authenticated with the
    /// scoped `read_config` key from `POS_EDGE_SYNC_KEY`); when absent, the edge runs LAN-only and
    /// spawns no cloud loops, exactly as a demo or a not-yet-provisioned box does. It is not a secret —
    /// the credential is — so it lives here rather than in the environment.
    #[serde(default)]
    pub cloud_url: Option<url::Url>,
    /// Where the SQLite event store lives ([ADR-0015](../../../docs/adr/0015-sqlite-access.md)).
    /// Relative to the working directory the service unit sets. Defaults to `store.sqlite`.
    #[serde(default = "default_store_path")]
    pub store_path: PathBuf,
    /// Where this store publishes its committed events
    /// ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)). Absent means the edge
    /// runs no event-publish loop: it still trades, and its outbox simply grows until a stream is
    /// configured. The server **URL** is deliberately not here — it is the one field that would carry
    /// a credential, so it comes from the environment (see [`NatsConfig`]).
    #[serde(default)]
    pub nats: Option<NatsConfig>,
}

/// The JetStream stream this store publishes into
/// ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)).
///
/// Both fields must match the cloud consumer's `stream` / `filter_subject`, or the events land
/// somewhere nothing reads. Neither is a secret, which is why they live in `config.toml` while the
/// server URL does not.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NatsConfig {
    /// The stream name — one per store, e.g. `POS_STORE_<id>`.
    pub stream: String,
    /// The subject every event is published to.
    pub subject: String,
}

impl EdgeConfig {
    /// Builds a configuration directly, for tests and the on-fakes example.
    #[must_use]
    pub fn new(bind: SocketAddr, store_id: StoreId) -> Self {
        Self {
            bind,
            store_id,
            advertised_ip: None,
            cloud_url: None,
            store_path: default_store_path(),
            nats: None,
        }
    }

    /// The host to advertise in the pairing URL: the configured `advertised_ip`, or the bind IP when
    /// it names a concrete interface (not `0.0.0.0`/`::`), or `None` when only the operator knows it.
    #[must_use]
    pub fn advertised_host(&self) -> Option<IpAddr> {
        self.advertised_ip.or_else(|| {
            let ip = self.bind.ip();
            if ip.is_unspecified() { None } else { Some(ip) }
        })
    }

    /// Parses a configuration from TOML.
    ///
    /// # Errors
    ///
    /// [`EdgeError::ConfigParse`] if the text is not valid configuration — an unknown key, a missing
    /// `store_id`, or a malformed address.
    pub fn from_toml_str(text: &str) -> Result<Self, EdgeError> {
        toml::from_str(text).map_err(EdgeError::ConfigParse)
    }

    /// Reads and parses the configuration file at `path`.
    ///
    /// # Errors
    ///
    /// [`EdgeError::ConfigRead`] if the file cannot be read, or [`EdgeError::ConfigParse`] if its
    /// contents are not valid configuration.
    pub fn load(path: &Path) -> Result<Self, EdgeError> {
        let text = std::fs::read_to_string(path).map_err(|source| EdgeError::ConfigRead {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_toml_str(&text)
    }
}

/// The serde default for [`EdgeConfig::bind`]. Infallible: [`DEFAULT_BIND`] is a valid literal, and a
/// unit test proves it.
fn default_bind() -> SocketAddr {
    DEFAULT_BIND
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 8787)))
}

/// The serde default for [`EdgeConfig::store_path`].
fn default_store_path() -> PathBuf {
    PathBuf::from("store.sqlite")
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BIND, EdgeConfig, default_bind};

    #[test]
    fn the_default_bind_literal_parses() {
        assert_eq!(default_bind().to_string(), DEFAULT_BIND);
    }

    #[test]
    fn a_store_id_is_required() {
        // No store_id: the edge does not know which log it owns, so this must fail rather than
        // silently pick one.
        let err = EdgeConfig::from_toml_str("bind = \"0.0.0.0:9000\"");
        assert!(err.is_err(), "missing store_id must be rejected");
    }

    #[test]
    fn bind_defaults_when_omitted() {
        let config =
            EdgeConfig::from_toml_str("store_id = \"01JQ0000000000000000000001\"").expect("parses");
        assert_eq!(config.bind, default_bind());
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        // deny_unknown_fields: a typo in a config key is a mistake to surface at load, not to ignore.
        let text = "store_id = \"01JQ0000000000000000000001\"\nlisten = \"0.0.0.0:80\"";
        assert!(EdgeConfig::from_toml_str(text).is_err());
    }

    #[test]
    fn cloud_url_defaults_to_none() {
        // A config that names no cloud is LAN-only — the edge spawns no cloud loops (ADR-0085), which
        // keeps the on-fakes example and a not-yet-provisioned box booting exactly as before.
        let config =
            EdgeConfig::from_toml_str("store_id = \"01JQ0000000000000000000001\"").expect("parses");
        assert!(config.cloud_url.is_none());
    }

    #[test]
    fn a_cloud_url_parses_when_present() {
        let text =
            "store_id = \"01JQ0000000000000000000001\"\ncloud_url = \"https://acme.pos.example\"";
        let config = EdgeConfig::from_toml_str(text).expect("parses");
        assert_eq!(
            config.cloud_url.as_ref().map(url::Url::as_str),
            Some("https://acme.pos.example/"),
        );
    }

    #[test]
    fn nats_defaults_to_none() {
        // No stream configured is a store that trades and keeps its events locally (ADR-0087); the
        // outbox grows rather than the box refusing to boot.
        let config =
            EdgeConfig::from_toml_str("store_id = \"01JQ0000000000000000000001\"").expect("parses");
        assert!(config.nats.is_none());
    }

    #[test]
    fn a_nats_section_parses_stream_and_subject() {
        let text = "store_id = \"01JQ0000000000000000000001\"\n\
                    [nats]\n\
                    stream = \"POS_STORE_01JQ0000000000000000000001\"\n\
                    subject = \"pos.store.01JQ0000000000000000000001.events\"";
        let config = EdgeConfig::from_toml_str(text).expect("parses");
        let nats = config.nats.expect("the section is present");
        assert_eq!(nats.stream, "POS_STORE_01JQ0000000000000000000001");
        assert_eq!(nats.subject, "pos.store.01JQ0000000000000000000001.events");
    }

    #[test]
    fn a_nats_url_in_config_is_rejected() {
        // The server URL is the one field that would carry a credential, so it comes from the
        // environment and `deny_unknown_fields` makes putting it here a load-time error rather than a
        // secret quietly committed to `config.toml` (ADR-0086, ADR-0087).
        let text = "store_id = \"01JQ0000000000000000000001\"\n\
                    [nats]\n\
                    stream = \"S\"\n\
                    subject = \"s\"\n\
                    url = \"nats://user:secret@cloud.example:4222\"";
        assert!(EdgeConfig::from_toml_str(text).is_err());
    }
}
