#![cfg_attr(any(target_os = "none", target_os = "uefi"), no_std)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

//! DriverKit — Fullerene user-space driver IPC client.
//!
//! The public surface is deliberately a C ABI.  Handles and message buffers
//! cross the process boundary as integers and pointer/length pairs; Rust
//! references, allocation-bearing types, and Rust error types do not cross
//! this boundary.
//!
//! The kernel remains the authority for validating user mappings and device
//! capabilities.  This crate does not manufacture a [`sealant`] capability
//! from an untrusted C pointer.  Sealant-backed mapped-buffer APIs belong to a
//! later capability-grant layer, after the kernel has established the mapping.

use fullerene_abi::{
    AbiInfo, BlockDeviceInfo, BlockRequest, DeviceCapabilityInfo, IpcBufferDescriptor,
    IpcMessageHeader,
};
use petroleum::common::syscall::syscall;

#[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
#[panic_handler]
fn panic_handler(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Successful operation status.
pub const DRIVERKIT_OK: i32 = 0;
/// Invalid argument status.
pub const DRIVERKIT_INVALID_ARGUMENT: i32 = 22;
/// A user buffer or address was invalid.
pub const DRIVERKIT_ADDRESS_FAULT: i32 = 14;
/// The requested device does not exist.
pub const DRIVERKIT_NO_SUCH_DEVICE: i32 = 19;
/// The handle is invalid or stale.
pub const DRIVERKIT_BAD_HANDLE: i32 = 104;
/// The operation is not supported by this kernel or device.
pub const DRIVERKIT_NOT_SUPPORTED: i32 = 95;
/// The operation would block.
pub const DRIVERKIT_WOULD_BLOCK: i32 = 140;
/// The channel or request queue is full.
pub const DRIVERKIT_AGAIN: i32 = 11;
/// A capability cannot be revoked while it still has a live mapping.
pub const DRIVERKIT_BUSY: i32 = 16;
/// The operation failed for an unspecified reason.
pub const DRIVERKIT_UNKNOWN_ERROR: i32 = 0x7fff;

/// Maximum message size accepted by the current channel syscall.
pub const DRIVERKIT_MAX_CHANNEL_MESSAGE_SIZE: usize = fullerene_abi::IPC_CHANNEL_MAX_MESSAGE_SIZE;
/// Maximum NUL-terminated device identifier length accepted by the kernel.
pub const DRIVERKIT_MAX_DEVICE_ID_SIZE: usize = 128;
/// Fixed size of the versioned IPC envelope header.
pub const DRIVERKIT_IPC_MESSAGE_HEADER_SIZE: usize = IpcMessageHeader::BYTE_SIZE;
/// Fixed size of an IPC shared-buffer descriptor.
pub const DRIVERKIT_IPC_BUFFER_DESCRIPTOR_SIZE: usize = IpcBufferDescriptor::BYTE_SIZE;
/// Shared-buffer mapping may be read.
pub const DRIVERKIT_SHARED_BUFFER_READ: u64 = fullerene_abi::shared_buffer_flags::READ;
/// Shared-buffer mapping may be written.
pub const DRIVERKIT_SHARED_BUFFER_WRITE: u64 = fullerene_abi::shared_buffer_flags::WRITE;
/// The allocation is explicitly documented as zero-initialized.
pub const DRIVERKIT_SHARED_BUFFER_ZEROED: u64 = fullerene_abi::shared_buffer_flags::ZEROED;

/// Result returned by scalar DriverKit functions.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitResult {
    /// Zero on success, otherwise a positive DriverKit status code.
    pub status: i32,
    /// Operation-specific value.  Only valid when `status == DRIVERKIT_OK`.
    pub value: u64,
}

/// ABI version returned by [`driverkit_abi_query`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitAbiVersion {
    /// Major ABI version.
    pub major: u16,
    /// Minor ABI version.
    pub minor: u16,
    /// Patch ABI version.
    pub patch: u16,
    /// Reserved for future use; must be zero when sent by a client.
    pub reserved: u16,
}

/// ABI information returned by [`driverkit_abi_query`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitAbiInfo {
    /// Kernel ABI version.
    pub version: DriverKitAbiVersion,
    /// Size of this structure understood by the kernel.
    pub struct_size: u32,
    /// Number of syscall entries advertised by the kernel.
    pub syscall_count: u32,
    /// Kernel capability bitset.
    pub capabilities: u64,
    /// Reserved for ABI extension.
    pub reserved: [u64; 2],
}

/// ABI query result.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitAbiResult {
    /// Operation status.
    pub status: i32,
    /// ABI information; meaningful only when `status == DRIVERKIT_OK`.
    pub info: DriverKitAbiInfo,
}

/// Device record returned by [`driverkit_enumerate_devices`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitDeviceInfo {
    /// Device class, using `fullerene_abi::device_class` values.
    pub class: u32,
    /// Stable device identifier.
    pub device_id: u32,
    /// PCI vendor identifier, or zero for non-PCI devices.
    pub vendor_id: u32,
    /// PCI product identifier, or zero for non-PCI devices.
    pub product_id: u32,
}

/// Device capability query result.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitDeviceCapabilitiesResult {
    /// Operation status.
    pub status: i32,
    /// Device class.
    pub class: u32,
    /// Reserved for ABI extension.
    pub reserved: u32,
    /// Device operation capability bitset.
    pub capabilities: u64,
}

/// Block-device geometry query result.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitBlockInfoResult {
    /// Operation status.
    pub status: i32,
    /// Sector size in bytes.
    pub sector_size: u32,
    /// Reserved for ABI extension.
    pub reserved: u32,
    /// Number of addressable sectors.
    pub total_sectors: u64,
}

/// Shared-buffer capability descriptor embedded in an IPC payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DriverKitIpcBufferDescriptor {
    /// Shared-buffer capability handle.
    pub handle: u64,
    /// Byte offset from the beginning of the buffer.
    pub offset: u64,
    /// Number of bytes referenced.
    pub length: u64,
    /// Descriptor rights.
    pub flags: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
}

#[inline]
const fn ok(value: u64) -> DriverKitResult {
    DriverKitResult {
        status: DRIVERKIT_OK,
        value,
    }
}

#[inline]
const fn error(status: i32) -> DriverKitResult {
    DriverKitResult { status, value: 0 }
}

#[inline]
const fn status_from_errno(errno: i64) -> i32 {
    if errno > 0 && errno <= i32::MAX as i64 {
        // Preserve the kernel's positive errno value, including codes added
        // by a newer ABI.  The named constants above cover the common codes;
        // callers may still inspect any future numeric status directly.
        errno as i32
    } else {
        DRIVERKIT_UNKNOWN_ERROR
    }
}

#[inline]
fn call(number: fullerene_abi::SyscallNumber, arg1: u64, arg2: u64, arg3: u64) -> DriverKitResult {
    let raw = unsafe { syscall(number.as_u64(), arg1, arg2, arg3, 0, 0, 0) };
    let signed = raw as i64;
    if signed < 0 {
        error(status_from_errno(-signed))
    } else {
        ok(raw)
    }
}

#[inline]
fn check_buffer(ptr: *const u8, len: usize, max: usize) -> Result<(), i32> {
    if ptr.is_null() || len == 0 || len > max {
        return Err(DRIVERKIT_INVALID_ARGUMENT);
    }
    if (ptr as usize).checked_add(len).is_none() {
        return Err(DRIVERKIT_ADDRESS_FAULT);
    }
    Ok(())
}

/// Query the kernel ABI version and capabilities.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_abi_query() -> DriverKitAbiResult {
    let mut info = AbiInfo::EMPTY;
    let result = call(
        fullerene_abi::SyscallNumber::AbiQuery,
        (&mut info as *mut AbiInfo) as u64,
        AbiInfo::BYTE_SIZE as u64,
        0,
    );
    if result.status != DRIVERKIT_OK {
        return DriverKitAbiResult {
            status: result.status,
            info: DriverKitAbiInfo::default(),
        };
    }
    DriverKitAbiResult {
        status: DRIVERKIT_OK,
        info: DriverKitAbiInfo {
            version: DriverKitAbiVersion {
                major: info.version.major,
                minor: info.version.minor,
                patch: info.version.patch,
                reserved: info.version.reserved,
            },
            struct_size: info.struct_size,
            syscall_count: info.syscall_count,
            capabilities: info.capabilities.bits(),
            reserved: info.reserved,
        },
    }
}

/// Enumerate devices of `class` into a caller-owned array.
///
/// The returned value is the total number of matching records.  Only the
/// first `capacity` records are written, so callers can retry with a larger
/// buffer when the returned value exceeds `capacity`.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_enumerate_devices(
    class: u32,
    devices: *mut DriverKitDeviceInfo,
    capacity: usize,
) -> DriverKitResult {
    let bytes = match capacity.checked_mul(core::mem::size_of::<DriverKitDeviceInfo>()) {
        Some(bytes) => bytes,
        None => return error(DRIVERKIT_INVALID_ARGUMENT),
    };
    if check_buffer(devices as *const u8, bytes, 1 << 20).is_err() {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    call(
        fullerene_abi::SyscallNumber::EnumerateDevices,
        class as u64,
        devices as u64,
        bytes as u64,
    )
}

/// Open a device by its NUL-terminated `/dev` name or stable identifier.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_open_device(device_id: *const u8) -> DriverKitResult {
    if device_id.is_null() {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    call(
        fullerene_abi::SyscallNumber::OpenDevice,
        device_id as u64,
        0,
        0,
    )
}

/// Query capabilities for an opened device handle.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_device_capabilities(handle: u64) -> DriverKitDeviceCapabilitiesResult {
    let mut info = DeviceCapabilityInfo::default();
    let result = call(
        fullerene_abi::SyscallNumber::DeviceIoctl,
        handle,
        fullerene_abi::device_ioctl::GET_CAPABILITIES,
        (&mut info as *mut DeviceCapabilityInfo) as u64,
    );
    DriverKitDeviceCapabilitiesResult {
        status: result.status,
        class: info.class,
        reserved: info.reserved,
        capabilities: info.capabilities,
    }
}

/// Query geometry for an opened block-device handle.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_block_info(handle: u64) -> DriverKitBlockInfoResult {
    let mut info = BlockDeviceInfo::default();
    let result = call(
        fullerene_abi::SyscallNumber::DeviceIoctl,
        handle,
        fullerene_abi::device_ioctl::GET_BLOCK_INFO,
        (&mut info as *mut BlockDeviceInfo) as u64,
    );
    DriverKitBlockInfoResult {
        status: result.status,
        sector_size: info.sector_size,
        reserved: info.reserved,
        total_sectors: info.total_sectors,
    }
}

fn block_io(
    handle: u64,
    lba: u64,
    count: u16,
    buffer: *const u8,
    buffer_len: usize,
    write: bool,
) -> DriverKitResult {
    if count == 0 || buffer_len > u32::MAX as usize {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    if check_buffer(buffer as *const u8, buffer_len, u32::MAX as usize).is_err() {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    let request = BlockRequest {
        lba,
        count,
        reserved: 0,
        buffer_len: buffer_len as u32,
        buffer_ptr: buffer as u64,
    };
    call(
        fullerene_abi::SyscallNumber::DeviceIoctl,
        handle,
        if write {
            fullerene_abi::device_ioctl::WRITE_BLOCKS
        } else {
            fullerene_abi::device_ioctl::READ_BLOCKS
        },
        (&request as *const BlockRequest) as u64,
    )
}

/// Read sectors from an opened block-device handle into `buffer`.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_block_read(
    handle: u64,
    lba: u64,
    count: u16,
    buffer: *mut u8,
    buffer_len: usize,
) -> DriverKitResult {
    block_io(handle, lba, count, buffer, buffer_len, false)
}

/// Write sectors from `buffer` to an opened block-device handle.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_block_write(
    handle: u64,
    lba: u64,
    count: u16,
    buffer: *const u8,
    buffer_len: usize,
) -> DriverKitResult {
    block_io(handle, lba, count, buffer, buffer_len, true)
}

/// Create a kernel IPC channel.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_channel_create(flags: u64) -> DriverKitResult {
    call(fullerene_abi::SyscallNumber::ChannelCreate, flags, 0, 0)
}

/// Send one message through a kernel IPC channel.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_channel_send(
    handle: u64,
    data: *const u8,
    length: usize,
) -> DriverKitResult {
    if check_buffer(data, length, DRIVERKIT_MAX_CHANNEL_MESSAGE_SIZE).is_err() {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    call(
        fullerene_abi::SyscallNumber::ChannelSend,
        handle,
        data as u64,
        length as u64,
    )
}

/// Receive one message from a kernel IPC channel without blocking.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_channel_recv(
    handle: u64,
    buffer: *mut u8,
    capacity: usize,
) -> DriverKitResult {
    if check_buffer(
        buffer as *const u8,
        capacity,
        DRIVERKIT_MAX_CHANNEL_MESSAGE_SIZE,
    )
    .is_err()
    {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    call(
        fullerene_abi::SyscallNumber::ChannelRecv,
        handle,
        buffer as u64,
        capacity as u64,
    )
}

/// Allocate a zeroed kernel-owned shared buffer and return its capability.
///
/// `length` is rounded up to a page boundary by the kernel.  The returned
/// value is a handle, not a pointer; call [`driverkit_shared_buffer_map`] to
/// obtain a process-local mapping.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_shared_buffer_create(length: usize, flags: u64) -> DriverKitResult {
    if length == 0 {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    call(
        fullerene_abi::SyscallNumber::SharedBufferCreate,
        length as u64,
        flags,
        0,
    )
}

/// Map a shared-buffer capability into the current process.
///
/// Pass zero for `address_hint` to let the kernel choose a page-aligned user
/// address.  The result value is the mapped address.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_shared_buffer_map(
    handle: u64,
    address_hint: u64,
    flags: u64,
) -> DriverKitResult {
    call(
        fullerene_abi::SyscallNumber::SharedBufferMap,
        handle,
        address_hint,
        flags,
    )
}

/// Unmap one process-local mapping while retaining the capability handle.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_shared_buffer_unmap(handle: u64, address: u64) -> DriverKitResult {
    call(
        fullerene_abi::SyscallNumber::SharedBufferUnmap,
        handle,
        address,
        0,
    )
}

/// Send one versioned IPC message through a kernel channel.
///
/// `message` must point to one contiguous buffer containing an
/// [`IpcMessageHeader`] followed by its payload. The kernel channel remains a
/// byte transport; service implementations validate the envelope and opcode.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_message_send(
    handle: u64,
    message: *const u8,
    length: usize,
) -> DriverKitResult {
    if length < DRIVERKIT_IPC_MESSAGE_HEADER_SIZE
        || check_buffer(message, length, DRIVERKIT_MAX_CHANNEL_MESSAGE_SIZE).is_err()
    {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    call(
        fullerene_abi::SyscallNumber::ChannelSend,
        handle,
        message as u64,
        length as u64,
    )
}

/// Receive one versioned IPC message from a kernel channel without blocking.
///
/// The returned value is the number of bytes copied. The caller should parse
/// the fixed header, verify [`IpcMessageHeader::is_valid`], and then enforce
/// `header.total_size() <= returned_value` before reading the payload.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_message_recv(
    handle: u64,
    message: *mut u8,
    capacity: usize,
) -> DriverKitResult {
    if capacity < DRIVERKIT_IPC_MESSAGE_HEADER_SIZE
        || check_buffer(
            message as *const u8,
            capacity,
            DRIVERKIT_MAX_CHANNEL_MESSAGE_SIZE,
        )
        .is_err()
    {
        return error(DRIVERKIT_INVALID_ARGUMENT);
    }
    call(
        fullerene_abi::SyscallNumber::ChannelRecv,
        handle,
        message as u64,
        capacity as u64,
    )
}

/// Revoke and close a kernel handle.
#[unsafe(no_mangle)]
pub extern "C" fn driverkit_handle_revoke(handle: u64) -> DriverKitResult {
    call(fullerene_abi::SyscallNumber::HandleRevoke, handle, 0, 0)
}

const _: () = {
    assert!(core::mem::size_of::<DriverKitDeviceInfo>() == fullerene_abi::DeviceInfo::BYTE_SIZE);
    assert!(core::mem::size_of::<DriverKitAbiInfo>() == AbiInfo::BYTE_SIZE);
    assert!(core::mem::size_of::<DriverKitIpcBufferDescriptor>() == IpcBufferDescriptor::BYTE_SIZE);
};
