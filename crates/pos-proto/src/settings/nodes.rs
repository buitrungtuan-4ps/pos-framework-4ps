// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The settings each published node carries, one function per node, which
//! [`register`](super::register) lists in turn.

use crate::backup::{self, PublishedBackup};
use crate::counter::{PublishedCounter, WalkInChannel};
use crate::locale::{LocaleSettings, TaxRounding};
use crate::people::PublishedPermissions;
use crate::printing::{self, PublishedPrinting, ReceiptLanguage, ReceiptSecondLanguage};
use crate::qr::{PublishedQr, TableOrder};
use crate::session::{self, PublishedSession};
use crate::shift::{self, CloseReport, DrawerDayEnd, DrawerModel, NoShiftSelling, PublishedShift};
use crate::tender_keys::{self, PublishedTenderKeys};
use crate::wire_enum::WireEnum;

use super::{
    EVERY_RELEASE, NEXT_RELEASE, STORE_WIDE, Setting, SettingShape, SettingUnit, TENANT_ONLY,
    choices,
};

/// The settings on the `shift` node: whether a till sells while no shift is open, the opening float, and
/// whether the close count is blind, in the register's order.
pub(super) fn shift_settings() -> Vec<Setting> {
    vec![
        Setting {
            node: PublishedShift::NODE,
            field: "no_shift_selling",
            shape: SettingShape::Choice {
                values: choices::<NoShiftSelling>(),
                default: PublishedShift::default().no_shift_selling().as_wire(),
                // Confirmed by the owner on 2026-09-30: a new store refuses, an existing one keeps
                // selling.
                preset: Some(NoShiftSelling::Refuse.as_wire()),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "Whether a till may seat a table, start a counter order or take a payment \
                      while no shift is open. `NO_SHIFT_SELLING_REFUSE` refuses each of them with \
                      `OPEN_SHIFT_REQUIRED` until a shift opens.",
        },
        Setting {
            node: PublishedShift::NODE,
            field: "opening_float_minor",
            shape: SettingShape::Int {
                min: *shift::OPENING_FLOAT_MINOR.start(),
                max: *shift::OPENING_FLOAT_MINOR.end(),
                unit: SettingUnit::MinorUnits,
                default: PublishedShift::default().opening_float_minor(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The float the Shift screen fills in when a shift opens, in the store \
                      currency's minor unit: `500000` is 500,000 đồng at a store in Vietnam. The \
                      cashier can change it before opening the shift, and the shift opens with \
                      what they send. `0` fills in nothing, as before. A float that is not a whole \
                      number of the currency's main unit fills in nothing either, because the \
                      till's keypad types whole units.",
        },
        Setting {
            node: PublishedShift::NODE,
            field: "blind_close",
            shape: SettingShape::Bool {
                default: PublishedShift::default().blind_close(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "Whether the count at the close of a shift is blind. On, the till says \
                      nothing about what the drawer should hold until the shift closes, so the \
                      cashier counts first. Off, the Shift screen shows the cash the drawer \
                      should hold beside the count, and the variance still appears only at the \
                      close.",
        },
    ]
}

/// The settings on the `shift` node's cash drawers: how many the store keeps, what prints when one
/// closes, what its business day's end does to it, and how far it may be out before its close
/// asks why, in the register's order.
pub(super) fn drawer_settings() -> Vec<Setting> {
    vec![
        Setting {
            node: PublishedShift::NODE,
            field: "drawer_model",
            shape: SettingShape::Choice {
                values: choices::<DrawerModel>(),
                default: PublishedShift::default().drawer_model().as_wire(),
                // Chosen by the owner on 2026-10-01 (ADR-0167 decision 1): a new store keeps a
                // drawer for each till, and an existing one keeps its one drawer until it is set.
                preset: Some(DrawerModel::PerTerminal.as_wire()),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "How many cash drawers the store keeps. `DRAWER_MODEL_PER_STORE` keeps one, \
                      every till's, as before. `DRAWER_MODEL_PER_TERMINAL` keeps a drawer for each \
                      till, each started, counted and closed on its own: cash is taken only at a \
                      device bound to a till, and each till's drawer springs at the receipt \
                      printer its terminal names. A change waits until no drawer is open.",
        },
        Setting {
            node: PublishedShift::NODE,
            field: "close_report",
            shape: SettingShape::Choice {
                values: choices::<CloseReport>(),
                default: PublishedShift::default().close_report().as_wire(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "What prints when a drawer's shift closes. `CLOSE_REPORT_PER_DRAWER` prints \
                      each drawer's report where it is closed: at the store's receipt printer, or, \
                      where the store keeps a drawer per till, at the receipt printer of the till \
                      the close is made at. `CLOSE_REPORT_COMBINED` prints one slip for several \
                      counted drawers closed at once, the store's totals and then each drawer, \
                      and a drawer closed on its own still prints its own report. \
                      `CLOSE_REPORT_NONE` prints nothing, and the close still answers with the \
                      drawer's figures.",
        },
        Setting {
            node: PublishedShift::NODE,
            field: "drawer_day_end",
            shape: SettingShape::Choice {
                values: choices::<DrawerDayEnd>(),
                default: PublishedShift::default().drawer_day_end().as_wire(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "What happens to a drawer whose shift is still open once its business day \
                      has ended at the store's cutoff. `DRAWER_DAY_END_FLAG` refuses nothing, and \
                      the shift reads say the drawer's day has ended. \
                      `DRAWER_DAY_END_REQUIRE_CLOSE` also refuses cash, a paid in and a paid out \
                      into it with `DRAWER_DAY_ENDED` until it is counted and closed; a card, the \
                      count, the close and a no-sale opening still work.",
        },
        Setting {
            node: PublishedShift::NODE,
            field: "variance_reason_minor",
            shape: SettingShape::Int {
                min: *shift::VARIANCE_REASON_MINOR.start(),
                max: *shift::VARIANCE_REASON_MINOR.end(),
                unit: SettingUnit::MinorUnits,
                default: PublishedShift::default().variance_reason_minor(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "How far over or short a drawer may close, either way, before its close asks \
                      a reason, in the store currency's minor unit: `50000` is 50,000 đồng at a \
                      store in Vietnam. The till asks only after the count, so a blind close stays \
                      blind, and `cash.shift.closed` records the reason \
                      (`REASON_ACTION_CASH_VARIANCE`). `0` asks no reason, as before, and a store \
                      whose reason list offers none for a variance still closes.",
        },
    ]
}

/// The settings on the `session` node: when a till locks, and the staff PIN's rules, in the register's order.
pub(super) fn session_settings() -> Vec<Setting> {
    // What the edge reads a `session` node that sets nothing as.
    let unset = PublishedSession::default();
    vec![
        Setting {
            node: PublishedSession::NODE,
            field: "idle_lock_seconds",
            shape: SettingShape::Int {
                min: *session::IDLE_LOCK_SECONDS.start(),
                max: *session::IDLE_LOCK_SECONDS.end(),
                unit: SettingUnit::Seconds,
                default: i64::from(unset.idle_lock_seconds()),
                // Confirmed by the owner on 2026-10-01: a new store's till locks after two minutes
                // without a touch. An existing store keeps a till that never locks.
                preset: Some(120),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "A security setting: how long a till may sit with no touch, key, click or \
                      scroll before it signs its person out and locks, until their PIN opens it \
                      again on the screen they left. `0` never locks. The kitchen board, the pass \
                      and the screens before sign-in never lock.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "sign_in_idle_timeout_minutes",
            shape: SettingShape::Int {
                min: *session::SIGN_IN_IDLE_TIMEOUT_MINUTES.start(),
                max: *session::SIGN_IN_IDLE_TIMEOUT_MINUTES.end(),
                unit: SettingUnit::Minutes,
                default: i64::from(session::DEFAULT_SIGN_IN_IDLE_TIMEOUT_MINUTES),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "A security setting: how long a signed-in device may go unused before its \
                      sign-in lapses and it asks for a PIN again, which bounds how long a till \
                      carried off while signed in keeps trading. A store that does not set it runs \
                      `sign_in_idle_timeout_minutes` from its local file if that sets one, which \
                      is deprecated.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "lockout_attempts",
            shape: SettingShape::Int {
                min: *session::LOCKOUT_ATTEMPTS.start(),
                max: *session::LOCKOUT_ATTEMPTS.end(),
                unit: SettingUnit::Count,
                default: i64::from(unset.lockout_attempts()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "A security setting: how many wrong PINs in a row lock a person out, \
                      counted across sign-in and a manager's approval. No value switches the \
                      lockout off.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "lockout_minutes",
            shape: SettingShape::Int {
                min: *session::LOCKOUT_MINUTES.start(),
                max: *session::LOCKOUT_MINUTES.end(),
                unit: SettingUnit::Minutes,
                default: i64::from(unset.lockout_minutes()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "A security setting: how long a person stays locked out after too many wrong \
                      PINs. A lockout already running keeps the end it was given. No value \
                      switches the lockout off.",
        },
        Setting {
            node: PublishedSession::NODE,
            field: "pin_min_length",
            shape: SettingShape::Int {
                min: *session::PIN_MIN_LENGTH.start(),
                max: *session::PIN_MIN_LENGTH.end(),
                unit: SettingUnit::Count,
                default: i64::from(unset.pin_min_length()),
                preset: None,
            },
            // A PIN belongs to a person, who may work at every store of the tenant.
            scopes: TENANT_ONLY,
            since: EVERY_RELEASE,
            confirm: None,
            summary: "A security setting: the fewest digits a staff PIN may have, from 4 to 8. It \
                      applies to PINs set or reset from now on, and PINs already set keep working. \
                      It is set for the whole tenant, because a person may work at every store, and \
                      the cloud applies it whatever release a store runs. It is a policy choice \
                      rather than a defence, which is the PIN's hashing and the lockout.",
        },
    ]
}

/// The settings on the `permissions` node, in the register's order.
pub(super) fn permission_settings() -> Vec<Setting> {
    vec![Setting {
        node: PublishedPermissions::NODE,
        field: "enforced",
        shape: SettingShape::Bool {
            default: PublishedPermissions::default().enforced,
            // ADR-0158 Rollout, confirmed by the owner on 2026-10-01: on for a store created
            // after this lands, off for the stores that exist until someone turns it on.
            preset: Some(true),
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "A security setting: whether the till decides every act with the signed-in \
                      person's own permissions. What they hold directly they do alone, what they \
                      hold with approval needs the PIN of someone who holds it directly, and \
                      anything else is refused. Off, every act works as before. Set each role up \
                      before turning it on.",
    }]
}

/// The settings on the `printing` node, in the register's order: what a receipt prints, then how
/// large the server draws a line.
pub(super) fn printing_settings() -> Vec<Setting> {
    let mut settings = receipt_settings();
    settings.push(font_size_setting());
    settings
}

/// The `printing` settings about a receipt: its languages, whether it prints on settle, and its tax
/// components.
fn receipt_settings() -> Vec<Setting> {
    vec![
        Setting {
            node: PublishedPrinting::NODE,
            field: "receipt_language",
            shape: SettingShape::Choice {
                values: choices::<ReceiptLanguage>(),
                default: PublishedPrinting::default().receipt_language().as_wire(),
                // Confirmed by the owner on 2026-10-01: a new store prints in its country's language.
                // A token rather than a language, because the console gives a store these values
                // when it creates it, before anyone has said which country the store is in.
                preset: Some(ReceiptLanguage::Country.as_wire()),
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The language a receipt, its copy and a pre-bill print their labels and item \
                      names in. `RECEIPT_LANGUAGE_DISPLAY` follows the store's display language. \
                      `RECEIPT_LANGUAGE_COUNTRY` is the language of the store's country, or the \
                      display language while the store's locale names none the edge prints labels \
                      in. An item the menu does not translate keeps its own name, and a box with \
                      no fonts prints English labels.",
        },
        Setting {
            node: PublishedPrinting::NODE,
            field: "receipt_second_language",
            shape: SettingShape::Choice {
                values: choices::<ReceiptSecondLanguage>(),
                default: PublishedPrinting::default()
                    .receipt_second_language()
                    .as_wire(),
                // The owner, 2026-10-01: a bilingual receipt is a choice each store makes, so a new
                // store is given none.
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "A second language a receipt, its copy and a pre-bill print in, after the \
                      first. Each label prints in both, as `Tạm tính / Subtotal`, or with the \
                      second under the first where the two do not fit on one line. An item, a \
                      modifier or a fee prints its name in the second language under its own \
                      where the menu or the fee's rule translates it. `RECEIPT_SECOND_LANGUAGE_NONE` \
                      prints one language, and so does a second language the receipt already \
                      prints in, or one a box with no fonts cannot print.",
        },
        Setting {
            node: PublishedPrinting::NODE,
            field: "receipt_printed_on_settle",
            shape: SettingShape::Bool {
                default: PublishedPrinting::default().receipt_printed_on_settle(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "Whether settling a bill prints the guest's receipt. Off, a settle prints \
                      nothing, and the till's print button prints a copy.",
        },
        Setting {
            node: PublishedPrinting::NODE,
            field: "receipt_tax_components",
            shape: SettingShape::Bool {
                default: PublishedPrinting::default().receipt_tax_components(),
                // The owner, 2026-10-07: every receipt prints them, so a new store is given what
                // the default already prints.
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "Whether a receipt, its copy and a pre-bill print each tax line's named \
                      components under its rate, such as CGST and SGST under India's GST, \
                      wherever the store's `tax` node names them. A copy prints the components \
                      its settle recorded. Off, each prints the tax line alone, and the settle \
                      still records the components. A store whose `tax` node names none prints \
                      the same either way.",
        },
    ]
}

/// The `printing` setting for the size the server draws a line the printer cannot.
fn font_size_setting() -> Setting {
    Setting {
        node: PublishedPrinting::NODE,
        field: "font_size_dots",
        shape: SettingShape::Int {
            min: *printing::FONT_SIZE_DOTS.start(),
            max: *printing::FONT_SIZE_DOTS.end(),
            unit: SettingUnit::Count,
            default: i64::from(printing::DEFAULT_FONT_SIZE_DOTS),
            // No store is given another value: a new store prints at 24, as every store did
            // before the setting.
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "How large a line the printer cannot draw in its own characters is printed, \
                      in printer dots per em, from 16 to 48: 24 is a comfortable receipt body at \
                      the 203 dpi most thermal printers run at. The store server draws only such \
                      lines, Vietnamese ones among them; a line the printer's own character set \
                      covers prints in the printer's font, which this does not change. Double-size \
                      text is drawn at twice it. A change applies from the next print. A box whose \
                      config.toml sets font_size_dots keeps it until this is set, and this then \
                      wins.",
    }
}

/// The settings on the `qr` node, in the register's order.
pub(super) fn qr_settings() -> Vec<Setting> {
    vec![Setting {
        node: PublishedQr::NODE,
        field: "table_order",
        shape: SettingShape::Choice {
            values: choices::<TableOrder>(),
            default: PublishedQr::default().table_order().as_wire(),
            // Confirmed by the owner on 2026-10-01 (ADR-0160 item 2): a new store's guest orders
            // join the table's order. An existing store keeps a separate order until someone
            // sets it.
            preset: Some(TableOrder::Join.as_wire()),
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "What a guest's QR order does at a table that already has an open order. \
                      `TABLE_ORDER_JOIN` adds its lines to that order, so the table has one order \
                      and one bill; where staff confirm a guest's order, its lines join once they \
                      do. A line that joins keeps the price the guest was shown, and the table's \
                      order sets its tax and fees. `TABLE_ORDER_SEPARATE` gives the guest's order \
                      its own order, as before. A table whose bill is already open or paid takes a \
                      guest's order as its own order either way.",
    }]
}

/// The settings on the `locale` node, in the register's order.
pub(super) fn locale_settings() -> Vec<Setting> {
    vec![Setting {
        node: LocaleSettings::NODE,
        field: "tax_rounding",
        shape: SettingShape::Choice {
            values: choices::<TaxRounding>(),
            default: LocaleSettings::default().tax_rounding().as_wire(),
            // No country is given a different value (ADR-0160 decision 2), so no store changes
            // when it takes the release that brings the setting.
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "How each tax amount and each fee rounds to the currency's minor unit: the \
                      tax of each tax class, whether prices include their tax or not, the tax on a \
                      fee, and the fee itself. `TAX_ROUNDING_HALF_UP` rounds half a unit or more \
                      up, as before. `TAX_ROUNDING_DOWN` drops the fraction. It does not round the \
                      total to the country's coins, which stays half-up, or a line's price. A bill \
                      is computed with the mode in force when it is computed, so change it outside \
                      trading hours.",
    }]
}

/// The settings on the `tender_keys` node, in the register's order.
pub(super) fn tender_key_settings() -> Vec<Setting> {
    vec![
        Setting {
            node: PublishedTenderKeys::NODE,
            field: "first_tip_percent",
            shape: SettingShape::Int {
                min: *tender_keys::TIP_PERCENT.start(),
                max: *tender_keys::TIP_PERCENT.end(),
                unit: SettingUnit::Percent,
                default: i64::from(PublishedTenderKeys::default().first_tip_percent()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The first tip key the pay screen offers, as a whole percentage of the bill: \
                      `10` is ten percent. `0` hides the key. The pay screen offers its keys in \
                      order, after the key for no tip, wherever the store takes tips.",
        },
        Setting {
            node: PublishedTenderKeys::NODE,
            field: "second_tip_percent",
            shape: SettingShape::Int {
                min: *tender_keys::TIP_PERCENT.start(),
                max: *tender_keys::TIP_PERCENT.end(),
                unit: SettingUnit::Percent,
                default: i64::from(PublishedTenderKeys::default().second_tip_percent()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The second tip key the pay screen offers, as a whole percentage of the \
                      bill. `0` hides the key, and so does a percentage the first key offers.",
        },
        Setting {
            node: PublishedTenderKeys::NODE,
            field: "third_tip_percent",
            shape: SettingShape::Int {
                min: *tender_keys::TIP_PERCENT.start(),
                max: *tender_keys::TIP_PERCENT.end(),
                unit: SettingUnit::Percent,
                default: i64::from(PublishedTenderKeys::default().third_tip_percent()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The third tip key the pay screen offers, as a whole percentage of the bill. \
                      `0` hides the key, and so does a percentage an earlier key offers. With no key \
                      left to offer, the pay screen shows no tip row.",
        },
        Setting {
            node: PublishedTenderKeys::NODE,
            field: "split_ways_max",
            shape: SettingShape::Int {
                min: *tender_keys::SPLIT_WAYS_MAX.start(),
                max: *tender_keys::SPLIT_WAYS_MAX.end(),
                unit: SettingUnit::Count,
                default: i64::from(PublishedTenderKeys::default().split_ways_max()),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The most guests the pay screen offers to split a bill between evenly: it \
                      offers every number from two up to this one. `6`, as before, fits one row; \
                      above it the row scrolls sideways on a narrow screen rather than wrapping, so \
                      the tenders below it stay where they are.",
        },
    ]
}

/// The settings on the `counter` node, in the register's order.
pub(super) fn counter_settings() -> Vec<Setting> {
    vec![Setting {
        node: PublishedCounter::NODE,
        field: "walk_in_channel",
        shape: SettingShape::Choice {
            values: choices::<WalkInChannel>(),
            default: PublishedCounter::default().walk_in_channel().as_wire(),
            // No store is given another value (ADR-0160 decision 2), so a counter opens its
            // walk-ins for takeaway, as before, until someone sets it.
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "The channel an order the counter starts for a walk-in guest takes, which \
                      decides the price book it is sold from and its tax. \
                      `WALK_IN_CHANNEL_TAKEAWAY` takes every walk-in away, as before. \
                      `WALK_IN_CHANNEL_DINE_IN` has every walk-in eaten in. `WALK_IN_CHANNEL_ASK` \
                      has the cashier choose Eat in or Take away for each walk-in. A store that \
                      publishes the channels it accepts must accept the one a walk-in takes, or \
                      the order is refused.",
    }]
}

/// The settings on the `backup` node, in the register's order.
pub(super) fn backup_settings() -> Vec<Setting> {
    vec![Setting {
        node: PublishedBackup::NODE,
        field: "interval_hours",
        shape: SettingShape::Int {
            min: *backup::INTERVAL_HOURS.start(),
            max: *backup::INTERVAL_HOURS.end(),
            unit: SettingUnit::Hours,
            default: i64::from(backup::DEFAULT_INTERVAL_HOURS),
            // No store is given another value: a new store archives once a day, as every store
            // did before the setting (ADR-0124).
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "How many hours apart a store ships a sealed archive of its database off the \
                      box, from 1 to 168: the most trading it can lose if its disk dies. A change \
                      applies from the next archive. No value switches archiving off. Only \
                      `backup_interval_hours = 0` in a box's own config.toml does, and it wins. A \
                      box whose config.toml sets another number keeps it until this is set, and \
                      this then wins.",
    }]
}
