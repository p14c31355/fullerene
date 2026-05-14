use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};
use petroleum::graphics::{Console, Renderer, UefiFramebufferWriter};
use petroleum::graphics::text::VgaBuffer;
use petroleum::write_serial_bytes;
use alloc::boxed::Box;
use spin::Mutex;

/// Global primary console — stored as a concrete type (no Box, no allocator).
pub static PRIMARY_CONSOLE: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);
/// Global primary renderer — stored as a concrete type (no Box, no allocator).
pub static PRIMARY_RENDERER: Mutex<Option<UefiFramebufferWriter>> = Mutex::new(None);

/// Guard flag to prevent double initialization of the graphics subsystem.
static GRAPHICS_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Fallback VGA text console (used when UEFI framebuffer is not available).
static VGA_CONSOLE: Mutex<Option<VgaBuffer>> = Mutex::new(None);

/// Initializes the system graphics and primary console.
///
/// This function is idempotent: calling it more than once has no effect.
/// 
/// Priority:
/// 1. GOP Framebuffer (from bootloader config)
/// 2. Fallback GOP detection (QEMU/etc)
/// 3. Legacy VGA Text Mode (fallback)
pub fn init_graphics() {
    // Force reset GRAPHICS_INITIALIZED to handle un-zeroed .bss after world switch.
    // This mirrors the force-reset pattern used for ALLOCATOR, HEAP_INITIALIZED,
    // and LOCAL_APIC_ADDRESS in uefi_init.rs.
    GRAPHICS_INITIALIZED.store(false, Ordering::SeqCst);
    if GRAPHICS_INITIALIZED.swap(true, Ordering::SeqCst) {
        petroleum::debug_log!("Graphics: Already initialized, skipping\n");
        return;
    }

    // 0: Initialize framebuffer config from KernelArgs if not already set.
    // The bootloader (bellows) correctly detects the framebuffer and passes it
    // through KernelArgs. However, since the bootloader and kernel are separate
    // binaries, their copies of FULLERENE_FRAMEBUFFER_CONFIG are different memory
    // locations. We must read from KernelArgs to bridge this gap.
    if petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().is_none() {
        // SAFETY: KERNEL_ARGS is set by efi_main_stage2 before init_common
        // is called, and points to valid memory allocated by the bootloader.
        // It is only read once during early init, before any concurrent access.
        unsafe {
            let args_ptr = petroleum::transition::KERNEL_ARGS;
            if !args_ptr.is_null() {
                let args = &*args_ptr;
                // Validate KernelArgs framebuffer values to detect garbage/uninitialized data.
                // The bootloader should set valid values, but if something went wrong during
                // the bootloader→kernel transition, the fields may contain garbage.
                const MAX_REASONABLE_WIDTH: u32 = 16384;
                const MAX_REASONABLE_HEIGHT: u32 = 16384;
                let fb_valid = args.fb_address >= 0x100000
                    && args.fb_width > 0 && args.fb_width <= MAX_REASONABLE_WIDTH
                    && args.fb_height > 0 && args.fb_height <= MAX_REASONABLE_HEIGHT
                    && (args.fb_bpp == 8 || args.fb_bpp == 16 || args.fb_bpp == 24 || args.fb_bpp == 32);
                if fb_valid {
                    petroleum::debug_log!(
                        "Graphics: Initializing framebuffer from KernelArgs: {}x{} @ {:#x}",
                        args.fb_width, args.fb_height, args.fb_address
                    );
                    // Use checked arithmetic to avoid overflow from garbage values
                    let stride = (args.fb_width as u64)
                        .checked_mul(args.fb_bpp as u64 / 8)
                        .and_then(|s| u32::try_from(s).ok())
                        .unwrap_or(0);
                    let config = petroleum::create_framebuffer_config(
                        args.fb_address,
                        args.fb_width,
                        args.fb_height,
                        petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                        args.fb_bpp,
                        stride,
                    );
                    if stride > 0 {
                        petroleum::FULLERENE_FRAMEBUFFER_CONFIG
                            .call_once(|| spin::Mutex::new(Some(config)));
                        petroleum::debug_log!("Graphics: Framebuffer config initialized from KernelArgs");
                    } else {
                        petroleum::debug_log!("Graphics: KernelArgs framebuffer stride is zero, skipping");
                    }
                } else {
                    petroleum::debug_log!("Graphics: KernelArgs framebuffer values invalid, skipping");
                }
            }
        }
    }

    // 1 & 2: Try GOP / Framebuffer
    let config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG.get().and_then(|mutex| {
        let lock = mutex.lock();
        *lock
    }).or_else(|| petroleum::kernel_fallback_framebuffer_detection());

    if let Some(fb_config) = config {
        petroleum::debug_log!("Initializing graphics: GOP Framebuffer mode");

        // Map framebuffer region into page table before any access.
        // Note: efi_main_stage2 calls UefiInitContext::map_mmio before init_common,
        // which should already have mapped the framebuffer. This call is a safety
        // net in case the early MMIO mapping was skipped or failed.
        let fb_phys = fb_config.address;
        let fb_size = (fb_config.width as u64 * fb_config.height as u64 * fb_config.bpp as u64) / 8;
        let fb_pages = ((fb_size + 4095) / 4096) as usize;
        let fb_virt = fb_phys + petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64;
        let frame_allocator = petroleum::page_table::constants::get_frame_allocator_mut();
        let phys_offset = x86_64::VirtAddr::new(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64);
        let l4 = unsafe { petroleum::page_table::active_level_4_table(phys_offset) };
        // Use NO_CACHE (Uncacheable) for the framebuffer to match MTRR settings
        // set by UEFI firmware for PCI MMIO regions. MTRR/PAT type mismatch would
        // cause #GP on access.
        let flags = x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE
            | x86_64::structures::paging::PageTableFlags::NO_CACHE;
        unsafe {
            for i in 0..fb_pages {
                let v = x86_64::VirtAddr::new(fb_virt + i as u64 * 4096);
                let p = x86_64::PhysAddr::new(fb_phys + i as u64 * 4096);
                // map_page_4k_l1 handles HUGE_PAGE splitting and overwriting
                // existing 4KB entries, so it's safe to call even if map_mmio
                // already mapped these pages.
                petroleum::page_table::kernel::init::map_page_4k_l1(
                    l4, v, p, flags, frame_allocator, phys_offset,
                ).expect("Failed to map framebuffer page");
            }
        }
        // Flush TLB
        let cr3_val = x86_64::registers::control::Cr3::read();
        unsafe { x86_64::registers::control::Cr3::write(cr3_val.0, cr3_val.1); }

        let info = petroleum::graphics::color::FramebufferInfo {
            address: fb_virt,
            width: fb_config.width,
            height: fb_config.height,
            stride: fb_config.stride,
            pixel_format: Some(fb_config.pixel_format),
            colors: petroleum::graphics::color::ColorScheme::UEFI_GREEN_ON_BLACK,
        };

        let writer = petroleum::UefiFramebufferWriter::Uefi32(
            petroleum::graphics::framebuffer::FramebufferWriter::<u32>::new(info)
        );

        let mut writer_mut = writer.clone();
        petroleum::graphics::console::Console::clear(&mut writer_mut);

        *PRIMARY_CONSOLE.lock() = Some(writer.clone());
        *PRIMARY_RENDERER.lock() = Some(writer);

        petroleum::debug_log!("Graphics initialized with GOP Framebuffer");
        return;
    }

    // 3: Fallback to Legacy VGA Text Mode
    petroleum::debug_log!("Initializing graphics: Falling back to Legacy VGA Text Mode");
    // We use a dummy address for init_vga as it's handled internally or via constants
    crate::vga::init_vga_legacy();

    // Initialize VGA text console as fallback display
    let mut vga = VgaBuffer::with_address(
        petroleum::page_table::constants::VGA_MEMORY_START as usize
    );
    vga.enable();
    Console::clear(&mut vga);
    *VGA_CONSOLE.lock() = Some(vga);
}

pub fn set_primary_console(console: UefiFramebufferWriter) {
    *PRIMARY_CONSOLE.lock() = Some(console);
}

pub fn set_primary_renderer(renderer: UefiFramebufferWriter) {
    *PRIMARY_RENDERER.lock() = Some(renderer);
}

/// Helper to write to the primary console (with VGA fallback).
pub fn print_to_console(s: &str) {
    let mut console = PRIMARY_CONSOLE.lock();
    if let Some(ref mut console) = *console {
        let _ = console.write_str(s);
        return;
    }
    drop(console);
    // Fallback to VGA text console
    let mut vga = VGA_CONSOLE.lock();
    if let Some(ref mut vga) = *vga {
        let _ = core::fmt::write(vga, format_args!("{}", s));
    }
}

/// Helper to write formatted text to the primary console (with VGA fallback).
pub fn print_fmt(args: core::fmt::Arguments) {
    let mut console = PRIMARY_CONSOLE.lock();
    if let Some(ref mut console) = *console {
        let _ = core::fmt::write(console, args);
        return;
    }
    drop(console);
    // Fallback to VGA text console
    let mut vga = VGA_CONSOLE.lock();
    if let Some(ref mut vga) = *vga {
        let _ = core::fmt::write(vga, args);
    }
}

/// Internal print helper used by boot and other early stages.
pub fn _print(args: core::fmt::Arguments) {
    print_fmt(args);
}

// Re-export desktop drawing
pub use petroleum::graphics::draw_os_desktop;

// Re-export color conversion utility
pub use petroleum::graphics::color::u32_to_rgb888;