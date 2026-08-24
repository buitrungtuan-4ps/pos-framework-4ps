// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Large objects: backups and OTA artifacts.
//!
//! # Deliberately thin, and deliberately temporary
//!
//! [ADR-0021](../../../docs/adr/0021-corrected-port-list.md) says not to invest in this
//! abstraction, and [ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md) says why:
//! object storage exists in this system **only** to satisfy Litestream. Once WAL shipping
//! is in-house, Garage and MinIO disappear and this port is deleted outright. So the API
//! is four methods over byte slices, and any richer shape — multipart uploads, streaming,
//! presigned URLs, lifecycle rules — would be work thrown away.
//!
//! The consequence to accept: an object must fit in memory on both sides. Every current
//! caller does. `docs/architecture.md` §8 sizes store backups and OTA artifacts in tens of
//! megabytes, and the one genuinely large object — a continuously shipped WAL — is exactly
//! the case that removes the port.

use core::fmt;

use core::future::Future;

use crate::error::PortError;

/// A key in the object store.
///
/// Validated on construction rather than trusted, because a key built from a tenant slug
/// and a filename is a path-traversal vector, and an object store's flat namespace makes
/// `../` look like an ordinary character until something maps keys onto a filesystem.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobKey(Box<str>);

/// Why a key was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKeyError {
    /// A key must name something.
    Empty,
    /// Longer than [`BlobKey::MAX_LEN`].
    TooLong,
    /// Contained a byte outside the permitted set.
    ForbiddenCharacter,
    /// Contained `..`, a leading `/`, or an empty segment.
    ForbiddenPath,
}

impl fmt::Display for BlobKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "a blob key must not be empty",
            Self::TooLong => "a blob key is at most 1024 bytes",
            Self::ForbiddenCharacter => {
                "a blob key may contain only ASCII letters, digits, '-', '_', '.' and '/'"
            }
            Self::ForbiddenPath => {
                "a blob key must not contain '..', a leading '/', or an empty segment"
            }
        })
    }
}

impl core::error::Error for BlobKeyError {}

impl BlobKey {
    /// The longest key any supported backend accepts.
    pub const MAX_LEN: usize = 1024;

    /// Validates and wraps a key.
    ///
    /// # Errors
    ///
    /// [`BlobKeyError`] if the key is empty, too long, contains a character outside
    /// `[A-Za-z0-9._/-]`, or contains a path component that could escape its prefix.
    pub fn parse(key: &str) -> Result<Self, BlobKeyError> {
        if key.is_empty() {
            return Err(BlobKeyError::Empty);
        }
        if key.len() > Self::MAX_LEN {
            return Err(BlobKeyError::TooLong);
        }
        if !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
        {
            return Err(BlobKeyError::ForbiddenCharacter);
        }
        if key.starts_with('/')
            || key
                .split('/')
                .any(|segment| segment.is_empty() || segment == "..")
        {
            return Err(BlobKeyError::ForbiddenPath);
        }
        Ok(Self(key.into()))
    }

    /// The key as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this key sits under `prefix`.
    ///
    /// Segment-aware: `stores/1` is a prefix of `stores/1/backup` but not of
    /// `stores/10/backup`. A plain `starts_with` would list one tenant's objects under
    /// another's prefix, which in a multi-tenant system is the failure that matters most.
    ///
    /// A prefix therefore never needs a trailing `/` — and cannot have one, since
    /// [`Self::parse`] refuses an empty final segment.
    #[must_use]
    pub fn is_under(&self, prefix: &Self) -> bool {
        match self.as_str().strip_prefix(prefix.as_str()) {
            None => false,
            Some("") => true,
            Some(rest) => rest.starts_with('/'),
        }
    }
}

impl fmt::Display for BlobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for BlobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobKey({})", self.0)
    }
}

/// Stores and retrieves whole objects.
///
/// # Contract
///
/// 1. **`put` is idempotent and last-write-wins.** Writing the same key twice leaves the
///    second body, and neither call errors.
/// 2. **`get` of an absent key is `Ok(None)`**, not [`PortError::not_found`]. A missing
///    backup is a fact the caller reasons about, not an exception — and the restore drill
///    in `docs/roadmap.md` P8 depends on being able to ask without handling an error.
/// 3. **`delete` of an absent key succeeds.** Cleanup runs more than once.
/// 4. **`list` is prefix-scoped and segment-aware**, so a prefix cannot leak into a
///    sibling whose name merely starts the same way.
pub trait BlobStore: Send + Sync {
    /// Writes an object.
    ///
    /// # Errors
    ///
    /// [`PortError::resource_exhausted`] if the backend is out of space,
    /// [`PortError::invalid_argument`] if the body exceeds the backend's object limit, or
    /// [`PortError::unavailable`] if the backend cannot be reached.
    fn put(&self, key: &BlobKey, body: &[u8])
    -> impl Future<Output = Result<(), PortError>> + Send;

    /// Reads an object, or `None` if it is not there.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the backend cannot be reached, or
    /// [`PortError::internal`] if the object is present but unreadable.
    fn get(&self, key: &BlobKey)
    -> impl Future<Output = Result<Option<Vec<u8>>, PortError>> + Send;

    /// Removes an object, succeeding whether or not it existed.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the backend cannot be reached.
    fn delete(&self, key: &BlobKey) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Every key under a prefix, ascending.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the backend cannot be reached.
    fn list(
        &self,
        prefix: &BlobKey,
    ) -> impl Future<Output = Result<Vec<BlobKey>, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{BlobKey, BlobKeyError};

    fn key(text: &str) -> BlobKey {
        BlobKey::parse(text).expect("valid")
    }

    #[test]
    fn ordinary_keys_parse() {
        for text in [
            "stores/01J000000000000000000000/backup.sqlite",
            "releases/v1.2.3/pos_edge-x86_64-pc-windows-msvc.exe",
            "a",
        ] {
            assert!(BlobKey::parse(text).is_ok(), "{text} should parse");
        }
    }

    #[test]
    fn traversal_and_absolute_keys_are_refused() {
        // The point of validating at all: an object store's namespace is flat, so `..`
        // stays inert until something maps a key onto a filesystem, and then it is a
        // vulnerability that was introduced years earlier.
        for text in [
            "../secrets",
            "stores/../other/backup",
            "/stores/x",
            "stores//x",
        ] {
            assert_eq!(
                BlobKey::parse(text),
                Err(BlobKeyError::ForbiddenPath),
                "{text} should be refused"
            );
        }
    }

    #[test]
    fn unusual_characters_and_sizes_are_refused() {
        assert_eq!(BlobKey::parse(""), Err(BlobKeyError::Empty));
        assert_eq!(BlobKey::parse("a b"), Err(BlobKeyError::ForbiddenCharacter));
        assert_eq!(
            BlobKey::parse("bún-chả"),
            Err(BlobKeyError::ForbiddenCharacter)
        );
        let long = "a".repeat(BlobKey::MAX_LEN + 1);
        assert_eq!(BlobKey::parse(&long), Err(BlobKeyError::TooLong));
    }

    #[test]
    fn a_prefix_stops_at_a_segment_boundary() {
        // The multi-tenant failure this prevents: listing `stores/1` must not return
        // `stores/10`'s objects.
        let one = key("stores/1");
        assert!(key("stores/1").is_under(&one));
        assert!(key("stores/1/backup.sqlite").is_under(&one));
        assert!(
            !key("stores/10/backup.sqlite").is_under(&one),
            "10 is not under 1"
        );
        assert!(!key("stores").is_under(&one));

        // And a prefix cannot be written with a trailing slash, so there is no second
        // spelling of the same prefix to keep consistent.
        assert_eq!(
            BlobKey::parse("stores/1/"),
            Err(BlobKeyError::ForbiddenPath)
        );
    }
}
