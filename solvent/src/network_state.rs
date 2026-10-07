//! Network snapshot shared between the tick loop and the desktop services.
//!
//! The x86 tree fills it from the Wi-Fi stack; the ANE keeps the same shape
//! with no device behind it, so the tick loop and the top panel compile on
//! both sides. Only `lattice` types are involved, so this file is portable.

extern crate alloc;

use alloc::vec::Vec;
use spin::Mutex;

/// Latest scan/connection snapshot shown by the desktop.
pub struct NetworkSnapshot {
    pub aps: Vec<lattice::network_menu::ApDisplay>,
    pub status: lattice::network_menu::NetStatus,
}

/// Shared snapshot cell; default state is "no device".
pub static NETWORK_SNAPSHOT: Mutex<NetworkSnapshot> = Mutex::new(NetworkSnapshot {
    aps: Vec::new(),
    status: lattice::network_menu::NetStatus::NoDevice,
});
