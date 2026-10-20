//! ANE haptics backend: the `hisi_vibrator@fff34000` ERM motor.
//!
//! Everything here is device-tree fact, read straight from
//! `firmware/ane_dtb/fdt.dtb` (node `//hisi_vibrator@fff34000`, status ok):
//!
//! - `vibrator-reg-on = 0x31` and `vibrator-reg-off = 0x31`: the motor is
//!   switched by one PMIC register, LDO31's enable bit. Writing 1 runs the
//!   motor, 0 stops it.
//! - `vibrator-bit-on = 1`: the enable bit is bit 0 of that register... the
//!   property records a *mask*, and the vendor driver's
//!   `vibrator_bit_on << ...` use makes 1 = bit 0.
//! - The rail behind register 0x31 is `ldo31` (phandle 0x244):
//!   `hisilicon,hisi-ctrl = <0x31 1 0x10>` (ctrl reg, enable bit, eco mask),
//!   `hisilicon,hisi-vset = <0x5a 0x07>` (vset reg, mask), 8 voltage steps
//!   from 2.5 V to 3.1 V, and the vibrator's own
//!   `vibrator_vout_min/max_voltage = 0x2dc6c0` = 3 000 000 uV = 3.0 V, the
//!   6th entry of the vset table.
//! - `vdd-supply` points at that ldo31 phandle, so the motor and the rail
//!   switch are the same register: enabling LDO31 both powers and runs it.
//!
//! The PMU register window is the one `pmic::pmic_rd/wr` already drives for
//! the coulomb counter (ANE battery), so no new MMIO ownership is taken.

use crate::platform::common::pmic;
use crate::platform::common::regulator::RegulatorDescriptor;

/// `vibrator-reg-on`/`vibrator-reg-off`: both name LDO31's control register.
const VIB_REG: usize = 0x31;
/// `vibrator-bit-on = 1`.
const VIB_BIT: u8 = 1 << 0;

/// LDO31 as the DTB describes it: ctrl 0x31 bit 0, vset 0x5A mask 0x07,
/// 3.0 V (the rail voltage the vibrator properties demand), 20 ms off->on
/// delay, 120 us enable time.
pub const VIBRATOR_REGULATOR: RegulatorDescriptor =
    RegulatorDescriptor::hisi_ldo(0x244, 0x31, 0x5a, 0x07, 3_000_000, 20_000, 120);

/// Drive the motor for one effect burst. `on_ms == 0` stops it.
///
/// The vendor timed-output semantics the Android HAL used: set the enable
/// bit, wait, clear. `off_on_delay_us = 20 000` means a re-trigger inside
/// 20 ms of a stop needs the delay; a single burst is simply set -> sleep ->
/// clear.
///
/// Returns false when the rail readback disagrees (regulator dead), true
/// when the burst was driven.
pub fn burst(on_ms: u32) -> bool {
    if on_ms == 0 {
        stop();
        return true;
    }
    // Enable LDO31 (this both powers and runs the motor - one register).
    pmic::pmic_rmw(VIB_REG, VIB_BIT, VIB_BIT);
    let on = pmic::pmic_rd(VIB_REG) & VIB_BIT != 0;
    if !on {
        return false;
    }
    // Same spin-wait primitive the touch bring-up uses: uptime_us-based,
    // with a spin-loop fallback if the counter has no declared frequency.
    if crate::timer::frequency() == 0 {
        for _ in 0..on_ms.saturating_mul(1_000_000) {
            core::hint::spin_loop();
        }
    } else {
        let want = (on_ms as u64) * 10_000;
        let t0 = crate::timer::uptime_us();
        while crate::timer::uptime_us().saturating_sub(t0) < want {
            core::hint::spin_loop();
        }
    }
    pmic::pmic_rmw(VIB_REG, VIB_BIT, 0);
    true
}

/// Stop the motor.
pub fn stop() {
    pmic::pmic_rmw(VIB_REG, VIB_BIT, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vibrator_descriptor_matches_the_dtb() {
        assert_eq!(VIBRATOR_REGULATOR.phandle, 0x244);
        assert_eq!(VIBRATOR_REGULATOR.control_register, 0x31);
        assert_eq!(VIBRATOR_REGULATOR.enable_mask, 1);
        assert_eq!(VIBRATOR_REGULATOR.voltage_set_register, 0x5a);
        assert_eq!(VIBRATOR_REGULATOR.voltage_set_mask, 0x07);
        assert_eq!(VIBRATOR_REGULATOR.voltage_uv, 3_000_000);
        assert_eq!(VIBRATOR_REGULATOR.off_on_delay_us, 20_000);
        assert_eq!(VIBRATOR_REGULATOR.enable_time_us, 120);
    }
}
