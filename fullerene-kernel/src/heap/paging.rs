//! Page table management and virtual memory operations
//!
//! This module handles page table initialization, mapping, and higher-half addressing.

use spin::{Mutex, Once};
use x86_64::registers::control::Cr3Flags;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags as Flags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

use petroleum::page_table::{BootInfoFrameAllocator, EfiMemoryDescriptor};
use petroleum::common::EfiMemoryType;

/// Physical memory offset (higher half)
pub static PHYSICAL_MEMORY_OFFSET: Once<VirtAddr> = Once::new();
pub static HIGHER_HALF_OFFSET: Once<VirtAddr> = Once::new();

/// Global mapper and frame allocator
pub(crate) static MAPPER: Once<Mutex<OffsetPageTable<'static>>> = Once::new();
pub(crate) static FRAME_ALLOCATOR: Once<Mutex<BootInfoFrameAllocator<'static>>> = Once::new();

/// Initialize page table with offset
pub fn init_page_table(physical_memory_offset: VirtAddr) {
    PHYSICAL_MEMORY_OFFSET.call_once(|| physical_memory_offset);
    let mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };
    MAPPER.call_once(|| Mutex::new(mapper));
}

/// Initialize heap in virtual memory
pub fn init(heap_start: VirtAddr, heap_size: usize) {
    let mut mapper = MAPPER.get().unwrap().lock();
    let mut frame_allocator = FRAME_ALLOCATOR.get().unwrap().lock();
    let physical_memory_offset = *PHYSICAL_MEMORY_OFFSET.get().unwrap();

    let start_page = Page::<Size4KiB>::containing_address(heap_start);
    let end_address = heap_start + heap_size as u64;
    let end_page = Page::<Size4KiB>::containing_address(end_address - 1);

    let mut current_page = start_page;
    while current_page <= end_page {
        let page_start_virt = current_page.start_address();
        let page_start_phys =
            PhysAddr::new(page_start_virt.as_u64() - physical_memory_offset.as_u64());
        let frame = PhysFrame::<Size4KiB>::containing_address(page_start_phys);

        unsafe {
            mapper
                .map_to(
                    current_page,
                    frame,
                    Flags::PRESENT | Flags::WRITABLE,
                    &mut *frame_allocator,
                )
                .unwrap()
                .flush();
        }

        current_page = current_page + 1;
    }

    drop(frame_allocator);
    drop(mapper);

    use super::allocator::{ALLOCATOR, HEAP_SIZE};
    unsafe {
        let heap_start_ptr = heap_start.as_mut_ptr::<u8>();
        ALLOCATOR.lock().init(heap_start_ptr, heap_size);
    }
}

/// Helper function to map a contiguous physical memory range to virtual memory
pub(crate) unsafe fn map_physical_range(
    mapper: &mut OffsetPageTable,
    start_phys: PhysAddr,
    end_phys: PhysAddr,
    start_virt: VirtAddr,
    flags: Flags,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    let mut current_phys = start_phys;
    while current_phys < end_phys {
        let virt_addr = start_virt + (current_phys - start_phys);
        let page = Page::<Size4KiB>::containing_address(virt_addr);
        let frame = PhysFrame::<Size4KiB>::containing_address(current_phys);

        unsafe {
            mapper
                .map_to(page, frame, flags, frame_allocator)
                .expect("Failed to map page")
                .flush();
        }

        current_phys += 4096u64;
    }
}

/// Allocate heap from memory map
pub fn allocate_heap_from_map(phys_start: PhysAddr, _size: usize) -> VirtAddr {
    if let Some(offset) = HIGHER_HALF_OFFSET.get() {
        VirtAddr::new(offset.as_u64() + phys_start.as_u64())
    } else {
        panic!("Higher half offset not initialized");
    }
}

/// Reinitialize page table for kernel mode
pub fn reinit_page_table(
    physical_memory_offset: VirtAddr,
    kernel_phys_start: PhysAddr,
    framebuffer_addr: Option<u64>,
    framebuffer_size: Option<u64>,
) {
    use x86_64::registers::control::Cr3;
    use x86_64::structures::paging::PageTable;

    petroleum::serial::serial_log(format_args!(
        "reinit_page_table: Starting with offset 0x{:x}, kernel_start 0x{:x}\n",
        physical_memory_offset.as_u64(),
        kernel_phys_start.as_u64()
    ));

    let mut frame_allocator = FRAME_ALLOCATOR.get().unwrap().lock();
    let memory_map = *super::memory_map::MEMORY_MAP.get().unwrap();

    let level_4_frame = frame_allocator
        .allocate_frame()
        .expect("Failed to allocate level 4 frame");
    petroleum::serial::serial_log(format_args!(
        "reinit_page_table: Allocated L4 frame at 0x{:x}\n",
        level_4_frame.start_address().as_u64()
    ));

    let temp_virt_page = Page::<Size4KiB>::containing_address(VirtAddr::new(0xFFFF_FF00_0000_F000));
    {
        let mut current_mapper = MAPPER.get().unwrap().lock();
        unsafe {
            current_mapper
                .map_to(
                    temp_virt_page,
                    level_4_frame,
                    Flags::PRESENT | Flags::WRITABLE,
                    &mut *frame_allocator,
                )
                .expect("Failed to map temp")
                .flush();
        }
    }

    let level_4_table: &mut PageTable =
        unsafe { &mut *temp_virt_page.start_address().as_mut_ptr() };
    level_4_table.zero();

    let mut new_mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };

    let higher_half_types = [
        EfiMemoryType::EfiConventionalMemory,
        EfiMemoryType::EfiLoaderCode,
        EfiMemoryType::EfiLoaderData,
        EfiMemoryType::EfiBootServicesCode,
        EfiMemoryType::EfiBootServicesData,
    ];
    for desc in memory_map {
        if desc.number_of_pages == 0 || !higher_half_types.contains(&desc.type_) {
            continue;
        }
        let start_phys = PhysAddr::new(desc.physical_start);
        let end_phys = start_phys + (desc.number_of_pages * 4096);
        let start_virt = physical_memory_offset + desc.physical_start;
        let flags = Flags::PRESENT | Flags::WRITABLE;
        unsafe {
            map_physical_range(
                &mut new_mapper,
                start_phys,
                end_phys,
                start_virt,
                flags,
                &mut frame_allocator,
            );
        }
    }

    let runtime_types = [
        EfiMemoryType::EfiRuntimeServicesCode,
        EfiMemoryType::EfiRuntimeServicesData,
    ];
    for desc in memory_map {
        if desc.number_of_pages == 0 || !runtime_types.contains(&desc.type_) {
            continue;
        }
        let start_phys = PhysAddr::new(desc.physical_start);
        let end_phys = start_phys + (desc.number_of_pages * 4096);
        let start_virt = VirtAddr::new(desc.physical_start);
        let flags = if desc.type_ == EfiMemoryType::EfiRuntimeServicesCode {
            Flags::PRESENT
        } else {
            Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE
        };
        unsafe {
            map_physical_range(
                &mut new_mapper,
                start_phys,
                end_phys,
                start_virt,
                flags,
                &mut frame_allocator,
            );
        }
    }

    HIGHER_HALF_OFFSET.call_once(|| physical_memory_offset);

    if let Some(fb_addr) = framebuffer_addr {
        let fb_start = PhysAddr::new(fb_addr);
        const FALLBACK_FRAMEBUFFER_SIZE: u64 = 4 * 1024 * 1024;
        let fb_size = framebuffer_size.unwrap_or(FALLBACK_FRAMEBUFFER_SIZE);
        let fb_end = fb_start + fb_size;
        let fb_virt = VirtAddr::new(fb_addr);
        unsafe {
            map_physical_range(
                &mut new_mapper,
                fb_start,
                fb_end,
                fb_virt,
                Flags::PRESENT | Flags::WRITABLE,
                &mut frame_allocator,
            );
        }
    }

    {
        let mut current_mapper = MAPPER.get().unwrap().lock();
        current_mapper
            .unmap(temp_virt_page)
            .expect("Failed to unmap temp")
            .1
            .flush();
    }

    unsafe { Cr3::write(level_4_frame, Cr3Flags::empty()) };
    let mapper = unsafe { petroleum::page_table::init(physical_memory_offset) };
    *MAPPER.get().unwrap().lock() = mapper;
}
