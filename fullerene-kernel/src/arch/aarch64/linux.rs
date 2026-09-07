//! Linux AArch64 syscall personality for the early userspace boundary.
//!
//! Linux syscall numbers overlap the native Fullerene numbers, so they must
//! never be detected from the number alone. [`super::task::AbiPersonality`]
//! selects this table for a task that was explicitly loaded as a Linux image.
//!
//! This is intentionally a small, honest first slice: it covers the process,
//! memory, clock, console, and read-only file operations needed by a static
//! bring-up payload. Unsupported calls return `-ENOSYS`; they do not fall
//! through to the native capability ABI. Android's dynamic linker, complete
//! signal/futex semantics, `/proc`/`/sys`/`/dev`, sockets, and namespaces are
//! still separate compatibility work.

use super::{allocator, exceptions::Aarch64TrapFrame, fs, task, timer, uart, user_memory};

const ENOSYS: u64 = (-(38i64)) as u64;
const EINVAL: u64 = (-(22i64)) as u64;
const EFAULT: u64 = (-(14i64)) as u64;
const ENOMEM: u64 = (-(12i64)) as u64;
const EAGAIN: u64 = (-(11i64)) as u64;
const EBADF: u64 = (-(9i64)) as u64;
const ENOTTY: u64 = (-(25i64)) as u64;
const EROFS: u64 = (-(30i64)) as u64;

const SYS_GETCWD: u64 = 17;
const SYS_IOCTL: u64 = 29;
const SYS_DUP: u64 = 23;
const SYS_DUP3: u64 = 24;
const SYS_OPENAT: u64 = 56;
const SYS_CLOSE: u64 = 57;
const SYS_PIPE2: u64 = 59;
const SYS_LSEEK: u64 = 62;
const SYS_READ: u64 = 63;
const SYS_WRITE: u64 = 64;
const SYS_READLINKAT: u64 = 78;
const SYS_FSTAT: u64 = 80;
const SYS_EXIT: u64 = 93;
const SYS_EXIT_GROUP: u64 = 94;
const SYS_FORK: u64 = 107;
const SYS_SET_TID_ADDRESS: u64 = 96;
const SYS_SET_ROBUST_LIST: u64 = 99;
const SYS_NANOSLEEP: u64 = 101;
const SYS_CLOCK_GETTIME: u64 = 113;
const SYS_SCHED_YIELD: u64 = 124;
const SYS_RT_SIGACTION: u64 = 134;
const SYS_RT_SIGPROCMASK: u64 = 135;
const SYS_UNAME: u64 = 160;
const SYS_PRCTL: u64 = 167;
const SYS_GETPID: u64 = 172;
const SYS_GETPPID: u64 = 173;
const SYS_GETUID: u64 = 174;
const SYS_GETEUID: u64 = 175;
const SYS_GETGID: u64 = 176;
const SYS_GETEGID: u64 = 177;
const SYS_GETTID: u64 = 178;
const SYS_BRK: u64 = 214;
const SYS_MUNMAP: u64 = 215;
const SYS_MMAP: u64 = 222;
const SYS_MPROTECT: u64 = 226;
const SYS_EXECVE: u64 = 221;
const SYS_EXECVEAT: u64 = 281;
const SYS_CLONE: u64 = 220;
const SYS_FUTEX: u64 = 98;
const SYS_FUTEX_TIME64: u64 = 422;
const SYS_RSEQ: u64 = 293;
const SYS_UNLINKAT: u64 = 35;
const SYS_MKDIRAT: u64 = 34;
const SYS_MOUNT: u64 = 40;
const SYS_UMOUNT2: u64 = 39;

const AT_FDCWD: i64 = -100;
const O_ACCMODE: u64 = 0x3;
const O_CREAT: u64 = 0x40;
const O_EXCL: u64 = 0x80;
const O_TRUNC: u64 = 0x200;
const O_APPEND: u64 = 0x400;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_FIXED: u64 = 0x10;
const MAP_PRIVATE: u64 = 0x02;
const MAP_SHARED: u64 = 0x01;
const PROT_MASK: u64 = 0x7;
const LINUX_HEAP_START: u64 = 0x4002_0000;

/// Dispatch one Linux AArch64 SVC. Unknown Linux calls are consumed and
/// return `-ENOSYS`, preserving the selected personality.
pub(super) fn dispatch(frame: &mut Aarch64TrapFrame) -> bool {
    let result = match frame.x[8] {
        SYS_GETCWD => getcwd(frame),
        SYS_IOCTL => ENOTTY,
        SYS_DUP => fs::linux_duplicate(frame.x[0], 0),
        SYS_DUP3 => fs::linux_duplicate_at(frame.x[0], frame.x[1], frame.x[2]),
        SYS_OPENAT => openat(frame),
        SYS_CLOSE => fs::linux_close(frame.x[0]),
        SYS_PIPE2 => ENOSYS,
        SYS_LSEEK => lseek(frame),
        SYS_READ => read(frame),
        SYS_WRITE => write(frame),
        SYS_READLINKAT => ENOSYS,
        SYS_FSTAT => fstat(frame),
        SYS_EXIT | SYS_EXIT_GROUP => return task::exit_syscall(frame, frame.x[0]),
        SYS_SET_TID_ADDRESS => task::current_pid().unwrap_or(ENOSYS),
        SYS_SET_ROBUST_LIST | SYS_RSEQ => 0,
        SYS_NANOSLEEP => return nanosleep(frame),
        SYS_CLOCK_GETTIME => clock_gettime(frame),
        SYS_SCHED_YIELD => return task::yield_syscall(frame),
        SYS_RT_SIGACTION | SYS_RT_SIGPROCMASK => 0,
        SYS_UNAME => uname(frame.x[0]),
        SYS_PRCTL => prctl(frame),
        SYS_GETPID | SYS_GETTID => task::current_pid().unwrap_or(ENOSYS),
        SYS_GETPPID => task::current_parent_pid().unwrap_or(0),
        SYS_GETUID | SYS_GETEUID | SYS_GETGID | SYS_GETEGID => 0,
        SYS_BRK => brk(frame.x[0]),
        SYS_MUNMAP => munmap(frame),
        SYS_MMAP => mmap(frame),
        SYS_MPROTECT => mprotect(frame),
        SYS_EXECVE => super::syscall::linux_exec_path(frame.x[0], frame.x[1], frame.x[2], frame),
        SYS_EXECVEAT => {
            if frame.x[0] as i64 != AT_FDCWD || frame.x[4] != 0 {
                ENOSYS
            } else {
                super::syscall::linux_exec_path(frame.x[1], frame.x[2], frame.x[3], frame)
            }
        }
        SYS_FORK => fork(frame),
        SYS_CLONE => clone(frame),
        SYS_FUTEX | SYS_FUTEX_TIME64 => futex(frame),
        SYS_UNLINKAT | SYS_MKDIRAT | SYS_MOUNT | SYS_UMOUNT2 => EROFS,
        _ => ENOSYS,
    };
    frame.x[0] = result;
    uart::put_hex("aarch64 linux syscall nr=", frame.x[8]);
    uart::put_hex("aarch64 linux syscall ret=", result);
    true
}

fn fork(frame: &mut Aarch64TrapFrame) -> u64 {
    match allocator::with_global(|frames| task::fork(frames, frame)) {
        Some(Ok(pid)) => pid,
        Some(Err(error)) => error,
        None => ENOMEM,
    }
}

fn clone(frame: &mut Aarch64TrapFrame) -> u64 {
    // A separate address space is a safe process clone.  Thread-style
    // CLONE_VM/CLONE_THREAD needs shared TLS, signal, and address-space state
    // that the bounded scheduler does not yet expose, so reject it rather
    // than silently turning a thread into a process.
    const CLONE_VM: u64 = 0x0000_0100;
    const CLONE_THREAD: u64 = 0x0001_0000;
    let flags = frame.x[0];
    if flags & (CLONE_VM | CLONE_THREAD) != 0 {
        return ENOSYS;
    }
    fork(frame)
}

fn read(frame: &Aarch64TrapFrame) -> u64 {
    match fs::linux_stdio_fd(frame.x[0]) {
        Some(0) => return EAGAIN,
        Some(_) => return EBADF,
        None => {}
    }
    let Some(native_handle) = fs::linux_native_handle(frame.x[0]) else {
        return EBADF;
    };
    fs::read(native_handle, frame.x[1], frame.x[2])
}

fn write(frame: &Aarch64TrapFrame) -> u64 {
    if let Some(stdio_fd) = fs::linux_stdio_fd(frame.x[0]) {
        if stdio_fd != 1 && stdio_fd != 2 {
            return EBADF;
        }
        return write_stdio(frame);
    }
    let Some(native_handle) = fs::linux_native_handle(frame.x[0]) else {
        return EBADF;
    };
    fs::write(native_handle, frame.x[1], frame.x[2])
}

fn write_stdio(frame: &Aarch64TrapFrame) -> u64 {
    let requested = usize::try_from(frame.x[2]).unwrap_or(usize::MAX);
    if requested == 0 {
        return 0;
    }
    let mut buffer = [0u8; 128];
    let mut offset = 0usize;
    while offset < requested {
        let count = (requested - offset).min(buffer.len());
        let Some(address) = frame.x[1].checked_add(offset as u64) else {
            return EFAULT;
        };
        if user_memory::copy_from_user(address, &mut buffer[..count]).is_err() {
            return EFAULT;
        }
        for byte in &buffer[..count] {
            uart::putc(*byte);
        }
        offset += count;
    }
    requested as u64
}

fn openat(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] as i64 != AT_FDCWD {
        return ENOSYS;
    }
    let flags = frame.x[2];
    if flags & O_ACCMODE != 0 || flags & (O_CREAT | O_EXCL | O_TRUNC | O_APPEND) != 0 {
        return EROFS;
    }
    let native_handle = fs::open(frame.x[1], flags, frame.x[3]);
    if (native_handle as i64) < 0 {
        return native_handle;
    }
    fs::linux_install_handle(native_handle, flags)
}

fn lseek(frame: &Aarch64TrapFrame) -> u64 {
    let Some(native_handle) = fs::linux_native_handle(frame.x[0]) else {
        return EBADF;
    };
    fs::seek(native_handle, frame.x[1] as i64, frame.x[2])
}

fn fstat(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[1] == 0 {
        return EFAULT;
    }
    let Some(native_handle) = fs::linux_native_handle(frame.x[0]) else {
        return EBADF;
    };
    let Ok(size) = fs::file_size(native_handle) else {
        return EBADF;
    };
    // Linux AArch64's asm-generic struct stat is 128 bytes.  Keep the
    // metadata conservative: all Fullerene VFS mounts are read-only and the
    // early compatibility path exposes opened objects as regular files.
    let mut stat = [0u8; 128];
    stat[16..20].copy_from_slice(&0o100444u32.to_ne_bytes());
    stat[20..24].copy_from_slice(&1u32.to_ne_bytes());
    stat[24..28].copy_from_slice(&0u32.to_ne_bytes());
    stat[28..32].copy_from_slice(&0u32.to_ne_bytes());
    stat[48..56].copy_from_slice(&size.to_ne_bytes());
    stat[56..60].copy_from_slice(&(4096i32).to_ne_bytes());
    stat[64..72].copy_from_slice(&(size.div_ceil(4096) * 8).to_ne_bytes());
    if user_memory::copy_to_user(frame.x[1], &stat).is_err() {
        EFAULT
    } else {
        0
    }
}

fn clock_gettime(frame: &Aarch64TrapFrame) -> u64 {
    let clock_id = frame.x[0];
    let destination = frame.x[1];
    if destination == 0 || !matches!(clock_id, 0 | 1 | 6 | 7) {
        return EINVAL;
    }
    let microseconds = timer::uptime_us();
    let seconds = (microseconds / 1_000_000) as i64;
    let nanoseconds = ((microseconds % 1_000_000) * 1_000) as i64;
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&seconds.to_ne_bytes());
    bytes[8..].copy_from_slice(&nanoseconds.to_ne_bytes());
    if user_memory::copy_to_user(destination, &bytes).is_err() {
        EFAULT
    } else {
        0
    }
}

fn nanosleep(frame: &mut Aarch64TrapFrame) -> bool {
    if frame.x[0] == 0 {
        frame.x[0] = EFAULT;
        return true;
    }
    let mut bytes = [0u8; 16];
    if user_memory::copy_from_user(frame.x[0], &mut bytes).is_err() {
        frame.x[0] = EFAULT;
        return true;
    }
    let seconds = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
    let nanoseconds = i64::from_ne_bytes(bytes[8..].try_into().unwrap());
    if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
        frame.x[0] = EINVAL;
        return true;
    }
    let seconds = u64::try_from(seconds).unwrap_or(u64::MAX);
    let duration_us = seconds
        .saturating_mul(1_000_000)
        .saturating_add((nanoseconds as u64) / 1_000);
    if !timer::irq_ready() || !task::can_block_sleep() {
        timer::delay_us(duration_us);
        frame.x[0] = 0;
        return true;
    }
    task::sleep_syscall(frame, duration_us)
}

fn mmap(frame: &Aarch64TrapFrame) -> u64 {
    let address = frame.x[0];
    let length = frame.x[1];
    let protection = frame.x[2];
    let flags = frame.x[3];
    let fd = frame.x[4] as i64;
    let offset = frame.x[5];
    if length == 0 || protection & !PROT_MASK != 0 {
        return EINVAL;
    }
    if flags & MAP_FIXED != 0 && address == 0 {
        return EINVAL;
    }
    if offset & 4095 != 0 {
        return EINVAL;
    }
    if flags & MAP_SHARED != 0 {
        // The Bramble Android partitions are mounted read-only.  Mapping
        // them shared would claim write-back semantics that the VFS cannot
        // provide, so keep the contract explicit until a write-capable
        // mapping object exists.
        return ENOSYS;
    }
    if flags & MAP_PRIVATE == 0 {
        return EINVAL;
    }
    if flags & !(MAP_PRIVATE | MAP_FIXED | MAP_ANONYMOUS) != 0 {
        return ENOSYS;
    }
    let file_handle = if flags & MAP_ANONYMOUS == 0 {
        if fd < 0 {
            return EBADF;
        }
        fs::linux_native_handle(fd as u64)
    } else {
        None
    };
    if flags & MAP_ANONYMOUS == 0 && file_handle.is_none() {
        return EBADF;
    }

    // Populate through a temporary writable mapping, then publish the exact
    // requested protection.  This lets the kernel copy file bytes into an
    // initially RX/RO ELF segment without ever writing through its final
    // user permissions.
    let final_protection = protection & PROT_MASK;
    let fill_protection = final_protection | 0x3;
    let mapped = match allocator::with_global(|frames| {
        task::map_memory(frames, address, length, fill_protection << 16)
    }) {
        Some(Ok(mapped)) => mapped,
        Some(Err(error)) => return error,
        None => return ENOMEM,
    };
    if let Some(native_handle) = file_handle {
        let requested = usize::try_from(length).unwrap_or(usize::MAX);
        let mut buffer = [0u8; 4096];
        let mut copied = 0usize;
        while copied < requested {
            let count = (requested - copied).min(buffer.len());
            let Some(file_offset) = offset.checked_add(copied as u64) else {
                let _ = allocator::with_global(|frames| task::unmap_memory(frames, mapped, length));
                return EINVAL;
            };
            let read = match fs::read_kernel_at(native_handle, file_offset, &mut buffer[..count]) {
                Ok(read) => read,
                Err(error) => {
                    let _ =
                        allocator::with_global(|frames| task::unmap_memory(frames, mapped, length));
                    return error;
                }
            };
            if read == 0 {
                break;
            }
            if user_memory::copy_to_user(mapped + copied as u64, &buffer[..read]).is_err() {
                let _ = allocator::with_global(|frames| task::unmap_memory(frames, mapped, length));
                return EFAULT;
            }
            copied += read;
            if read < count {
                break;
            }
        }
    }
    if final_protection != fill_protection {
        if let Err(error) = task::protect_memory(mapped, length, final_protection) {
            let _ = allocator::with_global(|frames| task::unmap_memory(frames, mapped, length));
            return error;
        }
    }
    mapped
}

fn munmap(frame: &Aarch64TrapFrame) -> u64 {
    match allocator::with_global(|frames| task::unmap_memory(frames, frame.x[0], frame.x[1])) {
        Some(Ok(result)) => result,
        Some(Err(error)) => error,
        None => ENOMEM,
    }
}

fn mprotect(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[2] & !PROT_MASK != 0 {
        return EINVAL;
    }
    task::protect_memory(frame.x[0], frame.x[1], frame.x[2] & PROT_MASK)
        .unwrap_or_else(|error| error)
}

fn brk(requested: u64) -> u64 {
    let current = if task::current_linux_brk() == 0 {
        let _ = task::set_current_linux_brk(LINUX_HEAP_START);
        LINUX_HEAP_START
    } else {
        task::current_linux_brk()
    };
    if requested == 0 || requested < LINUX_HEAP_START || requested <= current {
        // The bounded mapping table does not yet track a brk region
        // separately from mmap mappings, so shrinking is withheld rather
        // than leaving an overlapping region for a later growth request.
        return current;
    }
    let old_end = align_page(current);
    let new_end = align_page(requested);
    if new_end <= old_end {
        let _ = task::set_current_linux_brk(requested);
        return requested;
    }
    let result = allocator::with_global(|frames| {
        task::map_memory(
            frames,
            old_end,
            new_end.saturating_sub(old_end),
            (0x3u64) << 16,
        )
    });
    match result {
        Some(Ok(_)) if task::set_current_linux_brk(requested) => requested,
        _ => current,
    }
}

fn align_page(value: u64) -> u64 {
    value.saturating_add(4095) & !4095
}

fn getcwd(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] == 0 || frame.x[1] < 2 {
        return EINVAL;
    }
    if user_memory::copy_to_user(frame.x[0], b"/\0").is_err() {
        EFAULT
    } else {
        frame.x[0]
    }
}

fn uname(destination: u64) -> u64 {
    if destination == 0 {
        return EFAULT;
    }
    let mut bytes = [0u8; 390];
    put_uts_field(&mut bytes, 0, b"FullereneOS");
    put_uts_field(&mut bytes, 65, b"fullerene");
    put_uts_field(&mut bytes, 130, b"0.1-aarch64");
    put_uts_field(&mut bytes, 195, b"Fullerene AArch64");
    put_uts_field(&mut bytes, 260, b"aarch64");
    put_uts_field(&mut bytes, 325, b"fullerene");
    if user_memory::copy_to_user(destination, &bytes).is_err() {
        EFAULT
    } else {
        0
    }
}

fn put_uts_field(destination: &mut [u8; 390], offset: usize, value: &[u8]) {
    let count = value.len().min(64);
    destination[offset..offset + count].copy_from_slice(&value[..count]);
}

fn prctl(frame: &Aarch64TrapFrame) -> u64 {
    match frame.x[0] {
        3 => 1,      // PR_GET_DUMPABLE
        4 | 38 => 0, // PR_SET_DUMPABLE / PR_SET_NO_NEW_PRIVS
        15 => {
            let mut name = [0u8; 16];
            let Some(length) = copy_user_c_string(frame.x[1], &mut name) else {
                return EFAULT;
            };
            if task::set_current_name(&name[..length]) {
                0
            } else {
                EINVAL
            }
        }
        16 => {
            if frame.x[1] == 0 {
                return EFAULT;
            }
            let mut name = [0u8; 16];
            let length = task::current_name(&mut name);
            if user_memory::copy_to_user(frame.x[1], &name[..length]).is_err()
                || user_memory::copy_to_user(frame.x[1] + length as u64, &[0]).is_err()
            {
                EFAULT
            } else {
                0
            }
        }
        _ => ENOSYS,
    }
}

fn copy_user_c_string(address: u64, destination: &mut [u8]) -> Option<usize> {
    if address == 0 {
        return None;
    }
    for offset in 0..destination.len() {
        let mut byte = [0u8; 1];
        user_memory::copy_from_user(address.checked_add(offset as u64)?, &mut byte).ok()?;
        if byte[0] == 0 {
            return Some(offset);
        }
        destination[offset] = byte[0];
    }
    None
}

fn futex(frame: &Aarch64TrapFrame) -> u64 {
    // The address is checked before returning so a malformed pointer cannot
    // accidentally be reported as a successful synchronization operation.
    if frame.x[0] == 0 {
        return EFAULT;
    }
    let operation = frame.x[1] & 0x7f;
    if operation == 1 || operation == 9 {
        return 0;
    }
    EAGAIN
}
