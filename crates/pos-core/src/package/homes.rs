// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A version's homes: the screen each role starts on after sign-in
//! ([ADR-0210](../../../../docs/adr/0210-a-role-names-its-home-screen-in-the-package.md) decision 1).
//!
//! The homes are a variant list like any other part, so [`check_variants`] proves that each role and
//! device class has exactly one. On top of it, a home's selector states at most a role and a device
//! class, and each names a screen the version declares. A version that states no homes has nothing
//! to check: its tills start where their release starts them.

use pos_proto::package::{HomeScreen, TimeSlot, Variant};
use pos_proto::screen::Screen;

use super::{VariantRefusal, check_variants};

/// Why a version's homes cannot be published or applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeRefusal {
    /// The homes' variants cannot each be told apart, or none is the base.
    Variants(VariantRefusal),
    /// The home at this position states a device by id, a sales channel or a time slot.
    Dimension(usize),
    /// The home at this position names a screen the version does not declare.
    UnknownScreen(usize),
}

impl HomeRefusal {
    /// The variant at fault, when the refusal names one.
    #[must_use]
    pub const fn index(self) -> Option<usize> {
        match self {
            Self::Dimension(index)
            | Self::UnknownScreen(index)
            | Self::Variants(
                VariantRefusal::DeviceStatedTwice(index)
                | VariantRefusal::UnknownTimeSlot(index)
                | VariantRefusal::SameSelector { second: index, .. },
            ) => Some(index),
            Self::Variants(VariantRefusal::NoBaseVariant) => None,
        }
    }

    /// A stable token naming the fault, for the console.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Variants(VariantRefusal::NoBaseVariant) => "NO_BASE_HOME",
            Self::Variants(_) => "SAME_HOME_SELECTOR",
            Self::Dimension(_) => "HOME_SELECTOR",
            Self::UnknownScreen(_) => "UNKNOWN_HOME_SCREEN",
        }
    }
}

impl core::fmt::Display for HomeRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Variants(refusal) => write!(f, "{refusal}"),
            Self::Dimension(index) => write!(
                f,
                "home {index}: a home is chosen by role and device class only"
            ),
            Self::UnknownScreen(index) => write!(
                f,
                "home {index} names a screen the version does not declare"
            ),
        }
    }
}

impl core::error::Error for HomeRefusal {}

/// Checks a version's homes against its screens: none at all, or a base and one home per
/// selector, each stating at most a role and a device class and naming a declared screen.
///
/// # Errors
///
/// The first [`HomeRefusal`] found: a selector's dimensions, then the variants, then the screens.
pub fn check_homes(
    homes: &[Variant<HomeScreen>],
    screens: &[Variant<Screen>],
    time_slots: &[TimeSlot],
) -> Result<(), HomeRefusal> {
    if homes.is_empty() {
        return Ok(());
    }
    if let Some(index) = homes.iter().position(|home| {
        let selector = &home.selector;
        selector.device_id.is_some()
            || selector.states_sales_channel()
            || selector.states_time_slot()
    }) {
        return Err(HomeRefusal::Dimension(index));
    }
    check_variants(homes, time_slots).map_err(HomeRefusal::Variants)?;
    match homes.iter().position(|home| {
        !screens
            .iter()
            .any(|screen| screen.body.screen_id == home.body.screen_id)
    }) {
        Some(index) => Err(HomeRefusal::UnknownScreen(index)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::package::{HomeScreen, Variant};
    use pos_proto::screen::Screen;

    use super::{HomeRefusal, check_homes};
    use crate::package::VariantRefusal;

    fn screens() -> Vec<Variant<Screen>> {
        serde_json::from_str(
            r#"[{"body":{"screen_id":"welcome","root":{"component_id":"screen_frame"}}},
                {"body":{"screen_id":"bar","root":{"component_id":"screen_frame"}}}]"#,
        )
        .expect("screens parse")
    }

    fn homes(json: &str) -> Vec<Variant<HomeScreen>> {
        serde_json::from_str(json).expect("homes parse")
    }

    const MANAGER: &str = "01J00000000000000000000R01";

    #[test]
    fn no_homes_and_a_base_with_role_and_class_variants_pass() {
        assert_eq!(check_homes(&[], &screens(), &[]), Ok(()));
        let stated = homes(&format!(
            r#"[{{"body":{{"screen_id":"welcome"}}}},
                {{"selector":{{"role_id":"{MANAGER}"}},"body":{{"screen_id":"bar"}}}},
                {{"selector":{{"role_id":"{MANAGER}","device_class":"DEVICE_CLASS_PHONE"}},"body":{{"screen_id":"welcome"}}}}]"#
        ));
        assert_eq!(check_homes(&stated, &screens(), &[]), Ok(()));
    }

    #[test]
    fn a_home_needs_a_base_and_one_per_selector() {
        let no_base = homes(&format!(
            r#"[{{"selector":{{"role_id":"{MANAGER}"}},"body":{{"screen_id":"bar"}}}}]"#
        ));
        assert_eq!(
            check_homes(&no_base, &screens(), &[]),
            Err(HomeRefusal::Variants(VariantRefusal::NoBaseVariant))
        );
        let twice = homes(r#"[{"body":{"screen_id":"bar"}},{"body":{"screen_id":"welcome"}}]"#);
        assert_eq!(
            check_homes(&twice, &screens(), &[]).map_err(HomeRefusal::index),
            Err(Some(1))
        );
    }

    #[test]
    fn a_home_states_only_a_role_and_a_device_class() {
        for selector in [
            r#"{"sales_channel":"SALES_CHANNEL_DINE_IN"}"#,
            r#"{"time_slot_id":"lunch"}"#,
            r#"{"device_id":"01J00000000000000000000D01"}"#,
        ] {
            let stated = homes(&format!(
                r#"[{{"body":{{"screen_id":"welcome"}}}},{{"selector":{selector},"body":{{"screen_id":"bar"}}}}]"#
            ));
            assert_eq!(
                check_homes(&stated, &screens(), &[]),
                Err(HomeRefusal::Dimension(1)),
                "{selector}"
            );
        }
    }

    #[test]
    fn a_home_names_a_declared_screen() {
        let stated = homes(r#"[{"body":{"screen_id":"floor"}}]"#);
        assert_eq!(
            check_homes(&stated, &screens(), &[]),
            Err(HomeRefusal::UnknownScreen(0))
        );
        assert_eq!(
            check_homes(&stated, &[], &[]),
            Err(HomeRefusal::UnknownScreen(0)),
            "a version with no screens has no homes"
        );
    }
}
