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
use crate::platform::common::designware_i2c::{
    IC_CLR_TX_ABRT, IC_CON_MASTER, IC_CON_RESTART_EN, IC_CON_SLAVE_DISABLE, IC_CON_SPEED_FAST,
    IC_DATA_CMD, IC_DATA_CMD_READ, IC_DATA_CMD_RESTART, IC_DATA_CMD_STOP, IC_STATUS,
    IC_TX_ABRT_SOURCE,
};
use crate::platform::common::mmio::{rd32, wr32};
use core::sync::atomic::Ordering;

const IC_CON: usize = 0x00;
const IC_TAR: usize = 0x04;
const IC_SS_SCL_HCNT: usize = 0x14;
const IC_SS_SCL_LCNT: usize = 0x18;
const IC_FS_SCL_HCNT: usize = 0x1c;
const IC_FS_SCL_LCNT: usize = 0x20;
const IC_ENABLE: usize = 0x6c;
const IC_TXFLR: usize = 0x74;
const IC_RXFLR: usize = 0x78;

/// The touch bring-up state for the ring/strip.
pub static TOUCH_STAGE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(b'.');
pub static CHIP_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Busy-wait for about `ms` milliseconds.
///
/// The old version counted spin iterations (2000 per "millisecond"), which at
/// this core's speed is tens of microseconds - so the 200 ms the FocalTech
/// part requires after reset was really a couple of milliseconds, and the
/// chip was asked for its id long before its firmware was listening.
///
/// The counter is not trustworthy either: `uptime_us` runs about ten times
/// fast (that is what `oem sof` measures). So ask for ten times the time:
/// if the counter is wrong in the direction we believe, the wait is right;
/// if it is right after all, the cost is a second of boot, which is cheaper
/// than a touchscreen that never answers.
fn spin(ms: u32) {
    // A counter with no declared frequency would make the wait below spin
    // forever, and this runs before anyone could report it. Fall back to a
    // rough iteration budget instead - a short wait is survivable, a hung
    // boot is not.
    if crate::timer::frequency() == 0 {
        for _ in 0..ms.saturating_mul(1_000_000) {
            core::hint::spin_loop();
        }
        return;
    }
    let want = (ms as u64) * 10_000;
    let t0 = crate::timer::uptime_us();
    let mut guard = 0u64;
    while crate::timer::uptime_us().saturating_sub(t0) < want {
        core::hint::spin_loop();
        guard += 1;
        if guard > 2_000_000_000 {
            break; // the counter stopped moving; do not take the boot with it
        }
    }
    // A floor, in case the counter never advances at all on this path.
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
    // The counters are in units of the controller's input clock, so their
    // meaning depends on a number this port has two answers for: the DTB
    // inventory says 19.2 MHz, and the SoC dtsi says clock-rate = 128 MHz
    // for i2c0. Guessing wrong in the fast direction is what breaks a bus
    // (counters for 19.2 MHz on a 128 MHz input ask for 1.3 MHz of SCL), so
    // assume the fastest plausible input and aim at standard mode:
    // 128 MHz / 100 kHz = 1280 cycles, 40/60 split.
    //
    // If the mux turns out to select the divided clock instead, the bus runs
    // at 1280 cycles of 19.2 MHz = 15 kHz: slow, legal, and still enough for
    // a chip-id read. Slow is a cost; fast is a failure.
    wr32(base, IC_FS_SCL_HCNT, 512);
    wr32(base, IC_FS_SCL_LCNT, 768);
    wr32(base, IC_SS_SCL_HCNT, 512);
    wr32(base, IC_SS_SCL_LCNT, 768);
    // Master + fast mode + restart enabled, slave disabled.
    wr32(
        base,
        IC_CON,
        IC_CON_MASTER | IC_CON_SLAVE_DISABLE | IC_CON_RESTART_EN | IC_CON_SPEED_FAST,
    );
    // Set the target address; 7-bit addressing (the DW IC_TAR takes the
    // address right-aligned).
    wr32(base, IC_TAR, (ane::TOUCH_I2C0_ADDRESS as u32) & 0x3ff);
    wr32(base, IC_ENABLE, 1);
}

/// One I2C write_read transaction (combined format: write the register
/// pointer, repeated-start, read the data). Returns Err on abort/NACK.
///
/// The master is what makes a transaction *end*: the register byte goes out
/// with STOP suppressed, the first data byte carries RESTART so the bus
/// turns around, and the last data byte carries STOP. Without that last bit
/// the controller keeps the bus - the next transaction then aborts before it
/// starts, which is how a touch controller that is answering perfectly well
/// can look dead.
unsafe fn i2c0_write_read(register: u8, out: &mut [u8]) -> Result<(), u32> {
    let base = ane::TOUCH_I2C0_BASE;
    // Wait for the TX FIFO space.
    for _ in 0..100_000 {
        if rd32(base, IC_TXFLR) < 8 {
            break;
        }
        core::hint::spin_loop();
    }
    // The register pointer: no STOP when a read follows, STOP when this is
    // the whole transaction.
    let first = if out.is_empty() {
        register as u32 | IC_DATA_CMD_STOP
    } else {
        register as u32
    };
    wr32(base, IC_DATA_CMD, first);
    let n = out.len();
    for (i, slot) in out.iter_mut().enumerate() {
        let mut spins = 0u32;
        while rd32(base, IC_TXFLR) >= 8 {
            spins += 1;
            if spins > 100_000 {
                return Err(1);
            }
        }
        let mut cmd = IC_DATA_CMD_READ;
        if i == 0 {
            cmd |= IC_DATA_CMD_RESTART;
        }
        if i + 1 == n {
            cmd |= IC_DATA_CMD_STOP;
        }
        wr32(base, IC_DATA_CMD, cmd);
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
    // The STOP takes a few SCL periods to leave; a transaction that starts
    // while ACTIVITY is still set is refused by the controller.
    let mut spins = 0u32;
    while rd32(base, IC_STATUS) & 0x21 != 0 {
        spins += 1;
        if spins > 200_000 {
            return Err(3);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// v331n: the touch point snapshot the shell reads. Up to 5 points
/// (the FT5x06 class); each is (x, y, pressed). 0xFFFF = absent.
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub pressed: bool,
}

pub static TOUCH_POINTS: [core::sync::atomic::AtomicU32; 5] = [
    core::sync::atomic::AtomicU32::new(0xFFFF_FFFF),
    core::sync::atomic::AtomicU32::new(0xFFFF_FFFF),
    core::sync::atomic::AtomicU32::new(0xFFFF_FFFF),
    core::sync::atomic::AtomicU32::new(0xFFFF_FFFF),
    core::sync::atomic::AtomicU32::new(0xFFFF_FFFF),
];

/// The packed point: x | y<<16 (0xFFFF = absent), pressed in bit 31.
pub fn snapshot(index: usize) -> Option<(u16, u16)> {
    if index >= 5 {
        return None;
    }
    let v = TOUCH_POINTS[index].load(Ordering::Relaxed);
    if v & 0xFFFF == 0xFFFF {
        return None;
    }
    Some(((v & 0xFFFF) as u16, ((v >> 16) & 0xFFFF) as u16))
}

/// The first live contact, in panel pixels, with the press flag intact.
/// `None` when the controller reported nothing (no probe answered, finger
/// lifted and no lift record, or an empty read).
pub fn first_point() -> Option<TouchPoint> {
    let v = TOUCH_POINTS[0].load(Ordering::Relaxed);
    if v & 0xFFFF == 0xFFFF {
        return None;
    }
    Some(TouchPoint {
        x: (v & 0xFFFF) as u16,
        y: ((v >> 16) & 0xFFFF) as u16,
        pressed: v & (1 << 31) != 0,
    })
}

/// The largest x and y the panel has ever reported, packed `x << 16 | y`,
/// plus a sample count in the high half of a second word.
///
/// The panel is 1080x2340 and the driver's comment says the controller
/// reports a 1080x2280 box; nobody has measured which is true. Rather than
/// scale by a guess, remember the extent the controller itself asserts and
/// let `getvar touch` report it - a finger dragged into each corner turns the
/// question into a number.
pub static TOUCH_MAX: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
pub static TOUCH_SAMPLES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// (max x, max y, samples) as last reported.
pub fn touch_extent() -> (u16, u16, u32) {
    let v = TOUCH_MAX.load(Ordering::Relaxed);
    (
        (v >> 16) as u16,
        (v & 0xFFFF) as u16,
        TOUCH_SAMPLES.load(Ordering::Relaxed),
    )
}

/// Poll the touch IC: one read of 0x00..0x1E carries TD_STATUS and all five
/// six-byte point records. Called from the wait loop every pass; at a real
/// bus speed the whole frame is under a millisecond, and the bus timing is
/// programmed conservatively (see `i2c0_master_init`).
pub unsafe fn poll() {
    if TOUCH_STAGE.load(Ordering::Relaxed) != b'C' {
        return; // the chip never answered the probe
    }
    // One transaction for the whole frame: the status byte and all five
    // six-byte records are contiguous from 0x00, so a single START/STOP pair
    // fetches everything. The protocol itself is parsed above the platform,
    // where the host can test it (`crate::touch_report`).
    let mut frame = [0u8; crate::touch_report::FRAME_LEN];
    if i2c0_write_read(crate::touch_report::FRAME_REG, &mut frame).is_err() {
        return;
    }
    for slot in TOUCH_POINTS.iter() {
        slot.store(0xFFFF_FFFF, Ordering::Relaxed);
    }
    let (n, points) = crate::touch_report::parse(&frame);
    for (live, point) in points[..n].iter().enumerate() {
        let Some(p) = point else { continue };
        let packed = (p.x as u32) | (p.y as u32) << 16 | if p.pressed { 1 << 31 } else { 0 };
        TOUCH_POINTS[live].store(packed, Ordering::Relaxed);
        if p.pressed {
            let old = TOUCH_MAX.load(Ordering::Relaxed);
            let mx = ((old >> 16) as u16).max(p.x);
            let my = ((old & 0xFFFF) as u16).max(p.y);
            TOUCH_MAX.store((mx as u32) << 16 | my as u32, Ordering::Relaxed);
            TOUCH_SAMPLES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Read one frame and parse it, without touching the published snapshot.
///
/// `oem touch` uses this: it answers "does the controller report fingers at
/// all" without waiting for the shell to react, which is the first question
/// when the panel has never answered.
///
/// # Safety
/// MMIO on the live i2c0 controller.
pub unsafe fn frame() -> Option<(
    usize,
    [Option<crate::touch_report::Point>; crate::touch_report::MAX_POINTS],
)> {
    let mut buf = [0u8; crate::touch_report::FRAME_LEN];
    i2c0_write_read(crate::touch_report::FRAME_REG, &mut buf).ok()?;
    Some(crate::touch_report::parse(&buf))
}

/// The touch bring-up stage: returns true when the chip answered.
pub unsafe fn probe() -> bool {
    TOUCH_STAGE.store(b'R', Ordering::Relaxed);
    // 0a. pclk_gpio18 gate (SCTRL+0x10 bit 18, the DTB's hi3xxx-clk-gate
    // <0x10 0x40000>): a PL061 write with its pclk gated parks the bus -
    // the v331g hang. The gate lives in the proven-accessible SCTRL.
    const SCTRL: usize = 0xfff0_a000;
    const PCLK_GPIO18: u32 = 1 << 18;
    let g = rd32(SCTRL, 0x10);
    wr32(SCTRL, 0x10, g | PCLK_GPIO18);
    spin(2);
    // 0b. THE TOUCH RAILS (the v331s park hypothesis): the stock ts_kit
    // powers the touch VDD (3.0 V) + IO (1.8 V) BEFORE the first I2C
    // access. Without them the bus floats (the slave holds SDA low) and
    // the DW I2C waits forever on the transaction = the park. The
    // descriptors are the DTB's fts-vdd/io-supply phandles via the
    // hisi_ldo contract (the control register + the enable bit).
    for rail in ane::TOUCH_REGULATORS.iter() {
        let ctrl = rail.control_register as usize;
        let cur = crate::platform::common::pmic::pmic_rd(ctrl);
        crate::platform::common::pmic::pmic_wr(ctrl, cur | rail.enable_mask);
        spin(rail.enable_time_us / 1000 + 5);
    }
    crate::blackbox::text_puts("T0: rails on\n");
    // 1. Pinmux + controller reset release + master init - with a ring
    // marker between each step (the v331n park is inside one of them;
    // the last logged letter names the culprit).
    crate::blackbox::text_puts("T1: pinmux\n");
    i2c0_pinmux();
    crate::blackbox::text_puts("T2: reset\n");
    i2c0_reset_release();
    crate::blackbox::text_puts("T3: init\n");
    i2c0_master_init();
    crate::blackbox::text_puts("T4: initdone\n");
    TOUCH_STAGE.store(b'I', Ordering::Relaxed);
    // 2. Touch reset toggle.
    crate::blackbox::text_puts("T5: rsttoggle\n");
    touch_reset_toggle();
    crate::blackbox::text_puts("T6: rstdone\n");
    TOUCH_STAGE.store(b'T', Ordering::Relaxed);
    // 3. Chip ID read. FocalTech keeps the firmware ID at 0xA3 (boot ID)
    // and the panel/ chip version block at 0x00..0x03. Read 4 bytes from
    // 0x00 (the FT5x06 class: gate 0x00 = 0x54 on the FT5446 family).
    crate::blackbox::text_puts("T7: idread\n");
    let mut id = [0u8; 4];
    let r = i2c0_write_read(0x00, &mut id);
    crate::blackbox::text_puts("T8: iddone\n");
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
