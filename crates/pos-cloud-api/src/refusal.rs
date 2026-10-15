// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The refusals a route sends for a request it cannot act on, and the reads of request fields
//! that produce them: a closed-set token, an optional id, a field a builder refused, a composed
//! document that does not validate, and a named thing that is not there.
//!
//! Moved out of `pos-cloud`'s `http.rs` (ADR-0170 decision 4), where every business area's
//! routes shared them. `pos-cloud`'s `http` module imports them, so `crate::http::not_found` and the
//! rest still name them there.

use axum::response::Response;
use pos_proto::ErrorStatus;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::WireEnum;

use crate::response::{api_error, api_error_with_details};

/// The tokens a caller may actually send for `E`: every wire token except `UNSPECIFIED`.
///
/// `WireEnum::ALL` leads with `UNSPECIFIED`, which exists so an older client can read a newer
/// server's value and is never a choice a caller makes. Listing it as accepted would invite one.
pub fn accepted_tokens<E: WireEnum>() -> impl Iterator<Item = &'static str> {
    E::ALL
        .iter()
        .copied()
        .filter(|value| *value != E::UNSPECIFIED)
        .map(WireEnum::as_wire)
}

/// A choice a request names by its wire token: `None` where it names none, which leaves the choice
/// to whatever stands for it, or a token of `E` this release knows.
///
/// # Errors
///
/// A `400 INVALID_ENUM_VALUE` naming `field` for a token this release does not know, and for `E`'s
/// `UNSPECIFIED`, which is what an older reader takes a newer token to be and never a choice.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
pub fn optional_choice<E: WireEnum>(
    field: &str,
    token: Option<&str>,
) -> Result<Option<E>, Response> {
    let Some(token) = token else {
        return Ok(None);
    };
    match E::from_wire(token) {
        Some(known) if known != E::UNSPECIFIED => Ok(Some(known)),
        _ => Err(enum_refusal(field, accepted_tokens::<E>())),
    }
}

/// Parses an optional id field: an absent field, an empty string, or whitespace is `Ok(None)`; a
/// present value must be a ULID or the caller gets a `400` naming the field.
///
/// **The only place that rule is written.** There were seven typed wrappers around this, and two of
/// them — `brand_id` on a store and `parent_menu_id` on a menu — matched on `Some(text)` without
/// trimming or filtering, so an empty string meant "malformed" for those two fields and "unset" for
/// the other five. Whether `""` clears an optional reference is not a per-field question, and a
/// caller cannot be expected to know which of two identical-looking fields it is asking. Clearing a
/// select box sends `""`, and the console only avoided the `400` because every call site happened to
/// map it to `null` first — one screen forgetting that would have produced a refusal an operator
/// could not act on. So the wrappers are gone and every caller passes its own constructor
/// (`BrandId::new`, `MenuId::new`, …) here.
pub fn parse_optional_ulid<T>(
    value: Option<&str>,
    wrap: impl Fn(Ulid) -> T,
) -> Result<Option<T>, ()> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => text
            .parse::<Ulid>()
            .map(|ulid| Some(wrap(ulid)))
            .map_err(|_ignored| ()),
    }
}

/// The `422` a composed document earns when every field in it is valid and the document they make
/// together is not ([ADR-0096](../../../docs/adr/0096-unprocessable-status.md)).
///
/// The violations arrive from `pos-core` as prose a person can read — "the routing rule for station
/// `S01` names a backup station that does not exist" — not as field paths, so they join into
/// `message` rather than becoming `details`. Inventing a `field` for them would be fabricating
/// structure the domain never produced, and a client that branched on the fabricated `reason` would
/// be branching on a guess.
///
/// Separated from [`api_error`] so the four sites that report violations cannot each pick their own
/// separator: before this they could not even agree on a *body*, which is how the fifth shape —
/// the translation grid's — ended up with no reader in the console at all.
pub fn unprocessable_violations(violations: &[String]) -> Response {
    api_error(ErrorStatus::Unprocessable, violations.join("; "))
}

/// Why a request-to-record builder refused, and which field to send the caller to.
///
/// The four builders returned a bare `&'static str`, and their own doc comments said why: a
/// `Result<_, Response>` trips `clippy::result_large_err`. That is a real constraint and this
/// respects it — three `&'static str`s is 48 bytes, well under the threshold — while fixing what
/// the bare string cost, which is that a refusal naming no field is one no console can mark.
///
/// `field` carries the full path for a nested value (`action.rate`, not `rate`), following
/// ADR-0096's reasoning that a path a reader can act on beats a name they have to interpret.
#[derive(Clone, Copy)]
pub struct FieldRefusal {
    message: &'static str,
    field: &'static str,
    reason: &'static str,
}

impl FieldRefusal {
    pub const fn new(message: &'static str, field: &'static str, reason: &'static str) -> Self {
        Self {
            message,
            field,
            reason,
        }
    }

    /// The `400` this refusal becomes at the handler that called the builder.
    pub fn into_response(self) -> Response {
        api_error_with_details(
            ErrorStatus::InvalidArgument,
            self.message,
            &[(self.field, self.reason)],
        )
    }
}

/// The refusal for a field whose value is outside a **closed set**: `INVALID_ARGUMENT`, an
/// `INVALID_ENUM_VALUE` detail naming the field, and prose listing what is accepted.
///
/// The set is passed in rather than spelled into the message, so every caller derives it from the
/// enum that owns it — `EntityStatus::ALL`, `AdminRole::ALL`, `AdminStatus::ALL`,
/// `ConfigLevel::ORDER`. Adding a variant then updates the refusal, where before it would have left
/// a sentence listing the old set: `"status must be active or archived"` was written out at eighteen
/// routes, so a third status would have needed eighteen edits and got none.
///
/// The prose it builds matches what those sites already said, so only the `details` array is new.
pub fn enum_refusal<'token>(
    field: &str,
    accepted: impl IntoIterator<Item = &'token str>,
) -> Response {
    let tokens: Vec<&str> = accepted.into_iter().collect();
    api_error_with_details(
        ErrorStatus::InvalidArgument,
        format!("{field} must be {}", accepted_list(&tokens)),
        &[(field, "INVALID_ENUM_VALUE")],
    )
}

/// Reads a closed set as English: `"active or archived"`, `"owner, admin, ops, or viewer"`.
///
/// Separate from [`enum_refusal`] because this is the part with arities to get wrong, and a `String`
/// can be asserted where a `Response` body needs an async read. Both spellings match what the
/// hand-written sentences used, which is why generating them changed no message.
pub fn accepted_list(tokens: &[&str]) -> String {
    match tokens {
        [only] => (*only).to_owned(),
        [first, last] => format!("{first} or {last}"),
        [head @ .., last] => format!("{}, or {last}", head.join(", ")),
        // Unreachable: a closed set with nothing in it accepts nothing, so no value could be
        // refused *against* it.
        [] => "one of the accepted values".to_owned(),
    }
}

/// The refusal for a named thing that is not there: `NOT_FOUND`, `"no such {entity}"`, no details.
///
/// Sixty-one sites wrote this sentence out, thirty-five spellings of it, and the value of one home
/// here is smaller than for the ULID and closed-set refusals: nothing was *wrong*, only repeated.
/// What it does buy is that the next absent entity cannot arrive with a different phrasing or a
/// different status.
///
/// No `details` array, deliberately. `details` names a field the caller got wrong, and a caller
/// asking after an employee who does not exist got its fields right — the employee is absent.
/// Naming one here would send a client to fix an input that was fine.
pub fn not_found(entity: &str) -> Response {
    api_error(ErrorStatus::NotFound, format!("no such {entity}"))
}

/// A query param that was actually sent: absent, empty, and whitespace all read as absent.
///
/// The same rule `parse_optional_ulid` applies to optional id fields (#280), for the same reason —
/// clearing a form control sends `""`, and a caller that sent nothing meaningful should be treated as
/// having sent nothing.
pub fn present_param(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}
