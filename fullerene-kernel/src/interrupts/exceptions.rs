//! CPU exception handlers
//!
//! This module provides handlers for CPU exceptions like page faults,
//! breakpoints, and double faults.

use core::fmt::Write;
use crate::memory_management;
use crate::process;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::InterruptStackFrame;
use x86_64::structures::idt::PageFaultErrorCode;

/// Breakpoint exception handler
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    petroleum::lock_and_modify!(petroleum::SERIAL1, writer, {
        writeln!(writer, "\nEXCEPTION: BREAKPOINT\n{:#?}", stack_frame).ok();
    });
}

/// Page fault exception handler
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let fault_addr = match Cr2::read() {
        Ok(addr) => addr,
        Err(_) => {
            petroleum::serial::serial_log(format_args!(
                "\nEXCEPTION: PAGE FAULT but CR2 is invalid.\n"
            ));
            return;
        }
    };

    handle_page_fault(fault_addr, error_code, stack_frame);
}

/// Handle page fault logic
pub fn handle_page_fault(
    fault_addr: x86_64::VirtAddr,
    error_code: PageFaultErrorCode,
    _stack_frame: InterruptStackFrame,
) {
    let is_present = error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION);
    let is_write = error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE);
    let is_user = error_code.contains(PageFaultErrorCode::USER_MODE);

    petroleum::lock_and_modify!(petroleum::SERIAL1, writer, {
        write!(writer, "Page fault analysis: ").ok();
        if is_present {
            write!(writer, "Protection violation ").ok();
        } else {
            write!(writer, "Page not present ").ok();
        }
        if is_write {
            write!(writer, "(write access) ").ok();
        }
        if is_user {
            write!(writer, "(user mode)").ok();
        }
        writeln!(writer).ok();
    });

    if !is_user {
        // Kernel page fault - this is critical
        panic!(
            "Kernel page fault at {:#x}: {:?}",
            fault_addr.as_u64(),
            error_code
        );
    }

    if is_present {
        // Protection violation in user space
        petroleum::lock_and_modify!(petroleum::SERIAL1, writer, {
            write!(writer, "Protection violation in user space - terminating process\n").ok();
        });

        if let Some(pid) = crate::process::current_pid() {
            crate::process::terminate_process(pid, 1);
        }
    } else {
        // Page not present - attempt demand paging
        petroleum::lock_and_modify!(petroleum::SERIAL1, writer, {
            write!(writer, "Page not present - attempting to handle\n").ok();
        });

        if memory_management::is_user_address(fault_addr) {
            // For now, terminate process
            petroleum::lock_and_modify!(petroleum::SERIAL1, writer, {
                write!(writer, "Cannot handle page fault - terminating process\n").ok();
            });
            if let Some(pid) = crate::process::current_pid() {
                crate::process::terminate_process(pid, 1);
            }
        } else {
            // Invalid user address
            petroleum::lock_and_modify!(petroleum::SERIAL1, writer, {
                write!(writer, "Invalid user address - terminating process\n").ok();
            });
            if let Some(pid) = crate::process::current_pid() {
                crate::process::terminate_process(pid, 1);
            }
        }
    }
}

/// Double fault exception handler
#[unsafe(no_mangle)]
pub extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("\nEXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}
