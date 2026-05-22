//! Lattice‑based GUI subsystem.
//!
//! Owns the global desktop, terminal window, and event dispatcher.
//! Bridges Nozzle output → terminal buffer → Lattice Surface → compositor.
//!
//! # Event flow
//!
//! ```text
//! Hardware IRQ → raw buffers (keyboard scancode, mouse PS/2)
//! Scheduler loop → poll_input_events() → Resonance EventQueue
//!                               → process_events() → handlers
//!                                   ├─ WmEventHandler (mouse → desktop state)
//!                                   └─ TerminalInputHandler (keyboard → terminal)
//! Scheduler loop → render() → Compositor → framebuffer
//! ```

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use chronoline::{ChronoLine, Deadline, TimerId, TimerMode};
use lattice::compositor::{Compositor, RenderTarget};
use lattice::desktop::Desktop;
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use resonance::{
    Dispatcher, Event, EventHandler, EventQueue,
    InputEvent, KeyCode, MouseButton,
};
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

/// Cursor blink interval in ticks (~500ms).
const CURSOR_BLINK_INTERVAL: u64 = 500;

/// Timer ID for cursor blink.
const CURSOR_TIMER_ID: TimerId = TimerId(1);

// ── Global state ─────────────────────────────────────────────

/// Global GUI state.
pub static GUI: Mutex<Option<GuiState>> = Mutex::new(None);

/// The full GUI state.
pub struct GuiState {
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

/// Initialise the GUI subsystem.
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
        TimerMode::Repeating { interval_ticks: CURSOR_BLINK_INTERVAL },
    );

    // Register event handlers
    dispatcher.register(Box::new(WmEventHandler));
    dispatcher.register(Box::new(TerminalInputHandler));

    *GUI.lock() = Some(GuiState {
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

// ── Event handlers ───────────────────────────────────────────

/// Handles mouse events for the window manager (desktop cursor, dragging).
struct WmEventHandler;

impl EventHandler for WmEventHandler {
    fn handle(&mut self, event: &Event) -> bool {
        let mut gui = GUI.lock();
        let gui = match gui.as_mut() {
            Some(g) => g,
            None => return false,
        };

        match event {
            Event::Input(InputEvent::MouseMove { x, y }) => {
                gui.desktop.mouse_move(*x, *y);
                true // consumed
            }
            Event::Input(InputEvent::MouseDown(_btn)) => {
                // Trigger mouse_down at the current cursor position
                gui.desktop.set_cursor(gui.desktop.cursor.x, gui.desktop.cursor.y);
                gui.desktop.mouse_down();
                true
            }
            Event::Input(InputEvent::MouseUp(_btn)) => {
                gui.desktop.mouse_up();
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
        let mut gui = GUI.lock();
        let gui = match gui.as_mut() {
            Some(g) => g,
            None => return false,
        };

        // Only handle keyboard events destined for the terminal
        match event {
            Event::Input(InputEvent::KeyDown(key)) => {
                // Convert to ASCII and write to terminal buffer
                if let Some(ascii) = keycode_to_ascii(*key) {
                    gui.term_buf.put_str(core::str::from_utf8(&[ascii]).unwrap_or(""));
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

/// Poll hardware mouse state and inject Resonance events.
///
/// Called from the scheduler loop. Reads the PS/2 mouse state from the
/// Nitrogen driver (which processes packets in the interrupt handler)
/// and accumulates deltas into the kernel's absolute-position state.
/// Generates MouseMove / MouseDown / MouseUp events with edge detection.
pub fn poll_mouse_state() {
    use crate::interrupts::input::MOUSE_STATE;
    use nitrogen::ps2::mouse::latest_state;

    let mut gui = GUI.lock();
    let gui = match gui.as_mut() {
        Some(g) => g,
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

    // ── Re-read the now-updated MOUSE_STATE ──────────────────────
    let mouse = MOUSE_STATE.lock();
    let cx = mouse.x as i32;
    let cy = mouse.y as i32;
    let buttons = mouse.buttons;
    drop(mouse);

    // Always send mouse move (cursor position tracked by interrupt handler)
    gui.event_queue.push(Event::Input(InputEvent::MouseMove { x: cx, y: cy }));

    // Edge detection for button state changes
    let prev = gui.prev_mouse_buttons;

    // Left button (bit 0)
    if (buttons & 0x01) != 0 && (prev & 0x01) == 0 {
        gui.event_queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Left)));
    } else if (buttons & 0x01) == 0 && (prev & 0x01) != 0 {
        gui.event_queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Left)));
    }

    // Right button (bit 1)
    if (buttons & 0x02) != 0 && (prev & 0x02) == 0 {
        gui.event_queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Right)));
    } else if (buttons & 0x02) == 0 && (prev & 0x02) != 0 {
        gui.event_queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Right)));
    }

    // Middle button (bit 2)
    if (buttons & 0x04) != 0 && (prev & 0x04) == 0 {
        gui.event_queue.push(Event::Input(InputEvent::MouseDown(MouseButton::Middle)));
    } else if (buttons & 0x04) == 0 && (prev & 0x04) != 0 {
        gui.event_queue.push(Event::Input(InputEvent::MouseUp(MouseButton::Middle)));
    }

    gui.prev_mouse_buttons = buttons;
}

/// Poll keyboard state and inject Resonance events.
///
/// Reads from the keyboard driver's ASCII buffer and generates
/// KeyDown events. The shell also reads from this same buffer
/// via `LatticeTerminal::read_byte()`, so we need to be careful
/// not to steal characters from the shell.
///
/// For now, keyboard events are only bridged to Resonance if
/// the shell is NOT currently reading input. This avoids
/// double-processing.
pub fn poll_keyboard_state() {
    // Read available ASCII bytes from the keyboard buffer
    // and push them as KeyDown events.
    //
    // Note: The shell loop also reads from this buffer. To avoid
    // stealing characters, we only push events when the buffer
    // would overflow (i.e., the shell isn't consuming fast enough).
    // For now, we keep the shell as the primary consumer.
    //
    // Future: Route ALL input through Resonance events, change
    // LatticeTerminal::read_byte() to consume from the event queue.
}

// ── ChronoLine tick ──────────────────────────────────────────

/// Advance the ChronoLine clock and process expired timers.
///
/// Called from the scheduler loop every tick.
///
/// Uses `TimerMode::Repeating` for cursor blink, so we no longer
/// need to manually re‑register the timer.
pub fn chrono_tick(now: u64) {
    let mut gui = GUI.lock();
    let gui = match gui.as_mut() {
        Some(g) => g,
        None => return,
    };

    gui.chrono.tick(now);

    while let Some(timer) = gui.chrono.pop_expired() {
        match timer.id {
            CURSOR_TIMER_ID => {
                gui.cursor_visible = !gui.cursor_visible;
                // No need to re‑register — TimerMode::Repeating handles it automatically.
            }
            _ => {}
        }
    }
}

// ── Rendering ────────────────────────────────────────────────

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

    // Signal present & flush GPU
    let mut renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    if let Some(ref mut renderer) = *renderer_lock {
        renderer.present();
    }
    drop(renderer_lock);
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

/// Push a key event into the Resonance event queue.
pub fn push_key_event(event: Event) {
    let mut gui = GUI.lock();
    if let Some(ref mut g) = *gui {
        // Push to Resonance queue – handlers will process on next dispatch
        g.event_queue.push(event);
    }
}

/// Process pending Resonance events (called from scheduler).
pub fn process_events() {
    let mut gui = GUI.lock();
    if let Some(ref mut g) = *gui {
        g.dispatcher.dispatch_queue(&mut g.event_queue);
    }
}

// ── Framebuffer access ───────────────────────────────────────

/// Get a mutable slice of the framebuffer pixels and its dimensions.
/// Also returns a `MutexGuard` that must be kept alive while the slice is used.
fn get_framebuffer_slice() -> Option<(&'static mut [u32], u32, u32)> {
    let renderer_lock = crate::graphics::PRIMARY_RENDERER.lock();
    let renderer = renderer_lock.as_ref()?;
    let info = renderer.get_info();

    // `info.address` is a physical address that is identity‑mapped in the kernel's
    // page table.  We use it directly — adding `phys_offset` would produce an
    // invalid address because the framebuffer is NOT mapped in the higher half.
    let fb_ptr = info.address as *mut u32;
    let fb_len = (info.width as usize) * (info.height as usize);

    let fb_pixels = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };
    Some((fb_pixels, info.width, info.height))
}

// ── Fallback rendering ───────────────────────────────────────

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
