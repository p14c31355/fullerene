use x86_64::{VirtAddr};
use x86_64::structures::paging::{PhysFrame, Mapper, OffsetPageTable, PageTableFlags, PageTable};
use crate::page_table::constants::BootInfoFrameAllocator;


#[repr(C)]
pub struct TransitionArgs {
    pub load_gdt: *const (),
    pub load_idt: *const (),
    pub phys_offset: u64,
    pub l4_frame: u64,
    pub allocator: *mut BootInfoFrameAllocator,
    pub kernel_entry: usize,
    pub kernel_args: *const KernelArgs,
}

#[repr(C)]
pub struct KernelArgs {
    pub handle: usize,
    pub system_table: usize,
    pub map_ptr: usize,
    pub map_size: usize,
    pub descriptor_size: usize,
    pub kernel_phys_start: u64,
    pub kernel_entry: usize,
    // Graphics information passed from bootloader
    pub fb_address: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_bpp: u32,
}

#[unsafe(no_mangle)]
pub static mut KERNEL_ARGS: *const KernelArgs = core::ptr::null();

#[unsafe(no_mangle)]
pub static mut TRANSITION_KERNEL_ENTRY: usize = 0;

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub(crate) struct GdtEntry {
    pub limit_low: u16,
    pub base_low: u16,
    pub base_mid: u8,
    pub access: u8,
    pub flags: u8,
    pub base_high: u8,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub(crate) struct GdtDescriptor {
    pub limit: u16,
    pub base: u64,
}

#[repr(C, packed)]
pub(crate) struct TransitionGdt {
    pub descriptor: GdtDescriptor,
    pub entries: [GdtEntry; 3],
}

pub static mut TRANSITION_GDT: TransitionGdt = TransitionGdt {
    descriptor: GdtDescriptor {
        limit: (core::mem::size_of::<[GdtEntry; 3]>() - 1) as u16,
        base: 0,
    },
    entries: [
        GdtEntry { limit_low: 0, base_low: 0, base_mid: 0, access: 0, flags: 0, base_high: 0 },
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x9A,
            flags: 0xAF,
            base_high: 0,
        },
        GdtEntry {
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x92,
            flags: 0,
            base_high: 0,
        },
    ],
};

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn landing_zone(
    _load_gdt: Option<fn()>,
    _load_idt: Option<fn()>,
    _phys_offset: VirtAddr,
    _level_4_table_frame: PhysFrame,
    _frame_allocator: *mut BootInfoFrameAllocator,
    _logic_fn_high: usize,
    _kernel_entry: usize,
) {
    core::arch::naked_asm!(
        "mov rax, 0x4c4d4e58", // 'LMNX'
        "mov dx, 0x3f8",
        "out dx, al",

        // Create TransitionArgs on stack
        // Current stack: [rsp] = _kernel_entry, [rsp+8] = _kernel_args
        // We need to move the stack pointer to make room for TransitionArgs (7 * 8 = 56 bytes)
        "sub rsp, 56",
        
        // Store arguments into the struct at [rsp]
        "mov [rsp], rdi",             // load_gdt
        "mov [rsp + 8], rsi",         // load_idt
        "mov [rsp + 16], rdx",        // phys_offset
        "mov [rsp + 24], rcx",        // l4_frame
        "mov [rsp + 32], r8",         // allocator
        "mov rax, [rsp + 56]",        // kernel_entry (from original [rsp])
        "mov [rsp + 40], rax",
        "mov rax, [rsp + 64]",        // kernel_args (from original [rsp+8])
        "mov [rsp + 48], rax",
        
        "mov rdi, rsp",               // Pass pointer to TransitionArgs as first argument
        "mov r11, r9",                // Save _logic_fn_high in r11
        "jmp r11",                    // Jump to _logic_fn_high
    );
}

#[unsafe(no_mangle)]
#[inline(never)]
pub unsafe extern "sysv64" fn landing_zone_logic(
    ctx: *const TransitionArgs,
) {
    unsafe {
        let args = &*ctx;
        
        let actual_kernel_entry = if args.kernel_entry == 0 {
            crate::page_table::mapper::transition::TRANSITION_KERNEL_ENTRY
        } else {
            args.kernel_entry
        };
        
        let actual_kernel_args = if args.kernel_args.is_null() {
            crate::page_table::mapper::transition::KERNEL_ARGS
        } else {
            args.kernel_args
        };
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: Start\n");

        if !args.load_idt.is_null() {
            let load_idt: fn() = core::mem::transmute(args.load_idt);
            load_idt();
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: IDT Loaded\n");
        }

        if !args.load_gdt.is_null() {
            let load_gdt: fn() = core::mem::transmute(args.load_gdt);
            load_gdt();
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: GDT Loaded\n");
        }

        let l4_phys = args.l4_frame;
        let sign_extended_offset = if (args.phys_offset & (1 << 47)) != 0 {
            args.phys_offset | 0xFFFF_0000_0000_0000
        } else {
            args.phys_offset & 0x0000_FFFF_FFFF_FFFF
        };
        let local_phys_offset = VirtAddr::new(sign_extended_offset);
        let local_frame_allocator = args.allocator;

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"High-half transition: landing zone logic reached!\n");
        crate::flush_tlb_and_verify!();

        let l4_virt_raw = args.phys_offset.wrapping_add(l4_phys);
        let l4_virt_sign_extended = if (l4_virt_raw & (1 << 47)) != 0 {
            l4_virt_raw | 0xFFFF_0000_0000_0000
        } else {
            l4_virt_raw & 0x0000_FFFF_FFFF_FFFF
        };
        let l4_virt = VirtAddr::new(l4_virt_sign_extended);

        // Manual mapping helper to avoid OffsetPageTable overflow panics
        let mut map_page_raw = |v_addr_raw: u64, p_addr_raw: u64, flags: PageTableFlags| {
            let v_addr_sign_extended = if (v_addr_raw & (1 << 47)) != 0 {
                v_addr_raw | 0xFFFF_0000_0000_0000
            } else {
                v_addr_raw & 0x0000_FFFF_FFFF_FFFF
            };
            let p_addr = p_addr_raw & 0x000F_FFFF_FFFF_FFFF;
            
            // Use a temporary mapper with 0 offset to avoid overflow in internal calculations
            let mut temp_mapper = OffsetPageTable::new(
                &mut *(l4_virt.as_mut_ptr() as *mut PageTable),
                VirtAddr::new(0),
            );
            let _ = temp_mapper.map_to(
                x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(v_addr_sign_extended)),
                x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(p_addr)),
                flags,
                &mut *local_frame_allocator,
            );
        };

        // Map L4 table to itself
        map_page_raw(l4_virt_raw, l4_phys, PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE);
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Landing zone jumping to kernel entry!\n");
        
        if actual_kernel_entry == 0 {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"ERROR: actual_kernel_entry is 0! Hanging...\n");
            loop { core::hint::spin_loop(); }
        }

        // DEBUG: Print values before calculation
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: kernel_entry check [VERSION_20260502_03]\n");
            
             // Map KernelArgs first so we can read it
             let args_phys = actual_kernel_args as u64;
             let args_phys_raw = args_phys.wrapping_sub(local_phys_offset.as_u64());
             map_page_raw(args_phys, args_phys_raw, PageTableFlags::PRESENT | PageTableFlags::WRITABLE);

             let k_args = &*actual_kernel_args;

             let mut buf = [0u8; 16];
             crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: KernelArgs content:\n");
             
             crate::write_serial_bytes!(0x3F8, 0x3FD, b"  handle: 0x");
             let len = crate::serial::format_hex_to_buffer(k_args.handle as u64, &mut buf, 16);
             crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
             crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

             crate::write_serial_bytes!(0x3F8, 0x3FD, b"  st: 0x");
             let len = crate::serial::format_hex_to_buffer(k_args.system_table as u64, &mut buf, 16);
             crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
             crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

             crate::write_serial_bytes!(0x3F8, 0x3FD, b"  map: 0x");
             let len = crate::serial::format_hex_to_buffer(k_args.map_ptr as u64, &mut buf, 16);
             crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
             crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

             crate::write_serial_bytes!(0x3F8, 0x3FD, b"  size: 0x");
             let len = crate::serial::format_hex_to_buffer(k_args.map_size as u64, &mut buf, 16);
             crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
             crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

             crate::write_serial_bytes!(0x3F8, 0x3FD, b"  desc_size: 0x");
             let len = crate::serial::format_hex_to_buffer(k_args.descriptor_size as u64, &mut buf, 16);
             crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
             crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
             let kernel_phys_start = k_args.kernel_phys_start;
             let kernel_virt_start = kernel_phys_start.wrapping_add(local_phys_offset.as_u64());

             // Also explicitly map the memory map buffer
             let map_phys = k_args.map_ptr as u64;
             let map_virt = map_phys.wrapping_add(local_phys_offset.as_u64());
             let map_pages = (k_args.map_size as u64 + 4095) / 4096;
             for i in 0..map_pages {
                 map_page_raw(
                     map_virt.wrapping_add(i * 4096),
                     map_phys.wrapping_add(i * 4096),
                     PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                 );
             }
            
            // Map a larger kernel region (512MB to be safe) to ensure all sections and functions are covered
            for page_offset in 0..131072 {
                let v_page_raw = kernel_virt_start.wrapping_add(page_offset * 4096);
                let p_page = kernel_phys_start.wrapping_add(page_offset * 4096);
                map_page_raw(v_page_raw, p_page, PageTableFlags::PRESENT | PageTableFlags::WRITABLE);
            }

            // Also explicitly map the page containing the actual kernel entry and its surroundings
            let entry_page_start = (actual_kernel_entry as u64) & !0xFFF;
            for page_offset in -16i32..16i32 {
                let v_page = entry_page_start.wrapping_add((page_offset as i64 * 4096) as u64);
                let p_page = v_page.wrapping_sub(local_phys_offset.as_u64());
                map_page_raw(v_page, p_page, PageTableFlags::PRESENT | PageTableFlags::WRITABLE);
            }

            // Also explicitly map the stack area around the actual RSP to avoid PF during push/retfq
            let rsp_val: u64;
            core::arch::asm!("mov {}, rsp", out(reg) rsp_val);
            let stack_page_start = rsp_val & !0xFFF;
            // Map 16MB around the current RSP (8MB below and 8MB above)
            for page_offset in 0..4096 {
                let v_page = stack_page_start.wrapping_sub(8 * 1024 * 1024).wrapping_add(page_offset * 4096);
                let p_page = v_page.wrapping_sub(local_phys_offset.as_u64());
                map_page_raw(v_page, p_page, PageTableFlags::PRESENT | PageTableFlags::WRITABLE);
            }

            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: Jumping now ('Z')\n");
            
            // DEBUG: Print final state before jump
            let mut buf = [0u8; 16];
            let entry_val = actual_kernel_entry as u64;
            let len = crate::serial::format_hex_to_buffer(entry_val, &mut buf, 16);
            crate::write_serial_bytes(0x3F8, 0x3FD, b"  entry: 0x");
            crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            
            let rsp_val: u64;
            core::arch::asm!("mov {}, rsp", out(reg) rsp_val);
            let len = crate::serial::format_hex_to_buffer(rsp_val, &mut buf, 16);
            crate::write_serial_bytes(0x3F8, 0x3FD, b"  rsp: 0x");
            crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

             // Calculate arguments and use black_box to prevent "base + offset" optimization
             let h = core::hint::black_box(unsafe { (*actual_kernel_args).handle }.wrapping_add(local_phys_offset.as_u64() as usize));
             let s = core::hint::black_box(unsafe { (*actual_kernel_args).system_table }.wrapping_add(local_phys_offset.as_u64() as usize));
             let m = core::hint::black_box(unsafe { (*actual_kernel_args).map_ptr }.wrapping_add(local_phys_offset.as_u64() as usize));
             let sz = core::hint::black_box(unsafe { (*actual_kernel_args).map_size });

             core::arch::asm!(
                 "cli",
                 "mov ax, 0x10",
                 "mov ds, ax",
                 "mov es, ax",
                 "mov fs, ax",
                 "mov gs, ax",
                 "mov ss, ax",
                 "and rsp, -16",
                 
                 // Save all inputs to stack to completely eliminate register collisions
                 "push {h}",
                 "push {s}",
                 "push {m}",
                 "push {sz}",
                 "push {entry}",
                 
                 // Load from stack into ABI registers
                 // Stack layout: [rsp]=entry, [rsp+8]=sz, [rsp+16]=m, [rsp+24]=s, [rsp+32]=h
                 "mov rcx, [rsp + 32]", // h
                 "mov rdx, [rsp + 24]", // s
                 "mov r8, [rsp + 16]",  // m
                 "mov r9, [rsp + 8]",   // sz
                 
                 // Pass KernelArgs pointer as the first argument (RDI)
                 "mov rdi, {args}",
                 
                 // Pop entry into a scratch register and clean up the rest
                 "pop r11",            // r11 = entry
                 "add rsp, 32",        // clean h, s, m, sz
                 
                 // Now push CS and RIP for retfq
                 "push 0x08",
                 "push r11",           // entry
                 "retfq", 
                 h = in(reg) h,
                 s = in(reg) s,
                 m = in(reg) m,
                 sz = in(reg) sz,
                 args = in(reg) actual_kernel_args,
                 entry = in(reg) actual_kernel_entry,
                 options(noreturn)
             );
    }
}

#[repr(C)]
pub struct TransitionContext {
    pub cr3: u64,
    pub load_gdt: *const (),
    pub load_idt: *const (),
    pub phys_offset: u64,
    pub l4_frame: u64,
    pub allocator: *const BootInfoFrameAllocator,
    pub logic_fn_high: usize,
    pub kernel_entry: usize,
    pub kernel_args_virt: u64,
    pub cs_selector: u64,
    pub landing_zone_high: usize,
    pub offset_diff: u64,
    pub gdt_ptr: *const u8,
}

impl TransitionContext {
    pub fn prepare(
        phys_offset: VirtAddr,
        current_physical_memory_offset: VirtAddr,
        level_4_table_frame: PhysFrame,
        frame_allocator: &mut BootInfoFrameAllocator,
        load_gdt: Option<fn()>,
        load_idt: Option<fn()>,
        gdt_ptr: Option<*const u8>,
        kernel_entry: Option<usize>,
        kernel_args_phys: Option<u64>,
    ) -> Self {
        let current_offset = current_physical_memory_offset.as_u64();
        let target_offset = phys_offset.as_u64();
        let offset_diff = target_offset.wrapping_sub(current_offset);

        let lz_addr = landing_zone as *const () as u64;
        let lzl_addr = landing_zone_logic as *const () as u64;

        crate::debug_log_no_alloc!("TransitionContext::prepare - current_offset: 0x{:x}, target_offset: 0x{:x}, offset_diff: 0x{:x}", current_offset, target_offset, offset_diff);
        crate::debug_log_no_alloc!("TransitionContext::prepare - landing_zone: 0x{:x}, landing_zone_logic: 0x{:x}", lz_addr, lzl_addr);

        unsafe {
            let gdt_ptr_static = core::ptr::addr_of_mut!(TRANSITION_GDT);
            let entries_virt_addr = core::ptr::addr_of!((*gdt_ptr_static).entries) as *const _ as u64;
            let gdt_phys_base = entries_virt_addr.wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
            (*gdt_ptr_static).descriptor.base = gdt_phys_base;
        }

        let final_gdt_ptr_virt = gdt_ptr.unwrap_or(unsafe {
            core::ptr::addr_of!((*core::ptr::addr_of!(TRANSITION_GDT)).descriptor) as *const _ as *const u8
        });
        let final_gdt_ptr_high = (((final_gdt_ptr_virt as u64)
                .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF)
                .wrapping_add(target_offset)) as *const u8;

        let l_idt = load_idt.map_or(core::ptr::null(), |f| f as *const ());

        let final_kernel_entry = kernel_entry.map_or(0, |entry| {
            (entry as u64).wrapping_add(target_offset) as usize
        });

        let logic_fn_phys = (landing_zone_logic as *const () as u64)
            .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
        let logic_fn_high = logic_fn_phys.wrapping_add(target_offset);

        let landing_zone_phys = (landing_zone as *const () as u64)
            .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
        let landing_zone_high = landing_zone_phys.wrapping_add(target_offset);

        // DEBUG: Print calculated addresses to verify canonicality and correctness
        crate::debug_log_no_alloc!("Calculated landing_zone_high: 0x{:x}", landing_zone_high);
        crate::debug_log_no_alloc!("Calculated logic_fn_high: 0x{:x}", logic_fn_high);

        // Verify canonicality to catch calculation errors early
        let _ = VirtAddr::new(logic_fn_high);
        let _ = VirtAddr::new(landing_zone_high);

        Self {
            cr3: level_4_table_frame.start_address().as_u64(),
            load_gdt: load_gdt.map_or(core::ptr::null(), |f| f as *const ()),
            load_idt: l_idt,
            phys_offset: target_offset,
            l4_frame: level_4_table_frame.start_address().as_u64(),
            allocator: frame_allocator as *const _,
            logic_fn_high: logic_fn_high as usize,
            kernel_entry: final_kernel_entry,
            kernel_args_virt: kernel_args_phys.map_or(0, |phys| phys + target_offset),
            cs_selector: 0x08,
            landing_zone_high: landing_zone_high as usize,
            offset_diff,
            gdt_ptr: final_gdt_ptr_high,
        }
    }
}

#[inline(never)]
pub fn perform_world_switch(ctx: TransitionContext) -> ! {
    unsafe {
        TRANSITION_KERNEL_ENTRY = ctx.kernel_entry;
        KERNEL_ARGS = ctx.kernel_args_virt as *const KernelArgs;

        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(ctx.cr3)),
            x86_64::registers::control::Cr3Flags::empty(),
        );

        core::arch::asm!("lgdt [{}]", in(reg) ctx.gdt_ptr);

        core::arch::asm!(
            "add rsp, {offset_diff}",
            "and rsp, -16",
            "mov [rsp], {load_gdt}",
            "mov [rsp + 8], {load_idt}",
            "mov [rsp + 16], {phys_offset}",
            "mov [rsp + 24], {l4_frame}",
            "mov [rsp + 32], {allocator}",
            "mov [rsp + 40], {kernel_entry}",
            "mov [rsp + 48], {kernel_args}",
            "mov rdi, rsp",
            "push 0x08",
            "push {logic_fn_high}",
            "retfq",
            load_gdt = in(reg) ctx.load_gdt,
            load_idt = in(reg) ctx.load_idt,
            phys_offset = in(reg) ctx.phys_offset,
            l4_frame = in(reg) ctx.l4_frame,
            allocator = in(reg) ctx.allocator,
            kernel_entry = in(reg) ctx.kernel_entry,
            kernel_args = in(reg) ctx.kernel_args_virt,
            offset_diff = in(reg) ctx.offset_diff,
            logic_fn_high = in(reg) ctx.logic_fn_high,
            options(noreturn)
        );
        core::hint::unreachable_unchecked()
    }
}