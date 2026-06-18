// Linux process syscall implementations
extern crate alloc;
use super::runtime::{LinuxRuntime, Runtime, copy_user_string, copy_val_to_user, errno_code};
use super::numbers::*;
use crate::process::{self, ProcessId, ProcessContext};
use alloc::boxed::Box;
use alloc::vec::Vec;
use petroleum::page_table::types::PageTableHelper;
use x86_64::PhysAddr;
use x86_64::structures::paging::{FrameAllocator as X86FrameAllocator, PageTableFlags, Size4KiB};

pub fn sys_exit(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let code = args[0] as i32;
    // Clear child TID if set
    if rt.child_clear_tid != 0 {
        unsafe { core::ptr::write_volatile(rt.child_clear_tid as *mut i32, 0) };
    }
    if let Some(pid) = process::current_pid() {
        process::terminate_process(pid, code);
    }
    loop { x86_64::instructions::hlt() }
}

pub fn sys_exit_group(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    sys_exit(rt, args)
}

pub fn sys_getpid(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    process::current_pid().map(|pid| pid.0).unwrap_or(0)
}

pub fn sys_getppid(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let pid = process::current_pid().unwrap_or(ProcessId(0));
    process::PROCESS_MANAGER
        .with_process(pid, |p| p.parent_id.map(|id| id.0).unwrap_or(0))
        .unwrap_or(0)
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

    // TODO: Real clone implementation with proper VM sharing
    // For now, fork as a simple process creation
    let current_pid = match process::current_pid() {
        Some(p) => p,
        None => return errno_code(ESRCH),
    };

    // Get parent info
    let (parent_pt, parent_ctx) = process::PROCESS_MANAGER
        .with_process(current_pid, |p| {
            (p.page_table_phys_addr, p.context.clone())
        })
        .unwrap_or((PhysAddr::new(0), Box::new(ProcessContext::default())));

    // Clone page table
    let cloned_table = {
        let mut mgr_guard = crate::memory_management::get_memory_manager().lock();
        let mgr = match mgr_guard.as_mut() {
            Some(m) => m,
            None => return errno_code(ENOMEM),
        };
        let alloc = petroleum::page_table::constants::get_frame_allocator_mut();
        match mgr.clone_page_table(
            parent_pt.as_u64() as usize,
            alloc,
        ) {
            Ok(addr) => addr,
            Err(_) => return errno_code(ENOMEM),
        }
    };

    let cloned_frame = x86_64::structures::paging::PhysFrame::containing_address(
        x86_64::PhysAddr::new(cloned_table as u64),
    );

    let mut child_pt = petroleum::page_table::process::ProcessPageTable::new_with_frame(cloned_frame);
    let _ = petroleum::initializer::Initializable::init(&mut child_pt);

    // Allocate kernel stack
    let stack_layout = core::alloc::Layout::from_size_align(4096, 16).unwrap();
    let stack_ptr = petroleum::common::memory::allocate_layout(stack_layout).unwrap_or(core::ptr::null_mut());
    if stack_ptr.is_null() {
        return errno_code(ENOMEM);
    }
    let kernel_stack_top = x86_64::VirtAddr::new(stack_ptr as u64 + 4096);

    let child_pid = process::PROCESS_MANAGER.allocate_pid();

    let mut child_process = process::Process {
        id: child_pid,
        name: "linux-child",
        state: process::ProcessState::Ready,
        context: {
            let mut ctx = parent_ctx.clone();
            // Child returns 0 from clone
            ctx.regs[0] = 0;
            ctx
        },
        page_table_phys_addr: x86_64::PhysAddr::new(cloned_table as u64),
        page_table: Some(alloc::boxed::Box::new(child_pt)),
        kernel_stack: kernel_stack_top,
        user_stack: x86_64::VirtAddr::new(0),
        entry_point: x86_64::VirtAddr::new(0),
        is_user: true,
        exit_code: None,
        parent_id: Some(current_pid),
        task_data: 0,
        dispatch_mode: None,
    };

    let child_box = alloc::boxed::Box::new(child_process);
    if process::PROCESS_MANAGER.add(child_box).is_err() {
        return errno_code(ENOMEM);
    }

    child_pid.0
}

pub fn sys_fork(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    // fork() is clone(SIGCHLD, 0, NULL, NULL, 0)
    sys_clone(rt, &[SIGCHLD as u64, 0, 0, 0, 0, 0])
}

pub fn sys_execve(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let path_ptr = args[0];
    let argv_ptr = args[1];
    let _envp = args[2];

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
    let segments: Vec<(u64, usize, usize, usize, u32)> = elf.program_headers.iter()
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

    // ── Unmap old process memory ──────────────────────────
    // Clear the brk region
    if rt.program_break > rt.initial_break {
        let num_pages = ((rt.program_break - rt.initial_break + 4095) / 4096) as usize;
        if let Some(mgr) = crate::memory_management::get_memory_manager().lock().as_mut() {
            for i in 0..num_pages {
                let page_vaddr = (rt.initial_break + (i as u64) * 4096) as usize;
                if mgr.page_table_manager().translate_address(page_vaddr).is_ok() {
                    let _ = mgr.safe_unmap_page(page_vaddr);
                }
            }
        }
    }

    // ── Load and map new segments ─────────────────────────
    let frame_alloc = petroleum::page_table::constants::get_frame_allocator_mut();
    if let Some(mgr) = crate::memory_management::get_memory_manager().lock().as_mut() {
        for &(vaddr, file_off, file_sz, mem_sz, flags) in &segments {
            let num_pages = ((mem_sz + 4095) / 4096) as usize;
            for page_idx in 0..num_pages {
                let page_vaddr = (vaddr + (page_idx as u64) * 4096) as usize;
                if let Some(frame) = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) {
                    let mut page_flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
                    if (flags & goblin::elf::program_header::PF_W) != 0 {
                        page_flags |= PageTableFlags::WRITABLE;
                    }
                    if (flags & goblin::elf::program_header::PF_X) == 0 {
                        page_flags |= PageTableFlags::NO_EXECUTE;
                    }
                    let _ = mgr.safe_map_page(
                        page_vaddr,
                        frame.start_address().as_u64() as usize,
                        page_flags,
                    );

                    // Copy segment data to the newly allocated frame
                    let frame_vaddr = petroleum::common::memory::physical_to_virtual(frame.start_address().as_u64() as usize);
                    let page_offset = page_idx * 4096;
                    if page_offset < file_sz {
                        let copy_len = (file_sz - page_offset).min(4096);
                        let src_offset = file_off + page_offset;
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
                        // Zero-fill BSS page
                        unsafe {
                            core::ptr::write_bytes(frame_vaddr as *mut u8, 0, 4096);
                        }
                    }
                }
            }
        }
    }

    // ── Allocate a stack ──────────────────────────────────
    let stack_size: u64 = 2 * 1024 * 1024; // 2MB stack
    let stack_top_vaddr_default: u64 = 0x7ffffffff000;
    let stack_guard: u64 = 4096; // guard page
    let stack_base = stack_top_vaddr_default - stack_size - stack_guard;

    let frame_alloc = petroleum::page_table::constants::get_frame_allocator_mut();
    if let Some(mgr) = crate::memory_management::get_memory_manager().lock().as_mut() {
        for i in 0..(stack_size / 4096) as usize {
            let page_vaddr = (stack_base + stack_guard + (i as u64) * 4096) as usize;
            if let Some(frame) = X86FrameAllocator::<Size4KiB>::allocate_frame(frame_alloc) {
                let page_flags = PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE
                    | PageTableFlags::NO_EXECUTE;
                let _ = mgr.safe_map_page(
                    page_vaddr,
                    frame.start_address().as_u64() as usize,
                    page_flags,
                );
            }
        }
    }

    // ── Reset process state ───────────────────────────────
    let current_pid = process::current_pid().unwrap_or(ProcessId(0));

    process::PROCESS_MANAGER.with_process(current_pid, |p| {
        p.entry_point = x86_64::VirtAddr::new(entry);
        p.user_stack = x86_64::VirtAddr::new(stack_top_vaddr_default);

        // Reset context for the new binary
        p.context.rip = entry;
        p.context.regs[7] = stack_top_vaddr_default; // RSP

        if p.is_user {
            p.context.segments[0] = crate::gdt::user_code_selector_fallback()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(1);
            p.context.segments[1] = crate::gdt::user_data_selector_fallback()
                .as_ref()
                .map(|s| s.0 as u64)
                .unwrap_or(2);
        }

        // Clear registers
        for reg in &mut p.context.regs {
            *reg = 0;
        }
        p.context.regs[7] = stack_top_vaddr_default; // RSP

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

        // Set RSP to stack top with argc=1, argv[0]=path, NULL, envp=NULL
        let stack_top = stack_top_vaddr_default;
        let argc: u64 = 1;

        // Write to the new stack (it's mapped in the process address space,
        // and we're in kernel mode with the kernel page table. We need to
        // write through the process's page table).
        // For now, just set up a minimal stack.
        p.context.regs[7] = stack_top - 16; // push two zeros as envp[0]=NULL then argv[1]=NULL

        // Reset runtime state
        rt.program_break = rt.initial_break;
        rt.tls_ptr = 0;
        rt.signal_pending = 0;

        log::info!("execve: loaded {} entry=0x{:x} stack=0x{:x}", path, entry, stack_top_vaddr_default);
    });

    0
}

pub fn sys_wait4(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let pid = args[0] as i64;
    let status = args[1];
    let options = args[2] as i32;
    let _rusage = args[3];

    let target_pid = if pid <= 0 {
        // Wait for any child
        let current_pid = process::current_pid().unwrap_or(ProcessId(0));
        let mut found = None;
        process::PROCESS_MANAGER.with_list(|list| {
            for (id, p) in list.iter() {
                if p.parent_id == Some(current_pid) && p.state == process::ProcessState::Terminated {
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
    let exit_code = process::PROCESS_MANAGER
        .with_process(target_pid, |p| p.exit_code)
        .flatten()
        .unwrap_or(0);

    // Write status
    if status != 0 {
        // Encode exit status in the format wait4 expects:
        // WIFEXITED = true, WEXITSTATUS = exit_code
        unsafe { core::ptr::write_volatile(status as *mut i32, (exit_code & 0xff) << 8) };
    }

    // Remove the child process
    process::PROCESS_MANAGER.with_list(|list| {
        list.retain(|(id, _)| *id != target_pid);
    });

    target_pid.0
}

pub fn sys_kill(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _pid = args[0] as i32;
    let _sig = args[1] as i32;
    0 // No-op for now
}

pub fn sys_tkill(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _tid = args[0] as i32;
    let _sig = args[1] as i32;
    0
}

pub fn sys_tgkill(rt: &mut LinuxRuntime, args: &[u64; 6]) -> u64 {
    let _tgid = args[0] as i32;
    let _tid = args[1] as i32;
    let _sig = args[2] as i32;
    0
}
