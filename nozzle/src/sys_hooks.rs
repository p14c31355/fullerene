//! System hooks for Nozzle shell commands.
//!
//! These hooks let the kernel register callbacks for system information
//! commands (`mem`, `tasks`, `windows`, `dmesg`) and system control
//! commands (`reboot`, `shutdown`).
//!
//! Both hooks are bundled into a single [`SysHooks`] struct.

use carrier::exec::CommandContext;
use spin::Mutex;

/// Aggregated system hooks.
pub struct SysHooks {
    pub info: Option<fn(&mut CommandContext, &str)>,
    pub ctl: Option<fn(&str)>,
}

impl SysHooks {
    pub const fn none() -> Self {
        Self {
            info: None,
            ctl: None,
        }
    }

    /// Atomically install this set of hooks into the global [`SYS_HOOKS`].
    pub fn install(self) {
        *SYS_HOOKS.lock() = self;
    }
}

/// Global system‑hooks bag.
pub static SYS_HOOKS: Mutex<SysHooks> = Mutex::new(SysHooks::none());

pub fn call_sys_info_hook(ctx: &mut CommandContext, cmd: &str) {
    let hooks = SYS_HOOKS.lock();
    if let Some(f) = hooks.info {
        drop(hooks);
        f(ctx, cmd);
    } else {
        drop(hooks);
        ctx.terminal
            .write_str("(not available from this context)\n");
    }
}

/// SD card mount hook — set by the kernel.
pub static SD_MOUNT_HOOK: Mutex<Option<fn(&mut carrier::exec::CommandContext)>> = Mutex::new(None);

/// Block device mount hook — set by the kernel.
/// Called with args: ["mount", "/dev/<name>", "<mount_point>"]
pub static MOUNT_HOOK: Mutex<Option<fn(&mut carrier::exec::CommandContext)>> = Mutex::new(None);



pub fn call_sys_control_hook(cmd: &str) {
    let hooks = SYS_HOOKS.lock();
    if let Some(f) = hooks.ctl {
        drop(hooks);
        f(cmd);
    }
}
