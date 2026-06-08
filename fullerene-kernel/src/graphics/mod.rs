use alloc::boxed::Box;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use nitrogen::virtio::gpu::VirtioGpu;
use petroleum::graphics::UefiFramebufferWriter;
use petroleum::graphics::text::VgaBuffer;
use spin::Mutex;

/// Global primary framebuffer renderer (also used as text console).
pub static PRIMARY_RENDERER: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);

/// Global VirtIO GPU device.
pub static VIRTIO_GPU: Mutex<Option<Box<VirtioGpu>>> = Mutex::new(None);

/// Guard flag to prevent double initialization of the graphics subsystem.
static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Fallback VGA text console (used when UEFI framebuffer is not available).
static VGA_CONSOLE: Mutex<Option<VgaBuffer>> = Mutex::new(None);

/// Initializes the system graphics and primary console.
///
/// This function is idempotent: calling it more than once has no effect.
///
/// Priority:
/// 1. VirtIO-GPU (if present on PCI bus)
/// 2. GOP Framebuffer (from bootloader config, via safe_map_page WC overlay)
/// 3. Legacy VGA Text Mode (fallback)
pub fn init_graphics() {
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::debug_log!("Graphics: Already initialized, skipping\n");
        return;
    }

    // ── Path 1: VirtIO-GPU ────────────────────────────────────
    if let Some(ref gpu_device) = {
        let mut scanner = nitrogen::pci::PciScanner::new();
        let _ = scanner.scan_all_buses();
        scanner
            .get_devices()
            .iter()
            .find(|d| d.vendor_id == 0x1af4 && d.device_id == 0x1050)
            .cloned()
    } {
        petroleum::debug_log!("VirtIO-GPU: Initializing display\n");

        let caps = petroleum::virtio::pci::get_virtio_caps(gpu_device);
        petroleum::virtio::pci::dump_capabilities(gpu_device);

        let mut common_cap = None;
        let mut notify_cap = None;
        let mut pci_cfg_cap = None;
        for cap in &caps {
            match cap.cfg_type {
                petroleum::virtio::pci::VIRTIO_PCI_CAP_COMMON_CFG => common_cap = Some(cap),
                petroleum::virtio::pci::VIRTIO_PCI_CAP_NOTIFY_CFG => notify_cap = Some(cap),
                petroleum::virtio::pci::VIRTIO_PCI_CAP_PCI_CFG => pci_cfg_cap = Some(cap),
                _ => {}
            }
        }

        let common_cap = common_cap.expect("Common config not found");
        let notify_cap = notify_cap.expect("Notify config not found");
        let common_bar = common_cap.bar;
        let common_offset = common_cap.offset;
        let notify_bar = notify_cap.bar;
        let notify_offset = notify_cap.offset;

        petroleum::serial::serial_log(format_args!(
            "[graphics] common_cap: bar={}, offset={:#x}\n",
            common_bar, common_offset
        ));

        let bar_info = gpu_device
            .get_bar_info(common_bar)
            .expect("Failed to get Common BAR info");
        let notify_bar_info = gpu_device
            .get_bar_info(notify_bar)
            .expect("Failed to get Notify BAR info");

        for i in 0..6 {
            let offset = 0x10 + (i * 4);
            let val = nitrogen::pci::PciConfigSpace::read_config_dword(
                gpu_device.bus,
                gpu_device.device,
                gpu_device.function,
                offset as u8,
            );
            petroleum::serial::serial_log(format_args!(
                "[graphics] BAR{} (offset {:#x}) = {:#x}\n",
                i, offset, val
            ));
        }

        petroleum::serial::serial_log(format_args!(
            "[graphics] BARs: common_bar={}, addr={:#x}, offset={:#x}; notify_bar={}, addr={:#x}, offset={:#x}\n",
            common_bar, bar_info.address, common_offset, notify_bar, notify_bar_info.address, notify_offset
        ));

        let common_virt = 0xffff800060000000u64 as usize;
        let notify_virt = 0xffff800070000000u64 as usize;

        // Enable memory access AND bus mastering
        let mut config = nitrogen::pci::PciConfigSpace::read_from_device(
            gpu_device.bus, gpu_device.device, gpu_device.function,
        )
        .expect("Failed to read config");
        let cmd = config.command;
        let stat = config.status;
        petroleum::serial::serial_log(format_args!(
            "[graphics] Pre-enable: Command={:#x}, Status={:#x}\n", cmd, stat
        ));
        config.enable_memory_access(gpu_device.bus, gpu_device.device, gpu_device.function);
        config.command |= 0x0004;
        let val = (config.status as u32) << 16 | (config.command as u32);
        nitrogen::pci::PciConfigSpace::write_config_dword(
            &mut config, gpu_device.bus, gpu_device.device, gpu_device.function, 0x04, val,
        );

        // Map the full BARs — if this fails, fall through to GOP.
        let bars_ok = {
            let mut mm = crate::memory_management::get_memory_manager().lock();
            let mm = mm.as_mut().expect("MemoryManager not initialized");
            mm.map_mmio_region(bar_info.address as usize, common_virt, bar_info.size as usize)
                .is_ok()
                && mm.map_mmio_region(
                    notify_bar_info.address as usize,
                    notify_virt,
                    notify_bar_info.size as usize,
                )
                .is_ok()
        };

        'virtio: {
            if bars_ok {
                let common_virt_ptr = (common_virt + common_offset as usize) as *mut u32;
                let notify_virt_ptr = (notify_virt + notify_offset as usize) as *mut u32;

                use petroleum::page_table::constants::get_frame_allocator_mut;
                let off = petroleum::common::memory::get_physical_memory_offset() as u64;

                // Allocate command/response buffers
                let cmd_raw = match get_frame_allocator_mut().allocate_contiguous_frames(1) {
                    Ok(frame) => frame,
                    Err(_) => {
                        petroleum::serial::serial_log(format_args!(
                            "[graphics] Failed to allocate cmd buffer, falling back to GOP\n"
                        ));
                        break 'virtio;
                    }
                };
                let cmd_buf = (cmd_raw as u64 + off) as *mut u8;
                let resp_raw = match get_frame_allocator_mut().allocate_contiguous_frames(1) {
                    Ok(frame) => frame,
                    Err(_) => {
                        petroleum::serial::serial_log(format_args!(
                            "[graphics] Failed to allocate resp buffer, falling back to GOP\n"
                        ));
                        break 'virtio;
                    }
                };
                let resp_buf = (resp_raw as u64 + off) as *mut u8;
                unsafe {
                    core::ptr::write_bytes(cmd_buf, 0, 4096);
                    core::ptr::write_bytes(resp_buf, 0, 4096);
                }

                let gpu_result = nitrogen::virtio::gpu::init_virtio_gpu(
                    common_virt_ptr,
                    notify_virt_ptr,
                    gpu_device.clone(),
                    common_bar,
                    cmd_buf,
                    cmd_raw as u64,
                    4096,
                    resp_buf,
                    resp_raw as u64,
                    4096,
                );

                if let Some(mut gpu) = gpu_result {
                    let alloc_qmem = |size: usize| -> (*mut u8, u64) {
                        let pages = (size + 4095) / 4096;
                        let raw = get_frame_allocator_mut()
                            .allocate_contiguous_frames(pages)
                            .expect("VirtIO-GPU: failed to allocate queue memory");
                        let phys = raw as u64;
                        ((phys + off) as *mut u8, phys)
                    };
                    let (desc_virt, desc_phys) =
                        alloc_qmem(1024 * core::mem::size_of::<nitrogen::virtio::gpu::VringDesc>());
                    let (avail_virt, avail_phys) =
                        alloc_qmem(core::mem::size_of::<nitrogen::virtio::gpu::VringAvail>());
                    let (used_virt, used_phys) =
                        alloc_qmem(core::mem::size_of::<nitrogen::virtio::gpu::VringUsed>());
                    let desc = desc_virt as *mut nitrogen::virtio::gpu::VringDesc;
                    let avail = avail_virt as *mut nitrogen::virtio::gpu::VringAvail;
                    let used = used_virt as *mut nitrogen::virtio::gpu::VringUsed;
                    gpu.setup_queue(0, desc, desc_phys, avail, avail_phys, used, used_phys);

                    let fb_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
                        .get()
                        .and_then(|mutex| mutex.lock().clone());
                    let (fb_phys, fb_width, fb_height, fb_stride, fb_pixel_format) =
                        if let Some(ref c) = fb_config {
                            (c.address, c.width, c.height, c.stride, Some(c.pixel_format))
                        } else {
                            (
                                0x40000000u64, 1024u32, 768u32, 1024u32,
                                Some(petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor),
                            )
                        };

                    let fb_virt = fb_phys + off;
                    let fb_byte_size = (fb_stride as u64) * (fb_height as u64);
                    let pages = (fb_byte_size as usize + 4095) / 4096;
                    let fb_wc_flags = x86_64::structures::paging::PageTableFlags::WRITE_THROUGH
                        | x86_64::structures::paging::PageTableFlags::PRESENT
                        | x86_64::structures::paging::PageTableFlags::WRITABLE
                        | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;

                    // Use safe_map_page to split huge-page WB and overlay WC
                    let fb_mapped = {
                        let mut mm = crate::memory_management::get_memory_manager().lock();
                        let mm = mm.as_mut().expect("MemoryManager not initialized");
                        let mut ok = true;
                        for i in 0..pages {
                            if mm
                                .safe_map_page(
                                    (fb_virt + (i * 4096) as u64) as usize,
                                    (fb_phys + (i * 4096) as u64) as usize,
                                    fb_wc_flags,
                                )
                                .is_err()
                            {
                                ok = false;
                                break;
                            }
                        }
                        ok
                    };

                    if fb_mapped {
                        let fb_info = petroleum::graphics::color::FramebufferInfo {
                            address: fb_virt,
                            width: fb_width,
                            height: fb_height,
                            stride: fb_stride,
                            pixel_format: fb_pixel_format,
                            colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
                        };
                        match gpu.init_display(
                            fb_info.width,
                            fb_info.height,
                            fb_phys,
                            fb_info.stride * fb_info.height,
                        ) {
                            Ok(()) => {
                                let writer =
                                    petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(
                                        fb_info,
                                    );
                                let renderer =
                                    petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(
                                        writer,
                                    );
                                set_primary_renderer(renderer);
                                *VIRTIO_GPU.lock() = Some(Box::new(gpu));
                                petroleum::debug_log!("Graphics: VirtIO-GPU PRIMARY_RENDERER\n");
                                return;
                            }
                            Err(e) => {
                                petroleum::serial::serial_log(format_args!(
                                    "[graphics] VirtIO-GPU init_display failed: {:?}, GOP fallback.\n",
                                    e
                                ));
                            }
                        }
                    } else {
                        petroleum::serial::serial_log(format_args!(
                            "[graphics] FB WC map failed, GOP fallback.\n"
                        ));
                    }
                } else {
                    petroleum::serial::serial_log(format_args!(
                        "[graphics] VirtIO-GPU init_virtio_gpu returned None, GOP fallback.\n"
                    ));
                }
            } else {
                petroleum::serial::serial_log(format_args!(
                    "[graphics] VirtIO-GPU BAR map failed, GOP fallback.\n"
                ));
            }
        }
    } else {
        petroleum::serial::serial_log(format_args!(
            "[graphics] No VirtIO-GPU device found. Trying GOP.\n"
        ));
    }

    // ── Path 2: GOP / VGA mode 13h framebuffer ────────────────
    // Both 32bpp GOP and 8bpp VGA mode 13h linear framebuffers
    // are supported here.  The GUI subsystem (solvent) will skip
    // rendering if the framebuffer is too small or 8bpp, but the
    // kernel console text output will work.
    if let Some(fb_config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
        .get()
        .and_then(|mutex| mutex.lock().clone())
    {
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let fb_phys = fb_config.address;
        let fb_virt = fb_phys + off;
        let fb_size = (fb_config.stride as u64 * fb_config.height as u64) as usize;
        petroleum::debug_log!(
            "[graphics] GOP fallback: phys={:#x} virt={:#x} size={}\n",
            fb_phys, fb_virt, fb_size
        );

        // Do NOT call safe_map_page for WC remap on real hardware.
        // The boot-phase 1GB huge-page WB mapping is already live and
        // working (confirmed by pre-map write test + GOP pattern test).
        // safe_map_page's 4KB WC overlay breaks the mapping on InsydeH2O
        // because map_page_4k_l1 cannot safely split the 2MB/1GB huge page.
        // We rely on the existing identity mapping (WB via PAT/MTRR).
        let mapped_ok = true;

        if mapped_ok {
            if fb_config.bpp == 8 {
                // VGA mode 13h — reinitialize the DAC palette.
                // ExitBootServices may have reset it to all-black.
                petroleum::debug_log!(
                    "[graphics] 8bpp VGA mode 13h — reinit palette & fill\n"
                );
                // Re-run mode-13h setup (sets palette + registers)
                petroleum::graphics::setup::setup_vga_mode_13h();
                // Fill the framebuffer with a diagnostic pattern
                let fb_slice = unsafe {
                    core::slice::from_raw_parts_mut(
                        fb_virt as *mut u8,
                        fb_size,
                    )
                };
                for y in 0..fb_config.height.min(200) as usize {
                    for x in 0..fb_config.width.min(320) as usize {
                        let color: u8 = match y / 40 {
                            0 => 0x04, // red
                            1 => 0x02, // green
                            2 => 0x01, // blue
                            3 => 0x0E, // yellow
                            _ => 0x0F, // white
                        };
                        fb_slice[y * fb_config.stride as usize + x] = color;
                    }
                }
                // Also try VGA text mode (0xB8000) as fallback
                petroleum::graphics::setup::setup_vga_text_mode();
                // Fall through to Path 3 for text console
            } else {
                let fb_info = petroleum::graphics::color::FramebufferInfo {
                    address: fb_virt,
                    width: fb_config.width,
                    height: fb_config.height,
                    stride: fb_config.stride,
                    pixel_format: Some(fb_config.pixel_format),
                    colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
                };
                let writer =
                    petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(fb_info);
                let renderer =
                    petroleum::graphics::framebuffer::UefiFramebufferWriter::Uefi32(writer);
                *PRIMARY_RENDERER.lock() = Some(renderer);
                petroleum::debug_log!("Graphics: GOP Framebuffer WC map OK (32bpp)\n");
                return;
            }
        }
        petroleum::debug_log!("[graphics] GOP WC map failed\n");
    }

    // ── Path 3: VGA text mode (0xB8000 character buffer) ─────
    // Fallback when no framebuffer config exists at all.
    petroleum::debug_log!("Graphics: Falling back to VGA text mode.\n");
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let vga_phys = petroleum::page_table::constants::VGA_MEMORY_START;
    let vga_virt = vga_phys + off;

    // Split WB huge-page and map VGA text buffer as UC.
    let vga_flags = x86_64::structures::paging::PageTableFlags::NO_CACHE
        | x86_64::structures::paging::PageTableFlags::PRESENT
        | x86_64::structures::paging::PageTableFlags::WRITABLE
        | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
    {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialized");
        let _ = mm.safe_map_page(vga_virt as usize, vga_phys as usize, vga_flags);
    }

    let mut vga = petroleum::graphics::text::VgaBuffer::with_address(vga_virt as usize);
    vga.enable();
    petroleum::graphics::Console::clear(&mut vga);
    use core::fmt::Write;
    let _ = write!(vga, "fullerene kernel — VGA text mode\n");
    *VGA_CONSOLE.lock() = Some(vga);
    petroleum::debug_log!("Graphics: VGA text console ready, GUI disabled.\n");
}

/// Set the primary framebuffer renderer (also used as text console).
pub fn set_primary_renderer(renderer: UefiFramebufferWriter) {
    *PRIMARY_RENDERER.lock() = Some(renderer);
}

/// Helper to flush the GPU if present.
///
/// When VirtIO-GPU is active, issues a hardware flush.
/// Otherwise, emits an `sfence` (store fence) to commit any
/// write-combining (WC) framebuffer writes to the display controller.
pub fn flush_gpu() {
    let mut gpu = VIRTIO_GPU.lock();
    if let Some(ref mut gpu) = *gpu {
        if let Some(ref r) = *PRIMARY_RENDERER.lock() {
            let info = r.get_info();
            gpu.flush(info.width, info.height);
        }
    } else {
        // No VirtIO-GPU → flush non-temporal stores to the framebuffer.
        // `sfence` orders NT stores ahead of it (movnti → WC buffer → sfence →
        // globally visible).  Regular fences (mfence) also work but sfence is
        // the correct companion to _mm_stream_si32 / movnti.
        unsafe { core::arch::x86_64::_mm_sfence(); }
    }
}

/// Helper to write to the primary renderer (with VGA fallback).
pub fn print_to_console(s: &str) {
    {
        let mut renderer = PRIMARY_RENDERER.lock();
        if let Some(ref mut r) = *renderer {
            let _ = r.write_str(s);
        } else {
            let mut vga = VGA_CONSOLE.lock();
            if let Some(ref mut vga) = *vga {
                let _ = core::fmt::write(vga, format_args!("{}", s));
            }
        }
    }
    flush_gpu();
}

/// Helper to write formatted text to the primary renderer (with VGA fallback).
pub fn print_fmt(args: core::fmt::Arguments) {
    {
        let mut renderer = PRIMARY_RENDERER.lock();
        if let Some(ref mut r) = *renderer {
            let _ = core::fmt::write(r, args);
        } else {
            let mut vga = VGA_CONSOLE.lock();
            if let Some(ref mut vga) = *vga {
                let _ = core::fmt::write(vga, args);
            }
        }
    }
    flush_gpu();
}

/// Internal print helper used by boot and other early stages.
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}