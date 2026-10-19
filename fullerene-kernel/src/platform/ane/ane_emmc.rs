//! eMMC (Arasan SDHCI @0xFF390000) bring-up for the ANE - phase 1: reads.
//!
//! Source of record: vendor `hi6250-kernel/drivers/mmc/host/sdhci-of-arasan.c`
//! (probe order: clk_ahb -> clk_xin -> reset toggle; reset bit =
//! `peri-crg-rst-emmc0-bit` = 16, RSTEN4/RSTDIS4/RSTSTAT4 = 0x90/0x94/0x98)
//! and the DT node `/sdhci@FF390000` (compatible "arasan,sdhci",
//! reg 64 KiB, clock-names clk_xin/clk_ahb -> clk_emmc0/hclk_emmc0,
//! vmmc-supply = LDO23, no vqmmc). Register offsets from the vendor
//! `sdhci.h` (PRESENT_STATE 0x24, CLOCK_CONTROL 0x2C, SOFT_RESET 0x2F,
//! NORMAL_INT 0x30, CAPABILITIES 0x40/0x44, SLOT_INT+VERSION 0xFC,
//! vendor DEBUG_REG0 0x420, PHY_CTRL1 0x430).
//!
//! The loader boots FROM this controller, so its clocks and reset are left
//! running: v52's CRG photo read PERCLKEN0 bit13 (hclk_emmc0) = 1 and
//! PERCLKEN4/PERSTAT4 bit15 (clk_emmc0) = 1. Phase 1 therefore verifies the
//! inherited state and reads the SDHCI identity - read-only, nothing
//! written, and the controller reads only run when the gates say clocked
//! (an unclocked peripheral read can stall this fabric).

use super::common::mmio::{rd8, rd16, rd32, wr8, wr16, wr32};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// v336j: the first failed multi-block chunk of a flash - bit31 = the
/// chunk failed, bits 16..32 = the error status word, bits 0..16 = the
/// normal status word. Reported to the host in the FAIL reply.
pub static LAST_WR: AtomicU32 = AtomicU32::new(0);
/// The outcome of the last `write_sectors` call as `ok << 32 | bad`.
/// A flash from the running kernel has nowhere to print a summary - the
/// boot log block is full by then - so the counts are kept here and
/// `fastboot getvar flash` reads them back.
pub static LAST_FLASH: AtomicU64 = AtomicU64::new(0);

pub(super) const CRG_BASE: usize = 0xFFF3_5000;
pub(super) const SDHCI_BASE: usize = 0xFF39_0000;

const HEX: [u8; 16] = *b"0123456789ABCDEF";

pub(super) fn put(buf: &mut [u8; 44], at: usize, s: &[u8]) -> usize {
    let n = s.len().min(buf.len() - at);
    buf[at..at + n].copy_from_slice(&s[..n]);
    at + n
}

pub(super) fn hex8(buf: &mut [u8; 44], at: usize, v: u32) -> usize {
    for i in 0..8 {
        buf[at + i] = HEX[((v >> (28 - 4 * i)) & 0xF) as usize];
    }
    at + 8
}

pub(super) fn hex4(buf: &mut [u8; 44], at: usize, v: u32) -> usize {
    for i in 0..4 {
        buf[at + i] = HEX[((v >> (12 - 4 * i)) & 0xF) as usize];
    }
    at + 4
}

pub(super) fn hex2(buf: &mut [u8; 44], at: usize, v: u8) -> usize {
    buf[at] = HEX[(v >> 4) as usize];
    buf[at + 1] = HEX[(v & 0xF) as usize];
    at + 2
}

/// One "EMn LABEL=........" row, painted and logged.
fn row2(log: fn(&str, u64), row: u32, label0: &[u8], a: u32) {
    let mut l = [b' '; 44];
    let at = put(&mut l, 0, label0);
    hex8(&mut l, at, a);
    super::ane_fb::paint(row, &l);
    log("em: ", a as u64);
}

/// The card clock the boot actually ended on, packed for `getvar emmc`:
/// bits 0..8 the field in CLOCK_CONTROL, 8..16 the field the boot
/// started with, 16..40 one nibble per rung tried, 40..48 the field
/// that failed first. Zero means the probe never ran.
static CLK_RESULT: AtomicU64 = AtomicU64::new(0);
static CLK_SAMPLE: AtomicU64 = AtomicU64::new(0);

/// The divider the boot's own reads work at. The loader leaves 0xFA (400 kHz
/// on a 200 MHz clk_xin); the first ladder overwrites this with whatever the
/// controller actually held, so going back is a measurement, not a constant.
pub static CLK_BASE_FIELD: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0xFA);

/// What the clock ladder settled on (see `clock_probe`).
pub fn clock_sample() -> u64 {
    CLK_SAMPLE.load(Ordering::Relaxed)
}

pub fn clock_result() -> u64 {
    CLK_RESULT.load(Ordering::Relaxed)
}

/// Climb to the fastest card clock that can still read, using reads only.
///
/// Two primary-source facts fix the shape of this ladder:
///
/// * The card clock is `clk_xin / (2 * field)` (vendor `sdhci_set_clock`,
///   spec-3.00 divisors are stored halved). The boot's own 0xFA = 250 is
///   the 400 kHz init clock, so clk_xin is the 200 MHz the vendor probe
///   programs - which makes field 8 = 12.5 MHz, 6 = 16.7, 5 = 20,
///   4 = 25, and field 1 = 100 MHz.
/// * With HS_TIMING still at default speed (JEDEC JESD84-B50 6.6.2) the
///   card is specified only to 26 MHz. A field above that is not a
///   faster bus, it is a dead one: the transfer dies and leaves the
///   controller holding the lines (v336t). The previous ladder started
///   at field 1 and poisoned every rung behind it - exactly the
///   all-rungs-failed answer `getvar emmc` gave.
///
/// So climb slowly to fast inside the envelope and stop at the first
/// failure: a clock that fails at 12.5 MHz will not answer at 25. Each
/// rung is a warm-up command followed by two CMD17 reads that must both
/// carry the MBR signature and agree. The warm-up is CMD16 (set block
/// length 512, which the card already is), not CMD13: CMD13 needs the
/// RCA the loader assigned and nobody told us, while CMD16 takes no RCA
/// and cannot change the card's state. Nothing is written, and a failed
/// read leaves the controller reset by `sdhci_data_read`'s recovery.
///
/// Packed for `getvar emmc`: bits 0..8 the chosen field, 8..16 the field
/// the boot started with, 16..32 one nibble per rung (0 untried, 1 read,
/// 2 clock refused, 3 status failed, 4 read failed, 5 read wrong,
/// 6 reads disagreed), 40..48 the first field that failed. Zero means the
/// probe never ran.
pub fn clock_probe(log: fn(&str, u64)) {
    let base = rd16(SDHCI_BASE, 0x2C) >> 8;
    CLK_BASE_FIELD.store(base, Ordering::Relaxed);
    // The inherited clock tree, so clk_xin stops being an inference:
    // 0xB4 bit0 picks PLL0/PLL3, bit2 the divider branch, bits 3..6 the
    // divider (2^val), and 0x40 is the gate word.
    log("em: crg40=", rd32(CRG_BASE, 0x40) as u64);
    log("em: crgb4=", rd32(CRG_BASE, 0xB4) as u64);

    // Slowest first. Fields 8..4 stay inside the 26 MHz default-speed
    // envelope; 3 (33 MHz) and 2 (50 MHz) exist only once CMD6 has told
    // the card it may run high speed - see `switch_hs`.
    const RUNGS: [u16; 6] = [8, 6, 5, 4, 3, 2];
    let rungs = if hs_mode() { &RUNGS[..] } else { &RUNGS[..4] };
    let mut code = 0u64;
    let mut chosen = 0u64;
    let mut first_fail = 0u64;
    let mut sample = [0u32; 128];
    for (i, field) in rungs.iter().enumerate() {
        let slot = (i * 4) as u64;
        let mut set = |c: u64| code |= c << slot;
        if !emmc_clock_set(*field, log) {
            set(2);
            first_fail = *field as u64;
            break;
        }
        let (st, _s, _e, _n) = sdhci_cmd(16, 512, 2, CMD_CRC | CMD_INDEX);
        if !st {
            set(3);
            first_fail = *field as u64;
            break;
        }
        let mut a = [0u32; 128];
        let (ok_a, _sa, _ea) = sdhci_data_read(17, 0, &mut a, log);
        if !ok_a {
            set(4);
            first_fail = *field as u64;
            break;
        }
        if (a[127] & 0xFFFF) != 0xAA55 {
            set(5);
            first_fail = *field as u64;
            break;
        }
        let mut b = [0u32; 128];
        let (ok_b, _sb, _eb) = sdhci_data_read(17, 0, &mut b, log);
        if !ok_b {
            set(4);
            first_fail = *field as u64;
            break;
        }
        if (b[127] & 0xFFFF) != 0xAA55 {
            set(5);
            first_fail = *field as u64;
            break;
        }
        if a[0] != b[0] {
            set(6);
            first_fail = *field as u64;
            break;
        }
        set(1);
        chosen = *field as u64;
        sample = a;
    }
    if chosen == 0 {
        // Nothing climbed: go back to what the boot had and make sure it
        // still reads, so the flash path is never left on a dead clock.
        let _ = emmc_clock_set(base, log);
        let mut m = [0u32; 128];
        let (ok, _s, _e) = sdhci_data_read(17, 0, &mut m, log);
        let good = ok && (m[127] & 0xFFFF) == 0xAA55;
        log("em: clk_back=", ((good as u64) << 8) | base as u64);
        if !good {
            log("em: clk_dead=", 1);
        }
    } else {
        // One more read on the clock we are keeping: the flash path runs
        // minutes later, and this is the only proof it was still good.
        let mut m = [0u32; 128];
        let (ok, _s, _e) = sdhci_data_read(17, 0, &mut m, log);
        log("em: clk_keep=", ((ok as u64) << 8) | chosen);
    }
    CLK_SAMPLE.store(
        ((sample[0] as u64) << 32) | sample[1] as u64,
        Ordering::Relaxed,
    );
    CLK_RESULT.store(
        chosen | (base as u64) << 8 | code << 16 | first_fail << 40,
        Ordering::Relaxed,
    );
    log("em: clk_res=", clock_result());
}

/// Recover the command path after a timeout/error: the SDHCI spec requires
/// software reset for CMD before it will issue anything again. The v66 run
/// showed a single CMD1 timeout leaving every following command silently
/// stuck (no completion, no error, full poll).
pub(super) fn sdhci_reset_cmd() {
    wr8(SDHCI_BASE, 0x2F, 0x02);
    for _ in 0..0x2_0000 {
        if rd8(SDHCI_BASE, 0x2F) & 0x02 == 0 {
            break;
        }
    }
}

/// SDHCI command-register field values, from the vendor header (sdhci.h
/// 46-55) rather than from memory: the response type lives in bits [1:0]
/// (0 none, 1 = 136-bit, 2 = 48-bit, 3 = 48-bit with busy), CRC check is
/// 0x08, index check 0x10, data-present 0x20. v67 built these from the
/// inverse table (0x02 read as "48-bit"), so CMD1 selected the 136-bit
/// capture and every response landed shifted into the high words.
pub(super) const CMD_CRC: u16 = 0x08;
pub(super) const CMD_INDEX: u16 = 0x10;
pub(super) const CMD_DATA: u16 = 0x20;

/// Single-block PIO read for a data command (vendor `sdhci_read_block_pio`
/// + `sdhci_transfer_pio`): write BLOCK_SIZE/BLOCK_COUNT, issue the
/// command with the data-present bit, poll PRESENT_STATE.DATA_AVAILABLE
/// (0x800) per word out of the buffer port 0x20, then wait for
/// TRANSFER_COMPLETE (NORMAL_INT bit1) or an error latch. Bounded
/// throughout; reports (complete, normal_int, error_status).
/// v336t: a burst that dies leaves the controller holding the CMD and DAT
/// lines, and every later command then spins to its poll bound and fails -
/// the card is fine, the host side is stuck. SPEC 3.00 3.3.13: software
/// reset for CMD (bit 1) and DAT (bit 2) returns just those state machines
/// to idle without touching the clock or the card.
pub(super) fn recover_lines(log: fn(&str, u64)) {
    wr8(SDHCI_BASE, 0x2F, 0x06);
    for n in 0..0x40_000u32 {
        if rd8(SDHCI_BASE, 0x2F) & 0x06 == 0 {
            break;
        }
        if n & 0xFFF == 0 {
            crate::platform::common::recovery::boot_heartbeat();
        }
    }
    wr16(SDHCI_BASE, 0x32, 0xFFFF);
    wr32(SDHCI_BASE, 0x30, 0xFFFF_FFFF);
    log("em: recovered=", rd8(SDHCI_BASE, 0x2F) as u64);
}

pub(super) fn sdhci_data_read(
    idx: u16,
    arg: u32,
    words: &mut [u32],
    log: fn(&str, u64),
) -> (bool, u32, u32) {
    wait_idle(0x40000, log);
    wr16(SDHCI_BASE, 0x04, 512); // block size 0x200
    wr16(SDHCI_BASE, 0x06, 1); // one block
    wr8(SDHCI_BASE, 0x2E, 0x0E); // v336e: maximum data timeout (see CMD25).
    wr16(SDHCI_BASE, 0x0C, 0x12); // BLK_CNT_EN | READ, no DMA (sdhci_set_transfer_mode)
    wr32(SDHCI_BASE, 0x08, arg);
    wr16(
        SDHCI_BASE,
        0x0E,
        (idx << 8) | 0x02 | CMD_CRC | CMD_INDEX | CMD_DATA,
    );
    let mut filled = 0u64;
    for w in words.iter_mut() {
        let mut ready = false;
        for j in 0..0x2_0000u32 {
            if j & 0xFFF == 0 {
                super::ane::watchdog_tend();
            }
            let ps = rd32(SDHCI_BASE, 0x24);
            if ps & 0x800 != 0 {
                ready = true;
                break;
            }
            if rd32(SDHCI_BASE, 0x30) & 0x8000 != 0 {
                break;
            }
        }
        if !ready {
            break;
        }
        *w = rd32(SDHCI_BASE, 0x20);
        filled += 1;
    }
    log("em: c8_filled=", filled);
    let mut sts = 0u32;
    let mut err = 0u32;
    for i in 0..0x80_0000u32 {
        if i & 0xFFF == 0 {
            super::ane::watchdog_tend();
        }
        sts = rd32(SDHCI_BASE, 0x30);
        if sts & (0x2 | 0x8000) != 0 {
            err = rd16(SDHCI_BASE, 0x32) as u32;
            break;
        }
    }
    wr32(SDHCI_BASE, 0x30, 0xFFFF_FFFF);
    if sts & 0x2 == 0 {
        recover_lines(log);
    }
    (sts & 0x2 != 0, sts, err)
}

/// Single-block PIO write, the sibling of `sdhci_data_read`: same framing
/// but the transfer mode drops the READ bit and each word goes out only
/// while PRESENT_STATE.SPACE_AVAILABLE (0x400) says the buffer will take
/// it (vendor `sdhci_write_block_pio` + `sdhci_transfer_pio`). The card's
/// DAT0 programming busy is absorbed by the completion poll. Reports
/// (complete, normal_int, error_status).
pub(super) fn sdhci_data_write(
    idx: u16,
    arg: u32,
    words: &[u32],
    log: fn(&str, u64),
) -> (bool, u32, u32) {
    wr16(SDHCI_BASE, 0x04, 512); // block size 0x200
    wr16(SDHCI_BASE, 0x06, 1); // one block
    wr16(SDHCI_BASE, 0x0C, 0x02); // BLK_CNT_EN, WRITE direction, no DMA
    wr32(SDHCI_BASE, 0x08, arg);
    wr16(
        SDHCI_BASE,
        0x0E,
        (idx << 8) | 0x02 | CMD_CRC | CMD_INDEX | CMD_DATA,
    );
    let mut sent = 0u64;
    for w in words.iter() {
        let mut ready = false;
        for j in 0..0x2_0000u32 {
            if j & 0xFFF == 0 {
                super::ane::watchdog_tend();
            }
            let ps = rd32(SDHCI_BASE, 0x24);
            if ps & 0x400 != 0 {
                ready = true;
                break;
            }
            if rd32(SDHCI_BASE, 0x30) & 0x8000 != 0 {
                break;
            }
        }
        if !ready {
            break;
        }
        wr32(SDHCI_BASE, 0x20, *w);
        sent += 1;
    }
    log("em: c24_sent=", sent);
    let mut sts = 0u32;
    let mut err = 0u32;
    for _ in 0..0x80_0000u32 {
        sts = rd32(SDHCI_BASE, 0x30);
        if sts & (0x2 | 0x8000) != 0 {
            err = rd16(SDHCI_BASE, 0x32) as u32;
            break;
        }
    }
    wr32(SDHCI_BASE, 0x30, 0xFFFF_FFFF);
    (sts & 0x2 != 0, sts, err)
}

/// The vendor's clock sequence (sdhci_set_clock, spec-2.00 divisor): stop
/// the clock, program the divisor, wait for INT_STABLE, enable the card
/// clock, then the vendor's settle delay ("need to wait 8 sd clk before a
/// new command" - 1 ms, missing in the v93 attempt that killed the first
/// post-switch read).
/// The PHY block at 0x430..0x443 (vendor `include/linux/mmc/sdhci.h`):
/// power it up, let the resistor trim settle, then switch from open-drain
/// to the push-pull drive the eMMC bus needs. Values are the vendor's own
/// (`sdhci_of_arasan_PHY_init` / `sdhci_disable_open_drain`) plus
/// `drv-strength = <4 4>` from hisi_6250_mmc.dtsi, i.e. 40 ohm.
pub(super) fn phy_init(log: fn(&str, u64)) {
    let base = rd32(SDHCI_BASE, 0x430);
    if base == 0xFFFF_FFFF {
        log("em: phy_none=", base as u64);
        return;
    }
    log("em: phy0=", base as u64);
    let mut r = base | (0x1 << 16); // EN_RTRIM
    r &= !(0xFFu32 << 8); // DLL_TRIM_ICP
    r |= 0x4 << 8;
    wr32(SDHCI_BASE, 0x430, r);

    r |= 0x1 << 25; // PDB: power the PHY
    wr32(SDHCI_BASE, 0x430, r);
    let mut n = 0u64;
    while n < 0x4000 && rd32(SDHCI_BASE, 0x440) & (0x1 << 6) == 0 {
        n += 1;
    }
    log("em: phy_cal=", n);
    let mut m = 0u64;
    while m < 0x10 && rd32(SDHCI_BASE, 0x440) & 0x1 != 0 {
        m += 1;
    }
    log("em: phy_exr=", m);

    // Push-pull bus, so open drain goes off and the data/cmd pull-ups go
    // on (vendor writes 0x003FF7FC to PHY_CTRL2 for exactly this).
    let mut r = rd32(SDHCI_BASE, 0x430);
    r &= !0x1; // ODEN_CMD
    r &= !(0x7u32 << 22); // DR_TY
    r |= 4 << 22; // 40 ohm
    wr32(SDHCI_BASE, 0x430, r);
    wr32(SDHCI_BASE, 0x434, 0x003F_F7FC);
    log("em: phy1=", rd32(SDHCI_BASE, 0x430) as u64);
    log("em: phy2=", rd32(SDHCI_BASE, 0x434) as u64);
}

pub(super) fn emmc_clock_set(field: u16, log: fn(&str, u64)) -> bool {
    wr16(SDHCI_BASE, 0x2C, 0);
    wr16(SDHCI_BASE, 0x2C, (field << 8) | 0x01);
    let mut stable = false;
    for _ in 0..0x2_0000u32 {
        if rd16(SDHCI_BASE, 0x2C) & 0x02 != 0 {
            stable = true;
            break;
        }
    }
    wr16(SDHCI_BASE, 0x2C, (field << 8) | 0x01 | 0x04);
    // Vendor `sdhci_set_clock` lets the new clock tick before anything is
    // asked of the card (`mdelay(1)`, and a NEIGH_DELAY quirk that guards
    // the internal-clock reset path). This loop asked for 2 ms of
    // `uptime_us`, but that counter runs about ten times the wall clock
    // (v336 series: a 900 s deadline fired at 85 s), so the real wait was
    // a fifth of a millisecond. Ask for the vendor millisecond with the
    // same tenfold slack and the wait is never short.
    let t0 = crate::timer::uptime_us();
    while crate::timer::uptime_us().saturating_sub(t0) < 20_000 {}
    log(
        "em: clk_set=",
        ((stable as u64) << 32) | ((field as u64) << 8) | (rd16(SDHCI_BASE, 0x2C) as u64),
    );
    stable
}

/// Public single-sector read wrapper for cross-module probes (the USB-side
/// `oem touch` times one plain CMD17 from the command context, isolating
/// the post-boot read path from the GPT logic in find_partition).
pub fn sdhci_read_probe(lba: u32, words: &mut [u32; 128], log: fn(&str, u64)) -> (bool, u32, u32) {
    sdhci_data_read(17, lba, words, log)
}

/// Look up a GPT partition by name (ASCII, e.g. b"kernel"): reads the
/// entry array (entries LBA 2, 128 entries, verified in v81/v82) and
/// compares the UTF-16LE name field at bytes 56..127. Returns the first
/// and last sector of an exact match.
pub fn find_partition(name: &[u8], log: fn(&str, u64)) -> Option<(u32, u32)> {
    let mut sec = [0u32; 128];
    let mut idx = 0u32;
    while idx < 32 {
        let (ok, _s, _e) = sdhci_data_read(17, 2 + idx, &mut sec, log);
        if !ok {
            return None;
        }
        let mut e = 0usize;
        while e < 4 {
            let base = e * 32;
            if (sec[base] != 0 || sec[base + 1] != 0) && name.len() <= 35 {
                let mut ok = true;
                let mut i = 0usize;
                while ok && i < name.len() {
                    let wi = base + 14 + i / 2;
                    let ch = if i % 2 == 0 {
                        sec[wi] & 0xFFFF
                    } else {
                        sec[wi] >> 16
                    };
                    if ch != name[i] as u32 {
                        ok = false;
                    }
                    i += 1;
                }
                if ok {
                    // the next UTF-16 unit must be NUL (exact, not a prefix)
                    let wi = base + 14 + name.len() / 2;
                    let next = if name.len() % 2 == 0 {
                        sec[wi] & 0xFFFF
                    } else {
                        sec[wi] >> 16
                    };
                    if next == 0 {
                        // first = words 8 (lo) + 9 (hi); last = 10 + 11.
                        let last = if sec[base + 11] != 0 {
                            0xFFFF_FFFF
                        } else {
                            sec[base + 10]
                        };
                        return Some((sec[base + 8], last));
                    }
                }
            }
            e += 1;
        }
        idx += 1;
    }
    None
}

/// Write `data` to consecutive sectors starting at `lba`. Runs of up to
/// 1024 sectors go out as single CMD25 bursts (fast); a failed burst
/// falls back to per-sector CMD24 for that run. Reports (sectors written,
/// sectors that errored).
/// Read `count` sectors into `out` as the sector's raw byte sequence (the
/// same big-endian 2-words-per-line order the forensics dump prints). Used by
/// the raw `oem xfer` stream; `out` must be at least `count * 512` bytes.
pub fn read_sectors_bytes(lba: u32, count: u32, out: &mut [u8], log: fn(&str, u64)) -> bool {
    let mut words = [0u32; 128];
    for s in 0..count {
        let l = lba.wrapping_add(s);
        let (ok, _st, _e) = sdhci_data_read(17, l, &mut words, log);
        if !ok {
            return false;
        }
        let base = (s as usize) * 512;
        for pair in 0..64 {
            let v = ((words[pair * 2] as u64) << 32) | words[pair * 2 + 1] as u64;
            out[base + pair * 8..base + pair * 8 + 8].copy_from_slice(&v.to_be_bytes());
        }
    }
    true
}

/// Persist one 8 KiB slice of the durable text ring to a scratch area at the
/// far end of userdata (640 sectors below the disk end - clear of the GPT
/// backup), rotating through the ring's exact seven slices so the whole ring
/// is copied within seven calls. A later boot can read the scratch with
/// `oem dump <lba> <count>` even after a crash or reset wiped RAM.
pub fn persist_log_slice(slice: u32, log: fn(&str, u64)) -> (u64, u64) {
    /// Measured disk layout: 0x3A3E000 sectors total, userdata is the last
    /// partition, so the disk tail is userdata.
    const SCRATCH_LBA: u32 = 61_078_912; // 0x3A3DD80
    /// The 0xE000 ring is exactly seven 8 KiB slices; no wrap to handle.
    const SLICE: usize = 0x2000;
    let idx = (slice % 7) as usize;
    let addr = crate::blackbox::TEXT_BLOCK_BASE as usize + idx * SLICE;
    let data = unsafe { core::slice::from_raw_parts(addr as *const u8, SLICE) };
    write_sectors(SCRATCH_LBA + (idx as u32) * (SLICE as u32 / 512), data, log)
}

/// Persist the WHOLE durable ring to the eMMC scratch, from the command
/// context only. The automatic IRQ-time lane deadlocked against the boot
/// thread's own SDHCI work (first lived proof: the v124 stall at the very
/// first beat%8192), so the reliable save runs on demand instead, when the
/// world is otherwise idle. Verify by reading `oem xfer m 3A3DD80 70` on a
/// later boot. Returns (written, failed) sectors.
pub fn persist_all(log: fn(&str, u64)) -> (u64, u64) {
    const SCRATCH_LBA: u32 = 61_078_912; // 0x3A3DD80
    const SLICE: usize = 0x2000;
    let mut ok = 0u64;
    let mut bad = 0u64;
    for idx in 0..7usize {
        let addr = crate::blackbox::TEXT_BLOCK_BASE as usize + idx * SLICE;
        let data = unsafe { core::slice::from_raw_parts(addr as *const u8, SLICE) };
        let (o, b) = write_sectors(SCRATCH_LBA + (idx as u32) * (SLICE as u32 / 512), data, log);
        ok += o;
        bad += b;
    }
    (ok, bad)
}

/// Put the card clock back where the boot found it.
///
/// Switching high-speed *off* while the divider still asks for 50 MHz would
/// leave a default-speed card over its timing, so the caller lowers the clock
/// first and the ladder climbs again from there.
pub fn clock_to_base(log: fn(&str, u64)) -> bool {
    let base = CLK_BASE_FIELD.load(Ordering::Relaxed);
    let ok = emmc_clock_set(base, log);
    log("em: clk_base=", ((ok as u64) << 8) | base as u64);
    ok
}

/// EXT_CSD byte 185 is the bus timing the card will use.
const EXT_CSD_HS_TIMING: usize = 185;
/// EXT_CSD byte 192 is the table's revision. Checking it proves the 512
/// bytes really are the table and not whatever the FIFO held.
const EXT_CSD_REV: usize = 192;
/// JEDEC JESD84-B50 6.6.2: 1 selects the high-speed timing (52 MHz).
const HS_TIMING_HIGH_SPEED: u8 = 1;

/// Set once the card has been told high speed is allowed. The ladder only
/// climbs past 25 MHz when this is true, because above that a card still in
/// default speed simply stops answering.
static HS_MODE: AtomicU32 = AtomicU32::new(0);

/// Whether the card has been switched to high-speed timing.
pub fn hs_mode() -> bool {
    HS_MODE.load(Ordering::Relaxed) != 0
}

fn ext_csd_byte(table: &[u32; 128], index: usize) -> u8 {
    ((table[index / 4] >> ((index % 4) * 8)) & 0xFF) as u8
}

/// Ask the card to switch between default and high-speed timing.
///
/// This is a CMD6 write to one EXT_CSD byte, which is persistent on the card
/// until it is switched back - the same write every Linux boot makes. It is
/// deliberately *not* done at boot: a mistyped switch there would cost the
/// only recovery window this device has. `oem hs on|off` calls it.
///
/// The whole table is read before and after, so the result is what the card
/// says, not what we wrote. On any failure the data lines are reset; timing
/// is left as the read-back reported it, and 400 kHz still works either way.
pub fn switch_hs(on: bool, log: fn(&str, u64)) -> Result<u8, u32> {
    let mut table = [0u32; 128];
    let (ok, _s, e) = sdhci_data_read(8, 0, &mut table, log);
    if !ok {
        recover_lines(log);
        return Err(0x1_0000 | e);
    }
    let rev = ext_csd_byte(&table, EXT_CSD_REV);
    if rev < 4 {
        // Not a table we can trust; do not write to it.
        recover_lines(log);
        return Err(0x2_0000 | rev as u32);
    }
    let before = ext_csd_byte(&table, EXT_CSD_HS_TIMING);
    let want = if on { HS_TIMING_HIGH_SPEED } else { 0 };
    if before & 0x0F == want {
        HS_MODE.store(on as u32, Ordering::Relaxed);
        return Ok(before);
    }
    // MMC_SWITCH(access, index, value, cmdset): access 3 = write byte, and
    // the index sits in bits 23..16 while the value sits in 15..8
    // (include/linux/mmc/mmc.h).
    let arg: u32 = (3 << 26) | ((EXT_CSD_HS_TIMING as u32) << 16) | ((want as u32) << 8);
    let zeros = [0u32; 128];
    let (wok, _ws, we) = sdhci_data_write(6, arg, &zeros, log);
    if !wok {
        recover_lines(log);
        return Err(0x3_0000 | we);
    }
    // CMD6 answers R1b: the card holds DAT0 while it commits the byte.
    if !wait_idle(0x20_0000, log) {
        recover_lines(log);
        return Err(0x4_0000);
    }
    let mut after = [0u32; 128];
    let (rok, _rs, re) = sdhci_data_read(8, 0, &mut after, log);
    if !rok {
        recover_lines(log);
        return Err(0x5_0000 | re);
    }
    let now = ext_csd_byte(&after, EXT_CSD_HS_TIMING);
    log(
        "em: hs=",
        ((before as u64) << 16) | ((now as u64) << 8) | want as u64,
    );
    if now & 0x0F != want {
        recover_lines(log);
        return Err(0x6_0000 | now as u32);
    }
    HS_MODE.store(on as u32, Ordering::Relaxed);
    Ok(now)
}

/// EXT_CSD byte 183 is the bus width the card drives.
const EXT_CSD_BUS_WIDTH: usize = 183;

/// Program the host side of the bus width. SDHCI HOST_CONTROL1 (0x28):
/// bits 2:1 are the data transfer width (0 = 1 bit, 1 = 4 bits, 2 = 8 bits).
fn set_host_width(bits: u8) -> bool {
    let field = match bits {
        1 => 0u8,
        4 => 1,
        8 => 2,
        _ => return false,
    };
    let cur = rd8(SDHCI_BASE, 0x28);
    wr8(SDHCI_BASE, 0x28, (cur & !0x06) | (field << 1));
    true
}

fn host_width() -> u8 {
    match (rd8(SDHCI_BASE, 0x28) >> 1) & 0x03 {
        0 => 1,
        1 => 4,
        2 => 8,
        _ => 0,
    }
}

/// Move the card and the host to a bus width, and prove it with a read.
///
/// The order is the one the card can follow: CMD6 (a command plus a 512-byte
/// write, which is always accepted) changes what the card will drive, then
/// the host starts listening on that many lines, then LBA0 is read twice. If
/// either read fails the card is switched back *before* the host shrinks,
/// because a card driving eight lines into a host reading one is garbage in
/// both directions.
///
/// Like the timing switch this is explicit (`oem bus 1|4|8`), not automatic:
/// a wrong width costs every read on the bus, and the only recovery is the
/// loader.
pub fn bus_width_set(bits: u8, log: fn(&str, u64)) -> Result<u8, u32> {
    if !matches!(bits, 1 | 4 | 8) {
        return Err(0x1_0000 | bits as u32);
    }
    let mut table = [0u32; 128];
    let (ok, _s, e) = sdhci_data_read(8, 0, &mut table, log);
    if !ok {
        recover_lines(log);
        return Err(0x2_0000 | e);
    }
    let before = ext_csd_byte(&table, EXT_CSD_BUS_WIDTH) & 0x0F;
    let want = match bits {
        1 => 0u8,
        4 => 1,
        _ => 2,
    };
    log("em: bw=", ((before as u64) << 8) | want as u64);
    if before == want && host_width() == bits {
        return Ok(want);
    }
    let arg: u32 = (3 << 26) | ((EXT_CSD_BUS_WIDTH as u32) << 16) | ((want as u32) << 8);
    let zeros = [0u32; 128];
    let (wok, _ws, we) = sdhci_data_write(6, arg, &zeros, log);
    if !wok {
        recover_lines(log);
        return Err(0x3_0000 | we);
    }
    if !wait_idle(0x20_0000, log) {
        recover_lines(log);
        return Err(0x4_0000);
    }
    set_host_width(bits);
    let mut a = [0u32; 128];
    let mut b = [0u32; 128];
    let (ok_a, _sa, _ea) = sdhci_data_read(17, 0, &mut a, log);
    let (ok_b, _sb, _eb) = sdhci_data_read(17, 0, &mut b, log);
    if !ok_a || !ok_b || (a[127] & 0xFFFF) != 0xAA55 || a[0] != b[0] {
        // Put the card back while it and the host still agree on the width.
        let back: u32 = (3 << 26) | ((EXT_CSD_BUS_WIDTH as u32) << 16) | ((before as u32) << 8);
        let _ = sdhci_data_write(6, back, &zeros, log);
        let _ = wait_idle(0x20_0000, log);
        set_host_width(match before {
            0 => 1,
            1 => 4,
            _ => 8,
        });
        recover_lines(log);
        return Err(0x5_0000 | bits as u32);
    }
    log("em: bw_ok=", bits as u64);
    Ok(want)
}

pub fn write_sectors(lba: u32, data: &[u8], log: fn(&str, u64)) -> (u64, u64) {
    const CHUNK: usize = 1024; // sectors per CMD25 run (512 KiB)
    let mut ok = 0u64;
    let mut bad = 0u64;
    let total = (data.len() + 511) / 512;
    // v336d: a flash is a guest in a live boot - it must return. The PIO
    // write is fast when CMD25 takes, and ~1000x slower when it falls back
    // to one CMD24 per sector; unbounded, the fallback held the boot thread
    // (and with it every USB service pass) for as long as the card took.
    // v336m: the loop is bounded by the work, not by a clock. A deadline
    // here used uptime_us, and that counter runs about ten times fast on
    // this board (1271 "s" elapsed during an 85 s write), so it cut a
    // write that was making steady progress. Every chunk now lands or
    // fails fast on its own polls, and the host hears the result.
    // The frozen-boot lane stands down for the duration: a flash that
    // takes minutes is working, not stuck.
    crate::platform::common::recovery::set_busy(true);
    let mut i = 0usize;
    while i < total {
        // v336i: a flash is progress. Without this the frozen-boot lane
        // reads the minutes a 24 MB partition takes as a dead boot and
        // resets the machine in the middle of the write.
        crate::platform::common::recovery::boot_heartbeat();
        let nblocks = core::cmp::min(CHUNK, total - i);
        let off = i * 512;
        let nbytes = core::cmp::min(nblocks * 512, data.len() - off);
        let (m_ok, _ms, _me) = sdhci_data_write_multi(
            25,
            lba + i as u32,
            nblocks as u16,
            &data[off..off + nbytes],
            log,
        );
        if m_ok {
            ok += nblocks as u64;
        } else {
            // v336j: the first failed chunk's status/error words, so the
            // reply itself can carry them (the boot log block is full by
            // the time a flash runs, so `em:` lines never make it out).
            if LAST_WR.load(Ordering::Relaxed) == 0 {
                LAST_WR.store(
                    0x8000_0000 | ((_me & 0xFFFF) << 16) | (_ms & 0xFFFF),
                    Ordering::Relaxed,
                );
            }
            let mut k = 0usize;
            let mut consec = 0u32;
            while k < nblocks {
                crate::platform::common::recovery::boot_heartbeat();
                let o = off + k * 512;
                let n = core::cmp::min(512, data.len() - o);
                let mut words = [0u32; 128];
                pack_words(&mut words, &data[o..o + n]);
                let (d, _s2, _e2) = sdhci_data_write(24, lba + (i + k) as u32, &words, log);
                if d {
                    ok += 1;
                    consec = 0;
                } else {
                    bad += 1;
                    consec += 1;
                    // v336u: a card that refuses eight single-sector writes
                    // in a row is busy or gone, not unlucky. The old bound
                    // was 64 blocks, and the 436-sector tail of a failed
                    // chunk spent minutes in that storm.
                    if consec >= 8 {
                        log("em: sb_gone=", consec as u64);
                        break;
                    }
                }
                // v336d: the single-block fallback is the slow lane - one
                // CMD24 per sector with its own buffer waits. It reported
                // nothing between chunks, so a fallback that ran for
                // minutes looked exactly like a dead device.
                if k % 64 == 0 {
                    log("em: wr_sb=", k as u64);
                }
                k += 1;
            }
            // Blocks the fallback never reached (it broke on eight in a row)
            // are not successes: count them so ok + bad is always the number
            // of sectors the host sent.
            if k < nblocks {
                bad += (nblocks - k) as u64;
            }
            // v336d: if the first 64 single-sector writes all failed, the
            // lane is not going to recover mid-chunk; the remaining blocks
            // count as bad and the caller fails fast (the host waits).
            if bad >= 64 {
                log("em: wr_bail=", bad);
                bad += (total - i - nblocks) as u64;
                break;
            }
        }
        // v336d: every chunk reports. A 1.2 MB kernel is ~5 chunks, and a
        // wedged flash used to leave the panel silent (the old stride of
        // 8192 sectors is longer than the whole image, so it never printed
        // once). A flash that stalls must be visible in the same photograph
        // that shows the stall.
        log("em: wr_prog=", i as u64);
        i += nblocks;
    }
    crate::platform::common::recovery::set_busy(false);
    LAST_FLASH.store((ok << 32) | (bad & 0xFFFF_FFFF), Ordering::Relaxed);
    (ok, bad)
}

/// Pack `data` (up to 512 bytes) into little-endian words, zero-padding
/// the tail of the final word.
pub(super) fn pack_words(words: &mut [u32; 128], data: &[u8]) {
    let n = core::cmp::min(512, data.len());
    let mut b = 0usize;
    while b < n {
        let mut w = 0u32;
        let mut k = 0usize;
        while k < 4 && b + k < n {
            w |= (data[b + k] as u32) << (8 * k);
            k += 1;
        }
        words[b / 4] = w;
        b += 4;
    }
}

/// Multi-block PIO write (CMD25): one command for `blocks` sectors of
/// `data` (blocks*512 bytes available), then the CMD12 stop the card needs
/// to close the burst. Transfer mode = BLK_CNT_EN | MULTI_BLOCK (0x22).
/// Reports (complete, normal_int, error_status).
/// v336s: a multi-block write programs flash cells for a long time after
/// the last block has left the host, and the card holds DAT0 low until it
/// is done. The controller reports that as DAT_INHIBIT (PRESENT_STATE bit
/// 1) plus CMD_INHIBIT (bit 0). A CMD25 issued into that window gets no
/// response at all and fails as CMD_TIMEOUT - which is exactly what killed
/// the last 436 sectors of a 1.2 MB flash while the two full chunks before
/// it went through.
pub(super) fn wait_idle(limit: u32, log: fn(&str, u64)) -> bool {
    for n in 0..limit {
        let ps = rd32(SDHCI_BASE, 0x24);
        if ps & 0x3 == 0 {
            return true;
        }
        if n & 0xFFF == 0 {
            crate::platform::common::recovery::boot_heartbeat();
        }
    }
    log("em: idle_timeout=", rd32(SDHCI_BASE, 0x24) as u64);
    false
}

pub(super) fn sdhci_data_write_multi(
    idx: u16,
    arg: u32,
    blocks: u16,
    data: &[u8],
    log: fn(&str, u64),
) -> (bool, u32, u32) {
    // v336s: never start a data phase while the card is still programming
    // the previous one.
    wait_idle(0x20_0000, log);
    wr16(SDHCI_BASE, 0x04, 512);
    wr16(SDHCI_BASE, 0x06, blocks);
    // v336e: TIMEOUT_CONTROL (0x2E) was never programmed, so it sat at its
    // reset value and gave the card the shortest data-timeout in the book.
    // A read answers immediately; a write has to programme flash cells, and
    // at this clock that is far slower - every write aborted with a data
    // timeout while every read passed. 0x0E is the maximum (2^27 TMCLK).
    wr8(SDHCI_BASE, 0x2E, 0x0E);
    wr16(SDHCI_BASE, 0x0C, 0x22); // BLK_CNT_EN | MULTI_BLOCK, WRITE, no DMA
    wr32(SDHCI_BASE, 0x08, arg);
    wr16(
        SDHCI_BASE,
        0x0E,
        (idx << 8) | 0x02 | CMD_CRC | CMD_INDEX | CMD_DATA,
    );
    let mut sent = 0u64;
    let mut blk = 0usize;
    'blocks: while blk < blocks as usize {
        // v336k: the buffer-write gate is per BLOCK, not per word. Asking
        // the controller once per 32-bit word cost ~100x: a 512 KiB chunk
        // spent longer in MMIO polling than the card spent programming,
        // and a 1.2 MB flash never finished inside the deadline.
        let mut ready = false;
        for _ in 0..0x40_000u32 {
            let ps = rd32(SDHCI_BASE, 0x24);
            if ps & 0x400 != 0 {
                ready = true;
                break;
            }
            if rd32(SDHCI_BASE, 0x30) & 0x8000 != 0 {
                break;
            }
        }
        if !ready {
            break 'blocks;
        }
        for j in 0..128usize {
            let wi = blk * 128 + j;
            let off = wi * 4;
            let mut w = 0u32;
            let mut k = 0usize;
            while k < 4 && off + k < data.len() {
                w |= (data[off + k] as u32) << (8 * k);
                k += 1;
            }
            wr32(SDHCI_BASE, 0x20, w);
            sent += 1;
        }
        blk += 1;
    }
    log("em: c25_sent=", sent);
    let mut sts = 0u32;
    let mut err = 0u32;
    for _ in 0..0x80_0000u32 {
        sts = rd32(SDHCI_BASE, 0x30);
        if sts & (0x2 | 0x8000) != 0 {
            err = rd16(SDHCI_BASE, 0x32) as u32;
            break;
        }
    }
    wr32(SDHCI_BASE, 0x30, 0xFFFF_FFFF);
    // Close the burst: CMD12 with the 48-bit-busy response type, exactly
    // as the vendor's stop path (mmc_stop: R1b). The card programs until
    // it releases DAT0; every later command waits for that anyway.
    let (d12, s12, e12, _it) = sdhci_cmd(12, 0, 3, CMD_CRC | CMD_INDEX);
    log(
        "em: c25_d12=",
        ((d12 as u64) << 32) | ((s12 & 0xFFFF) as u64),
    );
    log("em: c25_d12e=", e12 as u64);
    // v336s: let the card finish programming before the next chunk asks.
    let idle = wait_idle(0x100_0000, log);
    log("em: c25_idle=", idle as u64);
    if sts & 0x2 == 0 {
        recover_lines(log);
    }
    (sts & 0x2 != 0, sts, err)
}

/// Erase sectors [first, last] inclusive (block addresses), the vendor's
/// mmc_do_erase order: CMD35 (ERASE_GROUP_START, plain R1), CMD36
/// (ERASE_GROUP_END, plain R1), CMD38 (ERASE, R1b - the card holds DAT0
/// busy while it programs the erase, and the controller makes every
/// following command wait for that). Reports (ok, error_status).
pub fn erase_sectors(first: u32, last: u32, log: fn(&str, u64)) -> (bool, u32) {
    let (d35, _s35, e35, _) = sdhci_cmd(35, first, 2, CMD_CRC | CMD_INDEX);
    let (d36, _s36, e36, _) = sdhci_cmd(36, last, 2, CMD_CRC | CMD_INDEX);
    log("em: er35=", d35 as u64);
    log("em: er36=", d36 as u64);
    if !d35 || !d36 {
        return (false, if !d35 { e35 } else { e36 });
    }
    // CMD38 with a long bound: a real erase runs for seconds behind the
    // R1b response, beyond sdhci_cmd's ~0.1 s budget.
    wr32(SDHCI_BASE, 0x08, 0);
    wr16(SDHCI_BASE, 0x0E, (38u16 << 8) | 3 | CMD_CRC | CMD_INDEX);
    let mut done = false;
    let mut sts = 0u32;
    let mut err = 0u32;
    for _ in 0..0x100_0000u32 {
        sts = rd32(SDHCI_BASE, 0x30);
        if sts & 0x8000 != 0 {
            err = rd16(SDHCI_BASE, 0x32) as u32;
            break;
        }
        if sts & 0x1 != 0 {
            done = true;
            break;
        }
    }
    wr32(SDHCI_BASE, 0x30, 0xFFFF_FFFF);
    log("em: er38=", ((done as u64) << 32) | ((sts & 0xFFFF) as u64));
    log("em: er38e=", err as u64);
    if !done {
        return (false, err);
    }
    // The card's READY_FOR_DATA (R1 bit 8) confirms the erase retired.
    let (d13, _s13, _e13, _) = sdhci_cmd(13, 1 << 16, 2, CMD_CRC | CMD_INDEX);
    let r0 = rd32(SDHCI_BASE, 0x10);
    log("em: er13=", ((d13 as u64) << 32) | ((r0 & 0xFFFF) as u64));
    ((r0 & 0x100) != 0, err)
}

/// The eMMC as a genome BlockDevice: a thin synchronous view over the PIO
/// paths, registered as "emmc0" so the devfs / fs layers can lease it like
/// any other disk.
#[cfg(fullerene_aarch64_ane)]
pub struct EmmcBlockDevice {
    pub total: u64,
}

#[cfg(fullerene_aarch64_ane)]
fn block_no_log(_: &str, _: u64) {}

#[cfg(fullerene_aarch64_ane)]
impl genome::block::BlockDevice for EmmcBlockDevice {
    fn read_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf: &mut [u8],
    ) -> Result<(), genome::block::BlockError> {
        let need = (count as usize) * 512;
        if buf.len() < need {
            return Err(genome::block::BlockError::BufferTooSmall {
                required: need,
                provided: buf.len(),
            });
        }
        if lba.saturating_add(count as u64) > u32::MAX as u64 {
            return Err(genome::block::BlockError::LbaOverflow);
        }
        let mut l = lba as u32;
        let mut off = 0usize;
        for _ in 0..count {
            let mut words = [0u32; 128];
            let (ok, _s, _e) = sdhci_data_read(17, l, &mut words, block_no_log);
            if !ok {
                return Err(genome::block::BlockError::Device);
            }
            for (i, w) in words.iter().enumerate() {
                buf[off + i * 4..off + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            off += 512;
            l += 1;
        }
        Ok(())
    }

    fn write_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf: &[u8],
    ) -> Result<(), genome::block::BlockError> {
        let need = (count as usize) * 512;
        if buf.len() < need {
            return Err(genome::block::BlockError::BufferTooSmall {
                required: need,
                provided: buf.len(),
            });
        }
        if lba.saturating_add(count as u64) > u32::MAX as u64 {
            return Err(genome::block::BlockError::LbaOverflow);
        }
        let (ok, bad) = write_sectors(lba as u32, &buf[..need], block_no_log);
        if bad != 0 || ok == 0 {
            return Err(genome::block::BlockError::Device);
        }
        Ok(())
    }

    fn sector_size(&self) -> u32 {
        512
    }

    fn total_sectors(&self) -> u64 {
        self.total
    }
}

/// Probe the eMMC for the block layer: read SEC_COUNT (EXT_CSD bytes
/// 212..215, word 53), build an `EmmcBlockDevice`, and read sector 0
/// through the genome BlockDevice trait - the exact path the fs layers
/// will use once this bin links the devfs registry (the boot-stage
/// aarch64 bin deliberately carries a small module set).
#[cfg(fullerene_aarch64_ane)]
pub fn emmc_block_probe(log: fn(&str, u64)) {
    let mut xs = [0u32; 128];
    let (ok, _s, _e) = sdhci_data_read(8, 0, &mut xs, log);
    if !ok {
        log("em: blk_noext=", 1);
        return;
    }
    let secs = (xs[53] & 0xFFFF_FFFF) as u64;
    log("em: blk_secs=", secs);
    let mut dev = EmmcBlockDevice { total: secs };
    let mut mbr = [0u8; 512];
    match genome::block::BlockDevice::read_sectors(&mut dev, 0, 1, &mut mbr) {
        Ok(()) => {
            let sig = ((mbr[510] as u64) << 8) | mbr[511] as u64;
            log("em: blk_rd=", sig);
        }
        Err(_) => log("em: blk_rd=", 0),
    }
    // v97: the genome GPT parser over the same device - the exact path the
    // fullerene fs stack uses. Fingerprint = count + the first names'
    // leading bytes + the usable-LBA range.
    let mut dev2 = EmmcBlockDevice { total: secs };
    match genome::gpt::scan(&mut dev2) {
        Ok(table) => {
            log("em: gpt_n=", table.partitions.len() as u64);
            log(
                "em: gpt_usable=",
                (table.header.first_usable_lba << 32) | table.header.last_usable_lba,
            );
            let mut h = 0u64;
            for (i, p) in table.partitions.iter().take(3).enumerate() {
                let mut q = 0u64;
                for b in p.name().bytes().take(8) {
                    q = (q << 8) | b as u64;
                }
                log("em: gpt_p=", ((i as u64) << 32) | (q & 0xFFFF_FFFF));
                h = h
                    .wrapping_add(q)
                    .wrapping_add(p.first_lba)
                    .wrapping_add(p.last_lba);
            }
            log("em: gpt_h=", h);
        }
        Err(_) => log("em: gpt_e=", 1),
    }
}

/// A 512-byte-sector view of one partition range on the eMMC.
#[cfg(fullerene_aarch64_ane)]
struct PartView {
    base: u64,
    total: u64,
}

#[cfg(fullerene_aarch64_ane)]
impl genome::block::BlockDevice for PartView {
    fn read_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf: &mut [u8],
    ) -> Result<(), genome::block::BlockError> {
        let mut inner = EmmcBlockDevice { total: 0 };
        genome::block::BlockDevice::read_sectors(&mut inner, self.base + lba, count, buf)
    }
    fn write_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf: &[u8],
    ) -> Result<(), genome::block::BlockError> {
        let mut inner = EmmcBlockDevice { total: 0 };
        genome::block::BlockDevice::write_sectors(&mut inner, self.base + lba, count, buf)
    }
    fn sector_size(&self) -> u32 {
        512
    }
    fn total_sectors(&self) -> u64 {
        self.total
    }
}

/// v98: mount the ext4 filesystem on the "system" partition through the
/// genome stack (Ext4FileSystem over a PartView) and read the first bytes
/// of /build.prop - the first filesystem access from this kernel.
#[cfg(fullerene_aarch64_ane)]
pub fn emmc_ext4_probe(secs: u64, log: fn(&str, u64)) {
    let mut dev = EmmcBlockDevice { total: secs };
    let table = match genome::gpt::scan(&mut dev) {
        Ok(t) => t,
        Err(_) => {
            log("em: ex_gpt=", 0);
            return;
        }
    };
    let p = match table.partitions.iter().find(|q| q.name() == "system") {
        Some(p) => *p,
        None => {
            log("em: ex_part=", 0);
            return;
        }
    };
    log("em: ex_lba=", p.first_lba);
    let view = PartView {
        base: p.first_lba,
        total: p.last_lba - p.first_lba + 1,
    };
    match genome::ext4::Ext4FileSystem::new(alloc::boxed::Box::new(view)) {
        Ok(mut fs) => {
            log("em: ex_new=", 1);
            use genome::vfs::FileSystem;
            // v99: the directory listing and a full-file read (FNV-1a 64).
            match fs.readdir("/") {
                Ok(entries) => {
                    log("em: ex_n=", entries.len() as u64);
                    let mut h = 0u64;
                    for e in entries.iter().take(8) {
                        let mut q = 0u64;
                        for b in e.name.bytes().take(6) {
                            q = (q << 8) | b as u64;
                        }
                        h = h.wrapping_mul(31).wrapping_add(q);
                    }
                    let dirs = entries.iter().filter(|e| e.is_dir).count() as u64;
                    log("em: ex_dirs=", dirs);
                    log("em: ex_h=", h);
                }
                Err(_) => log("em: ex_n=", u64::MAX),
            }
            match fs.open("/build.prop", 0) {
                Some(f) => {
                    let fd = f.fd;
                    log("em: ex_open=", 1);
                    let mut total = 0u64;
                    let mut hash = 0xcbf2_9ce4_8422_2325u64;
                    let mut buf = [0u8; 256];
                    loop {
                        match fs.read(fd, &mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                for &b in buf[..n].iter() {
                                    hash ^= b as u64;
                                    hash = hash.wrapping_mul(0x100_0000_01b3);
                                }
                                total += n as u64;
                                if total > 65536 {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let _ = fs.close(fd);
                    log("em: ex_len=", total);
                    log("em: ex_fnv=", hash);
                }
                None => log("em: ex_open=", 0),
            }
        }
        Err(_) => log("em: ex_new=", 0),
    }
}

/// v101: identify the "userdata" partition's filesystem by RAW superblock
/// magic only (ext4 0xEF53 @ 0x438, f2fs 0xF2F52010 @ 0x400, erofs @
/// 0x400). No mount: the v100 full mount of a ~20 GB f2fs/erofs partition
/// is suspected of exhausting the small boot heap and killing an idle
/// wait loop (deferred OOM), so this stays allocation-free apart from the
/// sector buffers.
#[cfg(fullerene_aarch64_ane)]
pub fn emmc_userdata_probe(secs: u64, log: fn(&str, u64)) {
    let mut dev = EmmcBlockDevice { total: secs };
    let table = match genome::gpt::scan(&mut dev) {
        Ok(t) => t,
        Err(_) => {
            log("em: ud_gpt=", 0);
            return;
        }
    };
    let p = match table.partitions.iter().find(|q| q.name() == "userdata") {
        Some(p) => *p,
        None => {
            log("em: ud_part=", 0);
            return;
        }
    };
    log("em: ud_lba=", p.first_lba);
    let mut dev2 = EmmcBlockDevice { total: secs };
    let mut sb = [0u8; 512];
    if genome::block::BlockDevice::read_sectors(&mut dev2, p.first_lba + 2, 1, &mut sb).is_err() {
        log("em: ud_rd=", 0);
        return;
    }
    // sector 2 of the partition = bytes 1024..1536; f2fs/erofs magic at
    // 0x400 (offset 0), ext4 magic 0xEF53 at 0x438 (offset 0x38).
    let f2fs = u32::from_le_bytes([sb[0], sb[1], sb[2], sb[3]]);
    let erofs_le = u32::from_le_bytes([sb[0], sb[1], sb[2], sb[3]]);
    let ext4 = u16::from_le_bytes([sb[0x38], sb[0x39]]);
    log("em: ud_f2fs=", f2fs as u64);
    log("em: ud_erofs_le=", erofs_le as u64);
    log("em: ud_ext4=", ext4 as u64);
}

/// On-demand userdata mount (the `oem udmount` command). Deliberately NOT
/// in the boot path: the v102 attempt crash-looped the box (mounting the
/// real 20 GB F2FS "userdata" panicked even on the 8 MiB heap, and a
/// boot-time mount re-triggered on every reboot). On demand a crash
/// self-limits - the next boot starts clean and does not remount.
#[cfg(fullerene_aarch64_ane)]
pub fn emmc_userdata_mount(log: fn(&str, u64)) {
    let mut xs = [0u32; 128];
    let (ok, _s, _e) = sdhci_data_read(8, 0, &mut xs, log);
    if !ok {
        log("em: udm_ext=", 0);
        return;
    }
    let secs = (xs[53] & 0xFFFF_FFFF) as u64;
    let mut dev = EmmcBlockDevice { total: secs };
    let table = match genome::gpt::scan(&mut dev) {
        Ok(t) => t,
        Err(_) => {
            log("em: udm_gpt=", 0);
            return;
        }
    };
    let p = match table.partitions.iter().find(|q| q.name() == "userdata") {
        Some(p) => *p,
        None => {
            log("em: udm_part=", 0);
            return;
        }
    };
    log("em: udm_lba=", p.first_lba);
    let view = PartView {
        base: p.first_lba,
        total: p.last_lba - p.first_lba + 1,
    };
    match genome::android_fs::mount(alloc::boxed::Box::new(view)) {
        Ok((mut fs, kind)) => {
            let k = match kind {
                genome::android_fs::AndroidFilesystemKind::Ext4 => 1u64,
                genome::android_fs::AndroidFilesystemKind::Erofs => 2u64,
                genome::android_fs::AndroidFilesystemKind::F2fs => 3u64,
            };
            log("em: udm_kind=", k);
            use genome::vfs::FileSystem;
            match fs.readdir("/") {
                Ok(entries) => {
                    log("em: udm_n=", entries.len() as u64);
                    let mut h = 0u64;
                    for e in entries.iter().take(6) {
                        let mut q = 0u64;
                        for b in e.name.bytes().take(6) {
                            q = (q << 8) | b as u64;
                        }
                        h = h.wrapping_mul(31).wrapping_add(q);
                    }
                    log("em: udm_h=", h);
                }
                Err(_) => log("em: udm_n=", u64::MAX),
            }
        }
        Err(_) => log("em: udm_kind=", 0),
    }
}

/// Compact sector scanner: for each sector in [lba, lba+count), skip
/// entirely if all-zero; otherwise log the LBA, a 128-bit non-zero-word
/// bitmap (two u64s) and every non-zero word as (index << 32) | value.
/// Far fewer log lines than `dump_sectors`, so the ring stream catches
/// everything (the reader drops lines on dense dumps).
pub fn scan_sectors(lba: u32, count: u32, log: fn(&str, u64)) {
    let mut words = [0u32; 128];
    for s in 0..count {
        let l = lba.wrapping_add(s);
        let (ok, _st, _e) = sdhci_data_read(17, l, &mut words, log);
        if !ok {
            log("em: scan_bad=", l as u64);
            return;
        }
        let mut bm0 = 0u64;
        let mut bm1 = 0u64;
        for i in 0..64 {
            if words[i] != 0 {
                bm0 |= 1u64 << i;
            }
        }
        for i in 64..128 {
            if words[i] != 0 {
                bm1 |= 1u64 << (i - 64);
            }
        }
        if bm0 == 0 && bm1 == 0 {
            continue;
        }
        log("em: nz_lba=", l as u64);
        log("em: nz_bm0=", bm0);
        log("em: nz_bm1=", bm1);
        for i in 0..128 {
            if words[i] != 0 {
                log("em: nz_w=", ((i as u64) << 32) | words[i] as u64);
            }
        }
    }
    log("em: scan_done=", count as u64);
}

/// `dump_sectors`: read `count` sectors and log them as hex lines
/// (16 words of 8 hex digits per line, 8 lines per sector). The host-side
/// decoder (`dump_decode.py`) reconstructs the bytes. For BFM forensics:
/// misc @ 0x7C000 (507904), bootfail_info @ 0x1E8000 (1998848), oeminfo @
/// 0x8000 (32768).
#[cfg(fullerene_aarch64_ane)]
pub fn dump_sectors(lba: u32, count: u32, log: fn(&str, u64)) {
    let mut words = [0u32; 128];
    for s in 0..count {
        let l = lba.wrapping_add(s);
        let (ok, _st, _e) = sdhci_data_read(17, l, &mut words, log);
        log("em: dump_lba=", l as u64);
        log("em: dump_ok=", ok as u64);
        if !ok {
            return;
        }
        for row in 0..64 {
            // 64 lines per sector: each line = 2 words (8 bytes) packed
            // big-endian into one u64, so the printed hex digits are the
            // sector's byte sequence in order.
            let pair = row * 2;
            let w0 = words[pair] as u64;
            let w1 = words[pair + 1] as u64;
            let v = (w0 << 32) | w1;
            log("em: dd=", v);
        }
    }
}

/// One SDHCI command. `resp` = the response-type field (0 = none, 1 =
/// 136-bit R2, 2 = 48-bit, 3 = 48-bit with busy); `extra` = any of
/// CMD_CRC / CMD_INDEX / CMD_DATA. Polls NORMAL_INT_STATUS for
/// command-complete (bit0) or error (bit15), acks every latched bit, and
/// reports (complete, status, error status, poll iterations spent). The
/// bound is generous (~100 ms).
pub(super) fn sdhci_cmd(idx: u16, arg: u32, resp: u16, extra: u16) -> (bool, u32, u32, u32) {
    wr32(SDHCI_BASE, 0x08, arg);
    let mut cmd: u16 = idx << 8;
    cmd |= resp & 0x3;
    cmd |= extra & 0xF8;
    wr16(SDHCI_BASE, 0x0E, cmd);
    let mut done = false;
    let mut iters = 0u32;
    for i in 0..0x80_0000u32 {
        // v174: this loop is the boot's silent grind - keep the loader
        // watchdog fed while waiting on the card.
        if i & 0xFFF == 0 {
            super::ane::watchdog_tend();
        }
        let s = rd32(SDHCI_BASE, 0x30);
        iters = i;
        if s & (0x1 | 0x8000) != 0 {
            done = s & 0x1 != 0;
            break;
        }
    }
    let sts = rd32(SDHCI_BASE, 0x30);
    // 0x32 is the 16-bit error status; read it as 16 bits (a 32-bit
    // volatile access at this odd offset is an alignment fault on device
    // memory - this line crashed v60 and v61).
    let err = rd16(SDHCI_BASE, 0x32) as u32;
    wr32(SDHCI_BASE, 0x30, 0xFFFF_FFFF); // ack every latched bit
    (done, sts, err, iters)
}
