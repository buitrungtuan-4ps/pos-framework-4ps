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
//!    the cloud is the only authority on whether a code is live.
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

/// The number of payload symbols — the code length without its one checksum symbol, and the number
/// of entropy bytes [`ActivationCode::from_entropy`] consumes.
pub const PAYLOAD_LEN: usize = CODE_LENGTH - 1;

/// A validated activation code.
///
/// Constructed only through [`ActivationCode::parse`], which normalises human input and verifies the
/// checksum, so an `ActivationCode` value is always well-formed. It is a bearer credential until it is
/// redeemed, so its [`Debug`] is redacted; [`ActivationCode::as_str`] is the one accessor that yields
/// the code itself, named to be conspicuous in a diff.
#[derive(Clone, PartialEq, Eq)]
pub struct ActivationCode {
    /// The canonical `XXXX-XXXX-XXXX` text.
    canonical: String,
}

impl ActivationCode {
    /// Parses and validates an activation code from human input.
    ///
    /// Input is normalised the way Crockford's alphabet is meant to be read: case is ignored, hyphens
    /// and ASCII whitespace are discarded, and the ambiguous glyphs are folded (`I` and `L` become
    /// `1`, `O` becomes `0`). The result must be exactly twelve symbols and its trailing
    /// checksum must agree, so a mistyped code is rejected here rather than after a network round-trip.
    ///
    /// # Errors
    ///
    /// [`CodeError::Empty`] if nothing but separators was given, [`CodeError::WrongLength`] if the
    /// symbol count is not twelve, [`CodeError::BadSymbol`] for a character outside the
    /// alphabet, and [`CodeError::BadChecksum`] if the checksum symbol does not match the payload.
    pub fn parse(input: &str) -> Result<Self, CodeError> {
        let mut values: Vec<u8> = Vec::with_capacity(CODE_LENGTH);
        for raw in input.chars() {
            let folded = match raw {
                '-' | ' ' | '\t' | '\n' | '\r' => continue,
                'I' | 'i' | 'L' | 'l' => '1',
                'O' | 'o' => '0',
                other => other.to_ascii_uppercase(),
            };
            let byte = u8::try_from(u32::from(folded)).map_err(|_ignored| CodeError::BadSymbol)?;
            let value = ALPHABET
                .iter()
                .position(|&symbol| symbol == byte)
                .ok_or(CodeError::BadSymbol)?;
            let value = u8::try_from(value).map_err(|_ignored| CodeError::BadSymbol)?;
            values.push(value);
        }
        if values.is_empty() {
            return Err(CodeError::Empty);
        }
        if values.len() != CODE_LENGTH {
            return Err(CodeError::WrongLength);
        }
        let (check, payload) = values.split_last().ok_or(CodeError::WrongLength)?;
        if checksum(payload) != *check {
            return Err(CodeError::BadChecksum);
        }
        Ok(Self {
            canonical: canonical_form(&values),
        })
    }

    /// The canonical `XXXX-XXXX-XXXX` text of the code.
    ///
    /// The one accessor that yields the code itself, named to be conspicuous: the code is a bearer
    /// credential until it is redeemed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.canonical.as_str()
    }

    /// Mints a fresh code from `entropy`, one byte per payload symbol.
    ///
    /// Each byte picks a symbol by its remainder modulo the alphabet size; the choice is unbiased
    /// because 256 is a whole multiple of 32. The checksum symbol is then appended, so the result
    /// always [`ActivationCode::parse`]s. `pos-core` reads no randomness of its own — the caller
    /// supplies the entropy (the cloud passes bytes from its CSPRNG), which keeps code generation at
    /// the I/O edge.
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
        }
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
