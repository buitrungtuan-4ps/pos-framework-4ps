// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The words a receipt, a pre-bill, a shift report and a kitchen ticket are printed in.
//!
//! The till's screens translate through their catalogue. Paper had no catalogue at all: every label
//! was English, so a Vietnamese guest's receipt said "Subtotal" and "Tax 10.00%" in a shop whose
//! menu, cashier and law are Vietnamese. The store's language is already on the session
//! (`display_language`, from the `locale` node), and this is the table it picks from.
//!
//! # English whenever the box cannot draw the language
//!
//! An ESC/POS printer has no Vietnamese in its character set, and a line the box cannot rasterise
//! refuses the whole document (ADR-0102). A label is the one part of a receipt the store did not
//! write, so it must never be the part that stops one printing. When this box has no fonts, the
//! labels fall back to English and stay printable, whatever the language says.
//!
//! A language without a table here reads English too. Adding one is adding a table and a match arm.

/// Every fixed word a bill document or a shift report prints.
#[derive(Debug, PartialEq, Eq)]
pub struct PaperLabels {
    /// The heading of a check printed before payment.
    pub pre_bill: &'static str,
    /// The line under a pre-bill's total, so it is not taken for a receipt.
    pub not_a_receipt: &'static str,
    /// The heading under a copy's receipt number, so a copy is never taken for a second sale
    /// ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
    pub copy: &'static str,
    /// The word before a copy's number and the time it was printed.
    pub reprint: &'static str,
    /// The corporate buyer's line on a B2B invoice, before their name.
    pub bill_to: &'static str,
    /// The sum of the lines.
    pub subtotal: &'static str,
    /// Money taken off.
    pub discount: &'static str,
    /// Items given free.
    pub comps: &'static str,
    /// A service charge.
    pub service_charge: &'static str,
    /// A tax rate's line, before the rate.
    pub tax: &'static str,
    /// The cash rounding adjustment.
    pub rounding: &'static str,
    /// The heading of a closed shift's report.
    pub shift_report: &'static str,
    /// The float the drawer opened with.
    pub opening_float: &'static str,
    /// The cash taken during the shift.
    pub cash_taken: &'static str,
    /// Cash put into the drawer outside a sale (ADR-0165).
    pub paid_in: &'static str,
    /// Cash taken out of the drawer outside a sale.
    pub paid_out: &'static str,
    /// What the drawer should hold.
    pub expected_in_drawer: &'static str,
    /// What was counted.
    pub counted: &'static str,
    /// Counted less expected.
    pub variance: &'static str,
    /// The verdict when the drawer is short.
    pub short: &'static str,
    /// The verdict when it balances.
    pub balanced: &'static str,
    /// The verdict when it is over.
    pub over: &'static str,
    /// The heading over the store's totals on a slip for several drawers closed at once.
    pub store_totals: &'static str,
    /// What a kitchen ticket prints for a guest note the edge no longer holds — it restarted since
    /// the note was written ([ADR-0157](../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
    pub note_lost: &'static str,
}

/// English, and the fallback for every case the module documentation names.
pub static ENGLISH: PaperLabels = PaperLabels {
    pre_bill: "PRE-BILL",
    not_a_receipt: "Not a receipt",
    copy: "COPY",
    reprint: "Reprint",
    bill_to: "Bill to",
    subtotal: "Subtotal",
    discount: "Discount",
    comps: "Comps",
    service_charge: "Service charge",
    tax: "Tax",
    rounding: "Rounding",
    shift_report: "SHIFT REPORT",
    opening_float: "Opening float",
    cash_taken: "Cash taken",
    paid_in: "Paid in",
    paid_out: "Paid out",
    expected_in_drawer: "Expected in drawer",
    counted: "Counted",
    variance: "Variance",
    short: "Short",
    balanced: "Balanced",
    over: "Over",
    store_totals: "STORE TOTALS",
    note_lost: "A note was written - ask the server",
};

/// Vietnamese, in the words a Vietnamese receipt and cash-up sheet use.
pub static VIETNAMESE: PaperLabels = PaperLabels {
    pre_bill: "PHIẾU TẠM TÍNH",
    not_a_receipt: "Không phải hóa đơn thanh toán",
    copy: "BẢN SAO",
    reprint: "In lại lần",
    bill_to: "Người mua",
    subtotal: "Tạm tính",
    discount: "Giảm giá",
    comps: "Miễn phí",
    service_charge: "Phí dịch vụ",
    tax: "Thuế",
    rounding: "Làm tròn",
    shift_report: "BÁO CÁO CA",
    opening_float: "Tiền đầu ca",
    cash_taken: "Tiền mặt thu",
    paid_in: "Thu ngoài bán hàng",
    paid_out: "Chi ngoài bán hàng",
    expected_in_drawer: "Tiền trong két dự kiến",
    counted: "Đã đếm",
    variance: "Chênh lệch",
    short: "Thiếu",
    balanced: "Khớp",
    over: "Thừa",
    store_totals: "TỔNG CỬA HÀNG",
    note_lost: "Có ghi chú - hỏi nhân viên phục vụ",
};

impl PaperLabels {
    /// The labels for a store printing in `language` (a BCP 47 tag, `vi` or `vi-VN`), on a box that
    /// can or cannot rasterise text its printer has no characters for.
    #[must_use]
    pub fn for_store(language: Option<&str>, can_rasterise: bool) -> &'static Self {
        if !can_rasterise {
            return &ENGLISH;
        }
        language.and_then(Self::in_language).unwrap_or(&ENGLISH)
    }

    /// The table written in `language`, or `None` for a language the edge has no labels in.
    #[must_use]
    pub fn in_language(language: &str) -> Option<&'static Self> {
        let primary = language
            .split(['-', '_'])
            .next()
            .map(str::to_ascii_lowercase);
        match primary.as_deref() {
            Some("vi") => Some(&VIETNAMESE),
            Some("en") => Some(&ENGLISH),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ENGLISH, PaperLabels, VIETNAMESE};

    #[test]
    fn a_vietnamese_store_prints_vietnamese_only_where_the_box_can_draw_it() {
        assert_eq!(PaperLabels::for_store(Some("vi"), true), &VIETNAMESE);
        assert_eq!(PaperLabels::for_store(Some("vi-VN"), true), &VIETNAMESE);
        assert_eq!(PaperLabels::for_store(Some("VI_vn"), true), &VIETNAMESE);
        // No fonts: a label must never be the line that stops a receipt printing.
        assert_eq!(PaperLabels::for_store(Some("vi"), false), &ENGLISH);
    }

    #[test]
    fn a_language_with_no_table_or_no_language_reads_english() {
        assert_eq!(PaperLabels::for_store(Some("ja-JP"), true), &ENGLISH);
        assert_eq!(PaperLabels::for_store(None, true), &ENGLISH);
        assert_eq!(PaperLabels::in_language("ja-JP"), None);
        assert_eq!(PaperLabels::in_language("en-GB"), Some(&ENGLISH));
    }

    #[test]
    fn the_english_table_is_ascii_so_it_prints_on_any_printer() {
        for label in [
            ENGLISH.pre_bill,
            ENGLISH.not_a_receipt,
            ENGLISH.copy,
            ENGLISH.reprint,
            ENGLISH.bill_to,
            ENGLISH.subtotal,
            ENGLISH.discount,
            ENGLISH.comps,
            ENGLISH.service_charge,
            ENGLISH.tax,
            ENGLISH.rounding,
            ENGLISH.shift_report,
            ENGLISH.opening_float,
            ENGLISH.cash_taken,
            ENGLISH.paid_in,
            ENGLISH.paid_out,
            ENGLISH.expected_in_drawer,
            ENGLISH.counted,
            ENGLISH.variance,
            ENGLISH.short,
            ENGLISH.balanced,
            ENGLISH.over,
            ENGLISH.store_totals,
        ] {
            assert!(label.is_ascii(), "{label}");
        }
    }
}
