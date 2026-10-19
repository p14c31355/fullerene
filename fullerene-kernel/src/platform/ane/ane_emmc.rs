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
use super::common::pmic::pmic_rd;

const CRG_BASE: usize = 0xFFF3_5000;
const SDHCI_BASE: usize = 0xFF39_0000;

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

fn hex4(buf: &mut [u8; 44], at: usize, v: u32) -> usize {
    for i in 0..4 {
        buf[at + i] = HEX[((v >> (12 - 4 * i)) & 0xF) as usize];
    }
    at + 4
}

fn hex2(buf: &mut [u8; 44], at: usize, v: u8) -> usize {
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

/// eMMC phase 1 (and 2 with `stage >= 2`): verify the inherited
/// clocks/reset/rail, read the SDHCI identity, then run the vendor probe's
/// reset toggle + the standard SDHCI software reset and clock re-init.
/// Rows EM0..; every value also lands in the durable log.
pub fn probe(log: fn(&str, u64), stage: u32) {
    super::ane_fb::paint(26, b"EM0 EMMC PROBE");

    // CRG view: gates for hclk_emmc0 (PEREN0 bit13) and clk_emmc0
    // (PEREN4 bit15), plus their status mirrors.
    let k0 = rd32(CRG_BASE, 0x008); // PERCLKEN0
    let k4 = rd32(CRG_BASE, 0x048); // PERCLKEN4
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM1 K0=");
    at = hex8(&mut l, at, k0);
    at = put(&mut l, at, b" K4=");
    hex8(&mut l, at, k4);
    super::ane_fb::paint(27, &l);
    log("em: k0=", k0 as u64);
    log("em: k4=", k4 as u64);

    let s0 = rd32(CRG_BASE, 0x00C); // PERSTAT0
    let s4 = rd32(CRG_BASE, 0x04C); // PERSTAT4
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM2 S0=");
    at = hex8(&mut l, at, s0);
    at = put(&mut l, at, b" S4=");
    hex8(&mut l, at, s4);
    super::ane_fb::paint(28, &l);
    log("em: s0=", s0 as u64);
    log("em: s4=", s4 as u64);

    let rst = rd32(CRG_BASE, 0x098); // PERRSTSTAT4 (bit16 = emmc0)
    let mux = rd32(CRG_BASE, 0x0B4); // clk_mmc0 mux/div register
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM3 R=");
    at = hex8(&mut l, at, rst);
    at = put(&mut l, at, b" B4=");
    hex8(&mut l, at, mux);
    super::ane_fb::paint(29, &l);
    log("em: rst=", rst as u64);
    log("em: b4=", mux as u64);

    // vmmc rail (LDO23): ctrl 0x23 bit0 enable, vset 0x4C 3-bit selector.
    let c23 = pmic_rd(0x23);
    let v4c = pmic_rd(0x4C);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM4 C23=");
    at = hex2(&mut l, at, c23);
    at = put(&mut l, at, b" V4C=");
    at = hex2(&mut l, at, v4c);
    at = put(&mut l, at, b" E=");
    hex2(&mut l, at, rd32(CRG_BASE, 0x0EC) as u8);
    super::ane_fb::paint(30, &l);
    log("em: c23=", c23 as u64);
    log("em: v4c=", v4c as u64);

    // Gate guard: only touch the controller when both clock branches are
    // enabled and the reset is deasserted (bit16 clear). An unclocked
    // peripheral read can stall the fabric (v39's lesson on dwmmc1).
    let clocked = (k0 & (1 << 13) != 0)
        && (k4 & (1 << 15) != 0)
        && (s0 & (1 << 13) != 0)
        && (s4 & (1 << 15) != 0)
        && (rst & (1 << 16) == 0);
    if !clocked {
        super::ane_fb::paint(31, b"EM5 GATE OFF - SKIP SDHCI");
        log("em: skip=", 1);
        super::ane_fb::flush_panel();
        return;
    }
    log("em: skip=", 0);

    // SDHCI identity: HOST_VERSION (0xFE, inside the 0xFC word) and the
    // capability words. A working Arasan controller answers these.
    let slotver = rd32(SDHCI_BASE, 0xFC);
    let caps = rd32(SDHCI_BASE, 0x40);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM6 VER=");
    at = hex4(&mut l, at, slotver >> 16);
    at = put(&mut l, at, b" CAP=");
    hex8(&mut l, at, caps);
    super::ane_fb::paint(31, &l);
    log("em: ver=", (slotver >> 16) as u64);
    log("em: cap=", caps as u64);

    let caps1 = rd32(SDHCI_BASE, 0x44);
    let ps = rd32(SDHCI_BASE, 0x24);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM7 C1=");
    at = hex8(&mut l, at, caps1);
    at = put(&mut l, at, b" PS=");
    hex8(&mut l, at, ps);
    super::ane_fb::paint(32, &l);
    log("em: cap1=", caps1 as u64);
    log("em: ps=", ps as u64);

    // CLOCK_CONTROL/TIMEOUT/SOFT_RESET live in the 0x2C word; HOST_CONTROL
    // and POWER_CONTROL in the 0x28 word.
    let cc = rd32(SDHCI_BASE, 0x2C);
    let hc = rd32(SDHCI_BASE, 0x28);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM8 CC=");
    at = hex4(&mut l, at, cc & 0xFFFF);
    at = put(&mut l, at, b" SR=");
    at = hex2(&mut l, at, ((cc >> 16) & 0xFF) as u8);
    at = put(&mut l, at, b" P=");
    hex2(&mut l, at, ((hc >> 8) & 0xFF) as u8);
    super::ane_fb::paint(33, &l);
    log("em: cc=", cc as u64);
    log("em: hostc=", hc as u64);

    let ni = rd32(SDHCI_BASE, 0x30); // NORMAL_INT_STATUS
    let phy = rd32(SDHCI_BASE, 0x430); // vendor PHY_CTRL1
    let dbg = rd32(SDHCI_BASE, 0x420); // vendor DEBUG_REG0
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EM9 NI=");
    at = hex8(&mut l, at, ni);
    at = put(&mut l, at, b" PHY=");
    hex8(&mut l, at, phy);
    super::ane_fb::paint(34, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EMA DBG=");
    hex8(&mut l, at, dbg);
    super::ane_fb::paint(35, &l);
    log("em: ni=", ni as u64);
    log("em: phy=", phy as u64);
    log("em: dbg=", dbg as u64);

    // ---- phase 2 (stage >= 2): the vendor probe's reset toggle and the
    // standard SDHCI software reset + clock re-init. The LK leaves the
    // controller running (CLOCK_CONTROL = 0x000E0007 measured with the SD
    // clock on), so this is the first write pass on it.
    if stage < 2 {
        super::ane_fb::paint(36, b"EMB STAGE1 ONLY");
        super::ane_fb::flush_panel();
        log("em: stage=", stage as u64);
        return;
    }

    // v64: card-clock root fix. The loader left CRG+0xB4 = 0x3F (PLL3
    // branch, divider field 7 = /128) and may have powered AP-PLL3 down at
    // handoff; v63's CMD0 never completing (no completion, no error after
    // 7.5 s of polling) fits a dead card clock. PMCTRL = 0xFFF31000;
    // vendor clk-hi3xxx.c: PPLLCTRL0(n) = 0x30 + n*8, so PPLL3_CTRL0 =
    // 0x48 (EN = bit0, LOCK = bit26). The driver enables by setting the
    // bit directly, then syncs the LPM3 over IPC (we skip the IPC: the
    // direct write is what actually powers the PLL).
    let pmc = 0xFFF3_1000usize;
    let pll_b = rd32(pmc, 0x48);
    let pll1 = rd32(pmc, 0x4C);
    if pll_b & 1 == 0 {
        wr32(pmc, 0x48, pll_b | 1);
    }
    let mut pll_ok = false;
    for _ in 0..0x2_0000 {
        if rd32(pmc, 0x48) & (1 << 26) != 0 {
            pll_ok = true;
            break;
        }
    }
    let pll_a = rd32(pmc, 0x48);
    // The divider field in 0xB4 reads 0x3F: with the val+1 table (div =
    // val + 1) that is /8 = 200 MHz off a 1.6 GHz PLL3, exactly what the
    // stock driver reports (clk_xin=200000000) - the loader left the
    // correct value. Do not touch it.
    let b4_b = rd32(CRG_BASE, 0x0B4);
    log("em: pll_b=", pll_b as u64);
    log("em: pll1=", pll1 as u64);
    log("em: pll_ok=", pll_ok as u64);
    log("em: pll_a=", pll_a as u64);
    log("em: b4_b=", b4_b as u64);

    // Preserve what the loader set before the software reset wipes it.
    let pwr_before = rd8(SDHCI_BASE, 0x29);

    // 1. Vendor reset toggle (sdhci_of_arasan_hardware_reset): assert
    //    emmc0 (RSTEN4 bit16), wait RSTSTAT4 bit16 set; release, wait clear.
    wr32(CRG_BASE, 0x090, 1 << 16);
    let mut rst_set = false;
    for _ in 0..0x4000 {
        if rd32(CRG_BASE, 0x098) & (1 << 16) != 0 {
            rst_set = true;
            break;
        }
    }
    wr32(CRG_BASE, 0x094, 1 << 16);
    let mut rst_clr = false;
    for _ in 0..0x4000 {
        if rd32(CRG_BASE, 0x098) & (1 << 16) == 0 {
            rst_clr = true;
            break;
        }
    }
    // 2. SDHCI software reset (all): self-clearing, bounded poll.
    wr8(SDHCI_BASE, 0x2F, 0x01);
    let mut sw_ok = false;
    for _ in 0..0x2_0000 {
        if rd8(SDHCI_BASE, 0x2F) & 0x01 == 0 {
            sw_ok = true;
            break;
        }
    }
    // 3. Re-init: data timeout, bus power (what the loader had), then the
    //    SD clock at 400 kHz - the 8-bit divider (SDHCI 2.00 IP, VER
    //    0x1002): 200 MHz / (2 * 250) with select = 0xFA.
    wr8(SDHCI_BASE, 0x2E, 0x0F);
    wr8(SDHCI_BASE, 0x29, pwr_before);
    wr16(SDHCI_BASE, 0x2C, (0xFA << 8) | 0x01);
    let mut clk_stable = false;
    for _ in 0..0x2_0000 {
        if rd16(SDHCI_BASE, 0x2C) & 0x02 != 0 {
            clk_stable = true;
            break;
        }
    }
    wr16(SDHCI_BASE, 0x2C, rd16(SDHCI_BASE, 0x2C) | 0x04);
    // v64: the GICD dump (v63: IDs 96-191 all disabled) proves the
    // controller side cannot reach the CPU, so the stock's ier can be set
    // safely. A 32-bit write at 0x34 covers the normal enables and the
    // error enables at 0x36. SIGNAL_ENABLE (0x38) stays 0, so even a
    // wiggling line is invisible to the CPU. If this IP gates status
    // latching on the enable bits, CMD0/CMD1 will finally report.
    wr32(SDHCI_BASE, 0x34, 0x007F_0007);
    let cc_after = rd32(SDHCI_BASE, 0x2C);
    let pwr_after = rd32(SDHCI_BASE, 0x28);
    let int_after = rd32(SDHCI_BASE, 0x30);
    let ps_after = rd32(SDHCI_BASE, 0x24);

    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EMB R=");
    at = hex2(&mut l, at, (rst_set as u8) << 1 | rst_clr as u8);
    at = put(&mut l, at, b" SW=");
    at = hex2(&mut l, at, sw_ok as u8);
    at = put(&mut l, at, b" CLK=");
    at = hex2(&mut l, at, clk_stable as u8);
    super::ane_fb::paint(36, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EMC CC=");
    at = hex8(&mut l, at, cc_after);
    at = put(&mut l, at, b" P=");
    at = hex8(&mut l, at, pwr_after);
    super::ane_fb::paint(37, &l);
    let mut l = [b' '; 44];
    let mut at = put(&mut l, 0, b"EMD NI=");
    at = hex8(&mut l, at, int_after);
    at = put(&mut l, at, b" PS=");
    at = hex8(&mut l, at, ps_after);
    super::ane_fb::paint(38, &l);
    log("em: rst_set=", rst_set as u64);
    log("em: rst_clr=", rst_clr as u64);
    log("em: sw_ok=", sw_ok as u64);
    log("em: clk_stable=", clk_stable as u64);
    log("em: cc_after=", cc_after as u64);
    log("em: hc_after=", pwr_after as u64);
    log("em: ni_after=", int_after as u64);
    log("em: ps_after=", ps_after as u64);

    // ---- phase 3 (stage >= 3): the SDHCI command engine. CMD0 (GO_IDLE,
    // no response) puts the eMMC into idle; CMD1 (SEND_OP_COND, R3) starts
    // its power-up negotiation. Both bounded; the response/error registers
    // land in the log.
    if stage >= 3 {
        let e0t0 = crate::timer::uptime_us();
        let (ok0, sts0, er0, it0) = sdhci_cmd(0, 0, 0, 0);
        let e0t1 = crate::timer::uptime_us();
        let mut l = [b' '; 44];
        let mut at = put(&mut l, 0, b"EME C0=");
        at = hex2(&mut l, at, ok0 as u8);
        at = put(&mut l, at, b" S=");
        at = hex8(&mut l, at, sts0);
        super::ane_fb::paint(39, &l);
        log("em: c0_ok=", ok0 as u64);
        log("em: c0_sts=", sts0 as u64);
        log("em: c0_err=", er0 as u64);
        log("em: c0_it=", it0 as u64);
        log("em: c0_us=", e0t1.wrapping_sub(e0t0));
        log("em: c0_ps=", rd32(SDHCI_BASE, 0x24) as u64);

        // CMD1 with HCS + a wide voltage window, repeated until the card
        // reports ready. The v65 run returned the standard eMMC OCR shape
        // (0x40FF80xx) with the ready bit still clear, so a single shot
        // leaves CMD2/CMD3 unanswered. R3 has no CRC. Every response word
        // of every try goes to the log so the register mapping pins down.
        let mut last = [0u32; 4];
        let mut ready_k = 0u64;
        for k in 0..8u32 {
            let (ok1, sts1, er1, it1) = sdhci_cmd(1, 0x40FF_8000, 2, 0);
            last[0] = rd32(SDHCI_BASE, 0x10);
            last[1] = rd32(SDHCI_BASE, 0x14);
            last[2] = rd32(SDHCI_BASE, 0x18);
            last[3] = rd32(SDHCI_BASE, 0x1C);
            log("em: c1_k=", k as u64);
            log("em: c1_ok=", ok1 as u64);
            log("em: c1_sts=", sts1 as u64);
            log("em: c1_err=", er1 as u64);
            log("em: c1_it=", it1 as u64);
            log("em: c1w0=", last[0] as u64);
            log("em: c1w1=", last[1] as u64);
            log("em: c1w2=", last[2] as u64);
            log("em: c1w3=", last[3] as u64);
            // Ready = the OCR's bit31, checked on the raw RESPONSE_0 the
            // corrected RESP_SHORT select produces: busy = 0x40FF8080,
            // ready = 0xC0FF8080. (The v66/v67 long-capture runs showed
            // the same value shifted one byte - 0x0040FF80 -> 0x00C0FF80 -
            // because a 136-bit capture of a 48-bit response keeps its own
            // byte alignment.) Stop as soon as it is set: a ready card
            // stops answering CMD1 and extra tries only produce the
            // timeout that used to wedge the command path.
            if ok1 && (last[0] & 0x8000_0000) != 0 {
                ready_k = k as u64 + 1;
                log("em: c1_ready_k=", ready_k);
                break;
            }
            if !ok1 || er1 != 0 {
                sdhci_reset_cmd();
            }
            crate::timer::delay_us(20_000);
        }
        let mut l = [b' '; 44];
        let mut at = put(&mut l, 0, b"EMG W2=");
        at = hex8(&mut l, at, last[2]);
        at = put(&mut l, at, b" W3=");
        hex8(&mut l, at, last[3]);
        super::ane_fb::paint(41, &l);

        // v65: the identification chain. v64 proved the enable write
        // unlocks this controller's completion path; now see whether real
        // cards answer at all. CMD2 = ALL_SEND_CID (136-bit R2 - a live
        // card returns its CID), CMD3 = SET_RELATIVE_ADDR (R1), CMD9 =
        // SEND_CSD (136-bit R2). All responses land in 0x10..0x1C.
        let (ok2, sts2, er2, it2) = sdhci_cmd(2, 0, 1, CMD_CRC);
        if !ok2 || er2 != 0 {
            sdhci_reset_cmd();
        }
        let cid0 = rd32(SDHCI_BASE, 0x10);
        let cid1 = rd32(SDHCI_BASE, 0x14);
        let cid2 = rd32(SDHCI_BASE, 0x18);
        let cid3 = rd32(SDHCI_BASE, 0x1C);
        log("em: c2_ok=", ok2 as u64);
        log("em: c2_sts=", sts2 as u64);
        log("em: c2_err=", er2 as u64);
        log("em: c2_it=", it2 as u64);
        log("em: c2_r0=", cid0 as u64);
        log("em: c2_r1=", cid1 as u64);
        log("em: c2_r2=", cid2 as u64);
        log("em: c2_r3=", cid3 as u64);

        // v74: the stock assigns the RCA from the host (mmc_ops.c:304:
        // cmd.arg = card->rca << 16 with rca = 1), so the first CMD3
        // carries it. A re-issued CMD3 after this point goes unanswered
        // (v73 observed the timeout), so this is the only chance to set it.
        let (ok3, sts3, er3, it3) = sdhci_cmd(3, 1 << 16, 2, CMD_CRC | CMD_INDEX);
        if !ok3 || er3 != 0 {
            sdhci_reset_cmd();
        }
        let rca0 = rd32(SDHCI_BASE, 0x10);
        let rca1 = rd32(SDHCI_BASE, 0x14);
        let rca2 = rd32(SDHCI_BASE, 0x18);
        let rca3 = rd32(SDHCI_BASE, 0x1C);
        log("em: c3_ok=", ok3 as u64);
        log("em: c3_sts=", sts3 as u64);
        log("em: c3_err=", er3 as u64);
        log("em: c3_it=", it3 as u64);
        log("em: c3_rca=", rca0 as u64);
        log("em: c3_w1=", rca1 as u64);
        log("em: c3_w2=", rca2 as u64);
        log("em: c3_w3=", rca3 as u64);

        let (ok9, sts9, er9, it9) = sdhci_cmd(9, 0, 1, CMD_CRC);
        let csd0 = rd32(SDHCI_BASE, 0x10);
        let csd1 = rd32(SDHCI_BASE, 0x14);
        let csd2 = rd32(SDHCI_BASE, 0x18);
        let csd3 = rd32(SDHCI_BASE, 0x1C);
        log("em: c9_ok=", ok9 as u64);
        log("em: c9_sts=", sts9 as u64);
        log("em: c9_err=", er9 as u64);
        log("em: c9_it=", it9 as u64);
        log("em: c9_r0=", csd0 as u64);
        log("em: c9_r1=", csd1 as u64);
        log("em: c9_r2=", csd2 as u64);
        log("em: c9_r3=", csd3 as u64);

        // v78: CMD13 answers at RCA 1 while CMD7@1 goes silent (v77), so
        // the question is whether the select takes effect without ever
        // signalling. Probe sequence: confirm the state with CMD13, issue
        // CMD7, dump PRESENT_STATE / command / response registers, then
        // read the state again - state 4 (transfer) means the select worked
        // and only the completion flag was missed; state 3 (stand-by) means
        // the command never landed.
        let (ok13a, _s13a, e13a, _i13a) = sdhci_cmd(13, 1 << 16, 2, CMD_CRC | CMD_INDEX);
        if !ok13a || e13a != 0 {
            sdhci_reset_cmd();
        }
        log("em: c13a_r0=", rd32(SDHCI_BASE, 0x10) as u64);

        let (ok7, sts7, er7, it7) = sdhci_cmd(7, 1 << 16, 2, CMD_CRC | CMD_INDEX);
        log("em: c7_ok=", ok7 as u64);
        log("em: c7_sts=", sts7 as u64);
        log("em: c7_err=", er7 as u64);
        log("em: c7_it=", it7 as u64);
        log("em: c7_ps=", rd32(SDHCI_BASE, 0x24) as u64);
        log("em: c7_cmdr=", rd16(SDHCI_BASE, 0x0E) as u64);
        log("em: c7_resp=", rd32(SDHCI_BASE, 0x10) as u64);
        sdhci_reset_cmd();

        let (ok13b, _s13b, e13b, _i13b) = sdhci_cmd(13, 1 << 16, 2, CMD_CRC | CMD_INDEX);
        if !ok13b || e13b != 0 {
            sdhci_reset_cmd();
        }
        let st_after = rd32(SDHCI_BASE, 0x10);
        log("em: c13b_r0=", st_after as u64);
        log("em: c13b_state=", ((st_after >> 9) & 0xF) as u64);

        // CMD8 (SEND_EXT_CSD), arg 512: one 512-byte block through the
        // PIO buffer. The leading words carry the EXT_CSD revision bytes.
        let mut ext = [0u32; 128];
        let (ok8, sts8, er8) = sdhci_data_read(8, 512, &mut ext, log);
        log("em: c8_ok=", ok8 as u64);
        log("em: c8_sts=", sts8 as u64);
        log("em: c8_err=", er8 as u64);
        log("em: c8_w0=", ext[0] as u64);
        log("em: c8_w1=", ext[1] as u64);
        log("em: c8_w2=", ext[2] as u64);
        log("em: c8_w3=", ext[3] as u64);
        log("em: c8_w31=", ext[31] as u64);

        // v79: the EXT_CSD's own revision byte sits at offset 192 (word
        // 48); the stock kernel prints it as "EXT_CSD revision 0x08", so
        // it verifies the 512 bytes are real data and not bus noise.
        log("em: c8_rev=", (ext[48] & 0xFF) as u64);

        // Then the first block reads: CMD17 sector 0 (the partition table)
        // and sector 1 (a GPT header on Android-era layouts).
        let mut mbr = [0u32; 128];
        let (ok17, sts17, er17) = sdhci_data_read(17, 0, &mut mbr, log);
        log("em: c17_ok=", ok17 as u64);
        log("em: c17_sts=", sts17 as u64);
        log("em: c17_err=", er17 as u64);
        log("em: c17_m0=", mbr[0] as u64);
        log("em: c17_m1=", mbr[1] as u64);
        log("em: c17_sig=", mbr[127] as u64);

        let mut gpt = [0u32; 128];
        let (ok18, sts18, er18) = sdhci_data_read(17, 1, &mut gpt, log);
        log("em: c18_ok=", ok18 as u64);
        log("em: c18_sts=", sts18 as u64);
        log("em: c18_err=", er18 as u64);
        log("em: c18_g0=", gpt[0] as u64);
        log("em: c18_g1=", gpt[1] as u64);

        // v80: the GPT layout, read-only. Header LBA 1 is verified; the
        // partition entries live from LBA 2 (128-byte entries, 4 per
        // sector), so read the first four sectors and report the first
        // eight entries: type-GUID head, first LBA, and the first eight
        // UTF-16LE name bytes. The entry index rides in the high 32 bits
        // of each logged value.
        log("em: g_ent_lba=", gpt[18] as u64);
        log("em: g_nent=", gpt[20] as u64);
        log("em: g_esize=", gpt[21] as u64);
        let mut ents = [0u32; 4096]; // 32 sectors = all 128 entries
        let mut secs_ok = 0u64;
        let mut k = 0u32;
        while k < 32 {
            let mut sec = [0u32; 128];
            let (ok, _sts, _err) = sdhci_data_read(17, 2 + k, &mut sec, log);
            if !ok {
                break;
            }
            let mut w = 0usize;
            while w < 128 {
                ents[(k as usize) * 128 + w] = sec[w];
                w += 1;
            }
            secs_ok += 1;
            k += 1;
        }
        log("em: g_secs=", secs_ok);
        let mut e = 0usize;
        while e < 128 {
            let base = e * 32;
            // Skip unused entries (zero type GUID) to keep the log lean.
            if ents[base] != 0 || ents[base + 1] != 0 {
                log("em: ge_t=", ((e as u64) << 32) | ents[base] as u64);
                log("em: ge_s=", ((e as u64) << 32) | ents[base + 8] as u64);
                log("em: ge_n0=", ((e as u64) << 32) | ents[base + 14] as u64);
                log("em: ge_n1=", ((e as u64) << 32) | ents[base + 15] as u64);
                log("em: ge_n2=", ((e as u64) << 32) | ents[base + 16] as u64);
                log("em: ge_n3=", ((e as u64) << 32) | ents[base + 17] as u64);
                log("em: ge_n4=", ((e as u64) << 32) | ents[base + 18] as u64);
            }
            e += 1;
        }

        // v83: heads of three named partitions (read-only): the kernel
        // partition (the fastboot flash target - it should carry the
        // Android image magic that our own stock-embed writes), the
        // loader's bootfail_info (BFM log store) and the first erecovery
        // partition (the rescue image the BFM boots).
        let mut kp = [0u32; 128];
        let (ok_k, _s_k, _e_k) = sdhci_data_read(17, 901120, &mut kp, log);
        log("em: kp_ok=", ok_k as u64);
        log("em: kp_w0=", kp[0] as u64);
        log("em: kp_w1=", kp[1] as u64);
        log("em: kp_w2=", kp[2] as u64);
        let mut bp = [0u32; 128];
        let (ok_b, _s_b, _e_b) = sdhci_data_read(17, 1998848, &mut bp, log);
        log("em: bp_ok=", ok_b as u64);
        log("em: bp_w0=", bp[0] as u64);
        log("em: bp_w1=", bp[1] as u64);
        let mut ep = [0u32; 128];
        let (ok_e, _s_e, _e_e) = sdhci_data_read(17, 753664, &mut ep, log);
        log("em: ep_ok=", ok_e as u64);
        log("em: ep_w0=", ep[0] as u64);
        log("em: ep_w1=", ep[1] as u64);

        // v84: FIRST WRITE - the user cleared internal-storage writes on
        // 2026-10-06. Target: hisitest1 (LBA 1921024), the Hisilicon
        // factory scratch partition, so nothing a normal boot reads can be
        // disturbed. Sector 0: read-modify-write-identical (content cannot
        // change; the diff count proves the path). Sector 1: the full
        // destructive cycle - save, pattern write, verify, restore,
        // verify - which also proves the bytes actually land.
        let mut wbuf = [0u32; 128];
        let (ok_wr0, _sw0, _ew0) = sdhci_data_read(17, 1921024, &mut wbuf, log);
        let (ok_w0, st_w0, er_w0) = sdhci_data_write(24, 1921024, &wbuf, log);
        let mut wchk = [0u32; 128];
        let (ok_wc0, _sw1, _ew1) = sdhci_data_read(17, 1921024, &mut wchk, log);
        let mut diff0 = 0u32;
        for i in 0..128 {
            if wbuf[i] != wchk[i] {
                diff0 += 1;
            }
        }
        log("em: w0_rd=", ok_wr0 as u64);
        log("em: w0_wr=", ok_w0 as u64);
        log("em: w0_sts=", st_w0 as u64);
        log("em: w0_err=", er_w0 as u64);
        log("em: w0_ver=", ok_wc0 as u64);
        log("em: w0_diff=", diff0 as u64);

        let mut orig1 = [0u32; 128];
        let (_ok_r1, _sr1, _er1) = sdhci_data_read(17, 1921025, &mut orig1, log);
        let mut pat = [0u32; 128];
        for i in 0..128 {
            pat[i] = 0xA5A5_0000 ^ (i as u32);
        }
        let (ok_p, st_p, er_p) = sdhci_data_write(24, 1921025, &pat, log);
        let mut chk1 = [0u32; 128];
        let (ok_c1, _sc1, _ec1) = sdhci_data_read(17, 1921025, &mut chk1, log);
        let mut diff_p = 0u32;
        for i in 0..128 {
            if pat[i] != chk1[i] {
                diff_p += 1;
            }
        }
        let (ok_rst, st_r, er_r) = sdhci_data_write(24, 1921025, &orig1, log);
        let mut chk2 = [0u32; 128];
        let (ok_c2, _sc2, _ec2) = sdhci_data_read(17, 1921025, &mut chk2, log);
        let mut diff_r = 0u32;
        for i in 0..128 {
            if orig1[i] != chk2[i] {
                diff_r += 1;
            }
        }
        log("em: w1_wr=", ok_p as u64);
        log("em: w1_sts=", st_p as u64);
        log("em: w1_err=", er_p as u64);
        log("em: w1_ver=", ok_c1 as u64);
        log("em: w1_diff=", diff_p as u64);
        log("em: w1_rst=", ok_rst as u64);
        log("em: w1_rsts=", st_r as u64);
        log("em: w1_rerr=", er_r as u64);
        log("em: w1_rver=", ok_c2 as u64);
        log("em: w1_rdiff=", diff_r as u64);

        // v86: multi-block write trial (CMD25 + CMD12) on hisitest1
        // sectors 2..9: save 8 sectors, burst-write a pattern, verify,
        // burst-restore, verify. Any mismatch keeps write_sectors on the
        // proven CMD24 path.
        let mut orig8 = [0u32; 1024];
        let mut mrd = 0u64;
        for k in 0..8usize {
            let (ok, _s, _e) = sdhci_data_read(
                17,
                1921026 + k as u32,
                &mut orig8[k * 128..(k + 1) * 128],
                log,
            );
            if ok {
                mrd += 1;
            }
        }
        log("em: m_rd=", mrd);
        let mut mpb = [0u8; 4096];
        for (i, b) in mpb.iter_mut().enumerate() {
            *b = (i as u8) ^ 0x11;
        }
        let (m_ok, m_sts, m_err) = sdhci_data_write_multi(25, 1921026, 8, &mpb, log);
        log("em: m_wr=", m_ok as u64);
        log("em: m_sts=", m_sts as u64);
        log("em: m_err=", m_err as u64);
        let mut mv = [0u32; 1024];
        let mut mver = 0u64;
        for k in 0..8usize {
            let (ok, _s, _e) =
                sdhci_data_read(17, 1921026 + k as u32, &mut mv[k * 128..(k + 1) * 128], log);
            if ok {
                mver += 1;
            }
        }
        let mut mdiff = 0u32;
        for k in 0..8usize {
            for j in 0..128usize {
                let mut w = 0u32;
                for x in 0..4usize {
                    w |= (mpb[k * 512 + j * 4 + x] as u32) << (8 * x);
                }
                if mv[k * 128 + j] != w {
                    mdiff += 1;
                }
            }
        }
        log("em: m_ver=", mver);
        log("em: m_diff=", mdiff as u64);
        let mut orb = [0u8; 4096];
        for k in 0..1024usize {
            let w = orig8[k];
            orb[k * 4] = w as u8;
            orb[k * 4 + 1] = (w >> 8) as u8;
            orb[k * 4 + 2] = (w >> 16) as u8;
            orb[k * 4 + 3] = (w >> 24) as u8;
        }
        let (mr_ok, mr_sts, mr_err) = sdhci_data_write_multi(25, 1921026, 8, &orb, log);
        log("em: m_rst=", mr_ok as u64);
        log("em: m_rsts=", mr_sts as u64);
        log("em: m_rerr=", mr_err as u64);
        let mut rv = [0u32; 1024];
        for k in 0..8usize {
            let (_ok, _s, _e) =
                sdhci_data_read(17, 1921026 + k as u32, &mut rv[k * 128..(k + 1) * 128], log);
        }
        let mut rdiff = 0u32;
        for i in 0..1024usize {
            if orig8[i] != rv[i] {
                rdiff += 1;
            }
        }
        log("em: m_rdiff=", rdiff as u64);

        // Probe the eMMC for the block layer (before any clock games - the
        // BlockDevice path is verified on the known-good 400 kHz state).
        #[cfg(fullerene_aarch64_ane)]
        {
            emmc_block_probe(log);
            // v98: the first filesystem access (ext4 on "system").
            let mut xs2 = [0u32; 128];
            let (okx, _sx, _ex) = sdhci_data_read(8, 0, &mut xs2, log);
            if okx {
                let secs = (xs2[53] & 0xFFFF_FFFF) as u64;
                emmc_ext4_probe(secs, log);
                // v100: the second partition through the android_fs prober.
                emmc_userdata_probe(secs, log);
            }
        }

        // v94: clock ladder. Rung 1 = a pure clock bump to 25 MHz at the
        // default 1-bit timing (no card switches - within the default-speed
        // spec). Rung 2 = the full HS (CMD6 HS_TIMING, 50 MHz, CMD6
        // BUS_WIDTH, 8-bit host). Each rung is verified by a real read
        // (MBR magic 0xAA55) and the sequence ALWAYS ends on a verified
        // rung, falling back to the boot's 400 kHz state. The v93 lesson:
        // the vendor's 8-clock settle after a clock change was missing.
        let cc0 = rd16(SDHCI_BASE, 0x2C);
        let hc0 = rd8(SDHCI_BASE, 0x28);
        log("em: hs_cc0=", cc0 as u64);
        log("em: hs_hc0=", hc0 as u64);

        // Rung 1: 25 MHz, still 1-bit / default timing.
        let mut r_ok = emmc_clock_set(4, log);
        if r_ok {
            let mut mbr = [0u32; 128];
            let (ok, sts, err) = sdhci_data_read(17, 0, &mut mbr, log);
            log("em: r1_rd=", ((ok as u64) << 32) | ((sts & 0xFFFF) as u64));
            log("em: r1_err=", err as u64);
            log("em: r1_mbr=", (mbr[127] & 0xFFFF) as u64);
            r_ok = ok && (mbr[127] & 0xFFFF) == 0xAA55;
        }
        log("em: r1_ok=", r_ok as u64);

        // Rung 2: full HS - CMD6 HS_TIMING=1, 50 MHz, CMD6 BUS_WIDTH=2,
        // host 8-bit + HISPD (the stock's HOST_CONTROL reads 0x34).
        let mut hs_ok = r_ok;
        if hs_ok {
            let (d_hs, s_hs, e_hs, _) = sdhci_cmd(6, 0x03B9_0100, 3, CMD_CRC | CMD_INDEX);
            log(
                "em: hs_sw1=",
                ((d_hs as u64) << 32) | ((s_hs & 0xFFFF) as u64),
            );
            log("em: hs_sw1e=", e_hs as u64);
            hs_ok = d_hs;
        }
        if hs_ok {
            hs_ok = emmc_clock_set(2, log);
        }
        if hs_ok {
            let (d_w, s_w, e_w, _) = sdhci_cmd(6, 0x03B7_0200, 3, CMD_CRC | CMD_INDEX);
            log(
                "em: hs_sw2=",
                ((d_w as u64) << 32) | ((s_w & 0xFFFF) as u64),
            );
            log("em: hs_sw2e=", e_w as u64);
            hs_ok = d_w;
        }
        if hs_ok {
            wr8(SDHCI_BASE, 0x28, hc0 | 0x24);
            log("em: hs_hc=", rd8(SDHCI_BASE, 0x28) as u64);
        }
        if hs_ok {
            // Verify: EXT_CSD bytes 185 (HS_TIMING) and 183 (BUS_WIDTH).
            let mut xs = [0u32; 128];
            let (ok_x, ok_s, ok_e) = sdhci_data_read(8, 0, &mut xs, log);
            log(
                "em: hs_xrd=",
                ((ok_x as u64) << 32) | ((ok_s & 0xFFFF) as u64),
            );
            log("em: hs_xerr=", ok_e as u64);
            let hs_tim = ((xs[46] >> 8) & 0xFF) as u64;
            let bus_w = ((xs[45] >> 24) & 0xFF) as u64;
            log("em: hs_rev=", (xs[48] & 0xFF) as u64);
            log("em: hs_tim=", hs_tim);
            log("em: hs_bw=", bus_w);
            hs_ok = ok_x && hs_tim == 1 && bus_w == 2;
        }
        if hs_ok {
            // Identity write at the new clock (hisitest1 sector 10).
            let mut w10 = [0u32; 128];
            let (r_ok2, _sr, _er) = sdhci_data_read(17, 1921034, &mut w10, log);
            let (w_ok, w_sts, w_err) = sdhci_data_write(24, 1921034, &w10, log);
            let mut w10v = [0u32; 128];
            let (v_ok, _sv, _ev) = sdhci_data_read(17, 1921034, &mut w10v, log);
            let mut d10 = 0u32;
            for i in 0..128 {
                if w10[i] != w10v[i] {
                    d10 += 1;
                }
            }
            log("em: hs_prd=", r_ok2 as u64);
            log("em: hs_pwr=", w_ok as u64);
            log("em: hs_psts=", w_sts as u64);
            log("em: hs_perr=", w_err as u64);
            log("em: hs_pver=", v_ok as u64);
            log("em: hs_pdf=", d10 as u64);
            hs_ok = r_ok2 && w_ok && v_ok && d10 == 0;
        }

        // Back off to the highest verified rung.
        if !hs_ok {
            wr8(SDHCI_BASE, 0x28, hc0);
            let (r1, _s1, _e1, _) = sdhci_cmd(6, 0x03B9_0000, 3, CMD_CRC | CMD_INDEX);
            let (r2, _s2, _e2, _) = sdhci_cmd(6, 0x03B7_0100, 3, CMD_CRC | CMD_INDEX);
            log("em: hs_rev1=", r1 as u64);
            log("em: hs_rev2=", r2 as u64);
            if r_ok {
                let _ = emmc_clock_set(4, log);
            } else {
                let _ = emmc_clock_set(cc0 >> 8, log);
            }
        }

        // The sequence which rung is live.
        let mut m2 = [0u32; 128];
        let (f_ok, _fs, _fe) = sdhci_data_read(17, 0, &mut m2, log);
        log(
            "em: final_rd=",
            ((f_ok as u64) << 32) | ((m2[127] & 0xFFFF) as u64),
        );
        log("em: hs_ok=", hs_ok as u64);
        let mut l = [b' '; 44];
        let mut at = put(&mut l, 0, b"EMH C8=");
        at = hex2(&mut l, at, ok8 as u8);
        at = put(&mut l, at, b" E=");
        at = hex8(&mut l, at, er8);
        let _ = at;
        super::ane_fb::paint(42, &l);
    }

    super::ane_fb::flush_panel();
    log("em: probe_done=", 1);
    log("em: stage=", stage as u64);
}

/// Recover the command path after a timeout/error: the SDHCI spec requires
/// software reset for CMD before it will issue anything again. The v66 run
/// showed a single CMD1 timeout leaving every following command silently
/// stuck (no completion, no error, full poll).
fn sdhci_reset_cmd() {
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
const CMD_CRC: u16 = 0x08;
const CMD_INDEX: u16 = 0x10;
const CMD_DATA: u16 = 0x20;

/// Single-block PIO read for a data command (vendor `sdhci_read_block_pio`
/// + `sdhci_transfer_pio`): write BLOCK_SIZE/BLOCK_COUNT, issue the
/// command with the data-present bit, poll PRESENT_STATE.DATA_AVAILABLE
/// (0x800) per word out of the buffer port 0x20, then wait for
/// TRANSFER_COMPLETE (NORMAL_INT bit1) or an error latch. Bounded
/// throughout; reports (complete, normal_int, error_status).
fn sdhci_data_read(idx: u16, arg: u32, words: &mut [u32], log: fn(&str, u64)) -> (bool, u32, u32) {
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
    (sts & 0x2 != 0, sts, err)
}

/// Single-block PIO write, the sibling of `sdhci_data_read`: same framing
/// but the transfer mode drops the READ bit and each word goes out only
/// while PRESENT_STATE.SPACE_AVAILABLE (0x400) says the buffer will take
/// it (vendor `sdhci_write_block_pio` + `sdhci_transfer_pio`). The card's
/// DAT0 programming busy is absorbed by the completion poll. Reports
/// (complete, normal_int, error_status).
fn sdhci_data_write(idx: u16, arg: u32, words: &[u32], log: fn(&str, u64)) -> (bool, u32, u32) {
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
fn emmc_clock_set(field: u16, log: fn(&str, u64)) -> bool {
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
    let t0 = crate::timer::uptime_us();
    while crate::timer::uptime_us().saturating_sub(t0) < 2000 {}
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

pub fn write_sectors(lba: u32, data: &[u8], log: fn(&str, u64)) -> (u64, u64) {
    const CHUNK: usize = 1024; // sectors per CMD25 run (512 KiB)
    let mut ok = 0u64;
    let mut bad = 0u64;
    let total = (data.len() + 511) / 512;
    // v336d: a flash is a guest in a live boot - it must return. The PIO
    // write is fast when CMD25 takes, and ~1000x slower when it falls back
    // to one CMD24 per sector; unbounded, the fallback held the boot thread
    // (and with it every USB service pass) for as long as the card took.
    // 150 s covers the whole 24 MB kernel partition at the fallback rate;
    // past that, report the failure instead of going quiet. The host's
    // fastboot client then gets a FAIL rather than a silent endpoint.
    let t0 = crate::timer::uptime_us();
    let mut i = 0usize;
    while i < total {
        if crate::timer::uptime_us().saturating_sub(t0) > 150_000_000 {
            log("em: wr_timeout=", i as u64);
            bad += (total - i) as u64;
            break;
        }
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
            let mut k = 0usize;
            while k < nblocks {
                let o = off + k * 512;
                let n = core::cmp::min(512, data.len() - o);
                let mut words = [0u32; 128];
                pack_words(&mut words, &data[o..o + n]);
                let (d, _s2, _e2) = sdhci_data_write(24, lba + (i + k) as u32, &words, log);
                if d {
                    ok += 1;
                } else {
                    bad += 1;
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
    (ok, bad)
}

/// Pack `data` (up to 512 bytes) into little-endian words, zero-padding
/// the tail of the final word.
fn pack_words(words: &mut [u32; 128], data: &[u8]) {
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
fn sdhci_data_write_multi(
    idx: u16,
    arg: u32,
    blocks: u16,
    data: &[u8],
    log: fn(&str, u64),
) -> (bool, u32, u32) {
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
    let nwords = (blocks as usize) * 128;
    let mut sent = 0u64;
    let mut wi = 0usize;
    while wi < nwords {
        // Pack this word straight from the byte slice (the 512 KiB chunk
        // must not be copied whole - no buffer that large exists here).
        let off = wi * 4;
        let mut w = 0u32;
        let mut k = 0usize;
        while k < 4 && off + k < data.len() {
            w |= (data[off + k] as u32) << (8 * k);
            k += 1;
        }
        let mut ready = false;
        for _ in 0..0x2_0000u32 {
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
        wr32(SDHCI_BASE, 0x20, w);
        sent += 1;
        wi += 1;
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
fn sdhci_cmd(idx: u16, arg: u32, resp: u16, extra: u16) -> (bool, u32, u32, u32) {
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
