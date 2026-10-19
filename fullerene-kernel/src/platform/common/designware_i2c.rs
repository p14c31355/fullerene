//! Read-only Synopsys DesignWare I²C controller observation.
//!
//! The snapshot performs volatile reads only.  The caller must establish that
//! the controller's clock and MMIO ownership are valid before calling it.

use super::mmio::rd32;

pub const IC_CON: usize = 0x00;
pub const IC_TAR: usize = 0x04;
pub const IC_ENABLE: usize = 0x6c;
pub const IC_STATUS: usize = 0x70;
pub const IC_TXFLR: usize = 0x74;
pub const IC_RXFLR: usize = 0x78;
pub const IC_COMP_PARAM_1: usize = 0xf4;
pub const IC_COMP_VERSION: usize = 0xf8;
pub const IC_DATA_CMD: usize = 0x10;
pub const IC_CLR_TX_ABRT: usize = 0x54;
pub const IC_TX_ABRT_SOURCE: usize = 0x80;

/// IC_DATA_CMD is a command word, not a byte (DW_apb_i2c databook 3.9):
/// bit 8 asks for a read, bit 9 ends the transfer with a STOP, and bit 10
/// issues a RESTART before the command.
///
/// All three matter. A "read" without bit 8 is a write of zero, a bus that
/// never sees bit 9 is never released (every later transaction aborts), and
/// a direction change without bit 10 is not a repeated start. Both the touch
/// bus and the charger bus are this controller, so the bits are defined
/// once, here.
/// IC_CON, the control word every DW I2C master must be built from
/// (`i2c-designware-core.h`, `i2c_dw_configure_master`): master mode, slave
/// mode disabled, restart enabled, and a speed field of 1 = standard
/// (100 kHz), 2 = fast (400 kHz), 3 = high.
///
/// SLAVE_DISABLE is not decoration: with it clear the controller also answers
/// as a slave on its own address, which is how a bus can look busy while the
/// master never gets the wire.
pub const IC_CON_MASTER: u32 = 1 << 0;
pub const IC_CON_SPEED_STD: u32 = 1 << 1;
pub const IC_CON_SPEED_FAST: u32 = 2 << 1;
pub const IC_CON_SPEED_HIGH: u32 = 3 << 1;
pub const IC_CON_RESTART_EN: u32 = 1 << 5;
pub const IC_CON_SLAVE_DISABLE: u32 = 1 << 6;

pub const IC_DATA_CMD_READ: u32 = 1 << 8;
pub const IC_DATA_CMD_STOP: u32 = 1 << 9;
pub const IC_DATA_CMD_RESTART: u32 = 1 << 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerSnapshot {
    pub con: u32,
    pub target: u32,
    pub enable: u32,
    pub status: u32,
    pub tx_fifo_level: u32,
    pub rx_fifo_level: u32,
    pub component_parameters: u32,
    pub component_version: u32,
}

impl ControllerSnapshot {
    pub const fn is_enabled(self) -> bool {
        self.enable & 1 != 0
    }

    pub const fn is_activity_set(self) -> bool {
        self.status & (1 << 0) != 0
    }

    pub const fn fifo_levels_are_empty(self) -> bool {
        self.tx_fifo_level == 0 && self.rx_fifo_level == 0
    }
}

/// Read the standard DesignWare observation registers without modifying them.
///
/// # Safety
/// The caller must guarantee that `base` is a mapped, powered DesignWare I²C
/// controller and that another owner is not changing its state concurrently.
pub unsafe fn snapshot(base: usize) -> ControllerSnapshot {
    ControllerSnapshot {
        con: rd32(base, IC_CON),
        target: rd32(base, IC_TAR),
        enable: rd32(base, IC_ENABLE),
        status: rd32(base, IC_STATUS),
        tx_fifo_level: rd32(base, IC_TXFLR),
        rx_fifo_level: rd32(base, IC_RXFLR),
        component_parameters: rd32(base, IC_COMP_PARAM_1),
        component_version: rd32(base, IC_COMP_VERSION),
    }
}

#[cfg(test)]
mod tests {
    use super::ControllerSnapshot;

    #[test]
    fn reports_idle_disabled_controller() {
        let snapshot = ControllerSnapshot {
            con: 0,
            target: 0,
            enable: 0,
            status: 0,
            tx_fifo_level: 0,
            rx_fifo_level: 0,
            component_parameters: 0,
            component_version: 0,
        };
        assert!(!snapshot.is_enabled());
        assert!(!snapshot.is_activity_set());
        assert!(snapshot.fifo_levels_are_empty());
    }
}
