//! Runtime state, configuration, and initialization.
//!
//! This module preserves the current singleton ownership model. Consolidating
//! the singletons into an owned `RuntimeContext` is tracked separately.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use chronoline::{ChronoLine, Deadline, TimerId, TimerMode};
use lattice::desktop::Desktop;
use lattice::editor::EditorBuffer;
use lattice::shell_overlay::ShellState;
use lattice::terminal_surface::Cell as LatticeCell;
use lattice::window::WindowId;
use nozzle::terminal_buffer::TerminalBuffer;
use resonance::{Dispatcher, EventQueue};
use spin::Mutex;

use crate::handlers;

pub(crate) const DEFAULT_COLS: u32 = 80;
pub(crate) const DEFAULT_ROWS: u32 = 25;
pub(crate) const GLYPH_W: u32 = 8;
pub(crate) const GLYPH_H: u32 = 16;
pub(crate) const TERM_WIN_W: u32 = DEFAULT_COLS * GLYPH_W;
pub(crate) const TERM_WIN_H: u32 = DEFAULT_ROWS * GLYPH_H;
const BG_COLOR: u32 = 0x1a1a2e;
pub(crate) const CURSOR_BLINK_INTERVAL: u64 = 100;
pub(crate) const CURSOR_TIMER_ID: TimerId = TimerId(1);
pub(crate) const FRAME_INTERVAL_TICKS: u64 = 8;
pub(crate) const FRAME_INTERVAL_MS: u64 = 17;
pub(crate) const FRAME_TIMER_ID: TimerId = TimerId(2);

pub static MOUSE_SENSITIVITY: core::sync::atomic::AtomicI16 = core::sync::atomic::AtomicI16::new(6);
pub static DISPLAY_BRIGHTNESS_X100: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(100);
pub static HEAP_EXTEND_RESERVE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
pub(crate) static TSC_PER_MS: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(3_000_000);

pub(crate) static BACK_BUFFER: Mutex<Option<Vec<u32>>> = Mutex::new(None);
pub(crate) static RUNTIME: Mutex<Option<RuntimeState>> = Mutex::new(None);
pub(crate) static EVENT_QUEUE: Mutex<Option<EventQueue>> = Mutex::new(None);
pub(crate) static DISPATCHER: Mutex<Option<Dispatcher>> = Mutex::new(None);
pub(crate) static PREV_MOUSE_BUTTONS: Mutex<u8> = Mutex::new(0);
pub(crate) static FB_DIMS: Mutex<(u32, u32, u32)> = Mutex::new((1024, 768, 1024));

/// Mutable desktop runtime state protected by the crate's runtime lock.
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
    pub editor_window: Option<WindowId>,
    pub editor_buf: EditorBuffer,
    pub editor_launch_pending: bool,
    pub editor_dirty: bool,
    pub editor_file_path: Option<String>,
    pub explorer: Option<crate::explorer::ExplorerContext>,
    pub explorer_dirty: bool,
    pub settings_window: Option<WindowId>,
    pub settings_dirty: bool,
    /// Earliest cursor position still drawn on the framebuffer while a redraw
    /// is pending. The full and lightweight render paths both consume it.
    pub(crate) cursor_redraw_from: Option<(i32, i32)>,
}

impl RuntimeState {
    pub(crate) fn request_cursor_redraw(&mut self, previous: (i32, i32)) {
        self.cursor_redraw_from.get_or_insert(previous);
        if self.frame_due || !matches!(self.desktop.wm.drag_state(), lattice::wm::DragState::None) {
            self.frame_due = true;
        }
    }
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
        editor_window: None,
        editor_buf: EditorBuffer::new(),
        editor_launch_pending: false,
        editor_dirty: false,
        editor_file_path: None,
        explorer: None,
        explorer_dirty: false,
        settings_window: None,
        settings_dirty: false,
        cursor_redraw_from: None,
    });
}

pub fn is_initialized() -> bool {
    RUNTIME.lock().is_some()
}

pub fn apply_settings(sensitivity: f32, brightness_x100: u32, top_panel_enabled: bool) {
    MOUSE_SENSITIVITY.store(
        (sensitivity * 6.0) as i16,
        core::sync::atomic::Ordering::Relaxed,
    );
    DISPLAY_BRIGHTNESS_X100.store(brightness_x100, core::sync::atomic::Ordering::Relaxed);
    lattice::top_panel::set_top_panel_enabled(top_panel_enabled);
    crate::force_desktop_redraw();
}

pub fn set_tsc_per_ms(value: u64) {
    TSC_PER_MS.store(value, core::sync::atomic::Ordering::Relaxed);
}

pub fn get_tsc_per_ms() -> u64 {
    TSC_PER_MS.load(core::sync::atomic::Ordering::Relaxed)
}
