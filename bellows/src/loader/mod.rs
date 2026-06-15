use core::ffi::c_void;

use petroleum::common::{BellowsError, EfiBootServices, EfiMemoryType, EfiStatus, EfiSystemTable};

pub mod heap;

pub fn init_heap(bs: &EfiBootServices) -> petroleum::common::Result<()> {
    heap::init_heap(bs)
}

const MAP_BUFFER_SIZE: usize = 128 * 1024;
const KERNEL_ARGS_PAGES: usize = 256;
const PAGE_SIZE_4K: u64 = 4096;

pub fn exit_boot_services_and_jump(
    image_handle: usize,
    system_table: *mut EfiSystemTable,
    kernel_phys_start: x86_64::PhysAddr,
    kernel_entry_phys: u64,
    _entry: extern "efiapi" fn(usize, *mut EfiSystemTable, *mut c_void, usize) -> !,
) -> petroleum::common::Result<!> {
    use petroleum::page_table::allocator::FrameAllocatorExt;

    let bs = unsafe { &*(*system_table).boot_services };

    let map_buffer_size: usize = MAP_BUFFER_SIZE;
    let alloc_pages = petroleum::common::utils::calculate_pages_for_buffer(map_buffer_size);

    let mut args_phys_addr: usize = 0;
    if EfiStatus::from((bs.allocate_pages)(
        0usize, EfiMemoryType::EfiLoaderData, KERNEL_ARGS_PAGES, &mut args_phys_addr,
    )) != EfiStatus::Success
    {
        return Err(BellowsError::AllocationFailed("KernelArgs alloc failed"));
    }

    let mut map_phys_addr: usize = 0;
    if EfiStatus::from((bs.allocate_pages)(
        0usize, EfiMemoryType::EfiLoaderData, alloc_pages, &mut map_phys_addr,
    )) != EfiStatus::Success
    {
        return Err(BellowsError::AllocationFailed("Map buffer alloc failed"));
    }

    let map_ptr =
        petroleum::common::utils::calculate_map_data_ptr(map_phys_addr) as *mut c_void;

    let kernel_entry_virt = petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64()
        + kernel_entry_phys;
    let kernel_stack_top = petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64()
        + args_phys_addr as u64 + (KERNEL_ARGS_PAGES as u64 * PAGE_SIZE_4K);
    let safe_stack_phys = args_phys_addr as u64 + (KERNEL_ARGS_PAGES as u64 * PAGE_SIZE_4K);
    let l4_phys = args_phys_addr as u64 + 4096;
    let phys_off = petroleum::page_table::constants::HIGHER_HALF_OFFSET.as_u64();

    let jump_args_ptr = args_phys_addr as *mut petroleum::page_table::InitAndJumpArgs;
    let kernel_args_phys =
        args_phys_addr as u64 + core::mem::size_of::<petroleum::page_table::InitAndJumpArgs>() as u64;
    let kernel_args_phys_aligned = (kernel_args_phys + 15) & !15;
    let kernel_args_page = kernel_args_phys_aligned & !0xFFF;
    let kernel_args_offset = kernel_args_phys_aligned & 0xFFF;

    let fb = if let Some(cfg) =
        petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|m| *m.lock())
    {
        (cfg.address as u64, cfg.width, cfg.height, cfg.bpp, cfg.stride, cfg.pixel_format as u32)
    } else {
        (0, 0, 0, 0, 0, 0)
    };

    let mut map_size: usize = map_buffer_size;
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;
    let mut attempts = 0;
    const MAX_ATTEMPTS: usize = 10;

    loop {
        if attempts >= MAX_ATTEMPTS {
            let _ = (bs.free_pages)(map_phys_addr, alloc_pages);
            return Err(BellowsError::InvalidState("Too many attempts"));
        }
        attempts += 1;

        if EfiStatus::from((bs.get_memory_map)(
            &mut map_size, map_ptr, &mut map_key, &mut descriptor_size, &mut descriptor_version,
        )) != EfiStatus::Success
        {
            let _ = (bs.free_pages)(map_phys_addr, alloc_pages);
            return Err(BellowsError::InvalidState("get_memory_map failed"));
        }

        // ── Build everything BEFORE exit_boot_services ──────────────

        let num_desc = map_size.checked_div(descriptor_size).unwrap_or(0);
        let mut desc_vec: alloc::vec::Vec<
            petroleum::page_table::memory_map::MemoryMapDescriptor,
        > = alloc::vec::Vec::with_capacity(num_desc);
        if num_desc > 0 && !map_ptr.is_null() {
            for i in 0..num_desc {
                unsafe {
                    let dp = petroleum::common::utils::calculate_descriptor_ptr(
                        map_ptr as *const u8, i, descriptor_size,
                    );
                    desc_vec.push(
                        petroleum::page_table::memory_map::MemoryMapDescriptor::new(
                            dp, descriptor_size,
                        ),
                    );
                }
            }
        }

        let (_, total_frames, _) =
            petroleum::page_table::memory_map::processor::calculate_frame_allocation_params(
                &desc_vec,
            );
        let mut frame_allocator =
            petroleum::page_table::BitmapFrameAllocator::new(total_frames);
        frame_allocator.init(0);
        petroleum::page_table::memory_map::processor::mark_available_frames(
            &mut frame_allocator, &desc_vec,
        );

        // Reserve UEFI stack guard
        {
            let uefi_rsp: usize;
            unsafe { core::arch::asm!("mov {}, rsp", out(reg) uefi_rsp); }
            let r = uefi_rsp as u64;
            let s = (r.saturating_sub(2 * 1024 * 1024)) & !0xFFF;
            let p = ((r + 2 * 1024 * 1024 - s + 4095) / 4096) as usize;
            let _ = frame_allocator.reserve_frames(s, p);
        }

        // Final map size with FB config
        let mut final_map_size = map_size + core::mem::size_of::<usize>();
        if let Some(config) =
            petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|m| *m.lock())
        {
            let cwm = petroleum::common::uefi::ConfigWithMetadata {
                descriptor_size,
                magic: petroleum::common::uefi::FRAMEBUFFER_CONFIG_MAGIC,
                config,
            };
            let cs = core::mem::size_of::<petroleum::common::uefi::ConfigWithMetadata>();
            let co = petroleum::common::utils::calculate_config_offset(map_size);
            if petroleum::common::utils::check_buffer_overflow(
                map_phys_addr, co, cs, map_buffer_size,
            ) {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &cwm as *const _ as *const u8,
                        (map_phys_addr as *mut u8).add(co), cs,
                    );
                }
                final_map_size += cs;
            }
        }

        // Write InitAndJumpArgs
        unsafe {
            core::ptr::write_volatile(
                jump_args_ptr,
                petroleum::page_table::InitAndJumpArgs {
                    physical_memory_offset: petroleum::page_table::constants::HIGHER_HALF_OFFSET,
                    frame_allocator: &mut frame_allocator as *mut _,
                    kernel_phys_start: kernel_phys_start.as_u64(),
                    entry_virt: kernel_entry_virt,
                    stack_top: kernel_stack_top,
                    arg1: kernel_args_page,
                    arg2: kernel_args_offset,
                    map_phys_addr: map_phys_addr as u64,
                    map_size: final_map_size as u64,
                    l4_phys_addr: l4_phys,
                },
            );
        }

        // Write KernelArgs
        unsafe {
            let kp = kernel_args_phys_aligned as *mut petroleum::assembly::KernelArgs;
            core::ptr::write_volatile(
                kp,
                petroleum::assembly::KernelArgs {
                    handle: image_handle,
                    system_table: system_table as usize,
                    map_ptr: map_phys_addr,
                    map_size: final_map_size,
                    descriptor_size,
                    kernel_phys_start: kernel_phys_start.as_u64(),
                    kernel_entry: kernel_entry_virt as usize,
                    fb_address: fb.0,
                    fb_width: fb.1,
                    fb_height: fb.2,
                    fb_bpp: fb.3,
                    fb_stride: fb.4,
                    fb_pixel_format: fb.5,
                },
            );
        }

        petroleum::vga_debug::vga_puts(21, 0, b"BLW:call ebs");

        // ── Call exit_boot_services via asm ─────────────────────────
        // The compiler loads `in(reg)` operands into registers BEFORE the
        // asm executes (UEFI stack still writable).
        // The asm switches RSP to safe stack, calls exit_boot_services,
        // and on success jumps straight to init_and_jump (never returns).
        // On failure it falls through; we then just `continue` the loop.

        let ebs_fn = bs.exit_boot_services as usize;
        let init_fn = petroleum::page_table::init_and_jump as usize;

        unsafe {
            core::arch::asm!(
                "mov rsp, {safe_rsp}",  // switch to safe stack
                "mov rdi, {handle}",
                "mov rsi, {key}",
                "call {ebs}",           // exit_boot_services(handle, key)

                "cmp eax, 0",
                "je 2f",
                "cmp eax, 3",
                "je 2f",

                // Not success → fall through (the code below does `continue`)
                "jmp 3f",

                // ── Success path ──
                "2:",
                "mov rdi, {jargs}",
                "mov rsi, {st}",
                "mov rdx, {l4}",
                "mov rcx, {entry}",
                "mov r8,  {off}",
                "jmp {init}",           // tail-jump to init_and_jump

                // ── Failure path ──
                "3:",

                safe_rsp = in(reg) safe_stack_phys,
                handle   = in(reg) image_handle,
                key      = in(reg) map_key,
                ebs      = in(reg) ebs_fn,
                jargs    = in(reg) jump_args_ptr,
                st       = in(reg) kernel_stack_top,
                l4       = in(reg) l4_phys,
                entry    = in(reg) kernel_entry_virt,
                off      = in(reg) phys_off,
                init     = in(reg) init_fn,
                options(nomem),
            );
        }

        // If we reach here, exit_boot_services failed (probably
        // InvalidParameter — map key stale).  Loop again.
        map_size = map_buffer_size;
    }
}

pub fn load_efi_image(
    st: &petroleum::common::EfiSystemTable,
    file: &[u8],
    phys_offset: usize,
) -> petroleum::common::Result<(
    x86_64::addr::PhysAddr,
    u64,
    extern "efiapi" fn(usize, *mut petroleum::common::EfiSystemTable, *mut c_void, usize) -> !,
)> {
    petroleum::page_table::pe::load_efi_image(st, file, phys_offset)
}