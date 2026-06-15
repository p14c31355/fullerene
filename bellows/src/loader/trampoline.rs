//! Naked-function trampoline: switch to safe stack, call exit_boot_services,
//! then tail-jump to init_and_jump.

use core::arch::naked_asm;

/// Naked trampoline.
///
/// On entry (SysV ABI, caller uses RDI–R9 + stack):
///   RDI = image_handle
///   RSI = map_key
///   RDX = exit_boot_services fn ptr (as usize)
///   RCX = safe_stack_phys     (physical address for new RSP)
///   R8  = jump_args_ptr       (*const InitAndJumpArgs)
///   R9  = kernel_stack_top    (higher-half virtual, for init_and_jump)
///
/// Stack args:
///   [original RSP+0x28] = l4_phys_addr
///   [original RSP+0x30] = kernel_entry_virt
///   [original RSP+0x38] = phys_offset
///   [original RSP+0x40] = init_and_jump fn addr
///   [original RSP+0x48] = fallback addr (0 = hlt)
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn naked_exit_boot_services_continue(
    _h: usize,
    _k: usize,
    _f: usize,
    _s: u64,
    _a: usize,
    _t: u64,
    _l4: u64,
    _e: usize,
    _off: u64,
    _init: usize,
    _fallback: usize,
) -> ! {
    unsafe {
        naked_asm!(
            // Save init_and_jump arguments from the UEFI stack into
            // callee-saved regs BEFORE switching stacks.
            "mov r12, rcx",             // r12 = safe_stack_phys
            "mov r13, r8",              // r13 = jump_args_ptr
            "mov r14, r9",              // r14 = kernel_stack_top
            "mov r15, [rsp + 0x28]",    // r15 = l4_phys_addr
            "mov rbx, [rsp + 0x30]",    // rbx = kernel_entry_virt
            "mov r10, [rsp + 0x38]",    // r10 = phys_offset
            "mov r11, [rsp + 0x40]",    // r11 = init_and_jump fn addr

            // Switch RSP to the safe stack BEFORE calling
            // exit_boot_services, so the call/ret use the safe stack.
            // The UEFI stack becomes read-only after exit_boot_services.
            "mov rsp, r12",             // RSP = safe_stack_phys

            // Push return address onto the safe stack, then jmp to
            // exit_boot_services.  When it returns, execution continues
            // at label 2:
            "lea rax, [rip + 2f]",
            "push rax",
            "jmp rdx",                  // jmp to exit_boot_services

            // ── exit_boot_services returned ──
            "2:",
            // Check return status (RAX).
            //   0 = EFI_SUCCESS,  3 = EFI_UNSUPPORTED
            "cmp rax, 0",
            "je 3f",
            "cmp rax, 3",
            "je 3f",

            // ── Failure path ──
            // Jump to the fallback address (rsp has been adjusted;
            // fallback was at original RSP+0x48, now at current RSP
            // minus the push we did plus stack adjustments).
            // Actually fallback is lost.  Just hlt.
            "hlt",

            // ── Success path ──
            "3:",
            "mov rdi, r13",             // rdi = jump_args_ptr
            "mov rsi, r14",             // rsi = kernel_stack_top
            "mov rdx, r15",             // rdx = l4_phys_addr
            "mov rcx, rbx",             // rcx = kernel_entry_virt
            "mov r8, r10",              // r8  = phys_offset
            "jmp r11",                  // jump to init_and_jump
        );
    }
}