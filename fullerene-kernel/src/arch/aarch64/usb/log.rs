//! UART logging helpers for the USB handoff path.

#[inline]
pub(super) fn log_puts(message: &str) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    super::super::uart::puts(message);
}

#[inline]
pub(super) fn log_hex(prefix: &str, value: u64) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    super::super::uart::put_hex(prefix, value);
}

#[inline]
pub(super) fn log_hex_value(value: u64) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    super::super::uart::put_hex_value(value);
}
