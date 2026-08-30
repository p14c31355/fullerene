//! DWC3 gadget defaults and Qualcomm QSCRATCH session configuration.

use super::super::usb_regs::*;
use super::mmio::*;
use super::trace::{TRACE_DWC3_REVISION_QUIRK, TRACE_QSCRATCH_BEGIN, trace_event};

pub(super) fn gadget_speed_value(mut dcfg: u32, super_speed: bool, snpsid: u32) -> u32 {
    dcfg &= !DCFG_SPEED_MASK;
    // Linux's DWC3 metastability workaround: revisions before 2.20a must
    // keep the device in the SuperSpeed DCFG mode even when the negotiated
    // link is expected to fall back to USB2. Selecting High-Speed here can
    // make DCTL.Run/Stop fail at the exact point where EP0 is armed.
    let force_superspeed = (snpsid & 0xffff_0000) == 0x5533_0000 && snpsid < DWC3_REVISION_220A;
    dcfg | if super_speed || force_superspeed {
        DCFG_SUPERSPEED
    } else {
        DCFG_HIGHSPEED
    }
}

/// Select the maximum PHY-backed speed immediately before gadget Run/Stop.
///
/// Linux repeats this selection in `dwc3_gadget_run_stop()` because the
/// controller may have changed DCFG while the endpoint state was prepared.
/// Keep the device address and NUMP policy intact; only replace the speed
/// field at this final connect boundary.
pub(super) unsafe fn configure_gadget_speed(super_speed: bool) {
    unsafe {
        let dcfg = gadget_speed_value(read(DCFG), super_speed, read(GSNPSID));
        write(DCFG, dcfg);
        let _ = read(DCFG);
    }
}

/// Match the PHY low-power boundary in Linux's `__dwc3_gadget_start()`.
///
/// `dwc3_gadget_run_stop()` temporarily clears these bits around the actual
/// DCTL transition and restores the values it observed.  Therefore they must
/// be enabled before the first Run/Stop command, otherwise the handoff leaves
/// USB2 SUSPHY disabled even though the controller has entered gadget mode.
#[inline]
pub(super) unsafe fn enable_gadget_susphy() {
    unsafe {
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_SUSPHY;
        write(GUSB2PHYCFG0, usb2);

        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);
    }
}

/// Restore only the USB2 PHY wake bit for the Bramble USB2 handoff A/B.
///
/// The direct path intentionally clears SUSPHY while rebuilding the DWC3
/// endpoint state. Android's Run/Stop path preserves an enabled USB2 PHY
/// across its transition, so keep this differential separate from the
/// SuperSpeed helper and avoid changing the USB3 side of the experiment.
#[inline]
pub(super) unsafe fn enable_usb2_gadget_susphy() {
    unsafe {
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_SUSPHY;
        write(GUSB2PHYCFG0, usb2);
    }
}

#[inline]
pub(super) fn run_stop_value(mut dctl: u32, snpsid: u32) -> u32 {
    // Stock Bramble XBL's DwcCoreInit writes APPL1RES and HIRD_THRES=7 after
    // endpoint setup. Keep that exact device-mode state through the probe's
    // Run/Stop transition; the generic Lito DT value remains the default for
    // non-probe paths.
    #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
    {
        dctl = (dctl & !DCTL_HIRD_THRES_MASK) | DCTL_HIRD_THRES_XBL;
        dctl |= DCTL_APPL1RES;
    }
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    {
        // Lito's DWC3 node supplies snps,hird-threshold = 0x10. A Fastboot
        // handoff can inherit a different value, so restore the platform
        // value at every device Run/Stop transition.
        dctl = (dctl & !DCTL_HIRD_THRES_MASK) | DCTL_HIRD_THRES_LITO;
    }
    dctl &= !DCTL_TRGTULST_MASK;
    if (snpsid & 0xffff_0000) == 0x5533_0000 {
        if snpsid <= DWC3_REVISION_187A {
            dctl |= DCTL_TRGTULST_RX_DET;
        } else if snpsid >= DWC3_REVISION_194A {
            // Linux clears KEEP_CONNECT for revisions that implement it;
            // leaving a bootloader-owned bit set suppresses the fresh
            // disconnect/reconnect boundary needed by a gadget handoff.
            dctl &= !DCTL_KEEP_CONNECT;
        }
    }
    // Lito/Bramble uses the DWC_usb31 IP (0x3331xxxx), whose revision is
    // reported separately through VER_NUMBER. The KEEP_CONNECT field is
    // still part of the gadget reconnect contract on that IP; preserving a
    // bootloader value can keep the old Fastboot session logically attached
    // while the new device session is already advertising its pull-up.
    if matches!(snpsid >> 16, DWC31_IP | DWC32_IP) {
        dctl &= !DCTL_KEEP_CONNECT;
    }
    dctl | DCTL_RUN_STOP
}

/// Apply the Android/Linux DWC3 global setup after a device/core reset.
///
/// DWC3 revisions before 1.90a can fail to connect at SuperSpeed, fall back to
/// High-Speed, and then enter a connect/disconnect loop.  The upstream driver
/// sets GCTL.U2RSTECN during core setup.  Keep the check runtime-based: a
/// `fastboot boot` handoff must not assume a particular DWC3 revision, and an
/// unrecognised GSNPSID must not cause us to overwrite an unknown GCTL bit.
#[inline]
pub(super) unsafe fn configure_dwc3_global_control() {
    unsafe {
        let snpsid = read(GSNPSID);
        if (snpsid & 0xffff_0000) != 0x5533_0000 {
            return;
        }
        let mut gctl = read(GCTL);
        gctl &= !(GCTL_SCALEDOWN_MASK | GCTL_DISSCRAMBLE);
        gctl |= GCTL_DSBLCLKGTNG;
        let mut applied = GCTL_DSBLCLKGTNG;
        if snpsid < DWC3_REVISION_190A {
            gctl |= GCTL_U2RSTECN;
            applied |= GCTL_U2RSTECN;
        }
        write(GCTL, gctl);
        let _ = read(GCTL);
        // The lito/bramble vendor DT sets snps,disable-clk-gating, which
        // overrides the generic pwropt-based logic in
        // dwc3_core_setup_global_control(): this platform ALWAYS runs with
        // clock gating disabled. CSFTRST cleared GCTL.RAMCLKSEL; restore the
        // previous owner's select so the internal endpoint RAM keeps its
        // working clock.
        super::reapply_ramclksel();
        let gctl = read(GCTL);
        trace_event(TRACE_DWC3_REVISION_QUIRK, snpsid, applied, gctl, 0, 0);
        // Linux enables the asynchronous ENDTRANSFER activation-bit
        // handling on DWC3 3.10a and later. The reset/rearm path uses
        // ENDTRANSFER to revoke the pre-reset EP0 resource before issuing a
        // fresh SETUP STARTTRANSFER; without this bit that command can remain
        // pending after a host USB reset.
        if snpsid >= DWC3_REVISION_310A {
            let mut guctl2 = read(GUCTL2);
            guctl2 |= GUCTL2_RST_ACTBITLATER;
            write(GUCTL2, guctl2);
        }
        configure_usb2_phy_interface();
    }
}

/// Reapply the DWC3-side USB2 interface contract after a controller reset.
///
/// Linux's `dwc3_hs_phy_setup()` selects the UTMI interface and programs the
/// 8-bit turnaround timing before gadget endpoint commands are issued. A
/// Fastboot handoff cannot rely on the bootloader's pre-reset register value:
/// CSFTRST restores the controller defaults while the external QUSB2 PHY and
/// Type-C session remain powered. Leaving the defaults in place can prevent
/// the device from reaching the first pull-up even though the PHY itself is
/// still electrically attached.
#[inline]
pub(super) unsafe fn configure_usb2_phy_interface() {
    unsafe {
        let mut usb2 = read(GUSB2PHYCFG0);
        // Bramble's DWC3 node uses the default UTMI mode. Clear the ULPI
        // selector and choose the Linux UTMI 8-bit timing values; preserve
        // the power-management bits because their policy is handled by the
        // surrounding run/stop guard.
        usb2 &= !(GUSB2PHYCFG_ULPI_UTMI | GUSB2PHYCFG_PHYIF_MASK | GUSB2PHYCFG_USBTRDTIM_MASK);
        // USBTRDTIM is in PHY clock cycles. Preserve the Linux 8-bit default
        // unless an explicit A/B overrides it. This allows the 16-bit PHYIF
        // bit to be tested separately from the nominal 5-cycle timing.
        let trdtim = match option_env!("FULLERENE_USB_USBTRDTIM") {
            Some("5") => 5,
            Some("6") => 6,
            Some("7") => 7,
            Some("8") => 8,
            Some("10") => 10,
            Some("11") => 11,
            Some("12") => 12,
            Some("13") => 13,
            Some("14") => 14,
            Some("15") => 15,
            #[cfg(fullerene_aarch64_usb_phyif_16bit)]
            _ => 5,
            #[cfg(not(fullerene_aarch64_usb_phyif_16bit))]
            _ => 9,
        };
        usb2 |= trdtim << 10;
        #[cfg(fullerene_aarch64_usb_phyif_16bit)]
        {
            // DWC3's PHYIF field is bit 3; bit 8 is ENBLSLPM, not PHYIF.
            usb2 |= GUSB2PHYCFG_PHYIF_MASK;
        }
        #[cfg(fullerene_aarch64_usb_enblslpm)]
        {
            // Bit 8 is ENBLSLPM. Keep this explicit A/B separate from PHYIF.
            usb2 |= GUSB2PHYCFG_ENBLSLPM;
        }
        #[cfg(fullerene_aarch64_usb_u2_freeclk_clear)]
        {
            // Linux treats a missing USB2 free clock as a device-tree quirk,
            // but the Fastboot handoff cannot rely on our DT interpretation.
            // Keep bit 30 as an explicit post-reset A/B while preserving all
            // other inherited bits.
            usb2 &= !GUSB2PHYCFG_U2_FREECLK_EXISTS;
        }
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);
    }
}

/// Apply the msm-4.19 DWC3 usb31 reference deltas that the generic
/// `configure_dwc3_global_control()` path skips.
///
/// `configure_dwc3_global_control()` early-returns when GSNPSID is not a
/// 0x5533xxxx DWC_usb3 core, but lito's DWC_usb31 core (0x3331xxxx) receives
/// extra programming in the vendor tree: 4.19 derives `revision` from
/// VER_NUMBER with the high bit set, which makes the unsigned 2.x/3.x
/// revision comparisons in `dwc3_core_setup_global_control()` evaluate
/// differently than on the generic path. Reproduce those guards verbatim
/// (msm-4.19 core.c:1003-1095, gadget.c:2732-2736) so the controller
/// reaches the reference bit state before the first endpoint command.
/// Non-usb31 cores are already covered by the generic path.
#[inline]
pub(super) unsafe fn apply_usb31_gadget_reference_deltas() {
    unsafe {
        let snpsid = read(GSNPSID);
        if snpsid >> 16 != DWC31_IP {
            return;
        }
        // 4.19 core.c dwc3_core_is_valid(): usb31 revision = VER_NUMBER |
        // 0x80000000, e.g. 1.70a GA reports VER_NUMBER 0x3137302a.
        let revision = read(VER_NUMBER) | DWC3_REVISION_IS_DWC31;
        // core.c ~1003: the rev >= 2.50a GUCTL1 block. A usb31 core only
        // receives DEV_L1_EXIT_BY_HW from it; PARKMODE_DISABLE is gated on
        // !usb31 and the TX_IPGAP linecheck quirk is not set in the lito DT.
        if revision >= DWC3_REVISION_250A {
            let mut reg = read(GUCTL1);
            if revision >= DWC3_REVISION_290A {
                reg |= GUCTL1_DEV_L1_EXIT_BY_HW;
            }
            write(GUCTL1, reg);
        }
        // core.c ~1060: usb31 1.70a GA only, STAR 9001346572.
        if revision == DWC3_USB31_REVISION_170A && read(VER_TYPE) == DWC3_USB31_VER_TYPE_GA {
            let mut reg = read(GUCTL3);
            reg |= GUCTL3_USB20_RETRY_DISABLE;
            write(GUCTL3, reg);
        }
        #[cfg(fullerene_aarch64_usb_guctl3_usb20_retry_clear)]
        {
            // Separate the silicon-revision condition from the vendor
            // workaround itself: force the retry engine to its reset default
            // even when the reference path would set it.
            let mut reg = read(GUCTL3);
            reg &= !GUCTL3_USB20_RETRY_DISABLE;
            write(GUCTL3, reg);
        }
        #[cfg(fullerene_aarch64_usb_guctl3_usb20_retry_set)]
        {
            // This is the explicit opposite A/B for the same STAR workaround,
            // independent of the exact VER_NUMBER/VER_TYPE reading.
            let mut reg = read(GUCTL3);
            reg |= GUCTL3_USB20_RETRY_DISABLE;
            write(GUCTL3, reg);
        }
        // core.c ~1085: rev >= 1.70a, widen the inter-packet gap for EL_23.
        if revision >= DWC3_USB31_REVISION_170A {
            let mut reg = read(GUCTL1);
            reg |= GUCTL1_IP_GAP_ADD_ON;
            write(GUCTL1, reg);
        }
        // gadget.c __dwc3_gadget_start: rev >= 2.70a NRDY pipeline limit.
        if revision >= DWC3_REVISION_270A {
            let mut reg = read(GSBUSCFG1);
            reg &= !GSBUSCFG1_PIPETRANSLIMIT_MASK;
            reg |= GSBUSCFG1_PIPETRANSLIMIT_E;
            write(GSBUSCFG1, reg);
        }
    }
}

/// Calculate DWC3.DCFG.NUMP from the receive FIFO capacity.
///
/// Linux derives this from the RAM2 depth and internal memory-bus width. Use
/// saturating arithmetic here because an uninitialised or cut-down hardware
/// parameter must not wrap the subtraction and accidentally request NUMP=16.
#[inline]
pub(super) fn gadget_nump(ram2_depth: u32, mdwidth_bits: u32) -> u32 {
    let fifo_bytes = (ram2_depth as u64).saturating_mul(mdwidth_bits as u64) / 8;
    (fifo_bytes.saturating_sub(24 + 16) / 1024).min(16) as u32
}

/// Apply the non-endpoint defaults from Linux's `__dwc3_gadget_start()`.
///
/// This is deliberately separate from EP0 setup: it only programs controller
/// receive-packet policy and DCFG.NUMP, and is called after DCFG's speed/address
/// fields have been established but before any endpoint command is issued.
#[inline]
pub(super) unsafe fn configure_gadget_start_defaults() {
    unsafe {
        // Linux disables event-interrupt moderation when no IMOD interval is
        // requested. Fastboot may leave DEV_IMOD(0) non-zero; clear it before
        // handing the event ring to the direct polling consumer so a pending
        // EP0 event is not held behind the previous owner's moderation state.
        write(DEV_IMOD0, 0);

        let snpsid = read(GSNPSID);
        let ip = snpsid >> 16;
        let pktcntsel = match ip {
            DWC3_IP => DWC3_GRXTHRCFG_PKTCNTSEL,
            DWC31_IP | DWC32_IP => DWC31_GRXTHRCFG_PKTCNTSEL,
            _ => return,
        };

        // Select DCFG.NUMP as the ACK-TP packet count source. This is the
        // same policy Linux uses to avoid letting the core choose a smaller
        // burst count than the receive FIFO can sustain.
        let rx_threshold = read(GRXTHRCFG) & !pktcntsel;
        write(GRXTHRCFG, rx_threshold);

        let mdwidth = (read(GHWPARAMS0) >> GHWPARAMS0_MDWIDTH_SHIFT) & GHWPARAMS0_MDWIDTH_MASK;
        let ram2_depth =
            (read(GHWPARAMS7) >> GHWPARAMS7_RAM2_DEPTH_SHIFT) & GHWPARAMS7_RAM2_DEPTH_MASK;
        let nump = gadget_nump(ram2_depth, mdwidth);
        let mut dcfg = read(DCFG) & !DCFG_NUMP_MASK;
        dcfg |= nump << DCFG_NUMP_SHIFT;
        // msm-4.19 does not program DCFG.IGNSTRMPP. Keep the post-reset 0
        // by default, while allowing one direct-path A/B to match current
        // mainline DWC3's gadget-start sequence exactly.
        #[cfg(fullerene_aarch64_usb_dcfg_ignstrmpp)]
        {
            dcfg |= DCFG_IGNSTRMPP;
        }
        #[cfg(not(fullerene_aarch64_usb_dcfg_ignstrmpp))]
        {
            dcfg &= !DCFG_IGNSTRMPP;
        }
        write(DCFG, dcfg);
    }
}

#[inline]
pub(super) unsafe fn qscratch_set(offset: usize, mask: u32) {
    trace_event(TRACE_QSCRATCH_BEGIN, offset as u32, mask, 0, 0, 0);
    let value = unsafe { read_qscratch(offset) } | mask;
    unsafe { write_qscratch(offset, value) };
    // The QCOM glue driver performs a readback to make the peripheral-mode
    // session vote visible before it starts the DWC3 core.
    let _ = unsafe { read_qscratch(offset) };
}

#[cfg(test)]
mod tests {
    use super::{
        DCFG_SPEED_MASK, DCFG_SUPERSPEED, DCTL_HIRD_THRES_LITO, DCTL_HIRD_THRES_MASK,
        DCTL_KEEP_CONNECT, DCTL_RUN_STOP, DCTL_TRGTULST_MASK, DCTL_TRGTULST_RX_DET,
        DWC3_REVISION_187A, DWC3_REVISION_194A, DWC3_REVISION_220A, gadget_nump,
        gadget_speed_value, run_stop_value,
    };

    #[test]
    fn gadget_nump_uses_linux_fifo_formula_and_cap() {
        assert_eq!(gadget_nump(0, 0), 0);
        assert_eq!(gadget_nump(1, 8), 0);
        assert_eq!(gadget_nump(512, 32), 1);
        assert_eq!(gadget_nump(4096, 64), 16);
    }

    #[test]
    fn run_stop_value_applies_linux_reconnect_quirks() {
        let old = DCTL_KEEP_CONNECT | (3 << 24) | DCTL_TRGTULST_MASK;
        let legacy = run_stop_value(old, DWC3_REVISION_187A);
        assert_eq!(legacy & DCTL_HIRD_THRES_MASK, DCTL_HIRD_THRES_LITO);
        assert_eq!(legacy & DCTL_TRGTULST_MASK, DCTL_TRGTULST_RX_DET);
        assert_ne!(legacy & DCTL_RUN_STOP, 0);

        let modern = run_stop_value(old, DWC3_REVISION_194A);
        assert_eq!(modern & DCTL_HIRD_THRES_MASK, DCTL_HIRD_THRES_LITO);
        assert_eq!(modern & DCTL_TRGTULST_MASK, 0);
        assert_eq!(modern & DCTL_KEEP_CONNECT, 0);
        assert_ne!(modern & DCTL_RUN_STOP, 0);
    }

    #[test]
    fn gadget_speed_value_changes_only_speed_field() {
        let old = 0x00a5_1234;
        let usb2 = gadget_speed_value(old, false, DWC3_REVISION_220A);
        assert_eq!(usb2 & DCFG_SPEED_MASK, 0);
        assert_eq!(usb2 & !DCFG_SPEED_MASK, old & !DCFG_SPEED_MASK);

        let superspeed = gadget_speed_value(usb2, true, DWC3_REVISION_220A);
        assert_eq!(superspeed & DCFG_SPEED_MASK, DCFG_SUPERSPEED);
        assert_eq!(superspeed & !DCFG_SPEED_MASK, old & !DCFG_SPEED_MASK);

        let legacy = gadget_speed_value(old, false, DWC3_REVISION_187A);
        assert_eq!(legacy & DCFG_SPEED_MASK, DCFG_SUPERSPEED);
    }
}
