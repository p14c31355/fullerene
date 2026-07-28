//! Event handlers — extracted from lib.rs as part of god-module decomposition.
//!
//! These handlers are thin wrappers over state owned by `RuntimeContext`.
//! The heavy logic (menu dispatch, terminal I/O) lives in dedicated modules.

use crate::{FB_DIMS, RUNTIME_CONTEXT, SUPER_HELD, window_api::PENDING_LAUNCH};
use lattice::shell_overlay::ShellState;
use resonance::{Event, EventHandler, InputEvent, KeyCode, MouseButton};

const DOUBLE_CLICK_TICKS: u64 = 500;

fn style_launcher_at(rt: &crate::RuntimeState, x: i32, y: i32) -> Option<usize> {
    let count = rt.desktop.taskbar.entries.len();
    let (width, height, _) = *FB_DIMS.lock();
    let max = match lattice::style::kind() {
        lattice::common::ShellKind::Photon => lattice::common::PHOTON_LAUNCHER_ROUTES.len(),
        lattice::common::ShellKind::Prism => lattice::common::PRISM_LAUNCHER_ROUTES.len(),
        lattice::common::ShellKind::Basalt => 0,
    };
    (0..max).find(|&index| {
        lattice::style::launcher_entry_rect(index, count, width, height).is_some_and(
            |(lx, ly, lw, lh)| x >= lx && x < lx + lw as i32 && y >= ly && y < ly + lh as i32,
        )
    })
}

fn route_style_launcher(rt: &mut crate::RuntimeState, index: usize) -> bool {
    if lattice::style::kind() == lattice::common::ShellKind::Prism && index == 0 {
        rt.shell_state = ShellState::AppGrid;
        rt.frame_due = true;
        return true;
    }
    let route = match lattice::style::kind() {
        lattice::common::ShellKind::Photon => lattice::common::PHOTON_LAUNCHER_ROUTES.get(index),
        lattice::common::ShellKind::Prism => lattice::common::PRISM_LAUNCHER_ROUTES.get(index),
        lattice::common::ShellKind::Basalt => None,
    };
    let Some(route) = route.copied() else {
        return false;
    };
    match route {
        lattice::common::AppRoute::Shell => rt.shell_launch_pending = true,
        lattice::common::AppRoute::Terminal => crate::menu_actions::dispatch_menu_action(
            rt,
            &lattice::desktop::DesktopAction::NewTerminal,
        ),
        lattice::common::AppRoute::Files => {
            crate::menu_actions::open_info_window(rt, crate::menu_actions::InfoWindow::FileManager)
        }
        lattice::common::AppRoute::Settings => crate::menu_actions::open_settings_window(rt),
        lattice::common::AppRoute::About => {
            crate::menu_actions::open_info_window(rt, crate::menu_actions::InfoWindow::About)
        }
        lattice::common::AppRoute::Editor => rt.editor_launch_pending = true,
        lattice::common::AppRoute::Clock => {
            crate::menu_actions::open_info_window(rt, crate::menu_actions::InfoWindow::SystemInfo)
        }
        lattice::common::AppRoute::Unknown => return false,
    }
    rt.frame_due = true;
    true
}

fn apply_mouse_move(
    desktop: &mut lattice::desktop::Desktop,
    cursor_redraw_from: &mut Option<(i32, i32)>,
    frame_due: &mut bool,
    x: i32,
    y: i32,
) {
    let previous = (desktop.cursor.x, desktop.cursor.y);
    desktop.mouse_move(x, y);
    cursor_redraw_from.get_or_insert(previous);

    // Moving the pointer only changes the cursor pixels.  A full scene
    // render for every PS/2 packet makes a maximised terminal feel frozen
    // on real hardware.  Window moves/resizes still need a full render
    // because WindowManager::on_mouse_move dirties the window bounds.
    if !matches!(desktop.wm.drag_state(), lattice::wm::DragState::None) {
        *frame_due = true;
    }
}

pub(crate) struct WmEventHandler;

impl EventHandler for WmEventHandler {
    fn handle(&mut self, event: &Event) -> bool {
        let mut rt = RUNTIME_CONTEXT.runtime();
        let rt = match rt.as_mut() {
            Some(r) => r,
            None => return false,
        };

        if rt.shell_state != ShellState::Desktop {
            return handle_overlay_event(rt, event);
        }

        match event {
            Event::Input(InputEvent::MouseMove { x, y }) => {
                apply_mouse_move(
                    &mut rt.desktop,
                    &mut rt.cursor_redraw_from,
                    &mut rt.frame_due,
                    *x,
                    *y,
                );
                true
            }
            Event::Input(InputEvent::MouseWheel { dy, .. }) => {
                let target = rt
                    .desktop
                    .wm
                    .window_at(rt.desktop.cursor.x, rt.desktop.cursor.y);
                if rt.term_window.is_some() && target == rt.term_window {
                    if *dy > 0 {
                        rt.term_buf.scroll_back((*dy as usize).min(8));
                    } else if *dy < 0 {
                        rt.term_buf
                            .scroll_forward((dy.unsigned_abs() as usize).min(8));
                    }
                    rt.term_dirty = true;
                    rt.frame_due = true;
                    return true;
                }
                if rt.explorer.as_ref().and_then(|explorer| explorer.window_id) == target {
                    let visible_rows = rt
                        .explorer
                        .as_ref()
                        .and_then(|explorer| explorer.window_id)
                        .and_then(|id| {
                            rt.desktop
                                .wm
                                .windows()
                                .iter()
                                .find(|window| window.id == id)
                                .map(|window| {
                                    crate::explorer::visible_file_rows(window.surface.height())
                                })
                        })
                        .unwrap_or(1);
                    if let Some(explorer) = rt.explorer.as_mut() {
                        explorer.scroll_by(-(*dy as isize), visible_rows);
                        rt.explorer_dirty = true;
                        rt.frame_due = true;
                    }
                    return true;
                }
                false
            }
            Event::Input(InputEvent::MouseDown(btn)) => {
                let cx = rt.desktop.cursor.x;
                let cy = rt.desktop.cursor.y;

                if *btn == MouseButton::Left
                    && let Some(index) = style_launcher_at(rt, cx, cy)
                    && route_style_launcher(rt, index)
                {
                    return true;
                }

                // Check desktop icon clicks (left button only)
                if *btn == MouseButton::Left {
                    if let Some(icon_idx) = rt.desktop.desktop_icons.hit_test(cx, cy) {
                        if let Some(icon) = rt.desktop.desktop_icons.icons.get(icon_idx) {
                            match lattice::desktop_icons::DesktopIconLayer::route(icon) {
                                lattice::common::AppRoute::Shell => {
                                    // Defer shell launch — cannot call
                                    // ensure_terminal_window() or launch_shell()
                                    // while holding the runtime-state lock (deadlock).
                                    rt.shell_launch_pending = true;
                                    rt.frame_due = true;
                                    return true;
                                }
                                lattice::common::AppRoute::Terminal => {
                                    crate::menu_actions::dispatch_menu_action(
                                        rt,
                                        &lattice::desktop::DesktopAction::NewTerminal,
                                    );
                                    rt.frame_due = true;
                                    return true;
                                }
                                lattice::common::AppRoute::Files => {
                                    crate::menu_actions::open_info_window(
                                        rt,
                                        crate::menu_actions::InfoWindow::FileManager,
                                    );
                                    rt.frame_due = true;
                                    return true;
                                }
                                lattice::common::AppRoute::Settings => {
                                    crate::menu_actions::open_settings_window(rt);
                                    rt.frame_due = true;
                                    return true;
                                }
                                lattice::common::AppRoute::About => {
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

                if *btn == MouseButton::Left
                    && rt.settings_window.is_some()
                    && rt.desktop.wm.window_at(cx, cy) == rt.settings_window
                    && crate::settings_bridge::settings_handle_mouse(rt, cx, cy)
                {
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

#[cfg(test)]
mod tests {
    use super::apply_mouse_move;
    use lattice::desktop::Desktop;

    #[test]
    fn mouse_input_transitions_cursor_state_and_queues_cursor_redraw() {
        let mut desktop = Desktop::new(0);
        let previous = (desktop.cursor.x, desktop.cursor.y);
        let mut cursor_redraw_from = None;
        let mut frame_due = false;

        apply_mouse_move(
            &mut desktop,
            &mut cursor_redraw_from,
            &mut frame_due,
            23,
            41,
        );

        assert_eq!((desktop.cursor.x, desktop.cursor.y), (23, 41));
        assert_eq!(cursor_redraw_from, Some(previous));
        // A plain pointer move is handled by the cursor-only renderer. A
        // full frame is reserved for window dragging or other scene changes.
        assert!(!frame_due);
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
    let rel_y = cy - window.y - lattice::style::title_bar_height() as i32;

    // If context menu is open, handle clicks on it first
    if explorer.context_menu.open {
        let launch_path = crate::explorer::handle_context_menu_click(explorer, rel_x, rel_y)
            .and_then(|action| explorer.dispatch_context_action(action));
        rt.explorer_dirty = true;
        rt.frame_due = true;
        if let Some(path) = launch_path {
            *PENDING_LAUNCH.lock() = Some(path);
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
                        // Save path to be launched later, outside the
                        // runtime lock, to avoid VFS deadlock.
                        *PENDING_LAUNCH.lock() = Some(path);
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
            let previous = (rt.desktop.cursor.x, rt.desktop.cursor.y);
            rt.desktop.mouse_move(*x, *y);
            rt.request_cursor_redraw(previous);
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

    for idx in 0..lattice::common::APP_GRID_ROUTES.len() {
        let Some((ax, ay, aw, ah)) = lattice::shell_overlay::app_grid_item_rect(idx, fw, _fh)
        else {
            continue;
        };
        if cx >= ax && cx < ax + aw as i32 && cy >= ay && cy < ay + ah as i32 {
            match lattice::common::app_grid_route(idx) {
                Some(lattice::common::AppRoute::Shell) => {
                    // Shell launches the interactive shell session.
                    rt.shell_launch_pending = true;
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                Some(lattice::common::AppRoute::Terminal) => {
                    crate::menu_actions::dispatch_menu_action(
                        rt,
                        &lattice::desktop::DesktopAction::NewTerminal,
                    );
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                Some(lattice::common::AppRoute::Editor) => {
                    // Editor
                    rt.editor_launch_pending = true;
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                Some(lattice::common::AppRoute::Clock) => {
                    // Clock — show system info
                    crate::menu_actions::open_info_window(
                        rt,
                        crate::menu_actions::InfoWindow::SystemInfo,
                    );
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                Some(lattice::common::AppRoute::Settings) => {
                    // Settings
                    crate::menu_actions::open_settings_window(rt);
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                Some(lattice::common::AppRoute::Files) => {
                    // File Manager
                    crate::menu_actions::open_info_window(
                        rt,
                        crate::menu_actions::InfoWindow::FileManager,
                    );
                    rt.shell_state = ShellState::Desktop;
                    rt.frame_due = true;
                    return true;
                }
                Some(lattice::common::AppRoute::About) => {
                    // About
                    crate::menu_actions::open_info_window(
                        rt,
                        crate::menu_actions::InfoWindow::About,
                    );
                    rt.shell_state = ShellState::Desktop;
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
                if let Some(ref mut rt) = *RUNTIME_CONTEXT.runtime() {
                    if !matches!(
                        rt.desktop.wm.windows().last().map(|w| w.id),
                        Some(id) if Some(id) == rt.term_window
                    ) {
                        return false;
                    }
                    rt.term_buf.scroll_back(1);
                    rt.term_dirty = true;
                    rt.frame_due = true;
                }
                true
            }
            Event::Input(InputEvent::KeyDown(KeyCode::PageDown)) => {
                if let Some(ref mut rt) = *RUNTIME_CONTEXT.runtime() {
                    if !matches!(
                        rt.desktop.wm.windows().last().map(|w| w.id),
                        Some(id) if Some(id) == rt.term_window
                    ) {
                        return false;
                    }
                    rt.term_buf.scroll_forward(1);
                    rt.term_dirty = true;
                    rt.frame_due = true;
                }
                true
            }
            Event::Input(InputEvent::KeyDown(KeyCode::Home)) => {
                if let Some(ref mut rt) = *RUNTIME_CONTEXT.runtime() {
                    if !matches!(
                        rt.desktop.wm.windows().last().map(|w| w.id),
                        Some(id) if Some(id) == rt.term_window
                    ) {
                        return false;
                    }
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
        let mut rt = RUNTIME_CONTEXT.runtime();
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
