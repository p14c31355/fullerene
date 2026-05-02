use core::ffi::c_void;
use petroleum::common::{EfiSystemTable};
use crate::boot::uefi_init::UefiInitContext;
use crate::boot::uefi_main::efi_main_stage2;
use x86_64::VirtAddr;
use petroleum::write_serial_bytes;

#[cfg(target_os = "uefi")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.efi_main")]
#[unsafe(naked)]
pub unsafe extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut EfiSystemTable,
    memory_map: *mut c_void,
    memory_map_size: usize,
) {
    core::arch::naked_asm!(
        "cli", // Ensure interrupts are disabled
        "mov dx, 0x3f8", "mov al, 0x21", "out dx, al", // Immediate signal of entry ('!')
        "jmp efi_main_logic",
    );
}

#[cfg(target_os = "uefi")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "efiapi" fn efi_main_logic(
    _image_handle: usize,
    system_table: *mut EfiSystemTable,
    memory_map: *mut c_void,
    memory_map_size: usize,
) {
    core::arch::naked_asm!("jmp efi_main_real_logic");
}

#[cfg(target_os = "uefi")]
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main_real_logic(
    _image_handle: usize,
    system_table: *mut EfiSystemTable,
    memory_map: *mut c_void,
    memory_map_size: usize,
) -> ! {
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: efi_main_real reached!\n");
    
    let mut buf = [0u8; 16];
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Args check:\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  handle: 0x");
    let len = petroleum::serial::format_hex_to_buffer(_image_handle as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  st: 0x");
    let len = petroleum::serial::format_hex_to_buffer(system_table as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  map: 0x");
    let len = petroleum::serial::format_hex_to_buffer(memory_map as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  size: 0x");
    let len = petroleum::serial::format_hex_to_buffer(memory_map_size as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    let mut buf = [0u8; 16];
    let len = petroleum::serial::format_hex_to_buffer(system_table as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: system_table ptr: 0x");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to dereference system_table\n");
    let system_table_ref = unsafe { &*system_table };
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: system_table dereferenced successfully\n");

    let mut ctx = UefiInitContext {
        system_table: system_table_ref,
        memory_map,
        memory_map_size,
        physical_memory_offset: VirtAddr::zero(),
        virtual_heap_start: VirtAddr::zero(),
        heap_start_after_gdt: VirtAddr::zero(),
        heap_start_after_stack: VirtAddr::zero(),
    };

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling early_initialization\n");
    let kernel_phys_start = ctx.early_initialization();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: early_initialization returned\n");

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling memory_management_initialization\n");
    let (physical_memory_offset, heap_start, virtual_heap_start) =
        ctx.memory_management_initialization(kernel_phys_start);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: memory_management_initialization returned\n");

    crate::gdt::load();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: GDT loaded after memory init\n");

    let kernel_stack_top = ctx.prepare_kernel_stack(virtual_heap_start, physical_memory_offset);
    write_serial_bytes!(0x3F8, 0x3FD, b"GDT and stack prepared\n");
    
    ctx.setup_allocator(virtual_heap_start);
    write_serial_bytes!(0x3F8, 0x3FD, b"Allocator setup completed\n");

    let ctx_ptr = &mut ctx as *mut _;
    write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Jumping to efi_main_stage2\n");
    unsafe {
        core::arch::asm!(
            "mov rdi, {ctx_ptr}",
            "mov rsi, {phys_offset}",
            "mov rsp, {stack_top}",
            "call {stage2}",
            ctx_ptr = in(reg) ctx_ptr,
            phys_offset = in(reg) physical_memory_offset.as_u64(),
            stack_top = in(reg) kernel_stack_top,
            stage2 = in(reg) efi_main_stage2 as usize,
            options(noreturn)
        );
    }
}