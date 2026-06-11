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
use core::arch::x86_64;
use core::fmt::Write;
use core::sync::atomic::AtomicPtr;
use lattice::compositor::{Compositor, RenderTarget};
use lattice::desktop::Desktop;
use lattice::shell_overlay::{ShellState, render_app_grid, render_task_overview};
use lattice::terminal_surface::{self, Cell as LatticeCell};
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use resonance::{Dispatcher, Event, EventHandler, EventQueue, InputEvent, KeyCode, MouseButton};
use spin::Mutex;

/// Macro to define a callback pair: static + setter + caller helper.
macro_rules! define_callback {
    ($vis:vis $static_name:ident, $setter_name:ident, $arg_ty:ty, $ret_ty:ty) => {
        $vis static $static_name: Mutex<Option<$arg_ty>> = Mutex::new(None);
        $vis fn $setter_name(f: $arg_ty) { *$static_name.lock() = Some(f); }
        $vis fn $static_name () -> Option<$arg_ty> { *$static_name.lock() }
    };
    ($vis:vis $static_name:ident, $setter_name:ident, $arg_ty:ty, $ret_ty:ty, $getter_name:ident) => {
        define_callback!($vis $static_name, $setter_name, $arg_ty, $ret_ty);
        $vis fn $getter_name() -> Option<$ret_ty> { $static_name().map(|f| f()) }
    };
}

// Callback: shell command execution
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

// ── Utility Functions ────────────────────────────────────────

/// Truncate a string to at most `n` characters (not bytes), safely handling UTF-8 boundaries.
fn truncate_to_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
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
/// Interval (ticks) between forced compositor passes on Desktop.
///
/// The tick‑based timer is a coarse fallback; actual frame pacing is
/// enforced via TSC in `chrono_tick` so that real‑time FPS stays
/// consistent across QEMU (emulated) and real hardware (native).
const FRAME_INTERVAL_TICKS: u64 = 8;
/// Target frame interval: 16.7 ms (~60 FPS).
const FRAME_INTERVAL_MS: u64 = 17;
/// Conservative TSC‑per‑ms floor (2.5 GHz).  Set by the kernel during
/// early boot via `set_tsc_per_ms()`.  This guarantees ≤60 FPS on any
/// x86‑64 CPU from the last decade.
static TSC_PER_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(3_000_000);
const FRAME_TIMER_ID: TimerId = TimerId(2);

/// Maximum framebuffer size covering 4K (3840×2160). BSS static buffer;
/// displays exceeding this will skip rendering to avoid overflowing.
const MAX_FB_PIXELS: usize = 3840 * 2160;

/// Set the TSC‑per‑millisecond value from the kernel.
/// Called during early boot so the compositor can pace frames in real
/// time regardless of whether the yield‑loop runs at 500 Hz (QEMU TCG)
/// or 50 kHz (bare metal).
pub fn set_tsc_per_ms(val: u64) {
    TSC_PER_MS.store(val, core::sync::atomic::Ordering::Relaxed);
}

/// Last render TSC timestamp (for real‑time frame pacing).
static LAST_RENDER_TSC: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Callback to extend the kernel heap.
pub static HEAP_EXTEND_FN: Mutex<Option<fn(additional: usize) -> Result<(), ()>>> =
    Mutex::new(None);
/// Total bytes that have been successfully allocated via `HEAP_EXTEND_FN`.
pub static HEAP_EXTEND_RESERVE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
/// Register the kernel heap extension callback.
pub fn set_heap_extend_fn(f: fn(usize) -> Result<(), ()>) {
    *HEAP_EXTEND_FN.lock() = Some(f);
}

/// Callback to get wall‑clock time from UEFI (or RTC fallback).
pub static WALL_CLOCK_FN: Mutex<Option<fn() -> Option<(u16, u8, u8, u8, u8, u8)>>> =
    Mutex::new(None);
/// Register the wall‑clock callback.
pub fn set_wall_clock_fn(f: fn() -> Option<(u16, u8, u8, u8, u8, u8)>) {
    *WALL_CLOCK_FN.lock() = Some(f);
}

// ── VFS / Process / Device callbacks ──────────

/// A single VFS directory entry returned by the kernel.
#[derive(Debug, Clone)]
pub struct VfsEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
}

/// A single process entry returned by the kernel.
#[derive(Debug, Clone)]
pub struct ProcessEntry {
    pub pid: u64,
    pub name: String,
    pub state: ProcessStateKind,
}

/// Kernel-side process state (mirrors fullerene_kernel::process::ProcessState).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStateKind {
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// A single device entry returned by the kernel.
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub name: String,
    pub dev_type: String,
    pub enabled: bool,
}

/// Callback to list the contents of a VFS directory.
pub static VFS_READDIR_FN: Mutex<Option<fn(path: &str) -> Result<Vec<VfsEntry>, &'static str>>> =
    Mutex::new(None);
pub fn set_vfs_readdir_fn(f: fn(path: &str) -> Result<Vec<VfsEntry>, &'static str>) {
    *VFS_READDIR_FN.lock() = Some(f);
}

/// Callback to get the list of all processes.
pub static PROCESS_LIST_FN: Mutex<Option<fn() -> Vec<ProcessEntry>>> = Mutex::new(None);
pub fn set_process_list_fn(f: fn() -> Vec<ProcessEntry>) {
    *PROCESS_LIST_FN.lock() = Some(f);
}

/// Callback to get the list of all devices.
pub static DEVICE_LIST_FN: Mutex<Option<fn() -> Vec<DeviceEntry>>> = Mutex::new(None);
pub fn set_device_list_fn(f: fn() -> Vec<DeviceEntry>) {
    *DEVICE_LIST_FN.lock() = Some(f);
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

/// Cached framebuffer pointer (as usize) and dimensions for lightweight cursor updates.
static LAST_FB: Mutex<(usize, u32, u32)> = Mutex::new((0, 0, 0));

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

    /// Lightweight cursor save buffer (16×16 pixels) for overlay mode
    /// cursor movement without full re‑renders.
    pub cursor_save_buf: [u32; lattice::cursor::Cursor::SIZE * lattice::cursor::Cursor::SIZE],
    pub cursor_save_x: i32,
    pub cursor_save_y: i32,
    pub cursor_save_valid: bool,
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
        cursor_save_buf: [0u32; 256],
        cursor_save_x: 0,
        cursor_save_y: 0,
        cursor_save_valid: false,
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
                // In overlay mode, move the cursor and trigger a render
                // so the cursor is redrawn at its new position.
                // `mouse_move` also tracks cursor_moved for dirty-rect
                // optimisation; wm.on_mouse_move is a no‑op because drag
                // state is always None in overlay mode.
                Event::Input(InputEvent::MouseMove { x, y }) => {
                    rt.desktop.mouse_move(*x, *y);
                    cursor_lightweight_update(rt);
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
                rt.desktop.mouse_move(*x, *y);
                cursor_lightweight_update(rt);
                // Only schedule a full render when a drag is in
                // progress (the WM generates dirty rects that must
                // be composited).  Pure cursor movement is handled
                // by the lightweight update above.
                if !matches!(
                    rt.desktop.wm.drag_state(),
                    lattice::wm::DragState::None
                ) || rt.desktop.has_pending_dirty_rects() {
                    rt.frame_due = true;
                }
                true
            }
            Event::Input(InputEvent::MouseDown(btn)) => {
                let cx = rt.desktop.cursor.x;
                let cy = rt.desktop.cursor.y;

                // ── Desktop right-click → context menu ───
                if *btn == MouseButton::Right {
                    // Only show context menu if no window is under the cursor
                    // (i.e. click is on empty desktop)
                    let hit_window = rt.desktop.wm.window_at(cx, cy);
                    if hit_window.is_none() {
                        rt.desktop.show_context_menu(cx, cy);
                        rt.frame_due = true;
                        return true;
                    }
                }

                // ── Top-panel Activities button click ───
                if rt.desktop.top_panel.hit_activities_button(cx, cy) {
                    rt.shell_state = ShellState::TaskOverview;
                    rt.frame_due = true;
                    return true;
                }

                rt.desktop.set_cursor(cx, cy);
                let (fw, fh) = *FB_DIMS.lock();
                rt.desktop.mouse_down(fw, fh);
                rt.frame_due = true;

                // ── Dispatch menu action if one was clicked ─────
                if let Some(action) = rt.desktop.menu_action_pending.take() {
                    dispatch_menu_action(rt, &action);
                }

                rt.term_dirty = true;
                true
            }
            Event::Input(InputEvent::MouseUp(_btn)) => {
                rt.desktop.mouse_up();
                rt.frame_due = true;
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

/// Track whether a Super key is currently held (for shortcuts).
static SUPER_HELD: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Shell event handler — manages Super key double‑tap, Super+T tiling toggle, and Esc transitions.
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
                SUPER_HELD.store(true, core::sync::atomic::Ordering::Relaxed);
                // Double‑tap Super to cycle through overlays
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
            Event::Input(InputEvent::KeyUp(KeyCode::SuperLeft))
            | Event::Input(InputEvent::KeyUp(KeyCode::SuperRight)) => {
                SUPER_HELD.store(false, core::sync::atomic::Ordering::Relaxed);
                return false;
            }
            Event::Input(InputEvent::KeyDown(KeyCode::T))
                if SUPER_HELD.load(core::sync::atomic::Ordering::Relaxed)
                    && rt.shell_state == ShellState::Desktop =>
            {
                // Super+T: toggle tiling mode
                let (fw, fh) = *FB_DIMS.lock();
                let (ww, wh) = rt.desktop.work_area(fw, fh);
                rt.desktop.wm.toggle_tiling();
                rt.desktop.wm.retile(ww, wh);
                rt.desktop.force_full_redraw();
                rt.frame_due = true;
                return true;
            }
            Event::Input(InputEvent::KeyDown(KeyCode::Escape)) => {
                if rt.shell_state != ShellState::Desktop {
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                // Also clear Super held on Escape (stuck modifier guard)
                SUPER_HELD.store(false, core::sync::atomic::Ordering::Relaxed);
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

// ── Lightweight cursor update ───────────────────────────────

/// Update only the cursor on the cached framebuffer without
/// re‑rendering the entire scene.  Uses a small save buffer
/// to restore the old cursor position and draw the new one.
///
/// Used in both Desktop and shell overlay modes to avoid full
/// compositor passes on every mouse-move tick.
fn cursor_lightweight_update(rt: &mut RuntimeState) {
    let (fb_addr, fbw, fbh) = *LAST_FB.lock();
    if fb_addr == 0 || fbw == 0 || fbh == 0 {
        // Fallback: request a full render pass.
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
    let fbh_i = fbh as i32;
    let fb_len = (fbw as usize).saturating_mul(fbh as usize);

    unsafe {
        let fb = core::slice::from_raw_parts_mut(fb_ptr, fb_len);

        // Restore the old cursor area.
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
                    let idx = (dy * fbw_i + dx) as usize;
                    if idx < fb_len {
                        fb[idx] = rt.cursor_save_buf[(row * cur_sz + col) as usize];
                    }
                }
            }
        }

        // Save the pixels under the new cursor position.
        rt.cursor_save_x = new_x;
        rt.cursor_save_y = new_y;
        for row in 0..cur_sz {
            let sy = new_y + row;
            for col in 0..cur_sz {
                let val = if sy >= 0 && sy < fbh_i {
                    let sx = new_x + col;
                    if sx >= 0 && sx < fbw_i {
                        let idx = (sy * fbw_i + sx) as usize;
                        if idx < fb_len {
                            fb[idx]
                        } else {
                            0
                        }
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

        // Draw the cursor at the new position.
        Compositor::draw_cursor_direct(fb, fbw, fbh, cur);
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

/// Map a raw PS/2 scancode to Resonance KeyCode using const lookup tables.
fn scancode_to_resonance_keycode(scancode: u8) -> KeyCode {
    // Extended key table: indexed by base scancode, returns Some(KeyCode) or None.
    const EXT: [Option<KeyCode>; 128] = {
        let mut t = [None; 128];
        t[0x1D] = Some(KeyCode::Ctrl);
        t[0x38] = Some(KeyCode::Alt);
        t[0x5B] = Some(KeyCode::SuperLeft);
        t[0x5C] = Some(KeyCode::SuperRight);
        t
    };
    // Base scancode table: maps all standard scancodes.
    const BASE: [KeyCode; 128] = {
        use KeyCode::*;
        let mut t = [Unknown(0); 128];
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
    // Skip compositor when rendering is suspended (e.g. BadApple playback).
    if *RENDERING_SUSPENDED.lock() {
        return;
    }

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

    render_terminal(rt, rt.term_window);
    let tb_changed = rt.desktop.update_taskbar();

    let (fb_pixels, fb_width, fb_height) = match framebuffer_fn() {
        Some(t) => t,
        None => return,
    };

    // Cache FB dimensions for maximize toggle
    *FB_DIMS.lock() = (fb_width, fb_height);

    // Cache framebuffer pointer and dimensions for lightweight cursor updates in overlay mode.
    *LAST_FB.lock() = (fb_pixels.as_mut_ptr() as usize, fb_width, fb_height);

    // Push clock‑change or taskbar‑change dirty rects BEFORE prepare_frame
    // so they are consumed together with all other dirty rects (cursor
    // movement, window D&D, menu show/dismiss).  A second prepare_frame call
    // would overwrite dirty_cache and discard the event‑handler rects.
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

    let fb_len = (fb_width as usize) * (fb_height as usize);
    if fb_len > MAX_FB_PIXELS {
        return;
    }
    rt.back_len = fb_len;

    let has_dirty = rt.desktop.has_pending_dirty_rects();

    if has_dirty {
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

        // ── Re‑seed the cursor save buffer BEFORE drawing the cursor ──
        // The save buffer must capture the clean framebuffer (overlay +
        // desktop) without the cursor, so the next lightweight update can
        // restore the old area correctly.
        if rt.desktop.cursor.visible {
            let (fb_addr, _, _) = *LAST_FB.lock();
            if fb_addr != 0 {
                let fb_ptr = fb_addr as *mut u32;
                let cur_sz = lattice::cursor::Cursor::SIZE as i32;
                let cx = rt.desktop.cursor.x - lattice::cursor::Cursor::HOTSPOT_X;
                let cy = rt.desktop.cursor.y - lattice::cursor::Cursor::HOTSPOT_Y;
                let fbw_i = fb_width as i32;
                let fbh_i = fb_height as i32;
                let fb_len = (fb_width as usize)
                    .saturating_mul(fb_height as usize);
                unsafe {
                    let fb =
                        core::slice::from_raw_parts(fb_ptr, fb_len);
                    for row in 0..cur_sz {
                        let sy = cy + row;
                        for col in 0..cur_sz {
                            let val =
                                if sy >= 0 && sy < fbh_i {
                                    let sx = cx + col;
                                    if sx >= 0 && sx < fbw_i {
                                        let idx =
                                            (sy * fbw_i + sx) as usize;
                                        if idx < fb_len {
                                            fb[idx]
                                        } else {
                                            0
                                        }
                                    } else {
                                        0
                                    }
                                } else {
                                    0
                                };
                            rt.cursor_save_buf
                                [(row * cur_sz + col) as usize] = val;
                        }
                    }
                }
                rt.cursor_save_x = cx;
                rt.cursor_save_y = cy;
                rt.cursor_save_valid = true;
            }
        }

        // ── Cursor on top of shell overlays ────────────────
        // Shell overlays are drawn directly onto fb_pixels AFTER the
        // compositor back‑buffer blit.  Any overlay that covers the
        // full screen (TaskOverview, AppGrid) would hide the cursor
        // if we didn't redraw it here.  Draw the cursor as the
        // last layer so it is always visible.
        if rt.desktop.cursor.visible {
            use lattice::compositor::Compositor;
            Compositor::draw_cursor_direct(fb_pixels, fb_width, fb_height, &rt.desktop.cursor);
        }
    }
}

/// Copy `len` u32 pixels from back‑buffer `src` to framebuffer `dst`.
///
/// Uses `core::ptr::copy_nonoverlapping` for maximum throughput.
/// The caller must issue an `sfence` (or equivalent GPU flush) after
/// the copy to make the writes globally visible for WC/UC framebuffers.
///
/// # Safety
/// `dst` and `src` must be valid for `len` u32 reads/writes.
/// Both pointers must be suitably aligned for u32 access (4 bytes).
/// Regions must NOT overlap.
unsafe fn copy_to_fb_volatile(dst: *mut u32, src: *const u32, len: usize) {
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, len);
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

pub fn set_render_fn(f: fn()) {
    *RENDER_FN.lock() = Some(f);
}

fn runtime_tick_no_fb() {
    let now = YIELD_TICK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);

    // Suppress rendering while suspended.
    if *RENDERING_SUSPENDED.lock() {
        return;
    }
    poll_mouse_state();
    poll_keyboard();
    update_clock();
    chrono_tick(now);
    process_events();

    // ── TSC‑based frame pacing ──────────────────────────────
    // `frame_due` may be set by the tick‑based FRAME_TIMER or by
    // event handlers.  On bare metal the yield loop can run at
    // 50 kHz, so we additionally gate on real elapsed time (TSC)
    // to cap at ≈60 fps and avoid burning CPU / GPU bandwidth.
    let do_render = RUNTIME.lock().as_mut().map_or(false, |r| {
        let due = r.frame_due;
        if due {
            let tsc_per_ms = TSC_PER_MS.load(core::sync::atomic::Ordering::Relaxed);
            let frame_tsc = tsc_per_ms.saturating_mul(FRAME_INTERVAL_MS);
            let last = LAST_RENDER_TSC.load(core::sync::atomic::Ordering::Relaxed);
            let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
            if now_tsc.wrapping_sub(last) < frame_tsc {
                // Too soon — re‑que the frame and skip rendering.
                r.frame_due = true;
                return false;
            }
            LAST_RENDER_TSC.store(now_tsc, core::sync::atomic::Ordering::Relaxed);
            r.frame_due = false;
        }
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
    // Skip tick when rendering is suspended (e.g. BadApple playback).
    // Still update global tick to keep keyboard repeat timed.
    GLOBAL_TICK.store(now, core::sync::atomic::Ordering::Relaxed);
    if *RENDERING_SUSPENDED.lock() {
        return;
    }
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

/// Global flag to temporarily suspend the compositor render pass.
/// When set, `runtime_tick` and `render` will skip framebuffer
/// writes so that another subsystem (e.g. BadApple) can drive the
/// framebuffer exclusively.
static RENDERING_SUSPENDED: spin::Mutex<bool> = spin::Mutex::new(false);

/// Suspend solvent compositor rendering.
/// Call before taking exclusive control of the framebuffer.
pub fn suspend_rendering() {
    *RENDERING_SUSPENDED.lock() = true;
}

/// Resume solvent compositor rendering.
/// Call after releasing exclusive framebuffer control.
pub fn resume_rendering() {
    *RENDERING_SUSPENDED.lock() = false;
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

// ── Menu action dispatch ───────────────────────────────────────

/// Dispatch a context-menu or system-menu action to the appropriate handler.
fn dispatch_menu_action(rt: &mut RuntimeState, action: &lattice::desktop::DesktopAction) {
    use lattice::desktop::DesktopAction;
    match action {
        DesktopAction::NewTerminal => {
            let id = rt
                .desktop
                .wm
                .create_titled_window(60, 50, TERM_WIN_W, TERM_WIN_H, 0x000000, "Terminal");
            rt.desktop.wm.raise_to_top(id);
            rt.frame_due = true;
        }
        DesktopAction::NewShell => {
            let id = rt
                .desktop
                .wm
                .create_titled_window(80, 70, TERM_WIN_W, TERM_WIN_H, 0x0a0a2e, "Shell");
            rt.desktop.wm.raise_to_top(id);
            rt.frame_due = true;
        }
        DesktopAction::TaskManager => open_info_window(rt, InfoWindow::TaskManager),
        DesktopAction::DeviceManager => open_info_window(rt, InfoWindow::DeviceManager),
        DesktopAction::FileManager => open_info_window(rt, InfoWindow::FileManager),
        DesktopAction::Refresh => {
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        DesktopAction::About => open_info_window(rt, InfoWindow::About),
        DesktopAction::ToggleTiling => {
            let (fw, fh) = *FB_DIMS.lock();
            let (ww, wh) = rt.desktop.work_area(fw, fh);
            rt.desktop.wm.toggle_tiling();
            rt.desktop.wm.retile(ww, wh);
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
        DesktopAction::SysInfo => {}  // TODO
        DesktopAction::Shutdown => {} // TODO
        DesktopAction::Reboot => {}   // TODO
        DesktopAction::Separator => {}
        DesktopAction::ChangeWallpaperSettings => {
            // Cycle through wallpaper presets: SolidColor → Grid → Gradient → Preset 0 → 1 → 2 → SolidColor...
            // This avoids opening a new window while still inside the mouse-down event
            // handler (which would re-enter the WM and risk deadlocks on the RUNTIME Mutex).
            let next = match get_wallpaper() {
                WallpaperMode::SolidColor => WallpaperMode::GridPattern,
                WallpaperMode::GridPattern => WallpaperMode::Gradient,
                WallpaperMode::Gradient => WallpaperMode::Preset(0),
                WallpaperMode::Preset(0) => WallpaperMode::Preset(1),
                WallpaperMode::Preset(1) => WallpaperMode::Preset(2),
                WallpaperMode::Preset(2) => WallpaperMode::SolidColor,
                _ => WallpaperMode::GridPattern,
            };
            set_wallpaper(next);
            rt.desktop.force_full_redraw();
            rt.frame_due = true;
        }
    }
}

/// Kind of system information window.
#[derive(Clone, Copy)]
enum InfoWindow {
    TaskManager,
    DeviceManager,
    FileManager,
    About,
    WallpaperSettings,
}

impl InfoWindow {
    fn params(self) -> (&'static str, i32, i32, u32, u32, u32, u32) {
        match self {
            Self::TaskManager => ("Task Manager", 120, 80, 44, 2, 0x0d0d1a, 0xCCCCCC),
            Self::DeviceManager => ("Device Manager", 140, 100, 46, 2, 0x0d1a0d, 0xCCFFCC),
            Self::FileManager => ("File Manager", 160, 120, 50, 3, 0x1a1a0d, 0xFFFFCC),
            Self::About => ("About Fullerene", 180, 140, 32, 0, 0x1a0d1a, 0xFFCCFF),
            Self::WallpaperSettings => ("Wallpaper Settings", 200, 110, 26, 1, 0x1a1a2e, 0xCCCCCC),
        }
    }
}

fn open_info_window(rt: &mut RuntimeState, kind: InfoWindow) {
    let text = match kind {
        InfoWindow::TaskManager => {
            let Some(get_procs) = *PROCESS_LIST_FN.lock() else {
                return show_text_window(
                    rt,
                    "Task Manager",
                    120,
                    80,
                    44,
                    2,
                    0x0d0d1a,
                    0xCCCCCC,
                    "PID   NAME              STATE\n----  ----------------  --------\n (no process list callback)\n",
                );
            };
            let procs = get_procs();
            let mut s =
                String::from("PID   NAME              STATE\n----  ----------------  --------\n");
            for p in &procs {
                let state = match p.state {
                    ProcessStateKind::Ready => "ready",
                    ProcessStateKind::Running => "running",
                    ProcessStateKind::Blocked => "blocked",
                    ProcessStateKind::Terminated => "term",
                };
                let _ = core::write!(
                    &mut s,
                    " {:<4}  {:<16}  {:<8}\n",
                    p.pid,
                    truncate_to_chars(&p.name, 16),
                    state
                );
            }
            s
        }
        InfoWindow::DeviceManager => {
            let Some(get_devs) = *DEVICE_LIST_FN.lock() else {
                return show_text_window(
                    rt,
                    "Device Manager",
                    140,
                    100,
                    46,
                    2,
                    0x0d1a0d,
                    0xCCFFCC,
                    "DEVICE              TYPE        ENABLED\n------------------  ----------  -------\n (no device list callback)\n",
                );
            };
            let devs = get_devs();
            let mut s = String::from(
                "DEVICE              TYPE        ENABLED\n------------------  ----------  -------\n",
            );
            for d in &devs {
                let n = &d.name[..d.name.len().min(18)];
                let t = &d.dev_type[..d.dev_type.len().min(10)];
                let _ = core::write!(
                    &mut s,
                    " {:<18}  {:<10}  {:<7}\n",
                    n,
                    t,
                    if d.enabled { "yes" } else { "no" }
                );
            }
            s
        }
        InfoWindow::FileManager => {
            let Some(readdir) = *VFS_READDIR_FN.lock() else {
                return show_text_window(
                    rt,
                    "File Manager",
                    160,
                    120,
                    50,
                    3,
                    0x1a1a0d,
                    0xFFFFCC,
                    "  Name              Size        Type\n------------------  ----------  ----\n (no VFS readdir callback)\n",
                );
            };
            match readdir("/") {
                Ok(entries) => {
                    let mut s = String::from(
                        "  Name              Size        Type\n------------------  ----------  ----\n",
                    );
                    for e in &entries {
                        let size = if e.is_dir {
                            String::from("--")
                        } else if e.size >= 1048576 {
                            format!("{}.{} MB", e.size / 1048576, ((e.size % 1048576) * 10) / 1048576)
                        } else if e.size >= 1024 {
                            format!("{}.{} KB", e.size / 1024, (e.size % 1024) * 10 / 1024)
                        } else {
                            format!("{} B", e.size)
                        };
                        let n = {
                            let l = (0..=18)
                                .rev()
                                .find(|&l| e.name.is_char_boundary(l))
                                .unwrap_or(0);
                            &e.name[..l]
                        };
                        let _ = core::write!(
                            &mut s,
                            "  {:<18}  {:<10}  {}\n",
                            n,
                            size,
                            if e.is_dir { "dir" } else { "file" }
                        );
                    }
                    if entries.is_empty() {
                        s.push_str("  (empty directory)\n");
                    }
                    s.push_str(&format!("\n  Path: {}\n  {} entries", "/", entries.len()));
                    s
                }
                Err(e) => format!("  Error reading directory:\n  {} ({})\n", "/", e),
            }
        }
        InfoWindow::About => String::from(
            "Fullerene OS\n============\n\nA microkernel-based\noperating system\nwritten in Rust.\n\nVersion: 0.1.0\nLicense: MIT/Apache-2.0\n\n(c) 2025-2026\n",
        ),
        InfoWindow::WallpaperSettings => String::from(
            "  Wallpaper Settings\n ===================\n\n [ ] Beach\n [ ] Mountain\n [ ] City\n ───────────────────\n [ ] Solid Color\n [ ] Grid Pattern\n [ ] Gradient\n\n Use 'wallpaper <name>'\n in terminal to switch.\n\n Ex: wallpaper beach\n",
        ),
    };
    let (title, x, y, cols, extra_rows, bg, fg) = kind.params();
    show_text_window(rt, title, x, y, cols, extra_rows, bg, fg, &text);
}

/// Common helper: create a titled window, fill its surface with `text`,
/// raise it to top, and schedule a redraw.
fn show_text_window(
    rt: &mut RuntimeState,
    title: &str,
    x: i32,
    y: i32,
    cols: u32,
    extra_rows: u32,
    bg: u32,
    fg: u32,
    text: &str,
) {
    let rows = (text.lines().count() as u32) + extra_rows;
    let id = rt
        .desktop
        .wm
        .create_titled_window(x, y, cols * GLYPH_W, rows * GLYPH_H, bg, title);
    if let Some(w) = rt.desktop.wm.windows_mut().iter_mut().find(|w| w.id == id) {
        let _ = render_text_into_surface(&mut w.surface, text, cols, fg, bg);
    }
    rt.desktop.wm.raise_to_top(id);
    rt.frame_due = true;
}

/// Render a multi-line text string into a Surface.
/// Returns the number of lines rendered.
fn render_text_into_surface(
    surface: &mut lattice::surface::Surface,
    text: &str,
    max_cols: u32,
    fg_color: u32,
    bg_color: u32,
) -> u32 {
    use lattice::terminal_surface;
    use lattice::terminal_surface::Cell as LatticeCell;

    let cols = max_cols as usize;
    let lines_count = text.lines().count() as u32;
    let total = (cols as u32 * lines_count) as usize;
    let mut cells: Vec<LatticeCell> = Vec::new();
    cells.resize(
        total,
        LatticeCell {
            ch: b' ',
            fg: fg_color,
            bg: bg_color,
        },
    );

    for (row, line) in text.lines().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            if col < cols {
                let idx = row * cols + col;
                if idx < cells.len() {
                    cells[idx] = LatticeCell {
                        ch,
                        fg: fg_color,
                        bg: bg_color,
                    };
                }
            }
        }
    }

    terminal_surface::render(terminal_surface::RenderParams {
        surface,
        cells: &cells,
        cols: cols as u32,
        cursor_col: None,
        cursor_row: None,
        cursor_visible: false,
    });

    lines_count
}

// ── Theme / wallpaper bridges (avoid kernel → lattice coupling) ─────

pub use lattice::theme::{ThemeVariant, current_theme_variant, set_theme, toggle_theme};
pub use lattice::wallpaper::{
    WallpaperMode, WallpaperPreset, find_preset, get_wallpaper, set_wallpaper, wallpaper_presets,
};

// ── Window API (for external apps like RLE Player) ────────────────

/// Create a new titled window on the desktop and return its ID.
///
/// The returned [`WindowId`] can be used with the other window API
/// functions to draw into the surface, trigger redraws, or close
/// the window.
pub fn create_window(
    title: impl Into<alloc::string::String>,
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

/// Get a mutable reference to a window's surface pixels.
///
/// Returns `None` if the window does not exist or is minimized.
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
        let width = w.surface.width();
        let height = w.surface.height();
        Some(f(w.surface.pixels_mut(), width, height))
    })
}

/// Mark a window's surface as dirty so it will be redrawn on the
/// next compositor pass.
pub fn invalidate_window(id: WindowId) {
    if let Some(ref mut rt) = *RUNTIME.lock() {
        rt.desktop.invalidate_window(id);
        rt.frame_due = true;
        rt.term_dirty = true;
    }
}

/// Close (remove) a window from the desktop.
pub fn close_window(id: WindowId) -> bool {
    RUNTIME
        .lock()
        .as_mut()
        .map_or(false, |rt| rt.desktop.wm.close_window(id))
}

/// Get the current framebuffer dimensions.
pub fn framebuffer_dims() -> (u32, u32) {
    *FB_DIMS.lock()
}

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
