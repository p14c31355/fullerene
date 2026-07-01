/// System call numbers
#[repr(u64)]
#[derive(Debug, Clone, Copy)]
pub enum SyscallNumber {
    /// Exit the current process (exit_code in RDI)
    Exit = 1,
    /// Create a new process (entry_point in RDI)
    Fork = 2,
    /// Read from file descriptor (fd in RDI, buffer in RSI, count in RDX)
    Read = 3,
    /// Write to file descriptor (fd in RDI, buffer in RSI, count in RDX)
    Write = 4,
    /// Open file (filename in RDI, flags in RSI, mode in RDX)
    Open = 5,
    /// Close file descriptor (fd in RDI)
    Close = 6,
    /// Wait for process to finish (pid in RDI)
    Wait = 7,
    /// Get current process ID
    GetPid = 20,
    /// Get process name (buffer in RDI, size in RSI)
    GetProcessName = 21,
    /// Yield to scheduler
    Yield = 22,

    // ── Memory API (30–39) ──────────────────────────────────────
    /// Map virtual memory (addr, length, flags)
    MapMemory = 30,
    /// Unmap virtual memory (addr, length)
    UnmapMemory = 31,
    /// Change page protection (addr, length, prot)
    ProtectMemory = 32,
    /// Query memory region info (addr, info_buf)
    QueryMemory = 33,

    // ── Event API (40–49) ───────────────────────────────────────
    /// Create an event object (flags → event_handle)
    CreateEvent = 40,
    /// Wait on an event (handle, timeout_us)
    WaitEvent = 41,
    /// Signal an event (handle)
    SignalEvent = 42,
    /// Subscribe to an event type (event_type, callback_info)
    SubscribeEvent = 43,

    // ── Thread API (50–59) ──────────────────────────────────────
    /// Create a thread sharing the parent address space (entry, stack, flags → tid)
    CreateThread = 50,
    /// Wait for a thread to finish (tid → exit_code)
    JoinThread = 51,
    /// Detach a thread so it is cleaned up automatically (tid)
    DetachThread = 52,
    /// Exit the calling thread (exit_code)
    ExitThread = 53,

    // ── Window API (60–69) ──────────────────────────────────────
    /// Create a window (x, y, w, h, flags → window_handle)
    CreateWindow = 60,
    /// Destroy a window (handle)
    DestroyWindow = 61,
    /// Resize a window (handle, w, h)
    ResizeWindow = 62,
    /// Present / flush window contents (handle)
    PresentWindow = 63,
    /// Poll for the next window event (handle, event_buf)
    GetWindowEvent = 64,

    // ── Device API (70–79) ──────────────────────────────────────
    /// Enumerate devices of a given class (class, buf, bufsize → count)
    EnumerateDevices = 70,
    /// Open a device by ID (device_id → handle)
    OpenDevice = 71,
    /// Perform device-specific I/O control (handle, cmd, arg)
    DeviceIoctl = 72,

    // ── IPC API (80–89) ─────────────────────────────────────────
    /// Create a message channel (flags → channel_handle)
    ChannelCreate = 80,
    /// Send a message on a channel (handle, data, size)
    ChannelSend = 81,
    /// Receive a message from a channel (handle, buf, bufsize)
    ChannelRecv = 82,
    /// Create a unidirectional pipe (flags → [read_handle, write_handle])
    PipeCreate = 83,

    // ── Capability / Handle API (90–99) ─────────────────────────
    /// Transfer a handle to another process (target_pid, handle)
    HandleTransfer = 90,
    /// Duplicate a handle (handle → new_handle)
    HandleDuplicate = 91,
    /// Revoke a handle (handle)
    HandleRevoke = 92,

    // ── Time API (100–109) ──────────────────────────────────────
    /// Get current time from a clock (clock_id, timespec_buf)
    ClockGetTime = 100,
    /// Create a timer that signals an event (clock_id, deadline_ns, event_handle)
    TimerCreate = 101,
    /// Sleep for the given number of microseconds (us)
    Sleep = 102,
    /// Get system uptime in microseconds (uptime_buf)
    Uptime = 103,
}

/// Check if VDSO is available (user-space pointer initialized).
#[inline]
fn vdso_available() -> bool {
    crate::vdso::user::vdso_ptr_initialized()
}

/// List of syscalls that are safe for VDSO fast-path (no blocking).
/// Blocking calls like read/write/sleep must use the real `syscall` instruction
/// so the kernel can preempt the caller.
const VDSO_SAFE_SYSCALLS: &[SyscallNumber] = &[
    SyscallNumber::Uptime,
    SyscallNumber::GetPid,
];

fn is_vdso_safe(syscall_num: u64) -> bool {
    VDSO_SAFE_SYSCALLS.iter().any(|&n| n as u64 == syscall_num)
}

/// Execute a raw `syscall` instruction (always traps to kernel).
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
    result
}

/// Raw syscall: uses VDSO for non-blocking queries, `syscall` instruction otherwise.
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
    if vdso_available() && is_vdso_safe(syscall_num) {
        // VDSO path: zero-trap syscall via shared page (non-blocking only)
        crate::vdso::user::vdso_call_blocking(syscall_num, [arg1, arg2, arg3, arg4, arg5, arg6])
    } else {
        // Fallback: traditional syscall instruction (traps to kernel)
        syscall_insn(syscall_num, arg1, arg2, arg3, arg4, arg5, arg6)
    }
}

/// Simple write syscall wrapper
pub fn write(fd: i32, buf: &[u8]) -> i64 {
    unsafe {
        syscall(
            SyscallNumber::Write as u64,
            fd as u64,
            buf.as_ptr() as u64,
            buf.len() as u64,
            0,
            0,
            0,
        ) as i64
    }
}

/// Simple exit syscall wrapper
pub fn exit(code: i32) -> ! {
    unsafe {
        syscall(SyscallNumber::Exit as u64, code as u64, 0, 0, 0, 0, 0);
    }
    loop {} // unreachable, but to make ! return type
}

/// Get PID syscall wrapper
pub fn getpid() -> u64 {
    unsafe { syscall(SyscallNumber::GetPid as u64, 0, 0, 0, 0, 0, 0) }
}

/// Yield syscall wrapper
pub fn sleep() {
    unsafe {
        syscall(SyscallNumber::Yield as u64, 0, 0, 0, 0, 0, 0);
    }
}
