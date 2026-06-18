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
    pub process_list: Option<fn() -> Vec<ProcessEntry>>,
    pub device_list: Option<fn() -> Vec<DeviceEntry>>,
}

impl SolventCallbacks {
    pub const fn none() -> Self {
        Self {
            shell_cmd: None,
            launch_shell: None,
            heap_extend: None,
            wall_clock: None,
            vfs_readdir: None,
            process_list: None,
            device_list: None,
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
pub(crate) const MOUSE_SENSITIVITY: i16 = 6;
const FRAME_INTERVAL_TICKS: u64 = 8;
const FRAME_INTERVAL_MS: u64 = 17;
const FRAME_TIMER_ID: TimerId = TimerId(2);
pub(crate) static TSC_PER_MS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(3_000_000);
const MAX_FB_PIXELS: usize = 3840 * 2160;

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

        // Route key events to editor only when editor window exists
        // and is the topmost (focused) window.
        let editor_active = RUNTIME.lock().as_ref().map_or(false, |r| {
            if let Some(editor_id) = r.editor_window {
                let wms = r.desktop.wm.windows();
                wms.last().map_or(false, |top| top.id == editor_id)
            } else {
                false
            }
        });
        if editor_active && pressed {
            editor_handle_key(scancode);
            // Still push KeyDown to event queue for other handlers
            let key = scancode_to_resonance_keycode(scancode);
            let event = Event::Input(InputEvent::KeyDown(key));
            if let Some(ref mut queue) = *EVENT_QUEUE.lock() {
                queue.push(event);
            }
            continue;
        }

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
    if rt.editor_dirty {
        render_editor(rt);
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
    if rt.clock_changed {
        rt.desktop
            .push_dirty_rect(lattice::scene::DirtyRect::new(0, 0, fb_width, 24));
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

        if rt.shell_state == ShellState::Desktop {
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

/// Handle a key event for the editor.
pub fn editor_handle_key(scancode: u8) {
    let key = crate::scancode_to_resonance_keycode(scancode);
    let mut rt = RUNTIME.lock();
    let rt = match rt.as_mut() {
        Some(r) => r,
        None => return,
    };
    match key {
        KeyCode::Enter => {
            rt.editor_buf.insert_char(b'\n');
            rt.editor_dirty = true;
        }
        KeyCode::Backspace => {
            rt.editor_buf.backspace();
            rt.editor_dirty = true;
        }
        KeyCode::Left => {
            rt.editor_buf.cursor_left();
            rt.editor_dirty = true;
        }
        KeyCode::Right => {
            rt.editor_buf.cursor_right();
            rt.editor_dirty = true;
        }
        KeyCode::Up => {
            rt.editor_buf.cursor_up();
            rt.editor_dirty = true;
        }
        KeyCode::Down => {
            rt.editor_buf.cursor_down();
            rt.editor_dirty = true;
        }
        KeyCode::Home => {
            rt.editor_buf.cursor_home();
            rt.editor_dirty = true;
        }
        KeyCode::End => {
            rt.editor_buf.cursor_end();
            rt.editor_dirty = true;
        }
        KeyCode::PageUp => {
            // Calculate viewport from editor window dimensions, not terminal buffer.
            let viewport = if let Some(editor_window) = rt.editor_window {
                if let Some(window) = rt.desktop.wm.windows().iter().find(|w| w.id == editor_window) {
                    ((window.height / GLYPH_H).max(1) as usize)
                } else {
                    10 // fallback if window not found
                }
            } else {
                10 // fallback if no editor window
            };
            rt.editor_buf.page_up(viewport);
            rt.editor_dirty = true;
        }
        KeyCode::PageDown => {
            // Calculate viewport from editor window dimensions, not terminal buffer.
            let viewport = if let Some(editor_window) = rt.editor_window {
                if let Some(window) = rt.desktop.wm.windows().iter().find(|w| w.id == editor_window) {
                    ((window.height / GLYPH_H).max(1) as usize)
                } else {
                    10 // fallback if window not found
                }
            } else {
                10 // fallback if no editor window
            };
            rt.editor_buf.page_down(viewport);
            rt.editor_dirty = true;
        }
        KeyCode::Space => {
            rt.editor_buf.insert_char(b' ');
            rt.editor_dirty = true;
        }
        KeyCode::Tab => {
            rt.editor_buf.insert_char(b' ');
            rt.editor_buf.insert_char(b' ');
            rt.editor_dirty = true;
        }
        KeyCode::A => {
            rt.editor_buf.insert_char(b'a');
            rt.editor_dirty = true;
        }
        KeyCode::B => {
            rt.editor_buf.insert_char(b'b');
            rt.editor_dirty = true;
        }
        KeyCode::C => {
            rt.editor_buf.insert_char(b'c');
            rt.editor_dirty = true;
        }
        KeyCode::D => {
            rt.editor_buf.insert_char(b'd');
            rt.editor_dirty = true;
        }
        KeyCode::E => {
            rt.editor_buf.insert_char(b'e');
            rt.editor_dirty = true;
        }
        KeyCode::F => {
            rt.editor_buf.insert_char(b'f');
            rt.editor_dirty = true;
        }
        KeyCode::G => {
            rt.editor_buf.insert_char(b'g');
            rt.editor_dirty = true;
        }
        KeyCode::H => {
            rt.editor_buf.insert_char(b'h');
            rt.editor_dirty = true;
        }
        KeyCode::I => {
            rt.editor_buf.insert_char(b'i');
            rt.editor_dirty = true;
        }
        KeyCode::J => {
            rt.editor_buf.insert_char(b'j');
            rt.editor_dirty = true;
        }
        KeyCode::K => {
            rt.editor_buf.insert_char(b'k');
            rt.editor_dirty = true;
        }
        KeyCode::L => {
            rt.editor_buf.insert_char(b'l');
            rt.editor_dirty = true;
        }
        KeyCode::M => {
            rt.editor_buf.insert_char(b'm');
            rt.editor_dirty = true;
        }
        KeyCode::N => {
            rt.editor_buf.insert_char(b'n');
            rt.editor_dirty = true;
        }
        KeyCode::O => {
            rt.editor_buf.insert_char(b'o');
            rt.editor_dirty = true;
        }
        KeyCode::P => {
            rt.editor_buf.insert_char(b'p');
            rt.editor_dirty = true;
        }
        KeyCode::Q => {
            rt.editor_buf.insert_char(b'q');
            rt.editor_dirty = true;
        }
        KeyCode::R => {
            rt.editor_buf.insert_char(b'r');
            rt.editor_dirty = true;
        }
        KeyCode::S => {
            rt.editor_buf.insert_char(b's');
            rt.editor_dirty = true;
        }
        KeyCode::T => {
            rt.editor_buf.insert_char(b't');
            rt.editor_dirty = true;
        }
        KeyCode::U => {
            rt.editor_buf.insert_char(b'u');
            rt.editor_dirty = true;
        }
        KeyCode::V => {
            rt.editor_buf.insert_char(b'v');
            rt.editor_dirty = true;
        }
        KeyCode::W => {
            rt.editor_buf.insert_char(b'w');
            rt.editor_dirty = true;
        }
        KeyCode::X => {
            rt.editor_buf.insert_char(b'x');
            rt.editor_dirty = true;
        }
        KeyCode::Y => {
            rt.editor_buf.insert_char(b'y');
            rt.editor_dirty = true;
        }
        KeyCode::Z => {
            rt.editor_buf.insert_char(b'z');
            rt.editor_dirty = true;
        }
        KeyCode::Digit1 => {
            rt.editor_buf.insert_char(b'1');
            rt.editor_dirty = true;
        }
        KeyCode::Digit2 => {
            rt.editor_buf.insert_char(b'2');
            rt.editor_dirty = true;
        }
        KeyCode::Digit3 => {
            rt.editor_buf.insert_char(b'3');
            rt.editor_dirty = true;
        }
        KeyCode::Digit4 => {
            rt.editor_buf.insert_char(b'4');
            rt.editor_dirty = true;
        }
        KeyCode::Digit5 => {
            rt.editor_buf.insert_char(b'5');
            rt.editor_dirty = true;
        }
        KeyCode::Digit6 => {
            rt.editor_buf.insert_char(b'6');
            rt.editor_dirty = true;
        }
        KeyCode::Digit7 => {
            rt.editor_buf.insert_char(b'7');
            rt.editor_dirty = true;
        }
        KeyCode::Digit8 => {
            rt.editor_buf.insert_char(b'8');
            rt.editor_dirty = true;
        }
        KeyCode::Digit9 => {
            rt.editor_buf.insert_char(b'9');
            rt.editor_dirty = true;
        }
        KeyCode::Digit0 => {
            rt.editor_buf.insert_char(b'0');
            rt.editor_dirty = true;
        }
        _ => {}
    }
    if rt.editor_dirty {
        rt.frame_due = true;
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
