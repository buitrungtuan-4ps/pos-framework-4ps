// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The decision spine: `decide(state, command, ctx) -> Decision`.
//!
//! This is where the domain's pieces meet. A command arrives with the current aggregate state and a
//! [`DecisionCtx`]; the domain checks it against the permission registry (§9), the capability
//! profile (§10) and the aggregate's state machine (`architecture.md` §5), and — if it is allowed —
//! returns a [`LineDecision`] carrying the next state, the inventory ledger writes to append, and the
//! **effects to fire after commit**. It performs no I/O and awaits nothing: `now` and every flag
//! arrive as values, so the clock is read once and configuration cannot shift mid-decision
//! (`docs/adr/0013-async-strategy.md`).
//!
//! # Why the context is one value
//!
//! [`DecisionCtx`] is the single place a decision reads ambient truth: the instant, the business
//! date derived from it, who is acting, what they are granted, which capabilities the store has, and
//! whether it is online. Passing it by value is what makes two rules from earlier phases mechanical
//! rather than hoped-for — the clock is read exactly once per decision because `now` is a field, and
//! flags are read through [`CapabilityContext`] because there is no other flag to read.
//!
//! # This slice: the order line
//!
//! The order line is the richest command family — it is the one that exercises a PIN-gated permission
//! ([`Permission::VoidFiredLine`]), a capability gate ([`Capability::Courses`]), a state-machine
//! transition ([`OrderLine`]), and inventory consumption all at once — so it is the first aggregate
//! wired to this spine. Bill, shift and table commands follow the same shape: their own command
//! enum, the same [`DecisionCtx`], the same [`Effect`] vocabulary, a `decide_*` returning a decision.
//!
//! [`Permission::VoidFiredLine`]: crate::permission::Permission::VoidFiredLine
//! [`Capability::Courses`]: crate::capability::Capability::Courses

use pos_proto::OrderLineState;
use pos_proto::ids::{CourseId, DeviceId, EmployeeId, MenuItemId};
use pos_proto::money::CurrencyCode;
use pos_proto::quantity::Quantity;
use pos_proto::time::{BusinessDate, Timestamp};

use crate::campaign::Connectivity;
use crate::capability::{Capability, CapabilityContext};
use crate::error::DomainError;
use crate::inventory::{RecipeBook, StockMovement, consumption_for_fire};
use crate::machines::{LineTrigger, OrderLine};
use crate::permission::{Grant, Permission, PermissionSet, require};
use crate::state_machine::StateMachine;

/// Who is performing an action, for authorisation and the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    /// The employee acting.
    pub employee_id: EmployeeId,
    /// The device they are acting from.
    pub device_id: DeviceId,
}

/// Everything a decision reads that is not the command or the aggregate state.
///
/// Read once, passed by value. `now` is a value, not a clock, so a decision cannot read the time
/// twice and get two answers; `capabilities` is the one flag surface; `granted` is the one
/// permission surface.
#[derive(Debug, Clone)]
pub struct DecisionCtx {
    /// The instant the command is being decided, stamped by the caller from its `ClockSource`.
    pub now: Timestamp,
    /// The business date `now` falls in, derived once (ADR-0014) and carried so downstream events
    /// and rollups key on the same value.
    pub business_date: BusinessDate,
    /// Who is acting.
    pub actor: Actor,
    /// The permissions the actor's role grants, synced from the cloud (§9).
    pub granted: PermissionSet,
    /// The store's capability profile (§10).
    pub capabilities: CapabilityContext,
    /// Whether the store is currently online.
    pub connectivity: Connectivity,
    /// The store's currency, so money produced by a decision is in the right unit.
    pub currency: CurrencyCode,
}

impl DecisionCtx {
    /// Authorises a permission against what the actor is granted (§9's one gate).
    ///
    /// # Errors
    ///
    /// [`DomainError::PermissionDenied`] if the actor's set does not grant it.
    pub fn require(&self, permission: Permission) -> Result<Grant, DomainError> {
        require(permission, self.granted)
    }

    /// Requires a store capability (§10's one flag read point).
    ///
    /// # Errors
    ///
    /// [`DomainError::CapabilityDisabled`] if the store does not have it on.
    pub const fn require_capability(&self, capability: Capability) -> Result<(), DomainError> {
        self.capabilities.require(capability)
    }
}

/// A side effect to perform **after** the decision's events commit.
///
/// Effects are things the outside world must do that are not themselves domain state: printing,
/// recomputing a projection, notifying a marketplace. They run after commit so a rolled-back
/// transaction never prints a ticket. Deliberately `non_exhaustive` — new commands add effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Print a void ticket to the kitchen, so the line is un-made rather than silently dropped.
    PrintVoidTicket,
    /// Recompute availability and, if an item crossed its threshold, auto-86 it and tell the
    /// marketplaces (§8).
    RecheckAvailability,
}

/// The outcome of an order-line command: the next state, the inventory ledger to append, and the
/// effects to fire after commit.
///
/// `#[must_use]` because dropping a decision drops the events and effects with it — a settled
/// intention that never happened.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDecision {
    /// The line's state after the command.
    pub next_state: OrderLineState,
    /// Stock movements to append to the ledger — the consumption a fire causes. Empty for commands
    /// that consume nothing.
    pub stock_movements: Vec<StockMovement>,
    /// Effects to run after the events commit.
    pub effects: Vec<Effect>,
}

/// A command against one order line.
#[derive(Debug, Clone)]
pub enum LineCommand {
    /// Hold the line (do not fire it yet).
    Hold,
    /// Resume a held line.
    Resume,
    /// Fire the line to the kitchen, consuming its recipe. `by_course` fires it as part of a course
    /// and requires the `courses_enabled` capability.
    Fire {
        /// The item being made.
        base_item: MenuItemId,
        /// The chosen modifiers, each with its own recipe.
        modifiers: Vec<MenuItemId>,
        /// How many, as a [`Quantity`] (supports halves for split items).
        quantity: Quantity,
        /// The course to fire against, if firing by course.
        course: Option<CourseId>,
    },
    /// Void the line. Voiding a line that has already fired needs [`Permission::VoidFiredLine`] and,
    /// because that permission is PIN-flagged, a verified PIN.
    Void {
        /// Whether the edge has collected and verified the actor's PIN for this action.
        pin_verified: bool,
    },
}

/// Decides an order-line command against the line's current state.
///
/// The one place a line command is checked: the state machine decides whether the transition is
/// legal at all, the permission registry gates a void-after-fire, the capability profile gates
/// firing by course, and inventory computes what a fire consumes. Nothing here performs I/O — the
/// caller appends the returned events inside its transaction and runs the effects after it commits.
///
/// # Errors
///
/// - [`DomainError::Transition`] if the command is not a legal move from `current`.
/// - [`DomainError::PermissionDenied`] if a void-after-fire is not granted, or is granted but its
///   required PIN was not verified.
/// - [`DomainError::CapabilityDisabled`] if firing by course but `courses_enabled` is off.
/// - [`DomainError::Money`] if the consumption arithmetic overflows.
pub fn decide_line(
    current: OrderLineState,
    command: LineCommand,
    ctx: &DecisionCtx,
    book: &RecipeBook,
) -> Result<LineDecision, DomainError> {
    match command {
        LineCommand::Hold => transition_only(current, LineTrigger::Hold),
        LineCommand::Resume => transition_only(current, LineTrigger::Resume),
        LineCommand::Fire {
            base_item,
            modifiers,
            quantity,
            course,
        } => {
            if course.is_some() {
                ctx.require_capability(Capability::Courses)?;
            }
            let next_state = OrderLine::step(current, LineTrigger::Fire)?;
            let stock_movements = consumption_for_fire(
                base_item,
                &modifiers,
                quantity,
                book,
                pos_proto::money::Rounding::HalfUp,
            )?;
            Ok(LineDecision {
                next_state,
                stock_movements,
                effects: vec![Effect::RecheckAvailability],
            })
        }
        LineCommand::Void { pin_verified } => {
            let next_state = OrderLine::step(current, LineTrigger::Void)?;
            // Voiding a line that already fired is the fraud-sensitive case (§9, §11): it needs the
            // permission, and because that permission is PIN-flagged it needs a verified PIN. Voiding
            // a line that never fired is an ordinary cancel.
            let mut effects = Vec::new();
            if current == OrderLineState::Fired {
                let grant = ctx.require(Permission::VoidFiredLine)?;
                if grant.pin_required && !pin_verified {
                    return Err(DomainError::PermissionDenied {
                        permission: Permission::VoidFiredLine.meta().id,
                    });
                }
                // The kitchen already made it, so a void ticket must print. Default config does not
                // return stock (§8): the consumption booked at fire stands and the void is recorded
                // as waste for reporting via the void event itself, so no new stock movement is
                // emitted here.
                effects.push(Effect::PrintVoidTicket);
            }
            Ok(LineDecision {
                next_state,
                stock_movements: Vec::new(),
                effects,
            })
        }
    }
}

/// A transition that carries no permission, capability, inventory or effect — Hold and Resume.
fn transition_only(
    current: OrderLineState,
    trigger: LineTrigger,
) -> Result<LineDecision, DomainError> {
    let next_state = OrderLine::step(current, trigger)?;
    Ok(LineDecision {
        next_state,
        stock_movements: Vec::new(),
        effects: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::{Actor, DecisionCtx, Effect, LineCommand, decide_line};
    use crate::campaign::Connectivity;
    use crate::capability::{Capability, CapabilityContext};
    use crate::error::DomainError;
    use crate::inventory::{Recipe, RecipeBook, RecipeLine};
    use crate::permission::{Permission, PermissionSet};
    use pos_proto::ids::{CourseId, DeviceId, EmployeeId, IngredientId, MenuItemId};
    use pos_proto::money::CurrencyCode;
    use pos_proto::quantity::Quantity;
    use pos_proto::time::{BusinessDate, Timestamp};
    use pos_proto::{OrderLineState, Ulid};

    fn menu_item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(n))
    }

    fn ingredient(n: u128) -> IngredientId {
        IngredientId::new(Ulid::from_u128(n))
    }

    /// A context with a given permission set and capability profile; online, VND, a fixed instant.
    fn ctx_with(granted: PermissionSet, capabilities: CapabilityContext) -> DecisionCtx {
        DecisionCtx {
            now: Timestamp::EPOCH,
            business_date: BusinessDate::from_ymd(2026, 8, 19).expect("valid date"),
            actor: Actor {
                employee_id: EmployeeId::new(Ulid::from_u128(1)),
                device_id: DeviceId::new(Ulid::from_u128(2)),
            },
            granted,
            capabilities,
            connectivity: Connectivity::Online,
            currency: CurrencyCode::VND,
        }
    }

    fn book_with_one_recipe(item: MenuItemId, ing: IngredientId) -> RecipeBook {
        let mut book = RecipeBook::new();
        book.insert(
            item,
            Recipe::new(vec![RecipeLine {
                ingredient: ing,
                per_unit: Quantity::from_milli(100_000),
            }]),
        );
        book
    }

    #[test]
    fn holding_then_resuming_moves_the_line() {
        let ctx = ctx_with(PermissionSet::EMPTY, CapabilityContext::NONE);
        let book = RecipeBook::new();
        let held =
            decide_line(OrderLineState::Added, LineCommand::Hold, &ctx, &book).expect("hold");
        assert_eq!(held.next_state, OrderLineState::Held);
        let resumed =
            decide_line(OrderLineState::Held, LineCommand::Resume, &ctx, &book).expect("resume");
        assert_eq!(resumed.next_state, OrderLineState::Added);
    }

    #[test]
    fn an_illegal_transition_is_refused() {
        let ctx = ctx_with(PermissionSet::EMPTY, CapabilityContext::NONE);
        let book = RecipeBook::new();
        // Resume is only legal from Held.
        assert!(matches!(
            decide_line(OrderLineState::Added, LineCommand::Resume, &ctx, &book),
            Err(DomainError::Transition(_))
        ));
    }

    #[test]
    fn firing_consumes_the_recipe_and_asks_for_a_recheck() {
        let item = menu_item(1);
        let ing = ingredient(2);
        let ctx = ctx_with(PermissionSet::EMPTY, CapabilityContext::NONE);
        let book = book_with_one_recipe(item, ing);
        let fired = decide_line(
            OrderLineState::Added,
            LineCommand::Fire {
                base_item: item,
                modifiers: Vec::new(),
                quantity: Quantity::ONE,
                course: None,
            },
            &ctx,
            &book,
        )
        .expect("fire");
        assert_eq!(fired.next_state, OrderLineState::Fired);
        assert_eq!(fired.stock_movements.len(), 1);
        assert_eq!(
            fired.stock_movements.first().map(|m| m.delta.milli),
            Some(-100_000)
        );
        assert!(fired.effects.contains(&Effect::RecheckAvailability));
    }

    #[test]
    fn firing_by_course_needs_the_courses_capability() {
        let item = menu_item(1);
        let ing = ingredient(2);
        let book = book_with_one_recipe(item, ing);
        let command = || LineCommand::Fire {
            base_item: item,
            modifiers: Vec::new(),
            quantity: Quantity::ONE,
            course: Some(CourseId::new(Ulid::from_u128(9))),
        };

        // Courses off → refused.
        let no_courses = ctx_with(PermissionSet::EMPTY, CapabilityContext::NONE);
        assert!(matches!(
            decide_line(OrderLineState::Added, command(), &no_courses, &book),
            Err(DomainError::CapabilityDisabled {
                capability: "courses_enabled"
            })
        ));

        // Courses on → allowed.
        let with_courses = ctx_with(
            PermissionSet::EMPTY,
            CapabilityContext::NONE.with(Capability::Courses),
        );
        assert!(decide_line(OrderLineState::Added, command(), &with_courses, &book).is_ok());
    }

    #[test]
    fn voiding_a_fired_line_needs_the_permission() {
        let ctx = ctx_with(PermissionSet::EMPTY, CapabilityContext::NONE);
        let book = RecipeBook::new();
        assert!(matches!(
            decide_line(
                OrderLineState::Fired,
                LineCommand::Void { pin_verified: true },
                &ctx,
                &book
            ),
            Err(DomainError::PermissionDenied {
                permission: "sales.line.void_fired"
            })
        ));
    }

    #[test]
    fn voiding_a_fired_line_needs_a_verified_pin_even_when_granted() {
        let granted = PermissionSet::EMPTY.with(Permission::VoidFiredLine);
        let ctx = ctx_with(granted, CapabilityContext::NONE);
        let book = RecipeBook::new();
        // Granted, but the PIN was not verified — the permission is PIN-flagged, so it is denied.
        assert!(matches!(
            decide_line(
                OrderLineState::Fired,
                LineCommand::Void {
                    pin_verified: false
                },
                &ctx,
                &book
            ),
            Err(DomainError::PermissionDenied { .. })
        ));
    }

    #[test]
    fn voiding_a_fired_line_with_permission_and_pin_prints_a_void_ticket() {
        let granted = PermissionSet::EMPTY.with(Permission::VoidFiredLine);
        let ctx = ctx_with(granted, CapabilityContext::NONE);
        let book = RecipeBook::new();
        let decision = decide_line(
            OrderLineState::Fired,
            LineCommand::Void { pin_verified: true },
            &ctx,
            &book,
        )
        .expect("void");
        assert_eq!(decision.next_state, OrderLineState::Voided);
        assert!(decision.effects.contains(&Effect::PrintVoidTicket));
        assert!(
            decision.stock_movements.is_empty(),
            "default config does not return stock on a void-after-fire"
        );
    }

    #[test]
    fn voiding_an_unfired_line_is_an_ordinary_cancel() {
        // No permission, no PIN, no ticket — the kitchen never saw it.
        let ctx = ctx_with(PermissionSet::EMPTY, CapabilityContext::NONE);
        let book = RecipeBook::new();
        let decision = decide_line(
            OrderLineState::Added,
            LineCommand::Void {
                pin_verified: false,
            },
            &ctx,
            &book,
        )
        .expect("cancel");
        assert_eq!(decision.next_state, OrderLineState::Voided);
        assert!(decision.effects.is_empty());
    }
}
