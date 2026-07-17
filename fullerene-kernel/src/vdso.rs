use core::sync::atomic::Ordering;

use petroleum::page_table::types::PageTableHelper;
use petroleum::vdso::{VDSO_USER_BASE, VdsoPage};
use x86_64::VirtAddr;
use x86_64::structures::paging::{FrameAllocator, PageTableFlags, PhysFrame, Size4KiB};

/// Reference to a VDSO page: kernel-side pointer + the physical frame
/// so `Drop` can free the frame when the process is destroyed.
pub struct VdsoPageRef {
    pub kernel_ptr: &'static VdsoPage,
    pub phys: PhysFrame<Size4KiB>,
}

impl Drop for VdsoPageRef {
    fn drop(&mut self) {
        use petroleum::initializer::FrameAllocator;
        let mut mgr = crate::memory_management::get_memory_manager().lock();
        if let Some(m) = mgr.as_mut() {
            let _ = m.free_frame(self.phys.start_address().as_u64() as usize);
        }
    }
}

/// Create and map a read-only VDSO page for a user-space process.
///
/// The physical frame is:
///   - zero-initialised and PID-stamped via the kernel's phys_offset mapping,
///   - mapped into the **user's** page table at `VDSO_USER_BASE` with
///     read-only user-accessible flags (no `WRITABLE` — userspace can
///     never corrupt the VDSO data),
///   - returned as a `VdsoPageRef` so the kernel can write time metadata
///     through `kernel_ptr` (which points into the phys_offset region).
pub fn create_vdso_page(
    process_pt: &mut impl PageTableHelper,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    pid: u64,
) -> Result<VdsoPageRef, petroleum::MemoryError> {
    let frame = frame_allocator
        .allocate_frame()
        .ok_or(petroleum::MemoryError::FrameAllocationFailed)?;
    let phys_addr = frame.start_address();

    let phys_offset = petroleum::PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed) as u64;
    let kernel_virt = VirtAddr::new(phys_addr.as_u64() + phys_offset);
    let page = unsafe { &mut *kernel_virt.as_mut_ptr::<VdsoPage>() };
    *page = VdsoPage::new();
    page.pid = pid;

    // User-side: read-only.  Kernel writes via phys_offset, not through
    // the user's page table, so no WRITABLE flag needed.
    let flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    process_pt
        .map_page(
            VDSO_USER_BASE as usize,
            phys_addr.as_u64() as usize,
            flags,
            frame_allocator,
        )
        .map_err(|_| {
            use petroleum::initializer::FrameAllocator;
            let mut mgr = crate::memory_management::get_memory_manager().lock();
            if let Some(m) = mgr.as_mut() {
                let _ = m.free_frame(frame.start_address().as_u64() as usize);
            }
            petroleum::MemoryError::MappingFailed
        })?;

    petroleum::debug_log_no_alloc!(
        "VDSO: created for PID {} at phys={:#x}, user={:#x}",
        pid,
        phys_addr.as_u64(),
        VDSO_USER_BASE
    );

    Ok(VdsoPageRef {
        kernel_ptr: page,
        phys: frame,
    })
}

/// Update per-process VDSO metadata (uptime, wall-clock time).
///
/// Called once per scheduler tick.  Writes are done through the kernel's
/// phys_offset mapping and are visible to user-space via the read-only
/// shared page.
pub fn update_vdso_metadata(now_us: u64, wall_us: u64, vdso: &VdsoPage) {
    vdso.uptime_us.store(now_us, Ordering::Relaxed);
    vdso.time_us.store(wall_us, Ordering::Release);
}
