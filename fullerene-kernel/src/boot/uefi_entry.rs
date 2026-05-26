//! UEFI entry point (only compiled for uefi target)
#![cfg(target_os = "uefi")]

use crate::boot::uefi_init::UefiInitContext;
use crate::boot::uefi_main::efi_main_stage2;
use core::ffi::c_void;
use petroleum::common::EfiSystemTable;
use petroleum::early::transition::KernelTransition;
use x86_64::VirtAddr;

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.efi_main")]
#[unsafe(naked)]
pub unsafe extern "efiapi" fn efi_main(
    _image_handle: usize,
    system_table: *mut EfiSystemTable,
    memory_map: *mut core::ffi::c_void,
    memory_map_size: usize,
) {
    core::arch::naked_asm!(
        "cli", // Ensure interrupts are disabled
        "mov dx, 0x3f8",
        "mov al, 0x21",
        "out dx, al", // Immediate signal of entry ('!')
        "jmp efi_main_real_logic",
    );
}

#[cfg(target_os = "uefi")]
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn efi_main_real_logic(
    args_ptr: *const petroleum::assembly::KernelArgs,
) -> ! {
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] Entering efi_main_real_logic\n"
    );

    let captured_args_ptr = args_ptr;
    let args = unsafe { &*captured_args_ptr };

    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_entry] Args dereferenced\n");
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_entry] FB Address: 0x");
    let mut fb_addr_buf = [0u8; 16];
    let fb_addr_len =
        petroleum::serial::format_hex_to_buffer(args.fb_address as u64, &mut fb_addr_buf, 16);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, &fb_addr_buf[..fb_addr_len]);
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

    let system_table_virt = (args.system_table as u64
        + petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64())
        as *mut EfiSystemTable;

    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] About to dereference system_table (virt)\n"
    );
    let system_table_ref = unsafe { &*system_table_virt };
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] system_table dereferenced successfully\n"
    );

    let mut ctx = UefiInitContext {
        args_ptr: captured_args_ptr,
        system_table: system_table_ref,
        memory_map: args.map_ptr as *mut c_void,
        memory_map_size: args.map_size,
        descriptor_size: args.descriptor_size,
        physical_memory_offset: VirtAddr::zero(),
        virtual_heap_start: VirtAddr::zero(),
        heap_start_after_gdt: VirtAddr::zero(),
        heap_start_after_stack: VirtAddr::zero(),
    };

    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] Calling early_initialization\n"
    );
    let kernel_phys_start = ctx.early_initialization();
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] early_initialization returned\n"
    );

    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] Calling memory_management_initialization\n"
    );
    let (physical_memory_offset, heap_start, virtual_heap_start) =
        ctx.memory_management_initialization(kernel_phys_start);
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] memory_management_initialization returned\n"
    );

    crate::gdt::load();
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_entry] GDT loaded\n");

    let kernel_stack_top = ctx.prepare_kernel_stack(virtual_heap_start, physical_memory_offset);
    let kernel_stack_top_virt = VirtAddr::new(kernel_stack_top.as_u64());
    petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [uefi_entry] Stack prepared\n");

    ctx.setup_allocator(virtual_heap_start);
    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] Allocator setup completed\n"
    );

    petroleum::write_serial_bytes!(
        0x3F8,
        0x3FD,
        b"DEBUG: [uefi_entry] Performing world switch to kernel\n"
    );

    let cr3 = x86_64::registers::control::Cr3::read();
    let l4_frame = cr3.0;

    let allocator_ptr = {
        let mut lock = crate::heap::FRAME_ALLOCATOR.lock();
        lock.as_mut()
            .expect("Frame allocator should be initialized") as *mut _
    };

    let world = petroleum::early::transition::WorldSwitchBuilder::default()
        .with_phys_offset(physical_memory_offset)
        .with_stack(kernel_stack_top_virt)
        .with_entry(VirtAddr::new(efi_main_stage2 as u64))
        .with_args(captured_args_ptr)
        .with_gdt(core::ptr::null()) // GDT already loaded by gdt::load(), no need to reload during transition
        .with_idt(core::ptr::null()) // IDT is not yet available during transition
        .with_page_table(l4_frame)
        .with_allocator(allocator_ptr)
        .build()
        .expect("Failed to build WorldSwitch");

    let transition = petroleum::early::transition::UefiToHigherHalf {
        world,
        landing_zone: VirtAddr::new(petroleum::assembly::landing_zone as u64),
    };

    unsafe {
        transition.perform();
    }
}
