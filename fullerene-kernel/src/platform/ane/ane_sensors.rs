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

/// (label, address, id register, expected id) straight from the DTS.
pub const CANDIDATES: [(&str, u8, u8, u8); 5] = [
    ("acc-lis3dh", 0x18, 0x0f, 0x33),
    ("acc-kionix", 0x1e, 0x0f, 0x15),
    ("mag-ak09911", 0x0d, 0x00, 0x48),
    ("als-bh1745", 0x38, 0x40, 0x0b),
    ("ps-pa224", 0x1e, 0x01, 0x10),
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
