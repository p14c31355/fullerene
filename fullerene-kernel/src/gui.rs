//! Lattice‑based GUI subsystem.
//!
//! Owns the global desktop, terminal window, and event dispatcher.
//! Bridges Nozzle output → terminal buffer → Lattice Surface → compositor.

extern crate alloc;

use alloc::vec::Vec;
use lattice::compositor::{Compositor, RenderTarget};
use lattice::desktop::Desktop;
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use petroleum::graphics::Renderer;
use spin::Mutex;

// ── Constants ────────────────────────────────────────────────

/// Terminal grid dimensions.
const TERM_COLS: u32 = 80;
const TERM_ROWS: u32 = 25;

/// Window dimensions in pixels (80×8 = 640, 25×16 = 400).
const TERM_WIN_W: u32 = TERM_COLS * 8;
const TERM_WIN_H: u32 = TERM_ROWS * 16;

/// Desktop background colour.
const BG_COLOR: u32 = 0x1a1a2e;

// ── Global state ─────────────────────────────────────────────

/// Global GUI state, lazily initialised.
pub static GUI: Mutex<Option<GuiState>> = Mutex::new(None);

/// The full GUI state.
pub struct GuiState {
    pub desktop: Desktop,
    pub term_window: WindowId,
    pub term_buf: TerminalBuffer,
    pub cursor_visible: bool,
}

/// Initialise the GUI subsystem.
pub fn init() {
    let mut desktop = Desktop::new(BG_COLOR);
    let term_window = desktop.create_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000);
    let term_buf = TerminalBuffer::new(TERM_COLS, TERM_ROWS);
    *GUI.lock() = Some(GuiState {
        desktop,
        term_window,
        term_buf,
        cursor_visible: true,
    });
}

/// Render the desktop onto the primary framebuffer using Lattice compositor.
pub fn render() {
    let mut gui_lock = GUI.lock();
    let gui = match gui_lock.as_mut() {
        Some(g) => g,
        None => {
            drop(gui_lock);
            render_fallback();
            return;
        }
    };

    // Re-render terminal buffer onto the window's surface
    render_terminal(gui);

    // Get framebuffer memory
    let fb_result = get_framebuffer_slice();
    let (fb_pixels, fb_width, fb_height) = match fb_result {
        Some(t) => t,
        None => return,
    };

    // Composite via Lattice
    let mut target = FramebufferTarget {
        pixels: fb_pixels,
        width: fb_width,
        height: fb_height,
    };
    let scene = gui.desktop.scene();
    Compositor::render(&scene, &mut target);
    drop(gui_lock);

    // Flush GPU if present
    crate::graphics::flush_gpu();
}

/// Render the terminal buffer onto the terminal window's surface.
fn render_terminal(gui: &mut GuiState) {
    let window = match gui.desktop.wm.windows_mut().iter_mut().find(|w| w.id == gui.term_window) {
        Some(w) => w,
        None => return,
    };

    let cells: Vec<LatticeCell> = gui.term_buf.cells().iter().map(|c| LatticeCell {
        ch: c.ch,
        fg: c.fg,
        bg: c.bg,
    }).collect();

    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &cells,
        cols: gui.term_buf.cols(),
        cursor_col: Some(gui.term_buf.cursor_col()),
        cursor_row: Some(gui.term_buf.cursor_row()),
        cursor_visible: gui.cursor_visible,
    });
}

/// Toggle cursor visibility (called by ChronoLine timer).
pub fn toggle_cursor() {
    let mut gui = GUI.lock();
    if let Some(ref mut g) = *gui {
        g.cursor_visible = !g.cursor_visible;
    }
}

// ── Framebuffer access ───────────────────────────────────────

/// Get a mutable slice of the framebuffer pixels and its dimensions.
fn get_framebuffer_slice() -> Option<(&'static mut [u32], u32, u32)> {
    let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    let renderer = renderer_lock.as_ref()?;
    let info = renderer.get_info();

    let phys_offset = petroleum::common::memory::get_physical_memory_offset() as u64;
    let fb_virt = (info.address as u64) + phys_offset;
    let fb_len = (info.width as usize) * (info.height as usize);

    // Safety: framebuffer is mapped at this virtual address
    let fb_pixels = unsafe { core::slice::from_raw_parts_mut(fb_virt as *mut u32, fb_len) };

    Some((fb_pixels, info.width, info.height))
}

// ── Fallback rendering ───────────────────────────────────────

/// Fallback: use petroleum's built-in desktop renderer.
fn render_fallback() {
    let mut renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut renderer) = *renderer_lock {
        petroleum::graphics::draw_os_desktop(renderer);
        renderer.present();
    }
    drop(renderer_lock);
    crate::graphics::flush_gpu();
}

// ── Framebuffer RenderTarget ─────────────────────────────────

struct FramebufferTarget<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
}

impl RenderTarget for FramebufferTarget<'_> {
    fn buffer(&mut self) -> &mut [u32] {
        self.pixels
    }
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

// ── LatticeTerminal ──────────────────────────────────────────

/// A [`nozzle::Terminal`] that writes to the Lattice‑backed terminal buffer.
pub struct LatticeTerminal;

impl nozzle::Terminal for LatticeTerminal {
    fn write_str(&mut self, s: &str) {
        let mut gui = GUI.lock();
        if let Some(ref mut g) = *gui {
            g.term_buf.put_str(s);
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        crate::keyboard::read_char()
    }

    fn input_available(&self) -> bool {
        crate::keyboard::input_available()
    }
}
