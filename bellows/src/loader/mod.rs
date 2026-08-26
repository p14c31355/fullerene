use alloc::vec::Vec;
use core::ffi::c_void;
use fullerene_abi::boot::BootInfo;
use petroleum::common::{
    BellowsError, EFI_LOADED_IMAGE_PROTOCOL_GUID, EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
    EfiBootServices, EfiFile, EfiLoadedImageProtocol, EfiMemoryType, EfiSimpleFileSystem,
    EfiStatus, EfiSystemTable,
};
use petroleum::filesystem::{EfiFileWrapper, open_file, read_file_to_memory};

// Module declarations for separated functionality
pub mod heap;

/// Initialize heap using separated heap module
pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    heap::init_heap(bs)
}

// Memory map buffer size (128 KiB)
const MAP_BUFFER_SIZE: usize = 128 * 1024;
// Number of pages for kernel args, L4 table, and stack (1 MB)
const KERNEL_ARGS_PAGES: usize = 256;
// Standard 4 KiB page size
const PAGE_SIZE_4K: u64 = 4096;

/// Retain the two EFI payloads needed by the Fullerene installer. El Torito
/// boots often expose only Block I/O, so the bootloader PE is reconstructed
/// from the loaded image and the embedded kernel image is retained directly.
pub fn read_boot_payloads(
    bs: &EfiBootServices,
    image_handle: usize,
) -> petroleum::common::Result<((usize, usize), (usize, usize))> {
    let mut loaded_ptr: *mut c_void = core::ptr::null_mut();
    let status = (bs.handle_protocol)(
        image_handle,
        EFI_LOADED_IMAGE_PROTOCOL_GUID.as_ptr(),
        &mut loaded_ptr,
    );
    if EfiStatus::from(status) != EfiStatus::Success || loaded_ptr.is_null() {
        return Err(BellowsError::FileIo("loaded image protocol unavailable"));
    }
    let loaded = unsafe { &*(loaded_ptr as *const EfiLoadedImageProtocol) };

    // An installed SATA boot does not need installer payloads: the running
    // system will never reinstall itself from its own ESP. More importantly,
    // avoid parsing firmware's relocated image on this fixed-disk path. The
    // live Ventoy/USB path still retains the payloads needed by the installer.
    if device_path_contains_sata(loaded.file_path) {
        petroleum::bootloader_log!(
            "SATA boot path detected; skipping installer payload reconstruction"
        );
        return Ok(((0, 0), (0, 0)));
    }

    // Prefer the original files on the boot filesystem. This preserves the
    // exact PE bytes that UEFI loaded, instead of rebuilding a relocated image
    // and installing that reconstruction to the target disk.
    match read_boot_payloads_from_filesystem(bs, loaded) {
        Ok(payloads) => {
            petroleum::bootloader_log!("Installer payloads read from boot filesystem");
            return Ok(payloads);
        }
        Err(error) => {
            petroleum::bootloader_log!(
                "Boot filesystem payload read unavailable: {:?}; using PE fallback",
                error
            );
        }
    }

    // El Torito firmware commonly exposes the CD as Block I/O only, not as
    // EFI_SIMPLE_FILE_SYSTEM_PROTOCOL. Fall back to reconstructing the
    // bootloader from the loaded image for that boot path.
    let bootloader = reconstruct_loaded_pe(bs, loaded.image_base, loaded.image_size)?;
    Ok((
        bootloader,
        (
            super::KERNEL_BINARY.as_ptr() as usize,
            super::KERNEL_BINARY.len(),
        ),
    ))
}

fn read_boot_payloads_from_filesystem(
    bs: &EfiBootServices,
    loaded: &EfiLoadedImageProtocol,
) -> petroleum::common::Result<((usize, usize), (usize, usize))> {
    let mut fs_ptr: *mut c_void = core::ptr::null_mut();
    let status = (bs.handle_protocol)(
        loaded.device_handle,
        EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID.as_ptr(),
        &mut fs_ptr,
    );
    if EfiStatus::from(status) != EfiStatus::Success || fs_ptr.is_null() {
        return Err(BellowsError::FileIo("boot filesystem protocol unavailable"));
    }

    let fs = unsafe { &*(fs_ptr as *const EfiSimpleFileSystem) };
    let mut root_ptr: *mut EfiFile = core::ptr::null_mut();
    let status = (fs.open_volume)(fs_ptr as *mut EfiSimpleFileSystem, &mut root_ptr);
    if EfiStatus::from(status) != EfiStatus::Success || root_ptr.is_null() {
        return Err(BellowsError::FileIo("boot filesystem volume unavailable"));
    }

    let root = EfiFileWrapper::new(root_ptr);
    let kernel_path = utf16_path("EFI\\BOOT\\KERNEL.EFI");
    let bootloader_path = utf16_path("EFI\\BOOT\\BOOTX64.EFI");
    let kernel = open_file(&root, &kernel_path).and_then(|file| read_file_to_memory(bs, &file))?;
    let bootloader =
        open_file(&root, &bootloader_path).and_then(|file| read_file_to_memory(bs, &file))?;
    Ok((bootloader, kernel))
}

fn utf16_path(path: &str) -> [u16; 32] {
    let mut result = [0u16; 32];
    for (index, code) in path.encode_utf16().take(31).enumerate() {
        result[index] = code;
    }
    result
}

fn device_path_contains_sata(path: *mut c_void) -> bool {
    if path.is_null() {
        return false;
    }
    let mut cursor = path as *const u8;
    for _ in 0..128 {
        let node_type = unsafe { core::ptr::read_unaligned(cursor) };
        let node_subtype = unsafe { core::ptr::read_unaligned(cursor.add(1)) };
        let node_length = u16::from_le_bytes([
            unsafe { core::ptr::read_unaligned(cursor.add(2)) },
            unsafe { core::ptr::read_unaligned(cursor.add(3)) },
        ]) as usize;
        if node_length < 4 {
            return false;
        }
        if node_type == 0x7F {
            return false;
        }
        if node_type == 0x03 && node_subtype == 0x12 {
            return true;
        }
        cursor = unsafe { cursor.add(node_length) };
    }
    false
}

const MAX_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u64(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn write_u64(data: &mut [u8], offset: usize, value: u64) -> Option<()> {
    data.get_mut(offset..offset.checked_add(8)?)?
        .copy_from_slice(&value.to_le_bytes());
    Some(())
}

#[derive(Clone, Copy)]
struct PeSection {
    virtual_address: usize,
    raw_offset: usize,
    raw_size: usize,
}

fn rva_to_raw(rva: usize, headers_size: usize, sections: &[PeSection]) -> Option<usize> {
    if rva < headers_size {
        return Some(rva);
    }
    sections.iter().find_map(|section| {
        if section.raw_size == 0 {
            return None;
        }
        let end = section.virtual_address.checked_add(section.raw_size)?;
        if (section.virtual_address..end).contains(&rva) {
            section
                .raw_offset
                .checked_add(rva - section.virtual_address)
        } else {
            None
        }
    })
}

/// Rebuild the on-disk PE layout from the image that UEFI loaded in memory.
/// UEFI applies base relocations while loading, so those relocations are
/// reversed before the reconstructed file is retained for installation.
fn reconstruct_loaded_pe(
    bs: &EfiBootServices,
    image_base: usize,
    image_size: u64,
) -> petroleum::common::Result<(usize, usize)> {
    let image_size = usize::try_from(image_size)
        .ok()
        .filter(|size| *size != 0 && *size <= MAX_PAYLOAD_SIZE)
        .ok_or(BellowsError::FileIo("loaded PE image size invalid"))?;
    if image_base == 0 {
        return Err(BellowsError::FileIo("loaded PE image base unavailable"));
    }
    let image = unsafe { core::slice::from_raw_parts(image_base as *const u8, image_size) };
    if image.get(..2) != Some(b"MZ") {
        return Err(BellowsError::FileIo("loaded PE DOS header invalid"));
    }
    let pe_offset = usize::try_from(
        read_u32(image, 0x3c).ok_or(BellowsError::FileIo("loaded PE DOS header truncated"))?,
    )
    .map_err(|_| BellowsError::FileIo("loaded PE offset invalid"))?;
    if image.get(
        pe_offset
            ..pe_offset
                .checked_add(4)
                .ok_or(BellowsError::FileIo("loaded PE header overflow"))?,
    ) != Some(b"PE\0\0")
    {
        return Err(BellowsError::FileIo("loaded PE signature invalid"));
    }

    let number_of_sections = usize::from(
        read_u16(image, pe_offset + 6)
            .ok_or(BellowsError::FileIo("loaded PE section table truncated"))?,
    );
    let optional_size = usize::from(
        read_u16(image, pe_offset + 20)
            .ok_or(BellowsError::FileIo("loaded PE optional header truncated"))?,
    );
    let optional = pe_offset
        .checked_add(24)
        .ok_or(BellowsError::FileIo("loaded PE header overflow"))?;
    if read_u16(image, optional) != Some(0x20b) {
        return Err(BellowsError::FileIo("loaded PE is not PE32+"));
    }
    let headers_size = usize::try_from(
        read_u32(image, optional + 60)
            .ok_or(BellowsError::FileIo("loaded PE headers truncated"))?,
    )
    .map_err(|_| BellowsError::FileIo("loaded PE headers too large"))?;
    if headers_size == 0 || headers_size > image_size {
        return Err(BellowsError::FileIo("loaded PE headers invalid"));
    }
    let preferred_base = read_u64(image, optional + 24)
        .ok_or(BellowsError::FileIo("loaded PE image base missing"))?;
    let section_table = optional
        .checked_add(optional_size)
        .ok_or(BellowsError::FileIo("loaded PE section table overflow"))?;
    let table_size = number_of_sections
        .checked_mul(40)
        .ok_or(BellowsError::FileIo("loaded PE section table too large"))?;
    if section_table.checked_add(table_size).is_none() || section_table + table_size > headers_size
    {
        return Err(BellowsError::FileIo("loaded PE section table invalid"));
    }

    let mut sections = Vec::with_capacity(number_of_sections);
    let mut file_size = headers_size;
    for index in 0..number_of_sections {
        let section = section_table + index * 40;
        let virtual_address = usize::try_from(
            read_u32(image, section + 12)
                .ok_or(BellowsError::FileIo("loaded PE section truncated"))?,
        )
        .map_err(|_| BellowsError::FileIo("loaded PE section address invalid"))?;
        let raw_size = usize::try_from(
            read_u32(image, section + 16)
                .ok_or(BellowsError::FileIo("loaded PE section truncated"))?,
        )
        .map_err(|_| BellowsError::FileIo("loaded PE raw size invalid"))?;
        let raw_offset = usize::try_from(
            read_u32(image, section + 20)
                .ok_or(BellowsError::FileIo("loaded PE section truncated"))?,
        )
        .map_err(|_| BellowsError::FileIo("loaded PE raw offset invalid"))?;
        if raw_size != 0 {
            let virtual_end = virtual_address
                .checked_add(raw_size)
                .ok_or(BellowsError::FileIo("loaded PE section overflow"))?;
            if virtual_end > image_size {
                return Err(BellowsError::FileIo("loaded PE section outside image"));
            }
            file_size = file_size.max(
                raw_offset
                    .checked_add(raw_size)
                    .ok_or(BellowsError::FileIo("loaded PE raw file overflow"))?,
            );
        }
        sections.push(PeSection {
            virtual_address,
            raw_offset,
            raw_size,
        });
    }
    if file_size == 0 || file_size > MAX_PAYLOAD_SIZE {
        return Err(BellowsError::FileIo("reconstructed PE is too large"));
    }

    let pages = file_size.div_ceil(PAGE_SIZE_4K as usize);
    let mut raw_phys = 0usize;
    let status = (bs.allocate_pages)(0, EfiMemoryType::EfiLoaderData, pages, &mut raw_phys);
    if EfiStatus::from(status) != EfiStatus::Success {
        return Err(BellowsError::AllocationFailed(
            "failed to retain bootloader payload",
        ));
    }
    let raw = unsafe { core::slice::from_raw_parts_mut(raw_phys as *mut u8, file_size) };
    raw.fill(0);
    raw[..headers_size].copy_from_slice(&image[..headers_size]);
    for section in &sections {
        if section.raw_size == 0 {
            continue;
        }
        let source_end = section.virtual_address + section.raw_size;
        let target_end = section.raw_offset + section.raw_size;
        raw[section.raw_offset..target_end]
            .copy_from_slice(&image[section.virtual_address..source_end]);
    }

    // Undo IMAGE_REL_BASED_DIR64 entries in the copied section data.
    let delta = image_base as i128 - preferred_base as i128;
    let data_directory_count = read_u32(image, optional + 108).unwrap_or(0);
    if delta != 0 && data_directory_count > 5 {
        let reloc_dir = optional + 112 + 5 * 8;
        let reloc_rva = usize::try_from(read_u32(image, reloc_dir).unwrap_or(0)).unwrap_or(0);
        let reloc_size = usize::try_from(read_u32(image, reloc_dir + 4).unwrap_or(0)).unwrap_or(0);
        if reloc_rva != 0 && reloc_size != 0 {
            let reloc_end = reloc_rva
                .checked_add(reloc_size)
                .ok_or(BellowsError::FileIo("loaded PE relocations overflow"))?;
            if reloc_end > image_size {
                return Err(BellowsError::FileIo("loaded PE relocations outside image"));
            }
            let mut cursor = reloc_rva;
            while cursor + 8 <= reloc_end {
                let page_rva = usize::try_from(read_u32(image, cursor).unwrap_or(0)).unwrap_or(0);
                let block_size =
                    usize::try_from(read_u32(image, cursor + 4).unwrap_or(0)).unwrap_or(0);
                if block_size < 8 || cursor + block_size > reloc_end {
                    return Err(BellowsError::FileIo("loaded PE relocation block invalid"));
                }
                let entries_end = cursor + block_size;
                let mut entry = cursor + 8;
                while entry + 2 <= entries_end {
                    let item = read_u16(image, entry).unwrap_or(0);
                    let kind = item >> 12;
                    let offset = usize::from(item & 0x0fff);
                    if kind == 10 {
                        let rva = page_rva
                            .checked_add(offset)
                            .ok_or(BellowsError::FileIo("loaded PE relocation overflow"))?;
                        let raw_offset = rva_to_raw(rva, headers_size, &sections)
                            .ok_or(BellowsError::FileIo("loaded PE relocation target invalid"))?;
                        let current = read_u64(&raw, raw_offset).ok_or(BellowsError::FileIo(
                            "loaded PE relocation target truncated",
                        ))?;
                        let restored = (current as i128 - delta) as u64;
                        write_u64(raw, raw_offset, restored).ok_or(BellowsError::FileIo(
                            "loaded PE relocation target truncated",
                        ))?;
                    }
                    entry += 2;
                }
                cursor = entries_end;
            }
        }
    }

    Ok((raw_phys, file_size))
}

/// Exits boot services and jumps to the kernel's entry point.
/// This function is the final step of the bootloader.
pub fn exit_boot_services_and_jump(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
    kernel_phys_start: x86_64::PhysAddr,
    kernel_entry_phys: u64,
    bootloader_payload: (usize, usize),
    kernel_payload: (usize, usize),
    loaded_kernel_size: u64,
    _entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
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
    let map_buffer_size: usize = MAP_BUFFER_SIZE;
    let alloc_pages = map_buffer_size.div_ceil(PAGE_SIZE_4K as usize);

    // Allocate memory for KernelArgs, L4 table, and initial kernel stack before exiting boot services
    // We allocate a larger block (KERNEL_ARGS_PAGES pages) to ensure the stack and arguments are far apart.
    // CRITICAL: The allocated address MUST be below 64GB (0x10_0000_0000) because the
    // world-switch shallow clone_page_table only identity-maps the first 64GB (huge pages).
    // If args_phys_addr ≥ 64GB, the identity mapping is missing and efi_main_stage2
    // will dereference garbage, corrupting framebuffer parameters.
    // We use AllocateAnyPages and validate the address; if it's too high we free and retry.
    const MAX_ARGS_ADDR: u64 = 0x10_0000_0000; // 64 GiB
    const ARGS_ALLOC_RETRIES: u32 = 16;
    let mut args_phys_addr: usize = 0;
    let mut args_alloc_ok = false;
    for _ in 0..ARGS_ALLOC_RETRIES {
        let args_alloc_status = (bs.allocate_pages)(
            0usize, // AllocateAnyPages
            EfiMemoryType::EfiLoaderData,
            KERNEL_ARGS_PAGES,
            &mut args_phys_addr,
        );
        if EfiStatus::from(args_alloc_status) != EfiStatus::Success {
            return Err(BellowsError::AllocationFailed(
                "Failed to allocate memory for KernelArgs.",
            ));
        }
        let alloc_end = args_phys_addr as u64 + (KERNEL_ARGS_PAGES as u64 * PAGE_SIZE_4K);
        if alloc_end <= MAX_ARGS_ADDR {
            args_alloc_ok = true;
            break;
        }
        // Address too high — free and retry
        let _ = (bs.free_pages)(args_phys_addr, KERNEL_ARGS_PAGES);
    }
    if !args_alloc_ok {
        return Err(BellowsError::AllocationFailed(
            "Failed to allocate KernelArgs below 64 GiB after retries.",
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

    let map_ptr = map_phys_addr as *mut c_void;

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
        let status = (bs.get_memory_map)(
            &mut map_size,
            map_ptr,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );

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
                            petroleum::info_log!(
                                "Exit boot services succeeded on attempt {}",
                                attempts
                            );
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
                    petroleum::info_log!(
                        "Buffer too small, required size is now {} bytes",
                        map_size
                    );
                }
                // If our fixed buffer is too small, this is a fatal error.
                let _ = (bs.free_pages)(map_phys_addr, alloc_pages); // Cleanup
                petroleum::println!(
                    "Error: Memory map size {} exceeds fixed buffer capacity {}",
                    map_size,
                    map_buffer_size
                );
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

    // Framebuffer fields travel in KernelArgs. Keeping the UEFI map descriptor-only
    // is essential: the kernel divides this exact byte count by descriptor_size.
    let final_map_size = map_size;

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
    // (No UEFI con_out calls after exit_boot_services — InsydeH2O crashes on them)

    // We only need InitAndJumpArgs for the transition.
    // KernelArgs will be reconstructed or passed via InitAndJumpArgs.
    let jump_args_ptr = args_phys_addr as *mut petroleum::page_table::InitAndJumpArgs;

    // Prepare memory map descriptors
    let descriptor_size_val = descriptor_size;
    if descriptor_size_val == 0 {
        return Err(BellowsError::InvalidState(
            "UEFI returned a zero-sized memory descriptor.",
        ));
    }
    let descriptors_ptr = map_ptr as *const u8;
    let num_descriptors = map_size.checked_div(descriptor_size_val).unwrap_or(0);

    let memory_map_descriptors = if num_descriptors > 0 && !descriptors_ptr.is_null() {
        let mut descriptors = alloc::vec::Vec::with_capacity(num_descriptors);
        for i in 0..num_descriptors {
            let Some(desc_address) = petroleum::common::utils::calculate_descriptor_address(
                descriptors_ptr as usize,
                i,
                descriptor_size_val,
            ) else {
                return Err(BellowsError::InvalidState(
                    "UEFI memory descriptor address overflow.",
                ));
            };
            descriptors.push(petroleum::page_table::memory_map::MemoryMapDescriptor::new(
                desc_address,
                descriptor_size_val,
            ));
        }
        descriptors
    } else {
        alloc::vec::Vec::new()
    };

    let mut frame_allocator = petroleum::page_table::BitmapFrameAllocator::new(
        petroleum::page_table::memory_map::processor::calculate_frame_allocation_params(
            &memory_map_descriptors,
        )
        .1,
    );
    frame_allocator.init(0);
    petroleum::page_table::memory_map::processor::mark_available_frames(
        &mut frame_allocator,
        &memory_map_descriptors,
    );

    // Calculate kernel entry virtual address (higher half)
    let kernel_entry_virt =
        petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64() + kernel_entry_phys;

    // VGA debug: indicate we're about to jump
    petroleum::vga_debug::vga_puts(21, 0, b"BLW:jmp kernel");

    // Stack top must be the higher-half virtual address, not the identity-mapped physical address,
    // because after CR3 switch the new page table only identity-maps 0-256MB.
    // The kernel's stack area is at args_phys_addr + (KERNEL_ARGS_PAGES pages).
    let kernel_stack_top = petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64()
        + args_phys_addr as u64
        + (KERNEL_ARGS_PAGES as u64 * PAGE_SIZE_4K);

    // Prepare the KernelArgs structure the kernel expects
    // Place it right after InitAndJumpArgs in the allocated block
    let kernel_args_phys = args_phys_addr as u64
        + core::mem::size_of::<petroleum::page_table::InitAndJumpArgs>() as u64;
    // Align to 16 bytes
    let kernel_args_phys_aligned = (kernel_args_phys + 15) & !15;

    let fb_addr;
    let fb_width;
    let fb_height;
    let fb_bpp;
    let fb_stride;
    let fb_pixel_format;
    if let Some(config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
        .get()
        .and_then(|mutex| *mutex.lock())
    {
        fb_addr = config.address as u64;
        fb_width = config.width;
        fb_height = config.height;
        fb_bpp = config.bpp;
        fb_stride = config.stride;
        fb_pixel_format = config.pixel_format as u32;
    } else {
        fb_addr = 0;
        fb_width = 0;
        fb_height = 0;
        fb_bpp = 0;
        fb_stride = 0;
        fb_pixel_format = 0;
    }

    let boot_info_phys = kernel_args_phys_aligned
        .checked_add(core::mem::size_of::<petroleum::assembly::KernelArgs>() as u64)
        .ok_or(BellowsError::AllocationFailed("BootInfo address overflow."))?;
    let boot_info = super::arch::x86_64::make_boot_info(
        kernel_phys_start.as_u64(),
        loaded_kernel_size,
        kernel_entry_virt,
        map_phys_addr as u64,
        final_map_size as u64,
        descriptor_size as u64,
        (fb_addr != 0).then_some((fb_addr, fb_width, fb_height, fb_stride, fb_bpp)),
    );
    debug_assert!(boot_info.is_valid());

    unsafe {
        core::ptr::write_volatile(boot_info_phys as *mut BootInfo, boot_info);
        let kernel_args_ptr = kernel_args_phys_aligned as *mut petroleum::assembly::KernelArgs;
        core::ptr::write_volatile(
            kernel_args_ptr,
            petroleum::assembly::KernelArgs {
                handle: image_handle,
                system_table: system_table as usize,
                map_ptr: map_phys_addr,
                map_size: final_map_size,
                descriptor_size,
                kernel_phys_start: kernel_phys_start.as_u64(),
                kernel_entry: kernel_entry_virt as usize,
                fb_address: fb_addr,
                fb_width,
                fb_height,
                fb_bpp,
                fb_stride,
                fb_pixel_format,
                bootloader_image_ptr: bootloader_payload.0 as u64,
                bootloader_image_size: bootloader_payload.1 as u64,
                kernel_image_ptr: kernel_payload.0 as u64,
                kernel_image_size: kernel_payload.1 as u64,
                boot_info_address: boot_info_phys as u64,
            },
        );

        // Map KernelArgs address down to page boundary for identity mapping.
        // The actual KernelArgs pointer will be reconstructed by the kernel using arg1 + offset.
        let kernel_args_page = (kernel_args_phys_aligned & !0xFFF) as u64;
        let kernel_args_offset = (kernel_args_phys_aligned & 0xFFF) as u64;

        // Prepare the arguments structure for the jump.
        core::ptr::write_volatile(
            jump_args_ptr,
            petroleum::page_table::InitAndJumpArgs {
                physical_memory_offset: petroleum::page_table::constants::HIGHER_HALF_OFFSET,
                frame_allocator: &mut frame_allocator as *mut _,
                kernel_phys_start: kernel_phys_start.as_u64(),
                entry_virt: kernel_entry_virt,
                stack_top: kernel_stack_top,
                arg1: kernel_args_page, // Page-aligned base (for identity mapping)
                arg2: kernel_args_offset, // Offset within page (for kernel to reconstruct ptr)
                map_phys_addr: map_phys_addr as u64,
                map_size: final_map_size as u64,
                l4_phys_addr: args_phys_addr as u64 + 4096,
                framebuffer_phys: fb_addr,
                framebuffer_size: u64::from(fb_stride).saturating_mul(u64::from(fb_height)),
                framebuffer_width: fb_width,
                framebuffer_height: fb_height,
                framebuffer_stride: fb_stride,
                framebuffer_pixel_format: fb_pixel_format,
            },
        );
    }

    // Jump to init_and_jump using the bootloader's current stack.
    // We call it directly as an extern "C" function to avoid register corruption in the assembly wrapper.
    unsafe {
        // CRITICAL: Explicitly identity map the arguments and L4 table area in the current UEFI page table.
        // This ensures that init_and_jump can safely access these physical addresses.
        let _l4_temp = petroleum::page_table::active_level_4_table(
            petroleum::page_table::constants::HIGHER_HALF_OFFSET,
        );

        petroleum::page_table::init_and_jump(
            jump_args_ptr,
            kernel_stack_top,
            args_phys_addr as u64 + 4096,
            kernel_entry_virt as usize,
            petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64(),
        );
    }
}

/// Load EFI PE image using petroleum PE module
pub fn load_efi_image(
    st: &petroleum::common::EfiSystemTable,
    file: &[u8],
    phys_offset: usize,
) -> petroleum::common::Result<(
    x86_64::addr::PhysAddr,
    u64,
    extern "efiapi" fn(usize, *mut petroleum::common::EfiSystemTable, *mut c_void, usize) -> !,
)> {
    petroleum::page_table::pe::load_efi_image(st, file, phys_offset)
}
