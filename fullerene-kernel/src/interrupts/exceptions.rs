//! CPU exception handlers with recovery mechanism
//!
//! This module provides handlers for all 32 CPU exception vectors with
//! a safe recovery mechanism. Key design principles:
//!
//! 1. **Safe stack** - All critical exceptions use IST stacks (configured in GDT)
//!    so they never run on a corrupted stack.
//! 2. **Lock-free logging** - Exception handlers write to serial directly using
//!    raw port I/O without acquiring locks.
//! 3. **User-mode recovery** - If a user process causes an exception, the handler
//!    records the fault and terminates the process. The scheduler then picks
//!    the next runnable process.
//! 4. **Kernel-mode safe panic** - If kernel code causes an exception, we log
//!    the fault and halt, avoiding triple faults.

use core::arch::asm;
use core::fmt::Write;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptStackFrame, InterruptStackFrameValue, PageFaultErrorCode};

// ──────────────────────────────────────────────
//  Raw serial output (lock-free)
// ──────────────────────────────────────────────

struct RawSerialWriter;

impl Write for RawSerialWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            while unsafe { core::ptr::read_volatile(0x3FD as *const u8) } & 0x20 == 0 {
                unsafe { asm!("pause", options(nomem, nostack, preserves_flags)) };
            }
            unsafe { core::ptr::write_volatile(0x3F8 as *mut u8, b) };
        }
        Ok(())
    }
}

#[inline(always)]
fn raw_serial_fmt(args: core::fmt::Arguments<'_>) {
    let _ = RawSerialWriter.write_fmt(args);
}

macro_rules! raw_log {
    ($($arg:tt)*) => {
        raw_serial_fmt(format_args!($($arg)*));
    };
}

// ──────────────────────────────────────────────
//  Helpers
// ──────────────────────────────────────────────

#[inline(always)]
fn is_user_mode(frame: &InterruptStackFrame) -> bool {
    frame.code_segment.0 & 3 == 3
}

fn exception_name(vector: u8) -> &'static str {
    match vector {
        0 => "Divide-by-zero",
        1 => "Debug",
        2 => "Non-maskable Interrupt",
        3 => "Breakpoint",
        4 => "Overflow",
        5 => "Bound Range Exceeded",
        6 => "Invalid Opcode",
        7 => "Device Not Available",
        8 => "Double Fault",
        9 => "Coprocessor Segment Overrun",
        10 => "Invalid TSS",
        11 => "Segment Not Present",
        12 => "Stack-Segment Fault",
        13 => "General Protection Fault",
        14 => "Page Fault",
        15 => "Reserved",
        16 => "x87 FPU Error",
        17 => "Alignment Check",
        18 => "Machine Check",
        19 => "SIMD FP Exception",
        20 => "Virtualization Exception",
        21 => "Control Protection Exception",
        22..=27 => "Reserved",
        28 => "Hypervisor Injection Exception",
        29 => "VMM Communication Exception",
        30 => "Security Exception",
        31 => "Reserved",
        _ => "Unknown",
    }
}

// ──────────────────────────────────────────────
//  Safe halt
// ──────────────────────────────────────────────

fn safe_halt() -> ! {
    raw_log!("--- System halted ---\n");
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack, preserves_flags)) };
    }
}

fn kernel_fault_halt(frame: &InterruptStackFrame, name: &str, extra: &str) -> ! {
    raw_log!(
        "\n=== KERNEL EXCEPTION: {} ===\n  RIP={:#x} RSP={:#x} CS={:#x}\n  Extra: {}\n",
        name,
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        frame.code_segment.0,
        extra,
    );
    
    petroleum::debug::print_backtrace(&mut RawSerialWriter);
    
    safe_halt()
}

// ──────────────────────────────────────────────
//  Trampoline for user-mode recovery
// ──────────────────────────────────────────────

static mut SCHEDULE_TRAMPOLINE: Option<x86_64::VirtAddr> = None;

/// Set the schedule trampoline address.
///
/// SAFETY: This function is marked unsafe because it writes to a static mut variable.
/// It is only called during system initialization (in init) or in exception handlers
/// after interrupts have been disabled, so there are no concurrency issues.
pub(crate) unsafe fn set_schedule_trampoline(addr: x86_64::VirtAddr) {
    unsafe {
        SCHEDULE_TRAMPOLINE = Some(addr);
    }
}

/// Recovery trampoline called after a user-mode exception to switch to the next process.
///
/// SAFETY: This function is called from exception handlers in kernel mode. The call to
/// `context_switch` is unsafe because it involves manipulating CPU state and stack pointers.
/// However, it is safe here because we are switching to a valid process that has been
/// properly scheduled and has a valid context.
#[unsafe(no_mangle)]
pub extern "C" fn exception_recovery_trampoline() -> ! {
    raw_log!("Recovery trampoline: cleaning up and scheduling next\n");
    crate::process::cleanup_terminated_processes();
    crate::process::schedule_next();
    let new_pid = crate::process::current_pid()
        .expect("schedule_next failed after exception");
    raw_log!("Switching to process {}\n", new_pid);
    unsafe { crate::process::context_switch(None, new_pid); }
    safe_halt()
}

/// Mark current process as terminated and redirect execution to trampoline.
/// Uses `as_mut()` + `Volatile::write()` to modify the interrupt return frame.
fn terminate_and_recover(frame: &mut InterruptStackFrame, reason: &str) {
    raw_log!("EXCEPTION: {} - terminating process\n", reason);

    let current_pid = crate::process::CURRENT_PROCESS
        .load(core::sync::atomic::Ordering::Relaxed);
    if current_pid == 0 {
        safe_halt();
    }

    let pid = crate::process::ProcessId(current_pid as u64);
    crate::process::PROCESS_MANAGER.with_process(pid, |p| {
        p.state = crate::process::ProcessState::Terminated;
        p.exit_code = Some(1);
    });

    unsafe {
        if let Some(tramp) = SCHEDULE_TRAMPOLINE {
            // Replace the interrupt return frame with one pointing to our trampoline.
            // The trampoline runs in kernel mode with kernel segments.
            let new_frame = InterruptStackFrameValue::new(
                tramp,
                crate::gdt::kernel_code_selector(),
                frame.cpu_flags,
                frame.stack_pointer,
                crate::gdt::kernel_data_selector(),
            );
            frame.as_mut().write(new_frame);
        } else {
            safe_halt();
        }
    }
}

// ──────────────────────────────────────────────
//  Generic handler macros
// ──────────────────────────────────────────────

macro_rules! define_no_err_handler {
    ($name:ident, $vector:expr) => {
        #[unsafe(no_mangle)]
        pub extern "x86-interrupt" fn $name(mut frame: InterruptStackFrame) {
            let exc_name = exception_name($vector);
            if is_user_mode(&frame) {
                raw_log!("EXC {} at user RIP={:#x}\n", exc_name, frame.instruction_pointer.as_u64());
                terminate_and_recover(&mut frame, exc_name);
            } else {
                kernel_fault_halt(&frame, exc_name, "");
            }
        }
    };
}

macro_rules! define_err_handler {
    ($name:ident, $vector:expr) => {
        #[unsafe(no_mangle)]
        pub extern "x86-interrupt" fn $name(
            mut frame: InterruptStackFrame,
            error_code: u64,
        ) {
            let exc_name = exception_name($vector);
            if is_user_mode(&frame) {
                raw_log!("EXC {} err={:#x} at user RIP={:#x}\n",
                    exc_name, error_code, frame.instruction_pointer.as_u64());
                terminate_and_recover(&mut frame, exc_name);
            } else {
                raw_log!("  Error code: {:#x}\n", error_code);
                kernel_fault_halt(&frame, exc_name, "kernel exc");
            }
        }
    };
}

// ──────────────────────────────────────────────
//  Handlers: no error code
// ──────────────────────────────────────────────

define_no_err_handler!(divide_error_handler, 0);
define_no_err_handler!(debug_handler, 1);
define_no_err_handler!(nmi_handler, 2);
define_no_err_handler!(overflow_handler, 4);
define_no_err_handler!(bound_range_exceeded_handler, 5);
define_no_err_handler!(invalid_opcode_handler, 6);
define_no_err_handler!(device_not_available_handler, 7);
define_no_err_handler!(coprocessor_segment_overrun_handler, 9);
define_no_err_handler!(x87_fp_error_handler, 16);
define_no_err_handler!(simd_fp_exception_handler, 19);
define_no_err_handler!(virtualization_handler, 20);
define_no_err_handler!(hv_injection_exception_handler, 28);

// ──────────────────────────────────────────────
//  Handlers: with error code
// ──────────────────────────────────────────────

define_err_handler!(invalid_tss_handler, 10);
define_err_handler!(segment_not_present_handler, 11);
define_err_handler!(stack_segment_fault_handler, 12);
define_err_handler!(general_protection_fault_handler, 13);
define_err_handler!(alignment_check_handler, 17);
define_err_handler!(cp_protection_exception_handler, 21);
define_err_handler!(vmm_communication_exception_handler, 29);
define_err_handler!(security_exception_handler, 30);

// ──────────────────────────────────────────────
//  Machine check (diverging handler)
// ──────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn machine_check_handler(frame: InterruptStackFrame) -> ! {
    kernel_fault_halt(&frame, "Machine Check", "");
}

// ──────────────────────────────────────────────
//  Breakpoint
// ──────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn breakpoint_handler(_frame: InterruptStackFrame) {
    raw_log!("\nBREAKPOINT\n");
}

// ──────────────────────────────────────────────
//  Double fault
// ──────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn double_fault_handler(
    mut frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    raw_log!(
        "\n=== DOUBLE FAULT === RIP={:#x} RSP={:#x} CS={:#x}\n",
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        frame.code_segment.0,
    );
    if is_user_mode(&frame) {
        raw_log!("  (user mode) - terminating process\n");
        let pid = crate::process::CURRENT_PROCESS
            .load(core::sync::atomic::Ordering::Relaxed);
        if pid != 0 {
            crate::process::PROCESS_MANAGER.with_process(
                crate::process::ProcessId(pid as u64),
                |p| { p.state = crate::process::ProcessState::Terminated; p.exit_code = Some(1); },
            );
            crate::process::cleanup_terminated_processes();
        }
    }
    safe_halt()
}

// ──────────────────────────────────────────────
//  Page fault
// ──────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn page_fault_handler(
    mut frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let fault_addr = match Cr2::read() {
        Ok(a) => a,
        Err(_) => {
            raw_log!("PF: CR2 invalid\n");
            if is_user_mode(&frame) {
                terminate_and_recover(&mut frame, "PF(invalid CR2)");
            } else {
                kernel_fault_halt(&frame, "Page Fault", "CR2 invalid");
            }
            return;
        }
    };

    let is_present = error_code.intersects(PageFaultErrorCode::PROTECTION_VIOLATION);
    let is_write = error_code.intersects(PageFaultErrorCode::CAUSED_BY_WRITE);
    let is_user = error_code.intersects(PageFaultErrorCode::USER_MODE);

    raw_log!(
        "PF @ {:#x}: {} {} {}\n",
        fault_addr.as_u64(),
        if is_present { "prot" } else { "np" },
        if is_write { "W" } else { "R" },
        if is_user { "(user)" } else { "(kernel)" },
    );

    if !is_user {
        raw_log!("  Fault addr: {:#x}\n", fault_addr.as_u64());
        kernel_fault_halt(&frame, "Page Fault", "kernel PF");
    } else {
        // User mode page fault - terminate the process
        if petroleum::common::memory::is_user_address(fault_addr) || is_present {
            terminate_and_recover(&mut frame, "Page Fault(user)");
        } else {
            terminate_and_recover(&mut frame, "Page Fault(invalid addr)");
        }
    }
}
