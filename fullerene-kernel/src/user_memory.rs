//! Shared user-memory copy primitives for all syscall ABIs.
//!
//! ABI modules are responsible for translating [`UserCopyError`] into their
//! public error representation.  This module owns validation and copying so
//! native and compatibility ABIs cannot drift into different safety models.

use alloc::string::String;
use alloc::vec::Vec;
use petroleum::common::logging::SystemError;
use petroleum::common::memory::{UserPtr, UserSlice};

const PAGE_SIZE: usize = 4096;

/// Failure produced while copying data across the user/kernel boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserCopyError {
    /// Address validation, permission checking, or allocation failed.
    System(SystemError),
    /// A user-provided C string was not valid UTF-8.
    InvalidUtf8,
    /// No NUL terminator was found within the caller-provided limit.
    MissingNul,
}

impl From<SystemError> for UserCopyError {
    fn from(error: SystemError) -> Self {
        Self::System(error)
    }
}

fn bytes_until_page_end(address: usize) -> usize {
    PAGE_SIZE - (address & (PAGE_SIZE - 1))
}

#[cfg(test)]
fn decode_c_string(mut bytes: Vec<u8>) -> Result<String, UserCopyError> {
    let nul = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(UserCopyError::MissingNul)?;
    bytes.truncate(nul);
    String::from_utf8(bytes).map_err(|_| UserCopyError::InvalidUtf8)
}

/// Copy a bounded NUL-terminated UTF-8 string from user memory.
///
/// Validation is performed one page at a time.  Consequently, a string whose
/// terminator is the final byte of a mapped page does not require the next page
/// to be mapped merely because `max_len` extends into it.
///
/// # Safety
///
/// The caller must keep the current process address space stable for the
/// duration of the copy.  In particular, the referenced pages must not be
/// concurrently unmapped after validation.
pub(crate) unsafe fn copy_c_string(
    ptr: *const u8,
    max_len: usize,
) -> Result<String, UserCopyError> {
    if ptr.is_null() || max_len == 0 {
        return Err(UserCopyError::System(SystemError::InvalidArgument));
    }

    let mut bytes = Vec::new();
    let mut offset = 0;
    while offset < max_len {
        let current = ptr.wrapping_add(offset);
        let chunk_len = (max_len - offset).min(bytes_until_page_end(current as usize));
        bytes
            .try_reserve_exact(chunk_len)
            .map_err(|_| UserCopyError::System(SystemError::MemOutOfMemory))?;

        let chunk_start = bytes.len();
        bytes.resize(chunk_start + chunk_len, 0);
        let user = UserSlice::new(current as *mut u8, chunk_len, false)?;
        unsafe { user.copy_from_user(&mut bytes[chunk_start..])? };

        if let Some(nul_idx) = bytes[chunk_start..].iter().position(|&b| b == 0) {
            bytes.truncate(chunk_start + nul_idx);
            return String::from_utf8(bytes).map_err(|_| UserCopyError::InvalidUtf8);
        }
        offset += chunk_len;
    }

    Err(UserCopyError::MissingNul)
}

/// Copy `len` bytes from user memory into a kernel-owned buffer.
///
/// # Safety
///
/// The caller must keep the current process address space stable for the
/// duration of the copy.
pub(crate) unsafe fn copy_bytes_from_user(
    ptr: *const u8,
    len: usize,
) -> Result<Vec<u8>, UserCopyError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| UserCopyError::System(SystemError::MemOutOfMemory))?;
    bytes.resize(len, 0);

    let user = UserSlice::new(ptr as *mut u8, len, false)?;
    unsafe { user.copy_from_user(&mut bytes)? };
    Ok(bytes)
}

/// Copy kernel-owned bytes into user memory.
///
/// # Safety
///
/// The caller must keep the current process address space stable for the
/// duration of the copy.
pub(crate) unsafe fn copy_bytes_to_user(ptr: *mut u8, bytes: &[u8]) -> Result<(), UserCopyError> {
    let user = UserSlice::new(ptr, bytes.len(), true)?;
    unsafe { user.copy_to_user(bytes)? };
    Ok(())
}

/// Copy one `Copy` value into user memory with alignment-independent access.
///
/// # Safety
///
/// The caller must keep the current process address space stable for the
/// duration of the copy.
pub(crate) unsafe fn copy_value_to_user<T: Copy>(
    ptr: *mut T,
    value: &T,
) -> Result<(), UserCopyError> {
    let user = UserPtr::new_mut(ptr)?;
    unsafe { user.copy_to_user(*value)? };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn page_chunk_stops_at_the_current_page_boundary() {
        assert_eq!(bytes_until_page_end(0x1000), PAGE_SIZE);
        assert_eq!(bytes_until_page_end(0x1fff), 1);
        assert_eq!(bytes_until_page_end(0x2123), PAGE_SIZE - 0x123);
    }

    #[test]
    fn c_string_decoder_stops_at_nul() {
        assert_eq!(
            decode_c_string(b"fullerene\0ignored".to_vec()),
            Ok(String::from("fullerene"))
        );
    }

    #[test]
    fn c_string_decoder_rejects_invalid_utf8_and_missing_nul() {
        assert_eq!(
            decode_c_string(vec![0xff, 0]),
            Err(UserCopyError::InvalidUtf8)
        );
        assert_eq!(
            decode_c_string(b"unterminated".to_vec()),
            Err(UserCopyError::MissingNul)
        );
    }
}
