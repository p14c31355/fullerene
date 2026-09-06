//! Bounded AArch64 native filesystem boundary.
//!
//! This is the first storage layer used by the native launchd payload. It
//! starts with Genome's in-memory VFS, but the syscall contract is real:
//! paths and buffers cross the EL0 copy boundary, descriptors are owned by
//! the current PID, and the backing VFS can later be replaced by an
//! initramfs/FAT mount without changing OPEN/READ/CLOSE dispatch.

use alloc::boxed::Box;
use genome::vfs::{MemFileSystem, Vfs};
use spin::Mutex;

use super::{
    allocator, devices, exceptions::Aarch64TrapFrame, mmu, task, uart, user_memory, window,
};

const ERR_BAD_FD: u64 = (-(9i64)) as u64;
const ERR_ADDRESS: u64 = (-(14i64)) as u64;
const ERR_NO_ENTRY: u64 = (-(2i64)) as u64;
const ERR_WOULD_BLOCK: u64 = (-(11i64)) as u64;
const ERR_PERMISSION: u64 = (-(13i64)) as u64;
const ERR_INVALID: u64 = (-(22i64)) as u64;
const ERR_OVERFLOW: u64 = (-(75i64)) as u64;
const ERR_NAME_TOO_LONG: u64 = (-(36i64)) as u64;
const ERR_OUT_OF_MEMORY: u64 = (-(12i64)) as u64;
const FILE_HANDLE_TAG: u64 = 1 << 62;
const FILE_HANDLE_INDEX_BITS: u64 = 8;
const FILE_HANDLE_INDEX_MASK: u64 = (1 << FILE_HANDLE_INDEX_BITS) - 1;
const FILE_HANDLE_GENERATION_SHIFT: u64 = FILE_HANDLE_INDEX_BITS;
const MAX_OPEN_FILES: usize = 32;
const MAX_HANDLE_SLOTS: usize = 16;
const MAX_PATH: usize = 256;
const MAX_READ: usize = 4096;
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
const MAX_TERMINAL_SLOTS: usize = 8;
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
static INITRAMFS: &[u8] = include_bytes!(env!("FULLERENE_AARCH64_INITRAMFS"));

#[derive(Clone, Copy)]
struct OpenFile {
    owner_pid: u64,
    handle_index: u16,
    local_fd: u32,
    generation: u64,
    kind: u8,
    pipe_slot: u8,
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

impl OpenFile {
    const EMPTY: Self = Self {
        owner_pid: 0,
        handle_index: 0,
        local_fd: 0,
        generation: 0,
        kind: KIND_FILE,
        pipe_slot: 0,
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
static mut TERMINAL_SLOTS: [TerminalSlot; MAX_TERMINAL_SLOTS] =
    [TerminalSlot::EMPTY; MAX_TERMINAL_SLOTS];
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

fn install_handle(owner_pid: u64, local_fd: u32, kind: u8, pipe_slot: u8) -> Result<u64, u64> {
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
            generation,
            kind,
            pipe_slot,
            active: true,
        };
        generation
    };
    Ok(FILE_HANDLE_TAG | (generation << FILE_HANDLE_GENERATION_SHIFT) | handle_index as u64)
}

pub(crate) fn install_device_handle(owner_pid: u64, device_slot: u8) -> Result<u64, u64> {
    install_handle(owner_pid, 0, KIND_DEVICE, device_slot)
}

pub(crate) fn install_window_handle(owner_pid: u64, window_slot: u8) -> Result<u64, u64> {
    install_handle(owner_pid, 0, KIND_WINDOW, window_slot)
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
    let owner_pid = match task::resource_owner_pid() {
        Some(pid) => pid,
        None => return ERR_BAD_FD,
    };
    let Some(local_fd) = with_vfs(|vfs| vfs.open(path, flags as u32).map(|file| file.fd)).flatten()
    else {
        return ERR_NO_ENTRY;
    };

    match install_handle(owner_pid, local_fd, KIND_FILE, 0) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = with_vfs(|vfs| vfs.close_at(0, local_fd));
            error
        }
    }
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
    match install_handle(owner_pid, 0, KIND_TERMINAL, slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe {
                (*core::ptr::addr_of_mut!(TERMINAL_SLOTS))[slot] = TerminalSlot::EMPTY;
            }
            error
        }
    }
}

pub(crate) fn pipe_create(buffer_address: u64) -> u64 {
    if buffer_address == 0 {
        return ERR_ADDRESS;
    }
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let Some(pipe_slot) = (0..MAX_PIPE_SLOTS)
        .find(|&index| unsafe { !(*core::ptr::addr_of!(PIPE_SLOTS))[index].active })
    else {
        return ERR_OUT_OF_MEMORY;
    };
    unsafe {
        (*core::ptr::addr_of_mut!(PIPE_SLOTS))[pipe_slot] = PipeSlot {
            active: true,
            ..PipeSlot::EMPTY
        };
    }

    let read_handle = match install_handle(owner_pid, 0, KIND_PIPE_READ, pipe_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            unsafe { (*core::ptr::addr_of_mut!(PIPE_SLOTS))[pipe_slot] = PipeSlot::EMPTY };
            return error;
        }
    };
    let write_handle = match install_handle(owner_pid, 0, KIND_PIPE_WRITE, pipe_slot as u8) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = close(read_handle);
            unsafe { (*core::ptr::addr_of_mut!(PIPE_SLOTS))[pipe_slot] = PipeSlot::EMPTY };
            return error;
        }
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
    match install_handle(owner_pid, 0, KIND_CHANNEL, channel_slot as u8) {
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

pub(crate) fn cleanup_shared_mappings(owner_pid: u64) {
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
    true
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
    match install_handle(owner_pid, 0, KIND_SHARED_BUFFER, buffer_slot as u8) {
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
    match install_handle(owner_pid, 0, KIND_EVENT, event_slot as u8) {
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
    match install_handle(owner_pid, 0, KIND_TIMER, timer_slot as u8) {
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
    match install_handle(owner_pid, 0, KIND_THREAD, thread_slot as u8) {
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
    match install_handle(owner_pid, source.local_fd, source.kind, source.pipe_slot) {
        Ok(handle) => handle,
        Err(error) => error,
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
    let mut buffer = [0u8; MAX_READ];
    let Some(bytes_read) =
        with_vfs(|vfs| vfs.read_at(0, local_fd, &mut buffer[..count]).ok()).flatten()
    else {
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
    let Some(bytes_written) =
        with_vfs(|vfs| vfs.write_at(0, local_fd, &buffer[..count]).ok()).flatten()
    else {
        return ERR_BAD_FD;
    };
    bytes_written as u64
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
    let Some((storage_index, file)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    if file.kind == KIND_FILE {
        let last_reference = unsafe {
            (*core::ptr::addr_of!(OPEN_FILES))
                .iter()
                .enumerate()
                .all(|(index, other)| {
                    index == storage_index
                        || !other.active
                        || other.kind != KIND_FILE
                        || other.local_fd != file.local_fd
                })
        };
        if last_reference && with_vfs(|vfs| vfs.close_at(0, file.local_fd).is_ok()) != Some(true) {
            return ERR_BAD_FD;
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
    install_handle(child_pid, 0, KIND_TERMINAL, source.pipe_slot)
}

/// Roll back descriptor rows after a failed fork installation.
pub(crate) fn drop_owner(owner_pid: u64) {
    cleanup_shared_mappings(owner_pid);
    let mut local_fds = [0u32; MAX_OPEN_FILES];
    let mut local_count = 0usize;
    let mut resource_kinds = [KIND_FILE; MAX_OPEN_FILES];
    let mut pipe_slots = [0u8; MAX_OPEN_FILES];
    let mut resource_count = 0usize;
    unsafe {
        let files = core::ptr::addr_of_mut!(OPEN_FILES);
        for file in (*files).iter_mut() {
            if file.active && file.owner_pid == owner_pid {
                if file.kind == KIND_FILE {
                    local_fds[local_count] = file.local_fd;
                    local_count += 1;
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
    for local_fd in local_fds.iter().copied().take(local_count) {
        let still_open = unsafe {
            (*core::ptr::addr_of!(OPEN_FILES))
                .iter()
                .any(|file| file.active && file.kind == KIND_FILE && file.local_fd == local_fd)
        };
        if !still_open {
            let _ = with_vfs(|vfs| vfs.close_at(0, local_fd));
        }
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
/// user process is sourced from the initramfs rather than a second embedded
/// image-specific exception.
pub(super) fn read_kernel_path(path: &str, destination: &mut [u8]) -> Result<usize, u64> {
    read_vfs_path(path, destination)
}

fn read_vfs_path(path: &str, destination: &mut [u8]) -> Result<usize, u64> {
    let read_result = with_vfs(|vfs| {
        let local_fd = vfs.open(path, 0).ok_or(ERR_NO_ENTRY)?.fd;
        let result = (|| {
            let mut total = 0usize;
            while total < destination.len() {
                let count = vfs
                    .read_at(0, local_fd, &mut destination[total..])
                    .map_err(|_| ERR_INVALID)?;
                if count == 0 {
                    return Ok(total);
                }
                total = total.checked_add(count).ok_or(ERR_OVERFLOW)?;
            }
            let mut extra = [0u8; 1];
            let count = vfs
                .read_at(0, local_fd, &mut extra)
                .map_err(|_| ERR_INVALID)?;
            if count != 0 {
                return Err(ERR_OVERFLOW);
            }
            Ok(total)
        })();
        let _ = vfs.close_at(0, local_fd);
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
