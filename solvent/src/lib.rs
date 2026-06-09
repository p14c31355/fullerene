//! Solvent — Runtime / Orchestration Layer
//!
//! Solvent is the orchestration/runtime layer that sits between the kernel
//! and the higher-level subsystems (Lattice, Nozzle, Resonance, ChronoLine).
//!
//! # Event Flow
//!
//! ```text
//! Hardware IRQ → raw buffers
//! Solvent tick → poll_mouse_state → Resonance EventQueue
//!             → process_events → handlers
//! Solvent tick → render → Compositor → framebuffer
//! ```

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use chronoline::{ChronoLine, Deadline, TimerId, TimerMode};
use lattice::compositor::{Compositor, RenderTarget};
use lattice::desktop::Desktop;
use lattice::shell_overlay::{ShellState, render_app_grid, render_task_overview};
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use resonance::{Dispatcher, Event, EventHandler, EventQueue, InputEvent, KeyCode, MouseButton};
use spin::Mutex;

/// Global shell command function pointer, set by the kernel.
pub static SHELL_CMD: Mutex<Option<fn(&str) -> alloc::string::String>> = Mutex::new(None);

pub fn set_shell_command_handler(f: fn(&str) -> alloc::string::String) {
    *SHELL_CMD.lock() = Some(f);
}

pub fn exec_shell_command(input: &str) -> alloc::string::String {
    if let Some(f) = *SHELL_CMD.lock() {
        f(input)
    } else {
        alloc::string::String::from("(no shell)\n")
    }
}

// ── Constants ────────────────────────────────────────────────

/// Default terminal columns.
const DEFAULT_COLS: u32 = 80;
/// Default terminal rows.
const DEFAULT_ROWS: u32 = 25;
/// Glyph width in pixels (from lattice::font::GLYPH_WIDTH).
const GLYPH_W: u32 = 8;
/// Glyph height in pixels (from lattice::font::GLYPH_HEIGHT).
const GLYPH_H: u32 = 16;
const TERM_WIN_W: u32 = DEFAULT_COLS * GLYPH_W;
const TERM_WIN_H: u32 = DEFAULT_ROWS * GLYPH_H;
const BG_COLOR: u32 = 0x1a1a2e;
const CURSOR_BLINK_INTERVAL: u64 = 100;
const CURSOR_TIMER_ID: TimerId = TimerId(1);
const MOUSE_SENSITIVITY: i16 = 6;
const FRAME_INTERVAL_TICKS: u64 = 1;
const FRAME_TIMER_ID: TimerId = TimerId(2);

/// Maximum framebuffer size covering 4K (3840×2160). BSS static buffer;
/// displays exceeding this will skip rendering to avoid overflowing.
const MAX_FB_PIXELS: usize = 3840 * 2160;

/// Callback to extend the kernel heap.
///
/// Set by the kernel before any rendering.  The function receives the
/// number of additional bytes requested and returns `Ok(())` on success.
pub static HEAP_EXTEND_FN: Mutex<Option<fn(additional: usize) -> Result<(), ()>>> =
    Mutex::new(None);

/// Total bytes that have been successfully allocated via `HEAP_EXTEND_FN`.
/// Used by `render_terminal` to estimate whether the current heap can
/// satisfy a terminal surface resize without calling extend again.
pub static HEAP_EXTEND_RESERVE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Register the kernel heap extension callback.
pub fn set_heap_extend_fn(f: fn(usize) -> Result<(), ()>) {
    *HEAP_EXTEND_FN.lock() = Some(f);
}

/// Callback to get wall‑clock time from UEFI (or RTC fallback).
///
/// Returns `Option<(year, month, day, hour, minute, second)>`.
pub static WALL_CLOCK_FN: Mutex<Option<fn() -> Option<(u16, u8, u8, u8, u8, u8)>>> =
    Mutex::new(None);

/// Register the wall‑clock callback.
pub fn set_wall_clock_fn(f: fn() -> Option<(u16, u8, u8, u8, u8, u8)>) {
    *WALL_CLOCK_FN.lock() = Some(f);
}

/// Latest wall‑clock string (updated each frame).
static CLOCK_STRING: Mutex<String> = Mutex::new(String::new());

/// Get a copy of the current clock string.
pub fn clock_string() -> String {
    CLOCK_STRING.lock().clone()
}

/// Tick counter for double‑tap detection (shared with the runtime).
pub static GLOBAL_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// ── Static back‑buffer (BSS) ─────────────────────────────────

static BACK_BUFFER: Mutex<[u32; MAX_FB_PIXELS]> = Mutex::new([0u32; MAX_FB_PIXELS]);

// ── Runtime state ────────────────────────────────────────────

static RUNTIME: Mutex<Option<RuntimeState>> = Mutex::new(None);
static EVENT_QUEUE: Mutex<Option<EventQueue>> = Mutex::new(None);
static DISPATCHER: Mutex<Option<Dispatcher>> = Mutex::new(None);
static PREV_MOUSE_BUTTONS: Mutex<u8> = Mutex::new(0);
static FB_DIMS: Mutex<(u32, u32)> = Mutex::new((1024, 768));

pub struct RuntimeState {
    pub desktop: Desktop,
    pub term_window: WindowId,
    pub term_buf: TerminalBuffer,
    pub chrono: ChronoLine,
    pub cursor_visible: bool,
    pub frame_due: bool,
    pub back_len: usize,
    pub term_cells: Vec<LatticeCell>,
    pub term_dirty: bool,

    // ── Shell overlay state ─────────────────────────────
    /// Current shell UI state.
    pub shell_state: ShellState,

    /// Whether the clock text changed since the last compositor pass.
    pub clock_changed: bool,
}

pub fn init() {
    let mut desktop = Desktop::new(BG_COLOR);
    let term_window = desktop
        .wm
        .create_titled_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
    let term_buf = TerminalBuffer::new(DEFAULT_COLS, DEFAULT_ROWS);
    let mut dispatcher = Dispatcher::new();
    let mut chrono = ChronoLine::new();

    chrono.register_with_mode(
        Deadline::new(CURSOR_BLINK_INTERVAL),
        CURSOR_TIMER_ID,
        TimerMode::Repeating {
            interval_ticks: CURSOR_BLINK_INTERVAL,
        },
    );

    dispatcher.register(Box::new(WmEventHandler));
    dispatcher.register(Box::new(TerminalInputHandler));
    dispatcher.register(Box::new(ShellEventHandler));

    *EVENT_QUEUE.lock() = Some(EventQueue::new());
    *DISPATCHER.lock() = Some(dispatcher);

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
        term_dirty: true,
        shell_state: ShellState::Desktop,
        clock_changed: false,
    });
}

pub fn is_initialized() -> bool {
    RUNTIME.lock().is_some()
}

// ── Event handlers ───────────────────────────────────────────

struct WmEventHandler;

impl EventHandler for WmEventHandler {
    fn handle(&mut self, event: &Event) -> bool {
        let mut rt = RUNTIME.lock();
        let rt = match rt.as_mut() {
            Some(r) => r,
            None => return false,
        };

        // In shell overlay modes, route mouse events to overlay handling.
        if rt.shell_state != ShellState::Desktop {
            match event {
                // In overlay mode, only move the cursor — do NOT trigger a
                // full render.  The cached framebuffer (LAST_FB) already
                // contains the overlay.  We erase the old cursor position and
                // draw the new one directly.
                Event::Input(InputEvent::MouseMove { x, y }) => {
                    let prev_x = rt.desktop.cursor.x;
                    let prev_y = rt.desktop.cursor.y;
                    rt.desktop.set_cursor(*x, *y);
                    render_cursor_only(prev_x, prev_y, *x, *y);
                    return true;
                }
                Event::Input(InputEvent::MouseDown(_))
                    if rt.shell_state == ShellState::TimeZoneSelector =>
                {
                    // In timezone selector: determine which entry was clicked
                    let mouse = MOUSE_STATE.lock();
                    let cx = mouse.x as i32;
                    let cy = mouse.y as i32;
                    drop(mouse);
                    let (fw, _fh) = *FB_DIMS.lock();

                    // Timezone entry layout (must match render_timezone_selector)
                    let timezones: &[i8] = &[-12, -8, -5, 0, 1, 3, 5, 8, 9, 10, 12];
                    let entry_h = 24i32;
                    let pad = 6i32;
                    let start_y = 40i32;
                    let max_label_chars = 16i32;
                    let entry_w = max_label_chars * 8 + 16;
                    let ex = ((fw as i32) - entry_w) / 2;

                    for (i, offset) in timezones.iter().enumerate() {
                        let ey = start_y + (i as i32) * (entry_h + pad);
                        if cy >= ey && cy < ey + entry_h && cx >= ex && cx < ex + entry_w {
                            TIMEZONE_OFFSET_HOURS
                                .store(*offset, core::sync::atomic::Ordering::Relaxed);
                            rt.shell_state = ShellState::Desktop;
                            rt.frame_due = true;
                            return true;
                        }
                    }
                    // Click outside entries → back to AppGrid
                    rt.shell_state = ShellState::AppGrid;
                    rt.frame_due = true;
                    return true;
                }
                Event::Input(InputEvent::MouseDown(_)) if rt.shell_state == ShellState::AppGrid => {
                    // Check if Settings icon was clicked
                    let mouse = MOUSE_STATE.lock();
                    let cx = mouse.x as i32;
                    let cy = mouse.y as i32;
                    drop(mouse);
                    let (fw, _fh) = *FB_DIMS.lock();

                    // AppGrid layout (must match render_app_grid)
                    let icon_size = 64i32;
                    let pad = 24i32;
                    let label_h = 18i32;
                    let columns = ((fw as i32) / (icon_size + pad)).max(1);
                    let start_y = 60i32;

                    // "Settings" is index 2 in the apps array
                    let idx = 2i32;
                    let col = idx % columns;
                    let row = idx / columns;
                    let ax = pad + col * (icon_size + pad);
                    let ay = start_y + row * (icon_size + label_h + pad);

                    if cx >= ax && cx < ax + icon_size && cy >= ay && cy < ay + icon_size + label_h
                    {
                        rt.shell_state = ShellState::TimeZoneSelector;
                        rt.frame_due = true;
                        return true;
                    }

                    // Click on other app icons or outside → back to Desktop
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                Event::Input(InputEvent::MouseDown(_)) => {
                    // Generic click in any other overlay → back to Desktop
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                _ => return false,
            }
        }

        match event {
            Event::Input(InputEvent::MouseMove { x, y }) => {
                let prev_x = rt.desktop.cursor.x;
                let prev_y = rt.desktop.cursor.y;
                rt.desktop.mouse_move(*x, *y);
                render_cursor_only(prev_x, prev_y, rt.desktop.cursor.x, rt.desktop.cursor.y);
                true
            }
            Event::Input(InputEvent::MouseDown(_btn)) => {
                let cx = rt.desktop.cursor.x;
                let cy = rt.desktop.cursor.y;

                // ── Top-panel Activities button click ───
                if rt.desktop.top_panel.hit_activities_button(cx, cy) {
                    rt.shell_state = ShellState::TaskOverview;
                    rt.frame_due = true;
                    return true;
                }

                rt.desktop.set_cursor(cx, cy);
                let (fw, fh) = *FB_DIMS.lock();
                rt.desktop.mouse_down(fw, fh);
                rt.term_dirty = true;
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

struct TerminalInputHandler;

impl EventHandler for TerminalInputHandler {
    fn handle(&mut self, _event: &Event) -> bool {
        // ── No-op event handler ──────────────────────────
        // LatticeTerminal::read_byte() already consumes keys
        // directly from nitrogen::ps2::keyboard::read_char().
        // Writing to term_buf here would cause double input
        // (once from this handler, once from the shell echo).
        // This handler is retained for potential future use
        // (e.g. clipboard paste, compose sequences) but does
        // NOT forward ASCII to the terminal buffer.
        false
    }
}

/// Shell event handler — manages Super key double‑tap and Esc transitions.
struct ShellEventHandler;

impl EventHandler for ShellEventHandler {
    fn handle(&mut self, event: &Event) -> bool {
        let mut rt = RUNTIME.lock();
        let rt = match rt.as_mut() {
            Some(r) => r,
            None => return false,
        };

        match event {
            Event::Input(InputEvent::KeyDown(KeyCode::SuperLeft))
            | Event::Input(InputEvent::KeyDown(KeyCode::SuperRight)) => {
                match rt.shell_state {
                    ShellState::Desktop => {
                        rt.shell_state = ShellState::TaskOverview;
                        rt.frame_due = true;
                    }
                    ShellState::TaskOverview => {
                        rt.shell_state = ShellState::AppGrid;
                        rt.frame_due = true;
                    }
                    ShellState::AppGrid => {
                        rt.shell_state = ShellState::Desktop;
                        rt.frame_due = true;
                    }
                    ShellState::TimeZoneSelector => {
                        rt.shell_state = ShellState::Desktop;
                        rt.frame_due = true;
                    }
                }
                return true;
            }
            Event::Input(InputEvent::KeyDown(KeyCode::Escape)) => {
                if rt.shell_state != ShellState::Desktop {
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
            }
            _ => {}
        }
        false
    }
}

macro_rules! key_ascii {
    ($($variant:ident => $ch:expr),+ $(,)?) => {
        fn keycode_to_ascii(key: KeyCode) -> Option<u8> {
            use KeyCode::*;
            Some(match key { $($variant => $ch,)+ _ => return None })
        }
    };
}
key_ascii!(
    Enter => b'\n', Space => b' ', Backspace => 0x08, Tab => b'\t',
    A => b'a', B => b'b', C => b'c', D => b'd', E => b'e', F => b'f',
    G => b'g', H => b'h', I => b'i', J => b'j', K => b'k', L => b'l',
    M => b'm', N => b'n', O => b'o', P => b'p', Q => b'q', R => b'r',
    S => b's', T => b't', U => b'u', V => b'v', W => b'w', X => b'x',
    Y => b'y', Z => b'z',
    Digit0 => b'0', Digit1 => b'1', Digit2 => b'2', Digit3 => b'3',
    Digit4 => b'4', Digit5 => b'5', Digit6 => b'6', Digit7 => b'7',
    Digit8 => b'8', Digit9 => b'9',
);

// ── Input polling ────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MouseState {
    pub x: i16,
    pub y: i16,
    pub buttons: u8,
}

pub static MOUSE_STATE: Mutex<MouseState> = Mutex::new(MouseState {
    x: 512,
    y: 384,
    buttons: 0,
});

macro_rules! mouse_edge {
    ($queue:expr, $buttons:expr, $prev:expr, $bit:expr, $btn:ident) => {
        if ($buttons & $bit) != 0 && ($prev & $bit) == 0 {
            $queue.push(Event::Input(InputEvent::MouseDown(MouseButton::$btn)));
        } else if ($buttons & $bit) == 0 && ($prev & $bit) != 0 {
            $queue.push(Event::Input(InputEvent::MouseUp(MouseButton::$btn)));
        }
    };
}

pub fn poll_mouse_state() {
    {
        let ps2_state = nitrogen::ps2::mouse::consume_state();
        let dx = ps2_state.get_x();
        let dy = ps2_state.get_y();
        let btn = nitrogen::ps2::mouse::mouse_buttons();

        let mut mouse = MOUSE_STATE.lock();
        let old_x = mouse.x;
        let old_y = mouse.y;
        mouse.x = mouse.x.wrapping_add(dx.wrapping_mul(MOUSE_SENSITIVITY));
        mouse.y = mouse
            .y
            .wrapping_add(dy.wrapping_mul(MOUSE_SENSITIVITY).wrapping_neg());
        mouse.buttons = btn;

        let cx = mouse.x as i32;
        let cy = mouse.y as i32;
        let buttons = mouse.buttons;
        let moved = old_x != mouse.x || old_y != mouse.y;
        drop(mouse);

        // Only push MouseMove if the position actually changed.
        // Otherwise stale events accumulate in the queue and cause
        // render storms when the shell overlay event handler sets
        // frame_due on every single one.
        if moved {
            if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
                queue.push(Event::Input(InputEvent::MouseMove { x: cx, y: cy }));
            }
        }

        let mut prev_btn = PREV_MOUSE_BUTTONS.lock();
        let prev = *prev_btn;
        if buttons != prev {
            let mut eq_lock = EVENT_QUEUE.lock();
            if let Some(ref mut queue) = *eq_lock {
                mouse_edge!(queue, buttons, prev, 0x01, Left);
                mouse_edge!(queue, buttons, prev, 0x02, Right);
                mouse_edge!(queue, buttons, prev, 0x04, Middle);
            }
        }
        *prev_btn = buttons;
    }
}

// ── Keyboard polling (raw PS/2 → Resonance events) ──────────

/// Poll raw PS/2 key events and push them into the Resonance event queue.
/// Call from runtime_tick before process_events.
pub fn poll_keyboard() {
    while nitrogen::ps2::keyboard::raw_key_available() {
        let (scancode, pressed) = match nitrogen::ps2::keyboard::pop_raw_key() {
            Some(k) => k,
            None => break,
        };

        // Map scancode to resonance KeyCode
        let key = scancode_to_resonance_keycode(scancode);
        let event = if pressed {
            Event::Input(InputEvent::KeyDown(key))
        } else {
            Event::Input(InputEvent::KeyUp(key))
        };

        if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
            queue.push(event);
        }
    }
}

/// Map a raw PS/2 scancode (with 0x80 bit for extended) to Resonance KeyCode.
fn scancode_to_resonance_keycode(scancode: u8) -> KeyCode {
    let extended = scancode & 0x80 != 0;
    let base = scancode & 0x7F;

    if extended {
        match base {
            0x1D => return KeyCode::Ctrl, // RCtrl as Ctrl
            0x38 => return KeyCode::Alt,  // RAlt as Alt
            0x5B => return KeyCode::SuperLeft,
            0x5C => return KeyCode::SuperRight,
            _ => {}
        }
    }

    match base {
        0x01 => KeyCode::Escape,
        0x02 => KeyCode::Digit1,
        0x03 => KeyCode::Digit2,
        0x04 => KeyCode::Digit3,
        0x05 => KeyCode::Digit4,
        0x06 => KeyCode::Digit5,
        0x07 => KeyCode::Digit6,
        0x08 => KeyCode::Digit7,
        0x09 => KeyCode::Digit8,
        0x0A => KeyCode::Digit9,
        0x0B => KeyCode::Digit0,
        0x0E => KeyCode::Backspace,
        0x0F => KeyCode::Tab,
        0x10 => KeyCode::Q,
        0x11 => KeyCode::W,
        0x12 => KeyCode::E,
        0x13 => KeyCode::R,
        0x14 => KeyCode::T,
        0x15 => KeyCode::Y,
        0x16 => KeyCode::U,
        0x17 => KeyCode::I,
        0x18 => KeyCode::O,
        0x19 => KeyCode::P,
        0x1C => KeyCode::Enter,
        0x1D => KeyCode::Ctrl,
        0x1E => KeyCode::A,
        0x1F => KeyCode::S,
        0x20 => KeyCode::D,
        0x21 => KeyCode::F,
        0x22 => KeyCode::G,
        0x23 => KeyCode::H,
        0x24 => KeyCode::J,
        0x25 => KeyCode::K,
        0x26 => KeyCode::L,
        0x2A => KeyCode::Shift,
        0x2C => KeyCode::Z,
        0x2D => KeyCode::X,
        0x2E => KeyCode::C,
        0x2F => KeyCode::V,
        0x30 => KeyCode::B,
        0x31 => KeyCode::N,
        0x32 => KeyCode::M,
        0x36 => KeyCode::Shift,
        0x38 => KeyCode::Alt,
        0x39 => KeyCode::Space,
        0x3B => KeyCode::F1,
        0x3C => KeyCode::F2,
        0x3D => KeyCode::F3,
        0x3E => KeyCode::F4,
        0x3F => KeyCode::F5,
        0x40 => KeyCode::F6,
        0x41 => KeyCode::F7,
        0x42 => KeyCode::F8,
        0x43 => KeyCode::F9,
        0x44 => KeyCode::F10,
        0x47 => KeyCode::Home,
        0x48 => KeyCode::Up,
        0x49 => KeyCode::PageUp,
        0x4B => KeyCode::Left,
        0x4D => KeyCode::Right,
        0x4F => KeyCode::End,
        0x50 => KeyCode::Down,
        0x51 => KeyCode::PageDown,
        0x57 => KeyCode::F11,
        0x58 => KeyCode::F12,
        _ => KeyCode::Unknown(base as u32),
    }
}

// ── ChronoLine tick ──────────────────────────────────────────

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
                rt.term_dirty = true;
            }
            FRAME_TIMER_ID => {
                // Only auto-frame when on Desktop — shell overlays render
                // exclusively when shell_state changes (handlers set frame_due
                // explicitly on transition).
                if rt.shell_state == ShellState::Desktop {
                    rt.frame_due = true;
                }
            }
            _ => {}
        }
    }
}

// ── Event processing ─────────────────────────────────────────

pub fn push_key_event(event: Event) {
    if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
        queue.push(event);
    }
}

pub fn process_events() {
    let mut disp_lock = DISPATCHER.lock();
    let mut queue_lock = EVENT_QUEUE.lock();
    if let Some(ref mut dispatcher) = *disp_lock {
        if let Some(ref mut queue) = *queue_lock {
            dispatcher.dispatch_queue(queue);
        }
    }
}

// ── Clock update ─────────────────────────────────────────────

/// Days in a given month, accounting for leap years.
fn days_in_month(month: i16, year: i16) -> i16 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
            if leap { 29 } else { 28 }
        }
        _ => 31,
    }
}

/// Update the taskbar clock from the wall‑clock callback.
/// Format: "YYYY MMDD HHMM" (e.g. "2026 0606 2200").
pub static TIMEZONE_OFFSET_HOURS: core::sync::atomic::AtomicI8 =
    core::sync::atomic::AtomicI8::new(9);

pub fn update_clock() {
    let offset = TIMEZONE_OFFSET_HOURS.load(core::sync::atomic::Ordering::Relaxed);

    let time_str = if let Some(get_time) = *WALL_CLOCK_FN.lock() {
        if let Some((year, month, day, mut hour, minute, _second)) = get_time() {
            let mut local_hour = hour as i16 + offset as i16;
            let mut local_day = day as i16;
            let mut local_month = month as i16;
            let mut local_year = year as i16;

            while local_hour < 0 {
                local_hour += 24;
                local_day -= 1;
            }
            while local_hour >= 24 {
                local_hour -= 24;
                local_day += 1;
            }

            if local_day > days_in_month(local_month, local_year) {
                local_day = 1;
                local_month += 1;
                if local_month > 12 {
                    local_month = 1;
                    local_year += 1;
                }
            } else if local_day < 1 {
                local_month -= 1;
                if local_month < 1 {
                    local_month = 12;
                    local_year -= 1;
                }
                local_day = days_in_month(local_month, local_year) + local_day;
            }

            format!(
                "{} {:02}{:02} {:02}{:02}",
                local_year as u16, local_month as u8, local_day as u8, local_hour as u8, minute
            )
        } else {
            String::from("---- ---- ----")
        }
    } else {
        String::from("---- ---- ----")
    };

    let mut rt = RUNTIME.lock();
    if let Some(ref mut r) = *rt {
        let old = &r.desktop.clock_text;
        if *old != time_str {
            r.clock_changed = true;
            r.desktop.clock_text = time_str.clone();
            r.desktop.top_panel.clock_text = time_str.clone();
        }
    }
    *CLOCK_STRING.lock() = time_str;
}

// ── Rendering ────────────────────────────────────────────────

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

pub fn render<F>(framebuffer_fn: F)
where
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32)>,
{
    let mut rt_lock = RUNTIME.lock();
    let rt = match rt_lock.as_mut() {
        Some(r) => r,
        None => return,
    };

    // Force full redraw on any shell state transition so that the
    // previous overlay is painted over by the compositor.
    // Without this, returning from TaskOverview/AppGrid to Desktop
    // would leave old overlay pixels on the framebuffer.
    static PREV_SHELL_STATE: Mutex<ShellState> = Mutex::new(ShellState::Desktop);
    static PREV_TRANSITION: Mutex<bool> = Mutex::new(false);
    {
        let prev = *PREV_SHELL_STATE.lock();
        if rt.shell_state != prev {
            rt.desktop.force_full_redraw();
            *PREV_SHELL_STATE.lock() = rt.shell_state;
            // Signal that a full-screen blit is needed
            // to erase old overlay pixels from fb_pixels.
            *PREV_TRANSITION.lock() = true;
        }
    }

    // Also keep overlay-active full redraw to avoid accumulation.
    if rt.shell_state != ShellState::Desktop {
        rt.desktop.force_full_redraw();
    }

    render_terminal(rt, rt.term_window);
    rt.desktop.update_taskbar();

    let (fb_pixels, fb_width, fb_height) = match framebuffer_fn() {
        Some(t) => t,
        None => return,
    };

    // Cache FB dimensions for maximize toggle
    *FB_DIMS.lock() = (fb_width, fb_height);

    rt.desktop.prepare_frame(fb_width, fb_height);

    let fb_len = (fb_width as usize) * (fb_height as usize);
    if fb_len > MAX_FB_PIXELS {
        return;
    }
    rt.back_len = fb_len;

    // ── Skip compositor when nothing changed ──────────────────
    // In Desktop mode the FRAME_TIMER fires every tick and calls
    // render(), but if no window has moved and the clock text is stale
    // the compositor has no work to do.  Cursor redraws are already
    // handled by the lightweight render_cursor_only() in event handlers,
    // so we can skip the heavy compositor pass entirely.
    let has_dirty = rt.desktop.has_pending_dirty_rects();
    let clock_dirty = rt.clock_changed;

    if has_dirty || clock_dirty {
        rt.clock_changed = false;

        // On shell-state transitions, copy the ENTIRE back-buffer
        // to fb_pixels.  Shell overlays are drawn directly onto
        // fb_pixels (bypassing the back-buffer), so a partial
        // dirty-rect blit would leave old overlay pixels on screen.
        let was_transition = {
            let prev = *PREV_TRANSITION.lock();
            if prev {
                *PREV_TRANSITION.lock() = false;
            }
            prev
        };
        {
            let mut back = BACK_BUFFER.lock();
            let mut back_target = FramebufferTarget {
                pixels: &mut back[..fb_len],
                width: fb_width,
                height: fb_height,
            };
            let scene = rt.desktop.scene();
            let (bx, by, bw, bh) = Compositor::render(&scene, &mut back_target);

            if was_transition || (bw > 0 && bh > 0) {
                let fb_w = fb_width as usize;
                if was_transition {
                    // Full-screen blit on transition: non‑temporal store
                    let copy_len = fb_len.min(back.len());
                    unsafe {
                        copy_to_fb_volatile(fb_pixels.as_mut_ptr(), back.as_ptr(), copy_len);
                    }
                } else {
                    let b_w = bw as usize;
                    for row in 0..bh {
                        let off = ((by + row) as usize) * fb_w + (bx as usize);
                        let len = b_w.min(fb_len.saturating_sub(off));
                        if len > 0 {
                            unsafe {
                                copy_to_fb_volatile(
                                    fb_pixels.as_mut_ptr().add(off),
                                    back.as_ptr().add(off),
                                    len,
                                );
                            }
                        }
                    }
                }
            }
        }

        // ── Shell overlay rendering (post‑compositor, onto fb_pixels) ──────
        match rt.shell_state {
            ShellState::TaskOverview => {
                render_task_overview(fb_pixels, fb_width, fb_height, rt.desktop.wm.windows());
            }
            ShellState::AppGrid => {
                render_app_grid(fb_pixels, fb_width, fb_height);
            }
            ShellState::TimeZoneSelector => {
                let current_offset =
                    TIMEZONE_OFFSET_HOURS.load(core::sync::atomic::Ordering::Relaxed);
                lattice::shell_overlay::render_timezone_selector(
                    fb_pixels,
                    fb_width,
                    fb_height,
                    current_offset,
                );
            }
            ShellState::Desktop => {}
        }

        // ── Top Panel (only on Desktop; overlays cover it) ───
        if rt.shell_state == ShellState::Desktop {
            rt.desktop.top_panel.render(fb_pixels, fb_width, fb_height);
        }
    }

    // ── Cursor overlay (drawn after everything else) ──────
    // The compositor draws the cursor inside the back‑buffer, but
    // shell overlays overwrite that area.  Redraw the cursor directly
    // onto fb_pixels so it is always visible regardless of shell state.
    //
    // Also cache the framebuffer pointer so render_cursor_only()
    // can do lightweight cursor updates without a full render.
    unsafe {
        LAST_FB_PTR = fb_pixels.as_mut_ptr();
        LAST_FB_DIMS = (fb_width, fb_height);
    }
    let cursor = &rt.desktop.cursor;
    if cursor.visible {
        // Save backing store for future cursor-only redraws
        save_cursor_backing(fb_pixels, fb_width, fb_height, cursor.x, cursor.y);
        draw_cursor_on_fb(fb_pixels, fb_width, fb_height, cursor.x, cursor.y);
    }
}

/// Save the pixels under the current cursor for `render_cursor_only`.
fn save_cursor_backing(fb: &[u32], fbw: u32, fbh: u32, cx: i32, cy: i32) {
    use lattice::cursor::Cursor;
    let sz = Cursor::SIZE as i32;
    let dst_x = cx - Cursor::HOTSPOT_X;
    let dst_y = cy - Cursor::HOTSPOT_Y;
    let fb_w = fbw as usize;
    let fb_len = fb.len();

    for row in 0..sz {
        let dy = dst_y + row;
        if dy < 0 || dy >= fbh as i32 {
            continue;
        }
        for col in 0..sz {
            let dx = dst_x + col;
            if dx < 0 || dx >= fbw as i32 {
                continue;
            }
            let idx = (dy as usize) * fb_w + dx as usize;
            if idx < fb_len {
                unsafe {
                    CURSOR_BACKING[(row as usize) * (sz as usize) + col as usize] = fb[idx];
                }
            }
        }
    }
    unsafe {
        CURSOR_SAVED_X = dst_x;
        CURSOR_SAVED_Y = dst_y;
    }
}

/// Draw the cursor shape directly onto a framebuffer.
fn draw_cursor_on_fb(fb: &mut [u32], fbw: u32, fbh: u32, cx: i32, cy: i32) {
    use lattice::cursor::Cursor;
    let pixels = Cursor::shape();
    let sz = Cursor::SIZE as i32;
    let dst_x = cx - Cursor::HOTSPOT_X;
    let dst_y = cy - Cursor::HOTSPOT_Y;
    let fb_w = fbw as usize;
    let fb_len = fb.len();

    for row in 0..sz {
        let dy = dst_y + row;
        if dy < 0 || dy >= fbh as i32 {
            continue;
        }
        for col in 0..sz {
            let dx = dst_x + col;
            if dx < 0 || dx >= fbw as i32 {
                continue;
            }
            let s = pixels[(row as usize) * (sz as usize) + col as usize];
            if s & 0xFF000000 == 0 {
                continue;
            }
            let idx = (dy as usize) * fb_w + dx as usize;
            if idx < fb_len {
                fb[idx] = s;
            }
        }
    }
}

/// Volatile copy of `len` u32 pixels from `src` to `dst`.
///
/// Uses `write_volatile` / `read_volatile` which work correctly with
/// all framebuffer memory types (WB, WT, WC, UC).  Non‑temporal stores
/// (`_mm_stream_si32`) are NOT used here because the framebuffer may
/// not be mapped as WC — on real hardware WB/WT is common.
///
/// # Safety
/// `dst` and `src` must be valid for `len` u32 reads/writes.
/// Both pointers must be suitably aligned for u32 access (4 bytes).
unsafe fn copy_to_fb_volatile(dst: *mut u32, src: *const u32, len: usize) {
    for i in 0..len {
        let v = core::ptr::read_volatile(src.add(i));
        core::ptr::write_volatile(dst.add(i), v);
    }
}

/// Lightweight cursor-only redraw — no compositor, no overlay re‑render.
///
/// Restores the pixels under the old cursor position from `CURSOR_BACKING`,
/// saves the pixels under the new position, and draws the cursor shape.
fn render_cursor_only(prev_x: i32, prev_y: i32, new_x: i32, new_y: i32) {
    let fb_ptr = unsafe { LAST_FB_PTR };
    if fb_ptr.is_null() {
        return;
    }
    let (fbw, fbh) = unsafe { LAST_FB_DIMS };
    if fbw == 0 || fbh == 0 {
        return;
    }
    let fb_len = (fbw as usize) * (fbh as usize);
    let fb = unsafe { core::slice::from_raw_parts_mut(fb_ptr, fb_len) };

    use lattice::cursor::Cursor;
    let sz = Cursor::SIZE as i32;
    let fb_w = fbw as usize;

    // 1. Restore old cursor position from backing store
    let old_dst_x = prev_x - Cursor::HOTSPOT_X;
    let old_dst_y = prev_y - Cursor::HOTSPOT_Y;
    let saved_x = unsafe { CURSOR_SAVED_X };
    let saved_y = unsafe { CURSOR_SAVED_Y };
    if saved_x >= 0 && saved_y >= 0 {
        for row in 0..sz {
            let dy = saved_y + row;
            if dy < 0 || dy >= fbh as i32 {
                continue;
            }
            for col in 0..sz {
                let dx = saved_x + col;
                if dx < 0 || dx >= fbw as i32 {
                    continue;
                }
                let idx = (dy as usize) * fb_w + dx as usize;
                if idx < fb_len {
                    let backing =
                        unsafe { CURSOR_BACKING[(row as usize) * (sz as usize) + col as usize] };
                    fb[idx] = backing;
                }
            }
        }
    }

    // 2. Save new cursor position backing
    let new_dst_x = new_x - Cursor::HOTSPOT_X;
    let new_dst_y = new_y - Cursor::HOTSPOT_Y;
    for row in 0..sz {
        let dy = new_dst_y + row;
        if dy < 0 || dy >= fbh as i32 {
            continue;
        }
        for col in 0..sz {
            let dx = new_dst_x + col;
            if dx < 0 || dx >= fbw as i32 {
                continue;
            }
            let idx = (dy as usize) * fb_w + dx as usize;
            if idx < fb_len {
                unsafe {
                    CURSOR_BACKING[(row as usize) * (sz as usize) + col as usize] = fb[idx];
                }
            }
        }
    }
    unsafe {
        CURSOR_SAVED_X = new_dst_x;
        CURSOR_SAVED_Y = new_dst_y;
    }

    // 3. Draw cursor at new position
    let pixels = Cursor::shape();
    for row in 0..sz {
        let dy = new_dst_y + row;
        if dy < 0 || dy >= fbh as i32 {
            continue;
        }
        for col in 0..sz {
            let dx = new_dst_x + col;
            if dx < 0 || dx >= fbw as i32 {
                continue;
            }
            let s = pixels[(row as usize) * (sz as usize) + col as usize];
            if s & 0xFF000000 == 0 {
                continue;
            }
            let idx = (dy as usize) * fb_w + dx as usize;
            if idx < fb_len {
                fb[idx] = s;
            }
        }
    }
}

fn render_terminal(rt: &mut RuntimeState, term_window: WindowId) {
    if !rt.term_dirty {
        return;
    }

    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == term_window)
    {
        Some(w) => w,
        None => return,
    };

    // ── Dynamic terminal resize ──────────────────────────────
    // Compute the terminal grid size that fits the current window.
    let new_cols = (window.width / GLYPH_W).max(1);
    let new_rows = (window.height / GLYPH_H).max(1);

    let cur_cols = rt.term_buf.cols();
    let cur_rows = rt.term_buf.rows();

    if new_cols != cur_cols || new_rows != cur_rows {
        // Estimate required memory for the new surface + buffer.
        // Surface: new_cols*GLYPH_W × new_rows*GLYPH_H pixels × 4 bytes.
        let new_surface_pixels = (new_cols * new_rows * GLYPH_W * GLYPH_H) as usize;
        // TerminalBuffer cells: Cell is 12 bytes each.
        let new_buf_cells = (new_cols * new_rows) as usize * 12;
        let needed = (new_surface_pixels * 4).saturating_add(new_buf_cells);

        // Try to extend the kernel heap if needed.
        if needed > HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed) {
            let additional = needed
                .saturating_sub(HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed))
                .next_multiple_of(4096);
            if let Some(extend_fn) = *HEAP_EXTEND_FN.lock() {
                if extend_fn(additional).is_err() {
                    // Extension failed — keep old size, don't risk OOM.
                    return;
                } else {
                    HEAP_EXTEND_RESERVE
                        .fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
                }
            } else {
                return;
            }
        }

        // Allocate new TerminalBuffer and Surface.
        let new_buf = TerminalBuffer::new(new_cols, new_rows);
        let old_buf = core::mem::replace(&mut rt.term_buf, new_buf);
        // Try to transfer any visible content from old buffer to new.
        // We do this by copying cells that fit in both grids.
        {
            let src_cells = old_buf.cells();
            let src_cols = cur_cols as usize;
            let copy_rows = (cur_rows as usize).min(new_rows as usize);
            let copy_cols = (cur_cols as usize).min(new_cols as usize);
            for row in 0..copy_rows {
                for col in 0..copy_cols {
                    let src_idx = row * src_cols + col;
                    if src_idx < src_cells.len() {
                        let c = src_cells[src_idx];
                        if let Some(dst) = rt.term_buf.cell_mut(col as u32, row as u32) {
                            *dst = nozzle::terminal_buffer::Cell {
                                ch: c.ch,
                                fg: c.fg,
                                bg: c.bg,
                            };
                        }
                    }
                }
            }
        }
        let _ = old_buf; // drop old buffer

        window.surface = lattice::surface::Surface::new(
            new_cols * GLYPH_W,
            new_rows * GLYPH_H,
            window.surface.get_pixel(0, 0).unwrap_or(0x000000),
        );

        // Rebuild term_cells to match new size.
        rt.term_cells.clear();
        rt.term_cells.resize(
            (new_cols * new_rows) as usize,
            LatticeCell {
                ch: b' ',
                fg: 0,
                bg: 0,
            },
        );
    }

    // Always sync term_cells from current term_buf
    let term_buf = &rt.term_buf;
    let total = (term_buf.cols() * term_buf.rows()) as usize;

    if rt.term_cells.len() != total {
        rt.term_cells.resize(
            total,
            LatticeCell {
                ch: b' ',
                fg: 0,
                bg: 0,
            },
        );
    }
    for (i, c) in term_buf.cells().iter().enumerate() {
        if i < rt.term_cells.len() {
            rt.term_cells[i] = LatticeCell {
                ch: c.ch,
                fg: c.fg,
                bg: c.bg,
            };
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

    rt.desktop.invalidate_window(term_window);
    rt.term_dirty = false;
}

// ── LatticeTerminal (nozzle::Terminal impl) ──────────────────

pub struct LatticeTerminal;

impl nozzle::Terminal for LatticeTerminal {
    fn write_str(&mut self, s: &str) {
        let mut rt = RUNTIME.lock();
        if let Some(ref mut r) = *rt {
            r.term_buf.put_str(s);
            r.term_dirty = true;
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
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

static YIELD_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static RENDER_FN: Mutex<Option<fn()>> = Mutex::new(None);

/// Cached framebuffer for cursor‑only redraws (lightweight, no compositor).
/// `static mut` is acceptable here because the kernel is single‑threaded.
static mut LAST_FB_PTR: *mut u32 = core::ptr::null_mut();
static mut LAST_FB_DIMS: (u32, u32) = (0, 0);

/// Saved 16×16 pixel region under the cursor, restored on cursor move.
static mut CURSOR_BACKING: [u32; 256] = [0u32; 256];
static mut CURSOR_SAVED_X: i32 = -100;
static mut CURSOR_SAVED_Y: i32 = -100;

pub fn set_render_fn(f: fn()) {
    *RENDER_FN.lock() = Some(f);
}

fn runtime_tick_no_fb() {
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);
    poll_mouse_state();
    poll_keyboard();
    update_clock();
    chrono_tick(now);
    process_events();

    let do_render = RUNTIME.lock().as_mut().map_or(false, |r| {
        let due = r.frame_due;
        r.frame_due = false;
        due
    });
    if do_render {
        if let Some(render_fn) = *RENDER_FN.lock() {
            render_fn();
        }
    }
}

// ── Runtime tick (main loop step) ────────────────────────────

pub fn runtime_tick<F>(now: u64, framebuffer_fn: F)
where
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32)>,
{
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);
    poll_mouse_state();
    poll_keyboard();
    update_clock();
    chrono_tick(now);
    process_events();

    let do_render = RUNTIME.lock().as_mut().map_or(false, |r| {
        let due = r.frame_due;
        r.frame_due = false;
        due
    });
    if do_render {
        render(framebuffer_fn);
    }
}

pub fn write_terminal(s: &str) {
    if let Some(ref mut r) = *RUNTIME.lock() {
        r.term_buf.put_str(s);
        r.term_dirty = true;
    }
}

/// Trigger a full desktop redraw on the next render pass.
/// Used by external subsystems (e.g. BadApple) to restore the
/// desktop after direct framebuffer manipulation.
pub fn force_desktop_redraw() {
    if let Some(ref mut r) = *RUNTIME.lock() {
        r.desktop.force_full_redraw();
        r.frame_due = true;
    }
}

// ── Theme / wallpaper bridges (avoid kernel → lattice coupling) ─────

pub use lattice::theme::{ThemeVariant, current_theme_variant, set_theme, toggle_theme};
pub use lattice::wallpaper::{WallpaperMode, get_wallpaper, set_wallpaper};

// ── Shell bootstrap (moved from kernel to respect dependency direction)

/// Run the nozzle shell on the given terminal.
/// This function lives in solvent so the kernel does not need to depend on
/// nozzle / lattice directly (AGENTS.md §3).
pub fn run_shell_on(terminal: &mut dyn nozzle::Terminal, prompt: &str) {
    let commands = nozzle::default_commands();
    let mut shell = nozzle::Shell::new(terminal, commands);
    shell.set_prompt(prompt);
    shell.run();
}
