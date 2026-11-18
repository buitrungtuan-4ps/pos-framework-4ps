// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The business-type templates shipped in `business-types/`, read and checked against the settings
//! register and the country packs
//! ([ADR-0200](../../../docs/adr/0200-six-business-type-templates-ship-with-the-framework.md)).
//!
//! A template is a package plus values (decision 1). Each lives in `business-types/<id>/`, one
//! directory per [`BusinessType`] (decision 2), and is compiled into this crate, so a binary that
//! offers templates offers exactly the ones its release was tested with. What each directory holds
//! today:
//!
//! - `template.json`: the business type it is for, and its **recommended settings** for a new
//!   store, as *(setting key, value)* pairs. [`templates`] refuses a key the register does not
//!   have, a value the setting does not take, a key named twice, and any `package.*` key: choosing
//!   a template pins its own copy (decision 3), so a template never chooses a version.
//! - `demo.json`: its obviously fake demo menu, priced in every shipped country's currency, and
//!   its floor where it serves at tables ([`demo`]), which the simulator's template-by-country
//!   matrix sells from.
//! - Its **capability preset** is `pos-core`'s, [`Template::capabilities`].
//!
//! The package joins each directory as the capabilities it uses arrive (decision 5). Seeding order is the country pack, then the template, then the register's
//! preset (decision 4); a test over every shipped pack refuses a template that sets a key a pack
//! sets, so the two never compete.

pub mod demo;

pub use demo::{Demo, DemoFault};
use pos_core::capability::CapabilityContext;
use pos_proto::BusinessType;
use pos_proto::settings::{Setting, ValueRefusal, register};
use pos_proto::wire_enum::WireEnum;
use serde::Deserialize;

/// The directory under `business-types/` each template lives in, its `template.json` and its
/// `demo.json`.
const SOURCES: [(BusinessType, &str, &str, &str); 6] = [
    (
        BusinessType::FullService,
        "full-service",
        include_str!("../../../business-types/full-service/template.json"),
        include_str!("../../../business-types/full-service/demo.json"),
    ),
    (
        BusinessType::Cafe,
        "cafe",
        include_str!("../../../business-types/cafe/template.json"),
        include_str!("../../../business-types/cafe/demo.json"),
    ),
    (
        BusinessType::FastFood,
        "fast-food",
        include_str!("../../../business-types/fast-food/template.json"),
        include_str!("../../../business-types/fast-food/demo.json"),
    ),
    (
        BusinessType::Bar,
        "bar",
        include_str!("../../../business-types/bar/template.json"),
        include_str!("../../../business-types/bar/demo.json"),
    ),
    (
        BusinessType::BakeryRetail,
        "bakery-retail",
        include_str!("../../../business-types/bakery-retail/template.json"),
        include_str!("../../../business-types/bakery-retail/demo.json"),
    ),
    (
        BusinessType::SelfOrder,
        "self-order",
        include_str!("../../../business-types/self-order/template.json"),
        include_str!("../../../business-types/self-order/demo.json"),
    ),
];

/// The most recommended settings one template may carry: a template states a way of working, not a
/// store's whole configuration.
pub const MAX_RECOMMENDED_SETTINGS: usize = 32;

/// One shipped template, as read and checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    /// The business type it is for.
    pub business_type: BusinessType,
    /// Its directory under `business-types/`.
    pub directory: &'static str,
    /// The settings a new store made from it starts with, in the file's order.
    pub recommended_settings: Vec<RecommendedSetting>,
    /// Its fake demo menu and floor.
    pub demo: Demo,
}

impl Template {
    /// The capability preset the template starts a store with.
    #[must_use]
    pub fn capabilities(&self) -> Option<CapabilityContext> {
        CapabilityContext::for_business_type(self.business_type)
    }
}

/// One recommended setting: the register's key and the value, as the setting's node carries it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecommendedSetting {
    /// The register's key, `node.field`.
    pub setting_key: String,
    /// The value, which the setting's own check accepts.
    pub value: serde_json::Value,
}

/// `template.json` as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateFile {
    business_type: String,
    recommended_settings: Vec<RecommendedSetting>,
}

/// Why a shipped template is refused. Each names the template's directory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// `template.json` or `demo.json` is not the shape this crate reads.
    #[error("a file in business-types/{directory} is not the shape it should be: {reason}")]
    Malformed {
        /// The template's directory.
        directory: &'static str,
        /// What the reader said.
        reason: String,
    },
    /// The file names another business type than its directory is for.
    #[error("business-types/{directory} names {named}, not its own business type")]
    WrongBusinessType {
        /// The template's directory.
        directory: &'static str,
        /// The token the file names.
        named: String,
    },
    /// More recommended settings than [`MAX_RECOMMENDED_SETTINGS`].
    #[error("business-types/{directory} recommends more than {MAX_RECOMMENDED_SETTINGS} settings")]
    TooManySettings {
        /// The template's directory.
        directory: &'static str,
    },
    /// A key the register does not have.
    #[error("business-types/{directory} recommends {setting_key}, which is not a setting")]
    UnknownSetting {
        /// The template's directory.
        directory: &'static str,
        /// The key.
        setting_key: String,
    },
    /// A key named twice.
    #[error("business-types/{directory} recommends {setting_key} twice")]
    Duplicate {
        /// The template's directory.
        directory: &'static str,
        /// The key.
        setting_key: String,
    },
    /// A `package.*` key: a template never chooses a version (ADR-0200 decision 3).
    #[error("business-types/{directory} recommends {setting_key}; a template pins its own copy")]
    PackageSetting {
        /// The template's directory.
        directory: &'static str,
        /// The key.
        setting_key: String,
    },
    /// `demo.json` is not a demo this crate accepts.
    #[error("business-types/{directory}/demo.json is refused: {fault:?}")]
    Demo {
        /// The template's directory.
        directory: &'static str,
        /// What is wrong with it.
        fault: DemoFault,
    },
    /// A value the setting does not take.
    #[error("business-types/{directory} recommends a value for {setting_key} that {refusal}")]
    Refused {
        /// The template's directory.
        directory: &'static str,
        /// The key.
        setting_key: String,
        /// The setting's own refusal.
        refusal: ValueRefusal,
    },
}

/// The directory under `business-types/` of `business_type`'s template, or `None` for
/// `BUSINESS_TYPE_UNSPECIFIED`.
#[must_use]
pub fn directory(business_type: BusinessType) -> Option<&'static str> {
    SOURCES
        .iter()
        .find(|(shipped, _, _, _)| *shipped == business_type)
        .map(|(_, directory, _, _)| *directory)
}

/// Every shipped template, read and checked, in [`BusinessType`] order.
///
/// # Errors
///
/// The first [`TemplateError`] found. A test reads them all, so a shipped release never refuses.
pub fn templates() -> Result<Vec<Template>, TemplateError> {
    let settings = register();
    SOURCES
        .iter()
        .map(|(business_type, directory, source, demo)| {
            read(*business_type, directory, source, &settings)
                .and_then(|template| with_demo(template, demo))
        })
        .collect()
}

/// The shipped template for `business_type`, or `None` for `BUSINESS_TYPE_UNSPECIFIED`.
///
/// # Errors
///
/// As [`templates`], for this one template.
pub fn template(business_type: BusinessType) -> Result<Option<Template>, TemplateError> {
    let settings = register();
    SOURCES
        .iter()
        .find(|(shipped, _, _, _)| *shipped == business_type)
        .map(|(shipped, directory, source, demo)| {
            read(*shipped, directory, source, &settings)
                .and_then(|template| with_demo(template, demo))
        })
        .transpose()
}

/// Reads one template and checks it against the register.
fn read(
    business_type: BusinessType,
    directory: &'static str,
    source: &str,
    settings: &[Setting],
) -> Result<Template, TemplateError> {
    let file: TemplateFile =
        serde_json::from_str(source).map_err(|error| TemplateError::Malformed {
            directory,
            reason: error.to_string(),
        })?;
    if BusinessType::from_wire(&file.business_type) != Some(business_type) {
        return Err(TemplateError::WrongBusinessType {
            directory,
            named: file.business_type,
        });
    }
    if file.recommended_settings.len() > MAX_RECOMMENDED_SETTINGS {
        return Err(TemplateError::TooManySettings { directory });
    }
    for (index, recommended) in file.recommended_settings.iter().enumerate() {
        check_setting(directory, recommended, settings)?;
        let named_before = file
            .recommended_settings
            .iter()
            .take(index)
            .any(|earlier| earlier.setting_key == recommended.setting_key);
        if named_before {
            return Err(TemplateError::Duplicate {
                directory,
                setting_key: recommended.setting_key.clone(),
            });
        }
    }
    Ok(Template {
        business_type,
        directory,
        recommended_settings: file.recommended_settings,
        demo: Demo {
            items: Vec::new(),
            floor: None,
            stations: Vec::new(),
        },
    })
}

/// Reads a template's `demo.json` onto it and checks it.
fn with_demo(mut template: Template, source: &str) -> Result<Template, TemplateError> {
    let directory = template.directory;
    let demo: Demo = serde_json::from_str(source).map_err(|error| TemplateError::Malformed {
        directory,
        reason: error.to_string(),
    })?;
    demo.check()
        .map_err(|fault| TemplateError::Demo { directory, fault })?;
    template.demo = demo;
    Ok(template)
}

/// Checks one recommended setting: a register key, not a package key, a value it takes.
fn check_setting(
    directory: &'static str,
    recommended: &RecommendedSetting,
    settings: &[Setting],
) -> Result<(), TemplateError> {
    let setting_key = &recommended.setting_key;
    if setting_key.starts_with("package.") {
        return Err(TemplateError::PackageSetting {
            directory,
            setting_key: setting_key.clone(),
        });
    }
    let setting = settings
        .iter()
        .find(|setting| setting.key() == *setting_key)
        .ok_or_else(|| TemplateError::UnknownSetting {
            directory,
            setting_key: setting_key.clone(),
        })?;
    setting
        .check(&recommended.value)
        .map_err(|refusal| TemplateError::Refused {
            directory,
            setting_key: setting_key.clone(),
            refusal,
        })
}

#[cfg(test)]
mod tests;
