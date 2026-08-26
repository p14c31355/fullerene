pub const UART_BASE: usize = 0x0900_0000;
// QEMU's virt machine places the DTB at this address for the direct
// `-kernel` boot path used by Flasks. The entry-point x0 value remains the
// authoritative source when firmware supplies one.
pub const DTB_BASE: u64 = 0x4400_0000;

pub const GICD_BASE: usize = 0x0800_0000;
pub const GICR_BASE: usize = 0x080a_0000;

/// Bring up QEMU virt's GICv3 path for the EL1 physical timer PPI.
pub fn init_interrupt_controller(gicd_base: Option<usize>, gicr_base: Option<usize>) {
    super::gicv3::init(
        gicd_base.unwrap_or(GICD_BASE),
        gicr_base.unwrap_or(GICR_BASE),
        None,
    );
}
