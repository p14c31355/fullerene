//! Low-level assembly helpers for the petroleum crate.
//! 
//! This module isolates raw assembly instructions to provide type-safe 
//! wrappers and reduce register clobbering in high-level logic.

use crate::page_table::constants::BootInfoFrameAllocator;

#[repr(C)]
pub struct TransitionArgs {
    pub load_gdt: *const (),
    pub load_idt: *const (),
    pub phys_offset: u64,
    pub l4_frame: u64,
    pub allocator: *mut BootInfoFrameAllocator,
    pub kernel_entry: usize,
    pub kernel_args: *const KernelArgs,
}

#[repr(C)]
pub struct KernelArgs {
    pub handle: usize,
    pub system_table: usize,
    pub map_ptr: usize,
    pub map_size: usize,
    pub descriptor_size: usize,
    pub kernel_phys_start: u64,
    pub kernel_entry: usize,
    pub fb_address: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_bpp: u32,
}

#[repr(C)]
pub struct TransitionFrame {
    pub args: TransitionArgs,
    pub logic_fn: usize,
}


/// Initializes all segment registers to the data segment (0x10).
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
#[unsafe(naked)]
pub unsafe extern "C" fn jump_with_new_stack(stack_ptr: u64, entry: usize) -> ! {
    core::arch::naked_asm!(
        "mov rsp, rdi",
        "jmp rsi",
    )
}

/// The landing zone for the world switch transition.
/// 
/// RDI contains a pointer to a `TransitionFrame` constructed on the stack
/// by `perform_world_switch`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn landing_zone(_frame: *const TransitionFrame) {
    core::arch::naked_asm!(
        "mov rax, 0x4c4d4e58", // 'LMNX'
        "mov dx, 0x3f8",
        "out dx, al",

        // RDI contains the TransitionFrame pointer.
        // 1. Load logic_fn from the frame (TransitionFrame is args[56 bytes] + logic_fn[8 bytes])
        "mov r11, [rdi + 56]",
        // 2. Jump to the logic function. 
        // RDI is preserved as the first argument to landing_zone_logic.
        "jmp r11",
    );
}

/// Jumps to the kernel entry point with the provided arguments.
/// 
/// This function is the final step of the world switch. It ensures segment
/// registers are set and the stack is aligned before performing a `retfq`
/// to the kernel entry.
/// 
/// Arguments:
/// - `entry`: The virtual address of the kernel entry point (passed in RDI).
/// - `args`: A pointer to the `KernelArgs` structure (passed in RSI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn jump_to_kernel(entry: usize, args: *const KernelArgs) -> ! {
    core::arch::asm!(
        "cli",
        "mov ax, 0x10",
        "mov ds, ax",
        "mov es, ax",
        "mov ss, ax",
        "and rsp, -16",
        // Use a temporary register to ensure no clobbering
        "mov r11, {entry}",
        "mov rdi, {args}",
        "jmp r11",
        entry = in(reg) entry,
        args = in(reg) args,
        options(noreturn)
    );
}
