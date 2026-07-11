#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![cfg_attr(not(test), feature(alloc_error_handler))]
#![allow(unused_features)]
extern crate alloc;

// ── Panic-screen framebuffer drawing (no alloc, no locks) ─────────────
//
// Snapshot the stored FB params once, then draw directly to the
// identity-mapped or higher-half VA.  This runs in the panic handler
// and must not allocate, lock, or dereference any pointer that might
// itself have caused the panic.
#[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
mod panic_screen {
    /// Fill the framebuffer with a diagnostic color and return the boot stage.
    ///
    /// Color encodes the **last reached boot stage** so that even without
    /// serial or a GPU driver, a single glance at the screen tells you
    /// how far the kernel got before panicking.
    ///
    /// | Stage | Screen color  | Meaning                   |
    /// |-------|---------------|---------------------------|
    /// | 1     | dark blue     | KernelEntry               |
    /// | 2     | blue          | MemoryMapped              |
    /// | 3     | cyan          | HeapReady                 |
    /// | 4     | dark green    | InterruptsReady           |
    /// | 5     | green         | KernelContextReady        |
    /// | 6     | yellow-green  | PciBarsReady              |
    /// | 7     | yellow        | GraphicsReady             |
    /// | 8     | orange        | InputReady                |
    /// | 9     | dark orange   | ProcessReady              |
    /// | 10    | red           | SyscallReady              |
    /// | 11    | dark red      | FilesystemReady           |
    /// | 12    | magenta       | LoaderReady               |
    /// | 13    | pink          | GuiReady                  |
    /// | 14    | purple        | TaskManagerReady          |
    /// | 15    | gray          | AppRunnerReady            |
    /// | 255   | bright red    | **PANIC** (no init stage) |
    pub fn draw() {
        // ── 1. Physical FB address from the linker-section snapshot ──
        let phys = unsafe { crate::graphics::discovery::STORED_FB_PHYS };
        let w    = unsafe { crate::graphics::discovery::STORED_FB_WIDTH };
        let h    = unsafe { crate::graphics::discovery::STORED_FB_HEIGHT };
        let stride_raw = unsafe { crate::graphics::discovery::STORED_FB_STRIDE };

        if !(0x100_000..1 << 52).contains(&phys)
            || !(80..=16_384).contains(&w)
            || !(25..=16_384).contains(&h)
            || stride_raw < w.saturating_mul(4)
            || stride_raw % 4 != 0
        {
            return;
        }
        let stride = usize::try_from(stride_raw).unwrap_or(w as usize * 4);

        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        let Some(fb_va) = phys.checked_add(off) else { return };

        // ── 2. Choose colour based on last boot stage ──
        let stage = crate::boot_stage::last_stage().map(|s| s as u8).unwrap_or(0);
        let color: u32 = stage_color(stage);

        // ── 3. Fill visible area ──
        // SAFETY: bootstrap installed a dedicated WC mapping for this range.
        let fb_ptr = fb_va as *mut u32;
        let total_pixels = stride / 4 * (h as usize);
        for i in 0..total_pixels {
            unsafe { core::ptr::write_volatile(fb_ptr.add(i), color) };
        }

        // ── 4. Encode stage number as a 1-pixel-wide bar at the top ──
        // Each stage gets a unique column index (stage * 8) so you can
        // count columns from the left to determine the stage even when
        // colours look similar.
        let cols = (w as usize).min(total_pixels).min(256);
        for col in 0..cols {
            let idx = col;
            if idx < total_pixels {
                unsafe {
                    core::ptr::write_volatile(fb_ptr.add(idx), 0x00FFFFFF); // white
                }
            }
        }
        // Dark band over the stage's column
        let stage_col = (stage as usize).saturating_mul(8).min(cols.saturating_sub(1));
        for row in 0..8.min(h as usize) {
            let idx = row * (stride / 4) + stage_col;
            if idx < total_pixels {
                unsafe {
                    core::ptr::write_volatile(fb_ptr.add(idx), 0x00000000); // black
                }
            }
        }
        unsafe { core::arch::x86_64::_mm_sfence() };
    }

    /// Return a unique colour for each boot stage (0 = panic before any stage).
    fn stage_color(stage: u8) -> u32 {
        // BGR encoding: 0x00BBGGRR
        match stage {
            0   => 0x00_00_00_FF, // bright red      – crash before any stage
            1   => 0x00_00_00_44, // dark blue
            2   => 0x00_00_00_88, // blue
            3   => 0x00_88_88_00, // cyan
            4   => 0x00_00_44_00, // dark green
            5   => 0x00_00_88_00, // green
            6   => 0x00_00_88_44, // yellow-green
            7   => 0x00_00_88_88, // yellow
            8   => 0x00_00_44_88, // orange
            9   => 0x00_00_00_88, // dark orange
            10  => 0x00_00_00_AA, // red
            11  => 0x00_00_00_55, // dark red
            12  => 0x00_88_00_88, // magenta
            13  => 0x00_44_00_88, // pink
            14  => 0x00_44_00_44, // purple
            15  => 0x00_55_55_55, // gray
            _   => 0x00_FF_00_FF, // bright magenta  – unknown stage
        }
    }
}

// ---- Custom panic handler (replaces petroleum::define_panic_handler!) ----
#[cfg(all(any(target_os = "none", target_os = "uefi"), not(test)))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crate::boot_stage::set_boot_stage(crate::boot_stage::BootStage::Panic);

    // ── Draw diagnostic to framebuffer / VGA ──
    panic_screen::draw();

    // ── Also write to serial (when available) ──
    petroleum::serial::_print(format_args!("\n========== KERNEL PANIC ==========\n"));
    if let Some(loc) = info.location() {
        petroleum::serial::_print(format_args!(
            "  at {}:{}:{}\n",
            loc.file(),
            loc.line(),
            loc.column()
        ));
    }
    petroleum::serial::_print(format_args!("  {}\n", info));
    petroleum::serial::_print(format_args!("==================================\n"));

    loop {
        x86_64::instructions::hlt();
    }
}

// ── Host-target panic handler (enables `cargo check` on Linux) ──
#[cfg(not(any(target_os = "none", target_os = "uefi")))]
#[panic_handler]
fn panic_host(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

petroleum::define_alloc_error_handler!();

// Exported globals
pub use heap::MEMORY_MAP;

// Module declarations
// ── Drivers (storage, GPU, network) ───────────────────────────────
pub mod drivers;

// ── WiFi service (registered with Solvent at runtime) ──────────────

// ── DriverContext bridge (kernel → nitrogen) ──────────────────────
pub mod driver_context_impl;

// ── Plugin registry ───────────────────────────────────────────────
pub mod plugin;

// ── DevFs ─────────────────────────────────────────────────────────
pub mod devfs;

// ── Kernel core ────────────────────────────────────────────────────
pub mod boot;
pub mod boot_stage;
pub mod context_switch;
pub mod contexts;
pub mod fs;
pub mod gdt;
pub mod graphics;
pub mod gui;
pub mod hardware;
pub mod heap;
pub mod init;
pub mod initramfs;
pub mod interrupts;
pub mod klog;
pub mod loader;
pub mod memory_management;
pub mod process;
pub mod scheduler;
pub mod scheduler_context;
pub mod shell;
pub mod slab;
pub mod syscall;
pub mod task;
// tracing.rs is now a thin re-export of resonance::tracing
pub mod linux;
pub mod vdso;
pub mod vfs;
