//! System call mechanism
//!
//! This module implements the Fast System Call mechanism using SYSCALL/SYSRET instructions.

use petroleum::mem_debug;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::Msr;
use x86_64::registers::rflags::RFlags;

/// Static kernel stack for syscall to prevent page fault vulnerabilities
const SYSCALL_STACK_SIZE: usize = 4096;

use core::sync::atomic::{AtomicPtr, Ordering};
static SYSCALL_KERNEL_STACK: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());

/// Kernel CR3 for syscall to access kernel heap
#[unsafe(no_mangle)]
pub static mut KERNEL_CR3_U64: u64 = 0;

/// Set kernel CR3 for syscall switching
pub fn set_kernel_cr3(cr3: u64) {
    unsafe {
        KERNEL_CR3_U64 = cr3;
    }
}

/// Initialize syscall kernel stack
pub fn init_syscall_stack() {
    mem_debug!("Syscall: init_syscall_stack start\n");
    use alloc::alloc::{Layout, alloc};
    let layout = Layout::from_size_align(SYSCALL_STACK_SIZE, 16).unwrap();
    mem_debug!("Syscall: allocating stack\n");
    let ptr = unsafe { alloc(layout) };
    mem_debug!("Syscall: stack allocated\n");
    let stack_top = unsafe { ptr.add(SYSCALL_STACK_SIZE) };
    SYSCALL_KERNEL_STACK.store(stack_top, Ordering::Relaxed);
    mem_debug!("Syscall: init_syscall_stack done\n");
}

/// System call entry point (naked function for manual assembly handling)
#[unsafe(naked)]
pub extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // Switch to kernel stack using swapgs
        "swapgs",
        "mov rsp, gs:0",
        // Save syscall number in RBX and switch CR3 to kernel page table
        "mov rbx, rax",
        "mov rax, cr3",
        "push rax",
        "lea rax, [rip + KERNEL_CR3_U64]",
        "mov rax, [rax]",
        "mov cr3, rax",
        // Entry: SYSCALL puts RIP in RCX, RFLAGS in R11
        "push rcx",
        "push r11",
        // Shuffle arguments: syscall ABI (rdi,rsi,rdx,r10,r8,r9)
        // to C ABI (rdi,rsi,rdx,rcx,r8,r9)
        "mov rcx, r10",
        "mov rdi, rbx",
        "push rsp",
        "call handle_syscall",
        "add rsp, 8",
        // Restore user CR3 and RFLAGS/RIP
        "pop r11",
        "pop rcx",
        "pop rax",
        "mov cr3, rax",
        "swapgs",
        "sysretq"
    );
}

/// Set up Fast System Call mechanism
pub fn setup_syscall() {
    mem_debug!("Syscall: setup_syscall start\n");

    // Enable SYSCALL/SYSRET with SCE bit in EFER
    mem_debug!("Syscall: writing EFER\n");
    unsafe {
        let current = Msr::new(0xC0000080).read();
        Msr::new(0xC0000080).write(current | (1 << 0));
    }
    mem_debug!("Syscall: EFER written\n");

    // Set LSTAR MSR to syscall entry point
    let entry_addr = syscall_entry as u64;
    mem_debug!("Syscall: writing LSTAR\n");
    unsafe {
        Msr::new(0xC0000082).write(entry_addr);
    }
    mem_debug!("Syscall: LSTAR written\n");

    // Set STAR MSR for CS/SS switching
    let user_cs = crate::gdt::user_code_selector().0 as u64;
    let kernel_cs = crate::gdt::kernel_code_selector().0 as u64;
    let star_value = (user_cs << 48) | (kernel_cs << 32);
    mem_debug!("Syscall: writing STAR\n");
    unsafe {
        Msr::new(0xC0000081).write(star_value);
    }
    mem_debug!("Syscall: STAR written\n");

    // Mask RFLAGS during syscall
    mem_debug!("Syscall: writing SFMASK\n");
    unsafe {
        Msr::new(0xC0000084).write(RFlags::INTERRUPT_FLAG.bits() | RFlags::TRAP_FLAG.bits());
    }
    mem_debug!("Syscall: SFMASK written\n");

    // Set KERNEL_GS_BASE to point to the static variable holding the syscall kernel stack top.
    use x86_64::registers::model_specific::KernelGsBase;
    mem_debug!("Syscall: writing KernelGsBase\n");
    unsafe {
        KernelGsBase::write(VirtAddr::new(&SYSCALL_KERNEL_STACK as *const _ as u64));
    }
    mem_debug!("Syscall: KernelGsBase written\n");

    let stack_top_addr = SYSCALL_KERNEL_STACK.load(Ordering::Relaxed) as u64;
    petroleum::debug_log_no_alloc!("Syscall: initialized. LSTAR: ", entry_addr);
    petroleum::debug_log_no_alloc!("Syscall: kernel stack: ", stack_top_addr);
    mem_debug!("Syscall: setup_syscall done\n");
}