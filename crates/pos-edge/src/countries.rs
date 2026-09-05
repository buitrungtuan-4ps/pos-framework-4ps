// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The country modules this binary was built with.
//!
//! A cell serves exactly one country ([ADR-0011](../../../docs/adr/0011-country-in-hostname.md)), and
//! a country is a Cargo feature, not a runtime plug-in
//! ([ADR-0027](../../../docs/adr/0027-country-modules.md)). The registry is built from the enabled
//! features by the [`country_registry!`](pos_country::country_registry) macro, so the list of
//! countries and the manifest cannot drift: adding `country-vn` to the `[features]` table and an arm
//! here is the whole edit.
//!
//! `pos_edge` interrogates this at start-up to log which countries it can serve — the first question
//! when invoices are not issuing ([`docs/architecture.md`](../../../docs/architecture.md) §8) — and to
//! refuse to start if two modules claim one country.

use pos_country::CountryRegistry;

/// The registry of country modules compiled into this binary.
///
/// Each arm is guarded by the feature that pulls its crate in. A build with no `country-*` feature
/// (the default) yields an empty registry, which is valid: the edge serves and syncs before any
/// fiscalisation is configured (that arrives in P10), so a country is not needed to start.
#[must_use]
pub fn registry() -> CountryRegistry {
    pos_country::country_registry! {
        #[cfg(feature = "country-zz")] pos_country_zz::Zz,
        #[cfg(feature = "country-vn")] pos_country_vn::Vietnam,
        #[cfg(feature = "country-jp")] pos_country_jp::Japan,
        #[cfg(feature = "country-in")] pos_country_in::India,
    }
}
