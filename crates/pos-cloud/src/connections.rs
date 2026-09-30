// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's connections to its vendors
//! ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decisions 3 and 4).
//!
//! A *connection* is how a tenant uses a vendor without anyone writing code: a provider id from the
//! catalogue ([`crate::providers`]), a scope (the whole tenant, one brand, or one store), the
//! provider's settings, and its secrets. Switching vendor is editing a connection.
//!
//! # Secrets are sealed the moment they arrive, and never leave
//!
//! A secret field's value is sealed with XChaCha20-Poly1305 under [`ConnectionSecret`], a key
//! `bootstrap.sh` mints into `cloud.toml`, before the record reaches the store. The API accepts a
//! new value and never returns one. A read says only *which* secrets are set
//! ([`ConnectionView::secrets_set`]).
//!
//! The key is not in the database, for the reason `archive.rs` gives: `backup.sh` ships a
//! `pg_dump` off-box, and a plaintext column, or a key stored beside its ciphertext, would put a
//! tenant's e-invoice and gateway credentials in that bucket. The residue is the same too: the key
//! lives on the same box as the database, so a compromise of the *box* reaches both.
//!
//! Each sealed value is bound to its tenant, its connection and its field (the AEAD's associated
//! data). A sealed value copied into another connection's row, or under another field's key, does
//! not open.
//!
//! # What is not here yet
//!
//! Nothing *uses* a connection yet. The connectors that open a secret to call a vendor, the
//! `integrations` node that tells a store which connection serves it, and the test-connection
//! route come next. This module is the record, its sealing and its store.

use core::fmt;
use core::future::Future;
use std::collections::{BTreeMap, BTreeSet};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use pos_proto::ids::TenantId;
use pos_providers::Settings;

use crate::archive::{hex_bytes, to_hex};
use crate::version::{CreateOutcome, UpdateOutcome, Version, Versioned};

/// Bytes in the sealing key.
const KEY_LEN: usize = 32;

/// Characters in the key's text form.
pub const KEY_TEXT_LEN: usize = KEY_LEN * 2;

/// Bytes of nonce: XChaCha20-Poly1305, so a random nonce per seal needs no counter.
const NONCE_LEN: usize = 24;

/// What every seal is bound to besides its tenant, connection and field.
const SEAL_CONTEXT: &[u8] = b"pos-cloud/integration-secret/v1";

/// The box-local key every connection secret is sealed under.
///
/// 64 hexadecimal characters, like `archive_key_secret`. [`fmt::Debug`] prints nothing, because
/// [`crate::config::CloudConfig`] derives `Debug`.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct ConnectionSecret(String);

impl ConnectionSecret {
    /// Wraps a key as read from configuration. Validity is checked by [`Self::is_well_formed`] at
    /// boot, so a malformed value is one named refusal rather than a deserialization error.
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// Whether this key is usable, for [`crate::config::CloudConfig::validate`].
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.material().is_some()
    }

    fn material(&self) -> Option<Zeroizing<[u8; KEY_LEN]>> {
        let text = self.0.trim();
        if text.len() != KEY_TEXT_LEN {
            return None;
        }
        let bytes = Zeroizing::new(hex_bytes(text)?);
        let key: [u8; KEY_LEN] = bytes.as_slice().try_into().ok()?;
        Some(Zeroizing::new(key))
    }
}

impl fmt::Debug for ConnectionSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionSecret(redacted)")
    }
}

/// Where one sealed value belongs: what it is bound to, so it cannot be moved.
#[derive(Clone, Copy, Debug)]
pub struct SealedFor<'a> {
    /// The tenant that owns the connection.
    pub tenant_id: TenantId,
    /// The connection's id.
    pub connection_id: &'a str,
    /// The secret field's key.
    pub field_key: &'a str,
}

impl SealedFor<'_> {
    fn associated_data(&self) -> Vec<u8> {
        let tenant = self.tenant_id.to_string();
        let mut aad = Vec::with_capacity(
            SEAL_CONTEXT.len() + tenant.len() + self.connection_id.len() + self.field_key.len() + 3,
        );
        for part in [
            SEAL_CONTEXT,
            tenant.as_bytes(),
            self.connection_id.as_bytes(),
            self.field_key.as_bytes(),
        ] {
            aad.extend_from_slice(part);
            // A separator no part can contain, so `ab` + `c` never binds the same as `a` + `bc`.
            aad.push(0);
        }
        aad
    }
}

/// Why a secret could not be sealed or opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// The OS entropy source was unavailable, so no nonce could be drawn.
    #[error("the OS entropy source is unavailable, so a secret could not be sealed")]
    Entropy,
    /// `integration_secret` is malformed or is not the key this value was sealed under, or the
    /// value was sealed for a different tenant, connection or field. An authenticated cipher
    /// cannot tell these apart, and the operator's next step is the same: check `cloud.toml`.
    #[error(
        "integration_secret is malformed or is not the key this secret was sealed under, or the \
         sealed value was moved from another connection"
    )]
    Key,
    /// The stored value is not something [`seal`] wrote.
    #[error("a stored connection secret is not a sealed value")]
    Malformed,
}

/// Seals one secret value for storage, as lowercase hex of the nonce followed by the ciphertext.
///
/// # Errors
///
/// [`SealError::Key`] if the key is malformed, or [`SealError::Entropy`] if no nonce could be
/// drawn. A secret is never stored unsealed as a fallback.
pub fn seal(
    key: &ConnectionSecret,
    sealed_for: SealedFor<'_>,
    value: &str,
) -> Result<String, SealError> {
    let material = key.material().ok_or(SealError::Key)?;
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_error| SealError::Entropy)?;
    let aad = sealed_for.associated_data();
    let sealed = XChaCha20Poly1305::new((&*material).into())
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: value.as_bytes(),
                aad: &aad,
            },
        )
        .map_err(|_error| SealError::Key)?;
    let mut stored = Vec::with_capacity(NONCE_LEN + sealed.len());
    stored.extend_from_slice(&nonce);
    stored.extend_from_slice(&sealed);
    Ok(to_hex(&stored))
}

/// Opens a value [`seal`] stored, for the connector that has to send it to the vendor.
///
/// The plaintext is zeroized when dropped. It must go nowhere but the vendor's request: not a log,
/// an audit record, an error message or a response.
///
/// # Errors
///
/// [`SealError::Malformed`] if `stored` is not a sealed value, or [`SealError::Key`] if it does not
/// open under this key for this tenant, connection and field.
pub fn open(
    key: &ConnectionSecret,
    sealed_for: SealedFor<'_>,
    stored: &str,
) -> Result<Zeroizing<String>, SealError> {
    let material = key.material().ok_or(SealError::Key)?;
    let bytes = hex_bytes(stored.trim()).ok_or(SealError::Malformed)?;
    let nonce = bytes.get(..NONCE_LEN).ok_or(SealError::Malformed)?;
    let body = bytes.get(NONCE_LEN..).ok_or(SealError::Malformed)?;
    let nonce = XNonce::try_from(nonce).map_err(|_error| SealError::Malformed)?;
    let aad = sealed_for.associated_data();
    let opened = Zeroizing::new(
        XChaCha20Poly1305::new((&*material).into())
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: &aad,
                },
            )
            .map_err(|_error| SealError::Key)?,
    );
    let text = core::str::from_utf8(&opened).map_err(|_error| SealError::Malformed)?;
    Ok(Zeroizing::new(text.to_owned()))
}

/// Which layer of the config tree a connection serves.
///
/// The tree's own layers ([ADR-0033](../../../docs/adr/0033-config-tree.md)), so a connection is
/// inherited the way every other setting is: a store uses its own, or its brand's, or the tenant's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeLevel {
    /// Every store the tenant runs.
    #[serde(rename = "CONNECTION_SCOPE_TENANT")]
    Tenant,
    /// Every store of one brand. [`Connection::scope_id`] is the brand's id.
    #[serde(rename = "CONNECTION_SCOPE_BRAND")]
    Brand,
    /// One store. [`Connection::scope_id`] is the store's id.
    #[serde(rename = "CONNECTION_SCOPE_STORE")]
    Store,
}

/// One connection as stored: everything the tenant configured, with its secrets sealed.
///
/// Never serialized to a response. [`ConnectionView`] is what the API answers, and it has no field
/// a sealed value could travel in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// The connection's id, a server-minted ULID.
    pub connection_id: String,
    /// The provider it uses, from the catalogue.
    pub provider_id: String,
    /// The layer it serves.
    pub scope_level: ScopeLevel,
    /// The brand or store it serves; absent for the tenant layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    /// What the console calls it, so two connections to one provider can be told apart.
    pub display_name: String,
    /// Whether it is in use. A disabled connection is kept, with its secrets, for a vendor a
    /// tenant switches back to.
    pub enabled: bool,
    /// The provider's non-secret settings, validated against its schema.
    #[serde(default)]
    pub settings: Settings,
    /// Each secret field's sealed value, by field key.
    #[serde(default)]
    pub sealed_secrets: BTreeMap<String, String>,
}

impl Connection {
    /// The secret fields this connection holds a value for.
    #[must_use]
    pub fn secrets_set(&self) -> BTreeSet<String> {
        self.sealed_secrets.keys().cloned().collect()
    }
}

/// A connection as the API answers it: the record without its sealed values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConnectionView {
    connection_id: String,
    provider_id: String,
    scope_level: ScopeLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope_id: Option<String>,
    display_name: String,
    enabled: bool,
    settings: Settings,
    /// Which secret fields have a value. The values themselves are never returned.
    secrets_set: Vec<String>,
}

impl ConnectionView {
    /// Drops the sealed values from a record.
    #[must_use]
    pub fn of(connection: &Connection) -> Self {
        Self {
            connection_id: connection.connection_id.clone(),
            provider_id: connection.provider_id.clone(),
            scope_level: connection.scope_level,
            scope_id: connection.scope_id.clone(),
            display_name: connection.display_name.clone(),
            enabled: connection.enabled,
            settings: connection.settings.clone(),
            secrets_set: connection.secrets_set().into_iter().collect(),
        }
    }
}

/// Which connections serve one store, as the `integrations` node it receives
/// ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decision 5).
///
/// A connection serves the store when it is enabled, names a provider this cloud has, and is scoped
/// to the store itself, to the store's brand, or to the tenant. They are ordered most specific
/// first. In a family a scope may hold only one of (e-invoice, ERP), only the most specific
/// survives, so a store with its own e-invoice provider is never also sent its tenant's; every
/// other family is plural, and the store receives them all.
///
/// Settings go only with a family the store drives itself (card terminals), and never a secret: the
/// node has no field a sealed value could travel in.
#[must_use]
pub fn resolve_for_store(
    connections: &[Connection],
    providers: &pos_providers::Registry,
    store_id: &str,
    brand_id: Option<&str>,
) -> pos_proto::integrations::PublishedIntegrations {
    use pos_proto::integrations::{PublishedIntegration, PublishedIntegrations};
    use pos_proto::text::DisplayName;
    use pos_providers::Runtime;

    // How specific a connection is to this store, or `None` if it does not serve it.
    let specificity = |connection: &Connection| -> Option<u8> {
        match (connection.scope_level, connection.scope_id.as_deref()) {
            (ScopeLevel::Store, Some(id)) if id == store_id => Some(0),
            (ScopeLevel::Brand, Some(id)) if Some(id) == brand_id => Some(1),
            (ScopeLevel::Tenant, _) => Some(2),
            _ => None,
        }
    };
    let mut serving: Vec<(u8, &Connection, &'static pos_providers::ProviderDescriptor)> =
        connections
            .iter()
            .filter(|connection| connection.enabled)
            .filter_map(|connection| {
                let rank = specificity(connection)?;
                let provider = providers.get(&connection.provider_id)?;
                Some((rank, connection, provider))
            })
            .collect();
    serving.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.connection_id.cmp(&b.1.connection_id))
    });

    let mut taken = BTreeSet::new();
    let integrations = serving
        .into_iter()
        .filter(|(_, _, provider)| {
            // The first in an exclusive family is the most specific; the rest are shadowed.
            !provider.family.exclusive() || taken.insert(provider.family)
        })
        .map(|(_, connection, provider)| PublishedIntegration {
            connection_id: connection.connection_id.clone(),
            family: provider.family.integration_family().into(),
            provider_id: connection.provider_id.clone(),
            display_name: DisplayName::new(connection.display_name.clone()),
            settings: if provider.family.runs_on() == Runtime::Edge {
                connection.settings.clone()
            } else {
                BTreeMap::new()
            },
        })
        .collect();
    PublishedIntegrations::new(integrations)
}

/// Persists and reads a tenant's connections.
///
/// The same per-record shape as [`crate::reason_codes::ReasonCodeStore`]: tenant-scoped, a create
/// that refuses a taken id, an update only at the version the caller read, and a delete that is not
/// an error when the row is already gone.
pub trait ConnectionStore {
    /// Every connection a tenant has, in id order (creation order, for a ULID).
    fn list(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<Connection>>, ConnectionStoreError>> + Send;

    /// One connection by id, or `None`.
    fn get(
        &self,
        tenant_id: TenantId,
        connection_id: &str,
    ) -> impl Future<Output = Result<Option<Versioned<Connection>>, ConnectionStoreError>> + Send;

    /// Inserts a connection, refusing if one already holds its id.
    fn create(
        &self,
        tenant_id: TenantId,
        connection: &Connection,
    ) -> impl Future<Output = Result<CreateOutcome, ConnectionStoreError>> + Send;

    /// Replaces a connection, only at the version the caller read it at.
    fn update(
        &self,
        tenant_id: TenantId,
        connection: &Connection,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, ConnectionStoreError>> + Send;

    /// Removes a connection. Removing one that does not exist is not an error.
    ///
    /// Unlike a reason code, nothing historic cites a connection by id, so deleting one is the
    /// ordinary way to be rid of it — and it takes the sealed secrets with it, which is what an
    /// operator removing a vendor's credentials expects.
    fn delete(
        &self,
        tenant_id: TenantId,
        connection_id: &str,
    ) -> impl Future<Output = Result<(), ConnectionStoreError>> + Send;
}

/// A failure of the connection store itself: the database is unreachable, or a stored row could
/// not be decoded.
#[derive(Debug, thiserror::Error)]
#[error("the connection store failed: {0}")]
pub struct ConnectionStoreError(String);

impl ConnectionStoreError {
    /// Wraps a message (for the server's log). Never a secret's value.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pos_proto::ids::TenantId;
    use pos_proto::ulid::Ulid;
    use pos_providers::SettingValue;

    use super::{
        Connection, ConnectionSecret, ConnectionView, ScopeLevel, SealError, SealedFor, open, seal,
    };

    fn key() -> ConnectionSecret {
        ConnectionSecret::new("11".repeat(32))
    }

    fn sealed_for(field_key: &str) -> SealedFor<'_> {
        SealedFor {
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            connection_id: "01J0000000000000000000CONN",
            field_key,
        }
    }

    #[test]
    fn a_sealed_secret_opens_only_where_it_was_sealed() {
        let stored = seal(&key(), sealed_for("api_key"), "s3cret").expect("seals");
        assert!(!stored.contains("s3cret"));
        assert_eq!(
            open(&key(), sealed_for("api_key"), &stored)
                .expect("opens")
                .as_str(),
            "s3cret"
        );

        // Moved under another field, another connection, or another tenant, it does not open.
        assert_eq!(
            open(&key(), sealed_for("password"), &stored).map(|_| ()),
            Err(SealError::Key)
        );
        let elsewhere = SealedFor {
            connection_id: "01J0000000000000000000OTHR",
            ..sealed_for("api_key")
        };
        assert_eq!(
            open(&key(), elsewhere, &stored).map(|_| ()),
            Err(SealError::Key)
        );
        let other_tenant = SealedFor {
            tenant_id: TenantId::new(Ulid::from_u128(2)),
            ..sealed_for("api_key")
        };
        assert_eq!(
            open(&key(), other_tenant, &stored).map(|_| ()),
            Err(SealError::Key)
        );

        // Under another key, it does not open either.
        let other_key = ConnectionSecret::new("22".repeat(32));
        assert_eq!(
            open(&other_key, sealed_for("api_key"), &stored).map(|_| ()),
            Err(SealError::Key)
        );
    }

    #[test]
    fn two_seals_of_one_value_differ_and_garbage_is_malformed() {
        let first = seal(&key(), sealed_for("api_key"), "same").expect("seals");
        let second = seal(&key(), sealed_for("api_key"), "same").expect("seals");
        assert_ne!(first, second, "a fresh nonce per seal");
        assert_eq!(
            open(&key(), sealed_for("api_key"), "not hex").map(|_| ()),
            Err(SealError::Malformed)
        );
        assert_eq!(
            open(&key(), sealed_for("api_key"), "abcd").map(|_| ()),
            Err(SealError::Malformed),
            "too short to hold a nonce"
        );
        // One altered character in the ciphertext and the tag no longer verifies.
        let mut tampered = first.clone();
        let last = tampered.pop().expect("a sealed value is not empty");
        tampered.push(if last == '0' { '1' } else { '0' });
        assert_eq!(
            open(&key(), sealed_for("api_key"), &tampered).map(|_| ()),
            Err(SealError::Key)
        );
    }

    #[test]
    fn a_malformed_key_seals_nothing() {
        let short = ConnectionSecret::new("abc");
        assert!(!short.is_well_formed());
        assert_eq!(
            seal(&short, sealed_for("api_key"), "x").map(|_| ()),
            Err(SealError::Key)
        );
        assert!(key().is_well_formed());
        assert_eq!(format!("{:?}", key()), "ConnectionSecret(redacted)");
    }

    fn connection(
        id: &str,
        provider_id: &str,
        scope: (ScopeLevel, Option<&str>),
        enabled: bool,
    ) -> Connection {
        Connection {
            connection_id: id.to_owned(),
            provider_id: provider_id.to_owned(),
            scope_level: scope.0,
            scope_id: scope.1.map(str::to_owned),
            display_name: format!("Connection {id}"),
            enabled,
            settings: BTreeMap::from([(
                "address".to_owned(),
                SettingValue::Text("192.0.2.40".to_owned()),
            )]),
            sealed_secrets: BTreeMap::from([("api_key".to_owned(), "00ff".to_owned())]),
        }
    }

    static LEDGER: pos_providers::ProviderDescriptor = pos_providers::ProviderDescriptor {
        provider_id: "erp.ledger",
        family: pos_providers::Family::Erp,
        name_key: "provider.erp.ledger",
        countries: &[],
        fields: &[],
        capabilities: &[],
        sandbox: true,
    };
    static COURIER: pos_providers::ProviderDescriptor = pos_providers::ProviderDescriptor {
        provider_id: "courier.fast",
        family: pos_providers::Family::Courier,
        ..LEDGER
    };
    static TERMINAL: pos_providers::ProviderDescriptor = pos_providers::ProviderDescriptor {
        provider_id: "card.counter",
        family: pos_providers::Family::CardTerminal,
        ..LEDGER
    };

    #[test]
    fn a_store_gets_its_own_ledger_over_its_brands_and_every_courier_that_serves_it() {
        use pos_proto::integrations::IntegrationFamily;

        let registry = pos_providers::Registry::new([&LEDGER, &COURIER, &TERMINAL]).expect("valid");
        let all = [
            connection("01A", "erp.ledger", (ScopeLevel::Tenant, None), true),
            connection(
                "01B",
                "erp.ledger",
                (ScopeLevel::Brand, Some("BRAND")),
                true,
            ),
            connection("01C", "courier.fast", (ScopeLevel::Tenant, None), true),
            connection(
                "01D",
                "courier.fast",
                (ScopeLevel::Store, Some("STORE")),
                true,
            ),
            connection(
                "01E",
                "courier.fast",
                (ScopeLevel::Store, Some("ELSEWHERE")),
                true,
            ),
            connection("01F", "courier.fast", (ScopeLevel::Tenant, None), false),
            connection("01G", "courier.nobody", (ScopeLevel::Tenant, None), true),
            connection(
                "01H",
                "card.counter",
                (ScopeLevel::Store, Some("STORE")),
                true,
            ),
        ];
        let node = super::resolve_for_store(&all, &registry, "STORE", Some("BRAND"));
        let ids: Vec<&str> = node
            .integrations()
            .iter()
            .map(|integration| integration.connection_id.as_str())
            .collect();
        // Store scope first, then brand, then tenant. The tenant's ledger is shadowed by the
        // brand's; another store's, a disabled one and an unknown provider serve nobody.
        assert_eq!(ids, vec!["01D", "01H", "01B", "01C"]);

        let ledger = node
            .in_family(IntegrationFamily::Erp)
            .next()
            .expect("one ledger");
        assert!(
            ledger.settings.is_empty(),
            "the cloud drives an ERP; its settings stay there"
        );
        let terminal = node
            .in_family(IntegrationFamily::CardTerminal)
            .next()
            .expect("the counter terminal");
        assert_eq!(
            terminal.settings.get("address"),
            Some(&SettingValue::Text("192.0.2.40".to_owned())),
            "a driver the store runs needs its settings"
        );
        let json = serde_json::to_string(&node).expect("serializes");
        assert!(
            !json.contains("00ff") && !json.contains("sealed"),
            "no secret, ever: {json}"
        );
    }

    #[test]
    fn the_view_names_which_secrets_are_set_and_carries_none_of_them() {
        let connection = Connection {
            connection_id: "01J0000000000000000000CONN".to_owned(),
            provider_id: "courier.ahamove".to_owned(),
            scope_level: ScopeLevel::Store,
            scope_id: Some("01J00000000000000000STORE1".to_owned()),
            display_name: "Ahamove, District 1".to_owned(),
            enabled: true,
            settings: BTreeMap::from([(
                "base_url".to_owned(),
                SettingValue::Text("https://api.example".to_owned()),
            )]),
            sealed_secrets: BTreeMap::from([("api_key".to_owned(), "deadbeef".to_owned())]),
        };
        let view = serde_json::to_value(ConnectionView::of(&connection)).expect("serializes");
        assert_eq!(view["secrets_set"], serde_json::json!(["api_key"]));
        assert_eq!(view["scope_level"], "CONNECTION_SCOPE_STORE");
        assert!(view.get("sealed_secrets").is_none());
        assert!(!view.to_string().contains("deadbeef"));

        // The stored document round-trips, sealed values included.
        let stored = serde_json::to_string(&connection).expect("serializes");
        let back: Connection = serde_json::from_str(&stored).expect("parses");
        assert_eq!(back, connection);
    }
}
