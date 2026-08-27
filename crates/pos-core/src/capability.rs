// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store capabilities: the flags a profile is made of, read through one point.
//!
//! `docs/pos-spec.md` §10 fixes the shape. A store profile is **not** three separate applications
//! but a set of capability flags in the configuration tree — `tables_enabled`, `kds_enabled`,
//! `pay_first_enabled`, and the rest. Full-service, cafe-counter and retail are three presets over
//! the same flags, not three code paths.
//!
//! Two rules from §10 shape this module:
//!
//! - **Flags are read through a single [`CapabilityContext`]; scattering `if flag` through the code
//!   is banned.** A command handler that needs seat-level ordering calls
//!   [`CapabilityContext::require`] and gets a named [`DomainError::CapabilityDisabled`] when the
//!   store does not have it — it never reads a raw boolean out of the config document. Because the
//!   read point is one type, "which flags exist" is answered by [`Capability::ALL`] and cannot drift
//!   from what the cloud validates.
//! - **Inter-flag validity is validated in the cloud before a config version is published.** That
//!   validation is [`conflicts`], a pure function over enumerable [`RULES`], so the cloud rejects a
//!   bad combination (and keeps last-good) using the same logic the edge would — no second,
//!   divergent implementation. `pay_first_enabled` with `tables_enabled` is the conflict §10 names;
//!   `seats_enabled` without `tables_enabled` is the one the data model forces.
//!
//! # The catalogue is a contract
//!
//! `docs/snapshots/capabilities.txt` records every flag key. A key, once shipped, is a term in the
//! configuration document a synced edge reads, so `cargo xtask snapshot` refuses to let one
//! disappear — the same removal gate the event and permission catalogues use. A flag's `default`
//! may change (it is a tabbed, mutable line), because changing a default is a deliberate act that
//! already owes an upgrade note.

use crate::error::DomainError;

/// Declares the fixed capability catalogue.
///
/// One entry per flag, each carrying its config key, default, and description. The macro generates
/// the [`Capability`] enum, its `ALL`, and [`Capability::meta`] from the same block, so a new flag
/// is one entry that updates all three and cannot be added without a stated default.
macro_rules! capabilities {
    (
        $(
            $(#[$variant_meta:meta])*
            $variant:ident {
                key: $key:literal,
                default: $default:literal,
                description: $description:literal $(,)?
            }
        ),+ $(,)?
    ) => {
        /// A store capability flag from the fixed catalogue.
        ///
        /// Fieldless and `Copy`, so a [`CapabilityContext`] holds it as a single bit.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Capability {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl Capability {
            /// Every capability, in declaration order.
            pub const ALL: &'static [Self] = &[ $(Self::$variant),+ ];

            /// The capability's declared metadata.
            #[must_use]
            pub const fn meta(self) -> CapabilityMeta {
                match self {
                    $(
                        Self::$variant => CapabilityMeta {
                            key: $key,
                            default_on: $default,
                            description: $description,
                        },
                    )+
                }
            }
        }
    };
}

/// Everything a capability declares.
#[derive(Debug, Clone, Copy)]
pub struct CapabilityMeta {
    /// The config-tree key, `snake_case` with the `_enabled` suffix the naming standard requires
    /// (`docs/naming-and-api.md`; `docs/roadmap.md` D8).
    pub key: &'static str,
    /// Whether a store with no explicit setting has this on. The default profile
    /// ([`CapabilityContext::defaults`]) is these values.
    pub default_on: bool,
    /// A one-line description for the dashboard.
    pub description: &'static str,
}

capabilities! {
    /// Floor plan and table-service flow: an order belongs to a table, paid after service.
    Tables {
        key: "tables_enabled",
        default: true,
        description: "Table service: a floor plan, one open order per table, payment after service",
    },
    /// Long-running tabs held open across visits (a bar tab).
    Tabs {
        key: "tabs_enabled",
        default: false,
        description: "Keep a named tab open across several rounds before settling",
    },
    /// Seat-level ordering: items are assigned to seats within a table's order.
    Seats {
        key: "seats_enabled",
        default: false,
        description: "Assign items to seats within a table, for by-seat splitting",
    },
    /// A kitchen display system: fired lines appear on station screens.
    Kds {
        key: "kds_enabled",
        default: true,
        description: "Route fired lines to kitchen display stations rather than a printer only",
    },
    /// Courses: lines are grouped and fired by course rather than all at once.
    Courses {
        key: "courses_enabled",
        default: true,
        description: "Group lines into courses and fire them in sequence",
    },
    /// Pay-first: payment is taken before the order is prepared, and a queue number is issued.
    PayFirst {
        key: "pay_first_enabled",
        default: false,
        description: "Take payment before preparation; incompatible with table service",
    },
    /// Barcode entry, for retail-style 1:1-by-SKU selling.
    Barcode {
        key: "barcode_enabled",
        default: false,
        description: "Add items by scanning a barcode",
    },
    /// Daily takeaway queue numbers (distinct from the store-lifetime receipt counter).
    QueueNumber {
        key: "queue_number_enabled",
        default: false,
        description: "Issue a daily-resetting queue number for takeaway orders",
    },
    /// Tips: a tip is collected as a separate ledger, never as revenue.
    Tips {
        key: "tips_enabled",
        default: true,
        description: "Collect tips as a separate ledger from the bill total",
    },
    /// Guest QR ordering: guests submit orders from a cloud-served page.
    QrOrdering {
        key: "qr_ordering_enabled",
        default: false,
        description: "Let guests order from a QR page, arriving through the OrderIn port",
    },
}

// A `u16` bitset holds one bit per capability by discriminant, so the catalogue cannot outgrow it
// without a deliberate change here.
const _: () = assert!(
    Capability::ALL.len() <= 16,
    "CapabilityContext is a u16 bitset; a 17th capability needs a wider representation"
);

/// The set of capabilities a store has on — the **one** point flags are read.
///
/// Built once from the store's config document and passed to the domain by value, so a decision
/// reads a fixed snapshot of the profile rather than consulting mutable config mid-way. `Copy` and
/// cheap, because [`require`](Self::require) runs on every capability-gated action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilityContext(u16);

impl CapabilityContext {
    /// Nothing enabled.
    pub const NONE: Self = Self(0);

    /// The bit for one capability.
    const fn bit(capability: Capability) -> u16 {
        1_u16 << (capability as u16)
    }

    /// Whether the store has `capability` on. The single read accessor.
    #[must_use]
    pub const fn enabled(self, capability: Capability) -> bool {
        self.0 & Self::bit(capability) != 0
    }

    /// Turns a capability on.
    pub const fn insert(&mut self, capability: Capability) {
        self.0 |= Self::bit(capability);
    }

    /// Returns the context with `capability` added, for builder-style construction.
    #[must_use]
    pub const fn with(mut self, capability: Capability) -> Self {
        self.insert(capability);
        self
    }

    /// The gate a capability-gated action calls instead of reading a raw flag.
    ///
    /// # Errors
    ///
    /// [`DomainError::CapabilityDisabled`] naming the capability's key, when the store does not have
    /// it on.
    pub const fn require(self, capability: Capability) -> Result<(), DomainError> {
        if self.enabled(capability) {
            Ok(())
        } else {
            Err(DomainError::CapabilityDisabled {
                capability: capability.meta().key,
            })
        }
    }

    /// The default profile: every capability at its declared default. A fresh store starts here
    /// until the cloud publishes a profile.
    #[must_use]
    pub fn defaults() -> Self {
        let mut context = Self::NONE;
        for capability in Capability::ALL {
            if capability.meta().default_on {
                context.insert(*capability);
            }
        }
        context
    }

    /// The full-service preset: the default profile (table service, KDS, courses, tips).
    #[must_use]
    pub fn full_service() -> Self {
        Self::defaults()
    }

    /// The cafe/counter preset: pay first, issue a queue number, take tips — no tables.
    #[must_use]
    pub fn counter() -> Self {
        Self::NONE
            .with(Capability::PayFirst)
            .with(Capability::QueueNumber)
            .with(Capability::Tips)
    }

    /// The retail preset: barcode entry, no kitchen, no table service.
    #[must_use]
    pub fn retail() -> Self {
        Self::NONE.with(Capability::Barcode)
    }

    /// Rebuilds a context from a source of flag values: `is_on(key)` answers whether the capability
    /// with that `key` is set (`Some(true)`/`Some(false)`), or `None` when the source does not mention
    /// it — in which case the flag falls back to its declared [`default_on`](CapabilityMeta::default_on).
    ///
    /// This is the **one** place the flag keys of a config document become a capability set, kept in
    /// `pos-core` (serde-free — the caller supplies the lookup) so the cloud validator and the edge
    /// runtime cannot disagree on how a published profile is read (§10). The cloud passes a closure over
    /// its JSON `Value`; the edge passes the same over the document it pulled.
    #[must_use]
    pub fn from_flags(is_on: impl Fn(&str) -> Option<bool>) -> Self {
        Capability::ALL
            .iter()
            .copied()
            .filter(|capability| {
                let meta = capability.meta();
                is_on(meta.key).unwrap_or(meta.default_on)
            })
            .collect()
    }
}

impl FromIterator<Capability> for CapabilityContext {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        let mut context = Self::NONE;
        for capability in iter {
            context.insert(capability);
        }
        context
    }
}

/// One inter-flag validity rule.
///
/// A rule is satisfied or violated by a whole [`CapabilityContext`]; [`conflicts`] returns the ones
/// a context violates. The `check` is a non-capturing function, so [`RULES`] is a `const` the cloud
/// evaluates and a test enumerates.
#[derive(Clone, Copy)]
pub struct CapabilityRule {
    /// A stable id for the message and any audit record.
    pub id: &'static str,
    /// Why the combination is rejected, for the admin who has to fix it.
    pub description: &'static str,
    /// Returns `true` when the context satisfies the rule.
    check: fn(CapabilityContext) -> bool,
}

impl CapabilityRule {
    /// Whether `context` satisfies this rule.
    #[must_use]
    pub fn is_satisfied(&self, context: CapabilityContext) -> bool {
        (self.check)(context)
    }
}

impl core::fmt::Debug for CapabilityRule {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CapabilityRule")
            .field("id", &self.id)
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}

/// Every inter-flag validity rule §10 mandates. The cloud checks these before publishing a config
/// version.
pub const RULES: &[CapabilityRule] = &[
    CapabilityRule {
        id: "pay_first.excludes.tables",
        description: "pay_first_enabled cannot be on with tables_enabled: paying before service and \
                      table service are opposite flows",
        check: |c| !(c.enabled(Capability::PayFirst) && c.enabled(Capability::Tables)),
    },
    CapabilityRule {
        id: "seats.requires.tables",
        description: "seats_enabled needs tables_enabled: a seat is a subdivision of a table's order",
        check: |c| !c.enabled(Capability::Seats) || c.enabled(Capability::Tables),
    },
];

/// The rules `context` violates, in declaration order. Empty means the profile is valid.
///
/// This is the whole of §10's cloud-side inter-flag validation. It returns *all* violations rather
/// than the first, because the dashboard shows an admin every problem at once.
#[must_use]
pub fn conflicts(context: CapabilityContext) -> Vec<&'static CapabilityRule> {
    RULES
        .iter()
        .filter(|rule| !rule.is_satisfied(context))
        .collect()
}

/// One fact per line, sorted, for `docs/snapshots/capabilities.txt`.
///
/// The bare key line is the contract the removal gate protects — a config term a synced edge reads;
/// the `default=` line is mutable metadata (changing a default owes an upgrade note but is allowed).
#[must_use]
pub fn render_snapshot() -> String {
    let mut lines = Vec::new();
    for capability in Capability::ALL {
        let meta = capability.meta();
        lines.push(meta.key.to_owned());
        lines.push(format!("{}\tdefault={}", meta.key, meta.default_on));
    }
    lines.sort();
    let mut out =
        String::from("# Generated from crates/pos-core/src/capability.rs — do not edit.\n");
    out.push_str(
        "# One line per fact. A bare key is a contract; a tabbed default is mutable metadata.\n",
    );
    out.push_str(&lines.join("\n"));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::{Capability, CapabilityContext, RULES, conflicts, render_snapshot};
    use crate::error::DomainError;

    #[test]
    fn require_names_the_disabled_capability() {
        let context = CapabilityContext::NONE;
        assert!(matches!(
            context.require(Capability::Seats),
            Err(DomainError::CapabilityDisabled { capability }) if capability == "seats_enabled"
        ));
    }

    #[test]
    fn require_passes_when_enabled() {
        let context = CapabilityContext::NONE.with(Capability::Tables);
        assert!(context.require(Capability::Tables).is_ok());
        assert!(context.enabled(Capability::Tables));
        assert!(!context.enabled(Capability::Seats));
    }

    #[test]
    fn the_three_presets_are_each_internally_valid() {
        for context in [
            CapabilityContext::full_service(),
            CapabilityContext::counter(),
            CapabilityContext::retail(),
        ] {
            assert!(
                conflicts(context).is_empty(),
                "a shipped preset must satisfy every inter-flag rule: {context:?}"
            );
        }
    }

    #[test]
    fn pay_first_with_tables_is_a_conflict() {
        // The conflict §10 names by example.
        let context = CapabilityContext::NONE
            .with(Capability::PayFirst)
            .with(Capability::Tables);
        let found = conflicts(context);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found.first().map(|rule| rule.id),
            Some("pay_first.excludes.tables")
        );
    }

    #[test]
    fn seats_without_tables_is_a_conflict() {
        let context = CapabilityContext::NONE.with(Capability::Seats);
        let found = conflicts(context);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found.first().map(|rule| rule.id),
            Some("seats.requires.tables")
        );
    }

    #[test]
    fn seats_with_tables_is_fine() {
        let context = CapabilityContext::NONE
            .with(Capability::Seats)
            .with(Capability::Tables);
        assert!(conflicts(context).is_empty());
    }

    #[test]
    fn every_key_is_distinct_snake_case_and_enabled_suffixed() {
        let mut keys: Vec<&str> = Capability::ALL.iter().map(|c| c.meta().key).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "two capabilities share a key");
        for capability in Capability::ALL {
            let key = capability.meta().key;
            assert!(key.ends_with("_enabled"), "{key} must end with _enabled");
            assert!(
                key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{key} must be lower snake_case"
            );
        }
    }

    #[test]
    fn every_rule_id_is_distinct() {
        let mut ids: Vec<&str> = RULES.iter().map(|r| r.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two rules share an id");
    }

    #[test]
    fn the_snapshot_matches_the_committed_file() {
        let rendered = render_snapshot();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/snapshots/capabilities.txt");
        if std::env::var_os("POS_UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(&path, &rendered).expect("write capabilities snapshot");
        }
        let committed = std::fs::read_to_string(&path).expect(
            "docs/snapshots/capabilities.txt exists; regenerate with POS_UPDATE_SNAPSHOTS=1",
        );
        assert_eq!(
            rendered, committed,
            "regenerate with POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core"
        );
    }
}
