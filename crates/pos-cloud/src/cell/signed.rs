// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The directory this cell answers from, and the minisign check every new one passes first
//! ([ADR-0179](../../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C2).
//!
//! The directory is signed with its own key, held by the owner or a named delegate and never the
//! release key. This cell holds only the public half, from `[cell] directory_public_key`. A document
//! is accepted only when its signature verifies against that key, its content checks out, and its
//! version is higher than the one held. Anything else is refused with a message saying why, and the
//! directory already held stays: a bad file never takes a cell's answers away.
//!
//! Three modes, so an operator can tell them apart:
//!
//! - **self**: no directory key is configured. This is level 0: the cell is the only one, and every
//!   tenant is here.
//! - **missing**: a key is configured but no directory has been accepted yet. The cell knows only
//!   itself and answers for no tenant, rather than claiming every tenant of a fleet it cannot see.
//! - **signed**: a verified directory is held.

use std::sync::{Arc, RwLock};

use base64::Engine as _;
use pos_ports::signer::{KeyId, PublicKey, Signature, Signer as _};
use updater_minisign::MinisignVerifier;

use super::Cell;
use super::directory::Directory;
use crate::release_source::decode_minisig_file;

/// Bytes in a minisign public key blob: `algorithm(2) ∥ key_id(8) ∥ ed25519_key(32)`.
const PUBLIC_KEY_BLOB_LEN: usize = 42;

/// Which kind of answer the cell gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryMode {
    /// No directory key configured: the only cell, home to every tenant.
    SelfOnly,
    /// A key is configured and nothing has been accepted yet: this cell knows only itself.
    Missing,
    /// A verified directory is held.
    Signed,
}

impl DirectoryMode {
    /// The mode as the console reads it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SelfOnly => "self",
            Self::Missing => "missing",
            Self::Signed => "signed",
        }
    }
}

/// What was offered, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// The directory was accepted at this version.
    Accepted(u64),
    /// The directory held already has this version; nothing changed.
    Unchanged(u64),
}

/// Parses `[cell] directory_public_key`: the second line of a `minisign.pub` file, verbatim.
///
/// # Errors
///
/// A message saying what is wrong with it.
pub fn parse_public_key(line: &str) -> Result<PublicKey, String> {
    let fail = |why: &str| {
        Err(format!(
            "[cell] directory_public_key is not a minisign public key: {why}. It is the second \
             line of the directory key's minisign.pub, verbatim (ADR-0179)"
        ))
    };
    let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(line.trim()) else {
        return fail("it is not valid base64");
    };
    if blob.len() != PUBLIC_KEY_BLOB_LEN {
        return fail("it does not decode to 42 bytes");
    }
    if blob.get(0..2) != Some(b"Ed".as_slice()) {
        return fail("its algorithm is not minisign's Ed25519 `Ed`");
    }
    let (Some(id), Some(key)) = (blob.get(2..10), blob.get(10..PUBLIC_KEY_BLOB_LEN)) else {
        return fail("it is too short");
    };
    let Ok(id) = <[u8; 8]>::try_from(id) else {
        return fail("it has no key id");
    };
    Ok(PublicKey::new(KeyId::new(id), key.to_vec()))
}

/// The directory this cell answers from: the last good one, and why the last offer was refused.
#[derive(Debug)]
pub struct CellDirectory {
    cell: Cell,
    key: Option<PublicKey>,
    held: RwLock<Option<Arc<Directory>>>,
    last_refusal: RwLock<Option<String>>,
}

impl CellDirectory {
    /// A cell with no directory key: level 0, self mode.
    #[must_use]
    pub fn single(cell: Cell) -> Self {
        Self::new(cell, None)
    }

    /// A cell that trusts `key` for its directory, holding none yet.
    #[must_use]
    pub fn trusting(cell: Cell, key: PublicKey) -> Self {
        Self::new(cell, Some(key))
    }

    fn new(cell: Cell, key: Option<PublicKey>) -> Self {
        Self {
            cell,
            key,
            held: RwLock::new(None),
            last_refusal: RwLock::new(None),
        }
    }

    /// Which cell this is.
    #[must_use]
    pub fn cell(&self) -> &Cell {
        &self.cell
    }

    /// Which kind of answer the cell gives now.
    #[must_use]
    pub fn mode(&self) -> DirectoryMode {
        match (&self.key, self.held().is_some()) {
            (None, _) => DirectoryMode::SelfOnly,
            (Some(_), false) => DirectoryMode::Missing,
            (Some(_), true) => DirectoryMode::Signed,
        }
    }

    /// The verified directory held, if any.
    #[must_use]
    pub fn held(&self) -> Option<Arc<Directory>> {
        self.held.read().ok().and_then(|held| held.clone())
    }

    /// Why the last offer was refused, until one is accepted.
    #[must_use]
    pub fn last_refusal(&self) -> Option<String> {
        self.last_refusal.read().ok().and_then(|why| why.clone())
    }

    /// The cell a tenant is homed in: listed in the signed directory; or, in self mode, this cell.
    /// `None` when no held directory lists it.
    #[must_use]
    pub fn tenant_cell(&self, tenant_slug: &str) -> Option<String> {
        match self.mode() {
            DirectoryMode::SelfOnly => Some(self.cell.cell_id.clone()),
            DirectoryMode::Missing => None,
            DirectoryMode::Signed => self
                .held()
                .and_then(|held| held.tenant_cell(tenant_slug).map(str::to_owned)),
        }
    }

    /// Offers a directory document and its `.minisig` file. The held directory changes only when
    /// the offer verifies, checks out and is newer.
    ///
    /// # Errors
    ///
    /// Why it was refused; the same text is kept for [`Self::last_refusal`].
    pub fn offer(&self, document: &[u8], minisig_file: &[u8]) -> Result<Offer, String> {
        let outcome = self.check(document, minisig_file);
        self.set_refusal(outcome.as_ref().err().cloned());
        outcome
    }

    /// Records that no directory could be offered at all (the file could not be read), as a
    /// refusal the console shows. The directory held is kept.
    pub fn record_refusal(&self, why: String) {
        self.set_refusal(Some(why));
    }

    fn set_refusal(&self, why: Option<String>) {
        if let Ok(mut slot) = self.last_refusal.write() {
            *slot = why;
        }
    }

    fn check(&self, document: &[u8], minisig_file: &[u8]) -> Result<Offer, String> {
        let Some(key) = &self.key else {
            return Err(
                "this cell has no [cell] directory_public_key, so it trusts no directory"
                    .to_owned(),
            );
        };
        let signature = decode_minisig_file(minisig_file)
            .map_err(|why| format!("the directory's signature file {why}"))?;
        MinisignVerifier::new()
            .verify(document, &Signature::new(signature), key)
            .map_err(|_| {
                "the directory's signature does not verify against [cell] directory_public_key; \
                 the directory held is kept"
                    .to_owned()
            })?;
        let directory = Directory::parse(document, &self.cell)?;
        let version = directory.directory_version;
        let mut held = self
            .held
            .write()
            .map_err(|_| "the directory lock is poisoned".to_owned())?;
        match held.as_ref().map(|current| current.directory_version) {
            Some(current) if version == current => Ok(Offer::Unchanged(version)),
            Some(current) if version < current => Err(format!(
                "the directory's version {version} is older than the {current} this cell holds; \
                 the directory held is kept"
            )),
            _ => {
                *held = Some(Arc::new(directory));
                Ok(Offer::Accepted(version))
            }
        }
    }
}

#[cfg(test)]
mod tests;
