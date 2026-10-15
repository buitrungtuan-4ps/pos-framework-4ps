// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two halves of [`checked_locale_node`](super::checked_locale_node): the fields the console
//! sends, each checked with its own domain constructor, and the facts the store's country pack
//! adds beside them.

use axum::response::Response;

use pos_proto::ErrorStatus;
use pos_proto::locale::CountryCode;
use pos_proto::money::CurrencyCode;

use pos_core::business_date::{CutoffHour, StoreTimeZone};

use super::api_error_with_details;
use super::config_nodes::locale::{PackedFacts, PublishLocaleRequest, checked_denominations};

/// The request's country and currency, parsed, and its cash denominations sorted and deduplicated;
/// or the AIP-193 `400` naming the first field that does not parse.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(super) fn checked_locale_fields(
    request: &PublishLocaleRequest,
) -> Result<(CountryCode, CurrencyCode, Vec<i64>), Response> {
    // Checked first because it is the field this route did not use to have: a caller built against
    // the older shape must be told *that*, rather than being led through the fields it did send.
    let Ok(country) = CountryCode::parse(&request.country_code) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "country_code is required and must be an ISO 3166-1 alpha-2 code — two letters, such \
             as VN or JP. It records which country this store is in, beside its currency and \
             timezone",
            &[("country_code", "INVALID_FORMAT")],
        ));
    };
    let Ok(currency) = CurrencyCode::parse(&request.currency_code) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "currency_code is not a 3-letter code",
            &[("currency_code", "INVALID_FORMAT")],
        ));
    };
    if StoreTimeZone::from_iana_name(&request.timezone).is_err() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "timezone is not a valid IANA name",
            &[("timezone", "INVALID_FORMAT")],
        ));
    }
    if CutoffHour::new(request.cutoff_hour).is_err() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "cutoff_hour must be in 0..=23",
            &[("cutoff_hour", "OUT_OF_RANGE")],
        ));
    }
    let denominations = checked_denominations(request)?;
    Ok((country, currency, denominations))
}

/// Adds to `locale_value` what the store's country pack says rather than the operator: the
/// currency's decimals, how long a personal record is kept, how a number is written, and the
/// country's language. A fact the pack does not have is left out.
pub(super) fn add_country_pack_facts(
    locale_value: &mut serde_json::Value,
    facts: &PackedFacts,
    country: CountryCode,
    currency: CurrencyCode,
) {
    // How many decimals the store's currency has (ADR-0134), which that record says rides this node
    // — and did not. Without it the edge keeps the đồng-shaped zero its bootstrap starts with, and
    // an Indian store draws 26145 paise as `INR 26,145` on the till and prints `INR 26145` on a tax
    // invoice. The front ends each carry a fallback table for the window before a store syncs, and
    // neither one fires: `GET /api/locale` serves the session's zero as though it were an answer,
    // and `0 ?? fallback` is `0`. A published zero beats a correct guess, which is why this is the
    // cloud's to say rather than theirs to infer.
    //
    // Not a field on the request: an operator choosing their own exponent could state that the
    // rupee has three decimals, and ADR-0134 makes the country pack the authority precisely so
    // nobody has to.
    //
    // Omitted for a currency no pack names, which is the "absent leaves what the session has" case
    // `PublishedLocale` was deliberately built with — saying nothing rather than publishing a guess.
    // Unreachable from the console, whose currency picker is filled from this same country list.
    if let Some(exponent) = facts.exponents.get(&currency)
        && let serde_json::Value::Object(map) = locale_value
    {
        map.insert(
            "currency_exponent".to_owned(),
            serde_json::Value::from(*exponent),
        );
    }
    // How long the store keeps a personal record before its own sweep scrubs it
    // ([ADR-0107](../../../../docs/adr/0107-the-buyer-is-a-subject.md)), which the edge has read from
    // this node since that record and which this node has never carried.
    //
    // Harmless so far, and worth saying why rather than implying a breach: every pack,
    // `LocalePack::default` and the edge's bootstrap all say 365, so no store's period is wrong
    // today and the sweep on the edge's ticker really does run. What the gap would have cost is the
    // *next* change — each pack's figure is marked "a default, not a determination", and the day
    // somebody sets a country's real period every store would have gone on forgetting at 365 while
    // the publish looked applied.
    //
    // From the pack rather than from the request, for the reason the exponent is: a retention period
    // answers to a privacy notice, and a form field invites a number that suits the shop rather than
    // the law.
    if let Some(days) = facts.retention_days.get(&country)
        && let serde_json::Value::Object(map) = locale_value
    {
        map.insert(
            "default_retention_days".to_owned(),
            serde_json::Value::from(*days),
        );
    }
    // How the country writes a number
    // ([ADR-0136](../../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)). Compiled into
    // every pack since ADR-0027 and, until now, read by nothing anywhere: the till grouped `en-US`,
    // the receipt grouped with a comma, and Vietnam's own `1.234.567,50` appeared on neither.
    //
    // This release carries it to the store and no surface draws with it yet. The receipt and the
    // till follow separately, because each changes a figure a person is looking at and the receipt's
    // is a tax document.
    //
    // Serialised from the pack's own type, so the shape the edge parses and the shape the cloud
    // sends cannot drift apart into two spellings of the same three fields.
    if let Ok(format) = facts
        .number_formats
        .get(&country)
        .ok_or(())
        .and_then(|format| serde_json::to_value(format).map_err(|_| ()))
        && let serde_json::Value::Object(map) = locale_value
    {
        map.insert("number_format".to_owned(), format);
    }
    // The language of the store's country (ADR-0160), which a receipt set to
    // `RECEIPT_LANGUAGE_COUNTRY` prints in. The cloud's to say for the reason the rest of these are:
    // the edge a store runs carries no country pack of its own.
    if let Some(language) = facts.languages.get(&country)
        && let serde_json::Value::Object(map) = locale_value
    {
        map.insert(
            "country_language".to_owned(),
            serde_json::Value::String(language.clone()),
        );
    }
}
