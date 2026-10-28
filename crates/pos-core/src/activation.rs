// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The once-only activation exchange: a short code in, a device credential out
//! ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
//!
//! A new or replacement machine holds no credential on first boot. It is activated once: an operator
//! types a short **activation code**, the cloud exchanges it for the machine's long-lived credential,
//! and the edge stores that credential in the operating system's protected store (the `KeyVault` port,
//! under `SecretName::DeviceCredential`). The code is then useless — which is what makes swapping a
//! machine a five-minute job rather than a credential-distribution exercise
//! ([ADR-0003](../../../docs/adr/0003-cattle-not-pets.md), `docs/architecture.md` §4).
//!
//! This module is the pure half of that exchange, so the simulator (`docs/roadmap.md` P12) can exhaust
//! a first activation, a replay of a spent code, and a revoked code with no network:
//!
//!  * [`ActivationCode`] is the code's **format** — a human-typed string with a trailing checksum, so a
//!    typo is caught here rather than after a round-trip. The checksum is a typo guard, **not** a
//!    security check: the entropy of the code and the single-use redemption below are the security, and
//!    the cloud is the only authority on whether a code is live. A code may carry the id of the cell
//!    that issued it as a prefix, `VN1-XXXX-XXXX-XXXX`
//!    ([ADR-0181](../../../docs/adr/0181-the-activation-code-names-its-cell.md)): a routing hint that
//!    says which cell to present the code to, never part of the credential.
//!  * [`redeem`] is the **single-use rule** — deny by default, granting only a code the cloud still
//!    records as [`CodeStatus::Issued`]. The authoritative look-up-and-consume (flipping `Issued` to
//!    [`CodeStatus::Redeemed`] in the same transaction that mints the credential) is cloud I/O, exactly
//!    as an event append is ([ADR-0013](../../../docs/adr/0013-async-strategy.md)); the domain owns the
//!    rule, not the transaction.
//!  * [`device_activation`] is the edge-side statement that a box holding its device credential is
//!    activated, and one without it must run the exchange.
//!
//! On a successful grant the edge emits `device.activation.completed` (`pos_proto::events`); a machine
//! mid-activation sends its `Hello` frame with no lease token yet (`pos_proto::protocol`).

use core::fmt;

/// The symbols an activation code is written in: Crockford's base-32 alphabet, which omits the four
/// glyphs a human confuses — `I`, `L`, `O`, `U` — so a printed setup sheet reads unambiguously.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The alphabet size, as the modulus the checksum reduces by.
const ALPHABET_SIZE: u32 = 32;

/// The number of symbols in a code, the trailing checksum symbol included. Eleven payload symbols
/// carry 55 bits of entropy and the twelfth is the checksum; the code is displayed as three
/// hyphenated groups of four, `XXXX-XXXX-XXXX`.
const CODE_LENGTH: usize = 12;

/// The characters a code's reader skips between symbols.
const SEPARATORS: [char; 5] = ['-', ' ', '\t', '\n', '\r'];

/// The shortest cell id a prefix may be: three, so a cell id is never a two-letter country code
/// (ADR-0179). The same bounds `pos_cloud` checks a `[cell] cell_id` against.
const MIN_CELL_ID_LEN: usize = 3;

/// The longest cell id a prefix may be.
const MAX_CELL_ID_LEN: usize = 32;

/// The number of payload symbols — the code length without its one checksum symbol, and the number
/// of entropy bytes [`ActivationCode::from_entropy`] consumes.
pub const PAYLOAD_LEN: usize = CODE_LENGTH - 1;

/// A validated activation code.
///
/// Constructed only through [`ActivationCode::parse`], which normalises human input and verifies the
/// checksum, so an `ActivationCode` value is always well-formed. It is a bearer credential until it is
/// redeemed, so its [`Debug`] is redacted; [`ActivationCode::as_str`] and
/// [`ActivationCode::to_setup_sheet_text`] are the accessors that yield the code itself, named to be
/// conspicuous in a diff.
///
/// A code may carry a **cell prefix**, the id of the cell that issued it
/// ([ADR-0181](../../../docs/adr/0181-the-activation-code-names-its-cell.md)). The prefix is a routing
/// hint and never part of the credential: [`ActivationCode::as_str`] is the twelve symbols alone, which
/// is what a cloud hashes and redeems, so a prefix can neither make a spent code live nor change which
/// record it names.
#[derive(Clone, PartialEq, Eq)]
pub struct ActivationCode {
    /// The canonical `XXXX-XXXX-XXXX` text.
    canonical: String,
    /// The cell id the code names, lower-case, when it carries one.
    cell_id: Option<String>,
}

impl ActivationCode {
    /// Parses and validates an activation code from human input, with or without a cell prefix.
    ///
    /// Input is normalised the way Crockford's alphabet is meant to be read: case is ignored, hyphens
    /// and ASCII whitespace are discarded, and the ambiguous glyphs are folded (`I` and `L` become
    /// `1`, `O` becomes `0`). An input that reads as exactly twelve symbols is a code with no prefix,
    /// exactly as before prefixes existed. Otherwise the code is read from the **end**: the last
    /// twelve symbols, separators skipped, are the code, and what comes before them, less one
    /// separating hyphen, is a cell id, read case-insensitively and never folded. Reading from the
    /// end is what lets a cell id hold hyphens (`jp-east-2`) and letters outside the code's alphabet
    /// (`u`). Either way the trailing checksum must agree, so a mistyped code is rejected here rather
    /// than after a network round-trip.
    ///
    /// # Errors
    ///
    /// [`CodeError::Empty`] if nothing but separators was given, [`CodeError::WrongLength`] if there
    /// are fewer than twelve symbols, or more with no cell prefix before them,
    /// [`CodeError::BadSymbol`] for a character outside the alphabet among the code's twelve,
    /// [`CodeError::BadChecksum`] if the checksum symbol does not match the payload,
    /// [`CodeError::PrefixNotSeparated`] if something other than code symbols comes before the code
    /// with no hyphen between, and [`CodeError::BadCellPrefix`] if what comes before the hyphen is
    /// not a cell id.
    pub fn parse(input: &str) -> Result<Self, CodeError> {
        let (head, values) = read_code_from_end(input)?;
        let (check, payload) = values.split_last().ok_or(CodeError::WrongLength)?;
        let cell_id = if head.chars().all(is_separator) {
            None
        } else {
            Some(cell_prefix(head)?)
        };
        if checksum(payload) != *check {
            return Err(CodeError::BadChecksum);
        }
        Ok(Self {
            canonical: canonical_form(&values),
            cell_id,
        })
    }

    /// The canonical `XXXX-XXXX-XXXX` text of the code: its twelve symbols, without any cell prefix.
    ///
    /// The one accessor that yields the credential itself, named to be conspicuous: the code is a
    /// bearer credential until it is redeemed. It is what a cloud hashes, stores and redeems.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.canonical.as_str()
    }

    /// The id of the cell the code names, lower-case (`vn1`), when it carries a prefix.
    #[must_use]
    pub fn cell_id(&self) -> Option<&str> {
        self.cell_id.as_deref()
    }

    /// The code naming `cell_id` as its cell, for a cell that issues codes with its prefix.
    ///
    /// # Errors
    ///
    /// [`CodeError::BadCellPrefix`] if `cell_id` is not a cell id: 3 to 32 letters, digits or `-`,
    /// starting with a letter and not ending with `-`.
    pub fn with_cell_id(self, cell_id: &str) -> Result<Self, CodeError> {
        Ok(Self {
            cell_id: Some(checked_cell_id(cell_id)?),
            ..self
        })
    }

    /// The text printed on a setup sheet: `VN1-XXXX-XXXX-XXXX` with the cell prefix upper-case, or
    /// `XXXX-XXXX-XXXX` for a code with none. It parses back to this code.
    ///
    /// Like [`Self::as_str`], it yields the code itself.
    #[must_use]
    pub fn to_setup_sheet_text(&self) -> String {
        match &self.cell_id {
            Some(cell_id) => format!("{}-{}", cell_id.to_ascii_uppercase(), self.canonical),
            None => self.canonical.clone(),
        }
    }

    /// Mints a fresh code from `entropy`, one byte per payload symbol.
    ///
    /// Each byte picks a symbol by its remainder modulo the alphabet size; the choice is unbiased
    /// because 256 is a whole multiple of 32. The checksum symbol is then appended, so the result
    /// always [`ActivationCode::parse`]s. `pos-core` reads no randomness of its own — the caller
    /// supplies the entropy (the cloud passes bytes from its CSPRNG), which keeps code generation at
    /// the I/O edge. The code carries no cell prefix; [`Self::with_cell_id`] adds one.
    #[must_use]
    pub fn from_entropy(entropy: [u8; PAYLOAD_LEN]) -> Self {
        let mut values: Vec<u8> = entropy
            .iter()
            .map(|&byte| u8::try_from(usize::from(byte) % ALPHABET.len()).unwrap_or(0))
            .collect();
        let check = checksum(&values);
        values.push(check);
        Self {
            canonical: canonical_form(&values),
            cell_id: None,
        }
    }
}

/// Whether the code's reader skips `raw`.
fn is_separator(raw: char) -> bool {
    SEPARATORS.contains(&raw)
}

/// The alphabet value of one typed character, after case and the ambiguous glyphs are folded, or
/// `None` for a character outside the alphabet.
fn symbol_value(raw: char) -> Option<u8> {
    let folded = match raw {
        'I' | 'i' | 'L' | 'l' => '1',
        'O' | 'o' => '0',
        other => other.to_ascii_uppercase(),
    };
    let byte = u8::try_from(u32::from(folded)).ok()?;
    let value = ALPHABET.iter().position(|&symbol| symbol == byte)?;
    u8::try_from(value).ok()
}

/// Reads the code's twelve symbols from the end of `input`, skipping separators, and returns what
/// comes before them with their values in order.
fn read_code_from_end(input: &str) -> Result<(&str, Vec<u8>), CodeError> {
    let mut values: Vec<u8> = Vec::with_capacity(CODE_LENGTH);
    let mut start = input.len();
    for (index, raw) in input.char_indices().rev() {
        if values.len() == CODE_LENGTH {
            break;
        }
        if is_separator(raw) {
            start = index;
            continue;
        }
        values.push(symbol_value(raw).ok_or(CodeError::BadSymbol)?);
        start = index;
    }
    if values.is_empty() {
        return Err(CodeError::Empty);
    }
    if values.len() != CODE_LENGTH {
        return Err(CodeError::WrongLength);
    }
    values.reverse();
    let head = input.get(..start).ok_or(CodeError::WrongLength)?;
    Ok((head, values))
}

/// The cell id in what comes before a code's twelve symbols.
///
/// It must end with one separating hyphen, and what precedes that must be a cell id. A head made only
/// of code symbols with no hyphen after it is a code with too many symbols, not a prefix, and is
/// refused as such.
fn cell_prefix(head: &str) -> Result<String, CodeError> {
    let trimmed = head.trim_end_matches(|raw: char| raw != '-' && is_separator(raw));
    let Some(prefix) = trimmed.strip_suffix('-') else {
        let only_symbols = head
            .chars()
            .all(|raw| is_separator(raw) || symbol_value(raw).is_some());
        return Err(if only_symbols {
            CodeError::WrongLength
        } else {
            CodeError::PrefixNotSeparated
        });
    };
    checked_cell_id(prefix.trim_matches(|raw: char| raw != '-' && is_separator(raw)))
}

/// A cell id as ADR-0179 C1 defines it, lower-cased: 3 to 32 of `a-z`, `0-9` and `-`, starting with a
/// letter and not ending with `-`. Never folded.
fn checked_cell_id(raw: &str) -> Result<String, CodeError> {
    let cell_id = raw.to_ascii_lowercase();
    let well_formed = (MIN_CELL_ID_LEN..=MAX_CELL_ID_LEN).contains(&cell_id.len())
        && cell_id.starts_with(|c: char| c.is_ascii_lowercase())
        && !cell_id.ends_with('-')
        && cell_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if well_formed {
        Ok(cell_id)
    } else {
        Err(CodeError::BadCellPrefix)
    }
}

impl fmt::Debug for ActivationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ActivationCode(redacted)")
    }
}

/// The checksum symbol value for a payload: a position-weighted sum reduced modulo the alphabet size,
/// which catches the ordinary typo — a wrong symbol, or an adjacent pair swapped — without pretending
/// to be a cryptographic authenticator.
fn checksum(payload: &[u8]) -> u8 {
    let mut sum: u32 = 0;
    for (index, &value) in payload.iter().enumerate() {
        let weight = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
        sum = sum.saturating_add(weight.saturating_mul(u32::from(value)));
    }
    // `sum % ALPHABET_SIZE` is in `0..32`, so this conversion never fails.
    u8::try_from(sum % ALPHABET_SIZE).unwrap_or(0)
}

/// Renders validated symbol values as the canonical `XXXX-XXXX-XXXX` text.
fn canonical_form(values: &[u8]) -> String {
    let mut text = String::with_capacity(CODE_LENGTH + 2);
    for (index, &value) in values.iter().enumerate() {
        if index == 4 || index == 8 {
            text.push('-');
        }
        // Every value came from an alphabet position, so this lookup is always `Some`.
        if let Some(&symbol) = ALPHABET.get(usize::from(value)) {
            text.push(char::from(symbol));
        }
    }
    text
}

/// Why an activation code failed to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodeError {
    /// The input held no symbols — only separators, or nothing at all.
    Empty,
    /// The symbol count was not twelve.
    WrongLength,
    /// A character was not in the code alphabet.
    BadSymbol,
    /// The checksum symbol did not match the payload — a likely typo.
    BadChecksum,
    /// What came before the code, less its separating hyphen, is not a cell id
    /// ([ADR-0181](../../../docs/adr/0181-the-activation-code-names-its-cell.md)).
    BadCellPrefix,
    /// Something other than code symbols came before the code with no hyphen to separate it.
    PrefixNotSeparated,
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("activation code is empty"),
            Self::WrongLength => write!(f, "activation code must be {CODE_LENGTH} symbols"),
            Self::BadSymbol => {
                f.write_str("activation code contains a character outside the alphabet")
            }
            Self::BadChecksum => {
                f.write_str("activation code checksum does not match — likely a typo")
            }
            Self::BadCellPrefix => f.write_str(
                "activation code's cell prefix is not a cell id: 3 to 32 letters, digits or '-', \
                 starting with a letter",
            ),
            Self::PrefixNotSeparated => {
                f.write_str("activation code's cell prefix must be followed by a hyphen")
            }
        }
    }
}

impl core::error::Error for CodeError {}

/// The lifecycle state the cloud records for an issued activation code. The look-up that produces it
/// is cloud I/O; the domain only reasons over the resulting state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeStatus {
    /// Issued and never redeemed — the one state that grants.
    Issued,
    /// Already exchanged for a credential. Activation is single-use, so this is refused.
    Redeemed,
    /// Cancelled by an administrator before it was used — a printed setup sheet that leaked.
    Revoked,
}

/// Why a redemption was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The code was already redeemed. Activation is single-use.
    AlreadyRedeemed,
    /// The code was revoked by an administrator before use.
    Revoked,
}

/// The verdict for one redemption attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redemption {
    /// Mint the device credential and mark the code [`CodeStatus::Redeemed`] in the same transaction.
    Grant,
    /// Refuse, with the reason.
    Reject(RejectReason),
}

/// Decides whether an activation code may be redeemed, from the state the cloud records for it
/// ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
///
/// Single-use and deny by default: only a code still [`CodeStatus::Issued`] is granted, and a grant
/// obliges the caller to flip it to [`CodeStatus::Redeemed`] in the transaction that mints the
/// credential — a second attempt then refuses. The atomicity of that flip is the cloud's transaction,
/// not the domain's ([ADR-0013](../../../docs/adr/0013-async-strategy.md)).
#[must_use]
pub const fn redeem(status: CodeStatus) -> Redemption {
    match status {
        CodeStatus::Issued => Redemption::Grant,
        CodeStatus::Redeemed => Redemption::Reject(RejectReason::AlreadyRedeemed),
        CodeStatus::Revoked => Redemption::Reject(RejectReason::Revoked),
    }
}

/// A device's activation standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStanding {
    /// The device holds its credential and may proceed to present its lease.
    Activated,
    /// The device holds no credential and must run the activation exchange first.
    NeedsActivation,
}

/// A device's activation standing, from whether it holds its device credential.
///
/// The edge reads this from the `KeyVault` port — a `SecretName::DeviceCredential` that loads as
/// `Some` means activated — and this names the rule so the boot path has one statement rather than a
/// bare `if` repeated wherever activation is checked.
#[must_use]
pub const fn device_activation(credential_present: bool) -> ActivationStanding {
    if credential_present {
        ActivationStanding::Activated
    } else {
        ActivationStanding::NeedsActivation
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActivationCode, ActivationStanding, CodeError, CodeStatus, Redemption, RejectReason,
        canonical_form, checksum, device_activation, redeem,
    };

    /// Builds the canonical text of a valid code from an eleven-symbol payload, appending the checksum
    /// the parser will recompute.
    fn make_code(payload: [u8; 11]) -> String {
        let check = checksum(&payload);
        let mut values = payload.to_vec();
        values.push(check);
        canonical_form(&values)
    }

    #[test]
    fn a_valid_code_round_trips_through_parse() {
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let code = ActivationCode::parse(&text).expect("a freshly built code parses");
        assert_eq!(code.as_str(), text, "parse preserves the canonical form");
        assert_eq!(
            text.len(),
            14,
            "twelve symbols, two hyphens: XXXX-XXXX-XXXX"
        );
    }

    #[test]
    fn parse_ignores_case_hyphens_and_whitespace() {
        let text = make_code([2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31]);
        let scrambled = format!("  {}  ", text.to_lowercase().replace('-', " "));
        let code = ActivationCode::parse(&scrambled).expect("a scrambled but valid code parses");
        assert_eq!(
            code.as_str(),
            text,
            "normalisation recovers the same canonical code"
        );
    }

    #[test]
    fn parse_folds_the_ambiguous_glyphs() {
        // A payload whose canonical form begins "0123" so it contains a '0' and a '1' to substitute.
        let text = make_code([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert!(text.starts_with("0123"), "fixture assumption: {text}");
        let typed = text.replacen('0', "O", 1).replacen('1', "I", 1);
        let code = ActivationCode::parse(&typed).expect("O folds to 0 and I folds to 1");
        assert_eq!(code.as_str(), text);
    }

    #[test]
    fn parse_rejects_an_empty_or_separator_only_input() {
        assert_eq!(ActivationCode::parse(""), Err(CodeError::Empty));
        assert_eq!(ActivationCode::parse("  --  "), Err(CodeError::Empty));
    }

    #[test]
    fn parse_rejects_the_wrong_length() {
        assert_eq!(ActivationCode::parse("0123"), Err(CodeError::WrongLength));
        assert_eq!(
            ActivationCode::parse("0123-4567-89AB-CD"),
            Err(CodeError::WrongLength)
        );
    }

    #[test]
    fn parse_rejects_a_symbol_outside_the_alphabet() {
        // 'U' is one of the four glyphs Crockford omits, so it is never folded and never valid.
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let mut chars: Vec<char> = text.chars().collect();
        if let Some(first) = chars.iter_mut().find(|c| **c != '-') {
            *first = 'U';
        }
        let bad: String = chars.into_iter().collect();
        assert_eq!(ActivationCode::parse(&bad), Err(CodeError::BadSymbol));
    }

    #[test]
    fn parse_rejects_a_single_wrong_symbol() {
        // Substitute one payload symbol for a different valid symbol: the checksum no longer agrees.
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        // The first symbol is '1'; make it '2', which is a valid symbol but breaks the checksum.
        let mangled = text.replacen('1', "2", 1);
        assert_ne!(mangled, text);
        assert_eq!(ActivationCode::parse(&mangled), Err(CodeError::BadChecksum));
    }

    #[test]
    fn parse_rejects_an_adjacent_transposition() {
        // Swapping two adjacent, distinct payload symbols shifts the weighted sum, so it is caught.
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let mut chars: Vec<char> = text.chars().filter(|c| *c != '-').collect();
        chars.swap(0, 1); // '1' and '2' → '2' and '1'
        let swapped: String = chars.into_iter().collect();
        assert_eq!(ActivationCode::parse(&swapped), Err(CodeError::BadChecksum));
    }

    #[test]
    fn the_code_never_reveals_itself_through_debug() {
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let code = ActivationCode::parse(&text).expect("valid");
        let debugged = format!("{code:?}");
        assert!(!debugged.contains(code.as_str()), "got {debugged}");
        assert_eq!(debugged, "ActivationCode(redacted)");
    }

    #[test]
    fn a_cell_prefix_is_split_off_and_kept_out_of_the_code() {
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let code = ActivationCode::parse(&format!("VN1-{text}")).expect("a prefixed code parses");
        assert_eq!(code.cell_id(), Some("vn1"));
        assert_eq!(
            code.as_str(),
            text,
            "the credential is the twelve symbols alone"
        );
        let bare = ActivationCode::parse(&text).expect("the same code without a prefix");
        assert_eq!(bare.cell_id(), None);
        assert_eq!(
            bare.as_str(),
            code.as_str(),
            "a prefix never changes the code"
        );
    }

    #[test]
    fn a_cell_prefix_is_read_case_insensitively_and_never_folded() {
        let text = make_code([2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31]);
        for (typed, cell_id) in [
            ("jp-east-2", "jp-east-2"),
            ("US1", "us1"),
            ("lon1", "lon1"),
            ("Io-Oil", "io-oil"),
        ] {
            let code = ActivationCode::parse(&format!("{typed}-{}", text.to_lowercase()))
                .unwrap_or_else(|error| panic!("{typed}: {error}"));
            assert_eq!(code.cell_id(), Some(cell_id), "{typed}");
            assert_eq!(code.as_str(), text, "{typed}");
        }
    }

    #[test]
    fn a_cell_prefix_tolerates_spaces_around_its_hyphen_and_ungrouped_symbols() {
        let text = make_code([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let plain = text.replace('-', "");
        for typed in [
            format!("  vn1 - {text} "),
            format!("vn1-{plain}"),
            format!("vn1-{}", text.replace('-', " ")),
        ] {
            let code = ActivationCode::parse(&typed).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(code.cell_id(), Some("vn1"), "{typed:?}");
            assert_eq!(code.as_str(), text, "{typed:?}");
        }
    }

    #[test]
    fn the_setup_sheet_text_carries_the_prefix_upper_case_and_parses_back() {
        let code = ActivationCode::from_entropy([7; super::PAYLOAD_LEN]);
        assert_eq!(
            code.to_setup_sheet_text(),
            code.as_str(),
            "no prefix, no change"
        );
        let prefixed = code.clone().with_cell_id("jp-East-2").expect("a cell id");
        let printed = prefixed.to_setup_sheet_text();
        assert_eq!(printed, format!("JP-EAST-2-{}", code.as_str()));
        assert_eq!(ActivationCode::parse(&printed), Ok(prefixed));
    }

    #[test]
    fn with_cell_id_refuses_what_is_not_a_cell_id() {
        let code = ActivationCode::from_entropy([3; super::PAYLOAD_LEN]);
        let too_long = "a".repeat(33);
        for bad in [
            "",
            "vn",
            "1vn",
            "vn1-",
            "vn_1",
            "v\u{0129}1",
            too_long.as_str(),
        ] {
            assert_eq!(
                code.clone().with_cell_id(bad),
                Err(CodeError::BadCellPrefix),
                "{bad:?}"
            );
        }
        assert!(code.clone().with_cell_id(&"a".repeat(32)).is_ok());
        assert!(code.with_cell_id("abc").is_ok());
    }

    #[test]
    fn a_prefix_that_is_not_a_cell_id_is_refused() {
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        for prefix in ["1vn", "vn", "vn1-", "vn_1", "v\u{0129}1"] {
            assert_eq!(
                ActivationCode::parse(&format!("{prefix}-{text}")),
                Err(CodeError::BadCellPrefix),
                "{prefix:?}"
            );
        }
    }

    #[test]
    fn a_prefix_with_no_hyphen_before_the_code_is_refused() {
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(
            ActivationCode::parse(&format!("us1{text}")),
            Err(CodeError::PrefixNotSeparated),
            "a 'u' is no code symbol, so this is a prefix missing its hyphen"
        );
        assert_eq!(
            ActivationCode::parse(&format!("us1 {text}")),
            Err(CodeError::PrefixNotSeparated),
            "a space does not separate a prefix"
        );
        assert_eq!(
            ActivationCode::parse(&format!("vn1{text}")),
            Err(CodeError::WrongLength),
            "only code symbols before the code read as a code that is too long"
        );
    }

    #[test]
    fn a_prefixed_code_whose_twelve_symbols_are_wrong_is_refused_as_a_code() {
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let mangled = text.replacen('1', "2", 1);
        assert_eq!(
            ActivationCode::parse(&format!("vn1-{mangled}")),
            Err(CodeError::BadChecksum)
        );
        let with_u = text.replacen('5', "U", 1);
        assert_eq!(
            ActivationCode::parse(&format!("vn1-{with_u}")),
            Err(CodeError::BadSymbol)
        );
        assert_eq!(
            ActivationCode::parse("vn1-0123"),
            Err(CodeError::WrongLength)
        );
    }

    #[test]
    fn a_prefixed_code_never_reveals_itself_through_debug() {
        let text = make_code([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let code = ActivationCode::parse(&format!("vn1-{text}")).expect("valid");
        assert_eq!(format!("{code:?}"), "ActivationCode(redacted)");
    }

    #[test]
    fn only_an_issued_code_is_granted() {
        assert_eq!(redeem(CodeStatus::Issued), Redemption::Grant);
        assert_eq!(
            redeem(CodeStatus::Redeemed),
            Redemption::Reject(RejectReason::AlreadyRedeemed),
            "activation is single-use: a spent code is refused"
        );
        assert_eq!(
            redeem(CodeStatus::Revoked),
            Redemption::Reject(RejectReason::Revoked),
            "a cancelled code is refused"
        );
    }

    #[test]
    fn a_credential_makes_a_device_activated() {
        assert_eq!(device_activation(true), ActivationStanding::Activated);
        assert_eq!(
            device_activation(false),
            ActivationStanding::NeedsActivation
        );
    }

    #[test]
    fn from_entropy_always_produces_a_code_that_parses() {
        // Sweep every byte value in a uniform payload; each must yield a valid, parseable code —
        // proving the checksum is always consistent and no byte maps outside the alphabet.
        for seed in 0..=255_u8 {
            let code = ActivationCode::from_entropy([seed; super::PAYLOAD_LEN]);
            let reparsed = ActivationCode::parse(code.as_str()).expect("a minted code parses");
            assert_eq!(reparsed, code, "a minted code round-trips through parse");
        }
    }

    #[test]
    fn distinct_entropy_gives_distinct_codes() {
        let a = ActivationCode::from_entropy([0; super::PAYLOAD_LEN]);
        let b = ActivationCode::from_entropy([1; super::PAYLOAD_LEN]);
        assert_ne!(a, b);
    }
}
