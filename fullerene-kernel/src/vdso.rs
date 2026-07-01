use core::sync::atomic::Ordering;

use petroleum::page_table::types::PageTableHelper;
use petroleum::vdso::{VdsoPage, VDSO_COMPLETE, VDSO_PENDING, VDSO_USER_BASE};
use x86_64::structures::paging::{FrameAllocator, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

use crate::process::PROCESS_MANAGER;
use crate::syscall::handlers::handle_syscall;

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

pub fn create_vdso_page(
    process_pt: &mut impl PageTableHelper,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    pid: u64,
) -> Result<VdsoPageRef, &'static str> {
    let frame = frame_allocator
        .allocate_frame()
        .ok_or("VDSO: out of frames")?;
    let phys_addr = frame.start_address();

    let phys_offset = petroleum::PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed) as u64;
    let kernel_virt = VirtAddr::new(phys_addr.as_u64() + phys_offset);
    let page = unsafe { &mut *kernel_virt.as_mut_ptr::<VdsoPage>() };
    *page = VdsoPage::new();
    page.pid = pid;

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
        .map_err(|_| {
            use petroleum::initializer::FrameAllocator;
            let mut mgr = crate::memory_management::get_memory_manager().lock();
            if let Some(m) = mgr.as_mut() {
                let _ = m.free_frame(frame.start_address().as_u64() as usize);
            }
            "VDSO: map_page failed"
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

pub fn poll_vdso_page(vdso: &VdsoPage) {
    for slot in 0..petroleum::vdso::VDSO_RING_SIZE {
        let state = vdso.requests[slot].state.load(Ordering::Acquire);
        if state == VDSO_PENDING {
            let syscall_num = vdso.requests[slot].syscall_num();
            let args = vdso.requests[slot].args();
            let result = unsafe {
                handle_syscall(
                    syscall_num, args[0], args[1], args[2], args[3], args[4], args[5],
                )
            };
            vdso.requests[slot].set_result(result);
            vdso.requests[slot]
                .state
                .store(VDSO_COMPLETE, Ordering::Release);
        }
    }
}

pub fn poll_all_vdso_rings() {
    let mut pids = [0u64; 64];
    let mut count = 0;
    PROCESS_MANAGER.with_list(|list| {
        for (_, proc) in list.iter() {
            if count >= 64 { break; }
            if proc.vdso_page.is_some() && proc.state != crate::process::ProcessState::Terminated {
                pids[count] = proc.id.0;
                count += 1;
            }
        }
    });

    for i in 0..count {
        let pid = pids[i];
        let page_ptr = PROCESS_MANAGER.with_list(|list| {
            list.iter()
                .find(|(id, _)| id.0 == pid)
                .and_then(|(_, proc)| {
                    if proc.state != crate::process::ProcessState::Terminated {
                        proc.vdso_page.as_ref().map(|v| v.kernel_ptr)
                    } else {
                        None
                    }
                })
        });

        if let Some(ptr) = page_ptr {
            poll_vdso_page(ptr);
        }
    }
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
