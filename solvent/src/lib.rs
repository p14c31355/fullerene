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

// ── Modules ──────────────────────────────────────────────────
mod editor_bridge;
mod explorer;
mod handlers;
mod menu_actions;
mod settings_bridge;
mod viewers;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use chronoline::{ChronoLine, Deadline, TimerId, TimerMode};
use lattice::compositor::{Compositor, RenderTarget};
use lattice::desktop::Desktop;
use lattice::editor::EditorBuffer;
use lattice::shell_overlay::{ShellState, render_app_grid, render_task_overview};
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use resonance::{Dispatcher, Event, EventQueue, InputEvent, MouseButton};
use spin::Mutex;

// ── Aggregated kernel callbacks ──────────────────────────────

pub struct SolventCallbacks {
    pub shell_cmd: Option<fn(&str) -> String>,
    pub launch_shell: Option<fn()>,
    pub heap_extend: Option<fn(usize) -> Result<(), ()>>,
    pub wall_clock: Option<fn() -> Option<(u16, u8, u8, u8, u8, u8)>>,
    pub vfs_readdir: Option<fn(&str) -> Result<Vec<VfsEntry>, &'static str>>,
    pub vfs_read: Option<fn(&str) -> Result<Vec<u8>, &'static str>>,
    pub vfs_write: Option<fn(&str, &[u8]) -> Result<(), &'static str>>,
    pub vfs_create: Option<fn(&str) -> Result<(), &'static str>>,
    pub vfs_mkdir: Option<fn(&str) -> Result<(), &'static str>>,
    pub vfs_unlink: Option<fn(&str) -> Result<(), &'static str>>,
    pub process_list: Option<fn() -> Vec<ProcessEntry>>,
    pub device_list: Option<fn() -> Vec<DeviceEntry>>,
    pub usb_drive_list: Option<fn() -> Vec<(alloc::string::String, alloc::string::String)>>,
    pub usb_poll: Option<fn() -> bool>,
    pub settings_save: Option<fn()>,
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
            settings_save: None,
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
pub static MOUSE_SENSITIVITY: core::sync::atomic::AtomicI16 = core::sync::atomic::AtomicI16::new(6);

pub fn apply_settings(sensitivity: f32, brightness_x100: u32, top_panel_enabled: bool) {
    MOUSE_SENSITIVITY.store(
        (sensitivity * 6.0) as i16,
        core::sync::atomic::Ordering::Relaxed,
    );
    DISPLAY_BRIGHTNESS_X100.store(brightness_x100, core::sync::atomic::Ordering::Relaxed);
    lattice::top_panel::set_top_panel_enabled(top_panel_enabled);
    force_desktop_redraw();
}

pub static DISPLAY_BRIGHTNESS_X100: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(100);
const FRAME_INTERVAL_TICKS: u64 = 8;
const FRAME_INTERVAL_MS: u64 = 17;
const FRAME_TIMER_ID: TimerId = TimerId(2);
pub(crate) static TSC_PER_MS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(3_000_000);
const MAX_FB_PIXELS: usize = 3840 * 2160;

pub fn get_usb_drives() -> Vec<(String, String)> {
    SOLVENT_CALLBACKS
        .lock()
        .usb_drive_list
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

// ── Static back‑buffer ───────────────────────────────────────
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
    pub editor_window: Option<WindowId>,
    pub editor_buf: EditorBuffer,
    pub editor_launch_pending: bool,
    pub editor_dirty: bool,
    pub editor_file_path: Option<String>,
    pub explorer: Option<explorer::ExplorerContext>,
    pub explorer_dirty: bool,
    pub settings_window: Option<WindowId>,
    pub settings_dirty: bool,
    pub usb_poll_pending: bool,
}

pub fn init() {
    let desktop = Desktop::new(BG_COLOR);
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
        usb_poll_pending: false,
    });
}

pub fn is_initialized() -> bool {
    RUNTIME.lock().is_some()
}

// ── Cursor helpers ────────────────────────────────────────────

fn cursor_save_background(
    cursor: &lattice::cursor::Cursor,
    buf: &mut [u32; lattice::cursor::Cursor::SIZE as usize
             * lattice::cursor::Cursor::SIZE as usize],
    save_x: &mut i32,
    save_y: &mut i32,
    save_valid: &mut bool,
    fb: &[u32],
    fb_stride: u32,
    fb_width: u32,
    fb_height: u32,
) {
    if !cursor.visible {
        return;
    }
    let cur_sz = lattice::cursor::Cursor::SIZE as i32;
    let cx = cursor.x - lattice::cursor::Cursor::HOTSPOT_X;
    let cy = cursor.y - lattice::cursor::Cursor::HOTSPOT_Y;
    let stride_i = fb_stride as i32;
    let fbw_i = fb_width as i32;
    let fbh_i = fb_height as i32;
    let fb_len = (fb_stride as usize).saturating_mul(fb_height as usize);
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
            buf[(row * cur_sz + col) as usize] = val;
        }
    }
    *save_x = cx;
    *save_y = cy;
    *save_valid = true;
}

pub(crate) fn cursor_lightweight_update(rt: &mut RuntimeState) {
    let (fb_addr, fbw, fbh, fb_stride) = *LAST_FB.lock();
    if fb_addr == 0 || fbh == 0 {
        rt.frame_due = true;
        return;
    }
    if !rt.desktop.cursor.visible {
        // Cursor turned off: restore saved background and clear state
        if rt.cursor_save_valid {
            let fb_ptr = fb_addr as *mut u32;
            let cur_sz = lattice::cursor::Cursor::SIZE as i32;
            let fbw_i = fbw as i32;
            let stride_i = fb_stride as i32;
            let fbh_i = fbh as i32;
            let fb_len = (fb_stride as usize).saturating_mul(fbh as usize);
            unsafe {
                let fb = core::slice::from_raw_parts_mut(fb_ptr, fb_len);
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
            rt.cursor_save_valid = false;
        }
        return;
    }
    let fb_ptr = fb_addr as *mut u32;

    let cur_sz = lattice::cursor::Cursor::SIZE as i32;
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
        cursor_save_background(
            &rt.desktop.cursor,
            &mut rt.cursor_save_buf,
            &mut rt.cursor_save_x,
            &mut rt.cursor_save_y,
            &mut rt.cursor_save_valid,
            fb,
            fb_stride,
            fbw,
            fbh,
        );
        Compositor::draw_cursor_direct(fb, fb_stride, fbh, &rt.desktop.cursor);
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
    let ps2_state = nitrogen::ps2::mouse::consume_state();
    let dx = ps2_state.get_x();
    let dy = ps2_state.get_y();
    let btn = nitrogen::ps2::mouse::mouse_buttons();
    let mut mouse = MOUSE_STATE.lock();
    let old_x = mouse.x;
    let old_y = mouse.y;
    let sens = MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed);
    mouse.x = mouse.x.wrapping_add(dx.wrapping_mul(sens));
    mouse.y = mouse.y.wrapping_add(dy.wrapping_mul(sens).wrapping_neg());
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

// ── Keyboard polling ─────────────────────────────────────────

pub fn poll_keyboard() {
    while nitrogen::ps2::keyboard::raw_key_available() {
        let (scancode, pressed) = match nitrogen::ps2::keyboard::pop_raw_key() {
            Some(k) => k,
            None => break,
        };
        let mut rt = RUNTIME.lock();
        if let Some(ref mut r) = *rt {
            let top_id = r.desktop.wm.windows().last().map(|w| w.id);
            if top_id.is_some() && r.editor_window == top_id {
                drop(rt);
                editor_bridge::editor_handle_key(scancode, pressed);
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
            if top_id.is_some() && r.settings_window == top_id {
                settings_bridge::settings_handle_key_inner(r, scancode, pressed);
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

fn scancode_to_resonance_keycode(scancode: u8) -> resonance::KeyCode {
    resonance::scancode::from_scancode(scancode)
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

const DAYS_IN_MONTH: [i16; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
fn days_in_month(month: i16, year: i16) -> i16 {
    if month == 2 && ((year % 4 == 0 && year % 100 != 0) || year % 400 == 0) {
        29
    } else if (1..=12).contains(&month) {
        DAYS_IN_MONTH[month as usize]
    } else {
        31
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
        if r.desktop.clock_text != time_str {
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

    // Detect editor surface-size mismatch (e.g. after maximize)
    if !rt.editor_dirty {
        if let Some(editor_id) = rt.editor_window {
            if let Some(w) = rt.desktop.wm.windows().iter().find(|w| w.id == editor_id) {
                if w.surface.width() != (w.width / GLYPH_W).max(1) * GLYPH_W
                    || w.surface.height() != (w.height / GLYPH_H).max(1) * GLYPH_H
                {
                    rt.editor_dirty = true;
                }
            }
        }
    }

    if rt.editor_dirty {
        editor_bridge::render_editor(rt);
    }
    if rt.explorer_dirty {
        render_explorer(rt);
    }
    if rt.settings_dirty {
        settings_bridge::render_settings(rt);
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
        if lattice::top_panel::is_top_panel_enabled() {
            rt.desktop.push_dirty_rect(lattice::scene::DirtyRect::new(
                0,
                0,
                fb_width,
                lattice::top_panel::TOP_PANEL_HEIGHT,
            ));
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
            let mut prev = PREV_TRANSITION.lock();
            core::mem::replace(&mut *prev, false)
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
            let brightness = DISPLAY_BRIGHTNESS_X100.load(core::sync::atomic::Ordering::Relaxed);
            if brightness < 100 && bw > 0 && bh > 0 {
                let back_w = fb_width as usize;
                let rows: core::ops::Range<usize> = if was_transition {
                    0..fb_height as usize
                } else {
                    (by as usize)..((by + bh) as usize)
                };
                let cols: core::ops::Range<usize> = if was_transition {
                    0..fb_width as usize
                } else {
                    (bx as usize)..((bx + bw) as usize)
                };
                for row in rows {
                    for col in cols.clone() {
                        let idx = row * back_w + col;
                        if idx < back_len {
                            back[idx] =
                                lattice::compositor::apply_brightness(back[idx], brightness);
                        }
                    }
                }
            }
            if was_transition || (bw > 0 && bh > 0) {
                let back_w = fb_width as usize;
                if was_transition {
                    for row in 0..fb_height as usize {
                        let src_off = row * back_w;
                        let dst_off = row * fb_stride;
                        let copy_len = back_w.min(back_len.saturating_sub(src_off));
                        if copy_len > 0 {
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    back.as_ptr().add(src_off),
                                    fb_pixels.as_mut_ptr().add(dst_off),
                                    copy_len,
                                );
                            }
                        }
                    }
                } else {
                    for row in 0..bh {
                        let src_off = ((by + row) as usize) * back_w + (bx as usize);
                        let dst_off = ((by + row) as usize) * fb_stride + (bx as usize);
                        let len = (bw as usize)
                            .min(back_len.saturating_sub(src_off))
                            .min(fb_len.saturating_sub(dst_off));
                        if len > 0 {
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    back.as_ptr().add(src_off),
                                    fb_pixels.as_mut_ptr().add(dst_off),
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

        if rt.shell_state == ShellState::Desktop && lattice::top_panel::is_top_panel_enabled() {
            rt.desktop
                .top_panel
                .render(fb_pixels, fb_width, fb_height, fb_stride_pixels);
        }

        if rt.desktop.cursor.visible {
            let (fb_addr, _, _, fb_stride) = *LAST_FB.lock();
            if fb_addr != 0 {
                let fb_ptr = fb_addr as *mut u32;
                let fb_len = (fb_stride as usize).saturating_mul(fb_height as usize);
                unsafe {
                    let fb = core::slice::from_raw_parts(fb_ptr, fb_len);
                    cursor_save_background(
                        &rt.desktop.cursor,
                        &mut rt.cursor_save_buf,
                        &mut rt.cursor_save_x,
                        &mut rt.cursor_save_y,
                        &mut rt.cursor_save_valid,
                        fb,
                        fb_stride,
                        fb_width,
                        fb_height,
                    );
                }
            }
            Compositor::draw_cursor_direct(
                fb_pixels,
                fb_stride_pixels,
                fb_height,
                &rt.desktop.cursor,
            );
        }
    }

    if rt.usb_poll_pending {
        rt.usb_poll_pending = false;
        drop(rt_lock);
        let poll_fn = {
            let cb_guard = SOLVENT_CALLBACKS.lock();
            cb_guard.usb_poll
        };
        if let Some(f) = poll_fn {
            let _ = f();
        }
        if let Some(ref mut rt) = *RUNTIME.lock() {
            if let Some(ref mut explorer) = rt.explorer {
                explorer.refresh_sidebar();
                rt.explorer_dirty = true;
                rt.frame_due = true;
            }
        }
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
        let needed = ((new_cols * new_rows * GLYPH_W * GLYPH_H) as usize * 4)
            .saturating_add((new_cols * new_rows) as usize * 12);
        let reserve = HEAP_EXTEND_RESERVE.load(core::sync::atomic::Ordering::Relaxed);
        if needed > reserve {
            let additional = needed.saturating_sub(reserve).next_multiple_of(4096);
            match SOLVENT_CALLBACKS.lock().heap_extend {
                Some(f) if f(additional).is_ok() => {
                    HEAP_EXTEND_RESERVE
                        .fetch_add(additional, core::sync::atomic::Ordering::Relaxed);
                }
                _ => return,
            }
        }
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
                        if let Some(dst) = rt.term_buf.cell_mut(col as u32, row as u32) {
                            *dst = nozzle::terminal_buffer::Cell {
                                ch: src_cells[src_idx].ch,
                                fg: src_cells[src_idx].fg,
                                bg: src_cells[src_idx].bg,
                            };
                        }
                    }
                }
            }
        }
        rt.term_buf.set_cursor(
            old_cur_col.min(new_cols.saturating_sub(1)),
            old_cur_row.min(new_rows.saturating_sub(1)),
        );
        drop(old_buf);
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

    let total = (rt.term_buf.cols() * rt.term_buf.rows()) as usize;
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
    let visible = rt.term_buf.visible_cells();
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

// ── LatticeTerminal ──────────────────────────────────────────

pub struct LatticeTerminal;

impl carrier::terminal::Terminal for LatticeTerminal {
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

fn tick_core(now: u64) {
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);
    poll_mouse_state();
    poll_keyboard();
    update_clock();
    chrono_tick(now);
    process_events();
    if RUNTIME.lock().as_mut().map_or(false, |r| {
        let p = r.shell_launch_pending;
        r.shell_launch_pending = false;
        p
    }) {
        ensure_terminal_window();
        launch_shell();
    }
    if RUNTIME.lock().as_mut().map_or(false, |r| {
        let p = r.editor_launch_pending;
        r.editor_launch_pending = false;
        p
    }) {
        ensure_editor_window();
    }
}

fn runtime_tick_no_fb() {
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    tick_core(now);
    let do_render = RUNTIME.lock().as_mut().map_or(false, |r| {
        let due = r.frame_due;
        if due {
            let frame_tsc = TSC_PER_MS
                .load(core::sync::atomic::Ordering::Relaxed)
                .saturating_mul(FRAME_INTERVAL_MS);
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
    if RENDERING_SUSPENDED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    tick_core(now);

    static LAST_USB_POLL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
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

// ── Window API ───────────────────────────────────────────────
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
        let (ww, wh) = (w.surface.width(), w.surface.height());
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

pub fn ensure_terminal_window() -> Option<WindowId> {
    let mut rt = RUNTIME.lock();
    let rt = rt.as_mut()?;
    if let Some(id) = rt.term_window {
        if rt.desktop.wm.windows().iter().any(|w| w.id == id) {
            return Some(id);
        }
    }
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

// ── Editor / Settings bridge (re-exports from submodules) ────
pub use editor_bridge::editor_handle_key;
pub use settings_bridge::settings_handle_key;

/// Ensure an editor window exists.
pub fn ensure_editor_window() -> Option<WindowId> {
    RUNTIME
        .lock()
        .as_mut()
        .and_then(editor_bridge::ensure_editor_window)
}

// ── Explorer ─────────────────────────────────────────────────

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
            rt.explorer_dirty = false;
            return;
        }
    };
    explorer::render_explorer(explorer, &mut window.surface);
    rt.desktop.invalidate_window(explorer_id);
    rt.explorer_dirty = false;
}

pub fn launch_file(rt: &mut RuntimeState, path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = explorer::extension_of(name);
    let app = explorer::lookup_association(ext);
    let ext_lower = ext.to_lowercase();
    let is_text = matches!(
        ext_lower.as_str(),
        "txt"
            | "md"
            | "log"
            | "toml"
            | "rs"
            | "c"
            | "h"
            | "py"
            | "js"
            | "json"
            | "xml"
            | "yml"
            | "yaml"
            | "ini"
            | "cfg"
            | "sh"
            | "bat"
            | "env"
            | "gitignore"
            | "lock"
    );

    if is_text {
        let file_content = match SOLVENT_CALLBACKS.lock().vfs_read {
            Some(f) => match f(path) {
                Ok(data) => match core::str::from_utf8(&data) {
                    Ok(s) => String::from(s),
                    Err(_) => return,
                },
                Err(_) => return,
            },
            None => return,
        };
        let id = rt.desktop.wm.create_titled_window(
            100,
            80,
            DEFAULT_COLS * GLYPH_W,
            DEFAULT_ROWS * GLYPH_H,
            0x0a0a1e,
            "Text Editor",
        );
        if let Some(old_id) = rt.editor_window {
            if rt.desktop.wm.windows().iter().any(|w| w.id == old_id) {
                rt.desktop.wm.close_window(old_id);
            }
        }
        rt.editor_window = Some(id);
        rt.editor_buf = lattice::editor::EditorBuffer::from_text(&file_content);
        rt.editor_file_path = Some(String::from(path));
        rt.editor_dirty = true;
        rt.desktop.force_full_redraw();
        rt.frame_due = true;
        rt.explorer_dirty = true;
        return;
    }

    match ext_lower.as_str() {
        "bmp" => {
            crate::viewers::open_bmp(rt, path, name);
            return;
        }
        #[cfg(feature = "minipng")]
        "png" => {
            crate::viewers::open_png(rt, path, name);
            return;
        }
        "wav" => {
            crate::viewers::open_wav(rt, path, name);
            return;
        }
        #[cfg(feature = "rmp3")]
        "mp3" => {
            crate::viewers::open_mp3(rt, path, name);
            return;
        }
        #[cfg(feature = "shiguredo_mp4")]
        "mp4" => {
            crate::viewers::open_mp4(rt, path, name);
            return;
        }
        "tar" | "gz" | "xz" => {
            crate::viewers::open_tar(rt, path, name);
            return;
        }
        _ => {}
    }

    let app_name = app.unwrap_or("Unknown");
    let msg = alloc::format!(
        "File: {}\nType: .{}\nApp: {}\n\nOpening {} is not yet implemented.",
        name,
        ext,
        app_name,
        app_name
    );
    let cols = 50;
    let rows = (msg.lines().count() as u32) + 3;
    let id = rt.desktop.wm.create_titled_window(
        200,
        160,
        cols * GLYPH_W,
        rows * GLYPH_H,
        0x1a1a0d,
        "Open File",
    );
    if let Some(w) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = crate::menu_actions::render_text_into_surface(
            &mut w.surface,
            &msg,
            cols,
            0xFFFFCC,
            0x1a1a0d,
        );
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

// ── Shell bootstrap ──────────────────────────────────────────
pub fn run_shell_on(terminal: &mut dyn carrier::terminal::Terminal, prompt: &str) {
    let mut shell = nozzle::Shell::new(terminal, nozzle::default_commands());
    shell.set_prompt(prompt);
    shell.run();
}

pub(crate) static SUPER_HELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
