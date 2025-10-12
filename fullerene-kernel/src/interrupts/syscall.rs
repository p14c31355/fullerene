//! System call mechanism
//!
//! This module implements the Fast System Call mechanism using SYSCALL/SYSRET instructions.

use x86_64::registers::model_specific::{Efer, EferFlags, LStar, Msr, SFMask, Star};
use x86_64::registers::rflags::RFlags;

/// System call entry point (naked function for manual assembly handling)
#[unsafe(naked)]
pub extern "C" fn syscall_entry() {
    unsafe {
        core::arch::naked_asm!(
            // Entry: SYSCALL puts RIP in RCX, RFLAGS in R11
            "push rcx",        // Save return RIP
            "push r11",        // Save return RFLAGS
            // Shuffle arguments: syscall ABI (rdi,rsi,rdx,r10,r8,r9)
            // to C ABI (rdi,rsi,rdx,rcx,r8,r9)
            "mov rcx, r10",
            "push rax",        // Save syscall number
            "call handle_syscall",
            "pop r10",         // Restore syscall number
            "pop r11",         // Restore RFLAGS
            "pop rcx",         // Restore RIP
            "sysretq"
        );
    }
}

/// Set up Fast System Call mechanism
pub fn setup_syscall() {
    // Enable SYSCALL/SYSRET with SCE bit in EFER
    unsafe {
        let mut efer = Msr::new(0xC0000080);
        let current = efer.read();
        efer.write(current | (1 << 0)); // Set SCE bit
    }

    // Set LSTAR MSR to syscall entry point
    let entry_addr = syscall_entry as u64;
    unsafe {
        let mut lstar = Msr::new(0xC0000082);
        lstar.write(entry_addr);
    }

    // Set STAR MSR for CS/SS switching
    let user_cs = crate::gdt::user_code_selector().0 as u64;
    let kernel_cs = crate::gdt::kernel_code_selector().0 as u64;
    let star_value = (user_cs << 48) | (kernel_cs << 32);
    unsafe {
        let mut star = Msr::new(0xC0000081);
        star.write(star_value);
    }

    // Mask RFLAGS during syscall
    unsafe {
        let mut sfmask = Msr::new(0xC0000084);
        sfmask.write(RFlags::INTERRUPT_FLAG.bits() | RFlags::TRAP_FLAG.bits());
    }

    petroleum::serial::serial_log(format_args!(
        "Fast syscall mechanism initialized with LSTAR at {:#x}\n",
        entry_addr
    ));
}
