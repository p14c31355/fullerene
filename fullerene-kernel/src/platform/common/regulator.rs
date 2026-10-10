//! Platform-neutral regulator contracts.
//!
//! Descriptors identify a rail and its electrical contract.  A platform
//! backend remains responsible for ownership and for applying changes.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegulatorDescriptor {
    pub phandle: u32,
    pub control_register: u8,
    pub voltage_set_register: u8,
    pub voltage_set_mask: u8,
    pub voltage_uv: u32,
    pub enable_mask: u8,
    pub eco_mode_mask: u8,
    pub off_on_delay_us: u32,
    pub enable_time_us: u32,
}

impl RegulatorDescriptor {
    pub const fn hisi_ldo(
        phandle: u32,
        control_register: u8,
        voltage_set_register: u8,
        voltage_set_mask: u8,
        voltage_uv: u32,
        off_on_delay_us: u32,
        enable_time_us: u32,
    ) -> Self {
        Self {
            phandle,
            control_register,
            voltage_set_register,
            voltage_set_mask,
            voltage_uv,
            enable_mask: 1,
            eco_mode_mask: 0x10,
            off_on_delay_us,
            enable_time_us,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RegulatorDescriptor;

    #[test]
    fn describes_a_hisilicon_ldo_without_accessing_hardware() {
        let rail = RegulatorDescriptor::hisi_ldo(0x5f, 0x25, 0x4e, 0x07, 3_000_000, 20_000, 120);
        assert_eq!(rail.phandle, 0x5f);
        assert_eq!(rail.control_register, 0x25);
        assert_eq!(rail.voltage_set_register, 0x4e);
        assert_eq!(rail.voltage_set_mask, 0x07);
        assert_eq!(rail.voltage_uv, 3_000_000);
        assert_eq!(rail.enable_mask, 1);
        assert_eq!(rail.eco_mode_mask, 0x10);
        assert_eq!(rail.off_on_delay_us, 20_000);
    }
}
