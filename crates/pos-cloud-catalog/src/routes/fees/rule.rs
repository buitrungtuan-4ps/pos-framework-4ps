// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One fee rule as a write carries it: the body, its checks, and the rule it builds.

use std::collections::BTreeSet;

use axum::response::Response;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, ulid_refusal};
use pos_proto::enums::SalesChannel;
use pos_proto::fees::{FeeCode, FeeItems, FeeKind, FeeTax, FeeViolation, PublishedFee};
use pos_proto::ids::{FeeId, MenuItemId, TaxClassId};
use pos_proto::money::{Money, Ratio};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;
use pos_proto::{ErrorStatus, WireEnum};
use serde::Deserialize;

use crate::catalog::ItemCategoryId;
use crate::fees::FeeScope;
use crate::routes::{FieldRefusal, clean_name_translations, enum_refusal};

/// A `POST` or `PUT /admin/fees` body: one rule, at one scope.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct FeeRuleRequest {
    pub(super) tenant_id: String,
    /// `FEE_SCOPE_TENANT`, `_BRAND` or `_STORE`.
    pub(super) scope: String,
    /// The tenant, brand or store the rule is for.
    pub(super) scope_id: String,
    pub(super) rule: FeeRuleBody,
}

/// A rule as the console sends it: the wire rule's own fields, with each id as text so a malformed
/// one is refused by name, and the item categories an include or exclude list names besides its
/// items. A field left out takes the default the owner confirmed: after discounts, net of tax,
/// taxed the way its lines are, not waivable, on every channel and every item, and active.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the wire rule's four independent flags, each named on the wire and each with its own \
              default; deserialised by name and never built from positional booleans"
)]
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct FeeRuleBody {
    /// The fee: absent on a `POST`, which mints its id, and the fee written on a `PUT`.
    #[serde(default)]
    pub(super) fee_id: Option<String>,
    code: String,
    display_name: String,
    #[serde(default)]
    display_name_translations: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    kind: Open<FeeKind>,
    #[serde(default)]
    rate: Option<Ratio>,
    #[serde(default)]
    amount: Option<Money>,
    #[serde(default)]
    channels: Vec<Open<SalesChannel>>,
    #[serde(default)]
    item_scope: Open<FeeItems>,
    #[serde(default)]
    menu_item_ids: Vec<String>,
    #[serde(default)]
    item_category_ids: Vec<String>,
    #[serde(default = "authored_fee_base_is_discounted")]
    base_discounted: bool,
    #[serde(default)]
    base_tax_inclusive: bool,
    #[serde(default)]
    tax: Open<FeeTax>,
    #[serde(default)]
    tax_class_id: Option<String>,
    #[serde(default)]
    waivable: bool,
    #[serde(default = "authored_fee_is_active")]
    active: bool,
}

/// The default for [`FeeRuleBody::base_discounted`]: a percentage is taken after discounts and
/// comps, as the owner confirmed.
const fn authored_fee_base_is_discounted() -> bool {
    true
}

/// The default for [`FeeRuleBody::active`]: a rule written is a rule that applies.
const fn authored_fee_is_active() -> bool {
    true
}

/// A rule as a request writes it, checked field by field: every refusal the rule alone decides,
/// before anything is read. `fee_id` is the caller's on a `PUT` and minted on a `POST`.
///
/// A field the rule's kind, item scope or tax treatment ignores is dropped rather than kept, so the
/// stored rule is the rule that takes effect: an amount on a percentage, a rate or a base on an
/// amount, an item list on a fee for every line, a tax class on a fee not taxed at one. A token the
/// wire tolerates is refused here, because authoring one is a mistake: an unknown channel matches
/// nothing, and an unknown tax treatment would be read as the default.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
pub(super) fn build_fee_rule(
    body: &FeeRuleBody,
    fee_id: FeeId,
) -> Result<(PublishedFee, Vec<ItemCategoryId>), Response> {
    check_fee_text_and_tokens(body)?;
    let code = body.code.trim();
    let display_name = body.display_name.trim();
    let menu_item_ids = parse_id_list(&body.menu_item_ids, "rule.menu_item_ids", MenuItemId::new)?;
    let item_category_ids = parse_id_list(
        &body.item_category_ids,
        "rule.item_category_ids",
        ItemCategoryId::new,
    )?;
    let tax_class_id = match body.tax_class_id.as_deref() {
        None => None,
        Some(text) => match parse_ulid_fields([("rule.tax_class_id", text)]) {
            Ok([id]) => Some(TaxClassId::new(id)),
            Err(refusal) => return Err(refusal),
        },
    };

    let percent = !body.kind.is_unrecognised() && body.kind.known() == FeeKind::Percent;
    let item_scope = if body.item_scope.is_unspecified() && !body.item_scope.is_unrecognised() {
        Open::from_known(FeeItems::All)
    } else {
        body.item_scope.clone()
    };
    let listed = !item_scope.is_unrecognised()
        && matches!(item_scope.known(), FeeItems::Include | FeeItems::Exclude);
    let tax = if body.tax.is_unspecified() {
        Open::from_known(FeeTax::FollowLines)
    } else {
        body.tax.clone()
    };
    let mut channels = body.channels.clone();
    channels.sort_by(|left, right| left.as_wire().cmp(right.as_wire()));
    channels.dedup();
    let rule = PublishedFee {
        fee_id,
        code: FeeCode::new(code),
        display_name: DisplayName::new(display_name),
        display_name_translations: clean_name_translations(body.display_name_translations.clone())
            .into_iter()
            .map(|(locale, name)| (locale, DisplayName::new(name)))
            .collect(),
        kind: body.kind.clone(),
        rate: if percent { body.rate } else { None },
        amount: if percent { None } else { body.amount },
        channels,
        item_scope,
        menu_item_ids: if listed { menu_item_ids } else { Vec::new() },
        base_discounted: !percent || body.base_discounted,
        base_tax_inclusive: percent && body.base_tax_inclusive,
        tax_class_id: if tax.known() == FeeTax::TaxClass {
            tax_class_id
        } else {
            None
        },
        tax,
        waivable: body.waivable,
        active: body.active,
    };
    let item_category_ids = if listed {
        item_category_ids
    } else {
        Vec::new()
    };
    // An include or exclude list may be categories alone, which compile into items when the rule
    // is published, so the wire rule's own list may be empty here.
    let refusals: Vec<FeeViolation> = rule
        .violations()
        .into_iter()
        .filter(|violation| {
            *violation != FeeViolation::EmptyItemList || item_category_ids.is_empty()
        })
        .collect();
    if refusals.is_empty() {
        Ok((rule, item_category_ids))
    } else {
        Err(fee_violations_refusal(&refusals))
    }
}

/// Refuses a rule whose code or name is blank, or which names a channel or a tax treatment this
/// cloud does not know.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn check_fee_text_and_tokens(body: &FeeRuleBody) -> Result<(), Response> {
    if body.code.trim().is_empty() {
        return Err(FieldRefusal::new("code is required", "rule.code", "REQUIRED").into_response());
    }
    if body.display_name.trim().is_empty() {
        return Err(
            FieldRefusal::new("display_name is required", "rule.display_name", "REQUIRED")
                .into_response(),
        );
    }
    if body.channels.iter().any(Open::is_unspecified) {
        return Err(FieldRefusal::new(
            "a fee names an unknown sales channel",
            "rule.channels",
            "INVALID_ENUM_VALUE",
        )
        .into_response());
    }
    if body.tax.is_unrecognised() {
        return Err(enum_refusal(
            "rule.tax",
            FeeTax::ALL
                .iter()
                .filter(|tax| **tax != FeeTax::Unspecified)
                .map(|tax| tax.as_wire()),
        ));
    }
    Ok(())
}

/// The `400` for a rule whose shape pos-proto's checks refuse: every fault, each naming its field.
fn fee_violations_refusal(violations: &[FeeViolation]) -> Response {
    let message: Vec<String> = violations.iter().map(ToString::to_string).collect();
    let details: Vec<(&str, &str)> = violations
        .iter()
        .map(|violation| fee_violation_detail(*violation))
        .collect();
    api_error_with_details(ErrorStatus::InvalidArgument, message.join("; "), &details)
}

/// The field a fault in a rule's shape is about, and the reason a refusal gives for it.
const fn fee_violation_detail(violation: FeeViolation) -> (&'static str, &'static str) {
    match violation {
        FeeViolation::UnknownKind => ("rule.kind", "INVALID_ENUM_VALUE"),
        FeeViolation::MissingRate => ("rule.rate", "REQUIRED"),
        FeeViolation::RateOutOfRange => ("rule.rate", "OUT_OF_RANGE"),
        FeeViolation::MissingAmount => ("rule.amount", "REQUIRED"),
        FeeViolation::NegativeAmount => ("rule.amount", "OUT_OF_RANGE"),
        FeeViolation::MissingTaxClass => ("rule.tax_class_id", "REQUIRED"),
        FeeViolation::EmptyItemList => ("rule.menu_item_ids", "REQUIRED"),
        FeeViolation::UnknownItemScope => ("rule.item_scope", "INVALID_ENUM_VALUE"),
    }
}

/// Parses a list of ids sent as text, refusing the list by name if one is not a ULID. Each id is
/// kept once, in id order.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn parse_id_list<T: Ord>(
    texts: &[String],
    field: &str,
    wrap: impl Fn(Ulid) -> T,
) -> Result<Vec<T>, Response> {
    let mut ids = BTreeSet::new();
    for text in texts {
        match text.trim().parse::<Ulid>() {
            Ok(id) => {
                ids.insert(wrap(id));
            }
            Err(_ignored) => return Err(ulid_refusal(&[field])),
        }
    }
    Ok(ids.into_iter().collect())
}

/// Reads a scope token, refusing anything but the three a rule is written at.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
pub(super) fn parse_fee_scope(token: &str) -> Result<FeeScope, Response> {
    match FeeScope::from_wire(token.trim()) {
        Some(FeeScope::Unspecified) | None => Err(enum_refusal(
            "scope",
            FeeScope::WRITABLE.iter().map(|scope| scope.as_wire()),
        )),
        Some(scope) => Ok(scope),
    }
}
