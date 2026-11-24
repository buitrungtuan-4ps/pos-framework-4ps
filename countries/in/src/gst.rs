// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! India's GST facts about one bill
//! ([ADR-0227](../../../docs/adr/0227-an-indian-invoice-is-registered-through-a-port-and-upi-is-confirmed.md)):
//! where it is supplied, which GSTR-1 section it is filed under, its UPI payment request, and the
//! shape of an HSN or SAC code.
//!
//! Values and pure functions, as everything in a pack is (ADR-0105). Every rule is **to be confirmed
//! by legal or the accountant**, and every threshold is a parameter the caller passes, never a
//! figure fixed here, because India has moved them more than once.

use pos_proto::money::Money;

use crate::is_valid_gstin;

/// The SAC a restaurant service is filed under where an item names none: 9963, accommodation, food
/// and beverage services. To be confirmed by the accountant.
pub const RESTAURANT_SAC: &str = "9963";

/// Whether a sale is within one state or crosses into another, which decides CGST and SGST against
/// IGST (ADR-0227 decision 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceOfSupply {
    /// The buyer is in the seller's state, or names no state: CGST and SGST.
    IntraState,
    /// The buyer's GSTIN names another state: IGST at the full rate.
    InterState,
}

/// The place of supply of a bill from `seller_gstin`, to `buyer_gstin` where the guest gave one.
///
/// The state is a GSTIN's first two digits. Only a bill with a buyer can be inter-state; a buyer
/// whose GSTIN is not well formed, or a seller's that is not, is treated as within the state, which
/// is the restaurant's ordinary case.
#[must_use]
pub fn place_of_supply(seller_gstin: &str, buyer_gstin: Option<&str>) -> PlaceOfSupply {
    let state = |gstin: &str| {
        is_valid_gstin(gstin)
            .then(|| gstin.get(0..2))
            .flatten()
            .map(str::to_owned)
    };
    match (state(seller_gstin), buyer_gstin.and_then(state)) {
        (Some(seller), Some(buyer)) if seller != buyer => PlaceOfSupply::InterState,
        _ => PlaceOfSupply::IntraState,
    }
}

/// The GSTR-1 section an invoice is reported under (ADR-0227 decision 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gstr1Section {
    /// To a registered buyer, invoice by invoice.
    B2b,
    /// To an unregistered buyer in another state, above the large-invoice threshold, invoice by
    /// invoice.
    B2cl,
    /// Every other sale to an unregistered buyer, summarised by state and rate.
    B2cs,
}

/// The section for an invoice of `total`, to a buyer with `buyer_gstin` or none, supplied as
/// `supply`, where `b2cl_threshold` is the large-invoice threshold in force (to be confirmed).
#[must_use]
pub fn gstr1_section(
    buyer_gstin: Option<&str>,
    supply: PlaceOfSupply,
    total: Money,
    b2cl_threshold: Money,
) -> Gstr1Section {
    if buyer_gstin.is_some_and(is_valid_gstin) {
        return Gstr1Section::B2b;
    }
    if supply == PlaceOfSupply::InterState && total.amount_minor > b2cl_threshold.amount_minor {
        Gstr1Section::B2cl
    } else {
        Gstr1Section::B2cs
    }
}

/// Whether `code` is shaped as an HSN or SAC code: four, six or eight digits.
///
/// Shape only, as for a GSTIN; which digits an invoice must show at a turnover is to be confirmed.
#[must_use]
pub fn is_valid_hsn_sac(code: &str) -> bool {
    matches!(code.len(), 4 | 6 | 8) && code.bytes().all(|byte| byte.is_ascii_digit())
}

/// Why a UPI request could not be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpiError {
    /// The payee address is not `handle@provider`.
    PayeeAddress,
    /// The amount is not a positive amount of rupees.
    Amount,
    /// The reference is empty, longer than 35 characters, or not letters and digits.
    Reference,
}

/// The UPI intent a till shows as a QR for `amount` (ADR-0227 decision 2): pay `payee_vpa` the exact
/// amount in rupees, with the bill's own `reference`, so the gateway's confirmation can be matched to
/// the bill. The format follows NPCI's UPI linking specification and is to be confirmed with the
/// chosen gateway.
///
/// # Errors
///
/// The first [`UpiError`] the inputs break.
pub fn upi_intent(payee_vpa: &str, amount: Money, reference: &str) -> Result<String, UpiError> {
    let (handle, provider) = payee_vpa.split_once('@').ok_or(UpiError::PayeeAddress)?;
    let address_part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    };
    if !address_part(handle) || !address_part(provider) {
        return Err(UpiError::PayeeAddress);
    }
    if amount.amount_minor <= 0 {
        return Err(UpiError::Amount);
    }
    if reference.is_empty()
        || reference.len() > 35
        || !reference.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(UpiError::Reference);
    }
    let rupees = amount.amount_minor / 100;
    let paise = amount.amount_minor % 100;
    Ok(format!(
        "upi://pay?pa={payee_vpa}&am={rupees}.{paise:02}&cu=INR&tr={reference}"
    ))
}

#[cfg(test)]
mod tests;
