// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The sealed store archive: what a copy of a shop's database looks like once it leaves the shop
//! ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
//!
//! # Why the till seals it, and not the cloud
//!
//! A store's database is the one place at a till where personal data sits: `subjects` holds the
//! buyer's name, tax code and address that a B2B tax invoice needs
//! ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)). Nothing publishes that to the
//! cloud — the event log carries a `SubjectId` and `pos_proto::pii` makes a name in a payload a
//! compile error — so shipping the database off the box is the first time those details would
//! leave the shop at all. Sealing here means they leave as bytes nobody downstream can read: not
//! the relay that carries them, not the object store that keeps them, and not the off-box archive
//! tier that ADR-0046 syncs them to.
//!
//! # The format
//!
//! ```text
//! "P4PSTORE"   8 bytes   magic
//! 0x01         1 byte    format version
//! nonce       24 bytes   XChaCha20-Poly1305, fresh from the OS CSPRNG for every archive
//! body         n bytes   XChaCha20-Poly1305(deflate(snapshot)), 16-byte tag included
//! ```
//!
//! The magic and version are also the **associated data**, together with the store's id. So an
//! archive is cryptographically bound to the store it came from: presenting one shop's archive as
//! another's fails to open rather than restoring the wrong shop's trading onto a till.
//!
//! Deflate first, then seal — sealed bytes do not compress, and a store database is mostly text and
//! repeated page structure. The order is safe here because an archive is a whole database written
//! once, not a channel mixing attacker-chosen text with a secret.
//!
//! # Memory
//!
//! A till is a small machine, and the database is the big thing here, so neither direction holds
//! it whole. Sealing streams the snapshot file through the compressor into the one buffer that is
//! then sealed in place: the peak is the *archive*, not the database plus the archive. Opening
//! streams the inflated database out to a file, so its peak is the archive plus its own plaintext
//! — twice the archive, and still independent of how large the database is.
//!
//! The cap is therefore on the **archive**, the same quantity on both sides, so that anything this
//! module can seal it can also open. Past [`MAX_ARCHIVE_BYTES`] the answer is a refusal that names
//! the size, because a store that quietly runs out of memory at 04:00 is worse than one that says
//! its database has outgrown a single-object backup.

use std::io::{Read, Write};
use std::path::Path;

use chacha20poly1305::aead::{AeadInOut, Buffer, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

use pos_proto::ids::StoreId;

/// The first eight bytes of every archive.
const MAGIC: &[u8; 8] = b"P4PSTORE";

/// The format this module writes. A reader refuses anything else rather than guessing.
const FORMAT_VERSION: u8 = 1;

/// Bytes of nonce — XChaCha20-Poly1305's extended nonce, which is what makes a random nonce per
/// archive safe with no counter to keep.
const NONCE_LEN: usize = 24;

/// Where the sealed body starts.
const HEADER_LEN: usize = MAGIC.len() + 1 + NONCE_LEN;

/// The largest **archive** — sealed and compressed — this build will write or read.
///
/// `BlobStore` is explicit that an object must fit in memory on both sides
/// (`pos_ports::blob_store`), and `docs/architecture.md` §8 sizes a store backup in tens of
/// megabytes. This is the point at which that stops being true, and the refusal is deliberate: it
/// turns "the till fell over" into "this store has outgrown a whole-database backup", which is an
/// operator's decision to make rather than a crash to diagnose.
///
/// It bounds the sealed bytes and not the database inside them, and the same quantity on both
/// sides, so that anything this module can seal it can also open.
pub const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

/// How much is read from the snapshot at a time on the way into the compressor.
const CHUNK: usize = 64 * 1024;

/// The key an archive is sealed with: 32 bytes, held by the operator
/// ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
///
/// Zeroed on drop, and its [`Debug`] prints nothing but the type name, so a key cannot reach a log
/// line by accident — every other secret at the edge is handled the same way.
#[derive(Clone)]
pub struct ArchiveKey([u8; 32]);

impl ArchiveKey {
    /// How many characters the text form has.
    pub const TEXT_LEN: usize = 64;

    /// Mints a fresh key from the OS CSPRNG.
    ///
    /// # Errors
    ///
    /// [`getrandom::Error`] if the OS entropy source is unavailable — a key is never faked.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// Reads a key from its 64-character lowercase-hex text form.
    ///
    /// Uppercase is accepted too: an operator retyping a key from a printout should not be refused
    /// over the shift key.
    ///
    /// # Errors
    ///
    /// [`ArchiveError::KeyText`] if the text is not exactly 64 hexadecimal characters.
    pub fn parse(text: &str) -> Result<Self, ArchiveError> {
        let text = text.trim();
        if text.len() != Self::TEXT_LEN {
            return Err(ArchiveError::KeyText);
        }
        let mut bytes = [0_u8; 32];
        for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let high = hex_digit(pair[0]).ok_or(ArchiveError::KeyText)?;
            let low = hex_digit(pair[1]).ok_or(ArchiveError::KeyText)?;
            *slot = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    /// The key as 64 lowercase hexadecimal characters — the form a console shows and an operator
    /// keeps.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = String::with_capacity(Self::TEXT_LEN);
        for byte in self.0 {
            // `write!` to a String cannot fail, and the alternative here is a panic path in a
            // function that has no other one.
            let _ = core::fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"));
        }
        text
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }
}

impl Drop for ArchiveKey {
    fn drop(&mut self) {
        // Not a substitute for the cipher's own zeroizing of its round keys — this is the copy we
        // hold, and it is the one a core dump would otherwise carry. `zeroize` rather than
        // `fill(0)` because a plain write to memory nothing reads again is exactly what dead-store
        // elimination removes.
        self.0.zeroize();
    }
}

impl core::fmt::Debug for ArchiveKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ArchiveKey(…)")
    }
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Why an archive could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// A key is 64 hexadecimal characters.
    #[error("an archive key is 64 hexadecimal characters")]
    KeyText,
    /// The OS entropy source was unavailable, so no nonce could be drawn.
    #[error("the OS entropy source is unavailable, so no archive nonce could be drawn")]
    Entropy(#[source] getrandom::Error),
    /// Reading the snapshot, or writing the restored database, failed.
    #[error("{context}")]
    Io {
        /// What was being attempted.
        context: &'static str,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The archive would be, or turned out to be, larger than [`MAX_ARCHIVE_BYTES`].
    #[error(
        "this store's archive is over {MAX_ARCHIVE_BYTES} bytes, which is more than one object"
    )]
    TooLarge,
    /// The bytes are not an archive this build writes.
    #[error("this is not a store archive, or not one this version can read")]
    NotAnArchive,
    /// The key is wrong, the archive is for another store, or the bytes were altered.
    ///
    /// Deliberately one variant: an authenticated cipher cannot tell these apart, and a message
    /// that guessed would be a message that misleads.
    #[error(
        "this archive did not open: the key is wrong, it belongs to another store, or the bytes \
         were altered"
    )]
    Unsealable,
}

/// Seals an in-memory snapshot for `store`.
///
/// # Errors
///
/// [`ArchiveError`] if entropy is unavailable, the compressed archive exceeds
/// [`MAX_ARCHIVE_BYTES`], or compression fails.
pub fn seal(snapshot: &[u8], key: &ArchiveKey, store: StoreId) -> Result<Vec<u8>, ArchiveError> {
    seal_reader(snapshot, key, store)
}

/// Seals the snapshot file at `path` for `store`, streaming it through the compressor so the whole
/// database is never held in memory at once.
///
/// # Errors
///
/// [`ArchiveError`] if the file cannot be read, entropy is unavailable, or the compressed archive
/// exceeds [`MAX_ARCHIVE_BYTES`].
pub fn seal_file(path: &Path, key: &ArchiveKey, store: StoreId) -> Result<Vec<u8>, ArchiveError> {
    let file = std::fs::File::open(path).map_err(|source| ArchiveError::Io {
        context: "the store snapshot could not be opened to seal it",
        source,
    })?;
    seal_reader(file, key, store)
}

fn seal_reader<R: Read>(
    mut snapshot: R,
    key: &ArchiveKey,
    store: StoreId,
) -> Result<Vec<u8>, ArchiveError> {
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(ArchiveError::Entropy)?;

    // The header is written first and then the body is sealed *after* it in the same buffer, so
    // there is one allocation for the archive and no copy at the end.
    let mut archive = Vec::with_capacity(HEADER_LEN + CHUNK);
    archive.extend_from_slice(MAGIC);
    archive.push(FORMAT_VERSION);
    archive.extend_from_slice(&nonce);

    {
        let mut deflate =
            flate2::write::DeflateEncoder::new(&mut archive, flate2::Compression::default());
        let mut buffer = vec![0_u8; CHUNK];
        loop {
            let read = snapshot
                .read(&mut buffer)
                .map_err(|source| ArchiveError::Io {
                    context: "the store snapshot could not be read",
                    source,
                })?;
            if read == 0 {
                break;
            }
            deflate
                .write_all(buffer.get(..read).ok_or(ArchiveError::TooLarge)?)
                .map_err(|source| ArchiveError::Io {
                    context: "the store snapshot could not be compressed",
                    source,
                })?;
            if deflate.total_out() > MAX_ARCHIVE_BYTES as u64 {
                return Err(ArchiveError::TooLarge);
            }
        }
        deflate.finish().map_err(|source| ArchiveError::Io {
            context: "the store snapshot could not be compressed",
            source,
        })?;
    }

    // Checked before the seal rather than after it: `finish` can emit a last block past the
    // in-loop check, and there is no reason to encrypt bytes that are about to be refused.
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(ArchiveError::TooLarge);
    }

    let mut body = Tail {
        buffer: &mut archive,
        start: HEADER_LEN,
    };
    key.cipher()
        .encrypt_in_place(&XNonce::from(nonce), &associated_data(store), &mut body)
        .map_err(|_error| ArchiveError::TooLarge)?;

    Ok(archive)
}

/// Opens an archive sealed for `store`, returning the snapshot bytes.
///
/// # Errors
///
/// [`ArchiveError`] if the bytes are not an archive, the key or store is wrong, or the archive was
/// altered.
pub fn open(archive: &[u8], key: &ArchiveKey, store: StoreId) -> Result<Vec<u8>, ArchiveError> {
    let mut snapshot = Vec::new();
    open_into(archive, key, store, &mut snapshot)?;
    Ok(snapshot)
}

/// Opens an archive sealed for `store` and writes the database it holds to `destination`.
///
/// The destination is written only once the archive has been authenticated, so a wrong key leaves
/// no half-restored file behind.
///
/// # Errors
///
/// [`ArchiveError`] if the archive does not open or the destination cannot be written.
pub fn open_file(
    archive: &[u8],
    key: &ArchiveKey,
    store: StoreId,
    destination: &Path,
) -> Result<u64, ArchiveError> {
    let mut file = std::fs::File::create_new(destination).map_err(|source| ArchiveError::Io {
        context: "the restored database could not be created (a file is already there)",
        source,
    })?;
    let written = open_into(archive, key, store, &mut file);
    if written.is_err() {
        // A refused archive must not leave a stub behind that looks like a restore.
        drop(file);
        let _ = std::fs::remove_file(destination);
    }
    written
}

fn open_into<W: Write>(
    archive: &[u8],
    key: &ArchiveKey,
    store: StoreId,
    destination: &mut W,
) -> Result<u64, ArchiveError> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(ArchiveError::TooLarge);
    }
    let header = archive
        .get(..HEADER_LEN)
        .ok_or(ArchiveError::NotAnArchive)?;
    if header.get(..MAGIC.len()) != Some(MAGIC.as_slice())
        || header.get(MAGIC.len()) != Some(&FORMAT_VERSION)
    {
        return Err(ArchiveError::NotAnArchive);
    }
    let nonce = XNonce::try_from(
        header
            .get(MAGIC.len() + 1..)
            .ok_or(ArchiveError::NotAnArchive)?,
    )
    .map_err(|_error| ArchiveError::NotAnArchive)?;
    let mut body = archive
        .get(HEADER_LEN..)
        .ok_or(ArchiveError::NotAnArchive)?
        .to_vec();

    key.cipher()
        .decrypt_in_place(&nonce, &associated_data(store), &mut body)
        .map_err(|_error| ArchiveError::Unsealable)?;

    let mut inflate = flate2::read::DeflateDecoder::new(body.as_slice());
    let mut buffer = vec![0_u8; CHUNK];
    let mut total: u64 = 0;
    loop {
        let read = inflate
            .read(&mut buffer)
            .map_err(|_error| ArchiveError::Unsealable)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        destination
            .write_all(buffer.get(..read).ok_or(ArchiveError::TooLarge)?)
            .map_err(|source| ArchiveError::Io {
                context: "the restored database could not be written",
                source,
            })?;
    }
    destination.flush().map_err(|source| ArchiveError::Io {
        context: "the restored database could not be written",
        source,
    })?;
    Ok(total)
}

/// What the seal is bound to as well as the key: the format, and the store the archive came from.
fn associated_data(store: StoreId) -> Vec<u8> {
    let mut data = Vec::with_capacity(MAGIC.len() + 1 + 26);
    data.extend_from_slice(MAGIC);
    data.push(FORMAT_VERSION);
    data.extend_from_slice(store.to_string().as_bytes());
    data
}

/// A view of a `Vec` from `start` onwards, so the AEAD seals the body in place without moving the
/// header out of the way first.
struct Tail<'a> {
    buffer: &'a mut Vec<u8>,
    start: usize,
}

impl AsRef<[u8]> for Tail<'_> {
    fn as_ref(&self) -> &[u8] {
        self.buffer.get(self.start..).unwrap_or_default()
    }
}

impl AsMut<[u8]> for Tail<'_> {
    fn as_mut(&mut self) -> &mut [u8] {
        let start = self.start;
        self.buffer.get_mut(start..).unwrap_or_default()
    }
}

impl Buffer for Tail<'_> {
    fn extend_from_slice(&mut self, other: &[u8]) -> chacha20poly1305::aead::Result<()> {
        self.buffer.extend_from_slice(other);
        Ok(())
    }

    fn truncate(&mut self, len: usize) {
        self.buffer.truncate(self.start.saturating_add(len));
    }
}
