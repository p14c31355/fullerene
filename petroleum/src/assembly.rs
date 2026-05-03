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

/// Internal structure to capture registers passed to `landing_zone`.
#[repr(C)]
struct SavedRegs {
    load_gdt: *const (),
    load_idt: *const (),
    phys_offset: u64,
    l4_frame: u64,
    allocator: *mut BootInfoFrameAllocator,
    logic_fn: usize,
}

/// Populates a TransitionFrame on the stack using saved registers and original stack arguments.
/// 
/// This function is called from `landing_zone`. It reads the registers saved by the assembly
/// and the original arguments from the stack to construct a type-safe `TransitionFrame`.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn build_args_on_stack(frame_ptr: *mut TransitionFrame) {
    // The frame_ptr points to the start of the allocated space.
    // The saved registers are stored immediately after the TransitionFrame (offset 64).
    let regs = &*(frame_ptr.add(64) as *const SavedRegs);
    
    // The original stack arguments were shifted by the 'sub rsp, 128' in landing_zone.
    // Original stack: [rsp_orig] = 0x08, [rsp_orig + 8] = kernel_entry, [rsp_orig + 16] = kernel_args
    // Current rsp = rsp_orig - 128.
    // So kernel_entry is at [frame_ptr + 128 + 8]
    let stack_base = (frame_ptr as usize).wrapping_add(128);
    let kernel_entry = *(stack_base.wrapping_add(8) as *const usize);
    let kernel_args = *(stack_base.wrapping_add(16) as *const *const KernelArgs);

    let frame = &mut *frame_ptr;
    frame.args = TransitionArgs {
        load_gdt: regs.load_gdt,
        load_idt: regs.load_idt,
        phys_offset: regs.phys_offset,
        l4_frame: regs.l4_frame,
        allocator: regs.allocator,
        kernel_entry,
        kernel_args,
    };
    frame.logic_fn = regs.logic_fn;
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
/// This function allocates a frame on the stack, saves the incoming registers,
/// and calls `build_args_on_stack` to populate the frame using Rust logic.
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

        // Allocate space for TransitionFrame (64 bytes) + SavedRegs (48 bytes) 
        // and align to 16 bytes.
        "sub rsp, 128",
        
        // Save registers into the SavedRegs area (offset 64)
        "mov [rsp + 64], rdi", // load_gdt
        "mov [rsp + 72], rsi", // load_idt
        "mov [rsp + 80], rdx", // phys_offset
        "mov [rsp + 88], rcx", // l4_frame
        "mov [rsp + 96], r8",  // allocator
        "mov [rsp + 104], r9", // logic_fn_high
        
        // Pass the start of the frame (RSP) as the first argument to build_args_on_stack
        "mov rdi, rsp",
        "call {build_fn}",
        
        // Jump to the logic function stored in the frame
        // TransitionFrame layout: TransitionArgs (56) then logic_fn (8)
        "mov r11, [rsp + 56]", // logic_fn
        "mov rdi, rsp",        // Pass TransitionArgs pointer as 1st arg
        "jmp r11",
        build_fn = sym build_args_on_stack,
    );
}