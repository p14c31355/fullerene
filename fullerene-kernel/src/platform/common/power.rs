//! Platform-neutral power and battery state primitives.
//!
//! Backends publish measurements through this value type.  It intentionally
//! contains no policy and no MMIO access, so firmware-owned PMICs can be
//! described before a safe transport driver exists.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatteryState {
    pub voltage_mv: Option<u32>,
    pub current_ma: Option<i32>,
    pub temperature_millic: Option<i32>,
    pub charge_permille: Option<u16>,
    pub design_capacity_mah: Option<u32>,
    pub charging: bool,
    pub present: bool,
}

impl BatteryState {
    pub const UNKNOWN: Self = Self {
        voltage_mv: None,
        current_ma: None,
        temperature_millic: None,
        charge_permille: None,
        design_capacity_mah: None,
        charging: false,
        present: false,
    };

    pub const fn usable(self) -> bool {
        self.present && (self.voltage_mv.is_some() || self.charge_permille.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::BatteryState;

    #[test]
    fn unknown_state_is_not_usable() {
        assert!(!BatteryState::UNKNOWN.usable());
    }

    #[test]
    fn measured_state_is_usable() {
        let state = BatteryState {
            voltage_mv: Some(3800),
            present: true,
            ..BatteryState::UNKNOWN
        };
        assert!(state.usable());
    }
}
