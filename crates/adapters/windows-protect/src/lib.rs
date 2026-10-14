//! Windows' own protection for a store box's state
//! ([ADR-0174](../../../docs/adr/0174-a-windows-store-keeps-its-state-to-its-own-service.md)).
//!
//! **DPAPI, in machine scope.** [`protect_for_machine`] encrypts bytes so that only this machine
//! can decrypt them, and [`unprotect`] decrypts them again. The edge's vault key is kept this way
//! on Windows (`key-vault-keyring`): a copied disk or backup cannot open it, and the `data\`
//! directory's ACL keeps every other local account from reading the file it is in.
//!
//! **The one place FFI is allowed.** This crate is the adapter the workspace lint table reserves
//! for it: `unsafe_code` is lifted here, on Windows only, and nowhere else. Every `unsafe` block is
//! as small as the call it makes and says why it is sound. On every other platform the crate is
//! empty but for [`ProtectError`], so a Linux build compiles none of it.

#![cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "FFI to DPAPI (CryptProtectData, CryptUnprotectData), ADR-0174"
    )
)]
#![deny(clippy::undocumented_unsafe_blocks)]

use std::fmt;

#[cfg(windows)]
mod dpapi;

#[cfg(windows)]
pub use dpapi::{protect_for_machine, unprotect};

/// Why DPAPI refused, as the call and the Windows error code. Never carries the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectError {
    call: &'static str,
    code: u32,
}

impl ProtectError {
    /// A failure of `call`, with the code `GetLastError` reported.
    #[must_use]
    pub const fn new(call: &'static str, code: u32) -> Self {
        Self { call, code }
    }

    /// The Windows error code.
    #[must_use]
    pub const fn code(&self) -> u32 {
        self.code
    }
}

impl fmt::Display for ProtectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed with Windows error {}", self.call, self.code)
    }
}

impl std::error::Error for ProtectError {}

#[cfg(test)]
mod tests {
    use super::ProtectError;

    #[test]
    fn an_error_names_the_call_and_the_code_and_nothing_else() {
        let error = ProtectError::new("CryptUnprotectData", 13);
        assert_eq!(
            error.to_string(),
            "CryptUnprotectData failed with Windows error 13"
        );
        assert_eq!(error.code(), 13);
    }
}
