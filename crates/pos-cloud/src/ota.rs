// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The OTA release registry ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md), roadmap-v3
//! slice R2).
//!
//! R1's release workflow cross-compiles `pos-edge` for both store targets, signs each artifact with
//! minisign — keys in GitHub secrets, never on a VPS (debate D1) — and publishes the artifact and its
//! `.minisig` to a GitHub Release. The edge's `OtaUpdater` decides against the published rollout
//! ([ADR-0048](../../../docs/adr/0048-ota-rollout-model.md)), fetches the bytes, and verifies the
//! signature against its own trusted keys before staging anything
//! ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)). Between the two: nothing. This
//! module is the cloud's half of the join — the small record that says *a release exists, for this
//! target, at this blob key*.
//!
//! # What lives where
//!
//! Postgres holds the row; the bytes live in the object store under [`artifact_key`]. That split is
//! ADR-0088's, and the reason is size: a 30 MB binary per release per target in the transactional
//! database would ride along in every WAL archive, for data that is immutable and content-addressable.
//! It is the opposite call from media ([`crate::media`]), whose renditions are capped at 150 KB and
//! are cheaper in `bytea` than behind a second service.
//!
//! # The cloud is not a trust boundary
//!
//! [`ReleaseArtifact::sha256`] is an integrity check against a truncated upload or a corrupted blob —
//! **not** a signature, and not what makes an artifact safe to install. Only the detached minisign
//! signature does that, and only the edge checks it. A compromised cloud, a swapped blob, or a
//! spoofed host can make an update *fail*; none of them can make a box install code. That property is
//! what makes hosting binaries an acceptable job for the cloud at all, so nothing here should ever
//! grow into a verification step.
//!
//! # Not tenant-scoped
//!
//! A release is fleet-wide: the same signed binary serves every tenant, and the record carries no
//! tenant data — a tag, a target triple, a digest, a size. So unlike almost every other table in the
//! cloud schema there is no `tenant_id` and no row-level security to scope; the trusted admin
//! connection is the only reader.

use core::fmt;
use core::future::Future;

use pos_ports::blob_store::BlobKey;
use pos_proto::Timestamp;

/// The longest release tag or target triple accepted into a key.
///
/// Generous for both — `v1.2.3-rc.4` and `aarch64-unknown-linux-gnu` are far inside it — and small
/// enough that a composed key stays well under [`BlobKey::MAX_LEN`].
const MAX_SEGMENT_LEN: usize = 64;

/// The binary every release ships, and the stem its signature is named from.
const ARTIFACT_STEM: &str = "pos-edge";

/// The prefix every release artifact sits under in the object store.
const RELEASES_PREFIX: &str = "releases";

// ---------------------------------------------------------------------------------------------
// The build target
// ---------------------------------------------------------------------------------------------

/// A Rust target triple — the build an artifact was compiled for, e.g. `x86_64-unknown-linux-gnu`.
///
/// Validated on construction rather than trusted, for the same reason [`BlobKey`] is: a triple
/// arrives as text and ends up in an object-store key. The accepted set is deliberately *narrower*
/// than a blob key's — lowercase letters, digits, `_` and `-`, and nothing else — so a triple cannot
/// introduce a path separator, a `.` , or a `..` even before the key is assembled.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetTriple(Box<str>);

/// Why a target triple was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetTripleError {
    /// A triple must name something.
    Empty,
    /// Longer than [`MAX_SEGMENT_LEN`].
    TooLong,
    /// Contained a byte outside `[a-z0-9_-]`.
    ForbiddenCharacter,
}

impl fmt::Display for TargetTripleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "a target triple must not be empty",
            Self::TooLong => "a target triple is at most 64 bytes",
            Self::ForbiddenCharacter => {
                "a target triple may contain only lowercase letters, digits, '_' and '-'"
            }
        })
    }
}

impl core::error::Error for TargetTripleError {}

impl TargetTriple {
    /// Validates and wraps a target triple.
    ///
    /// # Errors
    ///
    /// [`TargetTripleError`] if the triple is empty, longer than 64 bytes, or contains a character
    /// outside `[a-z0-9_-]`.
    pub fn parse(text: &str) -> Result<Self, TargetTripleError> {
        if text.is_empty() {
            return Err(TargetTripleError::Empty);
        }
        if text.len() > MAX_SEGMENT_LEN {
            return Err(TargetTripleError::TooLong);
        }
        if !text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        }) {
            return Err(TargetTripleError::ForbiddenCharacter);
        }
        Ok(Self(text.into()))
    }

    /// The triple as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TargetTriple {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------------------------
// The blob-key convention
// ---------------------------------------------------------------------------------------------

/// Which of a release's two blobs to name: the executable, or its detached signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// The `pos-edge` executable itself.
    Binary,
    /// The detached minisign signature the edge verifies before staging
    /// ([ADR-0047](../../../docs/adr/0047-minisign-verification.md)).
    Signature,
}

impl ArtifactKind {
    /// The file name this kind takes inside a release's directory.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Binary => ARTIFACT_STEM,
            Self::Signature => "pos-edge.minisig",
        }
    }
}

/// Why a release tag could not become part of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseTagError {
    /// A tag must name something.
    Empty,
    /// Longer than [`MAX_SEGMENT_LEN`].
    TooLong,
    /// Contained a byte outside `[A-Za-z0-9._-]`.
    ForbiddenCharacter,
    /// Was `.` or `..`, or contained `..` — a path component that could escape its prefix.
    ForbiddenPath,
}

impl fmt::Display for ReleaseTagError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "a release tag must not be empty",
            Self::TooLong => "a release tag is at most 64 bytes",
            Self::ForbiddenCharacter => {
                "a release tag may contain only letters, digits, '.', '_' and '-'"
            }
            Self::ForbiddenPath => "a release tag must not be '.', '..', or contain '..'",
        })
    }
}

impl core::error::Error for ReleaseTagError {}

/// Validates a release tag as one path segment.
///
/// [`ReleaseTag`](pos_proto::text::ReleaseTag) is deliberately unvalidated free text — it names a
/// version, and the domain has no business deciding what a version may be called. A key does: the tag
/// becomes a path segment, so it is checked here, at the boundary where it stops being a label and
/// starts being a location.
///
/// # Errors
///
/// [`ReleaseTagError`] if the tag is empty, longer than 64 bytes, contains a character outside
/// `[A-Za-z0-9._-]`, or is a traversal component.
pub fn validate_release_tag(tag: &str) -> Result<(), ReleaseTagError> {
    if tag.is_empty() {
        return Err(ReleaseTagError::Empty);
    }
    if tag.len() > MAX_SEGMENT_LEN {
        return Err(ReleaseTagError::TooLong);
    }
    if !tag
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ReleaseTagError::ForbiddenCharacter);
    }
    if tag == "." || tag == ".." || tag.contains("..") {
        return Err(ReleaseTagError::ForbiddenPath);
    }
    Ok(())
}

/// The object-store key one release artifact lives at: `releases/{tag}/{target}/pos-edge`, with the
/// signature beside it as `pos-edge.minisig`.
///
/// Both variable segments are validated before they are joined — the tag here, the target when its
/// [`TargetTriple`] was parsed — so the composed key cannot escape the `releases/` prefix. The final
/// [`BlobKey::parse`] is a third check of the same property, kept rather than skipped because it is
/// the one the object store itself relies on.
///
/// # Errors
///
/// [`ReleaseTagError`] if the tag cannot be a path segment. A valid tag and a parsed triple always
/// compose into a valid key, so the [`BlobKey::parse`] failure is unreachable and reported as
/// [`ReleaseTagError::ForbiddenCharacter`] rather than widening the error type for a case that cannot
/// occur.
pub fn artifact_key(
    tag: &str,
    target: &TargetTriple,
    kind: ArtifactKind,
) -> Result<BlobKey, ReleaseTagError> {
    validate_release_tag(tag)?;
    let key = format!(
        "{RELEASES_PREFIX}/{tag}/{target}/{file}",
        file = kind.file_name()
    );
    BlobKey::parse(&key).map_err(|_unreachable| ReleaseTagError::ForbiddenCharacter)
}

// ---------------------------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------------------------

/// One release artifact as recorded: which release, which target, how big, and the digest of the
/// bytes that were uploaded.
///
/// The blob keys are not stored — they are [`artifact_key`] of the tag and target, derived rather
/// than persisted so that a row and its blobs cannot disagree about where the bytes are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifact {
    /// The release, spelled the **same way** as the rollout's `target_version` and the binary's own
    /// [`version`](https://docs.rs/pos-edge) — bare, without the tag's leading `v` (e.g. `1.2.3`).
    ///
    /// One release, one name. R1b makes the workflow stamp `${TAG#v}` into the binary so a running
    /// store's version is comparable with a rollout's; a registry keyed by the `v`-prefixed tag would
    /// be a third spelling, and the only symptom of a disagreement is a `404` on the artifact route —
    /// which means "install nothing", so a fleet would sit at the old version with nothing saying why
    /// ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md) Amendment 2). No mapping function
    /// exists, deliberately: a mapping would be a fourth place for the spelling to drift.
    pub release: String,
    /// The target the binary was compiled for.
    pub target: TargetTriple,
    /// The executable's size in bytes.
    pub size_bytes: i64,
    /// The lowercase hex SHA-256 of the executable — an integrity check against a truncated upload,
    /// never a substitute for the minisign signature the edge verifies.
    pub sha256: String,
    /// When the artifact was recorded, stamped from the server clock.
    pub recorded_at: Timestamp,
}

/// What recording an artifact did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// The release/target was new; the row was written.
    Recorded,
    /// The release/target was already recorded with this exact digest, so the upload was a no-op.
    /// Re-running a release step is not an error.
    AlreadyRecorded,
}

/// A failure of the release registry.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseStoreError {
    /// The registry could not be reached.
    #[error("the release registry failed: {0}")]
    Unavailable(String),
    /// The release/target is already recorded with *different* bytes. A version that can change
    /// under a fleet is not a version ([ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)):
    /// an artifact a ring has already installed must keep meaning the same thing, or a rollback
    /// target stops being a known quantity.
    #[error("release {release} for {target} is already recorded with different bytes")]
    Immutable {
        /// The release tag that was re-uploaded.
        release: String,
        /// The target whose artifact differs.
        target: String,
    },
}

impl ReleaseStoreError {
    /// A registry failure carrying a human-readable reason, for the server's log.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable(message.into())
    }
}

/// Decides whether an incoming artifact may be recorded, given whatever digest is already stored for
/// that release and target.
///
/// The immutability rule lives here, as one pure function, rather than in the adapter's SQL — so the
/// registry cannot drift from the fake, and so the rule can be read without a database. Every
/// [`ReleaseStore`] implementation is expected to consult it.
///
/// # Errors
///
/// [`ReleaseStoreError::Immutable`] if the release/target is already recorded with a different
/// digest.
pub fn admit_artifact(
    stored_sha256: Option<&str>,
    incoming: &ReleaseArtifact,
) -> Result<RecordOutcome, ReleaseStoreError> {
    match stored_sha256 {
        None => Ok(RecordOutcome::Recorded),
        Some(stored) if stored == incoming.sha256 => Ok(RecordOutcome::AlreadyRecorded),
        Some(_different) => Err(ReleaseStoreError::Immutable {
            release: incoming.release.clone(),
            target: incoming.target.to_string(),
        }),
    }
}

/// Records and reads the releases the cloud hosts.
///
/// Deliberately three methods: the `/admin` upload records, the store-facing artifact route finds,
/// and the console lists. Nothing updates and nothing deletes — a release is immutable, and
/// collecting old artifacts is a retention concern ADR-0088 defers until there is more than one
/// release to collect.
pub trait ReleaseStore {
    /// Records one artifact, honouring [`admit_artifact`].
    ///
    /// # Errors
    ///
    /// [`ReleaseStoreError::Immutable`] if the release/target already carries different bytes, or
    /// [`ReleaseStoreError::Unavailable`] if the registry could not be written.
    fn record_artifact(
        &self,
        artifact: &ReleaseArtifact,
    ) -> impl Future<Output = Result<RecordOutcome, ReleaseStoreError>> + Send;

    /// The artifact recorded for `release` on `target`, or `None` if there is none.
    ///
    /// `None` is a fact the caller reasons about (it becomes the `404` that tells an edge to install
    /// nothing), not an error.
    ///
    /// # Errors
    ///
    /// [`ReleaseStoreError::Unavailable`] if the registry could not be read.
    fn find_artifact(
        &self,
        release: &str,
        target: &TargetTriple,
    ) -> impl Future<Output = Result<Option<ReleaseArtifact>, ReleaseStoreError>> + Send;

    /// Every artifact recorded for `release`, ordered by target.
    ///
    /// # Errors
    ///
    /// [`ReleaseStoreError::Unavailable`] if the registry could not be read.
    fn list_artifacts(
        &self,
        release: &str,
    ) -> impl Future<Output = Result<Vec<ReleaseArtifact>, ReleaseStoreError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactKind, RecordOutcome, ReleaseArtifact, ReleaseStoreError, ReleaseTagError,
        TargetTriple, TargetTripleError, admit_artifact, artifact_key, validate_release_tag,
    };
    use pos_proto::Timestamp;

    /// The two targets R1's release workflow actually cross-compiles.
    const SHIPPED_TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

    fn target(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    fn artifact(release: &str, sha256: &str) -> ReleaseArtifact {
        ReleaseArtifact {
            release: release.to_owned(),
            target: target("x86_64-unknown-linux-gnu"),
            size_bytes: 31_457_280,
            sha256: sha256.to_owned(),
            recorded_at: Timestamp::from_milliseconds_since_epoch(1_777_000_000_000)
                .expect("a representable instant"),
        }
    }

    #[test]
    fn both_targets_the_release_workflow_builds_are_accepted() {
        for text in SHIPPED_TARGETS {
            assert!(
                TargetTriple::parse(text).is_ok(),
                "{text} is a target this fleet ships"
            );
        }
    }

    #[test]
    fn a_target_triple_cannot_introduce_a_path_segment() {
        // The narrower character set is the whole point: a triple is joined into a key, and a `/`
        // or a `.` there would let the caller choose where the bytes are read from.
        for text in ["x86_64/../etc", "x86_64.unknown", "X86_64-linux", "a b"] {
            assert_eq!(
                TargetTriple::parse(text),
                Err(TargetTripleError::ForbiddenCharacter),
                "{text} should be refused"
            );
        }
        assert_eq!(TargetTriple::parse(""), Err(TargetTripleError::Empty));
        assert_eq!(
            TargetTriple::parse(&"a".repeat(65)),
            Err(TargetTripleError::TooLong)
        );
    }

    #[test]
    fn a_release_key_names_the_tag_and_the_target() {
        let key = artifact_key(
            "v1.2.3",
            &target("aarch64-unknown-linux-gnu"),
            ArtifactKind::Binary,
        )
        .expect("a key");
        assert_eq!(
            key.as_str(),
            "releases/v1.2.3/aarch64-unknown-linux-gnu/pos-edge"
        );
    }

    #[test]
    fn a_signature_sits_beside_its_binary() {
        let triple = target("x86_64-unknown-linux-gnu");
        let binary = artifact_key("v1.2.3", &triple, ArtifactKind::Binary).expect("a key");
        let signature = artifact_key("v1.2.3", &triple, ArtifactKind::Signature).expect("a key");
        assert_eq!(
            signature.as_str(),
            format!("{}.minisig", binary.as_str()),
            "the edge fetches the pair, so they must be one directory apart by name only"
        );
    }

    #[test]
    fn a_traversal_in_a_release_tag_never_reaches_a_key() {
        // `ReleaseTag` is free text by design, so this is the only place the check happens. A tag
        // that escaped `releases/` would let a caller read any object in the bucket.
        for tag in ["..", ".", "../../etc/passwd", "v1..2"] {
            let refused = artifact_key(
                tag,
                &target("x86_64-unknown-linux-gnu"),
                ArtifactKind::Binary,
            );
            assert!(refused.is_err(), "{tag} should never compose into a key");
        }
        assert_eq!(
            validate_release_tag(".."),
            Err(ReleaseTagError::ForbiddenPath)
        );
        assert_eq!(
            validate_release_tag("v1/2"),
            Err(ReleaseTagError::ForbiddenCharacter)
        );
        assert_eq!(validate_release_tag(""), Err(ReleaseTagError::Empty));
        assert_eq!(
            validate_release_tag(&"v".repeat(65)),
            Err(ReleaseTagError::TooLong)
        );
    }

    /// The spelling ADR-0088 Amendment 2 settled: a release is named the way `target_version` and the
    /// binary's own version name it — bare. Pinned as a test because the failure mode of getting it
    /// wrong is a `404` that reads as "nothing to install", which is silent by design.
    #[test]
    fn a_release_is_named_without_the_tags_leading_v() {
        assert_eq!(validate_release_tag("1.2.3"), Ok(()));
        let triple = TargetTriple::parse("x86_64-unknown-linux-gnu").expect("a triple");
        let key = artifact_key("1.2.3", &triple, ArtifactKind::Binary).expect("a key");
        assert_eq!(
            key.as_str(),
            "releases/1.2.3/x86_64-unknown-linux-gnu/pos-edge"
        );
    }

    #[test]
    fn ordinary_release_tags_compose() {
        for tag in ["v1.2.3", "v1.2.3-rc.4", "2026.09.01", "nightly_2026-09-01"] {
            assert!(
                artifact_key(
                    tag,
                    &target("x86_64-unknown-linux-gnu"),
                    ArtifactKind::Binary
                )
                .is_ok(),
                "{tag} is an ordinary tag"
            );
        }
    }

    #[test]
    fn a_release_target_that_is_new_is_recorded() {
        let outcome = admit_artifact(None, &artifact("v1.2.3", "abc123")).expect("recorded");
        assert_eq!(outcome, RecordOutcome::Recorded);
    }

    #[test]
    fn re_uploading_the_identical_artifact_is_a_no_op() {
        // Re-running a release step must not fail — only a *changed* artifact is a problem.
        let outcome =
            admit_artifact(Some("abc123"), &artifact("v1.2.3", "abc123")).expect("idempotent");
        assert_eq!(outcome, RecordOutcome::AlreadyRecorded);
    }

    #[test]
    fn re_uploading_a_tag_with_different_bytes_is_refused() {
        // The rule ADR-0088 exists to protect: a ring has already installed v1.2.3, so v1.2.3 must
        // keep meaning those bytes — otherwise the rollback target is no longer a known quantity.
        let refused = admit_artifact(Some("abc123"), &artifact("v1.2.3", "def456"))
            .expect_err("an immutable release");
        match refused {
            ReleaseStoreError::Immutable { release, target } => {
                assert_eq!(release, "v1.2.3");
                assert_eq!(target, "x86_64-unknown-linux-gnu");
            }
            other @ ReleaseStoreError::Unavailable(_) => {
                panic!("expected an immutability refusal, got {other:?}")
            }
        }
    }
}
