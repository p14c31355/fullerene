#![no_std]

/// Syscall numbers for the Fullerene native ABI.
pub mod syscall_numbers {
    // ABI version query
    pub const ABI_VERSION: u64 = 0;

    // Basic (1-22)
    pub const EXIT: u64 = 1;
    pub const FORK: u64 = 2;
    pub const READ: u64 = 3;
    pub const WRITE: u64 = 4;
    pub const OPEN: u64 = 5;
    pub const CLOSE: u64 = 6;
    pub const WAIT: u64 = 7;
    pub const GETPID: u64 = 20;
    pub const GET_PROCESS_NAME: u64 = 21;
    pub const YIELD: u64 = 22;

    // Memory (30-39)
    pub const MAP_MEMORY: u64 = 30;
    pub const UNMAP_MEMORY: u64 = 31;
    pub const PROTECT_MEMORY: u64 = 32;
    pub const QUERY_MEMORY: u64 = 33;

    // Event (40-49)
    pub const CREATE_EVENT: u64 = 40;
    pub const WAIT_EVENT: u64 = 41;
    pub const SIGNAL_EVENT: u64 = 42;
    pub const SUBSCRIBE_EVENT: u64 = 43;

    // Thread (50-59)
    pub const CREATE_THREAD: u64 = 50;
    pub const JOIN_THREAD: u64 = 51;
    pub const DETACH_THREAD: u64 = 52;
    pub const EXIT_THREAD: u64 = 53;

    // Window (60-69)
    pub const CREATE_WINDOW: u64 = 60;
    pub const DESTROY_WINDOW: u64 = 61;
    pub const RESIZE_WINDOW: u64 = 62;
    pub const PRESENT_WINDOW: u64 = 63;
    pub const GET_WINDOW_EVENT: u64 = 64;

    // Device (70-79)
    pub const ENUMERATE_DEVICES: u64 = 70;
    pub const OPEN_DEVICE: u64 = 71;
    pub const DEVICE_IOCTL: u64 = 72;

    // IPC (80-89)
    pub const CHANNEL_CREATE: u64 = 80;
    pub const CHANNEL_SEND: u64 = 81;
    pub const CHANNEL_RECV: u64 = 82;
    pub const PIPE_CREATE: u64 = 83;

    // Handle/Cap (90-99)
    pub const HANDLE_TRANSFER: u64 = 90;
    pub const HANDLE_DUPLICATE: u64 = 91;
    pub const HANDLE_REVOKE: u64 = 92;

    // Time (100-109)
    pub const CLOCK_GETTIME: u64 = 100;
    pub const TIMER_CREATE: u64 = 101;
    pub const SLEEP: u64 = 102;
    pub const UPTIME: u64 = 103;
}

/// Syscall error codes (aligned with Linux errno values for compatibility).
pub mod syscall_errors {
    pub const INVALID_SYSCALL: i64 = 1;
    pub const FILE_NOT_FOUND: i64 = 2;
    pub const NO_SUCH_PROCESS: i64 = 3;
    pub const BAD_FILE_DESCRIPTOR: i64 = 9;
    pub const AGAIN: i64 = 11;
    pub const OUT_OF_MEMORY: i64 = 12;
    pub const PERMISSION_DENIED: i64 = 13;
    pub const ALREADY_EXISTS: i64 = 17;
    pub const NO_SUCH_DEVICE: i64 = 19;
    pub const INVALID_ARGUMENT: i64 = 22;
    pub const NOT_SUPPORTED: i64 = 95;
    pub const BAD_HANDLE: i64 = 104;
    pub const TIMED_OUT: i64 = 110;
    pub const WOULD_BLOCK: i64 = 140;
}

/// ABI version information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub reserved: u16,
}

impl AbiVersion {
    pub const CURRENT: AbiVersion = AbiVersion {
        major: 0,
        minor: 2,
        patch: 0,
        reserved: 0,
    };
}

/// Capability bits for feature querying.
#[derive(Debug, Clone, Copy)]
#[repr(u64)]
pub enum Capability {
    NativeSyscall = 1 << 0,
    LinuxCompat = 1 << 1,
    MultiWindow = 1 << 2,
    EventSystem = 1 << 3,
    Threading = 1 << 4,
    IpcChannels = 1 << 5,
    IpcPipes = 1 << 6,
    TimerSystem = 1 << 7,
    DeviceEnumeration = 1 << 8,
}

// Size/alignment compile-time checks
const _: () = {
    assert!(core::mem::size_of::<AbiVersion>() == 8);
    assert!(core::mem::align_of::<AbiVersion>() == 2);
};
