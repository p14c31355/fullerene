pub const UART_BASE: usize = 0x0900_0000;
// QEMU's virt machine places the DTB at this address for the direct
// `-kernel` boot path used by Flasks. The entry-point x0 value remains the
// authoritative source when firmware supplies one.
pub const DTB_BASE: u64 = 0x4400_0000;

pub const GICD_BASE: usize = 0x0800_0000;
pub const GICR_BASE: usize = 0x080a_0000;

/// Bring up QEMU virt's GICv3 path for the EL1 physical timer PPI.
pub fn init_interrupt_controller(gicd_base: Option<usize>, gicr_base: Option<usize>) {
    let _ = super::gicv3::init(
        gicd_base.unwrap_or(GICD_BASE),
        gicr_base.unwrap_or(GICR_BASE),
        None,
    );
}

/// The identity map's Device list on QEMU virt: the GIC (0x08000000), the
/// PL011 (0x09000000) and the virtio bank (0x0a000000..0x0a004000) all sit
/// inside the first entry. Verbatim the shared list the qemu bring-ups used;
/// split per platform on 2026-10-04 (see `platform::ane::MMIO_RANGES`).
pub const MMIO_RANGES: &[(u64, u64)] = &[
    (0x0800_0000, 0x09ff_ffff),
    (0x0010_0000, 0x001f_ffff),
    (0x0080_0000, 0x009f_ffff),
    (0x17a0_0000, 0x17c1_ffff),
    (0x0a60_0000, 0x0a6f_ffff),
    (0x1500_0000, 0x153f_ffff),
    (0x0c20_0000, 0x0c3f_ffff),
    (0x0c40_0000, 0x0e7f_ffff),
];
