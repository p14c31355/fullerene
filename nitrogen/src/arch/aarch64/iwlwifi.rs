//! Wi-Fi status shim: the ANE has no iwlwifi device wired yet.
//!
//! Same names as the x86 tree; every query answers "not ready".

pub fn wifi_init_completed() -> bool {
    false
}

pub fn wifi_init_failed() -> bool {
    false
}

pub fn wifi_device_ready() -> bool {
    false
}
