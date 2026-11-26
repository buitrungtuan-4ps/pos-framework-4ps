// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_proto::einvoice::EInvoiceKind;

use super::{Correction, utc_date, void_correction};

#[test]
fn a_utc_date_is_written_as_the_calendar_writes_it() {
    assert_eq!(utc_date(0), "1970-01-01");
    assert_eq!(utc_date(1_795_564_800_000), "2026-11-25");
    assert_eq!(utc_date(1_798_761_599_999), "2026-12-31");
    assert_eq!(utc_date(1_772_323_200_000), "2026-03-01");
}

#[test]
fn a_void_takes_the_standing_total_to_zero_and_never_below() {
    assert_eq!(
        void_correction(EInvoiceKind::Adjustment, 165_000),
        Some(Correction {
            kind: EInvoiceKind::Adjustment,
            total_minor: -165_000
        })
    );
    assert_eq!(
        void_correction(EInvoiceKind::Replacement, 165_000),
        Some(Correction {
            kind: EInvoiceKind::Replacement,
            total_minor: 0
        })
    );
    assert_eq!(
        void_correction(EInvoiceKind::Original, 10),
        Some(Correction {
            kind: EInvoiceKind::Adjustment,
            total_minor: -10
        })
    );
    for kind in [EInvoiceKind::Adjustment, EInvoiceKind::Replacement] {
        assert_eq!(void_correction(kind, 0), None, "already at zero");
        assert_eq!(void_correction(kind, -5), None);
    }
}
