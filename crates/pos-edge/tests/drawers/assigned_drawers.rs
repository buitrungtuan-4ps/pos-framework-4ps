// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Assigned drawers ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)
//! decision 9), on the edge itself.

use super::*;

// --- Assigned drawers (decision 9) ------------------------------------------------------------

/// The store of [`session`], where each drawer is one person's for its session.
pub(super) fn assigned(model: DrawerModel, enforced: bool) -> EdgeSession {
    let mut session = session(model, enforced);
    session.shift.drawer_access = Open::from_known(DrawerAccess::Assigned);
    session
}

pub(super) fn employee(seed: u128) -> EmployeeId {
    EmployeeId::new(Ulid::from_u128(seed))
}

const ASSIGNMENT: &str = "cash.drawer.override_assignment";

/// Whether `outcome` is the refusal of an act on a drawer assigned to somebody else.
fn assigned_to_another<T>(outcome: &Result<T, AppError>) -> bool {
    matches!(outcome, Err(AppError::DrawerAssignedToAnother))
}

/// 10,000 moved `movement` for making change, by `by` at `scope` on `shift`, approved by `approval`.
async fn moved(
    edge: &Edge<FakeStore>,
    by: Actor,
    scope: TillScope,
    shift: ShiftId,
    movement: CashMovement,
    approval: Option<&Approval>,
) -> Result<ShiftView, AppError> {
    let reason = making_change();
    edge.record_cash_movement_at(by, scope, shift, movement, vnd(10_000), reason, approval)
        .await
}

/// Each `security.permission.overridden` in `log`: the shift it is on, what was approved, and who
/// approved it.
fn overrides(log: &[EventEnvelope<RawPayload>]) -> Vec<(Option<ShiftId>, String, String)> {
    log.iter()
        .filter(|envelope| envelope.event_type.as_str() == "security.permission.overridden")
        .map(|envelope| {
            let data: Value = serde_json::from_str(envelope.data.as_json()).unwrap_or_default();
            let text = |field: &str| data[field].as_str().unwrap_or_default().to_owned();
            (
                envelope.shift_id,
                text("permission_key"),
                text("approver_employee_id"),
            )
        })
        .collect()
}

/// Whom each `cash.shift.opened` in `log` assigns its drawer to, `Null` for nobody.
fn assignees(log: &[EventEnvelope<RawPayload>]) -> Vec<Value> {
    log.iter()
        .filter(|envelope| envelope.event_type.as_str() == "cash.shift.opened")
        .map(|envelope| {
            let data: Value = serde_json::from_str(envelope.data.as_json()).unwrap_or_default();
            data["assigned_employee_id"].clone()
        })
        .collect()
}

#[test]
fn an_assigned_drawer_takes_cash_only_from_its_owner_and_any_other_tender_from_anybody() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), assigned(DrawerModel::PerTerminal, true));
        let (cashier, other) = (person(CASHIER, 0xB1), person(ASKING, 0xB1));
        let bar = edge
            .open_shift_at(cashier, at(BAR), vnd(500_000), None, Some(employee(ASKING)))
            .await
            .expect("the cashier starts the bar's drawer");
        assert_eq!(
            bar.owner,
            Some(employee(CASHIER)),
            "at its own till a drawer is its starter's, whoever they name"
        );
        assert_eq!(
            assignees(&logged(&store).await),
            [json!(employee(CASHIER))],
            "its start records the person by id"
        );

        settle(&edge, cashier, at(BAR), 41, PaymentMethod::Cash)
            .await
            .expect("the owner takes cash");
        let (bill, due) = a_bill(&edge, other, 42).await;
        let refused = edge
            .settle_bill_at(
                other,
                at(BAR),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
                None,
            )
            .await;
        assert!(assigned_to_another(&refused), "{refused:?}");
        let card = edge
            .settle_bill_at(
                other,
                at(BAR),
                bill,
                paid(PaymentMethod::Card, due),
                None,
                None,
            )
            .await
            .expect("a card is the sale's, not the drawer's");
        assert!(!card.open_drawer);
        let said = AppError::DrawerAssignedToAnother.to_string();
        for seed in [CASHIER, ASKING] {
            assert!(!said.contains(&employee(seed).to_string()), "{said}");
        }
    });
}

#[test]
fn nobody_else_pays_in_or_out_opens_counts_or_closes_an_assigned_drawer() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), assigned(DrawerModel::PerTerminal, true));
        let (cashier, other) = (person(CASHIER, 0xB1), person(ASKING, 0xB1));
        let bar = edge
            .open_shift_at(cashier, at(BAR), vnd(500_000), None, None)
            .await
            .expect("the cashier's")
            .shift_id;
        for movement in [CashMovement::PaidIn, CashMovement::PaidOut] {
            let refused = moved(&edge, other, at(BAR), bar, movement, None).await;
            assert!(assigned_to_another(&refused), "{movement:?}: {refused:?}");
        }
        let counted = edge
            .count_shift_at(other, at(BAR), bar, 510_000, None)
            .await;
        assert!(assigned_to_another(&counted), "{counted:?}");
        let closed = edge.close_shift_at(other, at(BAR), bar, None, None).await;
        assert!(assigned_to_another(&closed), "{closed:?}");
        // Opening a drawer without a sale alone is not opening somebody else's.
        let opened = edge
            .open_drawer_no_sale_at(person(SUPERVISOR, 0xB1), at(BAR), making_change(), None)
            .await;
        assert!(assigned_to_another(&opened), "{opened:?}");

        moved(&edge, cashier, at(BAR), bar, CashMovement::PaidIn, None)
            .await
            .expect("the owner pays in");
        edge.count_shift_at(cashier, at(BAR), bar, 510_000, None)
            .await
            .expect("the owner counts");
        let closed = edge
            .close_shift_at(cashier, at(BAR), bar, None, None)
            .await
            .expect("the owner closes");
        assert_eq!(
            (closed.variance, closed.owner),
            (Some(vnd(0)), Some(employee(CASHIER)))
        );
        assert!(overrides(&logged(&store).await).is_empty());
    });
}

#[test]
fn somebody_elses_drawer_takes_the_override_held_directly_or_approved_for_one_act() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), assigned(DrawerModel::PerTerminal, true));
        let bar = edge
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None, None)
            .await
            .expect("the cashier's")
            .shift_id;

        moved(
            &edge,
            person(MANAGER, 0xB1),
            at(BAR),
            bar,
            CashMovement::PaidIn,
            None,
        )
        .await
        .expect("the manager holds the override directly");
        assert!(
            overrides(&logged(&store).await).is_empty(),
            "nobody approved anything"
        );

        let asking = person(ASKING, 0xB1);
        let (bill, due) = a_bill(&edge, asking, 41).await;
        let unapproved = edge
            .settle_bill_at(
                asking,
                at(BAR),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
                None,
            )
            .await;
        assert!(assigned_to_another(&unapproved), "{unapproved:?}");
        let settled = edge
            .settle_bill_at(
                asking,
                at(BAR),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
                Some(&manager()),
            )
            .await
            .expect("a manager approves this one settle");
        assert_eq!(
            (settled.open_drawer, settled.drawer_till),
            (true, Some(till(BAR)))
        );
        let approved = [(
            Some(bar),
            ASSIGNMENT.to_owned(),
            employee(MANAGER).to_string(),
        )];
        assert_eq!(overrides(&logged(&store).await), approved);

        let again = moved(&edge, asking, at(BAR), bar, CashMovement::PaidOut, None).await;
        assert!(
            assigned_to_another(&again),
            "an approval covers one act: {again:?}"
        );
        let supervisor = person(SUPERVISOR, 0xB1);
        let denied = moved(
            &edge,
            supervisor,
            at(BAR),
            bar,
            CashMovement::PaidOut,
            Some(&manager()),
        )
        .await;
        assert!(
            matches!(
                denied,
                Err(AppError::Domain(DomainError::PermissionDenied { permission }))
                    if permission == ASSIGNMENT
            ),
            "nobody approves what a role grants in neither way: {denied:?}"
        );
        assert_eq!(overrides(&logged(&store).await), approved);
    });
}

#[test]
fn one_approver_covers_a_no_sale_opening_and_the_override_beside_it() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), assigned(DrawerModel::PerTerminal, false));
        let bar = edge
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None, None)
            .await
            .expect("the cashier's")
            .shift_id;
        let manager_at_bar = person(MANAGER, 0xB1);
        let unapproved = edge
            .open_drawer_no_sale_at(manager_at_bar, at(BAR), making_change(), None)
            .await;
        assert!(
            matches!(unapproved, Err(AppError::ApprovalRequired)),
            "{unapproved:?}"
        );
        edge.open_drawer_no_sale_at(manager_at_bar, at(BAR), making_change(), Some(&manager()))
            .await
            .expect("where the store does not enforce, a manager approves their own");
        let approver = employee(MANAGER).to_string();
        assert_eq!(
            overrides(&logged(&store).await),
            [
                (
                    Some(bar),
                    "cash.drawer.open_no_sale".to_owned(),
                    approver.clone()
                ),
                (Some(bar), ASSIGNMENT.to_owned(), approver),
            ]
        );
    });
}

#[test]
fn a_drawer_started_for_another_till_is_the_person_named_from_the_roster() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), assigned(DrawerModel::PerTerminal, true));
        let starter = person(SUPERVISOR, 0xB1);
        for nobody in [None, Some(employee(99))] {
            let refused = edge
                .open_shift_at(
                    starter,
                    naming(Some(BAR), COUNTER),
                    vnd(300_000),
                    None,
                    nobody,
                )
                .await;
            assert!(
                matches!(refused, Err(AppError::AssigneeRequired)),
                "{nobody:?}: {refused:?}"
            );
        }
        let counter = edge
            .open_shift_at(
                starter,
                naming(Some(BAR), COUNTER),
                vnd(300_000),
                None,
                Some(employee(CASHIER)),
            )
            .await
            .expect("the supervisor starts the counter's drawer for the cashier");
        assert_eq!(counter.owner, Some(employee(CASHIER)));
        assert_eq!(assignees(&logged(&store).await), [json!(employee(CASHIER))]);

        let at_counter = |seed| person(seed, 0xC1);
        let shift = counter.shift_id;
        let refused = moved(
            &edge,
            at_counter(SUPERVISOR),
            at(COUNTER),
            shift,
            CashMovement::PaidIn,
            None,
        )
        .await;
        assert!(
            assigned_to_another(&refused),
            "the starter is not its owner: {refused:?}"
        );
        moved(
            &edge,
            at_counter(CASHIER),
            at(COUNTER),
            shift,
            CashMovement::PaidIn,
            None,
        )
        .await
        .expect("the person named takes it over");
    });
}

#[test]
fn a_batch_close_takes_the_override_for_each_drawer_that_is_somebody_elses() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), assigned(DrawerModel::PerTerminal, true));
        let mut shifts = Vec::new();
        for (owner, device, own, float) in [
            (CASHIER, 0xB1, BAR, 500_000),
            (MANAGER, 0xC1, COUNTER, 300_000),
        ] {
            let shift = edge
                .open_shift_at(person(owner, device), at(own), vnd(float), None, None)
                .await
                .expect("its owner starts it")
                .shift_id;
            edge.count_shift_at(person(owner, device), at(own), shift, float, None)
                .await
                .expect("its owner counts it");
            shifts.push(shift);
        }
        let (bar, counter) = (shifts[0], shifts[1]);

        let refused = edge
            .close_shifts_at(person(SUPERVISOR, 0xB1), at(BAR), &shifts, None, &[])
            .await;
        assert!(assigned_to_another(&refused), "{refused:?}");
        assert!(
            edge.current_shift_at(at(BAR)).is_some()
                && edge.current_shift_at(at(COUNTER)).is_some(),
            "every drawer or none"
        );

        let closed = edge
            .close_shifts_at(
                person(ASKING, 0xB1),
                at(BAR),
                &shifts,
                Some(&manager()),
                &[],
            )
            .await
            .expect("one approver covers the batch");
        assert_eq!(closed.shifts.len(), 2);
        let approver = employee(MANAGER).to_string();
        assert_eq!(
            overrides(&logged(&store).await),
            [
                (Some(bar), ASSIGNMENT.to_owned(), approver.clone()),
                (
                    Some(counter),
                    "cash.shift.manage_other_till".to_owned(),
                    approver.clone()
                ),
                (Some(counter), ASSIGNMENT.to_owned(), approver),
            ],
            "each drawer somebody else's carries its own override, under the one approver"
        );
    });
}

#[test]
fn a_restart_keeps_whose_each_drawer_is_until_it_closes() {
    run_ready(async {
        let store = FakeStore::new();
        let bar = edge_over(store.clone(), assigned(DrawerModel::PerTerminal, true))
            .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None, None)
            .await
            .expect("the cashier's")
            .shift_id;

        // A store moved back to shared drawers meanwhile shares from each drawer's next start.
        for shared in [false, true] {
            let session = if shared {
                session(DrawerModel::PerTerminal, true)
            } else {
                assigned(DrawerModel::PerTerminal, true)
            };
            let restarted = edge_over(store.clone(), session);
            restarted.rebuild().await.expect("replays the log");
            let view = restarted
                .current_shift_at(at(BAR))
                .expect("the bar's is open");
            assert_eq!((view.shift_id, view.owner), (bar, Some(employee(CASHIER))));
            let refused = restarted
                .count_shift_at(person(ASKING, 0xB1), at(BAR), bar, 500_000, None)
                .await;
            assert!(assigned_to_another(&refused), "{refused:?}");
            if shared {
                restarted
                    .count_shift_at(person(CASHIER, 0xB1), at(BAR), bar, 500_000, None)
                    .await
                    .expect("its owner counts it");
                restarted
                    .close_shift_at(person(CASHIER, 0xB1), at(BAR), bar, None, None)
                    .await
                    .expect("and closes it");
                let next = restarted
                    .open_shift_at(person(CASHIER, 0xB1), at(BAR), vnd(500_000), None, None)
                    .await
                    .expect("the next start");
                assert_eq!(next.owner, None);
            }
        }
    });
}

#[test]
fn shared_drawers_name_nobody_and_take_anybodys_cash() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), session(DrawerModel::PerTerminal, true));
        let bar = edge
            .open_shift_at(
                person(CASHIER, 0xB1),
                at(BAR),
                vnd(500_000),
                None,
                Some(employee(ASKING)),
            )
            .await
            .expect("the bar's");
        let counter = edge
            .open_shift_at(
                person(SUPERVISOR, 0xB1),
                naming(Some(BAR), COUNTER),
                vnd(300_000),
                None,
                None,
            )
            .await
            .expect("another till's, naming nobody");
        assert_eq!((bar.owner, counter.owner), (None, None));

        let other = person(ASKING, 0xB1);
        let shift = bar.shift_id;
        settle(&edge, other, at(BAR), 41, PaymentMethod::Cash)
            .await
            .expect("anybody's cash");
        moved(&edge, other, at(BAR), shift, CashMovement::PaidIn, None)
            .await
            .expect("anybody's paid in");
        edge.count_shift_at(other, at(BAR), shift, 675_000, None)
            .await
            .expect("counted by anybody at its till");
        edge.close_shift_at(other, at(BAR), shift, None, None)
            .await
            .expect("and closed");
        let log = logged(&store).await;
        assert!(
            log.iter()
                .all(|envelope| !envelope.data.as_json().contains("assigned_employee_id")),
            "shared drawers' events name nobody"
        );
        assert!(overrides(&log).is_empty());
    });
}

#[test]
fn the_stores_one_drawer_is_assigned_as_a_tills_is() {
    run_ready(async {
        let store = FakeStore::new();
        let edge = edge_over(store.clone(), assigned(DrawerModel::PerStore, true));
        let open = edge
            .open_shift(person(CASHIER, 0xB1), vnd(500_000))
            .await
            .expect("the cashier starts the store's drawer");
        assert_eq!((open.till, open.owner), (None, Some(employee(CASHIER))));

        let other = person(ASKING, 0xC1);
        let (bill, due) = a_bill(&edge, other, 41).await;
        let refused = edge
            .settle_bill(other, bill, paid(PaymentMethod::Cash, due), None)
            .await;
        assert!(assigned_to_another(&refused), "{refused:?}");
        let settled = edge
            .settle_bill_at(
                other,
                TillScope::default(),
                bill,
                paid(PaymentMethod::Cash, due),
                None,
                Some(&manager()),
            )
            .await
            .expect("approved for this settle");
        assert_eq!((settled.open_drawer, settled.drawer_till), (true, None));
        let log = logged(&store).await;
        assert_eq!(
            overrides(&log),
            [(
                Some(open.shift_id),
                ASSIGNMENT.to_owned(),
                employee(MANAGER).to_string()
            )]
        );
        assert_eq!(assignees(&log), [json!(employee(CASHIER))]);
        assert!(
            log.iter()
                .all(|envelope| !envelope.data.as_json().contains("terminal_device_id")),
            "the person, and still no till"
        );
    });
}
