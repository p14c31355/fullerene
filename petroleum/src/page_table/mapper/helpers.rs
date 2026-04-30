use x86_64::{PhysAddr, VirtAddr, structures::paging::{Mapper, OffsetPageTable, PageTableFlags, Size4KiB}};
use crate::page_table::constants::BootInfoFrameAllocator;
use crate::page_table::pe::{PeSection, derive_pe_flags};

pub unsafe fn map_pe_section(
    mapper: &mut OffsetPageTable,
    section: PeSection,
    pe_base_phys: u64,
    phys_offset: VirtAddr,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    let flags = derive_pe_flags(section.characteristics);
    let section_start_phys = pe_base_phys + section.pointer_to_raw_data as u64;
    let section_start_virt = phys_offset.as_u64() + section.virtual_address as u64;
    let section_size = section.virtual_size as u64;
    let pages = section_size.div_ceil(4096);
    for p in 0..pages {
        let phys_addr = crate::calc_offset_addr!(section_start_phys, p);
        let virt_addr = crate::calc_offset_addr!(section_start_virt, p);
        crate::map_with_offset!(mapper, frame_allocator, phys_addr, virt_addr, flags, "panic");
    }
}

pub fn derive_memory_descriptor_flags<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(desc: &T) -> PageTableFlags {
    use x86_64::structures::paging::PageTableFlags as Flags;
    if desc.get_type() == crate::common::EfiMemoryType::EfiRuntimeServicesCode as u32 {
        Flags::PRESENT
    } else {
        Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE
    }
}

pub unsafe fn map_available_memory_to_higher_half<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[T],
) {
    memory_map.iter().for_each(|desc| {
        if desc.is_valid() {
            let phys_start = desc.get_physical_start();
            let pages = desc.get_page_count();
            let flags = derive_memory_descriptor_flags(desc);
            crate::safe_map_to_higher_half!(
                mapper,
                frame_allocator,
                phys_offset,
                phys_start,
                pages,
                flags
            );
        }
    });
}

pub fn map_stack_to_higher_half<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[T],
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let rsp = crate::get_current_stack_pointer!();
    for desc in memory_map.iter() {
        if desc.is_valid() {
            let start = desc.get_physical_start();
            let end = start + desc.get_page_count() * 4096;
            if rsp >= start && rsp < end {
                crate::safe_map_to_higher_half!(
                    mapper,
                    frame_allocator,
                    phys_offset,
                    desc.get_physical_start(),
                    desc.get_page_count(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
                )?;
                break;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub struct MappingConfig {
    pub phys_start: u64,
    pub virt_start: u64,
    pub num_pages: u64,
    pub flags: PageTableFlags,
}

pub unsafe fn map_memory_descriptors_with_config<T: crate::page_table::efi_memory::MemoryDescriptorValidator, F>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[T],
    config_fn: F,
) where
    F: Fn(&T) -> Option<MappingConfig>,
{
    for desc in memory_map.iter() {
        if let Some(config) = config_fn(desc) {
            unsafe {
                crate::map_range_with_log_macro!(
                    mapper,
                    frame_allocator,
                    config.phys_start,
                    config.virt_start,
                    config.num_pages,
                    config.flags
                );
            }
        }
    }
}

pub unsafe fn unmap_identity_range(
    mapper: &mut OffsetPageTable,
    start_addr: u64,
    num_pages: u64,
) {
    use x86_64::structures::paging::{Page, Size4KiB};
    for i in 0..num_pages {
        let addr = start_addr + (i * 4096);
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
        let _ = mapper.unmap(page);
    }
}

pub unsafe fn map_to_higher_half_with_log(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    phys_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let virt_start = phys_offset.as_u64() + phys_start;
    crate::map_range_with_log_macro!(
        mapper,
        frame_allocator,
        phys_start,
        virt_start,
        num_pages,
        flags
    );
    Ok(())
}
