/// Generic border rectangle drawing
#[macro_export]
macro_rules! draw_border_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $fill_color:expr, $stroke_color:expr, $stroke_width:expr) => {{
        use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
        let rect = Rectangle::new(Point::new($x, $y), Size::new($w, $h));
        let style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($fill_color))
            .stroke_color($crate::u32_to_rgb888($stroke_color))
            .stroke_width($stroke_width)
            .build();
        rect.into_styled(style).draw($writer).ok();
    }};
}

/// Generic window drawing macro for desktop elements to reduce boilerplate
#[macro_export]
macro_rules! draw_window_shell {
    ($writer:expr, $x:expr, $y:expr, $width:expr, $height:expr, $title:expr, $content:block) => {{
        $crate::draw_window_base!($writer, $x, $y, $width, $height, $title);
        $content
    }};
}

/// Base window drawing macro
#[macro_export]
macro_rules! draw_window_base {
    ($writer:expr, $x:expr, $y:expr, $width:expr, $height:expr, $title:expr) => {{
        use embedded_graphics::mono_font::{MonoTextStyle, ascii::FONT_6X10};
        use embedded_graphics::primitives::{PrimitiveStyleBuilder, Rectangle};
        use embedded_graphics::{prelude::!, text::Text};

        let rect = Rectangle::new(Point::new($x as i32, $y as i32), Size::new($width, $height));
        let style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($crate::COLOR_WHITE))
            .stroke_color($crate::u32_to_rgb888($crate::COLOR_BLACK))
            .stroke_width(2)
            .build();
        rect.into_styled(style).draw($writer).ok();

        let title_rect = Rectangle::new(Point::new($x as i32, $y as i32), Size::new($width, 25));
        let title_style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($crate::COLOR_DARK_GRAY))
            .build();
        title_rect.into_styled(title_style).draw($writer).ok();

        let title_text_style =
            MonoTextStyle::new(&FONT_6X10, $crate::u32_to_rgb888($crate::COLOR_BLACK));
        let title_width = $crate::calc_text_width($title);
        let title_x = $x as i32 + (($width as i32 / 2) - (title_width as i32 / 2));
        Text::new($title, Point::new(title_x, $y as i32 + 8), title_text_style)
            .draw($writer)
            .ok();

        let content_rect = Rectangle::new(
            Point::new($x as i32 + 5, $y as i32 + 30),
            Size::new($width.saturating_sub(10), $height.saturating_sub(35)),
        );
        let content_style = PrimitiveStyleBuilder::new()
            .fill_color($crate::u32_to_rgb888($crate::COLOR_WINDOW_BG))
            .build();
        content_rect.into_styled(content_style).draw($writer).ok();
    }};
}

pub mod color;
pub mod console;
pub mod constants;
pub mod desktop;
pub mod framebuffer;
pub mod registers;
pub mod renderer;
pub mod setup;
pub mod text;
pub mod uefi;

// VGA constants
pub use constants::*;

// Re-exports for public API
pub use crate::hardware::ports::{HardwarePorts, PortWriter, VgaPortOps};
pub use color::*;
pub use console::Console;
pub use renderer::Renderer;
// VGA graphics modes
pub use setup::{
    detect_and_init_vga_graphics, detect_cirrus_vga, init_vga_graphics, init_vga_text_mode,
    setup_cirrus_vga_mode,
};
// VGA text operations
pub use desktop::*;
pub use framebuffer::UefiFramebufferWriter;
pub use framebuffer::*;
pub use text::{Color, ColorCode, ScreenChar, TextBufferOperations};

/// Result of the graphics drawing test
#[derive(Debug, PartialEq)]
pub enum DrawingTestResult {
    Pass,
    Fail(&'static str),
}

/// Trace macro for drawtest: logs a formatted message prefixed with file:line.
/// Use like: drawtest_trace!("step 1: writing pixel at ({}, {})", x, y);
#[macro_export]
macro_rules! drawtest_trace {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!(
            "[{}:{}] DRAW_TEST {}\n",
            core::file!(),
            core::line!(),
            format_args!($($arg)*),
        ))
    };
}

/// Check a single pixel readback in the drawing test. On mismatch, logs the
/// exact file/line and returns `Err` with a descriptive message.
macro_rules! check_pixel_eq {
    ($label:expr, $actual:expr, $expected:expr, $x:expr, $y:expr) => {{
        if $actual != $expected {
            $crate::serial::_print(format_args!(
                "[{}:{}] DRAW_TEST FAIL at pixel ({}, {}): {} expected={:#010x}, actual={:#010x}\n",
                core::file!(),
                core::line!(),
                $x,
                $y,
                $label,
                $expected,
                $actual,
            ));
            return Err($label);
        }
    }};
}

/// Helper: perform a volatile write. `drawtest_trace!` has already printed the
/// file:line location *before* this is invoked, so if the write causes a fault,
/// the exception handler's output will appear immediately after the last
/// `drawtest_trace!` line — the user can correlate them by looking at the
/// serial log.  No lock/atomic shared state is needed.
macro_rules! probe_write {
    ($addr:expr, $val:expr) => {{
        unsafe {
            core::ptr::write_volatile($addr, $val);
            core::arch::asm!("mfence", options(nostack, preserves_flags));
        }
    }};
}

/// Helper: perform a volatile read with `drawtest_trace!` prefix.
macro_rules! probe_read {
    ($addr:expr) => {{
        let __v;
        unsafe {
            __v = core::ptr::read_volatile($addr);
        }
        __v
    }};
}

/// Diagnose framebuffer accessibility.
///
/// Each MMIO access is wrapped with `probe_write_ok!` / `probe_read_ok!` so
/// that if a page-fault or #GP occurs, the exception handler can read
/// `PROBE_MARKER` and report the exact file:line where the fault happened.
///
/// See `fullerene-kernel/src/interrupts/exceptions.rs` for the handler side.
pub fn verify_drawing_test(config: &crate::graphics::color::FramebufferInfo) -> DrawingTestResult {
    let fb_virt = config.address;
    let w = config.width;
    let h = config.height;

    let test_color: u32 = 0xDEADBEEF;

    let r = (|| -> Result<(), &'static str> {
        // ── Step 0: sanity-check volatile readback on stack ────────────
        drawtest_trace!("Step 0: verify volatile readback works on normal memory");
        {
            let mut scratch: u32 = 0;
            let sp = &mut scratch as *mut u32;
            probe_write!(sp, test_color);
            let val = probe_read!(sp);
            check_pixel_eq!("stack-volatile", val, test_color, 0, 0);
        }
        drawtest_trace!("Step 0 passed");

        // ── Step 1: probe write to framebuffer origin ──────────────────
        drawtest_trace!("Step 1: probe writing {:#010x} to FB@(0,0)", test_color);
        let fb_ptr = fb_virt as *mut u32;
        probe_write!(fb_ptr, test_color);
        drawtest_trace!("Step 1: write OK (no fault)");

        // ── Step 2: probe readback from framebuffer origin ─────────────
        drawtest_trace!("Step 2: probe readback from FB@(0,0)");
        let read_val = probe_read!(fb_ptr);
        drawtest_trace!("Step 2: readback = {:#010x}", read_val);
        if read_val == test_color {
            drawtest_trace!("Step 2: readback matches!  Framebuffer fully accessible.");
            return Ok(());
        }

        // ── Step 3: probe write to top-right corner ────────────────────
        drawtest_trace!(
            "Step 3: probe write to top-right ({}, 0)",
            w.saturating_sub(1)
        );
        let tr_off = config.calculate_offset(w.saturating_sub(1), 0);
        let tr_ptr = unsafe { ((fb_virt as *mut u8).add(tr_off)) as *mut u32 };
        probe_write!(tr_ptr, 0xCAFEBABEu32);
        drawtest_trace!("Step 3: write to top-right completed");

        // ── Step 4: probe write to bottom-left corner ──────────────────
        drawtest_trace!(
            "Step 4: probe write to bottom-left (0, {})",
            h.saturating_sub(1)
        );
        let bl_off = config.calculate_offset(0, h.saturating_sub(1));
        let bl_ptr = unsafe { ((fb_virt as *mut u8).add(bl_off)) as *mut u32 };
        probe_write!(bl_ptr, 0xF00DBABEu32);
        drawtest_trace!("Step 4: write to bottom-left completed");

        // ── Step 5: try sfence + readback one more time ────────────────
        // (in case the first write primed the WC buffer)
        drawtest_trace!("Step 5: write + sfence + readback (retry)");
        {
            probe_write!(fb_ptr, test_color);
            unsafe {
                core::arch::asm!("sfence", options(nostack, preserves_flags));
            }
            let v = probe_read!(fb_ptr);
            drawtest_trace!("Step 5: retry readback = {:#010x}", v);
            if v == test_color {
                drawtest_trace!("Step 5: passed after retry");
                return Ok(());
            }
        }

        // ── Step 6: wbinvd attempt ─────────────────────────────────────
        drawtest_trace!("Step 6: write + wbinvd + readback");
        {
            probe_write!(fb_ptr, test_color);
            unsafe {
                core::arch::asm!("wbinvd", options(nostack, preserves_flags));
            }
            let v = probe_read!(fb_ptr);
            drawtest_trace!("Step 6: wbinvd readback = {:#010x}", v);
            if v == test_color {
                drawtest_trace!("Step 6: passed after wbinvd");
                return Ok(());
            }
        }

        // ── Step 7: scan nearby pixels for stray non-zero ──────────────
        drawtest_trace!("Step 7: scan first 16 FB pixels for non-zero");
        {
            for i in 0..16u32 {
                let v = probe_read!(fb_ptr.add(i as usize));
                if v != 0 {
                    drawtest_trace!("Step 7: pixel[{}] = {:#010x}", i, v);
                }
            }
        }

        // ── Final diagnosis ────────────────────────────────────────────
        drawtest_trace!(
            "DIAGNOSIS: FB@{:#x} ({:?}) — all writes completed without fault, \
             but readback always returns 0.  The mapping is PRESENT+WRITABLE \
             but the region is write-only (QEMU std-vga PCI MMIO).  Untouched \
             pixels are also 0.",
            fb_virt,
            config.pixel_format
        );
        drawtest_trace!("SUGGESTION: try PAT=WB (vs NO_CACHE/UC) or confirm physical address.");
        Err("write OK but readback always 0")
    })();

    match r {
        Ok(()) => {
            drawtest_trace!("all checks passed");
            DrawingTestResult::Pass
        }
        Err(m) => {
            drawtest_trace!("DIAGNOSTIC: {}", m);
            DrawingTestResult::Fail(m)
        }
    }
}
