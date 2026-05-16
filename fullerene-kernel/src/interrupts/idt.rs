//! Interrupt Descriptor Table (IDT) management
//!
//! This module provides IDT initialization and handler setup.

use super::exceptions::*;
use super::input::{keyboard_handler, mouse_handler, timer_handler};
use crate::gdt::{
    DOUBLE_FAULT_IST_INDEX, GP_FAULT_IST_INDEX, MACHINE_CHECK_IST_INDEX, NMI_IST_INDEX,
    PAGE_FAULT_IST_INDEX, STACK_FAULT_IST_INDEX,
};
use crate::interrupts::{KEYBOARD_INTERRUPT_INDEX, MOUSE_INTERRUPT_INDEX, TIMER_INTERRUPT_INDEX};
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::mem_debug;
use x86_64::structures::idt::InterruptDescriptorTable;

// Global Interrupt Descriptor Table
pub static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

/// Guard flag to prevent double initialization of the IDT.
static IDT_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize IDT (load it into the CPU)
///
/// This function is idempotent: calling it more than once has no effect.
#[allow(static_mut_refs)]
pub fn init() {
    if IDT_INITIALIZED.swap(true, Ordering::SeqCst) {
        mem_debug!("IDT: Already initialized, skipping\n");
        return;
    }

    mem_debug!("IDT: Initializing\n");

    unsafe {
        let idt = &mut IDT;

        // ── Exception handlers (vectors 0-31) ──

        // Vectors 0-7: no error code
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.debug.set_handler_fn(debug_handler);
        idt.non_maskable_interrupt
            .set_handler_fn(nmi_handler)
            .set_stack_index(NMI_IST_INDEX);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded
            .set_handler_fn(bound_range_exceeded_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.device_not_available
            .set_handler_fn(device_not_available_handler);

        // Vector 8: Double fault (with error code, diverging)
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(DOUBLE_FAULT_IST_INDEX);

        // Vector 9: Coprocessor segment overrun (reserved)
        idt[9].set_handler_fn(coprocessor_segment_overrun_handler);

        // Vectors 10-14: with error code
        idt.invalid_tss
            .set_handler_fn(invalid_tss_handler)
            .set_stack_index(STACK_FAULT_IST_INDEX);
        idt.segment_not_present
            .set_handler_fn(segment_not_present_handler)
            .set_stack_index(STACK_FAULT_IST_INDEX);
        idt.stack_segment_fault
            .set_handler_fn(stack_segment_fault_handler)
            .set_stack_index(STACK_FAULT_IST_INDEX);
        idt.general_protection_fault
            .set_handler_fn(general_protection_fault_handler)
            .set_stack_index(GP_FAULT_IST_INDEX);
        idt.page_fault
            .set_handler_fn(page_fault_handler)
            .set_stack_index(PAGE_FAULT_IST_INDEX);

        // Vector 16: x87 FPU error
        idt.x87_floating_point.set_handler_fn(x87_fp_error_handler);

        // Vector 17: Alignment check
        idt.alignment_check.set_handler_fn(alignment_check_handler);

        // Vector 18: Machine check (diverging)
        idt.machine_check
            .set_handler_fn(machine_check_handler)
            .set_stack_index(MACHINE_CHECK_IST_INDEX);

        // Vector 19: SIMD FP exception
        idt.simd_floating_point
            .set_handler_fn(simd_fp_exception_handler);

        // Vector 20: Virtualization exception
        idt.virtualization.set_handler_fn(virtualization_handler);

        // Vector 21: Control protection exception
        idt.cp_protection_exception
            .set_handler_fn(cp_protection_exception_handler);

        // Vector 28: Hypervisor injection exception
        idt.hv_injection_exception
            .set_handler_fn(hv_injection_exception_handler);

        // Vector 29: VMM communication exception
        idt.vmm_communication_exception
            .set_handler_fn(vmm_communication_exception_handler);

        // Vector 30: Security exception
        idt.security_exception
            .set_handler_fn(security_exception_handler);

        // ── Hardware interrupt handlers ──
        idt[TIMER_INTERRUPT_INDEX as u8].set_handler_fn(timer_handler);
        idt[KEYBOARD_INTERRUPT_INDEX as u8].set_handler_fn(keyboard_handler);
        idt[MOUSE_INTERRUPT_INDEX as u8].set_handler_fn(mouse_handler);

        // Set up scheduler trampoline address for exception recovery
        let trampoline_addr = x86_64::VirtAddr::new(
            super::exceptions::exception_recovery_trampoline as *const () as u64,
        );
        super::exceptions::set_schedule_trampoline(trampoline_addr);

        mem_debug!("IDT: Loading IDT\n");
        idt.load();
    }

    mem_debug!("IDT: Initialized successfully\n");
}
