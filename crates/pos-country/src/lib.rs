// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a country module is, and how a binary ends up holding only the ones it needs.
//!
//! # A country is not an adapter
//!
//! `store-sqlite` is one implementation of one port. A country is a **bundle**: a `Fiscalization`
//! implementation, a locale pack, format validation the rest of the system cannot know, a default
//! retention period, and sometimes vendor adapters that exist in one region and nowhere else.
//! [ADR-0027](../../../docs/adr/0027-country-modules.md) is the record; this crate is its shape.
//!
//! # Selection is a Cargo feature, not a runtime registry
//!
//! A cell serves exactly one country ([ADR-0011](../../../docs/adr/0011-country-in-hostname.md)), so
//! a store in Hanoi has no use for Japanese tax code and — on a 4 GB mini-PC — no room for it either.
//! A binary therefore names its countries as features:
//!
//! ```toml
//! [features]
//! default    = ["country-vn"]
//! country-vn = ["pos-country-vn"]
//! country-jp = ["pos-country-jp"]
//! ```
//!
//! A fork serving only Vietnam edits **one line and deletes nothing**, which is the property that
//! matters: deleting directories would make every upstream pull a conflict, in exactly the place a
//! fork most wants to stay current, because tax fixes come from upstream.
//!
//! This is a deliberate departure from how vendor families work. `DeliveryVendor` is selected at run
//! time from configuration, because a tenant sells on Grab *and* ShopeeFood at once. Countries are
//! mutually exclusive per deployment, so they are selected earlier and more cheaply.
//!
//! # The trait is not sealed
//!
//! A fork with a private country module — an unpublished market, a proprietary acquirer — adds a path
//! dependency and implements [`CountryModule`] from outside this repository. A framework whose
//! extension point only works for its own authors is not a framework, so there is no sealing trait
//! here and no macro a caller is required to use.

#![forbid(unsafe_code)]
#![doc(test(attr(deny(warnings))))]

use core::fmt;

use pos_proto::locale::{CountryCode, LocalePack};

/// One country's obligations, bundled.
///
/// # What an implementation owes
///
/// The locale pack and the country code are cheap and infallible, because a registry lists every
/// compiled-in country at startup to log what this build supports, and that listing must not be able
/// to fail. Constructing the `Fiscalization` adapter is separate and fallible: it needs credentials,
/// an endpoint, and a provider choice, none of which exist until configuration has arrived.
///
/// # What it must not do
///
/// Reach for the clock, the filesystem, or the network at construction. A country module is
/// interrogated during startup — before configuration, sometimes before an async runtime — so
/// everything here is a pure description of what the country requires.
pub trait CountryModule: Send + Sync + 'static {
    /// Which country this module is for.
    fn country_code(&self) -> CountryCode;

    /// The defaults a fresh store in this country is correct with.
    ///
    /// Returned by value rather than by reference so an implementation may build it from constants
    /// without holding a `static`, which matters for a country whose rate table is assembled from a
    /// few legislative constants rather than written out once.
    fn locale_pack(&self) -> LocalePack;

    /// A human-readable name, for the fleet view and for logs.
    ///
    /// English, because it is an operator-facing label in a system whose canonical language is
    /// English — not a user-facing string, which would go through the translation catalogue instead.
    fn display_name(&self) -> &'static str;

    /// Whether `tax_code` is well-formed for this country.
    ///
    /// Format only, never existence: checking that a tax code is *registered* is a call to the tax
    /// authority and belongs behind `Fiscalization`. Keeping the two apart is what lets a store
    /// validate a corporate customer's tax code with no internet, which is the whole offline-first
    /// posture applied to one field.
    ///
    /// A module with no opinion returns `true`, and the absence of an opinion is then visible in one
    /// place rather than spread across the callers.
    fn is_valid_tax_code(&self, tax_code: &str) -> bool;
}

/// Every country compiled into this binary.
///
/// # Why this is a value rather than a global
///
/// A `static` registry would be initialised before configuration and would make a test that needs a
/// different set of countries impossible to write without process isolation. Built by
/// [`country_registry!`] in the binary's own startup path instead, where it is one line and can be
/// substituted in a test.
pub struct CountryRegistry {
    modules: Vec<Box<dyn CountryModule>>,
}

impl fmt::Debug for CountryRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CountryRegistry")
            .field("countries", &self.country_codes())
            .finish()
    }
}

impl CountryRegistry {
    /// A registry over the given modules.
    ///
    /// Prefer [`country_registry!`], which builds this from the enabled features so the list cannot
    /// drift from the manifest.
    #[must_use]
    pub fn new(modules: Vec<Box<dyn CountryModule>>) -> Self {
        Self { modules }
    }

    /// An empty registry.
    ///
    /// A build with no country feature enabled. Representable rather than forbidden, because
    /// `pos_cloud` has work to do — the configuration tree, the dashboards, the fleet view — before
    /// any country module is needed, and refusing to start would make that work impossible to run.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            modules: Vec::new(),
        }
    }

    /// The module for a country, if this build has it.
    ///
    /// `None` is the answer a deployment gets when it is configured for a country its binary was not
    /// built with — a fork that enabled `country-vn` and then created a Japanese tenant. The caller
    /// must report that as a configuration error naming both the wanted country and
    /// [`Self::country_codes`], because the raw symptom otherwise is invoices silently not issuing.
    #[must_use]
    pub fn get(&self, country_code: CountryCode) -> Option<&dyn CountryModule> {
        self.modules
            .iter()
            .map(Box::as_ref)
            .find(|module| module.country_code() == country_code)
    }

    /// Every country in this build, in the order the features were listed.
    pub fn modules(&self) -> impl Iterator<Item = &dyn CountryModule> {
        self.modules.iter().map(Box::as_ref)
    }

    /// Every country code in this build.
    ///
    /// Logged once at startup. `docs/architecture.md` §8 wants a deployment to be diagnosable from
    /// its own logs, and "which countries can this binary actually serve" is the first question when
    /// invoices are not issuing.
    #[must_use]
    pub fn country_codes(&self) -> Vec<CountryCode> {
        self.modules
            .iter()
            .map(|module| module.country_code())
            .collect()
    }

    /// How many countries this build carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Whether this build carries no country module.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// The one country this build serves, if there is exactly one.
    ///
    /// The supported shape: a cell is one country, so a correctly built edge binary answers here.
    /// `None` for zero **or** more than one, which forces a caller wanting the sole country to say
    /// what it does otherwise rather than quietly taking the first.
    #[must_use]
    pub fn sole(&self) -> Option<&dyn CountryModule> {
        match self.modules.as_slice() {
            [only] => Some(only.as_ref()),
            _ => None,
        }
    }

    /// Fails if two modules claim the same country.
    ///
    /// # Errors
    ///
    /// [`RegistryError::Duplicate`] naming the country. Two modules for one country is not a
    /// resolvable ambiguity — they disagree about tax law and there is no basis for preferring
    /// either — so a build in that state must not start. Called once at startup.
    pub fn validate(&self) -> Result<(), RegistryError> {
        let mut seen: Vec<CountryCode> = Vec::with_capacity(self.modules.len());
        for module in &self.modules {
            let code = module.country_code();
            if seen.contains(&code) {
                return Err(RegistryError::Duplicate(code));
            }
            seen.push(code);
        }
        Ok(())
    }
}

/// A registry that cannot be used as built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// Two modules claim the same country.
    Duplicate(CountryCode),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(code) => write!(
                f,
                "two country modules claim {code}; they cannot both be right about its tax law, so \
                 this build must not start"
            ),
        }
    }
}

impl core::error::Error for RegistryError {}

/// Builds a [`CountryRegistry`] from the country modules a binary enabled.
///
/// Each arm is guarded by the feature that pulls in the crate, so the registry and the manifest
/// cannot disagree — which is the point. A hand-written `Vec` would be a second list of countries,
/// and a second list is a list that will eventually be wrong.
///
/// ```ignore
/// // In pos_edge's startup path:
/// let countries = pos_country::country_registry! {
///     #[cfg(feature = "country-vn")] pos_country_vn::Vietnam,
///     #[cfg(feature = "country-jp")] pos_country_jp::Japan,
/// };
/// countries.validate()?;
/// ```
#[macro_export]
macro_rules! country_registry {
    ( $( $(#[$meta:meta])* $module:path ),* $(,)? ) => {{
        let mut modules: ::std::vec::Vec<::std::boxed::Box<dyn $crate::CountryModule>> =
            ::std::vec::Vec::new();
        // `extend` of a one-element array rather than `push`, because `clippy::vec_init_then_push`
        // is denied and its suggested fix does not apply here: `vec![…]` cannot carry a `#[cfg]` on
        // each element, since attributes on expressions are not stable. A `let` binding per arm,
        // because an attribute on a statement is.
        $(
            $(#[$meta])*
            let () = modules.extend([
                ::std::boxed::Box::new(<$module as ::core::default::Default>::default())
                    as ::std::boxed::Box<dyn $crate::CountryModule>,
            ]);
        )*
        $crate::CountryRegistry::new(modules)
    }};
}

#[cfg(test)]
mod tests {
    use super::{CountryModule, CountryRegistry, RegistryError};
    use pos_proto::locale::{CountryCode, LocalePack, NumberFormat, TaxRateTable};
    use pos_proto::money::CurrencyCode;
    use pos_proto::text::TranslationKey;

    /// A module for whichever country it is told to be, so the registry's own rules are testable
    /// without depending on a real country crate.
    struct Stub(CountryCode);

    impl Stub {
        fn of(code: CountryCode) -> Self {
            Self(code)
        }
    }

    /// `Default` is what [`crate::country_registry`] constructs through, and `CountryCode` itself has
    /// none on purpose — a default country would silently pick a tax regime.
    impl Default for Stub {
        fn default() -> Self {
            Self::of(CountryCode::ZZ)
        }
    }

    impl CountryModule for Stub {
        fn country_code(&self) -> CountryCode {
            self.0
        }

        fn locale_pack(&self) -> LocalePack {
            LocalePack {
                country_code: self.0,
                currency_code: CurrencyCode::VND,
                tax_rate_table: TaxRateTable::new(),
                number_format: NumberFormat::default(),
                default_language: TranslationKey::new("en"),
                default_retention_days: 365,
            }
        }

        fn display_name(&self) -> &'static str {
            "Stub"
        }

        fn is_valid_tax_code(&self, _tax_code: &str) -> bool {
            true
        }
    }

    fn registry(codes: &[CountryCode]) -> CountryRegistry {
        CountryRegistry::new(
            codes
                .iter()
                .map(|code| Box::new(Stub::of(*code)) as Box<dyn CountryModule>)
                .collect(),
        )
    }

    #[test]
    fn a_build_finds_the_country_it_was_built_with() {
        let built = registry(&[CountryCode::VN]);
        assert!(built.get(CountryCode::VN).is_some());
        assert_eq!(built.len(), 1);
        assert_eq!(built.country_codes(), vec![CountryCode::VN]);
    }

    #[test]
    fn a_country_this_build_lacks_is_none_rather_than_a_wrong_module() {
        // The failure this shapes: a fork enables country-vn, then somebody creates a Japanese
        // tenant. Answering with the Vietnamese module would issue invoices under the wrong tax
        // regime, which is worse than not issuing them.
        let built = registry(&[CountryCode::VN]);
        assert!(built.get(CountryCode::JP).is_none());
    }

    #[test]
    fn an_empty_registry_is_representable() {
        // pos_cloud has the configuration tree, the dashboards and the fleet view to serve before any
        // country module is needed, so a build with no country feature must still start.
        let none = CountryRegistry::empty();
        assert!(none.is_empty());
        assert!(none.sole().is_none());
        assert!(none.get(CountryCode::VN).is_none());
        none.validate().expect("an empty registry is valid");
    }

    #[test]
    fn sole_answers_only_when_there_is_exactly_one() {
        // A cell is one country, so a correct edge build answers here. Returning None for two forces
        // the caller to say what it does instead of silently taking the first.
        assert!(registry(&[CountryCode::VN]).sole().is_some());
        assert!(
            registry(&[CountryCode::VN, CountryCode::JP])
                .sole()
                .is_none(),
            "two countries have no sole country, and picking one would be arbitrary"
        );
        assert!(CountryRegistry::empty().sole().is_none());
    }

    #[test]
    fn two_modules_for_one_country_refuse_to_start() {
        // They cannot both be right about its tax law, and there is no basis for preferring either.
        let clashing = registry(&[CountryCode::VN, CountryCode::VN]);
        assert_eq!(
            clashing.validate(),
            Err(RegistryError::Duplicate(CountryCode::VN))
        );
        let rendered = RegistryError::Duplicate(CountryCode::VN).to_string();
        assert!(
            rendered.contains("VN"),
            "the message names the country: {rendered}"
        );
    }

    #[test]
    fn several_distinct_countries_validate() {
        let multi = registry(&[CountryCode::VN, CountryCode::JP, CountryCode::ZZ]);
        multi.validate().expect("distinct countries are valid");
        assert_eq!(multi.len(), 3);
        assert_eq!(
            multi.modules().count(),
            3,
            "and every one is iterable, which is what the startup log reads"
        );
    }

    #[test]
    fn a_modules_locale_pack_names_its_own_country() {
        // A module returning somebody else's country code in its pack would make every downstream
        // lookup consistent and wrong.
        for code in [CountryCode::VN, CountryCode::JP, CountryCode::ZZ] {
            let built = registry(&[code]);
            let module = built.get(code).expect("present");
            assert_eq!(module.locale_pack().country_code, code);
        }
    }

    #[test]
    fn the_macro_builds_a_registry_from_enabled_arms() {
        // A false cfg must drop its arm rather than fail to compile, which is what makes the feature
        // mechanism work at all.
        // `any()` is always false, which is what a disabled feature reduces to — and unlike a
        // made-up feature name it does not draw an `unexpected_cfgs` warning.
        let built = crate::country_registry! {
            Stub,
            #[cfg(any())] Stub,
        };
        assert_eq!(built.len(), 1, "the disabled arm contributed nothing");
    }
}
