// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `integrations` config node
//! ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decision 5): what a
//! store needs to know about its tenant's vendor connections, and nothing more.
//!
//! A tenant configures a *connection* in the cloud — a provider, the scope it serves, its settings
//! and its sealed secrets. The cloud resolves which connections serve one store (its own, its
//! brand's, its tenant's) and publishes only this: per connection, its family, its provider and a
//! name to show. A store never receives a secret, because it never talks to an internet vendor: the
//! cloud does, over the channel the store already uses (decision 1).
//!
//! # Settings travel only for a driver the store runs itself
//!
//! A card terminal is a device on the shop's network, and only the edge can reach it, so a
//! connection in [`IntegrationFamily::CardTerminal`] carries the **non-secret** settings its driver
//! needs — an address, a terminal number. Every other family is driven by the cloud, and its
//! settings stay there: a store has no use for a gateway's endpoint, and a node that carried one
//! would be one more place a tenant's configuration leaks from a stolen box.
//!
//! **Never-blank, opt-in**, as for `devices`: an *absent* node means no online integration — every
//! family runs its offline path, which is how every store runs today (an invoice is issued from the
//! local range, a QR transfer is confirmed by hand). A *present* node is authoritative.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::text::DisplayName;
use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// A kind of external system a store works with, each behind its own port.
    IntegrationFamily, prefix = "INTEGRATION_FAMILY";
    /// Electronic tax invoices, submitted to a licensed provider.
    EInvoice = "E_INVOICE",
    /// QR and wallet payments confirmed by a gateway.
    QrPayment = "QR_PAYMENT",
    /// Card terminals on the store's own network — the one family the edge drives itself.
    CardTerminal = "CARD_TERMINAL",
    /// Delivery marketplaces that send orders in.
    Delivery = "DELIVERY",
    /// Couriers that take an order out.
    Courier = "COURIER",
    /// Accounting and ERP systems the day's figures are posted to.
    Erp = "ERP",
}

/// A non-secret setting's value, as the operator typed it: the plain JSON its schema's kind implies.
///
/// Untagged, so `true`, `30` and `"192.0.2.40"` are the values themselves. Whole numbers only — a
/// setting is an address, a port, a count or a timeout, never an amount, and money is never a
/// float anywhere in this workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingValue {
    /// A flag.
    Flag(bool),
    /// A whole number.
    Number(i64),
    /// Text, a URL, or one of a fixed set of choices.
    Text(String),
}

/// One connection that serves this store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedIntegration {
    /// The connection's id in the cloud, a ULID. What a store names when it asks the cloud to act
    /// through this connection, so a switch of vendor mid-request cannot send the request to the
    /// wrong one.
    pub connection_id: String,
    /// Which family it serves. An `Open`, so a family a newer cloud adds is carried rather than
    /// failing the node — and a store that does not know it simply does not use it.
    pub family: Open<IntegrationFamily>,
    /// The provider, as `<family>.<vendor>`. Opaque here: which driver answers to it is the edge's
    /// registry's question, and a provider id this build has no driver for is not an error in the
    /// node.
    pub provider_id: String,
    /// What the connection is called in the console, for a till that has to say which vendor it is
    /// using or which one failed.
    pub display_name: DisplayName,
    /// The non-secret settings a store-run driver needs. Empty for every family the cloud drives —
    /// see the module documentation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, SettingValue>,
}

/// The `integrations` config node: the connections that serve this store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedIntegrations {
    /// The connections, in the order the cloud resolved them: the store's own before its brand's
    /// before its tenant's, so the first in a family is the most specific.
    #[serde(default)]
    integrations: Vec<PublishedIntegration>,
}

impl PublishedIntegrations {
    /// A node listing exactly `integrations`.
    #[must_use]
    pub fn new(integrations: Vec<PublishedIntegration>) -> Self {
        Self { integrations }
    }

    /// Every connection the node lists, in resolution order.
    #[must_use]
    pub fn integrations(&self) -> &[PublishedIntegration] {
        &self.integrations
    }

    /// The connections serving one family, most specific first. A family this build does not know
    /// is `UNSPECIFIED` here and matches nothing, not even a caller asking for `UNSPECIFIED`.
    pub fn in_family(
        &self,
        family: IntegrationFamily,
    ) -> impl Iterator<Item = &PublishedIntegration> + '_ {
        self.integrations.iter().filter(move |integration| {
            !integration.family.is_unspecified() && integration.family.known() == family
        })
    }

    /// Whether the node lists no connection at all — every family on its offline path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.integrations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{IntegrationFamily, PublishedIntegration, PublishedIntegrations, SettingValue};
    use crate::text::DisplayName;
    use crate::wire_enum::WireEnum;

    fn terminal() -> PublishedIntegration {
        PublishedIntegration {
            connection_id: "01J0000000000000000000CONN".to_owned(),
            family: IntegrationFamily::CardTerminal.into(),
            provider_id: "card.sandbox".to_owned(),
            display_name: DisplayName::new("Counter terminal"),
            settings: BTreeMap::from([
                (
                    "address".to_owned(),
                    SettingValue::Text("192.0.2.40".to_owned()),
                ),
                ("port".to_owned(), SettingValue::Number(8800)),
                ("tipping".to_owned(), SettingValue::Flag(false)),
            ]),
        }
    }

    fn einvoice() -> PublishedIntegration {
        PublishedIntegration {
            connection_id: "01J0000000000000000000EINV".to_owned(),
            family: IntegrationFamily::EInvoice.into(),
            provider_id: "einvoice.sandbox".to_owned(),
            display_name: DisplayName::new("E-invoices"),
            settings: BTreeMap::new(),
        }
    }

    #[test]
    fn a_family_is_its_prefixed_token_on_the_wire() {
        assert_eq!(
            IntegrationFamily::CardTerminal.as_wire(),
            "INTEGRATION_FAMILY_CARD_TERMINAL"
        );
        assert_eq!(
            IntegrationFamily::EInvoice.as_wire(),
            "INTEGRATION_FAMILY_E_INVOICE"
        );
    }

    #[test]
    fn a_cloud_driven_connection_carries_no_settings_and_says_so_by_omission() {
        let node = PublishedIntegrations::new(vec![einvoice(), terminal()]);
        let json = serde_json::to_value(&node).expect("json");
        assert!(
            json.pointer("/integrations/0/settings").is_none(),
            "a family the cloud drives sends nothing but its identity: {json}"
        );
        assert_eq!(
            json.pointer("/integrations/1/settings/port"),
            Some(&serde_json::json!(8800))
        );
        assert_eq!(
            json.pointer("/integrations/1/settings/address"),
            Some(&serde_json::json!("192.0.2.40"))
        );
        let back: PublishedIntegrations = serde_json::from_value(json).expect("round trip");
        assert_eq!(back, node);
    }

    #[test]
    fn a_family_this_build_does_not_know_is_carried_and_matches_nothing() {
        let raw = r#"{"integrations":[
            {"connection_id":"01J0000000000000000000FUTR","family":"INTEGRATION_FAMILY_LOYALTY",
             "provider_id":"loyalty.acme","display_name":"Points"}
        ]}"#;
        let node: PublishedIntegrations = serde_json::from_str(raw).expect("an unknown family");
        assert_eq!(node.integrations().len(), 1);
        for family in [
            IntegrationFamily::Unspecified,
            IntegrationFamily::EInvoice,
            IntegrationFamily::QrPayment,
            IntegrationFamily::CardTerminal,
        ] {
            assert_eq!(node.in_family(family).count(), 0);
        }
    }

    #[test]
    fn an_absent_list_is_an_empty_node_and_each_family_reads_its_own() {
        let empty: PublishedIntegrations = serde_json::from_str("{}").expect("an empty node");
        assert!(empty.is_empty());
        let node = PublishedIntegrations::new(vec![einvoice(), terminal()]);
        let terminals: Vec<&str> = node
            .in_family(IntegrationFamily::CardTerminal)
            .map(|integration| integration.provider_id.as_str())
            .collect();
        assert_eq!(terminals, vec!["card.sandbox"]);
    }

    #[test]
    fn a_setting_is_the_plain_value_and_a_fraction_is_refused() {
        let value: SettingValue = serde_json::from_str("30").expect("a number");
        assert_eq!(value, SettingValue::Number(30));
        assert!(
            serde_json::from_str::<SettingValue>("1.5").is_err(),
            "a setting is never a fraction"
        );
    }
}
