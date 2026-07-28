//! PS/2 input polling and translation into desktop or Resonance events.

use lattice::desktop::DesktopAction;
use resonance::{Event, InputEvent, MouseButton};
use spin::Mutex;

use alloc::string::String;

use crate::{
    MOUSE_SENSITIVITY, PREV_MOUSE_BUTTONS, RUNTIME_CONTEXT, RuntimeState, editor_bridge,
    network_manager, settings_bridge,
};

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
    // IRQ12 is the normal delivery path. Drain AUX bytes as a fallback for
    // QEMU/firmware configurations where the legacy mouse route is not wired
    // through the I/O APIC.
    nitrogen::ps2::mouse::poll_input();
    let ps2_state = nitrogen::ps2::mouse::consume_state();
    let dx = ps2_state.get_x();
    let dy = ps2_state.get_y();
    let wheel = ps2_state.get_wheel();
    let buttons = nitrogen::ps2::mouse::mouse_buttons();
    let mut mouse = MOUSE_STATE.lock();
    let old_x = mouse.x;
    let old_y = mouse.y;
    let sensitivity = MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed);
    mouse.x = mouse.x.wrapping_add(dx.wrapping_mul(sensitivity));
    mouse.y = mouse
        .y
        .wrapping_add(dy.wrapping_mul(sensitivity).wrapping_neg());
    mouse.buttons = buttons;
    let cursor_x = mouse.x as i32;
    let cursor_y = mouse.y as i32;
    let moved = old_x != mouse.x || old_y != mouse.y;
    drop(mouse);

    if moved && let Some(queue) = RUNTIME_CONTEXT.event_queue().as_mut() {
        queue.push(Event::Input(InputEvent::MouseMove {
            x: cursor_x,
            y: cursor_y,
        }));
    }
    if wheel != 0
        && let Some(queue) = RUNTIME_CONTEXT.event_queue().as_mut()
    {
        queue.push(Event::Input(InputEvent::MouseWheel {
            dx: 0,
            // PS/2 reports positive wheel ticks for the physical upward
            // direction. Resonance uses the usual screen convention where
            // positive Y means scrolling down.
            dy: -i32::from(wheel),
        }));
    }

    let mut previous_buttons = PREV_MOUSE_BUTTONS.lock();
    let previous = *previous_buttons;
    if buttons != previous
        && let Some(queue) = RUNTIME_CONTEXT.event_queue().as_mut()
    {
        mouse_edge!(queue, buttons, previous, 0x01, Left);
        mouse_edge!(queue, buttons, previous, 0x02, Right);
        mouse_edge!(queue, buttons, previous, 0x04, Middle);
    }
    *previous_buttons = buttons;
}

pub fn poll_keyboard() {
    // Gate terminal input: only deliver ASCII keystrokes to shell/stdin when
    // the terminal window is the focused (topmost) window.
    {
        use nitrogen::ps2::keyboard::set_terminal_input_allowed;
        let runtime_guard = crate::RUNTIME_CONTEXT.runtime();
        let focused_process_terminal = runtime_guard.as_ref().and_then(|rt| {
            let top = rt.desktop.wm.windows().last().map(|w| w.id);
            rt.process_terminals
                .iter()
                .find(|terminal| Some(terminal.window_id) == top)
                .map(|terminal| terminal.window_id)
        });
        let allowed = runtime_guard.as_ref().map_or(true, |rt| {
            let top = rt.desktop.wm.windows().last().map(|w| w.id);
            focused_process_terminal.is_none()
                && rt.term_window.is_some()
                && top == rt.term_window
                && !rt.desktop.pwd_dialog_open
        });
        drop(runtime_guard);
        if focused_process_terminal.is_some() {
            use nitrogen::ps2::keyboard::set_terminal_input_allowed_preserve;
            set_terminal_input_allowed_preserve(false);
        } else {
            set_terminal_input_allowed(allowed);
        }
        // Key repeat is produced by the low-level driver without a new raw
        // queue entry. Drain those decoded bytes into the focused process
        // terminal before the normal raw-key loop handles fresh events.
        if let Some(window_id) = focused_process_terminal {
            while let Some(byte) = nitrogen::ps2::keyboard::pop_input_char_unchecked() {
                crate::terminal::push_process_terminal_input(window_id, &[byte]);
            }
        }
    }

    while nitrogen::ps2::keyboard::raw_key_available() {
        let (scancode, pressed) = match nitrogen::ps2::keyboard::pop_raw_key() {
            Some(key) => key,
            None => break,
        };

        // Fn is usually consumed by the keyboard firmware and is not visible
        // as a PS/2 modifier. The resulting E0 37 Print Screen event is the
        // stable signal for Fn+PrtSc on laptops, so handle it before normal
        // focused-window routing.
        if scancode_to_resonance_keycode(scancode) == resonance::KeyCode::PrintScreen {
            if pressed {
                if let Some(run_wasm) = RUNTIME_CONTEXT.callback_snapshot().run_wasm {
                    let args = ["/apps/emulsion.wasm", "capture"];
                    let _ = run_wasm("/apps/emulsion.wasm", &args);
                }
            }
            // Swallow both halves of the Print Screen sequence so the key-up
            // event cannot leak into the focused application.
            continue;
        }

        // Super is a desktop-global shortcut. It must bypass focused-window
        // routing (Settings, Editor, Explorer, and Terminal) so the shell
        // state machine always receives both key-down and key-up events.
        if matches!(
            scancode_to_resonance_keycode(scancode),
            resonance::KeyCode::SuperLeft | resonance::KeyCode::SuperRight
        ) {
            push_keyboard_event(scancode, pressed);
            continue;
        }

        let mut launch_path: Option<String> = None;
        let mut explorer_handled = false;
        {
            let mut runtime_guard = RUNTIME_CONTEXT.runtime();
            if let Some(runtime) = runtime_guard.as_mut() {
                if runtime.desktop.pwd_dialog_open {
                    handle_password_dialog_key(runtime, scancode, pressed);
                    continue;
                }

                let top_id = runtime.desktop.wm.windows().last().map(|window| window.id);
                let process_terminal_id = runtime
                    .process_terminals
                    .iter()
                    .find(|terminal| Some(terminal.window_id) == top_id)
                    .map(|terminal| terminal.window_id);
                if let Some(process_terminal_id) = process_terminal_id {
                    let key = scancode_to_resonance_keycode(scancode);
                    let sequence = match key {
                        resonance::KeyCode::Up if pressed => Some(b"\x1b[A".as_slice()),
                        resonance::KeyCode::Down if pressed => Some(b"\x1b[B".as_slice()),
                        _ => None,
                    };
                    drop(runtime_guard);
                    if let Some(sequence) = sequence {
                        crate::terminal::push_process_terminal_input(process_terminal_id, sequence);
                    } else if pressed
                        && let Some(byte) = nitrogen::ps2::keyboard::pop_input_char_unchecked()
                    {
                        crate::terminal::push_process_terminal_input(process_terminal_id, &[byte]);
                    }
                    continue;
                }
                if runtime.term_window.is_some() && top_id == runtime.term_window && pressed {
                    let key = scancode_to_resonance_keycode(scancode);
                    let sequence = match key {
                        resonance::KeyCode::Up => Some(b"\x1b[A".as_slice()),
                        resonance::KeyCode::Down => Some(b"\x1b[B".as_slice()),
                        _ => None,
                    };
                    if let Some(sequence) = sequence {
                        nitrogen::ps2::keyboard::push_input_bytes(sequence);
                        continue;
                    }
                }
                if top_id.is_some() && runtime.editor_window == top_id {
                    drop(runtime_guard);
                    editor_bridge::editor_handle_key(scancode, pressed);
                    push_keyboard_event(scancode, pressed);
                    continue;
                }
                if top_id.is_some() && runtime.settings_window == top_id {
                    settings_bridge::settings_handle_key_inner(runtime, scancode, pressed);
                    continue;
                }
                if top_id.is_some()
                    && runtime
                        .explorer
                        .as_ref()
                        .and_then(|explorer| explorer.window_id)
                        == top_id
                {
                    // Capture the launch path from Enter key, then drop the
                    // runtime lock BEFORE calling launch_file (which does VFS
                    // I/O that would deadlock if the lock were held).
                    launch_path = explorer_handle_key(runtime, scancode, pressed);
                    explorer_handled = true;
                    // Fall through to keyboard event push UNLESS we have a
                    // launch path (handled below).
                }
            }
            if !explorer_handled {
                drop(runtime_guard);
                push_keyboard_event(scancode, pressed);
            }
        }
        // VFS-backed file launch must happen outside the runtime lock.
        if let Some(path) = launch_path {
            *crate::window_api::PENDING_LAUNCH.lock() = Some(path);
        }
    }
}

fn push_keyboard_event(scancode: u8, pressed: bool) {
    let key = scancode_to_resonance_keycode(scancode);
    let event = if pressed {
        Event::Input(InputEvent::KeyDown(key))
    } else {
        Event::Input(InputEvent::KeyUp(key))
    };
    if let Some(queue) = RUNTIME_CONTEXT.event_queue().as_mut() {
        queue.push(event);
    }
}

pub(crate) fn scancode_to_resonance_keycode(scancode: u8) -> resonance::KeyCode {
    resonance::scancode::from_scancode(scancode)
}

fn handle_password_dialog_key(runtime: &mut RuntimeState, scancode: u8, pressed: bool) {
    let action = match scancode {
        0x1C => {
            if !pressed {
                return;
            }
            DesktopAction::SubmitPassword
        }
        0x01 => {
            if !pressed {
                return;
            }
            DesktopAction::DismissPasswordDialog
        }
        0x0E => {
            if !pressed {
                return;
            }
            DesktopAction::PasswordBackspace
        }
        0x2A | 0x36 => {
            runtime.desktop.shift_held = pressed;
            return;
        }
        _ => {
            if !pressed {
                return;
            }
            let mut character = scancode_to_ascii(scancode);
            if character == 0 {
                return;
            }
            if runtime.desktop.shift_held {
                character = crate::explorer::shifted_ascii(character);
            }
            DesktopAction::PasswordChar(character)
        }
    };
    let _ = network_manager::handle_network_action(runtime, &action);
    runtime.frame_due = true;
}

pub(crate) fn scancode_to_ascii(scancode: u8) -> u8 {
    match scancode {
        0x10 => b'q',
        0x11 => b'w',
        0x12 => b'e',
        0x13 => b'r',
        0x14 => b't',
        0x15 => b'y',
        0x16 => b'u',
        0x17 => b'i',
        0x18 => b'o',
        0x19 => b'p',
        0x1E => b'a',
        0x1F => b's',
        0x20 => b'd',
        0x21 => b'f',
        0x22 => b'g',
        0x23 => b'h',
        0x24 => b'j',
        0x25 => b'k',
        0x26 => b'l',
        0x2C => b'z',
        0x2D => b'x',
        0x2E => b'c',
        0x2F => b'v',
        0x30 => b'b',
        0x31 => b'n',
        0x32 => b'm',
        0x02 => b'1',
        0x03 => b'2',
        0x04 => b'3',
        0x05 => b'4',
        0x06 => b'5',
        0x07 => b'6',
        0x08 => b'7',
        0x09 => b'8',
        0x0A => b'9',
        0x0B => b'0',
        0x2B => b'\\',
        0x0C => b'-',
        0x0D => b'=',
        0x1A => b'[',
        0x1B => b']',
        0x27 => b';',
        0x28 => b'\'',
        0x29 => b'`',
        0x33 => b',',
        0x34 => b'.',
        0x35 => b'/',
        0x39 => b' ',
        _ => 0,
    }
}

/// Returns the path to launch (if Enter was pressed on a file),
/// or `None` for normal navigation keys.
fn explorer_handle_key(runtime: &mut RuntimeState, scancode: u8, pressed: bool) -> Option<String> {
    if let Some(explorer) = runtime.explorer.as_mut()
        && explorer.handle_operation_key(scancode, pressed)
    {
        runtime.explorer_dirty = true;
        runtime.frame_due = true;
        return None;
    }
    if !pressed {
        return None;
    }

    let key = scancode_to_resonance_keycode(scancode);
    let surface_height = runtime
        .explorer
        .as_ref()
        .and_then(|explorer| explorer.window_id)
        .and_then(|id| {
            runtime
                .desktop
                .wm
                .windows()
                .iter()
                .find(|window| window.id == id)
                .map(|window| window.surface.height())
        })
        .unwrap_or(400);
    let visible_rows = crate::explorer::visible_file_rows(surface_height);
    match key {
        resonance::KeyCode::PageUp => {
            if let Some(explorer) = runtime.explorer.as_mut() {
                explorer.scroll_by(-(visible_rows as isize), visible_rows);
                runtime.explorer_dirty = true;
                runtime.frame_due = true;
            }
            None
        }
        resonance::KeyCode::PageDown => {
            if let Some(explorer) = runtime.explorer.as_mut() {
                explorer.scroll_by(visible_rows as isize, visible_rows);
                runtime.explorer_dirty = true;
                runtime.frame_due = true;
            }
            None
        }
        resonance::KeyCode::Home => {
            if let Some(explorer) = runtime.explorer.as_mut() {
                explorer.scroll_offset = 0;
                explorer.selected_index = None;
                runtime.explorer_dirty = true;
                runtime.frame_due = true;
            }
            None
        }
        resonance::KeyCode::End => {
            if let Some(explorer) = runtime.explorer.as_mut() {
                explorer.scroll_by(isize::MAX, visible_rows);
                explorer.selected_index = None;
                runtime.explorer_dirty = true;
                runtime.frame_due = true;
            }
            None
        }
        resonance::KeyCode::Up => {
            let explorer = match runtime.explorer.as_mut() {
                Some(explorer) => explorer,
                None => return None,
            };
            let entry_count = explorer.entries.len();
            if entry_count == 0 {
                return None;
            }
            let index = explorer
                .selected_index
                .unwrap_or(entry_count.saturating_sub(1));
            explorer.selected_index = if index == 0 {
                Some(entry_count.saturating_sub(1))
            } else {
                Some(index - 1)
            };
            if let Some(selected) = explorer.selected_index
                && selected < explorer.scroll_offset
            {
                explorer.scroll_offset = selected;
            }
            runtime.explorer_dirty = true;
            runtime.frame_due = true;
            None
        }
        resonance::KeyCode::Down => {
            let explorer = match runtime.explorer.as_mut() {
                Some(explorer) => explorer,
                None => return None,
            };
            let entry_count = explorer.entries.len();
            if entry_count == 0 {
                return None;
            }
            let index = explorer.selected_index.unwrap_or(0);
            explorer.selected_index = if index + 1 >= entry_count {
                Some(0)
            } else {
                Some(index + 1)
            };
            if let Some(selected) = explorer.selected_index
                && selected >= explorer.scroll_offset + visible_rows
            {
                explorer.scroll_offset = selected.saturating_sub(visible_rows - 1);
            }
            runtime.explorer_dirty = true;
            runtime.frame_due = true;
            None
        }
        resonance::KeyCode::Enter => {
            let explorer = match runtime.explorer.as_mut() {
                Some(explorer) => explorer,
                None => return None,
            };
            let path = explorer
                .selected_index
                .and_then(|idx| explorer.activate_entry(idx));
            runtime.explorer_dirty = true;
            runtime.frame_due = true;
            path
        }
        _ => None,
    }
}
