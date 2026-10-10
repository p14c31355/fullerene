//! Hisilicon PMIC byte access (the ANE's PMIC cell at 0xFFF34000).
//!
//! The vendor `hisi_pmic.c` addresses one byte per 32-bit register slot:
//! register `reg` lives at byte `reg << 2`. Shared by the dwmmc1 (MicroSD)
//! and eMMC bring-ups for the vqmmc/vmmc rail setup and the LDO read-backs.

use super::mmio::{rd8, wr8};

/// PMIC cell base.
pub const PMIC_BASE: usize = 0xFFF3_4000;

/// Read PMIC register `reg` (byte at `reg << 2`).
pub fn pmic_rd(reg: usize) -> u8 {
    rd8(PMIC_BASE, reg << 2)
}

/// Write `v` to PMIC register `reg` (byte at `reg << 2`).
pub fn pmic_wr(reg: usize, v: u8) {
    wr8(PMIC_BASE, reg << 2, v);
}

/// Set `mask` bits in PMIC register `reg` to `v`.
pub fn pmic_rmw(reg: usize, mask: u8, v: u8) {
    let old = pmic_rd(reg);
    pmic_wr(reg, (old & !mask) | (v & mask));
}
