//! Process-owned terminal endpoints.

use super::interface::{SyscallError, SyscallResult};
use alloc::vec;
use petroleum::common::memory::UserSlice;

const MAX_TITLE_BYTES: usize = 128;

/// Create a terminal window for the calling process.
///
/// The returned endpoint is passed to `spawn` and becomes the child's fd 0/1
/// attachment. Terminal ownership is process lifecycle state, not a shell
/// special case.
pub(crate) fn syscall_create_terminal(title: *const u8, length: usize) -> SyscallResult {
    if length == 0 || length > MAX_TITLE_BYTES {
        return Err(SyscallError::InvalidArgument);
    }
    let slice =
        UserSlice::new(title as *mut u8, length, false).map_err(|_| SyscallError::AddressFault)?;
    let mut title_bytes = vec![0u8; length];
    unsafe { slice.copy_from_user(&mut title_bytes) }.map_err(|_| SyscallError::AddressFault)?;
    let title = core::str::from_utf8(&title_bytes).map_err(|_| SyscallError::InvalidArgument)?;
    if title.chars().any(|c| c.is_control()) {
        return Err(SyscallError::InvalidArgument);
    }
    let owner = crate::process::current_pid().ok_or(SyscallError::NoSuchProcess)?;
    let window = solvent::create_process_terminal(&title).ok_or(SyscallError::Io)?;
    if !crate::process::SCHEDULER.register_terminal_owner(window.0, owner) {
        solvent::close_process_terminal(window);
        return Err(SyscallError::Io);
    }
    Ok(window.0)
}
