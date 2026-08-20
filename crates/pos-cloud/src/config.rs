// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cloud configuration.
//!
//! The process's own boot configuration: where to bind, how to reach PostgreSQL, and — optionally —
//! the NATS cursor that feeds ingest in production ([`crate::cursor`]). The four-level
//! Tenant→Brand→Store→Device config tree the cloud *serves* (`docs/roadmap.md` P7) is a later slice;
//! this is not that.

use std::net::SocketAddr;

use serde::Deserialize;

/// How many messages one cursor pull gathers, when the config does not say.
const fn default_batch() -> usize {
    256
}

/// How long one cursor pull waits for a full batch, in seconds, when the config does not say.
const fn default_expires_secs() -> u64 {
    5
}

/// How the `pos_cloud` process boots.
#[derive(Debug, Clone, Deserialize)]
pub struct CloudConfig {
    /// The address the HTTP server binds.
    pub bind: SocketAddr,
    /// The PostgreSQL connection string, in libpq form
    /// (`host=… port=… user=… dbname=…`).
    pub database_url: String,
    /// The NATS cursor that drives ingest. Absent means the cursor is off and events arrive only
    /// through the `/internal/ingest` reconciliation re-push.
    #[serde(default)]
    pub nats: Option<NatsIngestConfig>,
}

/// Where the ingest cursor reads, and how it batches.
///
/// Mirrors `link-nats`'s `ConsumerConfig`, kept as plain data here so config parsing needs no NATS
/// types; `main.rs` maps it across.
#[derive(Debug, Clone, Deserialize)]
pub struct NatsIngestConfig {
    /// The NATS server URL (e.g. `127.0.0.1:4222`).
    pub url: String,
    /// The JetStream stream to consume — the stream the fleet's stores publish to.
    pub stream: String,
    /// The durable consumer name. Stable across restarts, which is what makes the cursor durable.
    pub durable: String,
    /// Restrict the cursor to this subject, or empty for every subject the stream captures.
    #[serde(default)]
    pub filter_subject: String,
    /// The most messages one pull gathers before returning.
    #[serde(default = "default_batch")]
    pub batch: usize,
    /// How long one pull waits for a full batch before returning what it has, in seconds.
    #[serde(default = "default_expires_secs")]
    pub expires_secs: u64,
}

impl CloudConfig {
    /// Parses configuration from TOML text.
    ///
    /// # Errors
    ///
    /// [`toml::de::Error`] if the text is not a valid configuration document.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::CloudConfig;

    #[test]
    fn parses_a_minimal_config() {
        let config = CloudConfig::from_toml(
            "bind = \"127.0.0.1:8443\"\ndatabase_url = \"host=localhost user=pos dbname=poscloud\"\n",
        )
        .expect("valid config");
        assert_eq!(config.bind.port(), 8443);
        assert!(config.database_url.contains("dbname=poscloud"));
        assert!(config.nats.is_none(), "no [nats] means the cursor is off");
    }

    #[test]
    fn parses_the_nats_cursor_section_with_defaults() {
        let config = CloudConfig::from_toml(
            "bind = \"127.0.0.1:8443\"\n\
             database_url = \"host=localhost dbname=poscloud\"\n\
             [nats]\n\
             url = \"127.0.0.1:4222\"\n\
             stream = \"POS_FLEET\"\n\
             durable = \"cloud_ingest\"\n",
        )
        .expect("valid config");
        let nats = config.nats.expect("a [nats] section");
        assert_eq!(nats.stream, "POS_FLEET");
        assert_eq!(nats.durable, "cloud_ingest");
        assert!(
            nats.filter_subject.is_empty(),
            "no filter means all subjects"
        );
        assert_eq!(nats.batch, 256, "the batch default");
        assert_eq!(nats.expires_secs, 5, "the expiry default");
    }
}
