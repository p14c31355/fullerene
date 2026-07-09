//! WiFi service — drives the iwlwifi driver lifecycle and bridges state
//! to the Solvent runtime via shared statics.
//!
//! This service is registered with the runtime after `solvent::init()`.
//! Solvent itself knows nothing about WiFi — it only ticks registered
//! services and provides a shared action queue + network snapshot.

use alloc::string::ToString;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use solvent::{WifiAction, NETWORK_SNAPSHOT, WIFI_ACTION_QUEUE};

/// Incremental WiFi init state — owned by the WifiService instead of
/// by Solvent so the runtime stays device-agnostic.
static WIFI_INIT_PENDING: AtomicBool = AtomicBool::new(true);

/// Tick-based timeout counter for WiFi init.
/// Each tick of WifiService increments this by 1; when the counter
/// exceeds WIFI_INIT_TIMEOUT_TICKS the init is forcibly cancelled.
/// This prevents a permanent hang when real-hardware MMIO accesses
/// do not return (PCIe completion timeout, ASPM L1, D3hot, etc.).
static WIFI_INIT_TICK_COUNT: AtomicU64 = AtomicU64::new(0);
const WIFI_INIT_TIMEOUT_TICKS: u64 = 300; // ~5 seconds at 60 fps

/// Service that drives Intel Wireless 7265 init, periodic hardware tick,
/// and UI state synchronisation.
pub struct WifiService;

impl solvent::Service for WifiService {
    fn tick(&mut self, now: u64) {
        // ── Phase 1: incremental device init ──────────────────
        if WIFI_INIT_PENDING.load(Ordering::Relaxed) {
            let tick = WIFI_INIT_TICK_COUNT.fetch_add(1, Ordering::Relaxed);
            if tick > WIFI_INIT_TIMEOUT_TICKS {
                // Init hung — forcibly mark as completed so the idle
                // loop is not blocked by an unresponsive PCIe device.
                WIFI_INIT_PENDING.store(false, Ordering::Release);
                // Also tell the driver to stop trying.
                nitrogen::iwlwifi::force_init_failed();
            } else if nitrogen::iwlwifi::wifi_init_completed() {
                WIFI_INIT_PENDING.store(false, Ordering::Release);
            } else {
                nitrogen::iwlwifi::try_init_wifi_device_step();
            }
        }

        // ── Phase 2: periodic hardware tick (after init) ──────
        nitrogen::iwlwifi::tick_wifi_device();

        // ── Phase 3: consume queued UI actions ────────────────
        let actions = core::mem::take(&mut *WIFI_ACTION_QUEUE.lock());
        for action in actions {
            match action {
                WifiAction::Connect(ssid, password) => {
                    nitrogen::iwlwifi::connect_to_ap(&ssid, password.as_deref());
                }
            }
        }

        // ── Phase 4: update network snapshot for the desktop ──
        // (only every ~20 ticks to avoid churn)
        if now % 20 == 0 {
            if let Some(wifi_state) = nitrogen::iwlwifi::wifi_state_snapshot() {
                let mut aps: Vec<lattice::network_menu::ApDisplay> = wifi_state
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
                        let is_connected =
                            wifi_state.connected_ssid.as_deref() == Some(&ssid);
                        lattice::network_menu::ApDisplay {
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

                let status = Self::convert_status(&wifi_state);

                let mut snap = NETWORK_SNAPSHOT.lock();
                snap.aps = aps;
                snap.status = status;
            }
        }
    }
}

impl WifiService {
    /// Convert a driver-level `WifiStatus` to a UI-level `NetStatus`.
    fn convert_status(state: &nitrogen::iwlwifi::types::WifiManager) -> lattice::network_menu::NetStatus {
        use bonder::wifi::WifiStatus;
        use lattice::network_menu::NetStatus;

        if !state.device_available {
            return NetStatus::NoDevice;
        }

        match state.status {
            WifiStatus::Connected => {
                let ip = state.ip_address.clone().unwrap_or_else(|| "0.0.0.0".into());
                NetStatus::Connected(state.connected_ssid.clone().unwrap_or_default(), ip)
            }
            WifiStatus::Scanning => NetStatus::Scanning,
            WifiStatus::Disconnected => NetStatus::Disconnected,
            WifiStatus::Error => NetStatus::Error("Connection failed".into()),
            WifiStatus::Authenticating => NetStatus::Connecting("Authenticating...".into()),
            WifiStatus::Associating => NetStatus::Connecting("Associating...".into()),
            WifiStatus::Handshake => NetStatus::Connecting("WPA Handshake...".into()),
        }
    }
}

/// Initialise the WiFi driver and return a registered service.
///
/// Called from the kernel init path **after** `solvent::init()`.
pub fn init_and_register() {
    nitrogen::iwlwifi::init_wifi_manager();
    solvent::register_service(alloc::boxed::Box::new(WifiService));
}
