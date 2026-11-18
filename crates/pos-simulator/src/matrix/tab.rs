// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bar tab (ADR-0221): a store that keeps tabs opens one and every till finds it by its number;
//! a store that does not is refused.

use serde_json::{Value, json};

use super::store::Till;

/// Opens a tab where the template keeps tabs and finds it in the list; elsewhere, checks the
/// open is refused.
pub(super) async fn tab(till: &Till) -> Result<(), (&'static str, String)> {
    let opened = till
        .send("POST", "/api/orders", Some(json!({ "tab": true })))
        .await;
    if !till.tabs() {
        return match opened {
            Err(refused) if refused.contains("CAPABILITY_DISABLED") => Ok(()),
            other => Err((
                "open a tab",
                format!("a store without tabs opened one: {other:?}"),
            )),
        };
    }
    let opened = opened.map_err(|detail| ("open a tab", detail))?;
    let tabs = till
        .send("GET", "/api/tabs", None)
        .await
        .map_err(|detail| ("list the tabs", detail))?;
    let listed: Vec<&Value> = tabs.as_array().into_iter().flatten().collect();
    match listed.as_slice() {
        [tab]
            if tab["order_id"] == opened["order_id"]
                && tab["tab_number"] == opened["queue_number"] =>
        {
            Ok(())
        }
        _ => Err((
            "list the tabs",
            format!("the tab {opened} is not the one listed: {tabs}"),
        )),
    }
}
