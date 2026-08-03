// Linux process syscall implementations
extern crate alloc;
use super::numbers::*;
use super::runtime::{
    LinuxRuntime, copy_to_user, copy_user_string, copy_val_from_user, copy_val_to_user, errno_code,
};
use crate::process::{self, ProcessContext, ProcessId};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use petroleum::page_table::FrameAllocatorExt;
use petroleum::page_table::types::PageTableHelper;
use x86_64::PhysAddr;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocator, OffsetPageTable, PageTable, PageTableFlags, Size4KiB,
};

fn release_private_frames(frames: &[u64]) {
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
    for &address in frames {
        if let Some(frame) = petroleum::page_table::PhysFrame::from_start_address(address) {
            frame_alloc.deallocate_frame(frame);
        }
    }
}

pub(crate) unsafe fn force_user_page_writable(root: PhysAddr, virtual_address: u64) -> bool {
    let offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let mut table = unsafe {
        &mut *(offset + root.as_u64()).as_mut_ptr::<x86_64::structures::paging::PageTable>()
    };
    let address = x86_64::VirtAddr::new(virtual_address);
    for (level, index) in [
        address.p4_index(),
        address.p3_index(),
        address.p2_index(),
        address.p1_index(),
    ]
    .into_iter()
    .enumerate()
    {
        let (flags, next_table) = {
            let entry = &mut table[index];
            let flags = entry.flags();
            if !flags.contains(PageTableFlags::PRESENT) {
                return false;
            }
            if level == 3 && flags.contains(PageTableFlags::BIT_9) {
                let source = entry.addr();
                let frame_alloc =
                    unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
                let Some(frame) = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) else {
                    return false;
                };
                let source_va =
                    petroleum::common::memory::physical_to_virtual(source.as_u64() as usize)
                        as *const u8;
                let dest_va = petroleum::common::memory::physical_to_virtual(
                    frame.start_address().as_u64() as usize,
                ) as *mut u8;
                unsafe {
                    core::ptr::copy_nonoverlapping(source_va, dest_va, 4096);
                }
                let mut private_flags = flags;
                private_flags.remove(PageTableFlags::BIT_9);
                private_flags.insert(PageTableFlags::WRITABLE);
                entry.set_addr(frame.start_address(), private_flags);
            } else if level == 3 {
                return false;
            }
            (flags, entry.addr())
        };
        if flags.contains(PageTableFlags::HUGE_PAGE) {
            // A 2 MiB/1 GiB leaf cannot be promoted by the 4 KiB COW path.
            // Report failure so the page-fault handler diagnoses the fault
            // instead of retrying the same unchanged mapping forever.
            return false;
        }
        if level == 3 {
            unsafe {
                core::arch::asm!(
                    "invlpg [{}]",
                    in(reg) virtual_address,
                    options(nostack, preserves_flags)
                );
            }
            return true;
        }
        table = unsafe {
            &mut *(offset + next_table.as_u64())
                .as_mut_ptr::<x86_64::structures::paging::PageTable>()
        };
    }
    false
}

/// Give a forked child private copies of every user leaf in the supplied
/// ranges.  This includes read-only mmap pages: `execve` must be able to
/// unmap the child's inherited dynamic-loader mappings without freeing frames
/// still referenced by the parent.
unsafe fn copy_child_user_leaves(root: PhysAddr, ranges: &[(u64, u64)]) -> Result<(), ()> {
    unsafe fn walk(
        table: *mut PageTable,
        level: usize,
        virtual_base: u64,
        ranges: &[(u64, u64)],
        frame_alloc: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> Result<(), ()> {
        let entry_count = if level == 4 { 256 } else { 512 };
        for index in 0..entry_count {
            let entry = unsafe { &mut (&mut *table)[index] };
            let flags = entry.flags();
            if !flags.contains(PageTableFlags::PRESENT) {
                continue;
            }
            let page_base = virtual_base | ((index as u64) << ((level - 1) * 9 + 12));
            let entry_size = 1u64 << ((level - 1) * 9 + 12);
            let entry_end = page_base.saturating_add(entry_size);
            if !ranges
                .iter()
                .any(|(start, end)| *start < entry_end && page_base < *end)
            {
                continue;
            }
            if level > 1 && !flags.contains(PageTableFlags::HUGE_PAGE) {
                let offset = x86_64::VirtAddr::new(
                    petroleum::common::memory::get_physical_memory_offset() as u64,
                );
                unsafe {
                    walk(
                        (offset + entry.addr().as_u64()).as_mut_ptr::<PageTable>(),
                        level - 1,
                        page_base,
                        ranges,
                        frame_alloc,
                    )?;
                }
                continue;
            }
            if level != 1
                || !ranges
                    .iter()
                    .any(|(start, end)| page_base >= *start && page_base < *end)
            {
                continue;
            }

            let Some(frame) = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) else {
                return Err(());
            };
            let source =
                petroleum::common::memory::physical_to_virtual(entry.addr().as_u64() as usize)
                    as *const u8;
            let destination = petroleum::common::memory::physical_to_virtual(
                frame.start_address().as_u64() as usize,
            ) as *mut u8;
            unsafe {
                core::ptr::copy_nonoverlapping(source, destination, 4096);
            }
            entry.set_addr(frame.start_address(), flags);
        }
        Ok(())
    }

    let offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
    unsafe {
        walk(
            (offset + root.as_u64()).as_mut_ptr::<PageTable>(),
            4,
            0,
            ranges,
            frame_alloc,
        )
    }
}

unsafe fn user_leaf_info(root: PhysAddr, virtual_address: u64) -> (u64, u64) {
    let offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let mut table = unsafe { &mut *(offset + root.as_u64()).as_mut_ptr::<PageTable>() };
    let address = x86_64::VirtAddr::new(virtual_address);
    for (level, index) in [
        address.p4_index(),
        address.p3_index(),
        address.p2_index(),
        address.p1_index(),
    ]
    .into_iter()
    .enumerate()
    {
        let entry = &mut table[index];
        let flags = entry.flags();
        if !flags.contains(PageTableFlags::PRESENT) {
            return (0, flags.bits());
        }
        if level == 3 || flags.contains(PageTableFlags::HUGE_PAGE) {
            return (entry.addr().as_u64(), flags.bits());
        }
        table = unsafe { &mut *(offset + entry.addr().as_u64()).as_mut_ptr::<PageTable>() };
    }
    (0, 0)
}

/// Replace a leaf in the current process page-table tree without freeing the
/// old frame. Forked Linux processes share old leaves with their parent until
/// execve, so freeing an old child mapping here would invalidate the parent.
unsafe fn replace_user_leaf_mapping(
    root: PhysAddr,
    virtual_address: u64,
    frame: PhysAddr,
    flags: PageTableFlags,
) -> bool {
    let offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let mut table = unsafe { &mut *(offset + root.as_u64()).as_mut_ptr::<PageTable>() };
    let address = x86_64::VirtAddr::new(virtual_address);
    for (level, index) in [
        address.p4_index(),
        address.p3_index(),
        address.p2_index(),
        address.p1_index(),
    ]
    .into_iter()
    .enumerate()
    {
        let entry = &mut table[index];
        if !entry.flags().contains(PageTableFlags::PRESENT)
            || entry.flags().contains(PageTableFlags::HUGE_PAGE)
        {
            return false;
        }
        if level == 3 {
            entry.set_addr(frame, flags);
            return true;
        }
        table = unsafe { &mut *(offset + entry.addr().as_u64()).as_mut_ptr::<PageTable>() };
    }
    false
}

/// Detach the page-table branches covering a small user range before execve
/// replaces its mappings. This also keeps execve correct for any legacy
/// shallow-clone caller that may still exist outside the Linux fork path.
fn make_user_range_private(start: u64, end: u64) -> Result<(), i32> {
    if start >= end {
        return Ok(());
    }
    let first = x86_64::VirtAddr::new(start);
    let last = x86_64::VirtAddr::new(end - 1);
    if first.p4_index() != last.p4_index() || first.p3_index() != last.p3_index() {
        return Err(ENOMEM);
    }

    let offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let (root_frame, _) = Cr3::read();
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };

    unsafe {
        let root = &mut *(offset + root_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
        let p4_entry = &mut root[first.p4_index()];
        let p3_source = p4_entry.frame().map_err(|_| ENOMEM)?;
        let p3_frame = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc).ok_or(ENOMEM)?;
        let p3_source_table = &*(offset + p3_source.start_address().as_u64()).as_ptr::<PageTable>();
        let p3_table = &mut *(offset + p3_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
        for index in 0..512 {
            p3_table[index] = p3_source_table[index].clone();
        }
        p4_entry.set_addr(p3_frame.start_address(), p4_entry.flags());

        let p3_entry = &mut p3_table[first.p3_index()];
        let p2_source = p3_entry.frame().map_err(|_| ENOMEM)?;
        let p2_frame = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc).ok_or(ENOMEM)?;
        let p2_source_table = &*(offset + p2_source.start_address().as_u64()).as_ptr::<PageTable>();
        let p2_table = &mut *(offset + p2_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
        for index in 0..512 {
            p2_table[index] = p2_source_table[index].clone();
        }
        p3_entry.set_addr(p2_frame.start_address(), p3_entry.flags());

        for p2_index in
            usize::from(u16::from(first.p2_index()))..=usize::from(u16::from(last.p2_index()))
        {
            let p2_entry = &mut p2_table[p2_index];
            if !p2_entry.flags().contains(PageTableFlags::PRESENT)
                || p2_entry.flags().contains(PageTableFlags::HUGE_PAGE)
            {
                continue;
            }
            let p1_source = p2_entry.frame().map_err(|_| ENOMEM)?;
            let p1_frame =
                X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc).ok_or(ENOMEM)?;
            let p1_source_table =
                &*(offset + p1_source.start_address().as_u64()).as_ptr::<PageTable>();
            let p1_table =
                &mut *(offset + p1_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
            for index in 0..512 {
                p1_table[index] = p1_source_table[index].clone();
            }
            p2_entry.set_addr(p1_frame.start_address(), p2_entry.flags());
        }
    }
    Ok(())
}

fn zero_existing_writable_user_pages(start: u64, end: u64) -> Result<(), i32> {
    let (root_frame, _) = Cr3::read();
    let mut address = start;
    while address < end {
        let (frame, flags) = unsafe { user_leaf_info(root_frame.start_address(), address) };
        if frame == 0
            || flags & PageTableFlags::WRITABLE.bits() == 0
            || flags & PageTableFlags::USER_ACCESSIBLE.bits() == 0
        {
            return Err(ENOMEM);
        }
        unsafe {
            core::ptr::write_bytes(
                petroleum::common::memory::physical_to_virtual(frame as usize) as *mut u8,
                0,
                4096,
            );
            core::arch::asm!(
                "invlpg [{}]",
                in(reg) address,
                options(nostack, preserves_flags)
            );
        }
        address += 4096;
    }
    Ok(())
}

fn zero_existing_writable_user_range(start: u64, end: u64) {
    let (root_frame, _) = Cr3::read();
    let mut address = start & !4095;
    let end = end.saturating_add(4095) & !4095;
    while address < end {
        let (frame, flags) = unsafe { user_leaf_info(root_frame.start_address(), address) };
        if frame != 0
            && flags & PageTableFlags::WRITABLE.bits() != 0
            && flags & PageTableFlags::USER_ACCESSIBLE.bits() != 0
        {
            unsafe {
                core::ptr::write_bytes(
                    petroleum::common::memory::physical_to_virtual(frame as usize) as *mut u8,
                    0,
                    4096,
                );
                core::arch::asm!(
                    "invlpg [{}]",
                    in(reg) address,
                    options(nostack, preserves_flags)
                );
            }
        }
        address = address.saturating_add(4096);
    }
}

fn copy_user_vector(vector: u64) -> Result<Vec<alloc::string::String>, i32> {
    if vector == 0 {
        return Ok(Vec::new());
    }
    const MAX_ARG_STRINGS: u64 = 0x7fff;
    const MAX_ARG_BYTES: usize = 2 * 1024 * 1024;
    let mut values = Vec::new();
    let mut copied_bytes = 0usize;
    for index in 0..MAX_ARG_STRINGS {
        let slot = vector
            .checked_add(
                index
                    .checked_mul(core::mem::size_of::<u64>() as u64)
                    .ok_or(EFAULT)?,
            )
            .ok_or(EFAULT)?;
        let pointer = unsafe { copy_val_from_user::<u64>(slot) }?;
        if pointer == 0 {
            return Ok(values);
        }
        let value = unsafe { copy_user_string(pointer, 4096) }?;
        copied_bytes = copied_bytes.checked_add(value.len() + 1).ok_or(E2BIG)?;
        if copied_bytes > MAX_ARG_BYTES {
            return Err(E2BIG);
        }
        values.push(value);
    }
    Err(E2BIG)
}

fn initialize_exec_stack(
    stack_top: u64,
    argv: &[alloc::string::String],
    envp: &[alloc::string::String],
) -> Result<u64, i32> {
    let mut cursor = stack_top;
    let mut argv_addresses = Vec::with_capacity(argv.len());
    for value in argv.iter().rev() {
        let size = (value.len() + 1) as u64;
        cursor = cursor.checked_sub(size).ok_or(EFAULT)?;
        unsafe { copy_to_user(cursor, value.as_bytes()) }?;
        unsafe { copy_to_user(cursor + value.len() as u64, &[0]) }?;
        argv_addresses.push(cursor);
    }
    argv_addresses.reverse();

    let mut env_addresses = Vec::with_capacity(envp.len());
    for value in envp.iter().rev() {
        let size = (value.len() + 1) as u64;
        cursor = cursor.checked_sub(size).ok_or(EFAULT)?;
        unsafe { copy_to_user(cursor, value.as_bytes()) }?;
        unsafe { copy_to_user(cursor + value.len() as u64, &[0]) }?;
        env_addresses.push(cursor);
    }
    env_addresses.reverse();

    cursor = cursor.checked_sub(16).ok_or(EFAULT)?;
    let random_address = cursor;
    let random = crate::loader::linux_stack_random();
    unsafe { copy_to_user(random_address, &random) }?;

    cursor &= !15;
    let word_count = 1 + argv_addresses.len() + 1 + env_addresses.len() + 1 + 6;
    let rsp = cursor
        .checked_sub((word_count * core::mem::size_of::<u64>()) as u64)
        .ok_or(EFAULT)?
        & !15;
    let mut words = Vec::with_capacity(word_count);
    words.push(argv_addresses.len() as u64);
    words.extend_from_slice(&argv_addresses);
    words.push(0);
    words.extend_from_slice(&env_addresses);
    words.push(0);
    words.extend_from_slice(&[6, 4096, 25, random_address, 0, 0]);
    for (index, value) in words.into_iter().enumerate() {
        unsafe { copy_val_to_user(rsp + (index as u64 * 8), &value) }?;
    }
    Ok(rsp)
}

pub fn sys_exit(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let code = args[0] as i32;
    let terminal_owner_exit = rt.terminal_window.is_some() && rt.tid == rt.terminal_owner_tid;
    if terminal_owner_exit {
        if let Some(window_id) = rt.terminal_window.take() {
            solvent::close_process_terminal(window_id);
        }
    }
    // Clear child TID if set
    if rt.child_clear_tid != 0 {
        let _ = unsafe { copy_val_to_user(rt.child_clear_tid, &0i32) };
    }
    // No more user-memory access is needed. Return to the canonical kernel
    // address space before touching scheduler-owned heap objects: process
    // page tables can lack kernel-heap mappings added after they were cloned.
    let kernel_root = crate::memory_management::kernel_page_table_phys();
    if kernel_root.as_u64() != 0 {
        let frame = x86_64::structures::paging::PhysFrame::containing_address(kernel_root);
        let (_, current_flags) = Cr3::read();
        unsafe {
            Cr3::write(frame, current_flags);
        }
    }
    if let Some(pid) = process::current_pid() {
        rt.fd_table.close_all();
        crate::klog_fmt!("[LINUX-DIAG] exit pid={} code={} enter\n", pid.0, code);
        if terminal_owner_exit {
            crate::klog_fmt!(
                "[BUSYBOX-DIAG] terminal owner exited pid={} code={} terminal closed\n",
                pid.0,
                code
            );
        }
        petroleum::serial::serial_log(format_args!(
            "[LINUX-DIAG] exit pid={} code={} enter\n",
            pid.0, code
        ));
        #[cfg(linux_musl_smoke)]
        crate::solvent_linux::launch::observe_smoke_exit(pid, code);
        #[cfg(linux_busybox_smoke)]
        crate::solvent_linux::launch::observe_busybox_exit(pid, code);
        process::terminate_process(pid, code);
    }
    loop {
        x86_64::instructions::hlt()
    }
}

pub fn sys_exit_group(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    sys_exit(rt, args)
}

pub fn sys_getpid(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    process::current_pid().map(|pid| pid.0).unwrap_or(0)
}

pub fn sys_getppid(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    let pid = process::current_pid().unwrap_or(ProcessId(0));
    process::SCHEDULER
        .with_process(pid, |p| p.parent_id.map(|id| id.0).unwrap_or(0))
        .unwrap_or(0)
}

/// Return the foreground process group for the current Linux process.
///
/// Fullerene does not yet expose independent Linux process-group/session
/// objects, so a process is its own group until that model is added. This is
/// sufficient for BusyBox shell startup and keeps the result stable and
/// nonzero for terminal-control queries.
pub fn sys_getpgrp(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    process::current_pid().map(|pid| pid.0).unwrap_or(0)
}

/// Fullerene currently gives each Linux process its own process group.
pub fn sys_setpgid(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    0
}

pub fn sys_gettid(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    sys_getpid(rt, args)
}

pub fn sys_clone(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _flags = args[0];
    let _child_stack = args[1];
    let _parent_tid = args[2];
    let _child_tls = args[3];
    let _child_tid = args[4];

    // Fork uses a private page-table tree with shared leaf frames. The
    // write-fault path handles the remaining leaf-frame isolation.
    let current_pid = match process::current_pid() {
        Some(p) => p,
        None => return errno_code(ESRCH),
    };

    // Get parent info
    let (parent_pt, parent_ctx, parent_fpu) = process::SCHEDULER
        .with_process(current_pid, |p| {
            (
                p.page_table_phys_addr,
                p.context.clone(),
                crate::fpu::save_and_snapshot(p.fpu_state.as_mut()),
            )
        })
        .unwrap_or((
            PhysAddr::new(0),
            Box::new(ProcessContext::default()),
            crate::fpu::XsaveState::initial(),
        ));

    // Clone the complete page-table tree. A PML4-only copy is insufficient:
    // execve detaches user branches in the child, and destroying a shallow
    // clone leaves the parent using branches that the child has modified.
    // Leaf frames remain shared for now; the write-fault path promotes the
    // corresponding entries without sharing the page-table metadata itself.
    let cloned_table = {
        let physical_offset =
            x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
        let parent_root = physical_offset + parent_pt.as_u64();
        let mut mapper = unsafe {
            OffsetPageTable::new(
                &mut *(parent_root.as_mut_ptr::<PageTable>()),
                physical_offset,
            )
        };
        let alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
        let mut allocated_frames = Vec::new();
        let mut cloned_tables = BTreeMap::new();
        let result = unsafe {
            petroleum::page_table::process::clone_page_table_recursive(
                &mut mapper,
                alloc,
                parent_pt,
                4,
                &mut allocated_frames,
                &mut cloned_tables,
            )
        };
        match result {
            Ok(address) => address.as_u64() as usize,
            Err(_) => {
                for frame in allocated_frames {
                    alloc.deallocate_frame(petroleum::page_table::PhysFrame {
                        start_address: frame.start_address().as_u64(),
                    });
                }
                return errno_code(ENOMEM);
            }
        }
    };

    let cloned_frame = x86_64::structures::paging::PhysFrame::containing_address(
        x86_64::PhysAddr::new(cloned_table as u64),
    );
    let (user_registers, user_rip, user_rflags) =
        crate::interrupts::syscall::current_user_return_context();

    // The recursive clone shares user leaf frames. Make the child's writable
    // user image private before it can resume, so stack/TLS/data writes cannot
    // corrupt the parent while the shell waits for the child.
    unsafe {
        let stack_top = crate::loader::LINUX_STACK_TOP;
        let stack_page = stack_top - crate::loader::LINUX_STACK_SIZE;
        let mut private_ranges = Vec::with_capacity(rt.mmap_regions.len() + 2);
        private_ranges.push((crate::loader::PROGRAM_LOAD_BASE, rt.program_break));
        private_ranges.push((stack_page, stack_top));
        private_ranges.push((
            crate::loader::DYNAMIC_LINKER_BASE,
            crate::loader::DYNAMIC_LINKER_BASE + crate::loader::DYNAMIC_LINKER_RESERVE_SIZE,
        ));
        private_ranges.extend(
            rt.mmap_regions
                .iter()
                .map(|region| (region.addr, region.addr.saturating_add(region.size))),
        );
        if copy_child_user_leaves(cloned_frame.start_address(), &private_ranges).is_err() {
            return errno_code(ENOMEM);
        }
    }

    let mut child_pt =
        petroleum::page_table::process::ProcessPageTable::new_with_frame(cloned_frame);
    // `clone_page_table` returns a fully copied PML4, but the helper object
    // still needs its mapper bound to that new frame before any child-only
    // mapping is removed or added.  BusyBox's first external applet reaches
    // this path immediately, so leaving mapper unset turns a normal fork into
    // a kernel panic.
    let physical_offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let l4_virt = physical_offset + cloned_frame.start_address().as_u64();
    let mapper = unsafe {
        x86_64::structures::paging::OffsetPageTable::new(
            &mut *(l4_virt.as_mut_ptr::<x86_64::structures::paging::PageTable>()),
            physical_offset,
        )
    };
    child_pt.mapper = Some(mapper);
    let _ = petroleum::initializer::Initializable::init(&mut child_pt);

    // Allocate kernel stack
    // Scheduler cleanup owns every process kernel stack using the common
    // process-stack size.  A smaller clone-only allocation would make the
    // later `top - KERNEL_STACK_SIZE` deallocation point before the block and
    // corrupt the global allocator's free list when the child exits.
    let stack_layout =
        core::alloc::Layout::from_size_align(crate::heap::KERNEL_STACK_SIZE, 16).unwrap();
    let stack_ptr =
        petroleum::common::memory::allocate_layout(stack_layout).unwrap_or(core::ptr::null_mut());
    if stack_ptr.is_null() {
        return errno_code(ENOMEM);
    }
    let kernel_stack_top =
        x86_64::VirtAddr::new(stack_ptr as u64 + crate::heap::KERNEL_STACK_SIZE as u64);

    let child_pid = process::SCHEDULER.allocate_pid();

    // The VDSO mapping is immutable user data. Keep the existing per-process
    // VDSO ownership model unchanged while forked tasks share its leaf frame.
    let child_vdso = None;
    let child_process = process::Process {
        id: child_pid,
        name: Box::from("linux-child"),
        state: process::ProcessState::Ready,
        context: {
            let mut ctx = parent_ctx.clone();
            // Child returns 0 from clone
            ctx.registers = user_registers;
            ctx.rip = user_rip;
            ctx.rflags = user_rflags;
            ctx.kernel_rsp = 0;
            ctx
        },
        fpu_state: Box::new(parent_fpu),
        page_table_phys_addr: x86_64::PhysAddr::new(cloned_table as u64),
        page_table: Some(alloc::boxed::Box::new(child_pt)),
        kernel_stack: kernel_stack_top,
        user_stack: x86_64::VirtAddr::new(0),
        entry_point: x86_64::VirtAddr::new(0),
        is_user: true,
        exit_code: None,
        fault: None,
        parent_id: Some(current_pid),
        task_data: 0,
        vdso_page: child_vdso,
        resources: process::ProcessResources::new(),
        dispatch_mode: {
            let mut child_rt = super::runtime::LinuxRuntime::new(child_pid.0, rt.initial_break);
            child_rt.program_break = rt.program_break;
            child_rt.tls_ptr = rt.tls_ptr;
            child_rt.fd_table.entries = rt.fd_table.entries.clone();
            child_rt.mmap_regions = rt.mmap_regions.clone();
            child_rt.terminal_window = rt.terminal_window;
            child_rt.terminal_owner_tid = rt.terminal_owner_tid;
            Some(super::runtime::DispatchMode::Linux(alloc::boxed::Box::new(
                child_rt,
            )))
        },
    };

    let child_box = alloc::boxed::Box::new(child_process);
    if process::SCHEDULER.add(child_box).is_err() {
        return errno_code(ENOMEM);
    }

    child_pid.0
}

pub fn sys_fork(rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    // fork() is clone(SIGCHLD, 0, NULL, NULL, 0)
    sys_clone(rt, &[SIGCHLD as u64, 0, 0, 0, 0, 0])
}

pub fn sys_execve(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let argv = match copy_user_vector(args[1]) {
        Ok(values) if !values.is_empty() => values,
        Ok(_) => return errno_code(EINVAL),
        Err(error) => return errno_code(error),
    };
    let envp = match copy_user_vector(args[2]) {
        Ok(values) => values,
        Err(error) => return errno_code(error),
    };

    let path = match unsafe { copy_user_string(path_ptr, 256) } {
        Ok(p) => p,
        Err(e) => return errno_code(e),
    };

    log::info!("Linux execve: {}", path);

    // Read the binary file
    let data = match crate::fs::read_entire_file(&path) {
        Ok(d) => d,
        Err(error) => {
            log::warn!("[EXEC-DIAG] read failed path={} error={:?}", path, error);
            crate::klog_fmt!(
                "[LINUX-DIAG] execve read failed path={} error={:?}\n",
                path,
                error
            );
            return errno_code(ENOENT);
        }
    };

    // Parse ELF with goblin
    let elf = match goblin::elf::Elf::parse(&data) {
        Ok(e) => e,
        Err(error) => {
            log::warn!("[EXEC-DIAG] parse failed path={} error={:?}", path, error);
            crate::klog_fmt!(
                "[LINUX-DIAG] execve parse failed path={} error={:?}\n",
                path,
                error
            );
            return errno_code(ENOEXEC);
        }
    };

    let current_pid = process::current_pid().unwrap_or(ProcessId(0));
    let dynamic_image =
        elf.header.e_type == goblin::elf::header::ET_DYN || elf.interpreter.is_some();
    if dynamic_image {
        let argv_refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let envp_refs = envp.iter().map(String::as_str).collect::<Vec<_>>();
        super::memory::reset_mmap_regions(rt);
        // The old brk pages are private in a forked child, but still contain
        // the previous image's allocator metadata.  A new dynamic image
        // starts with a clean heap at the ELF break.
        zero_existing_writable_user_range(rt.initial_break, rt.program_break);
        let result = process::SCHEDULER
            .with_process(current_pid, |p| {
                let page_table = p.page_table.as_mut().ok_or(ENOEXEC)?;
                crate::loader::load_linux_image_for_exec(page_table, &data, &argv_refs, &envp_refs)
                    .map_err(|_| ENOEXEC)
            })
            .ok_or(ENOEXEC)
            .and_then(|result| result);
        let (entry, rsp, initial_break) = match result {
            Ok(values) => values,
            Err(error) => {
                log::warn!("[EXEC-DIAG] dynamic image load failed path={}", path);
                // The loader is transactional and restores the old image for
                // ordinary failures. If that rollback ever reports an error,
                // terminate rather than returning into a partially replaced
                // address space.
                let _ = error;
                process::terminate_process(current_pid, -1);
                return 0;
            }
        };
        if let Some(p) = process::SCHEDULER.with_process(current_pid, |p| {
            p.entry_point = x86_64::VirtAddr::new(entry);
            p.user_stack = x86_64::VirtAddr::new(crate::loader::LINUX_STACK_TOP);
            p.context.registers = crate::process::GeneralRegisters::default();
            p.context.registers.rsp = rsp;
            p.context.rip = entry;
            p.context.kernel_rsp = 0;
            p.context.rflags = 0x202;
            p.context.segments.cs = crate::gdt::user_code()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(1);
            p.context.segments.ss = crate::gdt::user_data()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(2);
        }) {
            let _ = p;
        } else {
            return errno_code(ENOEXEC);
        }
        rt.initial_break = initial_break;
        rt.program_break = initial_break;
        rt.tls_ptr = 0;
        rt.signal_pending = 0;
        // The syscall entry assembly owns the live SYSRET frame. Updating the
        // scheduler context alone is insufficient: execve replaces the old
        // user stack before this syscall returns, so return directly to the
        // interpreter's entry point on the new Linux stack.
        crate::interrupts::syscall::override_user_return_context(entry, rsp, 0x202);
        log::info!(
            "execve: loaded dynamic {} entry=0x{:x} stack=0x{:x}",
            path,
            entry,
            rsp
        );
        return 0;
    }

    if elf.header.e_type != goblin::elf::header::ET_EXEC {
        log::warn!(
            "[EXEC-DIAG] type failed path={} type={}",
            path,
            elf.header.e_type
        );
        crate::klog_fmt!(
            "[LINUX-DIAG] execve type failed path={} type={}\n",
            path,
            elf.header.e_type
        );
        return errno_code(ENOEXEC);
    }

    let entry = elf.header.e_entry as u64;
    let segments: Vec<(u64, usize, usize, usize, u32)> = elf
        .program_headers
        .iter()
        .filter(|ph| ph.p_type == goblin::elf::program_header::PT_LOAD)
        .map(|ph| {
            let file_off = ph.p_offset as usize;
            let file_sz = ph.p_filesz as usize;
            let mem_sz = ph.p_memsz as usize;
            let vaddr = ph.p_vaddr as u64;
            let flags = ph.p_flags;
            (vaddr, file_off, file_sz, mem_sz, flags)
        })
        .collect();

    if let (Some(first_segment), Some(last_segment)) = (
        segments.iter().map(|segment| segment.0).min(),
        segments
            .iter()
            .filter_map(|segment| segment.0.checked_add(segment.3 as u64))
            .max(),
    ) {
        let Some(last_segment) = last_segment.checked_add(4095) else {
            return errno_code(ENOMEM);
        };
        if let Err(error) = make_user_range_private(first_segment & !4095, last_segment & !4095) {
            log::warn!(
                "[EXEC-DIAG] segment privacy failed path={} error={}",
                path,
                error
            );
            crate::klog_fmt!(
                "[LINUX-DIAG] execve segment privacy failed path={} error={}\n",
                path,
                error
            );
            return errno_code(error);
        }
        let (root_frame, _) = Cr3::read();
        if unsafe {
            copy_child_user_leaves(
                root_frame.start_address(),
                &[(first_segment & !4095, last_segment & !4095)],
            )
        }
        .is_err()
        {
            process::terminate_process(current_pid, -1);
            return 0;
        }
    }

    // ── Replace old process memory ────────────────────────
    // The old brk/TLS pages are already private in the forked child, and the
    // static BusyBox startup can access the inherited FS base before issuing
    // its first arch_prctl. Keep those pages mapped until process exit while
    // resetting the logical break below; replacing the ELF segments and
    // zeroing the new stack is sufficient for exec isolation.
    let old_break = rt.program_break;
    if old_break > rt.initial_break {
        zero_existing_writable_user_range(rt.initial_break, old_break);
    }

    // ── Load and map new segments ─────────────────────────
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
    let (root_frame, _) = Cr3::read();
    let mut replaced_frames = Vec::new();
    let mapped = process::SCHEDULER.with_process(current_pid, |p| {
        let Some(page_table) = p.page_table.as_mut() else {
            return Err(ENOMEM);
        };
        let page_table = &mut **page_table;
        for &(vaddr, file_off, file_sz, mem_sz, flags) in &segments {
            if mem_sz == 0 {
                continue;
            }
            let segment_end = vaddr.checked_add(mem_sz as u64).ok_or(ENOEXEC)?;
            let file_end = vaddr.checked_add(file_sz as u64).ok_or(ENOEXEC)?;
            let page_start = vaddr & !4095;
            let page_end = segment_end.checked_add(4095).ok_or(ENOEXEC)? & !4095;
            let mut page_vaddr = page_start;
            while page_vaddr < page_end {
                let Some(frame) = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) else {
                    return Err(ENOMEM);
                };
                let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
                if (flags & goblin::elf::program_header::PF_W) != 0 {
                    page_flags |= PageTableFlags::WRITABLE;
                }
                if (flags & goblin::elf::program_header::PF_X) == 0 {
                    page_flags |= PageTableFlags::NO_EXECUTE;
                }
                let old_frame = unsafe { user_leaf_info(root_frame.start_address(), page_vaddr).0 };
                if old_frame != 0 {
                    replaced_frames.push(old_frame);
                    let replaced = unsafe {
                        replace_user_leaf_mapping(
                            root_frame.start_address(),
                            page_vaddr,
                            frame.start_address(),
                            page_flags,
                        )
                    };
                    if !replaced {
                        return Err(ENOMEM);
                    }
                    unsafe {
                        core::arch::asm!(
                            "invlpg [{}]",
                            in(reg) page_vaddr,
                            options(nostack, preserves_flags)
                        );
                    }
                } else if PageTableHelper::map_page(
                    page_table,
                    page_vaddr as usize,
                    frame.start_address().as_u64() as usize,
                    page_flags,
                    frame_alloc,
                )
                .is_err()
                {
                    return Err(ENOMEM);
                }

                // Copy segment data to the newly allocated frame.
                let frame_vaddr = petroleum::common::memory::physical_to_virtual(
                    frame.start_address().as_u64() as usize,
                );
                unsafe {
                    core::ptr::write_bytes(frame_vaddr as *mut u8, 0, 4096);
                }
                let copy_start = page_vaddr.max(vaddr);
                let copy_end = (page_vaddr + 4096).min(file_end);
                if copy_start < copy_end {
                    let src_offset = file_off
                        .checked_add((copy_start - vaddr) as usize)
                        .ok_or(ENOEXEC)?;
                    let copy_len = (copy_end - copy_start) as usize;
                    if src_offset.checked_add(copy_len).ok_or(ENOEXEC)? > data.len() {
                        return Err(ENOEXEC);
                    }
                    let destination_offset = (copy_start - page_vaddr) as usize;
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            data[src_offset..src_offset + copy_len].as_ptr(),
                            (frame_vaddr as *mut u8).add(destination_offset),
                            copy_len,
                        );
                    }
                }
                page_vaddr += 4096;
            }
        }
        Ok(())
    });
    if mapped != Some(Ok(())) {
        log::warn!(
            "[EXEC-DIAG] segment map failed path={} result={:?}",
            path,
            mapped
        );
        crate::klog_fmt!(
            "[LINUX-DIAG] execve segment map failed path={} result={:?}\n",
            path,
            mapped
        );
        release_private_frames(&replaced_frames);
        process::terminate_process(current_pid, -1);
        return 0;
    }
    release_private_frames(&replaced_frames);

    // ── Allocate a stack ──────────────────────────────────
    let stack_size = crate::loader::LINUX_STACK_SIZE;
    let stack_top_vaddr_default = crate::loader::LINUX_STACK_TOP;
    let stack_guard: u64 = 4096; // guard page
    let stack_base = stack_top_vaddr_default - stack_size - stack_guard;

    if let Err(error) = make_user_range_private(stack_base + stack_guard, stack_top_vaddr_default) {
        log::warn!(
            "[EXEC-DIAG] stack privacy failed path={} error={}",
            path,
            error
        );
        crate::klog_fmt!(
            "[LINUX-DIAG] execve stack privacy failed path={} error={}\n",
            path,
            error
        );
        process::terminate_process(current_pid, -1);
        return 0;
    }

    if let Err(error) =
        zero_existing_writable_user_pages(stack_base + stack_guard, stack_top_vaddr_default)
    {
        log::warn!("[EXEC-DIAG] stack map failed path={} error={}", path, error);
        crate::klog_fmt!(
            "[LINUX-DIAG] execve stack map failed path={} error={}\n",
            path,
            error
        );
        process::terminate_process(current_pid, -1);
        return 0;
    }

    // ── Reset process state ───────────────────────────────
    let rsp = match initialize_exec_stack(stack_top_vaddr_default, &argv, &envp) {
        Ok(rsp) => rsp,
        Err(error) => {
            let _ = error;
            process::terminate_process(current_pid, -1);
            return 0;
        }
    };

    process::SCHEDULER.with_process(current_pid, |p| {
        p.entry_point = x86_64::VirtAddr::new(entry);
        p.user_stack = x86_64::VirtAddr::new(stack_top_vaddr_default);

        // Reset context for the new binary
        p.context.registers = crate::process::GeneralRegisters::default();
        p.context.rip = entry;
        p.context.kernel_rsp = 0;

        if p.is_user {
            p.context.segments.cs = crate::gdt::user_code()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(1);
            p.context.segments.ss = crate::gdt::user_data()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(2);
        }

        // Return 0 from execve on the new stack by pushing it
        // Actually, execve doesn't return on success - the new program starts at entry.
        // So we set up the stack so that the new program's _start function
        // receives argc, argv, envp in the standard Linux convention.
        //
        // Stack layout upon _start entry:
        //   [top of stack] = argc
        //   [argc + 8]     = argv[0], argv[1], ..., NULL
        //   [after argv]   = envp[0], envp[1], ..., NULL
        //   [after envp]   = auxiliary vector (AT_NULL terminated)

        p.context.registers.rsp = rsp;

        // Reset runtime state
        rt.program_break = rt.initial_break;
        rt.tls_ptr = 0;
        rt.signal_pending = 0;

        log::info!(
            "execve: loaded {} entry=0x{:x} stack=0x{:x}",
            path,
            entry,
            stack_top_vaddr_default
        );
    });

    unsafe {
        x86_64::registers::model_specific::Msr::new(0xC000_0100).write(0);
    }
    crate::interrupts::syscall::override_user_return_context(entry, rsp, 0x202);

    0
}

pub fn sys_wait4(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let pid = args[0] as i64;
    let status = args[1];
    let options = args[2] as i32;
    let _rusage = args[3];

    let target_pid = if pid <= 0 {
        // Wait for any child. A blocking context switch resumes inside this
        // syscall continuation, so re-scan after wakeup instead of returning
        // zero and making the shell lose the completed child status.
        loop {
            let current_pid = process::current_pid().unwrap_or(ProcessId(0));
            let mut found = None;
            let mut has_child = false;
            process::SCHEDULER.with_list(|list| {
                for (id, p) in list.iter() {
                    if p.parent_id != Some(current_pid) {
                        continue;
                    }
                    has_child = true;
                    if p.state == process::ProcessState::Terminated {
                        found = Some(*id);
                        break;
                    }
                }
            });
            if let Some(id) = found {
                break id;
            }
            if !has_child {
                return errno_code(ECHILD);
            }
            if (options & WNOHANG) != 0 {
                return 0;
            }
            process::block_current();
        }
    } else {
        ProcessId(pid as u64)
    };

    // Get the exit code
    let exit_code = process::SCHEDULER
        .with_process(target_pid, |p| p.exit_code)
        .flatten()
        .unwrap_or(0);

    // Write status
    if status != 0 {
        // Encode exit status in the format wait4 expects:
        // WIFEXITED = true, WEXITSTATUS = exit_code
        let status_val: i32 = (exit_code & 0xff) << 8;
        let _ = unsafe { copy_val_to_user(status, &status_val) };
    }

    // Reap through the scheduler owner path. Direct list mutation here would
    // invalidate the round-robin index while a syscall continuation is
    // suspended on another process's kernel stack.
    process::SCHEDULER.cleanup();

    target_pid.0
}

pub fn sys_kill(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    0 // No-op for now
}

pub fn sys_tkill(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    0
}

pub fn sys_tgkill(_rt: &mut LinuxRuntime, _args: &[u64; 6]) -> u64 {
    0
}
