//! PS/2 Mouse Driver
//!
//! Wraps the `ps2-mouse` crate to provide an ergonomic, no_std‑friendly
//! interface for the PS/2 mouse.  Initialization and packet processing are
//! handled by the underlying crate; this module exposes a static `MOUSE`
//! instance usable from an interrupt handler.

use ps2_mouse::{Mouse as Ps2MouseInner, MouseState as Ps2MouseState};
use spin::Mutex;

/// The global PS/2 mouse instance.
///
/// Initialise with [`init_mouse`] before enabling interrupts, then call
/// [`handle_mouse_data`] from the interrupt handler for each byte received
/// on the PS/2 data port (0x60).
pub static MOUSE: Mutex<Option<Ps2MouseInner>> = Mutex::new(None);

/// Static storage for the latest completed mouse state.
///
/// Updated atomically from the `on_complete` callback so the rest of the
/// kernel can poll it without holding the lock on `MOUSE`.
static LATEST_STATE: Mutex<Ps2MouseState> = Mutex::new(Ps2MouseState::new());

/// Raw status byte from the most recent completed mouse packet.
///
/// Bit 0 = left button, bit 1 = right button, bit 2 = middle button.
/// The `ps2-mouse` crate only exposes `left_button_down()` and
/// `right_button_down()` publicly, so we capture the raw status byte
/// here to obtain the middle button state.
static LATEST_STATUS: Mutex<u8> = Mutex::new(0);

/// Manually-tracked packet byte index (0, 1, 2) so we know when a new
/// packet starts.  The underlying ps2-mouse crate's field is private.
static PACKET_IDX: Mutex<u8> = Mutex::new(0);

/// Initialise the PS/2 mouse.
///
/// This sends the necessary commands to the PS/2 controller to enable the
/// mouse in streaming mode with default settings.  Must be called **once**
/// before any mouse interrupts are enabled.
///
/// # Errors
///
/// Returns an error string if any PS/2 controller command fails (e.g. the
/// mouse does not respond).
pub fn init_mouse() -> Result<(), &'static str> {
    let mut mouse = Ps2MouseInner::new();

    // Install the completion callback so LATEST_STATE is always up to date.
    mouse.set_on_complete(|state| {
        *LATEST_STATE.lock() = state;
    });

    log::info!("[nitrogen] PS/2 mouse: calling init()...");
    match mouse.init() {
        Ok(()) => {
            log::info!("[nitrogen] PS/2 mouse: init() succeeded, mouse now in streaming mode");
            *MOUSE.lock() = Some(mouse);
            Ok(())
        }
        Err(e) => {
            log::error!("[nitrogen] PS/2 mouse: init() FAILED: {}", e);
            Err(e)
        }
    }
}

/// Feed a byte from the PS/2 data port (0x60) to the mouse driver.
///
/// This should be called from the mouse interrupt handler for every byte
/// received.  Once three bytes have been accumulated into a complete packet,
/// the `on_complete` callback will fire and [`latest_state`] will return
/// the updated state.
///
/// The byte is also tracked for button state: each packet starts with a
/// status byte whose low 3 bits indicate left/right/middle button state.
pub fn handle_mouse_data(byte: u8) {
    if let Some(ref mut mouse) = *MOUSE.lock() {
        // Track the raw status byte (first byte of each 3-byte packet).
        // We maintain our own 0→1→2→0 index because the underlying
        // `current_packet` field on ps2-mouse::Mouse is private.
        let mut idx = PACKET_IDX.lock();
        if *idx == 0 {
            // First byte of a new packet → status byte.
            *LATEST_STATUS.lock() = byte & 0x07;
        }
        *idx = (*idx + 1) % 3;
        drop(idx);

        mouse.process_packet(byte);
    }
}

/// Return the most recently completed mouse state.
///
/// The state includes button flags and the accumulated X/Y delta for the
/// latest packet.  **Does NOT reset** the internal deltas — call
/// [`consume_state`] instead if you need to drain the accumulator.
pub fn latest_state() -> Ps2MouseState {
    *LATEST_STATE.lock()
}

/// Return the most recently completed mouse state **and reset** the
/// internal delta accumulators to zero.
///
/// This is the preferred function for polling loops: it prevents the
/// same packet delta from being applied multiple times, and avoids
/// losing deltas from intermediate packets when multiple packets are
/// completed between polls.
///
/// Interrupts are disabled during the read‑modify‑write to avoid a
/// deadlock between this function and the `on_complete` callback that
/// the PS/2 interrupt handler invokes (both try to lock `LATEST_STATE`).
pub fn consume_state() -> Ps2MouseState {
    let interrupts_enabled = x86_64::instructions::interrupts::are_enabled();
    if interrupts_enabled {
        x86_64::instructions::interrupts::disable();
    }
    let mut state = LATEST_STATE.lock();
    let out = *state;
    *state = Ps2MouseState::new();
    drop(state);
    if interrupts_enabled {
        x86_64::instructions::interrupts::enable();
    }
    out
}

/// Get the current mouse button flags as a raw byte.
///
/// Bit 0 = left, bit 1 = right, bit 2 = middle.
/// The value is extracted from the raw PS/2 status byte of the most
/// recently completed packet.
pub fn mouse_buttons() -> u8 {
    *LATEST_STATUS.lock()
}
