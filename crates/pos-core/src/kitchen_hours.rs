// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The hours a QR order's automatic send keeps
//! ([ADR-0255](../../../docs/adr/0255-a-qr-orders-automatic-send-keeps-the-hours-the-store-sets.md)).
//!
//! A store that sends a guest's order to the kitchen with no tap may do so only in one daily window,
//! in minutes after its local midnight. Outside the window the order arrives as the store's outside
//! mode says. The caller reads the minute from its clock in the store's own time zone; this decides.

use pos_proto::kitchen_release::KitchenRelease;

/// Whether `minute` (after local midnight) falls in the window `(from, until)`: `[from, until)`,
/// wrapping past midnight when `from` is later than `until`, and always when they are equal.
#[must_use]
pub const fn within(window: (u16, u16), minute: u16) -> bool {
    let (from, until) = window;
    if from == until {
        true
    } else if from < until {
        from <= minute && minute < until
    } else {
        minute >= from || minute < until
    }
}

/// How an order that arrives at `minute` reaches the kitchen: `release` itself, unless it is
/// [`KitchenRelease::AutoAcceptAndFire`] and the minute is outside `window`, when it is `outside`.
/// The window narrows only the automatic send (decision 2).
#[must_use]
pub fn release_at(
    release: KitchenRelease,
    window: (u16, u16),
    outside: KitchenRelease,
    minute: u16,
) -> KitchenRelease {
    if release == KitchenRelease::AutoAcceptAndFire && !within(window, minute) {
        outside
    } else {
        release
    }
}

#[cfg(test)]
mod tests {
    use super::{release_at, within};
    use pos_proto::kitchen_release::KitchenRelease::{
        AutoAcceptAndFire, ConfirmFires, ConfirmThenSend,
    };

    #[test]
    fn equal_ends_are_always_open() {
        for minute in [0, 600, 1439] {
            assert!(within((0, 0), minute));
            assert!(within((600, 600), minute));
        }
    }

    #[test]
    fn a_daytime_window_includes_its_start_and_not_its_end() {
        let lunch = (600, 840);
        assert!(!within(lunch, 599));
        assert!(within(lunch, 600));
        assert!(within(lunch, 839));
        assert!(!within(lunch, 840));
    }

    #[test]
    fn a_window_that_starts_later_than_it_ends_wraps_past_midnight() {
        let night = (1320, 120);
        assert!(within(night, 1320));
        assert!(within(night, 1439));
        assert!(within(night, 0));
        assert!(within(night, 119));
        assert!(!within(night, 120));
        assert!(!within(night, 600));
    }

    #[test]
    fn only_the_automatic_send_is_narrowed() {
        let lunch = (600, 840);
        assert_eq!(
            release_at(AutoAcceptAndFire, lunch, ConfirmThenSend, 700),
            AutoAcceptAndFire
        );
        assert_eq!(
            release_at(AutoAcceptAndFire, lunch, ConfirmThenSend, 900),
            ConfirmThenSend
        );
        assert_eq!(
            release_at(AutoAcceptAndFire, lunch, ConfirmFires, 900),
            ConfirmFires
        );
        assert_eq!(
            release_at(ConfirmFires, lunch, ConfirmThenSend, 900),
            ConfirmFires
        );
        assert_eq!(
            release_at(ConfirmThenSend, lunch, ConfirmFires, 900),
            ConfirmThenSend
        );
    }
}
