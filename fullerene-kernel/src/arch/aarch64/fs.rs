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

use super::{task, uart, user_memory};

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
const MAX_TASK_NAME: usize = 16;
const CPIO_HEADER_SIZE: usize = 110;
const MAX_INITRAMFS_ENTRIES: usize = 32;
const KIND_FILE: u8 = 0;
const KIND_PIPE_READ: u8 = 1;
const KIND_PIPE_WRITE: u8 = 2;
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

static mut VFS: Option<Mutex<Vfs>> = None;
static mut OPEN_FILES: [OpenFile; MAX_OPEN_FILES] = [OpenFile::EMPTY; MAX_OPEN_FILES];
static mut PIPE_SLOTS: [PipeSlot; MAX_PIPE_SLOTS] = [PipeSlot::EMPTY; MAX_PIPE_SLOTS];
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
        let slot = pipe_slot as usize;
        let valid = unsafe {
            if let Some(pipe) = (*core::ptr::addr_of_mut!(PIPE_SLOTS)).get_mut(slot) {
                if pipe.active {
                    pipe.references = pipe.references.saturating_add(1);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
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

pub(crate) fn open(path_address: u64, flags: u64, _mode: u64) -> u64 {
    let path_storage = match copy_path(path_address) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let path = match path_storage.as_str() {
        Ok(path) => path,
        Err(error) => return error,
    };
    let owner_pid = match task::current_pid() {
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

pub(crate) fn pipe_create(buffer_address: u64) -> u64 {
    if buffer_address == 0 {
        return ERR_ADDRESS;
    }
    let Some(owner_pid) = task::current_pid() else {
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

pub(crate) fn duplicate(handle: u64) -> u64 {
    let Some((_, source)) = locate_entry(handle) else {
        return ERR_BAD_FD;
    };
    let Some(owner_pid) = task::current_pid() else {
        return ERR_BAD_FD;
    };
    match install_handle(owner_pid, source.local_fd, source.kind, source.pipe_slot) {
        Ok(handle) => handle,
        Err(error) => error,
    }
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

fn release_pipe_reference(pipe_slot: u8) {
    unsafe {
        let Some(pipe) = (*core::ptr::addr_of_mut!(PIPE_SLOTS)).get_mut(pipe_slot as usize) else {
            return;
        };
        pipe.references = pipe.references.saturating_sub(1);
        if pipe.references == 0 {
            *pipe = PipeSlot::EMPTY;
        }
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
    } else if file.kind == KIND_PIPE_READ || file.kind == KIND_PIPE_WRITE {
        release_pipe_reference(file.pipe_slot);
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
                if let Some(pipe) =
                    (*core::ptr::addr_of_mut!(PIPE_SLOTS)).get_mut(file.pipe_slot as usize)
                {
                    pipe.references = pipe.references.saturating_add(1);
                }
            }
        }
    }
    true
}

/// Roll back descriptor rows after a failed fork installation.
pub(crate) fn drop_owner(owner_pid: u64) {
    let mut local_fds = [0u32; MAX_OPEN_FILES];
    let mut local_count = 0usize;
    let mut pipe_slots = [0u8; MAX_OPEN_FILES];
    let mut pipe_count = 0usize;
    unsafe {
        let files = core::ptr::addr_of_mut!(OPEN_FILES);
        for file in (*files).iter_mut() {
            if file.active && file.owner_pid == owner_pid {
                if file.kind == KIND_FILE {
                    local_fds[local_count] = file.local_fd;
                    local_count += 1;
                } else if file.kind == KIND_PIPE_READ || file.kind == KIND_PIPE_WRITE {
                    pipe_slots[pipe_count] = file.pipe_slot;
                    pipe_count += 1;
                }
                *file = OpenFile::EMPTY;
            }
        }
    }
    for pipe_slot in pipe_slots.iter().copied().take(pipe_count) {
        release_pipe_reference(pipe_slot);
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
    if handle & FILE_HANDLE_TAG == 0 {
        return None;
    }
    let handle_index = (handle & FILE_HANDLE_INDEX_MASK) as u16;
    if handle_index as usize >= MAX_HANDLE_SLOTS {
        return None;
    }
    let generation = (handle & !FILE_HANDLE_TAG) >> FILE_HANDLE_GENERATION_SHIFT;
    let owner_pid = task::current_pid()?;
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
