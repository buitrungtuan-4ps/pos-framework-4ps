// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /v1/qr/menu`: what a guest at a table may order
//! ([ADR-0224](../../../../docs/adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) decision 1).
//!
//! The signed table token is the credential, as it is for the intake beside it, and the same
//! guardrails apply: QR ordering's switch, the store being online, its business hours, and a
//! per-table rate limit. The limit counts reads on a counter of their own, with the store's
//! `per_table_limit` and window, so a guest reading the menu never spends the table's order budget.
//!
//! The answer is the store's published menu for the QR channel and nothing more: each item's
//! name in the guest's language, its price, whether it can be sold, and the modifier groups it asks
//! about. Tax classes, codes and courses are the store's business and stay off the guest's wire.
//! The request carries no personal data, and none is read or answered.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_ports::order_in::OrderIn;
use pos_proto::determinism::ClockSource;
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{MenuItemId, ModifierGroupId};
use pos_proto::text::DisplayName;
use pos_proto::{MenuBook, MenuCatalog, Money, SalesChannel};

use super::{QrState, qr_config_from, rejection_response, store_effective};
use crate::config_tree::ConfigTreeStore;
use crate::http::api_error;
use crate::qr::{QrDecision, QrFacts, QrRejection, verify_table_token};

/// The longest language tag a guest may ask for: BCP 47's own ceiling for a well-formed tag.
const MAX_LANGUAGE_LEN: usize = 35;

/// The guest's query: the table token printed in the QR code, and the language they read.
#[derive(Debug, Deserialize)]
pub(super) struct MenuQuery {
    /// The HMAC-signed table token.
    table_token: String,
    /// A BCP 47 language tag. Absent or malformed, the store's display language answers.
    #[serde(default)]
    lang: Option<String>,
}

/// The menu a guest reads.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct GuestMenu {
    /// The language the names are in: the one asked for, else the store's, else `en`.
    language: String,
    /// The languages the menu carries names in, sorted, for the page's language switch.
    languages: Vec<String>,
    /// The items, in the menu's order.
    items: Vec<GuestItem>,
    /// The modifier groups any item asks about.
    modifier_groups: Vec<GuestModifierGroup>,
}

/// One item as the guest sees it.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct GuestItem {
    menu_item_id: MenuItemId,
    name: DisplayName,
    /// Integer money in the currency's minor unit, as every price is.
    unit_price: Money,
    /// Whether it can be ordered right now.
    available: bool,
    modifier_group_ids: Vec<ModifierGroupId>,
}

/// One modifier group as the guest sees it.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct GuestModifierGroup {
    modifier_group_id: ModifierGroupId,
    name: DisplayName,
    min_select: u16,
    max_select: u16,
    member_menu_item_ids: Vec<MenuItemId>,
}

/// `GET /v1/qr/menu?table_token=…&lang=…`.
pub(super) async fn read_qr_menu<X, T, C>(
    State(state): State<QrState<X, T, C>>,
    Query(query): Query<MenuQuery>,
) -> Response
where
    X: OrderIn + Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let Ok(table) = verify_table_token(&state.secret, &query.table_token) else {
        return rejection_response(QrRejection::UntrustedTable);
    };
    let effective = store_effective(&state.config_trees, table.tenant_id, table.store_id).await;
    let config = effective
        .as_ref()
        .map_or_else(Default::default, qr_config_from);
    if !config.enabled {
        return api_error(
            ErrorStatus::NotFound,
            "QR ordering is not enabled for this store",
        );
    }
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let facts = QrFacts {
        token_valid: true,
        store_online: config.store_online,
        within_business_hours: config.within_business_hours(now_ms),
        submissions_in_window: state.menu_limiter.count_in_window(
            table.store_id,
            table.table_id,
            now_ms,
            config.window,
        ),
        per_table_limit: config.per_table_limit,
        staff_confirmation_required: config.staff_confirmation_required,
    };
    if let QrDecision::Reject(reason) = crate::qr::evaluate(&facts) {
        return rejection_response(reason);
    }
    state
        .menu_limiter
        .record(table.store_id, table.table_id, now_ms);
    let document = effective.unwrap_or_default();
    Json(guest_menu(&document, query.lang.as_deref())).into_response()
}

/// The guest's menu from a store's effective document. A `menu` node that does not parse reads as
/// an empty book, which offers nothing rather than guessing a price.
pub(crate) fn guest_menu(effective: &serde_json::Value, asked: Option<&str>) -> GuestMenu {
    let book: MenuBook = effective
        .get("menu")
        .and_then(|node| serde_json::from_value(node.clone()).ok())
        .unwrap_or_default();
    let catalog = book.catalog_for(SalesChannel::Qr);
    let store_language = effective
        .get("locale")
        .and_then(|locale| locale.get("display_language"))
        .and_then(serde_json::Value::as_str)
        .filter(|tag| well_formed(tag));
    let language = asked
        .filter(|tag| well_formed(tag))
        .or(store_language)
        .unwrap_or("en")
        .to_owned();
    let languages = languages_of(catalog, store_language);
    let localized = catalog.localized(&language);
    GuestMenu {
        language,
        languages,
        // An item sold by weight or at a price the cashier types is never offered to a guest, as
        // the kiosk offers neither: nobody at the table weighs it or prices it, and the store would
        // refuse the order (ADR-0223, ADR-0222). Found in the phase-10 demo.
        items: localized
            .items()
            .iter()
            .filter(|entry| !entry.codes.sold_by_weight() && entry.codes.open_price.is_none())
            .map(|entry| GuestItem {
                menu_item_id: entry.menu_item_id,
                name: entry.display_name.clone(),
                unit_price: entry.unit_price,
                available: entry.available,
                modifier_group_ids: entry.modifier_group_ids.clone(),
            })
            .collect(),
        modifier_groups: localized
            .modifier_groups()
            .iter()
            .map(|group| GuestModifierGroup {
                modifier_group_id: group.modifier_group_id,
                name: group.display_name.clone(),
                min_select: group.min_select,
                max_select: group.max_select,
                member_menu_item_ids: group.member_menu_item_ids.clone(),
            })
            .collect(),
    }
}

/// Every language an item name is translated into, with the store's own and `en` (each item's
/// untranslated name is the fallback any of them reads).
fn languages_of(catalog: &MenuCatalog, store_language: Option<&str>) -> Vec<String> {
    let mut languages: BTreeSet<String> = catalog
        .items()
        .iter()
        .flat_map(|entry| entry.display_name_translations.keys())
        .filter(|tag| well_formed(tag))
        .cloned()
        .collect();
    languages.insert("en".to_owned());
    if let Some(tag) = store_language {
        languages.insert(tag.to_owned());
    }
    languages.into_iter().collect()
}

/// Whether `tag` has a language tag's shape: 1 to 35 ASCII letters, digits and hyphens. Anything
/// else a guest sends is ignored rather than echoed.
fn well_formed(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_LANGUAGE_LEN
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::{guest_menu, well_formed};
    use serde_json::json;

    fn document() -> serde_json::Value {
        json!({
            "locale": { "display_language": "vi" },
            "menu": {
                "channels": [{
                    "sales_channel": "SALES_CHANNEL_QR",
                    "catalog": {
                        "items": [{
                            "menu_item_id": "01HZ0000000000000000000001",
                            "display_name": "Margherita",
                            "display_name_translations": { "vi": "Bánh Margherita", "ja": "マルゲリータ" },
                            "unit_price": { "currency_code": "VND", "amount_minor": 189_000 },
                            "tax_class_id": "01HZ00000000000000000000T1",
                            "modifier_group_ids": ["01HZ00000000000000000000G1"],
                            "codes": { "plu": "101" }
                        }, {
                            "menu_item_id": "01HZ0000000000000000000002",
                            "display_name": "Extra cheese",
                            "unit_price": { "currency_code": "VND", "amount_minor": 20_000 },
                            "tax_class_id": "01HZ00000000000000000000T1",
                            "available": false
                        }],
                        "modifier_groups": [{
                            "modifier_group_id": "01HZ00000000000000000000G1",
                            "display_name": "Toppings",
                            "display_name_translations": { "vi": "Thêm" },
                            "min_select": 0,
                            "max_select": 2,
                            "member_menu_item_ids": ["01HZ0000000000000000000002"]
                        }]
                    }
                }]
            }
        })
    }

    #[test]
    fn the_guest_reads_names_in_their_language_and_nothing_the_store_keeps() {
        let menu = serde_json::to_value(guest_menu(&document(), Some("ja"))).expect("encode");
        assert_eq!(menu["language"], "ja");
        assert_eq!(menu["languages"], json!(["en", "ja", "vi"]));
        assert_eq!(menu["items"][0]["name"], "マルゲリータ");
        assert_eq!(menu["items"][0]["unit_price"]["amount_minor"], 189_000);
        assert_eq!(menu["items"][1]["available"], false);
        assert_eq!(menu["modifier_groups"][0]["max_select"], 2);
        // Tax class and codes are the store's, not the guest's.
        let item = menu["items"][0].as_object().expect("an item");
        assert!(!item.contains_key("tax_class_id"));
        assert!(!item.contains_key("codes"));
    }

    #[test]
    fn an_item_sold_by_weight_or_at_a_typed_price_is_not_offered() {
        let mut effective = document();
        let items = effective["menu"]["channels"][0]["catalog"]["items"]
            .as_array_mut()
            .expect("items");
        for (id, codes) in [
            (
                "01HZ0000000000000000000003",
                json!({ "sold_by": "SOLD_BY_WEIGHT" }),
            ),
            (
                "01HZ0000000000000000000004",
                json!({ "open_price": { "minimum": { "currency_code": "VND", "amount_minor": 1_000 }, "maximum": { "currency_code": "VND", "amount_minor": 500_000 } } }),
            ),
        ] {
            items.push(json!({
                "menu_item_id": id,
                "display_name": "Fake by weight or typed price",
                "unit_price": { "currency_code": "VND", "amount_minor": 450_000 },
                "tax_class_id": "01HZ00000000000000000000T1",
                "codes": codes
            }));
        }
        let menu = guest_menu(&effective, None);
        let offered: Vec<String> = menu
            .items
            .iter()
            .map(|item| item.menu_item_id.to_string())
            .collect();
        assert_eq!(
            offered,
            ["01HZ0000000000000000000001", "01HZ0000000000000000000002"]
        );
    }

    #[test]
    fn a_malformed_or_absent_language_reads_the_stores() {
        for asked in [None, Some(""), Some("<script>"), Some(&*"x".repeat(36))] {
            let menu = guest_menu(&document(), asked);
            assert_eq!(menu.language, "vi", "{asked:?}");
        }
        let menu = serde_json::to_value(guest_menu(&document(), None)).expect("encode");
        assert_eq!(menu["items"][0]["name"], "Bánh Margherita");
        assert_eq!(menu["modifier_groups"][0]["name"], "Thêm");
    }

    #[test]
    fn a_store_with_no_menu_or_a_broken_one_offers_nothing() {
        for effective in [json!({}), json!({ "menu": 7 }), json!(null)] {
            let menu = guest_menu(&effective, Some("en"));
            assert!(menu.items.is_empty());
            assert_eq!(menu.language, "en");
        }
    }

    #[test]
    fn a_language_tag_is_letters_digits_and_hyphens() {
        assert!(well_formed("zh-Hant-TW"));
        assert!(!well_formed("en_US"));
        assert!(!well_formed("vi;q=0.9"));
    }
}
