// Linux signal syscall implementations
use super::numbers::*;
use super::runtime::{LinuxRuntime, copy_from_user, copy_val_to_user, errno_code};
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
        let old = &rt.signal_handlers[idx];
        unsafe { copy_val_to_user(oldact, old) }.ok();
    }

    // If act != NULL, set new handler
    if act != 0 {
        const SIGACTION_SIZE: usize = core::mem::size_of::<LinuxSigAction>();
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
    }

    0
}

pub fn sys_rt_sigprocmask(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _how = args[0] as i32; // SIG_BLOCK=0, SIG_UNBLOCK=1, SIG_SETMASK=2
    let _set = args[1]; // user pointer to sigset_t
    let _oldset = args[2]; // user pointer to old sigset_t

    // Read/write signal masks (simplified)
    if _oldset != 0 {
        let mask = rt.signal_pending;
        unsafe { core::ptr::write_volatile(_oldset as *mut u64, mask) };
    }

    if _set != 0 {
        let new_mask = unsafe { core::ptr::read_volatile(_set as *const u64) };
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
