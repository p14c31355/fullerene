use x86_64::{VirtAddr};
use x86_64::structures::paging::{PhysFrame, Mapper, OffsetPageTable};
use crate::page_table::constants::BootInfoFrameAllocator;

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
    unsafe {
        core::arch::naked_asm!(
            // 1. Immediate生存確認 (No stack usage)
            "mov dx, 0x3f8", "mov al, 0x4c", "out dx, al", // 'L'
            "mov dx, 0x3f8", "mov al, 0x4d", "out dx, al", // 'M'
            "mov dx, 0x3f8", "mov al, 0x4e", "out dx, al", // 'N'
            "mov dx, 0x3f8", "mov al, 0x58", "out dx, al", // 'X'

            // 2. Transition back to Rust logic immediately to preserve registers
            "mov dx, 0x3f8", "mov al, 0x57", "out dx, al", // 'W'
            "mov r10, rsp",
            "and rsp, -16",
            "call r12",
            "hlt",
        );
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

/// Global pointer to kernel arguments, set during the high-half transition.
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
        GdtEntry { limit_low: 0, base_low: 0, base_mid: 0, access: 0, flags: 0, base_high: 0 }, // 0x00: Null
        GdtEntry { // 0x08: Kernel Code
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x9A, // Present, Ring 0, Code, Exec/Read
            flags: 0xAF, // Long mode, 64-bit
            base_high: 0,
        },
        GdtEntry { // 0x10: Kernel Data
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x92, // Present, Ring 0, Data, Read/Write
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

        if !load_idt.is_null() {
            let idt_fn: fn() = core::mem::transmute(load_idt);
            idt_fn();
        }

        if !load_gdt.is_null() {
            let gdt_fn: fn() = core::mem::transmute(load_gdt);
            gdt_fn();
        }

        let l4_phys = l4_frame_raw;
        let local_phys_offset = VirtAddr::new(phys_offset_raw);
        let local_frame_allocator = frame_allocator;

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"High-half transition: landing zone logic reached!\n");
        crate::flush_tlb_and_verify!();

        let l4_virt = local_phys_offset + l4_phys;
        let mut mapper = x86_64::structures::paging::OffsetPageTable::new(
            &mut *(l4_virt.as_mut_ptr() as *mut x86_64::structures::paging::PageTable),
            local_phys_offset,
        );

        let _ = mapper.map_to(
            x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(l4_virt),
            x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(l4_phys)),
            x86_64::structures::paging::PageTableFlags::PRESENT | x86_64::structures::paging::PageTableFlags::WRITABLE | x86_64::structures::paging::PageTableFlags::NO_EXECUTE,
            &mut *local_frame_allocator,
        );
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Landing zone jumping to kernel entry!\n");
        
        if actual_kernel_entry != 0 {
            let args = &*actual_kernel_args;

            // Ensure 4MB around entry is mapped as executable to allow relative jumps
            let entry_page_virt = (actual_kernel_entry as u64) & !0xFFF;
            let entry_page_phys = entry_page_virt.wrapping_sub(local_phys_offset.as_u64());
            for page_offset in 0..1024 {
                let v_page = entry_page_virt + (page_offset * 4096);
                let p_page = entry_page_phys + (page_offset * 4096);
                let _ = mapper.map_to(
                    x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(v_page)),
                    x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(p_page)),
                    x86_64::structures::paging::PageTableFlags::PRESENT,
                    &mut *local_frame_allocator,
                );
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

        unsafe {
            let gdt_ptr_static = core::ptr::addr_of_mut!(TRANSITION_GDT);
            let entries_virt_addr = core::ptr::addr_of!((*gdt_ptr_static).entries) as *const _ as u64;
            let gdt_phys_base = entries_virt_addr.wrapping_sub(current_offset);
            let gdt_high_base = gdt_phys_base.wrapping_add(target_offset);
            (*gdt_ptr_static).descriptor.base = gdt_high_base;
        }

        let final_gdt_ptr_virt = gdt_ptr.unwrap_or(unsafe {
            core::ptr::addr_of!((*core::ptr::addr_of!(TRANSITION_GDT)).descriptor) as *const _ as *const u8
        });
        let final_gdt_ptr_high = (final_gdt_ptr_virt as u64)
            .wrapping_sub(current_offset)
            .wrapping_add(target_offset) as *const u8;

        let l_idt = load_idt.map_or(core::ptr::null(), |f| f as *const ());

        let final_kernel_entry = kernel_entry.map_or(0, |entry| {
            (entry as u64).wrapping_add(target_offset) as usize
        });

        Self {
            cr3: level_4_table_frame.start_address().as_u64(),
            load_gdt: load_gdt.map_or(core::ptr::null(), |f| f as *const ()),
            load_idt: l_idt,
            phys_offset: target_offset,
            l4_frame: level_4_table_frame.start_address().as_u64(),
            allocator: frame_allocator as *const _,
            logic_fn_high: ((landing_zone_logic as *const () as usize) as u64)
                .wrapping_sub(current_offset)
                .wrapping_add(target_offset) as usize,
            kernel_entry: final_kernel_entry,
            kernel_args_virt: kernel_args_phys.map_or(0, |phys| phys + target_offset),
            cs_selector: 0x08,
            landing_zone_high: ((landing_zone as *const () as usize) as u64)
                .wrapping_sub(current_offset)
                .wrapping_add(target_offset) as usize,
            offset_diff,
            gdt_ptr: final_gdt_ptr_high,
        }
    }
}

#[inline(never)]
pub fn perform_world_switch(ctx: TransitionContext) -> ! {
    unsafe {
        // Set globals before switching to avoid ABI issues
        TRANSITION_KERNEL_ENTRY = ctx.kernel_entry;
        KERNEL_ARGS = ctx.kernel_args_virt as *const KernelArgs;

        // 1. Switch CR3 using x86_64 crate
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(ctx.cr3)),
            x86_64::registers::control::Cr3Flags::empty(),
        );

        // 2. Load GDT using assembly (as x86_64 crate function was not found)
        core::arch::asm!("lgdt [{}]", in(reg) ctx.gdt_ptr);

        // 3. Transition to landing zone
        // We still need a small asm block for the far jump to ensure CS is correct
        // and to handle the RSP shift.
        core::arch::asm!(
            "add rsp, {offset_diff}",
            "and rsp, -16",
            "push {cs_selector}",
            "push {landing_zone_high}",
            "retfq",
            cs_selector = in(reg) ctx.cs_selector,
            landing_zone_high = in(reg) ctx.landing_zone_high,
            offset_diff = in(reg) ctx.offset_diff,
            options(noreturn)
        );
        core::hint::unreachable_unchecked()
    }
}