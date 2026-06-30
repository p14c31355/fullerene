use core::sync::atomic::Ordering;

use petroleum::page_table::types::PageTableHelper;
use petroleum::vdso::{VdsoPage, VDSO_PENDING, VDSO_USER_BASE};
use x86_64::structures::paging::{FrameAllocator, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::process::{Process, PROCESS_MANAGER};
use crate::syscall::handlers::handle_syscall;

pub struct VdsoPageRef {
    pub kernel_ptr: &'static mut VdsoPage,
    pub phys: PhysFrame<Size4KiB>,
}

pub fn create_vdso_page(
    process_pt: &mut impl PageTableHelper,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    pid: u64,
) -> Result<VdsoPageRef, &'static str> {
    let frame = frame_allocator
        .allocate_frame()
        .ok_or("VDSO: out of frames")?;
    let phys_addr = frame.start_address();

    // Initialize via kernel higher-half mapping
    let phys_offset = petroleum::PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed);
    let kernel_virt = VirtAddr::new(phys_addr.as_u64() + phys_offset);
    let page = unsafe { &mut *kernel_virt.as_mut_ptr::<VdsoPage>() };
    *page = VdsoPage::new();
    page.pid = pid;

    // Map into process address space via the process page table helper
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE;
    process_pt
        .map_page(
            VDSO_USER_BASE as usize,
            phys_addr.as_u64() as usize,
            flags,
            frame_allocator,
        )
        .map_err(|_| "VDSO: map_page failed")?;

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

pub fn poll_vdso_page(vdso: &mut VdsoPage) {
    for slot in 0..petroleum::vdso::VDSO_RING_SIZE {
        let req = &vdso.requests[slot];
        let state = req.state.load(Ordering::Acquire);
        if state == VDSO_PENDING {
            let syscall_num = req.syscall_num;
            let args = req.args;
            let result = unsafe {
                handle_syscall(
                    syscall_num, args[0], args[1], args[2], args[3], args[4], args[5],
                )
            };
            req.args[0] = result;
            req.state.store(result + 2, Ordering::Release);
        }
    }
}

pub fn poll_all_vdso_rings() {
    PROCESS_MANAGER.with_list(|list| {
        for (_, proc) in list.iter_mut() {
            if let Some(ref mut vdso) = proc.vdso_page {
                poll_vdso_page(&mut *vdso.kernel_ptr);
            }
        }
    });
}

pub fn update_vdso_metadata(now_us: u64, wall_us: u64) {
    PROCESS_MANAGER.with_list(|list| {
        for (_, proc) in list.iter_mut() {
            if let Some(ref vdso) = proc.vdso_page {
                vdso.kernel_ptr.uptime_us.store(now_us, Ordering::Relaxed);
                vdso.kernel_ptr.time_us.store(wall_us, Ordering::Release);
            }
        }
    });
}
