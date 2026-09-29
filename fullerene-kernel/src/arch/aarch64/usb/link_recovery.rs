//! Link recovery, gate readouts, and bounded probe reset paths.

use super::*;

/// Park for `seconds` (no pull-up toggling, no fallback path, no secondary
/// attempt, so gate readouts stay uncontaminated), then reset through PSCI
/// SYSTEM_RESET with the Qualcomm PS_HOLD release as the rejected-SMC
/// fallback (inlined here because usb_probe is a separate binary). The trace
/// marker carries the park duration so a later retained-trace read can name
/// the exact branch.
pub fn park_for_seconds(seconds: u64) -> ! {
    unsafe {
        trace_marker(
            TRACE_PROBE_WATCHDOG,
            0x5041_524B | ((seconds & 0xff) as u32) << 8,
        ); // "PARK"+secs
        let frequency = arch_counter_frequency();
        let deadline = arch_counter().saturating_add(frequency.saturating_mul(seconds));
        // Power keepalive: the restored USB domain is still collapsed by
        // RPMh ~5-8 s after the attach wakes it, even with the initial vote
        // set. Re-assert the rail votes and the GDSC enable periodically
        // (never the reset lines: those would kill a live controller) so
        // parks run to their own deadline. Pure spin, no other MMIO.
        let keepalive_period = frequency.saturating_div(2);
        let mut next_keepalive = arch_counter().saturating_add(keepalive_period);
        while frequency != 0 && arch_counter() < deadline {
            wdt_pet();
            if arch_counter() >= next_keepalive {
                // apply_usb_power early-returns once its state flag matches,
                // so the periodic rail re-vote must go through the refresh
                // path: the GDSC force-enable below cannot hold a domain
                // whose CX corner and interconnect vote RPMh has already
                // dropped.
                let _ = super::super::platform::bramble::refresh_usb_domain_votes(
                    super::super::platform::bramble::UsbBusVote::Nominal,
                    true,
                );
                let _ = super::super::platform::bramble::force_enable_usb30_gdsc();
                next_keepalive = arch_counter().saturating_add(keepalive_period);
            }
            core::hint::spin_loop();
        }
        unsafe {
            // PSCI SYSTEM_RESET (function 9) FIRST. The PS_HOLD release
            // lives in the PMIC/SPMI aperture, which the probe path never
            // clocks up; on this board that write can stall the CPU, handing
            // recovery to the secure watchdog (~37 s return) instead of the
            // PSCI reset. If the SMC returns (firmware rejected it), release
            // PS_HOLD behind it and let the watchdog finish recovery.
            // The old #7 encoding was MIGRATE_INFO_UP_CPU and could return
            // without resetting; SYSTEM_RESET is function 9 (0x84000009).
            core::arch::asm!(
                "mov w0, #9",
                "movk w0, #0x8400, lsl #16",
                "mov x1, xzr",
                "mov x2, xzr",
                "mov x3, xzr",
                "smc #0",
                out("x0") _,
                out("x1") _,
                out("x2") _,
                out("x3") _,
                options(nostack)
            );
            core::ptr::write_volatile(0x0c26_4000usize as *mut u32, 0);
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
        // On Bramble the Qualcomm session/VBUS override bits are not the
        // host-visible pull-up owner: clearing them alone still allowed the
        // host to reach HS attach in the code-9 control.  Use the same DCTL
        // Run/Stop path as the proven host-visible gate after removing the
        // glue overrides.  This remains diagnostic-only and deliberately
        // skips the halt readback so a wedged core cannot hide the drop.
        let _ = run_stop_device_no_readback(false);
    }
}

/// Diagnostic clock-source flip for the voteflip gate: repeat the
/// Qualcomm UTMI-as-PIPE selection sequence while the host is driving the
/// descriptor read. If the core's USB2 RX dies during this window, the
/// host journal shows a disconnect; if the -110 window completes normally
/// with -110 at attach+5 s, the clock mux is inert for enumeration.
pub fn flip_utmi_pipe_clock() {
    unsafe {
        select_utmi_pipe_clock();
        let _ = read_qscratch(QSCRATCH_GENERAL_CFG);
    }
}

/// Raw-write variant of the diagnostic clock-source flip for the
/// voteflip2 gate: drive QSCRATCH_GENERAL_CFG directly instead of through
/// the read-modify-write helper, ending at the restored UTMI-as-PIPE
/// value. Distinguishes a qscratch_set() path bug from a PHY-side clock
/// event that kills the core's RX regardless of the write encoding.
pub fn flip_utmi_pipe_clock_raw() {
    unsafe {
        write_qscratch(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_DIS);
        crate::timer::delay_us(100);
        write_qscratch(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
        crate::timer::delay_us(100);
        write_qscratch(QSCRATCH_GENERAL_CFG, PIPE_UTMI_CLK_SEL | PIPE3_PHYSTATUS_SW);
    }
}

/// Restore the Qualcomm glue session overrides after a signal-probe vote
/// experiment. Mirrors the handoff's vbus_override sequence exactly; used
/// by the voteflip gate so the attach survives the toggle.
pub fn restore_usb2_session_votes() {
    unsafe {
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        let _ = read_qscratch(QSCRATCH_HS_PHY_CTRL);
    }
}

/// Publish the physical pull-up from the signal probe after a failed
/// handoff. Restores the Qualcomm session overrides and Run/Stop so the
/// diagnostic gates remain host-visible even when init failed before its own
/// Run/Stop boundary (e.g. the pre-connect STARTTRANSFER differential).
/// Stop the core through DCTL Run/Stop (the inverse of the handoff's
/// soft-connect). Unlike ep0_signal_drop_pullup - which clears the QSCRATCH
/// session votes and is host-invisible on this board - the DWC3 Run/Stop bit
/// owns the physical pull-up: stopping the core while the host still tracks
/// the device publishes a real "USB disconnect" line in the host kernel
/// log. This is the one-bit gate-TRUE readout; the old SDIS-named path and
/// the QSCRATCH drop are both dead channels here. The wait acknowledges device
/// events while halting per the databook stop contract.
pub fn gate_true_stop_device() -> bool {
    unsafe { run_stop_device(false) }
}

/// Stop the gadget at a diagnostic boundary without waiting for the DWC3
/// halted-state readback. This keeps a SETUP-boundary probe inside the host's
/// pending control-transfer window; the ordinary gate helper remains the
/// readback-checked path for non-timing-sensitive tests.
pub fn gate_true_stop_device_fast() -> bool {
    unsafe { run_stop_device_no_readback(false) }
}

/// Public Run/Stop re-assert for the dstat readout: the host-visible side of
/// the stop/run cycle (Run/Stop owns the physical pull-up).
pub fn gate_true_run_device() -> bool {
    unsafe { run_stop_device(true) }
}

/// One-shot core-domain snapshot for the attach-delay readout: the gate-run
/// probe delays the physical attach by `code * 80 ms`, and the host kernel
/// log's attach timestamp publishes the code (±0.1 s, far inside the ~17 s
/// biter window that overrides every park and reset channel).
///
/// Encoding (6 bits, 0..63 -> 0..5.04 s):
///   bits 5:3 = raw GDSCR bits 2:0 (SW_COLLAPSE | PWR_ON-ish state words)
///   bit  2   = GSNPSID reads a DWC3 revision (the core answers MMIO)
///   bit  1   = DSTS.DEVCTRLHLT set
///   bit  0   = DSTS USBLNKST nibble nonzero
pub fn core_attach_delay_code() -> u32 {
    unsafe {
        let gdscr = core::ptr::read_volatile(
            super::super::platform::bramble::usb_resources().gdsc as *const u32,
        );
        let snpsid = read(GSNPSID);
        let dsts = read(DSTS);
        let gdscr_bits = (gdscr & 0x7) << 3;
        let snpsid_ok = known_dwc_core_ip(snpsid) as u32;
        let halt = (dsts & DSTS_DEVCTRLHLT != 0) as u32;
        let link = ((dsts >> 18) & 0xf != 0) as u32;
        gdscr_bits | (snpsid_ok << 2) | (halt << 1) | link
    }
}

/// Power-recovery stage test for the attach-delay readout (4 bits):
///   bit 0 = the RPMh rail votes ACKed through the normal USB2 power path
///   bit 1 = the forced GDSC enable reached PWR_ON
///   bit 2 = GDSCR reads back with SW_COLLAPSE cleared
///   bit 3 = GSNPSID answers a DWC3 revision (the core answers MMIO)
pub fn core_power_recovery_code() -> u32 {
    unsafe {
        let votes = super::super::platform::bramble::apply_usb_power(true, false);
        let gdsc_on = super::super::platform::bramble::force_enable_usb30_gdsc();
        let gdscr = core::ptr::read_volatile(
            super::super::platform::bramble::usb_resources().gdsc as *const u32,
        );
        let collapse_cleared = (gdscr & 1 == 0) as u32;
        let snpsid = read(GSNPSID);
        let snpsid_ok = known_dwc_core_ip(snpsid) as u32;
        votes as u32 | ((gdsc_on as u32) << 1) | (collapse_cleared << 2) | (snpsid_ok << 3)
    }
}

/// Full core-recovery sequence (rails + GDSC + clocks + reset pulses) and a
/// binary attach-delay readout: the attach lands ~0.5 s after entry when the
/// DWC3 core still fails to answer GSNPSID, and ~4.0 s after entry when the
/// core answers. The 3.5 s separation is far outside the ±0.5 s attach noise
/// and stays inside the biter window.
pub fn core_alive_attach_delay_secs() -> u64 {
    unsafe {
        let _ = super::super::platform::bramble::apply_usb_power(true, false);
        let _ = super::super::platform::bramble::force_enable_usb30_gdsc();
        let _ = super::super::platform::bramble::usb_clock::configure_usb_clocks(
            super::super::platform::bramble::UsbBusVote::Nominal,
        );
        let _ = super::super::platform::bramble::enable_usb_clock_branches();
        let _ = super::super::platform::bramble::usb_reset::reset_usb_blocks(false);
        let snpsid = read(GSNPSID);
        let snpsid_ok = known_dwc_core_ip(snpsid);
        if snpsid_ok { 4_000 } else { 500 }
    }
}

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

/// Post-init-failure self-heal, run from the signal probe's polling context
/// after a failed handoff. The host is already attached to the session
/// pull-up (see the -110 runs), so whatever init stage gave up, the missing
/// tail can still be issued here. The order mirrors Linux's
/// `dwc3_gadget_soft_connect`: event buffers, then DEPSTARTCFG /
/// SETEPCONFIG / the EP0 OUT STARTTRANSFER while the core is still in its
/// post-reset state, and ONLY THEN Run/Stop. The DCFG device address is
/// cleared because the bootloader's fastboot address must not survive into
/// the new enumeration (a stale non-zero DEVADDR makes the core ignore the
/// host's default-address SETUP tokens). Gated by a build-time env var; the
/// the status code is kept for the retained trace and the Run/Stop-pair
/// readout.
pub fn u0_arm_recovery() -> u32 {
    if option_env!("FULLERENE_USB_U0_ARM_PROBE")
        .filter(|value| *value != "0")
        .is_none()
    {
        return 0xFFFF_FFFF;
    }
    unsafe {
        if EP0_SETUP_ARMED && ENDPOINTS_READY {
            U0_ARM_STATUS = 0;
            return 0;
        }
        // Android's gadget restart rebuilds endpoint resources while the
        // device controller is halted, then starts it again. The direct
        // handoff may already have asserted Run/Stop before this recovery
        // path is entered, so make that ordering an explicit Bramble A/B.
        // Without the stop, DEPSTARTCFG/SETEPCONFIG can retire while the
        // controller is still running but leave STARTTRANSFER with status 1
        // (No Resource) on this unit.
        if option_env!("FULLERENE_USB_U0_ARM_STOP_FIRST") == Some("1")
            && read(DCTL) & DCTL_RUN_STOP != 0
            && !run_stop_device(false)
        {
            U0_ARM_STATUS = 1;
            return 1;
        }
        let event_address = ep0_event_address();
        cache_clean(ep0_event_dma_base(), ep0_event_size());
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, ep0_event_size() as u32);
        acknowledge_ep0_event_count();
        EVENT_OFFSET = 0;
        // The recovery path runs after the Fastboot handoff boundary. DSTS
        // still reports the previous SS Fastboot session there, so trusting
        // ConnectSpd can restore DCFG.SuperSpeed on a USB2-only handoff. The
        // "forcehs" experiment proves or refutes exactly that stale-speed
        // failure mode by pinning recovery to High-Speed/64-byte EP0.
        // "gdbforce" repeats that experiment while sampling GDBGLTSSM at the
        // normal gate window, separating a stale DCFG speed setting from the
        // observed GDBGLTSSM link state.
        let stale_speed = read(DSTS) & DSTS_CONNECTSPD_MASK;
        let force_hs = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("forcehs")
            || option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("gdbforce");
        let speed = if force_hs { 0 } else { stale_speed };
        let mut dcfg = read(DCFG) & !(DCFG_SPEED_MASK | DCFG_DEVADDR_MASK);
        dcfg |= if force_hs {
            DCFG_HIGHSPEED
        } else if speed == DSTS_SUPERSPEED {
            DCFG_SUPERSPEED
        } else {
            DCFG_HIGHSPEED
        };
        write(DCFG, dcfg);
        GadgetDriver::reset(gadget_mut());
        udc_mut().reset();
        EP0_STATE = Ep0State::Setup;
        CONFIGURED = false;
        DATA_ENDPOINTS_READY = false;
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
        DATA_RESOURCE_INDEX = [0; 2];
        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            U0_ARM_STATUS = 4;
            return 4;
        }
        let max_packet = if !force_hs && speed == DSTS_SUPERSPEED {
            INITIAL_EP0_MAX_PACKET_SIZE
        } else {
            64
        };
        if !configure_endpoint(0, max_packet, false) {
            U0_ARM_STATUS = 5;
            return 5;
        }
        if !configure_endpoint(1, max_packet, false) {
            U0_ARM_STATUS = 6;
            return 6;
        }
        ENDPOINTS_READY = true;
        write(DALEPENA, 0b11);
        write(DEVTEN, direct_gadget_devten());
        // Prepare the EP0 OUT SETUP TRB now. The STARTTRANSFER decision is
        // bramble-specific: see the start-after-connect note below.
        prepare_ep0_setup_trb();
        // Bramble's normal start-after-connect path deliberately does NOT
        // issue STARTTRANSFER before Run/Stop: a pre-link-ON command can
        // wedge this core's endpoint command engine even when Run/Stop still
        // publishes the pull-up. Keep the recovery path consistent with the
        // proven init path: defer the arm until after Run/Stop, then retry
        // for 100 ms while the link trains to ON.
        let defer_start = cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect);
        if defer_start {
            U0_ARM_STATUS = 0;
        } else if start_transfer(0, ep0_trb_ptr(0)) {
            EP0_SETUP_ARMED = true;
            PENDING_SETUP_ARM = false;
        } else {
            U0_ARM_STATUS = 8;
        }
        if !run_stop_device(true) {
            U0_ARM_STATUS = 1;
            return 1;
        }
        if defer_start {
            let deadline =
                arch_counter().saturating_add(arch_counter_frequency().saturating_mul(100) / 1000);
            let mut armed = false;
            while arch_counter() < deadline {
                if EP0_SETUP_ARMED {
                    armed = true;
                    break;
                }
                if try_arm_setup() {
                    armed = true;
                    break;
                }
                super::super::timer::delay_us(200);
            }
            if !armed {
                U0_ARM_STATUS = 8;
            }
        }
        // U0_ARM_STATUS is 0 (the SETUP TRB is armed or deferred in the
        // proven start-after-connect order) or 8 (STARTTRANSFER did not
        // retire even after the link reached ON).
        U0_ARM_STATUS
    }
}

/// Mid-window rescue for the read/64 -110: the host's descriptor URB keeps
/// retrying its SETUP token until the 5 s `initial_descriptor_timeout`, so
/// a full endpoint re-arm while the host is still polling can complete the
/// stuck enumeration. Device soft reset first: the post-init
/// u0_arm_recovery runs at probe entry with the link down, where Start
/// Transfer is rejected, and any stuck core control state left by the
/// original arm attempt is only cleared by CSFTRST. The host port stays
/// connected while the core is stopped (calibrated: the gate-TRUE
/// Run/Stop stop is host-invisible), so the reset plus the re-arm are
/// invisible and the host simply sees its retries answered. The readout is
/// the enumeration outcome in the host journal (1234:0001 = the re-arm
/// landed; -110 again = it did not). Returns the u0_arm_recovery status.
/// Re-initialize the USB2 PHY after Run/Stop.  This tests whether the PHY
/// RX path can be recovered by re-running the full init sequence while the
/// DWC3 is in Run mode (link ON).  The hypothesis: the Stop→Start cycle
/// loses PHY RX state that can only be restored by re-programming the PHY
/// after the link is established.
pub fn phy_retry_after_link() -> bool {
    unsafe {
        // Wait for link ON (USBLNKST == 0 in DSTS)
        let frequency = super::super::timer::frequency();
        let deadline = super::super::timer::counter().saturating_add(frequency.saturating_mul(5));
        while super::super::timer::counter() < deadline {
            wdt_pet();
            let dsts = read(DSTS);
            if (dsts >> 18) & 0xf == 0 && dsts & DSTS_DEVCTRLHLT == 0 {
                break;
            }
        }
        // Check if we're in U0
        let dsts = read(DSTS);
        if (dsts >> 18) & 0xf != 0 {
            return false;
        }
        // Re-run the full PHY init sequence
        if !cfg!(fullerene_aarch64_usb_skip_usb2_phy_reset) {
            if !super::super::platform::bramble::pulse_usb2_phy_reset() {
                return false;
            }
        }
        phy::init_hsphy();
        config::configure_usb2_phy_interface();
        // Clear SUSPHY and ENBLSLPM after PHY re-init
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1018);
        write(GUSB2PHYCFG0, usb2);
        true
    }
}

pub fn u0_arm_window_recovery() -> u32 {
    unsafe {
        if !device_soft_reset() {
            return 9;
        }
        // Force the full tail: the armed flags may be stale (set by a
        // rejected arm) or accurate; the re-issue is idempotent on a
        // freshly soft-reset core.
        EP0_SETUP_ARMED = false;
        ENDPOINTS_READY = false;
        u0_arm_recovery()
    }
}

/// Queue a host-visible blip readout to be emitted once the link reaches
/// ON. The host only attaches ~10 s after boot, so a blip issued right
/// after the failed handoff would be invisible; the poll loop emits it via
/// `try_u0_blip` when the core reports the link ON.
pub fn u0_arm_set_blips(count: u32) {
    // Clearing is unconditional so a diagnostic gate can take ownership of
    // the transport even when the legacy U0-arm feature was enabled.
    if count != 0
        && option_env!("FULLERENE_USB_U0_ARM_PROBE")
            .filter(|value| *value != "0")
            .is_none()
    {
        return;
    }
    unsafe {
        U0_BLIP_PENDING = count.min(6);
    }
}

/// Queue the host-visible pre-EP0 arm result for the direct handoff. One
/// Run/Stop pair means that SETUP STARTTRANSFER succeeded; two means the
/// DSTS HALT gate rejected the attempt; three means the command completed
/// with an error; four means it timed out; five is an unclassified/no-attempt
/// result. The count is intentionally encoded only in the diagnostic blip;
/// the EP0/TRB path itself is unchanged. If the link is not U0 yet, the
/// pending count remains for the timer poll, which is the reliable late-U0
/// readout. A 30-second fallback makes the marker host-visible even when the
/// device-side link FSM never reaches U0 before host enumeration.
pub fn arm_blip_queue() {
    if option_env!("FULLERENE_USB_ARM_BLIP")
        .filter(|value| *value != "0")
        .is_none()
    {
        return;
    }
    unsafe {
        let count = if EP0_SETUP_ARMED {
            1
        } else {
            match SETUP_ARM_FAILURE_STAGE {
                1 => 2,
                2 => 3,
                3 => 4,
                _ => 5,
            }
        };
        if EP0_SETUP_ARMED {
            U0_BLIP_PENDING = U0_BLIP_PENDING.max(count);
            // Use the same guarded helper as the timer path. It does not
            // force a Run/Stop transition while the link is training, and
            // leaves the queued differential intact for a later U0 sample.
            try_u0_blip();
        } else {
            U0_BLIP_PENDING = U0_BLIP_PENDING.max(count);
        }
        ARM_BLIP_QUEUED = true;
        ARM_BLIP_FORCE_DEADLINE =
            arch_counter().saturating_add(arch_counter_frequency().saturating_mul(30));
        try_u0_blip();
    }
}

/// Emit the queued blips once, when the link is ON. Same link test as
/// `try_arm_setup` (it is also issued after the arm attempt, so the SETUP
/// TRB is in place before the blip's re-attach re-runs enumeration).
pub(super) unsafe fn try_u0_blip() {
    unsafe {
        if U0_BLIP_PENDING == 0 {
            return;
        }
        let dsts = read(DSTS);
        let force = ARM_BLIP_FORCE_DEADLINE != 0 && arch_counter() >= ARM_BLIP_FORCE_DEADLINE;
        if !force && (dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0) {
            return;
        }
        let count = U0_BLIP_PENDING;
        U0_BLIP_PENDING = 0;
        ARM_BLIP_FORCE_DEADLINE = 0;
        ARM_BLIP_DONE = true;
        runstop_blips(count);
    }
}

/// Emit up to `count` Run/Stop blips only when the core reports the link ON
/// (running, unhalted, USBLNKST == 0). A non-U0 link is not a valid transport
/// precondition, so this is a silent no-op.
pub fn runstop_blips_link_on(count: u32) {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return;
        }
        runstop_blips(count);
    }
}

/// Host-visible diagnostic transport: toggle the qpr1 DWC3 Run/Stop control
/// `count` times. Each stop/run pair is one disconnect/re-attach pair in the
/// host kernel log. This is deliberately called only after the sampled state
/// has been latched. It is not a read-only observation.
pub fn runstop_blips(count: u32) {
    unsafe {
        for _ in 0..count.min(6) {
            let _ = run_stop_device_no_readback(false);
            super::super::timer::delay_ms(300);
            let _ = run_stop_device_no_readback(true);
            super::super::timer::delay_ms(200);
        }
    }
}

/// Fast Run/Stop transport for categorical readouts: 100 ms disconnected +
/// 70 ms reconnected per pair. Six pairs fit in about 1.2 s. The status
/// readback is intentionally skipped here to stay inside the secure-WDT
/// window; the normal handoff still uses the checked source-aligned helper.
pub fn runstop_blips_fast(count: u32) {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return;
        }
        for _ in 0..count.min(6) {
            let _ = run_stop_device_no_readback(false);
            super::super::timer::delay_ms(100);
            let _ = run_stop_device_no_readback(true);
            super::super::timer::delay_ms(70);
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
pub(super) unsafe fn enable_gadget_controller_irq() {
    unsafe {
        let _ = super::super::platform::gicv3::init(
            super::super::platform::bramble::GICD_BASE,
            super::super::platform::bramble::GICR_BASE,
            Some(super::super::platform::bramble::USB_DWC3_IRQ),
        );
    }
}

#[cfg(not(fullerene_aarch64_usb_gadget_handoff_probe))]
pub(super) unsafe fn enable_gadget_controller_irq() {}
