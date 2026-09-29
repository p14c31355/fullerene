//! USB link, EP0, and controller diagnostic signal predicates.

use super::*;

/// Update the signal-probe latches. Called from `ep0_signal_code()` so a
/// polling-only consumer does not need an extra tracing channel.
pub(super) unsafe fn update_signal_latches() {
    unsafe {
        // In start-after-connect mode the initial TRB is intentionally not
        // armed until the host link is usable.  Before that point the setup
        // buffer aliases an unowned TRB slot, so its reset/firmware contents
        // are not evidence of a host SETUP packet.  Keep every TRB/buffer
        // latch behind the same ownership bit used by the arm guard.
        let setup_armed = EP0_SETUP_ARMED;
        // The core retires a TRB by clearing HWO over DMA. Invalidate the
        // cached line first so the CPU observes the controller's write.
        let trb = ep0_trb_ptr(0);
        let host_reset_seen = SIGNAL_USB_RESET_SEEN;
        if setup_armed && host_reset_seen {
            cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
            if read_volatile(addr_of!((*trb).ctrl)) & TRB_HWO == 0 {
                SIGNAL_SETUP_TRB_RETIRED = true;
            }
        }
        let setup = ep0_setup_data_ptr() as *const u8;
        if setup_armed && host_reset_seen {
            cache_invalidate(setup as usize, 8);
        }
        if setup_armed && host_reset_seen && setup == trb.cast::<u8>() {
            // qpr1 deliberately aliases the eight-byte SETUP payload to the
            // first EP0 TRB. Before a host packet arrives, the first two
            // words therefore still contain the TRB's DMA destination. A
            // simple "any non-zero byte" test would report a false SETUP on
            // every armed transfer; compare the aliased words with the
            // expected DMA address instead. A real GET_DESCRIPTOR SETUP
            // overwrites them with the packet bytes.
            let expected = dma_iova_for(setup as usize);
            let current_low = read_volatile(setup.cast::<u32>());
            let current_high = read_volatile(setup.add(4).cast::<u32>());
            if current_low != expected as u32 || current_high != (expected >> 32) as u32 {
                SIGNAL_SETUP_PACKET_RECEIVED = true;
            }
        } else if setup_armed && host_reset_seen {
            // The separate-buffer A/B is zeroed before arming, so any
            // non-zero byte is a valid indication that the controller DMAed
            // a SETUP packet into it.
            for offset in 0..8 {
                if read_volatile(setup.add(offset)) != 0 {
                    SIGNAL_SETUP_PACKET_RECEIVED = true;
                    break;
                }
            }
        }
        // DSTS_HIGHSPEED is zero, so the link state cannot be read from
        // ConnectSpd. A changing SOF frame number instead proves the core is
        // receiving packets from the host at the transaction level.
        if host_reset_seen {
            let sofn = ((read(DSTS) & (0x3fff << 3)) >> 3) as u16;
            if !SIGNAL_SOF_BASELINED {
                SIGNAL_LAST_SOFFN = sofn;
                SIGNAL_SOF_BASELINED = true;
            } else if sofn != SIGNAL_LAST_SOFFN {
                SIGNAL_LAST_SOFFN = sofn;
                SIGNAL_SOF_SEEN = true;
            }
            // Latch the core's view of the USB2 link for the link-state ladder.
            let dsts = read(DSTS);
            match (dsts >> 18) & 0xf {
                0 => SIGNAL_LNKST_U0 = true,      // ON: link up at the detected speed
                5 => SIGNAL_LNKST_RXDET = true,   // RX.DETECT: core still waiting
                7 => SIGNAL_LNKST_POLLING = true, // POLLING: chirp phase observed
                14 => SIGNAL_LNKST_RESET = true,  // RESET: bus reset observed
                _ => {}
            }
            if dsts & DSTS_DEVCTRLHLT != 0 || read(DCTL) & DCTL_RUN_STOP == 0 {
                // A halted core or a cleared Run/Stop after a verified start makes
                // the physical attach a QSCRATCH session-override phantom.
                SIGNAL_CORE_HALTED = true;
            }
        }
    }
}

/// Encode the polled EP0/DMA observables as one host-visible code. The probe
/// drops the physical pull-up `3 * code` seconds after attach, so the host
/// dmesg delta between "new high-speed USB device" and "USB disconnect" names
/// the first stage that provably worked:
///   1 = a host USB Reset event reached the event-ring consumer
///   2 = DWC3 retired the armed EP0 SETUP TRB (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frames are arriving (transaction-level RX alive)
///   0 = none of the above (no drop; the host only sees its own -110)
/// SMMU read-only probe codes are handled by `probe_smmu_stream_state()`.
pub fn ep0_signal_code() -> u32 {
    unsafe {
        update_signal_latches();
        if SIGNAL_EVENT_DELIVERED && SIGNAL_USB_RESET_SEEN {
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

/// True once the EP0 SETUP payload has been observed either by the retained
/// setup latch or by the DMA-buffer sampler. The signal probe uses this as a
/// precise pre-response boundary: it is intentionally earlier than any
/// descriptor DATA/TRB completion and does not infer a wire-level CRC error.
pub fn ep0_setup_packet_seen() -> bool {
    unsafe { SIGNAL_SETUP_PACKET_RECEIVED }
}

/// True once the polling path has consumed a non-empty DWC3 device/event-ring
/// record. This is deliberately one boundary earlier than SETUP parsing and
/// lets the signal probe test event ownership without changing PHY or TRB
/// configuration.
pub fn ep0_event_delivered() -> bool {
    unsafe { SIGNAL_EVENT_DELIVERED }
}

/// Compact same-boot progress mask for the read-only diagnostic channel:
/// bit 0 = a DWC3 event reached the software consumer, bit 1 = the armed
/// EP0 SETUP TRB retired, bit 2 = a non-zero SETUP payload was DMAed, and
/// bit 3 = the DSTS SOF frame changed while polling. This combines latches
/// from the whole descriptor window so one selector can distinguish
/// link-up-without-RX from RX-without-event-DMA.
pub fn ep0_progress_mask() -> u32 {
    unsafe {
        update_signal_latches();
        u32::from(SIGNAL_EVENT_DELIVERED)
            | (u32::from(SIGNAL_SETUP_TRB_RETIRED) << 1)
            | (u32::from(SIGNAL_SETUP_PACKET_RECEIVED) << 2)
            | (u32::from(SIGNAL_SOF_SEEN) << 3)
    }
}

/// True once the DWC3 device-event stream delivered an ERRATIC_ERROR,
/// CMD_COMPLETE, or OVERFLOW notification. The signal probe uses this to
/// separate a controller-reported device error from the host's generic
/// xHCI `-EPROTO` completion.
pub fn dwc3_device_error_seen() -> bool {
    unsafe { SIGNAL_DWC3_DEVICE_ERROR }
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

/// One-shot raw link-state readout for the lnk-nib gate: the 4-bit
/// USBLNKST nibble, or 16 when the core reads halted, or 17 when Run/Stop
/// reads back cleared (a QSCRATCH/VBUSVLDEXT0 phantom attach - the PHY can
/// still answer the host's port reset and chirps while the core is out of
/// the loop, which would masquerade as a link-FSM desync).
pub fn ep0_raw_link_nibble() -> u32 {
    unsafe {
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 {
            return 16;
        }
        if read(DCTL) & DCTL_RUN_STOP == 0 {
            return 17;
        }
        (dsts >> 18) & 0xf
    }
}

/// Heartbeat control: toggle DCTL Run/Stop in one-second intervals starting
/// immediately after the verified connect. If the host still records a full
/// 5-second descriptor timeout against a continuously attached port, the
/// post-attach core ignores DCTL Run/Stop clears and the pull-up cannot be
/// dropped by software at all.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
pub(super) fn ep0_signal_heartbeat_check() {
    if option_env!("FULLERENE_USB_SIGNAL_HEARTBEAT") != Some("1") {
        return;
    }
    unsafe {
        for _ in 0..3 {
            let _ = run_stop_device(false);
            super::super::timer::delay_ms(1000);
            let _ = run_stop_device(true);
            super::super::timer::delay_ms(1000);
        }
    }
}

/// Control variant of the early drop: run immediately BEFORE the first
/// Run/Stop. If the pull-up still appears with this unconditional drop, the
/// Qualcomm session overrides do not gate the attach at all and the pull-up
/// is purely core-driven (DCTL.TermSelect).
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
pub(super) fn ep0_signal_pre_runstop_drop_check() {
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
///   1 = a host USB Reset event reached the event-ring consumer
///   2 = the armed EP0 SETUP TRB was retired (HWO cleared over DMA)
///   3 = the SETUP packet payload was DMAed into the setup buffer
///   5 = SOF frame numbers are changing (transaction-level RX alive)
///   6 = raw DSTS USBLNKST entered RESET, without requiring an event record
///   7 = raw DSTS USBLNKST entered POLLING, without requiring an event record
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
pub(super) fn ep0_signal_early_drop_check() {
    let condition = match option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP") {
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("5") => 5,
        Some("6") => 6,
        Some("7") => 7,
        Some("9") => 9,
        _ => 0,
    };
    if condition == 0 {
        return;
    }
    unsafe {
        let mut observed = 0;
        let mut ms = 0u32;
        // Bramble can take roughly 14 seconds from Fastboot's disconnect to
        // the host's first high-speed attach. The old 1.5-second window ended
        // before any SETUP packet could arrive, so it could not distinguish a
        // dead EP0 from normal pre-attach delay. Keep the safe 20-second
        // default, but honor the harness' explicit observation-window knob so
        // a slower attach can be measured without another code change.
        let observe_ms = option_env!("FULLERENE_USB_PROBE_OBSERVE_SECS")
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|secs| secs.checked_mul(1_000))
            .map(|millis| millis.min(u32::MAX as u64) as u32)
            .filter(|millis| *millis > 0)
            .unwrap_or(20_000);
        while ms < observe_ms {
            ms += 1;
            if condition != 9 {
                // Consume any pending events first. Condition 1 is narrowed
                // to the host USB Reset event so inherited Fastboot records
                // cannot masquerade as an on-wire boundary.
                poll_ep0_event_ring();
                update_signal_latches();
                // Select the requested latch directly. Event delivery is a
                // prerequisite for the later observations, so a priority
                // ladder here would make code 2/3/5 impossible whenever the
                // reset or Connect Done event had already arrived.
                observed = match condition {
                    1 if SIGNAL_EVENT_DELIVERED && SIGNAL_USB_RESET_SEEN => 1,
                    2 if SIGNAL_SETUP_TRB_RETIRED => 2,
                    3 if SIGNAL_SETUP_PACKET_RECEIVED => 3,
                    5 if SIGNAL_SOF_SEEN => 5,
                    6 if ((read(DSTS) >> 18) & 0xf) == 14 => 6,
                    7 if ((read(DSTS) >> 18) & 0xf) == 7 => 7,
                    _ => 0,
                };
                if observed == condition {
                    break;
                }
            }
            super::super::timer::delay_ms(1);
        }
        if condition == 9 || observed == condition {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_4544 | (condition << 8));
            ep0_signal_drop_pullup();
        }
    }
}

/// Continue the early-drop diagnostic after the handoff has returned to the
/// direct probe's normal polling owner. The initial bounded window can end
/// before xHCI publishes its first USB2 attach; keep the same condition
/// read-only until the polling loop's ordinary recovery deadline instead of
/// making the pre-return sleep unbounded.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
pub fn ep0_signal_early_drop_poll() -> bool {
    let condition = match option_env!("FULLERENE_USB_SIGNAL_EARLY_DROP") {
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some("5") => 5,
        Some("6") => 6,
        Some("7") => 7,
        Some("9") => 9,
        _ => 0,
    };
    if condition == 0 {
        return false;
    }
    unsafe {
        update_signal_latches();
        let observed = match condition {
            1 if SIGNAL_EVENT_DELIVERED && SIGNAL_USB_RESET_SEEN => true,
            2 if SIGNAL_SETUP_TRB_RETIRED => true,
            3 if SIGNAL_SETUP_PACKET_RECEIVED => true,
            5 if SIGNAL_SOF_SEEN => true,
            6 if ((read(DSTS) >> 18) & 0xf) == 14 => true,
            7 if ((read(DSTS) >> 18) & 0xf) == 7 => true,
            9 => true,
            _ => false,
        };
        if observed {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x5349_4550 | (condition << 8));
            ep0_signal_drop_pullup();
            return true;
        }
    }
    false
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
/// Latch the "core link FSM read U0" fact for the lnk-ever-on gate. Called
/// from poll, so it samples every loop iteration of whichever loop owns the
/// controller.
pub fn link_on_sample() {
    unsafe {
        let dsts = read(DSTS);
        if !LNK_EVER_ON && dsts & DSTS_DEVCTRLHLT == 0 && (dsts >> 18) & 0xf == 0 {
            LNK_EVER_ON = true;
        }
        if !LNK_MID_SEEN {
            let state = (dsts >> 18) & 0xf;
            if state == 8 || state == 9 || state == 11 || state == 14 || state == 15 {
                LNK_MID_SEEN = true;
            }
        }
    }
}

/// "lnk3" gate readout: did any poll sample since probe entry observe the
/// core's link FSM in a mid-transaction state (RECOV=8, HRESET=9, LPBK=11,
/// RESET=14, RESUME=15)? Latched in link_on_sample.
pub fn lnk_mid_transaction_seen() -> bool {
    unsafe { LNK_MID_SEEN }
}

/// Window-end "arm alive" probe for the armalive gate: has ANY EP0 SETUP
/// Start Transfer retired since probe entry, and has the host DMA'd a
/// SETUP into the buffer? Bit 0 = an armed TRB is still pending (a
/// retired arm whose SETUP the core never latched), bit 1 = a host DMA'd
/// SETUP sits in the buffer (a retired arm the core consumed). Both zero
/// = no Start Transfer ever retired in the window (persistent command
/// wedge). The buffer read mirrors the XferComplete path's
/// invalidate+read; the content is never zeroed after consumption, so the
/// read also covers an arm consumed inside the window.
/// Raw core link-FSM state (DSTS.USBLNKST, bits 21:18). This core is a
/// DWC_usb31 (>= 1.94a): upstream v5.10 core.h defines
/// DWC3_DSTS_USBLNKST_MASK as (0x0f << 18) and encodes the link states as
/// U0 = 0x00 (in HS, "ON"), U1 = 0x01, U2 = 0x02 (HS "SLEEP"),
/// U3 = 0x03 (HS "SUSPEND"), SS_DIS = 0x04, RX_DET = 0x05,
/// SS_INACT = 0x06, POLL = 0x07, RECOV = 0x08, HRESET = 0x09,
/// CMPLY = 0x0a, LPBK = 0x0b, RESET = 0x0e, RESUME = 0x0f - the same
/// table the AOSP dwc3 driver on this SoC uses. The legacy shift-18
/// guard reads exactly this field, so it is the correct reference.
/// (The older DWC_usb30 layout, USBLNKST at bits 23:20 with U0 = 1, does
/// NOT apply here.) The lnkalive gate (third pass) bites on the
/// mid-transaction states 8/9/11/14/15, so an early return names a core
/// stuck in the reset/resume handshake at the sample instant; a
/// non-early return names a link-down state (RX_DET/SS_INACT/SS_DIS/
/// POLL/CMPLY).
pub fn dsts_raw_link_state() -> u32 {
    unsafe { (read(DSTS) >> 18) & 0xf }
}

/// Raw link-transaction debug state from GDBGLTSSM. The Qualcomm DWC3
/// glue treats bits 25:22 as LINKSTATE (the same 4-bit selector used by
/// DSTS.USBLNKST, but read directly from the link-layer TX/RX FSM); this
/// distinguishes a reserved/legacy DSTS encoding such as 13 from the
/// physical LTSSM state and its real sub-state bits.
pub fn gdb_ltssm_link_state() -> u32 {
    unsafe {
        let snpsid = read(GSNPSID);
        let offset = if matches!(snpsid >> 16, DWC31_IP | DWC32_IP) {
            DWC31_LINK_GDBGLTSSM
        } else {
            GDBGLTSSM
        };
        (read(offset) >> 22) & 0xf
    }
}

/// DSTS SOF frame number (bits 16:3). A value that changes across samples
/// proves the core is receiving packets from the host at the transaction
/// level even when the link FSM never reports U0 or a mid-transaction
/// state.
/// DSTS.DEVCTRLHLT readout for the haltbit gate.
pub fn dsts_device_ctrl_halted() -> bool {
    unsafe { read(DSTS) & DSTS_DEVCTRLHLT != 0 }
}

/// DCTL.RUN_STOP readout for the dctlbit gate.
pub fn dctl_run_stop_set() -> bool {
    unsafe { read(DCTL) & DCTL_RUN_STOP != 0 }
}

/// One-shot DSTS word snapshot for raw readout gates.
pub fn dsts_word_snapshot() -> u32 {
    unsafe { read(DSTS) }
}

pub fn dsts_sof_frame_number() -> u32 {
    unsafe { (read(DSTS) >> 3) & 0x3fff }
}

/// U0_ARM_STATUS readout for the armstat gate: 0 = the pre-Run/Stop EP0
/// OUT STARTTRANSFER retired cleanly, 8 = it did not retire in the command
/// timeout, and the smaller values name the preceding setup step.
pub fn u0_arm_status_probe() -> u32 {
    unsafe { U0_ARM_STATUS }
}

/// Return the newest retained EP1 STARTTRANSFER command word. The value is
/// the completed DEPCMD register with the controller's status nibble intact
/// (bit 31 set = the command timed out; bit 16 alone = healthy XferRscIdx 1);
/// `0xFFFF_FFFF` means that no EP1 command was captured in this boot's trace.
pub fn ep1_start_status_probe() -> u32 {
    unsafe {
        harvest_trace_outcome();
        TRACE_HARVEST_EP1
    }
}

pub fn armalive_probe() -> u32 {
    unsafe {
        let mut state = 0u32;
        if EP0_SETUP_ARMED {
            state |= 0x1;
        }
        let setup = ep0_setup_data_ptr();
        cache_invalidate(setup as usize, 8);
        for offset in 0..8 {
            if read_volatile(setup.add(offset)) != 0 {
                state |= 0x2;
                break;
            }
        }
        state
    }
}

pub fn cmd_gate_condition_met() -> Option<bool> {
    // "mrad" skips every gate branch and the generic gate so the probe
    // falls through to the tail readout: the composite diag code rides the
    // tail wait (code*700 ms) ahead of the PSCI reset, and the Android
    // return time names the code.
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("mrad") {
        return None;
    }
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
            "timeout" => TRACE_HARVEST & 0x8000_0000 != 0,
            "done" => TRACE_HARVEST != 0xFFFF_FFFF && TRACE_HARVEST & 0x8000_0000 == 0,
            "last-timeout" => TRACE_HARVEST_LAST & 0x8000_0000 != 0,
            "last-done" => {
                TRACE_HARVEST_LAST != 0xFFFF_FFFF && TRACE_HARVEST_LAST & 0x8000_0000 == 0
            }
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
            // A timed-out command carries the 0x8000_0000 flag; bit 16 alone
            // is a healthy XferRscIdx=1 completion on physical EP1.
            "ep1-wedge" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0x8000_0000) != 0
            }
            // A clean data-phase start: the command retired with status 0
            // and the returned XferRscIdx equals EP1's allocated resource 1.
            "ep1-clean" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0x8000_0000) == 0
                    && (TRACE_HARVEST_EP1 & 0x7f_ffff) == 0x1_0000
            }
            "ep1-done" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0x8000_0000) == 0
                    && (TRACE_HARVEST_EP1 & 0xf000) == 0
            }
            "ep1-nores" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0x8000_0000) == 0
                    && (TRACE_HARVEST_EP1 & 0xf000) == 0x1000
            }
            // The DEPCMD status is bits 15:12. Ignore CMDIOC and any other
            // non-status completion bits: a zero status nibble is a command
            // success even when the raw register is not exactly zero.
            "ep1-status-success" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0xf000) == 0
            }
            "ep1-status-nores" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0xf000) == 0x1000
            }
            "ep1-status-bus-expiry" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF && (TRACE_HARVEST_EP1 & 0xf000) == 0x2000
            }
            "ep1-status-other" => {
                TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                    && (TRACE_HARVEST_EP1 & 0xf000) != 0
                    && (TRACE_HARVEST_EP1 & 0xf000) != 0x1000
                    && (TRACE_HARVEST_EP1 & 0xf000) != 0x2000
            }
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
            // Post-Run/Stop event-DMA differential: bit 0 = GETEPSTATE
            // returned, bit 1 = the live EP0 transfer remained armed, bit 2 =
            // the event ring received a word. The gate is useful only when
            // the optional probe was compiled in; a missing record is false.
            "post" => TRACE_HARVEST_POST == 0x1_0007,
            "post-command" => TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 1 != 0,
            "post-armed" => TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 2 != 0,
            "post-event" => TRACE_HARVEST_POST != 0xFFFF_FFFF && TRACE_HARVEST_POST & 4 != 0,
            "post-record" => TRACE_HARVEST_POST != 0xFFFF_FFFF,
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
            // Did the core's link FSM read U0 at ANY poll sample since boot
            // (latched in link_on_sample), even if it dropped again before
            // this gate's evaluation?
            "lnk-ever-on" => LNK_EVER_ON,
            "lnk-reset" => (read(DSTS) >> 18) & 0xf == 1,
            "lnk-suspend" => {
                let lnkst = (read(DSTS) >> 18) & 0xf;
                lnkst >= 5 && lnkst != 0xf
            }
            "halt" => read(DSTS) & DSTS_DEVCTRLHLT != 0,
            "epready" => ENDPOINTS_READY,
            "ep0armed" => EP0_SETUP_ARMED,
            "ep0setup" => EP0_STATE == Ep0State::Setup,
            // Direct post-handoff recovery result. Unlike `ep0armed`, this
            // preserves the failure stage even when the poll loop later
            // clears or retries the software state. The numeric values are
            // U0_ARM_STATUS: 0 = armed/deferred success, 1 = Run/Stop
            // failure, 4 = DEPSTARTCFG failure, 5/6 = EP0 config failure,
            // and 8 = SETUP STARTTRANSFER did not retire after Run/Stop.
            "u0-status0" => U0_ARM_STATUS == 0,
            "u0-status1" => U0_ARM_STATUS == 1,
            "u0-status4" => U0_ARM_STATUS == 4,
            "u0-status5" => U0_ARM_STATUS == 5,
            "u0-status6" => U0_ARM_STATUS == 6,
            "u0-status8" => U0_ARM_STATUS == 8,
            // Direct-path (init_with_super_speed) EP command sequence: how far
            // did the init get (is4=DEPSTARTCFG issued, is5=DEPCFG ep0,
            // is6=DEPCFG ep1), did the DEPSTARTCFG/DEPCFG command retire
            // (CMDACT bit 10 clear == done, set == the core never processed
            // it), and was the core ready (DCNRD bit 29) / halted (bit 22) /
            // link U0 at the first endpoint command?
            // Pre-DEPSTARTCFG progress: is2 = core reset + global control
            // done (a FALSE here names a core_soft_reset/CSFTRST failure),
            // is3 = post-reset setup + SMMU boundary done.
            "is2" => INIT_STAGE >= 2,
            "is3" => INIT_STAGE >= 3,
            "is4" => INIT_STAGE >= 4,
            "is5" => INIT_STAGE >= 5,
            "is6" => INIT_STAGE >= 6,
            // Core device-state at the moment the FIRST endpoint command was
            // ISSUED (DSTS.DEVCTRL field, bits 13:11): 0 = Reset, 1 =
            // Run/Stop, 5 = Suspend. The post-command DSTS captures are
            // post-timeout and cannot distinguish "never processed" from
            // "processed late".
            "ds-pre-rst" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF && (INIT_DEPSTART_PRE_DSTS >> 11) & 0x7 == 0
            }
            "ds-pre-rs" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF && (INIT_DEPSTART_PRE_DSTS >> 11) & 0x7 == 1
            }
            "ds-pre-susp" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF && (INIT_DEPSTART_PRE_DSTS >> 11) & 0x7 == 5
            }
            "ds-pre-halt" => {
                INIT_DEPSTART_PRE_DSTS != 0xFFFF_FFFF
                    && INIT_DEPSTART_PRE_DSTS & DSTS_DEVCTRLHLT != 0
            }
            // Live core state at gate-eval time (after init + fallback +
            // observation window): which device state does the core sit in,
            // and what is software asking for in DCTL?
            "dsts-rs" => (read(DSTS) >> 11) & 0x7 == 1,
            "dsts-rst" => (read(DSTS) >> 11) & 0x7 == 0,
            "dctl-run" => read(DCTL) & DCTL_RUN_STOP != 0,
            "dctl-csf" => read(DCTL) & DCTL_CSFTRST != 0,
            // Register-file liveness at gate eval: an all-ones readback
            // means the DWC3 aperture is unreachable (core clock/power down)
            // - a different failure class from a stuck reset handshake.
            "dctl-ok" => read(DCTL) != 0xFFFF_FFFF,
            "dsts-ok" => read(DSTS) != 0xFFFF_FFFF,
            // Core state at the moment the soft reset started (the
            // inheritance from the Fastboot teardown, before CSFTRST).
            "pre-res-rst" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && (INIT_PRE_RESET_DSTS >> 11) & 0x7 == 0
            }
            "pre-res-rs" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && (INIT_PRE_RESET_DSTS >> 11) & 0x7 == 1
            }
            "pre-res-susp" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && (INIT_PRE_RESET_DSTS >> 11) & 0x7 == 5
            }
            "pre-res-halt" => {
                INIT_PRE_RESET_DSTS != 0xFFFF_FFFF && INIT_PRE_RESET_DSTS & DSTS_DEVCTRLHLT != 0
            }
            // EP0 IN (physical endpoint 1) command outcome, mirroring the
            // DEPSTARTCFG/EP0-OUT gates.
            "ep1-stuck" => INIT_EPCFG1_RAW != 0xFFFF_FFFF && INIT_EPCFG1_RAW & DEPCMD_CMDACT != 0,
            "ep1-lnk0" => INIT_EPCFG1_RAW != 0xFFFF_FFFF && (INIT_EPCFG1_DSTS >> 18) & 0xf == 0,
            // Retained-trace harvest of the DEPSTARTCFG / SETTRANSFRESOURCE
            // commands (bit 12 = the command timed out with CMDACT stuck):
            // the harvest re-reads this run's live trace at gate eval, so
            // these cross-check the INIT_* snapshot gates above.
            "cfg-hv" => TRACE_HARVEST_CFG != 0xFFFF_FFFF,
            "cfg-hv-done" => {
                TRACE_HARVEST_CFG != 0xFFFF_FFFF && TRACE_HARVEST_CFG & 0x8000_0000 == 0
            }
            "cfg-hv-stuck" => TRACE_HARVEST_CFG & 0x8000_0000 != 0,
            "rsc-hv" => TRACE_HARVEST_RSC != 0xFFFF_FFFF,
            "rsc-hv-done" => {
                TRACE_HARVEST_RSC != 0xFFFF_FFFF && TRACE_HARVEST_RSC & 0x8000_0000 == 0
            }
            "rsc-hv-stuck" => TRACE_HARVEST_RSC & 0x8000_0000 != 0,
            "ds-stuck" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_RAW & DEPCMD_CMDACT != 0
            }
            "ds-done" => INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_RAW & DEPCMD_CMDACT == 0,
            "ep0-stuck" => INIT_EPCFG0_RAW != 0xFFFF_FFFF && INIT_EPCFG0_RAW & DEPCMD_CMDACT != 0,
            "ep0-ok" => INIT_EPCFG0_OK,
            "ep1-ok" => INIT_EPCFG1_OK,
            "ds-dcnrd" => INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_DSTS & DSTS_DCNRD != 0,
            "ep0-dcnrd" => INIT_EPCFG0_RAW != 0xFFFF_FFFF && INIT_EPCFG0_DSTS & DSTS_DCNRD != 0,
            "ds-halt" => {
                INIT_DEPSTART_RAW != 0xFFFF_FFFF && INIT_DEPSTART_DSTS & DSTS_DEVCTRLHLT != 0
            }
            "ds-lnk0" => INIT_DEPSTART_RAW != 0xFFFF_FFFF && (INIT_DEPSTART_DSTS >> 18) & 0xf == 0,
            "ep0-lnk0" => INIT_EPCFG0_RAW != 0xFFFF_FFFF && (INIT_EPCFG0_DSTS >> 18) & 0xf == 0,
            other => u32::from_str_radix(other.trim_start_matches("0x"), 16)
                .map(|value| TRACE_HARVEST == value)
                .unwrap_or(false),
        };
        Some(ok)
    }
}
