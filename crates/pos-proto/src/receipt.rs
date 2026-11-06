// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `receipt` config node: what a store prints above and below the legal body of a
//! receipt, per sales channel
//! ([ADR-0207](../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)).
//!
//! A template is structure, not a language (decision 3): translation keys for the lines above and
//! below, which optional sections print, which custom fields print under the header, and which
//! documents count as the person's own copy. No free text is ever stored here. The legal body (the
//! seller, the receipt number, the lines, the totals, the tax lines and a captured buyer) stays in
//! code, and a template cannot turn it off.
//!
//! The node is one setting of the register, `receipt.templates` ([`crate::settings`]), so it is
//! authored at the tenant, a brand, a store group or one store, and the most specific scope wins
//! (decision 1). A store whose node holds no template prints exactly what it printed before.
//!
//! [`check_templates`] is the structural rule the register applies when a value is written: the
//! limits of decision 7 and the shape. Whether a header or footer key is in the tenant's
//! translation grid with an `en` value, and whether a section a country requires is left on, are
//! the cloud's checks, because they need the grid and the country.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::enums::SalesChannel;
use crate::ids::CustomFieldId;
use crate::text::TranslationKey;
use crate::wire_enum;
use crate::wire_enum::{Open, WireEnum};

/// The most translation keys a template prints above the legal body, and the most below it.
pub const KEYS_MAX: usize = 4;

/// The most custom fields one template prints.
pub const CUSTOM_FIELDS_MAX: usize = 64;

/// The most per-channel templates a node holds.
pub const CHANNEL_TEMPLATES_MAX: usize = 16;

/// The largest node the cloud writes, in bytes of its JSON.
pub const NODE_BYTES_MAX: usize = 16 * 1024;

wire_enum! {
    /// A section of a receipt a template may leave out (ADR-0207 decision 3). Every other section is
    /// the legal body and always prints. A country module states which of these its law requires,
    /// and the cloud refuses a template that turns one of those off.
    ReceiptSection, prefix = "RECEIPT_SECTION";
    /// The modifiers under each item row.
    Modifiers = "MODIFIERS",
    /// The payments the bill was settled with.
    Payments = "PAYMENTS",
    /// The change handed back.
    ChangeLine = "CHANGE_LINE",
    /// The second language the `printing` node chooses.
    SecondLanguage = "SECOND_LANGUAGE",
}

wire_enum! {
    /// A document that may count as the copy that belongs to the person a personal value is about
    /// (ADR-0207 decision 5, ADR-0206 decision 9). A personal value prints only on such a copy.
    PersonCopy, prefix = "PERSON_COPY";
    /// A custom record's own slip.
    RecordSlip = "RECORD_SLIP",
    /// The buyer's copy of a tax invoice: the receipt of a bill whose buyer was captured.
    TaxInvoiceBuyer = "TAX_INVOICE_BUYER",
}

/// Which optional sections print. An absent switch is on, because every section printed before
/// templates existed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReceiptSections {
    /// The modifiers under each item row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub modifiers: Option<bool>,
    /// The payments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub payments: Option<bool>,
    /// The change line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub change_line: Option<bool>,
    /// The second language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub second_language: Option<bool>,
}

impl ReceiptSections {
    /// Whether `section` prints: on unless the template turns it off. `RECEIPT_SECTION_UNSPECIFIED`
    /// names no section, and reads as on.
    #[must_use]
    pub fn is_on(&self, section: ReceiptSection) -> bool {
        let switch = match section {
            ReceiptSection::Unspecified => None,
            ReceiptSection::Modifiers => self.modifiers,
            ReceiptSection::Payments => self.payments,
            ReceiptSection::ChangeLine => self.change_line,
            ReceiptSection::SecondLanguage => self.second_language,
        };
        switch.unwrap_or(true)
    }

    /// The sections the template turns off, in the enum's order.
    #[must_use]
    pub fn turned_off(&self) -> Vec<ReceiptSection> {
        ReceiptSection::ALL
            .iter()
            .copied()
            .filter(|section| *section != ReceiptSection::Unspecified && !self.is_on(*section))
            .collect()
    }
}

/// One template: what prints above and below a receipt's legal body (ADR-0207 decision 3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReceiptTemplate {
    /// At most [`KEYS_MAX`] translation keys, each a line above the legal body, in the receipt's
    /// language and then `en`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_keys: Vec<TranslationKey>,
    /// At most [`KEYS_MAX`] translation keys, each a line below it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footer_keys: Vec<TranslationKey>,
    /// Which optional sections print.
    #[serde(default, skip_serializing_if = "is_default")]
    pub sections: ReceiptSections,
    /// The custom fields that print under the header, in order, each as its label and value: at
    /// most [`CUSTOM_FIELDS_MAX`], none twice. A field the running package does not declare is
    /// skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_field_ids: Vec<CustomFieldId>,
    /// The documents this template counts as the person's own copy, on which a personal value
    /// prints (ADR-0207 decision 5). Empty: a personal value prints on none of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub person_copy: Vec<Open<PersonCopy>>,
}

/// Whether `sections` is the default, so a template that turns nothing off leaves it off the wire.
fn is_default(sections: &ReceiptSections) -> bool {
    *sections == ReceiptSections::default()
}

impl ReceiptTemplate {
    /// Whether the template counts `document` as the person's own copy. A token from a newer
    /// release counts as no document this release prints.
    #[must_use]
    pub fn is_person_copy(&self, document: PersonCopy) -> bool {
        document != PersonCopy::Unspecified
            && self.person_copy.iter().any(|copy| copy.known() == document)
    }
}

/// The node's one setting: a base template and at most one per sales channel (ADR-0207 decision 2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReceiptTemplates {
    /// The template an order's channel falls back to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub base: Option<ReceiptTemplate>,
    /// A template per sales channel, keyed by its `SALES_CHANNEL_*` token: at most
    /// [`CHANNEL_TEMPLATES_MAX`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, ReceiptTemplate>,
}

impl ReceiptTemplates {
    /// The template an order on `channel` prints with: the channel's own, then the base, then none.
    /// A channel this release does not know is `SALES_CHANNEL_UNSPECIFIED`, which no template is
    /// written for, so it falls back to the base.
    #[must_use]
    pub fn for_channel(&self, channel: SalesChannel) -> Option<&ReceiptTemplate> {
        self.channels
            .get(channel.as_wire())
            .filter(|_| channel != SalesChannel::Unspecified)
            .or(self.base.as_ref())
    }
}

/// The `receipt` node.
///
/// No `deny_unknown_fields`, as for every published node: an edge on an older release applies a node
/// that carries a field it does not know, rather than refusing the whole document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublishedReceipt {
    /// The templates. Absent, every receipt prints as it did before templates existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub templates: Option<ReceiptTemplates>,
}

impl PublishedReceipt {
    /// The node's key in a store's configuration document.
    pub const NODE: &'static str = "receipt";

    /// The template an order on `channel` prints with, or `None` to print as before.
    #[must_use]
    pub fn template_for(&self, channel: SalesChannel) -> Option<&ReceiptTemplate> {
        self.templates
            .as_ref()
            .and_then(|templates| templates.for_channel(channel))
    }
}

/// Why a written value is not a set of templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TemplateRefusal {
    /// Its shape is not a template's: a field the node does not have, a value of the wrong type,
    /// a channel or a document this release does not know, or a custom field named twice.
    #[error("it is not a set of receipt templates")]
    NotATemplate,
    /// It breaks a limit of ADR-0207 decision 7: too many keys, custom fields or channel templates,
    /// or more than [`NODE_BYTES_MAX`] bytes.
    #[error("it is over a receipt template's limits")]
    OverLimit,
}

/// The fields a [`ReceiptTemplates`] value may carry, and those of one [`ReceiptTemplate`] and of
/// its [`ReceiptSections`]. Exact on purpose: no field the node does not have is stored, so no free
/// text reaches it.
const TEMPLATES_FIELDS: &[&str] = &["base", "channels"];
const TEMPLATE_FIELDS: &[&str] = &[
    "header_keys",
    "footer_keys",
    "sections",
    "custom_field_ids",
    "person_copy",
];
const SECTION_FIELDS: &[&str] = &["modifiers", "payments", "change_line", "second_language"];

/// Checks a written `receipt.templates` value and returns it read.
///
/// An object with only the node's fields, each of its type. Every channel key is a known
/// `SALES_CHANNEL_*` token other than `UNSPECIFIED`, and every `person_copy` a known `PERSON_COPY_*`
/// token, none twice. Within the limits: at most [`KEYS_MAX`] header and footer keys each, none
/// empty; at most [`CUSTOM_FIELDS_MAX`] custom fields, none twice; at most
/// [`CHANNEL_TEMPLATES_MAX`] channel templates; at most [`NODE_BYTES_MAX`] bytes.
///
/// # Errors
///
/// [`TemplateRefusal`] naming what is wrong.
pub fn check_templates(value: &serde_json::Value) -> Result<ReceiptTemplates, TemplateRefusal> {
    if serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len()) > NODE_BYTES_MAX {
        return Err(TemplateRefusal::OverLimit);
    }
    let object = only_fields(value, TEMPLATES_FIELDS)?;
    if let Some(base) = object.get("base") {
        check_template(base)?;
    }
    if let Some(channels) = object.get("channels") {
        let channels = channels.as_object().ok_or(TemplateRefusal::NotATemplate)?;
        if channels.len() > CHANNEL_TEMPLATES_MAX {
            return Err(TemplateRefusal::OverLimit);
        }
        for (token, template) in channels {
            if SalesChannel::from_wire(token)
                .is_none_or(|channel| channel == SalesChannel::Unspecified)
            {
                return Err(TemplateRefusal::NotATemplate);
            }
            check_template(template)?;
        }
    }
    serde_json::from_value(value.clone()).map_err(|_| TemplateRefusal::NotATemplate)
}

/// The object `value` is, when it carries only `fields`.
fn only_fields<'a>(
    value: &'a serde_json::Value,
    fields: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>, TemplateRefusal> {
    let object = value.as_object().ok_or(TemplateRefusal::NotATemplate)?;
    if object.keys().all(|key| fields.contains(&key.as_str())) {
        Ok(object)
    } else {
        Err(TemplateRefusal::NotATemplate)
    }
}

/// Checks one template's shape and limits.
fn check_template(value: &serde_json::Value) -> Result<(), TemplateRefusal> {
    let object = only_fields(value, TEMPLATE_FIELDS)?;
    for keys in ["header_keys", "footer_keys"] {
        let Some(keys) = object.get(keys) else {
            continue;
        };
        let keys = keys.as_array().ok_or(TemplateRefusal::NotATemplate)?;
        if keys.len() > KEYS_MAX {
            return Err(TemplateRefusal::OverLimit);
        }
        if !keys
            .iter()
            .all(|key| key.as_str().is_some_and(|key| !key.trim().is_empty()))
        {
            return Err(TemplateRefusal::NotATemplate);
        }
    }
    if let Some(sections) = object.get("sections") {
        let sections = only_fields(sections, SECTION_FIELDS)?;
        if !sections.values().all(serde_json::Value::is_boolean) {
            return Err(TemplateRefusal::NotATemplate);
        }
    }
    if let Some(fields) = object.get("custom_field_ids") {
        let fields = fields.as_array().ok_or(TemplateRefusal::NotATemplate)?;
        if fields.len() > CUSTOM_FIELDS_MAX {
            return Err(TemplateRefusal::OverLimit);
        }
        distinct(fields, |field| {
            field
                .as_str()
                .is_some_and(|text| text.parse::<CustomFieldId>().is_ok())
        })?;
    }
    if let Some(copies) = object.get("person_copy") {
        let copies = copies.as_array().ok_or(TemplateRefusal::NotATemplate)?;
        distinct(copies, |copy| {
            copy.as_str()
                .and_then(PersonCopy::from_wire)
                .is_some_and(|copy| copy != PersonCopy::Unspecified)
        })?;
    }
    Ok(())
}

/// Every item passes `valid`, and none is there twice.
fn distinct(
    items: &[serde_json::Value],
    valid: impl Fn(&serde_json::Value) -> bool,
) -> Result<(), TemplateRefusal> {
    let ok = items.iter().enumerate().all(|(index, item)| {
        valid(item) && !items.iter().take(index).any(|earlier| earlier == item)
    });
    ok.then_some(()).ok_or(TemplateRefusal::NotATemplate)
}

#[cfg(test)]
mod tests;
