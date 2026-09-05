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
use std::time::Duration;

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
    /// How many minutes a signed-in device may sit idle before its sign-in stops counting
    /// ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)). Defaults to 30.
    ///
    /// Sign-in survives a restart, which is what stops a power blip or an OTA install making every
    /// member of staff re-enter a PIN mid-service. The cost of that is a till carried off while
    /// signed in as a manager, and this window is what bounds it: past it, the device is treated as
    /// signed out. Lower it towards the pre-S0d behaviour, or raise it for continuity and accept the
    /// wider window; `0` is refused by [`Self::validate`], because a zero window signs a device out
    /// between its own two requests.
    #[serde(default = "default_sign_in_idle_timeout_minutes")]
    pub sign_in_idle_timeout_minutes: u64,
    /// Where to load printing fonts from
    /// ([ADR-0102](../../../docs/adr/0102-printing-any-script.md)).
    ///
    /// A thermal printer's firmware carries a few hundred glyphs, which is not Vietnamese and is
    /// nowhere near Japanese or Devanagari, so anything outside plain ASCII is drawn by this box and
    /// sent as a raster. That needs fonts, and fonts are a deployment asset rather than framework
    /// code — a framework that embedded one would ship every store several megabytes it will not
    /// print and still not cover the next country.
    ///
    /// Every `.ttf`, `.otf` and `.ttc` directly inside each directory is loaded, directories in the
    /// order given and files within one in filename order. That order is the fallback order: put the
    /// face for ordinary Latin text first. Defaults to the standard system font directories for the
    /// platform, which is where the packages `deploy/edge/README.md` names install to.
    ///
    /// A box that loads no font still trades and still prints ASCII; it refuses the lines it cannot
    /// draw, and says which scripts it can print at start-up.
    #[serde(default = "default_font_directories")]
    pub font_directories: Vec<PathBuf>,
    /// How tall printed text is, in printer dots per em. Defaults to 24, which is a comfortable
    /// receipt body at the 203 dpi every common thermal printer runs at.
    ///
    /// Only applies to rasterised lines. A line the printer's own character set covers is still sent
    /// as text and drawn in the firmware's font, which this does not change.
    #[serde(default = "default_font_size_dots")]
    pub font_size_dots: u16,
}

/// The standard font directories for this platform.
///
/// Not a guess at one distribution's layout: each entry is where that platform's own font packages
/// install to, so a box that ran the install step in `deploy/edge/README.md` is already covered and
/// `font_directories` only has to be set for a font kept somewhere else.
fn default_font_directories() -> Vec<PathBuf> {
    if cfg!(windows) {
        vec![PathBuf::from(r"C:\Windows\Fonts")]
    } else {
        vec![
            PathBuf::from("/usr/share/fonts/truetype"),
            PathBuf::from("/usr/share/fonts/opentype"),
            PathBuf::from("/usr/local/share/fonts"),
        ]
    }
}

/// Printer dots per em for rasterised text.
const fn default_font_size_dots() -> u16 {
    24
}

/// Thirty minutes, as [`crate::auth::DEFAULT_SIGN_IN_IDLE_TIMEOUT`].
const fn default_sign_in_idle_timeout_minutes() -> u64 {
    30
}

/// The JetStream stream this store publishes into
/// ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)).
///
/// Both fields must match the cloud consumer's `stream` / `filter_subject`, or the events land
/// somewhere nothing reads. Neither is a secret, which is why they live in `config.toml` while the
/// server URL does not.
///
/// **Both are fleet-wide, and identical on every store** ([ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)
/// Amendment 1): the cloud binds one durable consumer to one named stream, so per-store streams
/// would be ingested one store deep, and a per-store *subject* inside a shared stream would be
/// refused for every box after the first — the handshake's create-or-get does not add a subject to a
/// stream that already exists. The store the event came from is inside the event, not in the
/// subject.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NatsConfig {
    /// The stream name — one for the fleet, `POS_FLEET`, matching `cloud.toml`'s `[nats] stream`.
    pub stream: String,
    /// The subject every event is published to — `pos.fleet.events`, the same on every store.
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
            sign_in_idle_timeout_minutes: default_sign_in_idle_timeout_minutes(),
            font_directories: default_font_directories(),
            font_size_dots: default_font_size_dots(),
        }
    }

    /// How long a signed-in device may idle, as a [`Duration`].
    #[must_use]
    pub const fn sign_in_idle_timeout(&self) -> Duration {
        Duration::from_secs(self.sign_in_idle_timeout_minutes * 60)
    }

    /// Rejects a configuration that would misbehave rather than starting with it.
    ///
    /// # Errors
    ///
    /// [`EdgeError::Config`] when `sign_in_idle_timeout_minutes` is zero: a zero window signs a
    /// device out between its own two requests, which looks like a broken till rather than like a
    /// policy. A deployment that wants the pre-S0d behaviour uses the revoke-all break-glass after a
    /// restart instead.
    pub fn validate(&self) -> Result<(), EdgeError> {
        if self.sign_in_idle_timeout_minutes == 0 {
            return Err(EdgeError::Config(
                "sign_in_idle_timeout_minutes must be at least 1: a zero window signs a device out \
                 between its own two requests (ADR-0091)"
                    .to_owned(),
            ));
        }
        Ok(())
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
        // The fleet shape the console generates, and the one `cloud.toml` is armed with: one stream,
        // one subject, identical on every store (ADR-0087 Amendment 1).
        let text = "store_id = \"01JQ0000000000000000000001\"\n\
                    [nats]\n\
                    stream = \"POS_FLEET\"\n\
                    subject = \"pos.fleet.events\"";
        let config = EdgeConfig::from_toml_str(text).expect("parses");
        let nats = config.nats.expect("the section is present");
        assert_eq!(nats.stream, "POS_FLEET");
        assert_eq!(nats.subject, "pos.fleet.events");
    }

    #[test]
    fn a_top_level_key_below_the_nats_table_is_refused_rather_than_misread() {
        // Why the console's generator puts `[nats]` last and says so in a comment (E3): TOML reads
        // every key after a table header as part of that table, so an operator who uncommented
        // `store_path` below the header would be setting `nats.store_path`. `deny_unknown_fields`
        // makes that a load-time refusal, which is the whole reason the warning can be trusted — the
        // alternative would be a store quietly ignoring its own configuration.
        let above = "store_id = \"01JQ0000000000000000000001\"\n\
                     store_path = \"store.sqlite\"\n\
                     [nats]\n\
                     stream = \"POS_FLEET\"\n\
                     subject = \"pos.fleet.events\"";
        let below = "store_id = \"01JQ0000000000000000000001\"\n\
                     [nats]\n\
                     stream = \"POS_FLEET\"\n\
                     subject = \"pos.fleet.events\"\n\
                     store_path = \"store.sqlite\"";
        // The same three lines in the order the generator emits them.
        assert_eq!(
            EdgeConfig::from_toml_str(above)
                .expect("parses with the table last")
                .store_path,
            std::path::PathBuf::from("store.sqlite")
        );
        // And the same three lines with only the position changed.
        assert!(EdgeConfig::from_toml_str(below).is_err());
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
