use x86_64::{VirtAddr, structures::paging::{PageTable, PhysFrame}};
use crate::page_table::constants::BootInfoFrameAllocator;
use x86_64::structures::paging::FrameAllocator;

pub fn test_page_table_copy_switch(
    phys_offset: VirtAddr,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[impl crate::page_table::efi_memory::MemoryDescriptorValidator],
) -> crate::common::logging::SystemResult<()> {
    crate::debug_log_no_alloc!("[PT TEST] Starting minimal page table copy test");

    // Get current CR3
    let (original_cr3, _) = crate::safe_cr3_read!();

    // Allocate new frame for cloned table
    let new_l4_frame = match frame_allocator.allocate_frame() {
        Some(frame) => frame,
        None => {
            crate::debug_log_no_alloc!("[PT TEST ERROR] Failed to allocate L4 frame");
            return Err(crate::common::logging::SystemError::FrameAllocationFailed);
        }
    };
    crate::debug_log_no_alloc!(
        "[PT TEST] Allocated new L4 frame at: 0x",
        new_l4_frame.start_address().as_u64() as usize
    );

    // Zero the new frame using identity mapping (UEFI maps all memory)
    unsafe {
        let new_l4_virt = phys_offset + new_l4_frame.start_address().as_u64();
        let table_ptr = new_l4_virt.as_mut_ptr() as *mut PageTable;
        *table_ptr = PageTable::new();
    }
    crate::debug_log_no_alloc!("[PT TEST] Zeroed new L4 table");

    // Copy from original table - just duplicate the frame contents
    unsafe {
        let original_virt = phys_offset + original_cr3.start_address().as_u64();
        let original_table = &*(original_virt.as_ptr() as *const PageTable);

        let new_l4_virt = phys_offset + new_l4_frame.start_address().as_u64();
        let new_table = &mut *(new_l4_virt.as_mut_ptr() as *mut PageTable);

        // Simple memcpy of the table contents
        core::ptr::copy_nonoverlapping(original_table as *const PageTable, new_table, 1);
    }
    crate::debug_log_no_alloc!("[PT TEST] Copied original L4 table contents");

    // Try the CR3 switch
    crate::debug_log_no_alloc!("[PT TEST] Attempting CR3 switch...");
    crate::safe_cr3_write!(new_l4_frame);

    // Verify switch
    let (current_cr3, _) = crate::safe_cr3_read!();
    if current_cr3 == new_l4_frame {
        crate::debug_log_no_alloc!("[PT TEST] CR3 switch succeeded!");
        x86_64::instructions::tlb::flush_all();
    } else {
        crate::debug_log_no_alloc!(
            "[PT TEST ERROR] CR3 switch failed - still at old table",
            current_cr3.start_address().as_u64() as usize
        );
        // Clean up allocated frame
        frame_allocator.deallocate_frame(new_l4_frame);
        return Err(crate::common::logging::SystemError::InternalError);
    }

    // Switch back to original
    crate::debug_log_no_alloc!("[PT TEST] Switching back to original CR3...");
    crate::safe_cr3_write!(original_cr3);

    // Verify back-switch
    let (final_cr3, _) = crate::safe_cr3_read!();
    if final_cr3 == original_cr3 {
        crate::debug_log_no_alloc!("[PT TEST] Switch back to original CR3 successful");
    } else {
        crate::debug_log_no_alloc!(
            "[PT TEST ERROR] Switch back to original CR3 failed",
            final_cr3.start_address().as_u64() as usize
        );
        // Clean up allocated frame
        frame_allocator.deallocate_frame(new_l4_frame);
        return Err(crate::common::logging::SystemError::InternalError);
    }

    // Clean up
    frame_allocator.deallocate_frame(new_l4_frame);
    crate::debug_log_no_alloc!("[PT TEST] Test completed successfully");

    Ok(())
}