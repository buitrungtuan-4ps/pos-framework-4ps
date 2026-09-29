// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The providers one binary was built with.
//!
//! A binary lists its adapters' descriptors once, one line each, and everything that asks "which
//! vendors are there?" asks this. A fork that adds a vendor adds its crate and its line; a fork
//! that drops one deletes a line and nothing else.

use core::fmt;
use std::collections::BTreeSet;

use crate::{Family, FieldKind, ProviderDescriptor};

/// The providers compiled into a binary, checked once at startup.
#[derive(Debug)]
pub struct Registry {
    providers: Vec<&'static ProviderDescriptor>,
}

impl Registry {
    /// Builds a registry, refusing a list a later lookup could get wrong.
    ///
    /// # Errors
    ///
    /// The first [`RegistryError`] found. Every one is a programming error in an adapter's
    /// descriptor or in the binary's list, so a binary treats it as a refusal to start: a catalogue
    /// with two providers under one id would store connections that resolve to whichever came
    /// first.
    pub fn new(
        providers: impl IntoIterator<Item = &'static ProviderDescriptor>,
    ) -> Result<Self, RegistryError> {
        let providers: Vec<&'static ProviderDescriptor> = providers.into_iter().collect();
        let mut ids = BTreeSet::new();
        for provider in &providers {
            check_descriptor(provider)?;
            if !ids.insert(provider.provider_id) {
                return Err(RegistryError::Duplicate(provider.provider_id));
            }
        }
        Ok(Self { providers })
    }

    /// The provider with this id, or `None` for one this build does not have — a connection saved
    /// by a newer cloud, or for a vendor a fork dropped.
    #[must_use]
    pub fn get(&self, provider_id: &str) -> Option<&'static ProviderDescriptor> {
        self.providers
            .iter()
            .copied()
            .find(|provider| provider.provider_id == provider_id)
    }

    /// Every provider in a family, in the order the binary listed them.
    pub fn in_family(
        &self,
        family: Family,
    ) -> impl Iterator<Item = &'static ProviderDescriptor> + '_ {
        self.providers
            .iter()
            .copied()
            .filter(move |provider| provider.family == family)
    }

    /// Every provider, in the order the binary listed them.
    pub fn iter(&self) -> impl Iterator<Item = &'static ProviderDescriptor> + '_ {
        self.providers.iter().copied()
    }
}

/// What makes a list of descriptors unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// Two descriptors claim one id.
    Duplicate(&'static str),
    /// An id is not `<family>.<vendor>` in lowercase letters, digits and `_`.
    MalformedId(&'static str),
    /// An id's prefix is not its descriptor's family.
    WrongFamily(&'static str),
    /// A descriptor lists one field key twice; the second would never be read.
    DuplicateField(&'static str, &'static str),
    /// A number field whose range is empty, or a choice with no options: nothing could satisfy it.
    UnsatisfiableField(&'static str, &'static str),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(id) => write!(f, "two providers claim the id {id}"),
            Self::MalformedId(id) => {
                write!(f, "provider id {id} is not <family>.<vendor> in lowercase")
            }
            Self::WrongFamily(id) => write!(f, "provider id {id} does not start with its family"),
            Self::DuplicateField(id, key) => write!(f, "provider {id} lists field {key} twice"),
            Self::UnsatisfiableField(id, key) => {
                write!(
                    f,
                    "provider {id} has field {key}, which no value can satisfy"
                )
            }
        }
    }
}

impl std::error::Error for RegistryError {}

fn check_descriptor(provider: &'static ProviderDescriptor) -> Result<(), RegistryError> {
    let id = provider.provider_id;
    let (family, vendor) = id.split_once('.').ok_or(RegistryError::MalformedId(id))?;
    let segment = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    };
    if !segment(family) || !segment(vendor) {
        return Err(RegistryError::MalformedId(id));
    }
    if family != provider.family.key() {
        return Err(RegistryError::WrongFamily(id));
    }
    let mut keys = BTreeSet::new();
    for field in provider.fields {
        if !keys.insert(field.key) {
            return Err(RegistryError::DuplicateField(id, field.key));
        }
        let satisfiable = match &field.kind {
            FieldKind::Number { min, max } => min <= max,
            FieldKind::Choice { options } => !options.is_empty(),
            FieldKind::Text { max_len } | FieldKind::Secret { max_len } => *max_len > 0,
            FieldKind::Url | FieldKind::Flag => true,
        };
        if !satisfiable {
            return Err(RegistryError::UnsatisfiableField(id, field.key));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Registry, RegistryError};
    use crate::{Family, FieldKind, ProviderDescriptor, SettingField};

    const EINVOICE: ProviderDescriptor = ProviderDescriptor {
        provider_id: "einvoice.example",
        family: Family::EInvoice,
        name_key: "provider.einvoice.example",
        countries: &[],
        fields: &[],
        capabilities: &[],
        sandbox: false,
    };
    const QR: ProviderDescriptor = ProviderDescriptor {
        provider_id: "qr.example",
        family: Family::QrPayment,
        name_key: "provider.qr.example",
        ..EINVOICE
    };
    static SECOND_EINVOICE: ProviderDescriptor = ProviderDescriptor {
        provider_id: "einvoice.other_2",
        ..EINVOICE
    };

    #[test]
    fn a_registry_finds_by_id_and_by_family_in_listed_order() {
        static A: ProviderDescriptor = EINVOICE;
        static B: ProviderDescriptor = QR;
        let registry = Registry::new([&A, &B, &SECOND_EINVOICE]).expect("a valid list");
        assert_eq!(
            registry.get("qr.example").map(|provider| provider.family),
            Some(Family::QrPayment)
        );
        assert!(registry.get("qr.unknown").is_none());
        let einvoice: Vec<&str> = registry
            .in_family(Family::EInvoice)
            .map(|provider| provider.provider_id)
            .collect();
        assert_eq!(einvoice, vec!["einvoice.example", "einvoice.other_2"]);
        assert_eq!(registry.iter().count(), 3);
    }

    #[test]
    fn a_list_a_lookup_could_get_wrong_is_refused() {
        static A: ProviderDescriptor = EINVOICE;
        static AGAIN: ProviderDescriptor = EINVOICE;
        static MISFILED: ProviderDescriptor = ProviderDescriptor {
            provider_id: "qr.misfiled",
            ..EINVOICE
        };
        static TEMPLATE: ProviderDescriptor = EINVOICE;
        assert_eq!(
            Registry::new([&A, &AGAIN]).map(|_| ()),
            Err(RegistryError::Duplicate("einvoice.example"))
        );

        assert_eq!(
            Registry::new([&MISFILED]).map(|_| ()),
            Err(RegistryError::WrongFamily("qr.misfiled"))
        );

        for id in [
            "einvoice",
            "einvoice.",
            "EInvoice.x",
            "einvoice.a-b",
            "einvoice.a.b",
        ] {
            let malformed: &'static ProviderDescriptor = Box::leak(Box::new(ProviderDescriptor {
                provider_id: id,
                ..TEMPLATE
            }));
            assert!(
                matches!(
                    Registry::new([malformed]),
                    Err(RegistryError::MalformedId(_))
                ),
                "{id}"
            );
        }
    }

    #[test]
    fn a_field_nothing_could_satisfy_or_listed_twice_is_refused() {
        static TWICE: ProviderDescriptor = ProviderDescriptor {
            fields: &[
                SettingField {
                    key: "endpoint",
                    label_key: "provider.field.endpoint",
                    kind: FieldKind::Url,
                    required: true,
                },
                SettingField {
                    key: "endpoint",
                    label_key: "provider.field.endpoint",
                    kind: FieldKind::Url,
                    required: false,
                },
            ],
            ..EINVOICE
        };
        static EMPTY_RANGE: ProviderDescriptor = ProviderDescriptor {
            fields: &[SettingField {
                key: "timeout_seconds",
                label_key: "provider.field.timeout_seconds",
                kind: FieldKind::Number { min: 10, max: 1 },
                required: false,
            }],
            ..EINVOICE
        };
        assert_eq!(
            Registry::new([&TWICE]).map(|_| ()),
            Err(RegistryError::DuplicateField(
                "einvoice.example",
                "endpoint"
            ))
        );

        assert_eq!(
            Registry::new([&EMPTY_RANGE]).map(|_| ()),
            Err(RegistryError::UnsatisfiableField(
                "einvoice.example",
                "timeout_seconds"
            ))
        );
    }
}
