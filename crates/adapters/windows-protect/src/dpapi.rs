//! DPAPI in machine scope: `CryptProtectData` and `CryptUnprotectData` with
//! `CRYPTPROTECT_LOCAL_MACHINE`, never showing a prompt.

use std::ptr;

use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    CryptUnprotectData,
};

use crate::ProtectError;

/// The flags of every call: this machine's key, and never a prompt, since a service has no desktop.
const FLAGS: u32 = CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN;

/// Encrypts `plain` so that only this machine can decrypt it.
///
/// # Errors
///
/// [`ProtectError`] if `plain` is larger than DPAPI takes, or DPAPI refuses.
pub fn protect_for_machine(plain: &[u8]) -> Result<Vec<u8>, ProtectError> {
    let input = blob(plain, "CryptProtectData")?;
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: `input` points at `plain`, which outlives the call and which DPAPI only reads. The
    // description, entropy, reserved and prompt arguments are null, which DPAPI documents as
    // "none". `output` is a valid place for DPAPI to write the buffer it allocates.
    let ok = unsafe {
        CryptProtectData(
            &raw const input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            FLAGS,
            &raw mut output,
        )
    };
    if ok == 0 {
        return Err(last_error("CryptProtectData"));
    }
    Ok(take(output, false))
}

/// Decrypts what [`protect_for_machine`] encrypted on this machine.
///
/// # Errors
///
/// [`ProtectError`] if `protected` was not encrypted on this machine, is damaged, or DPAPI refuses.
pub fn unprotect(protected: &[u8]) -> Result<Vec<u8>, ProtectError> {
    let input = blob(protected, "CryptUnprotectData")?;
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: as in `protect_for_machine`; the description out-parameter is null, so DPAPI
    // allocates no description for us to free.
    let ok = unsafe {
        CryptUnprotectData(
            &raw const input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            FLAGS,
            &raw mut output,
        )
    };
    if ok == 0 {
        return Err(last_error("CryptUnprotectData"));
    }
    Ok(take(output, true))
}

/// A read-only view of `bytes` in DPAPI's blob shape.
fn blob(bytes: &[u8], call: &'static str) -> Result<CRYPT_INTEGER_BLOB, ProtectError> {
    // ERROR_INVALID_PARAMETER: DPAPI's sizes are 32-bit.
    let len = u32::try_from(bytes.len()).map_err(|_| ProtectError::new(call, 87))?;
    // DPAPI never writes through the input blob; the pointer is `*mut` only in its declaration.
    Ok(CRYPT_INTEGER_BLOB {
        cbData: len,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

/// Copies DPAPI's output out and frees it, wiping it first when it is a plaintext.
fn take(blob: CRYPT_INTEGER_BLOB, wipe: bool) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    let len = blob.cbData as usize;
    // SAFETY: on success DPAPI set `pbData` to a buffer of `cbData` bytes it allocated with
    // `LocalAlloc`, which nothing else refers to.
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, len) }.to_vec();
    if wipe {
        // SAFETY: the same buffer, `len` bytes long, still ours until the `LocalFree` below.
        unsafe { ptr::write_bytes(blob.pbData, 0, len) };
    }
    // SAFETY: `pbData` came from DPAPI's `LocalAlloc` and is freed exactly once, here. The return
    // value is null on success and the buffer is not used again either way.
    let _ = unsafe { LocalFree(blob.pbData.cast()) };
    bytes
}

fn last_error(call: &'static str) -> ProtectError {
    // SAFETY: reads the calling thread's last-error value; no memory is involved.
    ProtectError::new(call, unsafe { GetLastError() })
}

#[cfg(test)]
mod tests {
    use super::{protect_for_machine, unprotect};

    #[test]
    fn what_this_machine_protects_it_unprotects() {
        let key = [7_u8; 32];
        let sealed = protect_for_machine(&key).expect("protect");
        assert_ne!(&sealed[..], &key[..], "the protected form is not the key");
        assert_eq!(unprotect(&sealed).expect("unprotect"), key.to_vec());
    }

    #[test]
    fn damaged_bytes_do_not_unprotect() {
        let mut sealed = protect_for_machine(b"vault key").expect("protect");
        if let Some(byte) = sealed.last_mut() {
            *byte ^= 0xFF;
        }
        assert!(unprotect(&sealed).is_err());
    }
}
