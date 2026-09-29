// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a vendor is to the rest of the system: a catalogue entry its own adapter describes
//! ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)).
//!
//! # One description, read by everyone who needs it
//!
//! An adapter crate exports one [`ProviderDescriptor`]: its id, its family, the countries it
//! serves, and the **settings schema** a tenant fills in to use it. The cloud builds a [`Registry`]
//! from the descriptors compiled into it, and so does the edge, one line per adapter. The console
//! draws its connection form from the schema, so there is no screen per vendor, and the cloud
//! checks what an operator typed against the same schema before it stores anything
//! ([`ProviderDescriptor::check`]).
//!
//! That is what makes "add a vendor" one crate and one line: the crate implements the family's
//! port and a descriptor, the line registers it, and nothing that reads the catalogue changes.
//!
//! # Why this is not `pos-ports`
//!
//! A port is a boundary the domain calls through. A descriptor is a description of whoever sits on
//! the far side of one: metadata for a registry, a form and a validator, never called by the core.
//! Putting it in the port crate would make every change to a form a change to a backbone crate.
//! It sits beside `pos-country` instead, which plays the same part for countries.
//!
//! # What this crate does not hold
//!
//! Secret *values*. A descriptor names which fields are secrets so the cloud can seal them and the
//! console can draw a write-only input; the values themselves live only in the cloud, sealed
//! (ADR-0153 decision 4), and nothing here can print one.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use pos_proto::locale::CountryCode;

pub mod fields;
mod registry;
mod settings;

pub use registry::{Registry, RegistryError};
pub use settings::{FieldKind, SettingError, SettingField, SettingValue, Settings};

/// A kind of external system the store works with, each with its own port.
///
/// The family is the first half of every [`ProviderDescriptor::provider_id`] (`einvoice.sandbox`),
/// so a stored id says what it is without a second column to disagree with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    /// Electronic tax invoices, submitted to a licensed provider.
    EInvoice,
    /// QR and wallet payments confirmed by a gateway.
    QrPayment,
    /// Card terminals on the store's own network.
    CardTerminal,
    /// Delivery marketplaces that send orders in.
    Delivery,
    /// Couriers that take an order out.
    Courier,
    /// Accounting and ERP systems that receive the day's figures.
    Erp,
}

impl Family {
    /// Every family, in the order the console lists them.
    pub const ALL: [Self; 6] = [
        Self::EInvoice,
        Self::QrPayment,
        Self::CardTerminal,
        Self::Delivery,
        Self::Courier,
        Self::Erp,
    ];

    /// The family's stable key: the prefix of its providers' ids, and what a database column holds.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::EInvoice => "einvoice",
            Self::QrPayment => "qr",
            Self::CardTerminal => "card",
            Self::Delivery => "delivery",
            Self::Courier => "courier",
            Self::Erp => "erp",
        }
    }

    /// Parses a key, or `None` for one this build does not know.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|family| family.key() == key)
    }

    /// The family as the admin API spells it, `UPPER_SNAKE_CASE` like every enum on the wire
    /// (`docs/naming-and-api.md` §3.3). Distinct from [`Self::key`], which is the prefix of an id.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::EInvoice => "INTEGRATION_FAMILY_E_INVOICE",
            Self::QrPayment => "INTEGRATION_FAMILY_QR_PAYMENT",
            Self::CardTerminal => "INTEGRATION_FAMILY_CARD_TERMINAL",
            Self::Delivery => "INTEGRATION_FAMILY_DELIVERY",
            Self::Courier => "INTEGRATION_FAMILY_COURIER",
            Self::Erp => "INTEGRATION_FAMILY_ERP",
        }
    }

    /// Where this family's adapter runs (ADR-0153 decision 1): beside the vendor.
    ///
    /// A card terminal is a device on the shop's network, and only the edge can reach it. Every
    /// other family is a service on the internet, and the cloud talks to it so the store never
    /// holds the vendor's credentials.
    #[must_use]
    pub const fn runs_on(self) -> Runtime {
        match self {
            Self::CardTerminal => Runtime::Edge,
            Self::EInvoice | Self::QrPayment | Self::Delivery | Self::Courier | Self::Erp => {
                Runtime::Cloud
            }
        }
    }
}

/// Which binary drives a family's adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// The store's edge, through a *driver*.
    Edge,
    /// The cloud, through a *connector*.
    Cloud,
}

impl Runtime {
    /// The runtime as the admin API spells it.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Edge => "PROVIDER_RUNTIME_EDGE",
            Self::Cloud => "PROVIDER_RUNTIME_CLOUD",
        }
    }
}

/// One vendor, as its adapter describes itself.
///
/// Everything is `'static` because a descriptor is a constant in its adapter crate: a registry is
/// built at startup, before configuration has arrived, and building it must not be able to fail for
/// any reason but a programming error the registry's own checks name.
#[derive(Debug)]
pub struct ProviderDescriptor {
    /// `<family>.<vendor>`, lowercase: stored in configuration and **never renamed**. A connection
    /// names its provider by this id, so renaming one orphans every connection that used it.
    pub provider_id: &'static str,
    /// The family this provider belongs to. Must match the id's prefix; the registry refuses one
    /// that does not.
    pub family: Family,
    /// The translation key for the provider's name, in the console's catalogue.
    pub name_key: &'static str,
    /// The countries the provider serves. Empty means every country, which is what a sandbox says.
    pub countries: &'static [CountryCode],
    /// The settings a tenant fills in, in the order the form shows them.
    pub fields: &'static [SettingField],
    /// What this provider can do within its family, as keys the family's code understands — an
    /// e-invoice provider that can cancel an invoice, against one that can only issue an adjustment.
    pub capabilities: &'static [&'static str],
    /// Whether this is a sandbox: deterministic, reachable without a vendor account, and never a
    /// place a real legal submission or payment goes (ADR-0153 decision 6). The console marks it.
    pub sandbox: bool,
}

impl ProviderDescriptor {
    /// Whether this provider serves `country`.
    #[must_use]
    pub fn serves(&self, country: CountryCode) -> bool {
        self.countries.is_empty() || self.countries.contains(&country)
    }

    /// The field with this key, if the schema has one.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&'static SettingField> {
        self.fields.iter().find(|field| field.key == key)
    }

    /// Whether this provider declares a capability.
    #[must_use]
    pub fn can(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability)
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::locale::CountryCode;

    use super::{Family, ProviderDescriptor, Runtime};

    const VIETNAM_ONLY: ProviderDescriptor = ProviderDescriptor {
        provider_id: "einvoice.example",
        family: Family::EInvoice,
        name_key: "provider.einvoice.example",
        countries: &[CountryCode::VN],
        fields: &[],
        capabilities: &["cancel"],
        sandbox: false,
    };

    #[test]
    fn a_family_key_round_trips_and_an_unknown_one_is_none() {
        for family in Family::ALL {
            assert_eq!(Family::from_key(family.key()), Some(family));
        }
        assert_eq!(Family::from_key("fax"), None);
    }

    #[test]
    fn only_the_device_on_the_shops_network_runs_on_the_edge() {
        let on_edge: Vec<Family> = Family::ALL
            .into_iter()
            .filter(|family| family.runs_on() == Runtime::Edge)
            .collect();
        assert_eq!(on_edge, vec![Family::CardTerminal]);
    }

    #[test]
    fn a_provider_serves_its_countries_and_an_empty_list_serves_all() {
        assert!(VIETNAM_ONLY.serves(CountryCode::VN));
        assert!(!VIETNAM_ONLY.serves(CountryCode::JP));
        let anywhere = ProviderDescriptor {
            countries: &[],
            ..VIETNAM_ONLY
        };
        assert!(anywhere.serves(CountryCode::JP));
        assert!(VIETNAM_ONLY.can("cancel"));
        assert!(!VIETNAM_ONLY.can("refund"));
    }
}
