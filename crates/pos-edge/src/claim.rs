// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `pos-edge claim --cloud <url>`: a box installed with no store claims itself
//! ([ADR-0148](../../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md)).
//!
//! One image fits every store. A box that boots from it has no `config.toml`, so instead of serving
//! it runs this once:
//!
//! 1. It opens a claim with its cloud and shows the code, in its log and on a page at
//!    [`CLAIM_PAGE`] that the till's own UI draws (`/claim`), in the store's languages.
//! 2. It polls until a person at the console binds that code to a device slot (Activation →
//!    Claim a box). An expired code is replaced with a new one.
//! 3. It collects its device credential, keeps it in the OS keyring, writes `config.toml` for the
//!    store it was claimed for, and exits. The service manager starts the store server next, and that
//!    credential is all it needs to sync
//!    ([ADR-0143](../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)).
//!
//! **Run it as the service's own user.** The keyring is per user on Linux, so a credential kept by
//! `root` is invisible to a service running as `pos`. The appliance's claim unit does exactly this.
//!
//! The secret that collects the credential never leaves this process except to the cloud that issued
//! it. The code is shown, which is its job: it lasts an hour, binds once, and binding it needs a
//! console session with device rights.

use core::time::Duration;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::response::Redirect;
use axum::routing::get;
use cloud_sync_http::{Collection, HttpClaim, HttpTransport, TlsHttpTransport};
use key_vault_keyring::{KeyringVault, OsKeyring};
use pos_ports::key_vault::{KeyVault, SecretName};
use pos_proto::ClockSource as _;
use pos_proto::ids::StoreId;

use crate::clock::SystemClock;
use crate::error::EdgeError;

/// The subcommand.
pub const CLAIM_COMMAND: &str = "claim";

/// Where the claim page is served: loopback only, for the box's own screen. The store server's port
/// is not used, so a kiosk pointed here while the box is unclaimed shows nothing stale once it is.
pub const CLAIM_PAGE: &str = "127.0.0.1:8080";

/// How long one request to the cloud may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait before asking again when the cloud cannot be reached or limits this box.
const RETRY_AFTER: Duration = Duration::from_secs(15);

/// How long the page shows "claimed" before the process exits and the service takes over.
const CLAIMED_GRACE: Duration = Duration::from_secs(3);

/// What the claim page shows, as `GET /api/claim` answers it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClaimStatus {
    /// Asking the cloud for a claim.
    Connecting {
        /// The cloud this box will belong to.
        cloud: String,
    },
    /// Showing a code for a person to bind at the console.
    Waiting {
        /// The code, `XXXX-XXXX`.
        user_code: String,
        /// When it stops working, in milliseconds since the Unix epoch.
        expires_at_ms: i64,
        /// The cloud whose console binds it.
        cloud: String,
    },
    /// The cloud could not be reached; asking again shortly.
    Unreachable {
        /// The cloud being asked.
        cloud: String,
    },
    /// Claimed: the box is this store now, and the store server starts next.
    Claimed {
        /// The store it was claimed for.
        store_id: String,
    },
}

/// What a successful claim produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    /// The store the box now is.
    pub store_id: StoreId,
    /// Where its `config.toml` was written.
    pub config_path: PathBuf,
}

/// Reads `--cloud <https URL>` from the arguments after `claim`.
///
/// # Errors
///
/// [`EdgeError::Install`] if the flag is missing or the URL is not `https` with a host, the only
/// kind of cloud the edge dials.
pub fn cloud_from_arguments(arguments: &[String]) -> Result<url::Url, EdgeError> {
    let value = arguments
        .iter()
        .position(|argument| argument == "--cloud")
        .and_then(|at| arguments.get(at + 1))
        .ok_or_else(|| {
            EdgeError::Install("say which cloud: pos-edge claim --cloud <https URL>".to_owned())
        })?;
    url::Url::parse(value)
        .ok()
        .filter(|url| url.scheme() == "https" && url.host_str().is_some())
        .ok_or_else(|| EdgeError::Install(format!("the cloud is not an https URL: {value}")))
}

/// Runs the claim for a box whose configuration would live at `config_path`.
///
/// A box that already has a `config.toml` is claimed or installed already, and this returns at once
/// without touching anything.
///
/// # Errors
///
/// [`EdgeError::Install`] for a bad command line, a keyring that refuses the credential, or a
/// configuration file that cannot be written; [`EdgeError::Runtime`] if no async runtime can start.
pub fn run(arguments: &[String], config_path: &Path) -> Result<(), EdgeError> {
    if config_path.exists() {
        tracing::info!(
            config = %config_path.display(),
            "this box already has a config.toml; there is nothing to claim"
        );
        return Ok(());
    }
    let cloud = cloud_from_arguments(arguments)?;
    let transport = TlsHttpTransport::new(cloud.as_str(), REQUEST_TIMEOUT)
        .map_err(|error| EdgeError::Install(error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(EdgeError::Runtime)?;
    runtime.block_on(async {
        let (status, page) = tokio::sync::watch::channel(ClaimStatus::Connecting {
            cloud: origin(&cloud),
        });
        serve_page(page).await;
        let vault = KeyringVault::new(OsKeyring::new());
        let claimed = claim(
            &HttpClaim::new(transport),
            &vault,
            &cloud,
            config_path,
            &status,
            RETRY_AFTER,
        )
        .await?;
        tracing::info!(
            store_id = %claimed.store_id,
            config = %claimed.config_path.display(),
            "claimed: this box is the store now, and the store server starts next"
        );
        tokio::time::sleep(CLAIMED_GRACE).await;
        Ok(())
    })
}

/// Opens claims until one is collected, then keeps the credential and writes the configuration.
///
/// Every state it passes through is published on `status` for the page. A cloud that cannot be
/// reached, or that limits this box, is asked again after `retry`; an expired or refused claim is
/// replaced with a new one at once.
///
/// # Errors
///
/// [`EdgeError::Install`] if the keyring refuses the credential or `config_path` cannot be written.
/// The claim is spent by then, and running this again opens a new one.
pub async fn claim<T, V>(
    claims: &HttpClaim<T>,
    vault: &V,
    cloud: &url::Url,
    config_path: &Path,
    status: &tokio::sync::watch::Sender<ClaimStatus>,
    retry: Duration,
) -> Result<Claimed, EdgeError>
where
    T: HttpTransport,
    V: KeyVault,
{
    let origin = origin(cloud);
    loop {
        let opened = match claims.open().await {
            Ok(opened) => opened,
            Err(error) => {
                tracing::warn!(%error, "could not open a claim; asking again shortly");
                status.send_replace(ClaimStatus::Unreachable {
                    cloud: origin.clone(),
                });
                tokio::time::sleep(retry).await;
                continue;
            }
        };
        let now_ms = SystemClock.now().as_milliseconds_since_epoch();
        let lifetime_ms = i64::try_from(opened.expires_in_secs.saturating_mul(1000)).unwrap_or(0);
        let expires_at_ms = now_ms.saturating_add(lifetime_ms);
        tracing::info!(
            code = %opened.user_code,
            cloud = %origin,
            "waiting to be claimed: in the console, open Activation → Claim a box and type this code"
        );
        status.send_replace(ClaimStatus::Waiting {
            user_code: opened.user_code.clone(),
            expires_at_ms,
            cloud: origin.clone(),
        });
        let poll = Duration::from_secs(opened.poll_interval_secs);
        loop {
            if SystemClock.now().as_milliseconds_since_epoch() >= expires_at_ms {
                break;
            }
            match claims.collect(&opened).await {
                Ok(Collection::Pending) => tokio::time::sleep(poll).await,
                Ok(Collection::Expired | Collection::Refused) => break,
                Ok(Collection::Collected {
                    store_id,
                    credential,
                    ..
                }) => {
                    vault
                        .store(SecretName::DeviceCredential, &credential)
                        .await
                        .map_err(|error| {
                            EdgeError::Install(format!(
                                "the keyring refused the device credential ({error}); run \
                                 pos-edge claim again as the service's own user"
                            ))
                        })?;
                    write_config(config_path, &config_text(store_id, cloud, config_path))?;
                    status.send_replace(ClaimStatus::Claimed {
                        store_id: store_id.to_string(),
                    });
                    return Ok(Claimed {
                        store_id,
                        config_path: config_path.to_path_buf(),
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "could not ask about the claim; asking again shortly");
                    tokio::time::sleep(poll.max(retry.min(Duration::from_secs(5)))).await;
                }
            }
        }
        tracing::info!("the code expired before anyone used it; showing a new one");
    }
}

/// The cloud as the page names it: its origin, with no trailing slash.
fn origin(cloud: &url::Url) -> String {
    cloud.as_str().trim_end_matches('/').to_owned()
}

/// The `config.toml` a claimed box starts with: which store it is and which cloud it dials, and
/// nothing secret. The database sits beside the file, named absolutely, so a service manager that
/// starts the process in another directory still finds it.
#[must_use]
pub fn config_text(store_id: StoreId, cloud: &url::Url, config_path: &Path) -> String {
    let store_path = config_path
        .parent()
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("store.sqlite"));
    let store_line = store_path.map_or_else(
        || {
            "# Optional — where the SQLite event store lives (default store.sqlite):\n\
             # store_path = \"store.sqlite\""
                .to_owned()
        },
        |path| {
            format!(
                "store_path = \"{}\"",
                toml_escape(&path.display().to_string())
            )
        },
    );
    format!(
        "# pos_edge bootstrap configuration\n\
         # Store:  {store_id}\n\
         #\n\
         # Written by `pos-edge claim` when this box was claimed at the console (ADR-0148). It tells\n\
         # the store server WHICH store it is and WHICH cloud to dial. It carries no credential: the\n\
         # device credential the claim collected is in the OS keyring.\n\
         \n\
         store_id = \"{store_id}\"\n\
         cloud_url = \"{cloud}\"\n\
         \n\
         # Optional — override the listen address (default 0.0.0.0:8787):\n\
         # bind = \"0.0.0.0:8787\"\n\
         \n\
         # Optional — the LAN IP to advertise in the pairing QR; pin it with a DHCP reservation:\n\
         # advertised_ip = \"192.168.1.50\"\n\
         \n\
         {store_line}\n",
        cloud = toml_escape(&origin(cloud)),
    )
}

/// A value safe inside a TOML basic string.
fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Writes `text` to `path` whole or not at all: a temporary file beside it, then a rename, so a box
/// that loses power mid-write never boots from half a configuration.
fn write_config(path: &Path, text: &str) -> Result<(), EdgeError> {
    let unwritable = |error: std::io::Error| {
        EdgeError::Install(format!(
            "could not write {}: {error}; the claim is spent, so run pos-edge claim again",
            path.display()
        ))
    };
    if let Some(directory) = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    {
        std::fs::create_dir_all(directory).map_err(unwritable)?;
    }
    let staging = path.with_extension("toml.claiming");
    let mut file = std::fs::File::create(&staging).map_err(unwritable)?;
    file.write_all(text.as_bytes()).map_err(unwritable)?;
    file.sync_all().map_err(unwritable)?;
    drop(file);
    std::fs::rename(&staging, path).map_err(unwritable)
}

/// The page's own routes: its status, a redirect from `/` to the till UI's claim screen, and the UI
/// itself for everything else.
fn page_router(status: tokio::sync::watch::Receiver<ClaimStatus>) -> Router {
    Router::new()
        .route("/api/claim", get(current_status))
        .route("/", get(|| async { Redirect::temporary("/claim") }))
        .fallback(crate::http::assets::serve)
        .with_state(status)
}

async fn current_status(
    State(status): State<tokio::sync::watch::Receiver<ClaimStatus>>,
) -> Json<ClaimStatus> {
    Json(status.borrow().clone())
}

/// Serves the claim page on [`CLAIM_PAGE`], in the background. A port that is taken is logged and
/// skipped: the code is in the log too, and the claim goes on without the page.
async fn serve_page(status: tokio::sync::watch::Receiver<ClaimStatus>) {
    match tokio::net::TcpListener::bind(CLAIM_PAGE).await {
        Ok(listener) => {
            tracing::info!("the claim page is at http://{CLAIM_PAGE}/");
            tokio::spawn(async move {
                if let Err(error) = axum::serve(listener, page_router(status)).await {
                    tracing::warn!(%error, "the claim page stopped");
                }
            });
        }
        Err(error) => tracing::warn!(
            %error,
            "the claim page could not listen on {CLAIM_PAGE}; the code is in this log instead"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;

    use super::{cloud_from_arguments, config_text};
    use crate::config::EdgeConfig;

    /// The configuration a claimed box writes loads as the store it was claimed for, dialling the
    /// cloud that claimed it, with its database beside the file.
    #[test]
    fn the_written_configuration_loads_as_the_claimed_store() {
        let store = StoreId::new(Ulid::from_u128(0x5708E));
        let cloud = url::Url::parse("https://pos.example.vn/").expect("a URL");
        let text = config_text(store, &cloud, Path::new("/var/lib/pos-edge/config.toml"));
        let config = EdgeConfig::from_toml_str(&text).expect("the file loads");
        assert_eq!(config.store_id, store);
        assert_eq!(
            config.cloud_url.as_ref().map(url::Url::as_str),
            Some("https://pos.example.vn/")
        );
        assert_eq!(
            config.store_path,
            Path::new("/var/lib/pos-edge/store.sqlite")
        );
    }

    /// Only an https cloud is accepted, as everywhere else the edge dials one.
    #[test]
    fn the_cloud_must_be_https() {
        let parse = |value: &str| cloud_from_arguments(&["--cloud".to_owned(), value.to_owned()]);
        assert!(parse("https://pos.example.vn").is_ok());
        assert!(parse("http://pos.example.vn").is_err());
        assert!(parse("pos.example.vn").is_err());
        assert!(cloud_from_arguments(&[]).is_err());
    }
}
