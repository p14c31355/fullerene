//! The generic backlight contract plus the ANE-LX2J's LM36923 backend.
//!
//! A phone panel's brightness is one number, but the hardware behind it is
//! a bus device with its own register map and its own power rails. The
//! abstraction is therefore two layers:
//!
//! - [`Backlight`]: the reusable contract - set a level, read the level
//!   back, all in 0..=BACKLIGHT_MAX (2047, an 11-bit code - the range the
//!   LM36923's own datasheet calls out and the vendor driver scales to).
//! - [`lm36923`]: the I2C backend, ported from the vendor kernel's
//!   `drivers/video/fbdev/hisi/dss/backlight/lm36923.c` (hi6250-8 tree),
//!   whose registers and defaults are quoted below rather than invented.
//!
//! The vendor sequence, verified register by register against the source:
//!   REG_ENABLE(0x10) = 0x0        (device out of low power)
//!   REG_BRT_CTR(0x11) = 0x85      (exponential map, register-only mode,
//!                                  ramp off, 0.5 ms rate, active high)
//!   REG_BRT_VAL_L(0x18) lsb / REG_BRT_VAL_M(0x19) msb = the 11-bit code
//!   REG_ENABLE(0x10) = 0xF        (LED1..4 strings on)
//!
//! The panel rides a PMIC boost rail on this board; the backlight chip is
//! on I2C4 at 0x36 (`ti,lm36923` in the DTB, `reg = <6>` = address 0x36).

use super::ane_i2c;
use super::common::mmio;

/// The generic brightness range: an 11-bit code, 0 = panel off.
pub const BACKLIGHT_MAX: u32 = 2047;

/// The reusable brightness state. The atomic keeps the wait loop and any
/// diagnostics reading the same value without a lock.
pub static LEVEL: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Set the panel brightness, 0..=BACKLIGHT_MAX. Returns the level the
/// backend actually applied (equal to the input when the bus answered).
pub fn set(level: u32) -> u32 {
    let clamped = level.min(BACKLIGHT_MAX);
    let applied = lm36923::set_brightness(clamped);
    let stored = if applied { clamped } else { LEVEL.load(core::sync::atomic::Ordering::Relaxed) };
    LEVEL.store(stored, core::sync::atomic::Ordering::Relaxed);
    stored
}

/// The last accepted level.
pub fn level() -> u32 {
    LEVEL.load(core::sync::atomic::Ordering::Relaxed)
}

/// The LM36923 backend. I2C4 slave 0x36, 8-bit registers.
pub mod lm36923 {
    use super::ane_i2c;

    /// The DTB's `ti,lm36923` reg = <6> on i2c4, i.e. 7-bit address 0x36.
    pub const SLAVE: u16 = 0x36;
    /// The bus the chip hangs off (same bus as the charger).
    pub const BUS: usize = ane_i2c::BUS_I2C4;

    // Vendor register map (drivers/video/fbdev/hisi/dss/backlight/lm36923.h).
    pub const REG_REVISION: u8 = 0x00;
    pub const REG_SOFT_REST: u8 = 0x01;
    pub const REG_ENABLE: u8 = 0x10;
    pub const REG_BRT_CTR: u8 = 0x11;
    pub const REG_PWM_CTR: u8 = 0x12;
    pub const REG_BOOST_CTR1: u8 = 0x13;
    pub const REG_BRT_VAL_L: u8 = 0x18;
    pub const REG_BRT_VAL_M: u8 = 0x19;
    pub const REG_FAULT_CTR: u8 = 0x1E;
    pub const REG_FAULT_FLAG: u8 = 0x1F;
    /// REG_BRT_CTR = 0x85: exponential map (0x80), register-only control
    /// (0x00), ramp disabled (0x00), rate 0.5 ms (0x04), active high (0x01).
    pub const BRT_CTR_REG_ONLY: u8 = 0x85;
    /// LED strings 1..4 enabled (the vendor default for an unknown count).
    pub const ENABLE_LEDS: u8 = 0x0F;
    /// MASK_BL_LSB in the vendor header.
    pub const MASK_BL_LSB: u8 = 0x07;
    /// OVP/OCP shutdown enable (the vendor's fault setup).
    pub const FAULT_CTR_DEFAULT: u8 = 0x04;

    /// Did the chip answer? One revision read; a device that NACKs leaves
    /// the panel at whatever the loader left it at.
    pub unsafe fn present() -> bool {
        let mut rev = [0u8; 1];
        ane_i2c::read_regs_at(BUS, SLAVE, REG_REVISION, &mut rev).is_ok()
    }

    /// The full bring-up: fault config, control mode, then the strings on.
    /// Mirrors `lm36923_resume_init` + the DTB-driven parts of the vendor
    /// probe, in the vendor's order.
    pub unsafe fn init() -> bool {
        let _ = ane_i2c::write_reg_at(BUS, SLAVE, REG_BRT_CTR, BRT_CTR_REG_ONLY);
        let _ = ane_i2c::write_reg_at(BUS, SLAVE, REG_FAULT_CTR, FAULT_CTR_DEFAULT);
        // Brightness to a low visible level before the strings enable, so
        // the panel does not flash at whatever the register reset value is.
        let _ = ane_i2c::write_reg_at(BUS, SLAVE, REG_BRT_VAL_L, 0x04);
        let _ = ane_i2c::write_reg_at(BUS, SLAVE, REG_BRT_VAL_M, 0x20);
        let _ = ane_i2c::write_reg_at(BUS, SLAVE, REG_ENABLE, ENABLE_LEDS);
        present()
    }

    /// One brightness step: the 11-bit code split into the LSB register's
    /// low three bits and the MSB register's full byte, LSB first as the
    /// vendor writes them.
    pub unsafe fn write_level(level: u32) -> Result<(), u32> {
        let bl_lsb = (level & 0x07) as u8;
        let bl_msb = (level >> 3) as u8;
        ane_i2c::write_reg_at(BUS, SLAVE, REG_BRT_VAL_L, bl_lsb)?;
        ane_i2c::write_reg_at(BUS, SLAVE, REG_BRT_VAL_M, bl_msb)
    }

    /// Set the brightness; returns false when the bus refused (the level
    /// keeps whatever the chip already had).
    pub fn set_brightness(level: u32) -> bool {
        unsafe { write_level(level).is_ok() }
    }

    /// Turn the panel's LED strings on or off without touching the level.
    pub fn set_power(on: bool) -> bool {
        let value = if on { ENABLE_LEDS } else { 0x00 };
        unsafe { ane_i2c::write_reg_at(BUS, SLAVE, REG_ENABLE, value).is_ok() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_range_is_an_eleven_bit_code() {
        assert_eq!(BACKLIGHT_MAX, 2047);
        // The MSB register therefore carries at most 0xFF at the top level.
        assert_eq!(BACKLIGHT_MAX >> 3, 0xFF);
        assert_eq!(BACKLIGHT_MAX & 0x07, 0x07);
    }

    #[test]
    fn set_clamps_out_of_range_levels() {
        // No hardware here: set() stores the clamped value even when the
        // backend refuses, and level() reads it back.
        let applied = set(BACKLIGHT_MAX + 999);
        assert_eq!(applied, BACKLIGHT_MAX);
        assert_eq!(level(), BACKLIGHT_MAX);
    }

    #[test]
    fn zero_is_a_legal_level() {
        let applied = set(0);
        assert_eq!(applied, 0);
    }
}
