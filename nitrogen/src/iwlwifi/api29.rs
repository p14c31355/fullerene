//! Intel 7265D firmware API-29 command path.
//!
//! The detected card is a 7265D (`8086:095b`, CSR HW revision type
//! `0x0210`). Its Linux baseline loads `iwlwifi-7265D-29.ucode`, whose
//! non-unified 7000-series INIT protocol is API 22..=29. Keep this dispatch
//! separate from the existing API-17 implementation: the latter remains
//! available for the older 7260/7265 images and for explicit regression
//! runs, but is never selected automatically for a 7265D.

use super::device::IwlWifiDevice;
use super::registers::{IWL_FW_API29_MAX, IWL_FW_API29_MIN};

fn is_api29(api: u32) -> bool {
    (IWL_FW_API29_MIN..=IWL_FW_API29_MAX).contains(&api)
}

/// Run the API-29 7265D non-unified INIT sequence.
///
/// NVM access and the completion notification retain the 7000-series wire
/// format. The API-29-specific implementation adds the upstream ordering of
/// TX antenna configuration before PHY calibration; it does not reuse the
/// API-17 selector or silently downgrade the firmware image.
pub(super) fn send_init_firmware_commands(
    device: &mut IwlWifiDevice,
) -> Result<(), crate::DriverError> {
    if !is_api29(device.fw_api_ver) {
        log::error!(
            "iwlwifi: api29.init rejected unexpected firmware API {}",
            device.fw_api_ver
        );
        return Err(crate::DriverError::Protocol);
    }
    log::info!(
        "iwlwifi: api29.init dispatch fw_api={} fw_build={}",
        device.fw_api_ver,
        device.fw_build
    );
    device.send_init_firmware_commands_profile(true)
}

/// Run the 7265D API-29 runtime setup.
///
/// 7265D is still a pre-new-station-API 7000-series device. Its runtime
/// station, PHY context, MAC context, MCC, and LMAC scan command payloads
/// therefore use the existing legacy wire layouts. The separate entry point
/// is deliberate: future API-29 capability-driven UMAC/DQA additions can be
/// made here without changing API-17 behavior or widening device matching.
pub(super) fn send_runtime_commands(device: &mut IwlWifiDevice) -> Result<(), crate::DriverError> {
    if !is_api29(device.fw_api_ver) {
        log::error!(
            "iwlwifi: api29.runtime rejected unexpected firmware API {}",
            device.fw_api_ver
        );
        return Err(crate::DriverError::Protocol);
    }
    log::info!(
        "iwlwifi: api29.runtime dispatch fw_api={} legacy_7000_wire=true",
        device.fw_api_ver
    );
    device.send_init_commands_legacy()
}
