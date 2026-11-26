// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The numbers a vendor gave the invoices a store issued "number pending"
//! ([ADR-0251](../../../../docs/adr/0251-a-vendor-numbered-invoice-waits-for-its-number-and-the-store-learns-it.md)
//! decision 4).
//!
//! The cloud writes each on the store's own `einvoice` node, as it writes the symbol, and the edge
//! learns it at its next config pull. No personal data: a bill, a number and the vendor's
//! reference. Every statement of law behind it is to be confirmed by legal or the accountant.

use serde::{Deserialize, Deserializer, Serialize};

use crate::ids::BillId;

/// The most numbers the node holds; past it the oldest is dropped (ADR-0251 decision 4).
pub const VENDOR_NUMBERS_MAX: usize = 200;

/// A number or reference as a vendor gave it, such as `1C26MAA/00000042`: it names an invoice and
/// nobody, so it may appear in an event.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(transparent)]
pub struct VendorCode(String);

impl VendorCode {
    /// The code as the vendor gave it.
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }

    /// The code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The number a vendor gave one bill's invoice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct VendorNumber {
    /// The bill whose invoice it numbers.
    pub bill_id: BillId,
    /// The number, as the vendor gave it.
    pub invoice_number: VendorCode,
    /// The vendor's own reference for it, where it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_reference: Option<VendorCode>,
}

/// Reads the node's `vendor_numbers` leniently: an entry this release cannot read is dropped rather
/// than refusing the whole node, and at most [`VENDOR_NUMBERS_MAX`] are kept, the newest last.
pub(super) fn numbers<'de, D>(deserializer: D) -> Result<Vec<VendorNumber>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    let mut read: Vec<VendorNumber> = raw
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| VendorNumber::deserialize(entry).ok())
        .filter(|entry| !entry.invoice_number.as_str().trim().is_empty())
        .collect();
    let excess = read.len().saturating_sub(VENDOR_NUMBERS_MAX);
    read.drain(..excess);
    Ok(read)
}

// The personal-data barrier for the part, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(VendorNumber {
    bill_id,
    invoice_number,
    authority_reference,
});

#[cfg(test)]
mod tests {
    use super::{VENDOR_NUMBERS_MAX, VendorNumber};
    use crate::einvoice::PublishedEInvoice;

    #[test]
    fn the_node_reads_its_vendor_numbers_leniently_and_keeps_the_newest() {
        let mut entries: Vec<serde_json::Value> = (1..=VENDOR_NUMBERS_MAX + 2)
            .map(|index| {
                serde_json::json!({
                    "bill_id": format!("01KB{index:022}"),
                    "invoice_number": format!("1C26MAA/{index:08}"),
                })
            })
            .collect();
        entries.push(serde_json::json!({ "bill_id": "not a bill", "invoice_number": "x" }));
        entries.push(
            serde_json::json!({ "bill_id": "01KB0000000000000000000000", "invoice_number": " " }),
        );
        let node: PublishedEInvoice =
            serde_json::from_value(serde_json::json!({ "vendor_numbers": entries }))
                .unwrap_or_default();
        assert_eq!(node.vendor_numbers.len(), VENDOR_NUMBERS_MAX);
        assert_eq!(
            node.vendor_numbers
                .last()
                .map(|entry: &VendorNumber| entry.invoice_number.as_str()),
            Some("1C26MAA/00000202")
        );
    }
}
