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
mod clock;
mod editor_bridge;
mod explorer;
mod handlers;
mod menu_actions;
mod network_manager;
mod render;
mod settings_bridge;
mod terminal;
mod viewers;

// ── Sub-module re-exports ─────────────────────────────────────
pub use clock::clock_string;
pub use render::{cursor_save_background, render, set_render_progress_fn};
pub use terminal::{LatticeTerminal, PIPE_STDIN, PIPE_STDOUT, render_terminal};

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use chronoline::{ChronoLine, Deadline, TimerId, TimerMode};
use lattice::desktop::{Desktop, DesktopAction};
use lattice::editor::EditorBuffer;
use lattice::shell_overlay::ShellState;
use lattice::terminal_surface::Cell as LatticeCell;
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
    pub mounted_drive_list: Option<fn() -> Vec<(alloc::string::String, alloc::string::String)>>,
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
            mounted_drive_list: None,
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

pub fn get_mounted_drives() -> Vec<(String, String)> {
    SOLVENT_CALLBACKS
        .lock()
        .mounted_drive_list
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

pub static GLOBAL_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// ── Service registry ─────────────────────────────────────────
pub trait Service: Send {
    fn tick(&mut self, now: u64);
}

pub(crate) static SERVICES: spin::Mutex<Vec<Box<dyn Service>>> = spin::Mutex::new(Vec::new());

pub fn register_service(service: Box<dyn Service>) {
    SERVICES.lock().push(service);
}

#[cfg(not(nitrogen_no_iwlwifi))]
pub use network_manager::register_wifi_service;

// ── WiFi action queue ────────────────────────────────────────
#[allow(dead_code)]
pub enum WifiAction {
    Connect(bonder::wifi::Ssid, Option<String>),
}

pub static WIFI_ACTION_QUEUE: spin::Mutex<Vec<WifiAction>> = spin::Mutex::new(Vec::new());

// ── Shared network state ─────────────────────────────────────
pub struct NetworkSnapshot {
    pub aps: Vec<lattice::network_menu::ApDisplay>,
    pub status: lattice::network_menu::NetStatus,
}

pub static NETWORK_SNAPSHOT: spin::Mutex<NetworkSnapshot> = spin::Mutex::new(NetworkSnapshot {
    aps: Vec::new(),
    status: lattice::network_menu::NetStatus::NoDevice,
});

// ── Back‑buffer ──────────────────────────────────────────────
pub(crate) static BACK_BUFFER: Mutex<Option<Vec<u32>>> = Mutex::new(None);

// ── Runtime state ────────────────────────────────────────────
pub(crate) static RUNTIME: Mutex<Option<RuntimeState>> = Mutex::new(None);
static EVENT_QUEUE: Mutex<Option<EventQueue>> = Mutex::new(None);
static DISPATCHER: Mutex<Option<Dispatcher>> = Mutex::new(None);
pub(crate) static PREV_MOUSE_BUTTONS: Mutex<u8> = Mutex::new(0);
pub(crate) static FB_DIMS: Mutex<(u32, u32, u32)> = Mutex::new((1024, 768, 1024));

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
    pub cursor_save_buf: [u32;
        lattice::cursor::Cursor::SIZE as usize * lattice::cursor::Cursor::SIZE as usize],
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
    pub cursor_only_update: bool,
}

pub fn init() {
    let desktop = Desktop::new(BG_COLOR);
    let term_buf = TerminalBuffer::new(DEFAULT_COLS, DEFAULT_ROWS);
    let mut dispatcher = Dispatcher::new();
    let mut chrono = ChronoLine::new();

    let _ = chrono.register_with_mode(
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

    let _ = chrono.register_with_mode(
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
        cursor_only_update: false,
    });
}

pub fn is_initialized() -> bool {
    RUNTIME.lock().is_some()
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
            if r.desktop.pwd_dialog_open {
                handle_password_dialog_key(r, scancode, pressed);
                continue;
            }

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
            if top_id.is_some() && r.explorer.as_ref().and_then(|e| e.window_id) == top_id {
                explorer_handle_key(r, scancode, pressed);
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

fn handle_password_dialog_key(rt: &mut RuntimeState, scancode: u8, pressed: bool) {
    let action = match scancode {
        0x1C => {
            if !pressed { return; }
            DesktopAction::SubmitPassword
        }
        0x01 => {
            if !pressed { return; }
            DesktopAction::DismissPasswordDialog
        }
        0x0E => {
            if !pressed { return; }
            DesktopAction::PasswordBackspace
        }
        0x2A | 0x36 => {
            rt.desktop.shift_held = pressed;
            return;
        }
        _ => {
            if !pressed { return; }
            let mut ch = scancode_to_ascii(scancode);
            if ch != 0 {
                if rt.desktop.shift_held {
                    let shifted = match ch {
                        b'1' => b'!', b'2' => b'@', b'3' => b'#', b'4' => b'$',
                        b'5' => b'%', b'6' => b'^', b'7' => b'&', b'8' => b'*',
                        b'9' => b'(', b'0' => b')', b'-' => b'_', b'=' => b'+',
                        b'[' => b'{', b']' => b'}', b'\\' => b'|', b';' => b':',
                        b'\'' => b'"', b'`' => b'~', b',' => b'<', b'.' => b'>',
                        b'/' => b'?',
                        _ if ch >= b'a' && ch <= b'z' => ch - b'a' + b'A',
                        _ => ch,
                    };
                    ch = shifted;
                }
                DesktopAction::PasswordChar(ch)
            } else {
                return;
            }
        }
    };
    let _ = network_manager::handle_network_action(rt, &action);
    rt.frame_due = true;
}

fn scancode_to_ascii(scancode: u8) -> u8 {
    match scancode {
        0x10 => b'q', 0x11 => b'w', 0x12 => b'e', 0x13 => b'r',
        0x14 => b't', 0x15 => b'y', 0x16 => b'u', 0x17 => b'i',
        0x18 => b'o', 0x19 => b'p', 0x1E => b'a', 0x1F => b's',
        0x20 => b'd', 0x21 => b'f', 0x22 => b'g', 0x23 => b'h',
        0x24 => b'j', 0x25 => b'k', 0x26 => b'l', 0x2C => b'z',
        0x2D => b'x', 0x2E => b'c', 0x2F => b'v', 0x30 => b'b',
        0x31 => b'n', 0x32 => b'm',
        0x02 => b'1', 0x03 => b'2', 0x04 => b'3', 0x05 => b'4',
        0x06 => b'5', 0x07 => b'6', 0x08 => b'7', 0x09 => b'8',
        0x0A => b'9', 0x0B => b'0',
        0x2B => b'\\', 0x0C => b'-', 0x0D => b'=', 0x1A => b'[',
        0x1B => b']', 0x27 => b';', 0x28 => b'\'', 0x29 => b'`',
        0x33 => b',', 0x34 => b'.', 0x35 => b'/',
        0x39 => b' ',
        _ => 0,
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

// ── Rendering runtime ────────────────────────────────────────

static YIELD_TICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static RENDER_FN: Mutex<Option<fn()>> = Mutex::new(None);

pub fn set_render_fn(f: fn()) {
    *RENDER_FN.lock() = Some(f);
}

pub fn tick_core(now: u64) {
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);

    poll_mouse_state();
    poll_keyboard();
    clock::update_clock();
    chrono_tick(now);

    // Callbacks may acquire runtime locks or register another service.
    let mut services = core::mem::take(&mut *SERVICES.lock());
    for service in &mut services {
        service.tick(now);
    }
    let mut registry = SERVICES.lock();
    services.append(&mut *registry);
    *registry = services;

    if now % 20 == 0 {
        let snap = NETWORK_SNAPSHOT.lock();
        let aps = snap.aps.clone();
        let status = snap.status.clone();
        drop(snap);
        if let Some(ref mut rt) = *RUNTIME.lock() {
            if rt.desktop.update_ap_list(aps, status) {
                rt.frame_due = true;
            }
        }
    }

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

pub fn runtime_tick_no_fb() {
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
    // Release RENDERING_SUSPENDED before calling render_fn, otherwise
    // render() will see it as already-suspended and early-return,
    // causing the display to hang permanently (e.g. during shell input).
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
    if do_render {
        if let Some(render_fn) = *RENDER_FN.lock() {
            render_fn();
        }
    }
}

pub fn consume_frame_due() -> bool {
    RUNTIME.lock().as_mut().map_or(false, |r| {
        let due = r.frame_due;
        r.frame_due = false;
        due
    })
}

pub fn runtime_tick(now: u64, fb: &mut petroleum::graphics::FramebufferGuard) {
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
    // Release RENDERING_SUSPENDED before calling render(), otherwise
    // render() will see it as already-suspended and early-return.
    RENDERING_SUSPENDED.store(false, core::sync::atomic::Ordering::SeqCst);
    if do_render {
        render(fb);
    }
}

pub fn write_terminal(s: &str) {
    if let Some(ref mut r) = *RUNTIME.lock() {
        r.term_buf.put_str(s);
        r.term_dirty = true;
    }
}

// ── Rendering suspend / resume ───────────────────────────────

pub(crate) static RENDERING_SUSPENDED: core::sync::atomic::AtomicBool =
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
pub use lattice::theme::{ThemeVariant, ThemeStyle, current_theme_variant, set_theme, toggle_theme, current_style, set_style, toggle_style};
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

// ── Editor / Settings bridge ─────────────────────────────────
pub use editor_bridge::editor_handle_key;
pub use settings_bridge::settings_handle_key;

pub fn ensure_editor_window() -> Option<WindowId> {
    RUNTIME
        .lock()
        .as_mut()
        .and_then(editor_bridge::ensure_editor_window)
}

// ── Explorer ─────────────────────────────────────────────────

pub(crate) fn render_explorer(rt: &mut RuntimeState) {
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
        "txt" | "md" | "log" | "toml" | "rs" | "c" | "h" | "py" | "js"
            | "json" | "xml" | "yml" | "yaml" | "ini" | "cfg" | "sh" | "bat"
            | "env" | "gitignore" | "lock"
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

    let app_name = app.unwrap_or("Unknown");
    let msg = alloc::format!(
        "File: {}\nType: .{}\nApp: {}\n\nOpening {} is not yet implemented.",
        name, ext, app_name, app_name
    );
    let cols = 50;
    let rows = (msg.lines().count() as u32) + 3;
    let id = rt.desktop.wm.create_titled_window(
        200, 160, cols * GLYPH_W, rows * GLYPH_H, 0x1a1a0d, "Open File",
    );
    if let Some(w) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = crate::menu_actions::render_text_into_surface(
            &mut w.surface, &msg, cols, 0xFFFFCC, 0x1a1a0d,
        );
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

// ── Explorer keyboard handling ──────────────────────────────
fn explorer_handle_key(rt: &mut RuntimeState, scancode: u8, pressed: bool) {
    if !pressed { return; }
    let key = scancode_to_resonance_keycode(scancode);
    let visible_rows = 20usize;
    // Pre-compute enter action to avoid borrow conflicts.
    let mut enter_action: Option<(String, bool)> = None;
    match key {
        resonance::KeyCode::Up => {
            let explorer = match rt.explorer.as_mut() { Some(e) => e, None => return };
            let n = explorer.entries.len();
            if n == 0 { return; }
            let idx = explorer.selected_index.unwrap_or(n.saturating_sub(1));
            explorer.selected_index = if idx == 0 { Some(n.saturating_sub(1)) } else { Some(idx - 1) };
            if let Some(s) = explorer.selected_index { if s < explorer.scroll_offset { explorer.scroll_offset = s; } }
            rt.explorer_dirty = true;
            rt.frame_due = true;
        }
        resonance::KeyCode::Down => {
            let explorer = match rt.explorer.as_mut() { Some(e) => e, None => return };
            let n = explorer.entries.len();
            if n == 0 { return; }
            let idx = explorer.selected_index.unwrap_or(0);
            explorer.selected_index = if idx + 1 >= n { Some(0) } else { Some(idx + 1) };
            if let Some(s) = explorer.selected_index {
                if s >= explorer.scroll_offset + visible_rows { explorer.scroll_offset = s.saturating_sub(visible_rows - 1); }
            }
            rt.explorer_dirty = true;
            rt.frame_due = true;
        }
        resonance::KeyCode::Enter => {
            let explorer = match rt.explorer.as_mut() { Some(e) => e, None => return };
            if let Some(idx) = explorer.selected_index {
                let name = match explorer.raw_names.get(idx) {
                    Some(n) => n,
                    None => return,
                };
                let is_dir = explorer.raw_is_dir.get(idx).copied().unwrap_or(false);
                let path = if explorer.current_dir.ends_with('/') {
                    alloc::format!("{}{}", explorer.current_dir, name)
                } else {
                    alloc::format!("{}/{}", explorer.current_dir, name)
                };
                if is_dir {
                    explorer.navigate_to(&path);
                    rt.explorer_dirty = true;
                    rt.frame_due = true;
                } else {
                    enter_action = Some((path, is_dir));
                }
            }
        }
        _ => {}
    }
    if let Some((path, false)) = enter_action {
        launch_file(rt, &path);
    }
}

// ── Shell bootstrap ──────────────────────────────────────────
pub fn run_shell_on(terminal: &mut dyn carrier::terminal::Terminal, prompt: &str) {
    let mut shell = nozzle::Shell::new(terminal, nozzle::default_commands());
    shell.set_prompt(prompt);
    shell.run();
}

pub(crate) static SUPER_HELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
