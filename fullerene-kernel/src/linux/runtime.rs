// LinuxRuntime implementation
use super::fs as linux_fs;
use super::memory as linux_mem;
use super::memory::LinuxMmapRegion;
use super::misc as linux_misc;
use super::numbers::*;
use super::process as linux_proc;
use super::signal as linux_signal;
use super::time as linux_time;
use super::types::*;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use petroleum::common::logging::SystemError;

use crate::user_memory::{self, UserCopyError};

/// Dispatch mode for the process: which runtime handles its syscalls.
pub enum DispatchMode {
    /// Fullerene native syscalls (existing behavior)
    Fullerene,
    /// Linux ABI emulation
    Linux(Box<LinuxRuntime>),
}

/// Linux process state.
pub struct LinuxRuntime {
    /// VFS fd table for the Linux runtime
    pub fd_table: LinuxFdTable,
    /// Per-process signal handlers (indexed by signal number - 1)
    pub signal_handlers: [LinuxSigAction; 64],
    /// Pending signal bitmask
    pub signal_pending: u64,
    /// Thread-local storage pointer (from ARCH_SET_FS)
    pub tls_ptr: u64,
    /// Current program break (for brk/sbrk)
    pub program_break: u64,
    /// Initial program break (start of heap)
    pub initial_break: u64,
    /// Thread ID (same as PID for main thread)
    pub tid: u64,
    /// Child TID to clear on exit (from set_tid_address / CLONE_CHILD_CLEARTID)
    pub child_clear_tid: u64,
    /// Robust list head (from set_robust_list)
    pub robust_list_head: u64,
    /// Robust list length
    pub robust_list_len: u64,
    /// Current working directory fd (for *at syscalls, AT_FDCWD = -100)
    pub cwd_fd: i32,
    /// Umask
    pub umask: u32,
    /// Per-process virtual memory regions tracked for mmap/munmap
    pub mmap_regions: Vec<LinuxMmapRegion>,
}

impl LinuxRuntime {
    pub fn new(tid: u64, initial_break: u64) -> Self {
        Self {
            fd_table: LinuxFdTable::new(),
            signal_handlers: [LinuxSigAction::default(); 64],
            signal_pending: 0,
            tls_ptr: 0,
            program_break: initial_break,
            initial_break,
            tid,
            child_clear_tid: 0,
            robust_list_head: 0,
            robust_list_len: 0,
            cwd_fd: -100,
            umask: 0o22,
            mmap_regions: Vec::new(),
        }
    }

    pub fn set_errno(&mut self, err: i32) -> u64 {
        err as u64
    }

    pub fn is_std_fd(fd: i32) -> bool {
        (0..=2).contains(&fd)
    }

    /// Dispatch a Linux syscall by number.
    pub fn dispatch(&mut self, syscall_no: u64, args: &[u64; 6]) -> u64 {
        match syscall_no {
            // File system
            SYS_READ => linux_fs::sys_read(self, args),
            SYS_WRITE => linux_fs::sys_write(self, args),
            SYS_OPEN => linux_fs::sys_open(self, args),
            SYS_CLOSE => linux_fs::sys_close(self, args),
            SYS_STAT => linux_fs::sys_stat(self, args),
            SYS_FSTAT => linux_fs::sys_fstat(self, args),
            SYS_LSTAT => linux_fs::sys_stat(self, args),
            SYS_LSEEK => linux_fs::sys_lseek(self, args),
            SYS_PREAD64 => linux_fs::sys_pread64(self, args),
            SYS_PWRITE64 => linux_fs::sys_pwrite64(self, args),
            SYS_READV => linux_fs::sys_readv(self, args),
            SYS_WRITEV => linux_fs::sys_writev(self, args),
            SYS_ACCESS => linux_fs::sys_access(self, args),
            SYS_GETDENTS => linux_fs::sys_getdents64(self, args),
            SYS_GETDENTS64 => linux_fs::sys_getdents64(self, args),
            SYS_OPENAT => linux_fs::sys_openat(self, args),
            SYS_NEWFSTATAT => linux_fs::sys_newfstatat(self, args),
            SYS_FACCESSAT => linux_fs::sys_faccessat(self, args),
            SYS_READLINK => linux_fs::sys_readlink(self, args),
            SYS_READLINKAT => linux_fs::sys_readlinkat(self, args),
            SYS_UNLINK => linux_fs::sys_unlink(self, args),
            SYS_UNLINKAT => linux_fs::sys_unlinkat(self, args),
            SYS_MKDIR => linux_fs::sys_mkdir(self, args),
            SYS_MKDIRAT => linux_fs::sys_mkdirat(self, args),
            SYS_RMDIR => linux_fs::sys_rmdir(self, args),
            SYS_SYMLINK => linux_fs::sys_symlink(self, args),
            SYS_RENAME => linux_fs::sys_rename(self, args),
            SYS_CHDIR => linux_fs::sys_chdir(self, args),
            SYS_GETCWD => linux_fs::sys_getcwd(self, args),
            SYS_MOUNT => linux_fs::sys_mount(self, args),
            SYS_UMOUNT2 => linux_fs::sys_umount2(self, args),
            SYS_DUP => linux_fs::sys_dup(self, args),
            SYS_DUP2 => linux_fs::sys_dup2(self, args),
            SYS_DUP3 => linux_fs::sys_dup3(self, args),
            SYS_FCNTL => linux_fs::sys_fcntl(self, args),
            SYS_IOCTL => linux_fs::sys_ioctl(self, args),
            SYS_PIPE => linux_fs::sys_pipe(self, args),
            SYS_PIPE2 => linux_fs::sys_pipe2(self, args),
            SYS_TRUNCATE => linux_fs::sys_truncate(self, args),
            SYS_FTRUNCATE => linux_fs::sys_ftruncate(self, args),
            SYS_FSYNC => linux_fs::sys_fsync(self, args),
            SYS_FDATASYNC => linux_fs::sys_fdatasync(self, args),
            SYS_CHMOD => linux_fs::sys_fchmod(self, args),
            SYS_FCHMOD => linux_fs::sys_fchmodat(self, args),
            SYS_CREAT => linux_fs::sys_creat(self, args),

            // Memory
            SYS_MMAP => linux_mem::sys_mmap(self, args),
            SYS_MUNMAP => linux_mem::sys_munmap(self, args),
            SYS_MPROTECT => linux_mem::sys_mprotect(self, args),
            SYS_BRK => linux_mem::sys_brk(self, args),
            SYS_MREMAP => linux_mem::sys_mremap(self, args),
            SYS_MADVISE => linux_mem::sys_madvise(self, args),

            // Process
            SYS_EXIT => linux_proc::sys_exit(self, args),
            SYS_EXIT_GROUP => linux_proc::sys_exit_group(self, args),
            SYS_GETPID => linux_proc::sys_getpid(self, args),
            SYS_GETPPID => linux_proc::sys_getppid(self, args),
            SYS_GETTID => linux_proc::sys_gettid(self, args),
            SYS_CLONE => linux_proc::sys_clone(self, args),
            SYS_FORK => linux_proc::sys_fork(self, args),
            SYS_EXECVE => linux_proc::sys_execve(self, args),
            SYS_WAIT4 => linux_proc::sys_wait4(self, args),
            SYS_KILL => linux_proc::sys_kill(self, args),
            SYS_TKILL => linux_proc::sys_tkill(self, args),
            SYS_TGKILL => linux_proc::sys_tgkill(self, args),

            // Signals
            SYS_RT_SIGACTION => linux_signal::sys_rt_sigaction(self, args),
            SYS_RT_SIGPROCMASK => linux_signal::sys_rt_sigprocmask(self, args),
            SYS_RT_SIGRETURN => linux_signal::sys_rt_sigreturn(self, args),

            // Time
            SYS_NANOSLEEP => linux_time::sys_nanosleep(self, args),
            SYS_CLOCK_GETTIME => linux_time::sys_clock_gettime(self, args),
            SYS_GETTIMEOFDAY => linux_time::sys_gettimeofday(self, args),
            SYS_TIME => linux_time::sys_time(self, args),

            // Misc
            SYS_UNAME => linux_misc::sys_uname(self, args),
            SYS_ARCH_PRCTL => linux_misc::sys_arch_prctl(self, args),
            SYS_SET_TID_ADDRESS => linux_misc::sys_set_tid_address(self, args),
            SYS_SET_ROBUST_LIST => linux_misc::sys_set_robust_list(self, args),
            SYS_GET_ROBUST_LIST => linux_misc::sys_get_robust_list(self, args),
            SYS_GETRANDOM => linux_misc::sys_getrandom(self, args),
            SYS_PRLIMIT64 => linux_misc::sys_prlimit64(self, args),
            SYS_GETRLIMIT => linux_misc::sys_getrlimit(self, args),
            SYS_SETRLIMIT => linux_misc::sys_setrlimit(self, args),
            SYS_SCHED_YIELD => linux_misc::sys_sched_yield(self, args),
            SYS_GETUID => linux_misc::sys_getuid(self, args),
            SYS_GETGID => linux_misc::sys_getgid(self, args),
            SYS_GETEUID => linux_misc::sys_geteuid(self, args),
            SYS_GETEGID => linux_misc::sys_getegid(self, args),
            SYS_UMASK => linux_misc::sys_umask(self, args),
            SYS_CAPGET => linux_misc::sys_capget(self, args),
            SYS_SYSINFO => linux_misc::sys_sysinfo(self, args),
            SYS_PRCTL => linux_misc::sys_prctl(self, args),
            SYS_FUTEX => linux_misc::sys_futex(self, args),
            SYS_STATFS => linux_misc::sys_statfs(self, args),
            SYS_FSTATFS => linux_misc::sys_fstatfs(self, args),
            SYS_SCHED_GETAFFINITY => linux_misc::sys_sched_getaffinity(self, args),
            SYS_SCHED_SETAFFINITY => linux_misc::sys_sched_setaffinity(self, args),

            _ => {
                log::warn!("Linux syscall {} unknown, returning ENOSYS", syscall_no);
                errno_code(ENOSYS)
            }
        }
    }
}

/// Linux file descriptor table.
pub struct LinuxFdTable {
    pub(crate) entries: BTreeMap<i32, LinuxFileDesc>,
    next_fd: i32,
}

#[derive(Debug, Clone)]
pub struct LinuxFileDesc {
    pub vfs_fd: u32,
    pub mount_index: usize,
    pub flags: i32,
    pub offset: u64,
    /// For directory fds: the path passed to `open`/`openat`,
    /// used by `getdents64` to read the correct directory.
    pub dir_path: Option<alloc::string::String>,
}

impl LinuxFdTable {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_fd: 3,
        }
    }

    /// Allocate a Linux fd, storing the vfs mapping.
    pub fn alloc(&mut self, vfs_fd: u32, mount_index: usize, flags: i32) -> i32 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.entries.insert(
            fd,
            LinuxFileDesc {
                vfs_fd,
                mount_index,
                flags,
                offset: 0,
                dir_path: None,
            },
        );
        fd
    }

    pub fn get(&self, fd: i32) -> Option<&LinuxFileDesc> {
        self.entries.get(&fd)
    }

    pub fn get_mut(&mut self, fd: i32) -> Option<&mut LinuxFileDesc> {
        self.entries.get_mut(&fd)
    }

    pub fn remove(&mut self, fd: i32) -> Option<LinuxFileDesc> {
        self.entries.remove(&fd)
    }

    pub fn contains(&self, fd: i32) -> bool {
        self.entries.contains_key(&fd)
    }
}

/// Typed positive Linux errno used at domain-to-ABI conversion boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct LinuxErrno(i32);

impl LinuxErrno {
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl From<genome::fs::FsError> for LinuxErrno {
    fn from(error: genome::fs::FsError) -> Self {
        use genome::fs::FsError;
        Self(match error {
            FsError::FileNotFound => ENOENT,
            FsError::FileExists => EEXIST,
            FsError::PermissionDenied => EACCES,
            FsError::InvalidFileDescriptor => EBADF,
            FsError::InvalidSeek | FsError::InvalidPath | FsError::InvalidInput => EINVAL,
            FsError::DiskFull => ENOSPC,
            FsError::NotADirectory => ENOTDIR,
            FsError::DirectoryNotEmpty => ENOTEMPTY,
            FsError::IsADirectory => EISDIR,
            FsError::NotSupported => ENOTSUP,
            FsError::Io => EIO,
        })
    }
}

impl From<genome::block::BlockError> for LinuxErrno {
    fn from(error: genome::block::BlockError) -> Self {
        use genome::block::BlockError;
        Self(match error {
            BlockError::Device => EIO,
            BlockError::BufferTooSmall { .. } => EINVAL,
            BlockError::LbaOverflow => EOVERFLOW,
            BlockError::SectorNotFound => ENOENT,
        })
    }
}

impl From<nitrogen::DriverError> for LinuxErrno {
    fn from(error: nitrogen::DriverError) -> Self {
        use nitrogen::DriverError;
        Self(match error {
            DriverError::DeviceNotFound => ENODEV,
            DriverError::NotReady => EAGAIN,
            DriverError::InvalidArgument => EINVAL,
            DriverError::OutOfMemory | DriverError::MmioMappingFailed => ENOMEM,
            DriverError::DmaMappingFailed
            | DriverError::Io
            | DriverError::Protocol
            | DriverError::DeviceFault => EIO,
            DriverError::TimedOut => ETIMEDOUT,
            DriverError::Busy => EBUSY,
            DriverError::NotSupported => ENOTSUP,
        })
    }
}

impl From<petroleum::MemoryError> for LinuxErrno {
    fn from(error: petroleum::MemoryError) -> Self {
        use petroleum::MemoryError;
        Self(match error {
            MemoryError::OutOfMemory | MemoryError::FrameAllocationFailed => ENOMEM,
            MemoryError::MappingFailed | MemoryError::InvalidAddress => EFAULT,
            MemoryError::UnmappingFailed
            | MemoryError::InvalidAlignment
            | MemoryError::InvalidSize
            | MemoryError::NotMapped => EINVAL,
            MemoryError::AddressOverflow => EOVERFLOW,
            MemoryError::AlreadyMapped => EEXIST,
            MemoryError::PermissionDenied => EACCES,
            MemoryError::NotInitialized => EFAULT,
        })
    }
}

/// Translate FsError to a Linux errno.
pub fn fs_errno(err: &genome::fs::FsError) -> i32 {
    LinuxErrno::from(*err).get()
}

/// Translate errno from FsError/C errors to Linux errno.
pub fn to_linux_errno(err: &str) -> i32 {
    match err {
        "not found" => ENOENT,
        "bad fd" => EBADF,
        "not a file" => EISDIR,
        "inode not found" => ENOENT,
        "directory not empty" => ENOTEMPTY,
        "invalid path" => EINVAL,
        "create failed" => EEXIST,
        "open failed after create" => EEXIST,
        "mkdir failed" => EACCES,
        "not a directory" => ENOTDIR,
        "is a directory" => EISDIR,
        "permission denied" => EACCES,
        "file exists" => EEXIST,
        "out of memory" => ENOMEM,
        _ => EIO,
    }
}

/// Convert a positive errno to a negative kernel return value.
#[inline]
pub fn errno_code(e: i32) -> u64 {
    (e as i64).wrapping_neg() as u64
}

/// Translate a Fullerene VFS error string to a negative errno return.
pub fn errno_result(err: &str) -> u64 {
    let e = to_linux_errno(err);
    errno_code(e)
}

/// Translate a Fullerene FsError to a negative errno return.
pub fn fs_errno_result(err: &genome::fs::FsError) -> u64 {
    errno_code(fs_errno(err))
}

/// Convert string from raw user pointer.
///
/// # Safety
///
/// The current process address space must remain stable while the string is
/// validated and copied.
pub unsafe fn copy_user_string(ptr: u64, max_len: usize) -> Result<alloc::string::String, i32> {
    unsafe { user_memory::copy_c_string(ptr as *const u8, max_len) }.map_err(linux_user_copy_error)
}

fn linux_user_copy_error(error: UserCopyError) -> i32 {
    match error {
        UserCopyError::System(SystemError::MemOutOfMemory | SystemError::SyscallOutOfMemory) => {
            ENOMEM
        }
        UserCopyError::InvalidUtf8 | UserCopyError::MissingNul => EINVAL,
        UserCopyError::System(_) => EFAULT,
    }
}

/// Copy data from user space to kernel with a hard memory-exhaustion limit.
const MAX_USER_COPY: usize = 64 * 1024;

fn checked_user_copy_len(count: usize) -> Result<usize, i32> {
    if count > MAX_USER_COPY {
        Err(E2BIG)
    } else {
        Ok(count)
    }
}

/// Copy bytes from a user pointer into kernel-owned memory.
///
/// Returns `E2BIG` rather than truncating when `count` exceeds
/// [`MAX_USER_COPY`].
///
/// # Safety
///
/// The current process address space must remain stable while the buffer is
/// validated and copied.
pub unsafe fn copy_from_user(buf: u64, count: usize) -> Result<alloc::vec::Vec<u8>, i32> {
    if buf == 0 {
        return Err(EFAULT);
    }
    let len = checked_user_copy_len(count)?;
    unsafe { user_memory::copy_bytes_from_user(buf as *const u8, len) }
        .map_err(linux_user_copy_error)
}

/// Copy data to user space.
///
/// # Safety
///
/// The current process address space must remain stable while the destination
/// is validated and written.
pub unsafe fn copy_to_user(buf: u64, data: &[u8]) -> Result<(), i32> {
    if buf == 0 {
        return Err(EFAULT);
    }
    unsafe { user_memory::copy_bytes_to_user(buf as *mut u8, data) }.map_err(linux_user_copy_error)
}

/// Copy an arbitrary sized value to user space.
///
/// # Safety
///
/// The current process address space must remain stable while the destination
/// is validated and written.
pub unsafe fn copy_val_to_user<T: Copy>(buf: u64, val: &T) -> Result<(), i32> {
    if buf == 0 {
        return Err(EFAULT);
    }
    unsafe { user_memory::copy_value_to_user(buf as *mut T, val) }.map_err(linux_user_copy_error)
}

#[cfg(test)]
mod user_copy_tests {
    use super::*;

    #[test]
    fn linux_copy_limit_rejects_instead_of_truncating() {
        assert_eq!(checked_user_copy_len(MAX_USER_COPY), Ok(MAX_USER_COPY));
        assert_eq!(checked_user_copy_len(MAX_USER_COPY + 1), Err(E2BIG));
    }

    #[test]
    fn linux_user_copy_errors_keep_errno_meaning() {
        assert_eq!(linux_user_copy_error(UserCopyError::InvalidUtf8), EINVAL);
        assert_eq!(linux_user_copy_error(UserCopyError::MissingNul), EINVAL);
        assert_eq!(
            linux_user_copy_error(UserCopyError::System(SystemError::MemOutOfMemory)),
            ENOMEM
        );
        assert_eq!(
            linux_user_copy_error(UserCopyError::System(SystemError::PermissionDenied)),
            EFAULT
        );
    }

    #[test]
    fn domain_errors_preserve_linux_errno_meaning() {
        assert_eq!(
            LinuxErrno::from(genome::fs::FsError::DiskFull).get(),
            ENOSPC
        );
        assert_eq!(
            LinuxErrno::from(genome::block::BlockError::LbaOverflow).get(),
            EOVERFLOW
        );
        assert_eq!(
            LinuxErrno::from(nitrogen::DriverError::TimedOut).get(),
            ETIMEDOUT
        );
        assert_eq!(
            LinuxErrno::from(petroleum::MemoryError::AlreadyMapped).get(),
            EEXIST
        );
        assert_eq!(
            LinuxErrno::from(petroleum::MemoryError::MappingFailed).get(),
            EFAULT
        );
        assert_eq!(
            LinuxErrno::from(petroleum::MemoryError::InvalidAddress).get(),
            EFAULT
        );
        assert_eq!(
            LinuxErrno::from(genome::fs::FsError::NotSupported).get(),
            ENOTSUP
        );
        assert_eq!(LinuxErrno::from(genome::fs::FsError::Io).get(), EIO);
        assert_eq!(
            LinuxErrno::from(petroleum::MemoryError::NotInitialized).get(),
            EFAULT
        );
    }
}
