//! APSS/secure watchdog recovery and diagnostic SMC readouts.
//!
//! This is a deliberately narrow module: the probe and handoff paths call the
//! public functions, while gate evaluation reads the latched diagnostics.

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

use super::trace::{TRACE_PROBE_WATCHDOG, trace_event, trace_marker};

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
pub(super) static mut WDT_TRACED: bool = false;
// Non-zero while a scheduled APSS-WDT bite owns recovery: wdt_pet() must
// stop petting (and must not re-arm the 100 s timeout) so the countdown
// runs to the bite. The bite reboots the handset at a controlled time,
// making the loop's return time a host-observable readout (the timing
// readout the broken SMC software reset never delivered).
pub(super) static mut WDT_BITE_PENDING: u32 = 0;
/// SCM result of the secure watchdog disable (0 = success; the sentinel
/// means the call has not run yet or ran out of retries).
pub(super) static mut SWDD_RESULT: u64 = 0xFFFF_FFFF;
/// SCM IS_CALL_AVAIL result for (SVC_BOOT, SEC_WDOG_DIS): 1 = the TZ
/// implements the function, 0 = it answers but does not implement it,
/// 0xFFFF_FFFF = it returned ARM_SMCCC_UNK_FUNC on every convention
/// (the SMC path itself is dead, or the encoding is wrong). High word =
/// convention attempt index (1 = SMC_64, 2 = SMC_32).
pub(super) static mut SWDD_AVAIL: u64 = 0xFFFF_FFFF;
/// WDT enable word captured at the first pet (probe entry).
/// 0xFFFF_FFFF = not captured yet.
pub(super) static mut WDT_KPSS_EN_AT_ENTRY: u32 = 0xFFFF_FFFF;

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
pub(super) static mut SWDD_STD: u64 = 0xFFFF_FFFF;
/// MDCR_EL2 (Memory Debug Control, EL2) captured at probe entry. Bit 14
/// (SMC) traps SMC instructions issued from EL0/EL1 into EL2: if the
/// bootloader left it set, every "SMC" we issue lands in the bootloader's
/// EL2 vector, never in TZ, and whatever that vector does (often a
/// default -1 return) is all we ever see.
pub(super) static mut MDCR_EL2_AT_ENTRY: u64 = u64::MAX;
/// CurrentEL at probe entry: 0b0101 = EL1h (expected), 0b1000 = EL2h.
pub(super) static mut CURRENT_EL_AT_ENTRY: u64 = u64::MAX;

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
        asm!(
            "mov x1, {arginfo}",
            "mov x2, {a0v}",
            "mov x3, {a1v}",
            "mov x4, {a2v}",
            "mov x5, xzr",
            "mov x7, xzr",
            "smc #0",
            inout("x0") fnid => result,
            arginfo = in(reg) arginfo,
            a0v = in(reg) a0,
            a1v = in(reg) a1,
            a2v = in(reg) a2,
            inout("x6") a6 => a6_out,
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
    // The fnid is build-selectable so the A/B run can swap encodings without
    // a source change: the default is the proven-to-be-tolerated legacy call
    // (0x02000407); the modern SMCCC OEN encodings of the same function
    // (BOOT/0x07, funcnum 0x0107, owner SIP) are selected by
    // FULLERENE_USB_SWDD_FNID, e.g. 0x82000107 = STD/SMC64, 0x02000107 =
    // FAST/SMC64, 0xA2000107 = STD/SMC32, 0x22000107 = FAST/SMC32. The upper
    // word of SWDD_RESULT records the fnid actually issued (the lower word
    // is the result, so the swdd-ok gate keeps its meaning).
    let fnid = option_env!("FULLERENE_USB_SWDD_FNID")
        .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0200_0407);
    let result = unsafe { scm_smc_call_once(fnid, 1, 1, 0, 0) }.0;
    unsafe {
        SWDD_RESULT = (fnid << 32) | (result & 0xFFFF_FFFF);
    }
    trace_event(
        TRACE_PROBE_WATCHDOG,
        0x5357_4444, // "SWDD"
        result as u32,
        (fnid >> 16) as u32,
        fnid as u32,
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
        let mut mdcr = u64::MAX;
        let cur_el: u64;
        asm!("mrs {c}, CurrentEL", c = out(reg) cur_el, options(nomem, nostack));
        if cur_el & 0x8 != 0 {
            asm!("mrs {m}, MDCR_EL2", m = out(reg) mdcr, options(nomem, nostack));
        }
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

/// Restart the apps watchdog countdown. On the first call, record the existing
/// configuration in the retained trace, then re-arm bark/bite with the probe's
/// own 100 s / 110 s timeouts and enable the watchdog.
pub fn wdt_pet() {
    unsafe {
        if WDT_BITE_PENDING != 0 {
            // A scheduled bite owns recovery now: any pet (including the
            // one-shot 100 s re-arm below) would cancel it.
            return;
        }
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
            asm!("dsb sy", options(nostack));
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
        asm!("dsb sy", options(nostack));
    }
}

/// Schedule the APSS watchdog bite `delay` seconds from now: re-arm
/// bark/bite at the 32765 Hz watchdog clock, restart the countdown, and
/// mark the pet path so nothing cancels it. The bite reboots the handset
/// on its own (no SMC software reset needed), so the loop's return time
/// — bite time plus the ~20 s Android boot — becomes a host-observable
/// readout of which u0_arm_recovery step failed. If the APSS WDT is not
/// writable from here (secure-owned), the bite never lands and the secure
/// watchdog's ~17 s bite (the ~37 s return) stands in for every bucket.
/// The callers own the gating (the u0-arm status match is unreachable
/// without the u0-arm env; the control bite has its own env).
pub fn u0_arm_wdt_bite(delay_secs: u32) {
    unsafe {
        let delay = delay_secs.clamp(1, 16);
        let bark = (delay.saturating_sub(1).max(1) * 32765) as u32;
        let bite = (delay * 32765) as u32;
        write_volatile(wdt_reg(APSS_WDT_BARK), bark);
        write_volatile(wdt_reg(APSS_WDT_BITE), bite);
        write_volatile(wdt_reg(APSS_WDT_RST), 1);
        write_volatile(wdt_reg(APSS_WDT_EN), 1);
        asm!("dsb sy", options(nostack));
        WDT_BITE_PENDING = delay;
        trace_marker(TRACE_PROBE_WATCHDOG, 0x5744_5442 | (delay & 0xff)); // "WDTB"
    }
}
