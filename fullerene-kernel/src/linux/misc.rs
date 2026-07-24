// Miscellaneous Linux syscall implementations
use super::numbers::*;
use super::runtime::{LinuxRuntime, copy_to_user, copy_val_to_user, errno_code};
use super::types::*;

pub fn sys_uname(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let buf = args[0];
    if buf == 0 {
        return errno_code(EFAULT);
    }
    let utsname = LinuxUtsname::new();
    match unsafe { copy_val_to_user(buf, &utsname) } {
        Ok(()) => 0,
        Err(error) => errno_code(error),
    }
}

pub fn sys_arch_prctl(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let code = args[0] as i32;
    let addr = args[1];

    // MSR addresses for FS.base and GS.base
    const MSR_FS_BASE: u32 = 0xC0000100;
    const MSR_GS_BASE: u32 = 0xC0000101;

    if matches!(code, ARCH_SET_FS | ARCH_SET_GS)
        && (addr != 0
            && x86_64::VirtAddr::try_new(addr)
                .map(|address| !petroleum::is_user_address(address))
                .unwrap_or(true))
    {
        return errno_code(EINVAL);
    }

    match code {
        ARCH_SET_FS => {
            rt.tls_ptr = addr;
            unsafe {
                x86_64::registers::model_specific::Msr::new(MSR_FS_BASE).write(addr);
            }
            0
        }
        ARCH_GET_FS => {
            if addr != 0 {
                let val =
                    unsafe { x86_64::registers::model_specific::Msr::new(MSR_FS_BASE).read() };
                if unsafe { copy_val_to_user(addr, &val) }.is_err() {
                    return errno_code(EFAULT);
                }
            }
            0
        }
        ARCH_SET_GS => {
            unsafe {
                x86_64::registers::model_specific::Msr::new(MSR_GS_BASE).write(addr);
            }
            0
        }
        ARCH_GET_GS => {
            if addr != 0 {
                let val =
                    unsafe { x86_64::registers::model_specific::Msr::new(MSR_GS_BASE).read() };
                if unsafe { copy_val_to_user(addr, &val) }.is_err() {
                    return errno_code(EFAULT);
                }
            }
            0
        }
        _ => errno_code(EINVAL),
    }
}

pub fn sys_set_tid_address(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let tidptr = args[0];
    rt.child_clear_tid = tidptr;
    rt.tid
}

pub fn sys_set_robust_list(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let head = args[0];
    let len = args[1];
    rt.robust_list_head = head;
    rt.robust_list_len = len;
    0
}

pub fn sys_get_robust_list(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _tid = args[0] as i32;
    let head_ptr = args[1];
    let _len_ptr = args[2];
    if head_ptr != 0 {
        if unsafe { copy_val_to_user(head_ptr, &rt.robust_list_head) }.is_err() {
            return errno_code(EFAULT);
        }
    }
    if _len_ptr != 0 {
        if unsafe { copy_val_to_user(_len_ptr, &rt.robust_list_len) }.is_err() {
            return errno_code(EFAULT);
        }
    }
    0
}

pub fn sys_getrandom(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let buf = args[0];
    let count = args[1];
    let _flags = args[2] as u32;

    if buf == 0 {
        return errno_code(EFAULT);
    }

    use core::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0);

    if count > 64 * 1024 {
        return errno_code(E2BIG);
    }
    let mut bytes = alloc::vec![0u8; count as usize];
    for byte in bytes.iter_mut() {
        let mut current = SEED.load(Ordering::Relaxed);
        if current == 0 {
            current = unsafe { core::arch::x86_64::_rdtsc() } ^ 0x9e3779b97f4a7c15;
        }
        let mut next = current;
        loop {
            next = next
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            match SEED.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        *byte = (next >> 32) as u8;
    }

    if unsafe { copy_to_user(buf, &bytes) }.is_err() {
        return errno_code(EFAULT);
    }
    count as u64
}

pub fn sys_prlimit64(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _pid = args[0] as i32;
    let resource = args[1] as i32;
    let _new_rlim = args[2];
    let old_rlim = args[3];

    if old_rlim != 0 {
        let rlim = match resource {
            RLIMIT_NOFILE => LinuxRLimit {
                rlim_cur: 256,
                rlim_max: 1024,
            },
            RLIMIT_STACK => LinuxRLimit {
                rlim_cur: 8 * 1024 * 1024,
                rlim_max: 8 * 1024 * 1024,
            },
            RLIMIT_NPROC => LinuxRLimit {
                rlim_cur: 64,
                rlim_max: 64,
            },
            RLIMIT_AS => LinuxRLimit {
                rlim_cur: u64::MAX,
                rlim_max: u64::MAX,
            },
            _ => LinuxRLimit {
                rlim_cur: u64::MAX,
                rlim_max: u64::MAX,
            },
        };
        if unsafe { copy_val_to_user(old_rlim, &rlim) }.is_err() {
            return errno_code(EFAULT);
        }
    }
    0
}

pub fn sys_getrlimit(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let resource = args[0] as i32;
    let rlim = args[1];

    if rlim == 0 {
        return errno_code(EFAULT);
    }

    let limit = match resource {
        RLIMIT_NOFILE => LinuxRLimit {
            rlim_cur: 256,
            rlim_max: 1024,
        },
        RLIMIT_STACK => LinuxRLimit {
            rlim_cur: 8 * 1024 * 1024,
            rlim_max: 8 * 1024 * 1024,
        },
        RLIMIT_NPROC => LinuxRLimit {
            rlim_cur: 64,
            rlim_max: 64,
        },
        RLIMIT_AS => LinuxRLimit {
            rlim_cur: u64::MAX,
            rlim_max: u64::MAX,
        },
        _ => LinuxRLimit {
            rlim_cur: u64::MAX,
            rlim_max: u64::MAX,
        },
    };

    match unsafe { copy_val_to_user(rlim, &limit) } {
        Ok(()) => 0,
        Err(error) => errno_code(error),
    }
}

linux_stub!(sys_setrlimit, 0);

pub fn sys_sched_yield(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    crate::process::yield_current();
    0
}

linux_stub!(sys_getuid, 0);
linux_stub!(sys_getgid, 0);
linux_stub!(sys_geteuid, 0);
linux_stub!(sys_getegid, 0);

pub fn sys_umask(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let new_mask = args[0] as u32;
    let old = rt.umask;
    rt.umask = new_mask & 0o777;
    old as u64
}

linux_stub!(sys_capget, 0);

pub fn sys_sysinfo(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let info = args[0];
    if info == 0 {
        return errno_code(EFAULT);
    }
    let si = LinuxSysinfo::new();
    match unsafe { copy_val_to_user(info, &si) } {
        Ok(()) => 0,
        Err(error) => errno_code(error),
    }
}

linux_stub!(sys_prctl, 0);

pub fn sys_futex(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _uaddr = args[0];
    let op = args[1] as i32;
    let _val = args[2] as i32;
    let _uaddr2 = args[3];
    let _val3 = args[4] as i32;

    const FUTEX_WAIT: i32 = 0;
    const FUTEX_WAKE: i32 = 1;

    match op & 0xf {
        FUTEX_WAIT => {
            // In a real impl, this would block. For now, just return 0.
            0
        }
        FUTEX_WAKE => {
            // Return number of woken threads (0 for now)
            0
        }
        _ => errno_code(ENOSYS),
    }
}

linux_stub_errno!(sys_statfs, ENOSYS);
linux_stub_errno!(sys_fstatfs, ENOSYS);

pub fn sys_sched_getaffinity(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _pid = args[0] as i32;
    let cpusetsize = args[1];
    let mask = args[2];

    if mask == 0 {
        return errno_code(EFAULT);
    }

    // Return a mask indicating CPU 0 is available
    let cpusetsize = cpusetsize.min(8); // at most 8 bytes
    let mut result = [0u8; 8];
    result[0] = 1;
    if unsafe { copy_to_user(mask, &result[..cpusetsize as usize]) }.is_err() {
        return errno_code(EFAULT);
    }
    0
}

linux_stub!(sys_sched_setaffinity, 0);
