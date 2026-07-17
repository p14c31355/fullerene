//! Process launch helpers for native Fullerene ELF applications.

use alloc::format;
use alloc::vec::Vec;
use fullerene_abi::SyscallErrorCode;

const MAX_PROGRAM_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecError {
    NotFound,
    InvalidExecutable,
    TooLarge,
    OutOfMemory,
    PermissionDenied,
    Unsupported,
    Io,
}

fn map_error(error: i64) -> ExecError {
    match -error {
        value if value == SyscallErrorCode::FileNotFound.as_i64() => ExecError::NotFound,
        value if value == SyscallErrorCode::InvalidArgument.as_i64() => {
            ExecError::InvalidExecutable
        }
        value if value == SyscallErrorCode::OutOfMemory.as_i64() => ExecError::OutOfMemory,
        value if value == SyscallErrorCode::PermissionDenied.as_i64() => {
            ExecError::PermissionDenied
        }
        value if value == SyscallErrorCode::NotSupported.as_i64() => ExecError::Unsupported,
        _ => ExecError::Io,
    }
}

pub fn spawn(binary: &[u8], args: &[&str]) -> Result<u64, ExecError> {
    let name = args.first().copied().unwrap_or("application");
    crate::sys::spawn_image(binary, name).map_err(map_error)
}

pub fn spawn_simple(name: &str) -> Result<u64, ExecError> {
    let path = format!("/packages/{name}/app.bin");
    let fd = crate::sys::open_read(&path).map_err(map_error)?;
    let mut binary = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let spawn_result = (|| {
        loop {
            let count = crate::sys::read(fd, &mut chunk).map_err(map_error)?;
            if count == 0 {
                break;
            }
            if binary.len().saturating_add(count) > MAX_PROGRAM_BYTES {
                return Err(ExecError::TooLarge);
            }
            binary.extend_from_slice(&chunk[..count]);
        }
        spawn(&binary, &[name])
    })();
    let close_result = crate::sys::close(fd).map_err(map_error);
    match (spawn_result, close_result) {
        (Ok(pid), Ok(())) => Ok(pid),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

pub fn list_programs() -> alloc::vec::Vec<&'static str> {
    alloc::vec!["toluene", "shell"]
}
