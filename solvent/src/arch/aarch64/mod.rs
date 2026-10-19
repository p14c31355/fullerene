//! aarch64 (ANE) runtime pieces.
//!
//! The phone has no timestamp counter equivalent wired into the runtime yet,
//! so time is driven by the kernel-side desktop timer: the driver bumps
//! [`ANE_TICKS`] on every tick and the frame scheduler reads it here. The
//! input stack stays on the x86 side until the ANE input bring-up lands, so
//! its pump hooks and scancode mappings exist here as inert stubs and the
//! shared desktop callers compile unchanged.

use core::sync::atomic::{AtomicU64, Ordering};

/// Tick counter advanced by the kernel's desktop timer driver.
pub static ANE_TICKS: AtomicU64 = AtomicU64::new(0);

/// Monotonic timestamp source: the ANE tick counter.
#[inline]
pub fn now_tsc() -> u64 {
    ANE_TICKS.load(Ordering::Relaxed)
}

// ── Input pump hooks (no keyboard/mouse stack on the ANE yet) ─────────

/// No keyboard to pump.
pub fn poll_keyboard() {}

/// Touch samples are drained into the event queue; the real implementation
/// lives at the bottom of this module next to the sample storage.

/// No cursor-paint instrumentation.
pub fn record_cursor_paint() {}

/// No PS/2 scancodes on the ANE yet.
pub fn scancode_to_ascii(_scancode: u8) -> u8 {
    0
}

/// No PS/2 scancodes on the ANE yet.
pub fn scancode_to_resonance_keycode(_scancode: u8) -> resonance::KeyCode {
    resonance::KeyCode::Escape
}

/// No Wi-Fi UI on the ANE yet.
pub fn handle_network_action(
    _rt: &mut crate::RuntimeState,
    _action: &lattice::desktop::DesktopAction,
) {
}

// ── Touch input ─────────────────────────────────────────────────────
//
// The phone has neither a PS/2 port nor an I2C-HID touchpad: its digitizer
// sits on a DesignWare I2C controller that the kernel driver polls. The
// boundary here is therefore a published sample, not a queue - the writer
// may be the timer IRQ, so the transfer is three relaxed stores and a
// release flag, and the reader never takes a lock.
//
// A touch sample is in *panel* coordinates. The shell lays out in the
// unrotated canvas, so the sample is carried back through the same
// rotation the scan-out applied (see `crate::rotate`) before it becomes a
// pointer position. Reporting panel coordinates straight to the shell is
// the "pointer lands somewhere else than the finger" class of bug.

use core::sync::atomic::AtomicU32;
use resonance::{Event, InputEvent};

const TOUCH_VALID: u32 = 1 << 0;
const TOUCH_DOWN: u32 = 1 << 1;

static TOUCH_X: AtomicU32 = AtomicU32::new(0);
static TOUCH_Y: AtomicU32 = AtomicU32::new(0);
static TOUCH_FLAGS: AtomicU32 = AtomicU32::new(0);

/// Last pointer position pushed into the event queue, as `x << 16 | y`.
/// `u32::MAX` means "no position yet".
static PREV_POSITION: AtomicU32 = AtomicU32::new(u32::MAX);
static PREV_DOWN: AtomicU32 = AtomicU32::new(0);

/// Publish one touch sample, in panel pixels. Called from the kernel's
/// touch poller; a release (`down == false`) keeps the last position.
pub fn set_touch(x: u16, y: u16, down: bool) {
    TOUCH_X.store(x as u32, Ordering::Relaxed);
    TOUCH_Y.store(y as u32, Ordering::Relaxed);
    TOUCH_FLAGS.store(
        TOUCH_VALID | if down { TOUCH_DOWN } else { 0 },
        Ordering::Release,
    );
}

/// The panel lost the contact entirely (finger lifted and the controller
/// stopped reporting). Equivalent to a released sample at the last point.
pub fn release_touch() {
    TOUCH_FLAGS.store(TOUCH_VALID, Ordering::Release);
}

/// Carry a panel point back into the canvas the shell laid out in.
///
/// With a portrait layout the two frames are the same size and the map is
/// the identity. With the landscape layout the canvas is the panel turned
/// a quarter, so the inverse rotation applies.
fn panel_to_canvas(x: u32, y: u32) -> (i32, i32) {
    let (canvas_w, canvas_h, _stride) = *crate::FB_DIMS.lock();
    let (panel_w, panel_h) = *crate::PANEL_DIMS.lock();
    let (sx, sy) = crate::rotate::touch_to_canvas(x, y, panel_w, panel_h, canvas_w, canvas_h);
    (sx as i32, sy as i32)
}

/// Drain the published touch sample into the desktop event queue.
pub fn poll_mouse_state() {
    let flags = TOUCH_FLAGS.load(Ordering::Acquire);
    if flags & TOUCH_VALID == 0 {
        return;
    }
    let (x, y) = panel_to_canvas(
        TOUCH_X.load(Ordering::Relaxed),
        TOUCH_Y.load(Ordering::Relaxed),
    );
    let down = flags & TOUCH_DOWN != 0;
    let position = ((x as u32 & 0xffff) << 16) | (y as u32 & 0xffff);
    let moved = PREV_POSITION.swap(position, Ordering::Relaxed) != position;
    let was_down = PREV_DOWN.swap(down as u32, Ordering::Relaxed) != 0;
    let push = |event: Event| {
        if let Some(queue) = crate::RUNTIME_CONTEXT.event_queue().as_mut() {
            queue.push(event);
        }
    };
    // A released sample carries no meaningful position: the controller
    // keeps reporting the last touch point, and following it would drag
    // the pointer on lift-off.
    if down && moved {
        push(Event::Input(InputEvent::MouseMove { x, y }));
    }
    match (down, was_down) {
        (true, false) => push(Event::Input(InputEvent::MouseDown(
            resonance::MouseButton::Left,
        ))),
        (false, true) => push(Event::Input(InputEvent::MouseUp(
            resonance::MouseButton::Left,
        ))),
        _ => {}
    }
}
