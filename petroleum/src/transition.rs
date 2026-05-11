//! High-level world switch and kernel transition abstractions.

use crate::assembly::{KernelArgs, TransitionArgs, TransitionFrame};
use crate::page_table::constants::BootInfoFrameAllocator;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Mapper, OffsetPageTable, PageTable, PageTableFlags, PhysFrame},
};

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct GdtEntry {
    pub limit_low: u16,
    pub base_low: u16,
    pub base_mid: u8,
    pub access: u8,
    pub flags: u8,
    pub base_high: u8,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct GdtDescriptor {
    pub limit: u16,
    pub base: u64,
}

#[repr(C, packed)]
pub struct TransitionGdt {
    pub descriptor: GdtDescriptor,
    pub entries: [GdtEntry; 3],
}

pub static mut TRANSITION_GDT: TransitionGdt = TransitionGdt {
    descriptor: GdtDescriptor {
        limit: (core::mem::size_of::<[GdtEntry; 3]>() - 1) as u16,
        base: 0,
    },
    entries: [
        GdtEntry {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0,
            flags: 0,
            base_high: 0,
        },
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
pub static mut KERNEL_ARGS: *const KernelArgs = core::ptr::null();

#[unsafe(no_mangle)]
pub static mut TRANSITION_KERNEL_ENTRY: usize = 0;

#[repr(C)]
pub struct WorldSwitch {
    pub load_gdt: *const (),
    pub load_idt: *const (),
    pub page_table: PhysFrame,
    pub phys_offset: VirtAddr,
    pub stack_top: VirtAddr,
    pub entry_point: VirtAddr,
    pub kernel_args: *const KernelArgs,
    pub allocator: *mut BootInfoFrameAllocator,
}

impl WorldSwitch {
    pub fn to_transition_args(&self) -> TransitionArgs {
        TransitionArgs {
            load_gdt: self.load_gdt,
            load_idt: self.load_idt,
            phys_offset: self.phys_offset.as_u64(),
            l4_frame: self.page_table.start_address().as_u64(),
            allocator: self.allocator,
            kernel_entry: self.entry_point.as_u64() as usize,
            kernel_args: self.kernel_args,
        }
    }
}

pub trait KernelTransition {
    unsafe fn perform(self) -> !;
}

pub struct UefiToHigherHalf {
    pub world: WorldSwitch,
    pub landing_zone: VirtAddr,
}

impl KernelTransition for UefiToHigherHalf {
    unsafe fn perform(self) -> ! {
        let world = self.world;
        let transition_args = world.to_transition_args();
        let frame = TransitionFrame {
            args: transition_args,
            logic_fn: landing_zone_logic as usize,
        };
        let lz: unsafe extern "sysv64" fn(*const TransitionFrame) -> ! =
            core::mem::transmute(self.landing_zone);
        lz(&frame)
    }
}

unsafe fn write_serial_hex(val: u64) {
    let mut buf = [0u8; 16];
    let len = crate::serial::format_hex_to_buffer(val, &mut buf, 16);
    crate::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
}

#[unsafe(no_mangle)]
#[inline(never)]
pub unsafe extern "sysv64" fn landing_zone_logic(ctx: *const TransitionArgs) {
    unsafe {
        let args = &*ctx;

        let actual_kernel_entry = if args.kernel_entry == 0 {
            TRANSITION_KERNEL_ENTRY
        } else {
            args.kernel_entry
        };

        let actual_kernel_args = if args.kernel_args.is_null() {
            KERNEL_ARGS
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

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"LZ: reached\n");

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DBG: entry=0x");
        write_serial_hex(actual_kernel_entry as u64);
        crate::write_serial_bytes!(0x3F8, 0x3FD, b" offset=0x");
        write_serial_hex(local_phys_offset.as_u64());
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        let kernel_entry_virt = (actual_kernel_entry as u64).wrapping_add(local_phys_offset.as_u64());
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DBG: entry_virt=0x");
        write_serial_hex(kernel_entry_virt);
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        crate::flush_tlb_and_verify!();

        let l4_virt_raw = args.phys_offset.wrapping_add(l4_phys);
        let l4_virt_sign_extended = if (l4_virt_raw & (1 << 47)) != 0 {
            l4_virt_raw | 0xFFFF_0000_0000_0000
        } else {
            l4_virt_raw & 0x0000_FFFF_FFFF_FFFF
        };
        let l4_virt = VirtAddr::new(l4_virt_sign_extended);

        let mut temp_mapper = OffsetPageTable::new(
            &mut *(l4_virt.as_mut_ptr() as *mut PageTable),
            VirtAddr::new(0),
        );

        let l4_v_sign = if (l4_virt_raw & (1 << 47)) != 0 {
            l4_virt_raw | 0xFFFF_0000_0000_0000
        } else {
            l4_virt_raw & 0x0000_FFFF_FFFF_FFFF
        };
        let _ = temp_mapper.map_to(
            x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(l4_v_sign)),
            x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(l4_phys & 0x000F_FFFF_FFFF_FFFF)),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            &mut *local_frame_allocator,
        );

        if actual_kernel_entry == 0 {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"ERROR: entry is 0!\n");
            loop { core::hint::spin_loop(); }
        }

        // Map KernelArgs at higher half
        let args_phys = actual_kernel_args as u64;
        let args_phys_raw = args_phys.wrapping_sub(local_phys_offset.as_u64());
        let args_v_sign = if (args_phys & (1 << 47)) != 0 {
            args_phys | 0xFFFF_0000_0000_0000
        } else {
            args_phys & 0x0000_FFFF_FFFF_FFFF
        };
        let _ = temp_mapper.map_to(
            x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(args_v_sign)),
            x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(args_phys_raw & 0x000F_FFFF_FFFF_FFFF)),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            &mut *local_frame_allocator,
        );

        let k_args = &*actual_kernel_args;
        let kernel_phys_start = k_args.kernel_phys_start;
        let kernel_virt_start = kernel_phys_start.wrapping_add(local_phys_offset.as_u64());

        // Map memory map buffer
        let map_phys = k_args.map_ptr as u64;
        let map_virt = map_phys.wrapping_add(local_phys_offset.as_u64());
        let map_pages = (k_args.map_size as u64 + 4095) / 4096;
        for i in 0..map_pages {
            let v_addr = map_virt.wrapping_add(i * 4096);
            let p_addr = map_phys.wrapping_add(i * 4096);
            let v_sign = if (v_addr & (1 << 47)) != 0 {
                v_addr | 0xFFFF_0000_0000_0000
            } else {
                v_addr & 0x0000_FFFF_FFFF_FFFF
            };
            let _ = temp_mapper.map_to(
                x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(v_sign)),
                x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(p_addr & 0x000F_FFFF_FFFF_FFFF)),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                &mut *local_frame_allocator,
            );
        }

        // Map kernel at higher half using 2MB pages
        for page_offset in 0..2048 {
            let v_addr_raw = kernel_virt_start.wrapping_add(page_offset * 2 * 1024 * 1024);
            let p_addr_raw = kernel_phys_start.wrapping_add(page_offset * 2 * 1024 * 1024);
            let v_addr_sign_extended = if (v_addr_raw & (1 << 47)) != 0 {
                v_addr_raw | 0xFFFF_0000_0000_0000
            } else {
                v_addr_raw & 0x0000_FFFF_FFFF_FFFF
            };
            let _ = temp_mapper.map_to(
                x86_64::structures::paging::Page::<x86_64::structures::paging::Size2MiB>::containing_address(VirtAddr::new(v_addr_sign_extended)),
                x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size2MiB>::containing_address(x86_64::PhysAddr::new(p_addr_raw)),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                &mut *local_frame_allocator,
            );
        }

        // Map entry point pages at higher half
        let entry_phys_start = (actual_kernel_entry as u64) & !0xFFF;
        for page_offset in -16i32..16i32 {
            let p_page = entry_phys_start.wrapping_add((page_offset as i64 * 4096) as u64);
            let v_page = p_page.wrapping_add(local_phys_offset.as_u64());
            let v_sign = if (v_page & (1 << 47)) != 0 {
                v_page | 0xFFFF_0000_0000_0000
            } else {
                v_page & 0x0000_FFFF_FFFF_FFFF
            };
            let _ = temp_mapper.map_to(
                x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(v_sign)),
                x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(p_page & 0x000F_FFFF_FFFF_FFFF)),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                &mut *local_frame_allocator,
            );
        }

        // Map stack pages at higher half
        let rsp_val: u64;
        core::arch::asm!("mov {}, rsp", out(reg) rsp_val);
        let stack_phys_start = (rsp_val & !0xFFF).wrapping_sub(local_phys_offset.as_u64());
        for page_offset in 0..4096 {
            let p_page = stack_phys_start
                .wrapping_sub(8 * 1024 * 1024)
                .wrapping_add(page_offset * 4096);
            let v_page = p_page.wrapping_add(local_phys_offset.as_u64());
            let v_sign = if (v_page & (1 << 47)) != 0 {
                v_page | 0xFFFF_0000_0000_0000
            } else {
                v_page & 0x0000_FFFF_FFFF_FFFF
            };
            let _ = temp_mapper.map_to(
                x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(v_sign)),
                x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::PhysAddr::new(p_page & 0x000F_FFFF_FFFF_FFFF)),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                &mut *local_frame_allocator,
            );
        }

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DBG: jumping to 0x");
        write_serial_hex(kernel_entry_virt);
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        crate::assembly::jump_to_kernel(
            kernel_entry_virt as usize,
            actual_kernel_args,
            local_phys_offset.as_u64(),
        );
    }
}

pub struct WorldSwitchBuilder {
    load_gdt: Option<*const ()>,
    load_idt: Option<*const ()>,
    page_table: Option<PhysFrame>,
    phys_offset: Option<VirtAddr>,
    stack_top: Option<VirtAddr>,
    entry_point: Option<VirtAddr>,
    kernel_args: Option<*const KernelArgs>,
    allocator: Option<*mut BootInfoFrameAllocator>,
}

impl Default for WorldSwitchBuilder {
    fn default() -> Self {
        Self {
            load_gdt: None,
            load_idt: None,
            page_table: None,
            phys_offset: None,
            stack_top: None,
            entry_point: None,
            kernel_args: None,
            allocator: None,
        }
    }
}

impl WorldSwitchBuilder {
    pub fn with_gdt(mut self, gdt: *const ()) -> Self { self.load_gdt = Some(gdt); self }
    pub fn with_idt(mut self, idt: *const ()) -> Self { self.load_idt = Some(idt); self }
    pub fn with_page_table(mut self, frame: PhysFrame) -> Self { self.page_table = Some(frame); self }
    pub fn with_phys_offset(mut self, offset: VirtAddr) -> Self { self.phys_offset = Some(offset); self }
    pub fn with_stack(mut self, stack: VirtAddr) -> Self { self.stack_top = Some(stack); self }
    pub fn with_entry(mut self, entry: VirtAddr) -> Self { self.entry_point = Some(entry); self }
    pub fn with_args(mut self, args: *const KernelArgs) -> Self { self.kernel_args = Some(args); self }
    pub fn with_allocator(mut self, allocator: *mut BootInfoFrameAllocator) -> Self { self.allocator = Some(allocator); self }

    pub fn build(self) -> Result<WorldSwitch, &'static str> {
        Ok(WorldSwitch {
            load_gdt: self.load_gdt.ok_or("Missing GDT")?,
            load_idt: self.load_idt.ok_or("Missing IDT")?,
            page_table: self.page_table.ok_or("Missing Page Table")?,
            phys_offset: self.phys_offset.ok_or("Missing Phys Offset")?,
            stack_top: self.stack_top.ok_or("Missing Stack Top")?,
            entry_point: self.entry_point.ok_or("Missing Entry Point")?,
            kernel_args: self.kernel_args.ok_or("Missing Kernel Args")?,
            allocator: self.allocator.ok_or("Missing Allocator")?,
        })
    }
}