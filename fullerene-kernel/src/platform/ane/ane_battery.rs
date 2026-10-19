//! ANE battery backend: the Hi6555V100 coulomb counter, read over the PMU cell.
//!
//! Evidence lives in `docs/hardware/battery.md`; the primary sources are the
//! vendor hi6250 kernel (`drivers/power/hisi/coul/hi6555v100/`) and the stock
//! ANE-LX2J device tree.  Nothing here is inferred: the register numbers, the
//! per-LSB units and both charge ladders are copied from those two places.

use crate::platform::common::pmic;
use crate::platform::common::power::{ocv, BatteryState};

const REG_V_OUT: usize = 0x1F4;
#[allow(dead_code)]
const REG_CURRENT: usize = 0x1F2;

/// `INVALID_TO_UPDATE_FCC`: the sample has not been refreshed this period.
const INVALID: u16 = 0x8000;

fn reg16(reg: usize) -> u16 {
    (pmic::pmic_rd(reg) as u16) | ((pmic::pmic_rd(reg + 1) as u16) << 8)
}

fn sample(reg: usize) -> Option<u16> {
    let raw = reg16(reg);
    if raw & INVALID != 0 {
        None
    } else {
        Some(raw)
    }
}

/// Pack voltage in millivolts.  1 LSB = 24 * 100 * 1000 / 10661 uV = 225.12 uV.
pub fn voltage_mv() -> Option<u32> {
    let raw = sample(REG_V_OUT)?;
    Some((raw as u64 * 2_400_000 / 10_661 / 1000) as u32)
}

/// Pack current in microamps, positive = charging.  1 LSB = 500 uV across the
/// 10 mohm sense resistor (`R_COUL_MOHM` default: the ANE tree does not set it).
pub fn current_ua() -> i32 {
    match sample(REG_CURRENT) {
        Some(raw) => (raw as i64 * 1000 * 500 / 10_661) as i32,
        None => 0,
    }
}

/// BATT_ID ADC (`adc_batt_id = <6>`) is not wired yet, so the pack cannot be
/// told apart.  `None` means "unidentified".
pub fn battery_identity_mv() -> Option<u32> {
    None
}

/// The `SCUDATL_2900_39k_4400` ladder (`id_identify` 1235..1759 mV).
pub const SCUDATL: ocv::OcvTable = ocv::OcvTable {
    percents: [
        100, 95, 90, 85, 80, 75, 70, 65, 60, 55, 50, 45, 40, 35, 30, 25, 20, 15, 10, 9, 8, 7, 6, 5,
        4, 3, 2, 1, 1,
    ],
    mv: [
        [4357, 4351, 4352, 4346, 4343, 4332],
        [4262, 4273, 4283, 4287, 4285, 4276],
        [4193, 4209, 4225, 4230, 4228, 4221],
        [4141, 4154, 4168, 4175, 4173, 4166],
        [4083, 4107, 4120, 4121, 4119, 4114],
        [3996, 4047, 4070, 4072, 4070, 4064],
        [3942, 3973, 4006, 4021, 4021, 4018],
        [3903, 3924, 3954, 3976, 3979, 3975],
        [3864, 3884, 3905, 3925, 3929, 3926],
        [3835, 3849, 3865, 3882, 3884, 3883],
        [3817, 3821, 3834, 3851, 3852, 3852],
        [3802, 3804, 3811, 3827, 3828, 3827],
        [3789, 3792, 3797, 3807, 3809, 3807],
        [3777, 3782, 3788, 3791, 3792, 3790],
        [3763, 3770, 3778, 3774, 3772, 3765],
        [3748, 3755, 3766, 3756, 3749, 3737],
        [3732, 3736, 3745, 3737, 3729, 3715],
        [3715, 3716, 3717, 3710, 3703, 3690],
        [3686, 3694, 3693, 3690, 3684, 3669],
        [3678, 3689, 3690, 3688, 3682, 3667],
        [3670, 3685, 3686, 3686, 3680, 3665],
        [3661, 3678, 3682, 3683, 3677, 3663],
        [3646, 3665, 3674, 3676, 3669, 3655],
        [3625, 3645, 3659, 3654, 3649, 3631],
        [3596, 3612, 3626, 3615, 3613, 3593],
        [3555, 3566, 3579, 3561, 3566, 3545],
        [3492, 3506, 3514, 3489, 3503, 3483],
        [3395, 3400, 3396, 3381, 3402, 3388],
        [3200, 3200, 3200, 3200, 3200, 3200],
    ],
};

/// The `DESAYLGA_2900_200k_4400` ladder (`id_identify` 2786..3312 mV).
pub const DESAYLGA: ocv::OcvTable = ocv::OcvTable {
    percents: SCUDATL.percents,
    mv: [
        [4364, 4353, 4352, 4349, 4344, 4318],
        [4252, 4256, 4268, 4289, 4289, 4265],
        [4174, 4186, 4203, 4234, 4235, 4213],
        [4118, 4137, 4145, 4181, 4182, 4161],
        [4016, 4092, 4103, 4130, 4130, 4111],
        [3957, 4031, 4054, 4080, 4080, 4064],
        [3905, 3962, 3983, 4035, 4034, 4020],
        [3873, 3909, 3938, 3984, 3989, 3978],
        [3852, 3867, 3889, 3937, 3940, 3926],
        [3837, 3842, 3847, 3888, 3889, 3884],
        [3822, 3825, 3824, 3856, 3858, 3855],
        [3809, 3813, 3810, 3832, 3834, 3830],
        [3793, 3801, 3800, 3811, 3813, 3810],
        [3778, 3787, 3790, 3793, 3794, 3789],
        [3761, 3773, 3778, 3777, 3773, 3762],
        [3746, 3752, 3757, 3758, 3751, 3736],
        [3729, 3728, 3733, 3735, 3727, 3708],
        [3709, 3709, 3711, 3697, 3685, 3680],
        [3676, 3692, 3689, 3686, 3680, 3664],
        [3669, 3688, 3685, 3685, 3678, 3662],
        [3657, 3683, 3679, 3683, 3675, 3660],
        [3643, 3675, 3673, 3678, 3670, 3653],
        [3623, 3662, 3664, 3668, 3653, 3631],
        [3597, 3637, 3648, 3640, 3619, 3598],
        [3560, 3597, 3617, 3599, 3575, 3556],
        [3510, 3542, 3565, 3548, 3520, 3505],
        [3443, 3472, 3493, 3481, 3448, 3441],
        [3349, 3374, 3391, 3381, 3349, 3349],
        [3200, 3200, 3200, 3200, 3200, 3200],
    ],
};

/// The ladder that matches the installed pack.
pub fn table() -> &'static ocv::OcvTable {
    match battery_identity_mv() {
        Some(mv) if (2786..=3312).contains(&mv) => &DESAYLGA,
        _ => &SCUDATL,
    }
}

/// Sample the pack once.
///
/// Temperature is not sampled yet, so the ladders are read at 25 C: charge is
/// an open-circuit-voltage estimate, not the vendor's coulomb-smoothed value.
pub fn state() -> BatteryState {
    let Some(mv) = voltage_mv() else {
        return BatteryState::UNKNOWN;
    };
    let ua = current_ua();
    BatteryState {
        voltage_mv: Some(mv),
        current_ma: Some(ua / 1000),
        temperature_millic: None,
        charge_permille: Some(ocv::percent(table(), mv, 25) as u16 * 10),
        design_capacity_mah: Some(3000), // battery_design_fcc = 0xBB8
        charging: ua > 0,
        present: true,
    }
}
