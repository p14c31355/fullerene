//! Paging and memory mapping macros for Fullerene OS

use crate::page_table::manager::PageTableHelper;

#[macro_export]
macro_rules! map_range_with_log_macro {
    ($mapper:expr, $allocator:expr, $phys:expr, $virt:expr, $pages:expr, $flags:expr) => {{
        unsafe {
            $crate::page_table::map_range_with_huge_pages(
                $mapper,
                $allocator,
                $phys,
                $virt,
                $pages,
                $flags,
                "panic",
            )
        }
    }};
}

#[macro_export]
macro_rules! map_pages_to_offset {
    ($mapper:expr, $allocator:expr, $phys_base:expr, $virt_offset:expr, $num_pages:expr, $flags:expr) => {{
        $crate::map_pages!(
            $mapper,
            $allocator,
            $phys_base,
            ($virt_offset + $phys_base),
            $num_pages,
            $flags,
            "panic"
        );
    }};
}

#[macro_export]
macro_rules! map_pages {
    ($mapper:expr, $allocator:expr, $phys_base:expr, $virt_calc:expr, $num_pages:expr, $flags:expr, $behavior:tt) => {{
        use x86_64::{
            PhysAddr, VirtAddr,
            structures::paging::{Page, PhysFrame, Size4KiB, Mapper, mapper::MapToError},
        };
        for i in 0..$num_pages {
            let phys_addr = $phys_base + i * 4096;
            let virt_addr = $virt_calc + i * 4096;
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt_addr));
            let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys_addr));
            unsafe {
                match $mapper.map_to(page, frame, $flags, $allocator) {
                    Ok(flush) => flush.flush(),
                    Err(MapToError::PageAlreadyMapped(_)) => {}
                    Err(e) => match $behavior {
                        "continue" => continue,
                        "panic" => panic!("Mapping error: {:?}", e),
                        _ => {}
                    },
                }
            }
        }
    }};
}

#[macro_export]
macro_rules! map_identity_range_macro {
    ($mapper:expr, $frame_allocator:expr, $start_addr:expr, $pages:expr, $flags:expr) => {{ 
        unsafe { $crate::page_table::utils::map_identity_range($mapper, $frame_allocator, $start_addr, $pages, $flags) } 
    }};
}

#[macro_export]
macro_rules! identity_map_range_with_log_macro {
    ($mapper:expr, $frame_allocator:expr, $start_addr:expr, $num_pages:expr, $flags:expr) => {{
        log_page_table_op!(
            "Identity mapping start",
            $start_addr,
            $start_addr,
            $num_pages
        );
        let result =
            map_identity_range_macro!($mapper, $frame_allocator, $start_addr, $num_pages, $flags);
        if result.is_ok() {
            log_page_table_op!(
                "Identity mapping complete",
                $start_addr,
                $start_addr,
                $num_pages
            );
        }
        result
    }};
}

#[macro_export]
macro_rules! map_to_higher_half_with_log_macro {
    ($mapper:expr, $frame_allocator:expr, $phys_offset:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {{
        let virt_start = $phys_offset.as_u64() + $phys_start;
        log_page_table_op!(
            "Higher half mapping start",
            $phys_start,
            virt_start,
            $num_pages
        );
        map_range_with_log_macro!(
            $mapper,
            $frame_allocator,
            $phys_start,
            virt_start,
            $num_pages,
            $flags
        );
        log_page_table_op!(
            "Higher half mapping complete",
            $phys_start,
            virt_start,
            $num_pages
        );
        Ok::<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>>(())
    }};
}

#[macro_export]
macro_rules! map_with_log_macro {
    ($mapper:expr, $allocator:expr, $phys:expr, $virt:expr, $pages:expr, $flags:expr, $behavior:tt) => {{
        unsafe {
            $crate::page_table::map_range_with_huge_pages(
                $mapper,
                $allocator,
                $phys,
                $virt,
                $pages,
                $flags,
                $behavior,
            )
        }
    }};
}

#[macro_export]
macro_rules! flush_tlb_and_verify {
    () => {{
        use x86_64::instructions::tlb;
        use x86_64::registers::control::{Cr3, Cr3Flags};
        tlb::flush_all();
        // Verify by reading CR3 to force a TLB reload
        let (frame, flags): (
            x86_64::structures::paging::PhysFrame<x86_64::structures::paging::Size4KiB>,
            Cr3Flags,
        ) = Cr3::read();
        unsafe { Cr3::write(frame, flags) };
    }};
}

#[macro_export]
macro_rules! create_page_and_frame {
    ($virt_addr:expr, $phys_addr:expr) => {{
        use x86_64::{
            PhysAddr, VirtAddr,
            structures::paging::{Page, PhysFrame, Size4KiB},
        };
        let virt = VirtAddr::new($virt_addr);
        let phys = PhysAddr::new($phys_addr);
        let page = Page::<Size4KiB>::containing_address(virt);
        let frame = PhysFrame::<Size4KiB>::containing_address(phys);
        (page, frame)
    }};
}

#[macro_export]
macro_rules! map_and_flush {
    ($mapper:expr, $page:expr, $frame:expr, $flags:expr, $allocator:expr) => {{
        unsafe {
            $mapper
                .map_to($page, $frame, $flags, $allocator)
                .expect("Failed to map page")
                .flush();
        }
    }};
}

#[macro_export]
macro_rules! map_with_offset {
    ($mapper:expr, $allocator:expr, $phys_addr:expr, $virt_addr:expr, $flags:expr, $behavior:tt) => {{
        let (page, frame) = create_page_and_frame!($virt_addr, $phys_addr);
        unsafe {
            match $mapper.map_to(page, frame, $flags, $allocator) {
                Ok(flush) => flush.flush(),
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {}
                Err(e) => match $behavior {
                    "continue" => {}
                    "panic" => panic!("Mapping error: {:?}", e),
                    _ => {}
                }
            }
        }
    }};
}

#[macro_export]
macro_rules! map_identity_range_checked {
    ($mapper:expr, $allocator:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {{
        unsafe {
            $crate::page_table::map_range_with_huge_pages(
                $mapper,
                $allocator,
                $phys_start,
                $phys_start,
                $num_pages,
                $flags,
                "panic",
            )
        }
    }};
}

#[macro_export]
macro_rules! map_page_range {
    ($mapper:expr, $allocator:expr, $base_virt:expr, $base_phys:expr, $num_pages:expr, $flags:expr) => {{
        for i in 0..$num_pages {
            let phys_addr = $base_phys + (i * 4096);
            let virt_addr = $base_virt + (i * 4096);
            $mapper.map_page(virt_addr, phys_addr, $flags, $allocator)?;
        }
    }};
}

#[macro_export]
macro_rules! unmap_page_range {
    ($mapper:expr, $base_virt:expr, $num_pages:expr) => {{
        for i in 0..$num_pages {
            let vaddr = $base_virt + (i * 4096);
            $mapper.unmap_page(vaddr)?;
        }
    }};
}

#[macro_export]
macro_rules! align_page {
    ($size:expr) => {{
        const PAGE_SIZE: usize = 4096;
        ($size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
    }};
}

#[macro_export]
macro_rules! calculate_kernel_pages {
    ($size:expr) => {
        ($size.div_ceil(4096))
    };
}

#[macro_export]
macro_rules! get_memory_stats {
    () => {{
        #[cfg(not(feature = "std"))]
        {
            if $crate::page_table::HEAP_INITIALIZED.load(core::sync::atomic::Ordering::SeqCst) {
                let allocator = $crate::page_table::ALLOCATOR.lock();
                let used = allocator.used();
                let total = allocator.size();
                let free = total.saturating_sub(used);
                (used, total, free)
            } else {
                (0, 0, 0)
            }
        }
        #[cfg(feature = "std")]
        {
            (0, 0, 0)
        }
    }};
}

#[macro_export]
macro_rules! page_flags_const {
    (READ_WRITE_NO_EXEC) => {
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
    };
    (READ_ONLY) => {
        PageTableFlags::PRESENT
    };
    (READ_WRITE) => {
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
    };
    (READ_WRITE_EXEC) => {
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE
    };
    (READ_EXECUTE) => {
        PageTableFlags::PRESENT
    };
}