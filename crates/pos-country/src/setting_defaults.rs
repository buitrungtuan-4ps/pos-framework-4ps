// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A value a country pack gives a new store for one setting of the register
//! ([ADR-0183](../../../docs/adr/0183-a-country-pack-seeds-a-new-stores-settings.md)).
//!
//! The register (`pos_proto::settings`) says what each setting takes; a pack says what a store in
//! its country starts with, in that country's units. A threshold counted in money is the case it
//! exists for: 20,000 is a sensible over/short in đồng and a large one in yen, so no single preset in
//! the register can be right for both.
//!
//! The value is one of the register's three shapes, as a pack writes it in a constant. Whoever
//! applies it reads it as the JSON the setting's node carries ([`SettingDefaultValue`]'s variants
//! are the register's `SettingShape` kinds), and the setting's own check decides whether it is one
//! the setting takes — a test over every compiled-in pack runs that check, so a renamed or tightened
//! setting fails the build rather than a store.

/// One setting a pack seeds: the register's key and the value a new store in the country starts
/// with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingDefault {
    /// The register's key, `node.field`, such as `shift.variance_reason_minor`.
    pub setting_key: &'static str,
    /// The value, in the country's units.
    pub value: SettingDefaultValue,
}

/// A seeded value, in the shape of the setting it is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingDefaultValue {
    /// One of a choice setting's wire tokens.
    Choice(&'static str),
    /// A whole number, in the unit the setting counts — minor units for money.
    Int(i64),
    /// On or off.
    Bool(bool),
}
