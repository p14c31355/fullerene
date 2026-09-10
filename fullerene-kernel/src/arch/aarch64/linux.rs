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
use genome::vfs::FileMetadata;

const ENOSYS: u64 = (-(38i64)) as u64;
const ENOENT: u64 = (-(2i64)) as u64;
const ESRCH: u64 = (-(3i64)) as u64;
const EINVAL: u64 = (-(22i64)) as u64;
const EFAULT: u64 = (-(14i64)) as u64;
const ENOMEM: u64 = (-(12i64)) as u64;
const EAGAIN: u64 = (-(11i64)) as u64;
const ETIMEDOUT: u64 = (-(110i64)) as u64;
const EBADF: u64 = (-(9i64)) as u64;
const ENOTSOCK: u64 = (-(88i64)) as u64;
const ENOTTY: u64 = (-(25i64)) as u64;
const EROFS: u64 = (-(30i64)) as u64;
const EPERM: u64 = (-(1i64)) as u64;

const SYS_GETCWD: u64 = 17;
const SYS_EVENTFD2: u64 = 19;
const SYS_INOTIFY_INIT1: u64 = 26;
const SYS_INOTIFY_ADD_WATCH: u64 = 27;
const SYS_INOTIFY_RM_WATCH: u64 = 28;
const SYS_EPOLL_CREATE1: u64 = 20;
const SYS_EPOLL_CTL: u64 = 21;
const SYS_EPOLL_PWAIT: u64 = 22;
const SYS_FCNTL: u64 = 25;
const SYS_IOCTL: u64 = 29;
const SYS_STATFS: u64 = 43;
const SYS_FSTATFS: u64 = 44;
const SYS_FTRUNCATE: u64 = 46;
const SYS_FACCESSAT: u64 = 48;
const SYS_PPOLL: u64 = 73;
const SYS_SIGNALFD4: u64 = 74;
const SYS_MKNODAT: u64 = 33;
const SYS_SYMLINKAT: u64 = 36;
const SYS_LINKAT: u64 = 37;
const SYS_RENAMEAT: u64 = 38;
const SYS_PIVOT_ROOT: u64 = 41;
const SYS_CHDIR: u64 = 49;
const SYS_FCHDIR: u64 = 50;
const SYS_CHROOT: u64 = 51;
const SYS_FCHMOD: u64 = 52;
const SYS_FCHMODAT: u64 = 53;
const SYS_FCHOWNAT: u64 = 54;
const SYS_FCHOWN: u64 = 55;
const SYS_DUP: u64 = 23;
const SYS_DUP3: u64 = 24;
const SYS_OPENAT: u64 = 56;
const SYS_CLOSE: u64 = 57;
const SYS_PIPE2: u64 = 59;
const SYS_GETDENTS64: u64 = 61;
const SYS_LSEEK: u64 = 62;
const SYS_READ: u64 = 63;
const SYS_WRITE: u64 = 64;
const SYS_READV: u64 = 65;
const SYS_WRITEV: u64 = 66;
const SYS_PREAD64: u64 = 67;
const SYS_READLINKAT: u64 = 78;
const SYS_FSTATAT: u64 = 79;
const SYS_FSTAT: u64 = 80;
const SYS_SYNC: u64 = 81;
const SYS_FSYNC: u64 = 82;
const SYS_FDATASYNC: u64 = 83;
const SYS_KILL: u64 = 129;
const SYS_TKILL: u64 = 130;
const SYS_TGKILL: u64 = 131;
const SYS_SETUID: u64 = 146;
const SYS_SETREUID: u64 = 145;
const SYS_SETGID: u64 = 144;
const SYS_SETREGID: u64 = 143;
const SYS_SETRESUID: u64 = 147;
const SYS_SETRESGID: u64 = 149;
const SYS_SETFSUID: u64 = 151;
const SYS_SETFSGID: u64 = 152;
const SYS_TIMES: u64 = 153;
const SYS_SETPGID: u64 = 154;
const SYS_GETPGID: u64 = 155;
const SYS_GETSID: u64 = 156;
const SYS_SETSID: u64 = 157;
const SYS_GETRUSAGE: u64 = 165;
const SYS_GETCPU: u64 = 168;
const SYS_GETTIMEOFDAY: u64 = 169;
const SYS_SETTIMEOFDAY: u64 = 170;
const SYS_CAPGET: u64 = 90;
const SYS_CAPSET: u64 = 91;
const SYS_PERSONALITY: u64 = 92;
const SYS_EXIT: u64 = 93;
const SYS_EXIT_GROUP: u64 = 94;
const SYS_SET_TID_ADDRESS: u64 = 96;
const SYS_SET_ROBUST_LIST: u64 = 99;
const SYS_NANOSLEEP: u64 = 101;
const SYS_CLOCK_GETTIME: u64 = 113;
const SYS_SCHED_YIELD: u64 = 124;
const SYS_RT_SIGACTION: u64 = 134;
const SYS_RT_SIGPROCMASK: u64 = 135;
const SYS_RT_SIGRETURN: u64 = 139;
const SYS_UNAME: u64 = 160;
const SYS_PRCTL: u64 = 167;
const SYS_GETPID: u64 = 172;
const SYS_GETPPID: u64 = 173;
const SYS_GETUID: u64 = 174;
const SYS_GETEUID: u64 = 175;
const SYS_GETGID: u64 = 176;
const SYS_GETEGID: u64 = 177;
const SYS_GETTID: u64 = 178;
const SYS_GETRESUID: u64 = 148;
const SYS_GETRESGID: u64 = 150;
const SYS_BRK: u64 = 214;
const SYS_MUNMAP: u64 = 215;
const SYS_MMAP: u64 = 222;
const SYS_MPROTECT: u64 = 226;
const SYS_MADVISE: u64 = 233;
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
const SYS_WAITID: u64 = 95;
const SYS_SCHED_SETAFFINITY: u64 = 122;
const SYS_SCHED_GETAFFINITY: u64 = 123;
const SYS_GETGROUPS: u64 = 158;
const SYS_SETGROUPS: u64 = 159;
const SYS_GETRLIMIT: u64 = 163;
const SYS_SETRLIMIT: u64 = 164;
const SYS_SYSINFO: u64 = 179;
const SYS_SOCKET: u64 = 198;
const SYS_SOCKETPAIR: u64 = 199;
const SYS_BIND: u64 = 200;
const SYS_LISTEN: u64 = 201;
const SYS_CONNECT: u64 = 203;
const SYS_SENDTO: u64 = 206;
const SYS_RECVFROM: u64 = 207;
const SYS_SETSOCKOPT: u64 = 208;
const SYS_GETSOCKOPT: u64 = 209;
const SYS_SHUTDOWN: u64 = 210;
const SYS_SENDMSG: u64 = 211;
const SYS_RECVMSG: u64 = 212;
const SYS_MREMAP: u64 = 216;
const SYS_WAIT4: u64 = 260;
const SYS_PRLIMIT64: u64 = 261;
const SYS_SETNS: u64 = 268;
const SYS_UNSHARE: u64 = 97;
const SYS_GETRANDOM: u64 = 278;
const SYS_READAHEAD: u64 = 213;
const SYS_STATX: u64 = 291;
const SYS_CLOSE_RANGE: u64 = 436;
const SYS_FACCESSAT2: u64 = 439;
const SYS_ACCEPT4: u64 = 242;
const SYS_FSETXATTR: u64 = 7;

const MSG_DONTWAIT: u64 = 0x40;
const MSG_NOSIGNAL: u64 = 0x4000;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;
const MSG_CTRUNC: u32 = 0x8;
const MSGHDR_SIZE: usize = 56;
const MSG_IOVEC_SIZE: u64 = 16;
const MAX_MSG_IOVECS: usize = 64;
const MAX_MSG_CONTROL: usize = 128;
const MAX_MSG_IO: usize = 4096;
const SOL_SOCKET: u32 = 1;
const SCM_CREDENTIALS: u32 = 2;

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
const MAP_IGNORED_ANDROID: u64 = 0x800 | 0x1000 | 0x4000 | 0x8000;
const PROT_MASK: u64 = 0x7;
const LINUX_HEAP_START: u64 = 0x4202_0000;

enum FutexAction {
    Return(u64),
    Wait { key: u64, timeout_us: u64 },
}

/// Dispatch one Linux AArch64 SVC. Unknown Linux calls are consumed and
/// return `-ENOSYS`, preserving the selected personality.
pub(super) fn dispatch(frame: &mut Aarch64TrapFrame) -> bool {
    if matches!(frame.x[8], SYS_FUTEX | SYS_FUTEX_TIME64) {
        return dispatch_futex(frame);
    }
    let result = match frame.x[8] {
        SYS_PPOLL => fs::linux_poll(frame.x[0], frame.x[1]),
        SYS_SIGNALFD4 => fs::linux_signalfd4(frame.x[0], frame.x[1], frame.x[2], frame.x[3]),
        SYS_EVENTFD2 => fs::linux_eventfd(frame.x[0], frame.x[1]),
        SYS_INOTIFY_INIT1 => fs::linux_inotify_init(frame.x[0]),
        SYS_INOTIFY_ADD_WATCH => fs::linux_inotify_add_watch(frame.x[0], frame.x[1], frame.x[2]),
        SYS_INOTIFY_RM_WATCH => fs::linux_inotify_rm_watch(frame.x[0], frame.x[1]),
        SYS_EPOLL_CREATE1 => fs::linux_epoll_create1(frame.x[0]),
        SYS_EPOLL_CTL => fs::linux_epoll_ctl(frame.x[0], frame.x[1], frame.x[2], frame.x[3]),
        SYS_EPOLL_PWAIT => fs::linux_epoll_wait(frame.x[0], frame.x[1], frame.x[2]),
        SYS_MKNODAT => fs::linux_mknodat(frame.x[1]),
        SYS_GETCWD => getcwd(frame),
        SYS_FCNTL => fs::linux_fcntl(frame.x[0], frame.x[1], frame.x[2]),
        SYS_IOCTL => ioctl(frame),
        SYS_STATFS => statfs(frame),
        SYS_FSTATFS => fstatfs(frame),
        SYS_FTRUNCATE => fs::linux_native_handle(frame.x[0])
            .map(|handle| fs::linux_ftruncate(handle, frame.x[1]))
            .unwrap_or(EBADF),
        SYS_FSETXATTR => fs::linux_native_handle(frame.x[0])
            .map(|handle| {
                if fs::linux_property_file(handle) {
                    0
                } else {
                    ENOSYS
                }
            })
            .unwrap_or(EBADF),
        SYS_FACCESSAT | SYS_FACCESSAT2 => faccessat(frame),
        SYS_CHDIR => fs::linux_chdir(frame.x[0]),
        SYS_FCHDIR => {
            if fs::linux_native_handle(frame.x[0]).is_some() {
                0
            } else {
                EBADF
            }
        }
        SYS_CHROOT => fs::linux_chdir(frame.x[0]),
        SYS_FCHMOD | SYS_FCHOWN => ENOSYS,
        SYS_FCHMODAT => {
            if frame.x[0] as i64 != AT_FDCWD || frame.x[3] != 0 {
                ENOSYS
            } else {
                fs::linux_chmod(frame.x[1], frame.x[2])
            }
        }
        SYS_FCHOWNAT => {
            if frame.x[0] as i64 != AT_FDCWD || frame.x[4] != 0 {
                ENOSYS
            } else {
                fs::linux_chown(frame.x[1], frame.x[2], frame.x[3])
            }
        }
        SYS_SYNC | SYS_FSYNC | SYS_FDATASYNC => 0,
        SYS_KILL => kill_process(frame),
        SYS_TKILL => kill_thread(frame),
        SYS_TGKILL => kill_thread_group(frame),
        SYS_SETUID => setuid(frame),
        SYS_SETREUID => setreuid(frame),
        SYS_SETRESUID => setresuid(frame),
        SYS_SETFSUID => setfsuid(frame),
        SYS_SETGID => setgid(frame),
        SYS_SETREGID => setregid(frame),
        SYS_SETRESGID => setresgid(frame),
        SYS_SETFSGID => setfsgid(frame),
        SYS_TIMES => times(frame),
        SYS_SETPGID => setpgid(frame),
        SYS_GETPGID => getpgid(frame),
        SYS_GETSID => getsid(frame),
        SYS_SETSID => task::linux_setsid(),
        SYS_GETRUSAGE => getrusage(frame),
        SYS_GETCPU => getcpu(frame),
        SYS_GETTIMEOFDAY => gettimeofday(frame),
        SYS_SETTIMEOFDAY => 0,
        SYS_DUP => fs::linux_duplicate(frame.x[0], 0),
        SYS_DUP3 => fs::linux_duplicate_at(frame.x[0], frame.x[1], frame.x[2]),
        SYS_OPENAT => openat(frame),
        SYS_CLOSE => fs::linux_close(frame.x[0]),
        SYS_PIPE2 => fs::linux_pipe2(frame.x[0], frame.x[1]),
        SYS_GETDENTS64 => fs::linux_getdents64(frame.x[0], frame.x[1], frame.x[2]),
        SYS_LSEEK => lseek(frame),
        SYS_READ => read(frame),
        SYS_WRITE => write(frame),
        SYS_READV => readv(frame),
        SYS_WRITEV => writev(frame),
        SYS_PREAD64 => pread64(frame),
        SYS_READLINKAT => readlinkat(frame),
        SYS_FSTAT => fstat(frame),
        SYS_FSTATAT => fstatat(frame),
        SYS_STATX => statx(frame),
        SYS_CAPGET => capget(frame),
        SYS_CAPSET => capset(frame),
        SYS_PERSONALITY => 0,
        SYS_EXIT | SYS_EXIT_GROUP => return task::exit_syscall(frame, frame.x[0]),
        SYS_WAIT4 => return wait4(frame),
        SYS_WAITID => return waitid(frame),
        SYS_SET_TID_ADDRESS => task::set_linux_clear_child_tid(frame.x[0]).unwrap_or(ENOSYS),
        SYS_GETRLIMIT => getrlimit(frame),
        SYS_SETRLIMIT => setrlimit(frame),
        SYS_PRLIMIT64 => prlimit64(frame),
        SYS_GETGROUPS => getgroups(frame),
        SYS_SETGROUPS => setgroups(frame),
        SYS_SCHED_GETAFFINITY => sched_getaffinity(frame),
        SYS_SCHED_SETAFFINITY => 0,
        SYS_SYSINFO => sysinfo(frame),
        SYS_CLOSE_RANGE => close_range(frame),
        SYS_MREMAP => mremap(frame),
        SYS_GETRANDOM => getrandom(frame),
        SYS_SETNS | SYS_UNSHARE => ENOSYS,
        SYS_SOCKET => fs::linux_socket(frame.x[0], frame.x[1], frame.x[2]),
        SYS_SOCKETPAIR => fs::linux_socketpair(frame.x[0], frame.x[1], frame.x[2], frame.x[3]),
        SYS_BIND => fs::linux_socket_bind(frame.x[0], frame.x[1], frame.x[2]),
        SYS_LISTEN => fs::linux_socket_listen(frame.x[0], frame.x[1]),
        SYS_CONNECT => fs::linux_socket_connect(frame.x[0], frame.x[1], frame.x[2]),
        SYS_ACCEPT4 => fs::linux_socket_accept4(frame.x[0], frame.x[3]),
        SYS_SENDTO => fs::linux_socket_sendto(frame.x[0], frame.x[1], frame.x[2]),
        SYS_RECVFROM => fs::linux_socket_recvfrom(frame.x[0], frame.x[1], frame.x[2]),
        SYS_SENDMSG => sendmsg(frame),
        SYS_RECVMSG => recvmsg(frame),
        SYS_SHUTDOWN => fs::linux_socket_shutdown(frame.x[0]),
        SYS_SETSOCKOPT => {
            fs::linux_socket_setsockopt(frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4])
        }
        SYS_GETSOCKOPT => {
            fs::linux_socket_getsockopt(frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4])
        }
        SYS_SET_ROBUST_LIST | SYS_RSEQ => 0,
        SYS_NANOSLEEP => return nanosleep(frame),
        SYS_CLOCK_GETTIME => clock_gettime(frame),
        SYS_SCHED_YIELD => return task::yield_syscall(frame),
        SYS_RT_SIGACTION => return rt_sigaction(frame),
        SYS_RT_SIGPROCMASK => return rt_sigprocmask(frame),
        SYS_RT_SIGRETURN => return task::linux_sigreturn(frame),
        SYS_UNAME => uname(frame.x[0]),
        SYS_PRCTL => prctl(frame),
        SYS_GETPID => task::current_process_pid().unwrap_or(ENOSYS),
        SYS_GETTID => task::current_pid().unwrap_or(ENOSYS),
        SYS_GETPPID => task::current_parent_pid().unwrap_or(0),
        SYS_GETUID => getuid(),
        SYS_GETEUID => geteuid(),
        SYS_GETGID => getgid(),
        SYS_GETEGID => getegid(),
        SYS_GETRESUID => getresid(frame, false),
        SYS_GETRESGID => getresid(frame, true),
        SYS_BRK => brk(frame.x[0]),
        SYS_MUNMAP => munmap(frame),
        SYS_MMAP => mmap(frame),
        SYS_MPROTECT => mprotect(frame),
        SYS_MADVISE => 0,
        SYS_EXECVE => super::syscall::linux_exec_path(frame.x[0], frame.x[1], frame.x[2], frame),
        SYS_EXECVEAT => {
            if frame.x[0] as i64 != AT_FDCWD || frame.x[4] != 0 {
                ENOSYS
            } else {
                super::syscall::linux_exec_path(frame.x[1], frame.x[2], frame.x[3], frame)
            }
        }
        SYS_CLONE => clone(frame),
        SYS_UNLINKAT => {
            if frame.x[0] as i64 != AT_FDCWD || frame.x[2] != 0 {
                ENOSYS
            } else {
                fs::linux_unlinkat(frame.x[1])
            }
        }
        SYS_MKDIRAT => {
            if frame.x[0] as i64 != AT_FDCWD {
                ENOSYS
            } else {
                fs::linux_mkdirat(frame.x[1])
            }
        }
        SYS_READAHEAD => {
            if fs::linux_native_handle(frame.x[0]).is_some() {
                0
            } else {
                EBADF
            }
        }
        SYS_MOUNT => fs::linux_mount(frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4]),
        SYS_UMOUNT2 => fs::linux_umount2(frame.x[0]),
        SYS_PIVOT_ROOT => 0,
        SYS_SYMLINKAT | SYS_LINKAT | SYS_RENAMEAT => EROFS,
        _ => ENOSYS,
    };
    frame.x[0] = result;
    uart::put_hex("aarch64 linux syscall nr=", frame.x[8]);
    uart::put_hex("aarch64 linux syscall ret=", result);
    true
}

fn dispatch_futex(frame: &mut Aarch64TrapFrame) -> bool {
    match futex(frame) {
        FutexAction::Return(result) => {
            frame.x[0] = result;
            true
        }
        FutexAction::Wait { key, timeout_us } => task::block_event(frame, key, timeout_us),
    }
}

fn fork(frame: &mut Aarch64TrapFrame) -> u64 {
    match allocator::with_global(|frames| task::fork(frames, frame)) {
        Some(Ok(pid)) => pid,
        Some(Err(error)) => error,
        None => ENOMEM,
    }
}

fn clone(frame: &mut Aarch64TrapFrame) -> u64 {
    const CLONE_VM: u64 = 0x0000_0100;
    const CLONE_FS: u64 = 0x0000_0200;
    const CLONE_FILES: u64 = 0x0000_0400;
    const CLONE_SIGHAND: u64 = 0x0000_0800;
    const CLONE_THREAD: u64 = 0x0001_0000;
    const CLONE_SYSVSEM: u64 = 0x0004_0000;
    const CLONE_SETTLS: u64 = 0x0008_0000;
    const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
    const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
    const CLONE_DETACHED: u64 = 0x0040_0000;
    const CLONE_CHILD_SETTID: u64 = 0x0100_0000;
    const SUPPORTED: u64 = CLONE_VM
        | CLONE_FS
        | CLONE_FILES
        | CLONE_SIGHAND
        | CLONE_THREAD
        | CLONE_SYSVSEM
        | CLONE_SETTLS
        | CLONE_PARENT_SETTID
        | CLONE_CHILD_CLEARTID
        | CLONE_DETACHED
        | CLONE_CHILD_SETTID;
    let flags = frame.x[0];
    if flags & !SUPPORTED != 0 {
        return ENOSYS;
    }
    if flags & CLONE_THREAD == 0 {
        if flags & CLONE_VM != 0 {
            return ENOSYS;
        }
        return fork(frame);
    }
    if flags & CLONE_VM == 0 {
        return EINVAL;
    }
    let parent_tid = if flags & CLONE_PARENT_SETTID != 0 {
        frame.x[2]
    } else {
        0
    };
    let child_tid = if flags & (CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID) != 0 {
        frame.x[4]
    } else {
        0
    };
    for address in [parent_tid, child_tid]
        .into_iter()
        .filter(|address| *address != 0)
    {
        let mut probe = [0u8; 4];
        if user_memory::copy_from_user(address, &mut probe).is_err() {
            return EFAULT;
        }
    }
    let tls = if flags & CLONE_SETTLS != 0 {
        frame.x[3]
    } else {
        frame.tpidr_el0
    };
    let clear_child_tid = if flags & CLONE_CHILD_CLEARTID != 0 {
        child_tid
    } else {
        0
    };
    let pid = match task::create_linux_thread(frame, frame.x[1], tls, clear_child_tid) {
        Ok(pid) => pid,
        Err(error) => return error,
    };
    if parent_tid != 0
        && user_memory::copy_to_user(parent_tid, &(pid as u32).to_ne_bytes()).is_err()
    {
        return EFAULT;
    }
    if child_tid != 0 && flags & CLONE_CHILD_SETTID != 0 {
        let _ = user_memory::copy_to_user(child_tid, &(pid as u32).to_ne_bytes());
    }
    pid
}

fn wait4(frame: &mut Aarch64TrapFrame) -> bool {
    const WNOHANG: u64 = 1;
    if frame.x[2] & !WNOHANG != 0 || frame.x[3] != 0 {
        frame.x[0] = ENOSYS;
        return true;
    }
    let requested = frame.x[0] as i64;
    let target = if requested <= 0 {
        u64::MAX
    } else {
        requested as u64
    };
    let status_address = frame.x[1];
    let nonblocking = frame.x[2] & WNOHANG != 0;
    let wait = |frames: &mut allocator::PhysicalFrameAllocator| {
        if nonblocking {
            task::wait_linux_nonblocking(frames, frame, target, status_address)
        } else {
            task::wait_linux_syscall(frames, frame, target, status_address)
        }
    };
    match allocator::with_global(wait) {
        Some(completed) => completed,
        None => {
            frame.x[0] = ENOMEM;
            true
        }
    }
}

fn waitid(frame: &mut Aarch64TrapFrame) -> bool {
    const P_ALL: u64 = 0;
    const P_PID: u64 = 1;
    const WNOHANG: u64 = 1;
    const WEXITED: u64 = 4;
    const SIGCHLD: i32 = 17;
    const CLD_EXITED: i32 = 1;
    const SIGINFO_SIZE: usize = 128;

    if !matches!(frame.x[0], P_ALL | P_PID)
        || (frame.x[0] == P_PID && frame.x[1] == 0)
        || frame.x[2] == 0
        || frame.x[3] & !(WNOHANG | WEXITED) != 0
        || frame.x[3] & WEXITED == 0
        || frame.x[4] != 0
        || frame.x[3] & WNOHANG == 0
    {
        frame.x[0] = ENOSYS;
        return true;
    }
    let target = if frame.x[0] == P_ALL {
        u64::MAX
    } else {
        frame.x[1]
    };
    let info_address = frame.x[2];
    let waited = match allocator::with_global(|frames| {
        task::wait_linux_nonblocking(frames, frame, target, info_address)
    }) {
        Some(completed) => completed,
        None => {
            frame.x[0] = ENOMEM;
            return true;
        }
    };
    if !waited || (frame.x[0] as i64) <= 0 {
        return waited;
    }

    let child_pid = frame.x[0] as i32;
    let mut wait_status = [0u8; 4];
    if user_memory::copy_from_user(info_address, &mut wait_status).is_err() {
        frame.x[0] = EFAULT;
        return true;
    }
    let mut info = [0u8; SIGINFO_SIZE];
    info[0..4].copy_from_slice(&SIGCHLD.to_ne_bytes());
    info[8..12].copy_from_slice(&CLD_EXITED.to_ne_bytes());
    info[16..20].copy_from_slice(&child_pid.to_ne_bytes());
    let exit_status = u32::from_ne_bytes(wait_status) >> 8;
    info[24..28].copy_from_slice(&(exit_status as i32).to_ne_bytes());
    if user_memory::copy_to_user(info_address, &info).is_err() {
        frame.x[0] = EFAULT;
        return true;
    }
    frame.x[0] = 0;
    true
}

fn kill_process(frame: &Aarch64TrapFrame) -> u64 {
    let target = frame.x[0] as i64;
    let signal = frame.x[1];
    match target {
        target if target > 0 => task::send_linux_process_signal(target as u64, signal),
        0 => {
            let Some(pgid) = task::current_linux_pgid() else {
                return ESRCH;
            };
            task::send_linux_process_group_signal(
                pgid,
                signal,
                task::current_process_pid() == Some(1),
            )
        }
        -1 => task::send_linux_all_processes_signal(signal),
        target if target < -1 => {
            let pgid = target.unsigned_abs();
            task::send_linux_process_group_signal(pgid, signal, false)
        }
        _ => EINVAL,
    }
}

fn kill_thread(frame: &Aarch64TrapFrame) -> u64 {
    let target = frame.x[0];
    let signal = frame.x[1];
    if target == 0 {
        return EINVAL;
    }
    task::send_linux_thread_signal(target, signal)
}

fn kill_thread_group(frame: &Aarch64TrapFrame) -> u64 {
    let tgid = frame.x[0];
    let tid = frame.x[1];
    let signal = frame.x[2];
    if tgid == 0 || tid == 0 || !task::linux_thread_belongs_to_process(tid, tgid) {
        return ESRCH;
    }
    task::send_linux_thread_signal(tid, signal)
}

fn requested_id(value: u64) -> Result<Option<u32>, u64> {
    if value == u64::MAX {
        Ok(None)
    } else {
        u32::try_from(value).map(Some).map_err(|_| EINVAL)
    }
}

fn current_credentials() -> Result<task::LinuxCredentials, u64> {
    task::current_linux_credentials().ok_or(ESRCH)
}

fn commit_credentials(credentials: task::LinuxCredentials) -> u64 {
    if task::set_current_linux_credentials(credentials) {
        0
    } else {
        ESRCH
    }
}

fn setuid(frame: &Aarch64TrapFrame) -> u64 {
    let uid = match requested_id(frame.x[0]) {
        Ok(Some(uid)) => uid,
        Ok(None) => return 0,
        Err(error) => return error,
    };
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    let privileged = credentials.effective_uid == 0;
    if !privileged
        && !matches!(
            uid,
            value if value == credentials.real_uid
                || value == credentials.effective_uid
                || value == credentials.saved_uid
        )
    {
        return EPERM;
    }
    if privileged {
        credentials.real_uid = uid;
        credentials.effective_uid = uid;
        credentials.saved_uid = uid;
    } else {
        credentials.effective_uid = uid;
    }
    credentials.fsuid = uid;
    commit_credentials(credentials)
}

fn setgid(frame: &Aarch64TrapFrame) -> u64 {
    let gid = match requested_id(frame.x[0]) {
        Ok(Some(gid)) => gid,
        Ok(None) => return 0,
        Err(error) => return error,
    };
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    let privileged = credentials.effective_uid == 0;
    if !privileged
        && !matches!(
            gid,
            value if value == credentials.real_gid
                || value == credentials.effective_gid
                || value == credentials.saved_gid
        )
    {
        return EPERM;
    }
    if privileged {
        credentials.real_gid = gid;
        credentials.effective_gid = gid;
        credentials.saved_gid = gid;
    } else {
        credentials.effective_gid = gid;
    }
    credentials.fsgid = gid;
    commit_credentials(credentials)
}

fn setreuid(frame: &Aarch64TrapFrame) -> u64 {
    set_re_identity(frame.x[0], frame.x[1], false)
}

fn setresuid(frame: &Aarch64TrapFrame) -> u64 {
    set_res_identity(frame.x[0], frame.x[1], frame.x[2], false)
}

fn setregid(frame: &Aarch64TrapFrame) -> u64 {
    set_re_identity(frame.x[0], frame.x[1], true)
}

fn setresgid(frame: &Aarch64TrapFrame) -> u64 {
    set_res_identity(frame.x[0], frame.x[1], frame.x[2], true)
}

fn set_re_identity(real: u64, effective: u64, groups: bool) -> u64 {
    let real = match requested_id(real) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let effective = match requested_id(effective) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    let privileged = credentials.effective_uid == 0;
    if groups {
        if !privileged {
            if real.is_some_and(|value| {
                value != credentials.real_gid && value != credentials.effective_gid
            }) || effective.is_some_and(|value| {
                value != credentials.real_gid
                    && value != credentials.effective_gid
                    && value != credentials.saved_gid
            }) {
                return EPERM;
            }
        }
        let old_real = credentials.real_gid;
        if let Some(value) = real {
            credentials.real_gid = value;
        }
        if let Some(value) = effective {
            credentials.effective_gid = value;
        }
        if privileged && (real.is_some() || effective.is_some_and(|value| value != old_real)) {
            credentials.saved_gid = credentials.effective_gid;
        }
        credentials.fsgid = credentials.effective_gid;
    } else {
        if !privileged {
            if real.is_some_and(|value| {
                value != credentials.real_uid && value != credentials.effective_uid
            }) || effective.is_some_and(|value| {
                value != credentials.real_uid
                    && value != credentials.effective_uid
                    && value != credentials.saved_uid
            }) {
                return EPERM;
            }
        }
        let old_real = credentials.real_uid;
        if let Some(value) = real {
            credentials.real_uid = value;
        }
        if let Some(value) = effective {
            credentials.effective_uid = value;
        }
        if privileged && (real.is_some() || effective.is_some_and(|value| value != old_real)) {
            credentials.saved_uid = credentials.effective_uid;
        }
        credentials.fsuid = credentials.effective_uid;
    }
    commit_credentials(credentials)
}

fn set_res_identity(real: u64, effective: u64, saved: u64, groups: bool) -> u64 {
    let real = match requested_id(real) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let effective = match requested_id(effective) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let saved = match requested_id(saved) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    let privileged = credentials.effective_uid == 0;
    if groups {
        if !privileged
            && [real, effective, saved].into_iter().flatten().any(|value| {
                value != credentials.real_gid
                    && value != credentials.effective_gid
                    && value != credentials.saved_gid
            })
        {
            return EPERM;
        }
        if let Some(value) = real {
            credentials.real_gid = value;
        }
        if let Some(value) = effective {
            credentials.effective_gid = value;
        }
        if let Some(value) = saved {
            credentials.saved_gid = value;
        }
        credentials.fsgid = credentials.effective_gid;
    } else {
        if !privileged
            && [real, effective, saved].into_iter().flatten().any(|value| {
                value != credentials.real_uid
                    && value != credentials.effective_uid
                    && value != credentials.saved_uid
            })
        {
            return EPERM;
        }
        if let Some(value) = real {
            credentials.real_uid = value;
        }
        if let Some(value) = effective {
            credentials.effective_uid = value;
        }
        if let Some(value) = saved {
            credentials.saved_uid = value;
        }
        credentials.fsuid = credentials.effective_uid;
    }
    commit_credentials(credentials)
}

fn setfsuid(frame: &Aarch64TrapFrame) -> u64 {
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    let old = credentials.fsuid;
    let requested = match requested_id(frame.x[0]) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Some(uid) = requested else {
        return old as u64;
    };
    if credentials.effective_uid == 0
        || matches!(
            uid,
            value if value == credentials.real_uid
                || value == credentials.effective_uid
                || value == credentials.saved_uid
                || value == credentials.fsuid
        )
    {
        credentials.fsuid = uid;
        if commit_credentials(credentials) != 0 {
            return ESRCH;
        }
    }
    old as u64
}

fn setfsgid(frame: &Aarch64TrapFrame) -> u64 {
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    let old = credentials.fsgid;
    let requested = match requested_id(frame.x[0]) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let Some(gid) = requested else {
        return old as u64;
    };
    if credentials.effective_uid == 0
        || matches!(
            gid,
            value if value == credentials.real_gid
                || value == credentials.effective_gid
                || value == credentials.saved_gid
                || value == credentials.fsgid
        )
    {
        credentials.fsgid = gid;
        if commit_credentials(credentials) != 0 {
            return ESRCH;
        }
    }
    old as u64
}

fn getuid() -> u64 {
    current_credentials()
        .map(|credentials| credentials.real_uid as u64)
        .unwrap_or(ESRCH)
}

fn geteuid() -> u64 {
    current_credentials()
        .map(|credentials| credentials.effective_uid as u64)
        .unwrap_or(ESRCH)
}

fn getgid() -> u64 {
    current_credentials()
        .map(|credentials| credentials.real_gid as u64)
        .unwrap_or(ESRCH)
}

fn getegid() -> u64 {
    current_credentials()
        .map(|credentials| credentials.effective_gid as u64)
        .unwrap_or(ESRCH)
}

fn setpgid(frame: &Aarch64TrapFrame) -> u64 {
    task::set_linux_pgid(frame.x[0], frame.x[1])
}

fn getpgid(frame: &Aarch64TrapFrame) -> u64 {
    task::linux_pgid_for(frame.x[0])
}

fn getsid(frame: &Aarch64TrapFrame) -> u64 {
    task::linux_sid_for(frame.x[0])
}

fn times(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] == 0 {
        return EFAULT;
    }
    let bytes = [0u8; 32];
    if user_memory::copy_to_user(frame.x[0], &bytes).is_err() {
        EFAULT
    } else {
        timer::uptime_us() / 10_000
    }
}

fn getrusage(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[1] == 0 {
        return EFAULT;
    }
    let bytes = [0u8; 144];
    if user_memory::copy_to_user(frame.x[1], &bytes).is_err() {
        EFAULT
    } else {
        0
    }
}

fn getcpu(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] != 0 && user_memory::copy_to_user(frame.x[0], &0u32.to_ne_bytes()).is_err() {
        return EFAULT;
    }
    if frame.x[1] != 0 && user_memory::copy_to_user(frame.x[1], &0u32.to_ne_bytes()).is_err() {
        return EFAULT;
    }
    0
}

fn gettimeofday(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] == 0 {
        return EFAULT;
    }
    let microseconds = timer::uptime_us();
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&(microseconds / 1_000_000).to_ne_bytes());
    bytes[8..].copy_from_slice(&(microseconds % 1_000_000).to_ne_bytes());
    if user_memory::copy_to_user(frame.x[0], &bytes).is_err() {
        EFAULT
    } else {
        0
    }
}

fn getrandom(_frame: &Aarch64TrapFrame) -> u64 {
    // A timestamp-seeded xorshift is predictable and must not be exposed as
    // Linux getrandom(). Keep the ABI honest until a CSPRNG is available.
    ENOSYS
}

fn read(frame: &Aarch64TrapFrame) -> u64 {
    match fs::linux_stdio_fd(frame.x[0]) {
        Some(0) => return EAGAIN,
        Some(_) => return EBADF,
        None => {}
    }
    if let Some(native_handle) = fs::linux_native_handle(frame.x[0]) {
        return fs::read(native_handle, frame.x[1], frame.x[2]);
    }
    if fs::linux_eventfd_slot(frame.x[0]).is_some() {
        return fs::linux_eventfd_read(frame.x[0], frame.x[1], frame.x[2]);
    }
    if fs::linux_inotify_slot(frame.x[0]).is_some() {
        return fs::linux_inotify_read(frame.x[0], frame.x[1], frame.x[2]);
    }
    if fs::linux_signalfd_slot(frame.x[0]).is_some() {
        return fs::linux_signalfd_read(frame.x[0], frame.x[1], frame.x[2]);
    }
    if fs::linux_socket_slot(frame.x[0]).is_some() {
        return fs::linux_socket_read(frame.x[0], frame.x[1], frame.x[2]);
    }
    EBADF
}

fn pread64(frame: &Aarch64TrapFrame) -> u64 {
    let count = usize::try_from(frame.x[2]).unwrap_or(usize::MAX);
    if count > 4096 {
        return EINVAL;
    }
    if count == 0 {
        return 0;
    }
    let Some(native_handle) = fs::linux_native_handle(frame.x[0]) else {
        return EBADF;
    };
    let mut buffer = [0u8; 4096];
    let read = match fs::read_kernel_at(native_handle, frame.x[3], &mut buffer[..count]) {
        Ok(read) => read,
        Err(error) => return error,
    };
    if user_memory::copy_to_user(frame.x[1], &buffer[..read]).is_err() {
        EFAULT
    } else {
        read as u64
    }
}

fn readlinkat(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] as i64 != AT_FDCWD {
        return ENOSYS;
    }
    let requested = usize::try_from(frame.x[3]).unwrap_or(usize::MAX);
    if requested == 0 {
        return 0;
    }
    if frame.x[2] == 0 {
        return EFAULT;
    }
    let mut target = [0u8; 4096];
    let length = match fs::read_link_path(frame.x[1], &mut target) {
        Ok(length) => length,
        Err(error) => return error,
    };
    let count = length.min(requested);
    if user_memory::copy_to_user(frame.x[2], &target[..count]).is_err() {
        EFAULT
    } else {
        count as u64
    }
}

fn write(frame: &Aarch64TrapFrame) -> u64 {
    if let Some(stdio_fd) = fs::linux_stdio_fd(frame.x[0]) {
        if stdio_fd != 1 && stdio_fd != 2 {
            return EBADF;
        }
        return write_stdio(frame);
    }
    if let Some(native_handle) = fs::linux_native_handle(frame.x[0]) {
        return fs::write(native_handle, frame.x[1], frame.x[2]);
    }
    if fs::linux_eventfd_slot(frame.x[0]).is_some() {
        return fs::linux_eventfd_write(frame.x[0], frame.x[1], frame.x[2]);
    }
    if fs::linux_socket_slot(frame.x[0]).is_some() {
        return fs::linux_socket_write(frame.x[0], frame.x[1], frame.x[2]);
    }
    EBADF
}

fn readv(frame: &Aarch64TrapFrame) -> u64 {
    vectored_io(frame, true)
}

fn writev(frame: &Aarch64TrapFrame) -> u64 {
    vectored_io(frame, false)
}

#[derive(Clone, Copy)]
struct LinuxMessageHeader {
    name_address: u64,
    name_length: u32,
    iovec_address: u64,
    iovec_count: usize,
    control_address: u64,
    control_length: usize,
}

fn message_header(address: u64) -> Result<LinuxMessageHeader, u64> {
    if address == 0 {
        return Err(EFAULT);
    }
    let mut bytes = [0u8; MSGHDR_SIZE];
    if user_memory::copy_from_user(address, &mut bytes).is_err() {
        return Err(EFAULT);
    }
    let iovec_count = usize::try_from(u64::from_ne_bytes(bytes[24..32].try_into().unwrap()))
        .map_err(|_| EINVAL)?;
    if iovec_count > MAX_MSG_IOVECS {
        return Err(EINVAL);
    }
    let control_length = usize::try_from(u64::from_ne_bytes(bytes[40..48].try_into().unwrap()))
        .map_err(|_| EINVAL)?;
    if control_length > MAX_MSG_CONTROL {
        return Err(EINVAL);
    }
    Ok(LinuxMessageHeader {
        name_address: u64::from_ne_bytes(bytes[..8].try_into().unwrap()),
        name_length: u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
        iovec_address: u64::from_ne_bytes(bytes[16..24].try_into().unwrap()),
        iovec_count,
        control_address: u64::from_ne_bytes(bytes[32..40].try_into().unwrap()),
        control_length,
    })
}

fn validate_message_header(header: LinuxMessageHeader) -> Result<(), u64> {
    if header.name_length != 0 {
        return Err(ENOSYS);
    }
    if header.iovec_count != 0 && header.iovec_address == 0 {
        return Err(EFAULT);
    }
    if header.control_length != 0 && header.control_address == 0 {
        return Err(EFAULT);
    }
    Ok(())
}

fn message_iovec(address: u64) -> Result<(u64, usize), u64> {
    let mut bytes = [0u8; MSG_IOVEC_SIZE as usize];
    if user_memory::copy_from_user(address, &mut bytes).is_err() {
        return Err(EFAULT);
    }
    let base = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
    let length =
        usize::try_from(u64::from_ne_bytes(bytes[8..].try_into().unwrap())).map_err(|_| EINVAL)?;
    if length != 0 && base.checked_add((length - 1) as u64).is_none() {
        return Err(EFAULT);
    }
    Ok((base, length))
}

fn message_io(frame: &Aarch64TrapFrame, receiving: bool) -> u64 {
    let supported_flags = MSG_DONTWAIT | MSG_NOSIGNAL | MSG_CMSG_CLOEXEC;
    if frame.x[2] & !supported_flags != 0 {
        return EINVAL;
    }
    if fs::linux_socket_slot(frame.x[0]).is_none() {
        return ENOTSOCK;
    }
    let header = match message_header(frame.x[1]) {
        Ok(header) => header,
        Err(error) => return error,
    };
    if let Err(error) = validate_message_header(header) {
        return error;
    }
    let mut total = 0u64;
    for index in 0..header.iovec_count {
        let Some(address) = header
            .iovec_address
            .checked_add((index as u64).saturating_mul(MSG_IOVEC_SIZE))
        else {
            return if total == 0 { EFAULT } else { total };
        };
        let (base, length) = match message_iovec(address) {
            Ok(value) => value,
            Err(error) => return if total == 0 { error } else { total },
        };
        let mut offset = 0usize;
        while offset < length {
            let chunk = (length - offset).min(MAX_MSG_IO);
            let Some(buffer_address) = base.checked_add(offset as u64) else {
                return if total == 0 { EFAULT } else { total };
            };
            let result = if receiving {
                fs::linux_socket_read(frame.x[0], buffer_address, chunk as u64)
            } else {
                fs::linux_socket_write(frame.x[0], buffer_address, chunk as u64)
            };
            if (result as i64) < 0 {
                return if total == 0 { result } else { total };
            }
            let completed = usize::try_from(result).unwrap_or(usize::MAX);
            total = total.saturating_add(result);
            offset = offset.saturating_add(completed);
            if completed < chunk {
                return total;
            }
        }
    }
    if receiving {
        write_message_credentials(frame.x[1], header, fs::linux_socket_passcred(frame.x[0]));
    }
    total
}

fn sendmsg(frame: &Aarch64TrapFrame) -> u64 {
    message_io(frame, false)
}

fn recvmsg(frame: &Aarch64TrapFrame) -> u64 {
    message_io(frame, true)
}

fn write_message_credentials(message_address: u64, header: LinuxMessageHeader, passcred: bool) {
    if message_address == 0 {
        return;
    }
    if let Some(flags_address) = message_address.checked_add(48) {
        let _ = user_memory::copy_to_user(flags_address, &0u32.to_ne_bytes());
    }
    if !passcred || header.control_address == 0 || header.control_length == 0 {
        return;
    }
    // The early property-service boundary only needs a root credential. The
    // cmsg is encoded with the AArch64 Linux alignment used by CMSG_SPACE.
    if header.control_length < 32 {
        if let Some(flags_address) = message_address.checked_add(48) {
            let _ = user_memory::copy_to_user(flags_address, &MSG_CTRUNC.to_ne_bytes());
        }
        return;
    }
    let mut control = [0u8; 32];
    control[..8].copy_from_slice(&28u64.to_ne_bytes());
    control[8..12].copy_from_slice(&SOL_SOCKET.to_ne_bytes());
    control[12..16].copy_from_slice(&SCM_CREDENTIALS.to_ne_bytes());
    // pid, uid, gid: the Android-init bring-up runs the bounded root domain.
    control[16..20].copy_from_slice(&1u32.to_ne_bytes());
    control[20..24].copy_from_slice(&0u32.to_ne_bytes());
    control[24..28].copy_from_slice(&0u32.to_ne_bytes());
    let _ = user_memory::copy_to_user(header.control_address, &control);
}

/// Process a bounded Linux AArch64 `struct iovec` array.  The underlying
/// Fullerene read/write boundary already caps each transfer at 4 KiB, so a
/// large iovec is split into those same chunks and a short/error result keeps
/// normal POSIX partial-I/O behavior.
fn vectored_io(frame: &Aarch64TrapFrame, reading: bool) -> u64 {
    const MAX_IOVECS: usize = 64;
    const IOVEC_SIZE: u64 = 16;
    let count = match usize::try_from(frame.x[2]) {
        Ok(count) if count <= MAX_IOVECS => count,
        _ => return EINVAL,
    };
    if count == 0 {
        return 0;
    }
    let mut total = 0u64;
    for index in 0..count {
        let Some(iovec_address) = frame.x[1].checked_add((index as u64).saturating_mul(IOVEC_SIZE))
        else {
            return if total == 0 { EFAULT } else { total };
        };
        let mut bytes = [0u8; IOVEC_SIZE as usize];
        if user_memory::copy_from_user(iovec_address, &mut bytes).is_err() {
            return if total == 0 { EFAULT } else { total };
        }
        let base = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
        let length = u64::from_ne_bytes(bytes[8..].try_into().unwrap());
        let Ok(length) = usize::try_from(length) else {
            return if total == 0 { EINVAL } else { total };
        };
        if length != 0 && base.checked_add((length - 1) as u64).is_none() {
            return if total == 0 { EFAULT } else { total };
        }
        let mut offset = 0usize;
        while offset < length {
            let chunk = (length - offset).min(4096);
            let Some(address) = base.checked_add(offset as u64) else {
                return if total == 0 { EFAULT } else { total };
            };
            let mut call = *frame;
            call.x[1] = address;
            call.x[2] = chunk as u64;
            let result = if reading { read(&call) } else { write(&call) };
            if (result as i64) < 0 {
                return if total == 0 { result } else { total };
            }
            total = total.saturating_add(result);
            let completed = usize::try_from(result).unwrap_or(usize::MAX);
            offset = offset.saturating_add(completed);
            if completed < chunk {
                return total;
            }
        }
    }
    total
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

fn ioctl(frame: &Aarch64TrapFrame) -> u64 {
    const FIONBIO: u64 = 0x5421;
    const FIONREAD: u64 = 0x541b;
    const TIOCINQ: u64 = 0x541b;
    const TIOCGWINSZ: u64 = 0x5413;
    const TIOCGETD: u64 = 0x5424;
    const TIOCSCTTY: u64 = 0x540e;
    const TIOCEXCL: u64 = 0x540c;
    const TIOCNXCL: u64 = 0x540d;
    const FIOCLEX: u64 = 0x5451;
    const FIONCLEX: u64 = 0x5450;
    const SIOCGIFFLAGS: u64 = 0x8913;
    const SIOCSIFFLAGS: u64 = 0x8914;
    const FS_IOC_GETFLAGS: u64 = 0x8008_6601;
    const FS_IOC_SETFLAGS: u64 = 0x4008_6602;
    const BLKROGET: u64 = 0x125e;
    const BLKGETSIZE64: u64 = 0x8008_1272;
    let fd = frame.x[0];
    let request = frame.x[1];
    let argument = frame.x[2];
    if argument == 0 {
        return EFAULT;
    }
    match request {
        FIONBIO => {
            let mut bytes = [0u8; 4];
            if user_memory::copy_from_user(argument, &mut bytes).is_err() {
                return EFAULT;
            }
            fs::linux_set_nonblocking(fd, u32::from_ne_bytes(bytes) != 0)
        }
        FIONREAD | TIOCINQ => {
            if user_memory::copy_to_user(argument, &0u32.to_ne_bytes()).is_err() {
                EFAULT
            } else {
                0
            }
        }
        TIOCGWINSZ => {
            let mut winsize = [0u8; 8];
            winsize[..2].copy_from_slice(&24u16.to_ne_bytes());
            winsize[2..4].copy_from_slice(&80u16.to_ne_bytes());
            if user_memory::copy_to_user(argument, &winsize).is_err() {
                EFAULT
            } else {
                0
            }
        }
        TIOCGETD => {
            if user_memory::copy_to_user(argument, &0u32.to_ne_bytes()).is_err() {
                EFAULT
            } else {
                0
            }
        }
        SIOCGIFFLAGS => {
            if fs::linux_socket_slot(fd).is_none() {
                return ENOTSOCK;
            }
            let mut ifreq = [0u8; 40];
            if user_memory::copy_from_user(argument, &mut ifreq).is_err() {
                return EFAULT;
            }
            // A real netdevice is not exposed yet; report the loopback-like
            // administrative/running bits so init's flag probe is stable.
            ifreq[16..18].copy_from_slice(&0x0041u16.to_ne_bytes());
            if user_memory::copy_to_user(argument, &ifreq).is_err() {
                EFAULT
            } else {
                0
            }
        }
        SIOCSIFFLAGS => {
            if fs::linux_socket_slot(fd).is_none() {
                return ENOTSOCK;
            }
            let mut ifreq = [0u8; 40];
            if user_memory::copy_from_user(argument, &mut ifreq).is_err() {
                EFAULT
            } else {
                0
            }
        }
        FIOCLEX => fs::linux_fcntl(fd, 2, 1),
        FIONCLEX => fs::linux_fcntl(fd, 2, 0),
        TIOCSCTTY | TIOCEXCL | TIOCNXCL => 0,
        FS_IOC_GETFLAGS => {
            if user_memory::copy_to_user(argument, &0u32.to_ne_bytes()).is_err() {
                EFAULT
            } else {
                0
            }
        }
        FS_IOC_SETFLAGS => EROFS,
        BLKROGET => {
            if user_memory::copy_to_user(argument, &1i32.to_ne_bytes()).is_err() {
                EFAULT
            } else {
                0
            }
        }
        BLKGETSIZE64 => {
            if user_memory::copy_to_user(argument, &0u64.to_ne_bytes()).is_err() {
                EFAULT
            } else {
                0
            }
        }
        _ => ENOTTY,
    }
}

fn openat(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] as i64 != AT_FDCWD {
        return ENOSYS;
    }
    let flags = frame.x[2];
    let device_path = fs::is_device_path(frame.x[1]).unwrap_or(false);
    let writable_virtual_path = fs::is_writable_virtual_path(frame.x[1]).unwrap_or(false);
    if (!device_path && !writable_virtual_path && flags & O_ACCMODE != 0)
        || (!device_path
            && !writable_virtual_path
            && flags & (O_CREAT | O_EXCL | O_TRUNC | O_APPEND) != 0)
    {
        return EROFS;
    }
    let native_handle = fs::open(frame.x[1], flags, frame.x[3]);
    if (native_handle as i64) < 0 {
        return native_handle;
    }
    fs::linux_install_handle(native_handle, flags)
}

fn faccessat(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] as i64 != AT_FDCWD || frame.x[3] & !0x100 != 0 {
        return ENOSYS;
    }
    match fs::path_exists(frame.x[1]) {
        Ok(true) => 0,
        Ok(false) => ENOENT,
        Err(error) => error,
    }
}

fn lseek(frame: &Aarch64TrapFrame) -> u64 {
    let Some(native_handle) = fs::linux_native_handle(frame.x[0]) else {
        return EBADF;
    };
    fs::seek(native_handle, frame.x[1] as i64, frame.x[2])
}

fn fstat(frame: &Aarch64TrapFrame) -> u64 {
    let Some(native_handle) = fs::linux_native_handle(frame.x[0]) else {
        return EBADF;
    };
    let Ok(metadata) = fs::file_metadata(native_handle) else {
        return EBADF;
    };
    write_regular_stat(frame.x[1], metadata)
}

fn fstatat(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] as i64 != AT_FDCWD {
        return ENOSYS;
    }
    let metadata = match fs::path_metadata(frame.x[1]) {
        Ok(metadata) => metadata,
        Err(error) => return error,
    };
    write_regular_stat(frame.x[2], metadata)
}

fn write_regular_stat(destination: u64, metadata: FileMetadata) -> u64 {
    if destination == 0 {
        return EFAULT;
    }
    // Linux AArch64's asm-generic struct stat is 128 bytes.  The bounded
    // virtual MemFileSystem supplies real mode/uid/gid values; read-only
    // filesystems use the conservative metadata fallback from fs.rs.
    let size = if metadata.kind == genome::vfs::InodeType::Directory {
        0
    } else {
        metadata.size
    };
    let mut stat = [0u8; 128];
    stat[16..20].copy_from_slice(&metadata.mode.to_ne_bytes());
    stat[20..24].copy_from_slice(&1u32.to_ne_bytes());
    stat[24..28].copy_from_slice(&metadata.uid.to_ne_bytes());
    stat[28..32].copy_from_slice(&metadata.gid.to_ne_bytes());
    stat[48..56].copy_from_slice(&size.to_ne_bytes());
    stat[56..60].copy_from_slice(&(4096i32).to_ne_bytes());
    stat[64..72].copy_from_slice(&(size.div_ceil(4096) * 8).to_ne_bytes());
    if user_memory::copy_to_user(destination, &stat).is_err() {
        EFAULT
    } else {
        0
    }
}

fn statx(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] as i64 != AT_FDCWD || frame.x[4] == 0 {
        return ENOSYS;
    }
    let metadata = match fs::path_metadata(frame.x[1]) {
        Ok(metadata) => metadata,
        Err(error) => return error,
    };
    let size = if metadata.kind == genome::vfs::InodeType::Directory {
        0
    } else {
        metadata.size
    };
    let mut statx = [0u8; 256];
    statx[0..4].copy_from_slice(&0x0000_07ffu32.to_ne_bytes());
    statx[4..8].copy_from_slice(&4096u32.to_ne_bytes());
    statx[16..20].copy_from_slice(&1u32.to_ne_bytes());
    statx[20..24].copy_from_slice(&metadata.uid.to_ne_bytes());
    statx[24..28].copy_from_slice(&metadata.gid.to_ne_bytes());
    statx[28..30].copy_from_slice(&(metadata.mode as u16).to_ne_bytes());
    statx[32..40].copy_from_slice(&1u64.to_ne_bytes());
    statx[40..48].copy_from_slice(&size.to_ne_bytes());
    statx[48..56].copy_from_slice(&(size.div_ceil(4096) * 8).to_ne_bytes());
    if user_memory::copy_to_user(frame.x[4], &statx).is_err() {
        EFAULT
    } else {
        0
    }
}

fn write_rlimit(destination: u64, resource: u64) -> u64 {
    if destination == 0 {
        return EFAULT;
    }
    let limit = if resource == 7 { 32u64 } else { u64::MAX };
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&limit.to_ne_bytes());
    bytes[8..].copy_from_slice(&limit.to_ne_bytes());
    if user_memory::copy_to_user(destination, &bytes).is_err() {
        EFAULT
    } else {
        0
    }
}

fn getrlimit(frame: &Aarch64TrapFrame) -> u64 {
    write_rlimit(frame.x[1], frame.x[0])
}

fn setrlimit(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[1] == 0 {
        return EFAULT;
    }
    let mut bytes = [0u8; 16];
    if user_memory::copy_from_user(frame.x[1], &mut bytes).is_err() {
        return EFAULT;
    }
    let soft = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
    let hard = u64::from_ne_bytes(bytes[8..].try_into().unwrap());
    if soft > hard || (frame.x[0] == 7 && hard > 32) {
        return EPERM;
    }
    0
}

fn prlimit64(frame: &Aarch64TrapFrame) -> u64 {
    let current = task::current_process_pid().unwrap_or(0);
    if frame.x[0] != 0 && frame.x[0] != current {
        return (-(3i64)) as u64;
    }
    if frame.x[3] != 0 {
        let result = write_rlimit(frame.x[3], frame.x[1]);
        if (result as i64) < 0 {
            return result;
        }
    }
    if frame.x[2] != 0 {
        let mut bytes = [0u8; 16];
        if user_memory::copy_from_user(frame.x[2], &mut bytes).is_err() {
            return EFAULT;
        }
        let soft = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
        let hard = u64::from_ne_bytes(bytes[8..].try_into().unwrap());
        if soft > hard || (frame.x[1] == 7 && hard > 32) {
            return EPERM;
        }
    }
    0
}

fn getgroups(frame: &Aarch64TrapFrame) -> u64 {
    let size = usize::try_from(frame.x[0]).unwrap_or(usize::MAX);
    let Ok(credentials) = current_credentials() else {
        return ESRCH;
    };
    let count = credentials.supplementary_group_count;
    if size == 0 {
        return count as u64;
    }
    if size < count || (count != 0 && frame.x[1] == 0) {
        return EINVAL;
    }
    let mut bytes = [0u8; task::MAX_LINUX_SUPPLEMENTARY_GROUPS * 4];
    for (index, group) in credentials
        .supplementary_groups
        .iter()
        .copied()
        .take(count)
        .enumerate()
    {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&group.to_ne_bytes());
    }
    if count != 0 && user_memory::copy_to_user(frame.x[1], &bytes[..count * 4]).is_err() {
        EFAULT
    } else {
        count as u64
    }
}

fn setgroups(frame: &Aarch64TrapFrame) -> u64 {
    let size = usize::try_from(frame.x[0]).unwrap_or(usize::MAX);
    if size > task::MAX_LINUX_SUPPLEMENTARY_GROUPS {
        return EINVAL;
    }
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    if credentials.effective_uid != 0 {
        return EPERM;
    }
    let mut groups = [0u32; task::MAX_LINUX_SUPPLEMENTARY_GROUPS];
    if size != 0 {
        if frame.x[1] == 0 {
            return EFAULT;
        }
        let mut bytes = [0u8; task::MAX_LINUX_SUPPLEMENTARY_GROUPS * 4];
        if user_memory::copy_from_user(frame.x[1], &mut bytes[..size * 4]).is_err() {
            return EFAULT;
        }
        for (index, group) in groups.iter_mut().take(size).enumerate() {
            *group = u32::from_ne_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
        }
    }
    credentials.supplementary_groups = groups;
    credentials.supplementary_group_count = size;
    commit_credentials(credentials)
}

fn sched_getaffinity(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[1] < 8 || frame.x[1] > 128 || frame.x[2] == 0 {
        return EINVAL;
    }
    if frame.x[0] != 0 && frame.x[0] != task::current_process_pid().unwrap_or(0) {
        return (-(3i64)) as u64;
    }
    if user_memory::copy_to_user(frame.x[2], &1u64.to_ne_bytes()).is_err() {
        EFAULT
    } else {
        8
    }
}

fn sysinfo(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[0] == 0 {
        return EFAULT;
    }
    let mut bytes = [0u8; 112];
    bytes[..8].copy_from_slice(&0u64.to_ne_bytes());
    bytes[32..40].copy_from_slice(&(256 * 1024u64).to_ne_bytes());
    bytes[40..48].copy_from_slice(&(128 * 1024u64).to_ne_bytes());
    bytes[48..56].copy_from_slice(&(128 * 1024u64).to_ne_bytes());
    bytes[64..68].copy_from_slice(&1u32.to_ne_bytes());
    if user_memory::copy_to_user(frame.x[0], &bytes).is_err() {
        EFAULT
    } else {
        0
    }
}

fn close_range(frame: &Aarch64TrapFrame) -> u64 {
    const CLOSE_RANGE_CLOEXEC: u64 = 2;
    if frame.x[2] & !CLOSE_RANGE_CLOEXEC != 0 || frame.x[0] > frame.x[1] {
        return EINVAL;
    }
    let first = frame.x[0].max(3);
    let last = frame.x[1].min(34);
    if first > last {
        return 0;
    }
    for fd in first..=last {
        let result = if frame.x[2] == CLOSE_RANGE_CLOEXEC {
            fs::linux_fcntl(fd, 2, 1)
        } else {
            fs::linux_close(fd)
        };
        if (result as i64) < 0 && result != EBADF {
            return result;
        }
    }
    0
}

fn mremap(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[3] != 0 || frame.x[0] == 0 || frame.x[1] == 0 || frame.x[2] == 0 {
        return ENOSYS;
    }
    if frame.x[2] <= frame.x[1] {
        frame.x[0]
    } else {
        ENOSYS
    }
}

fn getresid(frame: &Aarch64TrapFrame, groups: bool) -> u64 {
    let Ok(credentials) = current_credentials() else {
        return ESRCH;
    };
    let values = if groups {
        [
            credentials.real_gid,
            credentials.effective_gid,
            credentials.saved_gid,
        ]
    } else {
        [
            credentials.real_uid,
            credentials.effective_uid,
            credentials.saved_uid,
        ]
    };
    for (destination, value) in [frame.x[0], frame.x[1], frame.x[2]].into_iter().zip(values) {
        if destination == 0 || user_memory::copy_to_user(destination, &value.to_ne_bytes()).is_err()
        {
            return EFAULT;
        }
    }
    0
}

fn statfs(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[1] == 0 {
        return EFAULT;
    }
    if let Err(error) = fs::path_exists(frame.x[0]) {
        return error;
    }
    write_statfs(frame.x[1])
}

fn fstatfs(frame: &Aarch64TrapFrame) -> u64 {
    if frame.x[1] == 0 {
        return EFAULT;
    }
    if fs::linux_native_handle(frame.x[0]).is_none() {
        return EBADF;
    }
    write_statfs(frame.x[1])
}

fn write_statfs(destination: u64) -> u64 {
    // struct statfs on AArch64 is 120 bytes. The read-only VFS presents a
    // stable ext4-like block geometry to bionic's filesystem probes.
    let mut stat = [0u8; 120];
    stat[0..8].copy_from_slice(&(0xEF53u64).to_ne_bytes());
    stat[8..16].copy_from_slice(&(4096u64).to_ne_bytes());
    stat[16..24].copy_from_slice(&(1u64 << 20).to_ne_bytes());
    stat[24..32].copy_from_slice(&(1u64 << 19).to_ne_bytes());
    stat[32..40].copy_from_slice(&(1u64 << 19).to_ne_bytes());
    stat[40..48].copy_from_slice(&(1u64 << 20).to_ne_bytes());
    stat[48..56].copy_from_slice(&(1u64 << 19).to_ne_bytes());
    stat[64..72].copy_from_slice(&(255u64).to_ne_bytes());
    stat[72..80].copy_from_slice(&(4096u64).to_ne_bytes());
    if user_memory::copy_to_user(destination, &stat).is_err() {
        EFAULT
    } else {
        0
    }
}

fn capget(frame: &Aarch64TrapFrame) -> u64 {
    let Some((pid, _version)) = capability_header(frame.x[0]) else {
        return EFAULT;
    };
    if pid != 0 && task::current_process_pid() != Some(pid) {
        return EPERM;
    }
    if frame.x[1] == 0 {
        return EFAULT;
    }
    let Ok(credentials) = current_credentials() else {
        return ESRCH;
    };
    let data = capability_data(
        credentials.cap_effective,
        credentials.cap_permitted,
        credentials.cap_inheritable,
    );
    if user_memory::copy_to_user(frame.x[1], &data).is_err() {
        EFAULT
    } else {
        0
    }
}

fn capset(frame: &Aarch64TrapFrame) -> u64 {
    let Some((pid, _version)) = capability_header(frame.x[0]) else {
        return EFAULT;
    };
    if pid != 0 && task::current_process_pid() != Some(pid) {
        return EPERM;
    }
    if frame.x[1] == 0 {
        return EFAULT;
    }
    let mut data = [0u8; 24];
    if user_memory::copy_from_user(frame.x[1], &mut data).is_err() {
        return EFAULT;
    }
    let effective = capability_mask(&data, 0);
    let permitted = capability_mask(&data, 4);
    let inheritable = capability_mask(&data, 8);
    let Ok(mut credentials) = current_credentials() else {
        return ESRCH;
    };
    let privileged = credentials.effective_uid == 0;
    if effective & !permitted != 0
        || permitted & !credentials.cap_bounding != 0
        || inheritable & !credentials.cap_bounding != 0
    {
        return EPERM;
    }
    if !privileged
        && ((permitted & !credentials.cap_permitted) != 0
            || (effective & !credentials.cap_permitted) != 0
            || (inheritable & !credentials.cap_inheritable) != 0)
    {
        return EPERM;
    }
    credentials.cap_effective = effective;
    credentials.cap_permitted = permitted;
    credentials.cap_inheritable = inheritable;
    commit_credentials(credentials)
}

fn capability_header(address: u64) -> Option<(u64, u32)> {
    if address == 0 {
        return None;
    }
    let mut bytes = [0u8; 8];
    user_memory::copy_from_user(address, &mut bytes).ok()?;
    let version = u32::from_ne_bytes(bytes[..4].try_into().ok()?);
    let pid = i32::from_ne_bytes(bytes[4..].try_into().ok()?);
    (version == 0x2008_0522 && pid >= 0).then_some((pid as u64, version))
}

fn capability_data(effective: u64, permitted: u64, inheritable: u64) -> [u8; 24] {
    let mut data = [0u8; 24];
    for (offset, value) in [(0usize, effective), (4, permitted), (8, inheritable)] {
        data[offset..offset + 4].copy_from_slice(&(value as u32).to_ne_bytes());
        data[offset + 12..offset + 16].copy_from_slice(&((value >> 32) as u32).to_ne_bytes());
    }
    data
}

fn capability_mask(data: &[u8; 24], offset: usize) -> u64 {
    u32::from_ne_bytes(data[offset..offset + 4].try_into().unwrap()) as u64
        | ((u32::from_ne_bytes(data[offset + 12..offset + 16].try_into().unwrap()) as u64) << 32)
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
    let shared = flags & MAP_SHARED != 0;
    let private = flags & MAP_PRIVATE != 0;
    if shared && private {
        return EINVAL;
    }
    if !shared && !private {
        return EINVAL;
    }
    if flags & !(MAP_PRIVATE | MAP_SHARED | MAP_FIXED | MAP_ANONYMOUS | MAP_IGNORED_ANDROID) != 0 {
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
    if shared && protection & 0x2 != 0 {
        // Android's property bootstrap is the one bounded exception: its
        // area is represented by a stable virtual file and the initializer
        // needs a writable MAP_SHARED view while constructing it.  The
        // kernel-global property pages are shared by all property mappings;
        // ordinary files retain strict write-back semantics.
        if !file_handle.is_some_and(fs::linux_property_file) {
            return ENOSYS;
        }
    }
    if shared && file_handle.is_some_and(fs::linux_property_file) {
        return fs::linux_property_mmap(address, length, offset, protection)
            .unwrap_or_else(|error| error);
    }

    // Populate through a temporary writable mapping, then publish the exact
    // requested protection.  This lets the kernel copy file bytes into an
    // initially RX/RO ELF segment without ever writing through its final
    // user permissions.
    let final_protection = protection & PROT_MASK;
    let fill_protection = final_protection | 0x3;
    let map_flags = fill_protection << 16;
    let mapped = match allocator::with_global(|frames| {
        if flags & MAP_FIXED != 0 {
            task::map_memory_fixed(frames, address, length, map_flags)
        } else {
            task::map_memory(frames, address, length, map_flags)
        }
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
    if let Some(result) = fs::linux_property_unmap(frame.x[0], frame.x[1]) {
        return result;
    }
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
    if let Some(result) = fs::linux_property_mprotect(frame.x[0], frame.x[1], frame.x[2]) {
        return result;
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
        2
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

fn rt_sigaction(frame: &mut Aarch64TrapFrame) -> bool {
    const SIGKILL: u64 = 9;
    const SIGSTOP: u64 = 19;
    const SIGACTION_BYTES: usize = 32;
    let signal = frame.x[0];
    let action = frame.x[1];
    let old_action = frame.x[2];
    if !(1..=64).contains(&signal) || signal == SIGKILL || signal == SIGSTOP || frame.x[3] != 8 {
        frame.x[0] = EINVAL;
        return true;
    }
    let signal = signal as usize;
    if old_action != 0 {
        let Some(bytes) = task::current_linux_signal_action(signal) else {
            frame.x[0] = EINVAL;
            return true;
        };
        if user_memory::copy_to_user(old_action, &bytes[..SIGACTION_BYTES]).is_err() {
            frame.x[0] = EFAULT;
            return true;
        }
    }
    if action != 0 {
        let mut bytes = [0u8; SIGACTION_BYTES];
        if user_memory::copy_from_user(action, &mut bytes).is_err()
            || !task::set_current_linux_signal_action(signal, bytes)
        {
            frame.x[0] = EFAULT;
            return true;
        }
    }
    frame.x[0] = 0;
    true
}

fn rt_sigprocmask(frame: &mut Aarch64TrapFrame) -> bool {
    const SIG_BLOCK: u64 = 0;
    const SIG_UNBLOCK: u64 = 1;
    const SIG_SETMASK: u64 = 2;
    const SIGKILL_MASK: u64 = 1 << (9 - 1);
    const SIGSTOP_MASK: u64 = 1 << (19 - 1);
    if frame.x[3] != 8 || !matches!(frame.x[0], SIG_BLOCK | SIG_UNBLOCK | SIG_SETMASK) {
        frame.x[0] = EINVAL;
        return true;
    }
    let old_mask = task::current_linux_signal_mask().unwrap_or(0);
    if frame.x[2] != 0 && user_memory::copy_to_user(frame.x[2], &old_mask.to_ne_bytes()).is_err() {
        frame.x[0] = EFAULT;
        return true;
    }
    if frame.x[1] != 0 {
        let mut bytes = [0u8; 8];
        if user_memory::copy_from_user(frame.x[1], &mut bytes).is_err() {
            frame.x[0] = EFAULT;
            return true;
        }
        let requested = u64::from_ne_bytes(bytes) & !(SIGKILL_MASK | SIGSTOP_MASK);
        let mask = match frame.x[0] {
            SIG_BLOCK => old_mask | requested,
            SIG_UNBLOCK => old_mask & !requested,
            SIG_SETMASK => requested,
            _ => unreachable!(),
        };
        if !task::set_current_linux_signal_mask(mask) {
            frame.x[0] = EINVAL;
            return true;
        }
    }
    frame.x[0] = 0;
    true
}

fn prctl(frame: &Aarch64TrapFrame) -> u64 {
    match frame.x[0] {
        3 => current_credentials()
            .map(|credentials| u64::from(credentials.dumpable))
            .unwrap_or(ESRCH), // PR_GET_DUMPABLE
        4 => {
            if frame.x[1] > 1 {
                return EINVAL;
            }
            let Ok(mut credentials) = current_credentials() else {
                return ESRCH;
            };
            credentials.dumpable = frame.x[1] == 1;
            commit_credentials(credentials)
        } // PR_SET_DUMPABLE
        7 => current_credentials()
            .map(|credentials| u64::from(credentials.keep_caps))
            .unwrap_or(ESRCH), // PR_GET_KEEPCAPS
        8 => {
            if frame.x[1] > 1 {
                return EINVAL;
            }
            let Ok(mut credentials) = current_credentials() else {
                return ESRCH;
            };
            if credentials.effective_uid != 0 {
                return EPERM;
            }
            credentials.keep_caps = frame.x[1] == 1;
            commit_credentials(credentials)
        } // PR_SET_KEEPCAPS
        23 => {
            if frame.x[1] >= 41 {
                return EINVAL;
            }
            current_credentials()
                .map(|credentials| (credentials.cap_bounding >> frame.x[1]) & 1)
                .unwrap_or(ESRCH)
        } // PR_CAPBSET_READ
        24 => {
            if frame.x[1] >= 41 {
                return EINVAL;
            }
            let Ok(mut credentials) = current_credentials() else {
                return ESRCH;
            };
            if credentials.effective_uid != 0 {
                return EPERM;
            }
            credentials.cap_bounding &= !(1u64 << frame.x[1]);
            credentials.cap_permitted &= credentials.cap_bounding;
            credentials.cap_effective &= credentials.cap_bounding;
            commit_credentials(credentials)
        } // PR_CAPBSET_DROP
        38 => {
            if frame.x[1] != 1 {
                return EINVAL;
            }
            let Ok(mut credentials) = current_credentials() else {
                return ESRCH;
            };
            credentials.no_new_privs = true;
            commit_credentials(credentials)
        } // PR_SET_NO_NEW_PRIVS
        39 => current_credentials()
            .map(|credentials| u64::from(credentials.no_new_privs))
            .unwrap_or(ESRCH), // PR_GET_NO_NEW_PRIVS
        36 => {
            // PR_SET_CHILD_SUBREAPER. Fullerene's bounded scheduler already
            // adopts orphaned processes to PID 1, so PID 1 is the only
            // supported subreaper state for this early Android boundary.
            if frame.x[1] <= 1 { 0 } else { EINVAL }
        }
        37 => {
            // PR_GET_CHILD_SUBREAPER
            if frame.x[1] == 0 {
                return EFAULT;
            }
            let value = u32::from(task::current_process_pid() == Some(1));
            if user_memory::copy_to_user(frame.x[1], &value.to_ne_bytes()).is_err() {
                EFAULT
            } else {
                0
            }
        }
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

fn futex(frame: &Aarch64TrapFrame) -> FutexAction {
    const FUTEX_WAIT: u64 = 0;
    const FUTEX_WAKE: u64 = 1;
    const FUTEX_WAIT_BITSET: u64 = 9;
    const FUTEX_WAKE_BITSET: u64 = 10;
    const FUTEX_PRIVATE_FLAG: u64 = 0x80;
    const FUTEX_CLOCK_REALTIME: u64 = 0x100;
    let address = frame.x[0];
    if address == 0 {
        return FutexAction::Return(EFAULT);
    }
    if address & 3 != 0 {
        return FutexAction::Return(EINVAL);
    }
    let operation = frame.x[1] & 0x7f;
    let flags = frame.x[1] & !0x7f;
    if flags & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME) != 0
        || flags & FUTEX_CLOCK_REALTIME != 0
    {
        return FutexAction::Return(ENOSYS);
    }
    let key = task::linux_futex_event_key(address);
    match operation {
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let maximum = usize::try_from(frame.x[2]).unwrap_or(usize::MAX);
            FutexAction::Return(task::wake_event_count(key, maximum) as u64)
        }
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            if operation == FUTEX_WAIT_BITSET && frame.x[5] == 0 {
                return FutexAction::Return(EINVAL);
            }
            let mut value = [0u8; 4];
            if user_memory::copy_from_user(address, &mut value).is_err() {
                return FutexAction::Return(EFAULT);
            }
            if u32::from_ne_bytes(value) != frame.x[2] as u32 {
                return FutexAction::Return(EAGAIN);
            }
            let timeout_us = match if operation == FUTEX_WAIT_BITSET {
                futex_absolute_timeout(frame.x[3])
            } else {
                futex_timeout(frame.x[3])
            } {
                Ok(timeout_us) => timeout_us,
                Err(error) => return FutexAction::Return(error),
            };
            if timeout_us == 0 {
                return FutexAction::Return(ETIMEDOUT);
            }
            if !task::can_block_sleep() {
                return FutexAction::Return(EAGAIN);
            }
            FutexAction::Wait { key, timeout_us }
        }
        _ => FutexAction::Return(ENOSYS),
    }
}

fn futex_timeout(address: u64) -> Result<u64, u64> {
    if address == 0 {
        return Ok(u64::MAX);
    }
    let mut bytes = [0u8; 16];
    if user_memory::copy_from_user(address, &mut bytes).is_err() {
        return Err(EFAULT);
    }
    let seconds = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
    let nanoseconds = i64::from_ne_bytes(bytes[8..].try_into().unwrap());
    if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
        return Err(EINVAL);
    }
    Ok(seconds
        .unsigned_abs()
        .saturating_mul(1_000_000)
        .saturating_add((nanoseconds as u64) / 1_000))
}

fn futex_absolute_timeout(address: u64) -> Result<u64, u64> {
    if address == 0 {
        return Ok(u64::MAX);
    }
    let mut bytes = [0u8; 16];
    if user_memory::copy_from_user(address, &mut bytes).is_err() {
        return Err(EFAULT);
    }
    let seconds = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
    let nanoseconds = i64::from_ne_bytes(bytes[8..].try_into().unwrap());
    if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
        return Err(EINVAL);
    }
    let deadline = seconds
        .unsigned_abs()
        .saturating_mul(1_000_000)
        .saturating_add((nanoseconds as u64) / 1_000);
    Ok(deadline.saturating_sub(timer::uptime_us()))
}
