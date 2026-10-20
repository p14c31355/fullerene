//! Sensor identification, read-only.
//!
//! The vendor `sensorhub` node names two candidates per sensor and leaves the
//! board to pick one, all of them on i2c0 - the bus the touch controller is
//! already on. Each candidate is identified by one register read, so naming
//! the parts this unit carries costs five byte reads and no guesswork.
//!
//! Nothing here is a driver. It answers "which part is fitted", which is the
//! one question that has to be settled before a driver can be written.

use super::ane_i2c::{BUS_I2C0, read_regs_at};

/// (label, address, id register, expected id) — the parts the ANE-LX2J DTB
/// marks `ok` in `/sensorhub/*`, confirmed against the stock build's HAL
/// (`dumpsys sensorservice`: accelerometer/gyro = LSM6DS3, magnetometer =
/// AKM09918, light/proximity = LTR578). The DTB's other candidates
/// (lis3dh@0x19, kionix@0x1F, akm09911@0x0D) are `disabled`; they stay
/// discoverable through `oem i2c` but are not the fitted parts.
///
/// ID registers, from the vendor drivers:
/// - LSM6DS3 `WHO_AM_I` (0x0F) = 0x6A
/// - AKM09918 `WIA` (0x00) = 0x48
/// - LTR578 `MANUFACTURER_ID` (0x87) = 0x05 ("LiteOn")
pub const CANDIDATES: [(&str, u8, u8, u8); 5] = [
    ("acc-lsm6ds3", 0x6a, 0x0f, 0x6a),
    ("gyro-lsm6ds3", 0x6a, 0x0f, 0x6a),
    ("mag-ak09918", 0x0c, 0x00, 0x48),
    ("als-ltr578", 0x53, 0x87, 0x05),
    ("ps-ltr578", 0x53, 0x87, 0x05),
];

/// One candidate's answer: the id byte read, or `None` if the address NACKed.
pub fn read_id(addr: u8, reg: u8) -> Option<u8> {
    let mut one = [0u8; 1];
    unsafe { read_regs_at(BUS_I2C0, addr as u16, reg, &mut one) }
        .ok()
        .map(|_| one[0])
}

/// Which candidates answered with their expected id, as a bit per index.
pub fn identify() -> u32 {
    let mut mask = 0u32;
    for (i, (_name, addr, reg, want)) in CANDIDATES.iter().enumerate() {
        if read_id(*addr, *reg) == Some(*want) {
            mask |= 1 << i;
        }
    }
    mask
}
