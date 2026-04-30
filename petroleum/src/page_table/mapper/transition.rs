use x86_64::{VirtAddr};
use x86_64::structures::paging::{PhysFrame, Mapper, OffsetPageTable, PageTableFlags, PageTable};
use crate::page_table::constants::BootInfoFrameAllocator;

#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn landing_zone(
    _load_gdt: Option<fn()>,
    _load_idt: Option<fn()>,
    _phys_offset: VirtAddr,
    _level_4_table_frame: PhysFrame,
    _frame_allocator: *mut BootInfoFrameAllocator,
    _logic_fn_high: usize,
    _kernel_entry: usize,
) {
    // 1. Immediate生存確認
    crate::write_serial_bytes!(0x3F8, 0x3FD, b"LMNXW");

    // 2. Transition to logic
    landing_zone_logic(
        _load_gdt.map_or(core::ptr::null(), |f| f as *const ()),
        _load_idt.map_or(core::ptr::null(), |f| f as *const ()),
        _phys_offset.as_u64(),
        _level_4_table_frame.start_address().as_u64(),
        _frame_allocator,
        _kernel_entry,
        KERNEL_ARGS,
    );

    loop {
        core::hint::spin_loop();
    }
}

#[repr(C)]
pub struct KernelArgs {
    pub handle: usize,
    pub system_table: usize,
    pub map_ptr: usize,
    pub map_size: usize,
    pub kernel_phys_start: u64,
    pub kernel_entry: usize,
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
#[inline(never)]
pub unsafe extern "sysv64" fn landing_zone_logic(
    load_gdt: *const (),
    load_idt: *const (),
    phys_offset_raw: u64,
    l4_frame_raw: u64,
    frame_allocator: *mut BootInfoFrameAllocator,
    _kernel_entry: usize,
    kernel_args: *const KernelArgs,
) {
    unsafe {
        let actual_kernel_args = KERNEL_ARGS;
        let actual_kernel_entry = TRANSITION_KERNEL_ENTRY;
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: Start\n");

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: Skipping IDT load for debug\n");
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: IDT Load skipped\n");

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: Skipping GDT load for debug\n");
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: GDT Load skipped\n");

        let l4_phys = l4_frame_raw;
        let sign_extended_offset = if (phys_offset_raw & (1 << 47)) != 0 {
            phys_offset_raw | 0xFFFF_0000_0000_0000
        } else {
            phys_offset_raw & 0x0000_FFFF_FFFF_FFFF
        };
        let local_phys_offset = VirtAddr::new(sign_extended_offset);
        let local_frame_allocator = frame_allocator;

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"High-half transition: landing zone logic reached!\n");
        crate::flush_tlb_and_verify!();

        let l4_virt_raw = phys_offset_raw.wrapping_add(l4_phys);
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
        
        if actual_kernel_entry != 0 {
            // DEBUG: Print values before calculation
            // Since we can't easily print u64 with write_serial_bytes, we'll use a dummy loop or just a marker
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: kernel_entry check\n");
            
            let args = &*actual_kernel_args;
            let entry_page_virt = (actual_kernel_entry as u64) & !0xFFF;
            let entry_page_phys = entry_page_virt.wrapping_sub(local_phys_offset.as_u64());
            for page_offset in 0..1024 {
                let v_page_raw = entry_page_virt.wrapping_add(page_offset * 4096);
                let p_page = entry_page_phys.wrapping_add(page_offset * 4096);
                map_page_raw(v_page_raw, p_page, PageTableFlags::PRESENT);
            }

            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: Jumping now ('Z')\n");
            core::arch::asm!(
                "cli",
                "mov ax, 0x10",
                "mov ds, ax",
                "mov es, ax",
                "mov fs, ax",
                "mov gs, ax",
                "mov ss, ax",
                "and rsp, -16",
                "mov rdi, {handle}",
                "mov rsi, {st}",
                "mov rdx, {map}",
                "mov rcx, {size}",
                "mov r11, cr3",
                "mov cr3, r11",
                "push 0x08",
                "push {entry}",
                "retfq", 
                handle = in(reg) args.handle,
                st = in(reg) args.system_table,
                map = in(reg) args.map_ptr,
                size = in(reg) args.map_size,
                entry = in(reg) actual_kernel_entry,
                options(noreturn)
            );
        }
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
            let gdt_high_base = gdt_phys_base.wrapping_add(target_offset);
            (*gdt_ptr_static).descriptor.base = gdt_high_base;
        }

        let final_gdt_ptr_virt = gdt_ptr.unwrap_or(unsafe {
            core::ptr::addr_of!((*core::ptr::addr_of!(TRANSITION_GDT)).descriptor) as *const _ as *const u8
        });
        let final_gdt_ptr_high = (((final_gdt_ptr_virt as u64)
                .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF)
                .wrapping_add(target_offset)) as *const u8;

        let l_idt = load_idt.map_or(core::ptr::null(), |f| f as *const ());

        let final_kernel_entry = kernel_entry.map_or(0, |entry| {
            let phys = (entry as u64).wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
            phys.wrapping_add(target_offset) as usize
        });

        let logic_fn_phys = (landing_zone_logic as *const () as u64)
            .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
        let logic_fn_high = logic_fn_phys.wrapping_add(target_offset);

        let landing_zone_phys = (landing_zone as *const () as u64)
            .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
        let landing_zone_high = landing_zone_phys.wrapping_add(target_offset);

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
            "mov rdi, {load_gdt}",
            "mov rsi, {load_idt}",
            "mov rdx, {phys_offset}",
            "mov rcx, {l4_frame}",
            "mov r8, {allocator}",
            "mov r9, {logic_fn_high}",
            "push {kernel_entry}",
            "push {cs_selector}",
            "push {landing_zone_high}",
            "retfq",
            load_gdt = in(reg) ctx.load_gdt,
            load_idt = in(reg) ctx.load_idt,
            phys_offset = in(reg) ctx.phys_offset,
            l4_frame = in(reg) ctx.l4_frame,
            allocator = in(reg) ctx.allocator,
            logic_fn_high = in(reg) ctx.logic_fn_high,
            kernel_entry = in(reg) ctx.kernel_entry,
            cs_selector = in(reg) ctx.cs_selector,
            landing_zone_high = in(reg) ctx.landing_zone_high,
            offset_diff = in(reg) ctx.offset_diff,
            options(noreturn)
        );
        core::hint::unreachable_unchecked()
    }
}
