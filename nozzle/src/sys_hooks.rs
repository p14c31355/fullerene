//! System hooks for Nozzle shell commands.
//!
//! These hooks let the kernel register callbacks for system information
//! commands (`mem`, `tasks`, `windows`, `dmesg`) and system control
//! commands (`reboot`, `shutdown`).

use crate::exec::CommandContext;
use alloc::string::String;
use spin::Mutex;

/// Callback for system info commands: receives the command name
/// ("mem", "tasks", "windows", "dmesg") and the terminal context.
pub static SYS_INFO_FN: Mutex<
    Option<fn(&mut CommandContext, &str)>,
> = Mutex::new(None);

/// Callback for system control commands: receives "reboot" or "shutdown".
pub static SYS_CTL_FN: Mutex<
    Option<fn(&str)>,
> = Mutex::new(None);

pub fn set_sys_info_fn(f: fn(&mut CommandContext, &str)) {
    *SYS_INFO_FN.lock() = Some(f);
}

pub fn set_sys_ctl_fn(f: fn(&str)) {
    *SYS_CTL_FN.lock() = Some(f);
}

pub fn call_sys_info_hook(ctx: &mut CommandContext, cmd: &str) {
    if let Some(f) = *SYS_INFO_FN.lock() {
        f(ctx, cmd);
    } else {
        ctx.terminal.write_str("(not available from this context)\n");
    }
}

pub fn call_sys_control_hook(cmd: &str) {
    if let Some(f) = *SYS_CTL_FN.lock() {
        f(cmd);
    }
}