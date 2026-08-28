// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The infrastructure fakes: link, blobs, metrics, signing, secrets.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pos_ports::blob_store::{BlobKey, BlobStore};
use pos_ports::cloud_sync::{ActivationGrant, CloudSync, UpdateReport};
use pos_ports::key_vault::{KeyVault, Secret, SecretName};
use pos_ports::message_link::{LinkCapacity, MessageLink, PublishOutcome};
use pos_ports::metrics_sink::{MetricSample, MetricsSink};
use pos_ports::signer::{KeyId, PublicKey, Signature, Signer};
use pos_ports::{PortError, PortName};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{DeviceId, StoreId, TenantId};
use pos_proto::protocol::{Hello, HelloOutcome, MIN_SUPPORTED_PROTOCOL_VERSION, negotiate};
use pos_proto::text::ReleaseTag;
use pos_proto::{PROTOCOL_VERSION, Ulid};

use crate::lock;

/// How many events a fake cloud accepts before reporting itself full.
///
/// Bounded so the harness has something to fill and the back-pressure case has something to check.
/// An unbounded fake would pass every case about back-pressure by never exercising it.
pub const LINK_CAPACITY_MESSAGES: u64 = 1_000;

/// How many samples a fake sink holds before dropping.
pub const METRICS_CAPACITY: usize = 1_000;

// -----------------------------------------------------------------------------------------------
// MessageLink
// -----------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct LinkState {
    negotiated: Option<u32>,
    severed: bool,
    published: Vec<EventEnvelope<RawPayload>>,
    /// Set by the harness to simulate a full JetStream stream, independently of what was actually
    /// published — a suite should not have to publish a thousand events to check back-pressure.
    forced_messages: Option<u64>,
}

/// An in-memory `MessageLink`.
#[derive(Debug, Clone, Default)]
pub struct FakeLink {
    state: Arc<Mutex<LinkState>>,
}

impl FakeLink {
    /// A link with no handshake completed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the far side unreachable.
    pub fn sever(&self) {
        lock(&self.state).severed = true;
    }

    /// Fills the far side to its stated limit.
    pub fn fill(&self) {
        lock(&self.state).forced_messages = Some(LINK_CAPACITY_MESSAGES);
    }

    /// Everything the far side has accepted.
    #[must_use]
    pub fn published(&self) -> Vec<EventEnvelope<RawPayload>> {
        lock(&self.state).published.clone()
    }
}

impl MessageLink for FakeLink {
    async fn handshake(&self, hello: &Hello) -> Result<HelloOutcome, PortError> {
        if lock(&self.state).severed {
            return Err(PortError::unavailable(
                PortName::MessageLink,
                "the link is severed",
            ));
        }
        // The real negotiation function from pos-proto, not a reimplementation. A fake that decided
        // its own version overlap would pass the handshake case while telling us nothing about the
        // rule ADR-0024 actually specifies.
        let outcome = negotiate(hello, MIN_SUPPORTED_PROTOCOL_VERSION, PROTOCOL_VERSION);
        if let HelloOutcome::Accepted { protocol_version } = outcome {
            lock(&self.state).negotiated = Some(protocol_version);
        }
        Ok(outcome)
    }

    async fn publish(
        &self,
        events: &[EventEnvelope<RawPayload>],
    ) -> Result<PublishOutcome, PortError> {
        let mut state = lock(&self.state);
        if state.negotiated.is_none() {
            return Err(PortError::failed_precondition(
                PortName::MessageLink,
                "no handshake has succeeded on this connection",
            ));
        }
        if state.severed {
            return Err(PortError::unavailable(
                PortName::MessageLink,
                "the link is severed",
            ));
        }
        let held = state
            .forced_messages
            .unwrap_or_else(|| u64::try_from(state.published.len()).unwrap_or(u64::MAX));
        if held >= LINK_CAPACITY_MESSAGES {
            return Err(PortError::resource_exhausted(
                PortName::MessageLink,
                "the stream is at capacity",
            ));
        }

        let room =
            usize::try_from(LINK_CAPACITY_MESSAGES.saturating_sub(held)).unwrap_or(usize::MAX);
        // Capped by the declared batch size as well as by the remaining room. The suite caught this
        // too: without the second cap the fake accepted 257 events from a link that advertises 256,
        // which would make a caller size its outbox reads by a number the link does not honour.
        let batch_limit = usize::try_from(self.max_batch_size().get()).unwrap_or(usize::MAX);
        let accepted = events.len().min(room).min(batch_limit);
        // A prefix, which is what the port promises. Taking an arbitrary subset would satisfy the
        // count and corrupt the caller's cursor.
        state
            .published
            .extend(events.iter().take(accepted).cloned());
        Ok(PublishOutcome {
            accepted: u32::try_from(accepted).unwrap_or(u32::MAX),
        })
    }

    async fn capacity(&self) -> Result<LinkCapacity, PortError> {
        let state = lock(&self.state);
        if state.severed {
            return Err(PortError::unavailable(
                PortName::MessageLink,
                "the link is severed",
            ));
        }
        let messages = state
            .forced_messages
            .unwrap_or_else(|| u64::try_from(state.published.len()).unwrap_or(u64::MAX));
        Ok(LinkCapacity {
            messages,
            message_limit: Some(LINK_CAPACITY_MESSAGES),
            bytes: messages.saturating_mul(512),
            byte_limit: Some(LINK_CAPACITY_MESSAGES.saturating_mul(512)),
        })
    }

    fn max_batch_size(&self) -> core::num::NonZeroU32 {
        core::num::NonZeroU32::new(256).unwrap_or(core::num::NonZeroU32::MIN)
    }
}

// -----------------------------------------------------------------------------------------------
// BlobStore
// -----------------------------------------------------------------------------------------------

/// An in-memory `BlobStore`.
#[derive(Debug, Clone, Default)]
pub struct FakeBlobStore {
    objects: Arc<Mutex<BTreeMap<BlobKey, Vec<u8>>>>,
}

impl FakeBlobStore {
    /// A store with no objects.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlobStore for FakeBlobStore {
    async fn put(&self, key: &BlobKey, body: &[u8]) -> Result<(), PortError> {
        lock(&self.objects).insert(key.clone(), body.to_vec());
        Ok(())
    }

    async fn get(&self, key: &BlobKey) -> Result<Option<Vec<u8>>, PortError> {
        Ok(lock(&self.objects).get(key).cloned())
    }

    async fn delete(&self, key: &BlobKey) -> Result<(), PortError> {
        lock(&self.objects).remove(key);
        Ok(())
    }

    async fn list(&self, prefix: &BlobKey) -> Result<Vec<BlobKey>, PortError> {
        // `is_under` rather than `starts_with`, which is the whole point of that method: listing
        // `stores/1` must not return `stores/10`.
        Ok(lock(&self.objects)
            .keys()
            .filter(|key| key.is_under(prefix))
            .cloned()
            .collect())
    }
}

// -----------------------------------------------------------------------------------------------
// MetricsSink
// -----------------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct SinkState {
    samples: Vec<MetricSample>,
    saturated: bool,
}

/// An in-memory `MetricsSink`.
#[derive(Debug, Clone, Default)]
pub struct FakeMetricsSink {
    state: Arc<Mutex<SinkState>>,
}

impl FakeMetricsSink {
    /// A sink with nothing recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything recorded, in arrival order.
    #[must_use]
    pub fn recorded(&self) -> Vec<MetricSample> {
        lock(&self.state).samples.clone()
    }

    /// Fills the sink, so the back-pressure case has something to check.
    pub fn saturate(&self) {
        lock(&self.state).saturated = true;
    }
}

impl MetricsSink for FakeMetricsSink {
    async fn record(&self, samples: &[MetricSample]) -> Result<(), PortError> {
        let mut state = lock(&self.state);
        if state.saturated || state.samples.len().saturating_add(samples.len()) > METRICS_CAPACITY {
            // Dropped, and reported as success. Telemetry sits off the sales path, so the caller has
            // nothing useful to do about it and must not be given a `?` to propagate.
            return Ok(());
        }
        state.samples.extend_from_slice(samples);
        Ok(())
    }
}

// -----------------------------------------------------------------------------------------------
// Signer
// -----------------------------------------------------------------------------------------------

/// How long a fake signature is: eight bytes of key id, eight of tag.
const SIGNATURE_LEN: usize = 16;

/// An in-memory `Signer`.
///
/// # This is not cryptography
///
/// The tag is an FNV-1a hash of the key bytes followed by the artifact. It has the *shape* a
/// signature scheme has — a key id a verifier can read before trusting anything, rejection of a
/// modified artifact, and a different answer for the wrong key — and none of the security. Real
/// verification is minisign over a vetted Ed25519 crate
/// ([ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md)); it will pass the same suite.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeSigner;

impl FakeSigner {
    /// A signer.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// A key pair's public half, derived from `seed` so a harness can produce two distinct keys.
    #[must_use]
    pub fn key(seed: u8) -> PublicKey {
        PublicKey::new(KeyId::new([seed; 8]), vec![seed; 32])
    }

    /// Signs `artifact` with `key`. Test-only, and the reason it lives on the fake rather than on the
    /// port: `docs/architecture.md` §4 keeps signing offline, so no port may ever sign.
    #[must_use]
    pub fn sign(artifact: &[u8], key: &PublicKey) -> Signature {
        let mut bytes = key.key_id().as_bytes().to_vec();
        bytes.extend_from_slice(&Self::tag(artifact, key).to_be_bytes());
        Signature::new(bytes)
    }

    /// FNV-1a over the key bytes then the artifact.
    fn tag(artifact: &[u8], key: &PublicKey) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in key.as_bytes().iter().chain(artifact.iter()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash
    }

    /// Splits a signature into its key id and tag.
    fn parse(signature: &Signature) -> Result<(KeyId, u64), PortError> {
        let bytes = signature.as_bytes();
        if bytes.len() != SIGNATURE_LEN {
            return Err(PortError::invalid_argument(
                PortName::Signer,
                "a signature is sixteen bytes",
            ));
        }
        let (id, tag) = bytes.split_at(8);
        let id: [u8; 8] = id.try_into().map_err(|_| {
            PortError::invalid_argument(PortName::Signer, "malformed signature key id")
        })?;
        let tag: [u8; 8] = tag.try_into().map_err(|_| {
            PortError::invalid_argument(PortName::Signer, "malformed signature tag")
        })?;
        Ok((KeyId::new(id), u64::from_be_bytes(tag)))
    }
}

impl Signer for FakeSigner {
    fn verify(
        &self,
        artifact: &[u8],
        signature: &Signature,
        key: &PublicKey,
    ) -> Result<(), PortError> {
        let (claimed, tag) = Self::parse(signature)?;
        if claimed != key.key_id() {
            // Invalid argument, not permission denied: this says "try the other baked-in key", and
            // collapsing the two makes a two-key rollout look like an attack.
            return Err(PortError::invalid_argument(
                PortName::Signer,
                "the signature names a different key",
            ));
        }
        if tag != Self::tag(artifact, key) {
            return Err(PortError::permission_denied(
                PortName::Signer,
                "the signature does not verify",
            ));
        }
        Ok(())
    }

    fn key_id_of(&self, signature: &Signature) -> Result<KeyId, PortError> {
        Self::parse(signature).map(|(key_id, _)| key_id)
    }
}

// -----------------------------------------------------------------------------------------------
// KeyVault
// -----------------------------------------------------------------------------------------------

/// An in-memory `KeyVault`.
#[derive(Debug, Clone, Default)]
pub struct FakeKeyVault {
    secrets: Arc<Mutex<BTreeMap<SecretName, Vec<u8>>>>,
}

impl FakeKeyVault {
    /// A vault holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyVault for FakeKeyVault {
    async fn store(&self, name: SecretName, secret: &Secret) -> Result<(), PortError> {
        lock(&self.secrets).insert(name, secret.expose().to_vec());
        Ok(())
    }

    async fn load(&self, name: SecretName) -> Result<Option<Secret>, PortError> {
        Ok(lock(&self.secrets)
            .get(&name)
            .map(|bytes| Secret::new(bytes.clone())))
    }

    async fn delete(&self, name: SecretName) -> Result<(), PortError> {
        lock(&self.secrets).remove(&name);
        Ok(())
    }
}

// -----------------------------------------------------------------------------------------------
// CloudSync
// -----------------------------------------------------------------------------------------------

/// An in-memory `CloudSync`: one recognised activation code and one published release.
///
/// A transport has no state to reset, so this is a unit struct whose fixtures are associated
/// constants and functions — the harness echoes them to the suite so it knows the right answers.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeCloudSync;

impl FakeCloudSync {
    /// The one activation code this channel accepts; anything else is refused.
    pub const VALID_CODE: &'static str = "AAAA-AAAA-AAAA";
    /// The one release this channel publishes; anything else is not found.
    pub const KNOWN_RELEASE: &'static str = "v1.2.3";

    /// A channel with the fixed fixtures.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The device [`Self::VALID_CODE`] grants.
    #[must_use]
    pub fn granted_device() -> DeviceId {
        DeviceId::new(Ulid::from_u128(0x0DE7))
    }

    /// The credential bytes [`Self::VALID_CODE`] grants.
    #[must_use]
    pub fn credential_bytes() -> Vec<u8> {
        b"fake-device-credential".to_vec()
    }

    /// The artifact bytes [`Self::KNOWN_RELEASE`] returns.
    #[must_use]
    pub fn artifact_bytes() -> Vec<u8> {
        b"fake-update-artifact".to_vec()
    }

    /// A well-formed update report the channel accepts — a store on [`Self::KNOWN_RELEASE`] whose
    /// self-test passed.
    #[must_use]
    pub fn sample_report() -> UpdateReport {
        UpdateReport {
            tenant: TenantId::new(Ulid::from_u128(0x7E5A)),
            store: StoreId::new(Ulid::from_u128(0x570E)),
            installed: ReleaseTag::new(Self::KNOWN_RELEASE),
            self_test_passed: true,
        }
    }
}

impl CloudSync for FakeCloudSync {
    async fn activate(&self, activation_code: &str) -> Result<ActivationGrant, PortError> {
        if activation_code == Self::VALID_CODE {
            Ok(ActivationGrant {
                device_id: Self::granted_device(),
                credential: Secret::new(Self::credential_bytes()),
            })
        } else {
            // No oracle: a spent, revoked, or unknown code are all one refusal.
            Err(PortError::permission_denied(
                PortName::CloudSync,
                "activation refused",
            ))
        }
    }

    async fn fetch_update(&self, release: &ReleaseTag) -> Result<Vec<u8>, PortError> {
        if release.as_str() == Self::KNOWN_RELEASE {
            Ok(Self::artifact_bytes())
        } else {
            Err(PortError::not_found(
                PortName::CloudSync,
                "no such release is published",
            ))
        }
    }

    async fn report(&self, _report: &UpdateReport) -> Result<(), PortError> {
        // A faithful sink: a well-formed report is accepted. The fake has no read model to inspect;
        // the cloud adapter's contract test exercises the wire, and the store adapter its persistence.
        Ok(())
    }
}
