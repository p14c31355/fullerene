//! Typed system-call wrappers for the Toluene SDK.

use fullerene_abi::{AbiInfo, AbiVersion, SyscallErrorCode, SyscallNumber};

#[inline]
unsafe fn raw_syscall(
    number: SyscallNumber,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    unsafe {
        petroleum::common::syscall::syscall(number.as_u64(), arg1, arg2, arg3, arg4, arg5, arg6)
    }
}

#[inline]
fn syscall_result(value: u64) -> Result<u64, i64> {
    let signed = value as i64;
    if signed < 0 { Err(signed) } else { Ok(value) }
}

/// Query the packed ABI version. This works with kernels predating `AbiInfo`.
pub fn abi_version() -> AbiVersion {
    let packed = unsafe { raw_syscall(SyscallNumber::AbiQuery, 0, 0, 0, 0, 0, 0) };
    AbiVersion::unpack(packed)
}

/// Query the ABI version and capabilities advertised by the kernel.
pub fn abi_info() -> Result<AbiInfo, i64> {
    let mut info = AbiInfo::EMPTY;
    let value = unsafe {
        raw_syscall(
            SyscallNumber::AbiQuery,
            (&mut info as *mut AbiInfo) as u64,
            AbiInfo::BYTE_SIZE as u64,
            0,
            0,
            0,
            0,
        )
    };
    let written = syscall_result(value)? as usize;
    if written < AbiInfo::BYTE_SIZE || info.struct_size < AbiInfo::BYTE_SIZE as u32 {
        return Err(-SyscallErrorCode::NotSupported.as_i64());
    }
    Ok(info)
}

/// Get the current process ID.
pub fn current_pid() -> usize {
    unsafe { raw_syscall(SyscallNumber::GetPid, 0, 0, 0, 0, 0, 0) as usize }
}

/// Yield the CPU to the scheduler.
pub fn yield_now() {
    unsafe {
        raw_syscall(SyscallNumber::Yield, 0, 0, 0, 0, 0, 0);
    }
}

/// Terminate the process with an exit code.
pub fn exit_process(code: i32) -> ! {
    unsafe {
        raw_syscall(SyscallNumber::Exit, code as u64, 0, 0, 0, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Write raw bytes to a file descriptor.
pub fn write(fd: i32, data: &[u8]) -> Result<usize, i64> {
    let value = unsafe {
        raw_syscall(
            SyscallNumber::Write,
            fd as u64,
            data.as_ptr() as u64,
            data.len() as u64,
            0,
            0,
            0,
        )
    };
    syscall_result(value).map(|written| written as usize)
}

/// Open a file read-only.
pub fn open_read(path: &str) -> Result<i32, i64> {
    let mut nul_terminated = alloc::vec::Vec::with_capacity(path.len() + 1);
    nul_terminated.extend_from_slice(path.as_bytes());
    nul_terminated.push(0);
    let value = unsafe {
        raw_syscall(
            SyscallNumber::Open,
            nul_terminated.as_ptr() as u64,
            0,
            0,
            0,
            0,
            0,
        )
    };
    syscall_result(value).map(|fd| fd as i32)
}

/// Read bytes from a file descriptor.
pub fn read(fd: i32, data: &mut [u8]) -> Result<usize, i64> {
    let value = unsafe {
        raw_syscall(
            SyscallNumber::Read,
            fd as u64,
            data.as_mut_ptr() as u64,
            data.len() as u64,
            0,
            0,
            0,
        )
    };
    syscall_result(value).map(|read| read as usize)
}

/// Close a file descriptor.
pub fn close(fd: i32) -> Result<(), i64> {
    let value = unsafe { raw_syscall(SyscallNumber::Close, fd as u64, 0, 0, 0, 0, 0) };
    syscall_result(value).map(|_| ())
}

/// Start an ELF image in a new isolated process.
pub fn spawn_image(image: &[u8], name: &str) -> Result<u64, i64> {
    let value = unsafe {
        raw_syscall(
            SyscallNumber::Spawn,
            image.as_ptr() as u64,
            image.len() as u64,
            name.as_ptr() as u64,
            name.len() as u64,
            0,
            0,
        )
    };
    syscall_result(value)
}

/// Write raw bytes to stdout (fd 1).
pub fn stdout_write(data: &[u8]) -> Result<usize, i64> {
    write(1, data)
}

/// Print a string to stdout.
pub fn println(s: &str) {
    let _ = stdout_write(s.as_bytes());
    let _ = stdout_write(b"\n");
}

/// Print a string without newline.
pub fn print(s: &str) {
    let _ = stdout_write(s.as_bytes());
}

/// Get the number of active processes, if supported by the kernel.
pub fn process_count() -> Option<usize> {
    None
}

/// Get system uptime in microseconds.
pub fn uptime_ticks() -> Option<u64> {
    let mut uptime = 0u64;
    let value = unsafe {
        raw_syscall(
            SyscallNumber::Uptime,
            (&mut uptime as *mut u64) as u64,
            0,
            0,
            0,
            0,
            0,
        )
    };
    syscall_result(value).ok().map(|_| uptime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_returns_are_errors() {
        let raw = (-SyscallErrorCode::InvalidArgument.as_i64()) as u64;
        assert_eq!(syscall_result(raw), Err(-22));
        assert_eq!(syscall_result(7), Ok(7));
    }
}
