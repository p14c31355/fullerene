use core::ffi::c_void;

use x86_64::structures::paging::FrameAllocator;
use petroleum::common::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus, EfiSystemTable};

// Module declarations for separated functionality
pub mod heap;

/// Initialize heap using separated heap module
pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    heap::init_heap(bs)
}

/// Exits boot services and jumps to the kernel's entry point.
/// This function is the final step of the bootloader.
pub fn exit_boot_services_and_jump(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
    kernel_phys_start: x86_64::PhysAddr,
    kernel_entry_phys: u64,
    entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
) -> petroleum::common::Result<!> {
    // Immediate debug prints on entry to pinpoint exact hang location
    #[cfg(feature = "debug_loader")]
    {
        petroleum::info_log!("ENTER");
        petroleum::info_log!("system_table={:#x}", system_table as usize);
    }

    #[cfg(feature = "debug_loader")]
    petroleum::info_log!("About to get boot_services ptr");
    let bs = unsafe { &*(*system_table).boot_services };
    #[cfg(feature = "debug_loader")]
    petroleum::info_log!("Got boot_services ptr");

    #[cfg(feature = "debug_loader")]
    {
        petroleum::info_log!("bs obtained.");
        petroleum::info_log!("About to set up memory map vars.");
        petroleum::info_log!("About to setup buffer vars");
    }
    // Pre-allocate buffer before loop to include it in map key
    let map_buffer_size: usize = 128 * 1024; // 128 KiB
    let alloc_pages = petroleum::common::utils::calculate_pages_for_buffer(map_buffer_size);

    // Allocate memory for KernelArgs before exiting boot services to avoid memory corruption
    let mut args_phys_addr: usize = 0;
    let args_alloc_status = (bs.allocate_pages)(
        0usize,
        EfiMemoryType::EfiLoaderData,
        1,
        &mut args_phys_addr,
    );
    if EfiStatus::from(args_alloc_status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate memory for KernelArgs.",
        ));
    }

    #[cfg(feature = "debug_loader")]
    petroleum::info_log!("Buffer and KernelArgs vars setup");

    #[cfg(feature = "debug_loader")]
    {
        petroleum::info_log!("About to allocate fixed map buffer");
    }

    let mut map_phys_addr: usize = 0;
    let alloc_status = (bs.allocate_pages)(
        0usize, // AllocateAnyPages
        EfiMemoryType::EfiLoaderData,
        alloc_pages,
        &mut map_phys_addr,
    );

    if EfiStatus::from(alloc_status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate memory map buffer.",
        ));
    }

    let map_ptr: *mut c_void = petroleum::common::utils::calculate_map_data_ptr(map_phys_addr) as *mut c_void;

    // Setup variables for memory map
    let mut map_size: usize = map_buffer_size; // Start with full buffer size
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;

    // Loop to retry both get_memory_map and exit_boot_services until exit_boot_services succeeds
    // UEFI can make allocations between get_memory_map and exit_boot_services, causing map to become stale
    let mut attempts = 0;
    const MAX_ATTEMPTS: usize = 10; // Allow more attempts since both calls may need to be retried

    loop {
        if attempts >= MAX_ATTEMPTS {
            let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup before returning error
            return Err(BellowsError::InvalidState(
                "Too many attempts to exit boot services.",
            ));
        }
        attempts += 1;

        #[cfg(feature = "debug_loader")]
        {
            petroleum::info_log!("Combined loop, attempt {}", attempts);
        }

        // Call get_memory_map with pre-allocated buffer
        let status = unsafe {
            (bs.get_memory_map)(
                &mut map_size,
                map_ptr,
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )
        };

        match EfiStatus::from(status) {
            EfiStatus::Success => {
                #[cfg(feature = "debug_loader")]
                {
                    petroleum::info_log!(
                        "Memory map acquired successfully on attempt {}, size={:#x}, key={:#x}",
                        attempts,
                        map_size,
                        map_key
                    );
                    petroleum::info_log!("About to call exit_boot_services...");
                }

                // Immediately call exit_boot_services with the freshly acquired map_key
                let exit_status = (bs.exit_boot_services)(image_handle, map_key);

                match EfiStatus::from(exit_status) {
                    EfiStatus::Success => {
                        #[cfg(feature = "debug_loader")]
                        {
                            petroleum::info_log!("Exit boot services succeeded on attempt {}", attempts);
                            petroleum::info_log!("About to jump to kernel.");
                        }
                        break; // Success, exit the loop and proceed to kernel jump
                    }
                    EfiStatus::Unsupported => {
                        #[cfg(feature = "debug_loader")]
                        {
                            petroleum::info_log!(
                                "exit_boot_services returned Unsupported, proceeding anyway"
                            );
                        }
                        break; // Proceed to jump to kernel
                    }
                    EfiStatus::InvalidParameter => {
                        #[cfg(feature = "debug_loader")]
                        {
                            petroleum::info_log!(
                                "exit_boot_services returned InvalidParameter, retrying get_memory_map..."
                            );
                        }
                        // The map key is stale. Loop again to get a new memory map and key.
                        map_size = map_buffer_size;
                        continue;
                    }
                    _ => {
                        let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup
                        #[cfg(feature = "debug_loader")]
                        {
                            petroleum::error_log!(
                                "Error: Failed to exit boot services: status={:#x}",
                                exit_status as u32
                            );
                        }
                        return Err(BellowsError::InvalidState("Failed to exit boot services."));
                    }
                }
            }
            EfiStatus::BufferTooSmall => {
                #[cfg(feature = "debug_loader")]
                {
                    petroleum::info_log!("Buffer too small, required size is now {} bytes", map_size);
                }
                // If our fixed buffer is too small, this is a fatal error.
                let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup
                petroleum::serial::_print(format_args!(
                    "Error: Memory map size {} exceeds fixed buffer capacity {}\n",
                    map_size, map_buffer_size
                ));
                return Err(BellowsError::InvalidState(
                    "Memory map too large for buffer.",
                ));
            }
                _ => {
                    let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup
                    #[cfg(feature = "debug_loader")]
                    {
                        petroleum::error_log!("Error: Failed to get memory map: status={:#x}", status);
                    }
                    return Err(BellowsError::InvalidState("Failed to get memory map."));
                }
        }
    }

    // Check if framebuffer config is available and append it to memory map for kernel
    let mut final_map_size = map_size + core::mem::size_of::<usize>();
    if let Some(config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
        .get()
        .and_then(|mutex| *mutex.lock())
    {
        let config_with_metadata = petroleum::common::uefi::ConfigWithMetadata {
            descriptor_size,
            magic: petroleum::common::uefi::FRAMEBUFFER_CONFIG_MAGIC,
            config,
        };
        let config_size = core::mem::size_of::<petroleum::common::uefi::ConfigWithMetadata>();

        // The memory map data starts at map_ptr.
    // The total size of the map data is map_size.
    // We append the config immediately after the map data.
    let config_offset = petroleum::common::utils::calculate_config_offset(map_size);
    if petroleum::common::utils::check_buffer_overflow(map_phys_addr, config_offset, config_size, map_buffer_size)
    {
            unsafe {
                let dest_ptr = (map_phys_addr as *mut u8).add(config_offset);
                core::ptr::copy_nonoverlapping(
                    &config_with_metadata as *const _ as *const u8,
                    dest_ptr,
                    config_size,
                );
            }
            final_map_size += config_size;
            #[cfg(feature = "debug_loader")]
            petroleum::info_log!("Appended framebuffer config to memory map");
        }
    }

    // Note: The memory map buffer at `map_phys_addr` is intentionally not freed here
    // because after `exit_boot_services` is called, the boot services are no longer
    // available to the bootloader, making `bs.free_pages` an invalid call.

    // Jump to the kernel. This is the point of no return. We are calling the kernel entry point,
    // passing the memory map and other data. The validity of the `entry`
    // function pointer is assumed based on the successful PE file loading.
    //
    // Note: The `entry` function pointer is obtained via `load_efi_image`, which now 
    // handles high-half relocation and returns the virtual address.

    // Setup Page Tables before jumping to kernel
    petroleum::serial::_print(format_args!("Reinitializing page tables for kernel jump...\n"));

    let args_ptr = args_phys_addr as *mut petroleum::page_table::mapper::KernelArgs;
    unsafe {
        core::ptr::write_volatile(
            args_ptr,
            petroleum::page_table::mapper::KernelArgs {
                handle: image_handle,
                system_table: system_table as usize,
                map_ptr: map_phys_addr,
                map_size: final_map_size,
                descriptor_size,
                kernel_phys_start: kernel_phys_start.as_u64(),
                kernel_entry: kernel_entry_phys as usize,
            },
        );
    }

    let fb_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|m| *m.lock());
    let (fb_addr, fb_size) = match fb_config {
        Some(c) => (
            Some(x86_64::VirtAddr::new(c.address)),
            Some((c.width as u64 * c.height as u64 * c.bpp as u64) / 8),
        ),
        None => (None, None),
    };

    // Prepare memory map descriptors
    let descriptor_size_val = descriptor_size;
    // The actual descriptors start at map_ptr
    let descriptors_ptr = map_ptr as *const u8;
    
    // map_size is the size of the memory map returned by get_memory_map
    let num_descriptors = if descriptor_size_val > 0 {
        map_size / descriptor_size_val
    } else {
        0
    };

    let memory_map_descriptors = if num_descriptors > 0 && !descriptors_ptr.is_null() {
        let mut descriptors = alloc::vec::Vec::with_capacity(num_descriptors);
        for i in 0..num_descriptors {
            unsafe {
                let desc_ptr = petroleum::common::utils::calculate_descriptor_ptr(descriptors_ptr, i, descriptor_size_val);
                descriptors.push(petroleum::page_table::efi_memory::MemoryMapDescriptor::new(
                    desc_ptr,
                    descriptor_size_val,
                ));
            }
        }
        descriptors
    } else {
        alloc::vec::Vec::new()
    };

    let mut frame_allocator = unsafe {
        petroleum::page_table::BitmapFrameAllocator::init(&memory_map_descriptors)
    };

        let new_phys_offset = petroleum::page_table::reinit_page_table_with_allocator(
            kernel_phys_start,
            fb_addr,
            fb_size,
            &mut frame_allocator,
            &memory_map_descriptors,
            map_phys_addr as u64,
            final_map_size as u64,
            x86_64::VirtAddr::zero(),
            None::<fn()>,
            None::<fn()>,
            None::<fn(&mut x86_64::structures::paging::OffsetPageTable, &mut petroleum::page_table::BootInfoFrameAllocator, x86_64::VirtAddr)>,
            None,
            Some(kernel_entry_phys as usize),
            Some(args_phys_addr as u64),
        );
    
    petroleum::serial::_print(format_args!("New physical memory offset: {:#x}\n", new_phys_offset.as_u64()));
    petroleum::serial::_print(format_args!("Jumping to kernel entry point: {:#p}\n", entry));

    // Now jump to the kernel.
    unsafe {
        core::arch::asm!(
            "xor ax, ax",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            
            "mov rsp, {stack_top}",
            "mov rcx, {handle}",
            "mov rdx, {st}",
            "mov r8, {map}",
            "mov r9, {size}",
            
            "jmp {entry_addr}",
            stack_top = in(reg) (args_phys_addr + 4096),
            handle = in(reg) image_handle,
            st = in(reg) system_table,
            map = in(reg) map_phys_addr,
            size = in(reg) final_map_size,
            entry_addr = in(reg) entry as usize,
            options(noreturn),
        );
    }
}

/// Load EFI PE image using petroleum PE module
pub fn load_efi_image(
    st: &petroleum::common::EfiSystemTable,
    file: &[u8],
    phys_offset: usize,
) -> petroleum::common::Result<
    (x86_64::addr::PhysAddr, u64, extern "efiapi" fn(usize, *mut petroleum::common::EfiSystemTable, *mut c_void, usize) -> !),
> {
    petroleum::page_table::pe::load_efi_image(st, file, phys_offset)
}
