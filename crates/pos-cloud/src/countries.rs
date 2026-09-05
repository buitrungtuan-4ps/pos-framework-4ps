// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The country modules this cloud binary was built with (ADR-0027, Track M4).
//!
//! A country is a Cargo feature, not a runtime plug-in ([ADR-0027](../../../docs/adr/0027-country-modules.md)):
//! the registry is built from the enabled `country-*` features by the
//! [`country_registry!`](pos_country::country_registry) macro, so the list of countries and the
//! manifest cannot drift. Unlike a single-cell edge, the cloud serves a fleet across countries, so it
//! enables the reference module by default and a fork adds one line here and in `[features]`.
//!
//! The console reads this at start-up to surface countries, currencies and the locale catalogue as
//! **read-only master data** (ADR-0074) — the currency picker and the translation grid's column set.
//! Fiscalization and per-store defaults are not decided here; this is only "what the platform can
//! serve".

use pos_country::CountryRegistry;

/// The registry of country modules compiled into this binary.
///
/// Each arm is guarded by the feature that pulls its crate in. A build with no `country-*` feature
/// yields an empty registry, which is valid: the console simply has no countries to offer until one is
/// enabled.
#[must_use]
pub fn registry() -> CountryRegistry {
    pos_country::country_registry! {
        #[cfg(feature = "country-zz")] pos_country_zz::Zz,
        #[cfg(feature = "country-vn")] pos_country_vn::Vietnam,
        #[cfg(feature = "country-jp")] pos_country_jp::Japan,
        #[cfg(feature = "country-in")] pos_country_in::India,
    }
}
