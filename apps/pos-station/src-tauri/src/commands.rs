// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The four commands, and the only pages that may call them.
//!
//! `capabilities/local-pages.json` grants these to the `connect` and `status` windows and to
//! nothing else, and `build.rs` declares them in an app manifest so that grant is **required** —
//! without the manifest, any window showing a bundled page could call them all. The till is a remote
//! page (the edge's own URL) in a window no capability names, so it can invoke nothing (ADR-0147).
//! Each command also checks the calling window and its URL itself, so a mistake in the capability
//! file fails closed.
//!
//! All four are `async` so none runs on the main thread: the station's glue hops to the main thread
//! for windows and the tray, and a command already there would wait on itself.

use serde::Serialize;
use tauri::{AppHandle, Webview};

use crate::address::{self, AddressError, PairingCode};
use crate::edge::{EdgeClient, PairRefusal};
use crate::i18n::Lang;
use crate::station::{self, ConnectContext, StatusView};
use crate::vault;

/// A refusal the page shows: a key into `ui/i18n.js` and, for a rate limit, when to retry.
#[derive(Debug, Serialize)]
pub(crate) struct Refusal {
    key: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_seconds: Option<u64>,
}

impl Refusal {
    fn key(key: &'static str) -> Self {
        Self {
            key,
            retry_after_seconds: None,
        }
    }
}

/// Every key the commands can answer with. Each must exist in both languages in `ui/i18n.js`; a test
/// below reads that file and holds it to this list.
#[cfg(test)]
const REFUSAL_KEYS: &[&str] = &[
    NOT_ALLOWED,
    ADDRESS_EMPTY,
    ADDRESS_SCHEME,
    ADDRESS_CREDENTIALS,
    ADDRESS_HOST,
    ADDRESS_PORT,
    CODE_MISSING,
    CODE_FORMAT,
    CODE_REJECTED,
    TOO_MANY,
    UNAVAILABLE,
    UNREACHABLE,
    UNEXPECTED,
];

const NOT_ALLOWED: &str = "error.not_allowed";
const ADDRESS_EMPTY: &str = "connect.error.address_empty";
const ADDRESS_SCHEME: &str = "connect.error.address_scheme";
const ADDRESS_CREDENTIALS: &str = "connect.error.address_credentials";
const ADDRESS_HOST: &str = "connect.error.address_host";
const ADDRESS_PORT: &str = "connect.error.address_port";
const CODE_MISSING: &str = "connect.error.code_missing";
const CODE_FORMAT: &str = "connect.error.code_format";
const CODE_REJECTED: &str = "connect.error.code_rejected";
const TOO_MANY: &str = "connect.error.too_many";
const UNAVAILABLE: &str = "connect.error.unavailable";
const UNREACHABLE: &str = "connect.error.unreachable";
const UNEXPECTED: &str = "error.unexpected";

/// Refuses a call from anything but one of `allowed`, showing a bundled page.
fn local_page(webview: &Webview, allowed: &[&str]) -> Result<(), Refusal> {
    let from_allowed_window = allowed.contains(&webview.label());
    let from_bundled_page = webview.url().is_ok_and(|url| station::is_local_url(&url));
    if from_allowed_window && from_bundled_page {
        Ok(())
    } else {
        log::warn!("refused a command from window {}", webview.label());
        Err(Refusal::key(NOT_ALLOWED))
    }
}

/// The bundled pages report the webview's language (`navigator.language`) so the tray and the
/// notifications speak it too.
#[tauri::command]
pub(crate) async fn set_language(
    app: AppHandle,
    webview: Webview,
    language: String,
) -> Result<(), Refusal> {
    local_page(&webview, &["connect", "status"])?;
    station::set_language(&app, Lang::from_tag(&language));
    Ok(())
}

/// What the connect page prefills.
#[tauri::command]
pub(crate) async fn connect_context(
    app: AppHandle,
    webview: Webview,
) -> Result<ConnectContext, Refusal> {
    local_page(&webview, &["connect"])?;
    Ok(station::connect_context(&app))
}

/// Pairs with the edge at `address` using `code` — or the code carried by a pasted pairing link —
/// keeps the token in the OS credential store, and opens the till already paired.
///
/// A credential store that refuses the token does not fail the pairing. The code is single-use and
/// already spent by then, so refusing would cost the operator a new code and leave the till closed;
/// instead the app keeps the token for this run, opens the till, and the status page says the
/// pairing was not saved. That is the till's own rule (`credentials.ts`): a device that cannot
/// persist its token pairs again next session, which is degraded rather than broken.
#[tauri::command]
pub(crate) async fn pair(
    app: AppHandle,
    webview: Webview,
    address: String,
    code: String,
) -> Result<(), Refusal> {
    local_page(&webview, &["connect"])?;
    let input = address::parse(&address).map_err(|error| {
        Refusal::key(match error {
            AddressError::Empty => ADDRESS_EMPTY,
            AddressError::UnsupportedScheme => ADDRESS_SCHEME,
            AddressError::Credentials => ADDRESS_CREDENTIALS,
            AddressError::MissingHost | AddressError::BadHost => ADDRESS_HOST,
            AddressError::BadPort => ADDRESS_PORT,
        })
    })?;
    let code = if code.trim().is_empty() {
        input.code_from_link.ok_or(Refusal::key(CODE_MISSING))?
    } else {
        PairingCode::parse(&code).ok_or(Refusal::key(CODE_FORMAT))?
    };
    let origin = input.origin;
    let (origin, token, saved) = tauri::async_runtime::spawn_blocking(move || {
        let token = EdgeClient::new()
            .pair(&origin, &code)
            .map_err(|refusal| match refusal {
                PairRefusal::BadCode => Refusal::key(CODE_FORMAT),
                PairRefusal::Rejected => Refusal::key(CODE_REJECTED),
                PairRefusal::TooManyAttempts(retry_after_seconds) => Refusal {
                    key: TOO_MANY,
                    retry_after_seconds,
                },
                PairRefusal::Unavailable => Refusal::key(UNAVAILABLE),
                PairRefusal::Failed(error) => {
                    log::warn!("pairing with {origin}: {error}");
                    Refusal::key(UNREACHABLE)
                }
            })?;
        let saved = match vault::save(&origin.to_string(), &token) {
            Ok(()) => true,
            Err(error) => {
                log::error!("{error}; the pairing lasts until the app quits");
                false
            }
        };
        Ok::<_, Refusal>((origin, token, saved))
    })
    .await
    .map_err(|error| {
        log::error!("the pairing task failed: {error}");
        Refusal::key(UNEXPECTED)
    })??;
    station::paired(&app, &origin, token, saved);
    Ok(())
}

/// What the status page draws.
#[tauri::command]
pub(crate) async fn status(app: AppHandle, webview: Webview) -> Result<StatusView, Refusal> {
    local_page(&webview, &["status"])?;
    Ok(station::status_view(&app))
}

#[cfg(test)]
mod tests {
    use super::REFUSAL_KEYS;
    use crate::station::NOTICE_PAIRING_LOST;

    /// The page's own string table, which must hold every key the commands can send it.
    const PAGE_STRINGS: &str = include_str!("../../ui/i18n.js");

    #[test]
    fn every_key_a_command_sends_is_translated_in_both_languages() {
        for key in REFUSAL_KEYS.iter().chain([&NOTICE_PAIRING_LOST]) {
            let quoted = format!("\"{key}\":");
            assert_eq!(
                PAGE_STRINGS.matches(&quoted).count(),
                2,
                "{key} must appear once under en and once under vi in ui/i18n.js"
            );
        }
    }
}
