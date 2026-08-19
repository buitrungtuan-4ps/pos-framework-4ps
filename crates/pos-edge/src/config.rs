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
use std::path::Path;

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
}

impl EdgeConfig {
    /// Builds a configuration directly, for tests and the on-fakes example.
    #[must_use]
    pub fn new(bind: SocketAddr, store_id: StoreId) -> Self {
        Self {
            bind,
            store_id,
            advertised_ip: None,
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
}
