use x86_64::{VirtAddr};
use x86_64::structures::paging::{PhysFrame, Mapper, OffsetPageTable, PageTableFlags, PageTable};
use crate::page_table::constants::BootInfoFrameAllocator;


pub use crate::assembly::{TransitionArgs, TransitionFrame, KernelArgs};

// Moved to crate::transition

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
    /// UEFI Stage2 への簡易ワールドスイッチ（CR3変更なしの場合に最適化）
    pub fn prepare_for_efi_stage2(
        phys_offset: VirtAddr,
        current_physical_memory_offset: VirtAddr,
        _kernel_stack_top: VirtAddr,
        stage2_fn: unsafe extern "C" fn(*mut (), VirtAddr) -> !,
        ctx: *mut (),
    ) -> Self {
        let current_offset = current_physical_memory_offset.as_u64();
        let target_offset = phys_offset.as_u64();
        let offset_diff = target_offset.wrapping_sub(current_offset);

        let stage2_phys = (stage2_fn as *const () as u64)
            .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
        let stage2_high = stage2_phys.wrapping_add(target_offset);

        Self {
            cr3: 0, // CR3変更不要なら0
            load_gdt: core::ptr::null(),
            load_idt: core::ptr::null(),
            phys_offset: target_offset,
            l4_frame: 0,
            allocator: core::ptr::null(),
            logic_fn_high: stage2_high as usize,
            kernel_entry: stage2_high as usize,
            kernel_args_virt: ctx as u64,
            cs_selector: 0x08,
            landing_zone_high: 0,
            offset_diff,
            gdt_ptr: core::ptr::null(),
        }
    }

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

        let lz_addr = crate::assembly::landing_zone as *const () as u64;
        let lzl_addr = crate::transition::landing_zone_logic as *const () as u64;

        crate::debug_log_no_alloc!("TransitionContext::prepare - current_offset: 0x{:x}, target_offset: 0x{:x}, offset_diff: 0x{:x}", current_offset, target_offset, offset_diff);
        crate::debug_log_no_alloc!("TransitionContext::prepare - landing_zone: 0x{:x}, landing_zone_logic: 0x{:x}", lz_addr, lzl_addr);

        unsafe {
            let gdt_ptr_static = core::ptr::addr_of_mut!(crate::transition::TRANSITION_GDT);
            let entries_virt_addr = core::ptr::addr_of!((*gdt_ptr_static).entries) as *const _ as u64;
            let gdt_phys_base = entries_virt_addr.wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
            // GDT base must be a linear address in the new address space
            (*gdt_ptr_static).descriptor.base = gdt_phys_base.wrapping_add(target_offset);
        }

        let final_gdt_ptr_virt = gdt_ptr.unwrap_or(unsafe {
            core::ptr::addr_of!((*core::ptr::addr_of!(crate::transition::TRANSITION_GDT)).descriptor) as *const _ as *const u8
        });
        let final_gdt_ptr_high = (((final_gdt_ptr_virt as u64)
                .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF)
                .wrapping_add(target_offset)) as *const u8;

        let l_idt = load_idt.map_or(core::ptr::null(), |f| f as *const ());

        let final_kernel_entry = kernel_entry.map_or(0, |entry| {
            if (entry as u64) >= 0x8000_0000_0000_0000 {
                entry // Already a high-half address
            } else {
                (entry as u64).wrapping_add(target_offset) as usize
            }
        });

        let logic_fn_phys = (crate::transition::landing_zone_logic as *const () as u64)
            .wrapping_sub(current_offset) & 0x0000_FFFF_FFFF_FFFF;
        let logic_fn_high = logic_fn_phys.wrapping_add(target_offset);

        let landing_zone_phys = (crate::assembly::landing_zone as *const () as u64)
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
#[inline(never)]
pub fn perform_efi_stage2_switch(ctx: TransitionContext, stack_top: VirtAddr) -> ! {
    unsafe {
        // 新しいスタックへ切り替え + 直接ジャンプ（landing_zone不要）
        core::arch::asm!(
            "mov rsp, {stack}",
            "mov rdi, {arg1}",      // ctx pointer
            "mov rsi, {arg2}",      // physical_memory_offset
            "jmp {target}",
            stack = in(reg) stack_top.as_u64(),
            arg1 = in(reg) ctx.kernel_args_virt,
            arg2 = in(reg) ctx.phys_offset,
            target = in(reg) ctx.logic_fn_high,
            options(noreturn)
        );
    }
}

pub fn perform_world_switch(ctx: TransitionContext) -> ! {
    unsafe {
        // Create the transition frame on the stack
        let frame = TransitionFrame {
            args: TransitionArgs {
                load_gdt: ctx.load_gdt,
                load_idt: ctx.load_idt,
                phys_offset: ctx.phys_offset,
                l4_frame: ctx.l4_frame,
                allocator: ctx.allocator as *mut _,
                kernel_entry: ctx.kernel_entry,
                kernel_args: ctx.kernel_args_virt as *const _,
            },
            logic_fn: ctx.logic_fn_high,
        };

        // Translate the frame pointer to the target world's address space
        let frame_ptr_old = &frame as *const TransitionFrame as u64;
        let frame_ptr_new = frame_ptr_old.wrapping_add(ctx.offset_diff);

        // Update global tracking for debugging if necessary
        unsafe {
            crate::transition::TRANSITION_KERNEL_ENTRY = ctx.kernel_entry;
            crate::transition::KERNEL_ARGS = ctx.kernel_args_virt as *const KernelArgs;
        }

        // Minimum required switch: CR3 and GDT
        x86_64::registers::control::Cr3::write(
            x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(ctx.cr3)),
            x86_64::registers::control::Cr3Flags::empty(),
        );

        core::arch::asm!("lgdt [{}]", in(reg) ctx.gdt_ptr);

        // Transition to the landing zone
        core::arch::asm!(
            "add rsp, {offset_diff}",
            "and rsp, -16",
            "push 0x08",
            "push {lz_high}",
            "retfq",
            offset_diff = in(reg) ctx.offset_diff,
            lz_high = in(reg) ctx.landing_zone_high,
            in("rdi") frame_ptr_new,
            options(noreturn)
        );
        core::hint::unreachable_unchecked()
    }
}
