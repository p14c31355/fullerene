#![no_std]

//! Stable data types shared by the Fullerene kernel and user-space SDK.
//!
//! This crate intentionally has no dependencies. Everything that crosses the
//! syscall boundary lives here so the kernel and SDK cannot silently drift.

use core::convert::TryFrom;

macro_rules! all_syscall { ($($v:ident),* $(,)?) => { pub const ALL: &'static [Self] = &[$(Self::$v),*]; }; }
macro_rules! all_error { ($($v:ident),* $(,)?) => { pub const ALL: &'static [Self] = &[$(Self::$v),*]; }; }

/// A Fullerene native syscall number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
#[non_exhaustive]
pub enum SyscallNumber {
    AbiQuery = 0,
    Exit = 1,
    Fork = 2,
    Read = 3,
    Write = 4,
    Open = 5,
    Close = 6,
    Wait = 7,
    GetPid = 20,
    GetProcessName = 21,
    Yield = 22,
    Spawn = 23,
    MapMemory = 30,
    UnmapMemory = 31,
    ProtectMemory = 32,
    QueryMemory = 33,
    CreateEvent = 40,
    WaitEvent = 41,
    SignalEvent = 42,
    SubscribeEvent = 43,
    CreateThread = 50,
    JoinThread = 51,
    DetachThread = 52,
    ExitThread = 53,
    CreateWindow = 60,
    DestroyWindow = 61,
    ResizeWindow = 62,
    PresentWindow = 63,
    GetWindowEvent = 64,
    EnumerateDevices = 70,
    OpenDevice = 71,
    DeviceIoctl = 72,
    ChannelCreate = 80,
    ChannelSend = 81,
    ChannelRecv = 82,
    PipeCreate = 83,
    HandleTransfer = 90,
    HandleDuplicate = 91,
    HandleRevoke = 92,
    ClockGetTime = 100,
    TimerCreate = 101,
    Sleep = 102,
    Uptime = 103,
}

impl SyscallNumber {
    all_syscall! {
        AbiQuery, Exit, Fork, Read, Write, Open, Close, Wait,
        GetPid, GetProcessName, Yield, Spawn,
        MapMemory, UnmapMemory, ProtectMemory, QueryMemory,
        CreateEvent, WaitEvent, SignalEvent, SubscribeEvent,
        CreateThread, JoinThread, DetachThread, ExitThread,
        CreateWindow, DestroyWindow, ResizeWindow, PresentWindow, GetWindowEvent,
        EnumerateDevices, OpenDevice, DeviceIoctl,
        ChannelCreate, ChannelSend, ChannelRecv, PipeCreate,
        HandleTransfer, HandleDuplicate, HandleRevoke,
        ClockGetTime, TimerCreate, Sleep, Uptime,
    }

    #[inline]
    pub const fn as_u64(self) -> u64 {
        self as u64
    }
}

impl TryFrom<u64> for SyscallNumber {
    type Error = ();
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        macro_rules! match_num { ($($n:ident),* $(,)?) => { match value { $(syscall_numbers::$n => Ok(Self::$v),)* _ => Err(()) } }; ($($n:ident => $v:ident),* $(,)?) => { match value { $(syscall_numbers::$n => Ok(Self::$v),)* _ => Err(()) } }; }
        match_num! {
            ABI_QUERY => AbiQuery, EXIT => Exit, FORK => Fork, READ => Read, WRITE => Write,
            OPEN => Open, CLOSE => Close, WAIT => Wait, GETPID => GetPid, GET_PROCESS_NAME => GetProcessName,
            YIELD => Yield, SPAWN => Spawn, MAP_MEMORY => MapMemory, UNMAP_MEMORY => UnmapMemory,
            PROTECT_MEMORY => ProtectMemory, QUERY_MEMORY => QueryMemory,
            CREATE_EVENT => CreateEvent, WAIT_EVENT => WaitEvent, SIGNAL_EVENT => SignalEvent, SUBSCRIBE_EVENT => SubscribeEvent,
            CREATE_THREAD => CreateThread, JOIN_THREAD => JoinThread, DETACH_THREAD => DetachThread, EXIT_THREAD => ExitThread,
            CREATE_WINDOW => CreateWindow, DESTROY_WINDOW => DestroyWindow, RESIZE_WINDOW => ResizeWindow,
            PRESENT_WINDOW => PresentWindow, GET_WINDOW_EVENT => GetWindowEvent,
            ENUMERATE_DEVICES => EnumerateDevices, OPEN_DEVICE => OpenDevice, DEVICE_IOCTL => DeviceIoctl,
            CHANNEL_CREATE => ChannelCreate, CHANNEL_SEND => ChannelSend, CHANNEL_RECV => ChannelRecv, PIPE_CREATE => PipeCreate,
            HANDLE_TRANSFER => HandleTransfer, HANDLE_DUPLICATE => HandleDuplicate, HANDLE_REVOKE => HandleRevoke,
            CLOCK_GETTIME => ClockGetTime, TIMER_CREATE => TimerCreate, SLEEP => Sleep, UPTIME => Uptime,
        }
    }
}

/// Compatibility constants for code that matches on raw syscall numbers.
pub mod syscall_numbers {
    macro_rules! sc { ($($name:ident = $variant:ident),* $(,)?) => { $(pub const $name: u64 = super::SyscallNumber::$variant.as_u64();)* }; }
    sc! {
        ABI_QUERY = AbiQuery, ABI_VERSION = AbiQuery,
        EXIT = Exit, FORK = Fork, READ = Read, WRITE = Write, OPEN = Open, CLOSE = Close, WAIT = Wait,
        GETPID = GetPid, GET_PROCESS_NAME = GetProcessName, YIELD = Yield, SPAWN = Spawn,
        MAP_MEMORY = MapMemory, UNMAP_MEMORY = UnmapMemory, PROTECT_MEMORY = ProtectMemory, QUERY_MEMORY = QueryMemory,
        CREATE_EVENT = CreateEvent, WAIT_EVENT = WaitEvent, SIGNAL_EVENT = SignalEvent, SUBSCRIBE_EVENT = SubscribeEvent,
        CREATE_THREAD = CreateThread, JOIN_THREAD = JoinThread, DETACH_THREAD = DetachThread, EXIT_THREAD = ExitThread,
        CREATE_WINDOW = CreateWindow, DESTROY_WINDOW = DestroyWindow, RESIZE_WINDOW = ResizeWindow,
        PRESENT_WINDOW = PresentWindow, GET_WINDOW_EVENT = GetWindowEvent,
        ENUMERATE_DEVICES = EnumerateDevices, OPEN_DEVICE = OpenDevice, DEVICE_IOCTL = DeviceIoctl,
        CHANNEL_CREATE = ChannelCreate, CHANNEL_SEND = ChannelSend, CHANNEL_RECV = ChannelRecv, PIPE_CREATE = PipeCreate,
        HANDLE_TRANSFER = HandleTransfer, HANDLE_DUPLICATE = HandleDuplicate, HANDLE_REVOKE = HandleRevoke,
        CLOCK_GETTIME = ClockGetTime, TIMER_CREATE = TimerCreate, SLEEP = Sleep, UPTIME = Uptime,
    }
}

/// A positive error code returned as its negated value from a syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
#[non_exhaustive]
pub enum SyscallErrorCode {
    InvalidSyscall = 1,
    FileNotFound = 2,
    NoSuchProcess = 3,
    Io = 5,
    BadFileDescriptor = 9,
    Again = 11,
    OutOfMemory = 12,
    PermissionDenied = 13,
    AddressFault = 14,
    Busy = 16,
    AlreadyExists = 17,
    NoSuchDevice = 19,
    NotADirectory = 20,
    IsADirectory = 21,
    InvalidArgument = 22,
    NoSpace = 28,
    DirectoryNotEmpty = 39,
    Overflow = 75,
    NotSupported = 95,
    BadHandle = 104,
    TimedOut = 110,
    WouldBlock = 140,
}

impl SyscallErrorCode {
    all_error! {
        InvalidSyscall, FileNotFound, NoSuchProcess, Io, BadFileDescriptor, Again, OutOfMemory,
        PermissionDenied, AddressFault, Busy, AlreadyExists, NoSuchDevice,
        NotADirectory, IsADirectory, InvalidArgument, NoSpace, DirectoryNotEmpty,
        Overflow, NotSupported, BadHandle, TimedOut, WouldBlock,
    }

    #[inline]
    pub const fn as_i64(self) -> i64 {
        self as i64
    }
}

impl TryFrom<i64> for SyscallErrorCode {
    type Error = ();
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        macro_rules! match_err { ($($n:literal => $v:ident),* $(,)?) => { match value { $( $n => Ok(Self::$v),)* _ => Err(()) } }; }
        match_err! {
            1 => InvalidSyscall, 2 => FileNotFound, 3 => NoSuchProcess, 5 => Io, 9 => BadFileDescriptor,
            11 => Again, 12 => OutOfMemory, 13 => PermissionDenied, 14 => AddressFault, 16 => Busy,
            17 => AlreadyExists, 19 => NoSuchDevice, 20 => NotADirectory, 21 => IsADirectory, 22 => InvalidArgument,
            28 => NoSpace, 39 => DirectoryNotEmpty, 75 => Overflow, 95 => NotSupported, 104 => BadHandle,
            110 => TimedOut, 140 => WouldBlock,
        }
    }
}

/// Compatibility constants for raw error-code users.
pub mod syscall_errors {
    macro_rules! se { ($($name:ident = $variant:ident),* $(,)?) => { $(pub const $name: i64 = super::SyscallErrorCode::$variant.as_i64();)* }; }
    se! {
        INVALID_SYSCALL = InvalidSyscall, FILE_NOT_FOUND = FileNotFound, NO_SUCH_PROCESS = NoSuchProcess,
        IO_ERROR = Io, BAD_FILE_DESCRIPTOR = BadFileDescriptor, AGAIN = Again, OUT_OF_MEMORY = OutOfMemory,
        PERMISSION_DENIED = PermissionDenied, ADDRESS_FAULT = AddressFault, BUSY = Busy, ALREADY_EXISTS = AlreadyExists,
        NO_SUCH_DEVICE = NoSuchDevice, NOT_A_DIRECTORY = NotADirectory, IS_A_DIRECTORY = IsADirectory,
        INVALID_ARGUMENT = InvalidArgument, NO_SPACE = NoSpace, DIRECTORY_NOT_EMPTY = DirectoryNotEmpty,
        OVERFLOW = Overflow, NOT_SUPPORTED = NotSupported, BAD_HANDLE = BadHandle, TIMED_OUT = TimedOut, WOULD_BLOCK = WouldBlock,
    }
}

/// Semantic version of the native syscall ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub reserved: u16,
}

impl AbiVersion {
    pub const CURRENT: Self = Self {
        major: 0,
        minor: 4,
        patch: 0,
        reserved: 0,
    };

    #[inline]
    pub const fn pack(self) -> u64 {
        (self.major as u64) << 48
            | (self.minor as u64) << 32
            | (self.patch as u64) << 16
            | self.reserved as u64
    }

    #[inline]
    pub const fn unpack(value: u64) -> Self {
        Self {
            major: (value >> 48) as u16,
            minor: (value >> 32) as u16,
            patch: (value >> 16) as u16,
            reserved: value as u16,
        }
    }
}

/// One capability advertised by [`AbiInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
#[non_exhaustive]
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
    ProcessSpawn = 1 << 9,
}

impl Capability {
    #[inline]
    pub const fn bit(self) -> u64 {
        self as u64
    }
}

/// Capability bitset with a stable C-compatible representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct CapabilitySet(pub u64);

impl CapabilitySet {
    pub const EMPTY: Self = Self(0);
    pub const ALL_DEFINED: Self = Self(
        Capability::NativeSyscall.bit()
            | Capability::LinuxCompat.bit()
            | Capability::MultiWindow.bit()
            | Capability::EventSystem.bit()
            | Capability::Threading.bit()
            | Capability::IpcChannels.bit()
            | Capability::IpcPipes.bit()
            | Capability::TimerSystem.bit()
            | Capability::DeviceEnumeration.bit()
            | Capability::ProcessSpawn.bit(),
    );

    #[inline]
    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & capability.bit() != 0
    }

    #[inline]
    pub const fn with(self, capability: Capability) -> Self {
        Self(self.0 | capability.bit())
    }

    #[inline]
    pub const fn bits(self) -> u64 {
        self.0
    }
}

/// Result of the ABI query syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct AbiInfo {
    pub version: AbiVersion,
    pub struct_size: u32,
    pub syscall_count: u32,
    pub capabilities: CapabilitySet,
    pub reserved: [u64; 2],
}

impl AbiInfo {
    /// Minimum size accepted from older clients.
    /// This value remains fixed when fields are appended in later versions.
    pub const MIN_BYTE_SIZE: usize = 24;
    pub const BYTE_SIZE: usize = 40;
    pub const EMPTY: Self = Self {
        version: AbiVersion {
            major: 0,
            minor: 0,
            patch: 0,
            reserved: 0,
        },
        struct_size: 0,
        syscall_count: 0,
        capabilities: CapabilitySet::EMPTY,
        reserved: [0; 2],
    };
    pub const fn new(capabilities: CapabilitySet) -> Self {
        Self {
            version: AbiVersion::CURRENT,
            struct_size: Self::BYTE_SIZE as u32,
            syscall_count: SyscallNumber::ALL.len() as u32,
            capabilities,
            reserved: [0; 2],
        }
    }

    pub fn to_ne_bytes(self) -> [u8; Self::BYTE_SIZE] {
        let mut bytes = [0; Self::BYTE_SIZE];
        bytes[0..2].copy_from_slice(&self.version.major.to_ne_bytes());
        bytes[2..4].copy_from_slice(&self.version.minor.to_ne_bytes());
        bytes[4..6].copy_from_slice(&self.version.patch.to_ne_bytes());
        bytes[6..8].copy_from_slice(&self.version.reserved.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.struct_size.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.syscall_count.to_ne_bytes());
        bytes[16..24].copy_from_slice(&self.capabilities.bits().to_ne_bytes());
        bytes[24..32].copy_from_slice(&self.reserved[0].to_ne_bytes());
        bytes[32..40].copy_from_slice(&self.reserved[1].to_ne_bytes());
        bytes
    }
}

impl Default for AbiInfo {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Information returned by `query_memory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct MemoryInfo {
    pub base_address: u64,
    pub length: u64,
    pub protection: u32,
    pub flags: u32,
    pub page_size: u64,
    pub committed_bytes: u64,
    pub reserved: [u64; 3],
}

impl MemoryInfo {
    /// Size accepted from clients built against ABI version 0.3.
    /// This value remains fixed when fields are appended in later versions.
    pub const MIN_BYTE_SIZE: usize = 64;
    pub const BYTE_SIZE: usize = 64;

    pub fn to_ne_bytes(self) -> [u8; Self::BYTE_SIZE] {
        let mut bytes = [0; Self::BYTE_SIZE];
        bytes[0..8].copy_from_slice(&self.base_address.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.length.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.protection.to_ne_bytes());
        bytes[20..24].copy_from_slice(&self.flags.to_ne_bytes());
        bytes[24..32].copy_from_slice(&self.page_size.to_ne_bytes());
        bytes[32..40].copy_from_slice(&self.committed_bytes.to_ne_bytes());
        for (index, value) in self.reserved.iter().enumerate() {
            let start = 40 + index * 8;
            bytes[start..start + 8].copy_from_slice(&value.to_ne_bytes());
        }
        bytes
    }
}

/// Time value returned by `clock_gettime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct TimeSpec {
    pub seconds: u64,
    pub nanoseconds: u64,
}

impl TimeSpec {
    pub const BYTE_SIZE: usize = 16;

    pub fn to_ne_bytes(self) -> [u8; Self::BYTE_SIZE] {
        let mut bytes = [0; Self::BYTE_SIZE];
        bytes[0..8].copy_from_slice(&self.seconds.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.nanoseconds.to_ne_bytes());
        bytes
    }
}

/// One device record returned by `enumerate_devices`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct DeviceInfo {
    pub class: u32,
    pub device_id: u32,
    pub vendor_id: u32,
    pub product_id: u32,
}

impl DeviceInfo {
    pub const BYTE_SIZE: usize = 16;

    pub fn to_ne_bytes(self) -> [u8; Self::BYTE_SIZE] {
        let mut bytes = [0; Self::BYTE_SIZE];
        bytes[0..4].copy_from_slice(&self.class.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.device_id.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.vendor_id.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.product_id.to_ne_bytes());
        bytes
    }
}

/// Fixed-size window event record returned by `get_window_event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct WindowEvent {
    pub kind: u32,
    pub flags: u32,
    pub window_id: u64,
    pub data: [u64; 14],
}

impl WindowEvent {
    /// Size accepted from clients built against ABI version 0.3.
    /// This value remains fixed when fields are appended in later versions.
    pub const MIN_BYTE_SIZE: usize = 128;
    pub const BYTE_SIZE: usize = 128;

    pub fn to_ne_bytes(self) -> [u8; Self::BYTE_SIZE] {
        let mut bytes = [0; Self::BYTE_SIZE];
        bytes[0..4].copy_from_slice(&self.kind.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.flags.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.window_id.to_ne_bytes());
        for (index, value) in self.data.iter().enumerate() {
            let start = 16 + index * 8;
            bytes[start..start + 8].copy_from_slice(&value.to_ne_bytes());
        }
        bytes
    }
}

const _: () = {
    assert!(core::mem::size_of::<AbiVersion>() == 8);
    assert!(core::mem::align_of::<AbiVersion>() == 2);
    assert!(core::mem::size_of::<CapabilitySet>() == 8);
    assert!(core::mem::align_of::<CapabilitySet>() == 8);
    assert!(core::mem::size_of::<AbiInfo>() == AbiInfo::BYTE_SIZE);
    assert!(AbiInfo::MIN_BYTE_SIZE <= AbiInfo::BYTE_SIZE);
    assert!(core::mem::align_of::<AbiInfo>() == 8);
    assert!(core::mem::size_of::<MemoryInfo>() == MemoryInfo::BYTE_SIZE);
    assert!(MemoryInfo::MIN_BYTE_SIZE <= MemoryInfo::BYTE_SIZE);
    assert!(core::mem::align_of::<MemoryInfo>() == 8);
    assert!(core::mem::size_of::<TimeSpec>() == TimeSpec::BYTE_SIZE);
    assert!(core::mem::align_of::<TimeSpec>() == 8);
    assert!(core::mem::size_of::<DeviceInfo>() == DeviceInfo::BYTE_SIZE);
    assert!(core::mem::align_of::<DeviceInfo>() == 4);
    assert!(core::mem::size_of::<WindowEvent>() == WindowEvent::BYTE_SIZE);
    assert!(WindowEvent::MIN_BYTE_SIZE <= WindowEvent::BYTE_SIZE);
    assert!(core::mem::align_of::<WindowEvent>() == 8);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_are_unique_and_round_trip() {
        for (index, number) in SyscallNumber::ALL.iter().copied().enumerate() {
            assert_eq!(SyscallNumber::try_from(number.as_u64()), Ok(number));
            assert!(
                SyscallNumber::ALL[..index]
                    .iter()
                    .all(|other| other.as_u64() != number.as_u64())
            );
        }
        assert!(SyscallNumber::try_from(u64::MAX).is_err());
    }

    #[test]
    fn error_codes_are_unique_and_round_trip() {
        for (index, code) in SyscallErrorCode::ALL.iter().copied().enumerate() {
            assert_eq!(SyscallErrorCode::try_from(code.as_i64()), Ok(code));
            assert!(
                SyscallErrorCode::ALL[..index]
                    .iter()
                    .all(|other| other.as_i64() != code.as_i64())
            );
        }
    }

    #[test]
    fn version_packing_is_backwards_compatible() {
        assert_eq!(
            AbiVersion::unpack(AbiVersion::CURRENT.pack()),
            AbiVersion::CURRENT
        );
    }

    #[test]
    fn abi_info_serialization_matches_repr_c_layout() {
        let info = AbiInfo::new(CapabilitySet::ALL_DEFINED);
        let bytes = info.to_ne_bytes();
        assert_eq!(
            u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
            AbiInfo::BYTE_SIZE as u32
        );
        assert_eq!(
            u64::from_ne_bytes(bytes[16..24].try_into().unwrap()),
            CapabilitySet::ALL_DEFINED.bits()
        );
        assert!(info.capabilities.contains(Capability::NativeSyscall));
    }
}
