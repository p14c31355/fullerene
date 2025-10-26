use core::ffi::c_void;
use log::info;

use petroleum::common::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus, EfiSystemTable};
// Inline heap.rs to reduce file count and lines
use petroleum::debug_log;
use petroleum::debug_log_no_alloc;
use petroleum::serial::debug_print_str_to_com1;

/// Size of the heap we will allocate for `alloc` usage (bytes).
const HEAP_SIZE: usize = 128 * 1024; // 128 KiB

/// Tries to allocate pages with multiple strategies and memory types.
fn try_allocate_pages(
    bs: &EfiBootServices,
    pages: usize,
    preferred_type: EfiMemoryType,
) -> Result<usize, BellowsError> {
    // Try LoaderData first, then Conventional (skip if invalid)
    let types_to_try = [preferred_type, EfiMemoryType::EfiConventionalMemory];

    for mem_type in types_to_try {
        let type_str = match mem_type {
            EfiMemoryType::EfiLoaderData => "LoaderData",
            EfiMemoryType::EfiConventionalMemory => "Conventional",
            _ => "Other",
        };
        debug_log_no_alloc!(
            "Heap: About to call allocate_pages mem_type=",
            mem_type as usize
        );

        let mut phys_addr_local: usize = 0;
        debug_log_no_alloc!("Heap: Calling allocate_pages pages=", pages);
        debug_log_no_alloc!("Heap: Calling allocate_pages mem_type=", mem_type as usize);
        debug_log_no_alloc!("Heap: Entering allocate_pages call...");
        // Use AllocateAnyPages (0) for any mem
        let alloc_type = 0usize; // AllocateAnyPages
        let status = (bs.allocate_pages)(
            alloc_type,
            mem_type,
            pages, // Start with 1 for testing
            &mut phys_addr_local,
        );
        debug_log_no_alloc!(
            "Heap: Exited allocate_pages call phys_addr_local=",
            phys_addr_local
        );
        debug_log_no_alloc!("Heap: Exited allocate_pages call raw_status=", status);

        // Immediate validation: check if phys_addr_local is page-aligned (avoid invalid reads)
        if phys_addr_local != 0 && !phys_addr_local.is_multiple_of(4096) {
            debug_log_no_alloc!("Heap: WARNING: phys_addr_local not page-aligned!");
            let _ = (bs.free_pages)(phys_addr_local, pages); // Ignore status on free
            continue;
        }

        let status_efi = EfiStatus::from(status);
        let status_str = petroleum::common::efi_status_to_str(status_efi);
        debug_log_no_alloc!("Heap: Status: ", status_str);

        if status_efi == EfiStatus::InvalidParameter {
            debug_log_no_alloc!("Heap: -> Skipping invalid type.");
            continue; // Ignore Conventional memory type
        }

        if status_efi == EfiStatus::Success && phys_addr_local != 0 {
            debug_log_no_alloc!("Heap: Allocated at address, aligned OK.");
            return Ok(phys_addr_local);
        }
    }

    Err(BellowsError::AllocationFailed(
        "All allocation attempts failed.",
    ))
}

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    debug_log_no_alloc!("Heap: Allocating pages for heap...");
    let heap_pages = HEAP_SIZE.div_ceil(4096);
    debug_log_no_alloc!("Heap: Requesting pages=", heap_pages);
    let heap_phys = try_allocate_pages(bs, heap_pages, EfiMemoryType::EfiLoaderData)?; // 固定

    if heap_phys == 0 {
        debug_log_no_alloc!("Heap: Allocated heap address is null!");
        return Err(BellowsError::AllocationFailed(
            "Allocated heap address is null.",
        ));
    }

    // Calculate actual allocated size (we may have gotten fewer pages than requested)
    // For now, assume we got the full amount since we don't track partial allocations
    // In a more robust implementation, we'd modify try_allocate_pages to return the actual size
    let actual_heap_size = heap_pages * 4096;

    debug_log_no_alloc!("Heap: Initializing global allocator using petroleum...");
    petroleum::init_global_heap(heap_phys as *mut u8, actual_heap_size);
    debug_log_no_alloc!("Heap: Petroleum global heap init done. Returning Ok(()).");
    Ok(())
}

/// Exits boot services and jumps to the kernel's entry point.
/// This function is the final step of the bootloader.
pub fn exit_boot_services_and_jump(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
    entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
) -> petroleum::common::Result<!> {
    // Immediate debug prints on entry to pinpoint exact hang location
    #[cfg(feature = "debug_loader")]
    {
        log::info!("ENTER");
        log::info!("system_table={:#x}", system_table as usize);
    }

    #[cfg(feature = "debug_loader")]
    log::info!("About to get boot_services ptr");
    let bs = unsafe { &*(*system_table).boot_services };
    #[cfg(feature = "debug_loader")]
    log::info!("Got boot_services ptr");

    #[cfg(feature = "debug_loader")]
    {
        log::info!("bs obtained.");
        log::info!("About to set up memory map vars.");
        log::info!("About to setup buffer vars");
    }
    // Pre-allocate buffer before loop to include it in map key
    let map_buffer_size: usize = 128 * 1024; // 128 KiB
    let alloc_pages = (map_buffer_size + core::mem::size_of::<usize>())
        .div_ceil(4096)
        .max(1);
    #[cfg(feature = "debug_loader")]
    log::info!("Buffer vars setup");

    #[cfg(feature = "debug_loader")]
    {
        log::info!("About to allocate fixed map buffer");
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

    let map_ptr: *mut c_void = (map_phys_addr + core::mem::size_of::<usize>()) as *mut c_void;

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
            log::info!("Combined loop, attempt {}", attempts);
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
                    log::info!(
                        "Memory map acquired successfully on attempt {}, size={:#x}, key={:#x}",
                        attempts,
                        map_size,
                        map_key
                    );
                    log::info!("About to call exit_boot_services...");
                }

                // Immediately call exit_boot_services with the freshly acquired map_key
                let exit_status = (bs.exit_boot_services)(image_handle, map_key);

                match EfiStatus::from(exit_status) {
                    EfiStatus::Success => {
                        #[cfg(feature = "debug_loader")]
                        {
                            log::info!("Exit boot services succeeded on attempt {}", attempts);
                            log::info!("About to jump to kernel.");
                        }
                        break; // Success, exit the loop and proceed to kernel jump
                    }
                    EfiStatus::Unsupported => {
                        #[cfg(feature = "debug_loader")]
                        {
                            log::info!(
                                "exit_boot_services returned Unsupported, proceeding anyway"
                            );
                        }
                        break; // Proceed to jump to kernel
                    }
                    EfiStatus::InvalidParameter => {
                        #[cfg(feature = "debug_loader")]
                        {
                            log::info!(
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
                            log::info!(
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
                    log::info!("Buffer too small, required size is now {} bytes", map_size);
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
                    log::info!("Error: Failed to get memory map: status={:#x}", status);
                }
                return Err(BellowsError::InvalidState("Failed to get memory map."));
            }
        }
    }

    // Write descriptor_size before the memory map
    unsafe {
        *(map_phys_addr as *mut usize) = descriptor_size;
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

        // Check if buffer has space
        let config_offset = map_size + core::mem::size_of::<usize>();
        if map_phys_addr + config_offset + config_size
            <= map_phys_addr + map_buffer_size + core::mem::size_of::<usize>()
        {
            unsafe {
                core::ptr::copy(
                    &config_with_metadata as *const _ as *const u8,
                    (map_phys_addr as *mut u8).add(config_offset),
                    config_size,
                );
            }
            final_map_size += config_size;
            #[cfg(feature = "debug_loader")]
            log::info!("Appended framebuffer config to memory map");
        }
    }

    // Note: The memory map buffer at `map_phys_addr` is intentionally not freed here
    // because after `exit_boot_services` is called, the boot services are no longer
    // available to the bootloader, making `bs.free_pages` an invalid call.

    // Jump to the kernel. This is the point of no return. We are calling the kernel entry point,
    // passing the memory map and other data. The validity of the `entry`
    // function pointer is assumed based on the successful PE file loading.
    #[cfg(feature = "debug_loader")]
    {
        log::info!(
            "Jumping to kernel at {:#x} with map at {:#x} size {:#x}",
            entry as usize,
            map_phys_addr,
            final_map_size
        );
        log::info!("About to call kernel entry.");
    }
    entry(image_handle, system_table, map_phys_addr as *mut c_void, final_map_size);
}

// Inline full PE structures and pe.rs to reduce file count and lines
use petroleum::read_unaligned;

#[repr(C, packed)]
pub struct ImageDosHeader {
    pub e_magic: u16,
    pub _pad: [u8; 58],
    pub e_lfanew: i32,
}

#[repr(C, packed)]
pub struct ImageFileHeader {
    pub _machine: u16,
    pub number_of_sections: u16,
    pub _time_date_stamp: u32,
    pub _pointer_to_symbol_table: u32,
    pub _number_of_symbols: u32,
    pub size_of_optional_header: u16,
    pub _characteristics: u16,
}

#[repr(C, packed)]
pub struct ImageDataDirectory {
    pub virtual_address: u32,
    pub size: u32,
}

#[repr(C, packed)]
pub struct ImageOptionalHeader64 {
    pub _magic: u16,
    pub _major_linker_version: u8,
    pub _minor_linker_version: u8,
    pub _size_of_code: u32,
    pub _size_of_initialized_data: u32,
    pub _size_of_uninitialized_data: u32,
    pub address_of_entry_point: u32,
    pub _base_of_code: u32,
    pub image_base: u64,
    pub _section_alignment: u32,
    pub _file_alignment: u32,
    pub _major_operating_system_version: u16,
    pub _minor_operating_system_version: u16,
    pub _major_image_version: u16,
    pub _minor_image_version: u16,
    pub _major_subsystem_version: u16,
    pub _minor_subsystem_version: u16,
    pub _win32_version_value: u32,
    pub size_of_image: u32,
    pub _size_of_headers: u32,
    pub _checksum: u32,
    pub _subsystem: u16,
    pub _dll_characteristics: u16,
    pub size_of_stack_reserve: u64,
    pub size_of_stack_commit: u64,
    pub size_of_heap_reserve: u64,
    pub size_of_heap_commit: u64,
    pub _loader_flags: u32,
    pub number_of_rva_and_sizes: u32,
    pub data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
pub struct ImageNtHeaders64 {
    pub _signature: u32,
    pub _file_header: ImageFileHeader,
    pub optional_header: ImageOptionalHeader64,
}

#[repr(C, packed)]
pub struct ImageSectionHeader {
    pub _name: [u8; 8],
    pub _virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub _pointer_to_relocations: u32,
    pub _pointer_to_linenumbers: u32,
    pub _number_of_relocations: u16,
    pub _number_of_linenumbers: u16,
    pub _characteristics: u32,
}

#[repr(C, packed)]
pub struct ImageBaseRelocation {
    pub virtual_address: u32,
    pub size_of_block: u32,
}

#[repr(u16)]
pub enum ImageRelBasedType {
    Absolute = 0,
    Dir64 = 10,
}

/// Exits boot services and jumps to the kernel's entry point.
/// This function is the final step of the bootloader.
pub fn load_efi_image(
    st: &EfiSystemTable,
    file: &[u8],
) -> petroleum::common::Result<
    extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
> {
    let bs = unsafe { &*st.boot_services };

    if file.len() < core::mem::size_of::<ImageDosHeader>() {
        return Err(BellowsError::PeParse("File too small for DOS header."));
    }
    let dos_header_ptr = file.as_ptr() as *const ImageDosHeader;
    let e_magic = unsafe { core::ptr::read_unaligned(dos_header_ptr as *const u16) };
    if e_magic != 0x5a4d {
        return Err(BellowsError::PeParse("Invalid DOS signature (MZ)."));
    }
    let e_lfanew = unsafe {
        core::ptr::read_unaligned(
            (dos_header_ptr as *const u8).add(core::mem::offset_of!(ImageDosHeader, e_lfanew)) as *const i32,
        )
    };
    petroleum::println!("DOS header parsed. e_lfanew: {:#x}", e_lfanew);

    // Inline PE parsing to reduce dependency and lines
    let nt_headers_offset = e_lfanew as usize;
    if nt_headers_offset + core::mem::size_of::<ImageNtHeaders64>() > file.len() {
        return Err(BellowsError::PeParse("Invalid NT headers offset."));
    }
    let nt_headers_ptr = unsafe { file.as_ptr().add(nt_headers_offset) as *const ImageNtHeaders64 };
    let optional_header_magic = unsafe {
        core::ptr::read_unaligned(
            (nt_headers_ptr as *const u8)
                .add(core::mem::offset_of!(ImageNtHeaders64, optional_header) + core::mem::offset_of!(ImageOptionalHeader64, _magic)) as *const u16,
        )
    };
    if optional_header_magic != 0x20b {
        return Err(BellowsError::PeParse("Invalid PE32+ magic number."));
    }
    let image_size_ptr = unsafe { (nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, optional_header) + core::mem::offset_of!(ImageOptionalHeader64, size_of_image)) as *const u32 };
    let address_of_entry_point_ptr = unsafe { (nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, optional_header) + core::mem::offset_of!(ImageOptionalHeader64, address_of_entry_point)) as *const u32 };
    let address_of_entry_point = unsafe { core::ptr::read_unaligned(address_of_entry_point_ptr) } as usize;
    let image_size_val = unsafe { core::ptr::read_unaligned(image_size_ptr) as u64 };
    let pages_needed = (image_size_val.max(address_of_entry_point as u64 + 4096)).div_ceil(4096) as usize;

    let preferred_base_ptr = unsafe { (nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, optional_header) + core::mem::offset_of!(ImageOptionalHeader64, image_base)) as *const u64 };
    let preferred_base = unsafe { core::ptr::read_unaligned(preferred_base_ptr) } as usize;
    let mut phys_addr: usize = 0;
    let mut status;

    if preferred_base >= 0x1000_0000 {
        phys_addr = 0x100000;
        status = (bs.allocate_pages)(2, EfiMemoryType::EfiLoaderCode, pages_needed, &mut phys_addr);
        if EfiStatus::from(status) != EfiStatus::Success {
            phys_addr = 0;
            status = (bs.allocate_pages)(0, EfiMemoryType::EfiLoaderCode, _pages_needed, &mut phys_addr);
        }
    } else {
        status = (bs.allocate_pages)(0, EfiMemoryType::EfiLoaderCode, _pages_needed, &mut phys_addr);
    }

    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed("Failed to allocate memory for PE image."));
    }

    let size_of_headers_ptr = unsafe { (nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, optional_header) + core::mem::offset_of!(ImageOptionalHeader64, _size_of_headers)) as *const u32 };
    let size_of_headers = unsafe { core::ptr::read_unaligned(size_of_headers_ptr) } as usize;
    unsafe {
        core::ptr::copy_nonoverlapping(file.as_ptr(), phys_addr as *mut u8, size_of_headers);
    }

    let number_of_sections_ptr = unsafe { (nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, _file_header) + core::mem::offset_of!(ImageFileHeader, number_of_sections)) as *const u16 };
    let number_of_sections = unsafe { core::ptr::read_unaligned(number_of_sections_ptr) } as usize;
    let size_of_optional_header_ptr = unsafe { (nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, _file_header) + core::mem::offset_of!(ImageFileHeader, size_of_optional_header)) as *const u16 };
    let size_of_optional_header = unsafe { core::ptr::read_unaligned(size_of_optional_header_ptr) } as usize;

    let section_headers_offset = e_lfanew as usize + core::mem::size_of::<u32>() + core::mem::size_of::<ImageFileHeader>() + size_of_optional_header;
    let section_headers_size = number_of_sections * core::mem::size_of::<ImageSectionHeader>();
    if section_headers_offset + section_headers_size > file.len() {
        unsafe { (bs.free_pages)(phys_addr, _pages_needed) };
        return Err(BellowsError::PeParse("Section headers out of bounds."));
    }

    for i in 0..number_of_sections {
        let section_header_base_ptr = unsafe { file.as_ptr().add(section_headers_offset + i * core::mem::size_of::<ImageSectionHeader>()) };
        let virtual_address = unsafe { core::ptr::read_unaligned(section_header_base_ptr.add(core::mem::offset_of!(ImageSectionHeader, virtual_address)) as *const u32) };
        let size_of_raw_data = unsafe { core::ptr::read_unaligned(section_header_base_ptr.add(core::mem::offset_of!(ImageSectionHeader, size_of_raw_data)) as *const u32) };
        let pointer_to_raw_data = unsafe { core::ptr::read_unaligned(section_header_base_ptr.add(core::mem::offset_of!(ImageSectionHeader, pointer_to_raw_data)) as *const u32) };

        let src_addr = unsafe { file.as_ptr().add(pointer_to_raw_data as usize) };
        let dst_addr = unsafe { (phys_addr as *mut u8).add(virtual_address as usize) };

        if (src_addr as usize).saturating_add(size_of_raw_data as usize) > (file.as_ptr() as usize).saturating_add(file.len()) ||
           (dst_addr as usize).saturating_add(size_of_raw_data as usize) > ((phys_addr as *mut u8) as usize).saturating_add(_pages_needed * 4096) {
            unsafe { (bs.free_pages)(phys_addr, _pages_needed) };
            return Err(BellowsError::PeParse("Section data out of bounds."));
        }

        unsafe {
            core::ptr::copy_nonoverlapping(src_addr, dst_addr, size_of_raw_data as usize);
        }
    }

    let image_base_ptr = unsafe { (nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, optional_header) + core::mem::offset_of!(ImageOptionalHeader64, image_base)) as *const u64 };
    let image_base = unsafe { core::ptr::read_unaligned(image_base_ptr) } as usize;
    let image_base_delta = (phys_addr as u64).wrapping_sub(image_base as u64);

    if image_base_delta != 0 {
        let phys_nt_headers_ptr = phys_addr as *const ImageNtHeaders64;
        let optional_header_ptr = unsafe { (phys_nt_headers_ptr as *const u8).add(core::mem::offset_of!(ImageNtHeaders64, optional_header)) as *const ImageOptionalHeader64 };
        let optional_header = unsafe { &*optional_header_ptr };
        let reloc_dir = &optional_header.data_directory[5]; // 5 is BASE_RELOCATION
        if reloc_dir.size > 0 {
            let mut reloc_offset = reloc_dir.virtual_address as usize;
            while reloc_offset < reloc_dir.virtual_address as usize + reloc_dir.size as usize {
                let block_ptr = (phys_addr as *const u8).add(reloc_offset);
                let block_virtual_address = read_unaligned!(block_ptr, 0, u32);
                let size_of_block = read_unaligned!(block_ptr, 4, u32);
                let num_entries = (size_of_block - 8) / 2;
                for i in 0..num_entries {
                    let entry_offset = 8 + i * 2;
                    let entry = read_unaligned!(block_ptr, entry_offset as usize, u16);
                    let rel_type = (entry >> 12) as u8;
                    let rel_offset = (entry & 0xFFF) as u16;
                    if rel_type == ImageRelBasedType::Dir64 as u8 {
                        let rva = block_virtual_address + rel_offset as u32;
                        let ptr = (phys_addr + rva as usize) as *mut u64;
                        let val = read_unaligned!(ptr as *const u8, 0, u64);
                        unsafe { *(ptr as *mut u64) = val.wrapping_add(image_base_delta); }
                    }
                }
                reloc_offset += size_of_block as usize;
            }
        }
    }

    let entry_point_addr = phys_addr.saturating_add(address_of_entry_point);
    if entry_point_addr >= phys_addr + _pages_needed * 4096 || entry_point_addr < phys_addr {
        unsafe { (bs.free_pages)(phys_addr, _pages_needed) };
        return Err(BellowsError::PeParse("Entry point address is outside allocated memory."));
    }

    log::info!("PE: EFI image loaded. Entry: 0x{:x}", entry_point_addr);
    let entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> ! = unsafe { core::mem::transmute(entry_point_addr) };
    log::info!("PE: load_efi_image completed successfully.");
    Ok(entry)
}
