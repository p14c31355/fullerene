//! CPU exception handlers with recovery mechanism

use core::fmt::Write;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptStackFrame, InterruptStackFrameValue, PageFaultErrorCode};

// ── Raw serial output (lock-free) ──────────────────────────────

struct RawSerialWriter;

impl Write for RawSerialWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            unsafe {
                let mut timeout = 1_000_000;
                while timeout > 0 {
                    let status: u8;
                    core::arch::asm!(
                        "in al, dx",
                        out("al") status,
                        in("dx") 0x3FDu16,
                        options(nomem, nostack, preserves_flags),
                    );
                    if status & 0x20 != 0 {
                        break;
                    }
                    timeout -= 1;
                }
                core::arch::asm!(
                    "out dx, al",
                    in("dx") 0x3F8u16,
                    in("al") b,
                    options(nomem, nostack, preserves_flags),
                );
            }
        }
        Ok(())
    }
}

#[inline(always)]
fn raw_serial_fmt(args: core::fmt::Arguments<'_>) {
    let _ = RawSerialWriter.write_fmt(args);
}

macro_rules! raw_log {
    ($($arg:tt)*) => { raw_serial_fmt(format_args!($($arg)*)); };
}

// ── Helpers ────────────────────────────────────────────────────

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
        10 => "Invalid TSS",
        11 => "Segment Not Present",
        12 => "Stack-Segment Fault",
        13 => "General Protection Fault",
        14 => "Page Fault",
        16 => "x87 FPU Error",
        17 => "Alignment Check",
        18 => "Machine Check",
        19 => "SIMD FP Exception",
        20 => "Virtualization Exception",
        21 => "Control Protection Exception",
        28 => "Hypervisor Injection Exception",
        29 => "VMM Communication Exception",
        30 => "Security Exception",
        _ => "Unknown",
    }
}

// ── Safe halt ──────────────────────────────────────────────────

fn safe_halt() -> ! {
    raw_log!("--- System halted ---\n");
    loop {
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}

fn kernel_fault_halt(frame: &InterruptStackFrame, name: &str, extra: &str) -> ! {
    crate::klog_fmt!(
        "[FAULT] {} RIP={:#x} RSP={:#x} CS={:#x} {}\n",
        name,
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        frame.code_segment.0,
        extra
    );
    raw_log!(
        "\n=== KERNEL EXCEPTION: {} ===\n  RIP={:#x} RSP={:#x} CS={:#x}\n  Extra: {}\n",
        name,
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        frame.code_segment.0,
        extra
    );
    let mut collector = petroleum::debug::BacktraceCollector::new();
    collector.capture();
    raw_log!("Backtrace:\n");
    for (i, entry) in collector.entries().iter().enumerate() {
        raw_log!("  [{}] {:#x}\n", i, entry.ip);
    }
    safe_halt()
}

#[inline(always)]
fn render_fault_diagnostic() {
    let _ = crate::klog::try_render_live_surface();
}

fn page_walk_flags(address: u64) -> [u64; 4] {
    let (root, _) = x86_64::registers::control::Cr3::read();
    let mut result = [0u64; 4];
    let _ = unsafe {
        crate::memory_management::walk_page_table_entries(
            root.start_address(),
            address,
            |level, entry| result[level] = entry.flags().bits(),
        )
    };
    result
}

// ── Trampoline for user-mode recovery ──────────────────────────

static mut SCHEDULE_TRAMPOLINE: Option<x86_64::VirtAddr> = None;

pub(crate) unsafe fn set_schedule_trampoline(addr: x86_64::VirtAddr) {
    unsafe {
        SCHEDULE_TRAMPOLINE = Some(addr);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn exception_recovery_trampoline() -> ! {
    raw_log!("Recovery trampoline: cleaning up and scheduling next\n");
    crate::process::SCHEDULER.cleanup();
    crate::process::schedule_next();
    let new_pid = crate::process::current_pid().expect("schedule_next failed after exception");
    raw_log!("Switching to process {}\n", new_pid);
    unsafe {
        crate::process::context_switch(None, new_pid);
    }
    safe_halt()
}

fn terminate_and_recover(
    frame: &mut InterruptStackFrame,
    reason: &'static str,
    address: u64,
    error_code: u64,
) {
    raw_log!("EXCEPTION: {} - terminating process\n", reason);
    let current_pid = crate::process::SCHEDULER.current_pid();
    if current_pid == 0 {
        safe_halt();
    }
    let pid = crate::process::ProcessId(current_pid as u64);
    crate::process::mark_faulted(
        pid,
        crate::process::FaultRecord {
            reason,
            rip: frame.instruction_pointer.as_u64(),
            rsp: frame.stack_pointer.as_u64(),
            address,
            error_code,
        },
    );
    unsafe {
        if let Some(tramp) = SCHEDULE_TRAMPOLINE {
            let new_frame = InterruptStackFrameValue::new(
                tramp,
                crate::gdt::kernel_code_selector(),
                frame.cpu_flags,
                frame.stack_pointer,
                crate::gdt::kernel_data_selector(),
            );
            // SAFETY: InterruptStackFrameValue::write() modifies the interrupt stack
            // frame in-place. We own the frame and the write targets a valid value.
            frame.as_mut().write(new_frame);
        } else {
            safe_halt();
        }
    }
}

// ── Generic handler macros ────────────────────────────────────

macro_rules! define_no_err_handler {
    ($name:ident, $vector:expr) => {
        #[unsafe(no_mangle)]
        pub extern "x86-interrupt" fn $name(mut frame: InterruptStackFrame) {
            let exc_name = exception_name($vector);
            if is_user_mode(&frame) {
                raw_log!(
                    "EXC {} at user RIP={:#x}\n",
                    exc_name,
                    frame.instruction_pointer.as_u64()
                );
                crate::klog_fmt!(
                    "[FAULT] {} at user RIP={:#x}\n",
                    exc_name,
                    frame.instruction_pointer.as_u64()
                );
                render_fault_diagnostic();
                terminate_and_recover(&mut frame, exc_name, 0, 0);
            } else {
                kernel_fault_halt(&frame, exc_name, "");
            }
        }
    };
}

macro_rules! define_err_handler {
    ($name:ident, $vector:expr) => {
        #[unsafe(no_mangle)]
        pub extern "x86-interrupt" fn $name(mut frame: InterruptStackFrame, error_code: u64) {
            let exc_name = exception_name($vector);
            if is_user_mode(&frame) {
                raw_log!(
                    "EXC {} err={:#x} at user RIP={:#x}\n",
                    exc_name,
                    error_code,
                    frame.instruction_pointer.as_u64()
                );
                crate::klog_fmt!(
                    "[FAULT] {} err={:#x} at user RIP={:#x}\n",
                    exc_name,
                    error_code,
                    frame.instruction_pointer.as_u64()
                );
                render_fault_diagnostic();
                terminate_and_recover(&mut frame, exc_name, 0, error_code);
            } else {
                raw_log!("  Error code: {:#x}\n", error_code);
                kernel_fault_halt(&frame, exc_name, "kernel exc");
            }
        }
    };
}

define_no_err_handler!(divide_error_handler, 0);
define_no_err_handler!(debug_handler, 1);

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn nmi_handler(mut frame: InterruptStackFrame) {
    if nitrogen::mmio::mmio_watchdog_armed() {
        raw_log!("NMI: MMIO watchdog expired — forcing recovery\n");
        // The normal desktop renderer may be the code that is stuck.  Paint
        // the diagnostic before redirecting execution to the recovery path;
        // this bypasses runtime locks and remains visible without serial.
        crate::boot_stage::draw_hang_diagnostic(b"IWLWIFI MMIO HANG");
        nitrogen::mmio::mmio_watchdog_nmi_recovery();
        let trampoline =
            x86_64::VirtAddr::from_ptr(nitrogen::mmio::mmio_nmi_recovery_trampoline as *const ());
        let new_frame = InterruptStackFrameValue::new(
            trampoline,
            frame.code_segment,
            frame.cpu_flags,
            frame.stack_pointer,
            frame.stack_segment,
        );
        unsafe {
            frame.as_mut().write(new_frame);
        }
        return;
    }
    raw_log!("NMI: unexpected — halting\n");
    safe_halt();
}
define_no_err_handler!(overflow_handler, 4);
define_no_err_handler!(bound_range_exceeded_handler, 5);
define_no_err_handler!(invalid_opcode_handler, 6);
define_no_err_handler!(device_not_available_handler, 7);
define_no_err_handler!(coprocessor_segment_overrun_handler, 9);
define_no_err_handler!(x87_fp_error_handler, 16);
define_no_err_handler!(simd_fp_exception_handler, 19);
define_no_err_handler!(virtualization_handler, 20);
define_no_err_handler!(hv_injection_exception_handler, 28);

define_err_handler!(invalid_tss_handler, 10);
define_err_handler!(segment_not_present_handler, 11);
define_err_handler!(stack_segment_fault_handler, 12);
define_err_handler!(general_protection_fault_handler, 13);
define_err_handler!(alignment_check_handler, 17);
define_err_handler!(cp_protection_exception_handler, 21);
define_err_handler!(vmm_communication_exception_handler, 29);
define_err_handler!(security_exception_handler, 30);

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn machine_check_handler(frame: InterruptStackFrame) -> ! {
    kernel_fault_halt(&frame, "Machine Check", "");
}

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn breakpoint_handler(_frame: InterruptStackFrame) {
    raw_log!("\nBREAKPOINT\n");
}

#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn double_fault_handler(
    frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    raw_log!(
        "\n=== DOUBLE FAULT === RIP={:#x} RSP={:#x} CS={:#x}\n",
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        frame.code_segment.0
    );
    if is_user_mode(&frame) {
        let pid = crate::process::SCHEDULER.current_pid();
        if pid != 0 {
            crate::process::SCHEDULER.with_process(crate::process::ProcessId(pid as u64), |p| {
                p.state = crate::process::ProcessState::Terminated;
                p.exit_code = Some(1);
            });
            crate::process::SCHEDULER.cleanup();
        }
    }
    safe_halt()
}

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
                terminate_and_recover(&mut frame, "PF(invalid CR2)", 0, 0);
            } else {
                kernel_fault_halt(&frame, "Page Fault", "CR2 invalid");
            }
            return;
        }
    };

    let is_present = error_code.intersects(PageFaultErrorCode::PROTECTION_VIOLATION);
    let is_write = error_code.intersects(PageFaultErrorCode::CAUSED_BY_WRITE);
    let is_user = error_code.intersects(PageFaultErrorCode::USER_MODE);

    let cow_recovered = if is_present && is_write && is_user {
        unsafe {
            crate::solvent_linux::process::force_user_page_writable(
                x86_64::registers::control::Cr3::read().0.start_address(),
                fault_addr.as_u64(),
            )
        }
    } else {
        false
    };
    if cow_recovered {
        unsafe {
            let (root, flags) = x86_64::registers::control::Cr3::read();
            x86_64::registers::control::Cr3::write(root, flags);
        }
        return;
    }

    if !is_user {
        if !is_present
            && crate::memory_management::try_map_kernel_heap_extension_page(
                fault_addr.as_u64() as usize
            )
        {
            raw_log!(
                "PF: mapped kernel heap extension page @ {:#x}; resuming\n",
                fault_addr.as_u64() & !0xfff
            );
            return;
        }
        raw_log!("  Fault addr: {:#x}\n", fault_addr.as_u64());
        kernel_fault_halt(&frame, "Page Fault", "kernel PF");
    } else {
        raw_log!(
            "PF @ {:#x}: {} {} (user) rip={:#x} rsp={:#x}\n",
            fault_addr.as_u64(),
            if is_present { "prot" } else { "np" },
            if is_write { "W" } else { "R" },
            frame.instruction_pointer,
            frame.stack_pointer
        );
        let walk = page_walk_flags(fault_addr.as_u64());
        crate::klog_fmt!(
            "[FAULT] Page Fault addr={:#x} {} {} user RIP={:#x} RSP={:#x}\n",
            fault_addr.as_u64(),
            if is_present { "prot" } else { "np" },
            if is_write { "W" } else { "R" },
            frame.instruction_pointer.as_u64(),
            frame.stack_pointer.as_u64()
        );
        crate::klog_fmt!(
            "[FAULT] Page Fault err={:#x} walk={:#x}/{:#x}/{:#x}/{:#x}\n",
            error_code.bits(),
            walk[0],
            walk[1],
            walk[2],
            walk[3]
        );
        render_fault_diagnostic();
        if petroleum::common::memory::is_user_address(fault_addr) || is_present {
            terminate_and_recover(
                &mut frame,
                "Page Fault(user)",
                fault_addr.as_u64(),
                error_code.bits(),
            );
        } else {
            terminate_and_recover(
                &mut frame,
                "Page Fault(invalid addr)",
                fault_addr.as_u64(),
                error_code.bits(),
            );
        }
    }
}
