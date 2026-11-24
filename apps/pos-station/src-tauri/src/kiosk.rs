// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Kiosk behaviour that does not depend on the Windows edition
//! ([ADR-0201](../../../../docs/adr/0201-the-till-runs-on-every-device-class.md) decision 2).
//!
//! With `kiosk` on in `station.json`, the till's window is full screen, frameless and on top, it
//! has no close or minimise button, and a request to close it (Alt+F4, the taskbar) is refused; the
//! tray offers no Quit. What leaves it is the till's own exit, which needs a manager's PIN (ADR-0224),
//! or an administrator of the machine turning `kiosk` off. Home and Pro have nothing stronger to
//! give: someone who knows Windows can still leave through the sign-in screen, which is why
//! Enterprise's Shell Launcher or Assigned Access are documented as extra hardening, never required.
//!
//! The decisions are plain functions so they are tested on every platform; `station.rs` applies
//! them to the window and the tray.

/// How the till's window is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TillChrome {
    /// Full screen.
    pub(crate) full_screen: bool,
    /// Locked: frameless, always on top, and not closable or minimisable.
    pub(crate) locked: bool,
}

/// The chrome of a window showing the till. `asked` is what the caller asked for (the pairing QR
/// window asks for a window), `full_screen` the setting; a kiosk locks only the till itself.
pub(crate) fn till_chrome(
    is_till: bool,
    kiosk: bool,
    full_screen: bool,
    asked: Option<bool>,
) -> TillChrome {
    if is_till && kiosk {
        return TillChrome {
            full_screen: true,
            locked: true,
        };
    }
    TillChrome {
        full_screen: asked.unwrap_or(full_screen),
        locked: false,
    }
}

/// Whether a request to close the window labelled `label` is refused.
pub(crate) fn refuses_close(kiosk: bool, label: &str, till: &str) -> bool {
    kiosk && label == till
}

/// Whether the tray offers Quit.
pub(crate) fn offers_quit(kiosk: bool) -> bool {
    !kiosk
}

#[cfg(test)]
mod tests {
    use super::{TillChrome, offers_quit, refuses_close, till_chrome};

    #[test]
    fn a_kiosk_locks_the_till_full_screen_whatever_the_setting() {
        assert_eq!(
            till_chrome(true, true, false, None),
            TillChrome {
                full_screen: true,
                locked: true
            }
        );
    }

    #[test]
    fn a_kiosk_does_not_lock_another_window() {
        assert_eq!(
            till_chrome(false, true, true, Some(false)),
            TillChrome {
                full_screen: false,
                locked: false
            }
        );
    }

    #[test]
    fn without_kiosk_the_till_follows_the_setting() {
        assert_eq!(
            till_chrome(true, false, false, None),
            TillChrome {
                full_screen: false,
                locked: false
            }
        );
        assert_eq!(
            till_chrome(true, false, true, None),
            TillChrome {
                full_screen: true,
                locked: false
            }
        );
    }

    #[test]
    fn a_kiosk_refuses_to_close_the_till_and_offers_no_quit() {
        assert!(refuses_close(true, "till", "till"));
        assert!(!refuses_close(true, "status", "till"));
        assert!(!refuses_close(false, "till", "till"));
        assert!(!offers_quit(true));
        assert!(offers_quit(false));
    }
}
