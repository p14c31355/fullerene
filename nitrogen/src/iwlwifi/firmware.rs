//! Firmware selection, upload, and alive-state handling.

use super::device::IwlWifiDevice;
use super::registers::*;
use super::types::FirmwareBlob;

const FW_7260_17: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7260-17.ucode");
const FW_7260_16: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7260-16.ucode");
const FW_7265_17: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265-17.ucode");
const FW_7265_16: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265-16.ucode");
const FW_7265D_29: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265D-29.ucode");
const FW_7265D_27: &[u8] = include_bytes!("../../../bonder/iwlwifi/iwlwifi-7265D-27.ucode");

pub(super) fn select_firmware_list(device_id: u16, hw_rev: u16) -> &'static [FirmwareBlob] {
    match device_id {
        0x08B1 | 0x08B2 => &[
            FirmwareBlob {
                data: FW_7260_17,
                name: "iwlwifi-7260-17",
            },
            FirmwareBlob {
                data: FW_7260_16,
                name: "iwlwifi-7260-16",
            },
        ],
        0x095A | 0x095B if (hw_rev & CSR_HW_REV_TYPE_MASK) == CSR_HW_REV_TYPE_7265D => &[
            FirmwareBlob {
                data: FW_7265D_29,
                name: "iwlwifi-7265D-29",
            },
            FirmwareBlob {
                data: FW_7265D_27,
                name: "iwlwifi-7265D-27",
            },
        ],
        0x095A | 0x095B => &[
            FirmwareBlob {
                data: FW_7265_17,
                name: "iwlwifi-7265-17",
            },
            FirmwareBlob {
                data: FW_7265_16,
                name: "iwlwifi-7265-16",
            },
        ],
        _ => &[],
    }
}

impl IwlWifiDevice {
    pub fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        self.load_firmware_inner(fw_data)
    }

    pub fn start_firmware(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        self.start_firmware_inner(fw_data)
    }

    pub fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, crate::DriverError> {
        self.check_alive_nonblocking_inner(start_tsc)
    }
}

#[cfg(test)]
mod tests {
    use super::select_firmware_list;
    use crate::iwlwifi::registers::CSR_HW_REV_TYPE_7265D;

    #[test]
    fn selects_7260_firmware_in_preference_order() {
        let firmware = select_firmware_list(0x08B1, 0);
        assert_eq!(firmware.len(), 2);
        assert_eq!(firmware[0].name, "iwlwifi-7260-17");
        assert_eq!(firmware[1].name, "iwlwifi-7260-16");
    }

    #[test]
    fn selects_7265d_firmware_for_7265d_hw_rev() {
        let firmware = select_firmware_list(0x095B, CSR_HW_REV_TYPE_7265D);
        assert_eq!(firmware.len(), 2);
        assert_eq!(firmware[0].name, "iwlwifi-7265D-29");
        assert_eq!(firmware[1].name, "iwlwifi-7265D-27");
    }

    #[test]
    fn selects_legacy_7265_firmware_for_legacy_hw_rev() {
        let firmware = select_firmware_list(0x095B, 0);
        assert_eq!(firmware.len(), 2);
        assert_eq!(firmware[0].name, "iwlwifi-7265-17");
        assert_eq!(firmware[1].name, "iwlwifi-7265-16");
    }

    #[test]
    fn rejects_unsupported_devices() {
        assert!(select_firmware_list(0xFFFF, 0).is_empty());
    }
}
