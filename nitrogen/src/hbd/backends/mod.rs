//! Hardware-specific observation adapters.

#[cfg(not(nitrogen_no_hda))]
pub mod hda;
#[cfg(not(nitrogen_no_iwlwifi))]
pub mod iwlwifi;
#[cfg(not(nitrogen_no_usb))]
pub mod xhci;
