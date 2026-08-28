//! DWC3 device-mode support for the Bramble USB-C port.
//!
//! The early gadget has one bounded vendor function, while its controller
//! lifecycle follows the Qualcomm platform contract: Type-C attach,
//! PHY/session state, the Android event-buffer layout, SMMU DMA, GIC/PDC
//! interrupts, EP0 disconnect/reset/error handling, and ordinary UDC data
//! requests are kept separate from protocol data.
//! Early boot polls as a recovery path when firmware retains GIC ownership;
//! the same event ring is drained from the IRQ handler once the GIC is live.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use super::{
    uart,
    usb_protocol::{
        ControlAction, Ep0Simulator, GSI_DEFAULT_NUM_BUFFERS, GadgetDriver,
        TRACE_CONTROL_ENTRY_BYTES, TRACE_CONTROL_HEADER_BYTES, TRACE_CONTROL_PAGE_ENTRIES,
        TRACE_CONTROL_REQUEST, TRACE_CONTROL_REQUEST_TYPE, UsbUdc, gsi_ring_shape,
    },
    usb_regs::*,
};

#[inline]
fn log_puts(message: &str) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    uart::puts(message);
}

#[inline]
fn log_hex(prefix: &str, value: u64) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    uart::put_hex(prefix, value);
}

#[inline]
fn log_hex_value(value: u64) {
    #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
    uart::put_hex_value(value);
}

unsafe extern "C" {
    static __usb_dma_start: u8;
    static __usb_dma_end: u8;
}

// Qualcomm APSS watchdog (kpss register layout). XBL/ABL arm it with a
// pet-time of ~27 s; nothing in the early handoff pets it, so the handset
// reboots (bark -> bite) ~8 s after the probe enters - exactly when host
// enumeration is in flight. Pet it at entry and from every poll loop.
const APSS_WDT_BASE: usize = 0x17c1_0000;
// The SM7250 "qcom,wdt" (watchdog_v2) uses the kpss register layout at this
// base (WDT0_RST=0x04, EN=0x08, STS=0x0C, BARK=0x10, BITE=0x14). The
// apcs-timer offsets belong to other SoC families; writing them here would
// hit reserved registers in the timer page.
const APSS_WDT_RST: usize = 0x4;
const APSS_WDT_EN: usize = 0x8;
const APSS_WDT_STS: usize = 0xc;
const APSS_WDT_BARK: usize = 0x10;
const APSS_WDT_BITE: usize = 0x14;
static mut WDT_TRACED: bool = false;
/// SCM result of the secure watchdog disable (0 = success; the sentinel
/// means the call has not run yet or ran out of retries).
static mut SWDD_RESULT: u64 = 0xFFFF_FFFF;
/// SCM IS_CALL_AVAIL result for (SVC_BOOT, SEC_WDOG_DIS): 1 = the TZ
/// implements the function, 0 = it answers but does not implement it,
/// 0xFFFF_FFFF = it returned ARM_SMCCC_UNK_FUNC on every convention
/// (the SMC path itself is dead, or the encoding is wrong). High word =
/// convention attempt index (1 = SMC_64, 2 = SMC_32).
static mut SWDD_AVAIL: u64 = 0xFFFF_FFFF;
/// WDT enable word captured at the first pet (probe entry).
/// 0xFFFF_FFFF = not captured yet.
static mut WDT_KPSS_EN_AT_ENTRY: u32 = 0xFFFF_FFFF;

#[inline]
unsafe fn wdt_reg(offset: usize) -> *mut u32 {
    (APSS_WDT_BASE + offset) as *mut u32
}

/// Deactivate the SECURE watchdog through SCM (the same call the downstream
/// watchdog_v2 driver exposes via its disable sysfs).
///
/// The bramble-generation TZ (msm-5.4 class) speaks the ARM SMCCC vendor
/// interface (driver `qcom_scm-64.c` + `watchdog_v2.c`):
///   x0 = ARM_SMCCC_CALL_VAL(ARM_SMCCC_STD_CALL, <convention>,
///                           ARM_SMCCC_OWNER_SIP,
///                           QCOM_SCM_FNID(QCOM_SCM_SVC_BOOT,
///                                         QCOM_SCM_BOOT_SEC_WDOG_DIS))
///      = 0x00 | {0x20 (SMC_64) | 0x00 (SMC_32)} | 0x40 | ((0x1 << 8) | 0x7)
///   x1 = arginfo = 1, x2 = args[0] = 1, x3..x7 = 0.
/// The vendor driver PROBES the TZ's convention at runtime, so try SMC_64
/// (0x0167) first and fall back to SMC_32 (0x0147). XBL/ABL arm this
/// watchdog for the `fastboot boot` path; an unpetted bite reboots the
/// handset ~17 s into every probe (bootreason=watchdog) no matter what
/// happens to the APSS WDT registers. If TZ is servicing another client it
/// answers QCOM_SCM_INTERRUPTED (1) and expects the call to be reissued
/// with x0 = 1 and the x6 continuation value it returned. The trace/`SWDD`
/// record keeps the attempt index (1 = SMC_64, 2 = SMC_32) in the upper
/// word of `SWDD_RESULT`.
/// SMCCC_VERSION (ARM_SMCCC_VERSION = 0x80000000) result captured at
/// probe entry: a nonzero (major<<16|minor) value proves the EL3 SMCCC
/// dispatch answers at all; 0xFFFF_FFFF means every SMC is faked or
/// trapped (the answer never came from a SMCCC handler).
static mut SWDD_STD: u64 = 0xFFFF_FFFF;
/// MDCR_EL2 (Memory Debug Control, EL2) captured at probe entry. Bit 14
/// (SMC) traps SMC instructions issued from EL0/EL1 into EL2: if the
/// bootloader left it set, every "SMC" we issue lands in the bootloader's
/// EL2 vector, never in TZ, and whatever that vector does (often a
/// default -1 return) is all we ever see.
static mut MDCR_EL2_AT_ENTRY: u64 = u64::MAX;
/// CurrentEL at probe entry: 0b0101 = EL1h (expected), 0b1000 = EL2h.
static mut CURRENT_EL_AT_ENTRY: u64 = u64::MAX;

/// One QCOM SCM SMC call with the QCOM_SCM_INTERRUPTED retry loop: on a
/// return of 1 the call is reissued with x0 = 1 and x6 = the continuation
/// value the firmware stored in a6 (the ARM_SMCCC_QUIRK_QCOM_A6 pattern).
/// Returns (a0, a6) of the final answer.
unsafe fn scm_smc_call_once(fnid: u64, arginfo: u64, a0: u64, a1: u64, a2: u64) -> (u64, u64) {
    let mut fnid = fnid;
    let mut a6: u64 = 0;
    let mut result: u64 = 0xFFFF_FFFF;
    for _ in 0..100 {
        let mut a6_out: u64 = 0;
        core::arch::asm!(
            "mov x0, {fnid}",
            "mov x1, {arginfo}",
            "mov x2, {a0v}",
            "mov x3, {a1v}",
            "mov x4, {a2v}",
            "mov x5, xzr",
            "mov x6, {a6in}",
            "mov x7, xzr",
            "smc #0",
            "mov {result}, x0",
            "mov {a6out}, x6",
            fnid = in(reg) fnid,
            arginfo = in(reg) arginfo,
            a0v = in(reg) a0,
            a1v = in(reg) a1,
            a2v = in(reg) a2,
            a6in = in(reg) a6,
            result = out(reg) result,
            a6out = out(reg) a6_out,
            out("x1") _,
            out("x2") _,
            out("x3") _,
            out("x4") _,
            out("x5") _,
            out("x7") _,
            options(nostack)
        );
        if result != 1 {
            break;
        }
        // QCOM_SCM_INTERRUPTED: reissue with the continuation value.
        fnid = 1;
        a6 = a6_out;
    }
    (result, a6)
}

/// ARM_SMCCC_CALL_VAL(ARM_SMCCC_STD_CALL, conv, OWNER_SIP=2, fn) per the
/// 5.4 qcom_scm driver: TYPE(31)=0, CONV(30), OWNER(24..29)=2, FN(0..15).
#[inline]
fn scm_oen(conv: u64, fnid: u32) -> u64 {
    (conv << 30) | (2u64 << 24) | (fnid as u64)
}

pub fn secure_wdt_disable() -> bool {
    // One legacy-encoding QCOM SMC at probe entry: SMC id = 0x02000000 |
    // (SCM_SVC_BOOT << 10) | SCM_SVC_SEC_WDOG_DIS = 0x02000407, arginfo = 1,
    // args[0] = 1. This is the minimal entry that the fastboot handoff has
    // been proven to tolerate on this target: the extended multi-SMC
    // diagnostic sequence issued at entry (SMCCC VERSION, IS_CALL_AVAIL,
    // both-convention SEC_WDOG_DIS) deterministically wedges the handoff in
    // the standalone probe, so those probes live in secure_wdt_probes() and
    // run only in the signal-probe build, where the gate consumes them.
    let result = unsafe { scm_smc_call_once(0x0200_0407, 1, 1, 0, 0) }.0;
    unsafe {
        SWDD_RESULT = result;
    }
    trace_event(
        TRACE_PROBE_WATCHDOG,
        0x5357_4444, // "SWDD"
        result as u32,
        0,
        0,
        0,
    );
    result == 0
}

/// Extended SMCCC diagnostics consumed by the `swdd-*` / `scm-*` / `std-*` /
/// `mdcr-*` / `el1` / `el2` signal gates. Issued at probe entry, BEFORE any
/// SMC that the standalone build performs at entry is multiplied out, and
/// never in the standalone build (its multi-SMC entry sequence wedges the
/// fastboot handoff; see secure_wdt_disable).
pub fn secure_wdt_probes() {
    // Capture the exception-level context BEFORE issuing any SMC: if the
    // bootloader left MDCR_EL2.SMC (bit 14) set, SMC from EL1 traps to EL2
    // and never reaches TZ, which would fake every one of these probes.
    unsafe {
        let mdcr: u64;
        let cur_el: u64;
        core::arch::asm!(
            "mrs {m}, MDCR_EL2",
            "mrs {c}, CurrentEL",
            m = out(reg) mdcr,
            c = out(reg) cur_el,
            options(nostack)
        );
        MDCR_EL2_AT_ENTRY = mdcr;
        CURRENT_EL_AT_ENTRY = cur_el;
        trace_event(
            TRACE_PROBE_WATCHDOG,
            0x4D44_4352, // "MDCR"
            mdcr as u32,
            (mdcr >> 32) as u32,
            cur_el as u32,
            0,
        );
    }

    // Baseline probe: ARM SMCCC VERSION (OEN 0x80000000, no args). Per the
    // SMCCC spec every compliant EL3 handler must answer this, so it
    // separates "the SMC never reaches a SMCCC handler" from "the handler
    // works but does not carry the QCOM SIP service table".
    let std_result = unsafe { scm_smc_call_once(0x8000_0000, 0, 0, 0, 0) }.0;
    unsafe {
        SWDD_STD = std_result;
    }
    trace_event(
        TRACE_PROBE_WATCHDOG,
        0x5343_4D53, // "SCMS"
        std_result as u32,
        0,
        0,
        0,
    );

    // Diagnostic probe: QCOM_SCM_IS_CALL_AVAIL(SVC_INFO=0x06, CMD=0x01) with
    // args (SVC_BOOT, SEC_WDOG_DIS). The vendor driver itself calls this to
    // probe the convention, so the function is guaranteed present in a
    // working TZ. Its answer separates "SMC path dead / encoding wrong"
    // (UNK_FUNC = -1) from "the SMC works but the SEC_WDOG_DIS function is
    // absent" (0). fnid = (0x06<<8)|0x01 = 0x0601; args = (SVC_BOOT, 0x07).
    let mut avail_attempt: u32 = 0;
    let mut avail: u64 = 0xFFFF_FFFF;
    for conv in [scm_oen(1, 0x0601), scm_oen(0, 0x0601)] {
        avail_attempt += 1;
        avail = unsafe { scm_smc_call_once(conv, 2, 0x01, 0x07, 0) }.0;
        if avail != 0xFFFF_FFFF {
            break;
        }
    }
    unsafe {
        SWDD_AVAIL = ((avail_attempt as u64) << 32) | avail;
    }
    trace_event(
        TRACE_PROBE_WATCHDOG,
        0x5343_4D41, // "SCMA"
        avail as u32,
        avail_attempt,
        0,
        0,
    );
}

/// Restart the apps watchdog countdown and, once, record its configuration
/// in the retained trace, disabling it if it is armed.
pub fn wdt_pet() {
    unsafe {
        if !WDT_TRACED {
            WDT_TRACED = true;
            let en = read_volatile(wdt_reg(APSS_WDT_EN));
            WDT_KPSS_EN_AT_ENTRY = en;
            let bark = read_volatile(wdt_reg(APSS_WDT_BARK));
            let bite = read_volatile(wdt_reg(APSS_WDT_BITE));
            let sts = read_volatile(wdt_reg(APSS_WDT_STS));
            trace_event(
                TRACE_PROBE_WATCHDOG,
                0x5744_5430, // "WDT0"
                en,
                bark,
                bite,
                sts,
            );
            // Re-arm the countdown with OUR timeout: the downstream
            // watchdog_v2 configures bark = qcom,bark-time (+console offset)
            // and bite = bark + 3 s at a 32765 Hz watchdog clock. Writing a
            // 100 s bark / 110 s bite here moves an APSS-WDT-driven reboot
            // out of the enumeration window entirely; if the ~17 s reboot
            // still happens, the source is a different watchdog.
            let bark_100s: u32 = 100 * 32765;
            let bite_110s: u32 = 110 * 32765;
            write_volatile(wdt_reg(APSS_WDT_BARK), bark_100s);
            write_volatile(wdt_reg(APSS_WDT_BITE), bite_110s);
            write_volatile(wdt_reg(APSS_WDT_RST), 1);
            write_volatile(wdt_reg(APSS_WDT_EN), 1);
            core::arch::asm!("dsb sy", options(nostack));
            let en_after = read_volatile(wdt_reg(APSS_WDT_EN));
            trace_event(
                TRACE_PROBE_WATCHDOG,
                0x5744_5441, // "WDTA" armed by probe
                en,
                en_after,
                bark_100s >> 16,
                bite_110s >> 16,
            );
        }
        // The downstream watchdog_v2 pets by writing 1 (not 0) to RST.
        write_volatile(wdt_reg(APSS_WDT_RST), 1);
        core::arch::asm!("dsb sy", options(nostack));
    }
}

#[inline]
fn dwc3_base() -> usize {
    super::platform::bramble::usb_resources().dwc3_base
}
// Lito/SM7250's Apps SMMU owns the DWC3 stream ID declared by the board DT.
// The early Bramble path installs a small identity map in a context bank so
// the USB buffers remain inside the IOVA pool declared by the vendor DT.
// Google’s Bramble/Lito DTS places apps-smmu at 0x15000000.  The nearby
// 0x0c600000 range is the SPMI arbiter channel window, not the Apps SMMU;
// confusing the two makes the SMMU identity-map setup target unrelated PMIC
// registers before the first EP0 transfer.
const SMMU_ID0: usize = 0x20;
const SMMU_ID1: usize = 0x24;
const SMMU_ID0_NUMSMRG_MASK: u32 = 0xff;
const SMMU_ID1_PAGESIZE: u32 = 1 << 31;
const SMMU_ID1_NUMPAGENDXB_SHIFT: u32 = 28;
const SMMU_ID1_NUMPAGENDXB_MASK: u32 = 0x7;
const SMMU_ID1_NUMS2CB_SHIFT: u32 = 16;
const SMMU_ID1_NUMS2CB_MASK: u32 = 0xff;
const SMMU_ID1_NUMCB_MASK: u32 = 0xff;
const SMMU_SMR_BASE: usize = 0x800;
const SMMU_S2CR_BASE: usize = 0xc00;
const SMMU_TLB_ALL_H: usize = 0x6c;
const SMMU_TLB_SYNC: usize = 0x70;
const SMMU_TLB_STATUS: usize = 0x74;
const SMMU_TLB_STATUS_ACTIVE: u32 = 1;
const SMMU_SMR_VALID: u32 = 1 << 31;
const SMMU_SMR_MASK_SHIFT: u32 = 16;
const SMMU_S2CR_TYPE_MASK: u32 = 0x3 << 16;
const SMMU_S2CR_TYPE_TRANS: u32 = 0;
const SMMU_S2CR_TYPE_BYPASS: u32 = 1 << 16;
const SMMU_S2CR_CBNDX_MASK: u32 = 0xff;
const SMMU_GR1_CBAR_BASE: usize = 0x00;
const SMMU_GR1_CBA2R_BASE: usize = 0x800;
const SMMU_CBA2R_VA64: u32 = 1;
const SMMU_CBAR_IRPTNDX_MASK: u32 = 0xff;
const SMMU_CBAR_S1_TRANS_S2_BYPASS: u32 = 1 << 16;
const SMMU_CBAR_S1_MEMATTR_WB: u32 = 0xf << 12;
const SMMU_CBAR_S1_BPSHCFG_NSH: u32 = 3 << 8;
const SMMU_CB_SCTLR: usize = 0x00;
const SMMU_CB_TCR2: usize = 0x10;
const SMMU_CB_TTBR0: usize = 0x20;
const SMMU_CB_TTBR1: usize = 0x28;
const SMMU_CB_TCR: usize = 0x30;
const SMMU_CB_CONTEXTIDR: usize = 0x34;
const SMMU_CB_MAIR0: usize = 0x38;
const SMMU_CB_MAIR1: usize = 0x3c;
const SMMU_CB_RESUME: usize = 0x08;
const SMMU_CB_FSR: usize = 0x58;
const SMMU_CB_FAR: usize = 0x60;
const SMMU_CB_FSYNR0: usize = 0x68;
const SMMU_GR0_FSR: usize = 0x48;
const SMMU_GR0_FSYNR0: usize = 0x50;
const SMMU_RESUME_TERMINATE: u32 = 1;
const SMMU_GLOBAL_FSR_FAULT: u32 = 1 << 1;
const SMMU_FSR_SS: u32 = 1 << 30;
const SMMU_FSR_FAULT: u32 = (1 << 31)
    | (1 << 30)
    | (1 << 8)
    | (1 << 7)
    | (1 << 6)
    | (1 << 5)
    | (1 << 4)
    | (1 << 3)
    | (1 << 2)
    | (1 << 1);
const SMMU_SCTLR_S1_ASIDPNE: u32 = 1 << 12;
const SMMU_SCTLR_CFIE: u32 = 1 << 6;
const SMMU_SCTLR_CFRE: u32 = 1 << 5;
const SMMU_SCTLR_AFE: u32 = 1 << 2;
const SMMU_SCTLR_TRE: u32 = 1 << 1;
const SMMU_SCTLR_M: u32 = 1;
const SMMU_GR0_SCR0: usize = 0x00;
const SMMU_SCR0_GFRE: u32 = 1 << 1;
const SMMU_SCR0_GFIE: u32 = 1 << 2;
const SMMU_SCR0_GCFGFRE: u32 = 1 << 4;
const SMMU_SCR0_GCFGFIE: u32 = 1 << 5;
const SMMU_TCR_EPD1: u32 = 1 << 23;
const SMMU_TCR_SH0_INNER: u32 = 3 << 12;
const SMMU_TCR_ORGN0_WBWA: u32 = 1 << 10;
const SMMU_TCR_IRGN0_WBWA: u32 = 1 << 8;
const SMMU_TCR_T0SZ_32BIT: u32 = 32;
const SMMU_TCR_T0SZ_39BIT: u32 = 25;
const SMMU_TCR2_SEP_UPSTREAM: u32 = 0x7 << 15;
const SMMU_TCR2_AS: u32 = 1 << 4;
const SMMU_TCR2_PASIZE_40BIT: u32 = 2;

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct SmmuTable([u64; 512]);

// With T0SZ=32 and a 4 KiB granule, TTBR0 points at a level-1 table. The
// vendor DT's 0x90000000..0xf0000000 pool is represented by level-2 2 MiB
// identity blocks; addresses outside the pool are left unmapped. These tables
// are cleared together with the other USB DMA objects.
#[unsafe(link_section = ".usb_dma")]
static mut SMMU_L1: SmmuTable = SmmuTable([0; 512]);
#[unsafe(link_section = ".usb_dma")]
static mut SMMU_L2: [SmmuTable; 4] = [SmmuTable([0; 512]); 4];
static mut SMMU_CONTEXT_PAGE: usize = usize::MAX;
static mut SMMU_CONTEXT_PAGE_SIZE: usize = 0;

const SMMU_DESC_VALID: u64 = 1;
const SMMU_DESC_TABLE: u64 = 3;
const SMMU_DESC_BLOCK: u64 = 1;
const SMMU_DESC_TYPE_MASK: u64 = 3;
const SMMU_DESC_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const SMMU_DESC_AF: u64 = 1 << 10;
const SMMU_DESC_SH_INNER: u64 = 3 << 8;
const SMMU_DESC_ATTR_NORMAL: u64 = 0;
const SMMU_DESC_XN: u64 = (1 << 53) | (1 << 54);
#[inline]
fn apps_smmu_base() -> usize {
    super::platform::bramble::usb_resources().apps_smmu_base
}

#[inline]
fn hsphy_base() -> usize {
    super::platform::bramble::usb_resources().hs_phy_base
}

#[inline]
fn qmp_base() -> usize {
    super::platform::bramble::usb_resources().qmp_phy_base
}
// SM7250 exposes the Qualcomm glue/QSCRATCH block immediately above the
// DWC3 core.  The glue must report the cable's VBUS/session to the core when
// we take over directly from the bootloader.
#[inline]
fn qscratch_base() -> usize {
    super::platform::bramble::usb_resources().qscratch_base
}
const QSCRATCH_HS_PHY_CTRL: usize = 0x10;
const QSCRATCH_CGCTL: usize = 0x28;
const QSCRATCH_SS_PHY_CTRL: usize = 0x30;
const QSCRATCH_GENERAL_CFG: usize = 0x08;
const QSCRATCH_GENERAL_CFG_XHCI_REV: u32 = 1 << 2;
// Qualcomm glue power-event status/mask registers. These are consumed by
// dwc3-msm's threaded power IRQ, not by the DWC3 event ring.
const QSCRATCH_PWR_EVENT_STATUS: usize = 0x58;
const QSCRATCH_PWR_EVENT_MASK: usize = 0x5c;
const PWR_EVENT_POWERDOWN_IN_P3: u32 = 1 << 2;
const PWR_EVENT_POWERDOWN_OUT_P3: u32 = 1 << 3;
const PWR_EVENT_LPM_IN_L2: u32 = 1 << 4;
const PWR_EVENT_LPM_OUT_L2: u32 = 1 << 5;
const PWR_EVENT_LPM_OUT_L1: u32 = 1 << 13;

const GCTL: usize = 0xc110;
const GUCTL: usize = 0xc12c;
const GUCTL2: usize = 0xc19c;
// DWC3_GUCTL1 is part of the global register block immediately after GCTL;
// 0xc360 is in the FIFO-register area and is not a user-control register.
const GUCTL1: usize = 0xc11c;
const GSNPSID: usize = 0xc120;
const GRXTHRCFG: usize = 0xc10c;
const GHWPARAMS0: usize = 0xc140;
const GHWPARAMS1: usize = 0xc144;
const GHWPARAMS3: usize = 0xc14c;
const GHWPARAMS7: usize = 0xc15c;
const VER_NUMBER: usize = 0xc1a0;
const GFLADJ: usize = 0xc630;
const GUSB2PHYCFG0: usize = 0xc200;
const GUSB3PIPECTL0: usize = 0xc2c0;
const GUSB2PHYCFG_ULPI_UTMI: u32 = 1 << 4;
const GUSB2PHYCFG_PHYIF_MASK: u32 = 1 << 3;
const GUSB2PHYCFG_USBTRDTIM_MASK: u32 = 0xf << 10;
const GUSB2PHYCFG_USBTRDTIM_UTMI_8_BIT: u32 = 9 << 10;
const GEVNTADRLO0: usize = 0xc400;
const GEVNTADRHI0: usize = 0xc404;
const GEVNTSIZ0: usize = 0xc408;
const GEVNTCOUNT0: usize = 0xc40c;
const DEV_IMOD0: usize = 0xca00;
const GEVNT_BUFFER_STRIDE: usize = 0x10;
const DCFG: usize = 0xc700;
const DCTL: usize = 0xc704;
const DEVTEN: usize = 0xc708;
const DSTS: usize = 0xc70c;
const DALEPENA: usize = 0xc720;
const DEP_BASE: usize = 0xc800;

const GCTL_PRTCAPDIR_MASK: u32 = 3 << 12;
const GCTL_PRTCAP_DEVICE: u32 = 2 << 12;
const GCTL_U2RSTECN: u32 = 1 << 16;
const GCTL_SCALEDOWN_MASK: u32 = 3 << 4;
const GCTL_DISSCRAMBLE: u32 = 1 << 3;
const GCTL_CORESOFTRESET: u32 = 1 << 11;
const GCTL_DSBLCLKGTNG: u32 = 1;
const GUCTL_REFCLKPER_MASK: u32 = 0xffc0_0000;
const GUCTL_REFCLKPER_19_2MHZ: u32 = 52 << 22;
const GFLADJ_REFCLK_FLADJ_MASK: u32 = 0x003f_ff00;
const GFLADJ_REFCLK_LPM_SEL: u32 = 1 << 23;
const GFLADJ_REFCLK_240MHZ_DECR: u32 = 12 << 24;
const GFLADJ_REFCLK_240MHZDECR_PLS1: u32 = 1 << 31;
const GFLADJ_REFCLK_FLADJ_19_2MHZ: u32 = 200 << 8;
const GUSB2PHYCFG_SUSPHY: u32 = 1 << 6;
const GUSB2PHYCFG_ENBLSLPM: u32 = 1 << 8;
const GUSB2PHYCFG_PHYSOFTRST: u32 = 1 << 31;
const GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST: u32 = 1 << 8;
const GUSB3PIPECTL_SUSPHY: u32 = 1 << 17;
const GUSB3PIPECTL_PHYSOFTRST: u32 = 1 << 31;

const DCTL_CSFTRST: u32 = 1 << 30;
const DCTL_HIRD_THRES_MASK: u32 = 0x1f << 24;
const DCTL_HIRD_THRES_LITO: u32 = 0x10 << 24;
const DCTL_KEEP_CONNECT: u32 = 1 << 19;
const DCTL_TRGTULST_MASK: u32 = 0x0f << 17;
const DCTL_TRGTULST_RX_DET: u32 = 5 << 17;
const DCFG_NUMP_SHIFT: u32 = 17;
const DCFG_NUMP_MASK: u32 = 0x1f << DCFG_NUMP_SHIFT;
const DCFG_IGNSTRMPP: u32 = 1 << 23;
const DWC3_GRXTHRCFG_PKTCNTSEL: u32 = 1 << 29;
const DWC31_GRXTHRCFG_PKTCNTSEL: u32 = 1 << 26;
const GHWPARAMS0_MDWIDTH_SHIFT: u32 = 8;
const GHWPARAMS0_MDWIDTH_MASK: u32 = 0xff;
const GHWPARAMS7_RAM2_DEPTH_SHIFT: u32 = 16;
const GHWPARAMS7_RAM2_DEPTH_MASK: u32 = 0xffff;
const DWC3_IP: u32 = 0x5533;
const DWC31_IP: u32 = 0x3331;
const DWC32_IP: u32 = 0x3332;
const DWC31_REVISION_180A: u32 = 0x3138_302a;
const DWC31_REVISION_190A: u32 = 0x3139_302a;
// Linux applies the RxDetect reconnect workaround only through DWC3 1.87a.
// GSNPSID carries the same full revision value used by the upstream driver.
const DWC3_REVISION_187A: u32 = 0x5533_187a;
const DWC3_REVISION_190A: u32 = 0x5533_190a;
const DWC3_REVISION_194A: u32 = 0x5533_194a;
const DWC3_REVISION_220A: u32 = 0x5533_220a;
const DWC3_REVISION_250A: u32 = 0x5533_250a;
const DWC3_REVISION_310A: u32 = 0x5533_310a;
const GUCTL2_RST_ACTBITLATER: u32 = 1 << 14;

const HSPHY_UTMI_CTRL0: usize = 0x3c;
const HSPHY_UTMI_CTRL5: usize = 0x50;
const HSPHY_COMMON0: usize = 0x54;
const HSPHY_COMMON1: usize = 0x58;
const HSPHY_COMMON2: usize = 0x5c;
const HSPHY_CTRL1: usize = 0x60;
const HSPHY_CTRL2: usize = 0x64;
const HSPHY_CFG0: usize = 0x94;
const HSPHY_REFCLK_CTRL: usize = 0xa0;
const HSPHY_RTUNE_SEL: usize = 0xb4;
const HSPHY_TEST0: usize = 0x80;
const HSPHY_TEST1: usize = 0x84;

const HSPHY_UTMI_SLEEPM: u32 = 1 << 0;
const HSPHY_UTMI_ATE_RESET: u32 = 1 << 0;
const HSPHY_UTMI_POR: u32 = 1 << 1;
const HSPHY_COMMON0_FSEL_MASK: u32 = 0x7 << 4;
const HSPHY_COMMON0_VATESTENB_MASK: u32 = 0x3;
const HSPHY_COMMON1_VBUSVLDEXTSEL0: u32 = 1 << 4;
const HSPHY_COMMON1_PLLBTUNE: u32 = 1 << 5;
const HSPHY_COMMON2_VREGBYPASS: u32 = 1 << 0;
const HSPHY_CTRL1_VBUSVLDEXT0: u32 = 1 << 0;
const HSPHY_CTRL2_SUSPEND_N: u32 = 1 << 2;
const HSPHY_CTRL2_SUSPEND_N_SEL: u32 = 1 << 3;
const HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN: u32 = 1 << 1;
const HSPHY_TEST1_TESTDATAOUTSEL: u32 = 1 << 4;
const HSPHY_TEST1_TOGGLE_2WR: u32 = 1 << 6;
const HSPHY_TEST0_DATA_MASK: u32 = 0xff;

const PIPE_UTMI_CLK_SEL: u32 = 1 << 0;
const PIPE3_PHYSTATUS_SW: u32 = 1 << 3;
const PIPE_UTMI_CLK_DIS: u32 = 1 << 8;

// Qualcomm's Android wrapper reserves event buffers 1..N for GSI. These
// fields are part of the DWC3 event-buffer ABI, not ordinary endpoint
// registers, so keep the encoding next to the event-ring setup.
const GSI_TRB_ADDR_BIT_53: u32 = 1 << 21;
const GSI_TRB_ADDR_BIT_55: u32 = 1 << 23;
const GSI_CLK_EN: u32 = 1 << 12;
const GSI_RESTART_DBL_PNTR: u32 = 1 << 20;
const GSI_EN: u32 = 1 << 0;
const GSI_BLOCK_WR_GO: u32 = 1 << 1;
const GSI_EVENT_INTR_MASK: u32 = 1 << 31;
const GSI_EVENT_ADDR_EN_SHIFT: u32 = 22;
const GSI_EVENT_ADDR_INDEX_SHIFT: u32 = 16;
const GSI_WR_CTRL_STATE: u32 = 1 << 15;

const QMP_COM_PHY_MODE_CTRL: usize = 0x0000;
const QMP_COM_SW_RESET: usize = 0x0004;
const QMP_COM_POWER_DOWN_CTRL: usize = 0x0008;
const QMP_COM_TYPEC_CTRL: usize = 0x0010;
const QMP_COM_RESET_OVRD_CTRL: usize = 0x001c;
const QMP_PCS_STATUS1: usize = 0x1c14;
const QMP_PCS_AUTONOMOUS_MODE_CTRL: usize = 0x1f08;
const QMP_PCS_LFPS_RXTERM_IRQ_CLEAR: usize = 0x1f14;
const QMP_PCS_CLAMP_ENABLE: usize = 0x1c8c;
const QMP_PCS_POWER_DOWN_CONTROL: usize = 0x1c40;
const QMP_PCS_SW_RESET: usize = 0x1c00;
const QMP_PCS_START_CONTROL: usize = 0x1c44;
const QMP_PHYSTATUS: u32 = 1 << 6;
const QMP_ARCVR_DTCT_EN: u32 = 1 << 0;
const QMP_ALFPS_DTCT_EN: u32 = 1 << 1;
const QMP_ARCVR_DTCT_EVENT_SEL: u32 = 1 << 4;
const QMP_LFPS_IRQ_CLEAR: u32 = 1 << 0;
const QMP_CLAMP_EN: u32 = 1 << 0;

const QMP_INIT: [(usize, u32); 146] = [
    (0x1010, 0x01), // USB3_DP_QSERDES_COM_SSC_EN_CENTER
    (0x101c, 0x31), // USB3_DP_QSERDES_COM_SSC_PER1
    (0x1020, 0x01), // USB3_DP_QSERDES_COM_SSC_PER2
    (0x1024, 0xde), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE1_MODE0
    (0x1028, 0x07), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE2_MODE0
    (0x1030, 0xde), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE1_MODE1
    (0x1034, 0x07), // USB3_DP_QSERDES_COM_SSC_STEP_SIZE2_MODE1
    (0x1050, 0x0a), // USB3_DP_QSERDES_COM_SYSCLK_BUF_ENABLE
    (0x1060, 0x20), // USB3_DP_QSERDES_COM_CMN_IPTRIM
    (0x1074, 0x06), // USB3_DP_QSERDES_COM_CP_CTRL_MODE0
    (0x1078, 0x06), // USB3_DP_QSERDES_COM_CP_CTRL_MODE1
    (0x107c, 0x16), // USB3_DP_QSERDES_COM_PLL_RCTRL_MODE0
    (0x1080, 0x16), // USB3_DP_QSERDES_COM_PLL_RCTRL_MODE1
    (0x1084, 0x36), // USB3_DP_QSERDES_COM_PLL_CCTRL_MODE0
    (0x1088, 0x36), // USB3_DP_QSERDES_COM_PLL_CCTRL_MODE1
    (0x1094, 0x1a), // USB3_DP_QSERDES_COM_SYSCLK_EN_SEL
    (0x10a4, 0x04), // USB3_DP_QSERDES_COM_LOCK_CMP_EN
    (0x10ac, 0x14), // USB3_DP_QSERDES_COM_LOCK_CMP1_MODE0
    (0x10b0, 0x34), // USB3_DP_QSERDES_COM_LOCK_CMP2_MODE0
    (0x10b4, 0x34), // USB3_DP_QSERDES_COM_LOCK_CMP1_MODE1
    (0x10b8, 0x82), // USB3_DP_QSERDES_COM_LOCK_CMP2_MODE1
    (0x10bc, 0x82), // USB3_DP_QSERDES_COM_DEC_START_MODE0
    (0x10c4, 0x82), // USB3_DP_QSERDES_COM_DEC_START_MODE1
    (0x10cc, 0xab), // USB3_DP_QSERDES_COM_DIV_FRAC_START1_MODE0
    (0x10d0, 0xea), // USB3_DP_QSERDES_COM_DIV_FRAC_START2_MODE0
    (0x10d4, 0x02), // USB3_DP_QSERDES_COM_DIV_FRAC_START3_MODE0
    (0x10d8, 0xab), // USB3_DP_QSERDES_COM_DIV_FRAC_START1_MODE1
    (0x10dc, 0xea), // USB3_DP_QSERDES_COM_DIV_FRAC_START2_MODE1
    (0x10e0, 0x02), // USB3_DP_QSERDES_COM_DIV_FRAC_START3_MODE1
    (0x110c, 0x02), // USB3_DP_QSERDES_COM_VCO_TUNE_MAP
    (0x1110, 0x24), // USB3_DP_QSERDES_COM_VCO_TUNE1_MODE0
    (0x1118, 0x24), // USB3_DP_QSERDES_COM_VCO_TUNE1_MODE1
    (0x111c, 0x02), // USB3_DP_QSERDES_COM_VCO_TUNE2_MODE1
    (0x1158, 0x01), // USB3_DP_QSERDES_COM_HSCLK_SEL
    (0x116c, 0x08), // USB3_DP_QSERDES_COM_CORECLK_DIV_MODE1
    (0x11ac, 0xca), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE1_MODE0
    (0x11b0, 0x1e), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE2_MODE0
    (0x11b4, 0xca), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE1_MODE1
    (0x11b8, 0x1e), // USB3_DP_QSERDES_COM_BIN_VCOCAL_CMP_CODE2_MODE1
    (0x11bc, 0x11), // USB3_DP_QSERDES_COM_BIN_VCOCAL_HSCLK_SEL
    (0x1234, 0x00), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_TX
    (0x1238, 0x00), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_RX
    (0x123c, 0x16), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_OFFSET_TX
    (0x1240, 0x05), // USB3_DP_QSERDES_TXA_RES_CODE_LANE_OFFSET_RX
    (0x1284, 0x55), // USB3_DP_QSERDES_TXA_LANE_MODE_1
    (0x1288, 0x02), // USB3_DP_QSERDES_TXA_LANE_MODE_2
    (0x1290, 0x2a), // USB3_DP_QSERDES_TXA_LANE_MODE_4
    (0x1294, 0x3f), // USB3_DP_QSERDES_TXA_LANE_MODE_5
    (0x12a4, 0x12), // USB3_DP_QSERDES_TXA_RCV_DETECT_LVL_2
    (0x12e4, 0x20), // USB3_DP_QSERDES_TXA_PI_QEC_CTRL
    (0x1414, 0x05), // USB3_DP_QSERDES_RXA_UCDR_SO_GAIN
    (0x1430, 0x2f), // USB3_DP_QSERDES_RXA_UCDR_FASTLOCK_FO_GAIN
    (0x1434, 0x7f), // USB3_DP_QSERDES_RXA_UCDR_SO_SATURATION_AND_ENABLE
    (0x143c, 0xff), // USB3_DP_QSERDES_RXA_UCDR_FASTLOCK_COUNT_LOW
    (0x1440, 0x0f), // USB3_DP_QSERDES_RXA_UCDR_FASTLOCK_COUNT_HIGH
    (0x1444, 0x99), // USB3_DP_QSERDES_RXA_UCDR_PI_CONTROLS
    (0x144c, 0x04), // USB3_DP_QSERDES_RXA_UCDR_SB2_THRESH1
    (0x1450, 0x08), // USB3_DP_QSERDES_RXA_UCDR_SB2_THRESH2
    (0x1454, 0x05), // USB3_DP_QSERDES_RXA_UCDR_SB2_GAIN1
    (0x1458, 0x05), // USB3_DP_QSERDES_RXA_UCDR_SB2_GAIN2
    (0x14d4, 0x54), // USB3_DP_QSERDES_RXA_VGA_CAL_CNTRL1
    (0x14d8, 0x08), // USB3_DP_QSERDES_RXA_VGA_CAL_CNTRL2
    (0x14ec, 0x0f), // USB3_DP_QSERDES_RXA_RX_EQU_ADAPTOR_CNTRL2
    (0x14f0, 0x4a), // USB3_DP_QSERDES_RXA_RX_EQU_ADAPTOR_CNTRL3
    (0x14f4, 0x0a), // USB3_DP_QSERDES_RXA_RX_EQU_ADAPTOR_CNTRL4
    (0x14f8, 0xc0), // USB3_DP_QSERDES_RXA_RX_IDAC_TSETTLE_LOW
    (0x14fc, 0x00), // USB3_DP_QSERDES_RXA_RX_IDAC_TSETTLE_HIGH
    (0x1510, 0x77), // USB3_DP_QSERDES_RXA_RX_EQ_OFFSET_ADAPTOR_CNTRL1
    (0x151c, 0x04), // USB3_DP_QSERDES_RXA_SIGDET_CNTRL
    (0x1524, 0x0e), // USB3_DP_QSERDES_RXA_SIGDET_DEGLITCH_CNTRL
    (0x155c, 0xbf), // USB3_DP_QSERDES_RXA_RX_MODE_00_LOW
    (0x1560, 0xbf), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH
    (0x1564, 0x3f), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH2
    (0x1568, 0x7f), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH3
    (0x156c, 0x94), // USB3_DP_QSERDES_RXA_RX_MODE_00_HIGH4
    (0x1570, 0x5b), // USB3_DP_QSERDES_RXA_RX_MODE_01_LOW
    (0x1574, 0x1b), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH
    (0x1578, 0xd2), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH2
    (0x157c, 0x13), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH3
    (0x1580, 0xa9), // USB3_DP_QSERDES_RXA_RX_MODE_01_HIGH4
    (0x15a0, 0x04), // USB3_DP_QSERDES_RXA_DFE_EN_TIMER
    (0x15a4, 0x00), // USB3_DP_QSERDES_RXA_DFE_CTLE_POST_CAL_OFFSET
    (0x1460, 0xa0), // USB3_DP_QSERDES_RXA_AUX_DATA_TCOARSE_TFINE
    (0x15a8, 0x0c), // USB3_DP_QSERDES_RXA_DCC_CTRL1
    (0x14dc, 0x00), // USB3_DP_QSERDES_RXA_GM_CAL
    (0x15b0, 0x10), // USB3_DP_QSERDES_RXA_VTH_CODE
    (0x1634, 0x00), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_TX
    (0x1638, 0x00), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_RX
    (0x163c, 0x16), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_OFFSET_TX
    (0x1640, 0x05), // USB3_DP_QSERDES_TXB_RES_CODE_LANE_OFFSET_RX
    (0x1684, 0x55), // USB3_DP_QSERDES_TXB_LANE_MODE_1
    (0x1688, 0x02), // USB3_DP_QSERDES_TXB_LANE_MODE_2
    (0x1690, 0x2a), // USB3_DP_QSERDES_TXB_LANE_MODE_4
    (0x1694, 0x3f), // USB3_DP_QSERDES_TXB_LANE_MODE_5
    (0x16a4, 0x12), // USB3_DP_QSERDES_TXB_RCV_DETECT_LVL_2
    (0x16e4, 0x02), // USB3_DP_QSERDES_TXB_PI_QEC_CTRL
    (0x1814, 0x05), // USB3_DP_QSERDES_RXB_UCDR_SO_GAIN
    (0x1830, 0x2f), // USB3_DP_QSERDES_RXB_UCDR_FASTLOCK_FO_GAIN
    (0x1834, 0x7f), // USB3_DP_QSERDES_RXB_UCDR_SO_SATURATION_AND_ENABLE
    (0x183c, 0xff), // USB3_DP_QSERDES_RXB_UCDR_FASTLOCK_COUNT_LOW
    (0x1840, 0x0f), // USB3_DP_QSERDES_RXB_UCDR_FASTLOCK_COUNT_HIGH
    (0x1844, 0x99), // USB3_DP_QSERDES_RXB_UCDR_PI_CONTROLS
    (0x184c, 0x04), // USB3_DP_QSERDES_RXB_UCDR_SB2_THRESH1
    (0x1850, 0x08), // USB3_DP_QSERDES_RXB_UCDR_SB2_THRESH2
    (0x1854, 0x05), // USB3_DP_QSERDES_RXB_UCDR_SB2_GAIN1
    (0x1858, 0x05), // USB3_DP_QSERDES_RXB_UCDR_SB2_GAIN2
    (0x18d4, 0x54), // USB3_DP_QSERDES_RXB_VGA_CAL_CNTRL1
    (0x18d8, 0x08), // USB3_DP_QSERDES_RXB_VGA_CAL_CNTRL2
    (0x18ec, 0x0f), // USB3_DP_QSERDES_RXB_RX_EQU_ADAPTOR_CNTRL2
    (0x18f0, 0x4a), // USB3_DP_QSERDES_RXB_RX_EQU_ADAPTOR_CNTRL3
    (0x18f4, 0x0a), // USB3_DP_QSERDES_RXB_RX_EQU_ADAPTOR_CNTRL4
    (0x18f8, 0xc0), // USB3_DP_QSERDES_RXB_RX_IDAC_TSETTLE_LOW
    (0x18fc, 0x00), // USB3_DP_QSERDES_RXB_RX_IDAC_TSETTLE_HIGH
    (0x1910, 0x77), // USB3_DP_QSERDES_RXB_RX_EQ_OFFSET_ADAPTOR_CNTRL1
    (0x191c, 0x04), // USB3_DP_QSERDES_RXB_SIGDET_CNTRL
    (0x1924, 0x0e), // USB3_DP_QSERDES_RXB_SIGDET_DEGLITCH_CNTRL
    (0x195c, 0xbf), // USB3_DP_QSERDES_RXB_RX_MODE_00_LOW
    (0x1960, 0xbf), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH
    (0x1964, 0x3f), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH2
    (0x1968, 0x7f), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH3
    (0x196c, 0x94), // USB3_DP_QSERDES_RXB_RX_MODE_00_HIGH4
    (0x1970, 0x5b), // USB3_DP_QSERDES_RXB_RX_MODE_01_LOW
    (0x1974, 0x1b), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH
    (0x1978, 0xd2), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH2
    (0x197c, 0x13), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH3
    (0x1980, 0xa9), // USB3_DP_QSERDES_RXB_RX_MODE_01_HIGH4
    (0x19a0, 0x04), // USB3_DP_QSERDES_RXB_DFE_EN_TIMER
    (0x19a4, 0x00), // USB3_DP_QSERDES_RXB_DFE_CTLE_POST_CAL_OFFSET
    (0x1860, 0xa0), // USB3_DP_QSERDES_RXB_AUX_DATA_TCOARSE_TFINE
    (0x19a8, 0x0c), // USB3_DP_QSERDES_RXB_DCC_CTRL1
    (0x18dc, 0x00), // USB3_DP_QSERDES_RXB_GM_CAL
    (0x19b0, 0x10), // USB3_DP_QSERDES_RXB_VTH_CODE
    (0x1cc4, 0xd0), // USB3_DP_PCS_LOCK_DETECT_CONFIG1
    (0x1cc8, 0x07), // USB3_DP_PCS_LOCK_DETECT_CONFIG2
    (0x1ccc, 0x20), // USB3_DP_PCS_LOCK_DETECT_CONFIG3
    (0x1cd8, 0x13), // USB3_DP_PCS_LOCK_DETECT_CONFIG6
    (0x1cdc, 0x21), // USB3_DP_PCS_REFGEN_REQ_CONFIG1
    (0x1d88, 0xaa), // USB3_DP_PCS_RX_SIGDET_LVL
    (0x1db0, 0x0f), // USB3_DP_PCS_CDR_RESET_TIME
    (0x1dc0, 0x88), // USB3_DP_PCS_ALIGN_DETECT_CONFIG1
    (0x1dc4, 0x13), // USB3_DP_PCS_ALIGN_DETECT_CONFIG2
    (0x1dd0, 0x0c), // USB3_DP_PCS_PCS_TX_RX_CONFIG
    (0x1ddc, 0x4b), // USB3_DP_PCS_EQ_CONFIG1
    (0x1dec, 0x10), // USB3_DP_PCS_EQ_CONFIG5
    (0x1f18, 0xf8), // USB3_DP_PCS_USB3_LFPS_DET_HIGH_COUNT_VAL
    (0x1f38, 0x07), // USB3_DP_PCS_USB3_RXEQTRAINING_DFE_TIME_S2
];

/// Active PHY tables. The compiled values are the Bramble fallback, while
/// the DT path may replace them after validating the complete vendor
/// property. Keeping the delay array separate preserves the compact static
/// table and still executes the DT's third cell rather than silently dropping
/// it.
static mut ACTIVE_QMP_INIT: [(usize, u32); 146] = QMP_INIT;
static mut ACTIVE_QMP_INIT_DELAY_US: [u32; 146] = [0; 146];
static mut ACTIVE_HSPHY_PARAM_OVERRIDE: [(usize, u32); 3] =
    [(0x6c, 0x63), (0x70, 0x85), (0x74, 0x17)];

/// Install the complete PHY programming properties from the bootloader DTB.
/// A partial or malformed property is rejected as a unit, leaving the known
/// Bramble fallback in place. The QMP binding terminates its 146 triples with
/// `<0xffffffff 0xffffffff 0>`, which is a sentinel and is not written.
pub fn install_dt_phy_sequences(hs_raw: [Option<u32>; 6], qmp_raw: [Option<u32>; 441]) -> bool {
    let mut installed = false;

    if hs_raw.iter().all(Option::is_some) {
        let mut entries = [(0usize, 0u32); 3];
        let mut valid = true;
        for index in 0..3 {
            let value = hs_raw[index * 2].unwrap();
            let offset = hs_raw[index * 2 + 1].unwrap();
            valid &= value <= 0xff
                && matches!(offset, 0x6c | 0x70 | 0x74)
                && entries[..index]
                    .iter()
                    .all(|entry| entry.0 != offset as usize);
            entries[index] = (offset as usize, value);
        }
        if valid {
            unsafe { ACTIVE_HSPHY_PARAM_OVERRIDE = entries };
            installed = true;
        }
    }

    if qmp_raw.iter().all(Option::is_some)
        && qmp_raw[438] == Some(u32::MAX)
        && qmp_raw[439] == Some(u32::MAX)
        && qmp_raw[440] == Some(0)
    {
        let mut entries = [(0usize, 0u32); 146];
        let mut delays = [0u32; 146];
        let mut valid = true;
        for index in 0..146 {
            let raw = index * 3;
            let offset = qmp_raw[raw].unwrap();
            let value = qmp_raw[raw + 1].unwrap();
            let delay_us = qmp_raw[raw + 2].unwrap();
            valid &= offset <= 0x2fff && value <= 0xff && delay_us <= 1_000_000;
            entries[index] = (offset as usize, value);
            delays[index] = delay_us;
        }
        if valid {
            unsafe {
                ACTIVE_QMP_INIT = entries;
                ACTIVE_QMP_INIT_DELAY_US = delays;
            }
            installed = true;
        }
    }

    installed
}

const EVENT_BUFFER_SIZE: usize = 4096;
const MAX_PACKET_SIZE: u32 = 512;
// Linux starts the gadget with the SuperSpeed EP0 descriptor size while the
// link speed is still unknown, then changes it to 64 on a High-Speed
// Connect Done event. The first SETUP transfer must use that initial state.
const INITIAL_EP0_MAX_PACKET_SIZE: u32 = 512;

// The firmware-owned Fastboot event page is used only by the explicit
// --reuse-fastboot-dma differential. Keep every EP0 object inside that page
// so this test does not assume a second firmware allocation is accessible
// through the still-active SMMU context.
const FASTBOOT_EP0_EVENT_SIZE: usize = 0x100;
const FASTBOOT_EP0_SETUP_OFFSET: usize = 0x100;
const FASTBOOT_EP0_TRB_OFFSET: usize = 0x140;
const FASTBOOT_EP0_RESPONSE_OFFSET: usize = 0x180;
const TRACE_FASTBOOT_EVENT_DMA: u32 = 39;

#[repr(C, align(4096))]
struct EventBuffer([u8; EVENT_BUFFER_SIZE]);

#[repr(C, align(64))]
struct ResponseBuffer([u8; 512]);

#[unsafe(link_section = ".usb_dma")]
static mut EVENTS: EventBuffer = EventBuffer([0; EVENT_BUFFER_SIZE]);
// Linux copies the producer-owned event ring into a CPU-owned cache before
// acknowledging GEVNTCOUNT.  Keep the same ownership boundary in the
// polling path; otherwise process_event() can issue a new endpoint command
// while it is still reading a ring slot that DWC3 may reuse after an ACK.
#[repr(C, align(4096))]
struct EventCache([u8; EVENT_BUFFER_SIZE]);

static mut EVENT_CACHE: EventCache = EventCache([0; EVENT_BUFFER_SIZE]);

#[unsafe(link_section = ".usb_dma")]
static mut GSI_EVENTS: [EventBuffer; 3] = [
    EventBuffer([0; EVENT_BUFFER_SIZE]),
    EventBuffer([0; EVENT_BUFFER_SIZE]),
    EventBuffer([0; EVENT_BUFFER_SIZE]),
];
#[repr(C, align(64))]
struct SetupPacket([u8; 8]);

#[unsafe(link_section = ".usb_dma")]
static mut SETUP_PACKET: SetupPacket = SetupPacket([0; 8]);
#[unsafe(link_section = ".usb_dma")]
static mut EP0_TRBS: [Trb; 2] = [
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
];
#[unsafe(link_section = ".usb_dma")]
static mut DATA_TRBS: [Trb; 2] = [
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
    Trb {
        bpl: 0,
        bph: 0,
        size: 0,
        ctrl: 0,
    },
];
#[repr(C, align(64))]
struct DataBuffer([u8; MAX_PACKET_SIZE as usize]);

#[unsafe(link_section = ".usb_dma")]
static mut DATA_OUT_BUFFER: DataBuffer = DataBuffer([0; MAX_PACKET_SIZE as usize]);
#[unsafe(link_section = ".usb_dma")]
static mut RESPONSE: ResponseBuffer = ResponseBuffer([0; 512]);
static mut FASTBOOT_EVENT_DMA_BASE: u64 = 0;
static mut EVENT_OFFSET: usize = 0;
static mut GSI_EVENT_OFFSETS: [usize; 3] = [0; 3];
/// One retained request slot per Qualcomm event buffer. The Android GSI
/// wrapper is not a normal DWC3 ring: reusing a slot before its event arrives
/// would overwrite the TRB address that the wrapper is still consuming.
static mut GSI_PENDING: [bool; 3] = [false; 3];
static mut GSI_CHANNEL_ENDPOINT: [usize; 3] = [0; 3];
static mut GSI_CHANNEL_READY: [bool; 3] = [false; 3];
static mut GSI_REQUEST_SLOTS: [usize; 3] = [usize::MAX; 3];
static mut GSI_RING_BASES: [u64; 3] = [0; 3];
static mut GSI_RING_TRB_COUNTS: [usize; 3] = [0; 3];
static mut GSI_BUFFER_BASES: [u64; 3] = [0; 3];
static mut GSI_BUFFER_LENGTHS: [usize; 3] = [0; 3];
static mut GSI_DOORBELL_BASES: [u64; 3] = [0; 3];
static mut GSI_RESOURCE_INDEX: [u8; 3] = [0; 3];
static mut GSI_RING_ACTIVE: [bool; 3] = [false; 3];
static mut DMA_ALLOCATOR: Option<super::platform::bramble::DmaPoolAllocator> = None;

/// Latched signal-probe observables. The early Bramble handoff has no UART
/// and cannot enumerate, so these states are published to the host by
/// dropping the physical pull-up at a diagnostic delay (see
/// `ep0_signal_code()`); the host dmesg timestamps become the readout.
static mut SIGNAL_EVENT_DELIVERED: bool = false;
static mut SIGNAL_SETUP_TRB_RETIRED: bool = false;
static mut SIGNAL_SETUP_PACKET_RECEIVED: bool = false;
static mut SIGNAL_LAST_SOFFN: u16 = 0;
static mut SIGNAL_SOF_SEEN: bool = false;
/// Link-state ladder latches (see `ep0_link_signal_code()`).
static mut SIGNAL_LNKST_U0: bool = false;
static mut SIGNAL_LNKST_RESET: bool = false;
static mut SIGNAL_LNKST_POLLING: bool = false;
static mut SIGNAL_LNKST_RXDET: bool = false;
static mut SIGNAL_CORE_HALTED: bool = false;
/// True while the core owns an armed EP0 SETUP transfer. The core REJECTS
/// Start Transfer while the device link is not ON (including during the
/// host's bus reset), so the first arm attempt after Run/Stop completes with
/// "No resource" and must be retried once the link comes up; the poll-loop
/// guard uses this latch to re-arm exactly then, which also delivers any
/// SETUP packet the core latched while no TRB was armed.
static mut EP0_SETUP_ARMED: bool = false;
/// Set by the USB Reset / Connect Done handlers: the host is present and the
/// link is coming up, so the guard should arm the SETUP TRB (retrying with a
/// small cooldown until the link reaches ON). Arming is deliberately NOT
/// attempted before the first USB Reset: the core rejects Start Transfer
/// while disconnected, and millions of failed commands during the pre-attach
/// window can wedge the endpoint command engine.
static mut PENDING_SETUP_ARM: bool = false;
/// Poll retries to skip after a failed SETUP arm. The core fast-fails Start
/// Transfer with "No resource" while the link is not ON; hammering the
/// command engine at poll rate during that window can wedge it, so the
/// guard backs off between attempts.
static mut ARM_COOLDOWN: u32 = 0;
/// CNTPCT tick of the first successful post-connect Run/Stop (quiet-window
/// reference; 0 = no start recorded yet).
static mut RUN_STOP_TICK: u64 = 0;
/// Connect-delay one-shot latch (see the delay block in
/// `init_with_super_speed`). Only the first handoff attempt pays the delay
/// so the retry loop stays inside the EL1 recovery-timer budget.
static mut SIGNAL_CONNECT_DELAYED: bool = false;
/// Adopted SMMU mapping (see `adopt_smmu_dma_mapping()`). When the Apps-SMMU
/// stream is owned by a live TRANSLATE context that software cannot rewrite,
/// the EP0 DMA objects are relocated into a page that context already maps:
/// the CPU addresses the page at `DMA_ADOPTED_CPU` while DWC3 is published
/// the corresponding IOVA in `DMA_ADOPTED_IOVA`.
static mut DMA_ADOPTED: bool = false;
static mut DMA_ADOPTED_CPU: usize = 0;
static mut DMA_ADOPTED_IOVA: u64 = 0;

#[inline]
fn dma_mapping_adopted() -> bool {
    unsafe { DMA_ADOPTED }
}

/// Translate a CPU-side pointer inside the adopted page into the IOVA that
/// DWC3 must use. Outside adopted mode the CPU address IS the DMA address.
#[inline]
unsafe fn dma_iova_for(cpu: usize) -> u64 {
    unsafe {
        if DMA_ADOPTED {
            DMA_ADOPTED_IOVA + (cpu - DMA_ADOPTED_CPU) as u64
        } else {
            cpu as u64
        }
    }
}

/// Read-only Apps-SMMU stage-1 walk. Returns the physical output address for
/// `iova` when the stream's context bank translation maps it with a valid
/// 4 KiB page (or 2 MiB block whose base contains the whole 4 KiB window).
unsafe fn smmu_walk_iova(ttbr0: u64, three_level: bool, iova: u64) -> Option<u64> {
    const DESC_VALID: u64 = 1;
    const DESC_TABLE: u64 = 2;
    unsafe {
        let mut table = (ttbr0 & !0xfff) as usize;
        // 39-bit IOVA (T0SZ=25, 4 KiB granule): L1 root. 32-bit IOVA
        // (T0SZ=32): the walk starts at level 2 with 2 MiB blocks.
        let (l1_index, l2_index, l3_index) = if three_level {
            (
                ((iova >> 30) & 0x1ff) as usize,
                ((iova >> 21) & 0x1ff) as usize,
                ((iova >> 12) & 0x1ff) as usize,
            )
        } else {
            (
                0,
                ((iova >> 21) & 0x1ff) as usize,
                ((iova >> 12) & 0x1ff) as usize,
            )
        };
        if three_level {
            let descriptor = read_volatile((table + l1_index * 8) as *const u64);
            if descriptor & DESC_VALID == 0 || descriptor & DESC_TABLE == 0 {
                return None;
            }
            table = (descriptor & !0xfff) as usize;
        }
        let l2_descriptor = read_volatile((table + l2_index * 8) as *const u64);
        if l2_descriptor & DESC_VALID == 0 {
            return None;
        }
        if l2_descriptor & DESC_TABLE == 0 {
            // 2 MiB block: the whole 4 KiB window must fit inside it. The
            // block base is 2 MiB aligned, so the low attribute bits do not
            // overlap the output address.
            let block = (l2_descriptor & 0x000f_ffff_ffff_f000) as usize;
            let offset = (iova as usize) & 0x1f_ffff;
            if offset + 0x1000 > 0x20_0000 {
                return None;
            }
            return Some((block | offset) as u64);
        }
        let l3_table = (l2_descriptor & !0xfff) as usize;
        let l3_descriptor = read_volatile((l3_table + l3_index * 8) as *const u64);
        if l3_descriptor & DESC_VALID == 0 || l3_descriptor & DESC_TABLE != 0 {
            return None;
        }
        Some(l3_descriptor & 0x000f_ffff_ffff_f000)
    }
}

/// Read-only Apps-SMMU stream/context discovery. Returns the context-bank
/// page index when the DWC3 stream matches a valid SMR whose S2CR selects an
/// active TRANSLATE context. Never writes the SMMU.
unsafe fn smmu_find_translate_context() -> Option<(usize, usize, usize)> {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return None;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        let num_pages =
            1usize << (((id1 >> SMMU_ID1_NUMPAGENDXB_SHIFT) & SMMU_ID1_NUMPAGENDXB_MASK) + 1);
        let num_context_banks = (id1 & SMMU_ID1_NUMCB_MASK) as usize;
        if num_pages == 0 || num_context_banks == 0 {
            return None;
        }
        let stream_id = super::platform::bramble::usb_resources().dma_pool.stream_id;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask != 0 {
                continue;
            }
            let s2cr = read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4));
            if s2cr & SMMU_S2CR_TYPE_MASK != SMMU_S2CR_TYPE_TRANS {
                // BYPASS or FAULT: the CPU address is already the DMA
                // address and the normal linker section works.
                return None;
            }
            let cbndx = (s2cr & SMMU_S2CR_CBNDX_MASK) as usize;
            if cbndx >= num_context_banks {
                return None;
            }
            return Some((cbndx, num_pages, num_context_banks));
        }
        None
    }
}

/// Find ANY valid 4 KiB page mapping inside the live context's stage-1 tables
/// and return (iova, physical). The bootloader context is no longer active
/// once `fastboot boot` jumped away, so any page it mapped is a fair DMA
/// window for Fullerene's EP0 objects.
unsafe fn smmu_find_any_mapping(ttbr0: u64, three_level: bool) -> Option<(u64, u64)> {
    const DESC_VALID: u64 = 1;
    const DESC_TABLE: u64 = 2;
    unsafe {
        let l1_table = (ttbr0 & !0xfff) as usize;
        let l1_range = if three_level { 512usize } else { 1 };
        for l1_index in 0..l1_range {
            let mut l2_table = l1_table;
            if three_level {
                let l1_descriptor = read_volatile((l1_table + l1_index * 8) as *const u64);
                if l1_descriptor & DESC_VALID == 0 || l1_descriptor & DESC_TABLE == 0 {
                    continue;
                }
                l2_table = (l1_descriptor & !0xfff) as usize;
            }
            for l2_index in 0..512usize {
                let l2_descriptor = read_volatile((l2_table + l2_index * 8) as *const u64);
                if l2_descriptor & DESC_VALID == 0 {
                    continue;
                }
                let iova_base = ((l1_index as u64) << 30) | ((l2_index as u64) << 21);
                if l2_descriptor & DESC_TABLE == 0 {
                    // A 2 MiB block: use its first 4 KiB window.
                    let block = l2_descriptor & 0x000f_ffff_ffff_f000;
                    return Some((iova_base, block));
                }
                let l3_table = (l2_descriptor & !0xfff) as usize;
                for l3_index in 0..512usize {
                    let l3_descriptor = read_volatile((l3_table + l3_index * 8) as *const u64);
                    if l3_descriptor & DESC_VALID == 0 || l3_descriptor & DESC_TABLE != 0 {
                        continue;
                    }
                    let physical = l3_descriptor & 0x000f_ffff_ffff_f000;
                    if physical == 0 {
                        continue;
                    }
                    return Some((iova_base | ((l3_index as u64) << 12), physical));
                }
            }
        }
        None
    }
}

/// Newest STARTTRANSFER outcome harvested from the retained trace of the
/// previous attempts (0xFFFF_FFFF = none; bit 16 set = the command timed out;
/// otherwise the raw DEPCMD register: status in bits 15:12).
pub fn harvest_last_str_code() -> u32 {
    unsafe { TRACE_HARVEST_LAST }
}

/// Read-only Apps-SMMU stream-state ladder. Returns the deepest condition
/// that provably holds, so a host-visible attach gate can name the state one
/// run at a time:
///   0..=3 = an SMR matched the stream and its S2CR type is that value
///   251   = SMRs are implemented but none is valid
///   252   = at least one valid SMR exists but none matches the stream
///   253   = no SMRs are implemented (ID0.NUMSMRG == 0)
///   254   = the SMMU identification registers are unreadable (RAZ/all-ones)
unsafe fn smmu_stream_s2cr_type() -> u32 {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return 254;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        if num_smrs == 0 {
            return 253;
        }
        let stream_id = super::platform::bramble::usb_resources().dma_pool.stream_id;
        let mut any_valid = false;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            any_valid = true;
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask != 0 {
                continue;
            }
            let s2cr = read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4));
            return (s2cr & SMMU_S2CR_TYPE_MASK) >> 16;
        }
        if any_valid {
            // Stream unmatched. Distinguish an active SMMU (252) from a
            // globally bypassed one (250): with SMMUEN=0 or CLIENTPD=1 the
            // unmatched SMR is irrelevant and transactions already pass
            // untranslated.
            let scr0 = read_volatile(smmu_reg(SMMU_GR0_SCR0));
            let active = scr0 != u32::MAX && (scr0 & 1) != 0 && (scr0 & 2) == 0;
            return if active { 252 } else { 250 };
        }
        251
    }
}

/// Claim a free Apps-SMMU SMR slot for the DWC3 stream and point its S2CR at
/// BYPASS so the stream's transactions pass untranslated (CPU address ==
/// DMA address). Only slots whose VALID bit is clear are claimed, both
/// writes are verified by readback, and a rejected (secure-owned) write is
/// reported as failure instead of being assumed. The stream is known to be
/// unmatched (ladder 252) when this runs, so no live mapping is displaced.
unsafe fn smmu_install_stream_bypass() -> bool {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return false;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        if num_smrs == 0 {
            return false;
        }
        let stream_id = super::platform::bramble::usb_resources().dma_pool.stream_id & 0xffff;
        // An existing match means the stream already has an owner: leave the
        // configuration alone and report success (nothing to install).
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask == 0 {
                trace_event(TRACE_SMMU_HANDOFF, 0x494E, index as u32, 0, 1, 0);
                return true;
            }
        }
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID != 0 {
                continue;
            }
            // Claim this slot. Write the inert S2CR first (it only applies
            // once SMR.VALID is set), then publish the SMR, then verify both
            // by readback before declaring the stream owned. The catch-all
            // mode matches every stream ID so a misreported DWC3 stream ID
            // cannot keep the stream faulting.
            let catch_all = option_env!("FULLERENE_USB_SMMU_INSTALL_ALL") == Some("1");
            let smr_value = if catch_all {
                SMMU_SMR_VALID | (0x7fff << SMMU_SMR_MASK_SHIFT)
            } else {
                SMMU_SMR_VALID | stream_id
            };
            let s2cr_address = SMMU_S2CR_BASE + index * 4;
            let old_s2cr = read_volatile(smmu_reg(s2cr_address));
            let new_s2cr = (old_s2cr & !SMMU_S2CR_TYPE_MASK) | SMMU_S2CR_TYPE_BYPASS;
            write_volatile(smmu_reg(s2cr_address), new_s2cr);
            core::arch::asm!("dsb sy", options(nostack));
            write_volatile(smmu_reg(SMMU_SMR_BASE + index * 4), smr_value);
            core::arch::asm!("dsb sy", options(nostack));
            let smr_readback = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            let s2cr_readback = read_volatile(smmu_reg(s2cr_address));
            let smr_ok = smr_readback & SMMU_SMR_VALID != 0 && smr_readback == smr_value;
            let s2cr_ok = s2cr_readback & SMMU_S2CR_TYPE_MASK == SMMU_S2CR_TYPE_BYPASS;
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x534D_5200 | index as u32,
                smr_readback,
                s2cr_readback,
                smr_ok as u32,
                s2cr_ok as u32,
            );
            if smr_ok && s2cr_ok {
                smmu_tlb_sync();
                log_hex("usb: installed SMMU bypass SMR index=", index as u64);
                return true;
            }
            // The secure side rejected at least one write. Restore the slot
            // to its inert state as far as non-secure writes reach and
            // report the failure.
            write_volatile(smmu_reg(SMMU_SMR_BASE + index * 4), smr);
            write_volatile(smmu_reg(s2cr_address), old_s2cr);
            core::arch::asm!("dsb sy", options(nostack));
            return false;
        }
        false
    }
}
/// Relocate the EP0 DMA objects into a page that the live Apps-SMMU context
/// already maps. The stream's S2CR is TRANSLATE and software cannot rewrite
/// it from non-secure state, so the only working DMA window is one the
/// bootloader context maps: capture the bootloader's event-ring IOVA, walk
/// its page tables read-only, and adopt that page (CPU side = physical,
/// DWC3 side = IOVA). Returns the IOVA of the adopted page.
unsafe fn adopt_smmu_dma_mapping() -> Option<u64> {
    unsafe {
        let (cbndx, num_pages, _) = smmu_find_translate_context()?;
        let page_size = 0x1000usize;
        let cb_page = num_pages + cbndx;
        let sctlr = smmu_page_read(page_size, cb_page, SMMU_CB_SCTLR);
        if sctlr & SMMU_SCTLR_M == 0 {
            // Translation disabled: the CPU address is the DMA address.
            return None;
        }
        let ttbr0 = smmu_page_read64(page_size, cb_page, SMMU_CB_TTBR0);
        if ttbr0 == 0 || ttbr0 == u64::MAX {
            return None;
        }
        let three_level = super::platform::bramble::usb_resources().smmu_use_3_level_tables;
        let iova =
            (read_volatile(reg(GEVNTADRHI0)) as u64) << 32 | read_volatile(reg(GEVNTADRLO0)) as u64;
        if iova == 0
            || iova == u64::MAX
            || iova & 0xfff != 0
            || iova > usize::MAX as u64
            || iova >= 1 << (if three_level { 39 } else { 32 })
        {
            return None;
        }
        let (iova, physical) = match smmu_walk_iova(ttbr0, three_level, iova) {
            Some(physical) if physical != 0 => (iova, physical as u64),
            // The bootloader's own event-ring IOVA is no longer resolvable
            // (its teardown may have cleared GEVNTADR or the page). Adopt
            // ANY page the live context still maps instead: the bootloader
            // is gone and no other master owns the stream.
            _ => smmu_find_any_mapping(ttbr0, three_level)?,
        };
        if physical == 0 || iova & 0xfff != 0 {
            return None;
        }
        DMA_ADOPTED_CPU = physical as usize;
        DMA_ADOPTED_IOVA = iova;
        DMA_ADOPTED = true;
        log_hex("usb: adopted SMMU page physical=", physical);
        log_hex("usb: adopted SMMU page iova=", iova);
        Some(iova)
    }
}

#[inline]
unsafe fn ep0_event_dma_base() -> usize {
    unsafe {
        if DMA_ADOPTED {
            return DMA_ADOPTED_CPU;
        }
        let captured = FASTBOOT_EVENT_DMA_BASE;
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma) && captured != 0 {
            captured as usize
        } else {
            addr_of_mut!(EVENTS) as usize
        }
    }
}

#[inline]
unsafe fn ep0_event_address() -> u64 {
    unsafe {
        if DMA_ADOPTED {
            return DMA_ADOPTED_IOVA;
        }
        ep0_event_dma_base() as u64
    }
}

#[inline]
unsafe fn ep0_event_size() -> usize {
    unsafe {
        if DMA_ADOPTED {
            return FASTBOOT_EP0_EVENT_SIZE;
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
            && FASTBOOT_EVENT_DMA_BASE != 0
        {
            FASTBOOT_EP0_EVENT_SIZE
        } else {
            EVENT_BUFFER_SIZE
        }
    }
}

#[inline]
unsafe fn ep0_setup_ptr() -> *mut u8 {
    unsafe {
        if DMA_ADOPTED {
            return (DMA_ADOPTED_CPU as *mut u8).add(FASTBOOT_EP0_SETUP_OFFSET);
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
            && FASTBOOT_EVENT_DMA_BASE != 0
        {
            (ep0_event_dma_base() as *mut u8).add(FASTBOOT_EP0_SETUP_OFFSET)
        } else {
            addr_of_mut!(SETUP_PACKET).cast::<u8>()
        }
    }
}

#[inline]
unsafe fn ep0_trb_ptr(index: usize) -> *mut Trb {
    unsafe {
        if DMA_ADOPTED {
            return (DMA_ADOPTED_CPU as *mut u8)
                .add(FASTBOOT_EP0_TRB_OFFSET + index * core::mem::size_of::<Trb>())
                .cast::<Trb>();
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
            && FASTBOOT_EVENT_DMA_BASE != 0
        {
            (ep0_event_dma_base() as *mut u8)
                .add(FASTBOOT_EP0_TRB_OFFSET + index * core::mem::size_of::<Trb>())
                .cast::<Trb>()
        } else {
            addr_of_mut!(EP0_TRBS).cast::<Trb>().add(index)
        }
    }
}

#[inline]
unsafe fn ep0_response_ptr() -> *mut u8 {
    unsafe {
        if DMA_ADOPTED {
            return (DMA_ADOPTED_CPU as *mut u8).add(FASTBOOT_EP0_RESPONSE_OFFSET);
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma)
            && FASTBOOT_EVENT_DMA_BASE != 0
        {
            (ep0_event_dma_base() as *mut u8).add(FASTBOOT_EP0_RESPONSE_OFFSET)
        } else {
            addr_of_mut!(RESPONSE.0).cast::<u8>()
        }
    }
}

static mut EP0_STATE: Ep0State = Ep0State::Setup;
static mut CONTROL_IN: bool = false;
static mut CONTROL_HAS_DATA: bool = false;
static mut CONFIGURED: bool = false;
// The standalone handoff probe has a recovery deadline for the no-host case,
// but an idle, successfully-serviced EP0 is a valid steady state. Keep this
// separate from CONFIGURED: a descriptor-only host may never issue
// SET_CONFIGURATION while EP0 is nevertheless healthy.
static mut PROBE_EP0_PROGRESS: bool = false;
static mut ENDPOINTS_READY: bool = false;
static mut DATA_ENDPOINTS_READY: bool = false;
static mut DATA_REQUEST_SLOTS: [usize; 2] = [usize::MAX; 2];
/// DWC3 returns a resource index for every STARTTRANSFER, including normal
/// bulk endpoints. Keep it per endpoint so ENDTRANSFER remains valid after
/// a second queue/rearm cycle instead of relying on the first index.
static mut DATA_RESOURCE_INDEX: [u8; 2] = [0; 2];
/// True when the currently bound gadget function owns a GSI channel instead
/// of the ordinary DWC3 bulk pair. Keep this separate from
/// `DATA_ENDPOINTS_READY`: both paths share the gadget bind lifetime, but
/// their completion and teardown rules differ.
static mut GSI_GADGET_BOUND: bool = false;
static mut FUNCTION_BOUND: bool = false;
/// DWC3 returns a transfer-resource index from STARTTRANSFER.  Linux retains
/// it per endpoint and supplies it to ENDTRANSFER; using a fixed value works
/// only accidentally on the first controller generation.
static mut EP0_RESOURCE_INDEX: [u8; 2] = [0; 2];
/// Failure stage for the standalone gadget handoff probe. The probe uses
/// this to make a retained failure host-observable without publishing a
/// broken USB pull-up.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
static mut GADGET_HANDOFF_FAILURE_STAGE: u32 = 0;
// Direct-path (init_with_super_speed) EP command diagnostic snapshot. The
// direct path uses plain `return false` (no GADGET_HANDOFF_FAILURE_STAGE), so
// capture how far it gets, the raw DEPCTL after each command (CMDACT bit 10
// still set == the core never retired the command), and the core-state DSTS
// at the first endpoint command. 0xFFFF_FFFF = not reached.
static mut INIT_STAGE: u32 = 0;
static mut INIT_DEPSTART_RAW: u32 = 0xFFFF_FFFF;
static mut INIT_DEPSTART_DSTS: u32 = 0xFFFF_FFFF;
static mut INIT_EPCFG0_OK: bool = false;
static mut INIT_EPCFG0_RAW: u32 = 0xFFFF_FFFF;
static mut INIT_EPCFG0_DSTS: u32 = 0xFFFF_FFFF;
static mut INIT_EPCFG1_OK: bool = false;
static mut TYPEC_LANE_B: bool = false;
/// True only after the combo QMP PHY has completed its cold initialization.
/// USB2 handoff deliberately keeps this false: the USB2 path must not touch
/// SuperSpeed-only autonomous-mode registers owned by the bootloader.
static mut QMP_PHY_READY: bool = false;
static mut TYPEC_STATE_VALID: bool = false;
static mut TYPEC_STATE: super::platform::bramble::TypecState =
    super::platform::bramble::TypecState {
        arbiter_version: 0,
        apid: 0,
        writable: false,
        misc_status: 0,
        mode: 0,
        orientation_reverse: false,
        role: super::platform::bramble::UsbRole::None,
        sink_mode_written: false,
        attached: false,
        attach_settled: false,
        phase: super::platform::bramble::TypecPhase::Disabled,
    };
static mut TYPEC_POLL_TICKS: u32 = 0;
/// A Type-C parent SPI is a hard-IRQ notification; the SPMI child/arbiter
/// transaction belongs to the deferred role-switch context. Keep this bit
/// separate so a slow PMIC access cannot run inside DWC3 IRQ handling.
static mut TYPEC_IRQ_PENDING: bool = false;
/// A Qualcomm power-event IRQ is handled synchronously by the early exception
/// path, while Linux runs the corresponding handler in a threaded IRQ/work
/// context.  Defer the potentially long clock/PHY/controller resume until
/// poll() so an IRQ cannot execute a full runtime transition in exception
/// context.
static mut RESUME_PENDING: bool = false;
static mut USB_IN_P3: bool = false;
static mut USB_RUNTIME_STATE: super::platform::bramble::UsbRuntimeState =
    super::platform::bramble::UsbRuntimeState::Off;
/// The gadget driver is deliberately independent of DWC3 registers.  The
/// hardware UDC feeds it setup/complete callbacks, while the QEMU simulator
/// uses the same request/state implementation directly.
static mut GADGET: Ep0Simulator = Ep0Simulator::new();
static mut UDC: UsbUdc = UsbUdc::new();

#[inline]
unsafe fn gadget_mut() -> &'static mut Ep0Simulator {
    // Use a raw pointer for the retained early-boot singleton.  Rust 2024
    // rejects direct references to `static mut`; interrupt/polling access is
    // serialized by the single-core bring-up path.
    unsafe { &mut *addr_of_mut!(GADGET) }
}

#[inline]
unsafe fn gadget_ref() -> &'static Ep0Simulator {
    unsafe { &*addr_of!(GADGET) }
}

#[inline]
unsafe fn udc_mut() -> &'static mut UsbUdc {
    unsafe { &mut *addr_of_mut!(UDC) }
}

/// End the gadget-function lifetime exactly once before requests, endpoint
/// commands, or DMA channels are torn down.
unsafe fn unbind_function() {
    unsafe {
        if FUNCTION_BOUND {
            GadgetDriver::on_function_unbind(gadget_mut());
            FUNCTION_BOUND = false;
        }
    }
}

const USB_TRACE_CAPACITY: usize = 256;

// Numeric events keep the early USB path independent of UART, locks, and
// formatting. The buffer is CPU-owned; it is placed beside the DMA objects so
// a probe can preserve the same identity-mapped address discipline.
const TRACE_INIT: u32 = 1;
const TRACE_DEVICE_RESET: u32 = 2;
const TRACE_DEVICE_CONNECT: u32 = 3;
const TRACE_EP_COMMAND_ISSUE: u32 = 4;
const TRACE_EP_COMMAND_DONE: u32 = 5;
const TRACE_EP_COMMAND_TIMEOUT: u32 = 6;
const TRACE_SETUP_QUEUED: u32 = 7;
const TRACE_SETUP_RECEIVED: u32 = 8;
const TRACE_DESCRIPTOR_QUEUED: u32 = 9;
const TRACE_STATUS_QUEUED: u32 = 10;
const TRACE_TRANSFER_COMPLETE: u32 = 11;
const TRACE_USB_RESET: u32 = 12;
pub const TRACE_BOOT_USB_ENTRY: u32 = 13;
pub const TRACE_TYPEC_BEGIN: u32 = 14;
pub const TRACE_TYPEC_DONE: u32 = 15;
pub const TRACE_USB_HANDOFF_BEGIN: u32 = 16;
const TRACE_DWC3_RESET_BEGIN: u32 = 17;
const TRACE_QSCRATCH_BEGIN: u32 = 18;
pub const TRACE_EXCEPTION_SYNC: u32 = 19;
pub const TRACE_PROBE_WATCHDOG: u32 = 33;
const TRACE_LINK_STATUS: u32 = 20;
const TRACE_USB_WAKEUP: u32 = 21;
const TRACE_USB_SUSPEND: u32 = 22;
const TRACE_USB_DEVICE_ERROR: u32 = 23;
pub const TRACE_TYPEC_EVENT: u32 = 24;
pub const TRACE_PLATFORM_IRQ: u32 = 25;
pub const TRACE_UDC_REARM: u32 = 26;
const TRACE_SMMU_BEGIN: u32 = 27;
const TRACE_SMMU_READY: u32 = 28;
const TRACE_SMMU_HANDOFF: u32 = 34;
const TRACE_SMMU_PRESERVED: u32 = 35;
const TRACE_SMMU_FAULT: u32 = 36;
const TRACE_SMMU_GLOBAL_FAULT: u32 = 37;
const TRACE_UTMI_CLOCK: u32 = 29;
const TRACE_EVENT_RING_READY: u32 = 30;
const TRACE_DWC3_HALTED: u32 = 31;
const TRACE_DWC3_HALT_TIMEOUT: u32 = 32;
const TRACE_DWC3_REVISION_QUIRK: u32 = 38;
const TRACE_XFER_NOT_READY: u32 = 40;

#[repr(C)]
#[derive(Clone, Copy)]
struct UsbTraceEntry {
    sequence: u32,
    event: u32,
    request: u32,
    value: u32,
    index: u32,
    length: u32,
    ep0_state: u32,
    status: u32,
}

const EMPTY_USB_TRACE: UsbTraceEntry = UsbTraceEntry {
    sequence: 0,
    event: 0,
    request: 0,
    value: 0,
    index: 0,
    length: 0,
    ep0_state: 0,
    status: 0,
};

const USB_TRACE_MAGIC: u32 = 0x4655_5452; // "FUTR"
const USB_TRACE_VERSION: u32 = 1;

/// Outcome of the previous attempt's last STARTTRANSFER command, harvested
/// from the retained trace at the start of the next handoff attempt (see
/// `harvest_trace_outcome()`). Encoding: 0xFFFF = no record found,
/// 0x1_0000 | raw DEPCMD register = the command timed out, otherwise the raw
/// DEPCMD register at completion (status bits 15:12, resource index 22:16).
static mut TRACE_HARVEST: u32 = 0xFFFF_FFFF;
/// Raw DEPCMD register of the previous attempt's last SETTRANSFRESOURCE
/// (resource index bits 22:16, status bits 15:12) or 0xFFFF_FFFF.
static mut TRACE_HARVEST_RSC: u32 = 0xFFFF_FFFF;
/// Raw DEPCMD register of the previous attempt's last DEPSTARTCFG.
static mut TRACE_HARVEST_CFG: u32 = 0xFFFF_FFFF;
/// Raw DEPCMD register of the previous attempt's NEWEST STARTTRANSFER (the
/// last one issued before the reset), or 0xFFFF_FFFF.
static mut TRACE_HARVEST_LAST: u32 = 0xFFFF_FFFF;
/// Number of SETUP packets the previous attempt received (trace count of
/// TRACE_SETUP_RECEIVED).
static mut TRACE_HARVEST_SETUP: u32 = 0;
/// Number of descriptor DATA-IN transfers the previous attempt queued (trace
/// count of TRACE_DESCRIPTOR_QUEUED): proves the SETUP was parsed as a real
/// host request and the data phase was dispatched.
static mut TRACE_HARVEST_DESC: u32 = 0;
/// Raw DEPCMD register of the previous attempt's NEWEST STARTTRANSFER on
/// physical endpoint 1 (the data/status IN direction of EP0).
static mut TRACE_HARVEST_EP1: u32 = 0xFFFF_FFFF;
/// TRB status of the previous attempt's NEWEST XferComplete on physical
/// endpoint 1 (the control data-phase IN), or 0xFFFF_FFFF when the core
/// never completed the data TRB: 0x8 is the healthy LST|IOC completion, any
/// other value names the in-core transfer error.
static mut TRACE_HARVEST_EP1_XFER: u32 = 0xFFFF_FFFF;
/// Number of XferNotReady(CONTROL_DATA) events on physical endpoint 1: the
/// core reports it after fetching the data TRB, before any IN token is
/// answered with data.
static mut TRACE_HARVEST_EP1_NRDY: u32 = 0;
/// Number of STATUS-phase transfers the previous attempt queued (trace count
/// of TRACE_STATUS_QUEUED): proves the DATA phase completed on the wire and
/// the control state machine advanced.
static mut TRACE_HARVEST_STATUSQ: u32 = 0;
/// Number of poll-guard arm successes (TRACE_SETUP_QUEUED with the "ARME"
/// marker) in the previous attempts: proves the guard's deferred Start
/// Transfer ever succeeded while live.
static mut TRACE_HARVEST_ARMED: u32 = 0;
/// Sequence numbers of the OLDEST guard-arm (ARME) and OLDEST SETUP
/// reception: if the arm's sequence is lower, the SETUP TRB was armed before
/// the host's first SETUP token arrived (the arm won the race).
static mut TRACE_HARVEST_ARM_SEQ: u32 = 0xFFFF_FFFF;
static mut TRACE_HARVEST_SETUP_SEQ: u32 = 0xFFFF_FFFF;
/// Seconds between the previous attempt's Connect Done and its first SETUP
/// reception (0xFFFF = no such pair observed).
static mut TRACE_HARVEST_SETUP_DELAY: u32 = 0xFFFF;
/// CNTPCT tick of the last Connect Done, for the SETUP-delay measurement.
static mut CONNECT_TICK: u64 = 0;
/// Number of Connect Done events in the previous attempts: proves the core's
/// link FSM ever came up (without it the core cannot see any host traffic).
static mut TRACE_HARVEST_CONNECT: u32 = 0;
/// Number of SET_ADDRESS (bRequest=5) SETUP packets received: proves the
/// host accepted the device descriptor and moved to the next enumeration
/// stage, i.e. the DATA phase genuinely reached the host.
static mut TRACE_HARVEST_ADDR: u32 = 0;
/// 1 when a GET_DESCRIPTOR arrived AFTER a SET_ADDRESS: the host accepted
/// the address and sent the ADDRESSED read/all request, so the address
/// application worked and the failure is in the addressed response.
static mut TRACE_HARVEST_ADDR2: u32 = 0;
/// Newest "DARM" data-phase arm outcome (bit 16 = a record exists, bit 0 =
/// the Start Transfer ultimately queued after retries) or 0xFFFF_FFFF.
static mut TRACE_HARVEST_DARM: u32 = 0xFFFF_FFFF;
static mut INIT_CALLS: u32 = 0;
/// GCTL.RAMCLKSEL observed while the previous owner (Fastboot) still had a
/// working gadget. CSFTRST and the host's bus USB reset both clear this
/// field, and with the wrong select the DWC3 internal RAM misroutes
/// endpoint-context writes, which shows up as STARTTRANSFER failing with
/// "No resource" even though SETTRANSFRESOURCE reported success. Capture
/// the working value and re-apply it at every reset boundary.
static mut RAMCLK_CAPTURE: u32 = 0;

#[inline]
fn gctl_ramclksel(gctl: u32) -> u32 {
    (gctl >> 6) & 3
}

/// Architectural counter ticks (CNTPCT_EL0). Firmware always provides the
/// counter frequency on this platform; a zero read simply disables the
/// SETUP-delay measurement.
#[inline]
fn arch_counter() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!(
            "mrs {value}, CNTPCT_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[inline]
fn arch_counter_frequency() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!(
            "mrs {value}, CNTFRQ_EL0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

/// Restore the captured GCTL.RAMCLKSEL. Called after CSFTRST and after the
/// host's bus USB reset, both of which clear the field.
unsafe fn reapply_ramclksel() {
    unsafe {
        let captured = RAMCLK_CAPTURE;
        if captured == 0 {
            return;
        }
        let gctl = read(GCTL);
        let updated = (gctl & !(3 << 6)) | (captured << 6);
        if updated != gctl {
            write(GCTL, updated);
            let _ = read(GCTL);
            trace_event(TRACE_DWC3_REVISION_QUIRK, 0x524D_434B, gctl, updated, 0, 0);
        }
    }
}

/// Scan the retained trace backwards for the last STARTTRANSFER command
/// outcome. Called at the start of every handoff attempt except the first:
/// attempt N therefore reads attempt N-1's records, which are still intact
/// because the trace survives the in-boot DMA-region clear.
unsafe fn harvest_trace_outcome() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            return;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2)) as usize;
        if head == 0 {
            return;
        }
        let count = head.min(USB_TRACE_CAPACITY);
        TRACE_HARVEST_SETUP = 0;
        TRACE_HARVEST_DESC = 0;
        TRACE_HARVEST_STATUSQ = 0;
        TRACE_HARVEST_ARMED = 0;
        TRACE_HARVEST_ARM_SEQ = 0xFFFF_FFFF;
        TRACE_HARVEST_SETUP_SEQ = 0xFFFF_FFFF;
        TRACE_HARVEST_CONNECT = 0;
        TRACE_HARVEST_ADDR = 0;
        TRACE_HARVEST_ADDR2 = 0;
        TRACE_HARVEST_DARM = 0xFFFF_FFFF;
        TRACE_HARVEST_EP1_XFER = 0xFFFF_FFFF;
        TRACE_HARVEST_EP1_NRDY = 0;
        for offset in 0..count {
            let slot = (head.wrapping_sub(1 + offset)) % USB_TRACE_CAPACITY;
            let entry = addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(slot);
            let event = read_volatile(addr_of!((*entry).event));
            // Count every SETUP the previous attempts received: any count
            // above zero proves the core delivered a SETUP packet to DRAM.
            if event == TRACE_SETUP_RECEIVED {
                TRACE_HARVEST_SETUP = TRACE_HARVEST_SETUP.wrapping_add(1);
            }
            if event == TRACE_DEVICE_CONNECT {
                TRACE_HARVEST_CONNECT = TRACE_HARVEST_CONNECT.wrapping_add(1);
            }
            if event == TRACE_SETUP_RECEIVED {
                let request = read_volatile(addr_of!((*entry).request));
                if request == 5 {
                    TRACE_HARVEST_ADDR = TRACE_HARVEST_ADDR.wrapping_add(1);
                } else if request == 6 && TRACE_HARVEST_ADDR == 0 {
                    // Backward scan: a GET_DESCRIPTOR encountered BEFORE any
                    // SET_ADDRESS record is NEWER than every SET_ADDRESS,
                    // i.e. the host's post-address read/all request.
                    TRACE_HARVEST_ADDR2 = 1;
                }
            }
            if event == TRACE_DESCRIPTOR_QUEUED {
                TRACE_HARVEST_DESC = TRACE_HARVEST_DESC.wrapping_add(1);
                // The "DARM" record carries the final data-phase arm outcome
                // (bit 0 = queued after retries); the backward scan makes the
                // first hit the newest arm.
                if TRACE_HARVEST_DARM == 0xFFFF_FFFF
                    && read_volatile(addr_of!((*entry).request)) == 0x4441_524D
                {
                    TRACE_HARVEST_DARM = 0x1_0000 | (read_volatile(addr_of!((*entry).value)) & 1);
                }
            }
            if event == TRACE_STATUS_QUEUED {
                TRACE_HARVEST_STATUSQ = TRACE_HARVEST_STATUSQ.wrapping_add(1);
            }
            if event == TRACE_TRANSFER_COMPLETE {
                // The dispatch writes request=event kind (1), value=endpoint,
                // index=TRB status. The backward scan makes the first EP1 hit
                // the newest data-phase completion.
                if read_volatile(addr_of!((*entry).request)) == 1
                    && read_volatile(addr_of!((*entry).value)) == 1
                    && TRACE_HARVEST_EP1_XFER == 0xFFFF_FFFF
                {
                    TRACE_HARVEST_EP1_XFER = read_volatile(addr_of!((*entry).index));
                }
            }
            if event == TRACE_XFER_NOT_READY {
                // Recorded as request=endpoint, value=status (1 = CONTROL_DATA,
                // 2 = CONTROL_STATUS).
                if read_volatile(addr_of!((*entry).request)) == 1
                    && read_volatile(addr_of!((*entry).value)) == 1
                {
                    TRACE_HARVEST_EP1_NRDY = TRACE_HARVEST_EP1_NRDY.wrapping_add(1);
                }
            }
            if event == TRACE_SETUP_QUEUED {
                let marker = read_volatile(addr_of!((*entry).request));
                if marker == 0x4152_4D45 {
                    TRACE_HARVEST_ARMED = TRACE_HARVEST_ARMED.wrapping_add(1);
                    let sequence = read_volatile(addr_of!((*entry).sequence));
                    if sequence < TRACE_HARVEST_ARM_SEQ {
                        TRACE_HARVEST_ARM_SEQ = sequence;
                    }
                }
            }
            if event == TRACE_SETUP_RECEIVED {
                let sequence = read_volatile(addr_of!((*entry).sequence));
                if sequence < TRACE_HARVEST_SETUP_SEQ {
                    TRACE_HARVEST_SETUP_SEQ = sequence;
                }
            }
            if event != TRACE_EP_COMMAND_DONE && event != TRACE_EP_COMMAND_TIMEOUT {
                continue;
            }
            let command = read_volatile(addr_of!((*entry).request)) & 0x0f;
            let raw = read_volatile(addr_of!((*entry).index));
            let command_endpoint = read_volatile(addr_of!((*entry).value));
            let encode = |timeout: bool| -> u32 {
                if timeout {
                    0x1_0000 | raw
                } else {
                    raw & 0x7f_ffff
                }
            };
            let timed_out = event == TRACE_EP_COMMAND_TIMEOUT;
            // The backward scan overwrites: each field ends up holding the
            // chronologically FIRST record of its command type (attempt 1's
            // ep0-out command). The newest STARTTRANSFER values are captured
            // on the first hit before any overwrite can touch them.
            match command {
                DEPCMD_STARTTRANSFER => {
                    if TRACE_HARVEST_LAST == 0xFFFF_FFFF {
                        TRACE_HARVEST_LAST = encode(timed_out);
                    }
                    if command_endpoint == 1 && TRACE_HARVEST_EP1 == 0xFFFF_FFFF {
                        TRACE_HARVEST_EP1 = encode(timed_out);
                    }
                    TRACE_HARVEST = encode(timed_out);
                }
                DEPCMD_SETTRANSFRESOURCE => {
                    TRACE_HARVEST_RSC = encode(timed_out);
                }
                DEPCMD_DEPSTARTCFG => {
                    TRACE_HARVEST_CFG = encode(timed_out);
                }
                _ => {}
            }
        }
        // A SET_ADDRESS received after the newest GET_DESCRIPTOR invalidates
        // the read-all detection (that descriptor read was the pre-address
        // probe, not the post-address read/all).
        if TRACE_HARVEST_ADDR == 0 {
            TRACE_HARVEST_ADDR2 = 0;
        }
    }
}

#[repr(C, align(4096))]
struct UsbTraceBuffer {
    magic: u32,
    version: u32,
    head: u32,
    reserved: u32,
    entries: [UsbTraceEntry; USB_TRACE_CAPACITY],
}

#[unsafe(link_section = ".usb_trace")]
static mut USB_TRACE: UsbTraceBuffer = UsbTraceBuffer {
    magic: 0,
    version: 0,
    head: 0,
    reserved: 0,
    entries: [EMPTY_USB_TRACE; USB_TRACE_CAPACITY],
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ep0State {
    Setup,
    Data,
    Status,
}

/// Clear the linker-reserved DWC3 DMA region before enabling the controller.
///
/// The USB probe enters with caches/MMU disabled, so this is intentionally a
/// volatile byte/word clear rather than a normal Rust slice operation. The
/// caller must invoke it only after the previous controller owner has stopped
/// issuing DMA; it also seeds the allocator for later GSI/UDC allocations.
pub fn clear_dma_memory() {
    let mut current = addr_of!(__usb_dma_start) as usize;
    let end = addr_of!(__usb_dma_end) as usize;
    while current < end {
        unsafe {
            write_volatile(current as *mut u64, 0);
        }
        current += core::mem::size_of::<u64>();
    }
    unsafe {
        let pool = super::platform::bramble::usb_resources().dma_pool;
        let first_free = (end as u64 + 0xfff) & !0xfff;
        DMA_ALLOCATOR = super::platform::bramble::DmaPoolAllocator::new(pool, first_free);
    }
    trace_begin();
}

/// Allocate an identity-mapped USB DMA object from the active DT pool. The
/// caller must invoke this only after the SMMU/CPU mapping for the pool is
/// live; the returned pointer has the same address as the IOVA on Bramble.
pub unsafe fn allocate_usb_dma(size: usize, alignment: usize) -> Option<*mut u8> {
    if size == 0 || alignment == 0 {
        return None;
    }
    unsafe {
        let allocator = &mut *addr_of_mut!(DMA_ALLOCATOR);
        let allocator = allocator.as_mut()?;
        allocator
            .allocate(size as u64, alignment as u64)
            .map(|address| address as usize as *mut u8)
    }
}

/// Initialize the retained trace header and append a boot boundary marker.
/// The entry array is intentionally not cleared, so a subsequent boot can
/// inspect the last attempt after a warm reset.
fn trace_begin() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>(), USB_TRACE_MAGIC);
            write_volatile(
                addr_of_mut!(USB_TRACE).cast::<u32>().add(1),
                USB_TRACE_VERSION,
            );
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(2), 0);
            write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(3), 0);
        }
    }
    trace_event(TRACE_BOOT_USB_ENTRY, 0, 0, 0, 0, 0);
}

#[inline(always)]
fn ep0_state_code(state: Ep0State) -> u32 {
    match state {
        Ep0State::Setup => 1,
        Ep0State::Data => 2,
        Ep0State::Status => 3,
    }
}

#[inline(always)]
fn trace_event(event: u32, request: u32, value: u32, index: u32, length: u32, status: u32) {
    unsafe {
        let head_ptr = addr_of_mut!(USB_TRACE).cast::<u32>().add(2);
        let head = read_volatile(head_ptr);
        let slot = (head as usize) % USB_TRACE_CAPACITY;
        let entry = UsbTraceEntry {
            sequence: head.wrapping_add(1),
            event,
            request,
            value,
            index,
            length,
            ep0_state: ep0_state_code(EP0_STATE),
            status,
        };
        write_volatile(
            addr_of_mut!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(slot),
            entry,
        );
        write_volatile(head_ptr, head.wrapping_add(1));
    }
}

/// Start a retained trace for the standalone handoff probe without clearing
/// the previous attempt. A subsequent normal Fullerene boot can dump it over
/// UART before starting a new trace.
pub fn trace_probe_begin() {
    trace_begin();
}

/// Reset the retained trace cursor for a fresh boot. The region survives
/// warm resets by design, but between two `fastboot boot` runs Android
/// scribbles DRAM unpredictably: a surviving header would make the in-boot
/// harvest gates count the PREVIOUS run's records. The probe calls this once
/// per boot, before the first handoff attempt, so attempts 2/3 still see
/// attempt 1/2's records while cross-boot contamination is impossible.
pub fn trace_reset_head_for_boot() {
    unsafe {
        write_volatile(addr_of_mut!(USB_TRACE).cast::<u32>().add(2), 0);
        core::arch::asm!("dsb sy", options(nostack));
    }
}

/// Add a marker without touching the controller. This is used around PMIC
/// and platform transitions where the next MMIO access itself may abort.
pub fn trace_marker(event: u32, status: u32) {
    trace_event(event, 0, 0, 0, 0, status);
}

/// Read the retained trace cursor without changing the controller state.
/// Standalone probes use this as a watchdog activity signal: an EP0/device
/// event advances the cursor, while a completely absent USB session does not.
pub fn trace_head() -> u32 {
    unsafe { read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2)) }
}

/// Read the last committed event from the retained trace without advancing
/// it. The serial-string transport uses this pair as a compact host-visible
/// snapshot when the gadget has enumerated but UART is unavailable.
pub fn trace_last_event() -> u32 {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION || head == 0 {
            return 0;
        }
        let slot = (head.wrapping_sub(1) as usize) % USB_TRACE_CAPACITY;
        read_volatile(
            addr_of!(USB_TRACE.entries)
                .cast::<UsbTraceEntry>()
                .add(slot),
        )
        .event
    }
}

/// Fill one page of the retained trace for the vendor control request. The
/// page order is oldest-to-newest within the valid window, so a host can read
/// a consistent bounded snapshot without knowing the physical RAM address.
/// A request for page zero returns the header even when the trace is empty;
/// malformed or out-of-range pages are rejected by returning `None`.
unsafe fn fill_trace_control_response(
    response: &mut [u8],
    requested_length: usize,
    page: u16,
) -> Option<usize> {
    let requested_length = requested_length.min(response.len());
    if requested_length == 0 {
        return None;
    }
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            return None;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        let valid = (head as usize).min(USB_TRACE_CAPACITY);
        let page_start = (page as usize).checked_mul(TRACE_CONTROL_PAGE_ENTRIES)?;
        if page_start > valid {
            return None;
        }

        response[..requested_length].fill(0);
        let mut write_u32 = |offset: usize, value: u32| {
            if offset + 4 <= requested_length {
                response[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        };
        write_u32(0, magic);
        write_u32(4, version);
        write_u32(8, head);
        write_u32(12, valid as u32);
        if requested_length <= TRACE_CONTROL_HEADER_BYTES {
            return Some(requested_length);
        }

        let available = valid.saturating_sub(page_start);
        let records = available
            .min(TRACE_CONTROL_PAGE_ENTRIES)
            .min((requested_length - TRACE_CONTROL_HEADER_BYTES) / TRACE_CONTROL_ENTRY_BYTES);
        let oldest = (head as usize).saturating_sub(valid);
        for index in 0..records {
            let slot = (oldest + page_start + index) % USB_TRACE_CAPACITY;
            let entry = read_volatile(
                addr_of!(USB_TRACE.entries)
                    .cast::<UsbTraceEntry>()
                    .add(slot),
            );
            let values = [
                entry.sequence,
                entry.event,
                entry.request,
                entry.value,
                entry.index,
                entry.length,
                entry.ep0_state,
                entry.status,
            ];
            let base = TRACE_CONTROL_HEADER_BYTES + index * TRACE_CONTROL_ENTRY_BYTES;
            for (word, value) in values.into_iter().enumerate() {
                response[base + word * 4..base + word * 4 + 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
        }
        Some(TRACE_CONTROL_HEADER_BYTES + records * TRACE_CONTROL_ENTRY_BYTES)
    }
}

/// Return whether the handoff probe has successfully started at least one
/// EP0 DATA or STATUS transfer. This is intentionally weaker than
/// SET_CONFIGURATION: a host may fetch descriptors without configuring the
/// diagnostic gadget, and that must not look like a hung probe.
pub fn probe_ep0_progress() -> bool {
    unsafe { PROBE_EP0_PROGRESS }
}

fn note_probe_ep0_progress() {
    unsafe {
        PROBE_EP0_PROGRESS = true;
    }
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub fn gadget_handoff_failure_stage() -> u32 {
    unsafe { GADGET_HANDOFF_FAILURE_STAGE }
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub fn gadget_handoff_stage_probe_enabled() -> bool {
    cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_1)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_2)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_3)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_4)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_5)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_6)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_7)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_8)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_9)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_10)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_11)
        || cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_12)
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
fn gadget_handoff_fail(stage: u32) -> bool {
    unsafe {
        GADGET_HANDOFF_FAILURE_STAGE = stage;
    }
    trace_marker(TRACE_PROBE_WATCHDOG, 0x4641_0000 | (stage & 0xff)); // "FA" + stage
    // A selected stage probe must distinguish "the operation reached its
    // boundary" from "the operation failed before the boundary".  For the
    // pre-STARTTRANSFER stages the already-proven bare pull-up is still the
    // correct electrical probe.  Once EP0 has been armed, repeat only the
    // controller-side Run/Stop boundary; re-running the bare initializer
    // would rewrite endpoint/DMA state and hide the actual failure point.
    if gadget_handoff_stop_selected(stage) {
        unsafe {
            if stage >= 6 {
                let _ = stop_after_gadget_handoff_stage(stage);
            } else {
                let _ = init_usb2_bare_pullup_handoff_inner(true);
            }
        }
    }
    false
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
#[inline]
fn gadget_handoff_stop_selected(stage: u32) -> bool {
    match stage {
        1 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_1),
        2 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_2),
        3 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_3),
        4 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_4),
        5 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_5),
        6 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_6),
        7 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_7),
        8 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_8),
        9 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_9),
        10 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_10),
        11 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_11),
        12 => cfg!(fullerene_aarch64_usb_gadget_handoff_stop_after_12),
        _ => false,
    }
}

/// Publish the physical pull-up at one handoff boundary, then return through
/// the normal failure/recovery path. This is a host-observable stage probe:
/// it deliberately does not pretend that an EP0-less pull-up is a working
/// gadget, but it tells us whether the preceding DWC3 operation still leaves
/// the USB2 electrical path able to attach before the handset recovers.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn stop_after_gadget_handoff_stage(stage: u32) -> bool {
    if !gadget_handoff_stop_selected(stage) {
        return false;
    }
    trace_marker(TRACE_PROBE_WATCHDOG, 0x5354_0000 | (stage & 0xff)); // "ST" + stage
    if stage == 7 {
        // Stage 7 is immediately after STARTTRANSFER.  Keep this probe on
        // the exact production boundary: only reassert the Qualcomm session
        // votes, select the USB2 speed, and perform Run/Stop.  Re-running the
        // bare initializer would reset/reconfigure the controller and make
        // a successful STARTTRANSFER indistinguishable from a failed one.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        configure_gadget_speed(false);
        if !unsafe { run_stop_device(true) } {
            // If STARTTRANSFER completed but the production Run/Stop
            // boundary did not, reset the controller and expose the known
            // electrical probe.  No attach in this stage then points to the
            // STARTTRANSFER boundary itself; an attach points to Run/Stop.
            let _ = unsafe { device_soft_reset() };
            let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
        }
        return true;
    }
    if stage == 8 {
        // STARTTRANSFER may leave the endpoint command engine busy on a
        // failed handoff.  Reset only the DWC3 device state before falling
        // back to the known-good electrical probe, so this failure boundary
        // remains observable even when the command itself wedged the core.
        let _ = unsafe { device_soft_reset() };
        let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
        return true;
    }
    if stage >= 6 {
        // At this point the real handoff path has already performed the
        // controller-side PHY/clock setup and, for stage 6, queued the first
        // EP0 STARTTRANSFER. Re-running the bare initializer would rewrite
        // those stateful registers and make the stage probe test a different
        // path from the actual Run/Stop boundary.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        configure_gadget_speed(false);
        let _ = unsafe { run_stop_device(true) };
        return true;
    }
    // Reuse the exact bare path already proven to create a physical attach.
    // This keeps the stage experiment about the preceding handoff boundary,
    // rather than introducing a second, subtly different Run/Stop sequence.
    let _ = unsafe { init_usb2_bare_pullup_handoff_inner(true) };
    true
}

/// Dump the post-mortem USB trace after the controller has reached a safe
/// UART-visible stage. The hot path above never calls this or formats text.
pub fn dump_trace() {
    unsafe {
        let magic = read_volatile(addr_of!(USB_TRACE).cast::<u32>());
        let version = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(1));
        if magic != USB_TRACE_MAGIC || version != USB_TRACE_VERSION {
            uart::puts("usb trace: no retained record\n");
            return;
        }
        let head = read_volatile(addr_of!(USB_TRACE).cast::<u32>().add(2));
        let count = (head as usize).min(USB_TRACE_CAPACITY);
        let start = (head as usize).saturating_sub(count);
        uart::puts("usb trace begin\n");
        for offset in 0..count {
            let slot = (start + offset) % USB_TRACE_CAPACITY;
            let entry = read_volatile(
                addr_of!(USB_TRACE.entries)
                    .cast::<UsbTraceEntry>()
                    .add(slot),
            );
            uart::put_hex("usb trace event=", entry.event as u64);
            uart::put_hex(" request=", entry.request as u64);
            uart::put_hex(" value=", entry.value as u64);
            uart::put_hex(" index=", entry.index as u64);
            uart::put_hex(" length=", entry.length as u64);
            uart::put_hex(" state=", entry.ep0_state as u64);
            uart::put_hex(" status=", entry.status as u64);
        }
        uart::puts("usb trace end\n");
    }
}

#[inline]
fn reg(offset: usize) -> *mut u32 {
    (dwc3_base() + offset) as *mut u32
}

#[inline]
fn qscratch_reg(offset: usize) -> *mut u32 {
    (qscratch_base() + offset) as *mut u32
}

#[inline]
fn hsphy_reg(offset: usize) -> *mut u32 {
    (hsphy_base() + offset) as *mut u32
}

#[inline]
fn qmp_reg(offset: usize) -> *mut u32 {
    (qmp_base() + offset) as *mut u32
}

#[inline]
fn qmp_contract_offset(slot: usize, fallback: usize) -> usize {
    let offset = super::platform::bramble::usb_resources().qmp_reg_offsets[slot];
    if offset == 0xffff { fallback } else { offset }
}

#[inline]
unsafe fn smmu_reg(offset: usize) -> *mut u32 {
    (apps_smmu_base() + offset) as *mut u32
}

#[inline]
unsafe fn smmu_page_reg(page_size: usize, page: usize, offset: usize) -> *mut u32 {
    (apps_smmu_base() + page * page_size + offset) as *mut u32
}

#[inline]
unsafe fn smmu_page_write(page_size: usize, page: usize, offset: usize, value: u32) {
    unsafe { write_volatile(smmu_page_reg(page_size, page, offset), value) };
}

#[inline]
unsafe fn smmu_page_read(page_size: usize, page: usize, offset: usize) -> u32 {
    unsafe { read_volatile(smmu_page_reg(page_size, page, offset)) }
}

#[inline]
unsafe fn smmu_page_write64(page_size: usize, page: usize, offset: usize, value: u64) {
    unsafe { write_volatile(smmu_page_reg(page_size, page, offset).cast::<u64>(), value) };
}

#[inline]
unsafe fn smmu_page_read64(page_size: usize, page: usize, offset: usize) -> u64 {
    unsafe { read_volatile(smmu_page_reg(page_size, page, offset).cast::<u64>()) }
}

/// Verify that an already-live S1 context translates the complete linker DMA
/// section as an identity map. A Fastboot handoff may safely preserve Linux's
/// context only when its page tables cover every Fullerene TRB/event object;
/// preserving an unrelated bootloader map would make the first DMA fault look
/// like an EP0 protocol failure.
unsafe fn smmu_context_maps_identity(ttbr0: u64, tcr: u32, start: usize, end: usize) -> bool {
    let iova_start = start as u64;
    let iova_end = end as u64;
    if ttbr0 == 0
        || ttbr0 == u64::MAX
        || start >= end
        || iova_end > (1u64 << 39)
        || (tcr >> 14) & 0x3 != 0
    {
        return false;
    }

    // Linux's qcom,use-3-lvl-tables caps the AArch64 aperture at 39 bits;
    // T0SZ=25 (39-bit) and T0SZ=32 (32-bit) both use an L1->L2->L3 walk for
    // a 4 KiB granule. This checker intentionally refuses an unfamiliar
    // format instead of guessing at a table level.
    let iova_bits = 64u32.saturating_sub(tcr & 0x3f);
    if !(30..=39).contains(&iova_bits) {
        return false;
    }

    // Linux stores the context ASID in TTBR0[63:48]. It is not part of the
    // physical page-table address and must be removed before the CPU-side
    // identity walk below; otherwise a live Android context can appear
    // unmapped solely because its ASID is non-zero.
    let table_root = ttbr0 & 0x0000_ffff_ffff_f000;
    let read_entry = |base: u64, index: u64| -> Option<u64> {
        let offset = index.checked_mul(8)?;
        let address = base.checked_add(offset)?;
        (address <= usize::MAX as u64)
            .then(|| unsafe { read_volatile(address as usize as *const u64) })
    };
    let maps_one_2m_block = |address: u64| -> bool {
        let l1_index = (address >> 30) & 0x1ff;
        let Some(l1) = read_entry(table_root, l1_index) else {
            return false;
        };
        match l1 & SMMU_DESC_TYPE_MASK {
            // An existing Android/IOMMU mapping may use a 1 GiB identity
            // block for a large DMA aperture. It is just as valid for the
            // preservation check as the finer-grained L2/L3 forms.
            SMMU_DESC_BLOCK => {
                l1 & SMMU_DESC_ADDRESS_MASK == address & !((1u64 << 30) - 1)
                    && l1 & SMMU_DESC_VALID != 0
            }
            SMMU_DESC_TABLE => {
                let l2_base = l1 & SMMU_DESC_ADDRESS_MASK;
                let l2_index = (address >> 21) & 0x1ff;
                let Some(l2) = read_entry(l2_base, l2_index) else {
                    return false;
                };
                let block_base = address & !((1u64 << 21) - 1);
                match l2 & SMMU_DESC_TYPE_MASK {
                    SMMU_DESC_BLOCK => {
                        l2 & SMMU_DESC_ADDRESS_MASK == block_base && l2 & SMMU_DESC_VALID != 0
                    }
                    SMMU_DESC_TABLE => {
                        let l3_base = l2 & SMMU_DESC_ADDRESS_MASK;
                        let l3_index = (address >> 12) & 0x1ff;
                        let Some(l3) = read_entry(l3_base, l3_index) else {
                            return false;
                        };
                        l3 & SMMU_DESC_TYPE_MASK == SMMU_DESC_TABLE
                            && l3 & SMMU_DESC_ADDRESS_MASK == address & !0xfff
                            && l3 & SMMU_DESC_VALID != 0
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    };

    let mut address = iova_start & !((1u64 << 21) - 1);
    while address < iova_end {
        if !maps_one_2m_block(address) {
            return false;
        }
        address = address.saturating_add(1u64 << 21);
    }
    true
}

unsafe fn smmu_tlb_sync() {
    unsafe {
        write_volatile(smmu_reg(SMMU_TLB_ALL_H), 0);
        write_volatile(smmu_reg(SMMU_TLB_SYNC), 0);
        for _ in 0..100_000u32 {
            if read_volatile(smmu_reg(SMMU_TLB_STATUS)) & SMMU_TLB_STATUS_ACTIVE == 0 {
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Consume an Apps-SMMU context fault using the same ordering as Linux's
/// arm_smmu_context_fault(): sample FSR/FAR/FSYNR0, retain the evidence, then
/// clear FSR and terminate a stalled transaction. This is polled because the
/// The normal entry is now the Apps-SMMU global/context IRQ path; polling
/// remains as a recovery path for the short interval before the GIC owner is
/// installed or when firmware leaves a latched fault asserted.
unsafe fn service_smmu_fault() {
    let global_fsr = unsafe { read_volatile(smmu_reg(SMMU_GR0_FSR)) };
    if global_fsr != 0 && global_fsr != u32::MAX && global_fsr & SMMU_GLOBAL_FSR_FAULT != 0 {
        let global_fsynr0 = unsafe { read_volatile(smmu_reg(SMMU_GR0_FSYNR0)) };
        trace_event(TRACE_SMMU_GLOBAL_FAULT, global_fsr, global_fsynr0, 0, 0, 0);
        log_hex("usb: Apps SMMU global fault FSR=", global_fsr as u64);
        unsafe { write_volatile(smmu_reg(SMMU_GR0_FSR), global_fsr) };
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    }
    let page = unsafe { SMMU_CONTEXT_PAGE };
    let page_size = unsafe { SMMU_CONTEXT_PAGE_SIZE };
    if page == usize::MAX || page_size == 0 {
        return;
    }
    let fsr = unsafe { smmu_page_read(page_size, page, SMMU_CB_FSR) };
    if fsr == 0 || fsr == u32::MAX || fsr & SMMU_FSR_FAULT == 0 {
        return;
    }
    let far = unsafe { smmu_page_read64(page_size, page, SMMU_CB_FAR) };
    let fsynr0 = unsafe { smmu_page_read(page_size, page, SMMU_CB_FSYNR0) };
    trace_event(
        TRACE_SMMU_FAULT,
        fsr,
        fsynr0,
        far as u32,
        (far >> 32) as u32,
        page as u32,
    );
    log_hex("usb: Apps SMMU context fault FSR=", fsr as u64);
    log_hex("usb: Apps SMMU FAR=", far);
    unsafe { smmu_page_write(page_size, page, SMMU_CB_FSR, fsr) };
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    if fsr & SMMU_FSR_SS != 0 {
        unsafe { smmu_page_write(page_size, page, SMMU_CB_RESUME, SMMU_RESUME_TERMINATE) };
    }
}

unsafe fn install_smmu_identity_table(pool: super::platform::bramble::DmaPoolResource) -> bool {
    let pool_base = pool.iova_base as u64;
    let Some(pool_end) = pool_base.checked_add(pool.size as u64) else {
        return false;
    };
    // This table format intentionally maps only complete 2 MiB blocks. The
    // Android Lito pool is aligned to that granularity; refusing an unusual
    // DT pool is safer than exposing bytes outside the declared DMA window.
    if pool_base & 0x1f_ffff != 0
        || pool_end & 0x1f_ffff != 0
        || pool_end > (1u64 << 32)
        || pool_base >= pool_end
    {
        return false;
    }

    unsafe {
        for index in 0..512usize {
            write_volatile(addr_of_mut!(SMMU_L1.0[index]), 0);
        }
        for table in 0..4usize {
            for index in 0..512usize {
                write_volatile(addr_of_mut!(SMMU_L2[table].0[index]), 0);
            }
        }

        for l1_index in 0..4usize {
            let l1_base = (l1_index as u64) << 30;
            let l1_end = l1_base + (1u64 << 30);
            if pool_base >= l1_end || pool_end <= l1_base {
                continue;
            }
            // The L1 table descriptor points at the corresponding 4 KiB
            // level-2 table. Each L2 entry describes a 2 MiB block.
            let table_address = addr_of!(SMMU_L2[l1_index]) as usize as u64;
            write_volatile(
                addr_of_mut!(SMMU_L1.0[l1_index]),
                (table_address & !0xfff) | SMMU_DESC_TABLE,
            );
            for l2_index in 0..512usize {
                let physical = l1_base + (l2_index as u64) * (1u64 << 21);
                if physical >= pool_base && physical + (1u64 << 21) <= pool_end {
                    let descriptor = physical
                        | SMMU_DESC_VALID
                        | SMMU_DESC_AF
                        | SMMU_DESC_SH_INNER
                        | SMMU_DESC_ATTR_NORMAL
                        | SMMU_DESC_XN;
                    write_volatile(addr_of_mut!(SMMU_L2[l1_index].0[l2_index]), descriptor);
                }
            }
        }
        cache_clean(
            addr_of!(SMMU_L1) as usize,
            core::mem::size_of::<SmmuTable>(),
        );
        cache_clean(
            addr_of!(SMMU_L2) as usize,
            core::mem::size_of::<[SmmuTable; 4]>(),
        );
    }
    true
}

/// Install an AArch64 stage-1 identity mapping for DWC3's stream ID.
///
/// Bramble's vendor DT assigns DWC3 stream ID 0xe0 to the Apps SMMU and puts
/// USB buffers in the 0x90000000..0xf0000000 IOVA pool. Qualcomm's SMMU-500
/// firmware can reject a direct BYPASS write by turning it into FAULT, so a
/// real context-bank map is required here. We preserve the existing SMR and
/// route it to a context bank configured as S1 translation + S2 bypass.
pub fn configure_dwc3_smmu() -> bool {
    unsafe {
        // A failed reconfiguration must not leave the fault poller pointing
        // at a context bank from an earlier controller lifetime.
        SMMU_CONTEXT_PAGE = usize::MAX;
        SMMU_CONTEXT_PAGE_SIZE = 0;
        let pool = super::platform::bramble::usb_resources().dma_pool;
        let dma_start = addr_of!(__usb_dma_start) as usize;
        let dma_end = addr_of!(__usb_dma_end) as usize;
        trace_event(
            TRACE_SMMU_BEGIN,
            apps_smmu_base() as u32,
            pool.stream_id,
            dma_start as u32,
            dma_end as u32,
            pool.iova_base as u32,
        );
        let Some(pool_end) = pool.iova_base.checked_add(pool.size) else {
            log_puts("usb: invalid DT DMA pool\n");
            return false;
        };
        if dma_start < pool.iova_base || dma_end > pool_end || dma_start >= dma_end {
            log_puts("usb: DMA section is outside the DT IOVA pool\n");
            return false;
        }
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            log_puts("usb: Apps SMMU identification unavailable\n");
            return false;
        }

        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        let page_size = if id1 & SMMU_ID1_PAGESIZE != 0 {
            0x10000
        } else {
            0x1000
        };
        if page_size != 0x1000 {
            // The table below is intentionally 4 KiB-granule LPAE. Do not
            // enable a mismatched table on a future 64 KiB-only SMMU.
            log_puts("usb: Apps SMMU requires unsupported 64K tables\n");
            return false;
        }

        let num_pages =
            1usize << (((id1 >> SMMU_ID1_NUMPAGENDXB_SHIFT) & SMMU_ID1_NUMPAGENDXB_MASK) + 1);
        let num_s2_context_banks =
            ((id1 >> SMMU_ID1_NUMS2CB_SHIFT) & SMMU_ID1_NUMS2CB_MASK) as usize;
        let num_context_banks = (id1 & SMMU_ID1_NUMCB_MASK) as usize;
        if num_pages == 0 || num_context_banks == 0 {
            log_puts("usb: Apps SMMU has no usable context banks\n");
            return false;
        }
        // The GR0 window is page 0 and GR1 is page 1. Context-bank pages start
        // after the implementation-defined number of global pages.
        let gr1_page = 1usize;
        let cb_base_page = num_pages;
        log_hex("usb: Apps SMMU ID0=", id0 as u64);
        log_hex("usb: Apps SMMU ID1=", id1 as u64);
        log_hex("usb: Apps SMMU pages=", num_pages as u64);

        let mut matched = None;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if ((pool.stream_id ^ id) & !mask) == 0 {
                matched = Some((index, read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4))));
                break;
            }
        }
        let Some((smr_index, old_s2cr)) = matched else {
            log_puts("usb: DWC3 stream 0xe0 has no SMMU match\n");
            return false;
        };

        let old_type = old_s2cr & SMMU_S2CR_TYPE_MASK;
        let old_cb = (old_s2cr & SMMU_S2CR_CBNDX_MASK) as usize;
        // Fastboot commonly keeps the DWC3 stream in S2CR.BYPASS while its
        // USB buffers are identity-addressed. This is already a valid
        // physical=IOVA mapping for the linker DMA pool. Replacing it with a
        // freshly selected context bank during `fastboot boot` changes an
        // ownership boundary that Linux would preserve until its IOMMU
        // domain is fully attached, and can make the first EP0 TRB fault.
        if old_type == SMMU_S2CR_TYPE_BYPASS {
            trace_event(
                TRACE_SMMU_PRESERVED,
                smr_index as u32,
                old_cb as u32,
                old_s2cr,
                dma_start as u32,
                dma_end as u32,
            );
            log_puts("usb: preserving Fastboot Apps SMMU bypass\n");
            return true;
        }
        let cbndx = if old_type == SMMU_S2CR_TYPE_TRANS {
            if old_cb >= num_context_banks || old_cb < num_s2_context_banks {
                log_puts("usb: DWC3 SMMU context bank is out of range\n");
                return false;
            }
            old_cb
        } else {
            // This is the same reserved-last-context-bank strategy used by
            // Linux's qcom_smmu bypass-quirk path for firmware that refuses
            // BYPASS S2CR values.
            num_context_banks - 1
        };
        log_hex("usb: DWC3 SMMU SMR=", smr_index as u64);
        log_hex("usb: DWC3 SMMU CB=", cbndx as u64);
        // CBAR.IRPTNDX is an index into the dense context-bank IRQ list in
        // the provider DT, not a GIC SPI number.  Linux's irq-domain setup
        // programs the same association before enabling context faults.
        let context_irq_count = super::platform::bramble::usb_resources().smmu_context_irq_count;
        let irptndx = if context_irq_count != 0 {
            cbndx % context_irq_count
        } else {
            cbndx
        };
        SMMU_CONTEXT_PAGE = cb_base_page + cbndx;
        SMMU_CONTEXT_PAGE_SIZE = page_size;

        // Linux's SMMU driver owns this context, not the DWC3 glue. A
        // Fastboot handoff can therefore arrive with a valid stage-1 map
        // already installed for the same stream. Replacing that context
        // underneath firmware is unsafe: the controller may still have an
        // outstanding transaction using the old page tables, and changing
        // TTBR0 can turn a benign handoff into a stream fault. Preserve a
        // live translation context and use the declared pool check above as
        // the ownership boundary for Fullerene's DMA objects.
        if old_type == SMMU_S2CR_TYPE_TRANS {
            let cb_page = cb_base_page + cbndx;
            let sctlr = smmu_page_read(page_size, cb_page, SMMU_CB_SCTLR);
            let ttbr0 = smmu_page_read64(page_size, cb_page, SMMU_CB_TTBR0);
            let tcr = smmu_page_read(page_size, cb_page, SMMU_CB_TCR);
            if sctlr & SMMU_SCTLR_M != 0
                && smmu_context_maps_identity(ttbr0, tcr, dma_start, dma_end)
            {
                trace_event(
                    TRACE_SMMU_PRESERVED,
                    smr_index as u32,
                    cbndx as u32,
                    sctlr,
                    ttbr0 as u32,
                    (ttbr0 >> 32) as u32,
                );
                log_puts("usb: preserving active Apps SMMU translation\n");
                return true;
            }
            log_puts("usb: active Apps SMMU map does not cover Fullerene DMA\n");
        }

        if !install_smmu_identity_table(pool) {
            log_puts("usb: DT DMA pool is not 2 MiB aligned/32-bit addressable\n");
            return false;
        }

        // Stop the bank before changing its format and page-table pointer.
        smmu_page_write(page_size, cb_base_page + cbndx, SMMU_CB_SCTLR, 0);
        smmu_page_write(
            page_size,
            gr1_page,
            SMMU_GR1_CBA2R_BASE + cbndx * 4,
            SMMU_CBA2R_VA64,
        );
        smmu_page_write(
            page_size,
            gr1_page,
            SMMU_GR1_CBAR_BASE + cbndx * 4,
            SMMU_CBAR_S1_TRANS_S2_BYPASS
                | SMMU_CBAR_S1_MEMATTR_WB
                | SMMU_CBAR_S1_BPSHCFG_NSH
                | (irptndx as u32 & SMMU_CBAR_IRPTNDX_MASK),
        );

        // SCR0 interrupt enables are deliberately NOT written here. The
        // global/CFG fault lines are polled (service_smmu_fault) and the
        // secure-side SCR0 bits can reject non-secure writes on Qualcomm
        // firmware, which aborts the whole handoff before the pull-up.

        let cb_page = cb_base_page + cbndx;
        // 4 KiB granule, 32-bit IOVA, inner-shareable WBWA walks, and a
        // 40-bit output address size. TCR2 selects the AArch64 format.
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_TCR2,
            SMMU_TCR2_SEP_UPSTREAM | SMMU_TCR2_AS | SMMU_TCR2_PASIZE_40BIT,
        );
        let t0sz = if super::platform::bramble::usb_resources().smmu_use_3_level_tables {
            SMMU_TCR_T0SZ_39BIT
        } else {
            SMMU_TCR_T0SZ_32BIT
        };
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_TCR,
            SMMU_TCR_EPD1 | SMMU_TCR_SH0_INNER | SMMU_TCR_ORGN0_WBWA | SMMU_TCR_IRGN0_WBWA | t0sz,
        );
        smmu_page_write64(
            page_size,
            cb_page,
            SMMU_CB_TTBR0,
            addr_of!(SMMU_L1) as usize as u64,
        );
        smmu_page_write64(page_size, cb_page, SMMU_CB_TTBR1, 0);
        smmu_page_write(page_size, cb_page, SMMU_CB_CONTEXTIDR, 0);
        smmu_page_write(page_size, cb_page, SMMU_CB_MAIR0, 0xff);
        smmu_page_write(page_size, cb_page, SMMU_CB_MAIR1, 0);
        smmu_page_write(
            page_size,
            cb_page,
            SMMU_CB_SCTLR,
            SMMU_SCTLR_S1_ASIDPNE
                | SMMU_SCTLR_CFIE
                | SMMU_SCTLR_CFRE
                | SMMU_SCTLR_AFE
                | SMMU_SCTLR_TRE
                | SMMU_SCTLR_M,
        );

        // S2CR type TRANS is zero; preserve privilege and EXID bits from the
        // firmware entry while replacing only the context-bank selector.
        let new_s2cr = (old_s2cr & !SMMU_S2CR_CBNDX_MASK) & !SMMU_S2CR_TYPE_MASK
            | ((cbndx as u32) & SMMU_S2CR_CBNDX_MASK)
            | SMMU_S2CR_TYPE_TRANS;
        let s2cr_address = SMMU_S2CR_BASE + smr_index * 4;
        write_volatile(smmu_reg(s2cr_address), new_s2cr);
        core::arch::asm!("dsb sy", options(nostack));
        let readback = read_volatile(smmu_reg(s2cr_address));
        if readback & SMMU_S2CR_TYPE_MASK != SMMU_S2CR_TYPE_TRANS
            || (readback & SMMU_S2CR_CBNDX_MASK) as usize != cbndx
        {
            log_puts("usb: DWC3 SMMU S2CR translation rejected\n");
            return false;
        }
        smmu_tlb_sync();
        trace_event(
            TRACE_SMMU_READY,
            smr_index as u32,
            cbndx as u32,
            id0,
            id1,
            0,
        );
        true
    }
}

#[inline]
unsafe fn read_qscratch(offset: usize) -> u32 {
    unsafe { read_volatile(qscratch_reg(offset)) }
}

#[inline]
unsafe fn write_qscratch(offset: usize, value: u32) {
    unsafe { write_volatile(qscratch_reg(offset), value) };
    let _ = unsafe { read_volatile(qscratch_reg(offset)) };
}

#[inline]
unsafe fn hsphy_update(offset: usize, mask: u32, value: u32) {
    let current = unsafe { read_volatile(hsphy_reg(offset)) };
    unsafe { write_volatile(hsphy_reg(offset), (current & !mask) | (value & mask)) };
    let _ = unsafe { read_volatile(hsphy_reg(offset)) };
}

unsafe fn init_qmp_phy() -> bool {
    let com_power_down = qmp_contract_offset(8, QMP_COM_POWER_DOWN_CTRL);
    let pcs_power_down = qmp_contract_offset(3, QMP_PCS_POWER_DOWN_CONTROL);
    let reset_override = qmp_contract_offset(10, QMP_COM_RESET_OVRD_CTRL);
    let typec = qmp_contract_offset(12, QMP_COM_TYPEC_CTRL);
    let phy_mode = qmp_contract_offset(11, QMP_COM_PHY_MODE_CTRL);
    let com_sw_reset = qmp_contract_offset(9, QMP_COM_SW_RESET);
    let pcs_sw_reset = qmp_contract_offset(4, QMP_PCS_SW_RESET);
    let pcs_start = qmp_contract_offset(5, QMP_PCS_START_CONTROL);
    let pcs_status = qmp_contract_offset(0, QMP_PCS_STATUS1);
    unsafe {
        // Match msm_ssphy_qmp_init(): power the common and PCS blocks before
        // selecting the Type-C lane and USB+DP combo mode. The lane value is
        // 2 for lane A and 3 for lane B, as used by the Android QMP driver.
        write_volatile(qmp_reg(com_power_down), 0x01);
        write_volatile(qmp_reg(pcs_power_down), 0x01);
        let lane = if TYPEC_LANE_B { 0x03 } else { 0x02 };
        write_volatile(qmp_reg(reset_override), 0x0f);
        write_volatile(qmp_reg(typec), lane);
        let _ = read_volatile(qmp_reg(typec));
        write_volatile(qmp_reg(phy_mode), 0x03);
        let _ = read_volatile(qmp_reg(phy_mode));
        write_volatile(qmp_reg(reset_override), 0x00);

        for index in 0..146 {
            let (offset, value) = ACTIVE_QMP_INIT[index];
            write_volatile(qmp_reg(offset), value);
            let delay_us = ACTIVE_QMP_INIT_DELAY_US[index];
            if delay_us != 0 {
                super::timer::delay_us(delay_us as u64);
            }
        }

        write_volatile(qmp_reg(com_sw_reset), 0x00);
        write_volatile(qmp_reg(pcs_sw_reset), 0x00);
        write_volatile(qmp_reg(pcs_start), 0x03);
        let _ = read_volatile(qmp_reg(pcs_status));
        for _ in 0..1_000_000 {
            if read_volatile(qmp_reg(pcs_status)) & QMP_PHYSTATUS == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    log_puts("usb: QMP PHY initialization timeout\n");
    false
}

/// Clear the QMP LFPS receiver-detect interrupt using the required 1 -> 0
/// sequence from msm-ssusb-qmp. A readback between the writes is not needed
/// by the PHY, but the compiler/MMIO ordering barrier is: the second write
/// must not be observed before the clear is asserted.
unsafe fn qmp_clear_lfps_rxterm_irq() {
    let clear = qmp_contract_offset(2, QMP_PCS_LFPS_RXTERM_IRQ_CLEAR);
    unsafe {
        write_volatile(qmp_reg(clear), QMP_LFPS_IRQ_CLEAR);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write_volatile(qmp_reg(clear), 0);
        let _ = read_volatile(qmp_reg(clear));
    }
}

/// Match msm_ssusb_qmp_enable_autonomous()/disable_autonomous_mode() for the
/// Lito USB+DP combo PHY. The device-side SuperSpeed mode enables both
/// receiver-detect and LFPS detection; the receiver-detect event-select bit
/// stays clear in that mode. Autonomous mode also turns on the PCS I/O clamp
/// (the register is active-high for disabling the clamp, hence clear it when
/// enabling autonomous operation).
unsafe fn qmp_set_autonomous_mode(enable: bool) {
    let autonomous = qmp_contract_offset(1, QMP_PCS_AUTONOMOUS_MODE_CTRL);
    let clamp_offset = qmp_contract_offset(14, QMP_PCS_CLAMP_ENABLE);
    unsafe {
        if enable {
            qmp_clear_lfps_rxterm_irq();
            let mut value = read_volatile(qmp_reg(autonomous));
            value &= !(QMP_ARCVR_DTCT_EN | QMP_ALFPS_DTCT_EN | QMP_ARCVR_DTCT_EVENT_SEL);
            value |= QMP_ARCVR_DTCT_EN | QMP_ALFPS_DTCT_EN;
            write_volatile(qmp_reg(autonomous), value);
            // Android's combo-PHY path calls clamp_enable(true), which
            // writes !true to this active-high clamp control.
            let mut clamp = read_volatile(qmp_reg(clamp_offset));
            clamp &= !QMP_CLAMP_EN;
            write_volatile(qmp_reg(clamp_offset), clamp);
            let _ = read_volatile(qmp_reg(autonomous));
        } else {
            // Resume first releases the clamp, then disables both autonomous
            // detectors, and finally clears any receiver-detect edge left by
            // the suspended PHY.
            let mut clamp = read_volatile(qmp_reg(clamp_offset));
            clamp |= QMP_CLAMP_EN;
            write_volatile(qmp_reg(clamp_offset), clamp);
            let mut value = read_volatile(qmp_reg(autonomous));
            value &= !(QMP_ARCVR_DTCT_EN | QMP_ALFPS_DTCT_EN | QMP_ARCVR_DTCT_EVENT_SEL);
            write_volatile(qmp_reg(autonomous), value);
            qmp_clear_lfps_rxterm_irq();
        }
    }
}

/// Apply the small, non-calibration portion of the SM7250 USB2 PHY setup.
///
/// The full Linux driver also obtains regulators and a 19.2 MHz reference
/// clock from the board description. Those are already left on by the
/// Pixel boot chain; the register sequence below is the actual PHY setup
/// from the `qcom,usb-hsphy-snps-femto` driver and the Bramble override
/// sequence in its device tree.
unsafe fn init_hsphy() {
    unsafe {
        hsphy_update(
            HSPHY_CFG0,
            HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN,
            HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN,
        );
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_POR, HSPHY_UTMI_POR);
        hsphy_update(HSPHY_COMMON0, HSPHY_COMMON0_FSEL_MASK, 0);
        hsphy_update(
            HSPHY_COMMON1,
            HSPHY_COMMON1_PLLBTUNE,
            HSPHY_COMMON1_PLLBTUNE,
        );
        hsphy_update(HSPHY_REFCLK_CTRL, 0x3, 0x2);
        hsphy_update(
            HSPHY_COMMON1,
            HSPHY_COMMON1_VBUSVLDEXTSEL0,
            HSPHY_COMMON1_VBUSVLDEXTSEL0,
        );
        hsphy_update(
            HSPHY_CTRL1,
            HSPHY_CTRL1_VBUSVLDEXT0,
            HSPHY_CTRL1_VBUSVLDEXT0,
        );

        // qcom,param-override-seq is encoded as (value, register offset).
        for index in 0..3 {
            let (offset, value) = ACTIVE_HSPHY_PARAM_OVERRIDE[index];
            write_volatile(hsphy_reg(offset), value);
        }

        // Bramble does not declare an external-calibration resistor, so the
        // upstream driver enables the internal RTUNE path.
        hsphy_update(HSPHY_RTUNE_SEL, 1, 1);
        hsphy_update(
            HSPHY_COMMON2,
            HSPHY_COMMON2_VREGBYPASS,
            HSPHY_COMMON2_VREGBYPASS,
        );
        // The SNPS Femto driver uses the ATE/test toggle sequence to commit
        // the PHY's analog override values before releasing POR.
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_ATE_RESET, HSPHY_UTMI_ATE_RESET);
        hsphy_update(
            HSPHY_TEST1,
            HSPHY_TEST1_TESTDATAOUTSEL,
            HSPHY_TEST1_TESTDATAOUTSEL,
        );
        hsphy_update(HSPHY_TEST1, HSPHY_TEST1_TOGGLE_2WR, HSPHY_TEST1_TOGGLE_2WR);
        hsphy_update(HSPHY_COMMON0, HSPHY_COMMON0_VATESTENB_MASK, 0);
        hsphy_update(HSPHY_TEST0, HSPHY_TEST0_DATA_MASK, 0);
        hsphy_update(
            HSPHY_CTRL2,
            HSPHY_CTRL2_SUSPEND_N_SEL | HSPHY_CTRL2_SUSPEND_N,
            HSPHY_CTRL2_SUSPEND_N_SEL | HSPHY_CTRL2_SUSPEND_N,
        );
        hsphy_update(HSPHY_UTMI_CTRL0, HSPHY_UTMI_SLEEPM, HSPHY_UTMI_SLEEPM);
        hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_POR, 0);
        hsphy_update(HSPHY_CTRL2, HSPHY_CTRL2_SUSPEND_N_SEL, 0);
        hsphy_update(HSPHY_CFG0, HSPHY_CFG0_CMN_CTRL_OVERRIDE_EN, 0);
    }
}

unsafe fn select_utmi_pipe_clock() {
    // This is the Qualcomm glue sequence used when DWC3 operates without a
    // SuperSpeed PHY. It prevents the absent QMP PIPE clock from holding the
    // core in reset while the USB2 UTMI clock is already running.
    trace_event(TRACE_UTMI_CLOCK, 0, 0, 0, 0, 0);
    unsafe {
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_DIS);
        // dwc3_qcom_select_utmi_clk() uses usleep_range(100, 1000) between
        // each clock-source transition.  A fixed architectural delay keeps
        // the lower bound independent of the boot CPU frequency; a NOP loop
        // could be shorter than 100 us on a fast handset.
        super::timer::delay_us(100);
        qscratch_set(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
        super::timer::delay_us(100);
        let value = read_qscratch(QSCRATCH_GENERAL_CFG) & !PIPE_UTMI_CLK_DIS;
        write_qscratch(QSCRATCH_GENERAL_CFG, value);
    }
    trace_event(TRACE_UTMI_CLOCK, 1, 0, 0, 0, 0);
}

/// Apply the DWC3 post-reset reference-clock calibration from
/// dwc3_msm_update_ref_clk(). The GCC source clock is managed separately by
/// the Bramble platform layer; this only programs the controller's timing
/// registers after a core reset.
unsafe fn update_dwc3_ref_clock() {
    unsafe {
        let guctl = read(GUCTL);
        write(
            GUCTL,
            (guctl & !GUCTL_REFCLKPER_MASK) | GUCTL_REFCLKPER_19_2MHZ,
        );
        if read(GSNPSID) >= DWC3_REVISION_250A {
            let gfladj = read(GFLADJ);
            write(
                GFLADJ,
                (gfladj
                    & !(GFLADJ_REFCLK_FLADJ_MASK
                        | GFLADJ_REFCLK_LPM_SEL
                        | GFLADJ_REFCLK_240MHZ_DECR
                        | GFLADJ_REFCLK_240MHZDECR_PLS1))
                    | GFLADJ_REFCLK_LPM_SEL
                    | GFLADJ_REFCLK_240MHZ_DECR
                    | GFLADJ_REFCLK_240MHZDECR_PLS1
                    | GFLADJ_REFCLK_FLADJ_19_2MHZ,
            );
        }
    }
}

/// Reset the DWC3 core after taking ownership from the bootloader.
///
/// The Qualcomm glue invokes this as part of the DWC3 post-reset path. A
/// `fastboot boot` handoff skips that driver, so leaving the controller in its
/// bootloader device/host state can make endpoint commands retire without
/// ever allowing the peripheral pull-up to become visible.
unsafe fn device_soft_reset() -> bool {
    unsafe {
        trace_event(TRACE_DWC3_RESET_BEGIN, 0, 0, 0, 0, 0);
        trace_event(TRACE_DEVICE_RESET, 0, 0, 0, 0, 0);
        let initial_dctl = read(DCTL);
        // Match Linux's reconnect path: clear stale endpoint/device state
        // without touching the already-running Qualcomm PHY and clock
        // branches. RUN_STOP must be cleared in the same write; preserving
        // Fastboot's RUN_STOP bit can leave the device half-running while
        // CSFTRST is asserted.
        let mut dctl = initial_dctl;
        dctl |= DCTL_CSFTRST;
        dctl &= !DCTL_RUN_STOP;
        write_dctl_safe(dctl);
        let snpsid = read(GSNPSID);
        let ip = snpsid >> 16;
        // DWC_usb31 1.90a+ and DWC_usb32 synchronize CSFTRST through all
        // clocks and need the slower 20-ms polling cadence used by Linux.
        // Bramble's 0x5533 controller follows the ordinary 1-us path.
        let version = if ip == DWC31_IP || ip == DWC32_IP {
            read(VER_NUMBER)
        } else {
            0
        };
        let slow_reset = ip == DWC32_IP || (ip == DWC31_IP && version >= DWC31_REVISION_190A);
        let retries = if slow_reset { 10 } else { 1_000 };
        let mut device_reset_complete = false;
        for _ in 0..retries {
            if read(DCTL) & DCTL_CSFTRST == 0 {
                device_reset_complete = true;
                break;
            }
            if slow_reset {
                super::timer::delay_ms(20);
            } else {
                super::timer::delay_us(1);
            }
        }
        if !device_reset_complete {
            log_puts("usb: DWC3 device reset timeout\n");
            return false;
        }

        // Upstream Linux waits an additional 50 ms only for DWC_usb31 1.80a
        // and earlier before accessing its PHY domain. DWC3/1.90a+ do not
        // require that legacy synchronization delay.
        if ip == DWC31_IP && version <= DWC31_REVISION_180A {
            super::timer::delay_ms(50);
        }
        true
    }
}

/// Mirror Linux's dwc3_gadget_dctl_write_safe(). DCTL's link-state request
/// field is a command, not persistent configuration; carrying a Fastboot
/// request into CSFTRST or Run/Stop can make the next device transition race
/// the controller's link state machine.
#[inline]
unsafe fn write_dctl_safe(value: u32) {
    unsafe { write(DCTL, value & !DCTL_TRGTULST_MASK) };
}

/// Reset the DWC3 core and both PHY-facing domains for a cold platform start.
///
/// This is intentionally separate from `device_soft_reset`: a Fastboot
/// handoff must not reset the PHYs that own the Type-C session.
unsafe fn core_soft_reset(super_speed: bool) -> bool {
    unsafe {
        if !device_soft_reset() {
            return false;
        }

        let mut gctl = read(GCTL);
        gctl |= GCTL_CORESOFTRESET;
        write(GCTL, gctl);

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_PHYSOFTRST;
        write(GUSB2PHYCFG0, usb2);
        if super_speed {
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 |= GUSB3PIPECTL_PHYSOFTRST;
            write(GUSB3PIPECTL0, usb3);
        }

        // The upstream DWC3 core reset uses a 100 ms delay after releasing
        // both PHY resets. The architectural counter is firmware-provided
        // before this early probe, so use it instead of a CPU-dependent loop.
        super::timer::delay_ms(100);

        usb2 = read(GUSB2PHYCFG0) & !GUSB2PHYCFG_PHYSOFTRST;
        write(GUSB2PHYCFG0, usb2);
        if super_speed {
            let mut usb3 = read(GUSB3PIPECTL0);
            usb3 &= !GUSB3PIPECTL_PHYSOFTRST;
            write(GUSB3PIPECTL0, usb3);
        }
        super::timer::delay_ms(1);

        gctl = read(GCTL) & !GCTL_CORESOFTRESET;
        write(GCTL, gctl);
        true
    }
}

/// Stop a controller that was left running by Fastboot before reusing its
/// device-mode endpoint state. A DWC3 gadget must be halted before
/// DEPSTARTCFG/SETEPCONFIG are issued; a handoff cannot assume that the
/// bootloader performed the normal gadget-stop sequence.
unsafe fn stop_running_device() -> bool {
    unsafe { run_stop_device(false) }
}

const DWC3_RUN_STOP_POLL_MS: u64 = 1;
const DWC3_RUN_STOP_TIMEOUT_MS: u64 = 2_000;
// STARTTRANSFER must DMA-fetch the TRB before the command can retire, and on
// this platform that first fetch is far slower than the other endpoint
// commands (which complete from the register path alone). The probe-era
// 5,000-read window expired before the fetch finished, so give the command a
// time-based budget instead: 5000 reads is roughly 0.5-1 ms of MMIO polling;
// 2,000,000 reads bounds the wait at a comfortable fraction of a second
// without ever spinning forever.
const DWC3_EP_COMMAND_TIMEOUT: u32 = 2_000_000;

/// Wait for DWC3's device controller to reach the requested halt state after
/// a Run/Stop write. Linux polls DSTS at 1--2 ms intervals for up to 2,000
/// iterations; a fixed NOP count is too short on a fast boot CPU and can let
/// endpoint commands race the controller's previous Fastboot session.
unsafe fn wait_device_state(want_halted: bool) -> bool {
    unsafe {
        for _ in 0..DWC3_RUN_STOP_TIMEOUT_MS {
            // The DWC3 databook requires software to acknowledge device
            // events while waiting for DEVCTRLHLT during a gadget stop.
            // Linux's soft-disconnect path does this in parallel with the
            // halt poll; a Fastboot-owned stale disconnect/reset event can
            // otherwise keep the controller from completing Run/Stop.
            if want_halted {
                acknowledge_events_while_halting();
            }
            let dsts = read(DSTS);
            let halted = dsts & DSTS_DEVCTRLHLT != 0;
            if halted == want_halted {
                if want_halted {
                    trace_event(TRACE_DWC3_HALTED, 0, 0, 0, 0, dsts);
                }
                return true;
            }
            super::timer::delay_ms(DWC3_RUN_STOP_POLL_MS);
        }
        let dsts = read(DSTS);
        trace_event(TRACE_DWC3_HALT_TIMEOUT, want_halted as u32, 0, 0, 0, dsts);
        log_hex(
            if want_halted {
                "usb: DWC3 stop timeout during handoff, DSTS="
            } else {
                "usb: DWC3 start timeout during handoff, DSTS="
            },
            dsts as u64,
        );
        false
    }
}

/// Acknowledge events generated while DWC3 is draining a device stop.
///
/// This is intentionally separate from `acknowledge_ep0_event_count()`: event
/// buffer setup preserves the complete GEVNTCOUNT register (including EHB),
/// while the Run/Stop halt contract consumes only the byte count and advances
/// the software cursor just as Linux advances `ev_buf->lpos`.
unsafe fn acknowledge_events_while_halting() {
    unsafe {
        let count = read(GEVNTCOUNT0) & GEVNTCOUNT_MASK;
        if count != 0 {
            write(GEVNTCOUNT0, count);
            EVENT_OFFSET = (EVENT_OFFSET + count as usize) % ep0_event_size();
            core::arch::asm!("dsb sy", options(nostack));
        }
    }
}

/// Apply Linux's USB2 PHY guard around a DWC3 Run/Stop transition.
///
/// `dwc3_gadget_run_stop()` clears SUSPHY and ENBLSLPM before writing DCTL,
/// waits for DEVCTRLHLT, and restores the saved bits afterwards. Keeping that
/// sequence in one helper prevents the Fastboot handoff and runtime-PM paths
/// from diverging at exactly the transition where DWC3 is most sensitive to
/// a stale USB2 low-power state.
unsafe fn run_stop_device(is_on: bool) -> bool {
    unsafe {
        let mut usb2 = read(GUSB2PHYCFG0);
        let saved_config = usb2 & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        if saved_config != 0 {
            usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
            write(GUSB2PHYCFG0, usb2);
        }

        let mut dctl = read(DCTL);
        if is_on {
            dctl = run_stop_value(dctl, read(GSNPSID));
        } else {
            dctl &= !DCTL_RUN_STOP;
        }
        write_dctl_safe(dctl);
        let complete = wait_device_state(!is_on);

        if saved_config != 0 {
            let current = read(GUSB2PHYCFG0);
            write(GUSB2PHYCFG0, current | saved_config);
        }
        complete
    }
}

#[inline]
fn gadget_speed_value(mut dcfg: u32, super_speed: bool, snpsid: u32) -> u32 {
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
unsafe fn configure_gadget_speed(super_speed: bool) {
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
unsafe fn enable_gadget_susphy() {
    unsafe {
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_SUSPHY;
        write(GUSB2PHYCFG0, usb2);

        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);
    }
}

#[inline]
fn run_stop_value(mut dctl: u32, snpsid: u32) -> u32 {
    // Lito's DWC3 node supplies snps,hird-threshold = 0x10.  A Fastboot
    // handoff can inherit a different value, so restore the platform value
    // at every device Run/Stop transition.
    dctl = (dctl & !DCTL_HIRD_THRES_MASK) | DCTL_HIRD_THRES_LITO;
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
unsafe fn configure_dwc3_global_control() {
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
        // The lito/bramble vendor DT sets snps,disable-clk-gating, which
        // overrides the generic pwropt-based logic in
        // dwc3_core_setup_global_control(): this platform ALWAYS runs with
        // clock gating disabled. CSFTRST cleared GCTL.RAMCLKSEL; restore the
        // previous owner's select so the internal endpoint RAM keeps its
        // working clock.
        reapply_ramclksel();
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
unsafe fn configure_usb2_phy_interface() {
    unsafe {
        let mut usb2 = read(GUSB2PHYCFG0);
        // Bramble's DWC3 node uses the default UTMI mode. Clear the ULPI
        // selector and choose the Linux UTMI 8-bit timing values; preserve
        // the power-management bits because their policy is handled by the
        // surrounding run/stop guard.
        usb2 &= !(GUSB2PHYCFG_ULPI_UTMI | GUSB2PHYCFG_PHYIF_MASK | GUSB2PHYCFG_USBTRDTIM_MASK);
        usb2 |= GUSB2PHYCFG_USBTRDTIM_UTMI_8_BIT;
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);
    }
}

/// Calculate DWC3.DCFG.NUMP from the receive FIFO capacity.
///
/// Linux derives this from the RAM2 depth and internal memory-bus width. Use
/// saturating arithmetic here because an uninitialised or cut-down hardware
/// parameter must not wrap the subtraction and accidentally request NUMP=16.
#[inline]
fn gadget_nump(ram2_depth: u32, mdwidth_bits: u32) -> u32 {
    let fifo_bytes = (ram2_depth as u64).saturating_mul(mdwidth_bits as u64) / 8;
    (fifo_bytes.saturating_sub(24 + 16) / 1024).min(16) as u32
}

/// Apply the non-endpoint defaults from Linux's `__dwc3_gadget_start()`.
///
/// This is deliberately separate from EP0 setup: it only programs controller
/// receive-packet policy and DCFG.NUMP, and is called after DCFG's speed/address
/// fields have been established but before any endpoint command is issued.
#[inline]
unsafe fn configure_gadget_start_defaults() {
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
        dcfg |= DCFG_IGNSTRMPP;
        write(DCFG, dcfg);
    }
}

#[inline]
unsafe fn read(offset: usize) -> u32 {
    unsafe { read_volatile(reg(offset)) }
}

#[inline]
unsafe fn write(offset: usize, value: u32) {
    unsafe { write_volatile(reg(offset), value) }
}

#[inline]
unsafe fn qscratch_set(offset: usize, mask: u32) {
    trace_event(TRACE_QSCRATCH_BEGIN, offset as u32, mask, 0, 0, 0);
    let value = unsafe { read_qscratch(offset) } | mask;
    unsafe { write_qscratch(offset, value) };
    // The QCOM glue driver performs a readback to make the peripheral-mode
    // session vote visible before it starts the DWC3 core.
    let _ = unsafe { read_qscratch(offset) };
}

#[inline]
unsafe fn dep_reg(endpoint: usize, offset: usize) -> usize {
    DEP_BASE + endpoint * 0x10 + offset
}

#[inline]
fn gsi_transfer_params(event_buffer: u32, trb: usize) -> Option<(u32, u32)> {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if event_buffer == 0 || event_buffer > count || trb & 0x3f != 0 {
        return None;
    }
    Some((
        GSI_TRB_ADDR_BIT_53 | GSI_TRB_ADDR_BIT_55 | (event_buffer << GSI_EVENT_ADDR_INDEX_SHIFT),
        trb as u32,
    ))
}

/// Set up the Qualcomm GSI event-buffer ABI before any GSI endpoint can be
/// started. Android allocates three additional event buffers and marks them
/// with both the GSI enable/index bits in GEVNTADRHI and the interrupt-mask
/// bit in GEVNTCOUNT. EP0 continues to use event buffer zero.
unsafe fn configure_gsi_event_buffers() -> bool {
    let resources = super::platform::bramble::usb_resources();
    let gsi = resources.gsi;
    unsafe {
        let mut general = read_qscratch(gsi.general_cfg_offset);
        general |= GSI_CLK_EN;
        write_qscratch(gsi.general_cfg_offset, general);
        general |= GSI_RESTART_DBL_PNTR;
        write_qscratch(gsi.general_cfg_offset, general);
        general &= !GSI_RESTART_DBL_PNTR;
        write_qscratch(gsi.general_cfg_offset, general);
        if read_qscratch(gsi.general_cfg_offset) & GSI_CLK_EN == 0 {
            return false;
        }

        for index in 0..gsi.event_buffer_count as usize {
            let event = addr_of_mut!(GSI_EVENTS).cast::<EventBuffer>().add(index);
            let event_address = event as usize as u64;
            cache_clean(event as usize, EVENT_BUFFER_SIZE);
            let register = GEVNTADRLO0 + (index + 1) * GEVNT_BUFFER_STRIDE;
            write(register, event_address as u32);
            write(
                register + 4,
                (event_address >> 32) as u32
                    | (((index + 1) as u32) << GSI_EVENT_ADDR_EN_SHIFT)
                    | (((index + 1) as u32) << GSI_EVENT_ADDR_INDEX_SHIFT),
            );
            write(register + 8, EVENT_BUFFER_SIZE as u32);
            write(register + 12, GSI_EVENT_INTR_MASK);
        }
    }
    true
}

/// Enable the GSI wrapper at the point Android starts a GSI endpoint. Keeping
/// this separate from event-buffer allocation avoids asserting GSI_EN for a
/// normal gadget that has no IPA/GSI channel.
unsafe fn enable_gsi_wrapper() -> bool {
    let offset = super::platform::bramble::usb_resources()
        .gsi
        .general_cfg_offset;
    unsafe {
        let mut value = read_qscratch(offset);
        value |= GSI_CLK_EN;
        write_qscratch(offset, value);
        value |= GSI_EN;
        write_qscratch(offset, value);
        read_qscratch(offset) & GSI_EN != 0
    }
}

const GSI_MAX_RING_TRBS: usize = 10;

/// Build the circular DWC3 TRB ring consumed by Qualcomm's GSI wrapper. The
/// ring is caller-owned DMA memory, while buffer addresses are the contiguous
/// request pool supplied by the IPA/GSI client. This mirrors Android's
/// `gsi_prepare_trbs()` split between ring allocation and buffer storage.
unsafe fn prepare_gsi_ring(
    event_index: usize,
    endpoint: usize,
    ring_base: u64,
    buffer_base: usize,
    buffer_length: usize,
) -> bool {
    let in_direction = endpoint & 1 != 0;
    let Some(shape) = gsi_ring_shape(in_direction, GSI_DEFAULT_NUM_BUFFERS) else {
        return false;
    };
    let pool = super::platform::bramble::usb_resources().dma_pool;
    let ring_bytes = shape.num_trbs.saturating_mul(core::mem::size_of::<Trb>());
    let buffer_bytes = (shape.data_trbs as u64).saturating_mul(buffer_length as u64);
    if shape.num_trbs > GSI_MAX_RING_TRBS
        || !super::platform::bramble::dma_region_valid(pool, ring_base, ring_bytes as u64, 0x400)
        || !super::platform::bramble::dma_region_valid(pool, buffer_base as u64, buffer_bytes, 64)
        || buffer_length == 0
    {
        return false;
    }

    unsafe {
        let ring = ring_base as usize as *mut Trb;
        for index in 0..shape.num_trbs {
            let mut trb = Trb::default();
            if index == shape.num_trbs - 1 {
                // The GSI wrapper uses the same address[55:53] and
                // interrupter-index encoding as STARTTRANSFER.
                trb.bpl = ring_base as u32;
                trb.bph = (ring_base >> 32) as u32
                    | GSI_TRB_ADDR_BIT_53
                    | GSI_TRB_ADDR_BIT_55
                    | ((event_index as u32 + 1) << GSI_EVENT_ADDR_INDEX_SHIFT);
                trb.ctrl = TRB_LINK | TRB_HWO;
            } else if in_direction {
                // The first n+1 entries are deliberate zero-length normal
                // TRBs (ZLPs); the following n entries point at the
                // contiguous buffer pool. Android leaves HWO clear here and
                // lets the GSI path own the buffer progression.
                if index >= shape.first_buffer_trb {
                    let buffer_index = index - shape.first_buffer_trb;
                    let address = buffer_base
                        .saturating_add(buffer_index.saturating_mul(buffer_length))
                        as u64;
                    trb.bpl = address as u32;
                    trb.bph = (address >> 32) as u32;
                }
                trb.ctrl = TRB_NORMAL | TRB_IOC;
            } else if index == 0 {
                // The Bramble Android OUT ring starts with a link to the
                // second TRB, then closes with another link TRB.
                let next = ring_base + core::mem::size_of::<Trb>() as u64;
                trb.bpl = next as u32;
                trb.bph = (next >> 32) as u32;
                trb.ctrl = TRB_LINK;
            } else {
                let buffer_index = index - 1;
                let address =
                    buffer_base.saturating_add(buffer_index.saturating_mul(buffer_length)) as u64;
                trb.bpl = address as u32;
                trb.bph = (address >> 32) as u32;
                trb.size = buffer_length as u32;
                // OUT HWO is set by UPDATETRANSFER, matching Android's
                // lifecycle. Preparing a ring must not make it live early.
                trb.ctrl = TRB_NORMAL | TRB_IOC | TRB_CSP | TRB_ISP_IMI;
            }
            write_volatile(ring.add(index), trb);
        }
        cache_clean(
            ring_base as usize,
            shape.num_trbs * core::mem::size_of::<Trb>(),
        );
    }
    true
}

/// Publish the ring and doorbell addresses consumed by the IPA/GSI channel
/// setup, and prepare the complete circular TRB layout. Android does this
/// after endpoint configuration and before starting the channel; a normal
/// UDC endpoint therefore never writes to an unowned doorbell by accident.
pub unsafe fn configure_gsi_channel(
    endpoint: usize,
    event_buffer: u32,
    ring_base: u64,
    doorbell: u64,
) -> bool {
    // Do not retain the old incomplete ABI as a fake successful setup.
    // A GSI channel is meaningful only when the caller supplies the actual
    // contiguous request pool consumed by gsi_prepare_trbs().
    let _ = (endpoint, event_buffer, ring_base, doorbell);
    false
}

/// Configure one Qualcomm GSI channel with its complete DMA ownership.
/// `buffer_base..buffer_base + 4 * buffer_length` is the contiguous request
/// pool corresponding to Android's `gsi_prepare_trbs()` layout.  Both the
/// TRB ring and that pool must be in the DT-declared Apps-SMMU IOVA window.
pub unsafe fn configure_gsi_channel_with_buffers(
    endpoint: usize,
    event_buffer: u32,
    ring_base: u64,
    doorbell: u64,
    buffer_base: u64,
    buffer_length: usize,
) -> bool {
    let resources = super::platform::bramble::usb_resources();
    let count = resources.gsi.event_buffer_count.min(3);
    if endpoint < 2
        || event_buffer == 0
        || event_buffer > count
        || ring_base == 0
        || ring_base & 0x3ff != 0
        || doorbell == 0
        || doorbell & 0x3 != 0
        || doorbell >> 32 != 0
        || buffer_base > usize::MAX as u64
        || buffer_length == 0
    {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !prepare_gsi_ring(
            index,
            endpoint,
            ring_base,
            buffer_base as usize,
            buffer_length,
        ) {
            return false;
        }
        write_qscratch(
            resources.gsi.ring_base_low_offset + index * 4,
            ring_base as u32,
        );
        write_qscratch(
            resources.gsi.ring_base_high_offset + index * 4,
            (ring_base >> 32) as u32,
        );
        write_qscratch(
            resources.gsi.doorbell_low_offset + index * 4,
            doorbell as u32,
        );
        write_qscratch(
            resources.gsi.doorbell_high_offset + index * 4,
            (doorbell >> 32) as u32,
        );
        GSI_CHANNEL_ENDPOINT[index] = endpoint;
        GSI_CHANNEL_READY[index] = true;
        GSI_RING_BASES[index] = ring_base;
        GSI_RING_TRB_COUNTS[index] = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS)
            .map(|shape| shape.num_trbs)
            .unwrap_or(0);
        GSI_BUFFER_BASES[index] = buffer_base;
        GSI_BUFFER_LENGTHS[index] = buffer_length;
        GSI_DOORBELL_BASES[index] = doorbell;
        GSI_RESOURCE_INDEX[index] = 0;
        GSI_RING_ACTIVE[index] = false;
    }
    true
}

/// Allocate and configure a complete GSI channel from the active USB DMA
/// pool. This is the path used by a real gadget client; callers no longer
/// need to invent physical addresses for the ring or request buffers.
pub unsafe fn allocate_gsi_channel(
    endpoint: usize,
    event_buffer: u32,
    doorbell: u64,
    buffer_length: usize,
) -> Option<(*mut u8, *mut u8)> {
    let shape = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS)?;
    let ring_bytes = shape.num_trbs.checked_mul(core::mem::size_of::<Trb>())?;
    let buffer_bytes = shape.data_trbs.checked_mul(buffer_length)?;
    let ring = unsafe { allocate_usb_dma(ring_bytes, 0x400)? };
    let buffers = unsafe { allocate_usb_dma(buffer_bytes, 64)? };
    if unsafe {
        !configure_gsi_channel_with_buffers(
            endpoint,
            event_buffer,
            ring as usize as u64,
            doorbell,
            buffers as usize as u64,
            buffer_length,
        )
    } {
        return None;
    }
    Some((ring, buffers))
}

/// Ring the physical doorbell supplied by the IPA/GSI client. The Android
/// glue writes the address of the ring's final link TRB as two 32-bit MMIO
/// stores; it does not ring the DWC3 QSCRATCH register itself.
unsafe fn ring_gsi_doorbell(index: usize) -> bool {
    if index >= 3 {
        return false;
    }
    let doorbell = unsafe { GSI_DOORBELL_BASES[index] };
    let ring = unsafe { GSI_RING_BASES[index] };
    let count = unsafe { GSI_RING_TRB_COUNTS[index] };
    if doorbell == 0 || ring == 0 || count == 0 {
        return false;
    }
    let Some(link_offset) = (count - 1).checked_mul(core::mem::size_of::<Trb>()) else {
        return false;
    };
    let Some(link) = ring.checked_add(link_offset as u64) else {
        return false;
    };
    if !super::platform::bramble::dma_region_valid(
        super::platform::bramble::usb_resources().dma_pool,
        link,
        core::mem::size_of::<Trb>() as u64,
        64,
    ) {
        return false;
    }
    unsafe {
        // DWC3's GSI link TRB carries the interrupter/address-extension bits,
        // but the IPA doorbell receives the plain DMA address of that TRB.
        let db = doorbell as usize as *mut u32;
        let db_hi = doorbell.saturating_add(4) as usize as *mut u32;
        core::ptr::write_volatile(db, link as u32);
        let _ = core::ptr::read_volatile(db);
        core::ptr::write_volatile(db_hi, (link >> 32) as u32);
        let _ = core::ptr::read_volatile(db_hi);
    }
    true
}

/// Block or release the GSI write doorbell. Qualcomm runtime suspend blocks
/// writes, waits for IF_STS to idle, then halts DWC3 and drops the platform
/// vote in that order.
pub unsafe fn set_gsi_doorbell_blocked(blocked: bool) -> bool {
    let offset = super::platform::bramble::usb_resources()
        .gsi
        .general_cfg_offset;
    unsafe {
        let mut value = read_qscratch(offset);
        if blocked {
            value |= GSI_BLOCK_WR_GO;
        } else {
            value &= !GSI_BLOCK_WR_GO;
        }
        write_qscratch(offset, value);
        (read_qscratch(offset) & GSI_BLOCK_WR_GO != 0) == blocked
    }
}

unsafe fn gsi_ready_to_suspend() -> bool {
    let offset = super::platform::bramble::usb_resources()
        .gsi
        .interface_status_offset;
    unsafe {
        for _ in 0..1500 {
            if read_qscratch(offset) & GSI_WR_CTRL_STATE == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

/// True when the address lives in the Device-mapped 2 MiB block that holds
/// the .usb_dma/.usb_trace sections: the CPU accesses it uncached, so the
/// DC maintenance is unnecessary (and constrained-unpredictable on Device
/// memory) — skip it.
#[inline]
fn in_uncached_dma_window(address: usize) -> bool {
    let block_base = (addr_of!(__usb_dma_start) as usize) & !0x1_fFFF;
    let block_top = block_base + 0x20_0000;
    address >= block_base && address < block_top
}

unsafe fn cache_clean(address: usize, length: usize) {
    // DWC3 and the Apps SMMU consume these objects by DMA.  The probe may be
    // entered with the bootloader's caches enabled, so a no-op here would
    // leave the freshly written TRB/page table only in the CPU cache.
    if !super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    if in_uncached_dma_window(address) {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc cvac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

unsafe fn cache_invalidate(address: usize, length: usize) {
    if !super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    if in_uncached_dma_window(address) {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc ivac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

unsafe fn send_ep_command_result(
    endpoint: usize,
    command: u32,
    param0: u32,
    param1: u32,
    param2: u32,
) -> Option<u8> {
    trace_event(
        TRACE_EP_COMMAND_ISSUE,
        command,
        endpoint as u32,
        param0,
        param1,
        param2,
    );
    let mut saved_usb2_config = 0;
    unsafe {
        // The DWC3 programming guide requires SUSPENDUSB2 and ENBLSLPM to be
        // clear while issuing endpoint commands at USB2 speeds. Linux does
        // this in dwc3_send_gadget_ep_cmd(); a Fastboot handoff commonly
        // leaves one or both bits set after tearing down its gadget.
        let command_kind = command & 0x0f;
        if command_kind == DEPCMD_ENDTRANSFER
            || read(DSTS) & DSTS_CONNECTSPD_MASK != DSTS_SUPERSPEED
        {
            let mut usb2 = read(GUSB2PHYCFG0);
            saved_usb2_config = usb2 & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
            if saved_usb2_config != 0 {
                usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
                write(GUSB2PHYCFG0, usb2);
                let _ = read(GUSB2PHYCFG0);
            }
        }
        // The DWC3 register names are counter-intuitive: PAR2 is at +0x00,
        // PAR1 at +0x04, and PAR0 at +0x08. Keep both the software argument
        // order and the MMIO write order identical to Linux's
        // dwc3_send_gadget_ep_cmd().
        write(dep_reg(endpoint, 0x08), param0);
        write(dep_reg(endpoint, 0x04), param1);
        write(dep_reg(endpoint, 0x00), param2);
        // Linux's writel() provides the MMIO ordering barrier that separates
        // the parameter writes from the command latch. Preserve that ordering
        // explicitly in this freestanding Rust path.
        core::arch::asm!("dsb sy", options(nostack));
        write(dep_reg(endpoint, 0x0c), command | DEPCMD_CMDACT);
    }
    // Linux's dwc3_send_gadget_ep_cmd() uses a bounded 5,000-read polling
    // window. Keep this tight: a command that never retires must not leave
    // the early handoff spending an architecture-dependent amount of time in
    // a NOP loop while the host waits for EP0.
    for _ in 0..DWC3_EP_COMMAND_TIMEOUT {
        let status = unsafe { read(dep_reg(endpoint, 0x0c)) };
        if status & DEPCMD_CMDACT == 0 {
            trace_event(
                TRACE_EP_COMMAND_DONE,
                command,
                endpoint as u32,
                status,
                0,
                unsafe { read(DSTS) },
            );
            let success = status & 0xf000 == 0;
            let resource_index = ((status >> DEPCMD_PARAM_SHIFT) & 0x7f) as u8;
            if saved_usb2_config != 0 {
                unsafe {
                    let usb2 = read(GUSB2PHYCFG0);
                    write(GUSB2PHYCFG0, usb2 | saved_usb2_config);
                }
            }
            return success.then_some(resource_index);
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    trace_event(
        TRACE_EP_COMMAND_TIMEOUT,
        command,
        endpoint as u32,
        unsafe { read(dep_reg(endpoint, 0x0c)) },
        0,
        unsafe { read(DSTS) },
    );
    if saved_usb2_config != 0 {
        unsafe {
            let usb2 = read(GUSB2PHYCFG0);
            write(GUSB2PHYCFG0, usb2 | saved_usb2_config);
        }
    }
    log_puts("usb: DWC3 endpoint command timeout\n");
    None
}

#[inline]
unsafe fn send_ep_command(
    endpoint: usize,
    command: u32,
    param0: u32,
    param1: u32,
    param2: u32,
) -> bool {
    unsafe { send_ep_command_result(endpoint, command, param0, param1, param2).is_some() }
}

unsafe fn configure_endpoint(endpoint: usize, max_packet: u32, modify: bool) -> bool {
    unsafe { configure_endpoint_kind(endpoint, max_packet, DEPCFG_EP_TYPE_CONTROL, modify) }
}

unsafe fn configure_endpoint_kind(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
) -> bool {
    unsafe {
        configure_endpoint_kind_with_interrupter(endpoint, max_packet, endpoint_type, modify, 0)
    }
}

unsafe fn configure_endpoint_kind_with_interrupter(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
    interrupter: u32,
) -> bool {
    if !unsafe {
        configure_endpoint_config(endpoint, max_packet, endpoint_type, modify, interrupter)
    } {
        return false;
    }
    // Linux allocates a transfer resource immediately after configuring each
    // endpoint. DEPSTARTCFG only resets the allocation window; issuing
    // SETTRANSFRESOURCE for every possible endpoint is not equivalent and can
    // make the handoff fail before the first pull-up.
    if !modify
        && !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
        && !cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
    {
        return unsafe { send_ep_command(endpoint, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0) };
    }
    true
}

unsafe fn configure_endpoint_config(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
    interrupter: u32,
) -> bool {
    let action = if modify { DEPCMD_ACTION_MODIFY } else { 0 };
    let param0 = action | endpoint_type | (max_packet << DEPCFG_MAX_PACKET_SHIFT);
    let param1 = DEPCFG_XFER_COMPLETE_EN
        | DEPCFG_XFER_NOT_READY_EN
        | ((interrupter & 0x1f) << DEPCFG_INT_NUM_SHIFT)
        | ((endpoint as u32) << DEPCFG_EP_NUMBER_SHIFT);
    unsafe { send_ep_command(endpoint, DEPCMD_SETEPCONFIG, param0, param1, 0) }
}

unsafe fn start_transfer(endpoint: usize, trb: *const Trb) -> bool {
    let address = unsafe { dma_iova_for(trb as usize) };
    unsafe {
        // DWC3's STARTTRANSFER parameters are PAR0=address[63:32] and
        // PAR1=address[31:0]. The endpoint command helper writes the named
        // param0/param1 fields to those registers respectively.
        let Some(resource_index) = send_ep_command_result(
            endpoint,
            DEPCMD_STARTTRANSFER,
            (address >> 32) as u32,
            address as u32,
            0,
        ) else {
            return false;
        };
        if endpoint < 2 {
            EP0_RESOURCE_INDEX[endpoint] = resource_index;
        } else if endpoint < 4 {
            DATA_RESOURCE_INDEX[endpoint - 2] = resource_index;
        }
        true
    }
}

unsafe fn end_transfer(endpoint: usize) -> bool {
    let resource_index = if endpoint < 2 {
        let index = unsafe { EP0_RESOURCE_INDEX[endpoint] };
        if index == 0 { 1 } else { index }
    } else if endpoint < 4 {
        let index = unsafe { DATA_RESOURCE_INDEX[endpoint - 2] };
        if index == 0 { 1 } else { index }
    } else {
        1
    };
    unsafe {
        send_ep_command(
            endpoint,
            DEPCMD_ENDTRANSFER
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        )
    }
}

/// Revoke every ordinary UDC data transfer before endpoint state or request
/// ownership is reset. EP0 is handled by the control-reset path separately.
unsafe fn teardown_data_endpoints() {
    unsafe {
        if !DATA_ENDPOINTS_READY {
            return;
        }
        for endpoint in 2..=3 {
            if DATA_RESOURCE_INDEX[endpoint - 2] != 0 {
                let _ = end_transfer(endpoint);
            }
        }
        write(DALEPENA, read(DALEPENA) & !((1 << 2) | (1 << 3)));
        let _ = udc_mut().disable_endpoint(0x02);
        let _ = udc_mut().disable_endpoint(0x83);
        DATA_ENDPOINTS_READY = false;
        DATA_RESOURCE_INDEX = [0; 2];
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
    }
}

/// Cancel outstanding ordinary requests at the runtime-PM boundary while
/// retaining endpoint configuration for resume. DWC3 must no longer own a
/// TRB when the UDC is marked suspended.
unsafe fn suspend_data_transfers() {
    unsafe {
        if !DATA_ENDPOINTS_READY {
            return;
        }
        for endpoint in 2..=3 {
            let index = endpoint - 2;
            if DATA_RESOURCE_INDEX[index] != 0 {
                let _ = end_transfer(endpoint);
            }
            let address = if endpoint == 3 { 0x83 } else { 0x02 };
            let slot = DATA_REQUEST_SLOTS[index];
            if slot != usize::MAX {
                let length = udc_mut()
                    .request(address, slot)
                    .map(|request| request.length)
                    .unwrap_or(0);
                let _ = udc_mut().complete(address, slot, 0, true);
                GadgetDriver::on_data_complete(gadget_mut(), address, 0, true);
                let _ = udc_mut().release(address, slot);
                trace_event(TRACE_TRANSFER_COMPLETE, endpoint as u32, 0, 0, length, 1);
            }
            DATA_RESOURCE_INDEX[index] = 0;
            DATA_REQUEST_SLOTS[index] = usize::MAX;
        }
    }
}

/// Cancel live GSI requests without discarding their registered rings or
/// client doorbells. The function receives an explicit suspend callback and
/// can requeue after resume; no request is silently left owned by DWC3.
unsafe fn suspend_gsi_transfers() {
    unsafe {
        for index in 0..3 {
            if !GSI_CHANNEL_READY[index] {
                continue;
            }
            let endpoint = GSI_CHANNEL_ENDPOINT[index];
            let event_buffer = (index + 1) as u32;
            if GSI_RING_ACTIVE[index] {
                let _ = end_gsi_transfer(endpoint, event_buffer);
            }
            let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
            let slot = GSI_REQUEST_SLOTS[index];
            if slot != usize::MAX {
                GadgetDriver::on_gsi_data_complete(gadget_mut(), address, 0, true);
                let _ = udc_mut().release(address, slot);
            }
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
            GSI_RING_ACTIVE[index] = false;
            GSI_RESOURCE_INDEX[index] = 0;
        }
        if GSI_GADGET_BOUND {
            GadgetDriver::on_gsi_channel_suspend(gadget_mut());
        }
    }
}

/// Start a non-control transfer through Qualcomm's GSI event-buffer path.
/// event_buffer is the Android DWC3 interrupt/event-buffer index (1..=3);
/// EP0 must continue to use start_transfer and index zero.
unsafe fn start_gsi_transfer(endpoint: usize, event_buffer: u32, trb: *const Trb) -> Option<u8> {
    let Some((param0, param1)) = gsi_transfer_params(event_buffer, trb as usize) else {
        return None;
    };
    unsafe {
        if !enable_gsi_wrapper() {
            return None;
        }
        send_ep_command_result(endpoint, DEPCMD_STARTTRANSFER, param0, param1, 0)
    }
}

/// Set ownership on the OUT data TRBs and notify DWC3 of the GSI resource.
/// Android intentionally separates ring preparation from this step so a
/// channel can be armed only after its buffers and doorbell are ready.
pub unsafe fn update_gsi_transfer(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index]
            || GSI_CHANNEL_ENDPOINT[index] != endpoint
            || GSI_RING_BASES[index] == 0
            || GSI_RING_ACTIVE[index]
            || endpoint & 1 != 0
        {
            return false;
        }
        let Some(shape) = gsi_ring_shape(false, GSI_DEFAULT_NUM_BUFFERS) else {
            return false;
        };
        let ring = GSI_RING_BASES[index] as usize as *mut Trb;
        for trb_index in shape.first_buffer_trb..shape.first_buffer_trb + shape.data_trbs {
            let mut ctrl = read_volatile(addr_of!((*ring.add(trb_index)).ctrl));
            ctrl |= TRB_HWO;
            write_volatile(addr_of_mut!((*ring.add(trb_index)).ctrl), ctrl);
        }
        cache_clean(ring as usize, shape.num_trbs * core::mem::size_of::<Trb>());
        let resource_index = GSI_RESOURCE_INDEX[index];
        if resource_index == 0
            || !send_ep_command(
                endpoint,
                DEPCMD_UPDATETRANSFER | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            )
        {
            return false;
        }
        GSI_RING_ACTIVE[index] = true;
    }
    true
}

/// Stop a live GSI transfer before changing its ring or runtime-power state.
pub unsafe fn end_gsi_transfer(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index] || GSI_CHANNEL_ENDPOINT[index] != endpoint {
            return false;
        }
        let resource_index = GSI_RESOURCE_INDEX[index];
        if resource_index == 0 {
            return false;
        }
        let stopped = send_ep_command(
            endpoint,
            DEPCMD_ENDTRANSFER
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        );
        if stopped {
            GSI_RING_ACTIVE[index] = false;
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
        }
        stopped
    }
}

/// Configure a non-control bulk endpoint for the Qualcomm GSI event path.
/// This is intentionally opt-in: the normal UDC data path uses event buffer
/// zero and must not assert the global GSI enable bit merely because event
/// buffers are available.
pub unsafe fn enable_gsi_data_endpoint(
    endpoint: usize,
    event_buffer: u32,
    max_packet: u32,
) -> bool {
    let event_buffer_count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if endpoint < 2
        || endpoint >= 8
        || event_buffer == 0
        || event_buffer > event_buffer_count
        || max_packet == 0
    {
        return false;
    }
    let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
    unsafe {
        if !configure_endpoint_kind_with_interrupter(
            endpoint,
            max_packet,
            DEPCFG_EP_TYPE_BULK,
            false,
            event_buffer,
        ) {
            return false;
        }
        if !udc_mut().configure_endpoint(endpoint_address, max_packet as u16, true) {
            return false;
        }
        write(DALEPENA, read(DALEPENA) | (1 << endpoint));
    }
    true
}

/// Bind a complete GSI data endpoint in the same order as the Android client:
/// configure the DWC3 endpoint, allocate the ring/request pool, publish the
/// client doorbell, then enable the wrapper. A caller receives the owned
/// request-pool pointers and can pass the first one to `queue_gsi_transfer`.
pub unsafe fn configure_gsi_data_endpoint(
    endpoint: usize,
    event_buffer: u32,
    max_packet: u32,
    doorbell: u64,
    buffer_length: usize,
) -> Option<(*mut u8, *mut u8)> {
    if !unsafe { enable_gsi_data_endpoint(endpoint, event_buffer, max_packet) } {
        return None;
    }
    let allocation =
        unsafe { allocate_gsi_channel(endpoint, event_buffer, doorbell, buffer_length) };
    if allocation.is_none() {
        let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
        unsafe {
            let _ = udc_mut().disable_endpoint(address);
            write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
        }
        return None;
    }
    if !unsafe { enable_gsi_wrapper() } {
        unsafe {
            let _ = disable_gsi_data_endpoint(endpoint, event_buffer);
        }
        return None;
    }
    allocation
}

/// Tear down one GSI endpoint after its request has completed or been
/// cancelled. ENDTRANSFER precedes UDC removal, and the global wrapper is
/// disabled only once no channel remains published.
pub unsafe fn disable_gsi_data_endpoint(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index] || GSI_CHANNEL_ENDPOINT[index] != endpoint {
            return false;
        }
        if GSI_RING_ACTIVE[index] && !end_gsi_transfer(endpoint, event_buffer) {
            return false;
        }
        let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
        let _ = udc_mut().disable_endpoint(address);
        write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
        GSI_PENDING[index] = false;
        GSI_REQUEST_SLOTS[index] = usize::MAX;
        GSI_RING_ACTIVE[index] = false;
        GSI_RESOURCE_INDEX[index] = 0;
        GSI_CHANNEL_READY[index] = false;
        GSI_CHANNEL_ENDPOINT[index] = 0;

        let no_channels = !GSI_CHANNEL_READY[0] && !GSI_CHANNEL_READY[1] && !GSI_CHANNEL_READY[2];
        if no_channels {
            let offset = super::platform::bramble::usb_resources()
                .gsi
                .general_cfg_offset;
            let value = read_qscratch(offset) & !GSI_EN;
            write_qscratch(offset, value);
        }
    }
    true
}

/// Queue one DMA request on a previously configured GSI data endpoint. The
/// supplied buffer is treated as the beginning of the contiguous four-buffer
/// pool expected by Android's GSI ABI; callers must provide space for all
/// four `length`-sized buffers and must not reuse it until completion.
pub unsafe fn queue_gsi_transfer(
    endpoint: usize,
    event_buffer: u32,
    buffer: *const u8,
    length: usize,
) -> bool {
    let event_buffer_count = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if endpoint < 2
        || endpoint >= 8
        || event_buffer == 0
        || event_buffer > event_buffer_count
        || length == 0
    {
        return false;
    }
    let trb_index = (event_buffer - 1) as usize;
    let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
    unsafe {
        if GSI_CHANNEL_ENDPOINT[trb_index] != endpoint {
            return false;
        }
        if !GSI_CHANNEL_READY[trb_index] {
            return false;
        }
        if GSI_PENDING[trb_index] {
            return false;
        }
        let Some(shape) = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS) else {
            return false;
        };
        let total_buffer_bytes = (shape.data_trbs as u64).saturating_mul(length as u64);
        let pool = super::platform::bramble::usb_resources().dma_pool;
        if buffer as usize as u64 != GSI_BUFFER_BASES[trb_index]
            || length != GSI_BUFFER_LENGTHS[trb_index]
            || !super::platform::bramble::dma_region_valid(
                pool,
                buffer as usize as u64,
                total_buffer_bytes,
                64,
            )
        {
            return false;
        }
        let Some(request_slot) = udc_mut().queue(endpoint_address, length as u32) else {
            return false;
        };
        if !udc_mut().start(endpoint_address, request_slot) {
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        }
        let ring_base = GSI_RING_BASES[trb_index];
        if !prepare_gsi_ring(trb_index, endpoint, ring_base, buffer as usize, length) {
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        }
        GSI_PENDING[trb_index] = true;
        GSI_REQUEST_SLOTS[trb_index] = request_slot;
        let Some(resource_index) =
            start_gsi_transfer(endpoint, event_buffer, ring_base as usize as *const Trb)
        else {
            GSI_PENDING[trb_index] = false;
            GSI_REQUEST_SLOTS[trb_index] = usize::MAX;
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        };
        GSI_RESOURCE_INDEX[trb_index] = resource_index;
        let transfer_updated = endpoint & 1 != 0 || update_gsi_transfer(endpoint, event_buffer);
        if transfer_updated && ring_gsi_doorbell(trb_index) {
            GSI_RING_ACTIVE[trb_index] = true;
            true
        } else {
            GSI_PENDING[trb_index] = false;
            GSI_REQUEST_SLOTS[trb_index] = usize::MAX;
            let _ = end_gsi_transfer(endpoint, event_buffer);
            let _ = udc_mut().release(endpoint_address, request_slot);
            false
        }
    }
}

/// Queue an ordinary gadget bulk request on the function's EP2 OUT or EP3
/// IN endpoint. GSI is an Android IPA optimization; Linux's normal UDC path
/// still uses DWC3's event buffer zero and must remain usable independently.
pub unsafe fn queue_bulk_transfer(endpoint: usize, buffer: *const u8, length: usize) -> bool {
    if !DATA_ENDPOINTS_READY || (endpoint != 2 && endpoint != 3) || length == 0 {
        return false;
    }
    let index = endpoint - 2;
    unsafe {
        if DATA_REQUEST_SLOTS[index] != usize::MAX {
            return false;
        }
        let address = if endpoint == 3 { 0x83 } else { 0x02 };
        let pool = super::platform::bramble::usb_resources().dma_pool;
        if !super::platform::bramble::dma_region_valid(
            pool,
            buffer as usize as u64,
            length as u64,
            64,
        ) {
            return false;
        }
        let Some(slot) = udc_mut().queue(address, length as u32) else {
            return false;
        };
        if !udc_mut().start(address, slot) {
            let _ = udc_mut().release(address, slot);
            return false;
        }
        let trb = addr_of_mut!(DATA_TRBS).cast::<Trb>().add(index);
        prepare_trb_at(trb, buffer, length, TRB_NORMAL);
        DATA_REQUEST_SLOTS[index] = slot;
        if start_transfer(endpoint, trb) {
            true
        } else {
            DATA_REQUEST_SLOTS[index] = usize::MAX;
            DATA_RESOURCE_INDEX[index] = 0;
            let _ = udc_mut().release(address, slot);
            false
        }
    }
}

unsafe fn prepare_trb(index: usize, buffer: *const u8, length: usize, kind: u32) {
    let address = unsafe { dma_iova_for(buffer as usize) };
    let trb = unsafe { ep0_trb_ptr(index) };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
        write_volatile(
            addr_of_mut!((*trb).ctrl),
            kind | TRB_HWO | TRB_LST | TRB_IOC | TRB_ISP_IMI,
        );
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

unsafe fn prepare_trb_at(trb: *mut Trb, buffer: *const u8, length: usize, kind: u32) {
    let address = unsafe { dma_iova_for(buffer as usize) };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
        write_volatile(
            addr_of_mut!((*trb).ctrl),
            kind | TRB_HWO | TRB_LST | TRB_IOC | TRB_ISP_IMI,
        );
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

unsafe fn start_setup() -> bool {
    trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, unsafe { read(DSTS) });
    unsafe {
        prepare_trb(0, ep0_setup_ptr(), 8, TRB_CONTROL_SETUP);
        let armed = start_transfer(0, ep0_trb_ptr(0));
        EP0_SETUP_ARMED = armed;
        armed
    }
}

/// Best-effort SETUP arming for the poll-loop guard. Unlike `rearm_setup()`
/// this never tears the endpoint down on failure: the core rejects Start
/// Transfer while the link is not ON, and the guard simply retries on the
/// next poll until the link comes up.
unsafe fn try_arm_setup() -> bool {
    unsafe {
        if EP0_SETUP_ARMED || !ENDPOINTS_READY || EP0_STATE != Ep0State::Setup {
            return EP0_SETUP_ARMED;
        }
        if ARM_COOLDOWN != 0 {
            ARM_COOLDOWN -= 1;
            return false;
        }
        // The core rejects Start Transfer unless the device link is ON; a
        // retry during the host's bus reset would only burn a command. ON is
        // USBLNKST == 0 on a running core.
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return false;
        }
        prepare_trb(0, ep0_setup_ptr(), 8, TRB_CONTROL_SETUP);
        if start_transfer(0, ep0_trb_ptr(0)) {
            EP0_SETUP_ARMED = true;
            PENDING_SETUP_ARM = false;
            trace_event(TRACE_SETUP_QUEUED, 0x4152_4D45, 0, 0, 0, read(DSTS)); // "ARME"
            true
        } else {
            // Fast-fail ("No resource" completes immediately). The host's
            // first SETUP token lands ~1 ms after its bus reset ends, so the
            // retry rate must place an armed SETUP TRB inside that window
            // while still bounding the total failed-command count.
            ARM_COOLDOWN = 200;
            false
        }
    }
}

/// Re-arm EP0 only after a successful STARTTRANSFER command. On failure the
/// endpoint is removed from DALEPENA so a host cannot continue sending SETUP
/// packets into a stale resource; the next Connect Done/USB reset can rebuild
/// the endpoint allocation.
unsafe fn rearm_setup() -> bool {
    // A failed Start Transfer on this core means the device link is not ON
    // yet (the host's bus reset is in flight) - never a broken endpoint. The
    // old punitive path (DALEPENA clear + ENDPOINTS_READY=false) killed EP0
    // exactly when the host's post-reset descriptor read arrived, which is
    // the source of the first-read -110. Leave the endpoint alive: the
    // poll-loop guard retries the arm the moment the link reaches ON.
    if unsafe { start_setup() } {
        return true;
    }
    unsafe {
        trace_event(TRACE_USB_DEVICE_ERROR, 0, 0, 0, 0, read(DSTS));
    }
    false
}

/// Tear down every opt-in GSI channel before a USB reset or Type-C detach.
/// Linux removes queued gadget requests before reusing the endpoint; merely
/// clearing the bookkeeping here would leave DWC3 owning stale TRBs and an
/// outstanding resource index.
unsafe fn reset_gsi_channels() {
    unsafe {
        for index in 0..3 {
            let endpoint = GSI_CHANNEL_ENDPOINT[index];
            let event_buffer = (index + 1) as u32;
            let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
            let request_slot = GSI_REQUEST_SLOTS[index];
            if GSI_RING_ACTIVE[index] && endpoint >= 2 && GSI_RESOURCE_INDEX[index] != 0 {
                let _ = end_gsi_transfer(endpoint, event_buffer);
            }
            if request_slot != usize::MAX {
                // ENDTRANSFER must revoke DWC3 ownership before the gadget
                // request slot is returned to the function layer.
                let _ = udc_mut().release(endpoint_address, request_slot);
            }
            if GSI_CHANNEL_READY[index] && endpoint >= 2 {
                let _ = udc_mut().disable_endpoint(endpoint_address);
                write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
            }
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
            GSI_RING_ACTIVE[index] = false;
            GSI_RESOURCE_INDEX[index] = 0;
            GSI_RING_BASES[index] = 0;
            GSI_RING_TRB_COUNTS[index] = 0;
            GSI_BUFFER_BASES[index] = 0;
            GSI_BUFFER_LENGTHS[index] = 0;
            GSI_DOORBELL_BASES[index] = 0;
            GSI_CHANNEL_READY[index] = false;
            GSI_CHANNEL_ENDPOINT[index] = 0;
        }
        GSI_GADGET_BOUND = false;
    }
}

/// Re-arm the control endpoint after the host has issued a USB bus reset.
///
/// A bus reset terminates the setup transfer which was queued before the
/// host began enumeration, but it does not perform a DWC3 core reset.  The
/// transfer resources and endpoint configuration therefore remain usable.
/// Keeping DALEPENA cleared here leaves the device with a pull-up and no EP0,
/// which is indistinguishable from a dead gadget to the host.
unsafe fn restart_control_after_reset() {
    unsafe {
        // The host's bus USB reset clears GCTL.RAMCLKSEL (the Linux comment
        // about reprogramming it on Connect Done documents exactly this);
        // restore the captured working select before the EP0 rebuild.
        reapply_ramclksel();
        // Linux's dwc3_ep0_reset_state() is a NO-OP while EP0 sits in the
        // SETUP phase: the armed SETUP TRB stays valid across a USB reset,
        // and the reset handler must not tear it down or re-arm. Rewriting
        // DALEPENA, reprogramming DCFG.speed, or issuing a second Start
        // Transfer all race the host's first SETUP token (which lands ~1 ms
        // after the reset ends) and are the source of the first descriptor
        // read/64 error -110. When the TRB is armed, only clear the device
        // address (the hardware already did; Linux rewrites it) and reset the
        // software state that does not touch the armed transfer.
        if EP0_STATE == Ep0State::Setup && EP0_SETUP_ARMED && ENDPOINTS_READY {
            let dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
            write(DCFG, dcfg);
            unbind_function();
            teardown_data_endpoints();
            reset_gsi_channels();
            GadgetDriver::reset(gadget_mut());
            udc_mut().reset();
            CONFIGURED = false;
            DATA_ENDPOINTS_READY = false;
            DATA_REQUEST_SLOTS = [usize::MAX; 2];
            DATA_RESOURCE_INDEX = [0; 2];
            GSI_GADGET_BOUND = false;
            FUNCTION_BOUND = false;
            CONTROL_IN = false;
            CONTROL_HAS_DATA = false;
            // EP0_STATE, EP0_SETUP_ARMED, EP0_RESOURCE_INDEX, ENDPOINTS_READY,
            // DALEPENA, DCFG.speed, and the armed SETUP TRB are preserved.
            trace_event(
                TRACE_USB_RESET,
                0x4B45_504B, // "KEEP"
                0,
                0,
                0,
                read(DSTS),
            );
            return;
        }
        // A bus reset already flushed every in-flight EP0 transfer at the
        // wire level. Issuing ENDXFER here and then re-arming races the
        // resource release against the new Start Transfer: the core answers
        // the re-arm with "No Resource" until the ENDXFER completes, the
        // re-arm lands after the host's post-reset SETUP token, and the
        // first descriptor read times out (-110). Clear only the software
        // index; the hardware transfer state is reset by the bus reset.
        EP0_RESOURCE_INDEX = [0; 2];
        unbind_function();
        teardown_data_endpoints();
        reset_gsi_channels();
        GadgetDriver::reset(gadget_mut());
        udc_mut().reset();
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        // A USB bus reset terminates the active DWC3 EP0 transfer. Linux
        // drops the cached resource index at this boundary; retaining it
        // can make the next STARTTRANSFER look like a continuation of the
        // old Fastboot/control session on some DWC3 revisions.
        EP0_RESOURCE_INDEX = [0; 2];
        EP0_SETUP_ARMED = false;
        PENDING_SETUP_ARM = true;
        GSI_GADGET_BOUND = false;
        FUNCTION_BOUND = false;
        EP0_STATE = Ep0State::Setup;
        CONTROL_IN = false;
        CONTROL_HAS_DATA = false;

        let mut dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
        let speed = read(DSTS) & DSTS_CONNECTSPD_MASK;
        let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
        dcfg &= !DCFG_SPEED_MASK;
        dcfg |= if speed == DSTS_SUPERSPEED {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);

        // USB reset ends the active EP0 transfer, but the endpoint remains
        // configured on the non-core-reset path.  Reconfigure defensively
        // if a preceding Connect Done event did not get processed.
        if !ENDPOINTS_READY {
            ENDPOINTS_READY = configure_endpoint(0, max_packet, false)
                && configure_endpoint(1, max_packet, false);
        }
        if ENDPOINTS_READY {
            let _ = udc_mut().configure_endpoint(0, max_packet as u16, false);
            let _ = udc_mut().configure_endpoint(1, max_packet as u16, false);
            write(DALEPENA, 0b11);
            // The host's bus reset is still in progress when this event is
            // processed, and the core rejects Start Transfer until the link
            // returns to ON. Use the non-punitive arm: a failure here just
            // leaves the arming to the poll-loop guard, which fires the
            // moment the link is up and delivers any latched SETUP.
            let _ = try_arm_setup();
        }
    }
}

/// Reflect gadget-core state into the two pieces of DWC3 device state that
/// are committed only after a successful control status stage.  Linux does
/// not apply SET_ADDRESS or SET_CONFIGURATION at SETUP reception time.
unsafe fn sync_gadget_state() {
    unsafe {
        let address = gadget_ref().address() as u32;
        let dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
        write(DCFG, dcfg | (address << 3));
        CONFIGURED = gadget_ref().configured();
        udc_mut().address = gadget_ref().address();
        udc_mut().configured = CONFIGURED;
        if CONFIGURED && !DATA_ENDPOINTS_READY {
            // The protocol layer exposes one vendor function with either an
            // ordinary bulk pair or an explicitly supplied IPA/GSI binding.
            // Configure it only after SET_CONFIGURATION has committed,
            // matching gadget-core ordering.
            let gsi_config = gadget_ref().gsi_endpoint();
            if let Some(config) = gsi_config {
                if let Some((ring, buffers)) = configure_gsi_data_endpoint(
                    config.endpoint,
                    config.event_buffer,
                    config.max_packet,
                    config.doorbell,
                    config.buffer_length,
                ) {
                    GSI_GADGET_BOUND = true;
                    gadget_mut().on_gsi_channel_ready(config, ring, buffers);
                }
            }

            if !GSI_GADGET_BOUND {
                // Linux calls dwc3_gadget_start_config(2) when
                // SET_CONFIGURATION commits. DEPSTARTCFG(2) resets only
                // non-control endpoint resource allocation; omitting this
                // boundary leaves EP2/EP3 in Fastboot's allocation epoch and
                // can tear down the link immediately after enumeration.
                let data_ready =
                    send_ep_command(0, DEPCMD_DEPSTARTCFG | (2 << DEPCMD_PARAM_SHIFT), 0, 0, 0)
                        && configure_endpoint_kind(2, 512, DEPCFG_EP_TYPE_BULK, false)
                        && configure_endpoint_kind(3, 512, DEPCFG_EP_TYPE_BULK, false);
                if data_ready
                    && udc_mut().configure_endpoint(0x02, 512, true)
                    && udc_mut().configure_endpoint(0x83, 512, true)
                {
                    write(DALEPENA, read(DALEPENA) | (1 << 2) | (1 << 3));
                    DATA_ENDPOINTS_READY = true;
                    // Bind the function only after SET_CONFIGURATION has
                    // committed. Queueing the OUT request here makes the
                    // ordinary UDC data path live before the first packet.
                    FUNCTION_BOUND = true;
                    GadgetDriver::on_function_bind(gadget_mut());
                    let _ = queue_bulk_transfer(
                        2,
                        addr_of_mut!(DATA_OUT_BUFFER.0).cast::<u8>(),
                        MAX_PACKET_SIZE as usize,
                    );
                }
            } else {
                FUNCTION_BOUND = true;
                GadgetDriver::on_function_bind(gadget_mut());
            }
        } else if !CONFIGURED && (DATA_ENDPOINTS_READY || GSI_GADGET_BOUND) {
            teardown_data_endpoints();
            if GSI_GADGET_BOUND {
                reset_gsi_channels();
            }
            unbind_function();
        }
    }
}

unsafe fn start_status(endpoint: usize) -> bool {
    let kind = if unsafe { CONTROL_HAS_DATA } {
        TRB_CONTROL_STATUS3
    } else {
        TRB_CONTROL_STATUS2
    };
    trace_event(TRACE_STATUS_QUEUED, 0, endpoint as u32, kind, 0, unsafe {
        read(DSTS)
    });
    unsafe {
        prepare_trb(0, ep0_trb_ptr(0).cast::<u8>(), 0, kind);
        // Same flaky Start Transfer window as the data phase: retry the
        // command instead of failing the status stage (SET_ADDRESS and
        // SET_CONFIGURATION become visible only after this ZLP completes).
        let mut queued = start_transfer(endpoint, ep0_trb_ptr(0));
        if !queued {
            for _ in 0..50 {
                super::timer::delay_us(200);
                if start_transfer(endpoint, ep0_trb_ptr(0)) {
                    queued = true;
                    break;
                }
            }
        }
        queued
    }
}

unsafe fn stall_control(endpoint: usize) {
    // Linux's gadget core responds to an unsupported control request with a
    // real EP0 STALL. Leaving the endpoint idle is not equivalent: hosts may
    // keep waiting for the missing handshake and never issue the next SETUP.
    let _ = unsafe { send_ep_command(endpoint, DEPCMD_SETSTALL, 0, 0, 0) };
    unsafe {
        EP0_STATE = Ep0State::Setup;
    }
}

unsafe fn setup_request() -> [u8; 8] {
    let mut packet = [0; 8];
    unsafe {
        let setup = ep0_setup_ptr();
        cache_invalidate(setup as usize, 8);
        core::ptr::copy_nonoverlapping(setup, packet.as_mut_ptr(), 8);
    }
    packet
}

unsafe fn handle_setup() {
    let packet = unsafe { setup_request() };
    // Zero the DMA buffer after latching the packet: a later non-zero
    // buffer then proves the core delivered a NEW SETUP packet, even while
    // the software state machine was still in the Data/Status phase (the
    // host aborts in-flight control transfers with a new SETUP - Linux
    // handles this via its setup_packet_pending logic).
    unsafe {
        core::ptr::write_bytes(ep0_setup_ptr(), 0, 8);
        cache_clean(ep0_setup_ptr() as usize, 8);
    }
    let request_type = packet[0];
    let request = packet[1];
    let value = u16::from_le_bytes([packet[2], packet[3]]);
    let index = u16::from_le_bytes([packet[4], packet[5]]);
    let requested_length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
    let direction_in = request_type & 0x80 != 0;
    trace_event(
        TRACE_SETUP_RECEIVED,
        request as u32,
        value as u32,
        index as u32,
        requested_length as u32,
        unsafe { read(DSTS) },
    );
    unsafe {
        // Record the Connect Done -> first SETUP delay (seconds) so the
        // harvest gates can tell whether the control pipeline ran inside the
        // host's enumeration window or long after the host gave up.
        if TRACE_HARVEST_SETUP_DELAY == 0xFFFF && CONNECT_TICK != 0 {
            let frequency = arch_counter_frequency();
            if frequency != 0 {
                let delta_ticks = arch_counter().saturating_sub(CONNECT_TICK);
                TRACE_HARVEST_SETUP_DELAY = (delta_ticks / frequency).min(0xFFFE) as u32;
            }
        }
        CONTROL_IN = direction_in;
        CONTROL_HAS_DATA = requested_length != 0;
    }

    let action = unsafe {
        let response = core::slice::from_raw_parts_mut(ep0_response_ptr(), 512);
        if request_type == TRACE_CONTROL_REQUEST_TYPE && request == TRACE_CONTROL_REQUEST {
            // Keep trace reads outside the gadget function callback: this is
            // a diagnostic transport over the same EP0 path and must not
            // alter address/configuration state.
            fill_trace_control_response(response, requested_length, value)
                .map(ControlAction::DataIn)
                .unwrap_or(ControlAction::Stall)
        } else {
            // Keep the trace transport in the ordinary EP0 path: a host request
            // for string descriptor 3 can observe the retained cursor even when
            // no UART cable is attached.
            gadget_mut().set_trace_status(trace_head(), trace_last_event());
            GadgetDriver::on_setup(gadget_mut(), packet, response)
        }
    };
    match action {
        ControlAction::DataIn(length) => unsafe {
            let response = ep0_response_ptr();
            cache_clean(response as usize, length);
            prepare_trb(0, response, length, TRB_CONTROL_DATA);
            trace_event(
                TRACE_DESCRIPTOR_QUEUED,
                request as u32,
                value as u32,
                index as u32,
                length as u32,
                read(DSTS),
            );
            EP0_STATE = Ep0State::Data;
            // This core's endpoint command engine flakily rejects Start
            // Transfer right after the bus reset ("No resource" or a stuck
            // CMDACT) even though the identical command succeeds seconds
            // later - the host's first descriptor read and its retry straddle
            // exactly that window. The host keeps polling EP0 IN with IN
            // tokens while the data phase is pending and tolerates the NAKs,
            // so a bounded command retry answers the first read instead of
            // stalling the whole control transfer.
            let mut queued = start_transfer(1, ep0_trb_ptr(0));
            if !queued {
                // A USB bus reset can invalidate the endpoint 1 transfer
                // resource allocated at init, so the control data-phase Start
                // Transfer is answered "No resource". Re-allocate the resource
                // once, then retry the command.
                let _ = send_ep_command(1, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0);
                for _ in 0..50 {
                    super::timer::delay_us(200);
                    if start_transfer(1, ep0_trb_ptr(0)) {
                        queued = true;
                        break;
                    }
                }
            }
            trace_event(
                TRACE_DESCRIPTOR_QUEUED,
                0x4441_524D, // "DARM" data-phase arm outcome
                queued as u32,
                0,
                length as u32,
                read(DSTS),
            );
            if queued {
                note_probe_ep0_progress();
            } else {
                // A failed DATA-IN command must not leave EP0 in the Data
                // state: the next host request would otherwise be consumed
                // by a stale state machine with no active TRB.
                stall_control(1);
            }
        },
        ControlAction::StatusIn => unsafe {
            EP0_STATE = Ep0State::Status;
            // SET_ADDRESS/SET_CONFIGURATION become visible only after this
            // status IN transfer completes, matching gadget-core semantics.
            if start_status(1) {
                note_probe_ep0_progress();
            } else {
                stall_control(if direction_in { 1 } else { 0 });
            }
        },
        ControlAction::Stall => {
            log_puts("usb: unsupported control request\n");
            unsafe { stall_control(if direction_in { 1 } else { 0 }) };
        }
        ControlAction::Setup
        | ControlAction::StatusOut
        | ControlAction::SetHalt(_)
        | ControlAction::ClearHalt(_) => {
            log_puts("usb: invalid gadget control action\n");
            unsafe { stall_control(if direction_in { 1 } else { 0 }) };
        }
    }
}

unsafe fn process_event(raw: u32) {
    let endpoint_event = (raw & 1) == 0;
    if !endpoint_event {
        // DWC3's device event layout is: one_bit[0], device_event[1:7],
        // type[8:11].  The device_event field is zero for ordinary device
        // events; type carries Disconnect, USB Reset, and Connect Done.
        let device_event = (raw >> DEVICE_EVENT_KIND_SHIFT) & DEVICE_EVENT_KIND_MASK;
        match device_event {
            0 => {
                // Disconnect invalidates the active control transfer and the
                // device address. Do not rearm until Connect Done establishes
                // a fresh link, exactly as the Linux gadget lifecycle does.
                unsafe {
                    for endpoint in 0..2 {
                        if EP0_RESOURCE_INDEX[endpoint] != 0 {
                            let _ = end_transfer(endpoint);
                            EP0_RESOURCE_INDEX[endpoint] = 0;
                        }
                    }
                    unbind_function();
                    teardown_data_endpoints();
                    GadgetDriver::reset(gadget_mut());
                    udc_mut().reset();
                    CONFIGURED = false;
                    DATA_ENDPOINTS_READY = false;
                    DATA_REQUEST_SLOTS = [usize::MAX; 2];
                    DATA_RESOURCE_INDEX = [0; 2];
                    EP0_RESOURCE_INDEX = [0; 2];
                    EP0_SETUP_ARMED = false;
                    EP0_STATE = Ep0State::Setup;
                    CONTROL_IN = false;
                    CONTROL_HAS_DATA = false;
                    ENDPOINTS_READY = false;
                    write(DALEPENA, 0);
                }
                note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Disconnect);
            }
            1 => {
                trace_event(TRACE_USB_RESET, 0, 0, 0, 0, raw);
                note_runtime_event(super::platform::bramble::UsbRuntimeEvent::BusReset);
                unsafe { restart_control_after_reset() }
            }
            2 => {
                trace_event(TRACE_DEVICE_CONNECT, 0, 0, 0, 0, raw);
                let speed = unsafe { read(DSTS) & DSTS_CONNECTSPD_MASK };
                log_puts("usb: connect done, speed=");
                log_hex_value(speed as u64);
                unsafe {
                    CONNECT_TICK = arch_counter();
                    PENDING_SETUP_ARM = true;
                }
                // Linux's DWC3 gadget driver starts with the SuperSpeed EP0
                // size and modifies it after Connect Done.
                let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
                unsafe {
                    let first_connect = !ENDPOINTS_READY;
                    // A post-reset Connect Done (first_connect false) must not
                    // reconfigure the endpoints or rewrite DALEPENA while EP0
                    // holds an armed SETUP TRB. Linux's conndone only issues a
                    // DEPCFG MODIFY and never re-arms; our reconfigure plus the
                    // DALEPENA rewrite would race the host's first post-reset
                    // SETUP token and reproduce the descriptor read/64 -110.
                    if !first_connect && EP0_STATE == Ep0State::Setup && EP0_SETUP_ARMED {
                        note_runtime_event(
                            super::platform::bramble::UsbRuntimeEvent::ControllerStarted,
                        );
                        return;
                    }
                    let endpoints_ready = if first_connect {
                        configure_endpoint(0, max_packet, false)
                            && configure_endpoint(1, max_packet, false)
                    } else {
                        configure_endpoint(0, max_packet, true)
                            && configure_endpoint(1, max_packet, true)
                    };
                    if endpoints_ready {
                        ENDPOINTS_READY = true;
                        let _ = udc_mut().configure_endpoint(0, max_packet as u16, false);
                        let _ = udc_mut().configure_endpoint(1, max_packet as u16, false);
                        write(DALEPENA, 0b11);
                        // The two Bramble timing differentials own the first
                        // EP0 STARTTRANSFER at a different boundary. Do not
                        // issue a second STARTTRANSFER at Connect Done: the
                        // host's USB RESET path will revoke the old resource
                        // and arm the fresh SETUP transfer exactly once.
                        if !cfg!(any(
                            fullerene_aarch64_usb_gadget_handoff_start_after_connect,
                            fullerene_aarch64_usb_gadget_handoff_start_after_reset
                        )) {
                            rearm_setup();
                        }
                        note_runtime_event(
                            super::platform::bramble::UsbRuntimeEvent::ControllerStarted,
                        );
                    }
                }
            }
            DEVICE_EVENT_LINK_STATUS_CHANGE => {
                // The Qualcomm glue consumes link changes for its LPM/PHY
                // policy.  Keep the event visible in retained RAM even when
                // this early gadget has no negotiated LPM policy of its own.
                trace_event(TRACE_LINK_STATUS, 0, 0, 0, 0, raw);
            }
            DEVICE_EVENT_WAKEUP => {
                trace_event(TRACE_USB_WAKEUP, 0, 0, 0, 0, raw);
                // The normal Linux path queues resume work from the wakeup
                // event. Keep the same boundary here; process_event() may be
                // reached from the synchronous early IRQ dispatcher.
                unsafe {
                    RESUME_PENDING = true;
                }
            }
            DEVICE_EVENT_SUSPEND => {
                // DWC3 emits a suspend event during initial attach on some
                // revisions, before RESET/CONNECT_DONE and before the gadget
                // is configured. Linux deliberately ignores that event.
                // Once configured, this is still the USB bus entering L1/L2,
                // not a system runtime-PM request. Do not power-gate the
                // Qualcomm USB clock/rails here: doing so tears down a live
                // gadget and makes a successful enumeration disappear.
                let configured = unsafe { CONFIGURED };
                if configured {
                    trace_event(TRACE_USB_SUSPEND, 0, 0, 0, 0, raw);
                }
            }
            DEVICE_EVENT_HIBERNATION_REQUEST => {
                trace_event(TRACE_USB_DEVICE_ERROR, device_event, 0, 0, 0, raw);
                // A DWC3 hibernation notification is not by itself a system
                // suspend request. Keep the Qualcomm session powered while
                // the host keeps the SuperSpeed gadget idle; powering down
                // here makes a successfully configured bulk gadget disappear.
                // Explicit runtime suspend/resume remains available to the
                // platform policy, but this hardware event alone must not
                // invoke it.
            }
            DEVICE_EVENT_ERRATIC_ERROR | DEVICE_EVENT_CMD_COMPLETE | DEVICE_EVENT_OVERFLOW => {
                trace_event(TRACE_USB_DEVICE_ERROR, device_event, 0, 0, 0, raw);
            }
            _ => {}
        }
        return;
    }

    let endpoint = ((raw >> 1) & 0x1f) as usize;
    let event = (raw >> 6) & 0xf;
    let status = (raw >> 12) & 0xf;
    if event == 1 {
        if endpoint >= 2 {
            unsafe { complete_bulk_transfer(endpoint, status, raw) };
            return;
        }
        // Linux's dwc3_ep0_xfer_complete() does NOT look at the event status
        // at all: XferComplete status bits on EP0 carry LST/IOC-style flags
        // (our SETUP TRB sets LST, so a healthy completion reports 0x8), and
        // the dispatch is purely by ep0state. Routing non-zero statuses into
        // the recovery path would eat every healthy SETUP completion.
        unsafe {
            EP0_RESOURCE_INDEX[endpoint] = 0;
            // The previously armed SETUP/DATA/STATUS transfer is consumed;
            // the poll-loop guard re-arms the SETUP TRB once EP0 returns to
            // the Setup state.
            EP0_SETUP_ARMED = false;
            // A freshly DMAed SETUP packet overrides any in-flight phase:
            // hosts abort stalled control transfers by sending a new SETUP,
            // and the completion event for the OLD transfer carries it.
            // Linux recovers via setup_packet_pending; without this the new
            // SETUP is dispatched into the stale Data/Status handler and the
            // request is silently lost (the mid-enumeration death).
            let setup = ep0_setup_ptr();
            cache_invalidate(setup as usize, 8);
            let mut fresh_setup = false;
            for offset in 0..8 {
                if read_volatile(setup.add(offset)) != 0 {
                    fresh_setup = true;
                    break;
                }
            }
            if fresh_setup {
                EP0_STATE = Ep0State::Setup;
                handle_setup();
                return;
            }
        }
        trace_event(
            TRACE_TRANSFER_COMPLETE,
            event,
            endpoint as u32,
            status,
            0,
            raw,
        );
        unsafe {
            match EP0_STATE {
                Ep0State::Setup => handle_setup(),
                Ep0State::Data if endpoint == 0 || endpoint == 1 => {
                    let action = GadgetDriver::on_transfer_complete(gadget_mut());
                    EP0_STATE = Ep0State::Status;
                    match action {
                        ControlAction::StatusOut => {
                            if !start_status(0) {
                                stall_control(0);
                            }
                        }
                        ControlAction::StatusIn => {
                            if !start_status(1) {
                                stall_control(1);
                            }
                        }
                        _ => stall_control(if CONTROL_IN { 1 } else { 0 }),
                    }
                }
                Ep0State::Status => match GadgetDriver::on_transfer_complete(gadget_mut()) {
                    ControlAction::Setup => {
                        sync_gadget_state();
                        EP0_STATE = Ep0State::Setup;
                        rearm_setup();
                    }
                    ControlAction::SetHalt(address) => {
                        let endpoint = (address & 0x7f) as usize;
                        if send_ep_command(endpoint, DEPCMD_SETSTALL, 0, 0, 0)
                            && udc_mut().set_halt(address, true)
                        {
                            sync_gadget_state();
                            EP0_STATE = Ep0State::Setup;
                            rearm_setup();
                        } else {
                            stall_control(if CONTROL_IN { 1 } else { 0 });
                        }
                    }
                    ControlAction::ClearHalt(address) => {
                        let endpoint = (address & 0x7f) as usize;
                        if send_ep_command(endpoint, DEPCMD_CLEARSTALL, 0, 0, 0)
                            && udc_mut().set_halt(address, false)
                        {
                            sync_gadget_state();
                            EP0_STATE = Ep0State::Setup;
                            rearm_setup();
                        } else {
                            stall_control(if CONTROL_IN { 1 } else { 0 });
                        }
                    }
                    _ => stall_control(if CONTROL_IN { 1 } else { 0 }),
                },
                _ => {}
            }
        }
    } else if event == 3 {
        // XferNotReady: the core asks for the next phase's TRB. Record every
        // event for the harvest gates (request=endpoint, value=status); act
        // only on the STATUS ask while the state machine waits for it.
        trace_event(TRACE_XFER_NOT_READY, endpoint as u32, status, 0, 0, raw);
        if status == 2 {
            unsafe {
                if EP0_STATE == Ep0State::Status {
                    let endpoint = if CONTROL_HAS_DATA && CONTROL_IN { 0 } else { 1 };
                    if !start_status(endpoint) {
                        stall_control(endpoint);
                    }
                }
            }
        }
    }
}

/// Recover EP0 after a non-success transfer-complete status.
///
/// DWC3 can report a completed control transfer with an error status when a
/// host aborts the request, the link changes, or the controller loses the
/// transfer resource during a handoff. Linux removes the old request before
/// queueing the next SETUP; treating the event as a normal Data/Status
/// transition would instead leave EP0 pointing at a retired TRB and produce
/// another host timeout. Revoke the resource first, clear the software state,
/// and rearm SETUP only after the endpoint ownership boundary is restored.
unsafe fn recover_control_transfer(endpoint: usize, status: u32, raw: u32) {
    trace_event(
        TRACE_USB_DEVICE_ERROR,
        endpoint as u32,
        raw,
        status,
        EP0_STATE as u32,
        read(DSTS),
    );
    if endpoint < 2 && EP0_RESOURCE_INDEX[endpoint] != 0 {
        let _ = end_transfer(endpoint);
        EP0_RESOURCE_INDEX[endpoint] = 0;
    }
    EP0_STATE = Ep0State::Setup;
    CONTROL_IN = false;
    CONTROL_HAS_DATA = false;
    if ENDPOINTS_READY {
        let _ = rearm_setup();
    }
}

unsafe fn complete_bulk_transfer(endpoint: usize, status: u32, raw: u32) {
    if endpoint != 2 && endpoint != 3 {
        return;
    }
    let index = endpoint - 2;
    let slot = unsafe { DATA_REQUEST_SLOTS[index] };
    if slot == usize::MAX {
        trace_event(TRACE_USB_DEVICE_ERROR, endpoint as u32, raw, 0, 0, status);
        return;
    }
    let address = if endpoint == 3 { 0x83 } else { 0x02 };
    unsafe {
        let trb = addr_of_mut!(DATA_TRBS).cast::<Trb>().add(index);
        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
        let residual = read_volatile(addr_of!((*trb).size)) & 0x00ff_ffff;
        let actual = udc_mut()
            .request(address, slot)
            .map(|request| request.length.saturating_sub(residual))
            .unwrap_or(0);
        let error = status != 0;
        let _ = udc_mut().complete(address, slot, actual, error);
        GadgetDriver::on_data_complete(gadget_mut(), address, actual, error);
        trace_event(
            TRACE_TRANSFER_COMPLETE,
            endpoint as u32,
            raw,
            status,
            actual,
            error as u32,
        );
        let _ = udc_mut().release(address, slot);
        DATA_REQUEST_SLOTS[index] = usize::MAX;
        DATA_RESOURCE_INDEX[index] = 0;
        // Keep an OUT request posted after completion. This is the bounded
        // early-boot equivalent of a gadget function's request callback
        // requeue; the release above returns the UDC slot before reuse.
        if endpoint == 2 && CONFIGURED && DATA_ENDPOINTS_READY {
            let _ = queue_bulk_transfer(
                2,
                addr_of_mut!(DATA_OUT_BUFFER.0).cast::<u8>(),
                MAX_PACKET_SIZE as usize,
            );
        }
    }
}

/// Initialize the Bramble DWC3 in peripheral mode and connect the pull-up.
pub fn init() -> bool {
    init_with_super_speed(true, true, true)
}

/// Initialize only the USB2 path for the dependency-free hardware probe.
pub fn init_usb2_only() -> bool {
    init_with_super_speed(false, true, true)
}

/// Pass the PMIC/Type-C cable orientation into the QMP combo PHY path.
/// Android programs the QMP Type-C control register after the PHY is powered
/// and before releasing the combo-PHY reset override.
pub fn set_typec_orientation(orientation_reverse: bool) {
    unsafe {
        TYPEC_LANE_B = orientation_reverse;
    }
}

/// Install the PMIC state discovered before the controller is touched. The
/// APID is retained so later PDC/GIC events can refresh the same Type-C
/// peripheral without another arbiter tree walk.
pub fn install_typec_state(state: super::platform::bramble::TypecState) {
    unsafe {
        TYPEC_STATE = state;
        TYPEC_STATE_VALID = true;
        TYPEC_POLL_TICKS = 0;
    }
}

pub fn note_platform_powered() {
    unsafe {
        USB_RUNTIME_STATE = super::platform::bramble::usb_runtime_transition(
            USB_RUNTIME_STATE,
            super::platform::bramble::UsbRuntimeEvent::PlatformPowered,
        );
    }
}

pub fn note_typec_attached(attached: bool) {
    if !attached {
        return;
    }
    unsafe {
        USB_RUNTIME_STATE = super::platform::bramble::usb_runtime_transition(
            USB_RUNTIME_STATE,
            super::platform::bramble::UsbRuntimeEvent::TypecAttached,
        );
    }
}

/// Observe the Type-C state at the Fastboot handoff boundary without
/// changing PMIC registers.  Android obtains this state through the
/// Qualcomm role-switch/PMIC driver before it starts the UDC; the temporary
/// image has no role-switch framework, so perform the same read-only bridge
/// explicitly.  A failed observation is non-fatal: Fastboot has already
/// established a device-mode transport, and the later DWC3/EP0 probe must
/// remain useful for separating an SPMI aperture problem from a USB problem.
pub fn observe_typec_handoff() -> bool {
    note_runtime_event(super::platform::bramble::UsbRuntimeEvent::PlatformPowered);
    trace_marker(TRACE_TYPEC_BEGIN, 0x4f4253); // "OBS"
    let Some(state) = (unsafe { super::platform::bramble::observe_usb_device_role() }) else {
        trace_marker(TRACE_TYPEC_DONE, 0xffff_ffff);
        return false;
    };

    set_typec_orientation(state.orientation_reverse);
    note_typec_attached(state.attached);
    trace_event(
        TRACE_TYPEC_DONE,
        state.role as u32,
        state.attached as u32,
        state.orientation_reverse as u32,
        state.mode as u32,
        state.misc_status as u32,
    );
    unsafe {
        TYPEC_STATE = state;
        TYPEC_STATE_VALID = true;
        TYPEC_POLL_TICKS = 0;
    }
    true
}

/// Complete a deferred Type-C parent interrupt outside the hard IRQ entry.
/// This mirrors Linux's threaded qpnpint/role-switch boundary.
pub fn service_deferred_platform() {
    unsafe {
        if !TYPEC_IRQ_PENDING {
            return;
        }
        if !TYPEC_STATE_VALID {
            // The standalone gadget probe intentionally skips SPMI role
            // discovery. Leave the diagnostic parent SPI masked rather than
            // issuing an acknowledge against an uninitialized PMIC state.
            TYPEC_IRQ_PENDING = false;
            return;
        }
        // Linux's PMIC Type-C handler samples the child state in its threaded
        // context before clearing the parent summary. Do the same here: an
        // acknowledge-only path loses a real attach/detach edge and leaves
        // the DWC3 session in the previous role.
        let event = {
            let state = &mut *addr_of_mut!(TYPEC_STATE);
            let event = super::platform::bramble::refresh_usb_device_role(state);
            if event.is_some() {
                TYPEC_LANE_B = state.orientation_reverse;
            }
            event
        };
        if let Some(event) = event {
            apply_typec_event(event);
        }
        let state = &*addr_of!(TYPEC_STATE);
        if !super::platform::bramble::acknowledge_typec_irq(state) {
            trace_event(
                TRACE_USB_DEVICE_ERROR,
                super::platform::bramble::usb_typec_parent_irq(),
                0,
                0,
                0,
                0,
            );
        }
        TYPEC_IRQ_PENDING = false;
        super::platform::gicv3::enable_spis(
            super::platform::bramble::GICD_BASE,
            &[super::platform::bramble::usb_typec_parent_irq()],
        );
    }
}

fn note_runtime_event(event: super::platform::bramble::UsbRuntimeEvent) {
    unsafe {
        USB_RUNTIME_STATE =
            super::platform::bramble::usb_runtime_transition(USB_RUNTIME_STATE, event);
    }
}

unsafe fn apply_typec_event(event: super::platform::bramble::TypecEvent) {
    trace_event(TRACE_TYPEC_EVENT, event as u32, 0, 0, 0, 0);
    match event {
        super::platform::bramble::TypecEvent::DetachDetected => {
            // Linux's role-switch callback stops advertising before it tears
            // down the UDC queues. Do not issue endpoint commands after the
            // PMIC has removed the cable.
            unbind_function();
            teardown_data_endpoints();
            reset_gsi_channels();
            write(DALEPENA, 0);
            let _ = run_stop_device(false);
            ENDPOINTS_READY = false;
            CONFIGURED = false;
            DATA_ENDPOINTS_READY = false;
            DATA_REQUEST_SLOTS = [usize::MAX; 2];
            DATA_RESOURCE_INDEX = [0; 2];
            GadgetDriver::reset(gadget_mut());
            udc_mut().reset();
            note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Disconnect);
        }
        super::platform::bramble::TypecEvent::HostDetected => {
            // The PMIC role-switch may move directly from device to source
            // when another Type-C partner is attached. A source/host role
            // must never leave the old gadget pull-up or DMA request live.
            unbind_function();
            teardown_data_endpoints();
            reset_gsi_channels();
            write(DALEPENA, 0);
            let _ = run_stop_device(false);
            ENDPOINTS_READY = false;
            CONFIGURED = false;
            DATA_ENDPOINTS_READY = false;
            DATA_REQUEST_SLOTS = [usize::MAX; 2];
            DATA_RESOURCE_INDEX = [0; 2];
            GadgetDriver::reset(gadget_mut());
            udc_mut().reset();
            note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Disconnect);
        }
        super::platform::bramble::TypecEvent::AttachDetected => {
            // Attach is the prerequisite for the Qualcomm VBUS/session
            // override. Connect Done will reconfigure EP0 and rearm SETUP
            // when the host starts the new USB session.
            note_runtime_event(super::platform::bramble::UsbRuntimeEvent::TypecAttached);
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
            qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        }
        _ => {}
    }
}

/// Enable the Qualcomm glue notifications that the Android driver consumes.
/// The DWC3 event ring does not report P3/L1 transitions, so leaving this mask
/// at the bootloader default makes runtime-PM state diverge even when EP0 is
/// functioning.
unsafe fn enable_power_events() {
    let mask = unsafe { read_qscratch(QSCRATCH_PWR_EVENT_MASK) }
        | PWR_EVENT_POWERDOWN_IN_P3
        | PWR_EVENT_POWERDOWN_OUT_P3
        | PWR_EVENT_LPM_OUT_L1;
    unsafe { write_qscratch(QSCRATCH_PWR_EVENT_MASK, mask) };
}

#[inline]
const fn power_event_clear_mask(status: u32) -> u32 {
    // P3 and L1-out are edge notifications consumed by the Qualcomm glue.
    // L2-out is intentionally not included: the Android handler treats it as
    // an indication while the suspend path explicitly clears L2-in.
    status & (PWR_EVENT_POWERDOWN_IN_P3 | PWR_EVENT_POWERDOWN_OUT_P3 | PWR_EVENT_LPM_OUT_L1)
}

#[inline]
const fn power_event_requests_resume(status: u32) -> bool {
    status & PWR_EVENT_LPM_OUT_L1 != 0
}

/// Match the P3 bookkeeping in dwc3_pwr_event_handler().  When both bits are
/// reported the hardware does not identify the direction in the event word;
/// preserve the previous state until a link-state read is available rather
/// than guessing and changing the platform vote spuriously.
#[inline]
unsafe fn update_p3_state(status: u32) {
    let p3_in = status & PWR_EVENT_POWERDOWN_IN_P3 != 0;
    let p3_out = status & PWR_EVENT_POWERDOWN_OUT_P3 != 0;
    if p3_in && !p3_out {
        USB_IN_P3 = true;
    } else if p3_out && !p3_in {
        USB_IN_P3 = false;
    }
}

/// Prepare the USB2 PHY for runtime suspend using the same observable
/// boundary as Android's dwc3_msm_prepare_suspend().  The early image has no
/// jiffies/workqueue, so the bounded loop is expressed in MMIO polling
/// iterations.  A device-mode failure is recorded but is non-fatal, matching
/// the upstream path for a non-host/non-bus-suspend transition.
unsafe fn prepare_usb2_suspend() -> bool {
    unsafe {
        // Clear stale L2 notifications before asking the PHY to enter L2.
        write_qscratch(
            QSCRATCH_PWR_EVENT_STATUS,
            PWR_EVENT_LPM_IN_L2 | PWR_EVENT_LPM_OUT_L2,
        );
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 |= GUSB2PHYCFG_ENBLSLPM | GUSB2PHYCFG_SUSPHY;
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);

        let mut entered_l2 = false;
        for _ in 0..1_000_000u32 {
            if read_qscratch(QSCRATCH_PWR_EVENT_STATUS) & PWR_EVENT_LPM_IN_L2 != 0 {
                entered_l2 = true;
                break;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }

        if !entered_l2 {
            trace_event(
                TRACE_USB_DEVICE_ERROR,
                0x4c_32544f,
                read_qscratch(QSCRATCH_PWR_EVENT_STATUS),
                read(GUSB2PHYCFG0),
                read(DSTS),
                0,
            );
        }

        // The status bit is W1C.  This is done even on the device-mode timeout
        // path, as in Android's prepare_suspend(), so a stale L2-in event does
        // not wake the next runtime transition immediately.
        write_qscratch(QSCRATCH_PWR_EVENT_STATUS, PWR_EVENT_LPM_IN_L2);
        entered_l2
    }
}

/// Drain the Qualcomm glue power-event status separately from DWC3 device
/// events. Android's threaded power IRQ handles P3/L1 transitions here; if
/// the early boot path has not yet installed a working GIC route, polling the
/// same W1C status register keeps the transition observable without confusing
/// a power event with an EP0 transfer event.
unsafe fn service_power_event() {
    let status = unsafe { read_qscratch(QSCRATCH_PWR_EVENT_STATUS) };
    if status == 0 || status == u32::MAX {
        return;
    }

    unsafe { update_p3_state(status) };
    trace_event(
        TRACE_USB_DEVICE_ERROR,
        0x5057_5200,
        status,
        unsafe { USB_IN_P3 as u32 },
        0,
        0,
    );
    if status & (PWR_EVENT_LPM_IN_L2 | PWR_EVENT_LPM_OUT_L2) != 0 {
        trace_event(
            TRACE_USB_DEVICE_ERROR,
            0x4c,
            status & (PWR_EVENT_LPM_IN_L2 | PWR_EVENT_LPM_OUT_L2),
            0,
            0,
            0,
        );
    }
    if power_event_requests_resume(status) {
        unsafe {
            RESUME_PENDING = true;
        }
    }
    // L2-out is an indication used by the Qualcomm state machine; Linux
    // deliberately leaves it in the status value while processing the
    // transition. Do not write it back as W1C here.
    let clear = power_event_clear_mask(status);
    if clear != 0 {
        unsafe { write_qscratch(QSCRATCH_PWR_EVENT_STATUS, clear) };
    }
}

/// Poll the PMIC Type-C status at a bounded rate. This covers the interval
/// before a stable GIC owner exists; the IRQ path calls the same operation
/// immediately for USB-related parent interrupts.
unsafe fn poll_typec_state(force: bool) {
    if !TYPEC_STATE_VALID {
        return;
    }
    // Before the GIC/PMIC child IRQ route is live, bounded polling bridges
    // the handoff gap. Once Linux's normal role-change interrupt boundary is
    // installed, keep the PMIC read on that IRQ path only; polling every USB
    // event can sample a transient CC state and falsely apply detach to a
    // live gadget.
    if super::platform::bramble::usb_resource_state().irq_routes_enabled {
        return;
    }
    TYPEC_POLL_TICKS = TYPEC_POLL_TICKS.wrapping_add(1);
    if !force && TYPEC_POLL_TICKS & 0x3fff != 0 {
        return;
    }
    let state = unsafe { &mut *addr_of_mut!(TYPEC_STATE) };
    if let Some(event) = unsafe { super::platform::bramble::refresh_usb_device_role(state) } {
        TYPEC_LANE_B = state.orientation_reverse;
        unsafe { apply_typec_event(event) };
    }
}

/// Entry point used by the AArch64 IRQ dispatcher for Qualcomm power and PDC
/// parent lines. A PMIC event is kept separate from a DWC3 event-buffer word.
pub fn handle_platform_irq(interrupt_id: u32) {
    unsafe {
        trace_event(TRACE_PLATFORM_IRQ, interrupt_id, 0, 0, 0, 0);
        if super::platform::bramble::is_usb_smmu_irq(interrupt_id) {
            service_smmu_fault();
        }
        if interrupt_id == super::platform::bramble::usb_power_event_irq() {
            service_power_event();
        }
        if interrupt_id == super::platform::bramble::usb_typec_parent_irq() {
            // The initial role request above is authoritative for a
            // fastboot handoff.  The PMIC parent can deliver a stale
            // transition while Fastboot tears down its gadget; re-reading
            // MISC_STATUS here would turn that transient into a false
            // detach and remove the live Fullerene pull-up. Mark the parent
            // pending here; the SPMI child clear runs in the normal
            // processing context, like Linux's threaded qpnpint/role-switch
            // path.
            TYPEC_IRQ_PENDING = true;
        }
    }
}

/// Enter the same controller-side runtime suspend boundary as the Qualcomm
/// glue: drain GSI write state, stop the device, and only then allow the
/// platform vote to fall to the suspend case. The PM QoS/interconnect payload
/// is resolved by the platform resource contract; firmware-owned vote writes
/// are intentionally kept outside this MMIO-only early path.
pub fn runtime_suspend() -> bool {
    unsafe {
        if !set_gsi_doorbell_blocked(true) {
            return false;
        }
        if !gsi_ready_to_suspend() {
            let _ = set_gsi_doorbell_blocked(false);
            return false;
        }
        let _ = prepare_usb2_suspend();
        if QMP_PHY_READY {
            // The QMP driver keeps the connected SuperSpeed PHY powered and
            // switches it to autonomous receiver/LFPS detection before its
            // clocks are gated. This is separate from the USB2 L2 request.
            qmp_set_autonomous_mode(true);
        }
        if !run_stop_device(false) {
            let _ = set_gsi_doorbell_blocked(false);
            return false;
        }
        suspend_data_transfers();
        suspend_gsi_transfers();
        udc_mut().suspend();
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Suspend);
        if !super::platform::bramble::apply_usb_performance(
            super::platform::bramble::UsbBusVote::Suspend,
        ) {
            log_puts("usb: RPMh suspend vote unavailable\n");
        }
        if QMP_PHY_READY {
            if !super::platform::bramble::disable_usb30_gdsc() {
                log_puts("usb: USB3 GDSC collapse not observable\n");
            }
        }
        if !super::platform::bramble::disable_usb_clock_branches() {
            log_puts("usb: USB clock gate readback unavailable\n");
        }
        if !super::platform::bramble::apply_usb_power(false, QMP_PHY_READY) {
            log_puts("usb: RPMh regulator disable unavailable\n");
        }
        return true;
    }
    false
}

/// Resume the device controller after runtime suspend and reassert the
/// Qualcomm session-valid override before Run/Stop, matching the upstream
/// run/stop notifier ordering.
pub fn runtime_resume() -> bool {
    unsafe {
        if !super::platform::bramble::apply_usb_power(true, QMP_PHY_READY) {
            log_puts("usb: RPMh regulator enable unavailable\n");
        }
        if !super::platform::bramble::enable_usb30_gdsc() {
            log_puts("usb: USB3 GDSC restore not observable\n");
        }
        if !super::platform::bramble::enable_usb_clock_branches() {
            log_puts("usb: USB clock ungate readback unavailable\n");
        }
        if !super::platform::bramble::apply_usb_performance(
            super::platform::bramble::UsbBusVote::Nominal,
        ) {
            log_puts("usb: RPMh nominal vote unavailable\n");
        }
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        enable_power_events();
        if QMP_PHY_READY {
            qmp_set_autonomous_mode(false);
        }
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);
        let _ = set_gsi_doorbell_blocked(false);
        if !run_stop_device(true) {
            return false;
        }
        udc_mut().resume();
        if DATA_ENDPOINTS_READY {
            let _ = queue_bulk_transfer(
                2,
                addr_of_mut!(DATA_OUT_BUFFER.0).cast::<u8>(),
                MAX_PACKET_SIZE as usize,
            );
        }
        if GSI_GADGET_BOUND {
            GadgetDriver::on_gsi_channel_resume(gadget_mut());
        }
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::Resume);
        if ENDPOINTS_READY && !rearm_setup() {
            return false;
        }
        return true;
    }
    false
}

/// Take over the USB controller without resetting the PHY or clock branches.
/// Fastboot has already completed that hardware bring-up; resetting those
/// blocks during a `fastboot boot` handoff can remove the Type-C pull-up before
/// the new gadget has a chance to enumerate.
pub fn init_usb2_handoff() -> bool {
    // The first attempt must preserve Fastboot's secure-owned rails, clocks,
    // RPMh vote, and Type-C session. Reprogramming those resources underneath
    // the vendor controller can remove the pull-up before EP0 is ready.
    //
    // One thing this handoff CANNOT preserve is Fastboot's RPMh/interconnect
    // vote itself: it dies with the bootloader's exit, and ~25 seconds later
    // the USB clock branch collapses under the idle timer — every MMIO read
    // then faults with an asynchronous external abort and the exception
    // vector reboots the handset in the middle of host enumeration. Reassert
    // Fullerene's own votes up front (best-effort; the secure side may reject
    // individual transitions without making the handoff impossible).
    unsafe {
        let performance = super::platform::bramble::usb_performance_state(
            super::platform::bramble::UsbBusVote::Nominal,
        );
        if !super::platform::bramble::apply_usb_power(true, false) {
            log_puts("usb: RPMh USB PHY regulator vote unavailable; continuing\n");
        }
        let _ = super::platform::bramble::enable_usb30_gdsc();
        let _ = super::platform::bramble::apply_usb_performance(performance.vote);
        let _ = super::platform::bramble::usb_bus_vectors(performance.vote);
    }
    if init_with_super_speed(false, true, false) {
        return true;
    }

    // Only after the non-destructive handoff fails do we attempt the complete
    // Qualcomm platform sequence. The caller may then use the cold USB2 path
    // as an explicit diagnostic of missing platform ownership.
    init_usb2_gadget_handoff()
}

/// Connect only the physical USB2 pull-up during a Fastboot handoff.
///
/// This diagnostic intentionally does not touch the event ring, endpoint
/// commands, or SMMU. It answers the narrower hardware question first: can
/// the Qualcomm PHY and DWC3 device controller make the port visible after
/// the bootloader disconnects? A host may report an incomplete USB device
/// because EP0 is not configured; that is expected for this probe.
pub fn init_usb2_pullup_handoff() -> bool {
    unsafe {
        log_hex("usb pullup: DWC3 GSNPSID=", read(GSNPSID) as u64);

        // Qualcomm's glue asserts LANE0_PWR_PRESENT together with the HS
        // VBUS/session override when entering peripheral mode. This is also
        // required on the USB2-only handoff path; it is not gated on QMP PHY
        // calibration in the Linux role-switch path.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);

        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);

        if !device_soft_reset() {
            log_puts("usb pullup: DWC3 device reset failed\n");
            return false;
        }
        configure_dwc3_global_control();

        // This probe also resets the controller, so mirror the Qualcomm
        // post-reset callback before asking Run/Stop to reconnect.
        select_utmi_pipe_clock();
        update_dwc3_ref_clock();

        qscratch_set(
            QSCRATCH_SS_PHY_CTRL,
            1 << 24, // LANE0_PWR_PRESENT
        );
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);
        // Fastboot handoff skips the cold-start clock setup above, but a
        // USB2-only device still needs Qualcomm's UTMI-as-PIPE selection.
        // Linux performs this during the DWC3 post-reset callback.
        select_utmi_pipe_clock();

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);

        write(DCFG, DCFG_HIGHSPEED);
        write(DALEPENA, 0b11);
        // Fastboot leaves the USB2 link in its old negotiated state. Apply
        // the upstream RxDetect workaround only when GSNPSID identifies a
        // DWC3 revision for which that workaround is specified.
        // Keep the Qualcomm glue's VBUS/session override adjacent to the
        // connect transition, matching dwc3_qcom_run_stop_notifier().
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        if unsafe { run_stop_device(true) } {
            log_puts("usb pullup: DWC3 RUN/STOP active\n");
            return true;
        }
        log_hex("usb pullup: DWC3 remained halted, DSTS=", read(DSTS) as u64);
        false
    }
}

/// Perform only the writes needed to request a USB2 device pull-up.
///
/// This is intentionally a last-resort diagnostic. It avoids UART, DWC3
/// reset, event rings, endpoint commands, and SMMU access. The QSCRATCH VBUS
/// writes still use the Qualcomm glue's read-modify-write/readback sequence;
/// that ordering is part of the physical connect contract. If this does not
/// make the phone visible on the host, the failure is below the normal gadget
/// path: entry/exception handling, the Qualcomm USB glue, the PHY/session
/// state, or the bootloader's USB handoff itself.
unsafe fn init_usb2_bare_pullup_handoff_inner(connect: bool) -> bool {
    unsafe {
        // Match dwc3_qcom_vbus_override_enable(): the Qualcomm glue asserts
        // both the SuperSpeed lane power-present vote and the USB2
        // VBUS/session override, even when the gadget is intentionally
        // limited to USB2.  Writing only HS_PHY_CTRL leaves the role change
        // incomplete on platforms whose Type-C glue gates the pull-up with
        // the SS-side vote.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        enable_power_events();
        // Fastboot may leave the core in the USB2 suspended state when it
        // tears down its gadget just before jumping to the temporary image.
        // Waking the UTMI block is still below the EP0/DMA boundary and is
        // required before DCTL.Run/Stop can produce a new pull-up.
        let mut usb2 = read_volatile(reg(GUSB2PHYCFG0));
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write_volatile(reg(GUSB2PHYCFG0), usb2);
        let mut usb3 = read_volatile(reg(GUSB3PIPECTL0));
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write_volatile(reg(GUSB3PIPECTL0), usb3);
        let general = read_volatile(qscratch_reg(QSCRATCH_GENERAL_CFG));
        write_volatile(
            qscratch_reg(QSCRATCH_GENERAL_CFG),
            general | QSCRATCH_GENERAL_CFG_XHCI_REV,
        );
        // The bare path intentionally skips DWC3 reset, but it still needs
        // the Qualcomm glue's UTMI-as-PIPE clock selection when the Fastboot
        // session did not leave that mux configured for the temporary image.
        select_utmi_pipe_clock();

        let gctl = read_volatile(reg(GCTL));
        write_volatile(
            reg(GCTL),
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );
        configure_dwc3_global_control();
        write_volatile(reg(DCFG), DCFG_HIGHSPEED);
        // Linux disables endpoint advertising before stopping the device
        // controller.  In a Fastboot reuse this also prevents a stale EP0
        // resource from receiving a transaction while Run/Stop is draining.
        write_volatile(reg(DALEPENA), if connect { 0b11 } else { 0 });

        // Qualcomm's glue reasserts the VBUS override immediately before
        // enabling RUN_STOP so a stale Fastboot session cannot suppress the
        // connect-done transition.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // A gadget handoff uses the same proven PHY/session preparation but
        // keeps Run/Stop clear until its event ring and EP0 commands are
        // ready. The standalone bare probe requests the pull-up immediately.
        if connect {
            // The bare probe intentionally omits endpoint setup, but it still
            // uses the same PHY-safe Run/Stop boundary as Linux.
            run_stop_device(true)
        } else {
            run_stop_device(false)
        }
    }
}

pub fn init_usb2_bare_pullup_handoff() -> bool {
    unsafe { init_usb2_bare_pullup_handoff_inner(true) }
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn init_usb2_gadget_reuse_fastboot_ep0() -> bool {
    // Re-establish the Qualcomm PHY/session state without asserting the
    // pull-up yet.  EP0 must be fully configured, its event ring published,
    // and the first SETUP TRB armed before Run/Stop is allowed to advertise
    // the device; otherwise the host can issue the first descriptor request
    // while the handoff is still rebuilding DWC3 state.
    // Stage 1 is deliberately before even the initial stop/readback: it is
    // the control experiment against the already-proven bare pull-up path.
    if unsafe { stop_after_gadget_handoff_stage(1) } {
        return true;
    }
    if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma) {
        // Capture the address while Fastboot still owns the controller. The
        // no-SMMU differential deliberately preserves that firmware stream
        // mapping; changing the address to the linker section would defeat
        // this experiment before the first STARTTRANSFER.
        let event_address =
            (read_volatile(reg(GEVNTADRHI0)) as u64) << 32 | read_volatile(reg(GEVNTADRLO0)) as u64;
        if event_address == 0
            || event_address == u64::MAX
            || event_address & 0xfff != 0
            || event_address > usize::MAX as u64
        {
            log_puts("usb gadget handoff: Fastboot event DMA address invalid\n");
            trace_event(
                TRACE_FASTBOOT_EVENT_DMA,
                event_address as u32,
                (event_address >> 32) as u32,
                0,
                0,
                read(DSTS),
            );
            return gadget_handoff_fail(1);
        }
        FASTBOOT_EVENT_DMA_BASE = event_address;
        log_hex(
            "usb gadget handoff: reusing Fastboot event DMA=",
            event_address,
        );
        trace_event(
            TRACE_FASTBOOT_EVENT_DMA,
            event_address as u32,
            (event_address >> 32) as u32,
            FASTBOOT_EP0_EVENT_SIZE as u32,
            1,
            read(DSTS),
        );
    }
    if !unsafe { init_usb2_bare_pullup_handoff_inner(false) } {
        // Fastboot can leave DSTS.DEVCTRLHLT stale while the device session
        // is already quiescent. The DWC3 device soft reset below is the real
        // endpoint-resource ownership boundary and clears that state before
        // any Fullerene TRB is published. Keep the failed stop readback in
        // the retained trace/log, but do not discard an otherwise recoverable
        // handoff before reaching the reset that Linux performs next.
        log_puts(
            "usb gadget handoff: pre-reset halt readback timed out; continuing to device reset\n",
        );
        trace_event(TRACE_DWC3_HALT_TIMEOUT, 0, 0, 0, 0, read(DSTS));
    }
    // Fastboot may have stopped Run/Stop, but that is not the same ownership
    // boundary as Linux's DWC3 probe.  The default path terminates its
    // endpoint-resource epoch with a device core soft reset.  The explicit
    // preserve-core differential keeps that reset out of the experiment while
    // retaining the preceding halted-controller boundary; this tests whether
    // the reset itself destroys the live Qualcomm PHY/session handoff.
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core) {
        if !unsafe { device_soft_reset() } {
            log_puts("usb gadget handoff: DWC3 device reset failed\n");
            return gadget_handoff_fail(2); // core reset
        }
    } else {
        trace_marker(TRACE_DWC3_RESET_BEGIN, 0x50524553); // "PRES"
        log_puts("usb gadget handoff: preserving DWC3 core state\n");
    }
    if unsafe { stop_after_gadget_handoff_stage(2) } {
        return false;
    }
    unsafe { configure_dwc3_global_control() };
    // The halted-controller boundary above transfers DMA ownership from the
    // old Fastboot session.  Clear every linker-owned TRB/event/table object
    // only after that boundary, then seed the allocator used by a later
    // GSI/UDC bind; clearing it before the stop could race a final bootloader
    // DMA write.
    clear_dma_memory();
    unsafe {
        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);
        // CSFTRST restores the controller-side PHY mux/timing state on
        // DWC3 revisions used by Bramble. Reapply the Qualcomm controller
        // programming before any endpoint command. In the preserve-core
        // differential these writes are deliberately retained as the common
        // post-halt handoff sequence; only CSFTRST itself is omitted.
        select_utmi_pipe_clock();
        update_dwc3_ref_clock();
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);
    }
    // DWC3's device reset does not reset the external Femto PHY.  Reapply the
    // Linux USB2 PHY programming at the same post-reset boundary as the normal
    // Qualcomm glue path, without asserting the GCC/Type-C power-domain reset.
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core) {
        unsafe { init_hsphy() };
    }

    // The Fastboot session may have left the DWC3 stream behind an SMMU
    // mapping that only covers its own buffers.  Our TRBs/event ring are
    // intentionally identity-addressed in the 0x9b800000 DMA section.  Keep
    // the proven PHY/pull-up transition first, then install the identity map
    // before handing any new DMA object to DWC3.
    let smmu_ready = if cfg!(fullerene_aarch64_usb_gadget_handoff_no_smmu) {
        // Differential mode for a Fastboot-owned bypass: do not even read
        // the Apps-SMMU registers. The DMA section remains fixed inside the
        // declared Bramble pool, so this mode is valid only when firmware
        // leaves the DWC3 stream in physical=IOVA bypass.
        log_puts("usb gadget handoff: Apps SMMU untouched\n");
        true
    } else {
        configure_dwc3_smmu()
    };
    if !smmu_ready {
        log_puts("usb gadget handoff: DWC3 SMMU pool map unavailable\n");
        return gadget_handoff_fail(3); // SMMU
    }
    if unsafe { stop_after_gadget_handoff_stage(3) } {
        return false;
    }

    let event_address = unsafe { ep0_event_address() };
    unsafe {
        // Reusing the bootloader's DMA context must not expose stale event
        // words from the previous Fastboot session to the polled consumer.
        let event_size = ep0_event_size();
        let event_words = ep0_event_dma_base() as *mut u32;
        for index in 0..(event_size / core::mem::size_of::<u32>()) {
            write_volatile(event_words.add(index), 0);
        }
        core::ptr::write_bytes(ep0_setup_ptr(), 0, 8);
        core::ptr::write_bytes(
            ep0_trb_ptr(0).cast::<u8>(),
            0,
            2 * core::mem::size_of::<Trb>(),
        );
        core::ptr::write_bytes(ep0_response_ptr(), 0, 512);
        cache_clean(ep0_event_dma_base(), event_size);
        cache_clean(ep0_setup_ptr() as usize, 8);
        cache_clean(ep0_trb_ptr(0) as usize, 2 * core::mem::size_of::<Trb>());
        cache_clean(ep0_response_ptr() as usize, 512);
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, event_size as u32);
        acknowledge_ep0_event_count();
        trace_event(
            TRACE_EVENT_RING_READY,
            event_address as u32,
            (event_address >> 32) as u32,
            EVENT_BUFFER_SIZE as u32,
            0,
            0,
        );
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && !configure_gsi_event_buffers() {
            log_puts("usb: Qualcomm GSI event buffers unavailable\n");
        }
        EVENT_OFFSET = 0;
        GSI_EVENT_OFFSETS = [0; 3];
        GSI_PENDING = [false; 3];
        GSI_CHANNEL_ENDPOINT = [0; 3];
        GSI_CHANNEL_READY = [false; 3];
        GSI_REQUEST_SLOTS = [usize::MAX; 3];
        GSI_RING_BASES = [0; 3];
        GSI_RING_TRB_COUNTS = [0; 3];
        GSI_BUFFER_BASES = [0; 3];
        GSI_BUFFER_LENGTHS = [0; 3];
        GSI_DOORBELL_BASES = [0; 3];
        GSI_RESOURCE_INDEX = [0; 3];
        GSI_RING_ACTIVE = [false; 3];
        RESUME_PENDING = false;
        USB_IN_P3 = false;
        GadgetDriver::reset(gadget_mut());
        udc_mut().reset();
        EP0_STATE = Ep0State::Setup;
        EP0_SETUP_ARMED = false;
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        EP0_RESOURCE_INDEX = [0; 2];
        GSI_GADGET_BOUND = false;
        FUNCTION_BOUND = false;
        // The core reset above invalidates Fastboot's endpoint configuration
        // and transfer resources. Rebuild both control directions from the
        // INIT state; this is the same ownership boundary used by Linux.
        ENDPOINTS_READY = false;
        // Fastboot's handoff requires a known USB2 device-mode speed while
        // the endpoint resources are rebuilt. The final Run/Stop boundary
        // still reapplies the old-DWC3 speed workaround immediately before
        // connection, but leaving this intermediate state unspecified loses
        // the physical attach on Bramble.
        write(DCFG, DCFG_HIGHSPEED);
        configure_gadget_start_defaults();
        // Linux enables each endpoint only after its SETEPCONFIG and
        // SETTRANSFRESOURCE commands complete. Do not advertise EP0 before
        // the controller has accepted the corresponding resource state.
        write(DALEPENA, 0);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT
                | DEVTEN_USB_RESET
                | DEVTEN_CONNECT_DONE
                | DEVTEN_LINK_STATUS_CHANGE
                | DEVTEN_WAKEUP
                | DEVTEN_HIBERNATION_REQUEST
                | DEVTEN_SUSPEND
                | DEVTEN_ERRATIC_ERROR
                | DEVTEN_CMD_COMPLETE
                | DEVTEN_OVERFLOW,
        );
        // DEPSTARTCFG(0) opens a new endpoint-resource allocation window.
        // SETEPCONFIG(INIT) then allocates one resource per EP0 direction.
        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            log_puts("usb gadget handoff: DEPSTARTCFG failed\n");
            return gadget_handoff_fail(4); // resource window
        }
        if stop_after_gadget_handoff_stage(4) {
            return false;
        }
        // Android's msm DWC3 glue allocates transfer resources for the
        // available endpoints immediately after DEPSTARTCFG, before issuing
        // SETEPCONFIG. Keep this ordering as an explicit Bramble differential;
        // the upstream Linux ordering remains the default path elsewhere.
        if cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
        {
            // Android's msm driver walks dwc->eps[] rather than only the
            // endpoints that the current gadget will expose. The msm-4.19
            // implementation loops over ALL DWC3_ENDPOINTS_NUM (32) hardware
            // endpoints right after DEPSTARTCFG and before any SETEPCONFIG,
            // so mirror that exactly instead of trusting a GHWPARAMS3 field
            // encoding that may not match this core.
            for endpoint in 0..32u32 {
                if !send_ep_command(endpoint as usize, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0) {
                    log_puts("usb gadget handoff: Android resource preallocation failed\n");
                    return gadget_handoff_fail(5); // resource allocation
                }
            }
        }
        if !unsafe {
            configure_endpoint_config(
                0,
                INITIAL_EP0_MAX_PACKET_SIZE,
                DEPCFG_EP_TYPE_CONTROL,
                false,
                0,
            )
        } {
            log_puts("usb gadget handoff: USB2 EP0 OUT configure failed\n");
            return gadget_handoff_fail(5); // EP0 config
        }
        if stop_after_gadget_handoff_stage(9) {
            return false;
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
            && !send_ep_command(0, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0)
        {
            log_puts("usb gadget handoff: USB2 EP0 OUT resource failed\n");
            return gadget_handoff_fail(5); // EP0 config
        }
        if stop_after_gadget_handoff_stage(10) {
            return false;
        }
        write(DALEPENA, read(DALEPENA) | (1 << 0));
        // Stage 8 isolates the first SETEPCONFIG/SETTRANSFRESOURCE pair from
        // the corresponding EP0 IN pair. It is intentionally appended to the
        // original 1..7 sequence so existing stage numbers remain stable.
        if stop_after_gadget_handoff_stage(8) {
            return false;
        }
        if !configure_endpoint(1, INITIAL_EP0_MAX_PACKET_SIZE, false) {
            log_puts("usb gadget handoff: USB2 EP0 configure failed\n");
            return gadget_handoff_fail(5); // EP0 config
        }
        write(DALEPENA, read(DALEPENA) | (1 << 1));
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, 64, false);
        let _ = udc_mut().configure_endpoint(1, 64, false);
        if stop_after_gadget_handoff_stage(5) {
            return false;
        }
        trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, read(DSTS));
        prepare_trb(0, ep0_setup_ptr(), 8, TRB_CONTROL_SETUP);
        // Stage 11 isolates the cache-cleaned SETUP buffer/TRB publication
        // from the DWC3 STARTTRANSFER command itself. The old stage 6
        // combined both operations, so a failure there could not tell us
        // whether the DMA object or the command latch was the boundary.
        if stop_after_gadget_handoff_stage(11) {
            return false;
        }
        if !start_transfer(0, ep0_trb_ptr(0)) {
            log_puts("usb gadget handoff: SETUP STARTTRANSFER failed\n");
            return gadget_handoff_fail(12); // STARTTRANSFER
        }
        // Record the armed SETUP TRB so the USB-reset handler takes the
        // Linux-equivalent keep-the-TRB path instead of tearing it down and
        // racing the host's first post-reset SETUP token.
        EP0_SETUP_ARMED = true;
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
            enable_gadget_controller_irq();
        }
        // Linux enables the DWC3 event interrupt immediately after arming the
        // EP0 OUT SETUP TRB. The probe owns no asynchronous IRQ path yet, so
        // drain the ring once synchronously at the same boundary. This keeps
        // an early XFER_NOT_READY/command event from waiting until after the
        // final Run/Stop transition.
        poll_ep0_event_ring();
        // The Android downstream Bramble driver leaves the USB2 PHY wake
        // bits in the state restored by the endpoint command helper here.
        // Mainline Linux later adds an explicit dwc3_enable_susphy(true),
        // but the stage-11 control experiment shows that this older Android
        // boundary is the one that still reaches the physical pull-up.
        // Stage 12 is immediately after STARTTRANSFER completion and before
        // the final VBUS/session + Run/Stop transition.
        if stop_after_gadget_handoff_stage(12) {
            return false;
        }
        if stop_after_gadget_handoff_stage(6) {
            return false;
        }

        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        configure_gadget_speed(false);
        let start_readback_ok = unsafe { run_stop_device(true) };
        if !start_readback_ok {
            // Some Fastboot/DWC3 handoffs keep DSTS.DEVCTRLHLT stale even
            // after the Run/Stop write has reached the controller. The
            // endpoint resources and first SETUP TRB are already published
            // at this point, so discarding the handoff solely because the
            // status poll did not observe the transition would hide the
            // same physical pull-up/EP0 behaviour this probe is measuring.
            // Keep the timeout in retained trace and let host traffic decide
            // whether the controller is actually usable.
            log_puts("usb gadget handoff: DWC3 RUN/STOP readback timed out; continuing\n");
            trace_event(TRACE_DWC3_HALT_TIMEOUT, 0, 0, 0, 0, read(DSTS));
        }
        if stop_after_gadget_handoff_stage(7) {
            return false;
        }
        // The probe's Type-C observer establishes Powered/Attached before
        // this point, so record the same UDC-start boundary that the normal
        // Qualcomm gadget path records. If PMIC observation was unavailable
        // this is intentionally a no-op in the state machine, but it must
        // not block EP0 testing.
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::ControllerStarted);
        return true;
    }
}

/// Reuse the physical USB2 handoff, then add the minimum DWC3 gadget state
/// needed to answer USB control transfers. The PHY and Qualcomm session
/// remain untouched; this is the early Bramble handoff path and is also
/// usable as a standalone probe.
pub fn init_usb2_gadget_handoff() -> bool {
    unsafe {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
        return init_with_super_speed(true, true, false);

        #[cfg(all(
            fullerene_aarch64_usb_gadget_handoff_probe,
            not(fullerene_aarch64_usb_gadget_handoff_super_speed)
        ))]
        return init_usb2_gadget_reuse_fastboot_ep0();

        // The bare probe is the proven physical baseline on Bramble. Start
        // the gadget diagnostic from that exact pull-up sequence, then add
        // EP0 state on top of it. This makes a failure in the gadget setup
        // observable instead of hiding the already-working link behind a
        // second, subtly different pre-connect sequence.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if !init_usb2_bare_pullup_handoff_inner(true) {
            return false;
        }
        trace_event(TRACE_INIT, 0, 0, 0, 0, 0);
        let snpsid = read(GSNPSID);
        trace_event(TRACE_INIT, 0, 0, 0, 0, snpsid);
        // Keep the Qualcomm session valid while the DWC3 device state is
        // rebuilt. The physical handoff above is deliberately first so the
        // probe preserves the working Bramble reconnect contract; the soft
        // reset below then clears the old Fastboot endpoint state before the
        // complete gadget is connected again.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        let gctl = read(GCTL);
        write(
            GCTL,
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );

        // Fastboot leaves the DWC3 device controller running while its host
        // endpoint is torn down. After the proven PHY/session preparation,
        // follow Linux's soft-connect order and reset the device state before
        // issuing endpoint commands. The gadget probe intentionally omits
        // stop_running_device(): the bare preparation already cleared
        // Run/Stop and that extra ownership transition was the earlier
        // pre-pull-up failure point.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if !device_soft_reset() {
            log_puts("usb gadget handoff: DWC3 device reset failed\n");
            return false;
        }
        configure_dwc3_global_control();
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
        if !stop_running_device() || !device_soft_reset() {
            log_puts("usb gadget handoff: DWC3 reset failed\n");
            return false;
        }
        configure_dwc3_global_control();

        // The fallback also performs a DWC3 reset, so it must receive the
        // same post-reset UTMI/ref-clock programming as the normal handoff
        // path. The earlier bare pull-up sequence may have selected UTMI,
        // but CSFTRST invalidates that controller-side mux state.
        select_utmi_pipe_clock();
        update_dwc3_ref_clock();

        // The bootloader can leave the USB2 core in suspend/LPM state even
        // though the Type-C session is valid. Reapply only the
        // controller-side wakeup bits; do not reset the PHY or clocks.
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);

        // DWC3 has been stopped/reset above, so the fallback may establish
        // the same DMA ownership boundary as the normal path. This is
        // essential when Fastboot's stream mapping covered only its own
        // buffers and not the Fullerene linker-reserved DMA section.
        if configure_dwc3_smmu() {
            log_puts("usb gadget handoff: DWC3 SMMU DMA-pool map ready\n");
        } else {
            log_puts("usb gadget handoff: DWC3 SMMU DMA-pool map unavailable\n");
            return false;
        }

        // The linker-reserved region is identity-mapped by the early AArch64
        // MMU path. Clean it for the same handoff ordering whether this entry
        // is reached from the standalone probe or from the normal kernel.
        let event_address = ep0_event_address();
        cache_clean(ep0_event_dma_base(), ep0_event_size());
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, ep0_event_size() as u32);
        acknowledge_ep0_event_count();
        trace_event(
            TRACE_EVENT_RING_READY,
            event_address as u32,
            (event_address >> 32) as u32,
            EVENT_BUFFER_SIZE as u32,
            0,
            0,
        );
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && !configure_gsi_event_buffers() {
            log_puts("usb: Qualcomm GSI event buffers unavailable\n");
        }
        EVENT_OFFSET = 0;
        GSI_EVENT_OFFSETS = [0; 3];
        GSI_PENDING = [false; 3];
        GSI_CHANNEL_ENDPOINT = [0; 3];
        GSI_CHANNEL_READY = [false; 3];
        GSI_REQUEST_SLOTS = [usize::MAX; 3];
        GSI_RING_BASES = [0; 3];
        GSI_RING_TRB_COUNTS = [0; 3];
        GSI_BUFFER_BASES = [0; 3];
        GSI_BUFFER_LENGTHS = [0; 3];
        GSI_DOORBELL_BASES = [0; 3];
        GSI_RESOURCE_INDEX = [0; 3];
        GSI_RING_ACTIVE = [false; 3];
        RESUME_PENDING = false;
        USB_IN_P3 = false;
        GadgetDriver::reset(gadget_mut());
        udc_mut().reset();
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        GSI_GADGET_BOUND = false;
        FUNCTION_BOUND = false;
        ENDPOINTS_READY = false;

        write(DCFG, DCFG_HIGHSPEED);
        configure_gadget_start_defaults();
        write(DALEPENA, 0);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT
                | DEVTEN_USB_RESET
                | DEVTEN_CONNECT_DONE
                | DEVTEN_LINK_STATUS_CHANGE
                | DEVTEN_WAKEUP
                | DEVTEN_HIBERNATION_REQUEST
                | DEVTEN_SUSPEND
                | DEVTEN_ERRATIC_ERROR
                | DEVTEN_CMD_COMPLETE
                | DEVTEN_OVERFLOW,
        );

        // Drain any power event latched by the Fastboot teardown BEFORE the
        // endpoint commands: a pending PWR event keeps the core's clock/RAM
        // domain gated on this glue, which shows up as SETEPCONFIG /
        // STARTTRANSFER failing or wedging. The full handoff path calls
        // enable_power_events() and its poll loop clears the status; the
        // fallback must do the same synchronously.
        enable_power_events();
        service_power_event();

        // DWC3's device-start contract is: reserve the endpoint resources,
        // configure both directions of EP0, queue the first SETUP TRB, then
        // assert Run/Stop. Without this sequence the PHY can advertise a
        // USB2 pull-up while every host descriptor request times out at EP0.
        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0)
            || !configure_endpoint(0, 64, false)
            || !configure_endpoint(1, 64, false)
        {
            log_puts("usb gadget handoff: EP0 configuration failed\n");
            return false;
        }
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, 64, false);
        let _ = udc_mut().configure_endpoint(1, 64, false);
        write(DALEPENA, 0b11);
        if !start_setup() {
            log_puts("usb gadget handoff: SETUP STARTTRANSFER failed\n");
            return false;
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
            enable_gadget_controller_irq();
        }
        // Mirror Linux's post-ep0_out_start IRQ window before connecting the
        // device. In this early probe the equivalent is a bounded synchronous
        // event-ring drain; platform service remains outside this boundary.
        poll_ep0_event_ring();

        // Connect only after the event ring, transfer resources, EP0
        // descriptors, and first SETUP TRB are ready. This produces a fresh
        // USB2 attach without exposing an EP0-less device to the host.
        // Reassert the Qualcomm VBUS/session vote immediately before the
        // final Run/Stop write; this is the glue driver's pre_run_stop hook.
        configure_gadget_speed(false);
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        if !run_stop_device(true) {
            log_puts("usb gadget handoff: DWC3 RUN/STOP timeout\n");
            #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
            return gadget_handoff_fail(7); // Run/Stop
            #[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
            return false;
        }

        log_puts("usb gadget handoff: EP0 running\n");
        true
    }
}

fn init_with_super_speed(super_speed: bool, reset_core: bool, reset_platform: bool) -> bool {
    unsafe {
        QMP_PHY_READY = false;
        // The DWC3 stream is unattributed at the Apps-SMMU (ladder 252), and
        // Qualcomm firmware commonly leaves sCR0.WACFG set to stall+queue:
        // every DWC3 DMA then hangs in the SMMU while GEVNTCOUNT keeps
        // counting the core-internal event FIFO, which masquerades as a
        // working event ring. Rewriting SMR/S2CR from non-secure state did
        // not lift the stall, so clear the whole warning configuration and
        // take the SMMU out of the path entirely. This must happen before
        // any DWC3 DMA is armed. A rejected (secure-owned) write fails the
        // attempt so the host-visible attach names the outcome.
        #[cfg(fullerene_aarch64_usb_smmu_disable)]
        {
            let scr0 = read_volatile(smmu_reg(SMMU_GR0_SCR0));
            if scr0 == u32::MAX {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x5344_5242); // "SDRB"
                log_puts("usb: SMMU SCR0 unreadable; cannot disable\n");
                return false;
            }
            // sCR0.SMMUEN (bit 0) off, sCR0.CLIENTPD (bit 1) set, and
            // sCR0.WACFG (bits 7:6) = 00 (unattributed transactions pass).
            let new_scr0 = (scr0 & !0x1 & !(0b11 << 6)) | 0x2;
            write_volatile(smmu_reg(SMMU_GR0_SCR0), new_scr0);
            core::arch::asm!("dsb sy", options(nostack));
            let readback = read_volatile(smmu_reg(SMMU_GR0_SCR0));
            let ok = readback == new_scr0;
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x5344_4953,
                scr0,
                new_scr0,
                readback,
                ok as u32,
            );
            if !ok {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x5344_524A); // "SDRJ"
                log_puts("usb: SMMU disable rejected; suppressing pull-up\n");
                return false;
            }
        }
        // Harvest the previous attempt's STARTTRANSFER outcome before this
        // attempt's DMA-region clear wipes the trace. Attempt 1 skips the
        // harvest (the previous boot's trace was destroyed by Android).
        INIT_CALLS = INIT_CALLS.wrapping_add(1);
        if INIT_CALLS > 1 {
            harvest_trace_outcome();
        }
        // Reset the adopted-mapping state on every handoff attempt: a failed
        // attempt must not leave the next attempt publishing stale objects.
        DMA_ADOPTED = false;
        DMA_ADOPTED_CPU = 0;
        DMA_ADOPTED_IOVA = 0;
        // Read the bootloader's Apps-SMMU state and event-ring IOVA while
        // Fastboot still owns the controller. When the stream sits in a live
        // TRANSLATE context that software cannot rewrite, the EP0 DMA
        // objects are relocated into a page that context already maps.
        #[cfg(fullerene_aarch64_usb_ep0_dma_adopt)]
        {
            let adopted = adopt_smmu_dma_mapping();
            trace_event(
                TRACE_SMMU_HANDOFF,
                adopted.is_some() as u32,
                0,
                0,
                0,
                read(DSTS),
            );
        }
        if !super::platform::bramble::usb_power_contract_valid(super_speed) {
            if reset_platform {
                // A cold platform start actually re-applies the contract below,
                // so an invalid contract is fatal there.
                log_puts("usb: DT power contract invalid\n");
                return false;
            }
            // The non-destructive handoff preserves the bootloader's live
            // rails/clocks and never re-applies the contract (apply_usb_power
            // below is gated on reset_platform). The rails are empirically
            // powered (the device attaches), so a contract the fastboot DT
            // does not fully expose is not fatal for the handoff.
            log_puts("usb: DT power contract incomplete; preserving firmware state\n");
        }
        INIT_STAGE = 1;
        let performance = super::platform::bramble::usb_performance_state(
            super::platform::bramble::UsbBusVote::Nominal,
        );
        let bus_vectors = super::platform::bramble::usb_bus_vectors(performance.vote);
        log_hex("usb: nominal core clock=", performance.core_rate_hz as u64);
        log_hex(
            "usb: PM QoS latency us=",
            performance.pm_qos_latency_us as u64,
        );
        log_hex("usb: interconnect paths=", bus_vectors.len() as u64);
        // Select the RCG source before enabling its branch clocks and before
        // publishing the corresponding interconnect vote.  Handoff mode
        // intentionally skips this write because Fastboot owns a live clock
        // domain that must not be retuned underneath the controller.
        if reset_platform {
            if !super::platform::bramble::apply_usb_power(true, super_speed) {
                log_puts("usb: RPMh USB PHY regulator contract unavailable\n");
                return false;
            }
            if !super::platform::bramble::enable_usb30_gdsc() {
                // Some Pixel bootloaders keep the GDSC under secure/RPMh
                // ownership. Treat this as a non-fatal ownership warning.
                log_puts("usb: USB3 GDSC PWR_ON not observable; preserving vote\n");
            }
            if !super::platform::bramble::apply_usb_performance(performance.vote) {
                // A cold platform start may not have an idle Apps-RSC TCS or
                // may reject a GCC update. Preserve the firmware vote/rate
                // rather than issuing a partial secure-owned transaction.
                log_puts(
                    "usb: nominal clock/interconnect transition unavailable; preserving firmware state\n",
                );
            }
        }
        let snpsid = read(GSNPSID);
        log_hex("usb: DWC3 GSNPSID=", snpsid as u64);

        // The Linux lito-usb device tree supplies these clocks and resets to
        // the Qualcomm glue.  A RAM-booted Fullerene image has no clock
        // framework yet, so perform the small branch/reset part directly.
        let mut qmp_ready = if reset_platform {
            let _ = super::platform::bramble::enable_usb_clock_branches();
            let _ = super::platform::bramble::reset_usb_blocks(super_speed);

            init_hsphy();
            if super_speed { init_qmp_phy() } else { false }
        } else {
            false
        };
        QMP_PHY_READY = qmp_ready;
        // Match the QCOM DWC3 glue's peripheral-mode VBUS override.  The
        // bootloader's fastboot role is not a complete kernel-side OTG
        // session, so relying on the core alone leaves the device halted.
        // The Qualcomm glue asserts the SS-side lane power-present vote even
        // for a USB2-only session; it is the shared Type-C VBUS override path,
        // not a claim that SuperSpeed training completed.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // The legacy Qualcomm DWC3 glue enables the master clocks for the
        // controller RAMs here. Without these votes, DWC3 clock gating can
        // shut the RAM interface off even though the core and PHY clocks are
        // running, leaving the event ring and endpoint commands invisible.
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        enable_power_events();
        // Select peripheral mode before issuing the device soft reset. The
        // DCTL.CSFTRST handshake is only defined while the core is in device
        // capability mode; fastboot may have left the port in host/OTG mode.
        let mut gctl = read(GCTL);
        gctl &= !GCTL_PRTCAPDIR_MASK;
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);
        // Capture the previous owner's RAM clock select BEFORE any reset:
        // CSFTRST and the host's bus USB reset both clear GCTL.RAMCLKSEL,
        // and with the wrong select the internal endpoint RAM misroutes
        // writes, which is exactly the "No resource" STARTTRANSFER failure.
        RAMCLK_CAPTURE = gctl_ramclksel(read(GCTL));
        trace_event(
            TRACE_DWC3_REVISION_QUIRK,
            0x5243_4150,
            RAMCLK_CAPTURE,
            0,
            0,
            0,
        );
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_RAMCLK_GATE") {
            // One-bit readout of the previous owner's GCTL.RAMCLKSEL value.
            if let Ok(value) = want.parse::<u32>() {
                if RAMCLK_CAPTURE != value {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x5243_4700 | (RAMCLK_CAPTURE & 0xff));
                    log_puts("usb: ramclk gate mismatch; suppressing pull-up\n");
                    return false;
                }
            }
        }

        // Use the same pre-reset ownership boundary as the proven Bramble
        // gadget probe.  The helper wakes UTMI, selects the USB2 clock path,
        // clears stale endpoint advertising, and stops the old Fastboot
        // session before CSFTRST.  Repeating those writes inline here had
        // drifted from the working handoff sequence and could leave the
        // controller reset while its PHY/session state was still suspended.
        if !reset_platform && !super_speed {
            let _ = init_usb2_bare_pullup_handoff_inner(false);
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(1) {
            return true;
        }

        if !reset_core && !stop_running_device() {
            return false;
        }

        if reset_core {
            let reset_ok = if reset_platform {
                core_soft_reset(qmp_ready)
            } else if !super_speed {
                // Linux's reconnect path uses dwc3_core_soft_reset() before
                // rebuilding the event ring and EP0, even when the
                // Qualcomm PHY/clock ownership is retained by firmware.
                // For the USB2 direct handoff this resets only the DWC3
                // device core and USB2 PHY-facing state; the external QUSB2
                // rail, Type-C session, and USB3 PHY remain untouched and
                // are re-applied below.
                core_soft_reset(false)
            } else {
                device_soft_reset()
            };
            if !reset_ok {
                log_puts("usb: DWC3 reset failed\n");
                return false;
            }
        }
        configure_dwc3_global_control();
        INIT_STAGE = 2;
        // The device reset above is the ownership boundary for the previous
        // Fastboot transfer epoch. The direct probe normally enters before
        // usb_probe_entry's fallback allocator setup, so initialize the
        // linker-owned event/TRB objects here, after reset and before any
        // address is published to DWC3.
        if reset_core {
            clear_dma_memory();
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(2) {
            return true;
        }

        // Fastboot already owns the USB clocks and rails, but its QMP state
        // belongs to the old controller session. Reinitialize the combo PHY
        // after the DWC3 device reset and before publishing new DMA state.
        // This is the non-destructive handoff equivalent of the cold
        // Linux/Android QMP initialization sequence.
        if super_speed && !reset_platform {
            qmp_ready = init_qmp_phy();
            QMP_PHY_READY = qmp_ready;
            if !qmp_ready {
                log_puts("usb: Fastboot QMP SuperSpeed handoff unavailable\n");
                return false;
            }
        }

        // Fastboot leaves the USB2 PHY powered, but the DWC3 handoff can
        // clear the PHY's session-valid state while stopping the old gadget.
        // Reapply the non-destructive Femto PHY programming on the USB2
        // handoff path; this does not assert the GCC PHY reset or touch the
        // Type-C power domain.
        if !super_speed && !reset_platform {
            init_hsphy();
        }

        // Core reset restores the QSCRATCH-facing state on some DWC3
        // revisions, so re-apply the Qualcomm glue votes after reset.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // SM7250's DWC3 revision is older than 2.50a. The Qualcomm glue
        // advertises the XHCI 1.0 register layout through this QSCRATCH bit
        // during its reset callback.
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);

        // USB2-only starts need the same post-reset UTMI clock selection as
        // the Qualcomm glue. The DWC3 reset above invalidates the controller's
        // previous PIPE/UTMI selection, so this is required for handoff too;
        // it is a controller-side QSCRATCH mux change, not a PHY power reset.
        if !super_speed {
            select_utmi_pipe_clock();
        }

        // Match dwc3_msm_update_ref_clk() from the Qualcomm glue. This is a
        // controller post-reset setting, so it must also run after a
        // Fastboot handoff reset; it does not retune the GCC source clock.
        update_dwc3_ref_clock();

        // Linux/Android install the Apps-SMMU context before the DWC3 gadget
        // receives a request. A `fastboot boot` image has no IOMMU framework
        // to inherit that ownership, so the handoff must do the equivalent
        // after the old DWC3 session has been stopped/reset and before any
        // Fullerene event/TRB address is published. This is deliberately
        // performed for both cold and Fastboot paths; preserving a live
        // firmware mapping while using a different DMA pool is not a valid
        // non-destructive handoff.
        let smmu_ready = if cfg!(all(
            fullerene_aarch64_usb_gadget_handoff_probe,
            fullerene_aarch64_usb_gadget_handoff_no_smmu
        )) {
            // Keep the direct probe's no-SMMU differential meaningful: it
            // must not partially rewrite the Apps-SMMU before testing the
            // firmware-owned physical=IOVA bypass.
            trace_event(TRACE_SMMU_PRESERVED, 0, 0, 0, 0, 0);
            true
        } else {
            configure_dwc3_smmu()
        };
        trace_event(
            TRACE_SMMU_HANDOFF,
            smmu_ready as u32,
            reset_platform as u32,
            super::platform::bramble::usb_resources().dma_pool.stream_id,
            super::platform::bramble::usb_resources().dma_pool.iova_base as u32,
            super::platform::bramble::usb_resources().dma_pool.size as u32,
        );
        if smmu_ready {
            log_puts("usb: DWC3 SMMU DMA-pool map ready\n");
        } else {
            // Proceeding with an unverified IOVA map would turn the first
            // SETUP TRB into an opaque DMA fault, so let the caller choose its
            // explicit recovery/fallback path.
            log_puts(if reset_platform {
                "usb: DWC3 SMMU DMA-pool map unavailable\n"
            } else {
                "usb: Fastboot SMMU handoff map unavailable\n"
            });
            return false;
        }

        INIT_STAGE = 3;
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(3) {
            return true;
        }

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        write(GUSB2PHYCFG0, usb2);
        // Match dwc3_dis_sleep_mode(): the host-side L1 threshold helper is
        // independent of the USB2 PHY sleep bit and can survive a Fastboot
        // handoff with a stale value.
        let guctl1 = read(GUCTL1);
        write(GUCTL1, guctl1 & !GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST);
        let mut usb3 = read(GUSB3PIPECTL0);
        if qmp_ready {
            usb3 &= !GUSB3PIPECTL_SUSPHY;
        } else {
            // Keep the USB2 gadget usable if the board-specific SuperSpeed
            // calibration does not reach PHY ready.
            usb3 |= GUSB3PIPECTL_SUSPHY;
        }
        write(GUSB3PIPECTL0, usb3);

        let event_address = ep0_event_address();
        // The event ring lives in the normal-cacheable early heap mapping.
        // Evict any CPU-side zero-fill before handing the buffer to DWC3;
        // otherwise a later cache writeback could overwrite an event that the
        // controller has already posted.
        cache_clean(ep0_event_dma_base(), ep0_event_size());
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, ep0_event_size() as u32);
        acknowledge_ep0_event_count();
        trace_event(
            TRACE_EVENT_RING_READY,
            event_address as u32,
            (event_address >> 32) as u32,
            EVENT_BUFFER_SIZE as u32,
            0,
            0,
        );
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && !configure_gsi_event_buffers() {
            log_puts("usb: Qualcomm GSI event buffers unavailable\n");
        }
        EVENT_OFFSET = 0;
        GSI_EVENT_OFFSETS = [0; 3];
        GSI_PENDING = [false; 3];
        GSI_CHANNEL_ENDPOINT = [0; 3];
        GSI_CHANNEL_READY = [false; 3];
        GSI_REQUEST_SLOTS = [usize::MAX; 3];
        GSI_RING_BASES = [0; 3];
        GSI_RING_TRB_COUNTS = [0; 3];
        GSI_BUFFER_BASES = [0; 3];
        GSI_BUFFER_LENGTHS = [0; 3];
        GSI_DOORBELL_BASES = [0; 3];
        GSI_RESOURCE_INDEX = [0; 3];
        GSI_RING_ACTIVE = [false; 3];
        RESUME_PENDING = false;
        USB_IN_P3 = false;
        GadgetDriver::reset(gadget_mut());
        udc_mut().reset();
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        GSI_GADGET_BOUND = false;
        FUNCTION_BOUND = false;

        // The bootloader may leave DCFG in the speed/address state of its
        // Fastboot session. Reset both fields explicitly before enabling the
        // pull-up; Linux's gadget path selects the maximum PHY-backed speed
        // at the same point in its start sequence.
        let mut dcfg = read(DCFG) & !(DCFG_SPEED_MASK | DCFG_DEVADDR_MASK);
        // DCFG.SPEED must match a PHY the transfer engine can actually use
        // at Start Transfer time. With DCFG=SuperSpeed on a USB2-only handoff
        // (QMP absent), the SS link can never train and every EP0
        // STARTTRANSFER completes with "No resource" — the proven-working
        // fallback path programs DCFG_HIGHSPEED here and its EP0 pipeline
        // runs end to end. Linux's SuperSpeed-default convention only holds
        // when a SuperSpeed PHY is present (qmp_ready).
        dcfg |= if qmp_ready {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);
        configure_gadget_start_defaults();

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(4) {
            return true;
        }

        let depstart_ok = send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0);
        INIT_STAGE = 4;
        INIT_DEPSTART_RAW = read(dep_reg(0, 0x0c));
        INIT_DEPSTART_DSTS = read(DSTS);
        if !depstart_ok {
            log_puts("usb: DEPSTARTCFG failed\n");
            return false;
        }
        // Linux starts both physical EP0 directions with the SuperSpeed
        // packet size before the link speed is known, even when the PHY may
        // later negotiate only High-Speed. Connect Done then changes the
        // endpoint configuration to 64 bytes for USB2. Using 64 here makes
        // the direct handoff path diverge from the fallback probe precisely
        // at the first STARTTRANSFER boundary.
        let ep0_packet_size = INITIAL_EP0_MAX_PACKET_SIZE;
        let epcfg0 = configure_endpoint(0, ep0_packet_size, false);
        INIT_STAGE = 5;
        INIT_EPCFG0_OK = epcfg0;
        INIT_EPCFG0_RAW = read(dep_reg(0, 0x0c));
        INIT_EPCFG0_DSTS = read(DSTS);
        let epcfg1 = if epcfg0 {
            configure_endpoint(1, ep0_packet_size, false)
        } else {
            false
        };
        INIT_EPCFG1_OK = epcfg1;
        if epcfg0 {
            INIT_STAGE = 6;
        }
        if !epcfg0 || !epcfg1 {
            log_puts("usb: EP0 configuration failed\n");
            return false;
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(5) {
            return true;
        }
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, ep0_packet_size as u16, false);
        let _ = udc_mut().configure_endpoint(1, ep0_packet_size as u16, false);
        write(DALEPENA, 0b11);
        write(
            DEVTEN,
            DEVTEN_DISCONNECT
                | DEVTEN_USB_RESET
                | DEVTEN_CONNECT_DONE
                | DEVTEN_LINK_STATUS_CHANGE
                | DEVTEN_WAKEUP
                | DEVTEN_HIBERNATION_REQUEST
                | DEVTEN_SUSPEND
                | DEVTEN_ERRATIC_ERROR
                | DEVTEN_CMD_COMPLETE
                | DEVTEN_OVERFLOW,
        );
        trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, read(DSTS));
        prepare_trb(0, ep0_setup_ptr(), 8, TRB_CONTROL_SETUP);

        // Split the direct probe at the exact DMA publication boundary:
        // stage 6 has only written/cleaned the setup TRB, while stage 7 is
        // after the DWC3 STARTTRANSFER command has retired. This makes a
        // cache/SMMU/TRB fault distinguishable from a command-state failure.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(6) {
            return true;
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_PROBE") == Some("1") {
            // Event-DMA liveness probe. The endpoint is fully configured here
            // (DEPSTARTCFG, SETEPCONFIG, SETTRANSFRESOURCE done, TRB armed):
            // arm a real SETUP transfer on ep0 OUT and then ENDTRANSFER it
            // with CMDIOC — the exact Linux stop-active-transfer pattern, so
            // the core must post the completion event. GEVNTCOUNT > 0 proves
            // the core's event DMA reaches DRAM; gate the pull-up off when it
            // never arrives so the host-visible attach names a working DMA
            // path.
            //
            // Clear any latched Apps-SMMU faults first so the post-probe FSR
            // names only this attempt's DMA attempts.
            let fsr_before = read_volatile(smmu_reg(SMMU_GR0_FSR));
            if fsr_before != 0 && fsr_before != u32::MAX {
                write_volatile(smmu_reg(SMMU_GR0_FSR), fsr_before);
                core::arch::asm!("dsb sy", options(nostack));
            }
            // RAM readback gate: if the linker-reserved .usb_dma window is
            // not backed by real DRAM, every DMA write (event ring, TRB
            // fetch, setup buffer) vanishes and the CPU cannot detect it.
            // Write a pattern, evict it from the cache, and read it back;
            // gate the attach on the pattern surviving.
            if option_env!("FULLERENE_USB_SIGNAL_RAM_GATE") == Some("1") {
                // Verify EVERY object the controller will DMA, not just the
                // event ring: a partially backed region can pass the first
                // page while the TRB/SETUP pages hang the core's fetch.
                let mut ram_ok = true;
                let targets: [(usize, usize); 4] = [
                    (ep0_event_dma_base(), 16),
                    (ep0_trb_ptr(0) as usize, 64),
                    (ep0_setup_ptr() as usize, 8),
                    (ep0_response_ptr() as usize, 512),
                ];
                for (address, span) in targets {
                    let pattern = [0xA55A_5AA5u32, 0x1234_5678, 0xDEAD_BEEF, 0x0BAD_C0DE];
                    let words = span / 4;
                    for offset in 0..words {
                        unsafe {
                            write_volatile(
                                (address + offset * 4) as *mut u32,
                                pattern[offset % pattern.len()],
                            );
                        }
                    }
                    cache_clean(address, span);
                    cache_invalidate(address, span);
                    for offset in 0..words {
                        unsafe {
                            if read_volatile((address + offset * 4) as *const u32)
                                != pattern[offset % pattern.len()]
                            {
                                ram_ok = false;
                            }
                        }
                    }
                    for offset in 0..words {
                        unsafe { write_volatile((address + offset * 4) as *mut u32, 0) };
                    }
                    cache_clean(address, span);
                }
                trace_event(
                    TRACE_EVENT_RING_READY,
                    0x5241_4D00 | ram_ok as u32,
                    0,
                    0,
                    0,
                    0,
                );
                if !ram_ok {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x5241_4D46); // "RAMF"
                    log_puts("usb: .usb_dma readback failed; region is not usable RAM\n");
                    return false;
                }
            }
            let started = start_transfer(0, ep0_trb_ptr(0));
            let resource = if started {
                EP0_RESOURCE_INDEX[0].max(1)
            } else {
                1
            };
            let _ = send_ep_command(
                0,
                DEPCMD_ENDTRANSFER
                    | DEPCMD_CMDIOC
                    | DEPCMD_HIPRI_FORCERM
                    | ((resource as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            );
            EP0_RESOURCE_INDEX[0] = 0;
            let mut delivered = false;
            let mut event_word = 0u32;
            for _ in 0..100 {
                super::timer::delay_ms(1);
                if read(GEVNTCOUNT0) & GEVNTCOUNT_MASK != 0 {
                    delivered = true;
                    break;
                }
            }
            if delivered {
                // GEVNTCOUNT counts the core-internal event FIFO, not the
                // DMA completion. Read the ring slot the event should have
                // landed in: a zero word means the DMA write never reached
                // DRAM (stalled/blocked), which no amount of register setup
                // can mask.
                let slot = (unsafe { EVENT_OFFSET } % unsafe { ep0_event_size() }) & !0x3;
                let word = unsafe { read_volatile((ep0_event_dma_base() + slot) as *const u32) };
                event_word = word;
            }
            let fsr_after = read_volatile(smmu_reg(SMMU_GR0_FSR));
            trace_event(
                TRACE_EVENT_RING_READY,
                delivered as u32,
                event_word,
                fsr_after,
                0,
                0,
            );
            // Event-data gate: 1 = attach only when the event word actually
            // landed in DRAM, 2 = attach only when the ring slot stayed zero.
            match option_env!("FULLERENE_USB_SIGNAL_EVT_DATA_GATE") {
                Some("1") if event_word == 0 => {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4556_4430); // "EVD0"
                    log_puts("usb: event word never landed in DRAM\n");
                    return false;
                }
                Some("2") if event_word != 0 => {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4556_4431); // "EVD1"
                    log_puts("usb: event word landed but gate wanted zero\n");
                    return false;
                }
                _ => {}
            }
            // FSR gate (one bit per run): 1 = attach only when the SMMU
            // recorded a fault during the probe, 2 = attach only when it did
            // not. This separates "SMMU kills the DMA" from "the core's DMA
            // engine is dead".
            let fsr_gate = option_env!("FULLERENE_USB_SIGNAL_FSR_GATE");
            if fsr_gate == Some("1") || fsr_gate == Some("2") {
                let faulted = fsr_after != u32::MAX && fsr_after != 0;
                let wanted = fsr_gate == Some("1");
                if faulted != wanted {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4653_5200 | (fsr_after & 0xff));
                    log_puts("usb: FSR gate mismatch; suppressing pull-up\n");
                    return false;
                }
            }
            if !delivered {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x444D_4146); // "DMAF"
                log_puts("usb: event DMA probe found no delivered event\n");
                return false;
            }
            // Drain the probe events and re-arm a clean SETUP TRB so the
            // normal flow starts from the same state as a non-probe run.
            poll_ep0_event_ring();
            EVENT_OFFSET = 0;
            prepare_trb(0, ep0_setup_ptr(), 8, TRB_CONTROL_SETUP);
        }

        // Linux arms the initial EP0 OUT SETUP transfer before Run/Stop. Keep
        // that as the default, but retain a Bramble-only hardware differential
        // for controllers whose firmware handoff cannot tolerate DMA ownership
        // changing while the device is still halted. In that mode the same
        // prepared TRB is armed immediately after Run/Stop, before the host's
        // first descriptor request can be serviced.
        #[cfg(not(any(
            fullerene_aarch64_usb_gadget_handoff_start_after_connect,
            fullerene_aarch64_usb_gadget_handoff_start_after_reset,
            fullerene_aarch64_usb_gadget_handoff_start_at_connect_done
        )))]
        {
            // On this core a Start Transfer issued before the link reaches
            // ON not only fails with "No resource" but WEDGES the endpoint
            // command engine - the later Run/Stop then never publishes the
            // pull-up at all. Do not issue it here: the Connect Done handler
            // arms the SETUP TRB the moment the link comes up (which is
            // still before the host's first SETUP token), and the poll-loop
            // guard re-arms on any later reset.

            #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
            if cfg!(fullerene_aarch64_usb_gadget_handoff_direct)
                && stop_after_gadget_handoff_stage(7)
            {
                return true;
            }
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct) {
            enable_gadget_controller_irq();
        }
        // Linux starts consuming DWC3 events as soon as the initial EP0 OUT
        // SETUP transfer is armed. Do the same once before Run/Stop while the
        // early boot path is still polling rather than handling IRQs.
        poll_ep0_event_ring();

        // Use the same Linux-compatible Run/Stop preparation as the probe
        // path. In particular, do not inherit KEEP_CONNECT or the Fastboot
        // HIRD threshold across the temporary-image handoff.
        configure_gadget_speed(qmp_ready);
        enable_gadget_susphy();
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_RSC_GATE") {
            // One-bit readout of the previous attempt's SETTRANSFRESOURCE
            // raw DEPCMD register (resource index 22:16, status 15:12). A
            // healthy allocation returns 0x10000 (index 1, status 0).
            let ok = u32::from_str_radix(want.trim_start_matches("0x"), 16)
                .map(|value| TRACE_HARVEST_RSC == value)
                .unwrap_or(false);
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x5253_4300,
                TRACE_HARVEST_RSC,
                ok as u32,
                0,
                0,
            );
            if !ok {
                trace_marker(
                    TRACE_PROBE_WATCHDOG,
                    0x5253_4300 | (TRACE_HARVEST_RSC & 0xff),
                );
                log_puts("usb: resource gate mismatch; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_CFG_GATE") {
            // One-bit readout of the previous attempt's DEPSTARTCFG raw
            // DEPCMD register (returned XferRscIdx 22:16, status 15:12).
            let ok = u32::from_str_radix(want.trim_start_matches("0x"), 16)
                .map(|value| TRACE_HARVEST_CFG == value)
                .unwrap_or(false);
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x5243_4647,
                TRACE_HARVEST_CFG,
                ok as u32,
                0,
                0,
            );
            if !ok {
                trace_marker(
                    TRACE_PROBE_WATCHDOG,
                    0x5243_4647 | (TRACE_HARVEST_CFG & 0xff),
                );
                log_puts("usb: cfg gate mismatch; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") {
            // The gate is evaluated by the signal probe AFTER this run's
            // observation window (see run_ep0_signal_probe): evaluating it
            // here would read attempt 1's still-empty trace and park before
            // any data existed. Keep this marker for the retained trace.
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x434D_4741, // "CMGA"
                0,
                0,
                0,
                0,
            );
            let _ = want;
        }
        #[cfg(not(fullerene_aarch64_usb_ep0_signal_probe))]
        if let Some(want) = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") {
            // One-bit readouts of the previous attempt's command outcomes and
            // SETUP reception. The retained-trace harvest carries the raw
            // DEPCMD register values; the host-visible attach names them:
            //   "timeout"   -> OLDEST STARTTRANSFER timed out (CMDACT stuck)
            //   "done"      -> OLDEST STARTTRANSFER completed (any status)
            //   "last-timeout" / "last-done" -> NEWEST STARTTRANSFER outcome
            //   "setup"     -> at least one SETUP packet reached DRAM
            //   "none"      -> no STARTTRANSFER record was found
            //   hex value   -> the OLDEST raw DEPCMD register equals exactly
            //                  this value
            let ok = match want {
                "timeout" => TRACE_HARVEST & 0x1_0000 != 0,
                "done" => TRACE_HARVEST != 0xFFFF_FFFF && TRACE_HARVEST & 0x1_0000 == 0,
                "last-timeout" => TRACE_HARVEST_LAST & 0x1_0000 != 0,
                "last-done" => {
                    TRACE_HARVEST_LAST != 0xFFFF_FFFF && TRACE_HARVEST_LAST & 0x1_0000 == 0
                }
                "setup" => TRACE_HARVEST_SETUP > 0,
                "desc" => TRACE_HARVEST_DESC > 0,
                "statusq" => TRACE_HARVEST_STATUSQ > 0,
                "armed" => TRACE_HARVEST_ARMED > 0,
                "connect" => TRACE_HARVEST_CONNECT > 0,
                // Watchdog-state readouts: the host-visible attach names
                // whether the apps watchdog was ARMED at probe entry.
                // Attach only when the guard's arm preceded the host's first
                // SETUP token: the arm won the race.
                "arm-first" => {
                    TRACE_HARVEST_ARM_SEQ != 0xFFFF_FFFF
                        && TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                        && TRACE_HARVEST_ARM_SEQ < TRACE_HARVEST_SETUP_SEQ
                }
                // Attach only when the first SETUP arrived while no TRB was
                // armed: the arm lost the race (the -110 root cause).
                "setup-first" => {
                    TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                        && (TRACE_HARVEST_ARM_SEQ == 0xFFFF_FFFF
                            || TRACE_HARVEST_ARM_SEQ > TRACE_HARVEST_SETUP_SEQ)
                }
                "wdt-armed" => WDT_KPSS_EN_AT_ENTRY & 1 != 0,
                "wdt-off" => WDT_KPSS_EN_AT_ENTRY != 0xFFFF_FFFF && WDT_KPSS_EN_AT_ENTRY & 1 == 0,
                "scm-answ" => (SWDD_AVAIL & 0xFFFF_FFFF) != 0xFFFF_FFFF,
                "scm-avail" => (SWDD_AVAIL & 0xFFFF_FFFF) == 1,
                "scm-noimpl" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0,
                "scm-dead" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0xFFFF_FFFF,
                "std-ok" => SWDD_STD != 0xFFFF_FFFF && (SWDD_STD & 0xFFFF_FFFF) > 0xFFFF,
                "std-dead" => SWDD_STD == 0xFFFF_FFFF,
                "mdcr-trap" => MDCR_EL2_AT_ENTRY & (1 << 14) != 0,
                "mdcr-clean" => MDCR_EL2_AT_ENTRY != u64::MAX && MDCR_EL2_AT_ENTRY & (1 << 14) == 0,
                "el1" => CURRENT_EL_AT_ENTRY & 0xF == 0b0100,
                "el2" => CURRENT_EL_AT_ENTRY & 0xF == 0b1000,
                "addr" => TRACE_HARVEST_ADDR > 0,
                "readall" => TRACE_HARVEST_ADDR2 > 0,
                "second-setup" => TRACE_HARVEST_SETUP >= 2,
                // Attach only when the first SETUP arrived within 2 seconds
                // of Connect Done, i.e. inside the host's enumeration window.
                "setup-fast" => TRACE_HARVEST_SETUP > 0 && TRACE_HARVEST_SETUP_DELAY <= 2,
                // Attach only when a SETUP arrived but LATE (> 2 seconds
                // after Connect Done): the pipeline ran after the host gave
                // up, which is a pure timing failure.
                "setup-slow" => TRACE_HARVEST_SETUP > 0 && TRACE_HARVEST_SETUP_DELAY > 2,
                "ep1-done" => TRACE_HARVEST_EP1 != 0xFFFF_FFFF && TRACE_HARVEST_EP1 & 0x1_0000 == 0,
                "ep1-1000" => TRACE_HARVEST_EP1 == 0x1000,
                "none" => TRACE_HARVEST == 0xFFFF_FFFF,
                other => u32::from_str_radix(other.trim_start_matches("0x"), 16)
                    .map(|value| TRACE_HARVEST == value)
                    .unwrap_or(false),
            };
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x434D_4400,
                TRACE_HARVEST,
                TRACE_HARVEST_LAST,
                TRACE_HARVEST_SETUP | (TRACE_HARVEST_DESC << 16),
                ok as u32,
            );
            if !ok {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x434D_4400 | (TRACE_HARVEST & 0xff));
                log_puts("usb: command gate mismatch; suppressing pull-up\n");
                park_after_gate_failure();
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_smmu_gate)]
        {
            // One-bit SMMU readout: publish the pull-up only when the
            // stream's S2CR type matches the requested value, so the
            // host-visible attach itself names the Apps-SMMU stream state.
            // Parse the full value: the ladder codes 3 and 251..=254 are
            // equally valid gate targets as the raw S2CR types 0..=2.
            let want = option_env!("FULLERENE_USB_SMMU_GATE_TYPE")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(99);
            let actual = smmu_stream_s2cr_type();
            trace_event(TRACE_SMMU_HANDOFF, actual, want, 0, 0, read(DSTS));
            if actual != want {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x534d_4d55 | (actual & 0xff));
                log_puts("usb: SMMU gate mismatch; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_smmu_install)]
        {
            // The stream is unmatched (ladder 252): with an active SMMU every
            // DWC3 DMA faults, which is exactly the dead-event-ring / dead-EP0
            // symptom. Claim a free SMR and point it at BYPASS so DMA passes
            // untranslated. The gate is STRICT: only a verified install on an
            // active-and-unmatched stream publishes the pull-up, so the
            // host-visible attach names exactly this state.
            let before = smmu_stream_s2cr_type();
            let installed = before == 252 && smmu_install_stream_bypass();
            trace_event(
                TRACE_SMMU_HANDOFF,
                0x494E_5354,
                installed as u32,
                before,
                0,
                0,
            );
            if !installed {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_4E46); // "SINF"
                log_puts("usb: SMMU stream install rejected; suppressing pull-up\n");
                return false;
            }
        }

        #[cfg(fullerene_aarch64_usb_ep0_dma_adopt)]
        if !dma_mapping_adopted() {
            // The stream was not in a rewritable TRANSLATE context or the
            // page-table walk could not adopt a mapped page. Without a known
            // DMA window the EP0 path cannot work, so leave the pull-up
            // down: the host-visible ABSENCE of the attach is the one-bit
            // readout naming this branch.
            trace_marker(TRACE_PROBE_WATCHDOG, 0x534e_4f44); // "SNOD"
            log_puts("usb: no adopted SMMU window; suppressing pull-up\n");
            return false;
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        {
            // Timing channel: delay ONLY the first attempt's connect by a
            // fixed number of seconds. The host's attach timestamp relative
            // to the Fastboot-device disconnect in the same journal then
            // shows whether Run/Stop owns the physical pull-up or an earlier
            // init stage (e.g. init_hsphy's VBUSVLDEXT0) asserts it.
            let first_attempt = !SIGNAL_CONNECT_DELAYED;
            SIGNAL_CONNECT_DELAYED = true;
            if first_attempt {
                if let Some(secs) = option_env!("FULLERENE_USB_CONNECT_DELAY_SECS")
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0)
                {
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x4344_4C59); // "CDLY"
                    super::timer::delay_ms(secs.saturating_mul(1000));
                }
            }
        }
        if !run_stop_device(true) {
            log_hex("usb: DWC3 remained halted, DSTS=", read(DSTS) as u64);
            return false;
        }
        RUN_STOP_TICK = arch_counter();
        // Tight SETUP-arm window: retry the ep0 OUT Start Transfer every
        // 200 us for up to 100 ms after Run/Stop. The link reaches ON within
        // a few ms (the HS chirp handshake), and the host's first SETUP
        // token arrives only after its own attach debounce plus port reset -
        // arming in this window guarantees the first descriptor read is
        // answered instead of timing out (-110) while the poll-loop guard
        // was still waiting for the link state.
        {
            let arm_deadline =
                arch_counter().saturating_add(arch_counter_frequency().saturating_mul(100) / 1000);
            let mut armed = false;
            while arch_counter() < arm_deadline {
                if EP0_SETUP_ARMED {
                    armed = true;
                    break;
                }
                if try_arm_setup() {
                    armed = true;
                    break;
                }
                super::timer::delay_us(200);
            }
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5441_524D, // "TARM" tight-arm outcome
                armed as u32,
                0,
                0,
                read(DSTS),
            );
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_early_drop_check();

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_pre_runstop_drop_check();

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_heartbeat_check();

        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_after_connect)]
        {
            // Same link-state constraint as the default arm: defer the
            // failure to the poll-loop guard, which arms once the link is
            // ON (the proven path that reached the host's SET_ADDRESS).
            if !start_transfer(0, ep0_trb_ptr(0)) {
                log_puts("usb: post-connect SETUP STARTTRANSFER deferred\n");
            }
            poll_ep0_event_ring();
        }
        log_puts("usb: Fullerene DWC3 gadget connected\n");
        note_runtime_event(super::platform::bramble::UsbRuntimeEvent::ControllerStarted);
    }
    true
}

/// Consume the DWC3 EP0 event ring without touching platform power, Type-C,
/// or SMMU state. Linux has an IRQ window immediately after arming the first
/// SETUP TRB; the early handoff uses this bounded synchronous equivalent before
/// the normal polling path owns the controller.
///
/// DWC3 GEVNTCOUNT is a write-to-consume register. Writing zero does not clear
/// an event left by the previous Fastboot owner; Linux reads the masked count
/// and writes that same byte count back during event-buffer setup.
unsafe fn acknowledge_ep0_event_count() {
    // Linux's event_buffers_setup() writes back the complete register value,
    // including GEVNTCOUNT.EHB.  Preserve that bit: reducing the value to
    // the byte count alone can leave a Fastboot-owned event buffer blocked
    // behind its pending-interrupt state even when the count is zero.
    let count = unsafe { read(GEVNTCOUNT0) };
    if count & (GEVNTCOUNT_MASK | GEVNTCOUNT_EHB) != 0 {
        unsafe {
            write(GEVNTCOUNT0, count);
            core::arch::asm!("dsb sy", options(nostack));
        }
    }
}

unsafe fn poll_ep0_event_ring() -> bool {
    let count_register = unsafe { read(GEVNTCOUNT0) };
    let count = count_register & GEVNTCOUNT_MASK;
    if count == 0 {
        return false;
    }
    // Linux masks the event interrupt while the current ring contents are
    // consumed. This matters for the early IRQ path as well as polling: an
    // event posted during process_event() must not re-enter the same consumer
    // before its cursor and acknowledgement are updated.
    let event_base = unsafe { ep0_event_dma_base() };
    let event_size = unsafe { ep0_event_size() };
    unsafe {
        write(
            GEVNTSIZ0,
            GEVNTSIZ_INTMASK | (event_size as u32 & GEVNTSIZ_SIZE_MASK),
        );
    }
    // Snapshot the producer-owned ring before acknowledging it. This is the
    // same ownership transition as Linux's evt->cache copy in
    // dwc3_check_event_buf(); process_event() must consume this stable copy.
    let start_offset = unsafe { EVENT_OFFSET };
    let mut copied = 0usize;
    while copied < count as usize {
        let offset = (start_offset + copied) % event_size;
        unsafe { cache_invalidate(event_base + offset, 4) };
        let event = (event_base as *const u8).wrapping_add(offset);
        let raw = unsafe {
            u32::from_le_bytes([
                read_volatile(event),
                read_volatile(event.add(1)),
                read_volatile(event.add(2)),
                read_volatile(event.add(3)),
            ])
        };
        unsafe {
            write_volatile(
                addr_of_mut!(EVENT_CACHE.0)
                    .cast::<u32>()
                    .add(copied / core::mem::size_of::<u32>()),
                raw,
            );
        }
        copied += 4;
    }
    unsafe {
        SIGNAL_EVENT_DELIVERED = true;
        EVENT_OFFSET = (start_offset + count as usize) % event_size;
        // Runtime event consumption acknowledges only the byte count. Linux
        // reserves the full-register write (including EHB) for event-buffer
        // setup/cleanup; its interrupt path writes the masked count here and
        // handles EHB separately only when IMOD is enabled.
        write(GEVNTCOUNT0, count);
        core::arch::asm!("dsb sy", options(nostack));
        // Publish the acknowledgement before unmasking, matching the Linux
        // event-buffer handler's ordering.
        write(GEVNTSIZ0, event_size as u32 & GEVNTSIZ_SIZE_MASK);
    }
    let mut remaining = count as usize;
    let mut cached_offset = 0usize;
    while remaining >= 4 {
        let raw = unsafe {
            read_volatile(
                addr_of!(EVENT_CACHE.0)
                    .cast::<u32>()
                    .add(cached_offset / core::mem::size_of::<u32>()),
            )
        };
        unsafe { process_event(raw) };
        cached_offset += 4;
        remaining -= 4;
    }
    true
}

/// Update the signal-probe latches. Called from `ep0_signal_code()` so a
/// polling-only consumer does not need an extra tracing channel.
unsafe fn update_signal_latches() {
    unsafe {
        // The core retires a TRB by clearing HWO over DMA. Invalidate the
        // cached line first so the CPU observes the controller's write.
        let trb = addr_of!(EP0_TRBS[0]);
        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
        if read_volatile(addr_of!((*trb).ctrl)) & TRB_HWO == 0 {
            SIGNAL_SETUP_TRB_RETIRED = true;
        }
        let setup = addr_of!(SETUP_PACKET) as *const u8;
        cache_invalidate(setup as usize, 8);
        for offset in 0..8 {
            if read_volatile(setup.add(offset)) != 0 {
                SIGNAL_SETUP_PACKET_RECEIVED = true;
                break;
            }
        }
        // DSTS_HIGHSPEED is zero, so the link state cannot be read from
        // ConnectSpd. A changing SOF frame number instead proves the core is
        // receiving packets from the host at the transaction level.
        let sofn = ((read(DSTS) & (0x3fff << 3)) >> 3) as u16;
        if sofn != SIGNAL_LAST_SOFFN {
            SIGNAL_LAST_SOFFN = sofn;
            SIGNAL_SOF_SEEN = true;
        }
        // Latch the core's view of the USB2 link for the link-state ladder.
        let dsts = read(DSTS);
        match (dsts >> 18) & 0xf {
            0 => SIGNAL_LNKST_U0 = true,      // ON: link up at the detected speed
            5 => SIGNAL_LNKST_RXDET = true,   // RX.DETECT: core still waiting
            7 => SIGNAL_LNKST_POLLING = true, // POLLING: chirp phase observed
            13 => SIGNAL_LNKST_RESET = true,  // RESET: bus reset observed
            _ => {}
        }
        if dsts & DSTS_DEVCTRLHLT != 0 || read(DCTL) & DCTL_RUN_STOP == 0 {
            // A halted core or a cleared Run/Stop after a verified start makes
            // the physical attach a QSCRATCH session-override phantom.
            SIGNAL_CORE_HALTED = true;
        }
    }
}

/// Encode the polled EP0/DMA observables as one host-visible code. The probe
/// drops the physical pull-up `3 * code` seconds after attach, so the host
/// dmesg delta between "new high-speed USB device" and "USB disconnect" names
/// the first stage that provably worked:
///   1 = event ring delivered a record to GEVNTCOUNT
///   2 = DWC3 retired the armed EP0 SETUP TRB (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frames are arriving (transaction-level RX alive)
///   0 = none of the above (no drop; the host only sees its own -110)
/// SMMU read-only probe codes are handled by `probe_smmu_stream_state()`.
pub fn ep0_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        if SIGNAL_EVENT_DELIVERED {
            return 1;
        }
        if SIGNAL_SETUP_TRB_RETIRED {
            return 2;
        }
        if SIGNAL_SETUP_PACKET_RECEIVED {
            return 3;
        }
        if SIGNAL_SOF_SEEN {
            return 5;
        }
        0
    }
}

/// Link-state variant of the signal ladder. Priority reflects the deepest
/// USB2 link state the core ever reported after a verified Run/Stop start:
///   1 = ON (U0): the core believes the link is up at the detected speed
///   2 = core halted itself or Run/Stop read back cleared (phantom attach)
///   3 = RESET: bus reset observed but never ON
///   4 = POLLING: chirp phase observed but never ON
///   5 = RX.DETECT only: the core never saw the host session
///   0 = none of the above
pub fn ep0_link_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        if SIGNAL_LNKST_U0 {
            return 1;
        }
        if SIGNAL_CORE_HALTED {
            return 2;
        }
        if SIGNAL_LNKST_RESET {
            return 3;
        }
        if SIGNAL_LNKST_POLLING {
            return 4;
        }
        if SIGNAL_LNKST_RXDET {
            return 5;
        }
        0
    }
}

/// Raw DSTS.USBLNKST nibble at poll time. The dedicated raw run drops the
/// pull-up at `3 + 2 * value` seconds, so the host-visible delta names the
/// exact link-state encoding the core reports after its verified start.
pub fn ep0_raw_link_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        (read(DSTS) >> 18) & 0xf
    }
}

/// Read-only Apps-SMMU state for the DWC3 stream. This never writes the
/// SMMU; if the aperture is clock-gated or secure-owned the access aborts and
/// the handset reboots before the pull-up is published, which is itself a
/// distinct host-visible outcome.
///   6 = SMR matched the stream and S2CR selects TRANSLATE
///   7 = SMR matched the stream and S2CR selects BYPASS
///   8 = SMMU readable but no SMR matches the DWC3 stream
///   9 = SMMU identification registers are unreadable
pub fn probe_smmu_stream_state() -> u32 {
    unsafe {
        let id0 = read_volatile(smmu_reg(SMMU_ID0));
        let id1 = read_volatile(smmu_reg(SMMU_ID1));
        if id0 == 0 || id0 == u32::MAX || id1 == 0 || id1 == u32::MAX {
            return 9;
        }
        let num_smrs = ((id0 & SMMU_ID0_NUMSMRG_MASK) as usize).min(128);
        let stream_id = super::platform::bramble::usb_resources().dma_pool.stream_id;
        for index in 0..num_smrs {
            let smr = read_volatile(smmu_reg(SMMU_SMR_BASE + index * 4));
            if smr & SMMU_SMR_VALID == 0 {
                continue;
            }
            let id = smr & 0xffff;
            let mask = (smr >> SMMU_SMR_MASK_SHIFT) & 0x7fff;
            if (stream_id ^ id) & !mask == 0 {
                let s2cr = read_volatile(smmu_reg(SMMU_S2CR_BASE + index * 4));
                return if s2cr & SMMU_S2CR_TYPE_MASK == SMMU_S2CR_TYPE_BYPASS {
                    7
                } else {
                    6
                };
            }
        }
        8
    }
}

/// Heartbeat control: toggle DCTL Run/Stop in one-second intervals starting
/// immediately after the verified connect. If the host still records a full
/// 5-second descriptor timeout against a continuously attached port, the
/// post-attach core ignores DCTL Run/Stop clears and the pull-up cannot be
/// dropped by software at all.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
fn ep0_signal_heartbeat_check() {
    if option_env!("FULLERENE_USB_SIGNAL_HEARTBEAT") != Some("1") {
        return;
    }
    unsafe {
        for _ in 0..3 {
            let _ = run_stop_device(false);
            super::timer::delay_ms(1000);
            let _ = run_stop_device(true);
            super::timer::delay_ms(1000);
        }
    }
}

/// Control variant of the early drop: run immediately BEFORE the first
/// Run/Stop. If the pull-up still appears with this unconditional drop, the
/// Qualcomm session overrides do not gate the attach at all and the pull-up
/// is purely core-driven (DCTL.TermSelect).
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
fn ep0_signal_pre_runstop_drop_check() {
    if option_env!("FULLERENE_USB_SIGNAL_PRE_DROP") != Some("1") {
        return;
    }
    unsafe {
        trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_5052);
        ep0_signal_drop_pullup();
    }
}

/// One-bit host-visible signal: sample the condition latches for a bounded
/// window right after the first post-connect event poll and permanently drop
/// the pull-up when the requested condition is met. The host then never sees
/// the descriptor timeout (-110), so the ABSENCE of that line is the readout.
///   9 = unconditional (control run: proves the drop mechanism itself)
///   1 = event ring delivered a record (GEVNTCOUNT > 0)
///   2 = the armed EP0 SETUP TRB was retired (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frame numbers are changing (transaction-level RX alive)
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
fn ep0_signal_early_drop_check() {
    let condition = match option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP") {
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("5") => 5,
        Some("9") => 9,
        _ => 0,
    };
    if condition == 0 {
        return;
    }
    unsafe {
        let mut observed = 0;
        let mut ms = 0u32;
        while ms < 1500 {
            ms += 1;
            if condition != 9 {
                // Consume any pending events first: the delivery latch is
                // only set by a real event-ring poll.
                poll_ep0_event_ring();
                update_signal_latches();
                observed = if SIGNAL_EVENT_DELIVERED {
                    1
                } else if SIGNAL_SETUP_TRB_RETIRED {
                    2
                } else if SIGNAL_SETUP_PACKET_RECEIVED {
                    3
                } else if SIGNAL_SOF_SEEN {
                    5
                } else {
                    0
                };
                if observed == condition {
                    break;
                }
            }
            super::timer::delay_ms(1);
        }
        if condition == 9 || observed == condition {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_4544 | (condition << 8));
            ep0_signal_drop_pullup();
        }
    }
}

/// True when the diagnostic quiet window (FULLERENE_USB_QUIET_AFTER_SECS)
/// has passed: the probe must stop ALL MMIO access, including the watchdog
/// pet, so a surviving reboot is provably external.
pub fn mmio_quiet_active() -> bool {
    unsafe {
        if let Some(secs) = option_env!("FULLERENE_USB_QUIET_AFTER_SECS")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        {
            if RUN_STOP_TICK != 0 {
                let frequency = arch_counter_frequency();
                if frequency != 0 {
                    return arch_counter().saturating_sub(RUN_STOP_TICK)
                        >= frequency.saturating_mul(secs);
                }
            }
        }
        false
    }
}

/// Evaluate the FULLERENE_USB_SIGNAL_CMD_GATE condition against the
/// retained-trace harvest. None = no gate configured (or unparseable).
pub fn cmd_gate_condition_met() -> Option<bool> {
    let want = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")?;
    unsafe {
        // Re-harvest against this run's live trace: the gate must evaluate
        // the attempt that just flowed through the observation window, not
        // the init-time harvest of a previous attempt.
        harvest_trace_outcome();
        let ok = match want {
            // Mechanism self-test: unconditionally true. A clean (non-watchdog)
            // readout with this gate proves the gate path and our edits are
            // live in the running image.
            "always" => true,
            "timeout" => TRACE_HARVEST & 0x1_0000 != 0,
            "done" => TRACE_HARVEST != 0xFFFF_FFFF && TRACE_HARVEST & 0x1_0000 == 0,
            "last-timeout" => TRACE_HARVEST_LAST & 0x1_0000 != 0,
            "last-done" => TRACE_HARVEST_LAST != 0xFFFF_FFFF && TRACE_HARVEST_LAST & 0x1_0000 == 0,
            "setup" => TRACE_HARVEST_SETUP > 0,
            "desc" => TRACE_HARVEST_DESC > 0,
            "statusq" => TRACE_HARVEST_STATUSQ > 0,
            "armed" => TRACE_HARVEST_ARMED > 0,
            "arm-first" => {
                TRACE_HARVEST_ARM_SEQ != 0xFFFF_FFFF
                    && TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                    && TRACE_HARVEST_ARM_SEQ < TRACE_HARVEST_SETUP_SEQ
            }
            "setup-first" => {
                TRACE_HARVEST_SETUP_SEQ != 0xFFFF_FFFF
                    && (TRACE_HARVEST_ARM_SEQ == 0xFFFF_FFFF
                        || TRACE_HARVEST_ARM_SEQ > TRACE_HARVEST_SETUP_SEQ)
            }
            "connect" => TRACE_HARVEST_CONNECT > 0,
            "addr" => TRACE_HARVEST_ADDR > 0,
            "readall" => TRACE_HARVEST_ADDR2 > 0,
            // Data-phase (EP1 IN) arm outcome gates: TRACE_HARVEST_EP1 holds
            // the newest EP1 STARTTRANSFER raw DEPCMD register (status bits
            // 15:12), or 0xFFFF_FFFF when no EP1 command was ever issued.
            "ep1-none" => TRACE_HARVEST_EP1 == 0xFFFF_FFFF,
            "ep1-done" => TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0x1_1000) == 0,
            "ep1-nores" => TRACE_HARVEST_EP1 == 0x1000,
            // Final data-phase arm outcome after the bounded retry ("DARM").
            "darm" => TRACE_HARVEST_DARM == 0x1_0001,
            "darm-fail" => TRACE_HARVEST_DARM == 0x1_0000,
            // Data-phase TRB outcome: did the core COMPLETE the armed data
            // transfer (0x8 = healthy LST|IOC), and did it report the data
            // phase ready (XferNotReady) before any IN token was answered?
            "ep1-xfer" => TRACE_HARVEST_EP1_XFER != 0xFFFF_FFFF,
            "ep1-xfer-ok" => TRACE_HARVEST_EP1_XFER == 0x8,
            "ep1-xfer-err" => {
                TRACE_HARVEST_EP1_XFER != 0xFFFF_FFFF && TRACE_HARVEST_EP1_XFER != 0x8
            }
            "ep1-nrdy" => TRACE_HARVEST_EP1_NRDY > 0,
            "wdt-armed" => WDT_KPSS_EN_AT_ENTRY & 1 != 0,
            "wdt-off" => WDT_KPSS_EN_AT_ENTRY != 0xFFFF_FFFF && WDT_KPSS_EN_AT_ENTRY & 1 == 0,
            // Secure-watchdog SMC result readout (set at probe entry, before
            // the observation window): low word 0 = TZ accepted the disable
            // (high word = attempt index 1 = SMC_64, 2 = SMC_32).
            "swdd-ok" => (SWDD_RESULT & 0xFFFF_FFFF) == 0,
            "swdd-fail" => (SWDD_RESULT & 0xFFFF_FFFF) != 0,
            // SCM path diagnostics from the IS_CALL_AVAIL probe (probe
            // entry): did the SMC interface answer at all, and does the TZ
            // implement (SVC_BOOT, SEC_WDOG_DIS)?
            "scm-answ" => (SWDD_AVAIL & 0xFFFF_FFFF) != 0xFFFF_FFFF,
            "scm-avail" => (SWDD_AVAIL & 0xFFFF_FFFF) == 1,
            "scm-noimpl" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0,
            "scm-dead" => (SWDD_AVAIL & 0xFFFF_FFFF) == 0xFFFF_FFFF,
            // EL3 SMCCC liveness (SMCCC_VERSION answer): major<<16|minor
            // with major >= 1, i.e. a value above 0xFFFF.
            "std-ok" => SWDD_STD != 0xFFFF_FFFF && (SWDD_STD & 0xFFFF_FFFF) > 0xFFFF,
            "std-dead" => SWDD_STD == 0xFFFF_FFFF,
            // Exception-level context at probe entry: is SMC from EL1
            // trapped to EL2 (MDCR_EL2.SMC, bit 14), and at which EL are
            // we actually running (0b0101 = EL1h, 0b1000 = EL2h)?
            "mdcr-trap" => MDCR_EL2_AT_ENTRY & (1 << 14) != 0,
            "mdcr-clean" => MDCR_EL2_AT_ENTRY != u64::MAX && MDCR_EL2_AT_ENTRY & (1 << 14) == 0,
            "el1" => CURRENT_EL_AT_ENTRY & 0xF == 0b0100,
            "el2" => CURRENT_EL_AT_ENTRY & 0xF == 0b1000,
            // Live controller-state probes at gate-eval time (readout for the
            // "SETUP TRB never armed / no events processed" diagnosis): is the
            // device link ON (USBLNKST==0), is the core halted, are the
            // endpoints ready, is the SETUP TRB armed, and is EP0 in the
            // Setup state?
            "lnk-on" => (read(DSTS) >> 18) & 0xf == 0,
            "lnk-reset" => (read(DSTS) >> 18) & 0xf == 1,
            "lnk-suspend" => {
                let lnkst = (read(DSTS) >> 18) & 0xf;
                lnkst >= 5 && lnkst != 0xf
            }
            "halt" => read(DSTS) & DSTS_DEVCTRLHLT != 0,
            "epready" => ENDPOINTS_READY,
            "ep0armed" => EP0_SETUP_ARMED,
            "ep0setup" => EP0_STATE == Ep0State::Setup,
            // Direct-path (init_with_super_speed) EP command sequence: how far
            // did the init get (is4=DEPSTARTCFG issued, is5=DEPCFG ep0,
            // is6=DEPCFG ep1), did the DEPSTARTCFG/DEPCFG command retire
            // (CMDACT bit 10 clear == done, set == the core never processed
            // it), and was the core ready (DCNRD bit 29) / halted (bit 22) /
            // link U0 at the first endpoint command?
            "is4" => INIT_STAGE >= 4,
            "is5" => INIT_STAGE >= 5,
            "is6" => INIT_STAGE >= 6,
            "ds-stuck" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_RAW & DEPCMD_CMDACT != 0
            }
            "ds-done" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_RAW & DEPCMD_CMDACT == 0
            }
            "ep0-stuck" => {
                INIT_EPCFG0_RAW != 0xFFFF_FFFF && INIT_EPCFG0_RAW & DEPCMD_CMDACT != 0
            }
            "ep0-ok" => INIT_EPCFG0_OK,
            "ep1-ok" => INIT_EPCFG1_OK,
            "ds-dcnrd" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_DSTS & DSTS_DCNRD != 0
            }
            "ep0-dcnrd" => {
                INIT_EPCFG0_RAW != 0xFFFF_FFFF && INIT_EPCFG0_DSTS & DSTS_DCNRD != 0
            }
            "ds-halt" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_DSTS & DSTS_DEVCTRLHLT != 0
            }
            "ds-lnk0" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && (INIT_DEPSTART_DSTS >> 18) & 0xf == 0
            }
            "ep0-lnk0" => {
                INIT_EPCFG0_RAW != 0xFFFF_FFFF && (INIT_EPCFG0_DSTS >> 18) & 0xf == 0
            }
            other => u32::from_str_radix(other.trim_start_matches("0x"), 16)
                .map(|value| TRACE_HARVEST == value)
                .unwrap_or(false),
        };
        Some(ok)
    }
}

/// Park for `seconds` (no pull-up toggling, no fallback path, no secondary
/// attempt, so gate readouts stay uncontaminated), then reset through the
/// normal recovery path (PS_HOLD release + PSCI SYSTEM_RESET, inlined here
/// because usb_probe is a separate binary). The trace marker carries the
/// park duration so a later retained-trace read can name the exact branch.
pub fn park_for_seconds(seconds: u64) -> ! {
    unsafe {
        trace_marker(
            TRACE_PROBE_WATCHDOG,
            0x5041_524B | ((seconds & 0xff) as u32) << 8,
        ); // "PARK"+secs
        let frequency = arch_counter_frequency();
        let deadline = arch_counter().saturating_add(frequency.saturating_mul(seconds));
        while frequency == 0 || arch_counter() < deadline {
            wdt_pet();
            core::hint::spin_loop();
        }
        unsafe {
            core::ptr::write_volatile(0x0c26_4000usize as *mut u32, 0);
            core::arch::asm!(
                "mov w0, #9",
                "movk w0, #0x8400, lsl #16",
                "smc #0",
                out("x0") _,
                out("x1") _,
                out("x2") _,
                out("x3") _,
                options(nostack)
            );
        }
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Park the probe after a gate readout failed. Bounded: after 90 s the probe
/// resets through the normal recovery path even if the assembly timer is
/// late.
pub fn park_after_gate_failure() -> ! {
    park_for_seconds(90)
}

/// Deassert the pull-up so the host sees a physical disconnect.
///
/// The Qualcomm session overrides are cleared INSTEAD of toggling
/// DCTL.Run/Stop: a wedged core ignores DCTL, but the QSCRATCH session votes
/// reach the PHY directly and still control the physical pull-up.
pub fn ep0_signal_drop_pullup() {
    unsafe {
        let ss = read_qscratch(QSCRATCH_SS_PHY_CTRL);
        write_qscratch(QSCRATCH_SS_PHY_CTRL, ss & !(1 << 24));
        let hs = read_qscratch(QSCRATCH_HS_PHY_CTRL);
        write_qscratch(QSCRATCH_HS_PHY_CTRL, hs & !((1 << 20) | (1 << 28)));
        let _ = read_qscratch(QSCRATCH_HS_PHY_CTRL);
        if option_env!("FULLERENE_USB_SIGNAL_DROP_VBUS") == Some("1") {
            // The QUSB2 PHY's VBUSVLDEXT0 forces session-valid at the PHY, so
            // it can own the pull-up independently of DCTL and the QSCRATCH
            // session bits. Clear it (and its select latch) to test that
            // ownership with a host-visible disconnect/re-attach pair.
            hsphy_update(HSPHY_CTRL1, HSPHY_CTRL1_VBUSVLDEXT0, 0);
            hsphy_update(HSPHY_COMMON1, HSPHY_COMMON1_VBUSVLDEXTSEL0, 0);
        }
    }
}

/// Publish the physical pull-up from the signal probe after a failed
/// handoff. Restores the Qualcomm session overrides and Run/Stop so the
/// diagnostic gates remain host-visible even when init failed before its own
/// Run/Stop boundary (e.g. the pre-connect STARTTRANSFER differential).
pub fn ep0_signal_publish_pullup() {
    unsafe {
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        let _ = run_stop_device(true);
    }
}

/// Reassert the pull-up after a signal drop by restoring the same Qualcomm
/// session overrides the handoff applies.
pub fn ep0_signal_restore_pullup() {
    unsafe {
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
        if option_env!("FULLERENE_USB_SIGNAL_DROP_VBUS") == Some("1") {
            hsphy_update(
                HSPHY_COMMON1,
                HSPHY_COMMON1_VBUSVLDEXTSEL0,
                HSPHY_COMMON1_VBUSVLDEXTSEL0,
            );
            hsphy_update(
                HSPHY_CTRL1,
                HSPHY_CTRL1_VBUSVLDEXT0,
                HSPHY_CTRL1_VBUSVLDEXT0,
            );
        }
    }
}

/// Linux enables the DWC3 controller SPI immediately after arming the first
/// EP0 OUT SETUP TRB. The standalone probe's assembly entry prepares the
/// exception vector and CPU interface, but the Distributor still needs the
/// normal Rust GIC initialization before a USB SPI can be delivered. Keep
/// this probe-only: the normal Fullerene boot path owns GIC setup after USB
/// initialization and must not receive an early IRQ.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
unsafe fn enable_gadget_controller_irq() {
    unsafe {
        let _ = super::platform::gicv3::init(
            super::platform::bramble::GICD_BASE,
            super::platform::bramble::GICR_BASE,
            Some(super::platform::bramble::USB_DWC3_IRQ),
        );
    }
}

#[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
unsafe fn enable_gadget_controller_irq() {}

/// Poll the DWC3 event ring. This is intentionally cheap enough to run from
/// the early boot loop until the normal interrupt controller owns the device.
pub fn poll() {
    unsafe {
        // Diagnostic quiet window (see mmio_quiet_active): after this many
        // seconds past the first Run/Stop, stop ALL controller MMIO access.
        if mmio_quiet_active() {
            return;
        }
        let runtime = USB_RUNTIME_STATE;
        // In the no-SMMU differential the whole point is to never touch the
        // Apps-SMMU: the stream is unmatched there and the (inactive, often
        // clock-gated) SMMU aperture can fault the CPU with an asynchronous
        // external abort when its runtime clock gates later in the session,
        // which reboots the handset right in the middle of host enumeration.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_smmu)
            && !matches!(
                runtime,
                super::platform::bramble::UsbRuntimeState::Off
                    | super::platform::bramble::UsbRuntimeState::Suspended
            )
        {
            service_smmu_fault();
        }
        service_power_event();
        if RESUME_PENDING {
            RESUME_PENDING = false;
            if CONFIGURED && !runtime_resume() {
                // Keep the request pending if clocks/PHY are not yet ready;
                // the next poll then retries just as Linux's resume work does.
                RESUME_PENDING = true;
            }
        }
        // Signal builds must keep exactly one actuator (the diagnostic
        // pull-up toggle): a Type-C poll that samples a transient CC state
        // would otherwise apply an uncontrolled detach and pollute the
        // attach/disconnect readouts.
        if !cfg!(fullerene_aarch64_usb_ep0_signal_probe) {
            poll_typec_state(false);
        }
        if !poll_ep0_event_ring() {
            drain_gsi_event_buffers();
            // The core rejects Start Transfer while the link is not ON (this
            // includes the window right after Run/Stop and the host's bus
            // reset), so the initial SETUP arm can fail. Once the link comes
            // up, arm here: the core then immediately delivers any SETUP
            // packet it latched while no TRB was armed.
            let _ = try_arm_setup();
            return;
        }
        drain_gsi_event_buffers();
        let _ = try_arm_setup();
    }
}

/// Consume Qualcomm GSI event buffers. Android reserves event buffers 1..3 for
/// the data path; decode each record as an event word and retain it in the
/// same trace used by EP0. Unknown GSI event encodings are still acknowledged
/// without being mistaken for control transfers.
unsafe fn drain_gsi_event_buffers() {
    let configured = super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3) as usize;
    for index in 0..configured {
        let count_reg = GEVNTCOUNT0 + (index + 1) * GEVNT_BUFFER_STRIDE;
        let count = unsafe { read(count_reg) & 0xfffc } as usize;
        if count == 0 {
            continue;
        }
        let mut remaining = count;
        while remaining >= 4 {
            let offset = unsafe { GSI_EVENT_OFFSETS[index] };
            unsafe {
                cache_invalidate(
                    addr_of!(GSI_EVENTS) as usize + index * EVENT_BUFFER_SIZE + offset,
                    4,
                );
                let event_ptr =
                    (addr_of!(GSI_EVENTS) as *const u8).add(index * EVENT_BUFFER_SIZE + offset);
                let raw = u32::from_le_bytes([
                    read_volatile(event_ptr),
                    read_volatile(event_ptr.add(1)),
                    read_volatile(event_ptr.add(2)),
                    read_volatile(event_ptr.add(3)),
                ]);
                let endpoint = GSI_CHANNEL_ENDPOINT[index] as u8;
                let address = endpoint | if endpoint & 1 != 0 { 0x80 } else { 0 };
                let request_slot = GSI_REQUEST_SLOTS[index];
                let completion_status = (raw >> 12) & 0xf;
                let mut actual = 0;
                if request_slot != usize::MAX {
                    let in_direction = endpoint & 1 != 0;
                    let shape = gsi_ring_shape(in_direction, GSI_DEFAULT_NUM_BUFFERS);
                    let data_index = shape.map(|shape| shape.first_buffer_trb).unwrap_or(0);
                    let ring_base = GSI_RING_BASES[index];
                    let trb = ring_base as usize as *mut Trb;
                    cache_invalidate(
                        ring_base as usize,
                        GSI_RING_TRB_COUNTS[index] * core::mem::size_of::<Trb>(),
                    );
                    if let Some(request) = udc_mut().request(address, request_slot) {
                        let residual =
                            read_volatile(addr_of!((*trb.add(data_index)).size)) & 0x00ff_ffff;
                        actual = request.length.saturating_sub(residual);
                        let _ = udc_mut().complete(
                            address,
                            request_slot,
                            actual,
                            completion_status != 0,
                        );
                        GadgetDriver::on_gsi_data_complete(
                            gadget_mut(),
                            address,
                            actual,
                            completion_status != 0,
                        );
                        let _ = udc_mut().release(address, request_slot);
                    }
                }
                trace_event(
                    TRACE_TRANSFER_COMPLETE,
                    endpoint as u32,
                    raw,
                    offset as u32,
                    actual,
                    count as u32,
                );
                // The event buffer is the ownership boundary for this
                // single-slot early request queue. Keep the event word in
                // retained trace, then make the TRB reusable for the next
                // request.
                GSI_PENDING[index] = false;
                GSI_REQUEST_SLOTS[index] = usize::MAX;
                GSI_RING_ACTIVE[index] = false;
            }
            unsafe {
                GSI_EVENT_OFFSETS[index] = (offset + 4) % EVENT_BUFFER_SIZE;
            }
            remaining -= 4;
        }
        unsafe { write(count_reg, count as u32) };
    }
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
