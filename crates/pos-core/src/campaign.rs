// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Pricing and promotions: one model, a deterministic order, and a hard offline rule.
//!
//! `docs/pos-spec.md` §7. Happy hours, item and category discounts, combos, vouchers and manual
//! reductions are one `Campaign` shape, not five features. Three properties of §7 are mechanical
//! here rather than conventional:
//!
//! - **Evaluation order is deterministic:** item-level → combo → bill-level → voucher → manual, and
//!   within a kind by descending priority, with the campaign id as a final tiebreak. Two devices
//!   evaluating the same bill get the same reductions in the same order.
//! - **Timing is split.** Item and combo rules are evaluated **when the line is added**; bill-level
//!   and voucher rules **when payment begins** ([`CampaignKind::timing`]). A guest who ordered at
//!   16:59 keeps the happy-hour price even if they pay at 17:30, because the line captured it.
//! - **Rules run offline, uniqueness runs online.** Everything evaluates with no connection except a
//!   **voucher**, whose redemption is an atomic check-and-mark against the cloud. With
//!   [`Connectivity::Offline`] the voucher stage is skipped entirely — the button is greyed out —
//!   and everything else still sells.
//!
//! **Every applied campaign appears as its own line on the bill** (§7), so [`evaluate`] returns one
//! [`AppliedCampaign`] per reduction rather than a single merged number — the receipt and the audit
//! trail both need to show which promotion gave what.
//!
//! # What this models, and what it does not yet
//!
//! Actions here are a percentage or a fixed amount off. Combo-price and free-item actions, and
//! customer-group conditions, are named in §7 but need the menu/line model that the `decide`
//! orchestration will carry; they are deliberately left for that slice rather than guessed at now.
//! The evaluation order, the timing split and the offline rule — the parts with correctness
//! guarantees — are complete.

use pos_proto::enums::SalesChannel;
use pos_proto::ids::CampaignId;
use pos_proto::money::{Money, MoneyError, Ratio, Rounding};

/// The five campaign kinds, in the order §7 evaluates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CampaignKind {
    /// A discount on one item or category.
    ItemLevel,
    /// A combo: a set of items at a set price.
    Combo,
    /// A discount on the whole bill.
    BillLevel,
    /// A voucher code, redeemed atomically against the cloud.
    Voucher,
    /// A manual reduction entered by staff.
    Manual,
}

impl CampaignKind {
    /// Every kind, in evaluation order.
    pub const ALL: [Self; 5] = [
        Self::ItemLevel,
        Self::Combo,
        Self::BillLevel,
        Self::Voucher,
        Self::Manual,
    ];

    /// The kind's position in the evaluation order, 0 first.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::ItemLevel => 0,
            Self::Combo => 1,
            Self::BillLevel => 2,
            Self::Voucher => 3,
            Self::Manual => 4,
        }
    }

    /// When this kind is evaluated: item and combo at line-add, everything else at payment start.
    #[must_use]
    pub const fn timing(self) -> Timing {
        match self {
            Self::ItemLevel | Self::Combo => Timing::LineAdd,
            Self::BillLevel | Self::Voucher | Self::Manual => Timing::PaymentStart,
        }
    }

    /// Whether the kind needs a live connection to apply (only vouchers, for the uniqueness check).
    #[must_use]
    pub const fn needs_online(self) -> bool {
        matches!(self, Self::Voucher)
    }
}

/// The two moments a campaign is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// When a line is added to the order.
    LineAdd,
    /// When the guest begins to pay.
    PaymentStart,
}

/// Whether the store currently has a connection to the cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    /// Connected — the voucher uniqueness check can run.
    Online,
    /// Offline — the voucher stage is skipped; everything else still sells.
    Offline,
}

/// What a campaign takes off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A percentage off the base.
    Percentage(Ratio),
    /// A fixed amount off the base.
    AmountOff(Money),
}

impl Action {
    /// The reduction this action computes against `base`, before capping to what remains.
    ///
    /// # Errors
    ///
    /// [`MoneyError`] if the arithmetic overflows or currencies are mixed.
    pub fn reduction(self, base: Money, rounding: Rounding) -> Result<Money, MoneyError> {
        match self {
            Self::Percentage(rate) => base.mul_ratio(rate, rounding),
            Self::AmountOff(amount) => {
                // A fixed amount is in the base's currency; reject a mismatch rather than silently
                // taking the wrong money off.
                if amount.currency_code == base.currency_code {
                    Ok(amount)
                } else {
                    Err(MoneyError::CurrencyMismatch {
                        left: base.currency_code,
                        right: amount.currency_code,
                    })
                }
            }
        }
    }
}

/// A day of the week, for schedule windows. `Monday` is 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Weekday {
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday.
    Sunday,
}

impl Weekday {
    /// The day's bit, 0 for Monday.
    const fn bit(self) -> u8 {
        1_u8 << (self as u8)
    }
}

/// A set of weekdays, as a 7-bit mask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WeekdaySet(u8);

impl WeekdaySet {
    /// Every day.
    pub const EVERY_DAY: Self = Self(0b0111_1111);

    /// An empty set.
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Returns the set with `day` added.
    #[must_use]
    pub const fn with(self, day: Weekday) -> Self {
        Self(self.0 | day.bit())
    }

    /// Whether `day` is in the set.
    #[must_use]
    pub const fn contains(self, day: Weekday) -> bool {
        self.0 & day.bit() != 0
    }
}

impl FromIterator<Weekday> for WeekdaySet {
    fn from_iter<I: IntoIterator<Item = Weekday>>(iter: I) -> Self {
        let mut set = Self::none();
        for day in iter {
            set = set.with(day);
        }
        set
    }
}

/// A weekly schedule window: which days, and which minutes of the day.
///
/// A window may wrap past midnight (`start_minute > end_minute`), for a late happy hour. The window
/// is half-open: `start_minute` is included, `end_minute` is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    /// The days the window applies.
    pub days: WeekdaySet,
    /// The first included minute of the day, 0–1439.
    pub start_minute: u16,
    /// The first excluded minute of the day, 0–1439.
    pub end_minute: u16,
}

impl Schedule {
    /// Whether `at` falls inside the window.
    #[must_use]
    pub const fn is_active_at(&self, at: LocalTime) -> bool {
        if !self.days.contains(at.weekday) {
            return false;
        }
        let minute = at.minute_of_day;
        if self.start_minute <= self.end_minute {
            minute >= self.start_minute && minute < self.end_minute
        } else {
            // Wraps midnight: active late in the day or early the next.
            minute >= self.start_minute || minute < self.end_minute
        }
    }
}

/// A local wall-clock moment, reduced to what a schedule needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    /// The day of the week, in the store's timezone.
    pub weekday: Weekday,
    /// Minutes since local midnight, 0–1439.
    pub minute_of_day: u16,
}

/// The conditions a campaign requires before it applies.
#[derive(Debug, Clone, Default)]
pub struct Conditions {
    /// A minimum bill total. `None` means no minimum.
    pub min_bill: Option<Money>,
    /// The sales channels it applies on. `None` means every channel.
    pub channels: Option<Vec<SalesChannel>>,
    /// The schedule window. `None` means always active.
    pub schedule: Option<Schedule>,
}

impl Conditions {
    /// Whether these conditions are met in `ctx`.
    ///
    /// # Errors
    ///
    /// [`MoneyError`] if the minimum-bill comparison mixes currencies.
    fn are_met(&self, ctx: &EvalContext) -> Result<bool, MoneyError> {
        if let Some(schedule) = &self.schedule
            && !schedule.is_active_at(ctx.now)
        {
            return Ok(false);
        }
        if let Some(channels) = &self.channels
            && !channels.contains(&ctx.channel)
        {
            return Ok(false);
        }
        if let Some(min_bill) = self.min_bill {
            // Met when bill_total − min_bill is not negative.
            if ctx.bill_total.checked_sub(min_bill)?.is_negative() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// One campaign in the catalogue.
#[derive(Debug, Clone)]
pub struct Campaign {
    /// Its stable id — the tiebreak that makes ordering total.
    pub id: CampaignId,
    /// Which of the five kinds it is.
    pub kind: CampaignKind,
    /// Priority within its kind and exclusion group; higher applies first.
    pub priority: i32,
    /// An exclusion group: at most one campaign per group applies to a bill. `None` means it stacks
    /// with everything.
    pub exclusion_group: Option<u16>,
    /// What it takes off.
    pub action: Action,
    /// What must hold for it to apply.
    pub conditions: Conditions,
    /// Remaining quota; `None` is unlimited, `Some(0)` is exhausted.
    pub quota_remaining: Option<u32>,
}

/// Everything [`evaluate`] needs about the bill and the moment.
#[derive(Debug, Clone)]
pub struct EvalContext {
    /// The amount campaigns reduce: a line subtotal at [`Timing::LineAdd`], the bill subtotal at
    /// [`Timing::PaymentStart`].
    pub base: Money,
    /// The whole bill's total, for minimum-bill conditions.
    pub bill_total: Money,
    /// The sales channel.
    pub channel: SalesChannel,
    /// The local wall-clock moment of evaluation.
    pub now: LocalTime,
    /// Whether the store is online (gates the voucher stage).
    pub connectivity: Connectivity,
    /// How to round a percentage reduction.
    pub rounding: Rounding,
}

/// A campaign that applied, and the reduction it produced — one bill line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedCampaign {
    /// Which campaign applied.
    pub campaign: CampaignId,
    /// Its kind, so the receipt can group and order the lines.
    pub kind: CampaignKind,
    /// The reduction, capped so it never takes the base below zero.
    pub reduction: Money,
}

/// Evaluates the campaigns due at `timing` against `ctx`, in §7's deterministic order.
///
/// Campaigns whose kind does not match `timing` are ignored, so this is called twice per bill: once
/// at line-add with the line as the base, once at payment start with the bill subtotal. Within the
/// call, campaigns run in kind order then by descending priority; an exclusion group admits only its
/// first (highest-priority) match; conditions and quota gate each one; and each reduction is computed
/// against the amount **remaining** after the previous ones, so the sum can never exceed the base.
///
/// The voucher stage is skipped entirely when [`Connectivity::Offline`] — rules run offline,
/// uniqueness runs online.
///
/// # Errors
///
/// [`MoneyError`] if any reduction arithmetic overflows or mixes currencies.
pub fn evaluate(
    campaigns: &[Campaign],
    timing: Timing,
    ctx: &EvalContext,
) -> Result<Vec<AppliedCampaign>, MoneyError> {
    // The candidates for this timing, sorted into the deterministic order.
    let mut candidates: Vec<&Campaign> = campaigns
        .iter()
        .filter(|campaign| campaign.kind.timing() == timing)
        .filter(|campaign| {
            ctx.connectivity == Connectivity::Online || !campaign.kind.needs_online()
        })
        .collect();
    candidates.sort_by(|left, right| {
        left.kind
            .rank()
            .cmp(&right.kind.rank())
            .then(right.priority.cmp(&left.priority))
            .then(left.id.cmp(&right.id))
    });

    let mut applied = Vec::new();
    let mut spent_groups: Vec<u16> = Vec::new();
    let mut remaining = ctx.base;

    for campaign in candidates {
        if campaign.quota_remaining == Some(0) {
            continue;
        }
        if let Some(group) = campaign.exclusion_group
            && spent_groups.contains(&group)
        {
            continue;
        }
        if !campaign.conditions.are_met(ctx)? {
            continue;
        }
        if remaining.is_zero() {
            break;
        }

        let raw = campaign.action.reduction(remaining, ctx.rounding)?;
        // Never take more than what is left.
        let capped = if raw.amount_minor > remaining.amount_minor {
            remaining
        } else {
            raw
        };
        if capped.is_zero() {
            continue;
        }
        remaining = remaining.checked_sub(capped)?;
        if let Some(group) = campaign.exclusion_group {
            spent_groups.push(group);
        }
        applied.push(AppliedCampaign {
            campaign: campaign.id,
            kind: campaign.kind,
            reduction: capped,
        });
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::{
        Action, Campaign, CampaignKind, Conditions, Connectivity, EvalContext, LocalTime, Schedule,
        Timing, Weekday, WeekdaySet, evaluate,
    };
    use pos_proto::enums::SalesChannel;
    use pos_proto::ids::CampaignId;
    use pos_proto::money::{CurrencyCode, Money, Ratio, Rounding};
    use pos_proto::ulid::Ulid;

    fn campaign_id(n: u128) -> CampaignId {
        CampaignId::new(Ulid::from_u128(n))
    }

    fn vnd(amount: i64) -> Money {
        Money::new(CurrencyCode::VND, amount)
    }

    fn percent(value: i64) -> Action {
        Action::Percentage(Ratio::percent(value).expect("valid percent"))
    }

    /// A minimal campaign with no conditions, unlimited quota, no exclusion group.
    fn bare(id: u128, kind: CampaignKind, priority: i32, action: Action) -> Campaign {
        Campaign {
            id: campaign_id(id),
            kind,
            priority,
            exclusion_group: None,
            action,
            conditions: Conditions::default(),
            quota_remaining: None,
        }
    }

    fn ctx(base: Money) -> EvalContext {
        // bill_total defaults to the base for the simple cases; individual tests override it.
        EvalContext {
            base,
            bill_total: base,
            channel: SalesChannel::DineIn,
            now: LocalTime {
                weekday: Weekday::Monday,
                minute_of_day: 12 * 60,
            },
            connectivity: Connectivity::Online,
            rounding: Rounding::HalfUp,
        }
    }

    #[test]
    fn kinds_evaluate_in_the_fixed_order() {
        // A bill-level and a voucher campaign, both due at payment start; the order must be
        // bill-level then voucher regardless of insertion order.
        let campaigns = vec![
            bare(2, CampaignKind::Voucher, 0, percent(10)),
            bare(1, CampaignKind::BillLevel, 0, percent(10)),
        ];
        let context = ctx(vnd(100_000));
        let applied = evaluate(&campaigns, Timing::PaymentStart, &context).expect("evaluates");
        let kinds: Vec<CampaignKind> = applied.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, vec![CampaignKind::BillLevel, CampaignKind::Voucher]);
    }

    #[test]
    fn timing_splits_line_add_from_payment_start() {
        let campaigns = vec![
            bare(1, CampaignKind::ItemLevel, 0, percent(10)),
            bare(2, CampaignKind::BillLevel, 0, percent(10)),
        ];
        let context = ctx(vnd(100_000));
        let at_line = evaluate(&campaigns, Timing::LineAdd, &context).expect("line");
        let at_pay = evaluate(&campaigns, Timing::PaymentStart, &context).expect("pay");
        assert_eq!(at_line.len(), 1);
        assert_eq!(
            at_line.first().map(|a| a.kind),
            Some(CampaignKind::ItemLevel)
        );
        assert_eq!(at_pay.len(), 1);
        assert_eq!(
            at_pay.first().map(|a| a.kind),
            Some(CampaignKind::BillLevel)
        );
    }

    #[test]
    fn a_voucher_is_skipped_offline_but_the_rest_still_apply() {
        let campaigns = vec![
            bare(1, CampaignKind::BillLevel, 0, percent(10)),
            bare(2, CampaignKind::Voucher, 0, percent(10)),
        ];
        let mut context = ctx(vnd(100_000));
        context.connectivity = Connectivity::Offline;
        let applied = evaluate(&campaigns, Timing::PaymentStart, &context).expect("evaluates");
        let kinds: Vec<CampaignKind> = applied.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            vec![CampaignKind::BillLevel],
            "the voucher stage is greyed out offline"
        );
    }

    #[test]
    fn an_exclusion_group_admits_only_its_highest_priority() {
        let mut low = bare(1, CampaignKind::BillLevel, 1, percent(10));
        let mut high = bare(2, CampaignKind::BillLevel, 5, percent(20));
        low.exclusion_group = Some(7);
        high.exclusion_group = Some(7);
        let campaigns = vec![low, high];
        let context = ctx(vnd(100_000));
        let applied = evaluate(&campaigns, Timing::PaymentStart, &context).expect("evaluates");
        assert_eq!(applied.len(), 1, "only one campaign from the group applies");
        assert_eq!(
            applied.first().map(|a| a.reduction),
            Some(vnd(20_000)),
            "the higher priority wins"
        );
    }

    #[test]
    fn a_happy_hour_applies_inside_its_window_only() {
        let mut lunch = bare(1, CampaignKind::ItemLevel, 0, percent(15));
        lunch.conditions.schedule = Some(Schedule {
            days: WeekdaySet::EVERY_DAY,
            start_minute: 16 * 60,
            end_minute: 17 * 60,
        });
        let campaigns = vec![lunch];

        // 16:59 — inside; the line captures the price.
        let mut at_1659 = ctx(vnd(100_000));
        at_1659.now = LocalTime {
            weekday: Weekday::Monday,
            minute_of_day: 16 * 60 + 59,
        };
        assert_eq!(
            evaluate(&campaigns, Timing::LineAdd, &at_1659)
                .expect("eval")
                .len(),
            1
        );

        // 17:30 — outside.
        let mut at_1730 = ctx(vnd(100_000));
        at_1730.now = LocalTime {
            weekday: Weekday::Monday,
            minute_of_day: 17 * 60 + 30,
        };
        assert!(
            evaluate(&campaigns, Timing::LineAdd, &at_1730)
                .expect("eval")
                .is_empty()
        );
    }

    #[test]
    fn a_wrapping_window_covers_past_midnight() {
        let schedule = Schedule {
            days: WeekdaySet::EVERY_DAY,
            start_minute: 22 * 60,
            end_minute: 2 * 60,
        };
        let late = LocalTime {
            weekday: Weekday::Friday,
            minute_of_day: 23 * 60,
        };
        let early = LocalTime {
            weekday: Weekday::Friday,
            minute_of_day: 60,
        };
        let midday = LocalTime {
            weekday: Weekday::Friday,
            minute_of_day: 12 * 60,
        };
        assert!(schedule.is_active_at(late));
        assert!(schedule.is_active_at(early));
        assert!(!schedule.is_active_at(midday));
    }

    #[test]
    fn a_minimum_bill_gates_a_bill_level_campaign() {
        let mut big_spender = bare(1, CampaignKind::BillLevel, 0, percent(10));
        big_spender.conditions.min_bill = Some(vnd(200_000));
        let campaigns = vec![big_spender];

        let mut under = ctx(vnd(100_000));
        under.bill_total = vnd(150_000);
        assert!(
            evaluate(&campaigns, Timing::PaymentStart, &under)
                .expect("eval")
                .is_empty()
        );

        let mut over = ctx(vnd(100_000));
        over.bill_total = vnd(250_000);
        assert_eq!(
            evaluate(&campaigns, Timing::PaymentStart, &over)
                .expect("eval")
                .len(),
            1
        );
    }

    #[test]
    fn a_channel_condition_gates_by_channel() {
        let mut dine_in_only = bare(1, CampaignKind::ItemLevel, 0, percent(10));
        dine_in_only.conditions.channels = Some(vec![SalesChannel::DineIn]);
        let campaigns = vec![dine_in_only];

        let context = ctx(vnd(100_000)); // DineIn by default
        assert_eq!(
            evaluate(&campaigns, Timing::LineAdd, &context)
                .expect("eval")
                .len(),
            1
        );

        let mut takeaway = ctx(vnd(100_000));
        takeaway.channel = SalesChannel::Takeaway;
        assert!(
            evaluate(&campaigns, Timing::LineAdd, &takeaway)
                .expect("eval")
                .is_empty()
        );
    }

    #[test]
    fn an_exhausted_quota_is_skipped() {
        let mut used_up = bare(1, CampaignKind::BillLevel, 0, percent(10));
        used_up.quota_remaining = Some(0);
        let campaigns = vec![used_up];
        let context = ctx(vnd(100_000));
        assert!(
            evaluate(&campaigns, Timing::PaymentStart, &context)
                .expect("eval")
                .is_empty()
        );
    }

    #[test]
    fn reductions_never_exceed_the_base() {
        // Two stacking 60%-off campaigns computed on the running remainder: 60% then 60% of what is
        // left, never more than the base in total.
        let campaigns = vec![
            bare(1, CampaignKind::BillLevel, 2, percent(60)),
            bare(2, CampaignKind::BillLevel, 1, percent(60)),
        ];
        let context = ctx(vnd(100_000));
        let applied = evaluate(&campaigns, Timing::PaymentStart, &context).expect("eval");
        let total: i64 = applied.iter().map(|a| a.reduction.amount_minor).sum();
        assert!(
            total <= 100_000,
            "the sum of reductions cannot exceed the base"
        );
        // 60,000 then 60% of the remaining 40,000 = 24,000; total 84,000.
        assert_eq!(total, 84_000);
    }

    #[test]
    fn each_applied_campaign_is_its_own_line() {
        let campaigns = vec![
            bare(1, CampaignKind::BillLevel, 2, percent(10)),
            bare(2, CampaignKind::BillLevel, 1, Action::AmountOff(vnd(5_000))),
        ];
        let context = ctx(vnd(100_000));
        let applied = evaluate(&campaigns, Timing::PaymentStart, &context).expect("eval");
        assert_eq!(applied.len(), 2, "two campaigns, two lines");
        assert_eq!(applied.first().map(|a| a.reduction), Some(vnd(10_000)));
        assert_eq!(applied.get(1).map(|a| a.reduction), Some(vnd(5_000)));
    }
}
