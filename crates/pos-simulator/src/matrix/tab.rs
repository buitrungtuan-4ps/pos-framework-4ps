// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bar tab (ADR-0221): a store that keeps tabs opens one and every till finds it by its number,
//! and one nothing was sold on closes with the store's reason; a store that does not is refused.

use serde_json::{Value, json};

use super::sale::reason_for;
use super::store::Till;

/// Opens a tab where the template keeps tabs, finds it in the list and closes it empty with a
/// listed reason; elsewhere, checks the open is refused.
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
                && tab["tab_number"] == opened["queue_number"] => {}
        _ => {
            return Err((
                "list the tabs",
                format!("the tab {opened} is not the one listed: {tabs}"),
            ));
        }
    }
    let reason_code_id =
        reason_for(till, "REASON_ACTION_CLOSE_ORDER", "close an empty tab").await?;
    let order_id = opened["order_id"].as_str().unwrap_or_default();
    till.send(
        "POST",
        &format!("/api/orders/{order_id}/close"),
        Some(json!({ "reason_code_id": reason_code_id })),
    )
    .await
    .map_err(|detail| ("close an empty tab", detail))?;
    let left = till
        .send("GET", "/api/tabs", None)
        .await
        .map_err(|detail| ("list the tabs", detail))?;
    if left.as_array().is_some_and(Vec::is_empty) {
        Ok(())
    } else {
        Err(("close an empty tab", format!("still listed: {left}")))
    }
}
