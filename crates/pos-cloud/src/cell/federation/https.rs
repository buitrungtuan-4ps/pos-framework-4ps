// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`RollupFetcher`] over HTTPS: another cell's public
//! `GET /v1/stores/{store_id}/rollups/daily`, with the key that cell issued
//! ([ADR-0179](../../../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C10).
//!
//! Every call is bounded: the URL is vetted against SSRF before it is dialed, the call has
//! [`FETCH_TIMEOUT`], and the body is cut at [`MAX_ROLLUP_BYTES`]. It follows **no redirect**: a
//! `read_rollups` key is sent to the cell the signed directory names and nowhere else, so a cell that
//! answers with a redirect is an error, not a hop.

use core::time::Duration;

use hyper::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use pos_cloud_reports::types::DailyRollup;

use super::{RollupFetcher, RollupWindow, SourceKey};
use crate::outbound::{Answer, HttpsGet};

/// How long one store's read may take, end to end.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest answer read. A year of daily rollups with a few dozen event types is tens of
/// kilobytes; 1 MiB is far past any honest answer.
const MAX_ROLLUP_BYTES: usize = 1024 * 1024;

/// Who is calling, in the `User-Agent`.
const USER_AGENT_VALUE: &str = "pos-cloud-federation";

/// Reads rollups from other cells over HTTPS.
#[derive(Clone)]
pub struct HttpsRollupFetcher {
    client: HttpsGet,
}

// Hand-written: the client holds a TLS config, which says nothing useful, and no key is ever here.
impl core::fmt::Debug for HttpsRollupFetcher {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("HttpsRollupFetcher")
    }
}

impl Default for HttpsRollupFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpsRollupFetcher {
    /// A fetcher over the shared HTTPS client.
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: HttpsGet::new(),
        }
    }
}

impl RollupFetcher for HttpsRollupFetcher {
    async fn fetch(
        &self,
        base_url: &str,
        store_id: &str,
        key: &SourceKey,
        window: &RollupWindow,
    ) -> Result<Vec<DailyRollup>, String> {
        let url = rollups_url(base_url, store_id, window)?;
        let headers = [
            (USER_AGENT, USER_AGENT_VALUE.to_owned()),
            (ACCEPT, "application/json".to_owned()),
            (AUTHORIZATION, format!("Bearer {}", key.expose())),
        ];
        let call = self
            .client
            .get_once(&url, &headers, MAX_ROLLUP_BYTES, base_url);
        match tokio::time::timeout(FETCH_TIMEOUT, call).await {
            Ok(Ok(answer)) => read_answer(base_url, &answer),
            Ok(Err(why)) => Err(why),
            Err(_elapsed) => Err(format!(
                "{base_url} did not answer within {} seconds",
                FETCH_TIMEOUT.as_secs()
            )),
        }
    }
}

/// The `/v1` rollups URL for one store, with the window as its query.
///
/// # Errors
///
/// When the store id is not a ULID or a date is not `YYYY-MM-DD`: nothing that is not one of those
/// reaches a URL.
pub(crate) fn rollups_url(
    base_url: &str,
    store_id: &str,
    window: &RollupWindow,
) -> Result<String, String> {
    if store_id.parse::<pos_proto::ulid::Ulid>().is_err() {
        return Err(format!("the store {store_id:?} is not a ULID"));
    }
    let mut query = Vec::new();
    for (name, date) in [("from", &window.from), ("to", &window.to)] {
        if let Some(date) = date {
            if !is_date(date) {
                return Err(format!("{name} {date:?} is not a YYYY-MM-DD date"));
            }
            query.push(format!("{name}={date}"));
        }
    }
    let mut url = format!(
        "{}/v1/stores/{store_id}/rollups/daily",
        base_url.trim_end_matches('/')
    );
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    Ok(url)
}

/// `YYYY-MM-DD`, by shape: the cell that answers checks it is a real day.
pub(crate) fn is_date(text: &str) -> bool {
    text.len() == 10
        && text.char_indices().all(|(at, character)| match at {
            4 | 7 => character == '-',
            _ => character.is_ascii_digit(),
        })
}

/// What a cell's answer means: its rollups on a `200`, and a sentence an operator can act on
/// otherwise. A redirect is refused, never followed.
pub(crate) fn read_answer(base_url: &str, answer: &Answer) -> Result<Vec<DailyRollup>, String> {
    match answer.status {
        200 => serde_json::from_slice(&answer.body)
            .map_err(|error| format!("{base_url} answered rollups this cell cannot read: {error}")),
        300..=399 => Err(format!(
            "{base_url} answered with a redirect{}, which is not followed: a read_rollups key goes \
             only to the cell the directory names",
            answer
                .location
                .as_deref()
                .map(|to| format!(" to {to}"))
                .unwrap_or_default()
        )),
        401 => Err(format!(
            "{base_url} does not accept this key: ask that cell's operator for a new one"
        )),
        403 => Err(format!(
            "{base_url} refused: the key lacks the read_rollups scope, or the store is not in its \
             tenant"
        )),
        status => Err(format!("{base_url} answered {status}")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{RollupWindow, read_answer, rollups_url};
    use crate::outbound::Answer;

    const STORE: &str = "01M2219QK4T3W6Z0Y8FBQ2X5MV";
    const BASE: &str = "https://jp1.example.com";

    fn answer(status: u16, body: &str, location: Option<&str>) -> Answer {
        Answer {
            status,
            location: location.map(str::to_owned),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn the_url_is_the_public_v1_route_with_the_window() {
        assert_eq!(
            rollups_url(BASE, STORE, &RollupWindow::default()).as_deref(),
            Ok("https://jp1.example.com/v1/stores/01M2219QK4T3W6Z0Y8FBQ2X5MV/rollups/daily")
        );
        let window = RollupWindow {
            from: Some("2026-10-01".to_owned()),
            to: Some("2026-10-27".to_owned()),
        };
        assert_eq!(
            rollups_url("https://jp1.example.com/", STORE, &window).as_deref(),
            Ok(
                "https://jp1.example.com/v1/stores/01M2219QK4T3W6Z0Y8FBQ2X5MV/rollups/daily?from=2026-10-01&to=2026-10-27"
            )
        );
        let only_to = RollupWindow {
            from: None,
            to: Some("2026-10-27".to_owned()),
        };
        assert!(
            rollups_url(BASE, STORE, &only_to)
                .expect("a url")
                .ends_with("daily?to=2026-10-27")
        );
    }

    #[test]
    fn nothing_but_a_ulid_and_dates_reaches_the_url() {
        assert!(rollups_url(BASE, "../admin", &RollupWindow::default()).is_err());
        for bad in [
            "2026-10-1",
            "2026/10/01",
            "2026-10-01&x=1",
            "yyyy-mm-dd",
            "",
        ] {
            let window = RollupWindow {
                from: Some(bad.to_owned()),
                to: None,
            };
            assert!(rollups_url(BASE, STORE, &window).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_200_is_read_as_rollups() {
        let read = read_answer(
            BASE,
            &answer(
                200,
                r#"[{"business_date":"2026-10-27","total_events":3,"by_type":{"sales.order.created":3}}]"#,
                None,
            ),
        )
        .expect("rollups");
        assert_eq!(read.len(), 1);
        assert_eq!(
            read[0].by_type,
            BTreeMap::from([("sales.order.created".to_owned(), 3)])
        );
        assert!(
            read_answer(BASE, &answer(200, "{not json", None))
                .expect_err("unreadable")
                .contains("cannot read")
        );
    }

    #[test]
    fn a_redirect_is_refused_and_never_followed() {
        let why = read_answer(BASE, &answer(302, "", Some("https://elsewhere.example")))
            .expect_err("refused");
        assert!(why.contains("not followed"), "{why}");
        assert!(why.contains("https://elsewhere.example"), "{why}");
        assert!(read_answer(BASE, &answer(307, "", None)).is_err());
    }

    #[test]
    fn a_refusal_says_what_to_do() {
        assert!(
            read_answer(BASE, &answer(401, "", None))
                .expect_err("401")
                .contains("ask that cell's operator")
        );
        assert!(
            read_answer(BASE, &answer(403, "", None))
                .expect_err("403")
                .contains("read_rollups")
        );
        assert_eq!(
            read_answer(BASE, &answer(503, "", None)).expect_err("503"),
            "https://jp1.example.com answered 503"
        );
    }
}
