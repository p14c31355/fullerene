//! Network action dispatch — bridges between the lattice UI and the
//! external WifiService via a shared action queue.
//!
//! Solvent itself knows nothing about WiFi drivers.  This module only
//! manages the UI-side state (showing/hiding menus, dialogs) and queues
//! connection requests for an external service to consume.

use bonder::wifi::Ssid;
use lattice::desktop::DesktopAction;

#[cfg(not(nitrogen_no_iwlwifi))]
const WIFI_INIT_TIMEOUT_TICKS: u64 = 600;

/// Runtime-owned Wi-Fi lifecycle and UI projection.
#[allow(dead_code)]
struct WifiService {
    init_started: Option<u64>,
    init_pending: bool,
}

impl WifiService {
    #[allow(dead_code)]
    const fn new() -> Self {
        Self { init_started: None, init_pending: true }
    }

    #[cfg(not(nitrogen_no_iwlwifi))]
    fn advance_init(&mut self, now: u64) {
        let started = *self.init_started.get_or_insert(now);
        if nitrogen::iwlwifi::wifi_init_completed() {
            self.init_pending = false;
        } else if now.wrapping_sub(started) >= WIFI_INIT_TIMEOUT_TICKS {
            nitrogen::iwlwifi::force_init_failed();
            self.init_pending = false;
        } else {
            nitrogen::iwlwifi::try_init_wifi_device_step();
        }
    }

    #[cfg(not(nitrogen_no_iwlwifi))]
    fn update_snapshot() {
        use alloc::string::ToString;
        use alloc::vec::Vec;
        use lattice::network_menu::{ApDisplay, NetStatus};
        use bonder::wifi::WifiStatus;
        let Some(state) = nitrogen::iwlwifi::wifi_state_snapshot() else { return };
        let mut aps: Vec<_> = state.scan_results.iter().map(|ap| {
            let ssid = ap.ssid.to_string();
            ApDisplay {
                connected: state.connected_ssid.as_ref() == Some(&ssid),
                ssid,
                signal_bars: match ap.rssi { -39.. => 3, -59..=-40 => 2, -74..=-60 => 1, _ => 0 },
                has_lock: ap.security.needs_password(),
            }
        }).collect();
        aps.sort_by_key(|ap| !ap.connected);
        *crate::NETWORK_SNAPSHOT.lock() = crate::NetworkSnapshot {
            aps,
            status: if !state.device_available {
                NetStatus::NoDevice
            } else {
                match state.status {
                    WifiStatus::Connected => NetStatus::Connected(
                        state.connected_ssid.unwrap_or_default(),
                        state.ip_address.unwrap_or_else(|| "0.0.0.0".into()),
                    ),
                    WifiStatus::Scanning => NetStatus::Scanning,
                    WifiStatus::Disconnected => NetStatus::Disconnected,
                    WifiStatus::Error => NetStatus::Error("Connection failed".into()),
                    WifiStatus::Authenticating => NetStatus::Connecting("Authenticating...".into()),
                    WifiStatus::Associating => NetStatus::Connecting("Associating...".into()),
                    WifiStatus::Handshake => NetStatus::Connecting("WPA Handshake...".into()),
                }
            },
        };
    }
}

impl crate::Service for WifiService {
    fn tick(&mut self, now: u64) {
        #[cfg(nitrogen_no_iwlwifi)]
        let _ = now;
        #[cfg(not(nitrogen_no_iwlwifi))]
        if self.init_pending { self.advance_init(now); }
        #[cfg(not(nitrogen_no_iwlwifi))]
        nitrogen::iwlwifi::tick_wifi_device();
        #[cfg(not(nitrogen_no_iwlwifi))]
        if nitrogen::iwlwifi::wifi_init_completed() && now % 600 == 0 {
            nitrogen::iwlwifi::start_scan_if_idle();
        }
        for action in core::mem::take(&mut *crate::WIFI_ACTION_QUEUE.lock()) {
            let crate::WifiAction::Connect(ssid, password) = action;
            #[cfg(not(nitrogen_no_iwlwifi))]
            nitrogen::iwlwifi::connect_to_ap(&ssid, password.as_deref());
            #[cfg(nitrogen_no_iwlwifi)]
            let _ = (ssid, password);
        }
        #[cfg(not(nitrogen_no_iwlwifi))]
        if now % 20 == 0 { Self::update_snapshot(); }
    }
}

#[cfg(not(nitrogen_no_iwlwifi))]
pub fn register_wifi_service() {
    use alloc::boxed::Box;
    nitrogen::iwlwifi::init_wifi_manager();
    crate::register_service(Box::new(WifiService::new()));
}

/// Handle a network menu action.
///
/// Returns `true` if the action was handled.
pub fn handle_network_action(rt: &mut crate::RuntimeState, action: &DesktopAction) -> bool {
    match action {
        DesktopAction::ShowNetworkMenu => {
            let (fw, fh, _) = *crate::FB_DIMS.lock();
            rt.desktop.show_network_menu(fw, fh);
            rt.frame_due = true;
            true
        }
        DesktopAction::ConnectAp(idx) => {
            // Queue connection request (open network, no password)
            let snap = crate::NETWORK_SNAPSHOT.lock();
            if *idx < snap.aps.len() {
                let ap = &snap.aps[*idx];
                let ssid = Ssid::new(ap.ssid.as_bytes());
                drop(snap);
                crate::WIFI_ACTION_QUEUE.lock().push(crate::WifiAction::Connect(ssid, None));
            } else {
                drop(snap);
            }
            rt.desktop.dismiss_network_menu();
            rt.frame_due = true;
            true
        }
        DesktopAction::DismissPasswordDialog => {
            rt.desktop.pwd_dialog_open = false;
            rt.desktop.pwd_target_ap = None;
            rt.desktop.pwd_dialog_password.clear();
            rt.desktop.pwd_dialog_cursor = 0;
            rt.desktop.shift_held = false;
            rt.desktop.dismiss_network_menu();
            rt.frame_due = true;
            true
        }
        DesktopAction::SubmitPassword => {
            // Queue connection request with password
            if let Some(_) = rt.desktop.pwd_target_ap {
                let ssid_str = rt.desktop.pwd_dialog_ssid.clone();
                let password = rt.desktop.pwd_dialog_password.clone();
                let ssid = Ssid::new(ssid_str.as_bytes());
                crate::WIFI_ACTION_QUEUE.lock().push(crate::WifiAction::Connect(ssid, Some(password)));
            }
            rt.desktop.pwd_dialog_open = false;
            rt.desktop.pwd_target_ap = None;
            rt.desktop.pwd_dialog_password.clear();
            rt.desktop.pwd_dialog_cursor = 0;
            rt.desktop.shift_held = false;
            rt.desktop.dismiss_network_menu();
            rt.frame_due = true;
            true
        }
        DesktopAction::PasswordChar(c) => {
            if rt.desktop.pwd_dialog_cursor < 64 {
                rt.desktop.pwd_dialog_password.push(*c as char);
                rt.desktop.pwd_dialog_cursor += 1;
            }
            rt.frame_due = true;
            true
        }
        DesktopAction::PasswordBackspace => {
            rt.desktop.pwd_dialog_password.pop();
            rt.desktop.pwd_dialog_cursor = rt.desktop.pwd_dialog_cursor.saturating_sub(1);
            rt.frame_due = true;
            true
        }
        _ => false,
    }
}
