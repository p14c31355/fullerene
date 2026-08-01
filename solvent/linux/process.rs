// Linux process syscall implementations
extern crate alloc;
use super::numbers::*;
use super::runtime::{
    LinuxRuntime, copy_to_user, copy_user_string, copy_val_from_user, copy_val_to_user, errno_code,
};
use crate::process::{self, ProcessContext, ProcessId};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use petroleum::page_table::FrameAllocatorExt;
use petroleum::page_table::types::PageTableHelper;
use x86_64::PhysAddr;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator as X86FrameAllocator, OffsetPageTable, PageTable, PageTableFlags, Size4KiB,
};

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
            entry.set_flags(flags | PageTableFlags::WRITABLE);
            (flags, entry.addr())
        };
        if flags.contains(PageTableFlags::HUGE_PAGE) || level == 3 {
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
    true
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

fn replace_user_range_with_zeroed_pages(start: u64, end: u64) -> Result<(), i32> {
    let offset =
        x86_64::VirtAddr::new(petroleum::common::memory::get_physical_memory_offset() as u64);
    let (root_frame, _) = Cr3::read();
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    let mut address = start;
    while address < end {
        let virt = x86_64::VirtAddr::new(address);
        let frame = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc).ok_or(ENOMEM)?;
        unsafe {
            core::ptr::write_bytes(
                petroleum::common::memory::physical_to_virtual(
                    frame.start_address().as_u64() as usize
                ) as *mut u8,
                0,
                4096,
            );

            let root =
                &mut *(offset + root_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
            let p3_frame = root[virt.p4_index()].frame().map_err(|_| ENOMEM)?;
            let p3 = &mut *(offset + p3_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
            let p2_frame = p3[virt.p3_index()].frame().map_err(|_| ENOMEM)?;
            let p2 = &mut *(offset + p2_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
            let p1_frame = p2[virt.p2_index()].frame().map_err(|_| ENOMEM)?;
            let p1 = &mut *(offset + p1_frame.start_address().as_u64()).as_mut_ptr::<PageTable>();
            p1[virt.p1_index()].set_addr(frame.start_address(), flags);
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

fn copy_user_vector(vector: u64) -> Result<Vec<alloc::string::String>, i32> {
    if vector == 0 {
        return Ok(Vec::new());
    }
    let mut values = Vec::new();
    for index in 0..64u64 {
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
        values.push(unsafe { copy_user_string(pointer, 4096) }?);
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
    let random = [
        0x6d, 0x3a, 0x91, 0x27, 0xc4, 0x58, 0xe2, 0x0f, 0x83, 0xb6, 0x44, 0x19, 0xfa, 0x72, 0x0c,
        0xd1,
    ];
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
        crate::linux::launch::observe_smoke_exit(pid, code);
        #[cfg(linux_busybox_smoke)]
        crate::linux::launch::observe_busybox_exit(pid, code);
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
    let (parent_pt, parent_ctx) = process::SCHEDULER
        .with_process(current_pid, |p| (p.page_table_phys_addr, p.context.clone()))
        .unwrap_or((PhysAddr::new(0), Box::new(ProcessContext::default())));

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
        name: "linux-child",
        state: process::ProcessState::Ready,
        context: {
            let mut ctx = parent_ctx.clone();
            // Child returns 0 from clone
            ctx.registers.rax = 0;
            ctx.kernel_rsp = 0;
            ctx
        },
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
            child_rt.fd_table.entries = rt.fd_table.entries.clone();
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
        Err(_) => return errno_code(ENOENT),
    };

    // Parse ELF with goblin
    let elf = match goblin::elf::Elf::parse(&data) {
        Ok(e) => e,
        Err(_) => return errno_code(ENOEXEC),
    };

    if elf.header.e_type != goblin::elf::header::ET_EXEC {
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
            return errno_code(error);
        }
    }

    let current_pid = process::current_pid().unwrap_or(ProcessId(0));

    // ── Unmap old process memory ──────────────────────────
    // Clear the brk region
    if rt.program_break > rt.initial_break {
        let num_pages = ((rt.program_break - rt.initial_break + 4095) / 4096) as usize;
        let _ = process::SCHEDULER.with_process(current_pid, |p| {
            let Some(page_table) = p.page_table.as_mut() else {
                return;
            };
            let page_table = &mut **page_table;
            for i in 0..num_pages {
                let page_vaddr = (rt.initial_break + (i as u64) * 4096) as usize;
                if PageTableHelper::translate_address(page_table, page_vaddr).is_ok() {
                    let _ = PageTableHelper::unmap_page(page_table, page_vaddr);
                }
            }
        });
    }

    // ── Load and map new segments ─────────────────────────
    let frame_alloc = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() };
    let mapped = process::SCHEDULER.with_process(current_pid, |p| {
        let Some(page_table) = p.page_table.as_mut() else {
            return Err(ENOMEM);
        };
        let page_table = &mut **page_table;
        for &(vaddr, file_off, file_sz, mem_sz, flags) in &segments {
            let num_pages = ((mem_sz + 4095) / 4096) as usize;
            for page_idx in 0..num_pages {
                let page_vaddr = (vaddr + (page_idx as u64) * 4096) as usize;
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
                if PageTableHelper::map_page(
                    page_table,
                    page_vaddr,
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
                let page_offset = page_idx * 4096;
                if page_offset < file_sz {
                    let copy_len = (file_sz - page_offset).min(4096);
                    let src_offset = file_off + page_offset;
                    if src_offset + copy_len > data.len() {
                        return Err(ENOEXEC);
                    }
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            data[src_offset..src_offset + copy_len].as_ptr(),
                            frame_vaddr as *mut u8,
                            copy_len,
                        );
                        if copy_len < 4096 {
                            core::ptr::write_bytes(
                                (frame_vaddr as *mut u8).add(copy_len),
                                0,
                                4096 - copy_len,
                            );
                        }
                    }
                } else {
                    unsafe {
                        core::ptr::write_bytes(frame_vaddr as *mut u8, 0, 4096);
                    }
                }
            }
        }
        Ok(())
    });
    if mapped != Some(Ok(())) {
        return errno_code(mapped.unwrap_or(Err(ENOMEM)).unwrap_err());
    }

    // ── Allocate a stack ──────────────────────────────────
    let stack_size: u64 = 2 * 1024 * 1024; // 2MB stack
    let stack_top_vaddr_default: u64 = 0x7ffffffff000;
    let stack_guard: u64 = 4096; // guard page
    let stack_base = stack_top_vaddr_default - stack_size - stack_guard;

    if let Err(error) = make_user_range_private(stack_base + stack_guard, stack_top_vaddr_default) {
        return errno_code(error);
    }

    if let Err(error) =
        replace_user_range_with_zeroed_pages(stack_base + stack_guard, stack_top_vaddr_default)
    {
        return errno_code(error);
    }

    // ── Reset process state ───────────────────────────────
    let rsp = match initialize_exec_stack(stack_top_vaddr_default, &argv, &envp) {
        Ok(rsp) => rsp,
        Err(error) => return errno_code(error),
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

    0
}

pub fn sys_wait4(_rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let pid = args[0] as i64;
    let status = args[1];
    let options = args[2] as i32;
    let _rusage = args[3];

    let target_pid = if pid <= 0 {
        // Wait for any child
        let current_pid = process::current_pid().unwrap_or(ProcessId(0));
        let mut found = None;
        process::SCHEDULER.with_list(|list| {
            for (id, p) in list.iter() {
                if p.parent_id == Some(current_pid) && p.state == process::ProcessState::Terminated
                {
                    found = Some(*id);
                    break;
                }
            }
        });
        match found {
            Some(id) => id,
            None => {
                if (options & WNOHANG) != 0 {
                    return 0; // No child exited yet
                }
                // Block waiting
                process::block_current();
                return 0;
            }
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

    // Reap through the scheduler's owner path. Directly retaining from the
    // Linux syscall would invalidate the round-robin index while a syscall
    // continuation is suspended on another process's kernel stack.
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
