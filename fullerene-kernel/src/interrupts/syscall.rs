//! System call mechanism
//!
//! This module implements the Fast System Call mechanism using SYSCALL/SYSRET instructions.

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
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] init_syscall_stack start\n");
    use alloc::alloc::{Layout, alloc};
    let layout = Layout::from_size_align(SYSCALL_STACK_SIZE, 16).unwrap();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] allocating stack\n");
    let ptr = unsafe { alloc(layout) };
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] stack allocated\n");
    let stack_top = unsafe { ptr.add(SYSCALL_STACK_SIZE) };
    SYSCALL_KERNEL_STACK.store(stack_top, Ordering::Relaxed);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] init_syscall_stack done\n");
}

/// System call entry point (naked function for manual assembly handling)
#[unsafe(naked)]
pub extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // Switch to kernel stack using swapgs
        "swapgs",        // Swap GS base to kernel GS base
        "mov rsp, gs:0", // Load kernel stack pointer from GS:0
        // Save syscall number in RBX and switch CR3 to kernel page table
        "mov rbx, rax", // Save syscall number from RAX
        "mov rax, cr3", // Get user CR3
        "push rax",     // Save user CR3 on stack
        "lea rax, [rip + KERNEL_CR3_U64]",
        "mov rax, [rax]", // Load kernel CR3
        "mov cr3, rax",   // Switch to kernel page table
        // Entry: SYSCALL puts RIP in RCX, RFLAGS in R11
        "push rcx", // Save return RIP
        "push r11", // Save return RFLAGS
        // Shuffle arguments: syscall ABI (rdi,rsi,rdx,r10,r8,r9)
        // to C ABI (rdi,rsi,rdx,rcx,r8,r9)
        "mov rcx, r10",
        "mov rdi, rbx", // Pass syscall number in rdi (first argument)
        "push rsp",     // Preserve stack pointer (for cleanup)
        "call handle_syscall",
        "add rsp, 8", // Clean up stack (instead of pop r10)
        // Restore user CR3 and RFLAGS/RIP
        "pop r11", // Restore RFLAGS
        "pop rcx", // Restore RIP
        "pop rax", // Restore user CR3
        "mov cr3, rax",
        "swapgs", // Restore user GS base
        "sysretq"
    );
}

/// Set up Fast System Call mechanism
pub fn setup_syscall() {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] setup_syscall start\n");
    // Enable SYSCALL/SYSRET with SCE bit in EFER
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] writing EFER\n");
    unsafe {
        let current = Msr::new(0xC0000080).read();
        Msr::new(0xC0000080).write(current | (1 << 0)); // Set SCE bit
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] EFER written\n");

    // Set LSTAR MSR to syscall entry point
    let entry_addr = syscall_entry as u64;
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] writing LSTAR\n");
    unsafe {
        Msr::new(0xC0000082).write(entry_addr);
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] LSTAR written\n");

    // Set STAR MSR for CS/SS switching
    let user_cs = crate::gdt::user_code_selector().0 as u64;
    let kernel_cs = crate::gdt::kernel_code_selector().0 as u64;
    let star_value = (user_cs << 48) | (kernel_cs << 32);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] writing STAR\n");
    unsafe {
        Msr::new(0xC0000081).write(star_value);
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] STAR written\n");

    // Mask RFLAGS during syscall
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] writing SFMASK\n");
    unsafe {
        Msr::new(0xC0000084).write(RFlags::INTERRUPT_FLAG.bits() | RFlags::TRAP_FLAG.bits());
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] SFMASK written\n");

    // Set KERNEL_GS_BASE to point to the static variable holding the syscall kernel stack top.
    use x86_64::registers::model_specific::KernelGsBase;
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] writing KernelGsBase\n");
    unsafe {
        KernelGsBase::write(VirtAddr::new(&SYSCALL_KERNEL_STACK as *const _ as u64));
    }
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] KernelGsBase written\n");

    let stack_top_addr = SYSCALL_KERNEL_STACK.load(Ordering::Relaxed) as u64;
    // Use non-locking debug print to avoid hang in spin::Mutex
    petroleum::debug_log_no_alloc!("Fast syscall mechanism initialized. LSTAR: ", entry_addr);
    petroleum::debug_log_no_alloc!("Kernel stack: ", stack_top_addr);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [syscall] setup_syscall done\n");
}
