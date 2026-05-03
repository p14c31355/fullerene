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
        "jmp efi_main_real_logic",
    );
}

#[cfg(target_os = "uefi")]
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn efi_main_real_logic(
    args_ptr: *const petroleum::assembly::KernelArgs,
) -> ! {
    // Immediately capture args_ptr to avoid clobbering by subsequent function calls
    let captured_args_ptr = args_ptr;
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: efi_main_real reached!\n");
    
    let mut buf = [0u8; 16];
    
    // Print the raw pointer value to verify if it's correct
    let ptr_len = petroleum::serial::format_hex_to_buffer(captured_args_ptr as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: captured args_ptr: 0x");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..ptr_len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    let args = unsafe { &*captured_args_ptr };
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Args check:\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  handle: 0x");
    let len = petroleum::serial::format_hex_to_buffer(args.handle as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  st_phys: 0x");
    let len = petroleum::serial::format_hex_to_buffer(args.system_table as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  map_phys: 0x");
    let len = petroleum::serial::format_hex_to_buffer(args.map_ptr as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"  size: 0x");
    let len = petroleum::serial::format_hex_to_buffer(args.map_size as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    let system_table_phys = args.system_table;
    let system_table_virt = (system_table_phys as u64 + petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64()) as *mut EfiSystemTable;

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: About to dereference system_table (virt)\n");
    let system_table_ref = unsafe { &*system_table_virt };
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: system_table dereferenced successfully\n");

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Detecting descriptor_size from memory_map\n");
    let descriptor_size = unsafe {
        if args.map_ptr == 0 {
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: memory_map is null, using 0\n");
            0
        } else {
            let map_virt_ptr = (args.map_ptr as u64 + petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64()) as *const usize;
            let first_val = core::ptr::read_volatile(map_virt_ptr);
            if first_val >= 40 && first_val <= 64 {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Detected descriptor_size from map head\n");
                first_val
            } else {
                petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Using default descriptor_size 48\n");
                48
            }
        }
    };
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: descriptor_size obtained\n");

    let mut ctx = UefiInitContext {
        args_ptr: captured_args_ptr,
        system_table: system_table_ref,
        memory_map: args.map_ptr as *mut c_void,
        memory_map_size: args.map_size,
        descriptor_size,
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
    let kernel_stack_top_virt = VirtAddr::new(kernel_stack_top.as_u64());
    write_serial_bytes!(0x3F8, 0x3FD, b"GDT and stack prepared\n");
    
    ctx.setup_allocator(virtual_heap_start);
    write_serial_bytes!(0x3F8, 0x3FD, b"Allocator setup completed\n");

    let ctx_ptr = &mut ctx as *mut _;
    
    // Log addresses for debugging
    let mut buf = [0u8; 16];
    let len = petroleum::serial::format_hex_to_buffer(kernel_stack_top.as_u64(), &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: stack_top: 0x");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    
    let len = petroleum::serial::format_hex_to_buffer(efi_main_stage2 as u64, &mut buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: stage2_addr: 0x");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling perform_efi_stage2_switch\n");
    
    let transition_ctx = petroleum::page_table::mapper::transition::TransitionContext::prepare_for_efi_stage2(
        physical_memory_offset,
        VirtAddr::zero(),
        kernel_stack_top_virt,
        efi_main_stage2,
        ctx_ptr as *mut (),
    );

    petroleum::page_table::mapper::transition::perform_efi_stage2_switch(transition_ctx, kernel_stack_top_virt);
}