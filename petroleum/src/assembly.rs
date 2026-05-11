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

#[repr(C, align(16))]
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

impl TransitionFrame {
    pub const LOGIC_FN_OFFSET: usize = core::mem::offset_of!(TransitionFrame, logic_fn);
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

#[unsafe(no_mangle)]
#[inline(never)]
pub unsafe extern "C" fn jump_to_kernel_with_stack(stack_top: u64, args_ptr: *const (), entry: usize, l4_phys: u64, phys_offset: u64) -> ! {
    // To completely avoid memory layout issues, critical values ​​are passed directly using registers.
    // RDI: args_ptr, RSI: stack_top, RDX: l4_phys, RCX: entry, R8: phys_offset
    core::arch::asm!(
        "mov rdi, {0}",
        "mov rsi, {1}",
        "mov rdx, {2}",
        "mov rcx, {3}",
        "mov r8, {4}",
        "jmp {3}",
        in(reg) args_ptr,
        in(reg) stack_top,
        in(reg) l4_phys,
        in(reg) entry,
        in(reg) phys_offset,
        options(noreturn)
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
pub unsafe extern "C" fn jump_with_new_stack(stack_ptr: u64, entry: usize) -> ! {
    core::arch::asm!(
        "mov rsp, {stack}",
        "jmp {entry}",
        stack = in(reg) stack_ptr,
        entry = in(reg) entry,
        options(noreturn)
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
        "mov r11, [rdi + {offset}]",
        "jmp r11",
        offset = const TransitionFrame::LOGIC_FN_OFFSET,
    );
}

/// Jumps to the kernel entry point with the provided arguments.
///
/// This function is the final step of the world switch.
///
/// Arguments:
/// - `entry`: The virtual address of the kernel entry point.
/// - `args`: A pointer to the `KernelArgs` structure.
/// - `phys_offset`: The physical memory offset.
#[unsafe(no_mangle)]
#[inline(never)]
pub unsafe extern "C" fn jump_to_kernel(
    entry: usize,
    args: *const KernelArgs,
    phys_offset: u64,
) -> ! {
    // Ensure stack is aligned and interrupts are disabled before jump
    prepare_for_kernel_jump();

    core::arch::asm!(
        "jmp {entry}",
        entry = in(reg) entry,
        in("rdi") args,
        in("rsi") phys_offset,
        options(noreturn)
    );
}
