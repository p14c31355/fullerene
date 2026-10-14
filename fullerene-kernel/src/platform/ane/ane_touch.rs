//! v331g: the ANE touchscreen bring-up (FocalTech on DesignWare I2C0).
//!
//! The DTB inventory (all verified against the device's fdt.dtb):
//! - I2C0 master: 0xFFD71000, 19.2 MHz input, reset ctrl 0xFFD7E000
//!   bits@0x20/0x24/0x28 bit 3, IRQ SPI 118
//! - Touch IC: FocalTech (chip_id 0x54) at slave 0x38, 1080x2280 native
//! - Reset: PL061 gpio18 @0xE8A1D000 pin 1, active low, 200 ms settle
//! - IRQ: PL061 ao_gpio6 @0xFFF1D000 pin 0
//! - Pinmux: 0xE896C000 offsets 0x14/0x18 = func mode (0x1)
//! - Regulators: VDD 3.0 V / IO 1.8 V via the PMIC LDOs (the PMIC driver
//!   already owns the I2C2 bus they hang off)
//!
//! Bring-up order (the stock ts_kit sequence, simplified for no-IRQ
//! polling first): regulators -> pinmux -> controller reset release ->
//! DW I2C master init (400 kHz fast mode) -> touch reset toggle -> chip
//! ID read over I2C -> ring log. The panel IRQ is not touched here; the
//! first milestone is a synchronous chip-ID read proving the bus.

use super::ane;
use crate::platform::common::mmio::{rd32, wr32};
use core::sync::atomic::Ordering;

const IC_CON: usize = 0x00;
const IC_TAR: usize = 0x04;
const IC_SS_SCL_HCNT: usize = 0x14;
const IC_SS_SCL_LCNT: usize = 0x18;
const IC_FS_SCL_HCNT: usize = 0x1c;
const IC_FS_SCL_LCNT: usize = 0x20;
const IC_ENABLE: usize = 0x6c;
const IC_STATUS: usize = 0x70;
const IC_TXFLR: usize = 0x74;
const IC_RXFLR: usize = 0x78;
const IC_DATA_CMD: usize = 0x10;
const IC_CLR_TX_ABRT: usize = 0x54;
const IC_TX_ABRT_SOURCE: usize = 0x80;

const IC_CON_MASTER: u32 = 1 << 0;
const IC_CON_SPEED_FAST: u32 = 2 << 1;
const IC_CON_RESTART_EN: u32 = 1 << 5;
const IC_STATUS_TFNF: u32 = 1 << 1;
const IC_STATUS_RFNE: u32 = 1 << 3;

/// The touch bring-up state for the ring/strip.
pub static TOUCH_STAGE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(b'.');
pub static CHIP_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn spin(ms: u32) {
    for _ in 0..(ms * 2000) {
        core::hint::spin_loop();
    }
}

/// Assert then release the touch reset line (PL061 gpio18 pin 1).
/// PL061: data register at base+0x3fc masks by the pin bit; direction at
/// base+0x400. The stock sequence: hold reset low >= 2 ms, release, wait
/// 200 ms (hard_reset_delay) before the first I2C transaction.
unsafe fn touch_reset_toggle() {
    const GPIO18: usize = ane::TOUCH_RESET_GPIO_PROVIDER;
    const PIN: u32 = 1;
    let bit = 1u32 << PIN;
    // Pin 1 was configured by the stock firmware; force it to output.
    wr32(GPIO18, 0x400, rd32(GPIO18, 0x400) | bit);
    // Assert reset (active low).
    wr32(GPIO18, 0x3fc, rd32(GPIO18, 0x3fc) & !bit);
    spin(ane::TOUCH_RESET_ASSERT_TO_RELEASE_MS);
    // Release reset.
    wr32(GPIO18, 0x3fc, rd32(GPIO18, 0x3fc) | bit);
    spin(ane::TOUCH_RESET_SETTLE_MS);
    // The DTB's hard_reset_delay = 0xc8 = 200 ms before the first access.
    spin(200);
}

/// Release the I2C0 controller from its CRG reset (bits 3 at the three
/// reset registers: enable=1 deasserts, disable=0 asserts, status reads).
unsafe fn i2c0_reset_release() {
    const B: usize = ane::TOUCH_I2C0_RESET_BASE;
    const BIT: u32 = 1 << ane::TOUCH_I2C0_RESET_BIT;
    // Assert (write disable reg with the bit low), then deassert.
    wr32(B, ane::TOUCH_I2C0_RESET_DISABLE_OFFSET, BIT & !BIT);
    wr32(B, ane::TOUCH_I2C0_RESET_ENABLE_OFFSET, BIT);
    // Wait for the status to clear (0 = out of reset).
    for _ in 0..1000 {
        if rd32(B, ane::TOUCH_I2C0_RESET_STATUS_OFFSET) & BIT == 0 {
            break;
        }
        core::hint::spin_loop();
    }
}

/// Route gpio183/gpio184 to the I2C function.
unsafe fn i2c0_pinmux() {
    const PMX: usize = 0xe896_c000;
    wr32(PMX, 0x14, 0x1); // gpio183 = I2C0 SDA
    wr32(PMX, 0x18, 0x1); // gpio184 = I2C0 SCL
}

/// Program the DW I2C master for the touch IC: disable, set the FIFO
/// thresholds + timing, master mode + fast speed, enable.
unsafe fn i2c0_master_init() {
    let base = ane::TOUCH_I2C0_BASE;
    wr32(base, IC_ENABLE, 0);
    // 19.2 MHz / 400 kHz: the DW formula for fast mode with the stock
    // deviation allowance. HCNT/LCNT are register counts of the input
    // clock period: (19.2e6 / 400e3) = 48 total; the DW default split
    // for fast mode is ~40/60 duty: hcnt ~ 19, lcnt ~ 29, minus the
    // hardware minimums. The stock Linux hi6250 values for a 19.2 MHz
    // input: hcnt=0x1c (28), lcnt=0x40 (64) - conservative and proven.
    wr32(base, IC_FS_SCL_HCNT, 0x1c);
    wr32(base, IC_FS_SCL_LCNT, 0x40);
    wr32(base, IC_SS_SCL_HCNT, 0x8c);
    wr32(base, IC_SS_SCL_LCNT, 0x11e);
    // Master + fast mode + restart enabled, slave disabled.
    wr32(
        base,
        IC_CON,
        IC_CON_MASTER | IC_CON_SPEED_FAST | IC_CON_RESTART_EN,
    );
    // Set the target address; 7-bit addressing (the DW IC_TAR takes the
    // address right-aligned).
    wr32(base, IC_TAR, (ane::TOUCH_I2C0_ADDRESS as u32) & 0x3ff);
    wr32(base, IC_ENABLE, 1);
}

/// One I2C write_read transaction (combined format: write the register
/// pointer, repeated-start, read the data). Returns Err on abort/NACK.
unsafe fn i2c0_write_read(register: u8, out: &mut [u8]) -> Result<(), u32> {
    let base = ane::TOUCH_I2C0_BASE;
    // Wait for the TX FIFO space.
    for _ in 0..100_000 {
        if rd32(base, IC_TXFLR) < 8 {
            break;
        }
        core::hint::spin_loop();
    }
    // Issue: the register pointer byte with STOP suppressed... the DW
    // combined format: write cmd (no STOP bit flag = bit9 0), then the
    // read cmds (bit9 = 1 = RESTART for reads).
    wr32(base, IC_DATA_CMD, register as u32);
    for slot in out.iter_mut() {
        let mut spins = 0u32;
        while rd32(base, IC_TXFLR) >= 8 {
            spins += 1;
            if spins > 100_000 {
                return Err(1);
            }
        }
        wr32(base, IC_DATA_CMD, 0x100); // CMD read with RESTART
        let mut spins = 0u32;
        loop {
            if rd32(base, IC_RXFLR) > 0 {
                *slot = (rd32(base, IC_DATA_CMD) & 0xff) as u8;
                break;
            }
            spins += 1;
            if spins > 200_000 {
                return Err(2);
            }
        }
    }
    let abrt = rd32(base, IC_TX_ABRT_SOURCE);
    if abrt != 0 {
        wr32(base, IC_CLR_TX_ABRT, 0);
        return Err(abrt);
    }
    Ok(())
}

/// The touch bring-up stage: returns true when the chip answered.
pub unsafe fn probe() -> bool {
    TOUCH_STAGE.store(b'R', Ordering::Relaxed);
    // 0. pclk_gpio18 gate (SCTRL+0x10 bit 18, the DTB's hi3xxx-clk-gate
    // <0x10 0x40000>): a PL061 write with its pclk gated parks the bus -
    // the v331g hang. The gate lives in the proven-accessible SCTRL.
    const SCTRL: usize = 0xfff0_a000;
    const PCLK_GPIO18: u32 = 1 << 18;
    let g = rd32(SCTRL, 0x10);
    wr32(SCTRL, 0x10, g | PCLK_GPIO18);
    spin(2);
    // 1. Pinmux + controller reset release + master init.
    i2c0_pinmux();
    i2c0_reset_release();
    i2c0_master_init();
    TOUCH_STAGE.store(b'I', Ordering::Relaxed);
    // 2. Touch reset toggle.
    touch_reset_toggle();
    TOUCH_STAGE.store(b'T', Ordering::Relaxed);
    // 3. Chip ID read. FocalTech keeps the firmware ID at 0xA3 (boot ID)
    // and the panel/ chip version block at 0x00..0x03. Read 4 bytes from
    // 0x00 (the FT5x06 class: gate 0x00 = 0x54 on the FT5446 family).
    let mut id = [0u8; 4];
    let r = i2c0_write_read(0x00, &mut id);
    match r {
        Ok(()) => {
            let v =
                (id[0] as u32) | (id[1] as u32) << 8 | (id[2] as u32) << 16 | (id[3] as u32) << 24;
            CHIP_ID.store(v, Ordering::Relaxed);
            TOUCH_STAGE.store(b'C', Ordering::Relaxed);
            let mut hex = [0u8; 8];
            const HEXD: &[u8; 16] = b"0123456789ABCDEF";
            for (i, slot) in hex.iter_mut().enumerate() {
                let nib = ((v >> (28 - 4 * i)) & 0xf) as usize;
                *slot = HEXD[nib];
            }
            let mut line = [0u8; 16];
            line[..6].copy_from_slice(b"TOUCH:");
            line[6..14].copy_from_slice(&hex);
            line[14] = b'\n';
            let line = core::str::from_utf8(&line).unwrap_or("TOUCH: ????????\n");
            crate::blackbox::text_puts(line);
            true
        }
        Err(e) => {
            TOUCH_STAGE.store(b'E', Ordering::Relaxed);
            CHIP_ID.store(0xAA00_0000 | e, Ordering::Relaxed);
            crate::blackbox::text_puts("TOUCH: NOANS\n");
            false
        }
    }
}
