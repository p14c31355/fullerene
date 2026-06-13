//! Paging / memory-management bootstrap.
//!
//! Extracted from `uefi_init.rs` (~560 lines of `memory_management_initialization`).
//! Handles: page table setup, TSS allocation, kernel area mapping, heap allocation.

/// Core memory-management bootstrap: page tables, GDT/TSS, heap frames.
#[cfg(target_os = "uefi")]
#[allow(static_mut_refs)]
pub fn bootstrap_memory(
    ctx: &mut super::uefi_init::UefiInitContext,
    kernel_phys_start: x86_64::PhysAddr,
) -> (x86_64::VirtAddr, x86_64::PhysAddr, x86_64::VirtAddr) {
    use super::uefi_init::{create_tmp_mapper, debug_serial};
    use crate::MEMORY_MAP;
    use crate::heap;
    use petroleum::page_table::{ALLOCATOR as PETROLEUM_ALLOCATOR, MemoryMapDescriptor};
    use petroleum::{debug_log_no_alloc, write_serial_bytes};
    use x86_64::{
        PhysAddr, VirtAddr,
        structures::paging::{Mapper, PageTableFlags},
    };

    petroleum::set_physical_memory_offset(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE);
    ctx.physical_memory_offset =
        x86_64::VirtAddr::new(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64);

    write_serial_bytes(
        0x3F8,
        0x3FD,
        b"DEBUG: Starting memory_management_initialization\n",
    );

    // Early heap initialization check - no action needed, init happens later

    debug_log_no_alloc!("DEBUG: Starting memory_management_initialization");

    write_serial_bytes(
        0x3F8,
        0x3FD,
        b"DEBUG: [CircularDep] Using BootFrameAllocator\n",
    );
    let mut boot_allocator = super::uefi_init::BootFrameAllocator::new(0x2000000 / 4096);
    let _temp_mapper = unsafe {
        petroleum::page_table::init::<
            super::uefi_init::BootFrameAllocator,
            fn(
                &mut x86_64::structures::paging::OffsetPageTable,
                &mut super::uefi_init::BootFrameAllocator,
            ),
        >(
            ctx.physical_memory_offset,
            &mut boot_allocator,
            kernel_phys_start.as_u64(),
            None,
        )
    };

    debug_log_no_alloc!("DEBUG: Calling init_memory_map...");
    ctx.init_memory_map();
    debug_log_no_alloc!("DEBUG: init_memory_map returned");

    let boot_heap_ptr =
        unsafe { core::ptr::addr_of_mut!(crate::heap::TOTAL_HEAP_BUFFER) as *mut u8 };
    unsafe {
        petroleum::page_table::init_global_heap(boot_heap_ptr, crate::heap::HEAP_SIZE)
    };

    let memory_map_ref = MEMORY_MAP
        .lock()
        .as_ref()
        .expect("Memory map not initialized")
        .clone();
    crate::heap::init_frame_allocator(memory_map_ref);

    let kernel_size =
        unsafe { petroleum::page_table::pe::calculate_kernel_memory_size(kernel_phys_start) };
    {
        let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = fa_guard.as_mut().expect("Frame allocator not initialized");
        let kernel_pages = (kernel_size + 4095) / 4096;
        frame_allocator
            .reserve_frames(kernel_phys_start.as_u64(), kernel_pages as usize)
            .expect("Failed to reserve kernel memory");
    }

    write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: Allocating TSS stacks...\n");
    let tss_stack_pages = (crate::gdt::GDT_TSS_STACK_COUNT * crate::gdt::GDT_TSS_STACK_SIZE) / 4096;

    let tss_phys_addr = {
        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard.as_mut().expect("no frame allocator");
        match frame_allocator.allocate_contiguous_frames(tss_stack_pages) {
            Ok(phys_addr) => PhysAddr::new(phys_addr as u64),
            Err(_) => panic!("Failed to allocate TSS frames"),
        }
    };

    let base = VirtAddr::new(
        petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64 + tss_phys_addr.as_u64(),
    );
    let tss_stacks = crate::gdt::TssStacks::from_base(base);
    crate::gdt::init_with_stacks(tss_stacks);
    let tss = unsafe { crate::gdt::TSS.as_mut().expect("TSS not initialized") };
    let (gdt, code_sel, data_sel, tss_sel, user_data_sel, user_code_sel) =
        unsafe { crate::gdt::build_gdt(tss) };
    unsafe {
        crate::gdt::store_gdt(
            gdt,
            code_sel,
            data_sel,
            tss_sel,
            user_data_sel,
            user_code_sel,
        );
    };

    {
        let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let allocator = fa_guard.as_mut().expect("no frame allocator");
        let mut mapper =
            unsafe { create_tmp_mapper(ctx.physical_memory_offset, allocator, 0x100000) };
        let kernel_phys_aligned = kernel_phys_start.as_u64() & !0xFFF;
        let kernel_virt_aligned =
            petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64 & !0xFFF;
        unsafe {
            petroleum::page_table::raw::map_range_with_huge_pages(
                &mut mapper,
                allocator,
                kernel_phys_aligned,
                kernel_virt_aligned,
                256 * 1024,
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                "kernel_area",
            )
            .expect("Failed to map kernel area");
        }
        x86_64::instructions::tlb::flush_all();
    }

    let kernel_cr3 = x86_64::registers::control::Cr3::read();
    crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());

    let memory_map_ref2 = MEMORY_MAP.lock().as_ref().expect("Memory map gone").clone();
    let heap_phys_start = petroleum::uefi_helpers::find_heap_start(memory_map_ref2);
    let _heap_phys_addr =
        if heap_phys_start.as_u64() < 0x1000 || heap_phys_start.as_u64() >= 0x0000_8000_0000_0000 {
            PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
        } else {
            heap_phys_start
        };

    let heap_pages = (crate::heap::HEAP_SIZE + 4095) / 4096;
    let heap_phys_addr_val = {
        let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let fa = fa_guard.as_mut().expect("no frame allocator");
        fa.allocate_contiguous_frames(heap_pages)
            .expect("Failed to allocate heap frames")
    };
    let heap_phys_addr = PhysAddr::new(heap_phys_addr_val as u64);

    ctx.virtual_heap_start = ctx.physical_memory_offset + heap_phys_addr.as_u64();
    write_serial_bytes(0x3F8, 0x3FD, b"Heap allocated and mapped\n");

    (
        ctx.physical_memory_offset,
        heap_phys_addr,
        ctx.virtual_heap_start,
    )
}
