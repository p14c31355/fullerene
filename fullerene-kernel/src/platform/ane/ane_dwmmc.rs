//! dwmmc1 (MicroSDXC) bring-up for the ANE - phase B: power and clocks.
//!
//! Plan + measured facts: `p20-root/sd-recon/DWMMC1_BRINGUP.md` (vendor
//! `dw_mmc_hi3xxx.c` / `dw_mmc.c` / `hisi_regulator.c`, hi6250). Phase A
//! (recon, v45) measured the loader's state on the panel:
//!   - CRG gates PEREN0/PEREN4 = 0 (dwmmc1 is NOT clocked; its reset is not
//!     released either) - the loader never brought the SD controller up;
//!   - PMIC: vqmmc (LDO9) on with selector 5 = 2.95 V; vmmc (LDO16) OFF;
//!   - CRG divider sc_div_sd = 6 (19.2 MHz / 6 = 3.2 MHz = the vendor's ciu
//!     input for 400 kHz legacy, whose card ratio is 8:1).
//!
//! Phase B follows the vendor's own order (dw_mci_hs_probe -> set_controller
//! -> setup_clock -> set_ios POWER_UP):
//!   1. open the CRG gates (PEREN0 bit30 hclk_sd, PEREN4 bit17 clk_sd);
//!   2. release the reset (PERRSTDIS4 bit18);
//!   3. set the rails: vqmmc = LDO9 (vset 0x46 sel 5 = 2.95 V, ctrl 0x1D
//!      bit0), then vmmc = LDO16 (vset 0x4D sel 6 = 2.95 V, ctrl 0x24 bit0),
//!      with the vendor's inter-enable gap (the PMIC wants a distance
//!      between enable writes);
//!   4. only then read the controller block (an unclocked peripheral read
//!      stalls this fabric - v39's lesson).
//!
//! Every step paints a row below the fastboot strip; the photo is the
//! record, the durable log carries the same values.

use super::common::mmio::{rd32, wr32};
use super::common::pmic::{pmic_rd, pmic_rmw, pmic_wr};

const CRG_BASE: usize = 0xFFF3_5000;
const SCTRL_BASE: usize = 0xFFF0_A000;
/// dwmmc1 controller.
const MMC_BASE: usize = 0xFF37_F000;

/// Bounded busy wait (the vendor's mdelay substitutes; ane.rs's usb_spin
/// scale: ~0x80000 units is about a millisecond).
fn spin(units: u32) {
    for _ in 0..units {
        core::hint::spin_loop();
    }
}

/// Busy wait that keeps the USB responder serviced while it waits. The host
/// enumerates during this boot phase (the SD bring-up runs between the USB
/// init and the wait loop); control requests left unanswered here piled up
/// in the core and produced the "one request behind" enumeration break of
/// v49/v52. Sliced so service() runs at least every ~1 ms of waiting.
fn spin_usb(units: u32, log: fn(&str, u64)) {
    let mut left = units as u64;
    while left > 0 {
        let n = if left > 0x2_0000 {
            0x2_0000
        } else {
            left as u32
        };
        spin(n);
        super::ane_usb_dev::service(log);
        left -= n as u64;
    }
}

const HEX: [u8; 16] = *b"0123456789ABCDEF";

fn put(buf: &mut [u8; 44], at: usize, s: &[u8]) -> usize {
    let n = s.len().min(buf.len() - at);
    buf[at..at + n].copy_from_slice(&s[..n]);
    at + n
}

fn hex8(buf: &mut [u8; 44], at: usize, v: u32) -> usize {
    for i in 0..8 {
        buf[at + i] = HEX[((v >> (28 - 4 * i)) & 0xF) as usize];
    }
    at + 8
}

fn hex2(buf: &mut [u8; 44], at: usize, v: u8) -> usize {
    buf[at] = HEX[(v >> 4) as usize];
    buf[at + 1] = HEX[(v & 0xF) as usize];
    at + 2
}

fn paint2(row: u32, label: &[u8], a: u32) {
    let mut l = [b' '; 44];
    let at = put(&mut l, 0, label);
    hex8(&mut l, at, a);
    super::ane_fb::paint(row, &l);
}

/// Phase B: clocks, reset, rails - then the controller identity reads.
pub fn bringup(log: fn(&str, u64)) {
    // Snapshot before (the phase-A rows stay; B starts at row 5).
    let peren0 = rd32(CRG_BASE, 0x000);
    let peren4 = rd32(CRG_BASE, 0x040);
    let rstdis = rd32(CRG_BASE, 0x094);

    // 1. Open the gates (vendor: clk-hi3xxx sets these for clk_sd/hclk_sd).
    wr32(CRG_BASE, 0x000, peren0 | (1 << 30));
    wr32(CRG_BASE, 0x040, peren4 | (1 << 17));
    // 2. Release the controller reset.
    wr32(CRG_BASE, 0x094, rstdis | (1 << 18));
    spin_usb(0x8_0000, log); // ~1 ms settle
    paint2(5, b"SD5 PEREN0=", rd32(CRG_BASE, 0x000));
    paint2(6, b"SD6 PEREN4=", rd32(CRG_BASE, 0x040));
    paint2(7, b"SD7 RSTDIS=", rd32(CRG_BASE, 0x094));
    log("sd: b_peren0=", rd32(CRG_BASE, 0x000) as u64);
    log("sd: b_peren4=", rd32(CRG_BASE, 0x040) as u64);
    log("sd: b_rstdis=", rd32(CRG_BASE, 0x094) as u64);

    // 3. Rails, in the vendor's POWER_UP order: vqmmc first, gap, vmmc.
    //    vqmmc (LDO9): selector 5 = 2.95 V, then enable.
    pmic_rmw(0x46, 0x07, 5);
    pmic_rmw(0x1D, 0x01, 1);
    spin_usb(0x2_0000, log); // inter-enable gap (vendor protects back-to-back writes)
    //    vmmc (LDO16): selector 6 = 2.95 V, then enable.
    pmic_rmw(0x4D, 0x07, 6);
    pmic_rmw(0x24, 0x01, 1);
    spin_usb(0x8_0000, log); // ~1 ms for the rail to come up
    let l9c = pmic_rd(0x1D);
    let l9v = pmic_rd(0x46);
    let l16c = pmic_rd(0x24);
    let l16v = pmic_rd(0x4D);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SD8 L16C=");
    at = hex2(&mut l, at, l16c);
    at = put(&mut l, at, b" V=");
    at = hex2(&mut l, at, l16v);
    at = put(&mut l, at, b" L9C=");
    at = hex2(&mut l, at, l9c);
    at = put(&mut l, at, b" V=");
    hex2(&mut l, at, l9v);
    super::ane_fb::paint(8, &l);
    log("sd: b_l16c=", l16c as u64);
    log("sd: b_l16v=", l16v as u64);
    log("sd: b_l9c=", l9c as u64);
    log("sd: b_l9v=", l9v as u64);

    // 4. The controller block is clocked now; read its identity first
    //    (still no writes to it - that is phase C).
    super::ane_fb::paint(9, b"SD9 MMC READ..."); // hang bisector: photo names it
    let verid = rd32(MMC_BASE, 0x06C);
    let hcon = rd32(MMC_BASE, 0x070);
    let cdetect = rd32(MMC_BASE, 0x050);
    let ctrl = rd32(MMC_BASE, 0x000);
    let pwren = rd32(MMC_BASE, 0x004);
    let clkdiv = rd32(MMC_BASE, 0x008);
    let clkena = rd32(MMC_BASE, 0x010);
    let ctype = rd32(MMC_BASE, 0x018);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SDA VERID=");
    at = hex8(&mut l, at, verid);
    at = put(&mut l, at, b" HCON=");
    hex8(&mut l, at, hcon);
    super::ane_fb::paint(10, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SDB CD=");
    at = hex8(&mut l, at, cdetect);
    at = put(&mut l, at, b" CTRL=");
    hex8(&mut l, at, ctrl);
    super::ane_fb::paint(11, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SDC PWREN=");
    at = hex8(&mut l, at, pwren);
    at = put(&mut l, at, b" CLKDIV=");
    hex8(&mut l, at, clkdiv);
    super::ane_fb::paint(12, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SDD CLKENA=");
    at = hex8(&mut l, at, clkena);
    at = put(&mut l, at, b" CTYPE=");
    hex8(&mut l, at, ctype);
    super::ane_fb::paint(13, &l);
    log("sd: b_verid=", verid as u64);
    log("sd: b_hcon=", hcon as u64);
    log("sd: b_cdetect=", cdetect as u64);
    log("sd: b_ctrl=", ctrl as u64);
    log("sd: b_pwren=", pwren as u64);
    log("sd: b_clkdiv=", clkdiv as u64);
    log("sd: b_clkena=", clkena as u64);
    log("sd: b_ctype=", ctype as u64);

    // Push everything to the scan-out targets (canvas-only edits are
    // invisible when the panel scans a window - v45's lesson).
    super::ane_fb::flush_panel();
    log("sd: bringup_done=", 1);

    // Stage 2: register writes + the clock-latch dance, no commands yet
    // (bisect: v47/v49's USB died somewhere in the full sequence).
    card_init(log, 3);
}

// ---- phase C: controller init + the first card commands -----------------

const MMC_CTRL: usize = 0x000;
const MMC_PWREN: usize = 0x004;
const MMC_CLKDIV: usize = 0x008;
const MMC_CLKSRC: usize = 0x00C;
const MMC_CLKENA: usize = 0x010;
const MMC_TMOUT: usize = 0x014;
const MMC_CTYPE: usize = 0x018;
const MMC_BLKSIZ: usize = 0x01C;
const MMC_BYTCNT: usize = 0x020;
const MMC_INTMASK: usize = 0x024;
const MMC_CMDARG: usize = 0x028;
const MMC_CMD: usize = 0x02C;
const MMC_RESP0: usize = 0x030;
const MMC_RESP1: usize = 0x034;
const MMC_RESP3: usize = 0x03C;
const MMC_RINTSTS: usize = 0x044;
const MMC_FIFOTH: usize = 0x04C;
const MMC_GPIO: usize = 0x058;
const MMC_UHS_REG: usize = 0x074;
const MMC_CDTHRCTL: usize = 0x100;
const MMC_UHS_REG_EXT: usize = 0x108;
const MMC_ENABLE_SHIFT: usize = 0x110;
const MMC_FIFO: usize = 0x200;
/// PL061 GPIO bank (`gpio@fff0e000`) - the board's card detect pin 3.
const PL061_BASE: usize = 0xFFF0_E000;
/// RINTSTS bit 5: RX FIFO data request (dw_mmc.h:129).
const INT_RXDR: u32 = 1 << 5;

/// Poll the CMD register until the START bit clears (vendor mci_send_cmd).
/// Each read costs on the order of a microsecond over this bus, so 0x2_0000
/// iterations is about the vendor's 100 ms command timeout - and a card
/// that never answers bails there instead of crawling for minutes.
fn cmd_start_cleared() -> bool {
    for _ in 0..0x2_0000 {
        if rd32(MMC_BASE, MMC_CMD) & (1 << 31) == 0 {
            return true;
        }
    }
    false
}

/// One UPD_CLK pulse (START | UPD_CLK | PRV_DAT_WAIT; vendor setup_bus).
fn upd_clk() {
    wr32(MMC_BASE, MMC_CMD, (1 << 31) | (1 << 21) | (1 << 13));
    let _ = cmd_start_cleared();
}

/// UPD_CLK with the vendor's retry pattern (dw_mmc_hi3xxx.c:1683
/// mci_send_cmd): poll CMD.START; on timeout do a CTRL RESET wait and
/// retry, three tries of ~100 ms bounds. Returns the final CMD value.
fn upd_clk2(log: fn(&str, u64)) -> u32 {
    for _ in 0..3 {
        wr32(MMC_BASE, MMC_CMDARG, 0);
        wr32(MMC_BASE, MMC_CMD, (1 << 31) | (1 << 21) | (1 << 13));
        if cmd_start_cleared() {
            break;
        }
        wr32(MMC_BASE, MMC_CTRL, rd32(MMC_BASE, MMC_CTRL) | 1);
        for _ in 0..0x2_0000 {
            if rd32(MMC_BASE, MMC_CTRL) & 1 == 0 {
                break;
            }
        }
        // A failing poll spends ~100 ms; keep the device responder fed so
        // the host's enumeration never piles up behind this retry path.
        super::ane_usb_dev::service(log);
    }
    rd32(MMC_BASE, MMC_CMD)
}

/// Send one command with the vendor's flag assembly (dw_mmc.c:332-372 and
/// mci_send_cmd: CMDARG, then START|cmdr, poll until START clears).
/// Returns (start-cleared, RINTSTS after the command).
fn send_cmd(opcode: u32, arg: u32, resp_exp: bool, resp_long: bool, resp_crc: bool) -> (bool, u32) {
    wr32(MMC_BASE, MMC_CMDARG, arg);
    let mut cmdr = opcode | (1 << 13); // PRV_DAT_WAIT (always except stop)
    if resp_exp {
        cmdr |= 1 << 6;
        if resp_long {
            cmdr |= 1 << 7;
        }
    }
    if resp_crc {
        cmdr |= 1 << 8;
    }
    cmdr |= 1 << 29; // USE_HOLD_REG (the vendor always adds it)
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF); // start each command with a clean slate
    wr32(MMC_BASE, MMC_CMD, (1 << 31) | cmdr);
    let ok = cmd_start_cleared();
    let sts = rd32(MMC_BASE, MMC_RINTSTS);
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);
    (ok, sts)
}

/// Phase C: bring the controller up on the vendor's probe path and give the
/// card its first two commands (CMD0, CMD8). Every result lands on a panel
/// row; CMD8's resp0 == 0x1AA proves the card sees us.
///
/// `stage` gates the increments so a USB-breaking change can be bisected:
/// 1 = the register writes only, 2 = + the clock-latch dance,
/// 3 = + CMD0/CMD8 and the identification chain.
fn card_init(log: fn(&str, u64), stage: u32) {
    super::ane_fb::paint(14, b"SDE CTRL RESET...");
    // Controller reset: three resets, bounded wait (vendor dw_mci_probe).
    wr32(MMC_BASE, MMC_CTRL, 0x7);
    let mut rst_ok = false;
    for _ in 0..0x2_0000 {
        if rd32(MMC_BASE, MMC_CTRL) & 0x7 == 0 {
            rst_ok = true;
            break;
        }
    }
    // Fresh register state, vendor probe order.
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);
    wr32(MMC_BASE, MMC_INTMASK, 0);
    wr32(MMC_BASE, MMC_TMOUT, 0xFFFF_FFFF);
    wr32(MMC_BASE, MMC_FIFOTH, (0x2 << 28) | (127 << 16) | 128);
    wr32(MMC_BASE, MMC_CLKENA, 0);
    wr32(MMC_BASE, MMC_CLKSRC, 0);
    wr32(MMC_BASE, MMC_CDTHRCTL, 0x0200_0001); // vendor hi3xxx
    wr32(MMC_BASE, MMC_PWREN, rd32(MMC_BASE, MMC_PWREN) | 1);
    // Vendor legacy timing constants, 400 kHz (values from the stock kmsg).
    wr32(MMC_BASE, MMC_UHS_REG_EXT, 0x00EF_0000);
    wr32(MMC_BASE, MMC_ENABLE_SHIFT, 0);
    wr32(MMC_BASE, MMC_GPIO, 0x0001_0700); // cclk_div=7, CLK_EN
    wr32(MMC_BASE, MMC_UHS_REG, 0); // legacy 2.95 V card: keep SDMMC_UHS_18V clear
    // Clock latch dance (vendor setup_bus): upd, div, upd, enable, upd.
    upd_clk();
    wr32(MMC_BASE, MMC_CLKDIV, 0);
    upd_clk();
    wr32(MMC_BASE, MMC_CLKENA, rd32(MMC_BASE, MMC_CLKENA) | 1);
    upd_clk();
    // Let the powered card see stable clocks (the plan's 600 ms settle);
    // serviced so the host's boot-time enumeration is answered throughout.
    spin_usb(0x1E00_0000, log);

    let ctrl2 = rd32(MMC_BASE, MMC_CTRL);
    let pwren2 = rd32(MMC_BASE, MMC_PWREN);
    let clkena2 = rd32(MMC_BASE, MMC_CLKENA);
    let ctype2 = rd32(MMC_BASE, MMC_CTYPE);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SDF CTRL=");
    at = hex8(&mut l, at, ctrl2);
    at = put(&mut l, at, b" PWREN=");
    hex8(&mut l, at, pwren2);
    super::ane_fb::paint(15, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SG CLKENA=");
    at = hex8(&mut l, at, clkena2);
    at = put(&mut l, at, b" CTYPE=");
    hex8(&mut l, at, ctype2);
    super::ane_fb::paint(16, &l);
    log("sd: c_rst_ok=", rst_ok as u64);
    log("sd: c_ctrl=", ctrl2 as u64);
    log("sd: c_pwren=", pwren2 as u64);
    log("sd: c_clkena=", clkena2 as u64);
    log("sd: c_ctype=", ctype2 as u64);

    // ---- v53: vendor work_fail_reset replication (dw_mmc_hi3xxx.c:1708) ----
    // v52 measured the recovery trigger exactly: the clk_sd gate latches
    // (PERCLKEN4/PERSTAT4 bit17=1) yet CLKENA never sticks and RINTSTS
    // holds HLE (BIT12). The vendor answer is an IP power cycle with the
    // ciu clock toggled, a full re-init, and their retry-patterned clock
    // dance. New writes here: RSTEN4 bit24 (SD_M) + RSTEN4 bit18.
    let k_pre = rd32(MMC_BASE, MMC_CLKENA);
    let r_pre = rd32(MMC_BASE, MMC_RINTSTS);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SH K=");
    at = hex8(&mut l, at, k_pre);
    at = put(&mut l, at, b" R=");
    hex8(&mut l, at, r_pre);
    super::ane_fb::paint(17, &l);
    super::ane_fb::flush_panel();
    log("sd: v53_kpre=", k_pre as u64);
    log("sd: v53_rpre=", r_pre as u64);

    // (a) quiet the IP: INT_ENABLE off, unmask nothing, clear all latches.
    wr32(MMC_BASE, MMC_CTRL, rd32(MMC_BASE, MMC_CTRL) & !(1 << 4));
    wr32(MMC_BASE, MMC_INTMASK, 0);
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);

    // (b) assert SD_M (RSTEN4 bit24) then the controller reset (bit18).
    wr32(CRG_BASE, 0x090, 1 << 24);
    wr32(CRG_BASE, 0x090, 1 << 18);
    spin(0x4000); // ~20 us

    // (c) ciu off (PERDIS4 bit17) -> release SD_M -> ciu on (PEREN4 bit17)
    //     -> release the controller reset: the vendor's power cycle.
    wr32(CRG_BASE, 0x044, 1 << 17);
    spin(0x4000);
    wr32(CRG_BASE, 0x094, 1 << 24);
    wr32(CRG_BASE, 0x040, 1 << 17);
    spin(0x4000);
    wr32(CRG_BASE, 0x094, 1 << 18);

    // (d) CTRL 3-reset wait (mci_hi3xxx_wait_reset), then the vendor's
    //     re-init order; UHS_REG = 0 (legacy 2.95 V card - keep 18 V off).
    wr32(MMC_BASE, MMC_CTRL, 0x7);
    let mut rst2_ok = false;
    for _ in 0..0x2_0000 {
        if rd32(MMC_BASE, MMC_CTRL) & 0x7 == 0 {
            rst2_ok = true;
            break;
        }
    }
    wr32(MMC_BASE, MMC_CTYPE, 0);
    wr32(MMC_BASE, MMC_FIFOTH, (0x2 << 28) | (127 << 16) | 128);
    wr32(MMC_BASE, MMC_TMOUT, 0xFFFF_FFFF);
    wr32(MMC_BASE, MMC_CDTHRCTL, 0x0200_0001);
    wr32(MMC_BASE, MMC_UHS_REG, 0);
    wr32(MMC_BASE, MMC_GPIO, 0);
    spin(0x2000); // ~10 us
    wr32(MMC_BASE, MMC_UHS_REG_EXT, 0x00EF_0000);
    wr32(MMC_BASE, MMC_ENABLE_SHIFT, 0);
    wr32(MMC_BASE, MMC_GPIO, 0x0001_0700);

    // (e) vendor interrupt state, then the clock dance via the retry path.
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);
    wr32(MMC_BASE, MMC_INTMASK, 0);
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);
    wr32(MMC_BASE, MMC_CTRL, 1 << 4); // INT_ENABLE

    wr32(MMC_BASE, MMC_CLKENA, 0);
    wr32(MMC_BASE, MMC_CLKSRC, 0);
    let _ = upd_clk2(log);
    wr32(MMC_BASE, MMC_CLKDIV, 0);
    let _ = upd_clk2(log);
    wr32(MMC_BASE, MMC_CLKENA, 1);
    let c_aft = upd_clk2(log);

    let k_aft = rd32(MMC_BASE, MMC_CLKENA);
    let r_aft = rd32(MMC_BASE, MMC_RINTSTS);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SI K=");
    at = hex8(&mut l, at, k_aft);
    at = put(&mut l, at, b" C=");
    hex8(&mut l, at, c_aft);
    super::ane_fb::paint(18, &l);
    super::ane_fb::flush_panel();
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SJ R=");
    at = hex8(&mut l, at, r_aft);
    at = put(&mut l, at, b" S2=");
    hex8(&mut l, at, rst2_ok as u32);
    super::ane_fb::paint(19, &l);
    super::ane_fb::flush_panel();
    log("sd: v53_kaft=", k_aft as u64);
    log("sd: v53_raft=", r_aft as u64);
    log("sd: v53_caft=", c_aft as u64);
    log("sd: v53_rst2_ok=", rst2_ok as u64);

    // (f) still zero? clear HLE, poke once more, read back.
    let (k2, r2) = if k_aft & 1 == 0 {
        wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);
        wr32(MMC_BASE, MMC_CLKENA, 1);
        let _ = upd_clk2(log);
        spin_usb(0x8_0000, log);
        (rd32(MMC_BASE, MMC_CLKENA), rd32(MMC_BASE, MMC_RINTSTS))
    } else {
        (k_aft, r_aft)
    };
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SK K2=");
    at = hex8(&mut l, at, k2);
    at = put(&mut l, at, b" R2=");
    hex8(&mut l, at, r2);
    super::ane_fb::paint(20, &l);
    super::ane_fb::flush_panel();
    log("sd: v53_k2=", k2 as u64);
    log("sd: v53_r2=", r2 as u64);

    let k4 = rd32(CRG_BASE, 0x048); // PERCLKEN4
    let s4 = rd32(CRG_BASE, 0x04C); // PERSTAT4
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SL K4=");
    at = hex8(&mut l, at, k4);
    at = put(&mut l, at, b" S4=");
    hex8(&mut l, at, s4);
    super::ane_fb::paint(21, &l);
    super::ane_fb::flush_panel();
    let ureg = rd32(MMC_BASE, MMC_UHS_REG);
    let gpi = rd32(MMC_BASE, MMC_GPIO);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SM U=");
    at = hex8(&mut l, at, ureg);
    at = put(&mut l, at, b" G=");
    hex8(&mut l, at, gpi);
    super::ane_fb::paint(22, &l);
    super::ane_fb::flush_panel();
    let b8 = rd32(CRG_BASE, 0x0B8);
    let ec = rd32(CRG_BASE, 0x0EC);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SN B8=");
    at = hex8(&mut l, at, b8);
    at = put(&mut l, at, b" EC=");
    hex8(&mut l, at, ec);
    super::ane_fb::paint(23, &l);
    super::ane_fb::flush_panel();
    log("sd: v53_k4=", k4 as u64);
    log("sd: v53_s4=", s4 as u64);
    log("sd: v53_ureg=", ureg as u64);
    log("sd: v53_gpi=", gpi as u64);
    log("sd: v53_b8=", b8 as u64);
    log("sd: v53_ec=", ec as u64);

    if stage < 3 {
        super::ane_fb::flush_panel();
        log("sd: stage2_done=", 1);
        return;
    }

    // ---- v61: read-only recon of the two things that could have made the
    // cards deaf - no writes to any interrupt/status/voltage register
    // until the crash question (v60 storm?) is settled.
    // 1. SD slot IO-voltage select raw state (SCTRL+0x314; bit2 = SEL18).
    let iv_raw = rd32(0xFFF0_A000, 0x314);
    // 2. GICD enable state (kernel's proven base 0xE82B1000): which lines
    //    would actually reach the CPU if a controller asserted them.
    const GICD: usize = 0xE82B_1000;
    let gi0 = rd32(GICD, 0x100); // ISENABLER0: IDs 0..31
    let gi3 = rd32(GICD, 0x10C); // IDs 96..127
    let gi4 = rd32(GICD, 0x110); // IDs 128..159
    let gi5 = rd32(GICD, 0x114); // IDs 160..191
    super::ane_fb::paint(24, b"SO V61 RECON");
    log("sd: iv_raw=", iv_raw as u64);
    log("sd: gicd0=", gi0 as u64);
    log("sd: gicd3=", gi3 as u64);
    log("sd: gicd4=", gi4 as u64);
    log("sd: gicd5=", gi5 as u64);
    log("sd: intmask_pre=", rd32(MMC_BASE, MMC_INTMASK) as u64);
    // v64: with the GICD lines disabled (v63 data: IDs 96-191 all 0), the
    // vendor's wide mask cannot reach the CPU even if a line asserts. If
    // this IP gates status latching on INTMASK, the CMD8 retry below will
    // finally show CMD_DONE and a response.
    wr32(MMC_BASE, MMC_INTMASK, 0x003F_FFFF);
    let ctrl_ie = rd32(MMC_BASE, MMC_CTRL);
    wr32(MMC_BASE, MMC_CTRL, ctrl_ie | (1 << 4));
    log("sd: intmask_set=", rd32(MMC_BASE, MMC_INTMASK) as u64);
    log("sd: ctrl_ie=", rd32(MMC_BASE, MMC_CTRL) as u64);
    log("sd: ctrl_ie2=", rd32(MMC_BASE, MMC_CTRL) as u64);

    // CMD0: GO_IDLE_STATE (80 clocks + reset to idle), no response.
    let (ok0, sts0) = send_cmd(0, 0, false, false, false);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SH CMD0=");
    at = hex2(&mut l, at, if ok0 { 1 } else { 0 });
    at = put(&mut l, at, b" STS=");
    hex8(&mut l, at, sts0);
    super::ane_fb::paint(17, &l);
    log("sd: c_cmd0_ok=", ok0 as u64);
    log("sd: c_cmd0_sts=", sts0 as u64);

    // CMD8: SEND_IF_COND, VHS=1 + check pattern 0xAA, R7 (48-bit + CRC).
    let (ok8, sts8) = send_cmd(8, 0x1AA, true, false, true);
    let r8 = rd32(MMC_BASE, MMC_RESP0);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SI CMD8=");
    at = hex2(&mut l, at, if ok8 { 1 } else { 0 });
    at = put(&mut l, at, b" R0=");
    hex8(&mut l, at, r8);
    super::ane_fb::paint(18, &l);
    log("sd: c_cmd8_ok=", ok8 as u64);
    log("sd: c_cmd8_sts=", sts8 as u64);
    log("sd: c_cmd8_r0=", r8 as u64);

    // v61b: clock probe - time a fixed read loop (us-per-iteration) and the
    // retried command (the poll counts then translate to real time; the
    // timer's claimed microseconds run ~39x fast, ratios are what matter).
    let ct0 = crate::timer::uptime_us();
    let mut acc = 0u32;
    for _ in 0..0x2_0000u32 {
        acc ^= rd32(MMC_BASE, MMC_RINTSTS);
    }
    let ct1 = crate::timer::uptime_us();
    log("sd: calib_us=", ct1.wrapping_sub(ct0));
    log("sd: calib_xor=", acc as u64);

    // v59 instrumentation: re-send CMD8 and capture the whole picture - the
    // CMD register right after arming, RINTSTS at completion, and all four
    // response registers (the first read only looked at RESP0).
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);
    let tt0 = crate::timer::uptime_us();
    wr32(MMC_BASE, MMC_CMDARG, 0x1AA);
    let cmdr9 = 8u32 | (1 << 13) | (1 << 6) | (1 << 8) | (1 << 29);
    wr32(MMC_BASE, MMC_CMD, (1 << 31) | cmdr9);
    let cmd_now = rd32(MMC_BASE, MMC_CMD);
    let ok9 = cmd_start_cleared();
    let tt1 = crate::timer::uptime_us();
    let sts9 = rd32(MMC_BASE, MMC_RINTSTS);
    let r0 = rd32(MMC_BASE, MMC_RESP0);
    let r1 = rd32(MMC_BASE, MMC_RESP1);
    let r2 = rd32(MMC_BASE, 0x038);
    let r3 = rd32(MMC_BASE, MMC_RESP3);
    log("sd: c_ret_us=", tt1.wrapping_sub(tt0));
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SK C=");
    at = hex8(&mut l, at, cmd_now);
    at = put(&mut l, at, b" S=");
    hex8(&mut l, at, sts9);
    super::ane_fb::paint(20, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SL R0=");
    at = hex8(&mut l, at, r0);
    at = put(&mut l, at, b" R1=");
    hex8(&mut l, at, r1);
    super::ane_fb::paint(21, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SM R2=");
    at = hex8(&mut l, at, r2);
    at = put(&mut l, at, b" R3=");
    hex8(&mut l, at, r3);
    super::ane_fb::paint(22, &l);
    log("sd: c_ret_ok=", ok9 as u64);
    log("sd: c_ret_cmd=", cmd_now as u64);
    log("sd: c_ret_sts=", sts9 as u64);
    log("sd: c_ret_r0=", r0 as u64);
    log("sd: c_ret_r1=", r1 as u64);
    log("sd: c_ret_r2=", r2 as u64);
    log("sd: c_ret_r3=", r3 as u64);

    // Card present? The PL061 pin 3 is the board's card-detect (the plan
    // read the SoC's CDETECT as inconclusive: bit0 is 0 even with a card,
    // and the stock read was 0x10404). PL061 masked reads: addr+0x3FC sees
    // every pin.
    let cd_pins = rd32(PL061_BASE, 0x3FC);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SP CD3=");
    at = hex2(&mut l, at, if cd_pins & (1 << 3) != 0 { 1 } else { 0 });
    at = put(&mut l, at, b" PINS=");
    hex8(&mut l, at, cd_pins);
    super::ane_fb::paint(19, &l);
    log("sd: c_cd_pins=", cd_pins as u64);

    // Identification runs only when the card answered the CMD8 probe.
    if ok8 && r8 == 0x1AA {
        identify(log);
    }

    super::ane_fb::flush_panel();
    log("sd: phaseC_done=", 1);
}

/// Phase D: the SD identification chain (ACMD41, CMD2/3/9/7/16, CMD17),
/// entered only after CMD8's 0x1AA. OCR arg taken from the stock kmsg
/// (0xC1FF8000 reported back for a 0x41010000 request).
fn identify(log: fn(&str, u64)) {
    // ACMD41 loop until the card reports ready (OCR bit 31).
    let mut ocr = 0u32;
    let mut rounds = 0u32;
    while rounds < 0x40 {
        rounds += 1;
        let _ = send_cmd(55, 0, true, false, true); // APP_CMD, RCA = 0
        let _ = send_cmd(41, 0x4101_0000, true, false, false); // HCS + stock OCR
        ocr = rd32(MMC_BASE, MMC_RESP0);
        if ocr & (1 << 31) != 0 {
            break;
        }
        spin(0x2_0000); // pacing between polls, per the plan
    }
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SJ 41r=");
    at = hex2(&mut l, at, rounds as u8);
    at = put(&mut l, at, b" OCR=");
    hex8(&mut l, at, ocr);
    super::ane_fb::paint(20, &l);
    log("sd: d_ocr=", ocr as u64);
    log("sd: d_rounds=", rounds as u64);
    if ocr & (1 << 31) == 0 {
        log("sd: d_ocr_timeout=", 1);
        return;
    }

    // CMD2: ALL_SEND_CID (136-bit response).
    let (ok2, _) = send_cmd(2, 0, true, true, true);
    let cid0 = rd32(MMC_BASE, MMC_RESP0);
    let cid3 = rd32(MMC_BASE, MMC_RESP3);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SK C2=");
    at = hex2(&mut l, at, if ok2 { 1 } else { 0 });
    at = put(&mut l, at, b" CID3=");
    hex8(&mut l, at, cid3);
    super::ane_fb::paint(21, &l);
    log("sd: d_cid0=", cid0 as u64);
    log("sd: d_cid3=", cid3 as u64);

    // CMD3: SEND_RELATIVE_ADDR; the new RCA sits in the reply's 31:16.
    let (ok3, _) = send_cmd(3, 0, true, false, true);
    let rca = (rd32(MMC_BASE, MMC_RESP0) >> 16) & 0xFFFF;
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SL C3=");
    at = hex2(&mut l, at, if ok3 { 1 } else { 0 });
    at = put(&mut l, at, b" RCA=");
    hex8(&mut l, at, rca);
    super::ane_fb::paint(22, &l);
    log("sd: d_rca=", rca as u64);

    // CMD9: SEND_CSD (136-bit; arg = RCA << 16).
    let (ok9, _) = send_cmd(9, rca << 16, true, true, true);
    let csd1 = rd32(MMC_BASE, MMC_RESP1);
    // CMD7: SELECT_CARD.
    let (ok7, _) = send_cmd(7, rca << 16, true, false, true);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SM C9=");
    at = hex2(&mut l, at, if ok9 { 1 } else { 0 });
    at = put(&mut l, at, b" C7=");
    at = hex2(&mut l, at, if ok7 { 1 } else { 0 });
    at = put(&mut l, at, b" CSD1=");
    hex8(&mut l, at, csd1);
    super::ane_fb::paint(23, &l);
    log("sd: d_csd1=", csd1 as u64);

    // CMD16: SET_BLOCKLEN = 512.
    let (ok16, _) = send_cmd(16, 512, true, false, true);
    log("sd: d_c16=", ok16 as u64);

    // CMD17: READ_SINGLE_BLOCK of sector 0, then the PIO drain: the data
    // port sits at +0x200 and RXDR (RINTSTS bit 5) marks every word.
    wr32(MMC_BASE, MMC_BLKSIZ, 512);
    wr32(MMC_BASE, MMC_BYTCNT, 512);
    wr32(MMC_BASE, MMC_RINTSTS, 0xFFFF_FFFF);
    wr32(MMC_BASE, MMC_CMDARG, 0);
    // PRV_DAT_WAIT | DAT_EXP (a data transfer follows) | USE_HOLD_REG.
    let cmdr = 17u32 | (1 << 13) | (1 << 9) | (1 << 29);
    wr32(MMC_BASE, MMC_CMD, (1 << 31) | cmdr);
    let ok17 = cmd_start_cleared();
    let mut words = [0u32; 4];
    let mut got = 0usize;
    let mut last = 0u32;
    let mut received = 0u32;
    for i in 0..128 {
        let mut spins = 0u32;
        while rd32(MMC_BASE, MMC_RINTSTS) & INT_RXDR == 0 {
            spins += 1;
            if spins > 0x2_0000 {
                break;
            }
        }
        if spins > 0x2_0000 {
            break;
        }
        let w = rd32(MMC_BASE, MMC_FIFO);
        received += 1;
        if got < 4 {
            words[got] = w;
            got += 1;
        }
        if i == 127 {
            last = w; // bytes 508-511: the MBR signature (0xAA55) lives here
        }
    }
    let sts17 = rd32(MMC_BASE, MMC_RINTSTS);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SN C17=");
    at = hex2(&mut l, at, if ok17 { 1 } else { 0 });
    at = put(&mut l, at, b" STS=");
    hex8(&mut l, at, sts17);
    super::ane_fb::paint(24, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"SO W0=");
    at = hex8(&mut l, at, words[0]);
    at = put(&mut l, at, b" W127=");
    hex8(&mut l, at, last);
    super::ane_fb::paint(25, &l);
    log("sd: d_c17=", ok17 as u64);
    log("sd: d_c17_sts=", sts17 as u64);
    log("sd: d_w0=", words[0] as u64);
    log("sd: d_w1=", words[1] as u64);
    log("sd: d_w127=", last as u64);
    log("sd: d_got=", received as u64);
}
