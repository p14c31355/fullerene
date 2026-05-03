//! Low-level assembly helpers for the petroleum crate.
//! 
//! This module isolates raw assembly instructions to provide type-safe 
//! wrappers and reduce register clobbering in high-level logic.

/// Initializes all segment registers to the data segment (0x10).
/// 
/// This is typically called after a world switch or during kernel entry
/// to ensure a consistent execution environment.
#[inline(always)]
pub unsafe fn setup_segments() {
    core::arch::asm!(
        "mov ax, 0x10",
        "mov ds, ax",
        "mov es, ax",
        "mov fs, ax",
        "mov gs, ax",
        "mov ss, ax",
        options(preserves_flags)
    );
}

/// Prepares the CPU for a jump to the kernel by disabling interrupts,
/// setting up segment registers, and aligning the stack.
#[inline(always)]
pub unsafe fn prepare_for_kernel_jump() {
    core::arch::asm!(
        "cli",
        "mov ax, 0x10",
        "mov ds, ax",
        "mov es, ax",
        "mov fs, ax",
        "mov gs, ax",
        "mov ss, ax",
        "and rsp, -16",
        options(preserves_flags)
    );
}

/// Sets the stack pointer (RSP) to the specified address and jumps to the entry function.
/// 
/// # Safety
/// This function is unsafe because it directly manipulates the stack pointer
/// and jumps to an arbitrary address. The caller must ensure that `stack_ptr`
/// points to a valid, aligned stack and `entry` is a valid function pointer.
#[unsafe(naked)]
pub unsafe extern "C" fn jump_with_new_stack(stack_ptr: u64, entry: usize) -> ! {
    core::arch::naked_asm!(
        "mov rsp, rdi", // First argument (stack_ptr) becomes RSP
        "jmp rsi",      // Second argument (entry) is the jump target,
    )
}

/// The landing zone for the world switch transition.
/// 
/// This function is called via `retfq` from `perform_world_switch`.
/// It sets up the `TransitionArgs` structure on the stack and jumps to the 
/// `landing_zone_logic` function.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn landing_zone(
    _load_gdt: usize,
    _load_idt: usize,
    _phys_offset: u64,
    _level_4_table_frame: u64,
    _frame_allocator: usize,
    _logic_fn_high: usize,
    _kernel_entry: usize,
) {
    core::arch::naked_asm!(
        "mov rax, 0x4c4d4e58", // 'LMNX'
        "mov dx, 0x3f8",
        "out dx, al",

        // Ensure 16-byte alignment and space for TransitionArgs
        "sub rsp, 64",
        
        // Fill TransitionArgs from registers
        "mov [rsp + 0], rdi",  // load_gdt
        "mov [rsp + 8], rsi",  // load_idt
        "mov [rsp + 16], rdx", // phys_offset
        "mov [rsp + 24], rcx", // l4_frame
        "mov [rsp + 32], r8",  // allocator
        
        // Fill remaining TransitionArgs from the original stack
        // After retfq, original [rsp] = 0x08, [rsp+8] = kernel_entry, [rsp+16] = kernel_args
        // Now [rsp] is offset by -64
        "mov rax, [rsp + 72]", // original [rsp + 8] -> kernel_entry
        "mov [rsp + 40], rax",
        "mov rax, [rsp + 80]", // original [rsp + 16] -> kernel_args
        "mov [rsp + 48], rax",
        
        "mov rdi, rsp",        // TransitionArgs pointer as 1st arg
        "jmp r9",              // Jump to _logic_fn_high
    );
}
