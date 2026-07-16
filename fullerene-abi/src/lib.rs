#![no_std]

//! Stable data types shared by the Fullerene kernel and user-space SDK.
//!
//! This crate intentionally has no dependencies. Everything that crosses the
//! syscall boundary lives here so the kernel and SDK cannot silently drift.

use core::convert::TryFrom;

/// A Fullerene native syscall number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
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
    pub const ALL: &'static [Self] = &[
        Self::AbiQuery,
        Self::Exit,
        Self::Fork,
        Self::Read,
        Self::Write,
        Self::Open,
        Self::Close,
        Self::Wait,
        Self::GetPid,
        Self::GetProcessName,
        Self::Yield,
        Self::MapMemory,
        Self::UnmapMemory,
        Self::ProtectMemory,
        Self::QueryMemory,
        Self::CreateEvent,
        Self::WaitEvent,
        Self::SignalEvent,
        Self::SubscribeEvent,
        Self::CreateThread,
        Self::JoinThread,
        Self::DetachThread,
        Self::ExitThread,
        Self::CreateWindow,
        Self::DestroyWindow,
        Self::ResizeWindow,
        Self::PresentWindow,
        Self::GetWindowEvent,
        Self::EnumerateDevices,
        Self::OpenDevice,
        Self::DeviceIoctl,
        Self::ChannelCreate,
        Self::ChannelSend,
        Self::ChannelRecv,
        Self::PipeCreate,
        Self::HandleTransfer,
        Self::HandleDuplicate,
        Self::HandleRevoke,
        Self::ClockGetTime,
        Self::TimerCreate,
        Self::Sleep,
        Self::Uptime,
    ];

    #[inline]
    pub const fn as_u64(self) -> u64 {
        self as u64
    }
}

impl TryFrom<u64> for SyscallNumber {
    type Error = ();

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            syscall_numbers::ABI_QUERY => Ok(Self::AbiQuery),
            syscall_numbers::EXIT => Ok(Self::Exit),
            syscall_numbers::FORK => Ok(Self::Fork),
            syscall_numbers::READ => Ok(Self::Read),
            syscall_numbers::WRITE => Ok(Self::Write),
            syscall_numbers::OPEN => Ok(Self::Open),
            syscall_numbers::CLOSE => Ok(Self::Close),
            syscall_numbers::WAIT => Ok(Self::Wait),
            syscall_numbers::GETPID => Ok(Self::GetPid),
            syscall_numbers::GET_PROCESS_NAME => Ok(Self::GetProcessName),
            syscall_numbers::YIELD => Ok(Self::Yield),
            syscall_numbers::MAP_MEMORY => Ok(Self::MapMemory),
            syscall_numbers::UNMAP_MEMORY => Ok(Self::UnmapMemory),
            syscall_numbers::PROTECT_MEMORY => Ok(Self::ProtectMemory),
            syscall_numbers::QUERY_MEMORY => Ok(Self::QueryMemory),
            syscall_numbers::CREATE_EVENT => Ok(Self::CreateEvent),
            syscall_numbers::WAIT_EVENT => Ok(Self::WaitEvent),
            syscall_numbers::SIGNAL_EVENT => Ok(Self::SignalEvent),
            syscall_numbers::SUBSCRIBE_EVENT => Ok(Self::SubscribeEvent),
            syscall_numbers::CREATE_THREAD => Ok(Self::CreateThread),
            syscall_numbers::JOIN_THREAD => Ok(Self::JoinThread),
            syscall_numbers::DETACH_THREAD => Ok(Self::DetachThread),
            syscall_numbers::EXIT_THREAD => Ok(Self::ExitThread),
            syscall_numbers::CREATE_WINDOW => Ok(Self::CreateWindow),
            syscall_numbers::DESTROY_WINDOW => Ok(Self::DestroyWindow),
            syscall_numbers::RESIZE_WINDOW => Ok(Self::ResizeWindow),
            syscall_numbers::PRESENT_WINDOW => Ok(Self::PresentWindow),
            syscall_numbers::GET_WINDOW_EVENT => Ok(Self::GetWindowEvent),
            syscall_numbers::ENUMERATE_DEVICES => Ok(Self::EnumerateDevices),
            syscall_numbers::OPEN_DEVICE => Ok(Self::OpenDevice),
            syscall_numbers::DEVICE_IOCTL => Ok(Self::DeviceIoctl),
            syscall_numbers::CHANNEL_CREATE => Ok(Self::ChannelCreate),
            syscall_numbers::CHANNEL_SEND => Ok(Self::ChannelSend),
            syscall_numbers::CHANNEL_RECV => Ok(Self::ChannelRecv),
            syscall_numbers::PIPE_CREATE => Ok(Self::PipeCreate),
            syscall_numbers::HANDLE_TRANSFER => Ok(Self::HandleTransfer),
            syscall_numbers::HANDLE_DUPLICATE => Ok(Self::HandleDuplicate),
            syscall_numbers::HANDLE_REVOKE => Ok(Self::HandleRevoke),
            syscall_numbers::CLOCK_GETTIME => Ok(Self::ClockGetTime),
            syscall_numbers::TIMER_CREATE => Ok(Self::TimerCreate),
            syscall_numbers::SLEEP => Ok(Self::Sleep),
            syscall_numbers::UPTIME => Ok(Self::Uptime),
            _ => Err(()),
        }
    }
}

/// Compatibility constants for code that matches on raw syscall numbers.
pub mod syscall_numbers {
    use super::SyscallNumber;

    pub const ABI_QUERY: u64 = SyscallNumber::AbiQuery.as_u64();
    pub const ABI_VERSION: u64 = ABI_QUERY;
    pub const EXIT: u64 = SyscallNumber::Exit.as_u64();
    pub const FORK: u64 = SyscallNumber::Fork.as_u64();
    pub const READ: u64 = SyscallNumber::Read.as_u64();
    pub const WRITE: u64 = SyscallNumber::Write.as_u64();
    pub const OPEN: u64 = SyscallNumber::Open.as_u64();
    pub const CLOSE: u64 = SyscallNumber::Close.as_u64();
    pub const WAIT: u64 = SyscallNumber::Wait.as_u64();
    pub const GETPID: u64 = SyscallNumber::GetPid.as_u64();
    pub const GET_PROCESS_NAME: u64 = SyscallNumber::GetProcessName.as_u64();
    pub const YIELD: u64 = SyscallNumber::Yield.as_u64();
    pub const MAP_MEMORY: u64 = SyscallNumber::MapMemory.as_u64();
    pub const UNMAP_MEMORY: u64 = SyscallNumber::UnmapMemory.as_u64();
    pub const PROTECT_MEMORY: u64 = SyscallNumber::ProtectMemory.as_u64();
    pub const QUERY_MEMORY: u64 = SyscallNumber::QueryMemory.as_u64();
    pub const CREATE_EVENT: u64 = SyscallNumber::CreateEvent.as_u64();
    pub const WAIT_EVENT: u64 = SyscallNumber::WaitEvent.as_u64();
    pub const SIGNAL_EVENT: u64 = SyscallNumber::SignalEvent.as_u64();
    pub const SUBSCRIBE_EVENT: u64 = SyscallNumber::SubscribeEvent.as_u64();
    pub const CREATE_THREAD: u64 = SyscallNumber::CreateThread.as_u64();
    pub const JOIN_THREAD: u64 = SyscallNumber::JoinThread.as_u64();
    pub const DETACH_THREAD: u64 = SyscallNumber::DetachThread.as_u64();
    pub const EXIT_THREAD: u64 = SyscallNumber::ExitThread.as_u64();
    pub const CREATE_WINDOW: u64 = SyscallNumber::CreateWindow.as_u64();
    pub const DESTROY_WINDOW: u64 = SyscallNumber::DestroyWindow.as_u64();
    pub const RESIZE_WINDOW: u64 = SyscallNumber::ResizeWindow.as_u64();
    pub const PRESENT_WINDOW: u64 = SyscallNumber::PresentWindow.as_u64();
    pub const GET_WINDOW_EVENT: u64 = SyscallNumber::GetWindowEvent.as_u64();
    pub const ENUMERATE_DEVICES: u64 = SyscallNumber::EnumerateDevices.as_u64();
    pub const OPEN_DEVICE: u64 = SyscallNumber::OpenDevice.as_u64();
    pub const DEVICE_IOCTL: u64 = SyscallNumber::DeviceIoctl.as_u64();
    pub const CHANNEL_CREATE: u64 = SyscallNumber::ChannelCreate.as_u64();
    pub const CHANNEL_SEND: u64 = SyscallNumber::ChannelSend.as_u64();
    pub const CHANNEL_RECV: u64 = SyscallNumber::ChannelRecv.as_u64();
    pub const PIPE_CREATE: u64 = SyscallNumber::PipeCreate.as_u64();
    pub const HANDLE_TRANSFER: u64 = SyscallNumber::HandleTransfer.as_u64();
    pub const HANDLE_DUPLICATE: u64 = SyscallNumber::HandleDuplicate.as_u64();
    pub const HANDLE_REVOKE: u64 = SyscallNumber::HandleRevoke.as_u64();
    pub const CLOCK_GETTIME: u64 = SyscallNumber::ClockGetTime.as_u64();
    pub const TIMER_CREATE: u64 = SyscallNumber::TimerCreate.as_u64();
    pub const SLEEP: u64 = SyscallNumber::Sleep.as_u64();
    pub const UPTIME: u64 = SyscallNumber::Uptime.as_u64();
}

/// A positive error code returned as its negated value from a syscall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
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
    pub const ALL: &'static [Self] = &[
        Self::InvalidSyscall,
        Self::FileNotFound,
        Self::NoSuchProcess,
        Self::Io,
        Self::BadFileDescriptor,
        Self::Again,
        Self::OutOfMemory,
        Self::PermissionDenied,
        Self::AddressFault,
        Self::Busy,
        Self::AlreadyExists,
        Self::NoSuchDevice,
        Self::NotADirectory,
        Self::IsADirectory,
        Self::InvalidArgument,
        Self::NoSpace,
        Self::DirectoryNotEmpty,
        Self::Overflow,
        Self::NotSupported,
        Self::BadHandle,
        Self::TimedOut,
        Self::WouldBlock,
    ];

    #[inline]
    pub const fn as_i64(self) -> i64 {
        self as i64
    }
}

impl TryFrom<i64> for SyscallErrorCode {
    type Error = ();

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.as_i64() == value)
            .ok_or(())
    }
}

/// Compatibility constants for raw error-code users.
pub mod syscall_errors {
    use super::SyscallErrorCode;

    pub const INVALID_SYSCALL: i64 = SyscallErrorCode::InvalidSyscall.as_i64();
    pub const FILE_NOT_FOUND: i64 = SyscallErrorCode::FileNotFound.as_i64();
    pub const NO_SUCH_PROCESS: i64 = SyscallErrorCode::NoSuchProcess.as_i64();
    pub const IO_ERROR: i64 = SyscallErrorCode::Io.as_i64();
    pub const BAD_FILE_DESCRIPTOR: i64 = SyscallErrorCode::BadFileDescriptor.as_i64();
    pub const AGAIN: i64 = SyscallErrorCode::Again.as_i64();
    pub const OUT_OF_MEMORY: i64 = SyscallErrorCode::OutOfMemory.as_i64();
    pub const PERMISSION_DENIED: i64 = SyscallErrorCode::PermissionDenied.as_i64();
    pub const ADDRESS_FAULT: i64 = SyscallErrorCode::AddressFault.as_i64();
    pub const BUSY: i64 = SyscallErrorCode::Busy.as_i64();
    pub const ALREADY_EXISTS: i64 = SyscallErrorCode::AlreadyExists.as_i64();
    pub const NO_SUCH_DEVICE: i64 = SyscallErrorCode::NoSuchDevice.as_i64();
    pub const NOT_A_DIRECTORY: i64 = SyscallErrorCode::NotADirectory.as_i64();
    pub const IS_A_DIRECTORY: i64 = SyscallErrorCode::IsADirectory.as_i64();
    pub const INVALID_ARGUMENT: i64 = SyscallErrorCode::InvalidArgument.as_i64();
    pub const NO_SPACE: i64 = SyscallErrorCode::NoSpace.as_i64();
    pub const DIRECTORY_NOT_EMPTY: i64 = SyscallErrorCode::DirectoryNotEmpty.as_i64();
    pub const OVERFLOW: i64 = SyscallErrorCode::Overflow.as_i64();
    pub const NOT_SUPPORTED: i64 = SyscallErrorCode::NotSupported.as_i64();
    pub const BAD_HANDLE: i64 = SyscallErrorCode::BadHandle.as_i64();
    pub const TIMED_OUT: i64 = SyscallErrorCode::TimedOut.as_i64();
    pub const WOULD_BLOCK: i64 = SyscallErrorCode::WouldBlock.as_i64();
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
        minor: 3,
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
            | Capability::DeviceEnumeration.bit(),
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
    /// Size accepted from clients built against ABI version 0.3.
    /// This value remains fixed when fields are appended in later versions.
    pub const MIN_BYTE_SIZE: usize = 40;
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
