// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A Vietnamese e-invoice: its symbol, its number, its kind and its checks
//! ([ADR-0225](../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md)).
//!
//! Values and pure functions only. The edge takes the next number in the sale's transaction, and the
//! cloud submits it; both ask this module what a number looks like and whether an invoice adds up.
//! Every rule here is **to be confirmed by legal or the accountant** (`CONFORMANCE.md` R9, R11, R15
//! to R20): the module states the shape this framework issues, not the law.
//!
//! # A number is a symbol and a sequence
//!
//! The symbol is a form digit (ký hiệu mẫu số) followed by six characters (ký hiệu hóa đơn): `C` or
//! `K`, the year's last two digits, a letter for the kind of invoice, and two characters the seller
//! chooses. `1C26MAA` is form 1, 2026, letter `M`, seller characters `AA`. The sequence starts at 1
//! for each symbol and has at most eight digits. Because the year is inside the symbol, a new year is
//! a new symbol and the sequence restarts.
//!
//! Each issuing point (a store's lease generation) gets its own seller characters, so two boxes never
//! share a sequence and a replacement box never repeats a number (ADR-0225 decision 2).

use core::fmt;

use pos_ports::fiscalization::{InvoiceBuyer, InvoiceNumber};
use pos_proto::CalendarDate;

use crate::is_valid_mst;

/// The largest sequence a symbol may reach: eight digits.
pub const MAX_SEQUENCE: u32 = 99_999_999;

/// How many issuing points one symbol prefix can name: two letters, `AA` to `ZZ`.
pub const SELLER_CODES: u32 = 26 * 26;

/// Why a symbol was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolError {
    /// Not seven characters.
    Length,
    /// The form digit is not 1 to 9.
    Form,
    /// The second character is neither `C` nor `K`.
    Code,
    /// The year is not two digits.
    Year,
    /// The kind letter or the seller characters are not upper-case ASCII letters.
    Letters,
}

/// A validated invoice symbol: form digit and six characters, such as `1C26MAA`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InvoiceSymbol(Box<str>);

impl InvoiceSymbol {
    /// Checks `text`'s shape: a digit 1–9, `C` or `K`, two digits, then three upper-case letters.
    ///
    /// Shape only. Which kind letter a restaurant's cash-register invoices use, and which form digit,
    /// are to be confirmed by legal; the cloud publishes the symbol and this refuses only what cannot
    /// be one.
    ///
    /// # Errors
    ///
    /// The first [`SymbolError`] the text breaks.
    pub fn parse(text: &str) -> Result<Self, SymbolError> {
        let bytes = text.as_bytes();
        let [form, code, year_tens, year_units, kind, seller_1, seller_2] = bytes else {
            return Err(SymbolError::Length);
        };
        if !(b'1'..=b'9').contains(form) {
            return Err(SymbolError::Form);
        }
        if !matches!(code, b'C' | b'K') {
            return Err(SymbolError::Code);
        }
        if !year_tens.is_ascii_digit() || !year_units.is_ascii_digit() {
            return Err(SymbolError::Year);
        }
        if ![kind, seller_1, seller_2]
            .iter()
            .all(|letter| letter.is_ascii_uppercase())
        {
            return Err(SymbolError::Letters);
        }
        Ok(Self(text.into()))
    }

    /// Builds the symbol for one issuing point: `form`, `code`, the year, `kind`, and the seller
    /// characters of issuing point `seller_index` (0 is `AA`, 1 is `AB`, 675 is `ZZ`).
    ///
    /// The cloud gives each store's lease generation its own index, so every box numbers in a
    /// sequence of its own.
    ///
    /// # Errors
    ///
    /// [`SymbolError`] if the parts do not make a symbol, including an index past `ZZ`, a year
    /// outside 2000–2099, or a form outside 1–9.
    pub fn for_issuing_point(
        form: u8,
        code: char,
        year: i16,
        kind: char,
        seller_index: u32,
    ) -> Result<Self, SymbolError> {
        if seller_index >= SELLER_CODES {
            return Err(SymbolError::Letters);
        }
        let year_in_century = year
            .checked_sub(2000)
            .filter(|short| (0..100).contains(short));
        let Some(short_year) = year_in_century else {
            return Err(SymbolError::Year);
        };
        let letter = |offset: u32| char::from_u32(u32::from(b'A') + offset).unwrap_or('?');
        let text = format!(
            "{form}{code}{short_year:02}{kind}{}{}",
            letter(seller_index / 26),
            letter(seller_index % 26)
        );
        Self::parse(&text)
    }

    /// The symbol as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The calendar year the symbol names: 2000 plus its two digits.
    #[must_use]
    pub fn year(&self) -> i16 {
        let digits = self.0.as_bytes();
        let digit = |index: usize| digits.get(index).map_or(0, |byte| i16::from(byte - b'0'));
        2000 + digit(2) * 10 + digit(3)
    }

    /// Whether an invoice dated `on` may carry this symbol: only in the symbol's own year.
    #[must_use]
    pub fn is_valid_on(&self, on: CalendarDate) -> bool {
        self.year() == on.year()
    }
}

impl fmt::Debug for InvoiceSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InvoiceSymbol({})", self.0)
    }
}

impl fmt::Display for InvoiceSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why the next number could not be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberingError {
    /// The symbol has reached [`MAX_SEQUENCE`]: the cloud must publish a new one.
    Exhausted,
    /// The sale's date is outside the symbol's year: the cloud must publish the new year's symbol.
    WrongYear,
}

/// The next number after `last` under `symbol`, for an invoice dated `on`.
///
/// Gapless: `last + 1`, starting at 1. The caller holds `last` in the same transaction as the sale,
/// as it holds the receipt counter, so a number is taken exactly when the sale commits.
///
/// # Errors
///
/// [`NumberingError::WrongYear`] if `on` is not in the symbol's year;
/// [`NumberingError::Exhausted`] past eight digits. Neither invents a number.
pub fn next_number(
    symbol: &InvoiceSymbol,
    last: Option<u32>,
    on: CalendarDate,
) -> Result<u32, NumberingError> {
    if !symbol.is_valid_on(on) {
        return Err(NumberingError::WrongYear);
    }
    let next = last.map_or(Some(1), |last| last.checked_add(1));
    next.filter(|next| *next <= MAX_SEQUENCE)
        .ok_or(NumberingError::Exhausted)
}

/// The invoice number as the port carries it: `1C26MAA/00000042`.
#[must_use]
pub fn invoice_number(symbol: &InvoiceSymbol, sequence: u32) -> InvoiceNumber {
    InvoiceNumber::new(format!("{symbol}/{sequence:08}"))
}

/// What an invoice is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvoiceKind {
    /// The first invoice for a sale.
    Original,
    /// Changes the amounts of an earlier invoice, up or down (hóa đơn điều chỉnh).
    Adjustment {
        /// The invoice it adjusts.
        corrects: InvoiceNumber,
    },
    /// Takes the place of an earlier invoice that was wrong (hóa đơn thay thế).
    Replacement {
        /// The invoice it replaces.
        corrects: InvoiceNumber,
    },
}

/// When a settled bill issues an invoice: the `einvoice.issue_moment` setting (ADR-0225 decision 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueMoment {
    /// None is issued. The register's default.
    Off,
    /// Every settled bill, with a buyer or without. What the Vietnamese pack recommends.
    EverySale,
    /// Only a bill whose guest gave buyer details.
    OnRequest,
}

impl IssueMoment {
    /// Whether a bill settled under this moment issues an invoice.
    #[must_use]
    pub const fn issues(self, has_buyer: bool) -> bool {
        match self {
            Self::Off => false,
            Self::EverySale => true,
            Self::OnRequest => has_buyer,
        }
    }
}

/// The recommended moment for a new Vietnamese store. An open owner decision, and to be confirmed
/// by legal against Decree 70/2025's cash-register invoices.
pub const RECOMMENDED_ISSUE_MOMENT: IssueMoment = IssueMoment::EverySale;

/// Why a buyer's details cannot go on an invoice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuyerError {
    /// The name is blank.
    NoName,
    /// The mã số thuế is not ten digits, or ten and a three-digit branch.
    TaxCode,
}

/// Checks a buyer for a Vietnamese invoice: a name, and a well-formed mã số thuế where one is given.
///
/// Shape only, so a cashier can take it with the line down. Whether the code is registered is the
/// cloud's to ask the provider after the sale (ADR-0225 decision 4). Names no field in its error, so
/// nothing personal reaches a log through it.
///
/// # Errors
///
/// The first [`BuyerError`] the buyer breaks.
pub fn check_buyer(buyer: &InvoiceBuyer) -> Result<(), BuyerError> {
    if buyer.buyer_name.trim().is_empty() {
        return Err(BuyerError::NoName);
    }
    match buyer.buyer_tax_code.as_deref().map(str::trim) {
        Some(code) if !code.is_empty() && !is_valid_mst(code) => Err(BuyerError::TaxCode),
        _ => Ok(()),
    }
}

/// One rate's figures on an invoice, in đồng.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateTotal {
    /// The rate, in basis points.
    pub rate_basis_points: u32,
    /// The base taxed at it.
    pub taxable_base_minor: i64,
    /// The tax at it.
    pub tax_minor: i64,
}

/// Why an invoice's figures do not add up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotalsError {
    /// Two entries name the same rate: an invoice states each rate once.
    RepeatedRate,
    /// The bases and taxes do not sum to the total.
    DoesNotSum,
    /// An original invoice with a negative figure. Only an adjustment may go down.
    Negative,
}

/// Checks that an invoice states each rate once and that its rates sum to `total_minor`.
///
/// `rounding_minor` is the bill's cash-rounding line, which is on the bill but not taxed; whether an
/// invoice shows it is to be confirmed (`CONFORMANCE.md` R5).
///
/// # Errors
///
/// The first [`TotalsError`] the figures break.
pub fn check_totals(
    kind: &InvoiceKind,
    rates: &[RateTotal],
    rounding_minor: i64,
    total_minor: i64,
) -> Result<(), TotalsError> {
    let mut seen: Vec<u32> = Vec::with_capacity(rates.len());
    let mut sum: i64 = rounding_minor;
    for rate in rates {
        if seen.contains(&rate.rate_basis_points) {
            return Err(TotalsError::RepeatedRate);
        }
        seen.push(rate.rate_basis_points);
        let negative = rate.taxable_base_minor < 0 || rate.tax_minor < 0;
        if negative && !matches!(kind, InvoiceKind::Adjustment { .. }) {
            return Err(TotalsError::Negative);
        }
        sum = sum
            .checked_add(rate.taxable_base_minor)
            .and_then(|sum| sum.checked_add(rate.tax_minor))
            .ok_or(TotalsError::DoesNotSum)?;
    }
    if sum == total_minor {
        Ok(())
    } else {
        Err(TotalsError::DoesNotSum)
    }
}
