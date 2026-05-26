//! Solvent — Runtime / Orchestration Layer
//!
//! Solvent is the orchestration/runtime layer that sits between the kernel
//! and the higher-level subsystems (Lattice, Nozzle, Resonance, ChronoLine).
//!
//! # Architecture
//!
//! ```text
//! Kernel → Solvent → Lattice / Nozzle / Resonance / ChronoLine
//! ```
//!
//! Solvent owns:
//! - runtime coordination
//! - subsystem bootstrap
//! - event loop orchestration
//! - service ownership
//! - subsystem wiring
//! - frame/update pacing
//! - input polling (hardware → Resonance events)
//!
//! Solvent does NOT own:
//! - raw hardware access (→ Nitrogen)
//! - memory management (→ Kernel)
//! - process scheduling (→ Kernel)
//! - interrupt handling (→ Kernel)
//!
//! # Event Flow
//!
//! ```text
//! Hardware IRQ → raw buffers (keyboard scancode, mouse PS/2)
//! Solvent tick → poll_input_events() → Resonance EventQueue
//!             → process_events() → handlers
//!                 ├─ WmEventHandler (mouse → desktop state)
//!                 └─ TerminalInputHandler (keyboard → terminal)
//! Solvent tick → render() → Compositor → framebuffer
//! ```

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use chronoline::{ChronoLine, Deadline, TimerId, TimerMode};
use lattice::compositor::{Compositor, RenderTarget};
use lattice::desktop::Desktop;
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use resonance::{Dispatcher, Event, EventHandler, EventQueue, InputEvent, KeyCode, MouseButton};
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

/// Cursor blink interval in scheduler ticks (~500 ms at ~200 Hz).
const CURSOR_BLINK_INTERVAL: u64 = 100;

/// Timer ID for cursor blink.
const CURSOR_TIMER_ID: TimerId = TimerId(1);

/// Mouse delta sensitivity multiplier.
///
/// PS/2 mouse deltas are typically ±1–3 units per packet, which results
/// in unnoticeable cursor movement at 1:1 mapping.  Multiply by this
/// factor to get usable pixel displacement (1×12=12 px to 3×12=36 px/tick).
const MOUSE_SENSITIVITY: i16 = 12;

/// Minimum ticks between rendered frames (2 ticks at ~200 Hz ≈ 100 fps).
///
/// With double‑buffering the compositor's intermediate states are never
/// visible, so we can render at high frequency without flicker.
const FRAME_INTERVAL_TICKS: u64 = 2;

/// Timer ID for frame pacing.
const FRAME_TIMER_ID: TimerId = TimerId(2);

/// Maximum framebuffer size supported for double‑buffering.
///
/// 1280×800 = 1 024 000 pixels.  The back‑buffer lives in BSS so it
/// costs zero heap and incurs no allocator pressure.
const MAX_FB_PIXELS: usize = 1920 * 1080;

// ── Static back‑buffer (BSS, zero heap pressure) ──────────────

/// Off‑screen back‑buffer for double‑buffering.
///
/// The compositor renders here first, then only the changed region is
/// copied to the scan‑out framebuffer.  This avoids exposing intermediate
/// states (e.g. the background‑fill stage) to the display controller,
/// which is the root cause of visible flickering.
///
/// Stored in BSS (zero‑initialised by the bootloader) so it never touches
/// the kernel heap.  The `Mutex` guard protects against concurrent access
/// from interrupt handlers.
static BACK_BUFFER: Mutex<[u32; MAX_FB_PIXELS]> = Mutex::new([0u32; MAX_FB_PIXELS]);

// ── Runtime state ────────────────────────────────────────────

/// Global runtime state (desktop, terminal, timers).
static RUNTIME: Mutex<Option<RuntimeState>> = Mutex::new(None);

/// Event queue and dispatcher — separate from RUNTIME to avoid deadlock.
/// Handlers access RUNTIME, so dispatch must NOT hold the RUNTIME lock.
/// Wrapped in Option because EventQueue/Dispatcher have non‑const `new`.
static EVENT_QUEUE: Mutex<Option<EventQueue>> = Mutex::new(None);
static DISPATCHER: Mutex<Option<Dispatcher>> = Mutex::new(None);

/// Previous mouse button state for edge detection.
static PREV_MOUSE_BUTTONS: Mutex<u8> = Mutex::new(0);

/// The full runtime state, owned by Solvent.
pub struct RuntimeState {
    pub desktop: Desktop,
    pub term_window: WindowId,
    pub term_buf: TerminalBuffer,
    pub chrono: ChronoLine,
    pub cursor_visible: bool,
    /// Whether a new frame is due for rendering (set by the frame‑pacing timer).
    pub frame_due: bool,
    /// Number of valid pixels in [`BACK_BUFFER`] (= fb_width × fb_height).
    pub back_len: usize,
    /// Reusable cell buffer for the terminal surface renderer.
    ///
    /// Owned here rather than in a static so it can be reset on re‑init
    /// and avoids global‑state coupling with `render_terminal`.
    pub term_cells: Vec<LatticeCell>,
}

/// Initialise the Solvent runtime subsystem.
///
/// Creates the desktop, terminal window, event dispatcher, and timer
/// infrastructure.  The back‑buffer is already allocated in BSS.
pub fn init() {
    let mut desktop = Desktop::new(BG_COLOR);
    let term_window = desktop.wm.create_titled_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
    let term_buf = TerminalBuffer::new(TERM_COLS, TERM_ROWS);
    let mut dispatcher = Dispatcher::new();
    let mut chrono = ChronoLine::new();

    // Register repeating cursor blink timer using TimerMode::Repeating
    chrono.register_with_mode(
        Deadline::new(CURSOR_BLINK_INTERVAL),
        CURSOR_TIMER_ID,
        TimerMode::Repeating {
            interval_ticks: CURSOR_BLINK_INTERVAL,
        },
    );

    // Register event handlers
    dispatcher.register(Box::new(WmEventHandler));
    dispatcher.register(Box::new(TerminalInputHandler));

    *EVENT_QUEUE.lock() = Some(EventQueue::new());
    *DISPATCHER.lock() = Some(dispatcher);
    // Register frame-pacing repeating timer so we don't re-render at CPU speed.
    chrono.register_with_mode(
        Deadline::new(FRAME_INTERVAL_TICKS),
        FRAME_TIMER_ID,
        TimerMode::Repeating {
            interval_ticks: FRAME_INTERVAL_TICKS,
        },
    );

    *RUNTIME.lock() = Some(RuntimeState {
        desktop,
        term_window,
        term_buf,
        chrono,
        cursor_visible: true,
        frame_due: true,
        back_len: 0,
        term_cells: Vec::new(),
    });
}

/// Check if the runtime has been initialised.
pub fn is_initialized() -> bool {
    RUNTIME.lock().is_some()
}

// ── Event handlers ───────────────────────────────────────────

/// Handles mouse events for the window manager (desktop cursor, dragging).
struct WmEventHandler;

impl EventHandler for WmEventHandler {
    fn handle(&mut self, event: &Event) -> bool {
        let mut rt = RUNTIME.lock();
        let rt = match rt.as_mut() {
            Some(r) => r,
            None => return false,
        };

        match event {
            Event::Input(InputEvent::MouseMove { x, y }) => {
                rt.desktop.mouse_move(*x, *y);
                true // consumed
            }
            Event::Input(InputEvent::MouseDown(_btn)) => {
                rt.desktop
                    .set_cursor(rt.desktop.cursor.x, rt.desktop.cursor.y);
                rt.desktop.mouse_down();
                true
            }
            Event::Input(InputEvent::MouseUp(_btn)) => {
                rt.desktop.mouse_up();
                true
            }
            _ => false,
        }
    }
}

/// Handles keyboard events for the terminal buffer.
struct TerminalInputHandler;

impl EventHandler for TerminalInputHandler {
    fn handle(&mut self, event: &Event) -> bool {
        let mut rt = RUNTIME.lock();
        let rt = match rt.as_mut() {
            Some(r) => r,
            None => return false,
        };

        match event {
            Event::Input(InputEvent::KeyDown(key)) => {
                if let Some(ascii) = keycode_to_ascii(*key) {
                    rt.term_buf
                        .put_str(core::str::from_utf8(&[ascii]).unwrap_or(""));
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

/// Convert a Resonance KeyCode to an ASCII byte (or None if non-printable).
fn keycode_to_ascii(key: KeyCode) -> Option<u8> {
    match key {
        KeyCode::Enter => Some(b'\n'),
        KeyCode::Space => Some(b' '),
        KeyCode::Backspace => Some(0x08),
        KeyCode::Tab => Some(b'\t'),
        KeyCode::A => Some(b'a'),
        KeyCode::B => Some(b'b'),
        KeyCode::C => Some(b'c'),
        KeyCode::D => Some(b'd'),
        KeyCode::E => Some(b'e'),
        KeyCode::F => Some(b'f'),
        KeyCode::G => Some(b'g'),
        KeyCode::H => Some(b'h'),
        KeyCode::I => Some(b'i'),
        KeyCode::J => Some(b'j'),
        KeyCode::K => Some(b'k'),
        KeyCode::L => Some(b'l'),
        KeyCode::M => Some(b'm'),
        KeyCode::N => Some(b'n'),
        KeyCode::O => Some(b'o'),
        KeyCode::P => Some(b'p'),
        KeyCode::Q => Some(b'q'),
        KeyCode::R => Some(b'r'),
        KeyCode::S => Some(b's'),
        KeyCode::T => Some(b't'),
        KeyCode::U => Some(b'u'),
        KeyCode::V => Some(b'v'),
        KeyCode::W => Some(b'w'),
        KeyCode::X => Some(b'x'),
        KeyCode::Y => Some(b'y'),
        KeyCode::Z => Some(b'z'),
        KeyCode::Digit0 => Some(b'0'),
        KeyCode::Digit1 => Some(b'1'),
        KeyCode::Digit2 => Some(b'2'),
        KeyCode::Digit3 => Some(b'3'),
        KeyCode::Digit4 => Some(b'4'),
        KeyCode::Digit5 => Some(b'5'),
        KeyCode::Digit6 => Some(b'6'),
        KeyCode::Digit7 => Some(b'7'),
        KeyCode::Digit8 => Some(b'8'),
        KeyCode::Digit9 => Some(b'9'),
        _ => None,
    }
}

// ── Input polling ────────────────────────────────────────────

/// Mouse state structure (re-exported for kernel access).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MouseState {
    pub x: i16,
    pub y: i16,
    pub buttons: u8,
}

/// Global mouse state used by the kernel interrupt handler.
/// Initialised at screen centre to match the Desktop cursor start position.
pub static MOUSE_STATE: Mutex<MouseState> = Mutex::new(MouseState {
    x: 512,
    y: 384,
    buttons: 0,
});

/// Poll hardware mouse state and inject Resonance events.
///
/// Called from the runtime loop. Reads the PS/2 mouse state from the
/// Nitrogen driver (which processes packets in the interrupt handler)
/// and accumulates deltas into the absolute-position state.
/// Generates MouseMove / MouseDown / MouseUp events with edge detection.
pub fn poll_mouse_state() {
    // Read latest PS/2 mouse data and accumulate into absolute position.
    {
        let ps2_state = nitrogen::ps2::mouse::consume_state();
        let dx = ps2_state.get_x();
        let dy = ps2_state.get_y();
        let btn = nitrogen::ps2::mouse::mouse_buttons();

        let mut mouse = MOUSE_STATE.lock();
        mouse.x = mouse.x.wrapping_add(dx.wrapping_mul(MOUSE_SENSITIVITY));
        // PS/2 convention: +Y = up;  screen convention: +Y = down.  Negate.
        mouse.y = mouse.y.wrapping_add(dy.wrapping_mul(MOUSE_SENSITIVITY).wrapping_neg());
        mouse.buttons = btn;
    }

    // Always push MouseMove so the compositor draws the cursor at its tracked
    // position, even when no PS/2 packets have arrived yet.
    {
        let mouse = MOUSE_STATE.lock();
        let cx = mouse.x as i32;
        let cy = mouse.y as i32;
        let buttons = mouse.buttons;
        drop(mouse);

        if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
            queue.push(Event::Input(InputEvent::MouseMove { x: cx, y: cy }));
        }

        // Edge detection for button state changes
        let mut prev_btn = PREV_MOUSE_BUTTONS.lock();
        let prev = *prev_btn;
        if buttons != prev {
            let mut eq_lock = EVENT_QUEUE.lock();
            if let Some(ref mut queue) = *eq_lock {
                // Left button (bit 0)
                if (buttons & 0x01) != 0 && (prev & 0x01) == 0 {
                    queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
                } else if (buttons & 0x01) == 0 && (prev & 0x01) != 0 {
                    queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Left)));
                }

                // Right button (bit 1)
                if (buttons & 0x02) != 0 && (prev & 0x02) == 0 {
                    queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Right)));
                } else if (buttons & 0x02) == 0 && (prev & 0x02) != 0 {
                    queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Right)));
                }

                // Middle button (bit 2)
                if (buttons & 0x04) != 0 && (prev & 0x04) == 0 {
                    queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Middle)));
                } else if (buttons & 0x04) == 0 && (prev & 0x04) != 0 {
                    queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Middle)));
                }
            }
        }
        *prev_btn = buttons;
    }
}

// ── ChronoLine tick ──────────────────────────────────────────

/// Advance the ChronoLine clock and process expired timers.
///
/// Called from the runtime loop every tick.
pub fn chrono_tick(now: u64) {
    let mut rt = RUNTIME.lock();
    let rt = match rt.as_mut() {
        Some(r) => r,
        None => return,
    };

    rt.chrono.tick(now);

    while let Some(timer) = rt.chrono.pop_expired() {
        match timer.id {
            CURSOR_TIMER_ID => {
                rt.cursor_visible = !rt.cursor_visible;
            }
            FRAME_TIMER_ID => {
                rt.frame_due = true;
            }
            _ => {}
        }
    }
}

// ── Event processing ─────────────────────────────────────────

/// Push a key event into the Resonance event queue.
pub fn push_key_event(event: Event) {
    if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
        queue.push(event);
    }
}

/// Process pending Resonance events (called from runtime loop).
///
/// **IMPORTANT**: This function does NOT hold the RUNTIME lock while
/// dispatching events. Handlers acquire RUNTIME themselves.  If we held
/// RUNTIME here, spin::Mutex would deadlock because handlers try to lock
/// RUNTIME too (single‑core, no preemption).
pub fn process_events() {
    let mut disp_lock = DISPATCHER.lock();
    let mut queue_lock = EVENT_QUEUE.lock();
    if let Some(ref mut dispatcher) = *disp_lock {
        if let Some(ref mut queue) = *queue_lock {
            dispatcher.dispatch_queue(queue);
        }
    }
}

// ── Rendering ────────────────────────────────────────────────

/// Framebuffer render target adapter.
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

/// Render the desktop onto the primary framebuffer using Lattice compositor.
///
/// Uses the static [`BACK_BUFFER`] for double‑buffering with partial blit:
/// the compositor returns the bounding box of the changed region, and only
/// that region is copied to the scan‑out framebuffer.  This reduces the
/// per‑frame memory bandwidth from ~8 MiB (1920×1080) to a few KiB for
/// small updates (e.g. terminal typing).
///
/// `framebuffer_fn` provides access to the kernel's framebuffer slice.
pub fn render<F>(framebuffer_fn: F)
where
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32)>,
{
    let mut rt_lock = RUNTIME.lock();
    let rt = match rt_lock.as_mut() {
        Some(r) => r,
        None => {
            drop(rt_lock);
            return;
        }
    };

    // Re-render terminal buffer onto the window's surface
    render_terminal(rt);

    // Update taskbar entries before building the scene
    rt.desktop.update_taskbar();

    // Get framebuffer memory via the caller-provided closure
    let fb_result = framebuffer_fn();
    let (fb_pixels, fb_width, fb_height) = match fb_result {
        Some(t) => t,
        None => return,
    };

    let fb_len = (fb_width as usize) * (fb_height as usize);
    if fb_len > MAX_FB_PIXELS {
        // Framebuffer too large for the static back‑buffer — skip rendering.
        return;
    }

    // Update the back‑buffer length so the blit stage uses the correct slice.
    rt.back_len = fb_len;

    // ── 1. Composite into the BSS back‑buffer ───────────────
    let (bx, by, bw, bh) = {
        let mut back = BACK_BUFFER.lock();
        let mut back_target = FramebufferTarget {
            pixels: &mut back[..fb_len],
            width: fb_width,
            height: fb_height,
        };
        let scene = rt.desktop.scene();
        Compositor::render(&scene, &mut back_target)
    };

    // ── 2. Partial blit only the changed region ──────────
    if bw > 0 && bh > 0 {
        let back = BACK_BUFFER.lock();
        let fb_w = fb_width as usize;
        let b_w = bw as usize;
        for row in 0..bh {
            let src_off = ((by + row) as usize) * fb_w + (bx as usize);
            let dst_off = ((by + row) as usize) * fb_w + (bx as usize);
            let len = b_w.min(fb_len.saturating_sub(dst_off));
            if len > 0 {
                fb_pixels[dst_off..dst_off + len]
                    .copy_from_slice(&back[src_off..src_off + len]);
            }
        }
    }
}

/// Render the terminal buffer onto the terminal window's surface.
///
/// Uses `rt.term_cells` as a reusable buffer to avoid per‑frame heap
/// allocations (2000 cells × 100 fps = 200 k allocations/sec).
fn render_terminal(rt: &mut RuntimeState) {
    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == rt.term_window)
    {
        Some(w) => w,
        None => return,
    };

    let term_buf = &rt.term_buf;
    let total = (term_buf.cols() * term_buf.rows()) as usize;

    if rt.term_cells.len() != total {
        rt.term_cells.resize(total, LatticeCell { ch: b' ', fg: 0, bg: 0 });
    }
    for (i, c) in term_buf.cells().iter().enumerate() {
        if i < rt.term_cells.len() {
            rt.term_cells[i] = LatticeCell { ch: c.ch, fg: c.fg, bg: c.bg };
        }
    }

    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &rt.term_cells,
        cols: rt.term_buf.cols(),
        cursor_col: Some(rt.term_buf.cursor_col()),
        cursor_row: Some(rt.term_buf.cursor_row()),
        cursor_visible: rt.cursor_visible,
    });
}

// ── LatticeTerminal (nozzle::Terminal impl) ──────────────────

/// A [`nozzle::Terminal`] that writes to the Lattice‑backed terminal buffer.
///
/// `read_byte()` does NOT spin‑block: while waiting for keyboard input it
/// services the runtime (poll input, advance timers, process events, render)
/// so the desktop stays responsive and the scheduler loop keeps running.
pub struct LatticeTerminal;

impl nozzle::Terminal for LatticeTerminal {
    fn write_str(&mut self, s: &str) {
        let mut rt = RUNTIME.lock();
        if let Some(ref mut r) = *rt {
            r.term_buf.put_str(s);
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        // Loop until a keystroke is available, servicing the runtime on each
        // iteration so the desktop stays alive (mouse updates, cursor blink,
        // event processing, rendering).  Never returns `None` — the caller
        // (line editor) treats `None` as EOF, which would cause an infinite
        // prompt-redraw loop.
        loop {
            if let Some(ch) = nitrogen::ps2::keyboard::read_char() {
                return Some(ch);
            }
            runtime_tick_no_fb();
        }
    }

    fn input_available(&self) -> bool {
        nitrogen::ps2::keyboard::input_available()
    }
}

/// Monotonically‑increasing tick counter for use in the shell yield path.
static YIELD_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Framebuffer‑render function pointer injected by the kernel before
/// entering the shell loop.  When set, `runtime_tick_no_fb` can produce
/// visible output (terminal text, cursor position) even while the shell
/// is blocked waiting for keyboard input.
static RENDER_FN: Mutex<Option<fn()>> = Mutex::new(None);

/// Register a render‑to‑framebuffer callback for use during shell yield.
///
/// Called once by the kernel before the shell loop starts.  Without this
/// the terminal buffer is updated but never painted to the screen.
pub fn set_render_fn(f: fn()) {
    *RENDER_FN.lock() = Some(f);
}

/// Internal helper: run one full iteration of the runtime logic.
///
/// Uses its own monotonically‑increasing tick counter so that ChronoLine
/// timers (cursor blink, frame pacing) advance even while the shell waits
/// for keyboard input.  When a frame is due (`frame_due == true`) and a
/// render callback has been registered, it also blits the scene to the
/// scan‑out framebuffer.
fn runtime_tick_no_fb() {
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    poll_mouse_state();
    chrono_tick(now);
    process_events();

    // Render only when the frame‑pacing timer has fired and a kernel
    // callback is available (set via `set_render_fn` before shell entry).
    let do_render = {
        let rt = RUNTIME.lock();
        rt.as_ref().map_or(false, |r| r.frame_due)
    };
    if do_render {
        {
            let mut rt = RUNTIME.lock();
            rt.as_mut().map(|r| r.frame_due = false);
        }
        if let Some(render_fn) = *RENDER_FN.lock() {
            render_fn();
        }
    }

    // Short CPU pause to avoid burning the core at full speed during
    // the shell's wait loop.
    for _ in 0..10_000 {
        core::hint::spin_loop();
    }
}

// ── Runtime tick (main loop step) ────────────────────────────

/// Perform one tick of the runtime loop.
///
/// This is the main orchestrator function that:
/// 1. Polls hardware input → Resonance events
/// 2. Advances timers (cursor blink, frame pacing)
/// 3. Processes queued events
/// 4. Renders the desktop (only when the frame‑pacing timer fires)
///
/// The `framebuffer_fn` provides framebuffer access without coupling
/// Solvent to kernel-specific framebuffer management.
pub fn runtime_tick<F>(now: u64, framebuffer_fn: F)
where
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32)>,
{
    // 1. Poll hardware state → Resonance events (does NOT hold RUNTIME)
    poll_mouse_state();

    // 2. Advance ChronoLine timers
    chrono_tick(now);

    // 3. Process all queued Resonance events
    //    IMPORTANT: process_events must NOT be called while holding RUNTIME.
    //    Handlers (WmEventHandler) acquire RUNTIME themselves.
    process_events();

    // 4. Render the desktop — only when the frame‑pacing timer fires.
    {
        let mut rt = RUNTIME.lock();
        let do_render = rt.as_ref().map_or(false, |r| r.frame_due);
        if do_render {
            rt.as_mut().map(|r| r.frame_due = false);
        }
        drop(rt);
        if do_render {
            render(framebuffer_fn);
        }
    }
}

// ── Terminal buffer access (for kernel shell integration) ────

/// Write a string to the terminal buffer.
pub fn write_terminal(s: &str) {
    let mut rt = RUNTIME.lock();
    if let Some(ref mut r) = *rt {
        r.term_buf.put_str(s);
    }
}