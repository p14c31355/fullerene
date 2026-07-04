//! Network manager — bridges between the iwlwifi driver, bonder stack,
//! and the lattice UI.

use alloc::string::ToString;
use alloc::vec::Vec;

use bonder::wifi::WifiStatus;
use lattice::desktop::DesktopAction;
use lattice::network_menu::{ApDisplay, NetStatus};

/// Global network state for the UI.
#[derive(Debug)]
pub struct NetworkManager {
    /// Whether the iwlwifi device was found.
    pub device_found: bool,
    /// Cached APs for display.
    pub display_aps: Vec<ApDisplay>,
    /// Current status text.
    pub status: NetStatus,
    /// Tick counter for periodic scanning.
    tick_counter: u64,
    /// Whether we've started scanning.
    #[allow(dead_code)]
    scan_started: bool,
}

impl NetworkManager {
    pub fn new() -> Self {
        Self {
            device_found: false,
            display_aps: Vec::new(),
            status: NetStatus::NoDevice,
            tick_counter: 0,
            scan_started: false,
        }
    }

    /// Called every tick to update network state.
    ///
    /// Checks the global `nitrogen::iwlwifi::WIFI_MANAGER` state
    /// and updates the display data accordingly.
    pub fn tick(&mut self) {
        self.tick_counter += 1;

        // Read WiFi state from iwlwifi driver (through global snapshot)
        if let Some(wifi_state) = nitrogen::iwlwifi::wifi_state_snapshot() {
            self.device_found = wifi_state.device_available;

            // Build display AP list
            let mut aps: Vec<ApDisplay> = wifi_state
                .scan_results
                .iter()
                .map(|ap| {
                    let ssid = ap.ssid.to_string();
                    let signal_bars = match ap.rssi {
                        r if r > -40 => 3,
                        r if r > -60 => 2,
                        r if r > -75 => 1,
                        _ => 0,
                    };
                    let is_connected = wifi_state.connected_ssid.as_deref() == Some(&ssid);
                    ApDisplay {
                        ssid,
                        signal_bars,
                        has_lock: ap.security.needs_password(),
                        connected: is_connected,
                    }
                })
                .collect();

            // Put connected AP first
            aps.sort_by(|a, b| {
                match (a.connected, b.connected) {
                    (true, false) => core::cmp::Ordering::Less,
                    (false, true) => core::cmp::Ordering::Greater,
                    _ => core::cmp::Ordering::Equal,
                }
            });

            self.display_aps = aps;

            // Update status
            self.status = match wifi_state.status {
                WifiStatus::Connected => {
                    if let Some(ip) = wifi_state.ip_address {
                        NetStatus::Connected(
                            wifi_state.connected_ssid.unwrap_or_default(),
                            ip,
                        )
                    } else {
                        NetStatus::Connected(
                            wifi_state.connected_ssid.unwrap_or_default(),
                            "0.0.0.0".into(),
                        )
                    }
                }
                WifiStatus::Scanning => NetStatus::Scanning,
                WifiStatus::Disconnected => {
                    if self.display_aps.is_empty() {
                        NetStatus::Disconnected
                    } else {
                        NetStatus::Disconnected
                    }
                }
                WifiStatus::Error => {
                    NetStatus::Error("Connection failed".into())
                }
                WifiStatus::Authenticating => {
                    NetStatus::Connecting("Authenticating...".into())
                }
                WifiStatus::Associating => {
                    NetStatus::Connecting("Associating...".into())
                }
                WifiStatus::Handshake => {
                    NetStatus::Connecting("WPA Handshake...".into())
                }
            };

            // Start periodic scanning if idle
            if !wifi_state.device_available {
                self.status = NetStatus::NoDevice;
            }
        } else {
            self.device_found = false;
            self.status = NetStatus::NoDevice;
        }
    }

    /// Get the display AP list.
    pub fn get_aps(&self) -> &[ApDisplay] {
        &self.display_aps
    }

    /// Get the current status.
    pub fn get_status(&self) -> &NetStatus {
        &self.status
    }
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
            // Connect to open network (no password)
            if *idx < rt.net_manager.display_aps.len() {
                let ap = &rt.net_manager.display_aps[*idx];
                let ssid = bonder::wifi::Ssid::new(ap.ssid.as_bytes());
                // Call iwlwifi driver to connect
                nitrogen::iwlwifi::connect_to_ap(&ssid, None);
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
            // Trigger connection via iwlwifi with password
            if let Some(_) = rt.desktop.pwd_target_ap {
                let ssid_str = rt.desktop.pwd_dialog_ssid.clone();
                let password = rt.desktop.pwd_dialog_password.clone();
                let ssid = bonder::wifi::Ssid::new(ssid_str.as_bytes());
                // Call iwlwifi driver to connect with WPA2-PSK
                nitrogen::iwlwifi::connect_to_ap(&ssid, Some(&password));
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
