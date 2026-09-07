// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The published `reason_codes` config node ([ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md)):
//! the managed list every action that must give a reason draws from — a void, a discount, a comp,
//! a refund, an out-of-sale drawer opening, and the five more the event catalogue names below.
//!
//! `docs/pos-spec.md` §11 **Fraud controls**, item 2, requires *"mandatory reasons from a
//! cloud-managed list"* for six actions. Eleven event fields in [`crate::events`] carry a
//! [`ReasonCodeId`] — the spec's six plus five the event catalogue adds (`sales.order.rejected_by_staff`,
//! `cash.drawer.paid_in`, `cash.drawer.paid_out`, `inventory.stock.adjusted`,
//! `inventory.stock.wasted`) — and until this node existed, every one of them named a list nothing
//! produced. [`ReasonAction`] therefore has a variant for each of the eleven, not only the spec's
//! six: an operator who cannot author a reason for a cash paid-in cannot record one, and the field
//! on that event would stay unfillable.
//!
//! # Why the framework carries a default set
//!
//! Every other config node means "feature off" when it is absent. This one cannot. A store must
//! be able to void a mis-keyed line during a cloud outage, on its first day, before anyone has
//! opened the console — [ADR-0001](../../../docs/adr/0001-offline-first-store-autonomy.md)'s whole
//! posture is that the store keeps trading. So [`PublishedReasonCodes::framework_default`] ships a
//! small set in this crate, the edge starts with it, and a published node **replaces** it wholesale
//! rather than merging, so an operator can remove a framework reason they judge wrong for their
//! business.
//!
//! A framework entry's id is a reserved low [`crate::ulid::Ulid`]: a ULID minted from a clock
//! carries a non-zero 48-bit timestamp, so these ids can never collide with an authored one. The
//! cloud resolves them through [`PublishedReasonCodes::framework_default`] rather than through the
//! authoring table, which is why an event citing one is still readable in a report.
//!
//! # Tenant content, so the text is per-locale
//!
//! `docs/pos-spec.md` §12 puts tenant content in the configuration tree as
//! `{"en": required, "vi": …}` with English as the fallback, exactly as
//! [`crate::menu::MenuEntry`] holds an item's name. A reason code is tenant content, so it carries
//! [`PublishedReasonCode::display_name`] plus a per-locale map — not a
//! [`crate::text::TranslationKey`], which names a string in the *framework's* catalogue that no
//! tenant can extend. What travels on the wire is the id; the text stays here.
//!
//! Reason codes are configuration and reference data throughout: a code, a name, and the actions
//! it is valid for. Nothing here is a customer or employee identifier.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::ReasonCodeId;
use crate::text::DisplayName;
use crate::ulid::Ulid;
use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// An action a reason must be cited for.
    ///
    /// One variant per event field that declares a `reason_code_id`, and the event is named on each
    /// so the two cannot drift: the first six are `docs/pos-spec.md` §11 item 2 verbatim, the last
    /// five are actions whose events already demand a reason that §11's sentence does not mention.
    /// Without them an entry could not be tagged for a cash movement or a stock correction, and
    /// those fields would have nothing valid to put in them.
    ///
    /// Closed vocabulary: the framework owns which actions demand a reason. What a tenant
    /// configures is the *reasons*, which are data (`crate::enums`' module documentation makes the
    /// same split for courses, tax classes and station names).
    ReasonAction, prefix = "REASON_ACTION";
    /// Voiding a line — `sales.order_line.voided`. A void *after* firing needs
    /// `Permission::VoidFiredLine` and a verified PIN; an unfired line is an ordinary cancel
    /// (`docs/pos-spec.md` §5).
    VoidLine = "VOID_LINE",
    /// Voiding a bill — `billing.bill.voided`, which requires a manager (§6).
    VoidBill = "VOID_BILL",
    /// A manual price reduction — `billing.discount.applied` (§6). A campaign-granted one cites the
    /// campaign instead, which is why that event's field is optional.
    Discount = "DISCOUNT",
    /// Giving an item away — `billing.comp.applied`, which still consumes inventory and is recorded
    /// as cost (§6).
    Comp = "COMP",
    /// Refunding money already taken — `billing.refund.issued` (§6).
    Refund = "REFUND",
    /// Opening the drawer outside a sale — `cash.drawer.opened` with `standalone` set, which needs a
    /// permission and a reason (§7).
    DrawerOpen = "DRAWER_OPEN",
    /// The store refusing an inbound order — `sales.order.rejected_by_staff` for a guest's QR
    /// submission (§13), and the reason `pos_ports::DeliveryVendor::reject` demands when a
    /// marketplace order is declined. Two callers, one action: both are "we are not making this".
    RejectOrder = "REJECT_ORDER",
    /// Cash added to the drawer — `cash.drawer.paid_in` (§7).
    CashPaidIn = "CASH_PAID_IN",
    /// Cash removed from the drawer — `cash.drawer.paid_out` (§7).
    CashPaidOut = "CASH_PAID_OUT",
    /// A signed correction to counted stock — `inventory.stock.adjusted` (§8).
    StockAdjustment = "STOCK_ADJUSTMENT",
    /// Stock written off — `inventory.stock.wasted`: spoilage, breakage, or the waste a void after
    /// firing records instead of returning stock (§8).
    StockWaste = "STOCK_WASTE",
}

/// A short, stable, language-independent handle for a reason — `"WASTE"`, `"STAFF_ERROR"`.
///
/// Deliberately not in [`crate::text`], which holds the text types admissible in an *event
/// payload*. This one is never in a payload: an event carries the [`ReasonCodeId`], and the code
/// exists so a report can group by something an operator recognises rather than by a ULID
/// (`docs/pos-spec.md` §11 item 3 compares void and discount rates per employee) and so a CSV
/// export is readable.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReasonCode(Box<str>);

impl ReasonCode {
    /// Wraps a code.
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    /// The code as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One entry in the managed list: what it is called, and what it may be given for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedReasonCode {
    /// Its stable id — the value an event's `reason_code_id` carries.
    pub id: ReasonCodeId,
    /// The short handle a report groups by.
    pub code: ReasonCode,
    /// The name staff read on the picker. Always present, and the fallback for any locale
    /// [`display_name_translations`](Self::display_name_translations) does not carry (§12: English
    /// is always present and is the fallback).
    pub display_name: DisplayName,
    /// The name in each locale it is translated into, keyed by locale code (`"vi"`, `"ja"`, …), as
    /// [`crate::menu::MenuEntry::display_name_translations`] does for an item. Additive: an entry
    /// with none behaves exactly as one that predates the field.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub display_name_translations: BTreeMap<String, DisplayName>,
    /// The actions this reason may be given for.
    ///
    /// Empty means the entry is valid for nothing, which is a data-entry mistake rather than a
    /// posture — the publish refuses it (ADR-0115), because an unusable row in a picker is worse
    /// than a missing one. Each action is wrapped in [`Open`] so an action token from a newer
    /// cloud round-trips instead of failing the whole node; an unrecognised one matches nothing,
    /// which is the conservative answer for a fraud control.
    #[serde(default)]
    pub applies_to: Vec<Open<ReasonAction>>,
    /// Whether staff may still pick it. An inactive entry stays in the list so historic events
    /// keep resolving, and disappears from every picker. Absent in the document means active.
    #[serde(default = "active_by_default")]
    pub active: bool,
}

/// The default for [`PublishedReasonCode::active`] when a document omits it: a published reason is
/// one staff may pick.
const fn active_by_default() -> bool {
    true
}

impl PublishedReasonCode {
    /// An active entry valid for `applies_to`.
    #[must_use]
    pub fn new(
        id: ReasonCodeId,
        code: ReasonCode,
        display_name: DisplayName,
        applies_to: Vec<ReasonAction>,
    ) -> Self {
        Self {
            id,
            code,
            display_name,
            display_name_translations: BTreeMap::new(),
            applies_to: applies_to.into_iter().map(Open::from_known).collect(),
            active: true,
        }
    }

    /// The same entry with its per-locale names set. [`display_name`](Self::display_name) stays the
    /// fallback.
    #[must_use]
    pub fn with_name_translations(mut self, translations: BTreeMap<String, DisplayName>) -> Self {
        self.display_name_translations = translations;
        self
    }

    /// The same entry retired: it resolves for a historic event and shows on no picker.
    #[must_use]
    pub fn retired(mut self) -> Self {
        self.active = false;
        self
    }

    /// The name to show in `language`, falling back to [`display_name`](Self::display_name). Total
    /// and never blank.
    #[must_use]
    pub fn localized_name(&self, language: &str) -> &DisplayName {
        self.display_name_translations
            .get(language)
            .unwrap_or(&self.display_name)
    }

    /// Whether this entry declares `action`, whether or not it is still active.
    ///
    /// An unrecognised token in [`applies_to`](Self::applies_to) matches nothing: it degrades to
    /// `UNSPECIFIED`, and `UNSPECIFIED` is not an action anything can be done under.
    #[must_use]
    pub fn declares(&self, action: ReasonAction) -> bool {
        action != ReasonAction::Unspecified
            && self
                .applies_to
                .iter()
                .any(|declared| !declared.is_unrecognised() && declared.known() == action)
    }

    /// Whether staff may cite this entry for `action` right now — it declares the action *and* is
    /// active. This is the one question the domain asks.
    #[must_use]
    pub fn is_valid_for(&self, action: ReasonAction) -> bool {
        self.active && self.declares(action)
    }
}

/// The `reason_codes` config node: one catalogue, each entry tagged with the actions it is valid
/// for.
///
/// One list rather than six, because the spec says *a* list and an operator thinks in reasons
/// ("waste", "wrong item"), not in six parallel lists. A list rather than a map for the same
/// round-trips-in-a-diff reason [`crate::menu::MenuBook`] and [`crate::campaign::PublishedCampaigns`]
/// are lists.
///
/// Unlike every other node, **empty is not the safe default** — see the module documentation. The
/// edge starts from [`framework_default`](Self::framework_default), and a published node replaces
/// it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedReasonCodes {
    #[serde(default)]
    codes: Vec<PublishedReasonCode>,
}

/// The id of a framework-default entry.
///
/// Reserved: a ULID minted from a clock carries a non-zero 48-bit timestamp, so a low value can
/// never collide with an id the cloud generated for an authored entry.
const fn framework_id(index: u128) -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(index))
}

impl PublishedReasonCodes {
    /// An empty node.
    ///
    /// Not what an edge with no published node uses — that is
    /// [`framework_default`](Self::framework_default). This exists for a caller composing a node
    /// entry by entry.
    #[must_use]
    pub const fn new() -> Self {
        Self { codes: Vec::new() }
    }

    /// A node from its entries.
    #[must_use]
    pub const fn from_parts(codes: Vec<PublishedReasonCode>) -> Self {
        Self { codes }
    }

    /// The set the binary always carries, so a store can void, refund and open its drawer before
    /// anyone has published anything and while the cloud is unreachable (ADR-0115).
    ///
    /// Every [`ReasonAction`] is covered by at least one entry — the invariant that makes "absence
    /// is not a brick" true, and the one this module tests. The wording is deliberately minimal
    /// and generic; ADR-0115 records that which reasons a chain actually wants is a business
    /// decision, taken by publishing a node.
    #[must_use]
    pub fn framework_default() -> Self {
        use ReasonAction::{
            CashPaidIn, CashPaidOut, Comp, Discount, DrawerOpen, Refund, RejectOrder,
            StockAdjustment, StockWaste, VoidBill, VoidLine,
        };

        let vi = |text: &str| BTreeMap::from([("vi".to_owned(), DisplayName::new(text))]);

        Self::from_parts(vec![
            PublishedReasonCode::new(
                framework_id(1),
                ReasonCode::new("WASTE"),
                DisplayName::new("Waste or spoilage"),
                vec![VoidLine, StockWaste],
            )
            .with_name_translations(vi("Hàng hỏng hoặc bỏ đi")),
            PublishedReasonCode::new(
                framework_id(2),
                ReasonCode::new("WRONG_ITEM"),
                DisplayName::new("Wrong item"),
                vec![VoidLine, VoidBill, RejectOrder, StockAdjustment],
            )
            .with_name_translations(vi("Sai món")),
            PublishedReasonCode::new(
                framework_id(3),
                ReasonCode::new("CUSTOMER_CHANGED_MIND"),
                DisplayName::new("Customer changed their mind"),
                vec![VoidLine, VoidBill, Refund, RejectOrder],
            )
            .with_name_translations(vi("Khách đổi ý")),
            PublishedReasonCode::new(
                framework_id(4),
                ReasonCode::new("STAFF_ERROR"),
                DisplayName::new("Staff error"),
                vec![VoidLine, VoidBill, Refund, Discount, StockAdjustment],
            )
            .with_name_translations(vi("Nhân viên nhập sai")),
            PublishedReasonCode::new(
                framework_id(5),
                ReasonCode::new("SERVICE_RECOVERY"),
                DisplayName::new("Putting it right for a guest"),
                vec![Discount, Comp, Refund],
            )
            .with_name_translations(vi("Bù cho khách")),
            PublishedReasonCode::new(
                framework_id(6),
                ReasonCode::new("OUT_OF_STOCK"),
                DisplayName::new("Out of stock"),
                vec![VoidLine, RejectOrder, StockWaste],
            )
            .with_name_translations(vi("Hết nguyên liệu")),
            PublishedReasonCode::new(
                framework_id(7),
                ReasonCode::new("MAKING_CHANGE"),
                DisplayName::new("Making change"),
                vec![DrawerOpen, CashPaidIn, CashPaidOut],
            )
            .with_name_translations(vi("Đổi tiền lẻ")),
            PublishedReasonCode::new(
                framework_id(8),
                ReasonCode::new("TEST_TRANSACTION"),
                DisplayName::new("Test transaction"),
                vec![VoidLine, VoidBill],
            )
            .with_name_translations(vi("Giao dịch thử")),
        ])
    }

    /// Every entry, in the order the node lists them — including retired ones, so a historic event
    /// still resolves.
    #[must_use]
    pub fn codes(&self) -> &[PublishedReasonCode] {
        &self.codes
    }

    /// Whether the node carries no entry at all. True only of
    /// [`new`](Self::new) or a published empty list — never of
    /// [`framework_default`](Self::framework_default).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// The entry with this id, active or retired.
    #[must_use]
    pub fn find(&self, id: ReasonCodeId) -> Option<&PublishedReasonCode> {
        self.codes.iter().find(|code| code.id == id)
    }

    /// The question the route asks before it writes the event: may this id be cited for this
    /// action?
    ///
    /// `false` for an id the list does not hold, for an action the entry does not declare, and for
    /// a retired entry — the three ways a reason field stops being a fraud control.
    #[must_use]
    pub fn accepts(&self, id: ReasonCodeId, action: ReasonAction) -> bool {
        self.find(id).is_some_and(|code| code.is_valid_for(action))
    }

    /// The entries a picker should offer for `action`: active, and declaring it.
    pub fn for_action(&self, action: ReasonAction) -> impl Iterator<Item = &PublishedReasonCode> {
        self.codes
            .iter()
            .filter(move |code| code.is_valid_for(action))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        PublishedReasonCode, PublishedReasonCodes, ReasonAction, ReasonCode, framework_id,
    };
    use crate::text::DisplayName;
    use crate::wire_enum::{Open, WireEnum};

    #[test]
    fn the_node_round_trips_through_json() {
        let node = PublishedReasonCodes::framework_default();
        let json = serde_json::to_string(&node).expect("serialize");
        let back: PublishedReasonCodes = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
    }

    #[test]
    fn there_is_one_action_per_event_field_that_demands_a_reason() {
        // The reason this enum has eleven variants and not the specification's six. The catalogue
        // is the authority on which actions need a reason, and it currently names eleven; a
        // twelfth event that declares a `reason_code_id` fails here until `ReasonAction` and the
        // framework default set grow to cover it, rather than shipping a field the console cannot
        // author a value for.
        let demanded = crate::snapshot::render()
            .lines()
            .filter(|line| line.ends_with("\tfield=reason_code_id"))
            .count();
        assert_eq!(
            ReasonAction::ALL.len() - 1,
            demanded,
            "{demanded} event fields demand a reason, and ReasonAction names {} actions",
            ReasonAction::ALL.len() - 1
        );
    }

    #[test]
    fn every_action_the_framework_names_has_a_default_reason() {
        // The invariant behind ADR-0115's "absence is not a brick": an action with no applicable
        // entry has an empty picker, which blocks the action just as surely as an absent node
        // would. If a variant is added to `ReasonAction`, this test is what says the default set
        // has to grow with it.
        let node = PublishedReasonCodes::framework_default();
        for action in ReasonAction::ALL {
            if *action == ReasonAction::Unspecified {
                continue;
            }
            assert!(
                node.for_action(*action).next().is_some(),
                "no framework-default reason applies to {action}"
            );
        }
    }

    #[test]
    fn the_framework_ids_are_distinct_and_could_not_have_come_from_a_clock() {
        let node = PublishedReasonCodes::framework_default();
        let mut seen = Vec::new();
        for code in node.codes() {
            assert!(
                !seen.contains(&code.id),
                "duplicate framework id {}",
                code.id
            );
            assert_eq!(
                code.id.as_ulid().timestamp_ms(),
                0,
                "a framework id must be reserved, not minted from a clock"
            );
            seen.push(code.id);
        }
    }

    #[test]
    fn accepts_answers_no_three_ways() {
        let node = PublishedReasonCodes::framework_default();
        let waste = framework_id(1);

        assert!(node.accepts(waste, ReasonAction::VoidLine));
        // An action the entry does not declare.
        assert!(!node.accepts(waste, ReasonAction::Refund));
        // An id the list does not hold.
        assert!(!node.accepts(framework_id(9_999), ReasonAction::VoidLine));
        // A retired entry.
        let retired = PublishedReasonCodes::from_parts(vec![
            PublishedReasonCode::new(
                waste,
                ReasonCode::new("WASTE"),
                DisplayName::new("Waste"),
                vec![ReasonAction::VoidLine],
            )
            .retired(),
        ]);
        assert!(
            retired.find(waste).is_some(),
            "a retired entry still resolves"
        );
        assert!(!retired.accepts(waste, ReasonAction::VoidLine));
        assert_eq!(retired.for_action(ReasonAction::VoidLine).count(), 0);
    }

    #[test]
    fn unspecified_is_not_an_action_and_an_unknown_token_matches_nothing() {
        // Two halves of the same conservative answer. `Open` degrades an action token from a newer
        // cloud to `UNSPECIFIED`; if `UNSPECIFIED` matched, an entry an older edge does not
        // understand would become valid for everything it was asked about.
        let node = PublishedReasonCodes::framework_default();
        assert!(!node.accepts(framework_id(1), ReasonAction::Unspecified));
        assert_eq!(node.for_action(ReasonAction::Unspecified).count(), 0);

        let json = serde_json::json!({
            "id": framework_id(1).to_string(),
            "code": "FROM_THE_FUTURE",
            "display_name": "Something newer",
            "applies_to": ["REASON_ACTION_TELEPORT"],
        })
        .to_string();
        let entry: PublishedReasonCode = serde_json::from_str(&json).expect("deserialize");
        assert!(entry.active, "an entry that omits `active` is pickable");
        assert!(!entry.declares(ReasonAction::Unspecified));
        for action in ReasonAction::ALL {
            assert!(
                !entry.is_valid_for(*action),
                "an unrecognised action token must not make an entry valid for {action}"
            );
        }
        // …and it round-trips byte-identically, so the store can forward what it cannot read.
        assert_eq!(
            entry.applies_to.first().map(Open::as_wire),
            Some("REASON_ACTION_TELEPORT")
        );
    }

    #[test]
    fn a_name_falls_back_to_english_and_a_translation_overrides_it() {
        let entry = PublishedReasonCode::new(
            framework_id(1),
            ReasonCode::new("WASTE"),
            DisplayName::new("Waste or spoilage"),
            vec![ReasonAction::VoidLine],
        )
        .with_name_translations(BTreeMap::from([(
            "vi".to_owned(),
            DisplayName::new("Hàng hỏng"),
        )]));

        assert_eq!(entry.localized_name("vi").as_str(), "Hàng hỏng");
        assert_eq!(entry.localized_name("ja").as_str(), "Waste or spoilage");

        let untranslated = PublishedReasonCode::new(
            framework_id(2),
            ReasonCode::new("WRONG_ITEM"),
            DisplayName::new("Wrong item"),
            vec![ReasonAction::VoidLine],
        );
        let json = serde_json::to_string(&untranslated).expect("serialize");
        assert!(
            !json.contains("display_name_translations"),
            "an entry with no translations does not carry the field on the wire"
        );
    }

    #[test]
    fn an_empty_node_is_the_default_but_never_the_framework_set() {
        assert!(PublishedReasonCodes::default().is_empty());
        assert!(PublishedReasonCodes::new().is_empty());
        assert!(!PublishedReasonCodes::framework_default().is_empty());
    }
}
