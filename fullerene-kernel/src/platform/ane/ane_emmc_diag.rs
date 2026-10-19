//! The panel-side eMMC probe.
//!
//! `probe()` paints its findings straight onto the boot panel and copies
//! every value into the durable log, which is why it is the largest single
//! function in the driver and has nothing to do with moving sectors. It
//! lives here so that `ane_emmc` stays the transfer path.

use super::ane_emmc::*;
use super::ane_fb;
use super::common::mmio::{rd8, rd16, rd32, wr8, wr16, wr32};
use super::common::pmic::pmic_rd;

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

        // v336p: the vendor's `sdhci_of_arasan_platform_init`: the PHY has
        // to be powered and its resistor calibration finished before any
        // clock above the 400 kHz initialisation rate can be sampled. We
        // had been skipping it entirely, which is why every faster divider
        // read back garbage.
        phy_init(log);

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

    // v336n: the ladder above stops at the first rung it finds; this
    // climbs to the fastest divider that still reads and leaves the
    // result where `getvar emmc` can report it.
    clock_probe(log);

    super::ane_fb::flush_panel();
    log("em: probe_done=", 1);
    log("em: stage=", stage as u64);
}

