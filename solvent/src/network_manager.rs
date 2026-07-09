//! Network action dispatch — bridges between the lattice UI and the
//! external WifiService via a shared action queue.
//!
//! Solvent itself knows nothing about WiFi drivers.  This module only
//! manages the UI-side state (showing/hiding menus, dialogs) and queues
//! connection requests for an external service to consume.

use bonder::wifi::Ssid;
use lattice::desktop::DesktopAction;

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
