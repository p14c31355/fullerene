//! The reduced Nozzle command set for the ESP32 embedded profile.

use alloc::string::String;
use carrier::exec::CommandContext;

pub fn cmd_help(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str(
        "clear\thelp\tlatency\tmem\ttasks\treboot\tuptime\n",
    );
    true
}

/// Report the last drawing-latency probe.
///
/// The numbers come from the kernel's own measurement run, so this prints what
/// the device actually observed rather than what the clock rates imply.
pub fn cmd_latency(ctx: &mut CommandContext) -> bool {
    let report = latency_report();
    if report.is_empty() {
        ctx.terminal
            .write_str("no drawing latency probe has run yet\n");
    } else {
        ctx.terminal.write_str(&report);
    }
    true
}

pub fn cmd_mem(ctx: &mut CommandContext) -> bool {
    let (used, capacity) = meminfo();
    let text = String::from("heap: ");
    ctx.terminal.write_str(&text);
    let mut value = String::new();
    core::fmt::Write::write_fmt(&mut value, format_args!("{used}/{capacity} bytes\n")).ok();
    ctx.terminal.write_str(&value);
    true
}

pub fn cmd_tasks(ctx: &mut CommandContext) -> bool {
    for task in task_names() {
        ctx.terminal.write_str(&task);
        ctx.terminal.write_str("\n");
    }
    true
}

pub fn cmd_uptime(ctx: &mut CommandContext) -> bool {
    let mut value = String::new();
    core::fmt::Write::write_fmt(&mut value, format_args!("{} ms\n", uptime_millis())).ok();
    ctx.terminal.write_str(&value);
    true
}

pub fn cmd_reboot(ctx: &mut CommandContext) -> bool {
    ctx.terminal.write_str("Rebooting ESP32.\n");
    reboot()
}

pub const ESP32_COMMANDS: &[&dyn carrier::exec::Command] = carrier::define_commands!(
    ("help", "show ESP32 commands", cmd_help),
    ("mem", "show heap state", cmd_mem),
    ("tasks", "list scheduled tasks", cmd_tasks),
    ("latency", "show drawing latency measurements", cmd_latency),
    ("uptime", "show uptime", cmd_uptime),
    ("reboot", "reset the board", cmd_reboot),
);

/// Kernel-provided memory hooks; defaults are explicit, not fabricated.
pub static mut MEMINFO: Option<fn() -> (usize, usize)> = None;
pub static mut TASK_NAMES: Option<fn() -> alloc::vec::Vec<String>> = None;
pub static mut UPTIME: Option<fn() -> u64> = None;
pub static mut REBOOT: Option<fn() -> bool> = None;
/// Kernel-provided drawing-latency report; empty when no probe has run.
pub static mut LATENCY: Option<fn() -> String> = None;

fn meminfo() -> (usize, usize) {
    unsafe { MEMINFO.map(|hook| hook()).unwrap_or((0, 0)) }
}

fn task_names() -> alloc::vec::Vec<String> {
    unsafe { TASK_NAMES.map(|hook| hook()).unwrap_or_default() }
}

fn uptime_millis() -> u64 {
    unsafe { UPTIME.map(|hook| hook()).unwrap_or(0) }
}

fn reboot() -> bool {
    unsafe { REBOOT.map(|hook| hook()).unwrap_or(false) }
}

fn latency_report() -> String {
    unsafe { LATENCY.map(|hook| hook()).unwrap_or_default() }
}
