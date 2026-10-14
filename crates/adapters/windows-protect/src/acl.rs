//! Reading a directory's access-control list, and the account this process runs as.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_ALL, GENERIC_READ, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetTokenInformation,
    INHERIT_ONLY_ACE, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::ProtectError;
use crate::access::{Entry, Readers, readers};

/// `ACCESS_ALLOWED_ACE_TYPE`, from `winnt.h`.
const ACCESS_ALLOWED: u8 = 0;
/// `FILE_READ_DATA` (`FILE_LIST_DIRECTORY` on a directory), from `winnt.h`.
const FILE_READ_DATA: u32 = 0x0001;

/// Who may read `dir`, judged against the account this process runs as.
///
/// # Errors
///
/// [`ProtectError`] if the list or the account cannot be read.
pub fn dir_readers(dir: &Path) -> Result<Readers, ProtectError> {
    let own = own_sid()?;
    let list = dacl_of(dir)?;
    Ok(readers(list.as_deref(), &own))
}

/// The entries of `dir`'s access-control list, or `None` for a null list.
fn dacl_of(dir: &Path) -> Result<Option<Vec<Entry>>, ProtectError> {
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `wide` is a NUL-terminated path that outlives the call. Only the DACL is asked for,
    // so the owner, group and SACL out-pointers may be null. `dacl` and `descriptor` are valid
    // places for the two pointers the call writes; the DACL points into the descriptor.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(ProtectError::new("GetNamedSecurityInfoW", status));
    }
    let entries = if dacl.is_null() {
        None
    } else {
        Some(entries_of(dacl))
    };
    // SAFETY: `descriptor` was allocated by the call above with `LocalAlloc`, and is freed once,
    // here, after the last read through `dacl`, which points into it.
    let _ = unsafe { LocalFree(descriptor) };
    entries.transpose()
}

/// The allow entries of a non-null `dacl`. Deny and other entry types grant nothing to judge.
fn entries_of(dacl: *mut ACL) -> Result<Vec<Entry>, ProtectError> {
    // SAFETY: `dacl` is the non-null list `GetNamedSecurityInfoW` returned, valid while its
    // descriptor is, which the caller frees only after this returns.
    let count = unsafe { (*dacl).AceCount };
    let mut entries = Vec::with_capacity(usize::from(count));
    for index in 0..u32::from(count) {
        let mut ace: *mut core::ffi::c_void = ptr::null_mut();
        // SAFETY: `index` is below the list's own count, and `ace` is a valid place for the
        // pointer the call writes, which points into the list.
        if unsafe { GetAce(dacl, index, &raw mut ace) } == 0 {
            return Err(ProtectError::new("GetAce", last_error()));
        }
        // SAFETY: every entry starts with an `ACE_HEADER`.
        let header = unsafe { ptr::read_unaligned(ace.cast::<ACE_HEADER>()) };
        if header.AceType != ACCESS_ALLOWED {
            continue;
        }
        // SAFETY: an entry of type `ACCESS_ALLOWED_ACE_TYPE` is an `ACCESS_ALLOWED_ACE`.
        let allowed = unsafe { ptr::read_unaligned(ace.cast::<ACCESS_ALLOWED_ACE>()) };
        // The SID is stored in place, starting at `SidStart`.
        // SAFETY: `SidStart` is a field of the entry `ace` points at.
        let sid = unsafe { ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart) };
        entries.push(Entry {
            sid: sid_string(sid.cast())?,
            allows_read: allowed.Mask & (FILE_READ_DATA | GENERIC_READ | GENERIC_ALL) != 0,
            inherit_only: u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0,
        });
    }
    Ok(entries)
}

/// The SID this process runs as, in string form.
fn own_sid() -> Result<String, ProtectError> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no closing; `token` is a
    // valid place for the handle the call writes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(ProtectError::new("OpenProcessToken", last_error()));
    }
    let sid = token_user_sid(token);
    // SAFETY: `token` was opened above and is closed once, here.
    let _ = unsafe { CloseHandle(token) };
    sid
}

fn token_user_sid(token: HANDLE) -> Result<String, ProtectError> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer of length zero asks only for the length, which the call writes to
    // `needed`; it fails with ERROR_INSUFFICIENT_BUFFER by design.
    let _ = unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &raw mut needed) };
    // `u64` words, so the buffer is aligned for the `TOKEN_USER` the call writes at its start.
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8).max(1)];
    let length = u32::try_from(buffer.len() * 8)
        .map_err(|_| ProtectError::new("GetTokenInformation", 87))?;
    // SAFETY: `buffer` is `length` bytes, at least `needed`, and outlives the call.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(ProtectError::new("GetTokenInformation", last_error()));
    }
    // SAFETY: the call wrote a `TOKEN_USER` at the start of the aligned buffer; its SID points into
    // the same buffer, which lives until the end of this function.
    let user = unsafe { ptr::read(buffer.as_ptr().cast::<TOKEN_USER>()) };
    sid_string(user.User.Sid)
}

/// A SID in `S-1-…` form.
fn sid_string(sid: PSID) -> Result<String, ProtectError> {
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` points at a valid SID the caller holds; `text` is a valid place for the string
    // the call allocates.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(ProtectError::new("ConvertSidToStringSidW", last_error()));
    }
    let mut len = 0;
    // SAFETY: the call returned a NUL-terminated string; this walks it to the NUL.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `text` holds `len` UTF-16 units before its NUL.
    let units = unsafe { std::slice::from_raw_parts(text, len) };
    let string = String::from_utf16_lossy(units);
    // SAFETY: `text` came from the call's `LocalAlloc` and is freed once, here.
    let _ = unsafe { LocalFree(text.cast()) };
    Ok(string)
}

fn last_error() -> u32 {
    // SAFETY: reads the calling thread's last-error value; no memory is involved.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

#[cfg(test)]
mod tests {
    use super::{dir_readers, own_sid};

    #[test]
    fn this_process_has_an_account() {
        assert!(own_sid().expect("own").starts_with("S-1-"));
    }

    #[test]
    fn a_temporary_directory_can_be_judged() {
        let dir = std::env::temp_dir();
        assert!(dir_readers(&dir).is_ok());
    }
}
