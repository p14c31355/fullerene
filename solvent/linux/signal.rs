// Linux signal syscall implementations
use super::numbers::*;
use super::runtime::{
    LinuxRuntime, copy_from_user, copy_val_from_user, copy_val_to_user, errno_code,
};
use super::types::*;

pub fn sys_rt_sigaction(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let sig = args[0] as i32;
    let act = args[1]; // user pointer to new sigaction
    let oldact = args[2]; // user pointer to old sigaction
    let _sigsetsize = args[3];

    if sig < 1 || sig > 64 {
        return errno_code(EINVAL);
    }
    if sig == SIGKILL || sig == SIGSTOP {
        return errno_code(EINVAL);
    }

    let idx = (sig - 1) as usize;

    // If oldact != NULL, save current handler
    if oldact != 0 {
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] rt_sigaction oldact copy to {oldact:#x}\n"
        ));
        let old = &rt.signal_handlers[idx];
        if unsafe { copy_val_to_user(oldact, old) }.is_err() {
            return errno_code(EFAULT);
        }
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] rt_sigaction oldact copy done\n"
        ));
    }

    // If act != NULL, set new handler
    if act != 0 {
        const SIGACTION_SIZE: usize = core::mem::size_of::<LinuxSigAction>();
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!(
            "[linux-smoke] rt_sigaction act copy from {act:#x}, size {SIGACTION_SIZE}\n"
        ));
        let new = match unsafe { copy_from_user(act, SIGACTION_SIZE) } {
            Ok(data) => {
                if data.len() < SIGACTION_SIZE {
                    return errno_code(EFAULT);
                }
                unsafe { core::ptr::read_unaligned(data.as_ptr() as *const LinuxSigAction) }
            }
            Err(_) => return errno_code(EFAULT),
        };
        rt.signal_handlers[idx] = new;
        #[cfg(linux_musl_smoke)]
        petroleum::serial::serial_log(format_args!("[linux-smoke] rt_sigaction act copy done\n"));
    }

    0
}

pub fn sys_rt_sigprocmask(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _how = args[0] as i32; // SIG_BLOCK=0, SIG_UNBLOCK=1, SIG_SETMASK=2
    let set = args[1]; // user pointer to sigset_t
    let oldset = args[2]; // user pointer to old sigset_t

    // Read/write signal masks (simplified)
    if oldset != 0 {
        let mask = rt.signal_pending;
        if unsafe { copy_val_to_user(oldset, &mask) }.is_err() {
            return errno_code(EFAULT);
        }
    }

    if set != 0 {
        let new_mask = match unsafe { copy_val_from_user::<u64>(set) } {
            Ok(mask) => mask,
            Err(_) => return errno_code(EFAULT),
        };
        match _how {
            0 => rt.signal_pending |= new_mask,  // SIG_BLOCK
            1 => rt.signal_pending &= !new_mask, // SIG_UNBLOCK
            2 => rt.signal_pending = new_mask,   // SIG_SETMASK
            _ => return errno_code(EINVAL),
        }
    }

    0
}

pub fn sys_rt_sigreturn(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    // In a real implementation, this would restore the signal context
    // from the stack and return to the interrupted code.
    // For now, just return -EINTR to simulate a signal interruption.
    0
}

pub fn sys_sigaltstack(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    const SS_DISABLE: i32 = 2;
    const MINSIGSTKSZ: u64 = 2048;

    let new_stack = args[0];
    let old_stack = args[1];

    if old_stack != 0 && unsafe { copy_val_to_user(old_stack, &rt.signal_alt_stack) }.is_err() {
        return errno_code(EFAULT);
    }
    if new_stack == 0 {
        return 0;
    }

    let data = match unsafe { copy_from_user(new_stack, core::mem::size_of::<LinuxStack>()) } {
        Ok(data) => data,
        Err(_) => return errno_code(EFAULT),
    };
    let requested = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const LinuxStack) };
    if requested.ss_flags == SS_DISABLE {
        rt.signal_alt_stack = LinuxStack::disabled();
        return 0;
    }
    if requested.ss_flags != 0 || requested.ss_sp == 0 || requested.ss_size < MINSIGSTKSZ {
        return errno_code(EINVAL);
    }
    let Some(last) = requested
        .ss_sp
        .checked_add(requested.ss_size.saturating_sub(1))
    else {
        return errno_code(EINVAL);
    };
    let valid_range = x86_64::VirtAddr::try_new(requested.ss_sp)
        .ok()
        .zip(x86_64::VirtAddr::try_new(last).ok())
        .is_some_and(|(start, end)| {
            petroleum::is_user_address(start) && petroleum::is_user_address(end)
        });
    if !valid_range {
        return errno_code(EINVAL);
    }

    rt.signal_alt_stack = requested;
    0
}
