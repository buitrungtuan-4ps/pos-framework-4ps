// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! QR ordering — the signed `table_id` and the guardrail decision
//! ([ADR-0057](../../../docs/adr/0057-qr-ordering.md), [ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)).
//!
//! A guest scanning a printed table code has no API key — the QR itself is the credential — so the
//! `table_id` travels as an HMAC-signed token the store's admin minted. The cloud verifies it, then
//! weighs a small set of guardrails (offline, business hours, per-table rate limit) before the
//! submission joins the ordinary `OrderIn` intake ([ADR-0056](../../../docs/adr/0056-public-order-intake.md)).
//! Both halves here are pure: the token crypto and the [`evaluate`] decision, gathered facts in,
//! verdict out, so every branch is a test with no clock, socket, or config reader.

use core::fmt;

use hmac::digest::KeyInit as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use pos_proto::ids::{StoreId, TableId, TenantId};
use pos_proto::ulid::Ulid;

type HmacSha256 = Hmac<Sha256>;

/// The per-deployment secret the cloud signs and verifies table QR tokens with.
///
/// Redacted from [`fmt::Debug`], so a struct that derives `Debug` and holds one cannot log it.
#[derive(Clone)]
pub struct TableTokenSecret(String);

impl TableTokenSecret {
    /// Wraps a secret string.
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for TableTokenSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TableTokenSecret(<redacted>)")
    }
}

/// The identity a verified table token carries: which tenant, store, and table the QR names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableRef {
    /// The tenant the table belongs to.
    pub tenant_id: TenantId,
    /// The store the table is in.
    pub store_id: StoreId,
    /// The table itself.
    pub table_id: TableId,
}

/// Why a presented table token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QrTokenError {
    /// The token is not `{tenant}.{store}.{table}.{hex}` of the right shape.
    #[error("the table token is malformed")]
    Malformed,
    /// The token is well-formed but its signature does not match — forged or tampered.
    #[error("the table token signature does not verify")]
    BadSignature,
}

/// Mints the token printed into a table's QR code: `{tenant}.{store}.{table}.{hex_tag}`.
///
/// The tag is `HMAC-SHA256(secret, "{tenant}.{store}.{table}")`, so the three ids are bound together
/// and a token for one store's table can never be replayed as another's.
#[must_use]
pub fn mint_table_token(
    secret: &TableTokenSecret,
    tenant_id: TenantId,
    store_id: StoreId,
    table_id: TableId,
) -> String {
    let message = message(tenant_id, store_id, table_id);
    format!("{message}.{}", to_hex(&mac(secret, message.as_bytes())))
}

/// Verifies a presented table token, returning the table it names.
///
/// The signature comparison is constant-time (`Mac::verify_slice`), so a verifier cannot leak the
/// expected tag a byte at a time.
///
/// # Errors
///
/// [`QrTokenError::Malformed`] if the token is not four dot-separated parts of the right shape, and
/// [`QrTokenError::BadSignature`] if the signature does not match — a forged or tampered QR.
pub fn verify_table_token(
    secret: &TableTokenSecret,
    token: &str,
) -> Result<TableRef, QrTokenError> {
    let mut parts = token.split('.');
    let (Some(tenant), Some(store), Some(table), Some(tag_hex), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(QrTokenError::Malformed);
    };

    let tenant_id = TenantId::new(
        tenant
            .parse::<Ulid>()
            .map_err(|_ignored| QrTokenError::Malformed)?,
    );
    let store_id = StoreId::new(
        store
            .parse::<Ulid>()
            .map_err(|_ignored| QrTokenError::Malformed)?,
    );
    let table_id = TableId::new(
        table
            .parse::<Ulid>()
            .map_err(|_ignored| QrTokenError::Malformed)?,
    );
    let expected = from_hex(tag_hex).ok_or(QrTokenError::Malformed)?;

    let mut hasher = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_ignored| QrTokenError::BadSignature)?;
    hasher.update(message(tenant_id, store_id, table_id).as_bytes());
    hasher
        .verify_slice(&expected)
        .map_err(|_ignored| QrTokenError::BadSignature)?;

    Ok(TableRef {
        tenant_id,
        store_id,
        table_id,
    })
}

/// The signed message: the three ids joined, in canonical ULID form.
fn message(tenant_id: TenantId, store_id: StoreId, table_id: TableId) -> String {
    format!(
        "{}.{}.{}",
        tenant_id.as_ulid(),
        store_id.as_ulid(),
        table_id.as_ulid()
    )
}

/// The facts the QR guardrail weighs, gathered by the endpoint from the seams that own them
/// ([ADR-0057](../../../docs/adr/0057-qr-ordering.md)).
#[expect(
    clippy::struct_excessive_bools,
    reason = "a facts bag whose four fields are independent yes/no signals, each read from a \
              different seam (the token check, the store link, the config's hours and staff-\
              confirmation default); it is built field-by-field, so there is none of the positional \
              boolean-argument confusion this lint guards against"
)]
#[derive(Debug, Clone, Copy)]
pub struct QrFacts {
    /// Whether the presented table token verified.
    pub token_valid: bool,
    /// Whether the store link is up. A QR order is rejected when the store is unreachable
    /// ([ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)).
    pub store_online: bool,
    /// Whether the current time is within the store's configured business hours.
    pub within_business_hours: bool,
    /// How many orders this table has placed in the current rate-limit window.
    pub submissions_in_window: u32,
    /// The per-table limit for that window.
    pub per_table_limit: u32,
    /// Whether staff must confirm before the kitchen sees the order — the configured default, on.
    pub staff_confirmation_required: bool,
}

/// Why a QR submission was refused before it reached the intake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrRejection {
    /// The table token did not verify — a forged or expired printed code.
    UntrustedTable,
    /// The store is offline; the guest is told to ask a member of staff.
    StoreOffline,
    /// The store is closed.
    OutsideBusinessHours,
    /// This table has hit its per-window submission limit.
    RateLimited,
}

/// The QR guardrail verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrDecision {
    /// Let the submission through to the intake; `require_staff_confirmation` is the cloud-side
    /// policy the store must honour.
    Accept {
        /// Whether a member of staff must confirm before the kitchen sees it.
        require_staff_confirmation: bool,
    },
    /// Refuse, for the given reason.
    Reject(QrRejection),
}

/// Weighs the QR guardrails in a fixed precedence
/// ([ADR-0057](../../../docs/adr/0057-qr-ordering.md)): a forged token is refused first, then an
/// offline store, then out-of-hours, then the rate limit; otherwise the order is accepted.
#[must_use]
pub fn evaluate(facts: &QrFacts) -> QrDecision {
    if !facts.token_valid {
        return QrDecision::Reject(QrRejection::UntrustedTable);
    }
    if !facts.store_online {
        return QrDecision::Reject(QrRejection::StoreOffline);
    }
    if !facts.within_business_hours {
        return QrDecision::Reject(QrRejection::OutsideBusinessHours);
    }
    if facts.submissions_in_window >= facts.per_table_limit {
        return QrDecision::Reject(QrRejection::RateLimited);
    }
    QrDecision::Accept {
        require_staff_confirmation: facts.staff_confirmation_required,
    }
}

/// Lower-case hex, no separators.
fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        // A byte is always two hex digits; writing to a `String` is infallible.
        let _ignored = fmt::write(&mut out, format_args!("{byte:02x}"));
    }
    out
}

/// Decodes lower- or upper-case hex, or `None` if the input is not an even run of hex digits.
fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = core::str::from_utf8(pair).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
        .collect()
}

/// HMAC-SHA256 over `message`. HMAC accepts a key of any length, so `new_from_slice` cannot fail
/// here; the impossible error path yields an empty tag a verifier rejects rather than a panic.
fn mac(secret: &TableTokenSecret, message: &[u8]) -> Vec<u8> {
    match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(mut hasher) => {
            hasher.update(message);
            hasher.finalize().into_bytes().to_vec()
        }
        Err(_ignored) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        QrDecision, QrFacts, QrRejection, QrTokenError, TableTokenSecret, evaluate,
        mint_table_token, verify_table_token,
    };
    use pos_proto::ids::{StoreId, TableId, TenantId};
    use pos_proto::ulid::Ulid;

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(0x7E11A))
    }
    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(0x5_709E))
    }
    fn table() -> TableId {
        TableId::new(Ulid::from_u128(0x7AB1E))
    }

    fn facts() -> QrFacts {
        QrFacts {
            token_valid: true,
            store_online: true,
            within_business_hours: true,
            submissions_in_window: 0,
            per_table_limit: 5,
            staff_confirmation_required: true,
        }
    }

    #[test]
    fn a_minted_token_verifies_to_its_table() {
        let secret = TableTokenSecret::new("qr-signing-secret");
        let token = mint_table_token(&secret, tenant(), store(), table());
        let table_ref = verify_table_token(&secret, &token).expect("verifies");
        assert_eq!(table_ref.tenant_id, tenant());
        assert_eq!(table_ref.store_id, store());
        assert_eq!(table_ref.table_id, table());
    }

    #[test]
    fn a_tampered_tag_is_refused() {
        let secret = TableTokenSecret::new("qr-signing-secret");
        let token = mint_table_token(&secret, tenant(), store(), table());
        // Flip the last hex digit of the tag.
        let mut tampered = token;
        let last = tampered.pop().unwrap_or('0');
        tampered.push(if last == '0' { '1' } else { '0' });
        assert_eq!(
            verify_table_token(&secret, &tampered),
            Err(QrTokenError::BadSignature)
        );
    }

    #[test]
    fn the_wrong_secret_is_refused() {
        let token = mint_table_token(&TableTokenSecret::new("right"), tenant(), store(), table());
        assert_eq!(
            verify_table_token(&TableTokenSecret::new("wrong"), &token),
            Err(QrTokenError::BadSignature)
        );
    }

    #[test]
    fn a_token_for_one_table_does_not_verify_as_another() {
        // Re-signing a different table's message with the same tag is refused: the tag binds the ids.
        let secret = TableTokenSecret::new("qr-signing-secret");
        let token = mint_table_token(&secret, tenant(), store(), table());
        // Swap the table id part for a different one, keeping the original tag.
        let other_table = TableId::new(Ulid::from_u128(0x7AB2F));
        let parts: Vec<&str> = token.split('.').collect();
        let forged = format!(
            "{}.{}.{}.{}",
            parts[0],
            parts[1],
            other_table.as_ulid(),
            parts[3]
        );
        assert_eq!(
            verify_table_token(&secret, &forged),
            Err(QrTokenError::BadSignature)
        );
    }

    #[test]
    fn a_structurally_wrong_token_is_malformed() {
        let secret = TableTokenSecret::new("qr-signing-secret");
        assert_eq!(
            verify_table_token(&secret, "not-a-token"),
            Err(QrTokenError::Malformed)
        );
        assert_eq!(
            verify_table_token(&secret, "a.b.c.d.e"),
            Err(QrTokenError::Malformed),
            "five parts is not a table token"
        );
    }

    #[test]
    fn a_clean_order_is_accepted_with_staff_confirmation() {
        assert_eq!(
            evaluate(&facts()),
            QrDecision::Accept {
                require_staff_confirmation: true
            }
        );
    }

    #[test]
    fn an_untrusted_token_is_refused_first() {
        let mut facts = facts();
        facts.token_valid = false;
        // Even with everything else also wrong, the untrusted table is the reported reason.
        facts.store_online = false;
        assert_eq!(
            evaluate(&facts),
            QrDecision::Reject(QrRejection::UntrustedTable)
        );
    }

    #[test]
    fn an_offline_store_is_refused() {
        let mut facts = facts();
        facts.store_online = false;
        assert_eq!(
            evaluate(&facts),
            QrDecision::Reject(QrRejection::StoreOffline)
        );
    }

    #[test]
    fn a_closed_store_is_refused() {
        let mut facts = facts();
        facts.within_business_hours = false;
        assert_eq!(
            evaluate(&facts),
            QrDecision::Reject(QrRejection::OutsideBusinessHours)
        );
    }

    #[test]
    fn a_table_at_its_limit_is_rate_limited() {
        let mut facts = facts();
        facts.submissions_in_window = 5;
        assert_eq!(
            evaluate(&facts),
            QrDecision::Reject(QrRejection::RateLimited)
        );
    }

    #[test]
    fn the_staff_confirmation_default_can_be_off() {
        let mut facts = facts();
        facts.staff_confirmation_required = false;
        assert_eq!(
            evaluate(&facts),
            QrDecision::Accept {
                require_staff_confirmation: false
            }
        );
    }
}
