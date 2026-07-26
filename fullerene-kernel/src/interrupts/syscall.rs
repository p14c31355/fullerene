//! System call mechanism
//!
//! This module implements the Fast System Call mechanism using SYSCALL/SYSRET instructions.

use petroleum::mem_debug;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::Msr;
use x86_64::registers::rflags::RFlags;

/// Static kernel stack for syscalls.
const SYSCALL_STACK_SIZE: usize = 4096;

/// Per-CPU syscall entry state addressed through `KERNEL_GS_BASE`.
///
/// Fullerene is currently single-core, so one entry state is sufficient.
/// `user_rsp` is only scratch space until it has been copied to the kernel
/// stack; keeping it here lets the naked entry switch stacks without
/// clobbering a user register.
#[repr(C, align(16))]
struct SyscallEntryState {
    kernel_stack_top: u64,
    user_rsp: u64,
    syscall_number: u64,
}

static mut SYSCALL_ENTRY_STATE: SyscallEntryState = SyscallEntryState {
    kernel_stack_top: 0,
    user_rsp: 0,
    syscall_number: 0,
};

/// Initialize syscall kernel stack
pub fn init_syscall_stack() {
    mem_debug!("Syscall: init_syscall_stack start\n");
    use alloc::alloc::{Layout, alloc};
    let layout = Layout::from_size_align(SYSCALL_STACK_SIZE, 16).unwrap();
    mem_debug!("Syscall: allocating stack\n");
    let ptr = unsafe { alloc(layout) };
    mem_debug!("Syscall: stack allocated\n");
    let stack_top = unsafe { ptr.add(SYSCALL_STACK_SIZE) };
    unsafe {
        SYSCALL_ENTRY_STATE.kernel_stack_top = stack_top as u64;
    }
    crate::gdt::set_ring0_stack(VirtAddr::new(stack_top as u64));
    mem_debug!("Syscall: init_syscall_stack done\n");
}

/// System call entry point (naked function for manual assembly handling)
#[unsafe(naked)]
pub extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        // SYSCALL leaves the user stack in RSP.  Save it before switching to
        // the trusted kernel stack selected through KERNEL_GS_BASE.
        "swapgs",
        "mov gs:[16], rax",
        "mov gs:[8], rsp",
        "mov rsp, gs:[0]",
        // Keep the process CR3 active. Process page tables share the kernel
        // half, including this stack and the kernel heap, while retaining the
        // user half needed by copy_from_user/copy_to_user.
        // Save the SYSRET frame and all Linux syscall argument registers.
        // Linux only documents RAX, RCX, and R11 as clobbered by syscall.
        "push gs:[8]",
        "push rcx",
        "push r11",
        "push rdi",
        "push rsi",
        "push rdx",
        "push r10",
        "push r8",
        "push r9",
        // Shuffle Linux syscall ABI
        //   rax, rdi, rsi, rdx, r10, r8, r9
        // into the SysV C ABI used by
        //   handle_syscall(nr, a1, a2, a3, a4, a5, a6).
        // The seventh C argument is placed on the stack.  Ten pushes from an
        // aligned kernel-stack top leave RSP correctly aligned before CALL.
        "push r9",
        "mov r9, r8",
        "mov r8, r10",
        "mov rcx, rdx",
        "mov rdx, rsi",
        "mov rsi, rdi",
        "mov rdi, gs:[16]",
        "call handle_syscall",
        // Discard the seventh C argument, restore the user-visible argument
        // registers, then restore the SYSRET frame.
        "add rsp, 8",
        "pop r9",
        "pop r8",
        "pop r10",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop r11",
        "pop rcx",
        "pop qword ptr gs:[8]",
        // RAX remains the syscall result.
        "mov rsp, gs:[8]",
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
    let entry_addr = syscall_entry as *const () as u64;
    mem_debug!("Syscall: writing LSTAR\n");
    unsafe {
        Msr::new(0xC0000082).write(entry_addr);
    }
    mem_debug!("Syscall: LSTAR written\n");

    // Set STAR MSR for CS/SS switching
    // Use fallback selectors if GDT not yet fully initialized
    let user_cs = crate::gdt::user_code_selector_checked().0 as u64;
    let kernel_cs = crate::gdt::code_selector_checked().0 as u64;
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

    // Set KERNEL_GS_BASE to the entry state used by the naked assembly.
    use x86_64::registers::model_specific::KernelGsBase;
    mem_debug!("Syscall: writing KernelGsBase\n");
    KernelGsBase::write(VirtAddr::new(
        &raw const SYSCALL_ENTRY_STATE as *const _ as u64,
    ));
    mem_debug!("Syscall: KernelGsBase written\n");

    let stack_top_addr = unsafe { SYSCALL_ENTRY_STATE.kernel_stack_top };
    petroleum::debug_log_no_alloc!("Syscall: initialized. LSTAR: {}", entry_addr);
    petroleum::debug_log_no_alloc!("Syscall: kernel stack: {}", stack_top_addr);
    mem_debug!("Syscall: setup_syscall done\n");
}
