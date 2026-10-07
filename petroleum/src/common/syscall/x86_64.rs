//! Low-level syscall instruction and compatibility wrappers.
//!
//! Syscall numbers are owned by the dependency-free `fullerene-abi` crate.

pub use fullerene_abi::SyscallNumber;

#[inline]
fn vdso_available() -> bool {
    crate::vdso::user::vdso_ptr_initialized()
}

#[inline]
unsafe fn syscall_insn(
    syscall_num: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    let result: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rax") syscall_num,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            in("r10") arg4,
            in("r8") arg5,
            in("r9") arg6,
            lateout("rax") result,
            out("rcx") _,
            out("r11") _,
        );
    }
    result
}

/// Raw syscall: uses the VDSO for non-blocking queries and traps otherwise.
///
/// # Safety
///
/// The caller must follow the ABI contract for `syscall_num`, ensure every
/// pointer argument is valid for the operation, and uphold any aliasing and
/// lifetime requirements of buffers that the kernel may read or write.
#[inline]
pub unsafe fn syscall(
    syscall_num: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
    arg6: u64,
) -> u64 {
    if vdso_available() {
        if syscall_num == SyscallNumber::Uptime.as_u64() && arg1 != 0 {
            unsafe {
                core::ptr::write_unaligned(arg1 as *mut u64, crate::vdso::user::vdso_uptime_us());
            }
            return 0;
        }
        if syscall_num == SyscallNumber::GetPid.as_u64() {
            return crate::vdso::user::vdso_pid();
        }
    }
    unsafe { syscall_insn(syscall_num, arg1, arg2, arg3, arg4, arg5, arg6) }
}

/// Compatibility wrapper. New user-space code should use `toluene::sys`.
pub fn write(fd: i32, buf: &[u8]) -> i64 {
    unsafe {
        syscall(
            SyscallNumber::Write.as_u64(),
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        ) as i64
    }
}

/// Compatibility wrapper. New user-space code should use `toluene::sys`.
pub fn exit(code: i32) -> ! {
    unsafe {
        syscall(SyscallNumber::Exit.as_u64(), code as u64, 0, 0, 0, 0, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Compatibility wrapper. New user-space code should use `toluene::sys`.
pub fn getpid() -> u64 {
    unsafe { syscall(SyscallNumber::GetPid.as_u64(), 0, 0, 0, 0, 0, 0) }
}

/// Compatibility wrapper. New user-space code should use `toluene::sys`.
pub fn sleep() {
    unsafe {
        syscall(SyscallNumber::Yield.as_u64(), 0, 0, 0, 0, 0, 0);
    }
}
