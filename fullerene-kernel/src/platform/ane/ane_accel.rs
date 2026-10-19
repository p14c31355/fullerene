//! The accelerometer, read-only until asked.
//!
//! The vendor `sensorhub` node offers two parts on i2c0 (bus 0): an ST
//! LIS3DH at 0x18 whose WHO_AM_I reads 0x33, and a Rohm Kionix part at 0x1E
//! whose id register reads 0x15. `OEM sensors` in the fastboot responder
//! prints which of them actually answers, so nothing here runs at boot: a
//! driver that configures a part which is not fitted would be talking to
//! whatever else lives at that address.
//!
//! The register map is the LIS3DH datasheet's:
//!
//! | register | address | value written | meaning |
//! |---|---|---|---|
//! | WHO_AM_I | 0x0F | - | reads 0x33 |
//! | CTRL_REG1 | 0x20 | 0x57 | ODR 100 Hz, X/Y/Z enabled |
//! | CTRL_REG4 | 0x23 | 0x80 | block data update, +-2 g |
//! | OUT_X_L | 0x28 | - | six bytes, little endian, X/Y/Z |
//!
//! Multi-byte reads need the sub-address MSB set (0x28 | 0x80) - that is the
//! part's auto-increment flag, not a register bit.

use core::sync::atomic::{AtomicU8, Ordering};

use super::ane_i2c::{BUS_I2C0, read_regs_at};

const ACCEL_ADDR: u16 = 0x18;
const REG_WHO_AM_I: u8 = 0x0F;
const REG_CTRL1: u8 = 0x20;
const REG_CTRL4: u8 = 0x23;
const REG_OUT: u8 = 0x28;
const LIS3DH_ID: u8 = 0x33;

/// The id last read, 0 when nothing has answered.
pub static ACCEL_ID: AtomicU8 = AtomicU8::new(0);

/// What answered at 0x18, without touching any configuration register.
///
/// # Safety
/// MMIO on the live i2c0 controller.
pub unsafe fn identify() -> Option<u8> {
    let mut one = [0u8; 1];
    read_regs_at(BUS_I2C0, ACCEL_ADDR, REG_WHO_AM_I, &mut one).ok()?;
    ACCEL_ID.store(one[0], Ordering::Relaxed);
    Some(one[0])
}

/// Put the part into 100 Hz, +-2 g, block-data-update mode.
///
/// Only does anything when WHO_AM_I said LIS3DH: the writes below are that
/// part's, and another part at this address may well give them another
/// meaning. Returns the id it found.
///
/// # Safety
/// MMIO on the live i2c0 controller.
pub unsafe fn start() -> Option<u8> {
    let id = identify()?;
    if id != LIS3DH_ID {
        return Some(id);
    }
    // CTRL_REG1, CTRL_REG4 in one write? No: separate registers, and this
    // controller writes by register pointer, so two calls.
    if write_reg(REG_CTRL1, 0x57).is_err() {
        return None;
    }
    if write_reg(REG_CTRL4, 0x80).is_err() {
        return None;
    }
    Some(id)
}

/// One sample in raw counts (+-2 g full scale, 1 mg/digit on the LIS3DH).
///
/// # Safety
/// MMIO on the live i2c0 controller.
pub unsafe fn read() -> Option<(i16, i16, i16)> {
    if ACCEL_ID.load(Ordering::Relaxed) != LIS3DH_ID {
        return None;
    }
    let mut raw = [0u8; 6];
    read_regs_at(BUS_I2C0, ACCEL_ADDR, REG_OUT | 0x80, &mut raw).ok()?;
    Some((
        i16::from_le_bytes([raw[0], raw[1]]),
        i16::from_le_bytes([raw[2], raw[3]]),
        i16::from_le_bytes([raw[4], raw[5]]),
    ))
}

/// A single register write. `ane_i2c` exposes reads because the charger map
/// is unknown; this is the one write the port needs, and it is a write to a
/// part the port has identified first.
///
/// # Safety
/// MMIO on the live i2c0 controller.
pub unsafe fn write_reg(reg: u8, value: u8) -> Result<(), u32> {
    super::ane_i2c::write_reg_at(BUS_I2C0, ACCEL_ADDR, reg, value)
}
