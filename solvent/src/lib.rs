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
use alloc::vec::Vec;
use chronoline::{ChronoLine, Deadline, TimerId, TimerMode};
use lattice::compositor::{Compositor, RenderTarget};
use lattice::desktop::Desktop;
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

const TERM_COLS: u32 = 80;
const TERM_ROWS: u32 = 25;
const TERM_WIN_W: u32 = TERM_COLS * 8;
const TERM_WIN_H: u32 = TERM_ROWS * 16;
const BG_COLOR: u32 = 0x1a1a2e;
const CURSOR_BLINK_INTERVAL: u64 = 100;
const CURSOR_TIMER_ID: TimerId = TimerId(1);
const MOUSE_SENSITIVITY: i16 = 8;
const FRAME_INTERVAL_TICKS: u64 = 2;
const FRAME_TIMER_ID: TimerId = TimerId(2);

/// Maximum framebuffer size covering 4K (3840×2160). BSS static buffer;
/// displays exceeding this will skip rendering to avoid overflowing.
const MAX_FB_PIXELS: usize = 3840 * 2160;

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
}

pub fn init() {
    let mut desktop = Desktop::new(BG_COLOR);
    let term_window = desktop
        .wm
        .create_titled_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
    let term_buf = TerminalBuffer::new(TERM_COLS, TERM_ROWS);
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
        match event {
            Event::Input(InputEvent::MouseMove { x, y }) => {
                rt.desktop.mouse_move(*x, *y);
                true
            }
            Event::Input(InputEvent::MouseDown(_btn)) => {
                rt.desktop
                    .set_cursor(rt.desktop.cursor.x, rt.desktop.cursor.y);
                let (fw, fh) = *FB_DIMS.lock();
                rt.desktop.mouse_down(fw, fh);
                // Force terminal redraw after any title-bar action that
                // might have resized/moved the terminal window
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

fn keycode_to_ascii(key: KeyCode) -> Option<u8> {
    use KeyCode::*;
    Some(match key {
        Enter => b'\n',
        Space => b' ',
        Backspace => 0x08,
        Tab => b'\t',
        A => b'a',
        B => b'b',
        C => b'c',
        D => b'd',
        E => b'e',
        F => b'f',
        G => b'g',
        H => b'h',
        I => b'i',
        J => b'j',
        K => b'k',
        L => b'l',
        M => b'm',
        N => b'n',
        O => b'o',
        P => b'p',
        Q => b'q',
        R => b'r',
        S => b's',
        T => b't',
        U => b'u',
        V => b'v',
        W => b'w',
        X => b'x',
        Y => b'y',
        Z => b'z',
        Digit0 => b'0',
        Digit1 => b'1',
        Digit2 => b'2',
        Digit3 => b'3',
        Digit4 => b'4',
        Digit5 => b'5',
        Digit6 => b'6',
        Digit7 => b'7',
        Digit8 => b'8',
        Digit9 => b'9',
        _ => return None,
    })
}

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
        mouse.x = mouse.x.wrapping_add(dx.wrapping_mul(MOUSE_SENSITIVITY));
        mouse.y = mouse
            .y
            .wrapping_add(dy.wrapping_mul(MOUSE_SENSITIVITY).wrapping_neg());
        mouse.buttons = btn;
    }

    {
        let mouse = MOUSE_STATE.lock();
        let cx = mouse.x as i32;
        let cy = mouse.y as i32;
        let buttons = mouse.buttons;
        drop(mouse);

        if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
            queue.push(Event::Input(InputEvent::MouseMove { x: cx, y: cy }));
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
            FRAME_TIMER_ID => rt.frame_due = true,
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

    if bw > 0 && bh > 0 {
        let back = BACK_BUFFER.lock();
        let fb_w = fb_width as usize;
        let b_w = bw as usize;
        for row in 0..bh {
            let off = ((by + row) as usize) * fb_w + (bx as usize);
            let len = b_w.min(fb_len.saturating_sub(off));
            if len > 0 {
                fb_pixels[off..off + len].copy_from_slice(&back[off..off + len]);
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

    // Note: we intentionally do NOT resize surface or TerminalBuffer when
    // window dimensions change (e.g. after maximize).  The compositor clips
    // the surface to the window rectangle, so the terminal is drawn at its
    // original size in the top-left corner of the maximized window.
    // Resizing the surface would require a large allocation (~3 MiB for a
    // full-screen terminal) which OOMs on the current 4 MiB kernel heap.

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

pub fn set_render_fn(f: fn()) {
    *RENDER_FN.lock() = Some(f);
}

fn runtime_tick_no_fb() {
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    poll_mouse_state();
    chrono_tick(now);
    process_events();

    let do_render = RUNTIME.lock().as_ref().map_or(false, |r| r.frame_due);
    if do_render {
        RUNTIME.lock().as_mut().map(|r| r.frame_due = false);
        if let Some(render_fn) = *RENDER_FN.lock() {
            render_fn();
        }
    }
    for _ in 0..100 {
        core::hint::spin_loop();
    }
}

// ── Runtime tick (main loop step) ────────────────────────────

pub fn runtime_tick<F>(now: u64, framebuffer_fn: F)
where
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32)>,
{
    poll_mouse_state();
    chrono_tick(now);
    process_events();

    let do_render = RUNTIME.lock().as_ref().map_or(false, |r| r.frame_due);
    if do_render {
        RUNTIME.lock().as_mut().map(|r| r.frame_due = false);
        render(framebuffer_fn);
    }
}

pub fn write_terminal(s: &str) {
    if let Some(ref mut r) = *RUNTIME.lock() {
        r.term_buf.put_str(s);
        r.term_dirty = true;
    }
}