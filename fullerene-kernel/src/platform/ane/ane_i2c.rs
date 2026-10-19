//! I2C4 (0xFDF0D000): the bus the charger sits on.
//!
//! Every register value here is taken from the vendor tree, not guessed:
//!
//! - `/drivers/hisi/ap/platform/hi6250/soc_crgperiph_interface.h`:
//!   `PEREN2` 0x020, `PERCLKEN2` 0x028, `PERSTAT2` 0x02C, `PERRSTEN2`
//!   0x078, `PERRSTDIS2` 0x07C, `PERRSTSTAT2` 0x080 - all bit **27** for
//!   `gt_clk_i2c4` / `ip_rst_i2c4`. (The eMMC path in `ane_emmc` uses the
//!   same layout one group up at 0x090/0x094/0x098, bit 16.)
//! - `hi6250_udp_noRF_pin.dtsi`: `gpio037_pmx_func` = 0x088 -> 1,
//!   `gpio038_pmx_func` = 0x08C -> 1, with `gpio037/038_cfg_func` at
//!   0x094/0x098 -> 0. Those offsets are inside the `ioc` node,
//!   reg 0xE896C000.
//! - `hi6250_udp_default_config.dts`: `i2c4` enabled, and the board hangs
//!   `scharger_v200@6B` (hi6522), `fsa9685@25` and `max98925@31` off it.
//!
//! The controller timing is the same DesignWare programming the touch
//! driver uses for I2C0 (stock hi6250 values for a 19.2 MHz input), because
//! `clk_mux_i2c` feeds both buses. That part is borrowed, not proven, and
//! the first dump will show it: a bus that NACKs everything means the
//! timing or the pinmux is wrong, not that the chip is absent.

use super::common::designware_i2c::*;
use super::common::mmio::{rd32, wr32};

pub const I2C4_BASE: usize = 0xFDF0_D000;

const CRG: usize = 0xFFF3_5000;
const PEREN2: usize = 0x020;
const PERSTAT2: usize = 0x02C;
const PERRSTEN2: usize = 0x078;
const PERRSTDIS2: usize = 0x07C;
const PERRSTSTAT2: usize = 0x080;
const I2C4_BIT: u32 = 1 << 27;

const PMX_IOC: usize = 0xE896_C000;
const PMX_SDA: usize = 0x088; // gpio037
const PMX_SCL: usize = 0x08C; // gpio038
const CFG_SDA: usize = 0x094;
const CFG_SCL: usize = 0x098;

const IC_FS_SCL_HCNT: usize = 0x1c;
const IC_FS_SCL_LCNT: usize = 0x40;
const IC_SS_SCL_HCNT: usize = 0x8c;
const IC_SS_SCL_LCNT: usize = 0x11e;
const IC_CON_MASTER: u32 = 1 << 0;
const IC_CON_SPEED_FAST: u32 = 1 << 1;
const IC_CON_RESTART_EN: u32 = 1 << 5;

/// Gate the bus, release its reset and put both pins in I2C function.
/// Safe to call twice: the gate and reset writes are idempotent.
///
/// # Safety
/// MMIO only, after the CRG is known live (it is from the eMMC path).
pub unsafe fn bring_up(log: fn(&str, u64)) -> bool {
    wr32(CRG, PEREN2, I2C4_BIT);
    let gated = rd32(CRG, PERSTAT2) & I2C4_BIT != 0;

    wr32(CRG, PERRSTEN2, I2C4_BIT);
    let mut asserted = false;
    for _ in 0..0x4000 {
        if rd32(CRG, PERRSTSTAT2) & I2C4_BIT != 0 {
            asserted = true;
            break;
        }
    }
    wr32(CRG, PERRSTDIS2, I2C4_BIT);
    let mut released = false;
    for _ in 0..0x4000 {
        if rd32(CRG, PERRSTSTAT2) & I2C4_BIT == 0 {
            released = true;
            break;
        }
    }

    wr32(PMX_IOC, PMX_SDA, 1);
    wr32(PMX_IOC, PMX_SCL, 1);
    wr32(PMX_IOC, CFG_SDA, 0);
    wr32(PMX_IOC, CFG_SCL, 0);

    log("i2c4: gated=", gated as u64);
    log("i2c4: rst=", ((asserted as u64) << 8) | released as u64);
    log("i2c4: b4_perstat=", rd32(CRG, PERRSTSTAT2) as u64);
    released
}

/// Program the controller for a 7-bit target. Disable first: the DW core
/// refuses CON writes while enabled.
unsafe fn master_init(addr: u16) {
    wr32(I2C4_BASE, IC_ENABLE, 0);
    wr32(I2C4_BASE, IC_FS_SCL_HCNT, 0x1c);
    wr32(I2C4_BASE, IC_FS_SCL_LCNT, 0x40);
    wr32(I2C4_BASE, IC_SS_SCL_HCNT, 0x8c);
    wr32(I2C4_BASE, IC_SS_SCL_LCNT, 0x11e);
    wr32(
        I2C4_BASE,
        IC_CON,
        IC_CON_MASTER | IC_CON_SPEED_FAST | IC_CON_RESTART_EN,
    );
    wr32(I2C4_BASE, IC_TAR, (addr as u32) & 0x3ff);
    wr32(I2C4_BASE, IC_ENABLE, 1);
}

/// Read `count` bytes starting at `from`. One transaction: register
/// pointer write, repeated start, then that many reads.
pub unsafe fn read_regs(addr: u16, from: u8, out: &mut [u8]) -> Result<(), u32> {
    master_init(addr);
    for _ in 0..100_000 {
        if rd32(I2C4_BASE, IC_TXFLR) < 8 {
            break;
        }
        core::hint::spin_loop();
    }
    // The register pointer: no STOP when a read follows, STOP when this is
    // the whole transaction.
    let first = if out.is_empty() {
        from as u32 | IC_DATA_CMD_STOP
    } else {
        from as u32
    };
    wr32(I2C4_BASE, IC_DATA_CMD, first);
    let n = out.len();
    for (i, slot) in out.iter_mut().enumerate() {
        let mut spins = 0u32;
        while rd32(I2C4_BASE, IC_TXFLR) >= 8 {
            spins += 1;
            if spins > 100_000 {
                return Err(1);
            }
        }
        // RESTART on the first byte only (the rest are a plain read burst),
        // and READ on every byte - `RESTART` alone is a *write* of zero.
        let mut cmd = IC_DATA_CMD_READ;
        if i == 0 {
            cmd |= IC_DATA_CMD_RESTART;
        }
        if i + 1 == n {
            cmd |= IC_DATA_CMD_STOP;
        }
        wr32(I2C4_BASE, IC_DATA_CMD, cmd);
        let mut spins = 0u32;
        loop {
            if rd32(I2C4_BASE, IC_RXFLR) > 0 {
                *slot = (rd32(I2C4_BASE, IC_DATA_CMD) & 0xff) as u8;
                break;
            }
            spins += 1;
            if spins > 200_000 {
                return Err(2);
            }
        }
    }
    let abrt = rd32(I2C4_BASE, IC_TX_ABRT_SOURCE);
    if abrt != 0 {
        wr32(I2C4_BASE, IC_CLR_TX_ABRT, 0);
        return Err(abrt);
    }
    // ACTIVITY / MST_ACTIVITY: a transaction that starts while the previous
    // STOP is still on the wire is refused by the controller.
    let mut spins = 0u32;
    while rd32(I2C4_BASE, IC_STATUS) & 0x21 != 0 {
        spins += 1;
        if spins > 200_000 {
            return Err(3);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// A one-line hex row for the panel or an INFO reply.
pub fn hex_row(out: &mut [u8], at: usize, bytes: &[u8]) -> usize {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut i = at;
    for &b in bytes {
        if i + 2 < out.len() {
            out[i] = HEX[(b >> 4) as usize];
            out[i + 1] = HEX[(b & 0xF) as usize];
            out[i + 2] = b' ';
            i += 3;
        }
    }
    i
}
