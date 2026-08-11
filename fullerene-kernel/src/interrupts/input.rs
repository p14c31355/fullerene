//! Input device interrupt handlers
//!
//! This module handles keyboard and mouse interrupts.

use super::apic::send_eoi;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use petroleum::port_read_u8;
use x86_64::structures::idt::{InterruptStackFrame, InterruptStackFrameValue};

static LAST_KLOG_LIVE_GENERATION: AtomicU64 = AtomicU64::new(0);
static LAST_SCHEDULER_PROGRESS_TICK: AtomicU64 = AtomicU64::new(0);
static LAST_SCHEDULER_PROGRESS_TSC: AtomicU64 = AtomicU64::new(0);
static SCHEDULER_STALL_REPORTED: AtomicBool = AtomicBool::new(false);

/// Paint a diagnostic even when the scheduler is stuck before the MMIO
/// watchdog is armed (for example in PCI config access or a contended lock).
/// This runs from the periodic timer interrupt and only touches atomics plus
/// the lock-free direct framebuffer diagnostic path.
fn check_scheduler_progress() {
    if crate::scheduler_context::SCHEDULER
        .recovery_target()
        .is_none()
    {
        return;
    }

    // `scheduler_loop` is also the shell's kernel-side execution context.
    // After a command handoff, the current user process can legitimately
    // run (or block in stdin) without advancing the idle-loop tick. Do not
    // mistake that normal process execution for a scheduler hang.
    if crate::process::current_pid().is_some() {
        LAST_SCHEDULER_PROGRESS_TICK.store(
            crate::scheduler_context::SCHEDULER.current_tick(),
            Ordering::Release,
        );
        LAST_SCHEDULER_PROGRESS_TSC
            .store(unsafe { core::arch::x86_64::_rdtsc() }, Ordering::Release);
        SCHEDULER_STALL_REPORTED.store(false, Ordering::Release);
        return;
    }

    let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let current_tick = crate::scheduler_context::SCHEDULER.current_tick();
    let previous_tick = LAST_SCHEDULER_PROGRESS_TICK.load(Ordering::Acquire);
    let previous_tsc = LAST_SCHEDULER_PROGRESS_TSC.load(Ordering::Acquire);

    if previous_tsc == 0 || current_tick != previous_tick {
        LAST_SCHEDULER_PROGRESS_TICK.store(current_tick, Ordering::Release);
        LAST_SCHEDULER_PROGRESS_TSC.store(now_tsc, Ordering::Release);
        SCHEDULER_STALL_REPORTED.store(false, Ordering::Release);
        return;
    }

    let timeout_tsc = solvent::get_tsc_per_ms().saturating_mul(3_000);
    if timeout_tsc != 0
        && now_tsc.wrapping_sub(previous_tsc) >= timeout_tsc
        && !SCHEDULER_STALL_REPORTED.swap(true, Ordering::AcqRel)
    {
        crate::boot_stage::draw_hang_diagnostic(b"SCHEDULER STALLED");
    }
}

/// Macro to create input device interrupt handlers
macro_rules! define_input_interrupt_handler {
    ($handler_name:ident, $port:expr, $status_value:expr, $process_input:expr) => {
        #[unsafe(no_mangle)]
        pub extern "x86-interrupt" fn $handler_name(_stack_frame: InterruptStackFrame) {
            // IRQ1 and IRQ12 can be spuriously delivered while the controller
            // output buffer belongs to the other PS/2 port. Do not consume a
            // byte unless its AUX bit matches this handler; otherwise one
            // stray keyboard byte can desynchronise all following mouse
            // packets and appear as a cursor teleport.
            let status = port_read_u8!(0x64);
            if status & 0x21 == $status_value {
                let data = port_read_u8!($port);
                $process_input(data);
            }
            send_eoi();
        }
    };
}

// Keyboard interrupt handler
//
// Reads one byte from the PS/2 data port and feeds it to the Nitrogen
// PS/2 keyboard driver for scancode processing.  The driver handles
// scancode-to-ASCII conversion, modifier keys, and input buffering.
define_input_interrupt_handler!(keyboard_handler, 0x60, 0x01, |scancode: u8| {
    nitrogen::ps2::keyboard::handle_keyboard_scancode(scancode);
});

// Mouse interrupt handler
//
// Reads one byte from the PS/2 data port and feeds it to the Nitrogen
// PS/2 mouse driver for packet processing.  No manual packet parsing
// is performed here – the driver handles that with proper validation.
define_input_interrupt_handler!(mouse_handler, 0x60, 0x21, |byte: u8| {
    nitrogen::ps2::mouse::handle_mouse_data(byte);
});

/// Timer interrupt handler (no preemption - scheduler loop handles yielding).
/// Also detects NMI MMIO watchdog recovery and redirects to the scheduler loop.
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn timer_handler(mut frame: InterruptStackFrame) {
    // Increment global tick counter (lock-free atomic increment)
    let tick = super::TICK_COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    if nitrogen::mmio::mmio_watchdog_recovery_triggered() {
        petroleum::serial::serial_log(format_args!(
            "[timer_handler] NMI recovery triggered — jumping to scheduler_loop\n"
        ));
        let restart_fn = crate::scheduler_context::SCHEDULER.recovery_target();
        if let Some((rsp, rip)) = restart_fn {
            let new_frame = InterruptStackFrameValue::new(
                rip,
                frame.code_segment,
                frame.cpu_flags,
                rsp,
                frame.stack_segment,
            );
            unsafe {
                frame.as_mut().write(new_frame);
            }
            // Clear the trigger only after successfully writing the new frame.
            // If no restart target is available, leave the trigger set so a
            // later recovery attempt can succeed.
            nitrogen::mmio::clear_watchdog_recovery_trigger();
            send_eoi();
            return;
        }
    }

    // Klog Live has a direct, lock-free repaint path so the existing window
    // can continue updating while the normal scheduler/compositor is blocked.
    if tick % 50 == 0 && solvent::is_klog_live_active() {
        let generation = crate::klog::generation();
        let last = LAST_KLOG_LIVE_GENERATION.load(Ordering::Acquire);
        if generation != last && crate::klog::try_render_live_surface() {
            LAST_KLOG_LIVE_GENERATION.store(generation, Ordering::Release);
        }
    }

    check_scheduler_progress();

    send_eoi();
}

/// I2C-HID GPIO interrupt handler.
///
/// The I2C transaction itself stays out of interrupt context.  The handler
/// only records that a report is ready, masks the level-triggered line, and
/// lets the scheduler perform the bounded transfer in normal context.
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn i2c_hid_handler(_stack_frame: InterruptStackFrame) {
    let gsi = nitrogen::hid::GEMIBOOK_N150_I2C_HID.interrupt_gsi;
    crate::interrupts::apic::set_gsi_masked(gsi, true);
    nitrogen::i2c_hid::handle_interrupt();
    super::apic::send_eoi();
}
