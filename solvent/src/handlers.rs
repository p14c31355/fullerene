//! Event handlers — extracted from lib.rs as part of god-module decomposition.
//!
//! These handlers are thin wrappers that delegate to `crate` globals.
//! The heavy logic (menu dispatch, terminal I/O) lives in dedicated modules.

use crate::{FB_DIMS, RUNTIME, SUPER_HELD};
use lattice::shell_overlay::ShellState;
use resonance::{Event, EventHandler, InputEvent, KeyCode, MouseButton};

const DOUBLE_CLICK_TICKS: u64 = 500;

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
                rt.frame_due = true;
                true
            }
            Event::Input(InputEvent::MouseDown(btn)) => {
                let cx = rt.desktop.cursor.x;
                let cy = rt.desktop.cursor.y;

                // Check desktop icon clicks (left button only)
                if *btn == MouseButton::Left {
                    if let Some(icon_idx) = rt.desktop.desktop_icons.hit_test(cx, cy) {
                        if let Some(icon) = rt.desktop.desktop_icons.icons.get(icon_idx) {
                            match icon.label.as_str() {
                                "Shell" => {
                                    // Defer shell launch — cannot call
                                    // ensure_terminal_window() or launch_shell()
                                    // while holding RUNTIME lock (deadlock).
                                    rt.shell_launch_pending = true;
                                    rt.frame_due = true;
                                    return true;
                                }
                                "Files" => {
                                    crate::menu_actions::open_info_window(
                                        rt,
                                        crate::menu_actions::InfoWindow::FileManager,
                                    );
                                    rt.frame_due = true;
                                    return true;
                                }
                                "Settings" => {
                                    crate::menu_actions::open_settings_window(rt);
                                    rt.frame_due = true;
                                    return true;
                                }
                                "About" => {
                                    crate::menu_actions::open_info_window(
                                        rt,
                                        crate::menu_actions::InfoWindow::About,
                                    );
                                    rt.frame_due = true;
                                    return true;
                                }
                                _ => {}
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

                // Handle clicks within the explorer window's client area
                if *btn == MouseButton::Left || *btn == MouseButton::Right {
                    handle_explorer_click(rt, *btn, cx, cy);
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

// ── Explorer event handling ──────────────────────────────────

fn handle_explorer_click(rt: &mut crate::RuntimeState, btn: MouseButton, cx: i32, cy: i32) {
    let explorer = match rt.explorer.as_mut() {
        Some(e) => e,
        None => return,
    };
    let win_id = match explorer.window_id {
        Some(id) => id,
        None => return,
    };
    let window = match rt.desktop.wm.windows().iter().find(|w| w.id == win_id) {
        Some(w) => w,
        None => return,
    };
    // Only process clicks within the explorer's client area (below title bar)
    if !window.contains(cx, cy) {
        return;
    }
    let rel_x = cx - window.x;
    let rel_y = cy - window.y - lattice::compositor::TITLE_BAR_HEIGHT as i32;

    // If context menu is open, handle clicks on it first
    if explorer.context_menu.open {
        let launch_path = crate::explorer::handle_context_menu_click(explorer, rel_x, rel_y)
            .and_then(|action| explorer.dispatch_context_action(action));
        rt.explorer_dirty = true;
        rt.frame_due = true;
        if let Some(path) = launch_path {
            crate::launch_file(rt, &path);
        }
        return;
    }

    match btn {
        MouseButton::Left => {
            // Check toolbar buttons
            if let Some(btn_id) = crate::explorer::hit_toolbar_button(rel_x, rel_y) {
                match btn_id {
                    b'b' => explorer.go_back(),
                    b'f' => explorer.go_forward(),
                    b'u' => explorer.go_up(),
                    b'r' => explorer.refresh(),
                    _ => {}
                }
                rt.explorer_dirty = true;
                rt.frame_due = true;
                return;
            }

            // Check sidebar click
            if let Some(idx) = crate::explorer::hit_sidebar(explorer, rel_x, rel_y) {
                explorer.selected_sidebar = Some(idx);
                if let Some(item) = explorer.sidebar_items.get(idx) {
                    let path = item.path.clone();
                    explorer.navigate_to(&path);
                }
                rt.explorer_dirty = true;
                rt.frame_due = true;
                return;
            }

            // Check file list click
            let win_w = window.width;
            let win_h = window.height;
            if let Some(idx) = crate::explorer::hit_file_list(explorer, win_w, win_h, rel_x, rel_y)
            {
                // Double-click detection
                let now = crate::GLOBAL_TICK.load(core::sync::atomic::Ordering::Relaxed);
                let is_double = explorer.selected_index == Some(idx)
                    && explorer.last_click_entry == Some(idx)
                    && now.wrapping_sub(explorer.last_click_tick) <= DOUBLE_CLICK_TICKS;

                explorer.selected_index = Some(idx);

                if is_double {
                    let launch_path = explorer.activate_entry(idx);
                    explorer.last_click_entry = None;
                    if let Some(path) = launch_path {
                        // Save path, drop explorer borrow, then launch
                        let _ = explorer;
                        crate::launch_file(rt, &path);
                        return;
                    }
                } else {
                    explorer.last_click_entry = Some(idx);
                    explorer.last_click_tick = now;
                }

                rt.explorer_dirty = true;
                rt.frame_due = true;
            }
        }
        MouseButton::Right => {
            let win_w = window.width;
            let win_h = window.height;
            // The empty portion of a directory must expose Paste as well.
            if crate::explorer::hit_file_area(win_w, win_h, rel_x, rel_y) {
                let hit = crate::explorer::hit_file_list(explorer, win_w, win_h, rel_x, rel_y);
                explorer.context_menu.open = true;
                explorer.context_menu.x = (rel_x.max(0) as u32)
                    .min(win_w.saturating_sub(crate::explorer::CONTEXT_MENU_W));
                explorer.context_menu.y = (rel_y.max(0) as u32)
                    .min(win_h.saturating_sub(6 * crate::explorer::ROW_HEIGHT));
                explorer.selected_index = hit;
                rt.explorer_dirty = true;
                rt.frame_due = true;
            }
        }
        _ => {}
    }
}

fn handle_overlay_event(rt: &mut crate::RuntimeState, event: &Event) -> bool {
    match event {
        Event::Input(InputEvent::MouseMove { x, y }) => {
            rt.desktop.mouse_move(*x, *y);
            rt.frame_due = true;
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
    let cx = rt.desktop.cursor.x as i32;
    let cy = rt.desktop.cursor.y as i32;
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
            crate::clock::TIMEZONE_OFFSET_HOURS
                .store(*offset, core::sync::atomic::Ordering::Relaxed);
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
    let cx = rt.desktop.cursor.x as i32;
    let cy = rt.desktop.cursor.y as i32;
    let (fw, _fh, _stride) = *FB_DIMS.lock();

    let icon_size = 64i32;
    let pad = 24i32;
    let label_h = 18i32;
    let columns = ((fw as i32) / (icon_size + pad)).max(1);
    let start_y = 60i32;

    for idx in 0i32..7 {
        let col = idx % columns;
        let row = idx / columns;
        let ax = pad + col * (icon_size + pad);
        let ay = start_y + row * (icon_size + label_h + pad);
        if cx >= ax && cx < ax + icon_size && cy >= ay && cy < ay + icon_size + label_h {
            match idx {
                0 => {
                    // Defer shell launch — cannot call ensure_terminal_window()
                    // or launch_shell() while holding RUNTIME lock (deadlock).
                    rt.shell_launch_pending = true;
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                2 => {
                    // Defer editor launch — cannot call ensure_editor_window()
                    // while holding RUNTIME lock (deadlock).
                    rt.editor_launch_pending = true;
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                4 => {
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
