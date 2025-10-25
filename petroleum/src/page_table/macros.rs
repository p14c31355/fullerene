// Consolidated macros to reduce code duplication and improve organization

use crate::debug_log_no_alloc;

// read_unaligned moved to common/macros.rs

// Consolidated validation logging macro
macro_rules! debug_log_validate_macro {
    ($field:expr, $value:expr) => {
        debug_log_no_alloc!($field, " validated: ", $value);
    };
}

// Unified memory region constants macro
macro_rules! memory_region_const_macro {
    (VGA_START) => {
        0xA0000u64
    };
    (VGA_END) => {
        0xC0000u64
    };
    (BOOT_CODE_START) => {
        0x100000u64
    };
    (BOOT_CODE_PAGES) => {
        0x8000u64
    };
    (PAGE_SIZE) => {
        4096u64
    };
}

// Consolidated logging macro for page table operations
macro_rules! log_page_table_op {
    ($operation:expr) => {
        debug_log_no_alloc!($operation);
    };
    ($operation:expr, $msg:expr, $addr:expr) => {
        debug_log_no_alloc!($operation, $msg, " addr=", $addr);
    };
    ($stage:expr, $phys:expr, $virt:expr, $pages:expr) => {
        debug_log_no_alloc!(
            "Memory mapping stage=",
            $stage,
            " phys=0x",
            $phys,
            " virt=0x",
            $virt,
            " pages=",
            $pages
        );
    };
    ($operation:expr, $msg:expr) => {
        debug_log_no_alloc!($operation, $msg);
    };
}

// Memory descriptor processing macro
macro_rules! process_memory_descriptors_safely {
    ($descriptors:expr, $processor:expr) => {{
        for descriptor in $descriptors.iter() {
            if is_valid_memory_descriptor(descriptor) && descriptor.is_memory_available() {
                let start_frame = (descriptor.get_physical_start() / 4096) as usize;
                let end_frame = start_frame.saturating_add(descriptor.get_page_count() as usize);

                if start_frame < end_frame {
                    $processor(descriptor, start_frame, end_frame);
                }
            }
        }
    }};
}

// Page table flags constants macro
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
    (READ_EXECUTE) => {
        PageTableFlags::PRESENT
    };
}

// Integrated identity mapping macro
macro_rules! map_identity_range_macro {
    ($mapper:expr, $frame_allocator:expr, $start_addr:expr, $pages:expr, $flags:expr) => {{
        unsafe {
            map_identity_range($mapper, $frame_allocator, $start_addr, $pages, $flags)
                .expect("Failed to identity map range")
        }
    }};
}

// Range mapping with logging macro
macro_rules! map_range_with_log_macro {
    ($mapper:expr, $frame_allocator:expr, $phys_start:expr, $virt_start:expr, $num_pages:expr, $flags:expr) => {{
        log_page_table_op!("Mapping range", $phys_start, $virt_start, $num_pages);
        for i in 0..$num_pages {
            let phys_addr = $phys_start + i * 4096;
            let virt_addr = $virt_start + i * 4096;
            let (page, frame) = create_page_and_frame!(virt_addr, phys_addr);
            match $mapper.map_to(page, frame, $flags, $frame_allocator) {
                Ok(flush) => flush.flush(),
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }};
}

// Higher half configuration macro
macro_rules! higher_half_config {
    ($phys_offset:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {
        MappingConfig {
            phys_start: $phys_start,
            virt_start: $phys_offset.as_u64() + $phys_start,
            num_pages: $num_pages,
            flags: $flags,
        }
    };
}

// Stack pointer macro
macro_rules! get_current_stack_pointer {
    () => {{
        let rsp: u64;
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp); }
        rsp
    }};
}

// Initialization check macro
macro_rules! ensure_initialized {
    ($self:expr) => {
        if !$self.initialized {
            return Err(crate::common::logging::SystemError::InternalError);
        }
    };
}
