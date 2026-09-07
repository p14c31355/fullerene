//! Bounded AArch64 native filesystem boundary.
//!
//! This is the first storage layer used by the native launchd payload. It
//! starts with Genome's in-memory VFS, but the syscall contract is real:
//! paths and buffers cross the EL0 copy boundary, descriptors are owned by
//! the current PID, and the backing VFS can later be replaced by an
//! initramfs/FAT mount without changing OPEN/READ/CLOSE dispatch.

use alloc::boxed::Box;
#[cfg(fullerene_aarch64_bramble)]
use alloc::string::String;
#[cfg(fullerene_aarch64_bramble)]
use genome::android_fs::{self, AndroidFilesystemKind};
#[cfg(fullerene_aarch64_bramble)]
use genome::block::{BlockDevice, Sector512Device};
use genome::fs::FsError;
use genome::vfs::{
    FileDescriptor, FileMetadata, FileSystem, FileSystemCapabilities, InodeType, MemFileSystem,
    VNode, Vfs,
};
use spin::Mutex;

#[cfg(fullerene_aarch64_bramble)]
use super::ufs;
use super::{
    allocator, devices, exceptions::Aarch64TrapFrame, mmu, task, uart, user_memory, window,
};

const ERR_BAD_FD: u64 = (-(9i64)) as u64;
const ERR_ADDRESS: u64 = (-(14i64)) as u64;
const ERR_NO_ENTRY: u64 = (-(2i64)) as u64;
const ERR_WOULD_BLOCK: u64 = (-(11i64)) as u64;
const ERR_PERMISSION: u64 = (-(13i64)) as u64;
const ERR_NOT_SUPPORTED: u64 = (-(95i64)) as u64;
const ERR_ADDRESS_IN_USE: u64 = (-(98i64)) as u64;
const ERR_NOT_CONNECTED: u64 = (-(107i64)) as u64;
const ERR_NO_PROTOCOL: u64 = (-(92i64)) as u64;
const ERR_NOT_DIRECTORY: u64 = (-(20i64)) as u64;
const ERR_INVALID: u64 = (-(22i64)) as u64;
const ERR_OVERFLOW: u64 = (-(75i64)) as u64;
const ERR_NAME_TOO_LONG: u64 = (-(36i64)) as u64;
const ERR_OUT_OF_MEMORY: u64 = (-(12i64)) as u64;
const ERR_TOO_MANY_FILES: u64 = (-(24i64)) as u64;
const ERR_IO: u64 = (-(5i64)) as u64;
const ERR_BUSY: u64 = (-(16i64)) as u64;
const ERR_NO_SPACE: u64 = (-(28i64)) as u64;
const FILE_HANDLE_TAG: u64 = 1 << 62;
const FILE_HANDLE_INDEX_BITS: u64 = 8;
const FILE_HANDLE_INDEX_MASK: u64 = (1 << FILE_HANDLE_INDEX_BITS) - 1;
const FILE_HANDLE_GENERATION_SHIFT: u64 = FILE_HANDLE_INDEX_BITS;
const MAX_OPEN_FILES: usize = 32;
const MAX_LINUX_FDS_PER_PROCESS: usize = 32;
const MAX_LINUX_FD_ENTRIES: usize = task::MAX_TASKS * MAX_LINUX_FDS_PER_PROCESS;
const LINUX_FD_MIN: u32 = 3;
const LINUX_FD_MAX: u32 = LINUX_FD_MIN + MAX_LINUX_FDS_PER_PROCESS as u32 - 1;
const LINUX_STDIO_NONE: u8 = u8::MAX;
const LINUX_FD_NATIVE: u8 = 0;
const LINUX_FD_SOCKET: u8 = 1;
const LINUX_FD_EPOLL: u8 = 2;
const LINUX_FD_EVENTFD: u8 = 3;
const LINUX_FD_INOTIFY: u8 = 4;
const LINUX_FD_SIGNALFD: u8 = 5;
const LINUX_FD_CLOEXEC: u32 = 0x80000;
const MAX_HANDLE_SLOTS: usize = 16;
const MAX_PATH: usize = 256;
const MAX_READ: usize = 4096;
const DEBUG_SYNC_FILE_PATH_CAPACITY: usize = 256;
const DEBUG_SYNC_FILE_DATA_CAPACITY: usize = 4096;
const PIPE_CAPACITY: usize = 4096;
const MAX_PIPE_SLOTS: usize = 8;
const CHANNEL_MESSAGE_CAPACITY: usize = 4096;
const MAX_CHANNEL_MESSAGES: usize = 16;
const MAX_CHANNEL_SLOTS: usize = 8;
const MAX_EVENT_SLOTS: usize = 8;
const MAX_TIMER_SLOTS: usize = 8;
const MAX_THREAD_SLOTS: usize = 8;
const MAX_SHARED_BUFFER_SLOTS: usize = 4;
const MAX_SHARED_BUFFER_PAGES: usize = 64;
const MAX_SHARED_MAPPINGS: usize = 16;
const ANDROID_PROPERTY_AREA_PAGES: usize = 32;
const MAX_LINUX_PROPERTY_MAPPINGS: usize = 16;
const MAX_TERMINAL_SLOTS: usize = 8;
const MAX_DIRECTORY_SLOTS: usize = MAX_OPEN_FILES;
const MAX_LINUX_SOCKET_SLOTS: usize = 16;
const MAX_LINUX_EVENTFD_SLOTS: usize = 8;
const MAX_LINUX_INOTIFY_SLOTS: usize = 4;
const MAX_LINUX_SIGNALFD_SLOTS: usize = 4;
const SOCKET_BUFFER_CAPACITY: usize = 4096;
const SOCKET_PATH_CAPACITY: usize = 108;
const SOCKET_SLOT_NONE: u8 = u8::MAX;
const AF_UNIX: u64 = 1;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const SOCK_TYPE_MASK: u64 = 0xf;
const SOCK_NONBLOCK: u64 = 0x800;
const SOCK_CLOEXEC: u64 = 0x80000;
const SOL_SOCKET: u64 = 1;
const SO_TYPE: u64 = 3;
const SO_SNDBUF: u64 = 7;
const SO_RCVBUF: u64 = 8;
const SO_PASSCRED: u64 = 16;
const SO_REUSEADDR: u64 = 2;
const SO_REUSEPORT: u64 = 15;
const SO_KEEPALIVE: u64 = 9;
const SO_RCVTIMEO: u64 = 20;
const SO_SNDTIMEO: u64 = 21;
const EPOLL_SLOT_NONE: u8 = u8::MAX;
const MAX_EPOLL_SLOTS: usize = 8;
const MAX_EPOLL_WATCHES: usize = 16;
const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLL_CTL_MOD: u64 = 3;
const EPOLLIN: u32 = 0x001;
const EPOLLOUT: u32 = 0x004;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;
const POLLIN: u16 = 0x001;
const POLLOUT: u16 = 0x004;
const POLLERR: u16 = 0x008;
const POLLHUP: u16 = 0x010;
const POLLNVAL: u16 = 0x020;
const MAX_TERMINAL_TITLE: usize = 128;
const MAX_TASK_NAME: usize = 16;
const CPIO_HEADER_SIZE: usize = 110;
const MAX_INITRAMFS_ENTRIES: usize = 32;
const KIND_FILE: u8 = 0;
const KIND_PIPE_READ: u8 = 1;
const KIND_PIPE_WRITE: u8 = 2;
const KIND_CHANNEL: u8 = 3;
const KIND_EVENT: u8 = 4;
const KIND_SHARED_BUFFER: u8 = 5;
const KIND_TIMER: u8 = 6;
const KIND_THREAD: u8 = 7;
const KIND_TERMINAL: u8 = 8;
const KIND_DEVICE: u8 = 9;
const KIND_WINDOW: u8 = 10;
const KIND_DIRECTORY: u8 = 11;
const SELINUX_ATTR_NONE: u8 = 0;
const SELINUX_ATTR_CURRENT: u8 = 1;
const SELINUX_ATTR_EXEC: u8 = 2;
static INITRAMFS: &[u8] = include_bytes!(env!("FULLERENE_AARCH64_INITRAMFS"));

// The bounded ADB sync endpoint needs one writable landing zone for bring-up
// tools. Keep it explicitly in RAM and restrict it to diagnostic temp paths;
// this must never turn an ADB push into an implicit UFS/partition write.
static mut DEBUG_SYNC_FILE_PATH: [u8; DEBUG_SYNC_FILE_PATH_CAPACITY] =
    [0; DEBUG_SYNC_FILE_PATH_CAPACITY];
static mut DEBUG_SYNC_FILE_PATH_LENGTH: usize = 0;
static mut DEBUG_SYNC_FILE_DATA: [u8; DEBUG_SYNC_FILE_DATA_CAPACITY] =
    [0; DEBUG_SYNC_FILE_DATA_CAPACITY];
static mut DEBUG_SYNC_FILE_DATA_LENGTH: usize = 0;

#[derive(Clone, Copy)]
struct OpenFile {
    owner_pid: u64,
    handle_index: u16,
    local_fd: u32,
    mount_index: usize,
    generation: u64,
    kind: u8,
    pipe_slot: u8,
    selinux_attr: u8,
    active: bool,
}

#[derive(Clone, Copy)]
struct DirectorySlot {
    path: [u8; MAX_PATH],
    path_length: usize,
    references: u16,
    active: bool,
}

/// Linux's small integer descriptor namespace is layered over the native
/// generation-checked capability namespace. Keeping this table separate lets
/// Android/Linux code use fd 3, 4, ... without exposing native capability
/// tokens or changing the existing Fullerene ABI.
#[derive(Clone, Copy)]
struct LinuxFdEntry {
    owner_pid: u64,
    fd: u32,
    native_handle: u64,
    stdio_fd: u8,
    kind: u8,
    resource_slot: u8,
    flags: u32,
    active: bool,
}

#[derive(Clone, Copy)]
struct LinuxSocketSlot {
    buffer: [u8; SOCKET_BUFFER_CAPACITY],
    property_staging: [u8; 256],
    property_staging_length: usize,
    read_position: usize,
    write_position: usize,
    length: usize,
    peer: u8,
    references: u16,
    socket_type: u16,
    bound_path: [u8; SOCKET_PATH_CAPACITY],
    bound_length: usize,
    listening: bool,
    connected: bool,
    nonblocking: bool,
    passcred: bool,
    pending: u8,
    active: bool,
}

#[derive(Clone, Copy)]
struct LinuxEventFdSlot {
    counter: u64,
    semaphore: bool,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct LinuxInotifySlot {
    next_watch: i32,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct LinuxSignalFdSlot {
    mask: u64,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct EpollWatch {
    fd: u32,
    events: u32,
    data: u64,
    active: bool,
}

#[derive(Clone, Copy)]
struct EpollSlot {
    watches: [EpollWatch; MAX_EPOLL_WATCHES],
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct PipeSlot {
    buffer: [u8; PIPE_CAPACITY],
    read_position: usize,
    write_position: usize,
    length: usize,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct ChannelMessage {
    bytes: [u8; CHANNEL_MESSAGE_CAPACITY],
    length: usize,
}

#[derive(Clone, Copy)]
struct ChannelSlot {
    messages: [ChannelMessage; MAX_CHANNEL_MESSAGES],
    head: usize,
    length: usize,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct EventSlot {
    signaled: bool,
    manual_reset: bool,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct TimerSlot {
    event_owner_pid: u64,
    event_handle: u64,
    deadline_ns: u64,
    fired: bool,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct ThreadSlot {
    target_pid: u64,
    detached: bool,
    exited: bool,
    exit_status: u64,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct SharedBufferSlot {
    frames: [u64; MAX_SHARED_BUFFER_PAGES],
    page_count: usize,
    length: u64,
    flags: u64,
    references: u16,
    active: bool,
}

#[derive(Clone, Copy)]
struct SharedMapping {
    buffer_slot: u8,
    owner_pid: u64,
    address: u64,
    length: u64,
    active: bool,
}

#[derive(Clone, Copy)]
struct LinuxPropertyMapping {
    owner_pid: u64,
    address: u64,
    length: u64,
    active: bool,
}

#[derive(Clone, Copy)]
struct TerminalSlot {
    title: [u8; MAX_TERMINAL_TITLE],
    title_length: usize,
    references: u16,
    active: bool,
}

struct UserPath {
    bytes: [u8; MAX_PATH],
    length: usize,
}

impl UserPath {
    fn as_str(&self) -> Result<&str, u64> {
        core::str::from_utf8(&self.bytes[..self.length]).map_err(|_| ERR_INVALID)
    }
}

const MAX_ANDROID_DEV_FDS: usize = 32;
const MAX_ANDROID_DEV_NAME: usize = 64;
const MAX_ANDROID_BLOCK_ALIASES: usize = 96;
const ANDROID_PROPERTY_AREA_SIZE: u64 = 128 * 1024;
const ANDROID_PROPERTY_HEADER_SIZE: usize = 128;
const ANDROID_PROPERTY_BT_SIZE: usize = 20;
const ANDROID_PROPERTY_INFO_SIZE: usize = 96;
const ANDROID_PROPERTY_NAME_MAX: usize = 32;
const ANDROID_PROPERTY_VALUE_MAX: usize = 92;
const MAX_ANDROID_PROPERTIES: usize = 32;
const MAX_ANDROID_PROPERTY_NODES: usize = 128;
const ANDROID_PROPERTY_ROOT_RESERVED: usize = 112;
const DEBUG_MOUNT_TABLE_CAPACITY: usize = 1024;
const PROPERTY_SERVICE_PATH: &[u8] = b"/dev/socket/property_service";
const PROP_MSG_SETPROP: u32 = 1;
// Android's version-2 property protocol uses a tagged command value rather
// than the legacy command number 2. Bionic sends this exact value in
// `SocketWriter::WriteUint32(PROP_MSG_SETPROP2)`.
const PROP_MSG_SETPROP2: u32 = 0x0002_0001;

#[derive(Clone, Copy)]
struct AndroidProperty {
    name: [u8; ANDROID_PROPERTY_NAME_MAX],
    name_length: usize,
    value: [u8; ANDROID_PROPERTY_VALUE_MAX],
    value_length: usize,
    active: bool,
}

impl AndroidProperty {
    const EMPTY: Self = Self {
        name: [0; ANDROID_PROPERTY_NAME_MAX],
        name_length: 0,
        value: [0; ANDROID_PROPERTY_VALUE_MAX],
        value_length: 0,
        active: false,
    };
}

#[derive(Clone, Copy)]
struct AndroidPropertyNode {
    name: [u8; ANDROID_PROPERTY_NAME_MAX],
    name_length: u8,
    parent: u16,
    property: u16,
    left: u16,
    right: u16,
    children: u16,
    active: bool,
}

impl AndroidPropertyNode {
    const NONE: u16 = u16::MAX;
    const EMPTY: Self = Self {
        name: [0; ANDROID_PROPERTY_NAME_MAX],
        name_length: 0,
        parent: Self::NONE,
        property: Self::NONE,
        left: Self::NONE,
        right: Self::NONE,
        children: Self::NONE,
        active: false,
    };
}

fn android_property_area(path: &str) -> bool {
    path == "properties_serial" || path.starts_with("u:")
}

fn android_property_area_size(path: &str) -> Option<u64> {
    android_property_area(path).then_some(ANDROID_PROPERTY_AREA_SIZE)
}

fn property_write_u32(image: &mut [u8], offset: usize, value: u32) {
    if let Some(destination) = image.get_mut(offset..offset.saturating_add(4)) {
        destination.copy_from_slice(&value.to_ne_bytes());
    }
}

fn property_write_bytes(image: &mut [u8], offset: usize, bytes: &[u8]) {
    if let Some(destination) = image.get_mut(offset..offset.saturating_add(bytes.len())) {
        destination.copy_from_slice(bytes);
    }
}

fn property_area_write(offset: usize, bytes: &[u8]) -> bool {
    if offset
        .checked_add(bytes.len())
        .is_none_or(|end| end > ANDROID_PROPERTY_AREA_SIZE as usize)
    {
        return false;
    }
    let mut copied = 0usize;
    while copied < bytes.len() {
        let absolute = offset + copied;
        let page = absolute / 4096;
        let page_offset = absolute % 4096;
        let count = (bytes.len() - copied).min(4096 - page_offset);
        let frame = unsafe { (*core::ptr::addr_of!(LINUX_PROPERTY_AREA_FRAMES))[page] };
        if frame == 0 {
            return false;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr().add(copied),
                (frame as *mut u8).add(page_offset),
                count,
            );
        }
        copied += count;
    }
    true
}

fn property_area_write_u32(offset: usize, value: u32) -> bool {
    property_area_write(offset, &value.to_ne_bytes())
}

fn clear_property_area() -> bool {
    for page in 0..ANDROID_PROPERTY_AREA_PAGES {
        let frame = unsafe { (*core::ptr::addr_of!(LINUX_PROPERTY_AREA_FRAMES))[page] };
        if frame == 0 {
            return false;
        }
        unsafe {
            core::ptr::write_bytes(frame as *mut u8, 0, 4096);
        }
    }
    true
}

fn android_property_find(name: &[u8]) -> Option<usize> {
    unsafe {
        (*core::ptr::addr_of!(ANDROID_PROPERTIES))
            .iter()
            .enumerate()
            .find(|(_, property)| {
                property.active
                    && property.name_length == name.len()
                    && property.name[..property.name_length] == *name
            })
            .map(|(index, _)| index)
    }
}

fn android_property_table_init() {
    unsafe {
        if *core::ptr::addr_of!(ANDROID_PROPERTIES_INITIALIZED) {
            return;
        }
        let properties = core::ptr::addr_of_mut!(ANDROID_PROPERTIES);
        let first = &mut (*properties)[0];
        *first = AndroidProperty::EMPTY;
        first.name[..13].copy_from_slice(b"ro.debuggable");
        first.name_length = 13;
        first.value[..1].copy_from_slice(b"1");
        first.value_length = 1;
        first.active = true;
        let second = &mut (*properties)[1];
        *second = AndroidProperty::EMPTY;
        second.name[..11].copy_from_slice(b"ro.hardware");
        second.name_length = 11;
        second.value[..7].copy_from_slice(b"bramble");
        second.value_length = 7;
        second.active = true;
        let third = &mut (*properties)[2];
        *third = AndroidProperty::EMPTY;
        third.name[..27].copy_from_slice(b"ro.property_service.version");
        third.name_length = 27;
        third.value[..1].copy_from_slice(b"2");
        third.value_length = 1;
        third.active = true;
        *core::ptr::addr_of_mut!(ANDROID_PROPERTIES_INITIALIZED) = true;
    }
}

fn android_property_set(name: &[u8], value: &[u8]) -> bool {
    if name.is_empty()
        || name.len() >= ANDROID_PROPERTY_NAME_MAX
        || value.len() >= ANDROID_PROPERTY_VALUE_MAX
        || name
            .split(|byte| *byte == b'.')
            .any(|component| component.is_empty())
        || name.iter().any(|byte| !byte.is_ascii() || *byte == 0)
        || value.iter().any(|byte| *byte == 0)
    {
        return false;
    }
    android_property_table_init();
    let index = android_property_find(name).or_else(|| unsafe {
        (*core::ptr::addr_of!(ANDROID_PROPERTIES))
            .iter()
            .position(|property| !property.active)
    });
    let Some(index) = index else {
        return false;
    };
    unsafe {
        let properties = core::ptr::addr_of_mut!(ANDROID_PROPERTIES);
        let property = &mut (*properties)[index];
        if property.active && property.name_length > 3 && property.name[..3] == *b"ro." {
            return false;
        }
        *property = AndroidProperty::EMPTY;
        property.name[..name.len()].copy_from_slice(name);
        property.name_length = name.len();
        property.value[..value.len()].copy_from_slice(value);
        property.value_length = value.len();
        property.active = true;
        *core::ptr::addr_of_mut!(ANDROID_PROPERTY_SERIAL) =
            (*core::ptr::addr_of!(ANDROID_PROPERTY_SERIAL))
                .wrapping_add(1)
                .max(1);
    }
    if unsafe { *core::ptr::addr_of!(LINUX_PROPERTY_AREA_ACTIVE) } {
        android_property_area_rebuild()
    } else {
        true
    }
}

fn debug_append_bytes(destination: &mut [u8], length: &mut usize, bytes: &[u8]) -> bool {
    let Some(end) = length.checked_add(bytes.len()) else {
        return false;
    };
    if end > destination.len() {
        return false;
    }
    destination[*length..end].copy_from_slice(bytes);
    *length = end;
    true
}

/// Copy the current property table into an ADB-sized response buffer. The
/// USB completion path calls this without taking the VFS mutex; this is the
/// same table that is rebuilt for Android's shared property area.
pub(super) fn debug_property_dump(destination: &mut [u8]) -> usize {
    android_property_table_init();
    let mut length = 0usize;
    for property_index in 0..MAX_ANDROID_PROPERTIES {
        let property = unsafe { (*core::ptr::addr_of!(ANDROID_PROPERTIES))[property_index] };
        if !property.active
            || !debug_append_bytes(destination, &mut length, b"[")
            || !debug_append_bytes(
                destination,
                &mut length,
                &property.name[..property.name_length],
            )
            || !debug_append_bytes(destination, &mut length, b"]: [")
            || !debug_append_bytes(
                destination,
                &mut length,
                &property.value[..property.value_length],
            )
            || !debug_append_bytes(destination, &mut length, b"]\n")
        {
            break;
        }
    }
    length
}

/// Return one property in the same value-only form as `getprop name`.
pub(super) fn debug_property_value(name: &[u8], destination: &mut [u8]) -> Option<usize> {
    android_property_table_init();
    let index = android_property_find(name)?;
    let property = unsafe { (*core::ptr::addr_of!(ANDROID_PROPERTIES))[index] };
    let mut length = 0usize;
    debug_append_bytes(
        destination,
        &mut length,
        &property.value[..property.value_length],
    )
    .then_some(length)
}

fn update_debug_mount_table(mounts: &[u8]) {
    let length = mounts.len().min(DEBUG_MOUNT_TABLE_CAPACITY);
    unsafe {
        let table = core::ptr::addr_of_mut!(DEBUG_MOUNT_TABLE);
        (&mut *table)[..length].copy_from_slice(&mounts[..length]);
        *core::ptr::addr_of_mut!(DEBUG_MOUNT_TABLE_LENGTH) = length;
    }
}

/// Copy the last published Android mount table without entering the VFS.
pub(super) fn debug_mount_table(destination: &mut [u8]) -> usize {
    let length = unsafe { (*core::ptr::addr_of!(DEBUG_MOUNT_TABLE_LENGTH)).min(destination.len()) };
    unsafe {
        let table = core::ptr::addr_of!(DEBUG_MOUNT_TABLE);
        destination[..length].copy_from_slice(&(&*table)[..length]);
    }
    length
}

/// Return a read-only snapshot of the small virtual files that are safe to
/// expose through the bounded ADB sync reader. This deliberately avoids
/// entering the VFS mutex from the USB completion path.
pub(super) fn debug_file_snapshot(path: &[u8], destination: &mut [u8]) -> Option<usize> {
    if debug_sync_file_matches(path) {
        let length = unsafe { DEBUG_SYNC_FILE_DATA_LENGTH.min(destination.len()) };
        unsafe {
            destination[..length].copy_from_slice(&DEBUG_SYNC_FILE_DATA[..length]);
        }
        return Some(length);
    }
    match path {
        b"/proc/mounts" => Some(debug_mount_table(destination)),
        b"/proc/cmdline" => Some(debug_copy_static(
            destination,
            b"console=ttyMSM0 androidboot.hardware=bramble\n",
        )),
        b"/proc/version" => Some(debug_copy_static(
            destination,
            b"FullereneOS Linux compatibility boundary\n",
        )),
        b"/proc/filesystems" => Some(debug_copy_static(
            destination,
            b"nodev\tproc\nnodev\tsysfs\nnodev\ttmpfs\next4\neroFS\nf2fs\n",
        )),
        b"/proc/meminfo" => Some(debug_copy_static(
            destination,
            b"MemTotal:       262144 kB\nMemFree:        131072 kB\n",
        )),
        b"/sys/class/android_usb/state" => Some(debug_copy_static(destination, b"CONFIGURED\n")),
        _ => None,
    }
}

/// Store one bounded ADB-pushed diagnostic file in volatile memory.
///
/// The sync transport is used before the full VFS/storage write path is
/// trusted on Bramble. Accepting only temporary paths makes the feature useful
/// for bring-up binaries and test vectors while preserving the no-partition-
/// write invariant of the physical validation loop.
pub(super) fn debug_file_write(path: &[u8], data: &[u8]) -> bool {
    if !debug_sync_path_allowed(path)
        || path.len() > DEBUG_SYNC_FILE_PATH_CAPACITY
        || data.len() > DEBUG_SYNC_FILE_DATA_CAPACITY
    {
        return false;
    }
    unsafe {
        DEBUG_SYNC_FILE_PATH[..path.len()].copy_from_slice(path);
        DEBUG_SYNC_FILE_PATH_LENGTH = path.len();
        DEBUG_SYNC_FILE_DATA[..data.len()].copy_from_slice(data);
        DEBUG_SYNC_FILE_DATA_LENGTH = data.len();
    }
    true
}

fn debug_sync_path_allowed(path: &[u8]) -> bool {
    path.starts_with(b"/tmp/") || path.starts_with(b"/data/local/tmp/")
}

fn debug_sync_file_matches(path: &[u8]) -> bool {
    unsafe {
        DEBUG_SYNC_FILE_PATH_LENGTH == path.len()
            && DEBUG_SYNC_FILE_PATH_LENGTH != 0
            && DEBUG_SYNC_FILE_PATH[..DEBUG_SYNC_FILE_PATH_LENGTH] == *path
    }
}

fn debug_copy_static(destination: &mut [u8], source: &[u8]) -> usize {
    let length = source.len().min(destination.len());
    destination[..length].copy_from_slice(&source[..length]);
    length
}

fn android_property_node_offset(offsets: &[u32; MAX_ANDROID_PROPERTY_NODES], node: u16) -> u32 {
    (node != AndroidPropertyNode::NONE)
        .then_some(offsets[node as usize])
        .unwrap_or(0)
}

fn android_property_area_rebuild() -> bool {
    if !unsafe { *core::ptr::addr_of!(LINUX_PROPERTY_AREA_ACTIVE) } {
        return false;
    }
    android_property_table_init();
    let mut nodes = [AndroidPropertyNode::EMPTY; MAX_ANDROID_PROPERTY_NODES];
    nodes[0] = AndroidPropertyNode {
        active: true,
        ..AndroidPropertyNode::EMPTY
    };
    let mut node_count = 1usize;
    for property_index in 0..MAX_ANDROID_PROPERTIES {
        let property = unsafe { (*core::ptr::addr_of!(ANDROID_PROPERTIES))[property_index] };
        if !property.active {
            continue;
        }
        let mut parent = 0u16;
        let mut start = 0usize;
        loop {
            let end = property.name[start..property.name_length]
                .iter()
                .position(|byte| *byte == b'.')
                .map(|relative| start + relative)
                .unwrap_or(property.name_length);
            let component = &property.name[start..end];
            let mut child = None;
            for index in 1..node_count {
                let node = nodes[index];
                if node.active
                    && node.parent == parent
                    && node.name_length as usize == component.len()
                    && node.name[..component.len()] == *component
                {
                    child = Some(index as u16);
                    break;
                }
            }
            let child = if let Some(child) = child {
                child
            } else {
                if node_count == nodes.len() {
                    return false;
                }
                let mut node = AndroidPropertyNode::EMPTY;
                node.name[..component.len()].copy_from_slice(component);
                node.name_length = component.len() as u8;
                node.parent = parent;
                node.active = true;
                nodes[node_count] = node;
                node_count += 1;
                (node_count - 1) as u16
            };
            parent = child;
            if end == property.name_length {
                nodes[parent as usize].property = property_index as u16;
                break;
            }
            start = end + 1;
        }
    }

    let mut child_order = [AndroidPropertyNode::NONE; MAX_ANDROID_PROPERTY_NODES];
    for parent in 0..node_count {
        let mut count = 0usize;
        for child in 1..node_count {
            if nodes[child].active && nodes[child].parent == parent as u16 {
                child_order[count] = child as u16;
                count += 1;
            }
        }
        for index in 1..count {
            let key = child_order[index];
            let key_node = nodes[key as usize];
            let mut cursor = index;
            while cursor > 0 {
                let previous = child_order[cursor - 1];
                let previous_node = nodes[previous as usize];
                if previous_node.name[..previous_node.name_length as usize]
                    <= key_node.name[..key_node.name_length as usize]
                {
                    break;
                }
                child_order[cursor] = previous;
                cursor -= 1;
            }
            child_order[cursor] = key;
        }
        nodes[parent].children = if count == 0 {
            AndroidPropertyNode::NONE
        } else {
            child_order[0]
        };
        for index in 0..count {
            let child = child_order[index] as usize;
            nodes[child].left = AndroidPropertyNode::NONE;
            nodes[child].right = if index + 1 < count {
                child_order[index + 1]
            } else {
                AndroidPropertyNode::NONE
            };
        }
        child_order[..count].fill(AndroidPropertyNode::NONE);
    }

    let data = ANDROID_PROPERTY_HEADER_SIZE;
    let mut offsets = [0u32; MAX_ANDROID_PROPERTY_NODES];
    let mut used = ANDROID_PROPERTY_ROOT_RESERVED;
    for index in 1..node_count {
        offsets[index] = used as u32;
        used = used.saturating_add(
            (ANDROID_PROPERTY_BT_SIZE + nodes[index].name_length as usize + 1 + 3) & !3,
        );
    }
    let mut info_offsets = [0u32; MAX_ANDROID_PROPERTIES];
    for property_index in 0..MAX_ANDROID_PROPERTIES {
        let property = unsafe { (*core::ptr::addr_of!(ANDROID_PROPERTIES))[property_index] };
        if !property.active {
            continue;
        }
        info_offsets[property_index] = used as u32;
        used =
            used.saturating_add((ANDROID_PROPERTY_INFO_SIZE + property.name_length + 1 + 3) & !3);
    }
    if data
        .checked_add(used)
        .is_none_or(|end| end > ANDROID_PROPERTY_AREA_SIZE as usize)
        || !clear_property_area()
    {
        return false;
    }
    let serial = unsafe { *core::ptr::addr_of!(ANDROID_PROPERTY_SERIAL) };
    if !property_area_write_u32(0, used as u32)
        || !property_area_write_u32(4, serial << 8)
        || !property_area_write_u32(8, 0x504f_5250)
        || !property_area_write_u32(12, 0xfc6e_d0ab)
        || !property_area_write_u32(
            data + 16,
            android_property_node_offset(&offsets, nodes[0].children),
        )
    {
        return false;
    }
    for index in 1..node_count {
        let node = nodes[index];
        let offset = data + offsets[index] as usize;
        let property = if node.property == AndroidPropertyNode::NONE {
            0
        } else {
            info_offsets[node.property as usize]
        };
        if !property_area_write_u32(offset, node.name_length as u32)
            || !property_area_write_u32(offset + 4, property)
            || !property_area_write_u32(
                offset + 8,
                android_property_node_offset(&offsets, node.left),
            )
            || !property_area_write_u32(
                offset + 12,
                android_property_node_offset(&offsets, node.right),
            )
            || !property_area_write_u32(
                offset + 16,
                android_property_node_offset(&offsets, node.children),
            )
            || !property_area_write(
                offset + ANDROID_PROPERTY_BT_SIZE,
                &node.name[..node.name_length as usize + 1],
            )
        {
            return false;
        }
    }
    for property_index in 0..MAX_ANDROID_PROPERTIES {
        let property = unsafe { (*core::ptr::addr_of!(ANDROID_PROPERTIES))[property_index] };
        if !property.active {
            continue;
        }
        let offset = data + info_offsets[property_index] as usize;
        let property_serial = (serial << 8) | property.value_length as u32;
        if !property_area_write_u32(offset, property_serial)
            || !property_area_write(offset + 4, &property.value[..property.value_length + 1])
            || !property_area_write(
                offset + ANDROID_PROPERTY_INFO_SIZE,
                &property.name[..property.name_length + 1],
            )
        {
            return false;
        }
    }
    wake_android_property_waiters(&info_offsets);
    true
}

/// Wake Bionic waiters after publishing a new property serial.
///
/// The early property mapping is physically shared but may be mapped at more
/// than one user virtual address.  Futex keys in the bounded scheduler are
/// virtual-address based, so wake the serial word for every active mapping
/// and every active `prop_info`.  The table is deliberately small; this keeps
/// the wakeup exact without turning the scheduler's futex key into a global
/// physical-address ABI.
fn wake_android_property_waiters(info_offsets: &[u32; MAX_ANDROID_PROPERTIES]) {
    let mut mappings = [LinuxPropertyMapping::EMPTY; MAX_LINUX_PROPERTY_MAPPINGS];
    let mut mapping_count = 0usize;
    unsafe {
        for mapping in (*core::ptr::addr_of!(LINUX_PROPERTY_MAPPINGS))
            .iter()
            .copied()
            .filter(|mapping| mapping.active)
        {
            if mapping_count == mappings.len() {
                break;
            }
            mappings[mapping_count] = mapping;
            mapping_count += 1;
        }
    }
    for mapping in mappings.iter().copied().take(mapping_count) {
        if mapping.length >= 8 {
            let address = mapping.address.saturating_add(4);
            let _ = task::wake_event_count(task::linux_futex_event_key(address), usize::MAX);
        }
        for (property_index, offset) in info_offsets.iter().copied().enumerate() {
            let active =
                unsafe { (*core::ptr::addr_of!(ANDROID_PROPERTIES))[property_index].active };
            if !active {
                continue;
            }
            let serial_offset =
                (ANDROID_PROPERTY_HEADER_SIZE as u64).saturating_add(u64::from(offset));
            if serial_offset.saturating_add(4) > mapping.length {
                continue;
            }
            let address = mapping.address.saturating_add(serial_offset);
            let _ = task::wake_event_count(task::linux_futex_event_key(address), usize::MAX);
        }
    }
}

fn android_property_area_read(path: &str, offset: u64, buffer: &mut [u8]) -> usize {
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    if start >= ANDROID_PROPERTY_AREA_SIZE as usize {
        return 0;
    }
    let count = buffer
        .len()
        .min((ANDROID_PROPERTY_AREA_SIZE as usize).saturating_sub(start));
    if unsafe { *core::ptr::addr_of!(LINUX_PROPERTY_AREA_ACTIVE) } {
        let active_space = task::current_address_space();
        mmu::activate_kernel_identity_space();
        let mut copied = 0usize;
        while copied < count {
            let absolute = start + copied;
            let page = absolute / 4096;
            let page_offset = absolute % 4096;
            let chunk = (count - copied).min(4096 - page_offset);
            let frame = unsafe { (*core::ptr::addr_of!(LINUX_PROPERTY_AREA_FRAMES))[page] };
            if frame == 0 {
                break;
            }
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (frame as *const u8).add(page_offset),
                    buffer.as_mut_ptr().add(copied),
                    chunk,
                );
            }
            copied += chunk;
        }
        if let Some(space_id) = active_space {
            let _ = mmu::activate_user_space(space_id);
        }
        return copied;
    }
    let mut image = [0u8; 512];
    android_property_area_image(path, &mut image);
    buffer[..count].fill(0);
    if start < image.len() {
        let image_count = count.min(image.len() - start);
        buffer[..image_count].copy_from_slice(&image[start..start + image_count]);
    }
    count
}

/// Build the stable read-only property-area prefix used by the Linux mmap
/// compatibility path.  The image follows Android 14's 32-bit offset based
/// `prop_area`/`prop_bt`/`prop_info` layout and is intentionally small; the
/// remaining bytes in the 128 KiB area are zero-filled.
fn android_property_area_image(path: &str, image: &mut [u8; 512]) {
    image.fill(0);
    let data = ANDROID_PROPERTY_HEADER_SIZE;
    property_write_u32(image, 0, 420);
    property_write_u32(image, 8, 0x504f_5250);
    property_write_u32(image, 12, 0xfc6e_d0ab);
    if path == "properties_serial" {
        return;
    }

    // The root reserves one prop_bt and one PROP_VALUE_MAX dirty-backup area.
    let ro = data + 112;
    let debuggable = data + 136;
    let hardware = data + 168;
    let debuggable_info = data + 200;
    let hardware_info = data + 312;

    property_write_u32(image, data + 16, ro as u32 - data as u32);
    property_write_u32(image, ro, 2);
    property_write_u32(image, ro + 16, debuggable as u32 - data as u32);
    property_write_bytes(image, ro + ANDROID_PROPERTY_BT_SIZE, b"ro\0");

    property_write_u32(image, debuggable + 0, 10);
    property_write_u32(image, debuggable + 4, debuggable_info as u32 - data as u32);
    property_write_u32(image, debuggable + 12, hardware as u32 - data as u32);
    property_write_bytes(
        image,
        debuggable + ANDROID_PROPERTY_BT_SIZE,
        b"debuggable\0",
    );

    property_write_u32(image, hardware + 0, 8);
    property_write_u32(image, hardware + 4, hardware_info as u32 - data as u32);
    property_write_bytes(image, hardware + ANDROID_PROPERTY_BT_SIZE, b"hardware\0");

    property_write_u32(image, debuggable_info, 1 << 24);
    property_write_bytes(image, debuggable_info + 4, b"1\0");
    property_write_bytes(
        image,
        debuggable_info + ANDROID_PROPERTY_INFO_SIZE,
        b"ro.debuggable\0",
    );

    property_write_u32(image, hardware_info, 7 << 24);
    property_write_bytes(image, hardware_info + 4, b"bramble\0");
    property_write_bytes(
        image,
        hardware_info + ANDROID_PROPERTY_INFO_SIZE,
        b"ro.hardware\0",
    );
}

#[derive(Clone, Copy)]
struct AndroidDevFd {
    name: [u8; MAX_ANDROID_DEV_NAME],
    length: usize,
    fd: u32,
    offset: u64,
    active: bool,
}

impl AndroidDevFd {
    const EMPTY: Self = Self {
        name: [0; MAX_ANDROID_DEV_NAME],
        length: 0,
        fd: 0,
        offset: 0,
        active: false,
    };
}

static mut ANDROID_DEV_FDS: [AndroidDevFd; MAX_ANDROID_DEV_FDS] =
    [AndroidDevFd::EMPTY; MAX_ANDROID_DEV_FDS];
static mut NEXT_ANDROID_DEV_FD: u32 = 1;

#[derive(Clone, Copy)]
struct AndroidBlockAlias {
    name: [u8; MAX_ANDROID_DEV_NAME],
    length: usize,
    size: u64,
    active: bool,
}

impl AndroidBlockAlias {
    const EMPTY: Self = Self {
        name: [0; MAX_ANDROID_DEV_NAME],
        length: 0,
        size: 0,
        active: false,
    };
}

static mut ANDROID_BLOCK_ALIASES: [AndroidBlockAlias; MAX_ANDROID_BLOCK_ALIASES] =
    [AndroidBlockAlias::EMPTY; MAX_ANDROID_BLOCK_ALIASES];

fn android_block_alias_name(path: &str) -> Option<&str> {
    let name = path.strip_prefix("block/by-name/")?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

fn android_block_alias_index(name: &str) -> Option<usize> {
    unsafe {
        (*core::ptr::addr_of!(ANDROID_BLOCK_ALIASES))
            .iter()
            .position(|entry| android_block_alias_matches(entry, name))
    }
}

fn android_block_alias_matches(entry: &AndroidBlockAlias, name: &str) -> bool {
    entry.active && entry.length == name.len() && entry.name[..entry.length] == *name.as_bytes()
}

/// Publish a read-only `/dev/block/by-name` entry discovered from GPT or
/// Android Logical Partitions.  The entry is intentionally a kernel-side
/// device boundary: it provides the identity, size and read-only open
/// semantics required by Android init while storage reads continue through
/// the guarded UFS/filesystem path.
fn register_android_block_alias(name: &str, size: u64) {
    if name.is_empty() || name.len() > MAX_ANDROID_DEV_NAME || name.contains('/') {
        return;
    }
    unsafe {
        let aliases = &mut *core::ptr::addr_of_mut!(ANDROID_BLOCK_ALIASES);
        let index = aliases
            .iter()
            .position(|entry| android_block_alias_matches(entry, name))
            .or_else(|| aliases.iter().position(|entry| !entry.active));
        let Some(index) = index else {
            return;
        };
        let mut encoded = [0u8; MAX_ANDROID_DEV_NAME];
        encoded[..name.len()].copy_from_slice(name.as_bytes());
        aliases[index] = AndroidBlockAlias {
            name: encoded,
            length: name.len(),
            size,
            active: true,
        };
    }
}

fn android_block_alias_size(path: &str) -> Option<u64> {
    let name = android_block_alias_name(path)?;
    let index = android_block_alias_index(name)?;
    unsafe {
        (*core::ptr::addr_of!(ANDROID_BLOCK_ALIASES))[index]
            .active
            .then_some((*core::ptr::addr_of!(ANDROID_BLOCK_ALIASES))[index].size)
    }
}

fn android_lp_partition_size(metadata: &genome::android_lp::LpMetadata, name: &str) -> u64 {
    let Some(partition) = metadata
        .partitions
        .iter()
        .find(|partition| partition.name == name)
    else {
        return 0;
    };
    let start = partition.first_extent_index as usize;
    let end = start.saturating_add(partition.num_extents as usize);
    let sectors = metadata
        .extents
        .get(start..end)
        .unwrap_or(&[])
        .iter()
        .map(|extent| extent.num_sectors)
        .fold(0u64, u64::saturating_add);
    sectors.saturating_mul(genome::android_lp::LP_SECTOR_SIZE)
}

/// Small AArch64-only `/dev` boundary for the Android-init bring-up.
///
/// The device names are deliberately explicit.  They provide the harmless
/// null/zero and UART logging endpoints that early init probes, while random
/// data and hardware-specific nodes remain unsupported until a real entropy or
/// driver source is connected.
pub(crate) struct AndroidDevFs;

impl AndroidDevFs {
    pub(crate) const fn new() -> Self {
        Self
    }

    fn known(path: &str) -> bool {
        matches!(
            path,
            "null" | "zero" | "full" | "random" | "urandom" | "console" | "kmsg" | "tty" | "ptmx"
        ) || android_block_alias_name(path)
            .is_some_and(|name| android_block_alias_index(name).is_some())
            || matches!(
                path,
                "selinux/null"
                    | "selinux/enforce"
                    | "selinux/policyvers"
                    | "selinux/checkreqprot"
                    | "selinux/load"
                    | "selinux/context"
                    | "__properties__/properties_serial"
            )
            || path.starts_with("__properties__/u:")
    }

    fn fd_index(fd: u32) -> Option<usize> {
        unsafe {
            (*core::ptr::addr_of!(ANDROID_DEV_FDS))
                .iter()
                .position(|entry| entry.active && entry.fd == fd)
        }
    }

    fn entry_name(index: usize) -> Option<([u8; MAX_ANDROID_DEV_NAME], usize, u64)> {
        unsafe {
            let entry = (*core::ptr::addr_of!(ANDROID_DEV_FDS)).get(index)?;
            entry
                .active
                .then_some((entry.name, entry.length, entry.offset))
        }
    }

    fn advance(fd: u32, amount: usize) {
        unsafe {
            if let Some(index) = Self::fd_index(fd) {
                (*core::ptr::addr_of_mut!(ANDROID_DEV_FDS))[index].offset =
                    (*core::ptr::addr_of!(ANDROID_DEV_FDS))[index]
                        .offset
                        .saturating_add(amount as u64);
            }
        }
    }
}

impl FileSystem for AndroidDevFs {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(false, false, false, false, true)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let path = path.trim_start_matches('/');
        if !Self::known(path) {
            return None;
        }
        let length = path.len();
        if length > MAX_ANDROID_DEV_NAME {
            return None;
        }
        let index = unsafe {
            (*core::ptr::addr_of!(ANDROID_DEV_FDS))
                .iter()
                .position(|entry| !entry.active)?
        };
        let fd = unsafe {
            let fd = *core::ptr::addr_of!(NEXT_ANDROID_DEV_FD);
            *core::ptr::addr_of_mut!(NEXT_ANDROID_DEV_FD) = fd.wrapping_add(1).max(1);
            fd
        };
        let mut name = [0u8; MAX_ANDROID_DEV_NAME];
        name[..length].copy_from_slice(path.as_bytes());
        unsafe {
            (*core::ptr::addr_of_mut!(ANDROID_DEV_FDS))[index] = AndroidDevFd {
                name,
                length,
                fd,
                offset: 0,
                active: true,
            };
        }
        Some(FileDescriptor {
            fd,
            ino: android_dev_ino(path),
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buffer: &mut [u8]) -> Result<usize, FsError> {
        let index = Self::fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        let (name, length, offset) =
            Self::entry_name(index).ok_or(FsError::InvalidFileDescriptor)?;
        let name = core::str::from_utf8(&name[..length]).map_err(|_| FsError::InvalidInput)?;
        match name {
            "null" | "console" | "kmsg" | "tty" | "ptmx" => Ok(0),
            "zero" => {
                buffer.fill(0);
                Self::advance(fd, buffer.len());
                Ok(buffer.len())
            }
            "random" | "urandom" => Err(FsError::NotSupported),
            _ if android_block_alias_name(name)
                .is_some_and(|alias| android_block_alias_index(alias).is_some()) =>
            {
                Ok(0)
            }
            "full" => Err(FsError::DiskFull),
            "selinux/null" => Ok(0),
            "selinux/enforce" | "selinux/checkreqprot" => {
                let data = b"0\n";
                let start = usize::try_from(offset).unwrap_or(usize::MAX);
                if start >= data.len() {
                    return Ok(0);
                }
                let count = buffer.len().min(data.len() - start);
                buffer[..count].copy_from_slice(&data[start..start + count]);
                Self::advance(fd, count);
                Ok(count)
            }
            "selinux/policyvers" => {
                let data = b"30\n";
                let start = usize::try_from(offset).unwrap_or(usize::MAX);
                if start >= data.len() {
                    return Ok(0);
                }
                let count = buffer.len().min(data.len() - start);
                buffer[..count].copy_from_slice(&data[start..start + count]);
                Self::advance(fd, count);
                Ok(count)
            }
            "selinux/context" => {
                let data = b"u:r:init:s0\0";
                let start = usize::try_from(offset).unwrap_or(usize::MAX);
                if start >= data.len() {
                    return Ok(0);
                }
                let count = buffer.len().min(data.len() - start);
                buffer[..count].copy_from_slice(&data[start..start + count]);
                Self::advance(fd, count);
                Ok(count)
            }
            "__properties__/properties_serial" => {
                let count = android_property_area_read("properties_serial", offset, buffer);
                Self::advance(fd, count);
                Ok(count)
            }
            _ if name.starts_with("__properties__/u:") => {
                let count = android_property_area_read(
                    name.trim_start_matches("__properties__/"),
                    offset,
                    buffer,
                );
                Self::advance(fd, count);
                Ok(count)
            }
            _ => Err(FsError::NotSupported),
        }
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let index = Self::fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        let (name, length, _) = Self::entry_name(index).ok_or(FsError::InvalidFileDescriptor)?;
        let name = core::str::from_utf8(&name[..length]).map_err(|_| FsError::InvalidInput)?;
        match name {
            "null" | "zero" | "ptmx" => {
                Self::advance(fd, data.len());
                Ok(data.len())
            }
            "console" | "kmsg" | "tty" => {
                for byte in data {
                    uart::putc(*byte);
                }
                Self::advance(fd, data.len());
                Ok(data.len())
            }
            "full" => Err(FsError::DiskFull),
            "random" | "urandom" => Err(FsError::NotSupported),
            "selinux/enforce" | "selinux/checkreqprot" | "selinux/load" => {
                Self::advance(fd, data.len());
                Ok(data.len())
            }
            _ if android_block_alias_name(name)
                .is_some_and(|alias| android_block_alias_index(alias).is_some()) =>
            {
                Err(FsError::PermissionDenied)
            }
            _ => Err(FsError::NotSupported),
        }
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let index = Self::fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        unsafe {
            (*core::ptr::addr_of_mut!(ANDROID_DEV_FDS))[index] = AndroidDevFd::EMPTY;
        }
        Ok(())
    }

    fn seek(&mut self, fd: u32, position: u64) -> Result<(), FsError> {
        let index = Self::fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        unsafe {
            (*core::ptr::addr_of_mut!(ANDROID_DEV_FDS))[index].offset = position;
        }
        Ok(())
    }

    fn position(&mut self, fd: u32) -> Result<u64, FsError> {
        let index = Self::fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        Self::entry_name(index)
            .map(|(_, _, offset)| offset)
            .ok_or(FsError::InvalidFileDescriptor)
    }

    fn size(&mut self, fd: u32) -> Result<u64, FsError> {
        let index = Self::fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        let (name, length, _) = Self::entry_name(index).ok_or(FsError::InvalidFileDescriptor)?;
        let name = core::str::from_utf8(&name[..length]).map_err(|_| FsError::InvalidInput)?;
        if let Some(size) = android_block_alias_size(name) {
            return Ok(size);
        }
        Ok(android_property_area_size(name.trim_start_matches("__properties__/")).unwrap_or(0))
    }

    fn metadata(&mut self, path: &str) -> Result<FileMetadata, FsError> {
        let path = path.trim_matches('/');
        let size = android_block_alias_size(path).ok_or(FsError::FileNotFound)?;
        Ok(FileMetadata {
            // Linux S_IFBLK plus read-only permissions.  The VFS kind stays
            // File because this is the kernel's read-only block boundary.
            mode: 0o060440,
            uid: 0,
            gid: 0,
            size,
            kind: InodeType::File,
        })
    }

    fn metadata_at(&mut self, fd: u32) -> Result<FileMetadata, FsError> {
        let index = Self::fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        let (name, length, _) = Self::entry_name(index).ok_or(FsError::InvalidFileDescriptor)?;
        let name = core::str::from_utf8(&name[..length]).map_err(|_| FsError::InvalidInput)?;
        self.metadata(name)
    }

    fn create(&mut self, _path: &str, _kind: InodeType) -> Option<u64> {
        None
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    fn unlink(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    fn readdir(&mut self, path: &str) -> Result<alloc::vec::Vec<VNode>, FsError> {
        let path = path.trim_matches('/');
        if path == "block/by-name" {
            let mut entries = alloc::vec::Vec::new();
            unsafe {
                for alias in (*core::ptr::addr_of!(ANDROID_BLOCK_ALIASES))
                    .iter()
                    .filter(|alias| alias.active)
                {
                    let Ok(name) = core::str::from_utf8(&alias.name[..alias.length]) else {
                        continue;
                    };
                    entries.push(VNode {
                        name: alloc::string::String::from(name),
                        size: alias.size,
                        is_dir: false,
                    });
                }
            }
            return Ok(entries);
        }
        let names: &[(&str, bool)] = match path {
            "" => &[
                ("block", true),
                ("console", false),
                ("full", false),
                ("kmsg", false),
                ("null", false),
                ("ptmx", false),
                ("random", false),
                ("selinux", true),
                ("socket", true),
                ("tty", false),
                ("urandom", false),
                ("zero", false),
            ],
            "block" => &[("by-name", true)],
            "selinux" => &[
                ("checkreqprot", false),
                ("context", false),
                ("enforce", false),
                ("load", false),
                ("null", false),
                ("policyvers", false),
            ],
            "__properties__" => &[("properties_serial", false)],
            _ => return Err(FsError::NotADirectory),
        };
        Ok(names
            .iter()
            .map(|(name, is_dir)| VNode {
                name: alloc::string::String::from(*name),
                size: 0,
                is_dir: *is_dir,
            })
            .collect())
    }

    fn exists(&mut self, path: &str) -> bool {
        let path = path.trim_matches('/');
        Self::known(path)
            || matches!(
                path,
                "" | "block" | "block/by-name" | "selinux" | "socket" | "__properties__"
            )
    }

    fn is_property_file(&mut self, fd: u32) -> bool {
        let Some(index) = Self::fd_index(fd) else {
            return false;
        };
        let Some((name, length, _)) = Self::entry_name(index) else {
            return false;
        };
        let Ok(name) = core::str::from_utf8(&name[..length]) else {
            return false;
        };
        android_property_area(name.trim_start_matches("__properties__/"))
    }
}

const SELINUX_NODE_DIRECTORY: u8 = 0;
const SELINUX_NODE_ENFORCE: u8 = 1;
const SELINUX_NODE_CHECKREQPROT: u8 = 2;
const SELINUX_NODE_POLICYVERS: u8 = 3;
const SELINUX_NODE_LOAD: u8 = 4;
const SELINUX_NODE_CONTEXT: u8 = 5;
const SELINUX_NODE_DENY_UNKNOWN: u8 = 6;
const SELINUX_NODE_COMMIT_PENDING_BOOLEANS: u8 = 7;
const MAX_SELINUX_FDS: usize = 16;
const MAX_SELINUX_CONTEXT: usize = 64;
const MAX_SELINUX_OBJECT_PATH: usize = 96;
const SELINUX_POLICY_MAGIC_V1: &[u8] = b"FSP1";
const SELINUX_POLICY_MAGIC_V2: &[u8] = b"FSP2";
const SELINUX_POLICY_VERSION: u8 = 1;
const MAX_SELINUX_POLICY_RULES: usize = 16;
const MAX_SELINUX_POLICY_ALLOW_RULES: usize = 16;
const MAX_SELINUX_POLICY_BYTES: usize = 1024;
const SELINUX_ACCESS_READ: u8 = 1;
const SELINUX_ACCESS_WRITE: u8 = 2;

#[derive(Clone, Copy)]
struct SelinuxPolicyRule {
    source: [u8; MAX_SELINUX_CONTEXT],
    source_length: usize,
    target: [u8; MAX_SELINUX_CONTEXT],
    target_length: usize,
    active: bool,
}

impl SelinuxPolicyRule {
    const EMPTY: Self = Self {
        source: [0; MAX_SELINUX_CONTEXT],
        source_length: 0,
        target: [0; MAX_SELINUX_CONTEXT],
        target_length: 0,
        active: false,
    };
}

#[derive(Clone, Copy)]
struct SelinuxPolicyAllowRule {
    source: [u8; MAX_SELINUX_CONTEXT],
    source_length: usize,
    object: [u8; MAX_SELINUX_OBJECT_PATH],
    object_length: usize,
    permissions: u8,
    active: bool,
}

impl SelinuxPolicyAllowRule {
    const EMPTY: Self = Self {
        source: [0; MAX_SELINUX_CONTEXT],
        source_length: 0,
        object: [0; MAX_SELINUX_OBJECT_PATH],
        object_length: 0,
        permissions: 0,
        active: false,
    };
}

#[derive(Clone, Copy)]
struct SelinuxPolicyState {
    enforcing: bool,
    loaded: bool,
    rules: [SelinuxPolicyRule; MAX_SELINUX_POLICY_RULES],
    rule_count: usize,
    allow_rules: [SelinuxPolicyAllowRule; MAX_SELINUX_POLICY_ALLOW_RULES],
    allow_rule_count: usize,
}

impl SelinuxPolicyState {
    const EMPTY: Self = Self {
        enforcing: false,
        loaded: false,
        rules: [SelinuxPolicyRule::EMPTY; MAX_SELINUX_POLICY_RULES],
        rule_count: 0,
        allow_rules: [SelinuxPolicyAllowRule::EMPTY; MAX_SELINUX_POLICY_ALLOW_RULES],
        allow_rule_count: 0,
    };
}

static mut SELINUX_POLICY_STATE: SelinuxPolicyState = SelinuxPolicyState::EMPTY;

fn valid_selinux_context_bytes(context: &[u8]) -> bool {
    !context.is_empty()
        && context.len() <= MAX_SELINUX_CONTEXT
        && context.iter().all(|byte| matches!(byte, 0x21..=0x7e))
        && context.starts_with(b"u:")
        && context.iter().filter(|byte| **byte == b':').count() >= 3
}

fn valid_selinux_object_path_bytes(path: &[u8]) -> bool {
    !path.is_empty()
        && path.len() <= MAX_SELINUX_OBJECT_PATH
        && path.starts_with(b"/")
        && path.iter().all(|byte| matches!(byte, 0x21..=0x7e))
}

#[derive(Clone, Copy)]
struct SelinuxFd {
    fd: u32,
    node: u8,
    offset: u64,
    active: bool,
}

impl SelinuxFd {
    const EMPTY: Self = Self {
        fd: 0,
        node: SELINUX_NODE_DIRECTORY,
        offset: 0,
        active: false,
    };
}

/// Bounded selinuxfs surface for Android init.
///
/// This owns the file semantics that init/libselinux expects at the early
/// boot boundary: policy loading, enforcing, transition checks, and a small
/// exact-path object permission table are handled in Rust. FSP2 is deliberately
/// not an AOSP policydb encoding; unlisted paths remain outside this bounded
/// object-policy layer.
struct SelinuxFs {
    fds: [SelinuxFd; MAX_SELINUX_FDS],
    next_fd: u32,
    enforcing: bool,
    policy_loaded: bool,
    policy_size: u64,
    context: [u8; MAX_SELINUX_CONTEXT],
    context_length: usize,
    policy_rules: [SelinuxPolicyRule; MAX_SELINUX_POLICY_RULES],
    policy_rule_count: usize,
    policy_allow_rules: [SelinuxPolicyAllowRule; MAX_SELINUX_POLICY_ALLOW_RULES],
    policy_allow_rule_count: usize,
    policy_buffer: [u8; MAX_SELINUX_POLICY_BYTES],
    policy_buffer_length: usize,
}

impl SelinuxFs {
    fn new() -> Self {
        let filesystem = Self {
            fds: [SelinuxFd::EMPTY; MAX_SELINUX_FDS],
            next_fd: 1,
            enforcing: false,
            policy_loaded: false,
            policy_size: 0,
            context: [0; MAX_SELINUX_CONTEXT],
            context_length: 0,
            policy_rules: [SelinuxPolicyRule::EMPTY; MAX_SELINUX_POLICY_RULES],
            policy_rule_count: 0,
            policy_allow_rules: [SelinuxPolicyAllowRule::EMPTY; MAX_SELINUX_POLICY_ALLOW_RULES],
            policy_allow_rule_count: 0,
            policy_buffer: [0; MAX_SELINUX_POLICY_BYTES],
            policy_buffer_length: 0,
        };
        filesystem.publish_policy_state();
        filesystem
    }

    fn publish_policy_state(&self) {
        unsafe {
            *core::ptr::addr_of_mut!(SELINUX_POLICY_STATE) = SelinuxPolicyState {
                enforcing: self.enforcing,
                loaded: self.policy_loaded,
                rules: self.policy_rules,
                rule_count: self.policy_rule_count,
                allow_rules: self.policy_allow_rules,
                allow_rule_count: self.policy_allow_rule_count,
            };
        }
    }

    fn parse_policy(&mut self) -> Result<(), FsError> {
        let data = &self.policy_buffer[..self.policy_buffer_length];
        if data.len() < 8
            || (data[..4] != *SELINUX_POLICY_MAGIC_V1 && data[..4] != *SELINUX_POLICY_MAGIC_V2)
        {
            return Err(FsError::InvalidInput);
        }
        if data[4] != SELINUX_POLICY_VERSION || data[5] != 0 {
            return Err(FsError::InvalidInput);
        }
        let rule_count = u16::from_le_bytes([data[6], data[7]]) as usize;
        if rule_count == 0 || rule_count > MAX_SELINUX_POLICY_RULES {
            return Err(FsError::InvalidInput);
        }
        let is_v2 = data[..4] == *SELINUX_POLICY_MAGIC_V2;
        let allow_rule_count = if is_v2 {
            if data.len() < 10 {
                return Err(FsError::InvalidInput);
            }
            u16::from_le_bytes([data[8], data[9]]) as usize
        } else {
            0
        };
        if allow_rule_count > MAX_SELINUX_POLICY_ALLOW_RULES {
            return Err(FsError::InvalidInput);
        }
        let mut offset = if is_v2 { 10usize } else { 8usize };
        let mut rules = [SelinuxPolicyRule::EMPTY; MAX_SELINUX_POLICY_RULES];
        for rule in rules.iter_mut().take(rule_count) {
            let source_length = *data.get(offset).ok_or(FsError::InvalidInput)? as usize;
            let target_length = *data
                .get(offset.checked_add(1).ok_or(FsError::InvalidInput)?)
                .ok_or(FsError::InvalidInput)? as usize;
            offset = offset.checked_add(2).ok_or(FsError::InvalidInput)?;
            if source_length == 0
                || target_length == 0
                || source_length > MAX_SELINUX_CONTEXT
                || target_length > MAX_SELINUX_CONTEXT
            {
                return Err(FsError::InvalidInput);
            }
            let source_end = offset
                .checked_add(source_length)
                .ok_or(FsError::InvalidInput)?;
            let target_end = source_end
                .checked_add(target_length)
                .ok_or(FsError::InvalidInput)?;
            let source = data.get(offset..source_end).ok_or(FsError::InvalidInput)?;
            let target = data
                .get(source_end..target_end)
                .ok_or(FsError::InvalidInput)?;
            if !valid_selinux_context_bytes(source) || !valid_selinux_context_bytes(target) {
                return Err(FsError::InvalidInput);
            }
            rule.source[..source_length].copy_from_slice(source);
            rule.source_length = source_length;
            rule.target[..target_length].copy_from_slice(target);
            rule.target_length = target_length;
            rule.active = true;
            offset = target_end;
        }
        let mut allow_rules = [SelinuxPolicyAllowRule::EMPTY; MAX_SELINUX_POLICY_ALLOW_RULES];
        for rule in allow_rules.iter_mut().take(allow_rule_count) {
            let source_length = *data.get(offset).ok_or(FsError::InvalidInput)? as usize;
            let object_length = *data
                .get(offset.checked_add(1).ok_or(FsError::InvalidInput)?)
                .ok_or(FsError::InvalidInput)? as usize;
            let permissions = *data
                .get(offset.checked_add(2).ok_or(FsError::InvalidInput)?)
                .ok_or(FsError::InvalidInput)?;
            let reserved = *data
                .get(offset.checked_add(3).ok_or(FsError::InvalidInput)?)
                .ok_or(FsError::InvalidInput)?;
            offset = offset.checked_add(4).ok_or(FsError::InvalidInput)?;
            if source_length == 0
                || object_length == 0
                || source_length > MAX_SELINUX_CONTEXT
                || object_length > MAX_SELINUX_OBJECT_PATH
                || reserved != 0
                || permissions == 0
                || permissions & !(SELINUX_ACCESS_READ | SELINUX_ACCESS_WRITE) != 0
            {
                return Err(FsError::InvalidInput);
            }
            let source_end = offset
                .checked_add(source_length)
                .ok_or(FsError::InvalidInput)?;
            let object_end = source_end
                .checked_add(object_length)
                .ok_or(FsError::InvalidInput)?;
            let source = data.get(offset..source_end).ok_or(FsError::InvalidInput)?;
            let object = data
                .get(source_end..object_end)
                .ok_or(FsError::InvalidInput)?;
            if !valid_selinux_context_bytes(source) || !valid_selinux_object_path_bytes(object) {
                return Err(FsError::InvalidInput);
            }
            rule.source[..source_length].copy_from_slice(source);
            rule.source_length = source_length;
            rule.object[..object_length].copy_from_slice(object);
            rule.object_length = object_length;
            rule.permissions = permissions;
            rule.active = true;
            offset = object_end;
        }
        if offset != data.len() {
            return Err(FsError::InvalidInput);
        }
        self.policy_rules = rules;
        self.policy_rule_count = rule_count;
        self.policy_allow_rules = allow_rules;
        self.policy_allow_rule_count = allow_rule_count;
        self.policy_loaded = true;
        self.policy_size = self.policy_buffer_length as u64;
        self.publish_policy_state();
        Ok(())
    }

    fn node(path: &str) -> Option<u8> {
        match path.trim_matches('/') {
            "" => Some(SELINUX_NODE_DIRECTORY),
            "enforce" => Some(SELINUX_NODE_ENFORCE),
            "checkreqprot" => Some(SELINUX_NODE_CHECKREQPROT),
            "policyvers" => Some(SELINUX_NODE_POLICYVERS),
            "load" => Some(SELINUX_NODE_LOAD),
            "context" => Some(SELINUX_NODE_CONTEXT),
            "deny_unknown" => Some(SELINUX_NODE_DENY_UNKNOWN),
            "commit_pending_bools" => Some(SELINUX_NODE_COMMIT_PENDING_BOOLEANS),
            _ => None,
        }
    }

    fn fd_index(&self, fd: u32) -> Option<usize> {
        self.fds
            .iter()
            .position(|entry| entry.active && entry.fd == fd)
    }

    fn data(&self, node: u8, destination: &mut [u8; MAX_SELINUX_CONTEXT]) -> usize {
        let bytes: &[u8] = match node {
            SELINUX_NODE_ENFORCE => {
                if self.enforcing {
                    b"1\n"
                } else {
                    b"0\n"
                }
            }
            SELINUX_NODE_CHECKREQPROT => b"0\n",
            SELINUX_NODE_POLICYVERS => b"30\n",
            SELINUX_NODE_CONTEXT => {
                if self.context_length == 0 {
                    b"u:r:init:s0\0"
                } else {
                    &self.context[..self.context_length]
                }
            }
            SELINUX_NODE_DENY_UNKNOWN => b"0\n",
            SELINUX_NODE_COMMIT_PENDING_BOOLEANS => b"0\n",
            SELINUX_NODE_LOAD => return 0,
            _ => return 0,
        };
        let count = bytes.len().min(destination.len());
        destination[..count].copy_from_slice(&bytes[..count]);
        count
    }

    fn node_size(&self, node: u8) -> u64 {
        match node {
            SELINUX_NODE_ENFORCE | SELINUX_NODE_CHECKREQPROT => 2,
            SELINUX_NODE_POLICYVERS | SELINUX_NODE_DENY_UNKNOWN => 3,
            SELINUX_NODE_CONTEXT => {
                if self.context_length == 0 {
                    12
                } else {
                    self.context_length as u64
                }
            }
            SELINUX_NODE_LOAD => {
                if self.policy_loaded {
                    self.policy_size
                } else {
                    0
                }
            }
            SELINUX_NODE_COMMIT_PENDING_BOOLEANS => 2,
            _ => 0,
        }
    }
}

impl FileSystem for SelinuxFs {
    fn capabilities(&self) -> FileSystemCapabilities {
        FileSystemCapabilities::new(false, false, false, false, true)
    }

    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let node = Self::node(path)?;
        let index = self.fds.iter().position(|entry| !entry.active)?;
        let fd = self.next_fd;
        self.next_fd = self.next_fd.wrapping_add(1).max(1);
        self.fds[index] = SelinuxFd {
            fd,
            node,
            offset: 0,
            active: true,
        };
        Some(FileDescriptor {
            fd,
            ino: u64::from(node) + 1,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buffer: &mut [u8]) -> Result<usize, FsError> {
        let index = self.fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        let entry = self.fds[index];
        if entry.node == SELINUX_NODE_DIRECTORY {
            return Err(FsError::IsADirectory);
        }
        let mut data = [0u8; MAX_SELINUX_CONTEXT];
        let length = self.data(entry.node, &mut data);
        let start = usize::try_from(entry.offset).map_err(|_| FsError::InvalidSeek)?;
        if start >= length {
            return Ok(0);
        }
        let count = buffer.len().min(length - start);
        buffer[..count].copy_from_slice(&data[start..start + count]);
        self.fds[index].offset = entry
            .offset
            .checked_add(count as u64)
            .ok_or(FsError::InvalidSeek)?;
        Ok(count)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let index = self.fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        let entry = self.fds[index];
        if entry.node == SELINUX_NODE_DIRECTORY {
            return Err(FsError::IsADirectory);
        }
        match entry.node {
            SELINUX_NODE_ENFORCE => {
                self.enforcing = match data.first().copied() {
                    Some(b'0') => false,
                    Some(b'1') => true,
                    _ => return Err(FsError::InvalidInput),
                };
            }
            SELINUX_NODE_CHECKREQPROT | SELINUX_NODE_DENY_UNKNOWN => {
                if !matches!(data.first().copied(), Some(b'0' | b'1')) {
                    return Err(FsError::InvalidInput);
                }
            }
            SELINUX_NODE_LOAD => {
                if data.len() > self.policy_buffer.len() {
                    return Err(FsError::InvalidInput);
                }
                self.policy_buffer = [0; MAX_SELINUX_POLICY_BYTES];
                self.policy_buffer[..data.len()].copy_from_slice(data);
                self.policy_buffer_length = data.len();
                self.parse_policy()?;
            }
            SELINUX_NODE_CONTEXT => {
                if data.is_empty()
                    || data.len() > self.context.len()
                    || data.iter().any(|byte| *byte == 0)
                {
                    return Err(FsError::InvalidInput);
                }
                self.context[..data.len()].copy_from_slice(data);
                self.context_length = data.len();
            }
            SELINUX_NODE_COMMIT_PENDING_BOOLEANS => {}
            _ => return Err(FsError::PermissionDenied),
        }
        self.publish_policy_state();
        self.fds[index].offset = entry
            .offset
            .checked_add(data.len() as u64)
            .ok_or(FsError::InvalidSeek)?;
        Ok(data.len())
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let index = self.fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        self.fds[index] = SelinuxFd::EMPTY;
        Ok(())
    }

    fn seek(&mut self, fd: u32, position: u64) -> Result<(), FsError> {
        let index = self.fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        self.fds[index].offset = position;
        Ok(())
    }

    fn position(&mut self, fd: u32) -> Result<u64, FsError> {
        let index = self.fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        Ok(self.fds[index].offset)
    }

    fn size(&mut self, fd: u32) -> Result<u64, FsError> {
        let index = self.fd_index(fd).ok_or(FsError::InvalidFileDescriptor)?;
        Ok(self.node_size(self.fds[index].node))
    }

    fn create(&mut self, _path: &str, _kind: InodeType) -> Option<u64> {
        None
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    fn unlink(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    fn readdir(&mut self, path: &str) -> Result<alloc::vec::Vec<VNode>, FsError> {
        if Self::node(path) != Some(SELINUX_NODE_DIRECTORY) {
            return Err(FsError::NotADirectory);
        }
        let names = [
            "enforce",
            "checkreqprot",
            "policyvers",
            "load",
            "context",
            "deny_unknown",
            "commit_pending_bools",
        ];
        let mut entries = alloc::vec::Vec::new();
        for (index, name) in names.iter().enumerate() {
            entries.push(VNode {
                name: alloc::string::String::from(*name),
                size: self.node_size((index + 1) as u8),
                is_dir: false,
            });
        }
        Ok(entries)
    }

    fn exists(&mut self, path: &str) -> bool {
        Self::node(path).is_some()
    }
}

fn selinux_transition_allowed_in_policy(
    policy: &SelinuxPolicyState,
    source: &[u8],
    target: &[u8],
) -> bool {
    if !policy.enforcing || !policy.loaded {
        return true;
    }
    policy.rules.iter().take(policy.rule_count).any(|rule| {
        rule.active
            && rule.source_length == source.len()
            && rule.target_length == target.len()
            && rule.source[..rule.source_length] == source[..]
            && rule.target[..rule.target_length] == target[..]
    })
}

fn selinux_transition_allowed_from(source: &[u8], target: &[u8]) -> bool {
    let policy = unsafe { *core::ptr::addr_of!(SELINUX_POLICY_STATE) };
    selinux_transition_allowed_in_policy(&policy, source, target)
}

fn selinux_transition_allowed(target: &[u8]) -> bool {
    let mut source = [0u8; MAX_SELINUX_CONTEXT];
    let source_length = task::current_selinux_context(&mut source);
    selinux_transition_allowed_from(&source[..source_length], target)
}

/// Check the bounded userdebug debug-domain transition used by `adb root`.
///
/// This is deliberately a named boundary instead of making the ADB transport
/// reach into the policy table.  It models the exact contexts needed by the
/// bring-up image (`adbd` -> `su`); it is not an AOSP policydb or AVC parser.
pub(super) fn debug_adbd_to_su_transition_allowed() -> bool {
    selinux_transition_allowed_from(b"u:r:adbd:s0", b"u:r:su:s0")
}

fn selinux_object_access_allowed(path: &str, requested: u8) -> bool {
    let policy = unsafe { *core::ptr::addr_of!(SELINUX_POLICY_STATE) };
    if !policy.enforcing || !policy.loaded || policy.allow_rule_count == 0 {
        return true;
    }
    let path = path.as_bytes();
    let mut source = [0u8; MAX_SELINUX_CONTEXT];
    let source_length = task::current_selinux_context(&mut source);
    let mut object_is_labeled = false;
    for rule in policy.allow_rules.iter().take(policy.allow_rule_count) {
        if !rule.active
            || rule.object_length != path.len()
            || rule.object[..rule.object_length] != path[..]
        {
            continue;
        }
        object_is_labeled = true;
        if rule.source_length == source_length
            && rule.source[..rule.source_length] == source[..source_length]
            && rule.permissions & requested == requested
        {
            return true;
        }
    }
    // Keep the compatibility surface bounded: FSP2 only places exact rules on
    // objects named by the policy. A path with no Rust label is not silently
    // treated as a denied Android object because this is not an AOSP policydb.
    !object_is_labeled
}

fn android_dev_ino(path: &str) -> u64 {
    let mut value = 0u64;
    for byte in path.bytes() {
        value = value.wrapping_mul(33).wrapping_add(byte as u64);
    }
    value | 0x2000_0000_0000_0000
}

impl OpenFile {
    const EMPTY: Self = Self {
        owner_pid: 0,
        handle_index: 0,
        local_fd: 0,
        mount_index: 0,
        generation: 0,
        kind: KIND_FILE,
        pipe_slot: 0,
        selinux_attr: SELINUX_ATTR_NONE,
        active: false,
    };
}

impl DirectorySlot {
    const EMPTY: Self = Self {
        path: [0; MAX_PATH],
        path_length: 0,
        references: 0,
        active: false,
    };
}

impl LinuxFdEntry {
    const EMPTY: Self = Self {
        owner_pid: 0,
        fd: 0,
        native_handle: 0,
        stdio_fd: LINUX_STDIO_NONE,
        kind: LINUX_FD_NATIVE,
        resource_slot: 0,
        flags: 0,
        active: false,
    };
}

impl LinuxSocketSlot {
    const EMPTY: Self = Self {
        buffer: [0; SOCKET_BUFFER_CAPACITY],
        property_staging: [0; 256],
        property_staging_length: 0,
        read_position: 0,
        write_position: 0,
        length: 0,
        peer: SOCKET_SLOT_NONE,
        references: 0,
        socket_type: 0,
        bound_path: [0; SOCKET_PATH_CAPACITY],
        bound_length: 0,
        listening: false,
        connected: false,
        nonblocking: false,
        passcred: false,
        pending: SOCKET_SLOT_NONE,
        active: false,
    };
}

impl LinuxEventFdSlot {
    const EMPTY: Self = Self {
        counter: 0,
        semaphore: false,
        references: 0,
        active: false,
    };
}

impl LinuxInotifySlot {
    const EMPTY: Self = Self {
        next_watch: 1,
        references: 0,
        active: false,
    };
}

impl LinuxSignalFdSlot {
    const EMPTY: Self = Self {
        mask: 0,
        references: 0,
        active: false,
    };
}

impl EpollWatch {
    const EMPTY: Self = Self {
        fd: 0,
        events: 0,
        data: 0,
        active: false,
    };
}

impl EpollSlot {
    const EMPTY: Self = Self {
        watches: [EpollWatch::EMPTY; MAX_EPOLL_WATCHES],
        references: 0,
        active: false,
    };
}

impl PipeSlot {
    const EMPTY: Self = Self {
        buffer: [0; PIPE_CAPACITY],
        read_position: 0,
        write_position: 0,
        length: 0,
        references: 0,
        active: false,
    };
}

impl ChannelMessage {
    const EMPTY: Self = Self {
        bytes: [0; CHANNEL_MESSAGE_CAPACITY],
        length: 0,
    };
}

impl ChannelSlot {
    const EMPTY: Self = Self {
        messages: [ChannelMessage::EMPTY; MAX_CHANNEL_MESSAGES],
        head: 0,
        length: 0,
        references: 0,
        active: false,
    };
}

impl EventSlot {
    const EMPTY: Self = Self {
        signaled: false,
        manual_reset: false,
        references: 0,
        active: false,
    };
}

impl TimerSlot {
    const EMPTY: Self = Self {
        event_owner_pid: 0,
        event_handle: 0,
        deadline_ns: 0,
        fired: false,
        references: 0,
        active: false,
    };
}

impl ThreadSlot {
    const EMPTY: Self = Self {
        target_pid: 0,
        detached: false,
        exited: false,
        exit_status: 0,
        references: 0,
        active: false,
    };
}

impl SharedBufferSlot {
    const EMPTY: Self = Self {
        frames: [0; MAX_SHARED_BUFFER_PAGES],
        page_count: 0,
        length: 0,
        flags: 0,
        references: 0,
        active: false,
    };
}

impl SharedMapping {
    const EMPTY: Self = Self {
        buffer_slot: 0,
        owner_pid: 0,
        address: 0,
        length: 0,
        active: false,
    };
}

impl LinuxPropertyMapping {
    const EMPTY: Self = Self {
        owner_pid: 0,
        address: 0,
        length: 0,
        active: false,
    };
}

impl TerminalSlot {
    const EMPTY: Self = Self {
        title: [0; MAX_TERMINAL_TITLE],
        title_length: 0,
        references: 0,
        active: false,
    };
}

static mut VFS: Option<Mutex<Vfs>> = None;
static mut OPEN_FILES: [OpenFile; MAX_OPEN_FILES] = [OpenFile::EMPTY; MAX_OPEN_FILES];
static mut LINUX_FDS: [LinuxFdEntry; MAX_LINUX_FD_ENTRIES] =
    [LinuxFdEntry::EMPTY; MAX_LINUX_FD_ENTRIES];
static mut LINUX_SOCKET_SLOTS: [LinuxSocketSlot; MAX_LINUX_SOCKET_SLOTS] =
    [LinuxSocketSlot::EMPTY; MAX_LINUX_SOCKET_SLOTS];
static mut LINUX_EVENTFD_SLOTS: [LinuxEventFdSlot; MAX_LINUX_EVENTFD_SLOTS] =
    [LinuxEventFdSlot::EMPTY; MAX_LINUX_EVENTFD_SLOTS];
static mut LINUX_INOTIFY_SLOTS: [LinuxInotifySlot; MAX_LINUX_INOTIFY_SLOTS] =
    [LinuxInotifySlot::EMPTY; MAX_LINUX_INOTIFY_SLOTS];
static mut LINUX_SIGNALFD_SLOTS: [LinuxSignalFdSlot; MAX_LINUX_SIGNALFD_SLOTS] =
    [LinuxSignalFdSlot::EMPTY; MAX_LINUX_SIGNALFD_SLOTS];
static mut EPOLL_SLOTS: [EpollSlot; MAX_EPOLL_SLOTS] = [EpollSlot::EMPTY; MAX_EPOLL_SLOTS];
static mut PIPE_SLOTS: [PipeSlot; MAX_PIPE_SLOTS] = [PipeSlot::EMPTY; MAX_PIPE_SLOTS];
static mut CHANNEL_SLOTS: [ChannelSlot; MAX_CHANNEL_SLOTS] =
    [ChannelSlot::EMPTY; MAX_CHANNEL_SLOTS];
static mut EVENT_SLOTS: [EventSlot; MAX_EVENT_SLOTS] = [EventSlot::EMPTY; MAX_EVENT_SLOTS];
static mut TIMER_SLOTS: [TimerSlot; MAX_TIMER_SLOTS] = [TimerSlot::EMPTY; MAX_TIMER_SLOTS];
static mut THREAD_SLOTS: [ThreadSlot; MAX_THREAD_SLOTS] = [ThreadSlot::EMPTY; MAX_THREAD_SLOTS];
static mut SHARED_BUFFER_SLOTS: [SharedBufferSlot; MAX_SHARED_BUFFER_SLOTS] =
    [SharedBufferSlot::EMPTY; MAX_SHARED_BUFFER_SLOTS];
static mut SHARED_MAPPINGS: [SharedMapping; MAX_SHARED_MAPPINGS] =
    [SharedMapping::EMPTY; MAX_SHARED_MAPPINGS];
static mut LINUX_PROPERTY_AREA_FRAMES: [u64; ANDROID_PROPERTY_AREA_PAGES] =
    [0; ANDROID_PROPERTY_AREA_PAGES];
static mut LINUX_PROPERTY_AREA_ACTIVE: bool = false;
static mut ANDROID_PROPERTIES: [AndroidProperty; MAX_ANDROID_PROPERTIES] =
    [AndroidProperty::EMPTY; MAX_ANDROID_PROPERTIES];
static mut ANDROID_PROPERTIES_INITIALIZED: bool = false;
static mut ANDROID_PROPERTY_SERIAL: u32 = 1;
static mut DEBUG_MOUNT_TABLE: [u8; DEBUG_MOUNT_TABLE_CAPACITY] = [0; DEBUG_MOUNT_TABLE_CAPACITY];
static mut DEBUG_MOUNT_TABLE_LENGTH: usize = 0;
static mut LINUX_PROPERTY_MAPPINGS: [LinuxPropertyMapping; MAX_LINUX_PROPERTY_MAPPINGS] =
    [LinuxPropertyMapping::EMPTY; MAX_LINUX_PROPERTY_MAPPINGS];
static mut TERMINAL_SLOTS: [TerminalSlot; MAX_TERMINAL_SLOTS] =
    [TerminalSlot::EMPTY; MAX_TERMINAL_SLOTS];
static mut DIRECTORY_SLOTS: [DirectorySlot; MAX_DIRECTORY_SLOTS] =
    [DirectorySlot::EMPTY; MAX_DIRECTORY_SLOTS];
static mut NEXT_GENERATION: u64 = 1;

/// Install the initial root by unpacking the build-produced `newc` archive.
/// The filesystem object is locked even though the current bring-up scheduler
/// is single-core, so the syscall boundary does not bake that temporary
/// scheduling detail into its ownership contract.
pub(crate) fn init() {
    unsafe {
        *core::ptr::addr_of_mut!(VFS) = Some(Mutex::new(Vfs::new(Box::new(MemFileSystem::new()))));
    }
    let unpacked = with_vfs(|vfs| unpack_initramfs(vfs, INITRAMFS)).flatten();
    if let Some(count) = unpacked {
        uart::puts("aarch64 fs: initramfs entries=");
        uart::put_hex_value(count as u64);
    } else {
        uart::puts("aarch64 fs: initramfs setup failed\n");
    }
    match with_vfs(mount_android_virtual_filesystems) {
        Some(Ok(())) => uart::puts("aarch64 fs: Android virtual filesystems ready\n"),
        _ => uart::puts("aarch64 fs: Android virtual filesystem setup failed\n"),
    }
}

/// Install the static early-userspace filesystem boundary before Android init
/// is entered.  `/dev` has explicit harmless device semantics; `/proc` and
/// `/sys` expose only the bounded metadata needed for early discovery.  They
/// are intentionally not presented as dynamic kernel views.
fn mount_android_virtual_filesystems(vfs: &mut Vfs) -> Result<(), FsError> {
    for path in ["/dev", "/proc", "/sys"] {
        if !vfs.exists(path) {
            vfs.mkdir(path)?;
        }
    }
    vfs.mount("/dev", Box::new(AndroidDevFs::new()))?;
    vfs.mount("/proc", Box::new(MemFileSystem::new()))?;
    vfs.mount("/sys", Box::new(MemFileSystem::new()))?;

    for path in [
        "/proc/self",
        "/proc/self/fd",
        "/proc/self/attr",
        "/proc/1",
        "/proc/1/attr",
        "/proc/sys",
        "/proc/sys/kernel",
        "/proc/sys/fs",
        "/sys/class",
        "/sys/class/android_usb",
        "/sys/devices",
        "/sys/fs",
        "/sys/fs/cgroup",
        "/sys/fs/selinux",
        "/sys/firmware",
        "/sys/firmware/devicetree",
        "/sys/firmware/devicetree/base",
    ] {
        let _ = vfs.mkdir(path);
    }
    vfs.mount("/sys/fs/selinux", Box::new(SelinuxFs::new()))?;

    let default_mounts = b"proc /proc proc ro 0 0\nsysfs /sys sysfs ro 0 0\n";
    update_debug_mount_table(default_mounts);
    for (path, contents) in [
        (
            "/proc/cmdline",
            b"console=ttyMSM0 androidboot.hardware=bramble\n".as_slice(),
        ),
        (
            "/proc/version",
            b"FullereneOS Linux compatibility boundary\n".as_slice(),
        ),
        (
            "/proc/filesystems",
            b"nodev\tproc\nnodev\tsysfs\nnodev\ttmpfs\next4\neroFS\nf2fs\n".as_slice(),
        ),
        ("/proc/mounts", default_mounts.as_slice()),
        (
            "/proc/meminfo",
            b"MemTotal:       262144 kB\nMemFree:        131072 kB\n".as_slice(),
        ),
        ("/proc/self/cmdline", b"init\0".as_slice()),
        ("/proc/1/cmdline", b"init\0".as_slice()),
        ("/proc/self/attr/current", b"u:r:init:s0\0".as_slice()),
        ("/proc/1/attr/current", b"u:r:init:s0\0".as_slice()),
        ("/proc/self/attr/exec", b"".as_slice()),
        ("/proc/self/attr/fscreate", b"".as_slice()),
        ("/proc/self/attr/keycreate", b"".as_slice()),
        ("/proc/self/attr/sockcreate", b"".as_slice()),
        ("/proc/1/attr/exec", b"".as_slice()),
        ("/proc/1/attr/fscreate", b"".as_slice()),
        ("/proc/1/attr/keycreate", b"".as_slice()),
        ("/proc/1/attr/sockcreate", b"".as_slice()),
        (
            "/proc/self/status",
            b"Name:\tinit\nState:\tR (running)\nPid:\t1\nPPid:\t0\n".as_slice(),
        ),
        (
            "/proc/1/status",
            b"Name:\tinit\nState:\tR (running)\nPid:\t1\nPPid:\t0\n".as_slice(),
        ),
        ("/proc/sys/kernel/ostype", b"Linux\n".as_slice()),
        ("/proc/sys/kernel/osrelease", b"0.1-fullerene\n".as_slice()),
        ("/proc/sys/kernel/hostname", b"fullerene\n".as_slice()),
        ("/sys/class/android_usb/state", b"CONFIGURED\n".as_slice()),
    ] {
        seed_vfs_file(vfs, path, contents)?;
    }
    Ok(())
}

fn seed_vfs_file(vfs: &mut Vfs, path: &str, data: &[u8]) -> Result<(), FsError> {
    if vfs.exists(path) {
        vfs.unlink(path)?;
    }
    if !vfs.exists(path) {
        vfs.create(path).ok_or(FsError::PermissionDenied)?;
    }
    let (mount_index, file) = vfs.open_with_mount(path, 0).ok_or(FsError::FileNotFound)?;
    let result = vfs.write_at(mount_index, file.fd, data);
    let closed = vfs.close_at(mount_index, file.fd);
    result.map(|_| ()).and(closed)
}

/// Probe the installed Bramble UFS backend through Genome's filesystem
/// boundary. This is deliberately optional and read-only: Android's GPT
/// `super` metadata is mapped first, then supported ext4/EROFS logical
/// partitions are mounted at `/system` and `/vendor`; raw `userdata` is
/// mounted at `/data` when its F2FS checkpoint is clean. FAT/exFAT remains an
/// independent `/storage` fallback.
#[cfg(fullerene_aarch64_bramble)]
pub(crate) fn mount_bramble_ufs() {
    let Some(mut probe) = ufs::bramble_read_only_handle() else {
        uart::puts("aarch64 fs: UFS filesystem probe skipped; no device\n");
        return;
    };
    let table = match genome::gpt::scan(&mut probe) {
        Ok(table) => {
            uart::puts("aarch64 fs: UFS GPT partitions=");
            uart::put_hex_value(table.partitions.len() as u64);
            for partition in &table.partitions {
                uart::put_hex("aarch64 fs: UFS GPT partition start=", partition.first_lba);
                uart::put_hex(" end=", partition.last_lba);
                uart::puts(" name=");
                let name = partition.name();
                uart::puts(&name);
                uart::puts("\n");
            }
            Some(table)
        }
        Err(genome::gpt::GptError::InvalidSignature) => {
            uart::puts("aarch64 fs: UFS has no primary GPT signature\n");
            None
        }
        Err(error) => {
            let _ = error;
            uart::puts("aarch64 fs: UFS GPT probe rejected\n");
            None
        }
    };

    // Expose the physical GPT names before Android init gets a chance to
    // consume fstab.  Logical `super` entries are added below once LP
    // metadata has been validated.
    if let (Some(table), Some((sector_size, _))) = (table.as_ref(), ufs::bramble_block_info()) {
        for partition in &table.partitions {
            let sectors = partition
                .last_lba
                .saturating_sub(partition.first_lba)
                .saturating_add(1);
            let size = sectors.saturating_mul(sector_size as u64);
            let name = partition.name();
            register_android_block_alias(&name, size);
        }
    }

    let mut system_mounted = false;
    let mut vendor_mounted = false;
    let mut userdata_mounted = false;
    let mut system_kind = None;
    let mut vendor_kind = None;
    let mut userdata_kind = None;

    if let Some(super_partition) = table
        .as_ref()
        .and_then(|table| {
            table
                .partitions
                .iter()
                .find(|partition| partition.name() == "super")
        })
        .copied()
    {
        if let Some((sector_size, _)) = ufs::bramble_block_info() {
            let partition_sectors = super_partition
                .last_lba
                .saturating_sub(super_partition.first_lba)
                .saturating_add(1);
            let base_bytes = super_partition.first_lba.checked_mul(sector_size as u64);
            let total_bytes = partition_sectors.checked_mul(sector_size as u64);
            if let (Some(base_bytes), Some(total_bytes)) = (base_bytes, total_bytes)
                && base_bytes % genome::android_lp::LP_SECTOR_SIZE == 0
                && total_bytes % genome::android_lp::LP_SECTOR_SIZE == 0
            {
                if let Some(super_handle) = ufs::bramble_read_only_handle() {
                    let super_device = Sector512Device::new(
                        Box::new(super_handle),
                        base_bytes / genome::android_lp::LP_SECTOR_SIZE,
                        total_bytes / genome::android_lp::LP_SECTOR_SIZE,
                    );
                    match genome::android_lp::read_metadata(Box::new(super_device), 0) {
                        Ok(metadata) => {
                            for partition in &metadata.partitions {
                                let size = android_lp_partition_size(&metadata, &partition.name);
                                register_android_block_alias(&partition.name, size);
                                // Android fstab commonly uses the unsuffixed
                                // first-slot name even when LP metadata stores
                                // the concrete `_a` name.
                                if let Some(base) = partition.name.strip_suffix("_a") {
                                    register_android_block_alias(base, size);
                                }
                            }
                            uart::puts("aarch64 fs: Android LP partitions=");
                            uart::put_hex_value(metadata.partitions.len() as u64);
                            for partition in &metadata.partitions {
                                uart::puts(" name=");
                                uart::puts(&partition.name);
                            }
                            uart::puts("\n");
                            for candidate in ["system", "system_a", "vendor", "vendor_a"] {
                                let Some(super_handle) = ufs::bramble_read_only_handle() else {
                                    break;
                                };
                                let super_device = Sector512Device::new(
                                    Box::new(super_handle),
                                    base_bytes / genome::android_lp::LP_SECTOR_SIZE,
                                    total_bytes / genome::android_lp::LP_SECTOR_SIZE,
                                );
                                if let Ok(logical) = genome::android_lp::LinearBlockDevice::new(
                                    &metadata,
                                    Box::new(super_device),
                                    candidate,
                                ) {
                                    let logical_sectors = logical.total_sectors();
                                    uart::puts("aarch64 fs: Android LP mapped ");
                                    uart::puts(candidate);
                                    uart::put_hex(" sectors=", logical_sectors);
                                    uart::puts("\n");

                                    let (mount_point, mounted, mounted_kind) =
                                        if candidate.starts_with("system") {
                                            ("/system", &mut system_mounted, &mut system_kind)
                                        } else {
                                            ("/vendor", &mut vendor_mounted, &mut vendor_kind)
                                        };
                                    if !*mounted {
                                        match android_fs::mount(Box::new(logical)) {
                                            Ok((filesystem, kind)) => {
                                                let result = with_vfs(|vfs| {
                                                    if !vfs.exists(mount_point) {
                                                        vfs.mkdir(mount_point)?;
                                                    }
                                                    vfs.mount(mount_point, filesystem)
                                                });
                                                match result {
                                                    Some(Ok(())) => {
                                                        *mounted = true;
                                                        *mounted_kind = Some(kind);
                                                        uart::puts("aarch64 fs: Android ");
                                                        uart::puts(match kind {
                                                            AndroidFilesystemKind::Ext4 => "ext4",
                                                            AndroidFilesystemKind::Erofs => "EROFS",
                                                            AndroidFilesystemKind::F2fs => "F2FS",
                                                        });
                                                        uart::puts(" mounted at ");
                                                        uart::puts(mount_point);
                                                        uart::puts("\n");
                                                    }
                                                    Some(Err(_)) => uart::puts(
                                                        "aarch64 fs: Android VFS mount failed\n",
                                                    ),
                                                    None => uart::puts(
                                                        "aarch64 fs: Android VFS unavailable\n",
                                                    ),
                                                }
                                            }
                                            Err(_) => uart::puts(
                                                "aarch64 fs: Android LP partition filesystem unsupported\n",
                                            ),
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let _ = error;
                            uart::puts("aarch64 fs: Android LP metadata unavailable\n");
                        }
                    }
                }
            }
        }
    }

    if let Some(userdata_partition) = table.as_ref().and_then(|table| {
        table
            .partitions
            .iter()
            .find(|partition| partition.name() == "userdata")
            .copied()
    }) {
        if let Some((sector_size, _)) = ufs::bramble_block_info() {
            let partition_sectors = userdata_partition
                .last_lba
                .saturating_sub(userdata_partition.first_lba)
                .saturating_add(1);
            let base_bytes = userdata_partition.first_lba.checked_mul(sector_size as u64);
            let total_bytes = partition_sectors.checked_mul(sector_size as u64);
            if let (Some(base_bytes), Some(total_bytes)) = (base_bytes, total_bytes)
                && base_bytes % genome::android_lp::LP_SECTOR_SIZE == 0
                && total_bytes % genome::android_lp::LP_SECTOR_SIZE == 0
            {
                if let Some(userdata_handle) = ufs::bramble_read_only_handle() {
                    let userdata_device = Sector512Device::new(
                        Box::new(userdata_handle),
                        base_bytes / genome::android_lp::LP_SECTOR_SIZE,
                        total_bytes / genome::android_lp::LP_SECTOR_SIZE,
                    );
                    match android_fs::mount(Box::new(userdata_device)) {
                        Ok((filesystem, AndroidFilesystemKind::F2fs)) => {
                            let result = with_vfs(|vfs| {
                                if !vfs.exists("/data") {
                                    vfs.mkdir("/data")?;
                                }
                                vfs.mount("/data", filesystem)
                            });
                            match result {
                                Some(Ok(())) => {
                                    userdata_mounted = true;
                                    userdata_kind = Some(AndroidFilesystemKind::F2fs);
                                    uart::puts(
                                        "aarch64 fs: Android F2FS userdata mounted at /data\n",
                                    )
                                }
                                Some(Err(_)) => {
                                    uart::puts("aarch64 fs: Android userdata VFS mount failed\n")
                                }
                                None => {
                                    uart::puts("aarch64 fs: Android userdata VFS unavailable\n")
                                }
                            }
                        }
                        Ok((_filesystem, _)) => {
                            uart::puts("aarch64 fs: Android userdata is not F2FS\n")
                        }
                        Err(_) => {
                            uart::puts("aarch64 fs: Android userdata filesystem unsupported\n")
                        }
                    }
                }
            }
        }
    }

    publish_android_mount_table(system_kind, vendor_kind, userdata_kind);

    let Some(device) = ufs::bramble_read_only_handle() else {
        uart::puts("aarch64 fs: UFS filesystem probe skipped; device disappeared\n");
        return;
    };
    let mounted = match genome::fat::mount_device(Box::new(device) as Box<dyn BlockDevice>) {
        Ok(filesystem) => with_vfs(|vfs| {
            if !vfs.exists("/storage") {
                vfs.mkdir("/storage")?;
            }
            vfs.mount("/storage", filesystem)
        }),
        Err((error, _device)) => {
            let _ = error;
            uart::puts("aarch64 fs: UFS filesystem probe found no FAT/exFAT volume\n");
            return;
        }
    };
    match mounted {
        Some(Ok(())) => uart::puts("aarch64 fs: UFS FAT/exFAT mounted at /storage\n"),
        Some(Err(_)) => uart::puts("aarch64 fs: UFS filesystem mount failed\n"),
        None => uart::puts("aarch64 fs: UFS filesystem mount unavailable\n"),
    }
}

/// Refresh the bounded `/proc/mounts` view after the guarded physical mounts
/// have completed.  Android's early userspace uses this to reconcile the
/// fstab requests with the filesystems that were actually accepted.
#[cfg(fullerene_aarch64_bramble)]
fn publish_android_mount_table(
    system: Option<AndroidFilesystemKind>,
    vendor: Option<AndroidFilesystemKind>,
    userdata: Option<AndroidFilesystemKind>,
) {
    let mut mounts = String::from("proc /proc proc ro 0 0\nsysfs /sys sysfs ro 0 0\n");
    if let Some(kind) = system {
        mounts.push_str("/dev/block/by-name/system /system ");
        mounts.push_str(android_filesystem_name(kind));
        mounts.push_str(" ro 0 0\n");
    }
    if let Some(kind) = vendor {
        mounts.push_str("/dev/block/by-name/vendor /vendor ");
        mounts.push_str(android_filesystem_name(kind));
        mounts.push_str(" ro 0 0\n");
    }
    if let Some(kind) = userdata {
        mounts.push_str("/dev/block/by-name/userdata /data ");
        mounts.push_str(android_filesystem_name(kind));
        mounts.push_str(" ro 0 0\n");
    }
    update_debug_mount_table(mounts.as_bytes());
    let _ = with_vfs(|vfs| seed_vfs_file(vfs, "/proc/mounts", mounts.as_bytes()));
}

#[cfg(fullerene_aarch64_bramble)]
fn android_filesystem_name(kind: AndroidFilesystemKind) -> &'static str {
    match kind {
        AndroidFilesystemKind::Ext4 => "ext4",
        AndroidFilesystemKind::Erofs => "erofs",
        AndroidFilesystemKind::F2fs => "f2fs",
    }
}

fn seed_file(vfs: &mut Vfs, path: &str, data: &[u8]) -> bool {
    let Some(_) = vfs.create(path) else {
        return false;
    };
    let Some(file) = vfs.open(path, 0) else {
        return false;
    };
    let written = vfs
        .write_at(0, file.fd, data)
        .is_ok_and(|written| written == data.len());
    let closed = vfs.close_at(0, file.fd).is_ok();
    written && closed
}

fn unpack_initramfs(vfs: &mut Vfs, archive: &[u8]) -> Option<usize> {
    let mut offset = 0usize;
    let mut count = 0usize;
    while offset < archive.len() {
        if count == MAX_INITRAMFS_ENTRIES {
            return None;
        }
        let header_end = offset.checked_add(CPIO_HEADER_SIZE)?;
        let header = archive.get(offset..header_end)?;
        if header.get(..6)? != b"070701" {
            return None;
        }
        let mode = cpio_hex(header.get(14..22)?)?;
        let file_size = cpio_hex(header.get(54..62)?)? as usize;
        let name_size = cpio_hex(header.get(94..102)?)? as usize;
        if name_size == 0 {
            return None;
        }
        let name_end = header_end.checked_add(name_size)?;
        let name_with_nul = archive.get(header_end..name_end)?;
        if name_with_nul.last().copied()? != 0 {
            return None;
        }
        let name = name_with_nul.get(..name_size - 1)?;
        let body_start = align_up(name_end, 4)?;
        if name == b"TRAILER!!!" {
            return Some(count);
        }
        let body_end = body_start.checked_add(file_size)?;
        let body = archive.get(body_start..body_end)?;
        let path = cpio_path(name)?;
        let file_type = mode & 0o170000;
        if file_type == 0o040000 {
            if vfs.mkdir(path).is_ok() {
                count += 1;
            } else if !vfs.exists(path) {
                return None;
            }
        } else if file_type == 0o100000 {
            if !seed_file(vfs, path, body) {
                return None;
            }
            count += 1;
        } else if file_type != 0o120000 {
            return None;
        }
        offset = align_up(body_end, 4)?;
    }
    None
}

fn cpio_hex(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0u32;
    for &byte in bytes {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        value = value.checked_mul(16)?.checked_add(digit as u32)?;
    }
    Some(value)
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

fn cpio_path(name: &[u8]) -> Option<&str> {
    if name.is_empty() || name.starts_with(b"/") || name.contains(&b'\\') {
        return None;
    }
    let mut component_start = 0usize;
    while component_start < name.len() {
        let component_end = name[component_start..]
            .iter()
            .position(|&byte| byte == b'/')
            .map(|offset| component_start + offset)
            .unwrap_or(name.len());
        if component_end == component_start
            || name.get(component_start..component_end) == Some(b"..")
        {
            return None;
        }
        component_start = component_end.saturating_add(1);
    }
    core::str::from_utf8(name).ok().map(|path| {
        // The archive is generated with relative names; Vfs accepts the
        // absolute spelling used by the syscall layer.
        let _ = path;
        path
    })
}

fn install_handle(
    owner_pid: u64,
    local_fd: u32,
    mount_index: usize,
    kind: u8,
    pipe_slot: u8,
) -> Result<u64, u64> {
    let (storage_index, handle_index) = unsafe {
        let files = core::ptr::addr_of_mut!(OPEN_FILES);
        let handle_index = (0..MAX_HANDLE_SLOTS).find(|index| {
            !(*files).iter().any(|file| {
                file.active && file.owner_pid == owner_pid && file.handle_index == *index as u16
            })
        });
        let storage_index = (*files).iter().position(|file| !file.active);
        (storage_index, handle_index)
    };
    let (Some(storage_index), Some(handle_index)) = (storage_index, handle_index) else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    if kind != KIND_FILE {
        let valid = retain_resource(kind, pipe_slot);
        if !valid {
            return Err(ERR_BAD_FD);
        }
    }
    let generation = unsafe {
        let generation = *core::ptr::addr_of!(NEXT_GENERATION);
        *core::ptr::addr_of_mut!(NEXT_GENERATION) = generation.wrapping_add(1).max(1);
        (*core::ptr::addr_of_mut!(OPEN_FILES))[storage_index] = OpenFile {
            owner_pid,
            handle_index: handle_index as u16,
            local_fd,
            mount_index,
            generation,
            kind,
            pipe_slot,
            selinux_attr: SELINUX_ATTR_NONE,
            active: true,
        };
        generation
    };
    Ok(FILE_HANDLE_TAG | (generation << FILE_HANDLE_GENERATION_SHIFT) | handle_index as u64)
}

pub(crate) fn install_device_handle(owner_pid: u64, device_slot: u8) -> Result<u64, u64> {
    install_handle(owner_pid, 0, 0, KIND_DEVICE, device_slot)
}

pub(crate) fn install_window_handle(owner_pid: u64, window_slot: u8) -> Result<u64, u64> {
    install_handle(owner_pid, 0, 0, KIND_WINDOW, window_slot)
}

fn next_generation() -> u64 {
    unsafe {
        let generation = *core::ptr::addr_of!(NEXT_GENERATION);
        *core::ptr::addr_of_mut!(NEXT_GENERATION) = generation.wrapping_add(1).max(1);
        generation
    }
}

fn retain_resource(kind: u8, slot: u8) -> bool {
    let slot = slot as usize;
    unsafe {
        match kind {
            KIND_PIPE_READ | KIND_PIPE_WRITE => {
                let Some(pipe) = (*core::ptr::addr_of_mut!(PIPE_SLOTS)).get_mut(slot) else {
                    return false;
                };
                if !pipe.active {
                    return false;
                }
                pipe.references = pipe.references.saturating_add(1);
                true
            }
            KIND_CHANNEL => {
                let Some(channel) = (*core::ptr::addr_of_mut!(CHANNEL_SLOTS)).get_mut(slot) else {
                    return false;
                };
                if !channel.active {
                    return false;
                }
                channel.references = channel.references.saturating_add(1);
                true
            }
            KIND_EVENT => {
                let Some(event) = (*core::ptr::addr_of_mut!(EVENT_SLOTS)).get_mut(slot) else {
                    return false;
                };
                if !event.active {
                    return false;
                }
                event.references = event.references.saturating_add(1);
                true
            }
            KIND_SHARED_BUFFER => {
                let Some(buffer) = (*core::ptr::addr_of_mut!(SHARED_BUFFER_SLOTS)).get_mut(slot)
                else {
                    return false;
                };
                if !buffer.active {
                    return false;
                }
                buffer.references = buffer.references.saturating_add(1);
                true
            }
            KIND_TIMER => {
                let Some(timer) = (*core::ptr::addr_of_mut!(TIMER_SLOTS)).get_mut(slot) else {
                    return false;
                };
                if !timer.active {
                    return false;
                }
                timer.references = timer.references.saturating_add(1);
                true
            }
            KIND_THREAD => {
                let Some(thread) = (*core::ptr::addr_of_mut!(THREAD_SLOTS)).get_mut(slot) else {
                    return false;
                };
                if !thread.active {
                    return false;
                }
                thread.references = thread.references.saturating_add(1);
                true
            }
            KIND_TERMINAL => {
                let Some(terminal) =
                    (*core::ptr::addr_of_mut!(TERMINAL_SLOTS)).get_mut(slot as usize)
                else {
                    return false;
                };
                if !terminal.active {
                    return false;
                }
                terminal.references = terminal.references.saturating_add(1);
                true
            }
            KIND_DIRECTORY => {
                let Some(directory) = (*core::ptr::addr_of_mut!(DIRECTORY_SLOTS)).get_mut(slot)
                else {
                    return false;
                };
                if !directory.active {
                    return false;
                }
                directory.references = directory.references.saturating_add(1);
                true
            }
            KIND_DEVICE => devices::retain(slot as u8),
            KIND_WINDOW => window::retain(slot as u8),
            _ => false,
        }
    }
}

pub(crate) fn open(path_address: u64, flags: u64, _mode: u64) -> u64 {
    let path_storage = match copy_path(path_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    let requested_access = match flags & 0x3 {
        0 => SELINUX_ACCESS_READ,
        1 => SELINUX_ACCESS_WRITE,
        2 => SELINUX_ACCESS_READ | SELINUX_ACCESS_WRITE,
        _ => return ERR_INVALID,
    };
    if !selinux_object_access_allowed(path, requested_access) {
        return ERR_PERMISSION;
    }
    let owner_pid = match task::resource_owner_pid() {
        Some(pid) => pid,
        None => return ERR_BAD_FD,
    };
    let Some((mount_index, local_fd)) = with_vfs(|vfs| {
        vfs.open_with_mount(path, flags as u32)
            .map(|(mount_index, file)| (mount_index, file.fd))
    })
    .flatten() else {
        return ERR_NO_ENTRY;
    };

    let is_directory = with_vfs(|vfs| vfs.readdir(path).is_ok()).unwrap_or(false);
    let directory_slot = if is_directory {
        let Some(slot) = (0..MAX_DIRECTORY_SLOTS)
            .find(|&index| unsafe { !(*core::ptr::addr_of!(DIRECTORY_SLOTS))[index].active })
        else {
            let _ = with_vfs(|vfs| vfs.close_at(mount_index, local_fd));
            return ERR_OUT_OF_MEMORY;
        };
        unsafe {
            (*core::ptr::addr_of_mut!(DIRECTORY_SLOTS))[slot] = DirectorySlot {
                path: path_storage.bytes,
                path_length: path_storage.length,
                references: 0,
                active: true,
            };
        }
        Some(slot as u8)
    } else {
        None
    };
    let kind = if is_directory {
        KIND_DIRECTORY
    } else {
        KIND_FILE
    };
    let handle = match install_handle(
        owner_pid,
        local_fd,
        mount_index,
        kind,
        directory_slot.unwrap_or(0),
    ) {
        Ok(handle) => handle,
        Err(error) => {
            if let Some(slot) = directory_slot {
                unsafe {
                    (*core::ptr::addr_of_mut!(DIRECTORY_SLOTS))[slot as usize] =
                        DirectorySlot::EMPTY;
                }
            }
            let _ = with_vfs(|vfs| vfs.close_at(mount_index, local_fd));
            error
        }
    };
    if (handle as i64) >= 0 {
        let selinux_attr = match path {
            "/proc/self/attr/current" => SELINUX_ATTR_CURRENT,
            "/proc/self/attr/exec" => SELINUX_ATTR_EXEC,
            _ => SELINUX_ATTR_NONE,
        };
        if selinux_attr != SELINUX_ATTR_NONE {
            if let Some((storage_index, _)) = locate_entry(handle) {
                unsafe {
                    (*core::ptr::addr_of_mut!(OPEN_FILES))[storage_index].selinux_attr =
                        selinux_attr;
                }
            }
        }
    }
    handle
}

/// Create a bounded process-owned terminal endpoint.
///
/// The native AArch64 bring-up has no framebuffer terminal service yet, so
/// the endpoint is backed by the platform UART.  It is still a real
/// owner/generation-checked capability: it can be inherited by fork, attached
/// to spawn, duplicated/transferred, and released on close or exit.
pub(crate) fn create_terminal(title_address: u64, length: u64) -> u64 {
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    if title_address == 0 || length == 0 || length > MAX_TERMINAL_TITLE {
        return ERR_INVALID;
    }
    let mut title = [0u8; MAX_TERMINAL_TITLE];
    if user_memory::copy_from_user(title_address, &mut title[..length]).is_err() {
        return ERR_ADDRESS;
    }
    let Ok(title_text) = core::str::from_utf8(&title[..length]) else {
        return ERR_INVALID;
    };
    if title_text.chars().any(|character| character.is_control()) {
        return ERR_INVALID;
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some(slot) = (0..MAX_TERMINAL_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(TERMINAL_SLOTS))[index].active })
    else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(TERMINAL_SLOTS))[slot] = TerminalSlot {
            title,
            title_length: length,
            references: 0,
            active: true,
        };
    }
    match install_handle(owner_pid, 0, 0, KIND_TERMINAL, slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(TERMINAL_SLOTS))[slot] = TerminalSlot::EMPTY;
            }
            error
        }
    }
}

fn create_pipe_handles() -> Result<[u64; 2], u64> {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return Err(ERR_BAD_FD);
    };
    let Some(pipe_slot) = (0..MAX_PIPE_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(PIPE_SLOTS))[index].active })
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    unsafe {
        (*core::ptr::addr_of_mut!(PIPE_SLOTS))[pipe_slot] = PipeSlot {
            active: true,
            ..PipeSlot::EMPTY
        };
    }

    let read_handle = match install_handle(owner_pid, 0, 0, KIND_PIPE_READ, pipe_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe { (*core::ptr::addr_of_mut!(PIPE_SLOTS))[pipe_slot] = PipeSlot::EMPTY };
            return Err(error);
        }
    };
    let write_handle = match install_handle(owner_pid, 0, 0, KIND_PIPE_WRITE, pipe_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = close(read_handle);
            unsafe { (*core::ptr::addr_of_mut!(PIPE_SLOTS))[pipe_slot] = PipeSlot::EMPTY };
            return Err(error);
        }
    };
    Ok([read_handle, write_handle])
}

pub(crate) fn pipe_create(buffer_address: u64) -> u64 {
    if buffer_address == 0 {
        return ERR_ADDRESS;
    }
    let [read_handle, write_handle] = match create_pipe_handles() {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let mut handles = [0u8; 16];
    handles[..8].copy_from_slice(&read_handle.to_ne_bytes());
    handles[8..].copy_from_slice(&write_handle.to_ne_bytes());
    if user_memory::copy_to_user(buffer_address, &handles).is_err() {
        let _ = close(read_handle);
        let _ = close(write_handle);
        return ERR_ADDRESS;
    }
    0
}

pub(crate) fn channel_create(_flags: u64) -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some(channel_slot) = (0..MAX_CHANNEL_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(CHANNEL_SLOTS))[index].active })
    else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(CHANNEL_SLOTS))[channel_slot] = ChannelSlot {
            active: true,
            ..ChannelSlot::EMPTY
        };
    }
    match install_handle(owner_pid, 0, 0, KIND_CHANNEL, channel_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(CHANNEL_SLOTS))[channel_slot] = ChannelSlot::EMPTY;
            }
            error
        }
    }
}

pub(crate) fn channel_send(handle: u64, buffer_address: u64, requested: u64) -> u64 {
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if count == 0 {
        return ERR_INVALID;
    }
    if count > CHANNEL_MESSAGE_CAPACITY {
        return ERR_OVERFLOW;
    }
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind != KIND_CHANNEL {
        return ERR_PERMISSION;
    }
    let mut buffer = [0u8; CHANNEL_MESSAGE_CAPACITY];
    if user_memory::copy_from_user(buffer_address, &mut buffer[..count]).is_err() {
        return ERR_ADDRESS;
    }
    unsafe {
        let Some(channel) =
            (*core::ptr::addr_of_mut!(CHANNEL_SLOTS)).get_mut(file.pipe_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !channel.active {
            return ERR_BAD_FD;
        }
        if channel.length == MAX_CHANNEL_MESSAGES {
            return ERR_WOULD_BLOCK;
        }
        let index = (channel.head + channel.length) % MAX_CHANNEL_MESSAGES;
        channel.messages[index].bytes[..count].copy_from_slice(&buffer[..count]);
        channel.messages[index].length = count;
        channel.length += 1;
    }
    count as u64
}

pub(crate) fn channel_recv(handle: u64, buffer_address: u64, requested: u64) -> u64 {
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if buffer_address == 0 || count == 0 || count > CHANNEL_MESSAGE_CAPACITY {
        return ERR_INVALID;
    }
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind != KIND_CHANNEL {
        return ERR_PERMISSION;
    }
    let mut buffer = [0u8; CHANNEL_MESSAGE_CAPACITY];
    let bytes_read = unsafe {
        let Some(channel) =
            (*core::ptr::addr_of_mut!(CHANNEL_SLOTS)).get_mut(file.pipe_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !channel.active {
            return ERR_BAD_FD;
        }
        if channel.length == 0 {
            return ERR_WOULD_BLOCK;
        }
        let message = &channel.messages[channel.head];
        let bytes_read = message.length.min(count);
        buffer[..bytes_read].copy_from_slice(&message.bytes[..bytes_read]);
        bytes_read
    };
    if user_memory::copy_to_user(buffer_address, &buffer[..bytes_read]).is_err() {
        return ERR_ADDRESS;
    }
    unsafe {
        let channel = &mut (*core::ptr::addr_of_mut!(CHANNEL_SLOTS))[file.pipe_slot as usize];
        channel.messages[channel.head].length = 0;
        channel.head = (channel.head + 1) % MAX_CHANNEL_MESSAGES;
        channel.length -= 1;
    }
    bytes_read as u64
}

fn shared_buffer_has_any_mapping(buffer_slot: u8) -> bool {
    unsafe {
        (*core::ptr::addr_of!(SHARED_MAPPINGS))
            .iter()
            .any(|mapping| mapping.active && mapping.buffer_slot == buffer_slot)
    }
}

fn shared_buffer_mapping_slot(
    buffer_slot: u8,
    owner_pid: u64,
    address: u64,
) -> Option<(usize, u64)> {
    unsafe {
        (*core::ptr::addr_of!(SHARED_MAPPINGS))
            .iter()
            .enumerate()
            .find(|(_, mapping)| {
                mapping.active
                    && mapping.buffer_slot == buffer_slot
                    && mapping.owner_pid == owner_pid
                    && mapping.address == address
            })
            .map(|(index, mapping)| (index, mapping.length))
    }
}

fn release_shared_buffer_slot(buffer_slot: u8) {
    let (frames, page_count) = unsafe {
        let Some(buffer) =
            (*core::ptr::addr_of_mut!(SHARED_BUFFER_SLOTS)).get_mut(buffer_slot as usize)
        else {
            return;
        };
        let frames = buffer.frames;
        let page_count = buffer.page_count;
        *buffer = SharedBufferSlot::EMPTY;
        (frames, page_count)
    };
    let _ = allocator::with_global(|frames_allocator| {
        frames_allocator.release_frames(&frames[..page_count])
    });
}

fn ensure_linux_property_area() -> bool {
    if unsafe { *core::ptr::addr_of!(LINUX_PROPERTY_AREA_ACTIVE) } {
        return true;
    }
    let active_space = task::current_address_space();
    mmu::activate_kernel_identity_space();
    let mut frames = [0u64; ANDROID_PROPERTY_AREA_PAGES];
    let mut allocated = 0usize;
    let allocation_ok = allocator::with_global(|frame_allocator| {
        while allocated < frames.len() {
            let Some(frame) = frame_allocator.next_frame() else {
                return false;
            };
            unsafe { core::ptr::write_bytes(frame as *mut u8, 0, 4096) };
            frames[allocated] = frame;
            allocated += 1;
        }
        true
    })
    .unwrap_or(false);
    if !allocation_ok {
        let _ = allocator::with_global(|frame_allocator| {
            frame_allocator.release_frames(&frames[..allocated])
        });
        if let Some(space_id) = active_space {
            let _ = mmu::activate_user_space(space_id);
        }
        return false;
    }
    unsafe {
        *core::ptr::addr_of_mut!(LINUX_PROPERTY_AREA_FRAMES) = frames;
        *core::ptr::addr_of_mut!(LINUX_PROPERTY_AREA_ACTIVE) = true;
    }
    android_property_table_init();
    if !android_property_area_rebuild() {
        unsafe {
            *core::ptr::addr_of_mut!(LINUX_PROPERTY_AREA_ACTIVE) = false;
        }
        let _ = allocator::with_global(|frame_allocator| frame_allocator.release_frames(&frames));
        if let Some(space_id) = active_space {
            let _ = mmu::activate_user_space(space_id);
        }
        return false;
    }
    if let Some(space_id) = active_space {
        let _ = mmu::activate_user_space(space_id);
    }
    true
}

pub(crate) fn linux_property_mmap(
    address_hint: u64,
    length: u64,
    offset: u64,
    protection: u64,
) -> Result<u64, u64> {
    if length == 0 || offset & 4095 != 0 || protection & 2 != 0 {
        return Err(ERR_INVALID);
    }
    let rounded = length.checked_add(4095).ok_or(ERR_OVERFLOW)? & !4095;
    let area_end = offset.checked_add(rounded).ok_or(ERR_OVERFLOW)?;
    if area_end > ANDROID_PROPERTY_AREA_SIZE {
        return Err(ERR_INVALID);
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return Err(ERR_BAD_FD);
    };
    let Some(mapping_slot) = (0..MAX_LINUX_PROPERTY_MAPPINGS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(LINUX_PROPERTY_MAPPINGS))[index].active })
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    if !ensure_linux_property_area() {
        return Err(ERR_OUT_OF_MEMORY);
    }
    let address = task::reserve_shared_mapping(address_hint, rounded, protection)?;
    let Some(space_id) = task::current_address_space() else {
        let _ = task::release_shared_mapping(address, rounded);
        return Err(ERR_BAD_FD);
    };
    let frames = unsafe { *core::ptr::addr_of!(LINUX_PROPERTY_AREA_FRAMES) };
    let first_page = usize::try_from(offset / 4096).map_err(|_| ERR_OVERFLOW)?;
    let page_count = usize::try_from(rounded / 4096).map_err(|_| ERR_OVERFLOW)?;
    let mut mapped = 0usize;
    for index in 0..page_count {
        if !mmu::map_user_page(
            space_id,
            address + index as u64 * 4096,
            frames[first_page + index],
            protection & 1 != 0,
            protection & 2 != 0,
            protection & 4 != 0,
        ) {
            for mapped_index in 0..mapped {
                let _ = mmu::unmap_user_page(space_id, address + mapped_index as u64 * 4096);
            }
            let _ = task::release_shared_mapping(address, rounded);
            return Err(ERR_OUT_OF_MEMORY);
        }
        mapped += 1;
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_PROPERTY_MAPPINGS))[mapping_slot] = LinuxPropertyMapping {
            owner_pid,
            address,
            length: rounded,
            active: true,
        };
    }
    Ok(address)
}

pub(crate) fn linux_property_unmap(address: u64, length: u64) -> Option<u64> {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return None;
    };
    let mapping = unsafe {
        (*core::ptr::addr_of!(LINUX_PROPERTY_MAPPINGS))
            .iter()
            .enumerate()
            .find(|(_, mapping)| {
                mapping.active && mapping.owner_pid == owner_pid && mapping.address == address
            })
            .map(|(index, mapping)| (index, *mapping))
    }?;
    let rounded = length.checked_add(4095).map(|value| value & !4095);
    if rounded != Some(mapping.1.length) {
        return Some(ERR_INVALID);
    }
    if let Some(space_id) = task::address_space_for_pid(owner_pid) {
        let page_count = usize::try_from(mapping.1.length / 4096).unwrap_or(0);
        for index in 0..page_count {
            let _ = mmu::unmap_user_page(space_id, address + index as u64 * 4096);
        }
        let _ = task::release_shared_mapping_for_pid(owner_pid, address, mapping.1.length);
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_PROPERTY_MAPPINGS))[mapping.0] =
            LinuxPropertyMapping::EMPTY;
    }
    Some(0)
}

/// Apply the read-only protection contract to a property-area mapping.
///
/// Ordinary Linux mappings are owned by `task::protect_memory`, which
/// intentionally rejects shared mappings.  Android property pages are the
/// narrow shared exception, so update their live page-table descriptors here
/// while keeping writable protection impossible.
pub(crate) fn linux_property_mprotect(address: u64, length: u64, protection: u64) -> Option<u64> {
    let owner_pid = task::resource_owner_pid()?;
    let mapping = unsafe {
        (*core::ptr::addr_of!(LINUX_PROPERTY_MAPPINGS))
            .iter()
            .copied()
            .find(|mapping| {
                mapping.active && mapping.owner_pid == owner_pid && mapping.address == address
            })
    }?;
    if protection & !0x7 != 0 || protection & 2 != 0 {
        return Some(ERR_INVALID);
    }
    let rounded = match length.checked_add(4095) {
        Some(value) if value != 0 => value & !4095,
        _ => return Some(ERR_INVALID),
    };
    if rounded != mapping.length {
        return Some(ERR_INVALID);
    }
    let Some(space_id) = task::current_address_space() else {
        return Some(ERR_BAD_FD);
    };
    let page_count = usize::try_from(rounded / 4096).unwrap_or(usize::MAX);
    for index in 0..page_count {
        if !mmu::protect_user_page(
            space_id,
            address + index as u64 * 4096,
            protection & 1 != 0,
            false,
            protection & 4 != 0,
        ) {
            return Some(ERR_INVALID);
        }
    }
    Some(0)
}

fn cleanup_linux_property_mappings(owner_pid: u64) {
    let mut mappings = [LinuxPropertyMapping::EMPTY; MAX_LINUX_PROPERTY_MAPPINGS];
    let mut count = 0usize;
    unsafe {
        for mapping in (*core::ptr::addr_of!(LINUX_PROPERTY_MAPPINGS))
            .iter()
            .copied()
        {
            if mapping.active && mapping.owner_pid == owner_pid && count < mappings.len() {
                mappings[count] = mapping;
                count += 1;
            }
        }
    }
    for mapping in mappings.iter().copied().take(count) {
        if let Some(space_id) = task::address_space_for_pid(owner_pid) {
            let page_count = usize::try_from(mapping.length / 4096).unwrap_or(0);
            for index in 0..page_count {
                let _ = mmu::unmap_user_page(space_id, mapping.address + index as u64 * 4096);
            }
            let _ =
                task::release_shared_mapping_for_pid(owner_pid, mapping.address, mapping.length);
        }
        unsafe {
            (*core::ptr::addr_of_mut!(LINUX_PROPERTY_MAPPINGS))
                .iter_mut()
                .filter(|entry| {
                    entry.active
                        && entry.owner_pid == mapping.owner_pid
                        && entry.address == mapping.address
                        && entry.length == mapping.length
                })
                .for_each(|entry| *entry = LinuxPropertyMapping::EMPTY);
        }
    }
}

fn inherit_linux_property_mappings(parent_pid: u64, child_pid: u64) -> bool {
    let mut inherited = [LinuxPropertyMapping::EMPTY; MAX_LINUX_PROPERTY_MAPPINGS];
    let mut count = 0usize;
    unsafe {
        for mapping in (*core::ptr::addr_of!(LINUX_PROPERTY_MAPPINGS))
            .iter()
            .copied()
        {
            if mapping.active && mapping.owner_pid == parent_pid {
                if count == inherited.len() {
                    return false;
                }
                inherited[count] = LinuxPropertyMapping {
                    owner_pid: child_pid,
                    ..mapping
                };
                count += 1;
            }
        }
        let free = (*core::ptr::addr_of!(LINUX_PROPERTY_MAPPINGS))
            .iter()
            .filter(|mapping| !mapping.active)
            .count();
        if free < count {
            return false;
        }
        let mappings = core::ptr::addr_of_mut!(LINUX_PROPERTY_MAPPINGS);
        let mut copied = 0usize;
        for mapping in (*mappings).iter_mut() {
            if !mapping.active {
                *mapping = inherited[copied];
                copied += 1;
                if copied == count {
                    break;
                }
            }
        }
    }
    true
}

pub(crate) fn cleanup_shared_mappings(owner_pid: u64) {
    cleanup_linux_property_mappings(owner_pid);
    let mut mappings = [SharedMapping::EMPTY; MAX_SHARED_MAPPINGS];
    let mut count = 0usize;
    unsafe {
        for mapping in (*core::ptr::addr_of!(SHARED_MAPPINGS)).iter().copied() {
            if mapping.active && mapping.owner_pid == owner_pid && count < mappings.len() {
                mappings[count] = mapping;
                count += 1;
            }
        }
    }
    for mapping in mappings.iter().copied().take(count) {
        if let Some(space_id) = task::address_space_for_pid(owner_pid) {
            let page_count = usize::try_from(mapping.length / 4096).unwrap_or(0);
            for index in 0..page_count {
                let _ = mmu::unmap_user_page(space_id, mapping.address + index as u64 * 4096);
            }
            let _ =
                task::release_shared_mapping_for_pid(owner_pid, mapping.address, mapping.length);
        }
        unsafe {
            (*core::ptr::addr_of_mut!(SHARED_MAPPINGS))
                .iter_mut()
                .filter(|entry| {
                    entry.active
                        && entry.buffer_slot == mapping.buffer_slot
                        && entry.owner_pid == mapping.owner_pid
                        && entry.address == mapping.address
                        && entry.length == mapping.length
                })
                .for_each(|entry| *entry = SharedMapping::EMPTY);
        }
    }
}

/// Duplicate the process-local mapping records that accompany forked
/// address-space pages. The physical buffer remains owned by its capability;
/// this table only lets each child unmap its own virtual view later.
pub(crate) fn inherit_shared_mappings(parent_pid: u64, child_pid: u64) -> bool {
    let mut inherited = [SharedMapping::EMPTY; MAX_SHARED_MAPPINGS];
    let mut count = 0usize;
    unsafe {
        for mapping in (*core::ptr::addr_of!(SHARED_MAPPINGS)).iter().copied() {
            if mapping.active && mapping.owner_pid == parent_pid {
                if count == inherited.len() {
                    return false;
                }
                inherited[count] = SharedMapping {
                    owner_pid: child_pid,
                    ..mapping
                };
                count += 1;
            }
        }
        let free = (*core::ptr::addr_of!(SHARED_MAPPINGS))
            .iter()
            .filter(|mapping| !mapping.active)
            .count();
        if free < count {
            return false;
        }
        let mappings = core::ptr::addr_of_mut!(SHARED_MAPPINGS);
        let mut copied = 0usize;
        for mapping in (*mappings).iter_mut() {
            if !mapping.active {
                *mapping = inherited[copied];
                copied += 1;
                if copied == count {
                    break;
                }
            }
        }
    }
    inherit_linux_property_mappings(parent_pid, child_pid)
}

pub(crate) fn shared_buffer_create(length: u64, flags: u64) -> u64 {
    let requested = usize::try_from(length).unwrap_or(usize::MAX);
    let allowed = 0b111u64;
    if requested == 0
        || flags & !allowed != 0
        || flags & 0b11 == 0
        || requested > MAX_SHARED_BUFFER_PAGES * 4096
    {
        return ERR_INVALID;
    }
    let rounded = match requested.checked_add(4095) {
        Some(value) => value & !4095,
        None => return ERR_OVERFLOW,
    };
    let page_count = rounded / 4096;
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some(buffer_slot) = (0..MAX_SHARED_BUFFER_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(SHARED_BUFFER_SLOTS))[index].active })
    else {
        return ERR_OUT_OF_MEMORY;
    };

    let active_space = task::current_address_space();
    mmu::activate_kernel_identity_space();
    let mut frames = [0u64; MAX_SHARED_BUFFER_PAGES];
    let mut allocated = 0usize;
    let allocation_ok = allocator::with_global(|frame_allocator| {
        while allocated < page_count {
            let Some(frame) = frame_allocator.next_frame() else {
                return false;
            };
            unsafe { core::ptr::write_bytes(frame as *mut u8, 0, 4096) };
            frames[allocated] = frame;
            allocated += 1;
        }
        true
    })
    .unwrap_or(false);
    if !allocation_ok {
        let _ = allocator::with_global(|frame_allocator| {
            frame_allocator.release_frames(&frames[..allocated])
        });
        if let Some(space_id) = active_space {
            let _ = mmu::activate_user_space(space_id);
        }
        return ERR_OUT_OF_MEMORY;
    }
    if let Some(space_id) = active_space {
        let _ = mmu::activate_user_space(space_id);
    }
    unsafe {
        (*core::ptr::addr_of_mut!(SHARED_BUFFER_SLOTS))[buffer_slot] = SharedBufferSlot {
            frames,
            page_count,
            length: rounded as u64,
            flags,
            references: 0,
            active: true,
        };
    }
    match install_handle(owner_pid, 0, 0, KIND_SHARED_BUFFER, buffer_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            release_shared_buffer_slot(buffer_slot as u8);
            error
        }
    }
}

pub(crate) fn shared_buffer_map(handle: u64, addr_hint: u64, requested_flags: u64) -> u64 {
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind != KIND_SHARED_BUFFER {
        return ERR_PERMISSION;
    }
    let rights = unsafe {
        let Some(buffer) = (*core::ptr::addr_of!(SHARED_BUFFER_SLOTS)).get(file.pipe_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !buffer.active {
            return ERR_BAD_FD;
        }
        let rights = if requested_flags == 0 {
            buffer.flags & 0b11
        } else {
            requested_flags
        };
        if rights & !0b11 != 0 || rights & 0b11 == 0 || rights & !buffer.flags != 0 {
            return ERR_PERMISSION;
        }
        rights
    };
    let (length, frames, page_count) = unsafe {
        let buffer = &(*core::ptr::addr_of!(SHARED_BUFFER_SLOTS))[file.pipe_slot as usize];
        (buffer.length, buffer.frames, buffer.page_count)
    };
    let owner_pid = task::resource_owner_pid().unwrap_or(0);
    if owner_pid == 0 {
        return ERR_BAD_FD;
    }
    let mapping_free = unsafe {
        (*core::ptr::addr_of!(SHARED_MAPPINGS))
            .iter()
            .any(|mapping| !mapping.active)
    };
    if !mapping_free {
        return ERR_OUT_OF_MEMORY;
    }
    let protection = (rights & 0x3) as u64;
    let address = match task::reserve_shared_mapping(addr_hint, length, protection) {
        Ok(address) => address,
        Err(error) => return error,
    };
    let Some(space_id) = task::current_address_space() else {
        let _ = task::release_shared_mapping(address, length);
        return ERR_BAD_FD;
    };
    let mut mapped = 0usize;
    for frame in frames.iter().copied().take(page_count) {
        let virtual_address = address + mapped as u64 * 4096;
        if !mmu::map_user_page(
            space_id,
            virtual_address,
            frame,
            rights & 1 != 0,
            rights & 2 != 0,
            false,
        ) {
            for index in 0..mapped {
                let _ = mmu::unmap_user_page(space_id, address + index as u64 * 4096);
            }
            let _ = task::release_shared_mapping(address, length);
            return ERR_OUT_OF_MEMORY;
        }
        mapped += 1;
    }
    unsafe {
        if let Some(entry) = (*core::ptr::addr_of_mut!(SHARED_MAPPINGS))
            .iter_mut()
            .find(|entry| !entry.active)
        {
            *entry = SharedMapping {
                buffer_slot: file.pipe_slot,
                owner_pid,
                address,
                length,
                active: true,
            };
        } else {
            for index in 0..mapped {
                let _ = mmu::unmap_user_page(space_id, address + index as u64 * 4096);
            }
            let _ = task::release_shared_mapping(address, length);
            return ERR_OUT_OF_MEMORY;
        }
    }
    address
}

pub(crate) fn shared_buffer_unmap(handle: u64, address: u64) -> u64 {
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind != KIND_SHARED_BUFFER {
        return ERR_PERMISSION;
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some((mapping_index, length)) =
        shared_buffer_mapping_slot(file.pipe_slot, owner_pid, address)
    else {
        return ERR_INVALID;
    };
    let Some(space_id) = task::current_address_space() else {
        return ERR_BAD_FD;
    };
    let page_count = usize::try_from(length / 4096).unwrap_or(0);
    for index in 0..page_count {
        if mmu::unmap_user_page(space_id, address + index as u64 * 4096).is_none() {
            return ERR_INVALID;
        }
    }
    if !task::release_shared_mapping(address, length) {
        return ERR_INVALID;
    }
    unsafe {
        (*core::ptr::addr_of_mut!(SHARED_MAPPINGS))[mapping_index] = SharedMapping::EMPTY;
    }
    0
}

pub(crate) fn event_create(flags: u64) -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some(event_slot) = (0..MAX_EVENT_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(EVENT_SLOTS))[index].active })
    else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(EVENT_SLOTS))[event_slot] = EventSlot {
            manual_reset: flags & 1 != 0,
            active: true,
            ..EventSlot::EMPTY
        };
    }
    match install_handle(owner_pid, 0, 0, KIND_EVENT, event_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(EVENT_SLOTS))[event_slot] = EventSlot::EMPTY;
            }
            error
        }
    }
}

/// Create a one-shot timer which signals an event capability at an absolute
/// monotonic deadline. The timer stores the event's owner and generation-checked
/// handle, so closing or transferring that event before expiry cannot signal a
/// newly allocated capability that happens to reuse the same slot.
pub(crate) fn timer_create(clock_id: u64, deadline_ns: u64, event_handle: u64) -> u64 {
    if !matches!(clock_id, 0 | 1) {
        return ERR_INVALID;
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some((_, event)) = locate_entry(event_handle) else {
        return ERR_BAD_FD;
    };
    if event.kind != KIND_EVENT {
        return ERR_PERMISSION;
    }
    let Some(timer_slot) = (0..MAX_TIMER_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(TIMER_SLOTS))[index].active })
    else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(TIMER_SLOTS))[timer_slot] = TimerSlot {
            event_owner_pid: event.owner_pid,
            event_handle,
            deadline_ns,
            ..TimerSlot::EMPTY
        };
        (*core::ptr::addr_of_mut!(TIMER_SLOTS))[timer_slot].active = true;
    }
    match install_handle(owner_pid, 0, 0, KIND_TIMER, timer_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(TIMER_SLOTS))[timer_slot] = TimerSlot::EMPTY;
            }
            error
        }
    }
}

pub(crate) fn thread_handle_create(owner_pid: u64, target_pid: u64) -> Result<u64, u64> {
    if !task::contains_live_pid(target_pid) {
        return Err((-(3i64)) as u64);
    }
    let Some(thread_slot) = (0..MAX_THREAD_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(THREAD_SLOTS))[index].active })
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    unsafe {
        (*core::ptr::addr_of_mut!(THREAD_SLOTS))[thread_slot] = ThreadSlot {
            target_pid,
            active: true,
            ..ThreadSlot::EMPTY
        };
    }
    match install_handle(owner_pid, 0, 0, KIND_THREAD, thread_slot as u8) {
        Ok(handle) => Ok(handle),
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(THREAD_SLOTS))[thread_slot] = ThreadSlot::EMPTY;
            }
            Err(error)
        }
    }
}

pub(crate) fn thread_exit(target_pid: u64, status: u64) {
    unsafe {
        for thread in (*core::ptr::addr_of_mut!(THREAD_SLOTS)).iter_mut() {
            if !thread.active || thread.target_pid != target_pid {
                continue;
            }
            thread.exited = true;
            thread.exit_status = status;
            if thread.references == 0 {
                *thread = ThreadSlot::EMPTY;
            }
        }
    }
}

pub(crate) fn thread_join(handle: u64, frame: &mut Aarch64TrapFrame) -> bool {
    let Some((_, file)) = locate_entry(handle) else {
        frame.x[0] = ERR_BAD_FD;
        return true;
    };
    if file.kind != KIND_THREAD {
        frame.x[0] = ERR_PERMISSION;
        return true;
    }
    let Some(thread) =
        (unsafe { (*core::ptr::addr_of!(THREAD_SLOTS)).get(file.pipe_slot as usize) })
    else {
        frame.x[0] = ERR_BAD_FD;
        return true;
    };
    if !thread.active {
        frame.x[0] = ERR_BAD_FD;
        return true;
    }
    if thread.detached {
        frame.x[0] = (-(22i64)) as u64;
        return true;
    }
    if thread.exited {
        frame.x[0] = thread.exit_status;
        let _ = task::reap_thread_if_exited(thread.target_pid);
        return true;
    }
    task::join_thread(frame, thread.target_pid)
}

pub(crate) fn thread_detach(handle: u64) -> u64 {
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind != KIND_THREAD {
        return ERR_PERMISSION;
    }
    let target_pid = unsafe {
        let Some(thread) =
            (*core::ptr::addr_of_mut!(THREAD_SLOTS)).get_mut(file.pipe_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !thread.active {
            return ERR_BAD_FD;
        }
        if thread.detached {
            return 0;
        }
        thread.detached = true;
        thread.target_pid
    };
    let _ = task::mark_thread_detached(target_pid);
    let _ = task::reap_thread_if_exited(target_pid);
    0
}

/// Fire expired one-shot timers from the architectural timer boundary or a
/// bounded time-advancing syscall. This is deliberately static and one-shot;
/// repeating timers can be added without changing the event ABI.
pub(crate) fn fire_timers(now_ns: u64) {
    let mut targets = [(0u64, 0u64); MAX_TIMER_SLOTS];
    let mut target_count = 0usize;
    unsafe {
        for timer in (*core::ptr::addr_of_mut!(TIMER_SLOTS)).iter_mut() {
            if timer.active && !timer.fired && now_ns >= timer.deadline_ns {
                timer.fired = true;
                targets[target_count] = (timer.event_owner_pid, timer.event_handle);
                target_count += 1;
            }
        }
    }
    for (owner_pid, event_handle) in targets.iter().copied().take(target_count) {
        let _ = signal_event_for_owner(owner_pid, event_handle);
    }
}

pub(crate) fn event_signal(handle: u64) -> u64 {
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    signal_event_file(file)
}

fn signal_event_for_owner(owner_pid: u64, handle: u64) -> u64 {
    let Some((_, file)) = locate_entry_for_owner(owner_pid, handle) else {
        return ERR_BAD_FD;
    };
    signal_event_file(file)
}

fn signal_event_file(file: OpenFile) -> u64 {
    if file.kind != KIND_EVENT {
        return ERR_PERMISSION;
    }
    let manual_reset = unsafe {
        let Some(event) = (*core::ptr::addr_of_mut!(EVENT_SLOTS)).get_mut(file.pipe_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !event.active {
            return ERR_BAD_FD;
        }
        event.signaled = true;
        event.manual_reset
    };
    let woken = task::wake_event(file.pipe_slot as u64, manual_reset);
    if !manual_reset && woken != 0 {
        unsafe {
            (*core::ptr::addr_of_mut!(EVENT_SLOTS))[file.pipe_slot as usize].signaled = false;
        }
    }
    0
}

pub(crate) fn event_wait(handle: u64, timeout_us: u64, frame: &mut Aarch64TrapFrame) -> bool {
    let Some((_, file)) = locate_entry(handle) else {
        frame.x[0] = ERR_BAD_FD;
        return true;
    };
    if file.kind != KIND_EVENT {
        frame.x[0] = ERR_PERMISSION;
        return true;
    }
    let signaled = unsafe {
        let Some(event) = (*core::ptr::addr_of_mut!(EVENT_SLOTS)).get_mut(file.pipe_slot as usize)
        else {
            frame.x[0] = ERR_BAD_FD;
            return true;
        };
        if !event.active {
            frame.x[0] = ERR_BAD_FD;
            return true;
        }
        let signaled = event.signaled;
        if signaled && !event.manual_reset {
            event.signaled = false;
        }
        signaled
    };
    if signaled {
        frame.x[0] = 0;
        return true;
    }
    if timeout_us == 0 {
        frame.x[0] = ERR_WOULD_BLOCK;
        return true;
    }
    task::block_event(frame, file.pipe_slot as u64, timeout_us)
}

pub(crate) fn event_subscribe(_event_type: u64, handle: u64) -> u64 {
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind == KIND_EVENT {
        0
    } else {
        ERR_PERMISSION
    }
}

pub(crate) fn duplicate(handle: u64) -> u64 {
    let Some((_, source)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    match install_handle(
        owner_pid,
        source.local_fd,
        source.mount_index,
        source.kind,
        source.pipe_slot,
    ) {
        Ok(handle) => handle,
        Err(error) => error,
    }
}

fn linux_fd_entry(owner_pid: u64, fd: u32) -> Option<(usize, LinuxFdEntry)> {
    unsafe {
        (*core::ptr::addr_of!(LINUX_FDS))
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.active && entry.owner_pid == owner_pid && entry.fd == fd)
            .map(|(index, entry)| (index, *entry))
    }
}

fn linux_fd_in_use(owner_pid: u64, fd: u32) -> bool {
    linux_fd_entry(owner_pid, fd).is_some()
}

fn linux_fd_install_at(
    owner_pid: u64,
    fd: u32,
    native_handle: u64,
    stdio_fd: u32,
    flags: u64,
) -> Result<u64, u64> {
    if owner_pid == 0 || !(LINUX_FD_MIN..=LINUX_FD_MAX).contains(&fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    if linux_fd_in_use(owner_pid, fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    let Some(index) = (unsafe { *core::ptr::addr_of!(LINUX_FDS) })
        .iter()
        .position(|entry| !entry.active)
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_FDS))[index] = LinuxFdEntry {
            owner_pid,
            fd,
            native_handle,
            stdio_fd: u8::try_from(stdio_fd).unwrap_or(LINUX_STDIO_NONE),
            kind: LINUX_FD_NATIVE,
            resource_slot: 0,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        };
    }
    Ok(fd as u64)
}

fn linux_fd_install_socket_at(
    owner_pid: u64,
    fd: u32,
    socket_slot: u8,
    flags: u64,
) -> Result<u64, u64> {
    if owner_pid == 0 || !(LINUX_FD_MIN..=LINUX_FD_MAX).contains(&fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    if linux_fd_in_use(owner_pid, fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    let Some(index) = (unsafe { *core::ptr::addr_of!(LINUX_FDS) })
        .iter()
        .position(|entry| !entry.active)
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    if !linux_socket_retain(socket_slot) {
        return Err(ERR_BAD_FD);
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_FDS))[index] = LinuxFdEntry {
            owner_pid,
            fd,
            native_handle: 0,
            stdio_fd: LINUX_STDIO_NONE,
            kind: LINUX_FD_SOCKET,
            resource_slot: socket_slot,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        };
    }
    Ok(fd as u64)
}

fn linux_fd_install_epoll_at(
    owner_pid: u64,
    fd: u32,
    epoll_slot: u8,
    flags: u64,
) -> Result<u64, u64> {
    if owner_pid == 0 || !(LINUX_FD_MIN..=LINUX_FD_MAX).contains(&fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    if linux_fd_in_use(owner_pid, fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    let Some(index) = (unsafe { *core::ptr::addr_of!(LINUX_FDS) })
        .iter()
        .position(|entry| !entry.active)
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    if !linux_epoll_retain(epoll_slot) {
        return Err(ERR_BAD_FD);
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_FDS))[index] = LinuxFdEntry {
            owner_pid,
            fd,
            native_handle: 0,
            stdio_fd: LINUX_STDIO_NONE,
            kind: LINUX_FD_EPOLL,
            resource_slot: epoll_slot,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        };
    }
    Ok(fd as u64)
}

fn linux_fd_install_eventfd_at(
    owner_pid: u64,
    fd: u32,
    eventfd_slot: u8,
    flags: u64,
) -> Result<u64, u64> {
    if owner_pid == 0 || !(LINUX_FD_MIN..=LINUX_FD_MAX).contains(&fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    if linux_fd_in_use(owner_pid, fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    let Some(index) = (unsafe { *core::ptr::addr_of!(LINUX_FDS) })
        .iter()
        .position(|entry| !entry.active)
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    if !linux_eventfd_retain(eventfd_slot) {
        return Err(ERR_BAD_FD);
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_FDS))[index] = LinuxFdEntry {
            owner_pid,
            fd,
            native_handle: 0,
            stdio_fd: LINUX_STDIO_NONE,
            kind: LINUX_FD_EVENTFD,
            resource_slot: eventfd_slot,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        };
    }
    Ok(fd as u64)
}

fn linux_fd_install_inotify_at(
    owner_pid: u64,
    fd: u32,
    inotify_slot: u8,
    flags: u64,
) -> Result<u64, u64> {
    if owner_pid == 0 || !(LINUX_FD_MIN..=LINUX_FD_MAX).contains(&fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    if linux_fd_in_use(owner_pid, fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    let Some(index) = (unsafe { *core::ptr::addr_of!(LINUX_FDS) })
        .iter()
        .position(|entry| !entry.active)
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    if !linux_inotify_retain(inotify_slot) {
        return Err(ERR_BAD_FD);
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_FDS))[index] = LinuxFdEntry {
            owner_pid,
            fd,
            native_handle: 0,
            stdio_fd: LINUX_STDIO_NONE,
            kind: LINUX_FD_INOTIFY,
            resource_slot: inotify_slot,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        };
    }
    Ok(fd as u64)
}

fn linux_fd_install_signalfd_at(
    owner_pid: u64,
    fd: u32,
    signalfd_slot: u8,
    flags: u64,
) -> Result<u64, u64> {
    if owner_pid == 0 || !(LINUX_FD_MIN..=LINUX_FD_MAX).contains(&fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    if linux_fd_in_use(owner_pid, fd) {
        return Err(ERR_TOO_MANY_FILES);
    }
    let Some(index) = (unsafe { *core::ptr::addr_of!(LINUX_FDS) })
        .iter()
        .position(|entry| !entry.active)
    else {
        return Err(ERR_OUT_OF_MEMORY);
    };
    if !linux_signalfd_retain(signalfd_slot) {
        return Err(ERR_BAD_FD);
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_FDS))[index] = LinuxFdEntry {
            owner_pid,
            fd,
            native_handle: 0,
            stdio_fd: LINUX_STDIO_NONE,
            kind: LINUX_FD_SIGNALFD,
            resource_slot: signalfd_slot,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        };
    }
    Ok(fd as u64)
}

fn linux_next_fd(owner_pid: u64) -> Option<u32> {
    (LINUX_FD_MIN..=LINUX_FD_MAX).find(|&fd| !linux_fd_in_use(owner_pid, fd))
}

fn linux_socket_retain(socket_slot: u8) -> bool {
    unsafe {
        let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        else {
            return false;
        };
        if !socket.active {
            return false;
        }
        socket.references = socket.references.saturating_add(1);
        true
    }
}

fn linux_socket_release(socket_slot: u8) {
    let (peer, pending) = unsafe {
        let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        else {
            return;
        };
        if !socket.active {
            return;
        }
        socket.references = socket.references.saturating_sub(1);
        if socket.references != 0 {
            return;
        }
        let peer = socket.peer;
        let pending = socket.pending;
        *socket = LinuxSocketSlot::EMPTY;
        (peer, pending)
    };
    if pending != SOCKET_SLOT_NONE && pending != socket_slot {
        unsafe {
            if let Some(pending_socket) =
                (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(pending as usize)
            {
                let pending_peer = pending_socket.peer;
                *pending_socket = LinuxSocketSlot::EMPTY;
                if pending_peer != SOCKET_SLOT_NONE {
                    if let Some(client) = (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))
                        .get_mut(pending_peer as usize)
                    {
                        if client.active && client.peer == pending {
                            client.peer = SOCKET_SLOT_NONE;
                            client.connected = false;
                        }
                    }
                }
            }
        }
    }
    if peer != SOCKET_SLOT_NONE {
        unsafe {
            let mut drop_peer = false;
            if let Some(peer_socket) =
                (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(peer as usize)
            {
                if peer_socket.active && peer_socket.peer == socket_slot {
                    if peer_socket.references == 0 {
                        *peer_socket = LinuxSocketSlot::EMPTY;
                        drop_peer = true;
                    } else {
                        peer_socket.peer = SOCKET_SLOT_NONE;
                        peer_socket.connected = false;
                    }
                }
            }
            if drop_peer {
                for listener in (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).iter_mut() {
                    if listener.active && listener.pending == peer {
                        listener.pending = SOCKET_SLOT_NONE;
                    }
                }
            }
        }
    }
}

fn linux_epoll_retain(epoll_slot: u8) -> bool {
    unsafe {
        let Some(epoll) = (*core::ptr::addr_of_mut!(EPOLL_SLOTS)).get_mut(epoll_slot as usize)
        else {
            return false;
        };
        if !epoll.active {
            return false;
        }
        epoll.references = epoll.references.saturating_add(1);
        true
    }
}

fn linux_epoll_release(epoll_slot: u8) {
    unsafe {
        let Some(epoll) = (*core::ptr::addr_of_mut!(EPOLL_SLOTS)).get_mut(epoll_slot as usize)
        else {
            return;
        };
        epoll.references = epoll.references.saturating_sub(1);
        if epoll.references == 0 {
            *epoll = EpollSlot::EMPTY;
        }
    }
}

fn linux_eventfd_retain(eventfd_slot: u8) -> bool {
    unsafe {
        let Some(eventfd) =
            (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS)).get_mut(eventfd_slot as usize)
        else {
            return false;
        };
        if !eventfd.active {
            return false;
        }
        eventfd.references = eventfd.references.saturating_add(1);
        true
    }
}

fn linux_eventfd_release(eventfd_slot: u8) {
    unsafe {
        let Some(eventfd) =
            (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS)).get_mut(eventfd_slot as usize)
        else {
            return;
        };
        eventfd.references = eventfd.references.saturating_sub(1);
        if eventfd.references == 0 {
            *eventfd = LinuxEventFdSlot::EMPTY;
        }
    }
}

pub(crate) fn linux_eventfd_slot(fd: u64) -> Option<u8> {
    let owner_pid = task::resource_owner_pid()?;
    let fd = u32::try_from(fd).ok()?;
    let (_, entry) = linux_fd_entry(owner_pid, fd)?;
    (entry.kind == LINUX_FD_EVENTFD).then_some(entry.resource_slot)
}

fn linux_inotify_retain(inotify_slot: u8) -> bool {
    unsafe {
        let Some(inotify) =
            (*core::ptr::addr_of_mut!(LINUX_INOTIFY_SLOTS)).get_mut(inotify_slot as usize)
        else {
            return false;
        };
        if !inotify.active {
            return false;
        }
        inotify.references = inotify.references.saturating_add(1);
        true
    }
}

fn linux_inotify_release(inotify_slot: u8) {
    unsafe {
        let Some(inotify) =
            (*core::ptr::addr_of_mut!(LINUX_INOTIFY_SLOTS)).get_mut(inotify_slot as usize)
        else {
            return;
        };
        inotify.references = inotify.references.saturating_sub(1);
        if inotify.references == 0 {
            *inotify = LinuxInotifySlot::EMPTY;
        }
    }
}

pub(crate) fn linux_inotify_slot(fd: u64) -> Option<u8> {
    let owner_pid = task::resource_owner_pid()?;
    let fd = u32::try_from(fd).ok()?;
    let (_, entry) = linux_fd_entry(owner_pid, fd)?;
    (entry.kind == LINUX_FD_INOTIFY).then_some(entry.resource_slot)
}

fn linux_signalfd_retain(signalfd_slot: u8) -> bool {
    unsafe {
        let Some(signalfd) =
            (*core::ptr::addr_of_mut!(LINUX_SIGNALFD_SLOTS)).get_mut(signalfd_slot as usize)
        else {
            return false;
        };
        if !signalfd.active {
            return false;
        }
        signalfd.references = signalfd.references.saturating_add(1);
        true
    }
}

fn linux_signalfd_release(signalfd_slot: u8) {
    unsafe {
        let Some(signalfd) =
            (*core::ptr::addr_of_mut!(LINUX_SIGNALFD_SLOTS)).get_mut(signalfd_slot as usize)
        else {
            return;
        };
        signalfd.references = signalfd.references.saturating_sub(1);
        if signalfd.references == 0 {
            *signalfd = LinuxSignalFdSlot::EMPTY;
        }
    }
}

pub(crate) fn linux_signalfd_slot(fd: u64) -> Option<u8> {
    let owner_pid = task::resource_owner_pid()?;
    let fd = u32::try_from(fd).ok()?;
    let (_, entry) = linux_fd_entry(owner_pid, fd)?;
    (entry.kind == LINUX_FD_SIGNALFD).then_some(entry.resource_slot)
}

fn linux_signalfd_mask(fd: u64) -> Option<u64> {
    let slot = linux_signalfd_slot(fd)?;
    unsafe {
        (*core::ptr::addr_of!(LINUX_SIGNALFD_SLOTS))
            .get(slot as usize)
            .filter(|signalfd| signalfd.active)
            .map(|signalfd| signalfd.mask)
    }
}

pub(crate) fn linux_set_nonblocking(fd: u64, enabled: bool) -> u64 {
    const O_NONBLOCK: u32 = 0x800;
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Ok(fd) = u32::try_from(fd) else {
        return ERR_BAD_FD;
    };
    let Some((index, entry)) = linux_fd_entry(owner_pid, fd) else {
        return ERR_BAD_FD;
    };
    if entry.kind == LINUX_FD_SOCKET {
        unsafe {
            let Some(socket) = (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))
                .get_mut(entry.resource_slot as usize)
            else {
                return ERR_BAD_FD;
            };
            if !socket.active {
                return ERR_BAD_FD;
            }
            socket.nonblocking = enabled;
        }
        return 0;
    }
    if entry.kind == LINUX_FD_NATIVE
        || entry.kind == LINUX_FD_EVENTFD
        || entry.kind == LINUX_FD_INOTIFY
        || entry.kind == LINUX_FD_SIGNALFD
    {
        unsafe {
            let entry = &mut (*core::ptr::addr_of_mut!(LINUX_FDS))[index];
            if enabled {
                entry.flags |= O_NONBLOCK;
            } else {
                entry.flags &= !O_NONBLOCK;
            }
        }
        return 0;
    }
    ERR_NOT_SUPPORTED
}

fn linux_epoll_slot(fd: u64) -> Option<u8> {
    let owner_pid = task::resource_owner_pid()?;
    let fd = u32::try_from(fd).ok()?;
    let (_, entry) = linux_fd_entry(owner_pid, fd)?;
    (entry.kind == LINUX_FD_EPOLL).then_some(entry.resource_slot)
}

fn linux_socket_allocate(
    _domain: u64,
    socket_type: u64,
    _protocol: u64,
    nonblocking: bool,
) -> Option<u8> {
    let slot = unsafe {
        (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS))
            .iter()
            .position(|socket| !socket.active)?
    };
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))[slot] = LinuxSocketSlot {
            socket_type: u16::try_from(socket_type).ok()?,
            nonblocking: nonblocking,
            active: true,
            ..LinuxSocketSlot::EMPTY
        };
    }
    Some(slot as u8)
}

fn linux_socket_entry(fd: u64) -> Option<(usize, LinuxFdEntry)> {
    let owner_pid = task::resource_owner_pid()?;
    let fd = u32::try_from(fd).ok()?;
    let (index, entry) = linux_fd_entry(owner_pid, fd)?;
    (entry.kind == LINUX_FD_SOCKET).then_some((index, entry))
}

pub(crate) fn linux_socket_slot(fd: u64) -> Option<u8> {
    linux_socket_entry(fd).map(|(_, entry)| entry.resource_slot)
}

fn linux_socket_slot_for_fd(fd: u64) -> Result<u8, u64> {
    linux_socket_slot(fd).ok_or(ERR_BAD_FD)
}

fn linux_socket_path(
    address: u64,
    length: u64,
) -> Result<([u8; SOCKET_PATH_CAPACITY], usize), u64> {
    let length = usize::try_from(length).map_err(|_| ERR_INVALID)?;
    if length < 3 || length > SOCKET_PATH_CAPACITY + 2 || address == 0 {
        return Err(ERR_INVALID);
    }
    let mut sockaddr = [0u8; SOCKET_PATH_CAPACITY + 2];
    if user_memory::copy_from_user(address, &mut sockaddr[..length]).is_err() {
        return Err(ERR_ADDRESS);
    }
    if u16::from_ne_bytes([sockaddr[0], sockaddr[1]]) != AF_UNIX as u16 {
        return Err(ERR_INVALID);
    }
    let path_length = sockaddr[2..length]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(length - 2);
    if path_length == 0 || path_length > SOCKET_PATH_CAPACITY {
        return Err(ERR_INVALID);
    }
    if sockaddr[2] == 0 {
        return Err(ERR_NOT_SUPPORTED);
    }
    let mut path = [0u8; SOCKET_PATH_CAPACITY];
    path[..path_length].copy_from_slice(&sockaddr[2..2 + path_length]);
    Ok((path, path_length))
}

fn linux_socket_find_bound(path: &[u8; SOCKET_PATH_CAPACITY], length: usize) -> Option<u8> {
    unsafe {
        (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS))
            .iter()
            .enumerate()
            .find(|(_, socket)| {
                socket.active
                    && socket.bound_length == length
                    && socket.bound_path[..length] == path[..length]
            })
            .map(|(index, _)| index as u8)
    }
}

fn property_message_string(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .position(|byte| *byte == 0)
        .map(|length| &bytes[..length])
        .unwrap_or(bytes)
}

fn property_message_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// Consume the AOSP property-service wire shape as a kernel-side publication
/// hook while still leaving the bytes in the server socket for `/system/bin/init`.
/// This keeps the shared property area live even before a full property-service
/// implementation exists in userspace.
fn parse_property_message(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let command = property_message_u32(data, 0)?;
    match command {
        PROP_MSG_SETPROP
            if data.len() >= 4 + ANDROID_PROPERTY_NAME_MAX + ANDROID_PROPERTY_VALUE_MAX =>
        {
            let name_start = 4;
            let value_start = name_start + ANDROID_PROPERTY_NAME_MAX;
            Some((
                property_message_string(&data[name_start..value_start]),
                property_message_string(
                    &data[value_start..value_start + ANDROID_PROPERTY_VALUE_MAX],
                ),
            ))
        }
        PROP_MSG_SETPROP2 => {
            let name_length = usize::try_from(property_message_u32(data, 4)?).ok()?;
            let name_start: usize = 8;
            let name_end = name_start.checked_add(name_length)?;
            let value_length = usize::try_from(property_message_u32(data, name_end)?).ok()?;
            let value_start = name_end.checked_add(4)?;
            let value_end = value_start.checked_add(value_length)?;
            (value_end <= data.len()).then_some((
                property_message_string(&data[name_start..name_end]),
                property_message_string(&data[value_start..value_end]),
            ))
        }
        _ => None,
    }
}

fn property_service_message(data: &[u8]) {
    let Some((name, value)) = parse_property_message(data) else {
        return;
    };
    let active_space = task::current_address_space();
    mmu::activate_kernel_identity_space();
    let published = android_property_set(name, value);
    if let Some(space_id) = active_space {
        let _ = mmu::activate_user_space(space_id);
    }
}

fn property_message_frame_length(data: &[u8]) -> Result<Option<usize>, ()> {
    let Some(command) = property_message_u32(data, 0) else {
        return Ok(None);
    };
    match command {
        PROP_MSG_SETPROP => Ok((data.len()
            >= 4 + ANDROID_PROPERTY_NAME_MAX + ANDROID_PROPERTY_VALUE_MAX)
            .then_some(4 + ANDROID_PROPERTY_NAME_MAX + ANDROID_PROPERTY_VALUE_MAX)),
        PROP_MSG_SETPROP2 => {
            if data.len() < 8 {
                return Ok(None);
            }
            let name_length =
                usize::try_from(property_message_u32(data, 4).ok_or(())?).map_err(|_| ())?;
            if name_length >= ANDROID_PROPERTY_NAME_MAX {
                return Err(());
            }
            let value_length_offset = 8usize.checked_add(name_length).ok_or(())?;
            if data.len() < value_length_offset + 4 {
                return Ok(None);
            }
            let value_length =
                usize::try_from(property_message_u32(data, value_length_offset).ok_or(())?)
                    .map_err(|_| ())?;
            if value_length >= ANDROID_PROPERTY_VALUE_MAX {
                return Err(());
            }
            let frame_length = value_length_offset
                .checked_add(4)
                .and_then(|offset| offset.checked_add(value_length))
                .ok_or(())?;
            Ok((data.len() >= frame_length).then_some(frame_length))
        }
        _ => Err(()),
    }
}

fn property_service_stage(socket_slot: u8, data: &[u8]) {
    let mut staged = [0u8; 256];
    let mut staged_length;
    unsafe {
        let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        else {
            return;
        };
        let available = staged.len().saturating_sub(socket.property_staging_length);
        if data.len() > available {
            socket.property_staging_length = 0;
            return;
        }
        let start = socket.property_staging_length;
        socket.property_staging[start..start + data.len()].copy_from_slice(data);
        socket.property_staging_length += data.len();
        staged_length = socket.property_staging_length;
        staged[..staged_length].copy_from_slice(&socket.property_staging[..staged_length]);
    }
    loop {
        let frame_length = match property_message_frame_length(&staged[..staged_length]) {
            Ok(Some(length)) => length,
            Ok(None) => break,
            Err(()) => {
                staged_length = 0;
                break;
            }
        };
        let mut frame = [0u8; 256];
        frame[..frame_length].copy_from_slice(&staged[..frame_length]);
        property_service_message(&frame[..frame_length]);
        staged.copy_within(frame_length..staged_length, 0);
        staged_length -= frame_length;
    }
    unsafe {
        if let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        {
            socket.property_staging[..staged_length].copy_from_slice(&staged[..staged_length]);
            socket.property_staging_length = staged_length;
        }
    }
}

fn socket_write_slot(socket_slot: u8, data: &[u8]) -> u64 {
    let (peer, available, property_service) = unsafe {
        let Some(socket) = (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS)).get(socket_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !socket.active {
            return ERR_BAD_FD;
        }
        if !socket.connected || socket.peer == SOCKET_SLOT_NONE {
            return ERR_NOT_CONNECTED;
        }
        let Some(peer_socket) =
            (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS)).get(socket.peer as usize)
        else {
            return ERR_NOT_CONNECTED;
        };
        if !peer_socket.active {
            return ERR_NOT_CONNECTED;
        }
        (
            socket.peer,
            SOCKET_BUFFER_CAPACITY.saturating_sub(peer_socket.length),
            socket.bound_length == 0
                && peer_socket.bound_length == PROPERTY_SERVICE_PATH.len()
                && peer_socket.bound_path[..peer_socket.bound_length] == *PROPERTY_SERVICE_PATH,
        )
    };
    if available == 0 {
        return ERR_WOULD_BLOCK;
    }
    let count = data.len().min(available);
    unsafe {
        let Some(peer_socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(peer as usize)
        else {
            return ERR_NOT_CONNECTED;
        };
        for (index, byte) in data[..count].iter().enumerate() {
            peer_socket.buffer[(peer_socket.write_position + index) % SOCKET_BUFFER_CAPACITY] =
                *byte;
        }
        peer_socket.write_position = (peer_socket.write_position + count) % SOCKET_BUFFER_CAPACITY;
        peer_socket.length += count;
    }
    if property_service {
        property_service_stage(peer, &data[..count]);
    }
    count as u64
}

fn socket_read_slot(socket_slot: u8, destination: &mut [u8]) -> u64 {
    let (count, peer_closed) = unsafe {
        let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !socket.active {
            return ERR_BAD_FD;
        }
        if socket.length == 0 {
            (0, socket.peer == SOCKET_SLOT_NONE)
        } else {
            (destination.len().min(socket.length), false)
        }
    };
    if count == 0 {
        return if peer_closed { 0 } else { ERR_WOULD_BLOCK };
    }
    unsafe {
        let socket = &mut (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))[socket_slot as usize];
        for (index, byte) in destination[..count].iter_mut().enumerate() {
            *byte = socket.buffer[(socket.read_position + index) % SOCKET_BUFFER_CAPACITY];
        }
        socket.read_position = (socket.read_position + count) % SOCKET_BUFFER_CAPACITY;
        socket.length -= count;
    }
    count as u64
}

pub(crate) fn linux_socket_read(fd: u64, buffer_address: u64, requested: u64) -> u64 {
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if count > MAX_READ {
        return ERR_INVALID;
    }
    if count == 0 {
        return 0;
    }
    let socket_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    let mut buffer = [0u8; MAX_READ];
    let result = socket_read_slot(socket_slot, &mut buffer[..count]);
    if (result as i64) < 0 {
        return result;
    }
    let result_length = usize::try_from(result).unwrap_or(0);
    if user_memory::copy_to_user(buffer_address, &buffer[..result_length]).is_err() {
        ERR_ADDRESS
    } else {
        result
    }
}

pub(crate) fn linux_socket_write(fd: u64, buffer_address: u64, requested: u64) -> u64 {
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if count > MAX_READ {
        return ERR_INVALID;
    }
    if count == 0 {
        return 0;
    }
    let socket_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    let mut buffer = [0u8; MAX_READ];
    if user_memory::copy_from_user(buffer_address, &mut buffer[..count]).is_err() {
        return ERR_ADDRESS;
    }
    socket_write_slot(socket_slot, &buffer[..count])
}

pub(crate) fn linux_socket(domain: u64, socket_type: u64, protocol: u64) -> u64 {
    let flags = socket_type & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    let socket_type = socket_type & SOCK_TYPE_MASK;
    if domain != AF_UNIX || !matches!(socket_type, SOCK_STREAM | SOCK_DGRAM) || protocol != 0 {
        return ERR_NO_PROTOCOL;
    }
    let Some(slot) =
        linux_socket_allocate(domain, socket_type, protocol, flags & SOCK_NONBLOCK != 0)
    else {
        return ERR_OUT_OF_MEMORY;
    };
    match linux_install_socket(slot, flags & (SOCK_NONBLOCK | SOCK_CLOEXEC)) {
        error if (error as i64) < 0 => error,
        fd => fd,
    }
}

pub(crate) fn linux_socketpair(
    domain: u64,
    socket_type: u64,
    protocol: u64,
    buffer_address: u64,
) -> u64 {
    let flags = socket_type & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    let socket_type = socket_type & SOCK_TYPE_MASK;
    if domain != AF_UNIX || !matches!(socket_type, SOCK_STREAM | SOCK_DGRAM) || protocol != 0 {
        return ERR_NO_PROTOCOL;
    }
    let Some(first) =
        linux_socket_allocate(domain, socket_type, protocol, flags & SOCK_NONBLOCK != 0)
    else {
        return ERR_OUT_OF_MEMORY;
    };
    let Some(second) =
        linux_socket_allocate(domain, socket_type, protocol, flags & SOCK_NONBLOCK != 0)
    else {
        linux_socket_release(first);
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))[first as usize].peer = second;
        (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))[second as usize].peer = first;
        (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))[first as usize].connected = true;
        (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS))[second as usize].connected = true;
    }
    let first_fd = linux_install_socket(first, flags & (SOCK_NONBLOCK | SOCK_CLOEXEC));
    if (first_fd as i64) < 0 {
        linux_socket_release(second);
        return first_fd;
    }
    let second_fd = linux_install_socket(second, flags & (SOCK_NONBLOCK | SOCK_CLOEXEC));
    if (second_fd as i64) < 0 {
        let _ = linux_close(first_fd);
        linux_socket_release(second);
        return second_fd;
    }
    if buffer_address == 0 {
        let _ = linux_close(first_fd);
        let _ = linux_close(second_fd);
        return ERR_ADDRESS;
    }
    let mut descriptors = [0u8; 8];
    descriptors[..4].copy_from_slice(&(first_fd as u32).to_ne_bytes());
    descriptors[4..].copy_from_slice(&(second_fd as u32).to_ne_bytes());
    if user_memory::copy_to_user(buffer_address, &descriptors).is_err() {
        let _ = linux_close(first_fd);
        let _ = linux_close(second_fd);
        return ERR_ADDRESS;
    }
    0
}

pub(crate) fn linux_socket_bind(fd: u64, address: u64, length: u64) -> u64 {
    let socket_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    let (path, path_length) = match linux_socket_path(address, length) {
        Ok(path) => path,
        Err(error) => return error,
    };
    if linux_socket_find_bound(&path, path_length).is_some() {
        return ERR_ADDRESS_IN_USE;
    }
    unsafe {
        let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !socket.active || socket.bound_length != 0 || socket.connected {
            return ERR_INVALID;
        }
        socket.bound_path[..path_length].copy_from_slice(&path[..path_length]);
        socket.bound_length = path_length;
    }
    0
}

pub(crate) fn linux_socket_listen(fd: u64, _backlog: u64) -> u64 {
    let socket_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    unsafe {
        let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !socket.active || socket.bound_length == 0 || socket.socket_type != SOCK_STREAM as u16 {
            return ERR_INVALID;
        }
        socket.listening = true;
    }
    0
}

pub(crate) fn linux_socket_connect(fd: u64, address: u64, length: u64) -> u64 {
    let client_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    let (path, path_length) = match linux_socket_path(address, length) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let server_slot = match linux_socket_find_bound(&path, path_length) {
        Some(slot) => slot,
        None => return ERR_NO_ENTRY,
    };
    let Some(connection_slot) = linux_socket_allocate(AF_UNIX, SOCK_STREAM, 0, false) else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        let slots = core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS);
        let server = &mut (*slots)[server_slot as usize];
        let client = &mut (*slots)[client_slot as usize];
        if !server.active || !server.listening || server.pending != SOCKET_SLOT_NONE {
            *(&mut (*slots)[connection_slot as usize]) = LinuxSocketSlot::EMPTY;
            return ERR_WOULD_BLOCK;
        }
        if !client.active || client.connected {
            *(&mut (*slots)[connection_slot as usize]) = LinuxSocketSlot::EMPTY;
            return ERR_INVALID;
        }
        client.peer = connection_slot;
        client.connected = true;
        let connection = &mut (*slots)[connection_slot as usize];
        connection.peer = client_slot;
        connection.connected = true;
        connection.bound_path[..path_length].copy_from_slice(&path[..path_length]);
        connection.bound_length = path_length;
        server.pending = connection_slot;
    }
    0
}

pub(crate) fn linux_socket_accept4(fd: u64, flags: u64) -> u64 {
    if flags & !(SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return ERR_INVALID;
    }
    let listener_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    let (connection_slot, passcred) = unsafe {
        let Some(listener) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(listener_slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !listener.active || !listener.listening {
            return ERR_INVALID;
        }
        let pending = listener.pending;
        if pending == SOCKET_SLOT_NONE {
            return ERR_WOULD_BLOCK;
        }
        let passcred = listener.passcred;
        listener.pending = SOCKET_SLOT_NONE;
        (pending, passcred)
    };
    unsafe {
        if let Some(connection) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(connection_slot as usize)
        {
            connection.nonblocking = flags & SOCK_NONBLOCK != 0;
            connection.passcred = passcred;
        }
    }
    let result = linux_install_socket(connection_slot, flags & (SOCK_NONBLOCK | SOCK_CLOEXEC));
    result
}

pub(crate) fn linux_socket_sendto(fd: u64, buffer_address: u64, length: u64) -> u64 {
    linux_socket_write(fd, buffer_address, length)
}

pub(crate) fn linux_socket_recvfrom(fd: u64, buffer_address: u64, length: u64) -> u64 {
    linux_socket_read(fd, buffer_address, length)
}

pub(crate) fn linux_socket_shutdown(fd: u64) -> u64 {
    let socket_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    let peer = unsafe {
        (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS))
            .get(socket_slot as usize)
            .map(|socket| socket.peer)
            .unwrap_or(SOCKET_SLOT_NONE)
    };
    unsafe {
        if let Some(socket) =
            (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
        {
            socket.connected = false;
            socket.peer = SOCKET_SLOT_NONE;
        }
        if peer != SOCKET_SLOT_NONE {
            if let Some(peer_socket) =
                (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(peer as usize)
            {
                if peer_socket.peer == socket_slot {
                    peer_socket.peer = SOCKET_SLOT_NONE;
                }
            }
        }
    }
    0
}

pub(crate) fn linux_socket_setsockopt(
    fd: u64,
    level: u64,
    option: u64,
    value_address: u64,
    value_length: u64,
) -> u64 {
    let socket_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    if level != SOL_SOCKET
        || !matches!(
            option,
            SO_REUSEADDR
                | SO_REUSEPORT
                | SO_KEEPALIVE
                | SO_PASSCRED
                | SO_SNDBUF
                | SO_RCVBUF
                | SO_RCVTIMEO
                | SO_SNDTIMEO
        )
    {
        return ERR_NO_PROTOCOL;
    }
    if value_address != 0 && value_length != 0 {
        let mut value = [0u8; 16];
        let length = usize::try_from(value_length)
            .unwrap_or(usize::MAX)
            .min(value.len());
        if user_memory::copy_from_user(value_address, &mut value[..length]).is_err() {
            return ERR_ADDRESS;
        }
    }
    if option == SO_PASSCRED {
        if value_address == 0 || value_length < 4 {
            return ERR_INVALID;
        }
        let mut enabled = [0u8; 4];
        if user_memory::copy_from_user(value_address, &mut enabled).is_err() {
            return ERR_ADDRESS;
        }
        unsafe {
            if let Some(socket) =
                (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).get_mut(socket_slot as usize)
            {
                socket.passcred = u32::from_ne_bytes(enabled) != 0;
            }
        }
    }
    0
}

pub(crate) fn linux_socket_passcred(fd: u64) -> bool {
    let Some(socket_slot) = linux_socket_slot(fd) else {
        return false;
    };
    unsafe {
        (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS))
            .get(socket_slot as usize)
            .is_some_and(|socket| socket.active && socket.passcred)
    }
}

pub(crate) fn linux_socket_getsockopt(
    fd: u64,
    level: u64,
    option: u64,
    value_address: u64,
    length_address: u64,
) -> u64 {
    let socket_slot = match linux_socket_slot_for_fd(fd) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    if level != SOL_SOCKET || value_address == 0 || length_address == 0 {
        return ERR_INVALID;
    }
    let mut length_bytes = [0u8; 4];
    if user_memory::copy_from_user(length_address, &mut length_bytes).is_err() {
        return ERR_ADDRESS;
    }
    let requested = u32::from_ne_bytes(length_bytes) as usize;
    if requested < 4 {
        return ERR_INVALID;
    }
    let value = match option {
        SO_TYPE => unsafe {
            (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS))
                .get(socket_slot as usize)
                .map(|socket| u32::from(socket.socket_type))
                .unwrap_or(0)
        },
        SO_SNDBUF | SO_RCVBUF => SOCKET_BUFFER_CAPACITY as u32,
        _ => return ERR_NO_PROTOCOL,
    };
    if user_memory::copy_to_user(value_address, &value.to_ne_bytes()).is_err() {
        return ERR_ADDRESS;
    }
    let actual = 4u32.to_ne_bytes();
    if user_memory::copy_to_user(length_address, &actual).is_err() {
        return ERR_ADDRESS;
    }
    0
}

pub(crate) fn linux_fd_revents(fd: u64, requested: u32) -> u32 {
    if fd <= 2 {
        return if fd == 0 {
            requested & EPOLLIN
        } else {
            requested & EPOLLOUT
        };
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return EPOLLERR;
    };
    let Ok(fd) = u32::try_from(fd) else {
        return EPOLLERR;
    };
    let Some((_, entry)) = linux_fd_entry(owner_pid, fd) else {
        return EPOLLERR | EPOLLHUP;
    };
    if entry.kind == LINUX_FD_SOCKET {
        let (readable, writable, closed) = unsafe {
            let Some(socket) =
                (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS)).get(entry.resource_slot as usize)
            else {
                return EPOLLERR;
            };
            let peer_available = socket.peer != SOCKET_SLOT_NONE
                && (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS))
                    .get(socket.peer as usize)
                    .is_some_and(|peer| peer.active);
            let writable = peer_available
                && (*core::ptr::addr_of!(LINUX_SOCKET_SLOTS))
                    .get(socket.peer as usize)
                    .is_some_and(|peer| peer.length < SOCKET_BUFFER_CAPACITY);
            (
                socket.length != 0 || (socket.listening && socket.pending != SOCKET_SLOT_NONE),
                writable,
                !peer_available,
            )
        };
        let mut result = 0u32;
        if readable {
            result |= EPOLLIN;
        }
        if writable {
            result |= EPOLLOUT;
        }
        if closed {
            result |= EPOLLHUP;
        }
        result & requested
    } else if entry.kind == LINUX_FD_NATIVE {
        if entry.stdio_fd != LINUX_STDIO_NONE {
            if entry.stdio_fd == 0 {
                requested & EPOLLIN
            } else {
                requested & EPOLLOUT
            }
        } else {
            requested & (EPOLLIN | EPOLLOUT)
        }
    } else if entry.kind == LINUX_FD_EVENTFD {
        let (readable, writable) = unsafe {
            let Some(eventfd) =
                (*core::ptr::addr_of!(LINUX_EVENTFD_SLOTS)).get(entry.resource_slot as usize)
            else {
                return EPOLLERR;
            };
            (
                eventfd.active && eventfd.counter != 0,
                eventfd.active && eventfd.counter != u64::MAX,
            )
        };
        let mut result = 0u32;
        if readable {
            result |= EPOLLIN;
        }
        if writable {
            result |= EPOLLOUT;
        }
        result & requested
    } else if entry.kind == LINUX_FD_SIGNALFD {
        if linux_signalfd_mask(u64::from(fd))
            .is_some_and(|mask| task::current_linux_signalfd_readable(mask))
        {
            requested & EPOLLIN
        } else {
            0
        }
    } else {
        0
    }
}

pub(crate) fn linux_poll(buffer_address: u64, requested: u64) -> u64 {
    const POLLFD_SIZE: u64 = 8;
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if count > 64 || (count != 0 && buffer_address == 0) {
        return ERR_INVALID;
    }
    let mut ready = 0usize;
    for index in 0..count {
        let Some(address) = buffer_address.checked_add((index as u64) * POLLFD_SIZE) else {
            return ERR_ADDRESS;
        };
        let mut bytes = [0u8; POLLFD_SIZE as usize];
        if user_memory::copy_from_user(address, &mut bytes).is_err() {
            return ERR_ADDRESS;
        }
        let fd = i32::from_ne_bytes(bytes[..4].try_into().unwrap());
        let events = i16::from_ne_bytes(bytes[4..6].try_into().unwrap()) as u16;
        let revents = if fd < 0 {
            0
        } else if fd <= 2
            || linux_fd_entry(task::resource_owner_pid().unwrap_or(0), fd as u32).is_some()
        {
            let requested_events = u32::from(events & (POLLIN | POLLOUT)) | EPOLLERR | EPOLLHUP;
            let socket_events = linux_fd_revents(fd as u64, requested_events);
            let mut result = (socket_events & (EPOLLIN | EPOLLOUT)) as u16;
            if socket_events & EPOLLERR != 0 {
                result |= POLLERR;
            }
            if socket_events & EPOLLHUP != 0 {
                result |= POLLHUP;
            }
            result
        } else {
            u16::from(POLLNVAL)
        };
        if revents != 0 {
            ready += 1;
        }
        bytes[6..8].copy_from_slice(&revents.to_ne_bytes());
        if user_memory::copy_to_user(address, &bytes).is_err() {
            return ERR_ADDRESS;
        }
    }
    ready as u64
}

pub(crate) fn linux_epoll_create1(flags: u64) -> u64 {
    if flags & !SOCK_CLOEXEC != 0 {
        return ERR_INVALID;
    }
    let Some(slot) = (unsafe {
        (*core::ptr::addr_of!(EPOLL_SLOTS))
            .iter()
            .position(|epoll| !epoll.active)
    }) else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(EPOLL_SLOTS))[slot] = EpollSlot {
            active: true,
            ..EpollSlot::EMPTY
        };
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        unsafe { (*core::ptr::addr_of_mut!(EPOLL_SLOTS))[slot] = EpollSlot::EMPTY };
        return ERR_BAD_FD;
    };
    let Some(fd) = linux_next_fd(owner_pid) else {
        unsafe { (*core::ptr::addr_of_mut!(EPOLL_SLOTS))[slot] = EpollSlot::EMPTY };
        return ERR_TOO_MANY_FILES;
    };
    match linux_fd_install_epoll_at(owner_pid, fd, slot as u8, flags) {
        Ok(fd) => fd,
        Err(error) => {
            unsafe { (*core::ptr::addr_of_mut!(EPOLL_SLOTS))[slot] = EpollSlot::EMPTY };
            error
        }
    }
}

pub(crate) fn linux_epoll_ctl(
    epoll_fd: u64,
    operation: u64,
    watched_fd: u64,
    event_address: u64,
) -> u64 {
    let epoll_slot = match linux_epoll_slot(epoll_fd) {
        Some(slot) => slot,
        None => return ERR_BAD_FD,
    };
    if watched_fd == epoll_fd {
        return ERR_INVALID;
    }
    if watched_fd > 2 {
        let Some(owner_pid) = task::resource_owner_pid() else {
            return ERR_BAD_FD;
        };
        let Ok(watched_fd) = u32::try_from(watched_fd) else {
            return ERR_BAD_FD;
        };
        if linux_fd_entry(owner_pid, watched_fd).is_none() {
            return ERR_BAD_FD;
        }
    }
    let index = unsafe {
        (*core::ptr::addr_of!(EPOLL_SLOTS))[epoll_slot as usize]
            .watches
            .iter()
            .position(|watch| watch.active && watch.fd == watched_fd as u32)
    };
    match operation {
        EPOLL_CTL_DEL => {
            let Some(index) = index else {
                return ERR_NO_ENTRY;
            };
            unsafe {
                (*core::ptr::addr_of_mut!(EPOLL_SLOTS))[epoll_slot as usize].watches[index] =
                    EpollWatch::EMPTY;
            }
            0
        }
        EPOLL_CTL_ADD | EPOLL_CTL_MOD => {
            if event_address == 0 {
                return ERR_ADDRESS;
            }
            let mut event = [0u8; 16];
            if user_memory::copy_from_user(event_address, &mut event).is_err() {
                return ERR_ADDRESS;
            }
            let events = u32::from_ne_bytes(event[..4].try_into().unwrap());
            let data = u64::from_ne_bytes(event[8..16].try_into().unwrap());
            let target = match (operation, index) {
                (EPOLL_CTL_ADD, Some(_)) => return ERR_ADDRESS_IN_USE,
                (EPOLL_CTL_MOD, None) => return ERR_NO_ENTRY,
                (_, Some(index)) => index,
                (EPOLL_CTL_ADD, None) => unsafe {
                    let Some(index) = (*core::ptr::addr_of!(EPOLL_SLOTS))[epoll_slot as usize]
                        .watches
                        .iter()
                        .position(|watch| !watch.active)
                    else {
                        return ERR_OUT_OF_MEMORY;
                    };
                    index
                },
                _ => return ERR_INVALID,
            };
            unsafe {
                (*core::ptr::addr_of_mut!(EPOLL_SLOTS))[epoll_slot as usize].watches[target] =
                    EpollWatch {
                        fd: watched_fd as u32,
                        events,
                        data,
                        active: true,
                    };
            }
            0
        }
        _ => ERR_INVALID,
    }
}

pub(crate) fn linux_epoll_wait(epoll_fd: u64, event_address: u64, max_events: u64) -> u64 {
    let epoll_slot = match linux_epoll_slot(epoll_fd) {
        Some(slot) => slot,
        None => return ERR_BAD_FD,
    };
    let max_events = usize::try_from(max_events).unwrap_or(usize::MAX);
    if max_events == 0 || max_events > MAX_EPOLL_WATCHES || event_address == 0 {
        return ERR_INVALID;
    }
    let mut watches = [EpollWatch::EMPTY; MAX_EPOLL_WATCHES];
    let mut watch_count = 0usize;
    unsafe {
        for watch in (*core::ptr::addr_of!(EPOLL_SLOTS))[epoll_slot as usize]
            .watches
            .iter()
            .copied()
        {
            if watch.active && watch_count < watches.len() {
                watches[watch_count] = watch;
                watch_count += 1;
            }
        }
    }
    let mut ready = 0usize;
    for watch in watches.iter().copied().take(watch_count) {
        if ready == max_events {
            break;
        }
        let events = linux_fd_revents(watch.fd as u64, watch.events);
        if events == 0 {
            continue;
        }
        let mut output = [0u8; 16];
        output[..4].copy_from_slice(&events.to_ne_bytes());
        output[8..16].copy_from_slice(&watch.data.to_ne_bytes());
        let Some(address) = event_address.checked_add((ready as u64) * 16) else {
            return ERR_ADDRESS;
        };
        if user_memory::copy_to_user(address, &output).is_err() {
            return ERR_ADDRESS;
        }
        ready += 1;
    }
    ready as u64
}

/// Install a native file capability in the current process's Linux fd table.
/// On table exhaustion, the native capability is closed before returning the
/// Linux `EMFILE`/`ENOMEM` error so an `openat` failure cannot leak a handle.
pub(crate) fn linux_install_handle(native_handle: u64, flags: u64) -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        let _ = close(native_handle);
        return ERR_BAD_FD;
    };
    let Some(fd) = linux_next_fd(owner_pid) else {
        let _ = close(native_handle);
        return ERR_TOO_MANY_FILES;
    };
    match linux_fd_install_at(
        owner_pid,
        fd,
        native_handle,
        u32::from(LINUX_STDIO_NONE),
        flags,
    ) {
        Ok(fd) => fd,
        Err(error) => {
            let _ = close(native_handle);
            error
        }
    }
}

fn linux_install_socket(socket_slot: u8, flags: u64) -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        linux_socket_release(socket_slot);
        return ERR_BAD_FD;
    };
    let Some(fd) = linux_next_fd(owner_pid) else {
        linux_socket_release(socket_slot);
        return ERR_TOO_MANY_FILES;
    };
    match linux_fd_install_socket_at(owner_pid, fd, socket_slot, flags) {
        Ok(fd) => fd,
        Err(error) => {
            linux_socket_release(socket_slot);
            error
        }
    }
}

pub(crate) fn linux_eventfd(init_value: u64, flags: u64) -> u64 {
    const EFD_SEMAPHORE: u64 = 1;
    const EFD_NONBLOCK: u64 = 0x800;
    const EFD_CLOEXEC: u64 = 0x80000;
    if init_value > u64::from(u32::MAX)
        || flags & !(EFD_SEMAPHORE | EFD_NONBLOCK | EFD_CLOEXEC) != 0
    {
        return ERR_INVALID;
    }
    let Some(slot) = (unsafe {
        (*core::ptr::addr_of!(LINUX_EVENTFD_SLOTS))
            .iter()
            .position(|eventfd| !eventfd.active)
    }) else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS))[slot] = LinuxEventFdSlot {
            counter: init_value,
            semaphore: flags & EFD_SEMAPHORE != 0,
            references: 0,
            active: true,
        };
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        unsafe { (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS))[slot] = LinuxEventFdSlot::EMPTY };
        return ERR_BAD_FD;
    };
    let Some(fd) = linux_next_fd(owner_pid) else {
        unsafe { (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS))[slot] = LinuxEventFdSlot::EMPTY };
        return ERR_TOO_MANY_FILES;
    };
    match linux_fd_install_eventfd_at(owner_pid, fd, slot as u8, flags) {
        Ok(fd) => fd,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS))[slot] = LinuxEventFdSlot::EMPTY
            };
            error
        }
    }
}

pub(crate) fn linux_eventfd_read(fd: u64, buffer_address: u64, requested: u64) -> u64 {
    if requested != 8 {
        return ERR_INVALID;
    }
    if buffer_address == 0 {
        return ERR_ADDRESS;
    }
    let Some(slot) = linux_eventfd_slot(fd) else {
        return ERR_BAD_FD;
    };
    let value = unsafe {
        let Some(eventfd) = (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS)).get_mut(slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !eventfd.active || eventfd.counter == 0 {
            return ERR_WOULD_BLOCK;
        }
        let value = if eventfd.semaphore {
            1
        } else {
            eventfd.counter
        };
        eventfd.counter -= value;
        value
    };
    if user_memory::copy_to_user(buffer_address, &value.to_ne_bytes()).is_err() {
        unsafe {
            if let Some(eventfd) =
                (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS)).get_mut(slot as usize)
            {
                eventfd.counter = eventfd.counter.saturating_add(value);
            }
        }
        return ERR_ADDRESS;
    }
    8
}

pub(crate) fn linux_eventfd_write(fd: u64, buffer_address: u64, requested: u64) -> u64 {
    if requested != 8 {
        return ERR_INVALID;
    }
    if buffer_address == 0 {
        return ERR_ADDRESS;
    }
    let Some(slot) = linux_eventfd_slot(fd) else {
        return ERR_BAD_FD;
    };
    let mut bytes = [0u8; 8];
    if user_memory::copy_from_user(buffer_address, &mut bytes).is_err() {
        return ERR_ADDRESS;
    }
    let value = u64::from_ne_bytes(bytes);
    if value == u64::MAX {
        return ERR_INVALID;
    }
    unsafe {
        let Some(eventfd) = (*core::ptr::addr_of_mut!(LINUX_EVENTFD_SLOTS)).get_mut(slot as usize)
        else {
            return ERR_BAD_FD;
        };
        if !eventfd.active {
            return ERR_BAD_FD;
        }
        let Some(counter) = eventfd.counter.checked_add(value) else {
            return ERR_WOULD_BLOCK;
        };
        eventfd.counter = counter;
    }
    8
}

pub(crate) fn linux_inotify_init(flags: u64) -> u64 {
    const IN_NONBLOCK: u64 = 0x800;
    const IN_CLOEXEC: u64 = 0x80000;
    if flags & !(IN_NONBLOCK | IN_CLOEXEC) != 0 {
        return ERR_INVALID;
    }
    let Some(slot) = (unsafe {
        (*core::ptr::addr_of!(LINUX_INOTIFY_SLOTS))
            .iter()
            .position(|inotify| !inotify.active)
    }) else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_INOTIFY_SLOTS))[slot] = LinuxInotifySlot {
            next_watch: 1,
            references: 0,
            active: true,
        };
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        unsafe { (*core::ptr::addr_of_mut!(LINUX_INOTIFY_SLOTS))[slot] = LinuxInotifySlot::EMPTY };
        return ERR_BAD_FD;
    };
    let Some(fd) = linux_next_fd(owner_pid) else {
        unsafe { (*core::ptr::addr_of_mut!(LINUX_INOTIFY_SLOTS))[slot] = LinuxInotifySlot::EMPTY };
        return ERR_TOO_MANY_FILES;
    };
    match linux_fd_install_inotify_at(owner_pid, fd, slot as u8, flags) {
        Ok(fd) => fd,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(LINUX_INOTIFY_SLOTS))[slot] = LinuxInotifySlot::EMPTY
            };
            error
        }
    }
}

pub(crate) fn linux_inotify_add_watch(fd: u64, path_address: u64, _mask: u64) -> u64 {
    if linux_inotify_slot(fd).is_none() {
        return ERR_BAD_FD;
    }
    match path_exists(path_address) {
        Ok(true) => {}
        Ok(false) => return ERR_NO_ENTRY,
        Err(error) => return error,
    }
    unsafe {
        let Some(inotify) = (*core::ptr::addr_of_mut!(LINUX_INOTIFY_SLOTS))
            .get_mut(linux_inotify_slot(fd).unwrap() as usize)
        else {
            return ERR_BAD_FD;
        };
        let watch = inotify.next_watch;
        inotify.next_watch = inotify.next_watch.saturating_add(1);
        watch as u64
    }
}

pub(crate) fn linux_inotify_rm_watch(fd: u64, watch: u64) -> u64 {
    let Some(slot) = linux_inotify_slot(fd) else {
        return ERR_BAD_FD;
    };
    let valid = unsafe {
        (*core::ptr::addr_of!(LINUX_INOTIFY_SLOTS))
            .get(slot as usize)
            .is_some_and(|inotify| {
                inotify.active && watch >= 1 && watch < inotify.next_watch as u64
            })
    };
    if valid { 0 } else { ERR_INVALID }
}

pub(crate) fn linux_inotify_read(fd: u64, _buffer_address: u64, _requested: u64) -> u64 {
    if linux_inotify_slot(fd).is_some() {
        ERR_WOULD_BLOCK
    } else {
        ERR_BAD_FD
    }
}

pub(crate) fn linux_signalfd4(fd: u64, mask_address: u64, mask_size: u64, flags: u64) -> u64 {
    const SFD_NONBLOCK: u64 = 0x800;
    const SFD_CLOEXEC: u64 = 0x80000;
    if mask_address == 0 || !matches!(mask_size, 8 | 128) {
        return ERR_INVALID;
    }
    if flags & !(SFD_NONBLOCK | SFD_CLOEXEC) != 0 {
        return ERR_INVALID;
    }
    let mut mask_bytes = [0u8; 8];
    if user_memory::copy_from_user(mask_address, &mut mask_bytes).is_err() {
        return ERR_ADDRESS;
    }
    let mask = u64::from_ne_bytes(mask_bytes);
    if fd != u64::MAX {
        let Some(slot) = linux_signalfd_slot(fd) else {
            return ERR_BAD_FD;
        };
        unsafe {
            let Some(signalfd) =
                (*core::ptr::addr_of_mut!(LINUX_SIGNALFD_SLOTS)).get_mut(slot as usize)
            else {
                return ERR_BAD_FD;
            };
            if !signalfd.active {
                return ERR_BAD_FD;
            }
            signalfd.mask = mask;
        }
        return fd;
    }
    let Some(slot) = (unsafe {
        (*core::ptr::addr_of!(LINUX_SIGNALFD_SLOTS))
            .iter()
            .position(|signalfd| !signalfd.active)
    }) else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_SIGNALFD_SLOTS))[slot] = LinuxSignalFdSlot {
            mask,
            references: 0,
            active: true,
        };
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        unsafe {
            (*core::ptr::addr_of_mut!(LINUX_SIGNALFD_SLOTS))[slot] = LinuxSignalFdSlot::EMPTY
        };
        return ERR_BAD_FD;
    };
    let Some(new_fd) = linux_next_fd(owner_pid) else {
        unsafe {
            (*core::ptr::addr_of_mut!(LINUX_SIGNALFD_SLOTS))[slot] = LinuxSignalFdSlot::EMPTY
        };
        return ERR_TOO_MANY_FILES;
    };
    match linux_fd_install_signalfd_at(owner_pid, new_fd, slot as u8, flags) {
        Ok(new_fd) => new_fd,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(LINUX_SIGNALFD_SLOTS))[slot] = LinuxSignalFdSlot::EMPTY
            };
            error
        }
    }
}

pub(crate) fn linux_signalfd_read(fd: u64, _buffer_address: u64, requested: u64) -> u64 {
    if linux_signalfd_slot(fd).is_none() {
        return ERR_BAD_FD;
    }
    if _buffer_address == 0 {
        return ERR_ADDRESS;
    }
    if requested < 128 {
        return ERR_INVALID;
    }
    let Some(mask) = linux_signalfd_mask(fd) else {
        return ERR_BAD_FD;
    };
    let Some(signal) = task::take_current_linux_signalfd_signal(mask) else {
        return ERR_WOULD_BLOCK;
    };
    let mut info = [0u8; 128];
    info[..4].copy_from_slice(&(signal as u32).to_ne_bytes());
    let sender = task::current_process_pid().unwrap_or(0) as u32;
    info[12..16].copy_from_slice(&sender.to_ne_bytes());
    if user_memory::copy_to_user(_buffer_address, &info).is_err() {
        let _ = task::restore_current_linux_signalfd_signal(signal);
        return ERR_ADDRESS;
    }
    128
}

/// Create a Linux pipe directly in the integer-fd namespace.  The native
/// pipe remains generation-checked and bounded; only its two capabilities are
/// translated into Linux descriptors for the Android personality.
pub(crate) fn linux_pipe2(buffer_address: u64, flags: u64) -> u64 {
    const O_NONBLOCK: u64 = 0x800;
    const O_CLOEXEC: u64 = 0x80000;
    if buffer_address == 0 || flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return ERR_INVALID;
    }
    let [read_handle, write_handle] = match create_pipe_handles() {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let read_fd = linux_install_handle(read_handle, flags);
    if (read_fd as i64) < 0 {
        let _ = close(write_handle);
        return read_fd;
    }
    let write_fd = linux_install_handle(write_handle, flags);
    if (write_fd as i64) < 0 {
        let _ = linux_close(read_fd);
        return write_fd;
    }
    let mut descriptors = [0u8; 8];
    descriptors[..4].copy_from_slice(&(read_fd as u32).to_ne_bytes());
    descriptors[4..].copy_from_slice(&(write_fd as u32).to_ne_bytes());
    if user_memory::copy_to_user(buffer_address, &descriptors).is_err() {
        let _ = linux_close(read_fd);
        let _ = linux_close(write_fd);
        return ERR_ADDRESS;
    }
    0
}

/// Return the native capability behind a Linux fd, or `None` for stdio and
/// invalid descriptors. Linux stdio is handled directly by the personality.
pub(crate) fn linux_native_handle(fd: u64) -> Option<u64> {
    let owner_pid = task::resource_owner_pid()?;
    let fd = u32::try_from(fd).ok()?;
    let (_, entry) = linux_fd_entry(owner_pid, fd)?;
    (entry.kind == LINUX_FD_NATIVE && entry.stdio_fd == LINUX_STDIO_NONE)
        .then_some(entry.native_handle)
}

pub(crate) fn linux_property_file(handle: u64) -> bool {
    let Some((_, file)) = locate_entry(handle) else {
        return false;
    };
    file.kind == KIND_FILE
        && with_vfs(|vfs| vfs.is_property_file_at(file.mount_index, file.local_fd)).unwrap_or(false)
}

pub(crate) fn linux_ftruncate(handle: u64, length: u64) -> u64 {
    if !linux_property_file(handle) {
        return ERR_NOT_SUPPORTED;
    }
    (length == ANDROID_PROPERTY_AREA_SIZE)
        .then_some(0)
        .unwrap_or(ERR_INVALID)
}

/// Resolve a Linux fd to its stdio source. The base descriptors and their
/// duplicated table entries both use the UART-backed stdio boundary.
pub(crate) fn linux_stdio_fd(fd: u64) -> Option<u32> {
    if fd <= 2 {
        return Some(fd as u32);
    }
    let owner_pid = task::resource_owner_pid()?;
    let fd = u32::try_from(fd).ok()?;
    linux_fd_entry(owner_pid, fd).and_then(|(_, entry)| {
        (entry.stdio_fd != LINUX_STDIO_NONE).then_some(entry.stdio_fd as u32)
    })
}

pub(crate) fn linux_duplicate(fd: u64, flags: u64) -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let source = if fd <= 2 {
        LinuxFdEntry {
            owner_pid,
            fd: fd as u32,
            native_handle: 0,
            stdio_fd: fd as u8,
            kind: LINUX_FD_NATIVE,
            resource_slot: 0,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        }
    } else {
        let Ok(fd) = u32::try_from(fd) else {
            return ERR_BAD_FD;
        };
        let Some((_, source)) = linux_fd_entry(owner_pid, fd) else {
            return ERR_BAD_FD;
        };
        source
    };
    let Some(target_fd) = linux_next_fd(owner_pid) else {
        return ERR_TOO_MANY_FILES;
    };
    if source.stdio_fd != LINUX_STDIO_NONE {
        return linux_fd_install_at(owner_pid, target_fd, 0, source.stdio_fd as u32, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_SOCKET {
        return linux_fd_install_socket_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_EPOLL {
        return linux_fd_install_epoll_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_EVENTFD {
        return linux_fd_install_eventfd_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_INOTIFY {
        return linux_fd_install_inotify_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_SIGNALFD {
        return linux_fd_install_signalfd_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    let native_handle = duplicate(source.native_handle);
    if (native_handle as i64) < 0 {
        return native_handle;
    }
    match linux_fd_install_at(
        owner_pid,
        target_fd,
        native_handle,
        u32::from(LINUX_STDIO_NONE),
        flags,
    ) {
        Ok(fd) => fd,
        Err(error) => {
            let _ = close(native_handle);
            error
        }
    }
}

pub(crate) fn linux_duplicate_at(fd: u64, target_fd: u64, flags: u64) -> u64 {
    if fd == target_fd {
        return ERR_INVALID;
    }
    if flags & !0x80000 != 0 {
        return ERR_INVALID;
    }
    let Ok(target_fd) = u32::try_from(target_fd) else {
        return ERR_TOO_MANY_FILES;
    };
    if !(LINUX_FD_MIN..=LINUX_FD_MAX).contains(&target_fd) {
        return ERR_TOO_MANY_FILES;
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let source = if fd <= 2 {
        LinuxFdEntry {
            owner_pid,
            fd: fd as u32,
            native_handle: 0,
            stdio_fd: fd as u8,
            kind: LINUX_FD_NATIVE,
            resource_slot: 0,
            flags: u32::try_from(flags).unwrap_or(u32::MAX),
            active: true,
        }
    } else {
        let Ok(fd) = u32::try_from(fd) else {
            return ERR_BAD_FD;
        };
        let Some((_, source)) = linux_fd_entry(owner_pid, fd) else {
            return ERR_BAD_FD;
        };
        source
    };
    if linux_fd_in_use(owner_pid, target_fd) {
        let result = linux_close(target_fd as u64);
        if (result as i64) < 0 {
            return result;
        }
    }
    if source.stdio_fd != LINUX_STDIO_NONE {
        return linux_fd_install_at(owner_pid, target_fd, 0, source.stdio_fd as u32, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_SOCKET {
        return linux_fd_install_socket_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_EPOLL {
        return linux_fd_install_epoll_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_EVENTFD {
        return linux_fd_install_eventfd_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_INOTIFY {
        return linux_fd_install_inotify_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    if source.kind == LINUX_FD_SIGNALFD {
        return linux_fd_install_signalfd_at(owner_pid, target_fd, source.resource_slot, flags)
            .unwrap_or_else(|error| error);
    }
    let native_handle = duplicate(source.native_handle);
    if (native_handle as i64) < 0 {
        return native_handle;
    }
    match linux_fd_install_at(
        owner_pid,
        target_fd,
        native_handle,
        u32::from(LINUX_STDIO_NONE),
        flags,
    ) {
        Ok(fd) => fd,
        Err(error) => {
            let _ = close(native_handle);
            error
        }
    }
}

pub(crate) fn linux_close(fd: u64) -> u64 {
    if fd <= 2 {
        return 0;
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Ok(fd) = u32::try_from(fd) else {
        return ERR_BAD_FD;
    };
    let Some((index, entry)) = linux_fd_entry(owner_pid, fd) else {
        return ERR_BAD_FD;
    };
    if entry.kind == LINUX_FD_SOCKET {
        linux_socket_release(entry.resource_slot);
    } else if entry.kind == LINUX_FD_EPOLL {
        linux_epoll_release(entry.resource_slot);
    } else if entry.kind == LINUX_FD_EVENTFD {
        linux_eventfd_release(entry.resource_slot);
    } else if entry.kind == LINUX_FD_INOTIFY {
        linux_inotify_release(entry.resource_slot);
    } else if entry.kind == LINUX_FD_SIGNALFD {
        linux_signalfd_release(entry.resource_slot);
    } else if entry.stdio_fd == LINUX_STDIO_NONE {
        let result = close_for_owner(owner_pid, entry.native_handle);
        if (result as i64) < 0 {
            return result;
        }
    }
    unsafe {
        (*core::ptr::addr_of_mut!(LINUX_FDS))[index] = LinuxFdEntry::EMPTY;
    }
    0
}

/// Close Linux descriptors carrying `O_CLOEXEC` after a successful exec.
///
/// The descriptors are collected before closing so the global table is never
/// mutably aliased while it is being traversed.
pub(crate) fn linux_close_on_exec() -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let mut close_fds = [0u32; MAX_LINUX_FDS_PER_PROCESS];
    let mut close_count = 0usize;
    unsafe {
        for entry in (*core::ptr::addr_of!(LINUX_FDS)).iter() {
            if entry.active
                && entry.owner_pid == owner_pid
                && entry.flags & LINUX_FD_CLOEXEC != 0
                && close_count < close_fds.len()
            {
                close_fds[close_count] = entry.fd;
                close_count += 1;
            }
        }
    }
    for fd in close_fds.iter().copied().take(close_count) {
        let result = linux_close(fd as u64);
        if (result as i64) < 0 {
            return result;
        }
    }
    0
}

fn linux_inherit_capacity(parent_pid: u64) -> Option<usize> {
    let count = unsafe {
        (*core::ptr::addr_of!(LINUX_FDS))
            .iter()
            .filter(|entry| entry.active && entry.owner_pid == parent_pid)
            .count()
    };
    let free = unsafe {
        (*core::ptr::addr_of!(LINUX_FDS))
            .iter()
            .filter(|entry| !entry.active)
            .count()
    };
    (free >= count).then_some(count)
}

fn linux_inherit_fds_unchecked(parent_pid: u64, child_pid: u64, count: usize) {
    // A native Fullerene task may not own any Linux-personality descriptors.
    // The caller has already proved capacity for `count`; when that count is
    // zero there is nothing to copy, and entering the free-slot loop would
    // install `inherited[0]` repeatedly until the bounded array panics.
    if count == 0 {
        return;
    }
    let mut inherited = [LinuxFdEntry::EMPTY; MAX_LINUX_FDS_PER_PROCESS];
    let mut copied = 0usize;
    unsafe {
        for entry in (*core::ptr::addr_of!(LINUX_FDS)).iter().copied() {
            if entry.active && entry.owner_pid == parent_pid {
                inherited[copied] = LinuxFdEntry {
                    owner_pid: child_pid,
                    ..entry
                };
                copied += 1;
            }
        }
        let entries = core::ptr::addr_of_mut!(LINUX_FDS);
        let mut installed = 0usize;
        for entry in (*entries).iter_mut() {
            if !entry.active {
                *entry = inherited[installed];
                installed += 1;
                if installed == count {
                    break;
                }
            }
        }
    }
    for entry in inherited.iter().take(count) {
        if entry.kind == LINUX_FD_SOCKET {
            let _ = linux_socket_retain(entry.resource_slot);
        } else if entry.kind == LINUX_FD_EPOLL {
            let _ = linux_epoll_retain(entry.resource_slot);
        } else if entry.kind == LINUX_FD_EVENTFD {
            let _ = linux_eventfd_retain(entry.resource_slot);
        } else if entry.kind == LINUX_FD_INOTIFY {
            let _ = linux_inotify_retain(entry.resource_slot);
        } else if entry.kind == LINUX_FD_SIGNALFD {
            let _ = linux_signalfd_retain(entry.resource_slot);
        }
    }
}

/// Move one capability from the current process into a live target process.
///
/// The underlying VFS/resource reference is not retained a second time: this
/// is a move, matching the generic capability implementation.  The target
/// receives a fresh logical slot and generation, so the source token cannot
/// remain usable after a successful transfer.
pub(crate) fn transfer(target_pid: u64, handle: u64) -> u64 {
    let Some(source_pid) = task::current_pid() else {
        return ERR_BAD_FD;
    };
    if !task::contains_live_pid(target_pid) {
        return (-(3i64)) as u64;
    }
    let Some((storage_index, source)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if source.kind == KIND_SHARED_BUFFER && shared_buffer_has_any_mapping(source.pipe_slot) {
        return (-(16i64)) as u64;
    }

    let target_slot = unsafe {
        let files = core::ptr::addr_of!(OPEN_FILES);
        (0..MAX_HANDLE_SLOTS).find(|index| {
            !(*files).iter().any(|file| {
                file.active && file.owner_pid == target_pid && file.handle_index == *index as u16
            })
        })
    };
    let Some(target_slot) = target_slot else {
        return ERR_OUT_OF_MEMORY;
    };

    let generation = next_generation();
    let new_handle =
        FILE_HANDLE_TAG | (generation << FILE_HANDLE_GENERATION_SHIFT) | target_slot as u64;
    unsafe {
        (*core::ptr::addr_of_mut!(OPEN_FILES))[storage_index] = OpenFile {
            owner_pid: target_pid,
            handle_index: target_slot as u16,
            generation,
            ..source
        };
    }
    uart::put_hex("aarch64 handle transfer from=", source_pid);
    uart::put_hex("aarch64 handle transfer to=", target_pid);
    new_handle
}

/// Revoke one capability in the current process and release its resource.
/// Duplicated or previously transferred capabilities remain independent.
pub(crate) fn revoke(handle: u64) -> u64 {
    close(handle)
}

pub(crate) fn read(handle: u64, buffer_address: u64, requested: u64) -> u64 {
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if count > MAX_READ {
        return ERR_INVALID;
    }
    if count == 0 {
        return 0;
    }
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind == KIND_PIPE_READ {
        return read_pipe(file.pipe_slot, buffer_address, count);
    }
    if file.kind == KIND_TERMINAL {
        return ERR_WOULD_BLOCK;
    }
    if file.kind != KIND_FILE {
        return ERR_PERMISSION;
    }
    let local_fd = file.local_fd;
    let mount_index = file.mount_index;
    if file.selinux_attr != SELINUX_ATTR_NONE {
        let mut data = [0u8; MAX_SELINUX_CONTEXT + 1];
        let label_length = if file.selinux_attr == SELINUX_ATTR_CURRENT {
            task::current_selinux_context(&mut data)
        } else {
            task::current_selinux_exec_context(&mut data)
        };
        let data_length = if label_length == 0 {
            0
        } else {
            data[label_length] = 0;
            label_length + 1
        };
        let Some(position) = with_vfs(|vfs| vfs.position_at(mount_index, local_fd).ok()).flatten()
        else {
            return ERR_BAD_FD;
        };
        let start = usize::try_from(position).unwrap_or(usize::MAX);
        if start >= data_length {
            return 0;
        }
        let bytes_read = count.min(data_length - start);
        if user_memory::copy_to_user(buffer_address, &data[start..start + bytes_read]).is_err() {
            return ERR_ADDRESS;
        }
        if with_vfs(|vfs| {
            vfs.seek_at(
                mount_index,
                local_fd,
                position.saturating_add(bytes_read as u64),
            )
            .is_ok()
        }) != Some(true)
        {
            return ERR_BAD_FD;
        }
        return bytes_read as u64;
    }
    let mut buffer = [0u8; MAX_READ];
    let Some(bytes_read) = with_vfs(|vfs| {
        vfs.read_at(mount_index, local_fd, &mut buffer[..count])
            .ok()
    })
    .flatten() else {
        return ERR_BAD_FD;
    };
    if user_memory::copy_to_user(buffer_address, &buffer[..bytes_read]).is_err() {
        return ERR_ADDRESS;
    }
    bytes_read as u64
}

pub(crate) fn write(handle: u64, buffer_address: u64, requested: u64) -> u64 {
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if count > MAX_READ {
        return ERR_OVERFLOW;
    }
    if count == 0 {
        return 0;
    }
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    let mut buffer = [0u8; MAX_READ];
    if user_memory::copy_from_user(buffer_address, &mut buffer[..count]).is_err() {
        return ERR_ADDRESS;
    }
    if file.kind == KIND_PIPE_WRITE {
        return write_pipe(file.pipe_slot, &buffer[..count]);
    }
    if file.kind == KIND_TERMINAL {
        for byte in &buffer[..count] {
            uart::putc(*byte);
        }
        return count as u64;
    }
    if file.kind != KIND_FILE {
        return ERR_PERMISSION;
    }
    let local_fd = file.local_fd;
    let mount_index = file.mount_index;
    if file.selinux_attr != SELINUX_ATTR_NONE {
        if file.selinux_attr != SELINUX_ATTR_EXEC {
            return ERR_PERMISSION;
        }
        if !selinux_transition_allowed(&buffer[..count]) {
            return ERR_PERMISSION;
        }
        if !task::set_current_selinux_exec_context(&buffer[..count]) {
            return ERR_INVALID;
        }
        let Some(position) = with_vfs(|vfs| vfs.position_at(mount_index, local_fd).ok()).flatten()
        else {
            return ERR_BAD_FD;
        };
        if with_vfs(|vfs| {
            vfs.seek_at(mount_index, local_fd, position.saturating_add(count as u64))
                .is_ok()
        }) != Some(true)
        {
            return ERR_BAD_FD;
        }
        return count as u64;
    }
    let Some(bytes_written) =
        with_vfs(|vfs| vfs.write_at(mount_index, local_fd, &buffer[..count]).ok()).flatten()
    else {
        return ERR_BAD_FD;
    };
    bytes_written as u64
}

/// Seek a native file capability using Linux's `lseek` whence values. The
/// Linux fd table translates the integer descriptor before calling this
/// helper; the VFS continues to own the shared open-file offset.
pub(crate) fn seek(handle: u64, offset: i64, whence: u64) -> u64 {
    let Some((_, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind != KIND_FILE || !matches!(whence, 0..=2) {
        return ERR_INVALID;
    }
    let Some((current, size)) = with_vfs(|vfs| {
        Some((
            vfs.position_at(file.mount_index, file.local_fd).ok()?,
            vfs.size_at(file.mount_index, file.local_fd).ok()?,
        ))
    })
    .flatten() else {
        return ERR_BAD_FD;
    };
    let base = match whence {
        0 => 0,
        1 => current,
        2 => size,
        _ => return ERR_INVALID,
    };
    let position = if offset >= 0 {
        base.checked_add(offset as u64)
    } else {
        base.checked_sub(offset.unsigned_abs())
    };
    let Some(position) = position else {
        return ERR_INVALID;
    };
    if with_vfs(|vfs| {
        vfs.seek_at(file.mount_index, file.local_fd, position)
            .is_ok()
    }) != Some(true)
    {
        return ERR_BAD_FD;
    }
    position
}

/// Read file bytes at an explicit offset without changing the Linux-visible
/// file position.  This is the kernel side of file-backed `mmap`: VFS owns
/// the actual filesystem cursor, while the Linux fd ABI requires `mmap` to
/// leave the source descriptor's offset untouched.
pub(crate) fn read_kernel_at(
    handle: u64,
    offset: u64,
    destination: &mut [u8],
) -> Result<usize, u64> {
    let Some((_, file)) = locate_entry(handle) else {
        return Err(ERR_BAD_FD);
    };
    if file.kind != KIND_FILE {
        return Err(ERR_PERMISSION);
    }
    let result = with_vfs(|vfs| {
        let current = vfs
            .position_at(file.mount_index, file.local_fd)
            .map_err(|_| ERR_BAD_FD)?;
        vfs.seek_at(file.mount_index, file.local_fd, offset)
            .map_err(|_| ERR_BAD_FD)?;
        let result = vfs
            .read_at(file.mount_index, file.local_fd, destination)
            .map_err(|_| ERR_BAD_FD);
        let _ = vfs.seek_at(file.mount_index, file.local_fd, current);
        result
    });
    result.ok_or(ERR_BAD_FD)?
}

pub(crate) fn file_size(handle: u64) -> Result<u64, u64> {
    let Some((_, file)) = locate_entry(handle) else {
        return Err(ERR_BAD_FD);
    };
    if file.kind != KIND_FILE && file.kind != KIND_DIRECTORY {
        return Err(ERR_PERMISSION);
    }
    if file.selinux_attr != SELINUX_ATTR_NONE {
        let mut context = [0u8; MAX_SELINUX_CONTEXT];
        let length = if file.selinux_attr == SELINUX_ATTR_CURRENT {
            task::current_selinux_context(&mut context)
        } else {
            task::current_selinux_exec_context(&mut context)
        };
        return Ok(if length == 0 { 0 } else { (length + 1) as u64 });
    }
    let result = with_vfs(|vfs| {
        vfs.size_at(file.mount_index, file.local_fd)
            .map_err(|_| ERR_BAD_FD)
    });
    result.ok_or(ERR_BAD_FD)?
}

pub(crate) fn file_metadata(handle: u64) -> Result<FileMetadata, u64> {
    let Some((_, file)) = locate_entry(handle) else {
        return Err(ERR_BAD_FD);
    };
    if file.kind != KIND_FILE && file.kind != KIND_DIRECTORY {
        return Err(ERR_PERMISSION);
    }
    let result = with_vfs(|vfs| vfs.metadata_at(file.mount_index, file.local_fd));
    match result {
        Some(Ok(mut metadata)) => {
            if file.selinux_attr != SELINUX_ATTR_NONE {
                let mut context = [0u8; MAX_SELINUX_CONTEXT];
                let length = if file.selinux_attr == SELINUX_ATTR_CURRENT {
                    task::current_selinux_context(&mut context)
                } else {
                    task::current_selinux_exec_context(&mut context)
                };
                metadata.size = if length == 0 { 0 } else { (length + 1) as u64 };
            }
            Ok(metadata)
        }
        Some(Err(FsError::NotSupported)) => {
            let size = if file.selinux_attr != SELINUX_ATTR_NONE {
                let mut context = [0u8; MAX_SELINUX_CONTEXT];
                let length = if file.selinux_attr == SELINUX_ATTR_CURRENT {
                    task::current_selinux_context(&mut context)
                } else {
                    task::current_selinux_exec_context(&mut context)
                };
                if length == 0 { 0 } else { (length + 1) as u64 }
            } else {
                with_vfs(|vfs| vfs.size_at(file.mount_index, file.local_fd).ok())
                    .flatten()
                    .ok_or(ERR_BAD_FD)?
            };
            let mode = if file.kind == KIND_DIRECTORY {
                0o040555
            } else {
                0o100444
            };
            Ok(FileMetadata {
                mode,
                uid: 0,
                gid: 0,
                size,
                kind: if file.kind == KIND_DIRECTORY {
                    InodeType::Directory
                } else {
                    InodeType::File
                },
            })
        }
        Some(Err(error)) => Err(fs_error_to_errno(error)),
        None => Err(ERR_BAD_FD),
    }
}

pub(crate) fn file_is_directory(handle: u64) -> Result<bool, u64> {
    let Some((_, file)) = locate_entry(handle) else {
        return Err(ERR_BAD_FD);
    };
    Ok(file.kind == KIND_DIRECTORY)
}

/// Translate a Linux directory fd into bounded `getdents64` records. The
/// underlying VFS returns names, so inode numbers and d_type are deliberately
/// stable, conservative compatibility metadata rather than filesystem-native
/// values. The directory cursor is kept in the VFS descriptor position.
pub(crate) fn linux_getdents64(fd: u64, buffer_address: u64, requested: u64) -> u64 {
    let count = usize::try_from(requested).unwrap_or(usize::MAX);
    if count > MAX_READ {
        return ERR_INVALID;
    }
    if count == 0 {
        return 0;
    }
    let Some(native_handle) = linux_native_handle(fd) else {
        return ERR_BAD_FD;
    };
    let Some((_, file)) = locate_entry(native_handle) else {
        return ERR_BAD_FD;
    };
    if file.kind != KIND_DIRECTORY {
        return ERR_NOT_DIRECTORY;
    }
    let Some((path, path_length)) = (unsafe {
        (*core::ptr::addr_of!(DIRECTORY_SLOTS))
            .get(file.pipe_slot as usize)
            .and_then(|directory| {
                directory
                    .active
                    .then_some((directory.path, directory.path_length))
            })
    }) else {
        return ERR_BAD_FD;
    };
    let Ok(path) = core::str::from_utf8(&path[..path_length]) else {
        return ERR_INVALID;
    };
    let Some((position, entries)) = with_vfs(|vfs| {
        let position = vfs
            .position_at(file.mount_index, file.local_fd)
            .unwrap_or(0);
        let entries = vfs.readdir(path).ok()?;
        Some((usize::try_from(position).unwrap_or(usize::MAX), entries))
    })
    .flatten() else {
        return ERR_BAD_FD;
    };
    let total_entries = entries.len().saturating_add(2);
    if position >= total_entries {
        return 0;
    }

    let mut output = [0u8; MAX_READ];
    let mut written = 0usize;
    let mut next_position = position;
    while next_position < total_entries {
        let (name, is_directory) = match next_position {
            0 => (".", true),
            1 => ("..", true),
            index => {
                let entry = &entries[index - 2];
                (entry.name.as_str(), entry.is_dir)
            }
        };
        let name_length = name.len().saturating_add(1);
        let record_length = (19usize.saturating_add(name_length).saturating_add(7)) & !7;
        if record_length > count.saturating_sub(written) {
            if written == 0 {
                return ERR_INVALID;
            }
            break;
        }
        let record = &mut output[written..written + record_length];
        record[..8].copy_from_slice(&((next_position as u64).saturating_add(1)).to_ne_bytes());
        record[8..16].copy_from_slice(&((next_position as i64).saturating_add(1)).to_ne_bytes());
        record[16..18].copy_from_slice(&(record_length as u16).to_ne_bytes());
        record[18] = if is_directory { 4 } else { 8 };
        record[19..19 + name.len()].copy_from_slice(name.as_bytes());
        next_position += 1;
        written += record_length;
    }
    if user_memory::copy_to_user(buffer_address, &output[..written]).is_err() {
        return ERR_ADDRESS;
    }
    let _ = with_vfs(|vfs| vfs.seek_at(file.mount_index, file.local_fd, next_position as u64));
    written as u64
}

/// Check a pathname without opening a persistent Linux descriptor.
pub(crate) fn path_exists(path_address: u64) -> Result<bool, u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    with_vfs(|vfs| vfs.exists(path)).ok_or(ERR_NO_ENTRY)
}

pub(crate) fn path_is_directory(path_address: u64) -> Result<bool, u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    with_vfs(|vfs| {
        if !vfs.exists(path) {
            return Err(ERR_NO_ENTRY);
        }
        Ok(vfs.readdir(path).is_ok())
    })
    .ok_or(ERR_NO_ENTRY)?
}

pub(crate) fn is_device_path(path_address: u64) -> Result<bool, u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    Ok(path == "/dev" || path.starts_with("/dev/"))
}

fn is_runtime_socket_path(path: &str) -> bool {
    path == "/dev/socket" || path.starts_with("/dev/socket/")
}

fn is_virtual_namespace_path(path: &str) -> bool {
    matches!(
        path,
        "/dev"
            | "/dev/pts"
            | "/dev/shm"
            | "/dev/selinux"
            | "/dev/__properties__"
            | "/proc"
            | "/sys"
            | "/sys/fs/selinux"
            | "/sys/fs/cgroup"
            | "/metadata"
    )
}

fn is_android_filesystem_mount_target(path: &str) -> bool {
    matches!(path, "/system" | "/vendor" | "/data")
}

fn android_filesystem_is_mounted(path: &str) -> bool {
    with_vfs(|vfs| {
        vfs.mounted_fs_index(path)
            .is_some_and(|mount_index| mount_index != 0)
    })
    .unwrap_or(false)
}

fn android_mount_request_supported(path: &str, source: &str, filesystem: &str, flags: u64) -> bool {
    const MS_RDONLY: u64 = 1;
    if flags & MS_RDONLY == 0 || !source.starts_with("/dev/block/") {
        return false;
    }
    match path {
        "/system" => {
            (source.ends_with("/system") || source.ends_with("/system_a"))
                && matches!(filesystem, "ext4" | "erofs" | "auto")
        }
        "/vendor" => {
            (source.ends_with("/vendor") || source.ends_with("/vendor_a"))
                && matches!(filesystem, "ext4" | "erofs" | "auto")
        }
        "/data" => {
            (source.ends_with("/userdata") || source.ends_with("/userdata_a"))
                && matches!(filesystem, "f2fs" | "auto")
        }
        _ => false,
    }
}

pub(crate) fn is_writable_virtual_path(path_address: u64) -> Result<bool, u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    Ok(is_writable_virtual_path_string(path))
}

fn is_writable_virtual_path_string(path: &str) -> bool {
    matches!(
        path,
        "/sys/class/android_usb/state"
            | "/proc/sys/kernel/hostname"
            | "/proc/self/attr/exec"
            | "/sys/fs/selinux/enforce"
            | "/sys/fs/selinux/checkreqprot"
            | "/sys/fs/selinux/load"
            | "/sys/fs/selinux/context"
            | "/sys/fs/selinux/deny_unknown"
            | "/sys/fs/selinux/commit_pending_bools"
    )
}

fn fs_error_to_errno(error: FsError) -> u64 {
    match error {
        FsError::FileNotFound => ERR_NO_ENTRY,
        FsError::PermissionDenied => ERR_PERMISSION,
        FsError::InvalidFileDescriptor => ERR_BAD_FD,
        FsError::NotADirectory => ERR_NOT_DIRECTORY,
        FsError::NotSupported => ERR_NOT_SUPPORTED,
        FsError::DiskFull => ERR_NO_SPACE,
        FsError::Busy => ERR_BUSY,
        FsError::InvalidPath | FsError::InvalidInput => ERR_INVALID,
        FsError::FileExists
        | FsError::InvalidSeek
        | FsError::DirectoryNotEmpty
        | FsError::IsADirectory
        | FsError::UnexpectedEof
        | FsError::Io => ERR_IO,
    }
}

pub(crate) fn linux_chmod(path_address: u64, mode: u64) -> u64 {
    let path_storage = match copy_path(path_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    if !is_writable_virtual_path_string(path) {
        return ERR_PERMISSION;
    }
    if mode & !0o7777 != 0 {
        return ERR_INVALID;
    }
    match with_vfs(|vfs| vfs.chmod(path, mode as u32)) {
        Some(Ok(())) => 0,
        Some(Err(error)) => fs_error_to_errno(error),
        None => ERR_NO_ENTRY,
    }
}

pub(crate) fn linux_chown(path_address: u64, uid: u64, gid: u64) -> u64 {
    let path_storage = match copy_path(path_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    if !is_writable_virtual_path_string(path) {
        return ERR_PERMISSION;
    }
    let (Ok(uid), Ok(gid)) = (u32::try_from(uid), u32::try_from(gid)) else {
        return ERR_INVALID;
    };
    match with_vfs(|vfs| vfs.chown(path, uid, gid)) {
        Some(Ok(())) => 0,
        Some(Err(error)) => fs_error_to_errno(error),
        None => ERR_NO_ENTRY,
    }
}

fn is_runtime_namespace_entry(path: &str) -> bool {
    is_runtime_socket_path(path)
        || path.starts_with("/dev/selinux/")
        || path.starts_with("/dev/__properties__/")
}

pub(crate) fn linux_mkdirat(path_address: u64) -> u64 {
    let path_storage = match copy_path(path_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    if is_runtime_socket_path(path) || is_virtual_namespace_path(path) {
        0
    } else {
        (-(30i64)) as u64
    }
}

pub(crate) fn linux_unlinkat(path_address: u64) -> u64 {
    let path_storage = match copy_path(path_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    if !is_runtime_namespace_entry(path) || path == "/dev/socket" {
        return (-(30i64)) as u64;
    }
    let path_bytes = path.as_bytes();
    unsafe {
        for socket in (*core::ptr::addr_of_mut!(LINUX_SOCKET_SLOTS)).iter_mut() {
            if socket.active
                && socket.bound_length == path_bytes.len()
                && socket.bound_path[..socket.bound_length] == *path_bytes
            {
                socket.bound_length = 0;
                socket.listening = false;
                socket.pending = SOCKET_SLOT_NONE;
            }
        }
    }
    0
}

/// Android init mounts several kernel pseudo-filesystems before it starts
/// services. Fullerene owns bounded read-only replacements for those paths.
/// The Android filesystem targets are different: `mount_all` must only succeed
/// for them after the Bramble UFS/LP reader has installed a non-root VFS mount.
pub(crate) fn linux_mount(
    source_address: u64,
    target_address: u64,
    filesystem_address: u64,
    flags: u64,
    _data_address: u64,
) -> u64 {
    let path_storage = match copy_path(target_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    if is_android_filesystem_mount_target(path) {
        let source_storage = match copy_path(source_address) {
            Ok(source) => source,
            Err(error) => return error,
        };
        let source = match source_storage.as_str() {
            Ok(source) => source,
            Err(error) => return error,
        };
        let filesystem_storage = match copy_path(filesystem_address) {
            Ok(filesystem) => filesystem,
            Err(error) => return error,
        };
        let filesystem = match filesystem_storage.as_str() {
            Ok(filesystem) => filesystem,
            Err(error) => return error,
        };
        if !android_mount_request_supported(path, source, filesystem, flags) {
            return ERR_INVALID;
        }
        if android_filesystem_is_mounted(path) {
            0
        } else {
            ERR_NO_ENTRY
        }
    } else if is_virtual_namespace_path(path) {
        0
    } else {
        (-(30i64)) as u64
    }
}

pub(crate) fn linux_umount2(target_address: u64) -> u64 {
    let path_storage = match copy_path(target_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    if is_virtual_namespace_path(path) || is_android_filesystem_mount_target(path) {
        0
    } else {
        ERR_NOT_SUPPORTED
    }
}

pub(crate) fn linux_mknodat(path_address: u64) -> u64 {
    let path_storage = match copy_path(path_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    if path.starts_with("/dev/") {
        0
    } else {
        (-(30i64)) as u64
    }
}

pub(crate) fn linux_chdir(path_address: u64) -> u64 {
    match path_exists(path_address) {
        Ok(true) => 0,
        Ok(false) => ERR_NO_ENTRY,
        Err(error) => error,
    }
}

/// Read the size of a pathname for the small Linux `fstatat` compatibility
/// path. Directories are reported with size zero by this read-only boundary.
pub(crate) fn path_size(path_address: u64) -> Result<u64, u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    let result = with_vfs(|vfs| {
        let (mount_index, file) = vfs.open_with_mount(path, 0).ok_or(ERR_NO_ENTRY)?;
        let size = vfs.size_at(mount_index, file.fd).unwrap_or(0);
        let _ = vfs.close_at(mount_index, file.fd);
        Ok(size)
    });
    result.ok_or(ERR_NO_ENTRY)?
}

pub(crate) fn path_metadata(path_address: u64) -> Result<FileMetadata, u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    let result = with_vfs(|vfs| match vfs.metadata(path) {
        Ok(metadata) => Ok(metadata),
        Err(FsError::NotSupported) => {
            let (mount_index, file) = vfs.open_with_mount(path, 0).ok_or(FsError::FileNotFound)?;
            let size = vfs.size_at(mount_index, file.fd).unwrap_or(0);
            let directory = vfs.readdir(path).is_ok();
            let _ = vfs.close_at(mount_index, file.fd);
            Ok(FileMetadata {
                mode: if directory { 0o040555 } else { 0o100444 },
                uid: 0,
                gid: 0,
                size: if directory { 0 } else { size },
                kind: if directory {
                    InodeType::Directory
                } else {
                    InodeType::File
                },
            })
        }
        Err(error) => Err(error),
    });
    match result {
        Some(Ok(metadata)) => Ok(metadata),
        Some(Err(error)) => Err(fs_error_to_errno(error)),
        None => Err(ERR_NO_ENTRY),
    }
}

/// Implement the descriptor flag subset needed by bionic's early file
/// wrappers. Status flags remain read-only until a write-capable VFS exists;
/// close-on-exec is process-local and can be changed safely.
pub(crate) fn linux_fcntl(fd: u64, command: u64, argument: u64) -> u64 {
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    const FD_CLOEXEC: u64 = 1;
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let fd = match u32::try_from(fd) {
        Ok(fd) => fd,
        Err(_) => return ERR_BAD_FD,
    };
    if fd <= 2 {
        return match command {
            F_GETFD | F_SETFD | F_SETFL | F_GETFL => 0,
            _ => ERR_INVALID,
        };
    }
    let Some((index, entry)) = linux_fd_entry(owner_pid, fd) else {
        return ERR_BAD_FD;
    };
    match command {
        F_GETFD => u64::from((entry.flags & LINUX_FD_CLOEXEC) != 0) * FD_CLOEXEC,
        F_SETFD => {
            let flags = if argument & FD_CLOEXEC != 0 {
                entry.flags | LINUX_FD_CLOEXEC
            } else {
                entry.flags & !LINUX_FD_CLOEXEC
            };
            unsafe {
                (*core::ptr::addr_of_mut!(LINUX_FDS))[index].flags = flags;
            }
            0
        }
        F_GETFL => u64::from(entry.flags & !LINUX_FD_CLOEXEC),
        F_SETFL => 0,
        _ => ERR_INVALID,
    }
}

/// Read a final symlink target without resolving that component. The Linux
/// wrapper copies the returned bytes to the caller and deliberately does not
/// append a NUL, matching `readlinkat(2)`.
pub(crate) fn read_link_path(path_address: u64, destination: &mut [u8]) -> Result<usize, u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    let target = with_vfs(|vfs| vfs.read_link(path));
    let target = match target {
        Some(Ok(target)) => target,
        Some(Err(_)) => return Err(ERR_NO_ENTRY),
        None => return Err(ERR_NO_ENTRY),
    };
    let length = target.len().min(destination.len());
    destination[..length].copy_from_slice(&target.as_bytes()[..length]);
    Ok(length)
}

fn read_pipe(pipe_slot: u8, buffer_address: u64, requested: usize) -> u64 {
    let mut buffer = [0u8; MAX_READ];
    let bytes_read = unsafe {
        let Some(pipe) = (*core::ptr::addr_of_mut!(PIPE_SLOTS)).get_mut(pipe_slot as usize) else {
            return ERR_BAD_FD;
        };
        if !pipe.active {
            return ERR_BAD_FD;
        }
        if pipe.length == 0 {
            return ERR_WOULD_BLOCK;
        }
        let count = requested.min(pipe.length);
        for (index, byte) in buffer[..count].iter_mut().enumerate() {
            *byte = pipe.buffer[(pipe.read_position + index) % PIPE_CAPACITY];
        }
        pipe.read_position = (pipe.read_position + count) % PIPE_CAPACITY;
        pipe.length -= count;
        count
    };
    if user_memory::copy_to_user(buffer_address, &buffer[..bytes_read]).is_err() {
        return ERR_ADDRESS;
    }
    bytes_read as u64
}

fn write_pipe(pipe_slot: u8, buffer: &[u8]) -> u64 {
    unsafe {
        let Some(pipe) = (*core::ptr::addr_of_mut!(PIPE_SLOTS)).get_mut(pipe_slot as usize) else {
            return ERR_BAD_FD;
        };
        if !pipe.active {
            return ERR_BAD_FD;
        }
        let available = PIPE_CAPACITY - pipe.length;
        if available == 0 {
            return ERR_WOULD_BLOCK;
        }
        let count = buffer.len().min(available);
        for (index, byte) in buffer[..count].iter().enumerate() {
            pipe.buffer[(pipe.write_position + index) % PIPE_CAPACITY] = *byte;
        }
        pipe.write_position = (pipe.write_position + count) % PIPE_CAPACITY;
        pipe.length += count;
        count as u64
    }
}

fn release_resource(kind: u8, slot: u8) {
    let mut release_shared = false;
    unsafe {
        match kind {
            KIND_PIPE_READ | KIND_PIPE_WRITE => {
                let Some(pipe) = (*core::ptr::addr_of_mut!(PIPE_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                pipe.references = pipe.references.saturating_sub(1);
                if pipe.references == 0 {
                    *pipe = PipeSlot::EMPTY;
                }
            }
            KIND_CHANNEL => {
                let Some(channel) =
                    (*core::ptr::addr_of_mut!(CHANNEL_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                channel.references = channel.references.saturating_sub(1);
                if channel.references == 0 {
                    *channel = ChannelSlot::EMPTY;
                }
            }
            KIND_EVENT => {
                let Some(event) = (*core::ptr::addr_of_mut!(EVENT_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                event.references = event.references.saturating_sub(1);
                if event.references == 0 {
                    *event = EventSlot::EMPTY;
                }
            }
            KIND_SHARED_BUFFER => {
                let Some(buffer) =
                    (*core::ptr::addr_of_mut!(SHARED_BUFFER_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                buffer.references = buffer.references.saturating_sub(1);
                if buffer.references == 0 {
                    release_shared = true;
                }
            }
            KIND_TIMER => {
                let Some(timer) = (*core::ptr::addr_of_mut!(TIMER_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                timer.references = timer.references.saturating_sub(1);
                if timer.references == 0 {
                    *timer = TimerSlot::EMPTY;
                }
            }
            KIND_THREAD => {
                let Some(thread) = (*core::ptr::addr_of_mut!(THREAD_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                thread.references = thread.references.saturating_sub(1);
                let target_pid = thread.target_pid;
                let exited = thread.exited;
                if thread.references == 0 {
                    thread.detached = true;
                    let _ = task::mark_thread_detached(target_pid);
                    if exited {
                        let _ = task::reap_thread_if_exited(target_pid);
                        *thread = ThreadSlot::EMPTY;
                    }
                }
            }
            KIND_TERMINAL => {
                let Some(terminal) =
                    (*core::ptr::addr_of_mut!(TERMINAL_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                terminal.references = terminal.references.saturating_sub(1);
                if terminal.references == 0 {
                    *terminal = TerminalSlot::EMPTY;
                }
            }
            KIND_DIRECTORY => {
                let Some(directory) =
                    (*core::ptr::addr_of_mut!(DIRECTORY_SLOTS)).get_mut(slot as usize)
                else {
                    return;
                };
                directory.references = directory.references.saturating_sub(1);
                if directory.references == 0 {
                    *directory = DirectorySlot::EMPTY;
                }
            }
            KIND_DEVICE => devices::release(slot),
            KIND_WINDOW => window::release(slot),
            _ => {}
        }
    }
    if release_shared {
        release_shared_buffer_slot(slot);
    }
}

pub(crate) fn close(handle: u64) -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    close_for_owner(owner_pid, handle)
}

fn close_for_owner(owner_pid: u64, handle: u64) -> u64 {
    let Some((storage_index, file)) = locate_entry_for_owner(owner_pid, handle) else {
        return ERR_BAD_FD;
    };
    if file.kind == KIND_FILE || file.kind == KIND_DIRECTORY {
        let last_reference = unsafe {
            (*core::ptr::addr_of!(OPEN_FILES))
                .iter()
                .enumerate()
                .all(|(index, other)| {
                    index == storage_index
                        || !other.active
                        || other.kind != file.kind
                        || other.local_fd != file.local_fd
                        || other.mount_index != file.mount_index
                })
        };
        if last_reference
            && with_vfs(|vfs| vfs.close_at(file.mount_index, file.local_fd).is_ok()) != Some(true)
        {
            return ERR_BAD_FD;
        }
        if file.kind == KIND_DIRECTORY {
            release_resource(file.kind, file.pipe_slot);
        }
    } else if file.kind == KIND_PIPE_READ
        || file.kind == KIND_PIPE_WRITE
        || file.kind == KIND_CHANNEL
        || file.kind == KIND_EVENT
        || file.kind == KIND_TIMER
        || file.kind == KIND_THREAD
        || file.kind == KIND_TERMINAL
        || file.kind == KIND_DEVICE
        || file.kind == KIND_WINDOW
    {
        release_resource(file.kind, file.pipe_slot);
    } else if file.kind == KIND_SHARED_BUFFER {
        if shared_buffer_has_any_mapping(file.pipe_slot) {
            return (-(16i64)) as u64;
        }
        release_resource(file.kind, file.pipe_slot);
    } else {
        return ERR_BAD_FD;
    }
    unsafe {
        (*core::ptr::addr_of_mut!(OPEN_FILES))[storage_index] = OpenFile::EMPTY;
    }
    0
}

/// Duplicate a parent's open-handle view for a forked child. The logical
/// handle value is preserved, while both rows point at the same VFS open-file
/// description so the file offset follows the usual fork semantics.
pub(crate) fn inherit_fds(parent_pid: u64, child_pid: u64) -> bool {
    let Some(linux_count) = linux_inherit_capacity(parent_pid) else {
        return false;
    };
    let mut inherited = [OpenFile::EMPTY; MAX_OPEN_FILES];
    let mut inherited_count = 0usize;
    unsafe {
        let files = core::ptr::addr_of!(OPEN_FILES);
        for file in (*files).iter().copied() {
            if file.active && file.owner_pid == parent_pid {
                if inherited_count == inherited.len() {
                    return false;
                }
                inherited[inherited_count] = OpenFile {
                    owner_pid: child_pid,
                    ..file
                };
                inherited_count += 1;
            }
        }
        let storage_free = (*files).iter().filter(|file| !file.active).count();
        if storage_free < inherited_count {
            return false;
        }
        let files = core::ptr::addr_of_mut!(OPEN_FILES);
        let mut copied = 0usize;
        for file in (*files).iter_mut() {
            if !file.active {
                *file = inherited[copied];
                copied += 1;
                if copied == inherited_count {
                    break;
                }
            }
        }
        for file in inherited.iter().take(inherited_count) {
            if file.kind != KIND_FILE {
                let _ = retain_resource(file.kind, file.pipe_slot);
            }
        }
    }
    linux_inherit_fds_unchecked(parent_pid, child_pid, linux_count);
    true
}

/// Attach one terminal endpoint to a newly spawned child and return the
/// child's fresh logical handle. The source remains owned by the parent; the
/// endpoint is released only after both owners close or exit.
pub(crate) fn inherit_terminal_handle(
    parent_pid: u64,
    child_pid: u64,
    handle: u64,
) -> Result<u64, u64> {
    let Some((_, source)) = locate_entry_for_owner(parent_pid, handle) else {
        return Err(ERR_BAD_FD);
    };
    if source.kind != KIND_TERMINAL {
        return Err(ERR_PERMISSION);
    }
    install_handle(child_pid, 0, 0, KIND_TERMINAL, source.pipe_slot)
}

/// Roll back descriptor rows after a failed fork installation.
pub(crate) fn drop_owner(owner_pid: u64) {
    cleanup_shared_mappings(owner_pid);
    linux_drop_owner(owner_pid);
    let mut local_files = [(0usize, 0u32); MAX_OPEN_FILES];
    let mut local_count = 0usize;
    let mut resource_kinds = [KIND_FILE; MAX_OPEN_FILES];
    let mut pipe_slots = [0u8; MAX_OPEN_FILES];
    let mut resource_count = 0usize;
    unsafe {
        let files = core::ptr::addr_of_mut!(OPEN_FILES);
        for file in (*files).iter_mut() {
            if file.active && file.owner_pid == owner_pid {
                if file.kind == KIND_FILE || file.kind == KIND_DIRECTORY {
                    local_files[local_count] = (file.mount_index, file.local_fd);
                    local_count += 1;
                    if file.kind == KIND_DIRECTORY {
                        resource_kinds[resource_count] = file.kind;
                        pipe_slots[resource_count] = file.pipe_slot;
                        resource_count += 1;
                    }
                } else {
                    resource_kinds[resource_count] = file.kind;
                    pipe_slots[resource_count] = file.pipe_slot;
                    resource_count += 1;
                }
                *file = OpenFile::EMPTY;
            }
        }
    }
    for (kind, slot) in resource_kinds
        .iter()
        .copied()
        .zip(pipe_slots.iter().copied())
        .take(resource_count)
    {
        release_resource(kind, slot);
    }
    for (mount_index, local_fd) in local_files.iter().copied().take(local_count) {
        let still_open = unsafe {
            (*core::ptr::addr_of!(OPEN_FILES)).iter().any(|file| {
                file.active
                    && (file.kind == KIND_FILE || file.kind == KIND_DIRECTORY)
                    && file.mount_index == mount_index
                    && file.local_fd == local_fd
            })
        };
        if !still_open {
            let _ = with_vfs(|vfs| vfs.close_at(mount_index, local_fd));
        }
    }
}

fn linux_drop_owner(owner_pid: u64) {
    let mut native_handles = [0u64; MAX_LINUX_FDS_PER_PROCESS];
    let mut native_count = 0usize;
    let mut socket_slots = [SOCKET_SLOT_NONE; MAX_LINUX_FDS_PER_PROCESS];
    let mut socket_count = 0usize;
    let mut epoll_slots = [EPOLL_SLOT_NONE; MAX_LINUX_FDS_PER_PROCESS];
    let mut epoll_count = 0usize;
    let mut eventfd_slots = [SOCKET_SLOT_NONE; MAX_LINUX_FDS_PER_PROCESS];
    let mut eventfd_count = 0usize;
    let mut inotify_slots = [SOCKET_SLOT_NONE; MAX_LINUX_FDS_PER_PROCESS];
    let mut inotify_count = 0usize;
    let mut signalfd_slots = [SOCKET_SLOT_NONE; MAX_LINUX_FDS_PER_PROCESS];
    let mut signalfd_count = 0usize;
    unsafe {
        for entry in (*core::ptr::addr_of_mut!(LINUX_FDS)).iter_mut() {
            if !entry.active || entry.owner_pid != owner_pid {
                continue;
            }
            if entry.kind == LINUX_FD_SOCKET && socket_count < socket_slots.len() {
                socket_slots[socket_count] = entry.resource_slot;
                socket_count += 1;
            } else if entry.kind == LINUX_FD_EPOLL && epoll_count < epoll_slots.len() {
                epoll_slots[epoll_count] = entry.resource_slot;
                epoll_count += 1;
            } else if entry.kind == LINUX_FD_EVENTFD && eventfd_count < eventfd_slots.len() {
                eventfd_slots[eventfd_count] = entry.resource_slot;
                eventfd_count += 1;
            } else if entry.kind == LINUX_FD_INOTIFY && inotify_count < inotify_slots.len() {
                inotify_slots[inotify_count] = entry.resource_slot;
                inotify_count += 1;
            } else if entry.kind == LINUX_FD_SIGNALFD && signalfd_count < signalfd_slots.len() {
                signalfd_slots[signalfd_count] = entry.resource_slot;
                signalfd_count += 1;
            } else if entry.kind == LINUX_FD_NATIVE
                && entry.stdio_fd == LINUX_STDIO_NONE
                && native_count < native_handles.len()
            {
                native_handles[native_count] = entry.native_handle;
                native_count += 1;
            }
            *entry = LinuxFdEntry::EMPTY;
        }
    }
    for native_handle in native_handles.iter().copied().take(native_count) {
        let _ = close_for_owner(owner_pid, native_handle);
    }
    for socket_slot in socket_slots.iter().copied().take(socket_count) {
        linux_socket_release(socket_slot);
    }
    for epoll_slot in epoll_slots.iter().copied().take(epoll_count) {
        linux_epoll_release(epoll_slot);
    }
    for eventfd_slot in eventfd_slots.iter().copied().take(eventfd_count) {
        linux_eventfd_release(eventfd_slot);
    }
    for inotify_slot in inotify_slots.iter().copied().take(inotify_count) {
        linux_inotify_release(inotify_slot);
    }
    for signalfd_slot in signalfd_slots.iter().copied().take(signalfd_count) {
        linux_signalfd_release(signalfd_slot);
    }
}

/// Read one executable directly from the VFS without creating a user-owned
/// descriptor. This is the pathname half of the bounded AArch64 `EXEC_PATH`
/// boundary; the caller supplies a fixed staging buffer and receives the
/// basename used as the replacement process name.
pub(crate) fn read_path(
    path_address: u64,
    destination: &mut [u8],
) -> Result<(usize, [u8; MAX_TASK_NAME], usize), u64> {
    let path_storage = copy_path(path_address)?;
    let path = path_storage.as_str()?;
    let name = path.rsplit('/').next().ok_or(ERR_INVALID)?;
    if name.is_empty() {
        return Err(ERR_INVALID);
    }
    let name_length = name.len().min(MAX_TASK_NAME);
    let mut process_name = [0u8; MAX_TASK_NAME];
    process_name[..name_length].copy_from_slice(&name.as_bytes()[..name_length]);

    let length = read_vfs_path(path, destination)?;
    Ok((length, process_name, name_length))
}

/// Read a pathname from the kernel-owned VFS without crossing an EL0 pointer.
/// The bootstrap launchd uses this same path as `EXEC_PATH`, so the first
/// user process is sourced from the selected VFS route: the initramfs on
/// QEMU, or a more-specific physical Android mount on Bramble.
pub(super) fn read_kernel_path(path: &str, destination: &mut [u8]) -> Result<usize, u64> {
    read_vfs_path(path, destination)
}

/// Report whether Android's physical `/system` filesystem currently owns the
/// path namespace.  The initramfs remains the VFS root, so a successful
/// Bramble mount adds a more-specific `/system` route rather than replacing
/// the root object.  Launchd uses this only for an observable source marker;
/// path resolution itself remains the authority for the executable bytes.
pub(super) fn android_system_mounted() -> bool {
    with_vfs(|vfs| {
        vfs.mounted_fs_index("/system")
            .is_some_and(|mount_index| mount_index != 0)
    })
    .unwrap_or(false)
}

fn read_vfs_path(path: &str, destination: &mut [u8]) -> Result<usize, u64> {
    let read_result = with_vfs(|vfs| {
        let (mount_index, local_file) = vfs.open_with_mount(path, 0).ok_or(ERR_NO_ENTRY)?;
        let local_fd = local_file.fd;
        let result = (|| {
            let mut total = 0usize;
            while total < destination.len() {
                let count = vfs
                    .read_at(mount_index, local_fd, &mut destination[total..])
                    .map_err(|_| ERR_INVALID)?;
                if count == 0 {
                    return Ok(total);
                }
                total = total.checked_add(count).ok_or(ERR_OVERFLOW)?;
            }
            let mut extra = [0u8; 1];
            let count = vfs
                .read_at(mount_index, local_fd, &mut extra)
                .map_err(|_| ERR_INVALID)?;
            if count != 0 {
                return Err(ERR_OVERFLOW);
            }
            Ok(total)
        })();
        let _ = vfs.close_at(mount_index, local_fd);
        result
    });
    match read_result {
        Some(Ok(length)) => Ok(length),
        Some(Err(error)) => Err(error),
        None => Err(ERR_NO_ENTRY),
    }
}

fn locate_entry(handle: u64) -> Option<(usize, OpenFile)> {
    let owner_pid = task::resource_owner_pid()?;
    locate_entry_for_owner(owner_pid, handle)
}

pub(crate) fn device_slot(handle: u64) -> Option<u8> {
    locate_entry(handle).and_then(|(_, file)| (file.kind == KIND_DEVICE).then_some(file.pipe_slot))
}

pub(crate) fn window_slot(handle: u64) -> Option<u8> {
    locate_entry(handle).and_then(|(_, file)| (file.kind == KIND_WINDOW).then_some(file.pipe_slot))
}

fn locate_entry_for_owner(owner_pid: u64, handle: u64) -> Option<(usize, OpenFile)> {
    if handle & FILE_HANDLE_TAG == 0 {
        return None;
    }
    let handle_index = (handle & FILE_HANDLE_INDEX_MASK) as u16;
    if handle_index as usize >= MAX_HANDLE_SLOTS {
        return None;
    }
    let generation = (handle & !FILE_HANDLE_TAG) >> FILE_HANDLE_GENERATION_SHIFT;
    unsafe {
        (*core::ptr::addr_of!(OPEN_FILES))
            .iter()
            .enumerate()
            .find(|(_, file)| {
                file.active
                    && file.owner_pid == owner_pid
                    && file.handle_index == handle_index
                    && file.generation == generation
            })
            .map(|(index, file)| (index, *file))
    }
}

fn copy_path(address: u64) -> Result<UserPath, u64> {
    if address == 0 {
        return Err(ERR_ADDRESS);
    }
    let mut path = UserPath {
        bytes: [0; MAX_PATH],
        length: 0,
    };
    for offset in 0..MAX_PATH {
        let mut byte = [0u8; 1];
        user_memory::copy_from_user(
            address.checked_add(offset as u64).ok_or(ERR_ADDRESS)?,
            &mut byte,
        )
        .map_err(|_| ERR_ADDRESS)?;
        if byte[0] == 0 {
            return Ok(path);
        }
        path.bytes[offset] = byte[0];
        path.length += 1;
    }
    Err(ERR_NAME_TOO_LONG)
}

#[cfg(test)]
mod tests {
    use super::{SelinuxFs, android_mount_request_supported};

    #[test]
    fn android_mount_request_accepts_read_only_partition_contract() {
        assert!(android_mount_request_supported(
            "/system",
            "/dev/block/by-name/system",
            "ext4",
            1,
        ));
        assert!(android_mount_request_supported(
            "/vendor",
            "/dev/block/by-name/vendor",
            "erofs",
            1,
        ));
        assert!(android_mount_request_supported(
            "/data",
            "/dev/block/by-name/userdata",
            "f2fs",
            1,
        ));
    }

    #[test]
    fn android_mount_request_rejects_wrong_source_type_or_writable_flags() {
        assert!(!android_mount_request_supported(
            "/system",
            "/dev/block/by-name/vendor",
            "ext4",
            1,
        ));
        assert!(!android_mount_request_supported(
            "/data",
            "/dev/block/by-name/userdata",
            "ext4",
            1,
        ));
        assert!(!android_mount_request_supported(
            "/vendor",
            "/dev/block/by-name/vendor",
            "erofs",
            0,
        ));
    }

    #[test]
    fn selinux_policy_wire_format_loads_transition_rules() {
        let mut filesystem = SelinuxFs::new();
        let policy = b"FSP1\x01\x00\x01\x00\x0b\x11u:r:init:s0u:r:fullerened:s0";
        filesystem.policy_buffer[..policy.len()].copy_from_slice(policy);
        filesystem.policy_buffer_length = policy.len();
        assert!(filesystem.parse_policy().is_ok());
        assert!(filesystem.policy_loaded);
        assert_eq!(filesystem.policy_rule_count, 1);
        let rule = filesystem.policy_rules[0];
        assert_eq!(&rule.source[..rule.source_length], b"u:r:init:s0");
        assert_eq!(&rule.target[..rule.target_length], b"u:r:fullerened:s0");
    }

    #[test]
    fn selinux_policy_fsp2_loads_exact_path_allow_rules() {
        let mut filesystem = SelinuxFs::new();
        let policy = b"FSP2\x01\x00\x01\x00\x01\x00\x0b\x11u:r:init:s0u:r:fullerened:s0\x0b\x17\x02\x00u:r:init:s0/sys/fs/selinux/enforce";
        filesystem.policy_buffer[..policy.len()].copy_from_slice(policy);
        filesystem.policy_buffer_length = policy.len();
        assert!(filesystem.parse_policy().is_ok());
        assert!(filesystem.policy_loaded);
        assert_eq!(filesystem.policy_rule_count, 1);
        assert_eq!(filesystem.policy_allow_rule_count, 1);
        let rule = filesystem.policy_allow_rules[0];
        assert_eq!(&rule.source[..rule.source_length], b"u:r:init:s0");
        assert_eq!(
            &rule.object[..rule.object_length],
            b"/sys/fs/selinux/enforce"
        );
        assert_eq!(rule.permissions, SELINUX_ACCESS_WRITE);
    }

    #[test]
    fn selinux_policy_wire_format_rejects_trailing_bytes() {
        let mut filesystem = SelinuxFs::new();
        let policy = b"FSP1\x01\x00\x01\x00\x0b\x11u:r:init:s0u:r:fullerened:s0x";
        filesystem.policy_buffer[..policy.len()].copy_from_slice(policy);
        filesystem.policy_buffer_length = policy.len();
        assert!(filesystem.parse_policy().is_err());
        assert!(!filesystem.policy_loaded);
    }

    #[test]
    fn selinux_userdebug_adbd_to_su_transition_is_bounded() {
        let mut policy = SelinuxPolicyState::EMPTY;
        policy.enforcing = true;
        policy.loaded = true;
        policy.rule_count = 1;
        let rule = &mut policy.rules[0];
        rule.source[..11].copy_from_slice(b"u:r:adbd:s0");
        rule.source_length = 11;
        rule.target[..10].copy_from_slice(b"u:r:su:s0");
        rule.target_length = 10;
        rule.active = true;

        assert!(selinux_transition_allowed_in_policy(
            &policy,
            b"u:r:adbd:s0",
            b"u:r:su:s0"
        ));
        assert!(!selinux_transition_allowed_in_policy(
            &policy,
            b"u:r:shell:s0",
            b"u:r:su:s0"
        ));
    }
}

fn with_vfs<F, R>(function: F) -> Option<R>
where
    F: FnOnce(&mut Vfs) -> R,
{
    unsafe {
        (*core::ptr::addr_of!(VFS)).as_ref().map(|vfs| {
            let mut guard = vfs.lock();
            function(&mut guard)
        })
    }
}
