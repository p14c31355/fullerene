//! PS/2 input polling and translation into desktop or Resonance events.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use lattice::desktop::DesktopAction;
use resonance::{Event, InputEvent, MouseButton};
use spin::Mutex;

use alloc::string::String;

use crate::{
    FB_DIMS, MOUSE_SENSITIVITY, PREV_MOUSE_BUTTONS, RUNTIME_CONTEXT, RuntimeState, editor_bridge,
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

// Relative PS/2 packets can accumulate while a service performs bounded but
// comparatively long hardware I/O (notably Wi-Fi firmware/MMIO work). Do not
// turn that backlog into a single teleport across the desktop when polling
// resumes. The rest is intentionally discarded: it is stale motion, not a
// new pointer position that the user is still trying to reach.
const MAX_MOUSE_STEP_PX: i32 = 96;
const MOUSE_STALE_AFTER_MS: u64 = 50;
static LAST_MOUSE_POLL_TSC: AtomicU64 = AtomicU64::new(0);
static VIDEO_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

fn map_touch_axis(value: i32, minimum: i32, maximum: i32, pixels: u32) -> i16 {
    if pixels == 0 || maximum <= minimum {
        return 0;
    }
    let value = value.clamp(minimum, maximum) - minimum;
    let range = i64::from(maximum - minimum);
    (i64::from(value) * i64::from(pixels.saturating_sub(1)) / range)
        .clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

/// Consume an Escape press observed by the low-level keyboard poller.
///
/// The synchronous WASM viewer cannot receive the normal desktop event
/// dispatch while it is presenting a frame, so video playback uses this
/// small out-of-band request instead.
pub fn take_video_stop_request() -> bool {
    VIDEO_STOP_REQUESTED.swap(false, Ordering::AcqRel)
}

/// Discard an Escape request left over from a previous input epoch before a
/// new synchronous video playback starts.
pub fn clear_video_stop_request() {
    VIDEO_STOP_REQUESTED.store(false, Ordering::Release);
}

fn scaled_mouse_delta(delta: i16, sensitivity: i16) -> i32 {
    (i32::from(delta) * i32::from(sensitivity)).clamp(-MAX_MOUSE_STEP_PX, MAX_MOUSE_STEP_PX)
}

fn mouse_motion_is_stale(previous_poll: u64, now_tsc: u64, tsc_per_ms: u64) -> bool {
    previous_poll != 0
        && tsc_per_ms != 0
        && now_tsc.wrapping_sub(previous_poll) > tsc_per_ms.saturating_mul(MOUSE_STALE_AFTER_MS)
}

fn push_mouse_button_edges(queue: &mut resonance::EventQueue, buttons: u8, previous: u8) {
    for (mask, button) in [
        (0x01, MouseButton::Left),
        (0x02, MouseButton::Right),
        (0x04, MouseButton::Middle),
    ] {
        match ((buttons & mask) != 0, (previous & mask) != 0) {
            (true, false) => queue.push(Event::Input(InputEvent::MouseDown(button))),
            (false, true) => queue.push(Event::Input(InputEvent::MouseUp(button))),
            _ => {}
        }
    }
}

/// Merge physical HID buttons with the digitizer's Tip Switch. A precision
/// touchpad reports a tap/drag as contact state rather than as a separate
/// button field; exposing that state as left-button edges gives the desktop
/// normal click and drag semantics.
fn touchpad_button_bits(input: Option<&nitrogen::i2c_hid::TouchpadInput>) -> u8 {
    let Some(input) = input else { return 0 };
    let mut buttons = input.report.buttons & 0x03;
    if input.relative.is_none() && input.report.in_contact {
        buttons |= 0x01;
    }
    buttons
}

pub fn poll_mouse_state() {
    nitrogen::i2c_hid::poll_input();
    let touchpad = nitrogen::i2c_hid::consume_input();
    let touchpad_relative = touchpad.as_ref().and_then(|input| input.relative);
    let touchpad_absolute = touchpad
        .as_ref()
        .filter(|input| input.relative.is_none() && input.report.in_contact);
    // IRQ12 is the normal delivery path. Drain AUX bytes as a fallback for
    // QEMU/firmware configurations where the legacy mouse route is not wired
    // through the I/O APIC.
    nitrogen::ps2::mouse::poll_input();
    let ps2_state = nitrogen::ps2::mouse::consume_state();
    let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let previous_poll = LAST_MOUSE_POLL_TSC.swap(now_tsc, Ordering::Relaxed);
    let tsc_per_ms = crate::TSC_PER_MS.load(Ordering::Relaxed);
    let stale = mouse_motion_is_stale(previous_poll, now_tsc, tsc_per_ms);
    // A long gap means the relative packets describe motion that happened
    // while the desktop was blocked (for example during Wi-Fi firmware I/O),
    // not a reliable current pointer position. Consume and discard it.
    let dx = if stale { 0 } else { ps2_state.get_x() };
    let dy = if stale { 0 } else { ps2_state.get_y() };
    let wheel = ps2_state.get_wheel();
    let buttons = nitrogen::ps2::mouse::mouse_buttons();
    let mut mouse = MOUSE_STATE.lock();
    let old_x = mouse.x;
    let old_y = mouse.y;
    let sensitivity = MOUSE_SENSITIVITY.load(core::sync::atomic::Ordering::Relaxed);
    let (fb_width, fb_height, _) = *FB_DIMS.lock();
    let next_x = i32::from(mouse.x) + scaled_mouse_delta(dx, sensitivity);
    let next_y = i32::from(mouse.y) - scaled_mouse_delta(dy, sensitivity);
    if let Some((dx, dy)) = touchpad_relative {
        mouse.x = (next_x + scaled_mouse_delta(dx, sensitivity))
            .clamp(0, fb_width.saturating_sub(1) as i32) as i16;
        // HID relative mouse Y is positive downward. PS/2 uses the opposite
        // convention and is handled by the subtraction above.
        mouse.y = (next_y + scaled_mouse_delta(dy, sensitivity))
            .clamp(0, fb_height.saturating_sub(1) as i32) as i16;
    } else {
        mouse.x = if fb_width == 0 {
            next_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16
        } else {
            next_x.clamp(0, fb_width.saturating_sub(1) as i32) as i16
        };
        mouse.y = if fb_height == 0 {
            next_y.clamp(i16::MIN as i32, i16::MAX as i32) as i16
        } else {
            next_y.clamp(0, fb_height.saturating_sub(1) as i32) as i16
        };
    }
    // HID-over-I2C reports absolute coordinates.  Update the desktop only
    // while a finger is down; release reports must not snap the pointer back
    // to the controller's last coordinate.
    if let Some(touchpad) = touchpad_absolute {
        mouse.x = map_touch_axis(touchpad.report.x, touchpad.x_min, touchpad.x_max, fb_width);
        mouse.y = map_touch_axis(touchpad.report.y, touchpad.y_min, touchpad.y_max, fb_height);
    }
    let combined_buttons = (buttons & !0x03) | touchpad_button_bits(touchpad.as_ref());
    mouse.buttons = combined_buttons;
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
    if combined_buttons != previous
        && let Some(queue) = RUNTIME_CONTEXT.event_queue().as_mut()
    {
        push_mouse_button_edges(queue, combined_buttons, previous);
    }
    *previous_buttons = combined_buttons;
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_MOUSE_STEP_PX, MOUSE_STALE_AFTER_MS, map_touch_axis, mouse_motion_is_stale,
        scaled_mouse_delta, touchpad_button_bits,
    };

    #[test]
    fn caps_stale_accumulated_motion() {
        assert_eq!(scaled_mouse_delta(127, 6), MAX_MOUSE_STEP_PX);
        assert_eq!(scaled_mouse_delta(-127, 6), -MAX_MOUSE_STEP_PX);
        assert_eq!(scaled_mouse_delta(4, 6), 24);
    }

    #[test]
    fn discards_motion_only_after_a_real_poll_gap() {
        let tsc_per_ms = 1_000;
        assert!(!mouse_motion_is_stale(0, 100_000, tsc_per_ms));
        assert!(!mouse_motion_is_stale(
            100_000,
            100_000 + tsc_per_ms * MOUSE_STALE_AFTER_MS,
            tsc_per_ms,
        ));
        assert!(mouse_motion_is_stale(
            100_000,
            100_000 + tsc_per_ms * (MOUSE_STALE_AFTER_MS + 1),
            tsc_per_ms,
        ));
    }

    #[test]
    fn maps_n150_absolute_touch_coordinates_to_framebuffer_edges() {
        assert_eq!(map_touch_axis(0, 0, 1708, 1920), 0);
        assert_eq!(map_touch_axis(1708, 0, 1708, 1920), 1919);
        assert_eq!(map_touch_axis(1060 / 2, 0, 1060, 1080), 539);
        assert_eq!(map_touch_axis(-1, 0, 1708, 1920), 0);
        assert_eq!(map_touch_axis(1709, 0, 1708, 1920), 1919);
        assert_eq!(map_touch_axis(10, 10, 10, 1920), 0);
        assert_eq!(map_touch_axis(10, 0, 10, 0), 0);
    }

    #[test]
    fn maps_digitizer_contact_to_left_button_edges() {
        let pressed = nitrogen::i2c_hid::TouchpadInput {
            report: nitrogen::hid::TouchpadReport {
                x: 100,
                y: 200,
                buttons: 0,
                in_contact: true,
            },
            x_min: 0,
            x_max: 1708,
            y_min: 0,
            y_max: 1060,
            relative: None,
        };
        let released = nitrogen::i2c_hid::TouchpadInput {
            report: nitrogen::hid::TouchpadReport {
                in_contact: false,
                ..pressed.report
            },
            ..pressed
        };
        assert_eq!(touchpad_button_bits(Some(&pressed)), 0x01);
        assert_eq!(touchpad_button_bits(Some(&released)), 0);
    }
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
    }

    while nitrogen::ps2::keyboard::raw_key_available() {
        let event = match nitrogen::ps2::keyboard::pop_raw_key() {
            Some(event) => event,
            None => break,
        };
        let scancode = event.scancode;
        let pressed = event.pressed;
        let key = scancode_to_resonance_keycode(scancode);
        if pressed && key == resonance::KeyCode::Escape {
            VIDEO_STOP_REQUESTED.store(true, Ordering::Release);
        }

        // Fn is usually consumed by the keyboard firmware and is not visible
        // as a PS/2 modifier. The resulting E0 37 Print Screen event is the
        // stable signal for Fn+PrtSc on laptops, so handle it before normal
        // focused-window routing.
        if key == resonance::KeyCode::PrintScreen {
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
            key,
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
                    let ctrl_v = pressed
                        && key == resonance::KeyCode::V
                        && (event.modifiers.lctrl || event.modifiers.rctrl);
                    let sequence = match key {
                        resonance::KeyCode::Up if pressed => Some(b"\x1b[A".as_slice()),
                        resonance::KeyCode::Down if pressed => Some(b"\x1b[B".as_slice()),
                        _ => None,
                    };
                    drop(runtime_guard);
                    if ctrl_v && let Some(path) = crate::explorer::shell_clipboard_path() {
                        let replaced =
                            nitrogen::ps2::keyboard::replace_input_byte(0x16, path.as_bytes());
                        if replaced {
                            while let Some(byte) =
                                nitrogen::ps2::keyboard::pop_input_char_unchecked()
                            {
                                crate::terminal::push_process_terminal_input(
                                    process_terminal_id,
                                    &[byte],
                                );
                            }
                            continue;
                        }
                    }
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
                    if key == resonance::KeyCode::V
                        && (event.modifiers.lctrl || event.modifiers.rctrl)
                        && let Some(path) = crate::explorer::shell_clipboard_path()
                    {
                        // The low-level keyboard translator has already
                        // queued Ctrl+V as 0x16. Consume that control byte
                        // and inject the copied absolute path instead.
                        if nitrogen::ps2::keyboard::replace_input_byte(0x16, path.as_bytes()) {
                            continue;
                        }
                    }
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

    // Key repeat is produced by the low-level driver without a new raw queue
    // entry. Drain those decoded bytes after event-time Ctrl+V handling.
    let focused_process_terminal = RUNTIME_CONTEXT.runtime().as_ref().and_then(|runtime| {
        let top_id = runtime.desktop.wm.windows().last().map(|window| window.id);
        runtime
            .process_terminals
            .iter()
            .find(|terminal| Some(terminal.window_id) == top_id)
            .map(|terminal| terminal.window_id)
    });
    if let Some(window_id) = focused_process_terminal {
        while let Some(byte) = nitrogen::ps2::keyboard::pop_input_char_unchecked() {
            crate::terminal::push_process_terminal_input(window_id, &[byte]);
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
