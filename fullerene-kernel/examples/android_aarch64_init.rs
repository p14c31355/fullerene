#![no_std]
#![no_main]

//! Rust-owned PID 1 for the Fullerene Android-init bring-up path.
//!
//! This is deliberately a native Fullerene implementation of the small
//! early-init contract, not a disguised claim that AOSP init or Bionic is
//! present. It establishes the property-service socket, parses the bounded
//! Rust-owned Android service/action configuration, handles the bounded
//! `ctl.start`/`ctl.stop`/`ctl.restart` lifecycle, and remains alive as PID 1
//! so later Rust services can be added through the same Linux ABI.

use core::arch::asm;

mod android_init_actions;
mod android_init_services;

const SYS_SOCKET: u64 = 198;
const SYS_BIND: u64 = 200;
const SYS_LISTEN: u64 = 201;
const SYS_CONNECT: u64 = 203;
const SYS_ACCEPT4: u64 = 242;
const SYS_READ: u64 = 63;
const SYS_WRITE: u64 = 64;
const SYS_CLOSE: u64 = 57;
const SYS_CLONE: u64 = 220;
const SYS_EXECVE: u64 = 221;
const SYS_EXIT_GROUP: u64 = 94;
const SYS_KILL: u64 = 129;
const SYS_SIGNALFD4: u64 = 74;
const SYS_OPENAT: u64 = 56;
const SYS_FSTATAT: u64 = 79;
const SYS_SETPGID: u64 = 154;
const SYS_GETPGID: u64 = 155;
const SYS_GETSID: u64 = 156;
const SYS_SETSID: u64 = 157;
const SYS_WAITID: u64 = 95;
const SYS_RT_SIGACTION: u64 = 134;
const SYS_RT_SIGPROCMASK: u64 = 135;
const SYS_RT_SIGRETURN: u64 = 139;
const SYS_PRCTL: u64 = 167;
const SYS_GETPID: u64 = 172;
const SYS_SETUID: u64 = 146;
const SYS_SETGID: u64 = 144;
const SYS_GETUID: u64 = 174;
const SYS_GETEUID: u64 = 175;
const SYS_GETGID: u64 = 176;
const SYS_GETEGID: u64 = 177;
const SYS_GETRESUID: u64 = 148;
const SYS_GETRESGID: u64 = 150;
const SYS_SETGROUPS: u64 = 159;
const SYS_CAPGET: u64 = 90;
const SYS_CAPSET: u64 = 91;
const SYS_SCHED_YIELD: u64 = 124;
const SYS_MOUNT: u64 = 40;
const SYS_MKDIRAT: u64 = 34;
const SYS_FCHMODAT: u64 = 53;
const SYS_FCHOWNAT: u64 = 54;
const ERR_NO_ENTRY: u64 = (-2i64) as u64;

const AF_UNIX: u64 = 1;
const SOCK_STREAM: u64 = 1;
const SOCK_CLOEXEC: u64 = 0x80000;
const AT_FDCWD: u64 = (-100i64) as u64;
const PR_SET_NAME: u64 = 15;
const PR_GET_NO_NEW_PRIVS: u64 = 39;
const PR_CAPBSET_READ: u64 = 23;
const PR_SET_CHILD_SUBREAPER: u64 = 36;
const SIG_BLOCK: u64 = 0;
const SIG_SETMASK: u64 = 2;
const SIGCHLD: u64 = 17;
const SIGUSR1: u64 = 10;
const SIGSET_SIZE: u64 = 8;
const SIGACTION_SIZE: usize = 32;
const PROP_MSG_SETPROP: u32 = 1;
const PROP_MSG_SETPROP2: u32 = 0x0002_0001;
const PROP_SUCCESS: u32 = 0;
const PROP_ERROR_READ_CMD: u32 = 0x0004;
const PROP_ERROR_READ_DATA: u32 = 0x0008;
const PROP_ERROR_READ_ONLY_PROPERTY: u32 = 0x000b;
const PROP_ERROR_INVALID_NAME: u32 = 0x0010;
const PROP_ERROR_INVALID_VALUE: u32 = 0x0014;
const PROP_ERROR_INVALID_CMD: u32 = 0x001b;
const SIGTERM: u64 = 15;
const P_PID: u64 = 1;
const WEXITED: u64 = 4;
const WNOHANG: u64 = 1;
const MAX_PROPERTY_MESSAGE: usize = 256;
const PROPERTY_NAME_MAX: usize = 32;
const PROPERTY_VALUE_MAX: usize = 92;
const SERVICE_RETRY_LIMIT: usize = 4096;

static START: &[u8] = b"fullerene-init: Rust PID 1 active\n";
static SUBREAPER_READY: &[u8] = b"fullerene-init: child subreaper ready\n";
static SIGNAL_TEST_OK: &[u8] = b"fullerene-init: signal ABI v1 ok\n";
static SIGNAL_TEST_FAILED: &[u8] = b"fullerene-init: signal ABI selftest failed\n";
static PROCESS_GROUP_TEST_OK: &[u8] = b"fullerene-init: process groups v1 ok\n";
static PROCESS_GROUP_TEST_FAILED: &[u8] = b"fullerene-init: process groups selftest failed\n";
static SIGNAL_FD_TEST_OK: &[u8] = b"fullerene-init: signalfd v1 ok\n";
static SIGNAL_FD_TEST_FAILED: &[u8] = b"fullerene-init: signalfd selftest failed\n";
static CREDENTIALS_TEST_OK: &[u8] = b"fullerene-init: credentials v1 ok\n";
static CREDENTIALS_TEST_FAILED: &[u8] = b"fullerene-init: credentials selftest failed\n";
static SELINUX_ATTR_TEST_OK: &[u8] = b"fullerene-init: selinux attr v1 ok\n";
static SELINUX_ATTR_TEST_FAILED: &[u8] = b"fullerene-init: selinux attr selftest failed\n";
static SELINUX_POLICY_OK: &[u8] = b"fullerene-init: selinux policy v2 ok\n";
static SELINUX_POLICY_FAILED: &[u8] = b"fullerene-init: selinux policy v2 failed\n";
static SOCKET_READY: &[u8] = b"fullerene-init: property_service listening\n";
static SOCKET_FAILED: &[u8] = b"fullerene-init: property_service unavailable\n";
static SELF_TEST_OK: &[u8] = b"fullerene-init: property protocol v2 ok\n";
static SELF_TEST_FAILED: &[u8] = b"fullerene-init: property protocol selftest failed\n";
static SERVICE_TEST_OK: &[u8] = b"fullerene-init: service manager v2 ok\n";
static SERVICE_TEST_FAILED: &[u8] = b"fullerene-init: service manager selftest failed\n";
static SERVICE_CONFIG_OK: &[u8] = b"fullerene-init: service config v1 ok\n";
static SERVICE_CONFIG_FAILED: &[u8] = b"fullerene-init: service config selftest failed\n";
static ACTION_CONFIG_OK: &[u8] = b"fullerene-init: action config v1 ok\n";
static ACTION_CONFIG_FAILED: &[u8] = b"fullerene-init: action config selftest failed\n";
static ACTIONS_OK: &[u8] = b"fullerene-init: init actions v1 ok\n";
static ACTIONS_FAILED: &[u8] = b"fullerene-init: init actions failed\n";
static ACTION_METADATA_OK: &[u8] = b"fullerene-init: init metadata v1 ok\n";
static ACTION_METADATA_FAILED: &[u8] = b"fullerene-init: init metadata selftest failed\n";
static MOUNT_ALL_OK: &[u8] = b"fullerene-init: mount_all v1 ok\n";
static MOUNT_ALL_FAILED: &[u8] = b"fullerene-init: mount_all failed\n";
static PROPERTY_TRIGGER_OK: &[u8] = b"fullerene-init: property triggers v1 ok\n";
static PROPERTY_TRIGGER_FAILED: &[u8] = b"fullerene-init: property triggers selftest failed\n";
static SERVICE_CLASS_STARTED: &[u8] = b"fullerene-init: service class core started\n";
static SERVICE_CLASS_FAILED: &[u8] = b"fullerene-init: service class core failed\n";
static SERVICE_CRASH_RESTARTED: &[u8] = b"fullerene-init: service crash restarted\n";
static SERVICE_STARTED: &[u8] = b"fullerene-init: fullerened started\n";
static SERVICE_STOPPED: &[u8] = b"fullerene-init: fullerened stopped\n";
static SERVICE_RESTARTED: &[u8] = b"fullerene-init: fullerened restarted\n";
static SERVICE_EXEC_FAILED: &[u8] = b"fullerene-service: exec failed\n";
static NAME: &[u8] = b"fullerene-init\0";
static PROPERTY_SERVICE: &[u8] = b"/dev/socket/property_service\0";
static FULLERENE_SERVICE_NAME: &[u8] = b"fullerened";
static mut SERVICE_PIDS: [u64; android_init_services::MAX_SERVICES] =
    [0; android_init_services::MAX_SERVICES];
static mut PROPERTY_ACTION_DEPTH: u8 = 0;

const PROPERTY_ACTION_DEPTH_MAX: u8 = 4;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    write(START);
    let _ = syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0);
    let _ = syscall(SYS_PRCTL, PR_SET_NAME, NAME.as_ptr() as u64, 0, 0, 0, 0);
    if (syscall(SYS_PRCTL, PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0, 0) as i64) >= 0 {
        write(SUBREAPER_READY);
    }

    let server = syscall(SYS_SOCKET, AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, 0, 0, 0);
    if (server as i64) < 0 {
        write(SOCKET_FAILED);
        supervise(u64::MAX);
    }

    let (address, address_length) = property_address();
    if (syscall(
        SYS_BIND,
        server,
        address.as_ptr() as u64,
        address_length,
        0,
        0,
        0,
    ) as i64)
        < 0
        || (syscall(SYS_LISTEN, server, 8, 0, 0, 0, 0) as i64) < 0
    {
        write(SOCKET_FAILED);
        let _ = syscall(SYS_CLOSE, server, 0, 0, 0, 0, 0);
        supervise(u64::MAX);
    }

    write(SOCKET_READY);
    let selinux_policy_ok = load_bootstrap_selinux_policy();
    if self_test_enabled() {
        write(if selinux_policy_ok {
            SELINUX_POLICY_OK
        } else {
            SELINUX_POLICY_FAILED
        });
    }
    if !selinux_policy_ok {
        supervise(u64::MAX);
    }
    if self_test_enabled() {
        let property_ok = property_self_test(server);
        if property_ok {
            write(SELF_TEST_OK);
        } else {
            write(SELF_TEST_FAILED);
        }
        if android_init_services::self_test() {
            write(SERVICE_CONFIG_OK);
        } else {
            write(SERVICE_CONFIG_FAILED);
        }
        if android_init_actions::self_test() {
            write(ACTION_CONFIG_OK);
        } else {
            write(ACTION_CONFIG_FAILED);
        }
        if property_ok && android_init_services::self_test() && service_self_test(server) {
            write(SERVICE_TEST_OK);
        } else {
            write(SERVICE_TEST_FAILED);
        }
        if property_trigger_self_test(server) {
            write(PROPERTY_TRIGGER_OK);
        } else {
            write(PROPERTY_TRIGGER_FAILED);
        }
        if signal_self_test() {
            write(SIGNAL_TEST_OK);
        } else {
            write(SIGNAL_TEST_FAILED);
        }
        if process_group_self_test() {
            write(PROCESS_GROUP_TEST_OK);
        } else {
            write(PROCESS_GROUP_TEST_FAILED);
        }
        if signalfd_self_test() {
            write(SIGNAL_FD_TEST_OK);
        } else {
            write(SIGNAL_FD_TEST_FAILED);
        }
        if credentials_self_test() {
            write(CREDENTIALS_TEST_OK);
        } else {
            write(CREDENTIALS_TEST_FAILED);
        }
        if selinux_attr_self_test() {
            write(SELINUX_ATTR_TEST_OK);
        } else {
            write(SELINUX_ATTR_TEST_FAILED);
        }
    }
    let actions_ok = android_init_actions::self_test()
        && run_init_actions(server, b"early-init")
        && run_init_actions(server, b"init")
        && run_init_actions(server, b"boot");
    let metadata_ok = actions_ok && init_metadata_self_test();
    if self_test_enabled() {
        if actions_ok {
            write(ACTIONS_OK);
        } else {
            write(ACTIONS_FAILED);
        }
        if metadata_ok {
            write(ACTION_METADATA_OK);
        } else {
            write(ACTION_METADATA_FAILED);
        }
    }
    if start_service_class(b"core") {
        write(SERVICE_CLASS_STARTED);
    } else {
        write(SERVICE_CLASS_FAILED);
    }
    supervise(server);
}

fn self_test_enabled() -> bool {
    option_env!("FULLERENE_ANDROID_INIT_SELFTEST")
        .map(|value| value.as_bytes() == b"1")
        .unwrap_or(false)
}

fn init_metadata_self_test() -> bool {
    let path = b"/sys/class/android_usb/state\0";
    let mut stat = [0u8; 128];
    if (syscall(
        SYS_FSTATAT,
        AT_FDCWD,
        path.as_ptr() as u64,
        stat.as_mut_ptr() as u64,
        0,
        0,
        0,
    ) as i64)
        != 0
    {
        return false;
    }
    let mode = u32::from_ne_bytes([stat[16], stat[17], stat[18], stat[19]]);
    let uid = u32::from_ne_bytes([stat[24], stat[25], stat[26], stat[27]]);
    let gid = u32::from_ne_bytes([stat[28], stat[29], stat[30], stat[31]]);
    mode == 0o100644 && uid == 0 && gid == 0
}

fn signal_self_test() -> bool {
    let blocked = 1u64 << (10 - 1); // SIGUSR1
    let mut old_mask = 0u64;
    if (syscall(
        SYS_RT_SIGPROCMASK,
        SIG_BLOCK,
        (&blocked as *const u64) as u64,
        (&mut old_mask as *mut u64) as u64,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
        || old_mask != 0
    {
        return false;
    }
    let mut current_mask = 0u64;
    if (syscall(
        SYS_RT_SIGPROCMASK,
        SIG_SETMASK,
        0,
        (&mut current_mask as *mut u64) as u64,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
        || current_mask != blocked
    {
        return false;
    }
    if (syscall(
        SYS_RT_SIGPROCMASK,
        SIG_SETMASK,
        (&old_mask as *const u64) as u64,
        0,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
    {
        return false;
    }

    let mut action = [0u8; SIGACTION_SIZE];
    action[..8].copy_from_slice(&1u64.to_ne_bytes());
    if (syscall(
        SYS_RT_SIGACTION,
        SIGCHLD,
        action.as_ptr() as u64,
        0,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
    {
        return false;
    }
    let mut current_action = [0u8; SIGACTION_SIZE];
    if (syscall(
        SYS_RT_SIGACTION,
        SIGCHLD,
        0,
        current_action.as_mut_ptr() as u64,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
        || current_action[..8] != action[..8]
    {
        return false;
    }
    let default_action = [0u8; SIGACTION_SIZE];
    if (syscall(
        SYS_RT_SIGACTION,
        SIGCHLD,
        default_action.as_ptr() as u64,
        0,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
    {
        return false;
    }

    let mut handler_action = [0u8; SIGACTION_SIZE];
    handler_action[..8].copy_from_slice(&(signal_return_handler as usize as u64).to_ne_bytes());
    if (syscall(
        SYS_RT_SIGACTION,
        SIGUSR1,
        handler_action.as_ptr() as u64,
        0,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
    {
        return false;
    }
    let pid = syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0);
    if (syscall(SYS_KILL, pid, SIGUSR1, 0, 0, 0, 0) as i64 != 0) {
        return false;
    }
    (syscall(
        SYS_RT_SIGACTION,
        SIGUSR1,
        default_action.as_ptr() as u64,
        0,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        == 0
}

fn process_group_self_test() -> bool {
    let pid = syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0);
    (syscall(SYS_GETPGID, 0, 0, 0, 0, 0, 0) == pid)
        && (syscall(SYS_GETSID, 0, 0, 0, 0, 0, 0) == pid)
        && (syscall(SYS_SETPGID, 0, 0, 0, 0, 0, 0) as i64 == 0)
        && (syscall(SYS_KILL, 0, 0, 0, 0, 0, 0) as i64 == 0)
        && (syscall(SYS_SETSID, 0, 0, 0, 0, 0, 0) as i64 == -1)
}

fn signalfd_self_test() -> bool {
    const SFD_NONBLOCK: u64 = 0x800;
    let mask = 1u64 << (SIGUSR1 - 1);
    let mut old_mask = 0u64;
    if (syscall(
        SYS_RT_SIGPROCMASK,
        SIG_BLOCK,
        (&mask as *const u64) as u64,
        (&mut old_mask as *mut u64) as u64,
        SIGSET_SIZE,
        0,
        0,
    ) as i64)
        != 0
    {
        return false;
    }
    let fd = syscall(
        SYS_SIGNALFD4,
        u64::MAX,
        (&mask as *const u64) as u64,
        SIGSET_SIZE,
        SFD_NONBLOCK,
        0,
        0,
    );
    if (fd as i64) < 0 {
        return false;
    }
    let pid = syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0);
    if (syscall(SYS_KILL, pid, SIGUSR1, 0, 0, 0, 0) as i64 != 0) {
        let _ = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0);
        return false;
    }
    let mut info = [0u8; 128];
    let read = syscall(
        SYS_READ,
        fd,
        info.as_mut_ptr() as u64,
        info.len() as u64,
        0,
        0,
        0,
    );
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    let restored = syscall(
        SYS_RT_SIGPROCMASK,
        SIG_SETMASK,
        (&old_mask as *const u64) as u64,
        0,
        SIGSET_SIZE,
        0,
        0,
    ) as i64
        == 0;
    read == 128
        && u32::from_ne_bytes(info[..4].try_into().unwrap()) as u64 == SIGUSR1
        && closed
        && restored
}

fn credentials_self_test() -> bool {
    if syscall(SYS_GETUID, 0, 0, 0, 0, 0, 0) != 0
        || syscall(SYS_GETEUID, 0, 0, 0, 0, 0, 0) != 0
        || syscall(SYS_GETGID, 0, 0, 0, 0, 0, 0) != 0
        || syscall(SYS_GETEGID, 0, 0, 0, 0, 0, 0) != 0
    {
        return false;
    }
    let mut uid_values = [u32::MAX; 3];
    let mut gid_values = [u32::MAX; 3];
    if (syscall(
        SYS_GETRESUID,
        uid_values.as_mut_ptr() as u64,
        (uid_values.as_mut_ptr().wrapping_add(1)) as u64,
        (uid_values.as_mut_ptr().wrapping_add(2)) as u64,
        0,
        0,
        0,
    ) as i64)
        != 0
        || (syscall(
            SYS_GETRESGID,
            gid_values.as_mut_ptr() as u64,
            (gid_values.as_mut_ptr().wrapping_add(1)) as u64,
            (gid_values.as_mut_ptr().wrapping_add(2)) as u64,
            0,
            0,
            0,
        ) as i64)
            != 0
        || uid_values != [0; 3]
        || gid_values != [0; 3]
    {
        return false;
    }
    let cap_header = [0x2008_0522u32, 0u32];
    let mut cap_data = [0u32; 6];
    if (syscall(
        SYS_CAPGET,
        cap_header.as_ptr() as u64,
        cap_data.as_mut_ptr() as u64,
        0,
        0,
        0,
        0,
    ) as i64)
        != 0
        || (cap_data[0] == 0 && cap_data[3] == 0)
        || (syscall(
            SYS_CAPSET,
            cap_header.as_ptr() as u64,
            cap_data.as_ptr() as u64,
            0,
            0,
            0,
            0,
        ) as i64)
            != 0
        || (syscall(SYS_PRCTL, PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0, 0) as i64) != 0
        || (syscall(SYS_PRCTL, PR_CAPBSET_READ, 0, 0, 0, 0, 0) as i64) != 1
    {
        return false;
    }
    (syscall(SYS_SETUID, 0, 0, 0, 0, 0, 0) as i64) == 0
        && (syscall(SYS_SETGID, 0, 0, 0, 0, 0, 0) as i64) == 0
        && (syscall(SYS_SETGROUPS, 0, 0, 0, 0, 0, 0) as i64) == 0
}

fn selinux_attr_self_test() -> bool {
    const AT_FDCWD: u64 = (-100i64) as u64;
    static CURRENT: &[u8] = b"/proc/self/attr/current\0";
    static EXEC: &[u8] = b"/proc/self/attr/exec\0";
    let fd = syscall(SYS_OPENAT, AT_FDCWD, CURRENT.as_ptr() as u64, 0, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let mut context = [0u8; 64];
    let read = syscall(
        SYS_READ,
        fd,
        context.as_mut_ptr() as u64,
        context.len() as u64,
        0,
        0,
        0,
    );
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    if read != b"u:r:init:s0\0".len() as u64
        || context[..read as usize] != *b"u:r:init:s0\0"
        || !closed
    {
        return false;
    }
    let fd = syscall(SYS_OPENAT, AT_FDCWD, EXEC.as_ptr() as u64, 1, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let denied = syscall(
        SYS_WRITE,
        fd,
        b"u:r:untrusted_app:s0".as_ptr() as u64,
        b"u:r:untrusted_app:s0".len() as u64,
        0,
        0,
        0,
    );
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    if denied != (-(13i64) as u64) || !closed {
        return false;
    }
    let fd = syscall(SYS_OPENAT, AT_FDCWD, EXEC.as_ptr() as u64, 1, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let staged = write_bytes(fd, b"u:r:init:s0");
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    if !staged || !closed {
        return false;
    }
    let fd = syscall(SYS_OPENAT, AT_FDCWD, EXEC.as_ptr() as u64, 0, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let mut pending = [0u8; 64];
    let pending_read = syscall(
        SYS_READ,
        fd,
        pending.as_mut_ptr() as u64,
        pending.len() as u64,
        0,
        0,
        0,
    );
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    pending_read == b"u:r:init:s0\0".len() as u64
        && pending[..pending_read as usize] == *b"u:r:init:s0\0"
        && closed
}

fn append_policy_bytes(buffer: &mut [u8], length: &mut usize, bytes: &[u8]) -> bool {
    let Some(end) = length.checked_add(bytes.len()) else {
        return false;
    };
    if end > buffer.len() {
        return false;
    }
    buffer[*length..end].copy_from_slice(bytes);
    *length = end;
    true
}

fn append_policy_context_rule(
    buffer: &mut [u8],
    length: &mut usize,
    source: &[u8],
    target: &[u8],
) -> bool {
    source.len() <= u8::MAX as usize
        && target.len() <= u8::MAX as usize
        && append_policy_bytes(buffer, length, &[source.len() as u8, target.len() as u8])
        && append_policy_bytes(buffer, length, source)
        && append_policy_bytes(buffer, length, target)
}

fn append_policy_allow_rule(
    buffer: &mut [u8],
    length: &mut usize,
    source: &[u8],
    object: &[u8],
    permissions: u8,
) -> bool {
    source.len() <= u8::MAX as usize
        && object.len() <= u8::MAX as usize
        && append_policy_bytes(
            buffer,
            length,
            &[source.len() as u8, object.len() as u8, permissions, 0],
        )
        && append_policy_bytes(buffer, length, source)
        && append_policy_bytes(buffer, length, object)
}

fn build_bootstrap_selinux_policy(buffer: &mut [u8]) -> Option<usize> {
    static INIT: &[u8] = b"u:r:init:s0";
    static FULLERENED: &[u8] = b"u:r:fullerened:s0";
    static ADBD: &[u8] = b"u:r:adbd:s0";
    static SU: &[u8] = b"u:r:su:s0";
    static CURRENT: &[u8] = b"/proc/self/attr/current";
    static EXEC: &[u8] = b"/proc/self/attr/exec";
    static LOAD: &[u8] = b"/sys/fs/selinux/load";
    static ENFORCE: &[u8] = b"/sys/fs/selinux/enforce";
    static USB_STATE: &[u8] = b"/sys/class/android_usb/state";
    static HOSTNAME: &[u8] = b"/proc/sys/kernel/hostname";
    const READ: u8 = 1;
    const WRITE: u8 = 2;
    let mut length = 0;
    // FSP2 is generated from Rust byte slices rather than a checked-in CIL or
    // policydb blob; the kernel parser remains deliberately bounded.
    if !append_policy_bytes(buffer, &mut length, b"FSP2\x01\x00\x03\x00\x0b\x00")
        || !append_policy_context_rule(buffer, &mut length, INIT, INIT)
        || !append_policy_context_rule(buffer, &mut length, INIT, FULLERENED)
        // Bounded userdebug equivalent: adb root changes the debug domain
        // from adbd to su. The kernel still checks this rule at the ADB
        // control boundary; this is not an AOSP policydb encoding.
        || !append_policy_context_rule(buffer, &mut length, ADBD, SU)
        || !append_policy_allow_rule(buffer, &mut length, INIT, CURRENT, READ)
        || !append_policy_allow_rule(buffer, &mut length, INIT, EXEC, READ | WRITE)
        || !append_policy_allow_rule(buffer, &mut length, INIT, LOAD, WRITE)
        || !append_policy_allow_rule(buffer, &mut length, INIT, ENFORCE, WRITE)
        || !append_policy_allow_rule(buffer, &mut length, INIT, USB_STATE, WRITE)
        || !append_policy_allow_rule(buffer, &mut length, INIT, HOSTNAME, WRITE)
        || !append_policy_allow_rule(buffer, &mut length, FULLERENED, CURRENT, READ)
        || !append_policy_allow_rule(buffer, &mut length, ADBD, CURRENT, READ)
        || !append_policy_allow_rule(buffer, &mut length, ADBD, EXEC, READ | WRITE)
        || !append_policy_allow_rule(buffer, &mut length, SU, CURRENT, READ)
        || !append_policy_allow_rule(buffer, &mut length, SU, EXEC, READ)
    {
        return None;
    }
    Some(length)
}

fn load_bootstrap_selinux_policy() -> bool {
    static LOAD: &[u8] = b"/sys/fs/selinux/load\0";
    static ENFORCE: &[u8] = b"/sys/fs/selinux/enforce\0";
    let mut policy = [0u8; 512];
    let Some(policy_length) = build_bootstrap_selinux_policy(&mut policy) else {
        return false;
    };

    let fd = syscall(SYS_OPENAT, AT_FDCWD, LOAD.as_ptr() as u64, 1, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let loaded = write_bytes(fd, &policy[..policy_length]);
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    if !loaded || !closed {
        return false;
    }
    let fd = syscall(SYS_OPENAT, AT_FDCWD, ENFORCE.as_ptr() as u64, 1, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let enabled = write_bytes(fd, b"1");
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    enabled && closed
}

/// The bounded signal self-test enters a handler and asks the kernel to
/// restore the saved trap frame. A real Bionic image supplies its own
/// restorer; this direct SVC path keeps the kernel-side boundary testable
/// without requiring a separate VDSO trampoline in the probe image.
extern "C" fn signal_return_handler(_: u64) -> ! {
    unsafe {
        asm!(
            "mov x8, {nr}",
            "svc #0",
            nr = const SYS_RT_SIGRETURN,
            options(noreturn)
        );
    }
}

fn supervise(server: u64) -> ! {
    let mut message = [0u8; MAX_PROPERTY_MESSAGE];
    loop {
        reap_services();
        if server != u64::MAX {
            let client = syscall(SYS_ACCEPT4, server, 0, 0, 0, 0, 0);
            if (client as i64) >= 0 {
                serve_client(server, client, &mut message);
                let _ = syscall(SYS_CLOSE, client, 0, 0, 0, 0, 0);
            }
        }
        let _ = syscall(SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
    }
}

fn start_service_class(class: &[u8]) -> bool {
    let table = android_init_services::configured();
    if !table.valid {
        return false;
    }
    let mut found = false;
    let mut started = true;
    let mut index = 0;
    while index < table.len {
        if let Some(spec) = table.specs[index]
            && spec.class == class
            && !spec.disabled
        {
            found = true;
            if start_service(spec.name) != PROP_SUCCESS {
                started = false;
            }
        }
        index += 1;
    }
    found && started
}

fn reap_services() {
    let table = android_init_services::configured();
    if !table.valid {
        return;
    }
    let mut index = 0;
    while index < table.len {
        let Some(spec) = table.specs[index] else {
            index += 1;
            continue;
        };
        let pid = unsafe { SERVICE_PIDS[index] };
        if pid == 0 {
            index += 1;
            continue;
        }
        let mut info = [0u8; 128];
        let result = syscall(
            SYS_WAITID,
            P_PID,
            pid,
            info.as_mut_ptr() as u64,
            WEXITED | WNOHANG,
            0,
            0,
        );
        let reported_pid = u32::from_ne_bytes(info[16..20].try_into().unwrap()) as u64;
        if (result as i64) >= 0 && reported_pid == pid {
            unsafe {
                SERVICE_PIDS[index] = 0;
            }
            if !spec.oneshot && start_service(spec.name) == PROP_SUCCESS {
                write(SERVICE_CRASH_RESTARTED);
            }
        }
        index += 1;
    }
}

fn serve_client(server: u64, client: u64, message: &mut [u8; MAX_PROPERTY_MESSAGE]) {
    let mut length = 0usize;
    for _ in 0..SERVICE_RETRY_LIMIT {
        if length == message.len() {
            let _ = write_result(client, PROP_ERROR_READ_DATA);
            return;
        }
        let count = syscall(
            SYS_READ,
            client,
            message[length..].as_mut_ptr() as u64,
            (message.len() - length) as u64,
            0,
            0,
            0,
        );
        if (count as i64) > 0 {
            length += (count as usize).min(message.len() - length);
            if let Some((frame_length, response)) = property_frame(server, &message[..length]) {
                if frame_length <= length {
                    if let Some(result) = response {
                        let _ = write_result(client, result);
                    }
                    return;
                }
            }
        } else if count == 0 {
            if length < 4 {
                let _ = write_result(client, PROP_ERROR_READ_CMD);
            }
            return;
        } else {
            let _ = syscall(SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
        }
    }
    let _ = write_result(client, PROP_ERROR_READ_DATA);
}

fn property_frame(server: u64, data: &[u8]) -> Option<(usize, Option<u32>)> {
    let command = read_u32(data, 0)?;
    match command {
        PROP_MSG_SETPROP => {
            let frame_length = 4 + PROPERTY_NAME_MAX + PROPERTY_VALUE_MAX;
            if data.len() < frame_length {
                return None;
            }
            let name = trim_property(&data[4..4 + PROPERTY_NAME_MAX]);
            let value_start = 4 + PROPERTY_NAME_MAX;
            let value = trim_property(&data[value_start..value_start + PROPERTY_VALUE_MAX]);
            let _ = property_result(server, name, value);
            Some((frame_length, None))
        }
        PROP_MSG_SETPROP2 => {
            let name_length = read_u32(data, 4)? as usize;
            if name_length >= PROPERTY_NAME_MAX {
                return Some((8, Some(PROP_ERROR_INVALID_NAME)));
            }
            let value_length_offset = 8usize.checked_add(name_length)?;
            let value_length = read_u32(data, value_length_offset)? as usize;
            if value_length >= PROPERTY_VALUE_MAX {
                return Some((value_length_offset + 4, Some(PROP_ERROR_INVALID_VALUE)));
            }
            let value_start = value_length_offset.checked_add(4)?;
            let frame_length = value_start.checked_add(value_length)?;
            if data.len() < frame_length {
                return None;
            }
            let name = &data[8..value_length_offset];
            let value = &data[value_start..frame_length];
            Some((frame_length, property_result(server, name, value)))
        }
        _ => Some((4, Some(PROP_ERROR_INVALID_CMD))),
    }
}

fn property_result(server: u64, name: &[u8], value: &[u8]) -> Option<u32> {
    if name.is_empty()
        || name.iter().any(|byte| !byte.is_ascii() || *byte == 0)
        || name
            .split(|byte| *byte == b'.')
            .any(|component| component.is_empty())
    {
        return Some(PROP_ERROR_INVALID_NAME);
    }
    if value.iter().any(|byte| *byte == 0) {
        return Some(PROP_ERROR_INVALID_VALUE);
    }
    if name == b"ro.debuggable" || name == b"ro.hardware" || name == b"ro.property_service.version"
    {
        return Some(PROP_ERROR_READ_ONLY_PROPERTY);
    }
    if name == b"ctl.start" {
        return Some(start_service(value));
    }
    if name == b"ctl.stop" {
        return Some(stop_service(value));
    }
    if name == b"ctl.restart" {
        return Some(restart_service(value));
    }
    if name.starts_with(b"ctl.") {
        return Some(PROP_ERROR_INVALID_CMD);
    }
    let _ = run_property_actions(server, name, value);
    Some(PROP_SUCCESS)
}

fn trim_property(data: &[u8]) -> &[u8] {
    let end = data
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(data.len());
    &data[..end]
}

fn start_service(name: &[u8]) -> u32 {
    let Some((index, service)) = service_for(name) else {
        return PROP_ERROR_INVALID_VALUE;
    };
    let existing = unsafe { SERVICE_PIDS[index] };
    if existing != 0 {
        return PROP_SUCCESS;
    }

    let mut path = [0u8; 128];
    if service.path.len() + 1 > path.len() {
        return PROP_ERROR_INVALID_VALUE;
    }
    path[..service.path.len()].copy_from_slice(service.path);
    let argv = [path.as_ptr(), core::ptr::null()];
    let envp = [core::ptr::null::<u8>()];
    let pid = syscall(SYS_CLONE, 0, 0, 0, 0, 0, 0);
    if (pid as i64) < 0 {
        return PROP_ERROR_READ_DATA;
    }
    if pid == 0 {
        path[service.path.len()] = 0;
        if !apply_service_credentials(service) {
            write(SERVICE_EXEC_FAILED);
            let _ = syscall(SYS_EXIT_GROUP, 126, 0, 0, 0, 0, 0);
            loop {
                unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
            }
        }
        if !apply_service_seclabel(service) {
            write(SERVICE_EXEC_FAILED);
            let _ = syscall(SYS_EXIT_GROUP, 126, 0, 0, 0, 0, 0);
            loop {
                unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
            }
        }
        let result = syscall(
            SYS_EXECVE,
            path.as_ptr() as u64,
            argv.as_ptr() as u64,
            envp.as_ptr() as u64,
            0,
            0,
            0,
        );
        if (result as i64) < 0 {
            write(SERVICE_EXEC_FAILED);
        }
        let _ = syscall(SYS_EXIT_GROUP, 127, 0, 0, 0, 0, 0);
        loop {
            unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
        }
    }
    unsafe {
        SERVICE_PIDS[index] = pid;
    }
    write(SERVICE_STARTED);
    PROP_SUCCESS
}

fn stop_service(name: &[u8]) -> u32 {
    let Some((index, _)) = service_for(name) else {
        return PROP_ERROR_INVALID_VALUE;
    };
    let pid = unsafe { SERVICE_PIDS[index] };
    if pid == 0 {
        return PROP_SUCCESS;
    }

    if (syscall(SYS_KILL, pid, SIGTERM, 0, 0, 0, 0) as i64) < 0 {
        return PROP_ERROR_READ_DATA;
    }
    let mut info = [0u8; 128];
    let mut reaped = false;
    for _ in 0..SERVICE_RETRY_LIMIT {
        info = [0u8; 128];
        let result = syscall(
            SYS_WAITID,
            P_PID,
            pid,
            info.as_mut_ptr() as u64,
            WEXITED | WNOHANG,
            0,
            0,
        );
        if (result as i64) < 0 {
            return PROP_ERROR_READ_DATA;
        }
        let reported_pid = u32::from_ne_bytes(info[16..20].try_into().unwrap()) as u64;
        if reported_pid == pid {
            reaped = true;
            break;
        }
        let _ = syscall(SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
    }
    if !reaped {
        return PROP_ERROR_READ_DATA;
    }
    unsafe {
        SERVICE_PIDS[index] = 0;
    }
    write(SERVICE_STOPPED);
    PROP_SUCCESS
}

fn restart_service(name: &[u8]) -> u32 {
    let result = stop_service(name);
    if result != PROP_SUCCESS {
        return result;
    }
    let result = start_service(name);
    if result == PROP_SUCCESS {
        write(SERVICE_RESTARTED);
    }
    result
}

fn service_for(name: &[u8]) -> Option<(usize, android_init_services::ServiceSpec<'static>)> {
    let table = android_init_services::configured();
    if !table.valid {
        return None;
    }
    table.find(name)
}

fn run_init_actions(server: u64, trigger: &[u8]) -> bool {
    let table = android_init_actions::configured();
    if !table.valid {
        return false;
    }
    let Some((_, action)) = table.find(trigger) else {
        return false;
    };
    execute_init_action(server, action)
}

/// Run every configured `on property:<name>=<value>` action in source order.
/// The property service publishes the value in the kernel before this hook is
/// reached, so a trigger observes the same value that readers of the property
/// area see. A small depth guard prevents cyclic `setprop` actions from
/// consuming PID 1 forever.
fn run_property_actions(server: u64, name: &[u8], value: &[u8]) -> bool {
    let entered = unsafe {
        if PROPERTY_ACTION_DEPTH >= PROPERTY_ACTION_DEPTH_MAX {
            false
        } else {
            PROPERTY_ACTION_DEPTH += 1;
            true
        }
    };
    if !entered {
        return true;
    }

    let table = android_init_actions::configured();
    let mut successful = table.valid;
    let mut index = 0;
    while index < table.len {
        if table.valid
            && let Some(action) = table.actions[index]
            && android_init_actions::property_trigger_matches(action.trigger, name, value)
            && !execute_init_action(server, action)
        {
            successful = false;
        }
        index += 1;
    }
    unsafe {
        PROPERTY_ACTION_DEPTH -= 1;
    }
    successful
}

fn execute_init_action(server: u64, action: android_init_actions::ActionSpec<'_>) -> bool {
    let mut index = 0;
    while index < action.command_count {
        let Some(command) = action.commands[index] else {
            return false;
        };
        let successful = match command.name {
            b"setprop" => {
                let (Some(name), Some(value)) = (command.args[0], command.args[1]) else {
                    return false;
                };
                property_roundtrip(server, name, value)
            }
            b"start" => command.args[0].is_some_and(|name| start_service(name) == PROP_SUCCESS),
            b"stop" => command.args[0].is_some_and(|name| stop_service(name) == PROP_SUCCESS),
            b"restart" => command.args[0].is_some_and(|name| restart_service(name) == PROP_SUCCESS),
            b"mount" => mount_action(command),
            b"mount_all" => {
                let successful = mount_all_action(command);
                if self_test_enabled() {
                    write(if successful {
                        MOUNT_ALL_OK
                    } else {
                        MOUNT_ALL_FAILED
                    });
                }
                successful
            }
            b"mkdir" => mkdir_action(command),
            b"write" => write_action(command),
            b"chmod" => chmod_action(command),
            b"chown" => chown_action(command),
            b"wait" => command.args[0].is_some(),
            _ => false,
        };
        if !successful {
            return false;
        }
        index += 1;
    }
    true
}

fn mkdir_action(command: android_init_actions::InitCommand<'_>) -> bool {
    let (Some(path), Some(mode)) = (command.args[0], command.args[1]) else {
        return false;
    };
    let Some(mode) = parse_u64(mode) else {
        return false;
    };
    let mut path_buffer = [0u8; 128];
    let Some(path) = copy_cstring(path, &mut path_buffer) else {
        return false;
    };
    (syscall(SYS_MKDIRAT, AT_FDCWD, path, mode, 0, 0, 0) as i64) >= 0
}

fn write_action(command: android_init_actions::InitCommand<'_>) -> bool {
    let (Some(path), Some(contents)) = (command.args[0], command.args[1]) else {
        return false;
    };
    let mut path_buffer = [0u8; 128];
    let Some(path) = copy_cstring(path, &mut path_buffer) else {
        return false;
    };
    let fd = syscall(SYS_OPENAT, AT_FDCWD, path, 1 | 0x80000, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let written = write_bytes(fd, contents);
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    written && closed
}

fn chmod_action(command: android_init_actions::InitCommand<'_>) -> bool {
    let (Some(path), Some(mode)) = (command.args[0], command.args[1]) else {
        return false;
    };
    let Some(mode) = parse_u64(mode) else {
        return false;
    };
    let mut path_buffer = [0u8; 128];
    let Some(path) = copy_cstring(path, &mut path_buffer) else {
        return false;
    };
    (syscall(SYS_FCHMODAT, AT_FDCWD, path, mode, 0, 0, 0) as i64) >= 0
}

fn chown_action(command: android_init_actions::InitCommand<'_>) -> bool {
    let (Some(uid), Some(gid), Some(path)) = (command.args[0], command.args[1], command.args[2])
    else {
        return false;
    };
    let (Some(uid), Some(gid)) = (parse_u64(uid), parse_u64(gid)) else {
        return false;
    };
    let mut path_buffer = [0u8; 128];
    let Some(path) = copy_cstring(path, &mut path_buffer) else {
        return false;
    };
    (syscall(SYS_FCHOWNAT, AT_FDCWD, path, uid, gid, 0, 0) as i64) >= 0
}

/// Execute the deliberately small init `mount` form. Pseudo-filesystems use
/// the kernel's bounded virtual namespace boundary; Android filesystem targets
/// are validated against the early Bramble VFS mount table.
fn mount_action(command: android_init_actions::InitCommand<'_>) -> bool {
    let (Some(source), Some(target), Some(filesystem), Some(flags)) = (
        command.args[0],
        command.args[1],
        command.args[2],
        command.args[3],
    ) else {
        return false;
    };
    let Some(flags) = parse_u64(flags) else {
        return false;
    };
    let mut source_buffer = [0u8; 96];
    let mut target_buffer = [0u8; 96];
    let mut filesystem_buffer = [0u8; 96];
    let mut data_buffer = [0u8; 96];
    let Some(source) = copy_cstring(source, &mut source_buffer) else {
        return false;
    };
    let Some(target) = copy_cstring(target, &mut target_buffer) else {
        return false;
    };
    let Some(filesystem) = copy_cstring(filesystem, &mut filesystem_buffer) else {
        return false;
    };
    let data = match command.args[4] {
        Some(data) => {
            let Some(data) = copy_cstring(data, &mut data_buffer) else {
                return false;
            };
            data
        }
        None => 0,
    };
    (syscall(SYS_MOUNT, source, target, filesystem, flags, data, 0) as i64) >= 0
}

/// Read and apply the small Rust-owned fstab carried in the initramfs.  The
/// kernel performs the final target validation: pseudo-filesystems are
/// acknowledged by their bounded virtual implementations, while `/system`,
/// `/vendor`, and `/data` succeed only when the early Bramble UFS mount has
/// already installed them in the VFS.  `optional` entries keep QEMU useful
/// without turning a missing physical partition into a false boot success.
fn mount_all_action(command: android_init_actions::InitCommand<'_>) -> bool {
    let Some(path) = command.args[0] else {
        return false;
    };
    let mut path_buffer = [0u8; 128];
    let Some(path) = copy_cstring(path, &mut path_buffer) else {
        return false;
    };
    let fd = syscall(SYS_OPENAT, AT_FDCWD, path, SOCK_CLOEXEC, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }

    let mut contents = [0u8; 4096];
    let mut length = 0usize;
    let mut read_ok = true;
    while length < contents.len() {
        let count = syscall(
            SYS_READ,
            fd,
            contents[length..].as_mut_ptr() as u64,
            (contents.len() - length) as u64,
            0,
            0,
            0,
        );
        if (count as i64) < 0 {
            read_ok = false;
            break;
        }
        if count == 0 {
            break;
        }
        let count = (count as usize).min(contents.len() - length);
        length += count;
    }
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    read_ok && closed && apply_fstab(&contents[..length])
}

fn apply_fstab(contents: &[u8]) -> bool {
    let mut offset = 0usize;
    while offset < contents.len() {
        let line_end = contents[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|length| offset + length)
            .unwrap_or(contents.len());
        let mut line = trim_bytes(&contents[offset..line_end]);
        offset = if line_end < contents.len() {
            line_end + 1
        } else {
            contents.len()
        };
        if let Some(comment) = line.iter().position(|byte| *byte == b'#') {
            line = trim_bytes(&line[..comment]);
        }
        if line.is_empty() {
            continue;
        }

        let mut fields = [None; 6];
        let field_count = split_fields(line, &mut fields);
        if field_count < 4 {
            return false;
        }
        let source = fields[0].unwrap();
        let target = fields[1].unwrap();
        let filesystem = fields[2].unwrap();
        let options = fields[3].unwrap();
        if has_option(options, b"noauto") {
            continue;
        }
        let optional = has_option(options, b"optional");
        let flags = if has_option(options, b"ro") { 1 } else { 0 };
        let result = mount_fstab_entry(source, target, filesystem, flags);
        if (result as i64) < 0 {
            if !(optional && result == ERR_NO_ENTRY) {
                return false;
            }
        }
    }
    true
}

fn mount_fstab_entry(source: &[u8], target: &[u8], filesystem: &[u8], flags: u64) -> u64 {
    let mut source_buffer = [0u8; 128];
    let mut target_buffer = [0u8; 128];
    let mut filesystem_buffer = [0u8; 32];
    let Some(source) = copy_cstring(source, &mut source_buffer) else {
        return (-22i64) as u64;
    };
    let Some(target) = copy_cstring(target, &mut target_buffer) else {
        return (-22i64) as u64;
    };
    let Some(filesystem) = copy_cstring(filesystem, &mut filesystem_buffer) else {
        return (-22i64) as u64;
    };
    syscall(SYS_MOUNT, source, target, filesystem, flags, 0, 0)
}

fn split_fields<'a>(line: &'a [u8], fields: &mut [Option<&'a [u8]>; 6]) -> usize {
    let mut cursor = 0usize;
    let mut count = 0usize;
    while count < fields.len() {
        while cursor < line.len() && line[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == line.len() {
            break;
        }
        let start = cursor;
        while cursor < line.len() && !line[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        fields[count] = Some(&line[start..cursor]);
        count += 1;
    }
    count
}

fn has_option(options: &[u8], expected: &[u8]) -> bool {
    let mut start = 0usize;
    while start <= options.len() {
        let end = options[start..]
            .iter()
            .position(|byte| *byte == b',')
            .map(|offset| start + offset)
            .unwrap_or(options.len());
        if &options[start..end] == expected {
            return true;
        }
        if end == options.len() {
            break;
        }
        start = end + 1;
    }
    false
}

fn trim_bytes(data: &[u8]) -> &[u8] {
    let mut start = 0usize;
    let mut end = data.len();
    while start < end && data[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && data[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &data[start..end]
}

fn copy_cstring(data: &[u8], buffer: &mut [u8]) -> Option<u64> {
    if data.is_empty() || data.len() >= buffer.len() {
        return None;
    }
    buffer[..data.len()].copy_from_slice(data);
    buffer[data.len()] = 0;
    Some(buffer.as_ptr() as u64)
}

fn parse_u64(data: &[u8]) -> Option<u64> {
    if data.is_empty() {
        return None;
    }
    let (base, start) = if data.len() > 2 && data[0] == b'0' && data[1] == b'x' {
        (16, 2)
    } else if data.len() > 1 && data[0] == b'0' {
        (8, 1)
    } else {
        (10, 0)
    };
    if start == data.len() {
        return None;
    }
    let mut value = 0u64;
    let mut index = start;
    while index < data.len() {
        let byte = data[index];
        let digit = match byte {
            b'0'..=b'9' => (byte - b'0') as u64,
            b'a'..=b'f' if base == 16 => (byte - b'a' + 10) as u64,
            b'A'..=b'F' if base == 16 => (byte - b'A' + 10) as u64,
            _ => return None,
        };
        if digit >= base {
            return None;
        }
        value = value.checked_mul(base)?.checked_add(digit)?;
        index += 1;
    }
    Some(value)
}

fn apply_service_credentials(service: android_init_services::ServiceSpec<'_>) -> bool {
    if service.group_count > android_init_services::MAX_GROUPS {
        return false;
    }
    if (syscall(
        SYS_SETGROUPS,
        service.group_count as u64,
        service.groups.as_ptr() as u64,
        0,
        0,
        0,
        0,
    ) as i64)
        < 0
    {
        return false;
    }
    if (syscall(SYS_SETGID, service.gid as u64, 0, 0, 0, 0, 0) as i64) < 0 {
        return false;
    }
    (syscall(SYS_SETUID, service.uid as u64, 0, 0, 0, 0, 0) as i64) >= 0
}

fn apply_service_seclabel(service: android_init_services::ServiceSpec<'_>) -> bool {
    if service.seclabel.is_empty() {
        return true;
    }
    static EXEC: &[u8] = b"/proc/self/attr/exec\0";
    let fd = syscall(SYS_OPENAT, AT_FDCWD, EXEC.as_ptr() as u64, 1, 0, 0, 0);
    if (fd as i64) < 0 {
        return false;
    }
    let written = write_bytes(fd, service.seclabel);
    let closed = syscall(SYS_CLOSE, fd, 0, 0, 0, 0, 0) as i64 == 0;
    written && closed
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        data.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn write_result(client: u64, result: u32) -> bool {
    let bytes = result.to_ne_bytes();
    let mut written = 0usize;
    for _ in 0..SERVICE_RETRY_LIMIT {
        if written == bytes.len() {
            return true;
        }
        let count = syscall(
            SYS_WRITE,
            client,
            bytes[written..].as_ptr() as u64,
            (bytes.len() - written) as u64,
            0,
            0,
            0,
        );
        if (count as i64) > 0 {
            written += (count as usize).min(bytes.len() - written);
        } else {
            let _ = syscall(SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
        }
    }
    false
}

fn property_self_test(server: u64) -> bool {
    property_roundtrip(server, b"sys.fullerene.init", b"ready")
}

fn property_trigger_self_test(server: u64) -> bool {
    if !property_roundtrip(server, b"ctl.stop", FULLERENE_SERVICE_NAME)
        || service_pid(FULLERENE_SERVICE_NAME) != 0
    {
        return false;
    }
    property_roundtrip(server, b"sys.fullerene.trigger", b"restart")
        && service_pid(FULLERENE_SERVICE_NAME) != 0
}

fn service_self_test(server: u64) -> bool {
    if !property_roundtrip(server, b"ctl.start", FULLERENE_SERVICE_NAME)
        || service_pid(FULLERENE_SERVICE_NAME) == 0
    {
        return false;
    }
    if !property_roundtrip(server, b"ctl.stop", FULLERENE_SERVICE_NAME)
        || service_pid(FULLERENE_SERVICE_NAME) != 0
    {
        return false;
    }
    property_roundtrip(server, b"ctl.restart", FULLERENE_SERVICE_NAME)
        && service_pid(FULLERENE_SERVICE_NAME) != 0
}

fn service_pid(name: &[u8]) -> u64 {
    service_for(name)
        .map(|(index, _)| unsafe { SERVICE_PIDS[index] })
        .unwrap_or(0)
}

fn property_roundtrip(server: u64, name: &[u8], value: &[u8]) -> bool {
    if name.len() >= PROPERTY_NAME_MAX || value.len() >= PROPERTY_VALUE_MAX {
        return false;
    }
    let client = syscall(SYS_SOCKET, AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, 0, 0, 0);
    if (client as i64) < 0 {
        return false;
    }
    let (address, address_length) = property_address();
    if (syscall(
        SYS_CONNECT,
        client,
        address.as_ptr() as u64,
        address_length,
        0,
        0,
        0,
    ) as i64)
        < 0
    {
        let _ = syscall(SYS_CLOSE, client, 0, 0, 0, 0, 0);
        return false;
    }

    let mut message = [0u8; MAX_PROPERTY_MESSAGE];
    let mut offset = 0usize;
    message[offset..offset + 4].copy_from_slice(&PROP_MSG_SETPROP2.to_ne_bytes());
    offset += 4;
    message[offset..offset + 4].copy_from_slice(&(name.len() as u32).to_ne_bytes());
    offset += 4;
    message[offset..offset + name.len()].copy_from_slice(name);
    offset += name.len();
    message[offset..offset + 4].copy_from_slice(&(value.len() as u32).to_ne_bytes());
    offset += 4;
    message[offset..offset + value.len()].copy_from_slice(value);
    offset += value.len();
    if !write_bytes(client, &message[..offset]) {
        let _ = syscall(SYS_CLOSE, client, 0, 0, 0, 0, 0);
        return false;
    }

    let mut incoming = [0u8; MAX_PROPERTY_MESSAGE];
    let mut served = false;
    for _ in 0..SERVICE_RETRY_LIMIT {
        let accepted = syscall(SYS_ACCEPT4, server, 0, 0, 0, 0, 0);
        if (accepted as i64) >= 0 {
            serve_client(server, accepted, &mut incoming);
            let _ = syscall(SYS_CLOSE, accepted, 0, 0, 0, 0, 0);
            served = true;
            break;
        }
        let _ = syscall(SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
    }
    if !served {
        let _ = syscall(SYS_CLOSE, client, 0, 0, 0, 0, 0);
        return false;
    }

    let mut response = [0u8; 4];
    let mut received = 0usize;
    for _ in 0..SERVICE_RETRY_LIMIT {
        let count = syscall(
            SYS_READ,
            client,
            response[received..].as_mut_ptr() as u64,
            (response.len() - received) as u64,
            0,
            0,
            0,
        );
        if (count as i64) > 0 {
            received += (count as usize).min(response.len() - received);
            if received == response.len() {
                break;
            }
        } else {
            let _ = syscall(SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
        }
    }
    let _ = syscall(SYS_CLOSE, client, 0, 0, 0, 0, 0);
    received == response.len() && u32::from_ne_bytes(response) == PROP_SUCCESS
}

fn property_address() -> ([u8; 2 + 108], u64) {
    let mut address = [0u8; 2 + 108];
    address[0] = AF_UNIX as u8;
    address[1] = 0;
    let path_length = PROPERTY_SERVICE.len() - 1;
    address[2..2 + path_length].copy_from_slice(&PROPERTY_SERVICE[..path_length]);
    (address, (2 + path_length) as u64)
}

fn write_bytes(fd: u64, data: &[u8]) -> bool {
    let mut written = 0usize;
    for _ in 0..SERVICE_RETRY_LIMIT {
        if written == data.len() {
            return true;
        }
        let count = syscall(
            SYS_WRITE,
            fd,
            data[written..].as_ptr() as u64,
            (data.len() - written) as u64,
            0,
            0,
            0,
        );
        if (count as i64) > 0 {
            written += (count as usize).min(data.len() - written);
        } else {
            let _ = syscall(SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
        }
    }
    false
}

fn write(message: &[u8]) {
    let _ = syscall(
        SYS_WRITE,
        1,
        message.as_ptr() as u64,
        message.len() as u64,
        0,
        0,
        0,
    );
}

fn syscall(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> u64 {
    let mut result = arg0;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") result,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x4") arg4,
            in("x5") arg5,
            in("x8") number,
            options(nostack)
        );
    }
    result
}
