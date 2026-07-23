//! Solvent - runtime and orchestration layer.
//!
//! Solvent sits between the kernel and higher-level subsystems (Lattice,
//! Nozzle, Resonance, ChronoLine). It owns runtime coordination, subsystem
//! bootstrap, event processing, frame pacing, and service lifecycle.
//!
//! # Module boundaries
//!
//! - `runtime_context` owns runtime state definitions and initialization.
//! - `input_loop` translates hardware input into desktop or Resonance events.
//! - `event_loop` coordinates timers, services, events, and frame ticks.
//! - `window_api` exposes window lifecycle and redraw operations.
//! - `callbacks` defines the kernel-to-runtime integration contract.
//! - `services` owns runtime-managed service registration and snapshots.

#![no_std]

extern crate alloc;

mod callbacks;
mod clock;
mod editor_bridge;
mod event_loop;
mod explorer;
mod file;
mod handlers;
mod input_loop;
mod menu_actions;
mod network_manager;
mod render;
mod runtime_context;
mod services;
mod settings_bridge;
mod terminal;
pub mod viewer;
mod viewers;
mod window_api;

pub use callbacks::{
    DeviceEntry, ProcessEntry, ProcessStateKind, SolventCallbacks, VfsEntry, VfsHandle,
    exec_shell_command, get_mounted_drives, launch_shell,
};
pub use editor_bridge::editor_handle_key;
pub use event_loop::{
    GLOBAL_TICK, chrono_tick, consume_frame_due, cursor_update_due, process_events, push_key_event,
    runtime_tick, runtime_tick_no_fb, set_render_fn, tick_core,
};
pub use file::RuntimeFile;
pub use input_loop::{MOUSE_STATE, MouseState, poll_keyboard, poll_mouse_state};
pub use render::{render, render_cursor_fast, set_render_progress_fn};
pub use runtime_context::{
    DISPLAY_BRIGHTNESS_X100, HEAP_EXTEND_RESERVE, KLOG_SAVE_ENABLED, MOUSE_SENSITIVITY,
    RUNTIME_CONTEXT, RuntimeContext, RuntimeState, apply_settings, get_tsc_per_ms, init,
    is_initialized, set_tsc_per_ms,
};
#[cfg(not(nitrogen_no_iwlwifi))]
pub use services::register_wifi_service;
pub use services::{
    NETWORK_SNAPSHOT, NetworkSnapshot, Service, WIFI_ACTION_QUEUE, WifiAction, register_service,
};
pub use settings_bridge::settings_handle_key;
pub use terminal::{LatticeTerminal, PIPE_STDIN, PIPE_STDOUT, render_terminal};
pub use viewer::take_pending_shell_command;
pub use window_api::{
    close_window, create_window, ensure_editor_window, ensure_terminal_window,
    force_desktop_redraw, framebuffer_dims, invalidate_window, launch_file, resume_rendering,
    suspend_rendering, with_window_surface, write_terminal,
};

pub use lattice::theme::{
    ThemeStyle, ThemeVariant, current_style, current_theme_variant, set_style, set_theme,
    toggle_style, toggle_theme,
};
pub use lattice::wallpaper::{
    WallpaperMode, WallpaperPreset, find_preset, get_wallpaper, set_wallpaper, wallpaper_presets,
};

pub(crate) use input_loop::{scancode_to_ascii, scancode_to_resonance_keycode};
pub(crate) use runtime_context::{
    BACK_BUFFER, CURSOR_TIMER_ID, DEFAULT_COLS, DEFAULT_ROWS, FB_DIMS, FRAME_INTERVAL_MS,
    FRAME_TIMER_ID, GLYPH_H, GLYPH_W, PREV_MOUSE_BUTTONS, TERM_WIN_H, TERM_WIN_W, TSC_PER_MS,
};
pub(crate) use services::SERVICES;
pub(crate) use window_api::{RENDERING_SUSPENDED, render_explorer};

use alloc::string::String;

pub(crate) fn truncate_to_chars(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}

pub fn run_shell_on(
    terminal: &mut dyn carrier::terminal::Terminal,
    prompt: &str,
    services: nozzle::ShellServices,
) {
    run_shell_on_with_command(terminal, prompt, services, None);
}

pub fn run_shell_on_with_command(
    terminal: &mut dyn carrier::terminal::Terminal,
    prompt: &str,
    services: nozzle::ShellServices,
    initial_command: Option<&str>,
) {
    let mut shell = nozzle::Shell::new(terminal, nozzle::default_commands(), services);
    shell.set_prompt(prompt);
    shell.run_with_initial_line(initial_command);
}

pub(crate) static SUPER_HELD: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
