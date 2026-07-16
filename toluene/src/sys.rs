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
