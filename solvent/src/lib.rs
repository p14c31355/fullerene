//! Solvent — Runtime / Orchestration Layer
//!
//! Solvent sits between the kernel and higher-level subsystems (Lattice,
//! Nozzle, Resonance, ChronoLine).  It owns runtime state, event dispatch,
//! frame pacing, and subsystem wiring.
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

// ── Modules (god-module decomposition per AGENTS.md §10) ────
mod handlers;
mod menu_actions;
mod explorer;
mod viewers;

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
use resonance::{Dispatcher, Event, EventQueue, InputEvent, KeyCode, MouseButton};
use spin::Mutex;
use lattice::editor::EditorBuffer;

// ── Aggregated kernel callbacks ──────────────────────────────

pub struct SolventCallbacks {
    pub shell_cmd: Option<fn(&str) -> String>,
    pub launch_shell: Option<fn()>,
    pub heap_extend: Option<fn(usize) -> Result<(), ()>>,
    pub wall_clock: Option<fn() -> Option<(u16, u8, u8, u8, u8, u8)>>,
    pub vfs_readdir: Option<fn(&str) -> Result<Vec<VfsEntry>, &'static str>>,
    /// Read a file's entire content into a byte vector.
    /// Opens, reads all bytes, closes.
    pub vfs_read: Option<fn(&str) -> Result<Vec<u8>, &'static str>>,
    /// Write bytes to a file (creates or overwrites).
    pub vfs_write: Option<fn(&str, &[u8]) -> Result<(), &'static str>>,
    /// Create a new empty file.
    pub vfs_create: Option<fn(&str) -> Result<(), &'static str>>,
    /// Create a directory.
    pub vfs_mkdir: Option<fn(&str) -> Result<(), &'static str>>,
    /// Delete a file or empty directory.
    pub vfs_unlink: Option<fn(&str) -> Result<(), &'static str>>,
    pub process_list: Option<fn() -> Vec<ProcessEntry>>,
    pub device_list: Option<fn() -> Vec<DeviceEntry>>,
    /// List mounted USB drives (name strings).
    pub usb_drive_list: Option<fn() -> Vec<(alloc::string::String, alloc::string::String)>>,
    /// Poll USB controllers for newly connected devices. Returns true if new drive mounted.
    pub usb_poll: Option<fn() -> bool>,
}

impl SolventCallbacks {
    pub const fn none() -> Self {
        Self {
            shell_cmd: None,
            launch_shell: None,
            heap_extend: None,
            wall_clock: None,
            vfs_readdir: None,
            vfs_read: None,
            vfs_write: None,
            vfs_create: None,
            vfs_mkdir: None,
            vfs_unlink: None,
            process_list: None,
            device_list: None,
            usb_drive_list: None,
            usb_poll: None,
        }
    }
    pub fn install(self) {
        *SOLVENT_CALLBACKS.lock() = self;
    }
}

pub static SOLVENT_CALLBACKS: Mutex<SolventCallbacks> = Mutex::new(SolventCallbacks::none());

pub fn exec_shell_command(input: &str) -> String {
    let cb = SOLVENT_CALLBACKS.lock();
    if let Some(f) = cb.shell_cmd {
        drop(cb);
        f(input)
    } else {
        String::from("(no shell)\n")
    }
}

pub fn launch_shell() {
    let cb = SOLVENT_CALLBACKS.lock();
    if let Some(f) = cb.launch_shell {
        drop(cb);
        f();
    }
}

// ── Utility ──────────────────────────────────────────────────

pub(crate) fn truncate_to_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ── Constants ────────────────────────────────────────────────
const DEFAULT_COLS: u32 = 80;
const DEFAULT_ROWS: u32 = 25;
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 16;
const TERM_WIN_W: u32 = DEFAULT_COLS * GLYPH_W;
const TERM_WIN_H: u32 = DEFAULT_ROWS * GLYPH_H;
const BG_COLOR: u32 = 0x1a1a2e;
const CURSOR_BLINK_INTERVAL: u64 = 100;
const CURSOR_TIMER_ID: TimerId = TimerId(1);
/// Mouse sensitivity multiplier (set by kernel from SettingsContext).
/// Default = 6 (legacy default).  The kernel updates this whenever the
/// user changes the mouse sensitivity setting.
pub static MOUSE_SENSITIVITY: core::sync::atomic::AtomicI16 =
    core::sync::atomic::AtomicI16::new(6);

/// Apply settings from the kernel (called at boot and when settings change).
pub fn apply_settings(sensitivity: f32, brightness_x100: u32, top_panel_enabled: bool) {
    let sens_i16 = (sensitivity * 6.0) as i16; // scale to legacy multiplier
    MOUSE_SENSITIVITY.store(sens_i16, core::sync::atomic::Ordering::Relaxed);
    DISPLAY_BRIGHTNESS_X100.store(brightness_x100, core::sync::atomic::Ordering::Relaxed);
    lattice::top_panel::set_top_panel_enabled(top_panel_enabled);
    force_desktop_redraw();
}

/// Software display brightness × 100 (set by kernel from SettingsContext).
/// Default = 100 (1.0×).  Range: 10..100.
/// Applied in the compositor as a post‑processing step.
pub static DISPLAY_BRIGHTNESS_X100: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(100);
const FRAME_INTERVAL_TICKS: u64 = 8;
const FRAME_INTERVAL_MS: u64 = 17;
const FRAME_TIMER_ID: TimerId = TimerId(2);
pub(crate) static TSC_PER_MS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(3_000_000);
const MAX_FB_PIXELS: usize = 3840 * 2160;

pub fn get_usb_drives() -> alloc::vec::Vec<(alloc::string::String, alloc::string::String)> {
    SOLVENT_CALLBACKS.lock().usb_drive_list
        .map(|f| f())
        .unwrap_or_default()
}

pub fn set_tsc_per_ms(val: u64) {
    TSC_PER_MS.store(val, core::sync::atomic::Ordering::Relaxed);
}
pub fn get_tsc_per_ms() -> u64 {
    TSC_PER_MS.load(core::sync::atomic::Ordering::Relaxed)
}

static LAST_RENDER_TSC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static HEAP_EXTEND_RESERVE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// ── VFS / Process / Device types ─────────────────────────────

#[derive(Debug, Clone)]
pub struct VfsEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}
#[derive(Debug, Clone)]
pub struct ProcessEntry {
    pub pid: u64,
    pub name: String,
    pub state: ProcessStateKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStateKind {
    Ready,
    Running,
    Blocked,
    Terminated,
}
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub name: String,
    pub dev_type: String,
    pub enabled: bool,
}

static CLOCK_STRING: Mutex<String> = Mutex::new(String::new());
pub fn clock_string() -> String {
    CLOCK_STRING.lock().clone()
}
pub static GLOBAL_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// ── Static back‑buffer (BSS) ─────────────────────────────────
static BACK_BUFFER: Mutex<[u32; MAX_FB_PIXELS]> = Mutex::new([0u32; MAX_FB_PIXELS]);

// ── Runtime state ────────────────────────────────────────────
pub(crate) static RUNTIME: Mutex<Option<RuntimeState>> = Mutex::new(None);
static EVENT_QUEUE: Mutex<Option<EventQueue>> = Mutex::new(None);
static DISPATCHER: Mutex<Option<Dispatcher>> = Mutex::new(None);
pub(crate) static PREV_MOUSE_BUTTONS: Mutex<u8> = Mutex::new(0);
pub(crate) static FB_DIMS: Mutex<(u32, u32, u32)> = Mutex::new((1024, 768, 1024));
pub(crate) static LAST_FB: Mutex<(usize, u32, u32, u32)> = Mutex::new((0, 0, 0, 0));

pub struct RuntimeState {
    pub desktop: Desktop,
    pub term_window: Option<WindowId>,
    pub term_buf: TerminalBuffer,
    pub chrono: ChronoLine,
    pub cursor_visible: bool,
    pub frame_due: bool,
    pub back_len: usize,
    pub term_cells: Vec<LatticeCell>,
    pub term_dirty: bool,
    pub shell_state: ShellState,
    pub shell_launch_pending: bool,
    pub clock_changed: bool,
    pub cursor_save_buf:
        [u32; lattice::cursor::Cursor::SIZE as usize * lattice::cursor::Cursor::SIZE as usize],
    pub cursor_save_x: i32,
    pub cursor_save_y: i32,
    pub cursor_save_valid: bool,
    /// Editor state
    pub editor_window: Option<WindowId>,
    pub editor_buf: EditorBuffer,
    pub editor_launch_pending: bool,
    pub editor_dirty: bool,
    /// Path of the file currently open in the editor (for save).
    pub editor_file_path: Option<alloc::string::String>,
    /// File explorer state
    pub explorer: Option<explorer::ExplorerContext>,
    pub explorer_dirty: bool,
    /// Settings interactive state
    pub settings_window: Option<WindowId>,
    pub settings_dirty: bool,
}

pub fn init() {
    let desktop = Desktop::new(BG_COLOR);
    // Terminal window is created lazily when the user clicks the Shell icon.
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

    dispatcher.register(Box::new(handlers::WmEventHandler));
    dispatcher.register(Box::new(handlers::TerminalInputHandler));
    dispatcher.register(Box::new(handlers::ShellEventHandler));

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
        term_window: None,
        term_buf,
        chrono,
        cursor_visible: true,
        frame_due: true,
        back_len: 0,
        term_cells: Vec::new(),
        term_dirty: true,
        shell_state: ShellState::Desktop,
        shell_launch_pending: false,
        clock_changed: false,
        cursor_save_buf: [0u32;
            lattice::cursor::Cursor::SIZE as usize * lattice::cursor::Cursor::SIZE as usize],
        cursor_save_x: 0,
        cursor_save_y: 0,
        cursor_save_valid: false,
        editor_window: None,
        editor_buf: EditorBuffer::new(),
        editor_launch_pending: false,
        editor_dirty: false,
        editor_file_path: None,
        explorer: None,
        explorer_dirty: false,
        settings_window: None,
        settings_dirty: false,
    });
}

pub fn is_initialized() -> bool {
    RUNTIME.lock().is_some()
}

// ── Lightweight cursor update ────────────────────────────────

pub(crate) fn cursor_lightweight_update(rt: &mut RuntimeState) {
    let (fb_addr, fbw, fbh, fb_stride) = *LAST_FB.lock();
    if fb_addr == 0 || fbh == 0 {
        rt.frame_due = true;
        return;
    }
    let fb_ptr = fb_addr as *mut u32;
    let cur = &rt.desktop.cursor;
    if !cur.visible {
        return;
    }

    let cur_sz = lattice::cursor::Cursor::SIZE as i32;
    let new_x = cur.x - lattice::cursor::Cursor::HOTSPOT_X;
    let new_y = cur.y - lattice::cursor::Cursor::HOTSPOT_Y;
    let fbw_i = fbw as i32;
    let stride_i = fb_stride as i32;
    let fbh_i = fbh as i32;
    let fb_len = (fb_stride as usize).saturating_mul(fbh as usize);

    unsafe {
        let fb = core::slice::from_raw_parts_mut(fb_ptr, fb_len);
        if rt.cursor_save_valid {
            let sx = rt.cursor_save_x;
            let sy = rt.cursor_save_y;
            for row in 0..cur_sz {
                let dy = sy + row;
                if dy < 0 || dy >= fbh_i {
                    continue;
                }
                for col in 0..cur_sz {
                    let dx = sx + col;
                    if dx < 0 || dx >= fbw_i {
                        continue;
                    }
                    let idx = (dy * stride_i + dx) as usize;
                    if idx < fb_len {
                        fb[idx] = rt.cursor_save_buf[(row * cur_sz + col) as usize];
                    }
                }
            }
        }
        rt.cursor_save_x = new_x;
        rt.cursor_save_y = new_y;
        for row in 0..cur_sz {
            let sy = new_y + row;
            for col in 0..cur_sz {
                let val = if sy >= 0 && sy < fbh_i {
                    let sx = new_x + col;
                    if sx >= 0 && sx < fbw_i {
                        let idx = (sy * stride_i + sx) as usize;
                        if idx < fb_len { fb[idx] } else { 0 }
                    } else {
                        0
                    }
                } else {
                    0
                };
                rt.cursor_save_buf[(row * cur_sz + col) as usize] = val;
            }
        }
        rt.cursor_save_valid = true;
        Compositor::draw_cursor_direct(fb, fb_stride, fbh, cur);
    }
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
        let old_x = mouse.x;
        let old_y = mouse.y;
        let sens = MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed);
        mouse.x = mouse.x.wrapping_add(dx.wrapping_mul(sens));
        mouse.y = mouse
            .y
            .wrapping_add(dy.wrapping_mul(sens).wrapping_neg());
        mouse.buttons = btn;
        let cx = mouse.x as i32;
        let cy = mouse.y as i32;
        let buttons = mouse.buttons;
        let moved = old_x != mouse.x || old_y != mouse.y;
        drop(mouse);
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

// ── Keyboard polling ─────────────────────────────────────────

pub fn poll_keyboard() {
    while nitrogen::ps2::keyboard::raw_key_available() {
        let (scancode, pressed) = match nitrogen::ps2::keyboard::pop_raw_key() {
            Some(k) => k,
            None => break,
        };

        // Route key events to the topmost window if it's editor or settings.
        // Use a single lock acquisition to check and dispatch atomically.
        let mut rt = RUNTIME.lock();
        if let Some(ref mut r) = *rt {
            let wms = r.desktop.wm.windows();
            let top_id = wms.last().map(|w| w.id);
            // Editor routing
            if top_id.is_some()
                && r.editor_window == top_id
            {
                drop(rt);
                editor_handle_key(scancode, pressed);
                let key = scancode_to_resonance_keycode(scancode);
                let event = if pressed {
                    Event::Input(InputEvent::KeyDown(key))
                } else {
                    Event::Input(InputEvent::KeyUp(key))
                };
                if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
                    queue.push(event);
                }
                continue;
            }
            // Settings routing
            if top_id.is_some()
                && r.settings_window == top_id
            {
                settings_handle_key_inner(r, scancode, pressed);
                continue;
            }
        }
        drop(rt);

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

fn scancode_to_resonance_keycode(scancode: u8) -> KeyCode {
    const EXT: [Option<KeyCode>; 128] = {
        let mut t = [None; 128];
        t[0x1D] = Some(KeyCode::Ctrl);
        t[0x38] = Some(KeyCode::Alt);
        t[0x5B] = Some(KeyCode::SuperLeft);
        t[0x5C] = Some(KeyCode::SuperRight);
        t
    };
    const BASE: [KeyCode; 128] = {
        use KeyCode::*;
        let mut t = [Unknown(0); 128];
        let mut i = 0;
        while i < 128 {
            t[i] = Unknown(i as u32);
            i += 1;
        }
        t[0x01] = Escape;
        t[0x02] = Digit1;
        t[0x03] = Digit2;
        t[0x04] = Digit3;
        t[0x05] = Digit4;
        t[0x06] = Digit5;
        t[0x07] = Digit6;
        t[0x08] = Digit7;
        t[0x09] = Digit8;
        t[0x0A] = Digit9;
        t[0x0B] = Digit0;
        t[0x0E] = Backspace;
        t[0x0F] = Tab;
        t[0x10] = Q;
        t[0x11] = W;
        t[0x12] = E;
        t[0x13] = R;
        t[0x14] = T;
        t[0x15] = Y;
        t[0x16] = U;
        t[0x17] = I;
        t[0x18] = O;
        t[0x19] = P;
        t[0x1C] = Enter;
        t[0x1D] = Ctrl;
        t[0x1E] = A;
        t[0x1F] = S;
        t[0x20] = D;
        t[0x21] = F;
        t[0x22] = G;
        t[0x23] = H;
        t[0x24] = J;
        t[0x25] = K;
        t[0x26] = L;
        t[0x2A] = Shift;
        t[0x2C] = Z;
        t[0x2D] = X;
        t[0x2E] = C;
        t[0x2F] = V;
        t[0x30] = B;
        t[0x31] = N;
        t[0x32] = M;
        t[0x36] = Shift;
        t[0x38] = Alt;
        t[0x39] = Space;
        t[0x3B] = F1;
        t[0x3C] = F2;
        t[0x3D] = F3;
        t[0x3E] = F4;
        t[0x3F] = F5;
        t[0x40] = F6;
        t[0x41] = F7;
        t[0x42] = F8;
        t[0x43] = F9;
        t[0x44] = F10;
        t[0x47] = Home;
        t[0x48] = Up;
        t[0x49] = PageUp;
        t[0x4B] = Left;
        t[0x4D] = Right;
        t[0x4F] = End;
        t[0x50] = Down;
        t[0x51] = PageDown;
        t[0x57] = F11;
        t[0x58] = F12;
        t
    };
    let base = scancode & 0x7F;
    if scancode & 0x80 != 0 {
        EXT[base as usize].unwrap_or_else(|| BASE[base as usize])
    } else {
        BASE[base as usize]
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

pub(crate) static TIMEZONE_OFFSET_HOURS: core::sync::atomic::AtomicI8 =
    core::sync::atomic::AtomicI8::new(9);

fn days_in_month(month: i16, year: i16) -> i16 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 31,
    }
}

pub fn update_clock() {
    let offset = TIMEZONE_OFFSET_HOURS.load(core::sync::atomic::Ordering::Relaxed);
    let time_str = if let Some(get_time) = SOLVENT_CALLBACKS.lock().wall_clock {
        if let Some((year, month, day, hour, minute, _second)) = get_time() {
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
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32, u32)>,
{
    // Prevent re-entrancy: if a timer IRQ fires while we hold RUNTIME.lock(),
    // the inner runtime_tick() → process_events() would spin forever trying to
    // acquire the same lock.  Setting RENDERING_SUSPENDED tells runtime_tick()
    // to bail out immediately.
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }

    struct SuspendGuard;
    impl Drop for SuspendGuard {
        fn drop(&mut self) {
            RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
        }
    }
    let _guard = SuspendGuard;

    let mut rt_lock = RUNTIME.lock();
    let rt = match rt_lock.as_mut() {
        Some(r) => r,
        None => return,
    };

    static PREV_SHELL_STATE: Mutex<ShellState> = Mutex::new(ShellState::Desktop);
    static PREV_TRANSITION: Mutex<bool> = Mutex::new(false);
    {
        let prev = *PREV_SHELL_STATE.lock();
        if rt.shell_state != prev {
            rt.desktop.force_full_redraw();
            *PREV_SHELL_STATE.lock() = rt.shell_state;
            *PREV_TRANSITION.lock() = true;
        }
    }

    render_terminal(rt, rt.term_window);

    // Detect editor surface-size mismatch (e.g. after maximize) and
    // force a re-render even when `editor_dirty` is false.  The window
    // manager resizes the window but not the surface, so the compositor
    // would fill the extra area with the surface's bg_fallback colour
    // instead of editor text cells.
    if !rt.editor_dirty {
        if let Some(editor_id) = rt.editor_window {
            if let Some(w) = rt.desktop.wm.windows().iter().find(|w| w.id == editor_id) {
                let new_cols = (w.width / GLYPH_W).max(1);
                let new_rows = (w.height / GLYPH_H).max(1);
                if w.surface.width() != new_cols * GLYPH_W
                    || w.surface.height() != new_rows * GLYPH_H
                {
                    rt.editor_dirty = true;
                }
            }
        }
    }

    if rt.editor_dirty {
        render_editor(rt);
    }
    if rt.explorer_dirty {
        render_explorer(rt);
    }
    if rt.settings_dirty {
        render_settings(rt);
    }
    let tb_changed = rt.desktop.update_taskbar();
    let (fb_pixels, fb_width, fb_height, fb_stride_pixels) = match framebuffer_fn() {
        Some(t) => t,
        None => return,
    };
    *FB_DIMS.lock() = (fb_width, fb_height, fb_stride_pixels);
    *LAST_FB.lock() = (
        fb_pixels.as_mut_ptr() as usize,
        fb_width,
        fb_height,
        fb_stride_pixels,
    );

    let bar_h = lattice::taskbar::TASKBAR_HEIGHT;
    if rt.clock_changed || tb_changed {
        rt.desktop.push_dirty_rect(lattice::scene::DirtyRect::new(
            0,
            fb_height.saturating_sub(bar_h),
            fb_width,
            bar_h,
        ));
    }
    // Top panel dirty rect (when clock changes or enabled)
    if rt.clock_changed {
        let panel_h = if lattice::top_panel::is_top_panel_enabled() {
            lattice::top_panel::TOP_PANEL_HEIGHT
        } else {
            0
        };
        if panel_h > 0 {
            rt.desktop
                .push_dirty_rect(lattice::scene::DirtyRect::new(0, 0, fb_width, panel_h));
        }
    }
    rt.clock_changed = false;

    rt.desktop.prepare_frame(fb_width, fb_height);
    let fb_stride = fb_stride_pixels as usize;
    let fb_len = fb_stride.saturating_mul(fb_height as usize);
    let back_len = (fb_width as usize) * (fb_height as usize);
    if fb_len > MAX_FB_PIXELS || back_len > MAX_FB_PIXELS {
        return;
    }
    rt.back_len = back_len;

    let has_dirty = rt.desktop.has_pending_dirty_rects();
    if has_dirty {
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
                pixels: &mut back[..back_len],
                width: fb_width,
                height: fb_height,
            };
            let scene = rt.desktop.scene();
            let (bx, by, bw, bh) = Compositor::render(&scene, &mut back_target);
            if was_transition || (bw > 0 && bh > 0) {
                let back_w = fb_width as usize;
                if was_transition {
                    for row in 0..fb_height as usize {
                        let src_off = row * back_w;
                        let dst_off = row * fb_stride;
                        let copy_len = back_w.min(back_len.saturating_sub(src_off));
                        if copy_len > 0 {
                            unsafe {
                                copy_to_fb_volatile(
                                    fb_pixels.as_mut_ptr().add(dst_off),
                                    back.as_ptr().add(src_off),
                                    copy_len,
                                );
                            }
                        }
                    }
                } else {
                    let b_w = bw as usize;
                    for row in 0..bh {
                        let src_off = ((by + row) as usize) * back_w + (bx as usize);
                        let dst_off = ((by + row) as usize) * fb_stride + (bx as usize);
                        let len = b_w
                            .min(back_len.saturating_sub(src_off))
                            .min(fb_len.saturating_sub(dst_off));
                        if len > 0 {
                            unsafe {
                                copy_to_fb_volatile(
                                    fb_pixels.as_mut_ptr().add(dst_off),
                                    back.as_ptr().add(src_off),
                                    len,
                                );
                            }
                        }
                    }
                }
            }
        }

        match rt.shell_state {
            ShellState::TaskOverview => render_task_overview(
                fb_pixels,
                fb_width,
                fb_height,
                fb_stride_pixels,
                rt.desktop.wm.windows(),
            ),
            ShellState::AppGrid => {
                render_app_grid(fb_pixels, fb_width, fb_height, fb_stride_pixels)
            }
            ShellState::TimeZoneSelector => {
                let offset = TIMEZONE_OFFSET_HOURS.load(core::sync::atomic::Ordering::Relaxed);
                lattice::shell_overlay::render_timezone_selector(
                    fb_pixels,
                    fb_width,
                    fb_height,
                    fb_stride_pixels,
                    offset,
                );
            }
            ShellState::Desktop => {}
        }

        // Render top panel only when enabled
        if rt.shell_state == ShellState::Desktop
            && lattice::top_panel::is_top_panel_enabled()
        {
            rt.desktop
                .top_panel
                .render(fb_pixels, fb_width, fb_height, fb_stride_pixels);
        }

        if rt.desktop.cursor.visible {
            let (fb_addr, _, _, fb_stride) = *LAST_FB.lock();
            if fb_addr != 0 {
                let fb_ptr = fb_addr as *mut u32;
                let cur_sz = lattice::cursor::Cursor::SIZE as i32;
                let cx = rt.desktop.cursor.x - lattice::cursor::Cursor::HOTSPOT_X;
                let cy = rt.desktop.cursor.y - lattice::cursor::Cursor::HOTSPOT_Y;
                let fbw_i = fb_width as i32;
                let stride_i = fb_stride as i32;
                let fbh_i = fb_height as i32;
                let fb_len = (fb_stride as usize).saturating_mul(fb_height as usize);
                unsafe {
                    let fb = core::slice::from_raw_parts(fb_ptr, fb_len);
                    for row in 0..cur_sz {
                        let sy = cy + row;
                        for col in 0..cur_sz {
                            let val = if sy >= 0 && sy < fbh_i {
                                let sx = cx + col;
                                if sx >= 0 && sx < fbw_i {
                                    let idx = (sy * stride_i + sx) as usize;
                                    if idx < fb_len { fb[idx] } else { 0 }
                                } else {
                                    0
                                }
                            } else {
                                0
                            };
                            rt.cursor_save_buf[(row * cur_sz + col) as usize] = val;
                        }
                    }
                }
                rt.cursor_save_x = cx;
                rt.cursor_save_y = cy;
                rt.cursor_save_valid = true;
            }
        }

        if rt.desktop.cursor.visible {
            Compositor::draw_cursor_direct(
                fb_pixels,
                fb_stride_pixels,
                fb_height,
                &rt.desktop.cursor,
            );
        }

        // ── Post‑processing: apply software brightness ─────
        let brightness = DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed);
        if brightness < 100 {
            let fb_stride_u = fb_stride_pixels as usize;
            for row in 0..fb_height as usize {
                let row_off = row * fb_stride_u;
                for col in 0..fb_width as usize {
                    let idx = row_off + col;
                    if idx < fb_len {
                        fb_pixels[idx] = lattice::compositor::apply_brightness(fb_pixels[idx], brightness);
                    }
                }
            }
        }
    }
}

unsafe fn copy_to_fb_volatile(dst: *mut u32, src: *const u32, len: usize) {
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, len);
    }
}

fn render_terminal(rt: &mut RuntimeState, term_window: Option<WindowId>) {
    if !rt.term_dirty {
        return;
    }
    let term_window = match term_window {
        Some(id) => id,
        None => return,
    };
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
    let new_cols = (window.width / GLYPH_W).max(1);
    let new_rows = (window.height / GLYPH_H).max(1);
    let cur_cols = rt.term_buf.cols();
    let cur_rows = rt.term_buf.rows();

    if new_cols != cur_cols || new_rows != cur_rows {
        let new_surface_pixels = (new_cols * new_rows * GLYPH_W * GLYPH_H) as usize;
        let new_buf_cells = (new_cols * new_rows) as usize * 12;
        let needed = (new_surface_pixels * 4).saturating_add(new_buf_cells);
        if needed > HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed) {
            let additional = needed
                .saturating_sub(HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed))
                .next_multiple_of(4096);
            if let Some(extend_fn) = SOLVENT_CALLBACKS.lock().heap_extend {
                if extend_fn(additional).is_err() {
                    return;
                } else {
                    HEAP_EXTEND_RESERVE
                        .fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
                }
            } else {
                return;
            }
        }
        // Save the old cursor position before replacing the buffer.
        let old_cur_col = rt.term_buf.cursor_col();
        let old_cur_row = rt.term_buf.cursor_row();
        let new_buf = TerminalBuffer::new(new_cols, new_rows);
        let old_buf = core::mem::replace(&mut rt.term_buf, new_buf);
        {
            let src_cells = old_buf.cells();
            let src_cols = cur_cols as usize;
            for row in 0..(cur_rows as usize).min(new_rows as usize) {
                for col in 0..(cur_cols as usize).min(new_cols as usize) {
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
        // Restore the cursor to its old position, clamped to the new dimensions.
        rt.term_buf.set_cursor(
            old_cur_col.min(new_cols.saturating_sub(1)),
            old_cur_row.min(new_rows.saturating_sub(1)),
        );
        let _ = old_buf;
        window.surface = lattice::surface::Surface::new(
            new_cols * GLYPH_W,
            new_rows * GLYPH_H,
            window.surface.get_pixel(0, 0).unwrap_or(0x000000),
        );
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
    let visible = term_buf.visible_cells();
    rt.term_cells.resize(
        visible.len(),
        LatticeCell {
            ch: b' ',
            fg: 0,
            bg: 0,
        },
    );
    for (i, c) in visible.iter().enumerate() {
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
        if let Some(ref mut out) = *PIPE_STDOUT.lock() {
            out.push_str(s);
        } else {
            let mut rt = RUNTIME.lock();
            if let Some(ref mut r) = *rt {
                r.term_buf.put_str(s);
                r.term_dirty = true;
            }
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
    fn set_stdin(&mut self, data: String) {
        *PIPE_STDIN.lock() = Some(data);
    }
    fn take_stdout(&mut self) -> Option<String> {
        PIPE_STDOUT.lock().take()
    }
    fn take_stdin(&mut self) -> Option<String> {
        PIPE_STDIN.lock().take()
    }
    fn arm_pipe_stdout(&mut self) {
        *PIPE_STDOUT.lock() = Some(String::new());
    }
    fn clear_pipe_stdin(&mut self) {
        *PIPE_STDIN.lock() = None;
    }
}

static PIPE_STDIN: Mutex<Option<String>> = Mutex::new(None);
static PIPE_STDOUT: Mutex<Option<String>> = Mutex::new(None);

static YIELD_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static RENDER_FN: Mutex<Option<fn()>> = Mutex::new(None);

pub fn set_render_fn(f: fn()) {
    *RENDER_FN.lock() = Some(f);
}

fn runtime_tick_no_fb() {
    // Prevent re‑entrancy from timer IRQs while this tick is in progress.
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);
    poll_mouse_state();
    poll_keyboard();
    update_clock();
    chrono_tick(now);
    process_events();
    // Check for deferred shell launch (set by handlers while RUNTIME lock was held).
    // Must be done outside the RUNTIME lock to avoid deadlock.
    if RUNTIME.lock().as_mut().map_or(false, |r| {
        let pending = r.shell_launch_pending;
        r.shell_launch_pending = false;
        pending
    }) {
        ensure_terminal_window();
        launch_shell();
    }
    // Check for deferred editor launch.
    if RUNTIME.lock().as_mut().map_or(false, |r| {
        let pending = r.editor_launch_pending;
        r.editor_launch_pending = false;
        pending
    }) {
        ensure_editor_window();
    }
    let do_render = RUNTIME.lock().as_mut().map_or(false, |r| {
        let due = r.frame_due;
        if due {
            let tsc_per_ms = TSC_PER_MS.load(core::sync::atomic::Ordering::Relaxed);
            let frame_tsc = tsc_per_ms.saturating_mul(FRAME_INTERVAL_MS);
            let last = LAST_RENDER_TSC.load(core::sync::atomic::Ordering::Relaxed);
            let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
            if now_tsc.wrapping_sub(last) < frame_tsc {
                r.frame_due = true;
                return false;
            }
            LAST_RENDER_TSC.store(now_tsc, core::sync::atomic::Ordering::Relaxed);
            r.frame_due = false;
        }
        due
    });
    // Clear the re‑entrancy flag before calling render(), so render()
    // can set it again for its own critical section (where it holds
    // RUNTIME.lock() and touches the framebuffer).
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
    if do_render {
        if let Some(render_fn) = *RENDER_FN.lock() {
            render_fn();
        }
    }
}

pub fn runtime_tick<F>(now: u64, framebuffer_fn: F)
where
    F: FnOnce() -> Option<(&'static mut [u32], u32, u32, u32)>,
{
    // Prevent re‑entrancy from timer IRQs while this tick is in progress.
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);
    poll_mouse_state();
    poll_keyboard();
    update_clock();
    chrono_tick(now);
    process_events();
    // Check for deferred shell launch (set by handlers while RUNTIME lock was held).
    // Must be done outside the RUNTIME lock to avoid deadlock.
    if RUNTIME.lock().as_mut().map_or(false, |r| {
        let pending = r.shell_launch_pending;
        r.shell_launch_pending = false;
        pending
    }) {
        ensure_terminal_window();
        launch_shell();
    }
    // Check for deferred editor launch.
    if RUNTIME.lock().as_mut().map_or(false, |r| {
        let pending = r.editor_launch_pending;
        r.editor_launch_pending = false;
        pending
    }) {
        ensure_editor_window();
    }
    // Poll USB every ~100 ticks (~2 seconds at 17ms/tick).
    // Callback pointer is extracted before invocation to avoid holding
    // SOLVENT_CALLBACKS lock while VFS locks are acquired inside poll_usb().
    static LAST_USB_POLL: core::sync::atomic::AtomicU64 =
        core::sync::atomic::AtomicU64::new(0);
    let tick = GLOBAL_TICK.load(core::sync::atomic::Ordering::Relaxed);
    if tick.wrapping_sub(LAST_USB_POLL.load(core::sync::atomic::Ordering::Relaxed)) >= 100 {
        LAST_USB_POLL.store(tick, core::sync::atomic::Ordering::Relaxed);
        let poll_fn = SOLVENT_CALLBACKS.lock().usb_poll;
        if let Some(f) = poll_fn {
            if f() {
                if let Some(ref mut r) = *RUNTIME.lock() {
                    if let Some(ref mut e) = r.explorer {
                        e.refresh_sidebar();
                        r.explorer_dirty = true;
                        r.frame_due = true;
                    }
                }
            }
        }
    }

    let do_render = RUNTIME.lock().as_mut().map_or(false, |r| {
        let due = r.frame_due;
        r.frame_due = false;
        due
    });
    // Clear the re‑entrancy flag before calling render(), so render()
    // can set it again for its own critical section.
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
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

// ── Rendering suspend / resume ───────────────────────────────

static RENDERING_SUSPENDED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub fn suspend_rendering() {
    RENDERING_SUSPENDED.store(true, core::sync::atomic::Ordering::SeqCst);
}
pub fn resume_rendering() {
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
}
pub fn force_desktop_redraw() {
    // Prevent re-entrancy from timer IRQs while we hold RUNTIME.lock().
    // This is called from sys_control hooks (shell commands like
    // "wallpaper mountain") which run in a different kernel process
    // context.  If a timer IRQ fires while we hold the lock, the inner
    // runtime_tick() would deadlock on the same spin::Mutex.
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(ref mut r) = *RUNTIME.lock() {
        r.desktop.force_full_redraw();
        r.frame_due = true;
    }
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
}

// ── Theme / wallpaper bridges ────────────────────────────────
pub use lattice::theme::{ThemeVariant, current_theme_variant, set_theme, toggle_theme};
pub use lattice::wallpaper::{
    WallpaperMode, WallpaperPreset, find_preset, get_wallpaper, set_wallpaper, wallpaper_presets,
};

// ── Window API (for external apps like RLE Player) ───────────
pub fn create_window(
    title: impl Into<String>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Option<WindowId> {
    RUNTIME.lock().as_mut().map(|rt| {
        rt.desktop
            .wm
            .create_titled_window(x, y, width, height, 0x000000, title)
    })
}
pub fn with_window_surface<F, R>(id: WindowId, f: F) -> Option<R>
where
    F: FnOnce(&mut [u32], u32, u32) -> R,
{
    RUNTIME.lock().as_mut().and_then(|rt| {
        let w = rt
            .desktop
            .wm
            .windows_mut()
            .iter_mut()
            .find(|w| w.id == id)?;
        if w.minimized {
            return None;
        }
        let ww = w.surface.width();
        let wh = w.surface.height();
        Some(f(w.surface.pixels_mut(), ww, wh))
    })
}
pub fn invalidate_window(id: WindowId) {
    if let Some(ref mut rt) = *RUNTIME.lock() {
        rt.desktop.invalidate_window(id);
        rt.frame_due = true;
        rt.term_dirty = true;
    }
}
pub fn close_window(id: WindowId) -> bool {
    RUNTIME
        .lock()
        .as_mut()
        .map_or(false, |rt| rt.desktop.wm.close_window(id))
}
pub fn framebuffer_dims() -> (u32, u32) {
    let (w, h, _) = *FB_DIMS.lock();
    (w, h)
}

/// Ensure a terminal window exists for the shell, creating one if necessary.
/// Returns the WindowId of the terminal window.
pub fn ensure_terminal_window() -> Option<WindowId> {
    let mut rt = RUNTIME.lock();
    let rt = rt.as_mut()?;
    if let Some(id) = rt.term_window {
        // Check the window still exists (hasn't been closed)
        if rt.desktop.wm.windows().iter().any(|w| w.id == id) {
            return Some(id);
        }
    }
    // Create a new terminal window
    let id = rt
        .desktop
        .wm
        .create_titled_window(40, 30, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
    rt.term_window = Some(id);
    rt.desktop.force_full_redraw();
    rt.frame_due = true;
    rt.term_dirty = true;
    Some(id)
}

// ── Editor ───────────────────────────────────────────────────

/// Ensure an editor window exists, creating one if necessary.
pub fn ensure_editor_window() -> Option<WindowId> {
    let mut rt = RUNTIME.lock();
    let rt = rt.as_mut()?;
    if let Some(id) = rt.editor_window {
        if rt.desktop.wm.windows().iter().any(|w| w.id == id) {
            return Some(id);
        }
    }
    let id = rt.desktop.wm.create_titled_window(
        100,
        80,
        DEFAULT_COLS * GLYPH_W,
        DEFAULT_ROWS * GLYPH_H,
        0x0a0a1e,
        "Text Editor",
    );
    rt.editor_window = Some(id);
    rt.editor_dirty = true;
    rt.desktop.force_full_redraw();
    rt.frame_due = true;
    Some(id)
}

/// Render the editor buffer into its window surface.
fn render_editor(rt: &mut RuntimeState) {
    let editor_window = match rt.editor_window {
        Some(id) => id,
        None => return,
    };
    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == editor_window)
    {
        Some(w) => w,
        None => {
            rt.editor_window = None;
            return;
        }
    };

    let new_cols = (window.width / GLYPH_W).max(1);
    let new_rows = (window.height / GLYPH_H).max(1);

    // Resize the editor surface when the window changes (e.g. maximize).
    // Extend the global heap if needed, same pattern as render_terminal.
    let cur_surf_w = window.surface.width();
    let cur_surf_h = window.surface.height();
    let new_surf_w = new_cols * GLYPH_W;
    let new_surf_h = new_rows * GLYPH_H;
    if cur_surf_w != new_surf_w || cur_surf_h != new_surf_h {
        let new_pixels = (new_surf_w * new_surf_h) as usize;
        let needed = new_pixels * 4;
        let reserve = HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed);
        if needed > reserve {
            let additional = needed
                .saturating_sub(reserve)
                .next_multiple_of(4096);
            if let Some(extend_fn) = SOLVENT_CALLBACKS.lock().heap_extend {
                if extend_fn(additional).is_ok() {
                    HEAP_EXTEND_RESERVE.fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        let bg = window.surface.get_pixel(0, 0).unwrap_or(0x0a0a1e);
        window.surface = lattice::surface::Surface::new(new_surf_w, new_surf_h, bg);
    }

    rt.editor_buf.ensure_cursor_visible(new_rows as usize);

    let visible = rt.editor_buf.visible_lines(new_rows as usize);
    let total = (new_cols * new_rows) as usize;
    let mut cells: Vec<LatticeCell> = Vec::with_capacity(total);
    cells.resize(
        total,
        LatticeCell {
            ch: b' ',
            fg: 0xCCCCCC,
            bg: 0x0a0a1e,
        },
    );

    let scroll = rt.editor_buf.scroll_row;
    for (row_idx, line) in visible.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            if col < new_cols as usize {
                let cell_idx = row_idx * (new_cols as usize) + col;
                if cell_idx < total {
                    cells[cell_idx] = LatticeCell {
                        ch: if ch.is_ascii() { ch as u8 } else { b'?' },
                        fg: 0xCCCCCC,
                        bg: 0x0a0a1e,
                    };
                }
            }
        }
    }

    // Draw cursor if it's in the visible range
    if rt.editor_buf.cursor_row >= scroll
        && rt.editor_buf.cursor_row < scroll + new_rows as usize
    {
        let cursor_display_row = rt.editor_buf.cursor_row - scroll;
        let cursor_display_col = rt.editor_buf.cursor_col.min((new_cols - 1) as usize);
        let cursor_idx = cursor_display_row * (new_cols as usize) + cursor_display_col;
        if cursor_idx < total && rt.cursor_visible {
            cells[cursor_idx] = LatticeCell {
                ch: cells[cursor_idx].ch,
                fg: 0x0a0a1e,
                bg: 0xCCCCCC,
            };
        }
    }

    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &cells,
        cols: new_cols,
        cursor_col: None,
        cursor_row: None,
        cursor_visible: false,
    });
    rt.desktop.invalidate_window(editor_window);
    rt.editor_dirty = false;
}

// ── Settings (interactive) ───────────────────────────────────

/// Selected row in the settings UI (0=mouse, 1=brightness, 2=top panel).
/// Must be at module level so both `settings_handle_key_inner` and
/// `render_settings` see the same state.
static SETTINGS_SELECTED: Mutex<u32> = Mutex::new(0);

/// Handle a key event when the settings window is focused.
/// (Public entry point — acquires the runtime lock internally.)
pub fn settings_handle_key(scancode: u8, pressed: bool) {
    let mut rt = RUNTIME.lock();
    if let Some(ref mut r) = *rt {
        settings_handle_key_inner(r, scancode, pressed);
    }
}

fn settings_handle_key_inner(rt: &mut RuntimeState, scancode: u8, pressed: bool) {
    let key = crate::scancode_to_resonance_keycode(scancode);
    if !pressed {
        return;
    }

    let mut sel = SETTINGS_SELECTED.lock();

    match key {
        KeyCode::Up => {
            *sel = sel.saturating_sub(1).min(2);
        }
        KeyCode::Down => {
            *sel = (*sel + 1).min(2);
        }
        KeyCode::Left | KeyCode::Right => {
            let dec = key == KeyCode::Left;
            match *sel {
                0 => {
                    // Mouse sensitivity: step by 0.25
                    let cur = (crate::MOUSE_SENSITIVITY
                        .load(core::sync::atomic::Ordering::Relaxed) as f32)
                        / 6.0;
                    let step = 0.25f32;
                    let new_val = if dec {
                        (cur - step).max(0.25)
                    } else {
                        (cur + step).min(4.0)
                    };
                    let new_i16 = (new_val * 6.0) as i16;
                    crate::MOUSE_SENSITIVITY.store(new_i16, core::sync::atomic::Ordering::Relaxed);
                }
                1 => {
                    // Brightness: step by 5 (out of 100)
                    let cur = crate::DISPLAY_BRIGHTNESS_X100
                        .load(core::sync::atomic::Ordering::Relaxed) as i32;
                    let new_val = if dec {
                        (cur - 5).max(10)
                    } else {
                        (cur + 5).min(100)
                    };
                    crate::DISPLAY_BRIGHTNESS_X100
                        .store(new_val as u32, core::sync::atomic::Ordering::Relaxed);
                    rt.desktop.force_full_redraw();
                }
                2 => {
                    // Top panel toggle
                    if key == KeyCode::Right || key == KeyCode::Left {
                        lattice::top_panel::toggle_top_panel();
                        rt.desktop.force_full_redraw();
                    }
                }
                _ => {}
            }
        }
        KeyCode::Escape => {
            // Close settings window
            if let Some(id) = rt.settings_window.take() {
                rt.desktop.wm.close_window(id);
            }
            rt.settings_dirty = false;
            rt.frame_due = true;
            return;
        }
        _ => {}
    }
    drop(sel);
    rt.settings_dirty = true;
    rt.frame_due = true;
}

fn render_settings(rt: &mut RuntimeState) {
    let settings_id = match rt.settings_window {
        Some(id) => id,
        None => return,
    };
    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == settings_id)
    {
        Some(w) => w,
        None => {
            rt.settings_window = None;
            return;
        }
    };

    let sens = (crate::MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed) as f32) / 6.0;
    let bright = crate::DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed);
    let top_panel = lattice::top_panel::is_top_panel_enabled();

    let sel = *SETTINGS_SELECTED.lock();

    // Build the display text
    let prefix = |row: u32| -> &str {
        if row == sel { "> " } else { "  " }
    };

    let info = alloc::format!(
        "{}Settings\n\
         \n\
         {}Mouse Sensitivity: {:.2}\n\
         {}Display Brightness: {}.{:02}\n\
         {}Top Panel: {}",
        prefix(99), // title row (not selectable)
        prefix(0), sens,
        prefix(1), bright / 100, bright % 100,
        prefix(2), if top_panel { "ON " } else { "OFF" },
    );

    let info_bytes = info.as_bytes();
    let cols = 38u32;
    let total = cols as usize * 9usize;
    let mut cells: Vec<LatticeCell> = Vec::with_capacity(total);
    cells.resize(
        total,
        LatticeCell {
            ch: b' ',
            fg: 0xCCFFFF,
            bg: 0x0d1a1a,
        },
    );

    for (row, line) in info.lines().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if col < cols as usize {
                let idx = row * (cols as usize) + col;
                if idx < total {
                    cells[idx] = LatticeCell { ch, fg: 0xCCFFFF, bg: 0x0d1a1a };
                }
            }
        }
    }

    terminal_surface::render(terminal_surface::RenderParams {
        surface: &mut window.surface,
        cells: &cells,
        cols,
        cursor_col: None,
        cursor_row: None,
        cursor_visible: false,
    });
    rt.desktop.invalidate_window(settings_id);
    rt.settings_dirty = false;
}

// ── Explorer ──────────────────────────────────────────────────

fn render_explorer(rt: &mut RuntimeState) {
    let explorer = match rt.explorer.as_mut() {
        Some(e) => e,
        None => return,
    };
    let explorer_id = match explorer.window_id {
        Some(id) => id,
        None => return,
    };
    let window = match rt
        .desktop
        .wm
        .windows_mut()
        .iter_mut()
        .find(|w| w.id == explorer_id)
    {
        Some(w) => w,
        None => {
            rt.explorer = None;
            return;
        }
    };
    explorer::render_explorer(explorer, &mut window.surface);
    rt.desktop.invalidate_window(explorer_id);
    rt.explorer_dirty = false;
}

/// Launch a file based on its extension association.
///
/// Called when the user double-clicks a file in the explorer.
/// Reads the file content (if applicable) and opens the appropriate app.
pub fn launch_file(rt: &mut RuntimeState, path: &str) {
    let name = match path.rsplit('/').next() {
        Some(n) => n,
        None => path,
    };
    let ext = explorer::extension_of(name);
    let app = explorer::lookup_association(ext);

    // For text-based files, read content and open in editor
    let is_text = matches!(
        ext,
        "txt" | "md" | "log" | "toml" | "rs" | "c" | "h" | "py"
            | "js" | "json" | "xml" | "yml" | "yaml" | "ini"
            | "cfg" | "sh" | "bat" | "env" | "gitignore" | "lock"
    );

    if is_text {
        // Read file content in a separate scope to release SOLVENT_CALLBACKS lock
        let file_content = {
            let read_fn = match SOLVENT_CALLBACKS.lock().vfs_read {
                Some(f) => f,
                None => return,
            };
            match read_fn(path) {
                Ok(data) => match core::str::from_utf8(&data) {
                    Ok(s) => alloc::string::String::from(s),
                    Err(_) => return,
                },
                Err(_) => return,
            }
        };
        // Open editor with file content
        let id = rt.desktop.wm.create_titled_window(
            100, 80,
            DEFAULT_COLS * GLYPH_W, DEFAULT_ROWS * GLYPH_H,
            0x0a0a1e, "Text Editor",
        );
        if let Some(old_id) = rt.editor_window {
            if rt.desktop.wm.windows().iter().any(|w| w.id == old_id) {
                rt.desktop.wm.close_window(old_id);
            }
        }
        rt.editor_window = Some(id);
        rt.editor_buf = lattice::editor::EditorBuffer::from_text(&file_content);
        rt.editor_file_path = Some(alloc::string::String::from(path));
        rt.editor_dirty = true;
        rt.desktop.force_full_redraw();
        rt.frame_due = true;
        rt.explorer_dirty = true;
        return;
    }

    // Dispatch to format-specific viewers
    match ext {
        "bmp" => { crate::viewers::open_bmp(rt, path, name); return; }
        #[cfg(feature = "minipng")]
        "png" => { crate::viewers::open_png(rt, path, name); return; }
        "wav" => { crate::viewers::open_wav(rt, path, name); return; }
        #[cfg(feature = "rmp3")]
        "mp3" => { crate::viewers::open_mp3(rt, path, name); return; }
        #[cfg(feature = "shiguredo_mp4")]
        "mp4" => { crate::viewers::open_mp4(rt, path, name); return; }
        "tar" | "gz" | "xz" => { crate::viewers::open_tar(rt, path, name); return; }
        _ => {}
    }

    // Unknown file type: show info window
    let app_name = app.unwrap_or("Unknown");
    let msg = alloc::format!(
        "File: {}\nType: .{}\nApp: {}\n\nOpening {} is not yet implemented.",
        name, ext, app_name, app_name
    );
    let cols = 50;
    let rows = (msg.lines().count() as u32) + 3;
    let id = rt.desktop.wm.create_titled_window(
        200, 160, cols * GLYPH_W, rows * GLYPH_H,
        0x1a1a0d, "Open File",
    );
    if let Some(w) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = crate::menu_actions::render_text_into_surface(
            &mut w.surface, &msg, cols, 0xFFFFCC, 0x1a1a0d,
        );
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

/// Save the current editor buffer to its associated file.
fn editor_save_current(rt: &mut RuntimeState) {
    let path = match rt.editor_file_path.as_ref() {
        Some(p) => p.clone(),
        None => return,
    };
    let content = rt.editor_buf.full_text();
    // Write via VFS callback
    let write_fn = match SOLVENT_CALLBACKS.lock().vfs_write {
        Some(f) => f,
        None => return,
    };
    let result = write_fn(&path, content.as_bytes());
    if result.is_ok() {
        rt.editor_buf.dirty = false;
    }
    rt.editor_dirty = true;
    rt.frame_due = true;
}

/// Handle a key event for the editor.
pub fn editor_handle_key(scancode: u8, pressed: bool) {
    let key = crate::scancode_to_resonance_keycode(scancode);
    let mut rt = RUNTIME.lock();
    let rt = match rt.as_mut() {
        Some(r) => r,
        None => return,
    };

    static EDITOR_CTRL_HELD: core::sync::atomic::AtomicBool =
        core::sync::atomic::AtomicBool::new(false);
    if key == KeyCode::Ctrl {
        EDITOR_CTRL_HELD.store(pressed, core::sync::atomic::Ordering::Relaxed);
        return;
    }
    if key == KeyCode::S && EDITOR_CTRL_HELD.load(core::sync::atomic::Ordering::Relaxed) && pressed {
        editor_save_current(rt);
        return;
    }

    // Ignore key release events for all other keys — only presses produce editor actions
    if !pressed {
        return;
    }

    let viewport = |rt: &RuntimeState| -> usize {
        rt.editor_window.and_then(|id| {
            rt.desktop.wm.windows().iter().find(|w| w.id == id)
                .map(|w| (w.height / GLYPH_H).max(1) as usize)
        }).unwrap_or(10)
    };

    let vp = viewport(rt);

    match key {
        KeyCode::Enter => { rt.editor_buf.insert_char(b'\n'); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Backspace => { rt.editor_buf.backspace(); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Left => { rt.editor_buf.cursor_left(); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Right => { rt.editor_buf.cursor_right(); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Up => { rt.editor_buf.cursor_up(); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Down => { rt.editor_buf.cursor_down(); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Home => { rt.editor_buf.cursor_home(); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::End => { rt.editor_buf.cursor_end(); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::PageUp => { rt.editor_buf.page_up(vp); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::PageDown => { rt.editor_buf.page_down(vp); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Space => { rt.editor_buf.insert_char(b' '); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        KeyCode::Tab => { rt.editor_buf.insert_char(b' '); rt.editor_buf.insert_char(b' '); rt.editor_buf.clamp_scroll_with_viewport(vp); }
        _ => {
            if let Some(byte) = key_to_char(key) {
                rt.editor_buf.insert_char(byte);
                rt.editor_buf.clamp_scroll_with_viewport(vp);
            } else { return; }
        }
    }
    rt.editor_dirty = true;
    rt.frame_due = true;
}

fn key_to_char(key: KeyCode) -> Option<u8> {
    match key {
        KeyCode::A => Some(b'a'), KeyCode::B => Some(b'b'), KeyCode::C => Some(b'c'),
        KeyCode::D => Some(b'd'), KeyCode::E => Some(b'e'), KeyCode::F => Some(b'f'),
        KeyCode::G => Some(b'g'), KeyCode::H => Some(b'h'), KeyCode::I => Some(b'i'),
        KeyCode::J => Some(b'j'), KeyCode::K => Some(b'k'), KeyCode::L => Some(b'l'),
        KeyCode::M => Some(b'm'), KeyCode::N => Some(b'n'), KeyCode::O => Some(b'o'),
        KeyCode::P => Some(b'p'), KeyCode::Q => Some(b'q'), KeyCode::R => Some(b'r'),
        KeyCode::S => Some(b's'), KeyCode::T => Some(b't'), KeyCode::U => Some(b'u'),
        KeyCode::V => Some(b'v'), KeyCode::W => Some(b'w'), KeyCode::X => Some(b'x'),
        KeyCode::Y => Some(b'y'), KeyCode::Z => Some(b'z'),
        KeyCode::Digit1 => Some(b'1'), KeyCode::Digit2 => Some(b'2'), KeyCode::Digit3 => Some(b'3'),
        KeyCode::Digit4 => Some(b'4'), KeyCode::Digit5 => Some(b'5'), KeyCode::Digit6 => Some(b'6'),
        KeyCode::Digit7 => Some(b'7'), KeyCode::Digit8 => Some(b'8'), KeyCode::Digit9 => Some(b'9'),
        KeyCode::Digit0 => Some(b'0'),
        _ => None,
    }
}

// ── Shell bootstrap (kernel→solvent direction per AGENTS.md §3)
pub fn run_shell_on(terminal: &mut dyn nozzle::Terminal, prompt: &str) {
    let commands = nozzle::default_commands();
    let mut shell = nozzle::Shell::new(terminal, commands);
    shell.set_prompt(prompt);
    shell.run();
}

/// Track whether a Super key is held (for shortcuts).
pub(crate) static SUPER_HELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
