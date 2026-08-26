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

/// How often the rollup projector sweeps the fleet, in seconds, when the config does not say.
const fn default_projector_interval_secs() -> u64 {
    30
}

/// How long a super-admin session is valid, in seconds, when the config does not say — eight hours,
/// a working day, after which the admin signs in again ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
const fn default_admin_session_ttl_secs() -> u64 {
    8 * 60 * 60
}

/// How often the retention cron sweeps for records past their period, in seconds, when the config
/// does not say — daily, ample for a period measured in months ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
const fn default_retention_sweep_interval_secs() -> u64 {
    24 * 60 * 60
}

/// How often the webhook dispatcher sweeps the enabled fleet, in seconds, when the config does not
/// say — ten seconds, prompt enough that a subscriber sees an event soon after it lands
/// ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
const fn default_webhook_dispatch_interval_secs() -> u64 {
    10
}

/// How long one webhook delivery may take before it is abandoned as failed, in seconds, when the
/// config does not say — ten seconds, so a black-hole endpoint cannot wedge the dispatch loop
/// ([ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md)).
const fn default_webhook_delivery_timeout_secs() -> u64 {
    10
}

/// How often the metrics heartbeat samples, in seconds, when the monitoring profile is on but the
/// config does not say — 60, the low end of `docs/capacity-and-reliability.md`'s 60–120s sparse
/// sampling cadence.
const fn default_metrics_interval_secs() -> u64 {
    60
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
    /// How often the rollup projector sweeps the fleet to keep dashboards current, in seconds.
    #[serde(default = "default_projector_interval_secs")]
    pub projector_interval_secs: u64,
    /// How long a super-admin session cookie stays valid, in seconds
    /// ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
    #[serde(default = "default_admin_session_ttl_secs")]
    pub admin_session_ttl_secs: u64,
    /// The one-time super-admin setup token that gates first-boot enrolment
    /// ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md)). **No default**: when it is
    /// absent the `/admin/setup` route is off (a `404`). `bootstrap.sh` mints it into this file on the
    /// first deploy and it is removed once the first super-admin is enrolled.
    #[serde(default)]
    pub admin_setup_token: Option<String>,
    /// How long personal data is retained before the cron masks it, in days
    /// ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)). **No default**: the period
    /// is a legal decision, not a code guess, so when it is absent the retention cron does not run at
    /// all (masking on a guessed schedule would erase data early or keep it too long, both
    /// violations). Set it from the country's configured value.
    #[serde(default)]
    pub retention_days: Option<u32>,
    /// How often the retention cron sweeps for records past their period, in seconds.
    #[serde(default = "default_retention_sweep_interval_secs")]
    pub retention_sweep_interval_secs: u64,
    /// How often the webhook dispatcher sweeps the enabled fleet, in seconds
    /// ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
    #[serde(default = "default_webhook_dispatch_interval_secs")]
    pub webhook_dispatch_interval_secs: u64,
    /// How long one webhook delivery may take before it is abandoned as failed, in seconds
    /// ([ADR-0038](../../../docs/adr/0038-webhook-tls-sender.md)).
    #[serde(default = "default_webhook_delivery_timeout_secs")]
    pub webhook_delivery_timeout_secs: u64,
    /// The secret the cloud signs and verifies table QR tokens with
    /// ([ADR-0057](../../../docs/adr/0057-qr-ordering.md)). **No default**: a guest QR order carries no
    /// API key — the signed token is its only credential — so without a secret the `/v1/qr/orders`
    /// endpoint is off (any token would be unverifiable). `bootstrap.sh` mints it into this file, the
    /// same way it mints the admin setup token; it never leaves the server.
    #[serde(default)]
    pub table_token_secret: Option<String>,
    /// The optional monitoring profile (metrics-vm → `VictoriaMetrics`,
    /// [ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)). **No default / off**: per
    /// `docs/capacity-and-reliability.md` the monitoring profile is off below ~50 stores in favour of
    /// sparse sampling straight into PostgreSQL, so a pilot cell leaves this unset and emits no
    /// telemetry. Set it only when the `monitoring` compose profile is running.
    #[serde(default)]
    pub metrics: Option<MetricsConfig>,
}

/// The optional monitoring profile: where the sparse metrics heartbeat imports, and how often.
#[derive(Debug, Clone, Deserialize)]
pub struct MetricsConfig {
    /// The `VictoriaMetrics` JSON-import base URL (an `http://host:port`; the cloud reaches its own
    /// backend over the box's private network, so plain `http` — TLS terminates at the proxy, P8).
    pub url: String,
    /// How often the heartbeat samples, in seconds.
    #[serde(default = "default_metrics_interval_secs")]
    pub sample_interval_secs: u64,
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
        assert_eq!(
            config.projector_interval_secs, 30,
            "the projector interval defaults when unset"
        );
        assert_eq!(
            config.admin_session_ttl_secs,
            8 * 60 * 60,
            "the admin session TTL defaults to eight hours when unset"
        );
        assert_eq!(
            config.retention_days, None,
            "with no configured retention period the cron stays off — never a code default"
        );
        assert_eq!(
            config.admin_setup_token, None,
            "with no configured setup token the /admin/setup route stays off"
        );
        assert_eq!(
            config.retention_sweep_interval_secs,
            24 * 60 * 60,
            "the retention sweep interval defaults to daily when unset"
        );
        assert_eq!(
            config.webhook_dispatch_interval_secs, 10,
            "the webhook dispatch interval defaults to ten seconds when unset"
        );
        assert_eq!(
            config.webhook_delivery_timeout_secs, 10,
            "the webhook delivery timeout defaults to ten seconds when unset"
        );
        assert_eq!(
            config.table_token_secret, None,
            "with no configured QR secret the /v1/qr/orders endpoint stays off"
        );
        assert!(
            config.metrics.is_none(),
            "no [metrics] means the monitoring profile is off — sparse-sampling posture below ~50 stores"
        );
    }

    #[test]
    fn parses_the_metrics_section_with_defaults() {
        let config = CloudConfig::from_toml(
            "bind = \"127.0.0.1:8443\"\n\
             database_url = \"host=localhost dbname=poscloud\"\n\
             [metrics]\n\
             url = \"http://127.0.0.1:8428\"\n",
        )
        .expect("valid config");
        let metrics = config.metrics.expect("a [metrics] section");
        assert_eq!(metrics.url, "http://127.0.0.1:8428");
        assert_eq!(
            metrics.sample_interval_secs, 60,
            "the sample interval defaults to 60s (sparse-sampling cadence)"
        );
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
