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

/// Populates a TransitionFrame on the stack using provided arguments.
/// 
/// This function is called from `landing_zone`. It takes the register values as
/// arguments and resolves the kernel entry/args from the original stack relative
/// to the provided `frame_ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn build_args_on_stack(
    load_gdt: *const (),
    load_idt: *const (),
    phys_offset: u64,
    l4_frame: u64,
    allocator: *mut BootInfoFrameAllocator,
    logic_fn: usize,
    frame_ptr: *mut TransitionFrame,
) {
    // The frame_ptr is the current RSP when landing_zone allocated the frame.
    // Original stack layout before 'sub rsp, 64' and 'push frame_ptr':
    // [rsp_orig] = 0x08
    // [rsp_orig + 8] = kernel_entry
    // [rsp_orig + 16] = kernel_args
    
    // Current stack is [frame_ptr] ... [frame_ptr + 64] = 0x08
    // So kernel_entry is at frame_ptr + 64 + 8
    let original_rsp = (frame_ptr as usize).wrapping_add(64);
    let kernel_entry = *(original_rsp.wrapping_add(8) as *const usize);
    let kernel_args = *(original_rsp.wrapping_add(16) as *const *const KernelArgs);

    let frame = &mut *frame_ptr;
    frame.args = TransitionArgs {
        load_gdt,
        load_idt,
        phys_offset,
        l4_frame,
        allocator,
        kernel_entry,
        kernel_args,
    };
    frame.logic_fn = logic_fn;
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
/// This function allocates a frame on the stack and calls `build_args_on_stack`
/// to populate it. It uses a fixed offset to ensure the stack pointer is
/// 16-byte aligned relative to the return address (RSP = 16n + 8) before the call.
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

        // Allocate space for TransitionFrame (64 bytes) + 1 slot for 7th arg (8 bytes).
        // Total = 72 bytes. 
        // If starting RSP is 16n, then 16n - 72 = 16k + 8.
        // This ensures that after 'call' pushes 8 bytes, the callee sees a 16-byte aligned stack.
        "sub rsp, 72",
        
        // Prepare the 7th argument (frame_ptr) for build_args_on_stack.
        // The frame pointer is the start of the allocated space (current RSP).
        "mov rax, rsp",
        "mov [rsp], rax",
        
        "call {build_fn}",
        
        // After call, RSP is back to 16k + 8.
        // The TransitionFrame starts at RSP.
        "mov rdi, rsp",        // Pass TransitionArgs pointer as 1st arg
        "mov r11, [rsp + 56]", // Load logic_fn from the end of the frame
        "jmp r11",
        build_fn = sym build_args_on_stack,
    );
}
