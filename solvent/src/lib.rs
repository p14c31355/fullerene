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

/// Cursor blink interval in ticks (~500ms).
const CURSOR_BLINK_INTERVAL: u64 = 500;

/// Timer ID for cursor blink.
const CURSOR_TIMER_ID: TimerId = TimerId(1);

// ── Runtime state ────────────────────────────────────────────

/// Global runtime state.
static RUNTIME: Mutex<Option<RuntimeState>> = Mutex::new(None);

/// The full runtime state, owned by Solvent.
pub struct RuntimeState {
    pub desktop: Desktop,
    pub term_window: WindowId,
    pub term_buf: TerminalBuffer,
    pub dispatcher: Dispatcher,
    pub event_queue: EventQueue,
    pub chrono: ChronoLine,
    pub cursor_visible: bool,
    /// Previous mouse button state for edge detection.
    pub prev_mouse_buttons: u8,
}

/// Initialise the Solvent runtime subsystem.
///
/// Creates the desktop, terminal window, event dispatcher, and timer infrastructure.
pub fn init() {
    let mut desktop = Desktop::new(BG_COLOR);
    let term_window = desktop.create_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000);
    let term_buf = TerminalBuffer::new(TERM_COLS, TERM_ROWS);
    let mut dispatcher = Dispatcher::new();
    let event_queue = EventQueue::new();
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

    *RUNTIME.lock() = Some(RuntimeState {
        desktop,
        term_window,
        term_buf,
        dispatcher,
        event_queue,
        chrono,
        cursor_visible: true,
        prev_mouse_buttons: 0,
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
pub static MOUSE_STATE: Mutex<MouseState> = Mutex::new(MouseState { x: 0, y: 0, buttons: 0 });

/// Poll hardware mouse state and inject Resonance events.
///
/// Called from the runtime loop. Reads the PS/2 mouse state from the
/// Nitrogen driver (which processes packets in the interrupt handler)
/// and accumulates deltas into the absolute-position state.
/// Generates MouseMove / MouseDown / MouseUp events with edge detection.
pub fn poll_mouse_state() {
    use nitrogen::ps2::mouse::latest_state;

    let mut rt = RUNTIME.lock();
    let rt = match rt.as_mut() {
        Some(r) => r,
        None => return,
    };

    // Sync the ps2-mouse delta state into the accumulated absolute position.
    {
        let ps2_state = latest_state();
        let mut mouse = MOUSE_STATE.lock();
        mouse.x = mouse.x.wrapping_add(ps2_state.get_x());
        mouse.y = mouse.y.wrapping_add(ps2_state.get_y());
        mouse.buttons = nitrogen::ps2::mouse::mouse_buttons();
    }

    // Re-read the now-updated MOUSE_STATE
    let mouse = MOUSE_STATE.lock();
    let cx = mouse.x as i32;
    let cy = mouse.y as i32;
    let buttons = mouse.buttons;
    drop(mouse);

    // Always send mouse move
    rt.event_queue
        .push(Event::Input(InputEvent::MouseMove { x: cx, y: cy }));

    // Edge detection for button state changes
    let prev = rt.prev_mouse_buttons;

    // Left button (bit 0)
    if (buttons & 0x01) != 0 && (prev & 0x01) == 0 {
        rt.event_queue
            .push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
    } else if (buttons & 0x01) == 0 && (prev & 0x01) != 0 {
        rt.event_queue
            .push(Event::Input(InputEvent::MouseUp(MouseButton::Left)));
    }

    // Right button (bit 1)
    if (buttons & 0x02) != 0 && (prev & 0x02) == 0 {
        rt.event_queue
            .push(Event::Input(InputEvent::MouseDown(MouseButton::Right)));
    } else if (buttons & 0x02) == 0 && (prev & 0x02) != 0 {
        rt.event_queue
            .push(Event::Input(InputEvent::MouseUp(MouseButton::Right)));
    }

    // Middle button (bit 2)
    if (buttons & 0x04) != 0 && (prev & 0x04) == 0 {
        rt.event_queue
            .push(Event::Input(InputEvent::MouseDown(MouseButton::Middle)));
    } else if (buttons & 0x04) == 0 && (prev & 0x04) != 0 {
        rt.event_queue
            .push(Event::Input(InputEvent::MouseUp(MouseButton::Middle)));
    }

    rt.prev_mouse_buttons = buttons;
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
            _ => {}
        }
    }
}

// ── Event processing ─────────────────────────────────────────

/// Push a key event into the Resonance event queue.
pub fn push_key_event(event: Event) {
    let mut rt = RUNTIME.lock();
    if let Some(ref mut r) = *rt {
        r.event_queue.push(event);
    }
}

/// Process pending Resonance events (called from runtime loop).
pub fn process_events() {
    let mut rt = RUNTIME.lock();
    if let Some(ref mut r) = *rt {
        r.dispatcher.dispatch_queue(&mut r.event_queue);
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
/// The `framebuffer_fn` parameter is a closure that provides access to the
/// kernel's framebuffer slice, avoiding direct dependency on kernel internals.
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

    // Get framebuffer memory via the caller-provided closure
    let fb_result = framebuffer_fn();
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
    let scene = rt.desktop.scene();
    Compositor::render(&scene, &mut target);
}

/// Render the terminal buffer onto the terminal window's surface.
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

    let cells: Vec<LatticeCell> = rt
        .term_buf
        .cells()
        .iter()
        .map(|c| LatticeCell {
            ch: c.ch,
            fg: c.fg,
            bg: c.bg,
        })
        .collect();

    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &cells,
        cols: rt.term_buf.cols(),
        cursor_col: Some(rt.term_buf.cursor_col()),
        cursor_row: Some(rt.term_buf.cursor_row()),
        cursor_visible: rt.cursor_visible,
    });
}

// ── LatticeTerminal (nozzle::Terminal impl) ──────────────────

/// A [`nozzle::Terminal`] that writes to the Lattice‑backed terminal buffer.
pub struct LatticeTerminal;

impl nozzle::Terminal for LatticeTerminal {
    fn write_str(&mut self, s: &str) {
        let mut rt = RUNTIME.lock();
        if let Some(ref mut r) = *rt {
            r.term_buf.put_str(s);
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        nitrogen::ps2::keyboard::read_char()
    }

    fn input_available(&self) -> bool {
        nitrogen::ps2::keyboard::input_available()
    }
}

// ── Runtime tick (main loop step) ────────────────────────────

/// Perform one tick of the runtime loop.
///
/// This is the main orchestrator function that:
/// 1. Polls hardware input → Resonance events
/// 2. Advances timers (cursor blink, etc.)
/// 3. Processes queued events
/// 4. Renders the desktop
///
/// The `framebuffer_fn` provides framebuffer access without coupling
/// Solvent to kernel-specific framebuffer management.
pub fn runtime_tick<F>(now: u64, framebuffer_fn: F)
where
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32)>,
{
    // 1. Poll hardware state → Resonance events
    poll_mouse_state();

    // 2. Advance ChronoLine timers
    chrono_tick(now);

    // 3. Process all queued Resonance events
    process_events();

    // 4. Render the desktop
    render(framebuffer_fn);
}

// ── Terminal buffer access (for kernel shell integration) ────

/// Write a string to the terminal buffer.
pub fn write_terminal(s: &str) {
    let mut rt = RUNTIME.lock();
    if let Some(ref mut r) = *rt {
        r.term_buf.put_str(s);
    }
}