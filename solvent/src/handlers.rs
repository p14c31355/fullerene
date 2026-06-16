//! Event handlers — extracted from lib.rs as part of god-module decomposition.
//!
//! These handlers are thin wrappers that delegate to `crate` globals.
//! The heavy logic (menu dispatch, terminal I/O) lives in dedicated modules.

use crate::cursor_lightweight_update;
use crate::{
    FB_DIMS, MOUSE_STATE, RUNTIME, SUPER_HELD, TIMEZONE_OFFSET_HOURS,
    ensure_terminal_window, launch_shell,
};
use lattice::shell_overlay::ShellState;
use lattice::wm::DragState;
use resonance::{Event, EventHandler, InputEvent, KeyCode, MouseButton};

pub(crate) struct WmEventHandler;

impl EventHandler for WmEventHandler {
    fn handle(&mut self, event: &Event) -> bool {
        let mut rt = RUNTIME.lock();
        let rt = match rt.as_mut() {
            Some(r) => r,
            None => return false,
        };

        if rt.shell_state != ShellState::Desktop {
            return handle_overlay_event(rt, event);
        }

        match event {
            Event::Input(InputEvent::MouseMove { x, y }) => {
                rt.desktop.mouse_move(*x, *y);
                cursor_lightweight_update(rt);
                if !matches!(rt.desktop.wm.drag_state(), DragState::None)
                    || rt.desktop.has_pending_dirty_rects()
                {
                    rt.frame_due = true;
                }
                true
            }
            Event::Input(InputEvent::MouseDown(btn)) => {
                let cx = rt.desktop.cursor.x;
                let cy = rt.desktop.cursor.y;

                // Check desktop icon clicks (left button only)
                if *btn == MouseButton::Left {
                    if let Some(icon_idx) = rt.desktop.desktop_icons.hit_test(cx, cy) {
                        if let Some(icon) = rt.desktop.desktop_icons.icons.get(icon_idx) {
                            if icon.label == "Shell" {
                                // Ensure terminal window exists, then launch the shell
                                ensure_terminal_window();
                                launch_shell();
                                rt.frame_due = true;
                                return true;
                            }
                        }
                    }
                }

                if *btn == MouseButton::Right {
                    let hit_window = rt.desktop.wm.window_at(cx, cy);
                    if hit_window.is_none() {
                        rt.desktop.show_context_menu(cx, cy);
                        rt.frame_due = true;
                        return true;
                    }
                }

                if rt.desktop.top_panel.hit_activities_button(cx, cy) {
                    rt.shell_state = ShellState::TaskOverview;
                    rt.frame_due = true;
                    return true;
                }

                rt.desktop.set_cursor(cx, cy);
                let (fw, fh, _stride) = *FB_DIMS.lock();
                rt.desktop.mouse_down(fw, fh);
                rt.frame_due = true;

                if let Some(action) = rt.desktop.menu_action_pending.take() {
                    crate::menu_actions::dispatch_menu_action(rt, &action);
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

fn handle_overlay_event(rt: &mut crate::RuntimeState, event: &Event) -> bool {
    match event {
        Event::Input(InputEvent::MouseMove { x, y }) => {
            rt.desktop.mouse_move(*x, *y);
            cursor_lightweight_update(rt);
            true
        }
        Event::Input(InputEvent::MouseDown(_))
            if rt.shell_state == ShellState::TimeZoneSelector =>
        {
            handle_timezone_click(rt)
        }
        Event::Input(InputEvent::MouseDown(_)) if rt.shell_state == ShellState::AppGrid => {
            handle_appgrid_click(rt)
        }
        Event::Input(InputEvent::MouseDown(_)) => {
            rt.shell_state = ShellState::Desktop;
            rt.frame_due = true;
            true
        }
        _ => false,
    }
}

fn handle_timezone_click(rt: &mut crate::RuntimeState) -> bool {
    let mouse = MOUSE_STATE.lock();
    let cx = mouse.x as i32;
    let cy = mouse.y as i32;
    drop(mouse);
    let (fw, _fh, _stride) = *FB_DIMS.lock();

    let timezones: &[i8] = &[-12, -8, -5, 0, 1, 3, 5, 8, 9, 10, 12];
    let entry_h = 24i32;
    let pad = 6i32;
    let start_y = 40i32;
    let entry_w = 16 * 8 + 16;
    let ex = ((fw as i32) - entry_w) / 2;

    for (i, offset) in timezones.iter().enumerate() {
        let ey = start_y + (i as i32) * (entry_h + pad);
        if cy >= ey && cy < ey + entry_h && cx >= ex && cx < ex + entry_w {
            TIMEZONE_OFFSET_HOURS.store(*offset, core::sync::atomic::Ordering::Relaxed);
            rt.shell_state = ShellState::Desktop;
            rt.frame_due = true;
            return true;
        }
    }
    rt.shell_state = ShellState::AppGrid;
    rt.frame_due = true;
    true
}

fn handle_appgrid_click(rt: &mut crate::RuntimeState) -> bool {
    let mouse = MOUSE_STATE.lock();
    let cx = mouse.x as i32;
    let cy = mouse.y as i32;
    drop(mouse);
    let (fw, _fh, _stride) = *FB_DIMS.lock();

    let icon_size = 64i32;
    let pad = 24i32;
    let label_h = 18i32;
    let columns = ((fw as i32) / (icon_size + pad)).max(1);
    let start_y = 60i32;

    for idx in 0i32..6 {
        let col = idx % columns;
        let row = idx / columns;
        let ax = pad + col * (icon_size + pad);
        let ay = start_y + row * (icon_size + label_h + pad);
        if cx >= ax && cx < ax + icon_size && cy >= ay && cy < ay + icon_size + label_h {
            match idx {
                0 => {
                    ensure_terminal_window();
                    launch_shell();
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                3 => {
                    rt.shell_state = ShellState::TimeZoneSelector;
                    rt.frame_due = true;
                    return true;
                }
                _ => {
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
            }
        }
    }
    rt.shell_state = ShellState::Desktop;
    rt.frame_due = true;
    true
}

pub(crate) struct TerminalInputHandler;

impl EventHandler for TerminalInputHandler {
    fn handle(&mut self, event: &Event) -> bool {
        match event {
            Event::Input(InputEvent::KeyDown(KeyCode::PageUp)) => {
                if let Some(ref mut rt) = *RUNTIME.lock() {
                    rt.term_buf.scroll_back(1);
                    rt.term_dirty = true;
                    rt.frame_due = true;
                }
                true
            }
            Event::Input(InputEvent::KeyDown(KeyCode::PageDown)) => {
                if let Some(ref mut rt) = *RUNTIME.lock() {
                    rt.term_buf.scroll_forward(1);
                    rt.term_dirty = true;
                    rt.frame_due = true;
                }
                true
            }
            Event::Input(InputEvent::KeyDown(KeyCode::Home)) => {
                if let Some(ref mut rt) = *RUNTIME.lock() {
                    rt.term_buf.reset_scroll();
                    rt.term_dirty = true;
                    rt.frame_due = true;
                }
                true
            }
            _ => false,
        }
    }
}

pub(crate) struct ShellEventHandler;

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
                true
            }
            Event::Input(InputEvent::KeyUp(KeyCode::SuperLeft))
            | Event::Input(InputEvent::KeyUp(KeyCode::SuperRight)) => {
                SUPER_HELD.store(false, core::sync::atomic::Ordering::Relaxed);
                false
            }
            Event::Input(InputEvent::KeyDown(KeyCode::T))
                if SUPER_HELD.load(core::sync::atomic::Ordering::Relaxed)
                    && rt.shell_state == ShellState::Desktop =>
            {
                let (fw, fh, _stride) = *FB_DIMS.lock();
                let (ww, wh) = rt.desktop.work_area(fw, fh);
                rt.desktop.wm.toggle_tiling();
                rt.desktop.wm.retile(ww, wh);
                rt.desktop.force_full_redraw();
                rt.frame_due = true;
                true
            }
            Event::Input(InputEvent::KeyDown(KeyCode::Escape)) => {
                if rt.shell_state != ShellState::Desktop {
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                SUPER_HELD.store(false, core::sync::atomic::Ordering::Relaxed);
                false
            }
            _ => false,
        }
    }
}
