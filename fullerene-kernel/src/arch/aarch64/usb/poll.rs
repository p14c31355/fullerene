//! Early-boot and timer-driven DWC3 event polling.

use super::*;

/// ONE-SHOT BRACKET for the post-handoff consumer (selector
/// `usb2-live-pollbracket`).
///
/// Question: on this profile, does anything consume the EP0 SETUP buffer after
/// the handoff returns? If nothing does, the host's first GET_DESCRIPTOR is
/// never answered - the measured boundary.
///
/// Five host-visible pulses, counted from usbmon CCS rows, in this order:
///
///   1  the handoff readout arm's unconditional control pulse
///   2  the same arm's compile-time proof: it exists only when the
///      `option_env!` word really is this selector, so pulse 1 alone means the
///      instrument was compiled out and the reading is invalid
/// then one pulse per chain stage reached, in this order:
///
///   1   the arm window is about to run          (gadget_handoff.rs)
///   2   the arm window exited
///   3   the deferred DEVTEN write returned
///   4   milestone 21 and its trace marker returned
///   5   the post-runstop signal block returned
///   6   `gate_flow_blip()` returned
///   7   the handoff is about to return true
///   8   the handoff call returned               (usb_probe.rs)
///   9   `poll()` was entered                    (`poll()` in this file)
///   10  `poll_setup_buffer()` returned
///   11  `poll_ep0_event_ring()` returned - a whole iteration completed
///
/// The stages are monotone: each fires at most once, and a stage is skipped
/// only when the code between it and the previous stage does not run at all
/// (for example the arm-window stages are unreachable in a profile that skips
/// the window). Because the reachable set is known per configuration, the total
/// count still localises the stop: count = 2 + the number of reachable stages
/// that fired, and a missing tail reads as "the path stopped before here"
/// rather than as an independent zero.
///
/// Caveat from this project's own record: `ccs_pulse` toggles DCTL Run/Stop, so
/// the observation can move the subject (the run that measured this produced
/// `-71` where the plain profile gives `-110`). Read the pulse counts as the
/// result and treat the host errno as secondary.
static mut PROBE_BRACKET_STAGE: u8 = 0;

pub(crate) fn probe_bracket_selected() -> bool {
    matches!(
        option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT"),
        Some("usb2-live-pollbracket")
    )
}

/// Emit the bracket pulse for `stage`, once, and only if the previous stage
/// already fired. Inert in every other build, so no other profile is perturbed.
pub fn probe_bracket_mark(stage: u8) {
    if !probe_bracket_selected() {
        return;
    }
    unsafe {
        if PROBE_BRACKET_STAGE < stage {
            PROBE_BRACKET_STAGE = stage;
            ccs_pulse(300);
        }
    }
}

/// DRIVE FIRST.
///
/// Measured 2026-09-29 (selector `usb2-live-pollbracket`; see the bracket note
/// above for the stage table): with `--start-after-connect` the handoff never
/// returns to its caller, so `usb_probe.rs` never reaches `run_ep0_signal_probe()`
/// and the polling owner never runs. Nothing consumes the EP0 SETUP buffer,
/// nothing arms EP0 after the handoff, and the host's first GET_DESCRIPTOR is
/// never answered - the measured `-110` boundary. The host-visible cross-check
/// agrees: a `!gadget_ready` return would start a 15-180 s polling park, and every
/// recent run instead ends with `boot-reason=watchdog` and a 62-63 s capture.
///
/// So the consumer has to start *inside* the handoff, at the moment the pull-up
/// goes up, instead of after the call returns. That is what this function is: the
/// probe's drive-loop body, moved into the module both crate roots share so the
/// handoff itself can drive it. Nothing here is new machinery - it is the same
/// `wdt_pet()` / `poll()` / debug-window sample the probe's own window runs.
///
/// Why the window is longer than the probe's 10 s default: the host's attach
/// follows the pull-up by roughly 35-40 s on this unit, and its descriptor
/// request times out about 6 s later, so the window has to outlast ~45 s of host
/// behaviour. The probe's own window cannot cover that even when the handoff does
/// return, because it starts afterwards and stops early.
///
/// Deliberate cost: while this runs, everything the handoff does after the
/// pull-up (the readouts, the arm-window retry, the DEVTEN write, the milestones)
/// is delayed by the window. The diagnostic ordering is the price of having the
/// consumer live while the host is talking; a run that needs the readouts on time
/// should not use this path.
pub const DRIVE_FIRST_SECS: u64 = 50;

/// Carry a probe's outcome into the durable log, where the *next* boot can read it.
///
/// The probes' own fields cannot be read in the boot that runs them: their action is at 2.5 s and
/// a field published after it is outside the run window (measured twice as runs with no fields at
/// all), while a field published before it samples the "not attempted" zero. So the outcome is
/// recorded as codes and answered from the previous boot's segment by
/// `usb2-live-blackbox-prev-probe-retired` and `-prev-probe-trb`.
fn record_probe_outcome(retired: bool) {
    // Read the TRB itself, not `DATA_PHASE_TRB_AFTER`: that copy is written on one timeout path
    // only, so a run where no such timeout happened reports its initial value - "consumed" for a TRB
    // nobody has touched. Sampling here, at the moment the outcome is recorded, is what makes the
    // code mean what it says.
    let hwo = unsafe { super::ep0::data_trb_still_owned() };
    crate::blackbox::record(
        if retired {
            crate::blackbox::CODE_PROBE_RETIRED
        } else {
            crate::blackbox::CODE_PROBE_NOT_RETIRED
        },
        0,
        0,
        0,
    );
    crate::blackbox::record(
        if hwo {
            crate::blackbox::CODE_PROBE_TRB_STILL_OWNED
        } else {
            crate::blackbox::CODE_PROBE_TRB_CONSUMED
        },
        0,
        0,
        0,
    );
}

pub fn drive_polling_owner(seconds: u64) {
    // SETUP-FIRST DRIVE, variant B: arm once per *consumed* SETUP.
    //
    // What the code says this loop has to do (ep0.rs:896-948, 961):
    //
    //   * `poll_setup_buffer()` is the documented eventless SETUP reader - "the
    //     event path is the normal trigger, but on this handoff `GEVNTCOUNT0`
    //     stays 0 even though the host's traffic reaches the controller, so
    //     polling the buffer is the only way left to answer the host's
    //     GET_DESCRIPTOR". It returns true when it consumed a fresh SETUP, and
    //     on that path it runs the gadget state machine through
    //     `process_event()`, i.e. without the event ring.
    //   * the next SETUP can only be received once EP0 OUT is armed again, so the
    //     arm belongs to a consumed SETUP - not to every pass (run 364476, which
    //     armed every pass, regressed to a single entry pulse and left the host
    //     at the descriptor stage).
    //
    // Marks: two pulses on entry, one per consumed SETUP (capped at 4), one per
    // 200k idle passes (capped at 2), two on exit. The per-SETUP marks are the
    // point of this variant: they say how many of the host's requests this loop
    // actually consumed.
    // The window and the instrument follow the *compile-time* readout word,
    // because this function runs before every readout selector (it is called
    // immediately after the pull-up is published). A long window starves them:
    // the board's own fail-safe resets it ~17 s after the pull-up, so a 50 s
    // drive means no readout ever executes.
    //
    //   no word                       consumer configuration - drive the window
    //   `usb2-live-pollbracket`       drive the window (the bracket measures it)
    //   `usb2-live-soffn-after-drive` short window, then ask whether SOF frames
    //                                 arrive at all
    //   any other word                return immediately - it is a readout, and
    //                                 its own marks are the whole reading
    let word = option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT");
    let soffn_probe = matches!(word, Some("usb2-live-soffn-after-drive"));
    let arm_late = matches!(
        word,
        Some("usb2-live-arm-late") | Some("usb2-live-arm-steps") | Some("usb2-live-consume-steps")
    );
    // `usb2-live-consume-late` separates the *action* from the *marks*: the arm
    // has to happen immediately (a delayed arm loses the host's first request -
    // measured 2026-09-29: an arm at 600 ms consumed nothing in an 8 s window,
    // while arm-at-entry builds consumed it and answered DataIn), but a mark at
    // the pull-up can be absorbed by the host's own first observation of the
    // connect. So this word arms at entry and reports two seconds later.
    // `usb2-live-consume-late` arms immediately - that is the whole point of the
    // word: an arm at 600 ms loses the host's first SETUP entirely (measured).
    // Its reading is published by the two `consume_step_mark`s inside
    // `poll_setup_buffer()`, so this loop adds no reports of its own: one mark
    // costs ~2.2 s here (measured with `usb2-live-clock-check`), and the board's
    // usable window is only a few seconds.
    let consume_late = matches!(word, Some("usb2-live-consume-late"));
    // The probe outcomes are answered by the *next* boot, out of the durable log: a probe's own
    // fields cannot be read in the boot that runs it (see the note in live_digit_words.rs).
    let prev_probe_retired = matches!(word, Some("usb2-live-blackbox-prev-probe-retired"));
    let prev_probe_trb = matches!(word, Some("usb2-live-blackbox-prev-probe-trb"));
    // `usb2-live-clock-check`: nothing but two delayed pulses, to calibrate the
    // time base and confirm that a delayed mark is audible at all. Both have been
    // inferred rather than measured, and every other reading depends on them.
    let clock_check = matches!(word, Some("usb2-live-clock-check"));
    // `usb2-live-arm-land`: arm at entry (what actually lets the host's request
    // land), then report the two facts from *after* the connect has been seen, so
    // the marks are audible. Widths separate them from the arm's own short
    // toggle: that one is ~0.27 s low, every mark here is ~2.2 s low.
    //   ~1.5 s  1 pulse = the arm returned true, 2 = it refused
    //   ~3.0 s  1 pulse = a SETUP was consumed in the window, 2 = none was
    // `usb2-live-setup-inpage` is included here because `ep0_setup_data_ptr()`
    // already special-cases that word: it returns `ep0_trb_ptr(1)` instead of the
    // adopted page. Wiring it into the drive tests "is the core writing the SETUP
    // to the address this code polls?" with the arm in place, which is the
    // reading the last two runs point at: nothing lands in the buffer even when
    // EP0 was armed before the host's first request.
    let arm_land = matches!(
        word,
        Some("usb2-live-arm-land") | Some("usb2-live-setup-inpage")
    );
    // `usb2-live-setup-trb-state`: arm at entry, then ONE bit at ~3 s - did the
    // core retire the SETUP TRB? See `ep0::setup_trb_retired` for why that
    // separates "the request never arrived" from "it arrived, but the bytes went
    // somewhere this code does not poll". One mark, because that is the budget.
    let setup_trb_state = matches!(word, Some("usb2-live-setup-trb-state"));
    // `usb2-live-handler-steps`: arm at entry (so the host's first request can
    // land) and let the two marks inside `handle_setup()` speak. No marks from
    // this loop at all.
    let handler_steps = matches!(word, Some("usb2-live-handler-steps"));
    // `usb2-live-arm-settled`: question 1 from the ledger after the arming runs.
    // The arm goes in at 1.8 s (an immediate arm kills the loop; 600 ms completed
    // its steps but consumed nothing), the window runs to 8 s so the host's own
    // descriptor retries - ~0.3 s and ~5.4 s after the pull-up - are both inside
    // it, and a single bucket at the end says whether anything landed. The loop
    // pets the watchdog every pass, so it can outlive the 5.9 s that a stopped
    // loop dies at.
    let arm_settled = matches!(word, Some("usb2-live-arm-settled"));
    // `usb2-live-arm-result`: the arm goes in at 1.8 s and its result is published
    // *immediately*, not seconds later - run 604159 shows the loop stops right
    // there (its drop lands at 1.8 + 5.9 s, i.e. the watchdog after the last pet,
    // so the arm is where the code died). One pulse means the arm returned true,
    // two mean it refused, and nothing at all means it never returned.
    let arm_result = matches!(word, Some("usb2-live-arm-result"));
    // `usb2-live-setup-reached`: the arm that is known to *return* is the one at
    // 600 ms (run 537442), so that is the timing to ask the decisive question at.
    // One bit at ~2 s: did the controller retire the armed SETUP TRB? Retired
    // means the host's request reached the endpoint and the bytes went to whatever
    // buffer the controller was told to use - the pointer question. Still owned
    // means the request never got to the endpoint at all, and the fault is in the
    // link or in EP0's own configuration.
    let setup_reached = matches!(word, Some("usb2-live-setup-reached"));
    // `usb2-live-arm-stage`: arm at the timing known to *return* (600 ms) and
    // publish `arm_failure_stage() + 1` as pulses, so one run says why the arm
    // refused: 1 = it succeeded, 2 = the DSTS halt gate, 3 = the command came back
    // with a status error, 4 = the command never retired.
    let arm_stage = matches!(word, Some("usb2-live-arm-stage"));
    // `usb2-live-rearm-stage`: why the most recent setup re-arm stopped, published from the drive's
    // own window. A *new* word name rather than a share of `usb2-live-arm-stage`, which already
    // exists with its own arm timing and window: sharing the name silently used its behaviour and
    // produced no readout at all.
    let rearm_stage = matches!(word, Some("usb2-live-rearm-stage"));
    // `usb2-live-arm-rounds`: how many times EP0 was armed, i.e. how many SETUPs the controller has
    // delivered. Independent of the setup buffer's contents, which cannot distinguish delivery because
    // the host repeats the same request every attempt.
    let arm_rounds = matches!(word, Some("usb2-live-arm-rounds"));
    // `usb2-live-susphy`: the USB2 PHY's suspend bits as they stand between commands. The driver's own
    // comment names a parked receive path as a candidate for a device that answers once and stops.
    let susphy = matches!(word, Some("usb2-live-susphy"));
    // `usb2-live-susphy-test`: clear GUSB2PHYCFG.SUSPHY, then report the delivery count. Settles the
    // polarity disagreement between the databook and this driver's comment by measurement.
    let susphy_test = matches!(word, Some("usb2-live-susphy-test"));
    // `usb2-live-precheck`: never arms, so nothing can wedge, and reads out the
    // gates `try_arm_setup()` consults before it will issue a command. Five fields
    // in one run because each digit is published as *short* marks (~0.27 s):
    //
    //   field 1  EP0_STATE + 1            1 Setup, 2 Data, 3 Status
    //   field 2  ENDPOINTS_READY ? 1 : 2
    //   field 3  EP0_SETUP_ARMED ? 1 : 2
    //   field 4  ARM_COOLDOWN != 0 ? 2 : 1
    //   field 5  SETUP_ARM_FAILURE_STAGE + 1   1 success, 2 halt gate, 3 status
    //                                           error, 4 timeout, 5 not attempted
    //
    // Read a field by counting edges inside its window; the fields run back to back
    // at about 0.63 s per mark.
    let precheck = matches!(word, Some("usb2-live-precheck"));
    // `usb2-live-data-phase`: arm at entry (the configuration whose host-side
    // shapes are all prompt `-71`, i.e. the device answers every request) and then
    // ask where the answer dies. Two fields, both as *short* marks (~0.6 s each),
    // so the whole readout fits well inside the board's window:
    //
    //   field 1  control_data_trb_state() + 1
    //            1 = EP0 never left Setup, so the response path never advanced
    //            2 = EP0 is in Data but the core still owns the TRB: the descriptor
    //                was never transmitted
    //            3 = the core retired the data TRB, so the descriptor did go out
    //   field 2  data_phase_start_rejected() ? 2 : 1
    let data_phase = matches!(word, Some("usb2-live-data-phase"));
    // `usb2-live-arm-recover`: the project already built the repair for a failed
    // deferred arm - `u0_arm_window_recovery()` does one device-core soft reset and
    // re-arms EP0 - but it lives inside the handoff's arm window, which the drive
    // runs before, so drive-first has never exercised it. This word tries the arm
    // at 600 ms and, if it fails, runs the recovery from the drive and reports both
    // outcomes as self-delimited digits:
    //
    //   field 1  stage + 1 after the arm attempt (1 success, 2 halt gate,
    //            3 status error, 4 timeout, 5 not attempted)
    //   field 2  EP0_SETUP_ARMED after the recovery ? 1 : 2
    let arm_recover = matches!(word, Some("usb2-live-arm-recover"));
    // `usb2-live-triage`: four decisive fields in one run, each a digit of *short*
    // marks followed by a *medium* separator, so the fields can be told apart by
    // width instead of guessed from a flat run of evenly spaced marks. The arm goes
    // in at 600 ms, the fields follow:
    //
    //   field 1  arm result + 1   1 success, 2 halt gate, 3 status error,
    //                             4 timeout, 5 not attempted
    //   field 2  control_data_trb_state() + 1
    //                             1 EP0 never left Setup, 2 in Data with the core
    //                             still owning the TRB, 3 the core retired it
    //   field 3  data_phase_start_rejected() ? 2 : 1
    //   field 4  EP0_STATE + 1 (1 Setup, 2 Data, 3 Status)
    //
    // This is the cheapest way to read several of tonight's open questions at once:
    // one run instead of four, at ~0.6 s per mark. The event-count register is
    // deliberately not read here - the ledger records that touching GEVNTCOUNT0 is
    // what wedges the loop.
    let triage = matches!(word, Some("usb2-live-triage"));
    // `usb2-live-observe`: the same four fields as `triage`, but the drive arms
    // nothing at all. A drive-side arm wedges the loop often enough that triage came
    // back with zero pulses; precheck, which never arms, published on both of its
    // runs. And the precheck reading says EP0 is already armed successfully before
    // the drive runs, so the arming is happening elsewhere - the drive only has to
    // watch. Fields, each a digit of short marks then a medium separator:
    //
    //   1  EP0_SETUP_ARMED ? 1 : 2
    //   2  control_data_trb_state() + 1
    //   3  data_phase_start_rejected() ? 2 : 1
    //   4  EP0_STATE + 1 (1 Setup, 2 Data, 3 Status)
    let observe = matches!(word, Some("usb2-live-observe"));
    // `usb2-live-response-bytes`: six categorical fields, no arm, six digits of
    // fast blips. The response content is the last part of the chain that has never
    // been looked at: everything else in the path was checked against v4.19.278, and
    // the descriptor table itself is right, so if the bytes in the buffer are right
    // too then the fault is purely that the controller never puts them on the wire.
    //
    //   1  response[0]  == 18   (bLength)
    //   2  response[1]  == 1    (bDescriptorType = DEVICE)
    //   3  response[8..10] == 0x34,0x12      (idVendor 0x1234, little endian)
    //   4  response[10..12] == 0x01,0x00     (idProduct 0x0001)
    //   5  DATA_PHASE_PENDING_LEN != 0       (a data phase was armed at all)
    //   6  any of the twelve bytes non-zero  (the buffer holds something)
    // Each field is 1 for true, 2 for false.
    //
    // CAVEAT worth keeping: `runstop_blips_fast` returns without emitting anything
    // unless the link reports U0, so silence here is not a "no" - it is "the
    // transport declined to speak", and must not be read as a zero.
    let response_bytes_word = matches!(word, Some("usb2-live-response-bytes"));
    // `usb2-live-setup-buffer`: five categorical fields on the SETUP buffer itself,
    // fast blips, no arm. This is the direct answer to "did the host's packet ever
    // reach the buffer software polls", which the observe reading points at:
    //
    //   1  setup[0] != 0            (a bmRequestType arrived at all)
    //   2  setup[0] == 0x80         (device-to-host standard request)
    //   3  setup[1] == 6            (GET_DESCRIPTOR)
    //   4  setup[3] == 1            (descriptor type 1 in wValue's high byte)
    //   5  all eight bytes zero     (1 = yes, 2 = no)
    // Same caveat as the other words: the fast blip transport declines to speak
    // outside U0, so silence is "no transport", not a false.
    let setup_buffer_word = matches!(word, Some("usb2-live-setup-buffer"));
    // `usb2-live-fresh-gates`: decompose the reason `poll_setup_buffer()` keeps
    // returning "not fresh". Seven fields, one gate each, fast blips, no arm:
    //
    //   1  setup_packet_pending()          (the verdict itself)
    //   2  EP0_STATE == Setup
    //   3  EP0_SETUP_ARMED
    //   4  ctrl & 1 == 0                    (HWO cleared: the controller retired it)
    //   5  (ctrl >> 4) & 0x3f == 2          (TRBCTL == CONTROL_SETUP)
    //   6  size & 0x00ff_ffff == 0          (all eight bytes consumed)
    //   7  size >> 28 == 0                  (TRBSTS == OK)
    // 1 = true, 2 = false. Fields 4-7 are exactly the four conditions inside
    // `dwc3_setup_trb_complete()`, so one run names the failing gate.
    let fresh_gates = matches!(word, Some("usb2-live-fresh-gates"));
    // `usb2-live-data-report`: the response path's own latches. `fresh-gates` said the
    // freshness verdict is true and every gate open, so the packet is arriving and the
    // consumer should be building a response; these are the variables that code
    // maintains while doing it, and they have never been read from outside.
    //
    //   1  DATA_PHASE_PENDING_LEN != 0     (a response was prepared at all)
    //   2  DATA_PHASE_PENDING_START        (the data-phase STARTTRANSFER was refused)
    //   3  EP0_STATE + 1                   (1 Setup, 2 Data, 3 Status)
    //   4  arm command status nibble == 0  (the last STARTTRANSFER completed cleanly)
    //   5  arm command == 0xffff_ffff      (no command outcome was ever recorded)
    //   6  control_data_trb_state() + 1   (1 not Data, 2 core owns, 3 core retired)
    let data_report = matches!(word, Some("usb2-live-data-report"));
    // `usb2-live-data-cmd`: why the *data* phase's STARTTRANSFER was refused.
    // `data-report` showed the response was prepared (`DATA_PHASE_PENDING_LEN`) and
    // its start was refused (`DATA_PHASE_PENDING_START`), but the EP0-only
    // `SETUP_ARM_LAST_COMMAND` latch cannot hold that reason - it only records
    // endpoint 0 commands - so `DATA_PHASE_LAST_COMMAND` now records endpoint 1's.
    //
    //   1  timed out ? 1 : 2    (bit 31: the command never retired) - this is the *first*
    //   2  min(status, 5) + 1   1 = clean status, 2..5 = status 1..4, 6 = status >= 5
    //
    // These two were written the other way round here while the code publishes the timeout bit
    // first, so a reader following this spec decoded every run inverted.
    //   3  never recorded ? 1 : 2
    //   4  DATA_PHASE_PENDING_START ? 1 : 2
    //   5  DATA_PHASE_PENDING_LEN != 0 ? 1 : 2
    // `usb2-live-ack-event` shares this word's fields on purpose: the only difference
    // between the two runs is whether the response path acknowledges the EP0 event by
    // writing GEVNTCOUNT0 before issuing the data phase's command. Same readout, so
    // the comparison is exactly the effect of the acknowledgement.
    let data_cmd = matches!(
        word,
        Some("usb2-live-data-cmd")
            | Some("usb2-live-ack-event")
            | Some("usb2-live-single-try")
            | Some("usb2-live-reset-first")
    );
    // `usb2-live-reset-first`: give the controller a clean slate *before* anything
    // arms EP0, rather than resetting after a failure the way the project's arm-window
    // recovery does - that version wedged the drive when called late (one run, zero
    // pulses). The remaining difference between the setup phase, whose command the
    // controller executes, and the data phase, whose command it never touches, may be
    // leftover state from the bootloader's own controller use; this word clears it
    // first and then lets the normal flow run.
    let reset_first = matches!(word, Some("usb2-live-reset-first"));
    let ack_event = matches!(word, Some("usb2-live-ack-event"));
    let single_try = matches!(word, Some("usb2-live-single-try"));
    // `usb2-live-trb-after`: what the controller's own TRB says after it refuses to
    // retire the data phase's STARTTRANSFER. Five fields:
    //
    //   1  TRBSTS (size bits 31:28) as min(code,5)+1   1 = none/OK, 2..6 = codes 1..5
    //   2  HWO still set                (the controller never consumed the TRB)
    //   3  TRBCTL == CONTROL_DATA (5)   (the type it fetched is the one intended)
    //   4  size low 24 bits == 0        (nothing was transferred)
    //   5  IOC set in ctrl              (the interrupting-on-complete flag survived)
    // `usb2-live-trb-after-ep0`: same readout with the endpoint routing on, so the two
    // runs differ only in which physical endpoint receives the data phase's command.
    // If the TRB's HWO clears here and not in `usb2-live-trb-after`, the unrouted
    // command was being written to an endpoint block that does not exist.
    let trb_after = matches!(
        word,
        Some("usb2-live-trb-after")
            | Some("usb2-live-trb-after-ep0")
            | Some("usb2-live-ring-reset")
    );
    let _trb_after_ep0 = matches!(word, Some("usb2-live-trb-after-ep0"));
    // `usb2-live-depcmd`: read the controller's own endpoint-command block for
    // the data-phase endpoint (and, as a control, the setup endpoint). The
    // `data-cmd` latch says the EP1 STARTTRANSFER never retired; these
    // registers say whether the controller still holds it active (CMDACT), or
    // retired it late and with which status / transfer resource.
    //
    //   1  DEPCMD(1) CMDACT set ? 1 : 2    (bit 10: still active)
    //   2  DEPCMD(1) CMDSTATUS min(5)+1    (bits 15:12 of a late retirement)
    //   3  DEPCMD(1) xfer-resource != 0    (bits 22:16, nonzero ? 1 : 2)
    //   4  DEPCMD(0) CMDACT set ? 1 : 2    (sanity: expected clear, arm done)
    let depcmd = matches!(word, Some("usb2-live-depcmd"));
    // `usb2-live-ep0-physical`: the drive only has to keep polling; the experiment
    // lives in `send_ep_command_result`'s endpoint mapping. No arm, no marks.
    let ep0_physical = matches!(word, Some("usb2-live-ep0-physical"));
    // `usb2-live-fail-state`: the controller state latched at the instant the data
    // phase attempted its command, not afterwards. Five categorical fields:
    //
    //   1  DCTL.RUN_STOP == 0              (the controller is running)
    //   2  DSTS link state == U0           ((dsts >> 18) & 0xf == 0)
    //   3  EP0_SETUP_ARMED                 (the setup TRB was still armed)
    //   4  EP0 resource index != 0         (a transfer resource exists)
    //   5  no device halt                  (DSTS DEVCTRLHLT clear)
    // 1 = true, 2 = false. No arm from the drive.
    let fail_state = matches!(word, Some("usb2-live-fail-state"));
    // `usb2-live-global-block`: is the controller's *global* register block alive?
    //
    // The event ring lives in the global block (0xc100..0xc6ff) while the device
    // block (DCTL, DSTS, DEPCMD) is at 0xc700 and above, and every register this
    // project reads routinely is in the device block. `fail-state` shows the device
    // block answering perfectly at the instant the data phase fails: controller
    // running, link U0, setup armed. If the global block is gated, the event ring can
    // never be read *or acknowledged* - and an unacknowledged EP0 completion is
    // exactly what would keep the next STARTTRANSFER from ever retiring.
    //
    //   1  GCTL.DSBLCLKGTNG  (bit 0: clock gating disabled, clocks always on)
    //   2  GCTL not in core soft reset (bit 11 clear)
    //   3  GEVNTSIZ0 != 0       (the global block answers, and the ring is programmed)
    //   4  GEVNTADRLO0 != 0     (the ring address is programmed)
    //   5  GEVNTCOUNT0 reads and is zero
    // Field 5 is last on purpose: that read is the one the ledger records as never
    // returning, so if the run dies with four pulses the first four fields still
    // carry the answer and the missing fifth is itself the evidence.
    let global_block = matches!(word, Some("usb2-live-global-block"));
    // `usb2-live-event-count`: which of the two event buffers' count registers answers?
    //
    // `global-block` showed the global block itself is alive - clock gating disabled,
    // GEVNTSIZ0 and GEVNTADRLO0 read normally - while `read(GEVNTCOUNT0)` never
    // returned. Reading the *other* buffer's count separates two explanations that
    // look identical so far:
    //
    //   GEVNTCOUNT1 answers  -> the block and the register are fine, and buffer 0's
    //                           count specifically is what wedges
    //   GEVNTCOUNT1 wedges    -> any event count read wedges, so it is the register
    //                           access itself, not buffer 0's state
    //
    // The wedging read is last so the earlier fields survive it.
    let event_count = matches!(word, Some("usb2-live-event-count"));
    // `usb2-live-ring-reset`: re-programme the event ring from the drive, without
    // ever reading its count.
    //
    // CORRECTION (2026-09-30, later the same day): the text that used to sit here
    // claimed `event-count` had shown GEVNTCOUNT1 wedging the loop just like
    // GEVNTCOUNT0, and concluded that the register access itself never returns.
    // That reading is withdrawn. The word's declaration and its guards went in
    // while its *readout* did not, so it published no fields at all, and a run
    // with no fields was misread as a negative result. The fields are now
    // implemented in `live_digit_words::event_count_digit()`; until a run
    // actually contains them, nothing is known about GEVNTCOUNT1. What was
    // genuinely observed is the second half: GEVNTSIZ0 and GEVNTADRLO0 do read
    // normally under the same conditions.
    //
    // If the controller will not accept the data phase's STARTTRANSFER because an
    // unacknowledgeable completion is outstanding, then re-programming the ring may
    // clear it - the databook describes writing GEVNTSIZ as re-initialising the ring,
    // and Linux writes ADRLO/ADRHI/SIZ in `dwc3_event_buffers_setup()` for the same
    // purpose at init. Reading the current values first is safe (those registers
    // answer); only the count reads stall, and this word never performs one.
    //
    // The readout is `trb_after`'s, so the verdict is the same field that names the
    // failure without the reset: HWO still set on the data TRB means the controller
    // still never touched it.
    let ring_reset = matches!(word, Some("usb2-live-ring-reset"));
    // `usb2-live-blackbox`: did this boot claim the reserved-RAM region, and does
    // the ring hold records from a *previous* boot?
    //
    // Field 2 is the load-bearing one: `boots() >= 2` can only be true if the
    // header written by an earlier boot was still readable in RAM after a reset,
    // which is the whole premise of keeping a black box in reserved RAM. Measure
    // that before instrumenting anything into it. The field layout lives in
    // `live_digit_words::blackbox_digit()`.
    let blackbox_word = matches!(word, Some("usb2-live-blackbox"));
    // `usb2-live-blackbox-read`: the same region, read back, asking whether a
    // *different* boot wrote any of the records that are still there. Field 2 is
    // the answer; field 1 alone only says the ring is non-empty.
    let blackbox_read = matches!(word, Some("usb2-live-blackbox-read"));
    // `usb2-live-pulse-cal`: measure which Run/Stop pulse *widths* this board and
    // cable actually deliver to the host.
    //
    // The count-based transport is worth ~2.5 bits per field; carrying a 32-byte
    // record over it would take about twenty-six runs, so the width has to carry
    // symbols instead. How many classes survive is an empirical question - the
    // host sees 0.062 s for a 100 ms disconnect, so there is a fixed latency on
    // top of whatever we program and the classes are not evenly spaced at the
    // far end. This word emits a known ladder of widths once, in order, and the
    // capture says which ones stay separable.
    let pulse_cal = matches!(word, Some("usb2-live-pulse-cal"));
    // `usb2-live-blackbox-stage`: the last few stage codes, newest first. The
    // question a black box has to answer is "where did it stop", and that is a
    // handful of small numbers rather than a full dump.
    let blackbox_stage = matches!(word, Some("usb2-live-blackbox-stage"));
    // `usb2-live-blackbox-times`: *when* four stages happened, in ms since the pull-up. The same
    // clock the host's own timeline starts on, so a device record and a host capture can be read
    // against each other instead of guessed at.
    let blackbox_times = matches!(word, Some("usb2-live-blackbox-times"));
    // `usb2-live-setup-bytes`: what the first setup packet of this boot actually contained, as two
    // digits - the request class and the direction. The wire says the host gets no answer at all, so
    // the first thing to rule out is that the eight bytes we answer from are not the ones it sent.
    let setup_bytes = matches!(word, Some("usb2-live-setup-bytes"));
    // `usb2-live-dtrb-addr`: whether the data stage's buffer address is usable as a 32-bit physical
    // address. The command is accepted and nothing reaches the wire; this is the one part of that
    // path the counters cannot see.
    let dtrb_addr = matches!(word, Some("usb2-live-dtrb-addr"));
    // `usb2-live-dtrb-size`: the production data TRB's length field. The host's completion says it
    // transferred zero bytes while the reply built is 18; this says whether the TRB agrees with the
    // reply or with the wire.
    let dtrb_size = matches!(word, Some("usb2-live-dtrb-size"));
    // `usb2-live-ep0-context`: the EP0 packet size actually programmed, and the speed the code read.
    // Linux starts at 512 (SuperSpeed) and modifies after Connect Done on a USB2 link; a 512-byte
    // EP0 on a high-speed link turns the host's data transaction into a babble - a protocol error
    // with no valid data, which is what the wire shows.
    let ep0_context = matches!(word, Some("usb2-live-ep0-context"));
    // `usb2-live-dtrb-ctrl`: the data TRB's type nibble and HWO, read from the TRB itself.
    let dtrb_ctrl = matches!(word, Some("usb2-live-dtrb-ctrl"));
    // `usb2-live-setup-count`: how many of the host's attempts the handler actually saw. One means
    // the controller stopped delivering setups after the first, which is what a stuck EP0 state
    // machine looks like from this side.
    let setup_count = matches!(word, Some("usb2-live-setup-count"));
    // `usb2-live-gvnt0`: what reading GEVNTCOUNT0 back after the EHB write returned. "The write landed"
    // is not something to assume on a register whose read was reported not to return.
    let gvnt0 = matches!(word, Some("usb2-live-gvnt0"));
    // `usb2-live-setup-late`: is the setup buffer still holding the first request, or has it changed?
    // The handler saw one setup across three runs; this says whether that is the controller's doing or
    // the poll's blindness.
    let setup_late = matches!(word, Some("usb2-live-setup-late"));
    // `usb2-live-events-acked`: how many event bytes the drive acknowledged. Zero means the
    // acknowledgement never ran or found nothing; four or more means it found events the controller
    // had posted and had never been told were consumed.
    let events_acked = matches!(word, Some("usb2-live-events-acked"));
    // `usb2-live-blackbox-recent`: what *this* boot has written so far. Kept
    // separate from the previous-boot question so the two can be told apart while
    // the ring is being trusted.
    let blackbox_recent = matches!(word, Some("usb2-live-blackbox-recent"));
    // `usb2-live-blackbox-state`: which parts of the black box's state are set in
    // the copy the drive is running. Diagnostics for the reader, not for the boot.
    let blackbox_state = matches!(word, Some("usb2-live-blackbox-state"));
    // `usb2-live-blackbox-count`: how many records the ring holds, in thresholds.
    let blackbox_count = matches!(word, Some("usb2-live-blackbox-count"));
    // `usb2-live-blackbox-probe`: which internal value of the reader is wrong.
    let blackbox_probe = matches!(word, Some("usb2-live-blackbox-probe"));
    // One code per run, so the field cannot shift under an absorbed mark:
    // `blackbox-code0` is the previous boot's newest record, `-code1` the one
    // before it, `-code2` the one before that.
    let blackbox_code0 = matches!(word, Some("usb2-live-blackbox-code0"));
    let blackbox_code1 = matches!(word, Some("usb2-live-blackbox-code1"));
    let blackbox_code2 = matches!(word, Some("usb2-live-blackbox-code2"));
    // `usb2-live-blackbox-boots`: how many boots this ring has seen. Field 2 onward
    // is the persistence test that the record-count thresholds could not give.
    let blackbox_boots = matches!(word, Some("usb2-live-blackbox-boots"));
    // `usb2-live-blackbox-bootsn`: the boot counter as one digit, so consecutive
    // runs show whether it advanced.
    let blackbox_bootsn = matches!(word, Some("usb2-live-blackbox-bootsn"));
    // `usb2-live-blackbox-why`: why the previous-boot read returns nothing, as one
    // of four distinguishable states instead of a single silent None.
    let blackbox_why = matches!(word, Some("usb2-live-blackbox-why"));
    // `usb2-live-status-try`: issue a control *status* stage on EP0 and see whether the
    // command retires, without a data stage in front of it. The data stage's
    // STARTTRANSFER never retires, and that fact alone cannot say whether the controller
    // refuses any command after the setup or only that one.
    let status_try = matches!(word, Some("usb2-live-status-try"));
    // `usb2-live-data-zero`: a control data stage with length zero, to separate "the
    // length or buffer is the problem" from "CONTROL_DATA is refused outright".
    let data_zero = matches!(word, Some("usb2-live-data-zero"));
    // `usb2-live-data-alt`: the same 18-byte data stage, but pointed at a TRB slot in the
    // driver's own DMA arena rather than at the response buffer. Which of the two is the
    // fault - the length or the buffer address - is what this separates.
    let data_alt = matches!(word, Some("usb2-live-data-alt"));
    // `usb2-live-data-late`: re-issue the production data stage at 2.5 s. The three probes
    // that retired were all issued at that moment, so timing is the remaining variable.
    let data_late = matches!(word, Some("usb2-live-data-late"));
    // `usb2-live-data-persist`: re-issue the data stage until it retires. The fix candidate
    // for the data-stage stall, since re-issuing the same TRB later retires and issuing it
    // once, early, does not.
    let data_persist = matches!(word, Some("usb2-live-data-persist"));
    // `usb2-live-data-gap{500,1000,1500}`: issue the production data TRB exactly once, that many ms
    // after the drive began, with no retry window and no revocation. `data-late` fires at 2.5 s and
    // *retries*, and a failed attempt revokes the outstanding transfer - which clears the TRB's own
    // HWO. So this separates "the controller takes it when asked later" from "our revocation cleared
    // the flag"; three points bracket where the transition sits.
    let data_gap_ms: u64 = match word {
        Some("usb2-live-data-gap500") => 500,
        Some("usb2-live-data-gap1000") => 1000,
        Some("usb2-live-data-gap1500") => 1500,
        _ => 0,
    };
    let data_gap = data_gap_ms != 0;
    // `usb2-live-data-rewrite`: at 1 s, write the production data TRB's own bytes back to it and
    // issue nothing. `data-late` re-issues the *command*; this one only makes the controller look
    // again, so the two together say which of the two stories is true.
    let data_rewrite = matches!(word, Some("usb2-live-data-rewrite"));
    // `usb2-live-ioava-check`: is the DMA address the data stage is handed pointing at the
    // descriptor? The controller clears the TRB's HWO after the late re-issue, so it
    // fetched *something*, and the host's -71 says what it sent was malformed.
    let ioava_check = matches!(word, Some("usb2-live-ioava-check"));
    // `usb2-live-data-status`: late data stage, then a status stage. The host answers EPROTO
    // rather than a timeout, and a control transfer is incomplete without its status stage.
    let data_status = matches!(word, Some("usb2-live-data-status"));
    // `usb2-live-blackbox-pstore`: were the Android-readable mirror targets armed? The
    // mirror itself is written once per boot from the boot path, so this word drives
    // nothing and only reports.
    let pstore_ready = matches!(word, Some("usb2-live-blackbox-pstore"));
    // `usb2-live-blackbox-pmsg-read` reports the ring's zone: is the Linux header valid, is
    // our magic there, how many boots has this ring seen, how many records does it hold.
    let pmsg_read = matches!(word, Some("usb2-live-blackbox-pmsg-read"));
    // `usb2-live-blackbox-pmsg-age`: one field, the age of the pmsg record.
    let pmsg_age = matches!(word, Some("usb2-live-blackbox-pmsg-age"));
    // The durable log: `-text` reports its shape, `-prev-*` ask single yes/no questions about
    // the segment the *previous* boot left in it.
    let text_log = matches!(word, Some("usb2-live-blackbox-text"));
    let prev_record = matches!(word, Some("usb2-live-blackbox-prev-record"));
    let prev_boot = matches!(word, Some("usb2-live-blackbox-prev-boot"));
    let prev_region = matches!(word, Some("usb2-live-blackbox-prev-region"));
    let prev_stage = matches!(word, Some("usb2-live-blackbox-prev-stage"));
    // The probe outcomes, answered by the *next* boot: 1 = the last probe's command retired,
    // 2 = it did not, 3 = the previous boot ran no probe. The second word asks the same of the
    // TRB the controller was handed: 1 = it consumed it (HWO cleared), 2 = it still owns it.
    let prev_probe_retired = matches!(word, Some("usb2-live-blackbox-prev-probe-retired"));
    let prev_probe_trb = matches!(word, Some("usb2-live-blackbox-prev-probe-trb"));

    // `usb2-live-depcmd-state`: was the controller still busy with the *setup* phase's
    // command when the data phase issued its own? Five fields, from the snapshot:
    //
    //   1  EP0 DEPCMD CMDACT clear   (the setup command has retired)
    //   2  EP0 DEPCMD status nibble == 0
    //   3  EP1 DEPCMD CMDACT clear
    //   4  EP0 DEPCMD != 0xffff_ffff (the register answers at all)
    //   5  EP0 DEPCMD's command field == STARTTRANSFER (0x07)
    let depcmd_state = matches!(word, Some("usb2-live-depcmd-state"));
    let arm_buckets = matches!(word, Some("usb2-live-arm-late"));
    let window_secs = match word {
        None | Some("usb2-live-pollbracket") => seconds,
        Some("usb2-live-soffn-after-drive") => SOFFN_WINDOW_SECS,
        Some("usb2-live-arm-late")
        | Some("usb2-live-arm-steps")
        | Some("usb2-live-consume-steps") => LATE_WINDOW_SECS,
        Some("usb2-live-consume-late") => CONSUME_LATE_WINDOW_SECS,
        Some("usb2-live-clock-check") => CLOCK_CHECK_WINDOW_SECS,
        Some("usb2-live-arm-land") => ARM_LAND_WINDOW_SECS,
        // Read #9 completes the wiring the note above describes: without this
        // arm the word met `Some(_) => return` here and never reached the drive
        // at all (the `ep0_setup_data_ptr()` special case is only half the path).
        Some("usb2-live-setup-inpage") => ARM_LAND_WINDOW_SECS,
        Some("usb2-live-setup-trb-state") => SETUP_TRB_WINDOW_SECS,
        Some("usb2-live-handler-steps") => HANDLER_WINDOW_SECS,
        Some("usb2-live-arm-settled") => ARM_SETTLED_WINDOW_SECS,
        Some("usb2-live-arm-result") => ARM_RESULT_WINDOW_SECS,
        Some("usb2-live-setup-reached") => SETUP_REACHED_WINDOW_SECS,
        Some("usb2-live-arm-stage") => ARM_STAGE_WINDOW_SECS,
        Some("usb2-live-precheck") => PRECHECK_WINDOW_SECS,
        Some("usb2-live-data-phase") => DATA_PHASE_WINDOW_SECS,
        Some("usb2-live-arm-recover") => ARM_RECOVER_WINDOW_SECS,
        Some("usb2-live-triage") => TRIAGE_WINDOW_SECS,
        Some("usb2-live-observe") => OBSERVE_WINDOW_SECS,
        Some("usb2-live-response-bytes") => RESPONSE_BYTES_WINDOW_SECS,
        Some("usb2-live-setup-buffer") => SETUP_BUFFER_WINDOW_SECS,
        Some("usb2-live-fresh-gates") => FRESH_GATES_WINDOW_SECS,
        Some("usb2-live-data-report") => DATA_REPORT_WINDOW_SECS,
        Some("usb2-live-data-cmd") => DATA_CMD_WINDOW_SECS,
        Some("usb2-live-ack-event") => DATA_CMD_WINDOW_SECS,
        Some("usb2-live-single-try") => DATA_CMD_WINDOW_SECS,
        Some("usb2-live-trb-after") => DATA_CMD_WINDOW_SECS,
        Some("usb2-live-trb-after-ep0") => DATA_CMD_WINDOW_SECS,
        Some("usb2-live-reset-first") => DATA_CMD_WINDOW_SECS,
        Some("usb2-live-depcmd") => DEPCMD_WINDOW_SECS,
        Some("usb2-live-ep0-physical") => EP0_PHYSICAL_WINDOW_SECS,
        Some("usb2-live-fail-state") => FAIL_STATE_WINDOW_SECS,
        Some("usb2-live-global-block") => GLOBAL_BLOCK_WINDOW_SECS,
        Some("usb2-live-event-count") => EVENT_COUNT_WINDOW_SECS,
        Some("usb2-live-ring-reset") => DATA_CMD_WINDOW_SECS,
        Some("usb2-live-blackbox") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-read") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-pulse-cal") => PULSE_CAL_WINDOW_SECS,
        Some("usb2-live-blackbox-stage") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-times") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-setup-bytes") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-dtrb-addr") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-dtrb-size") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-ep0-context") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-dtrb-ctrl") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-setup-count") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-gvnt0") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-setup-late") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-rearm-stage") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-arm-rounds") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-susphy") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-susphy-test") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-events-acked") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-recent") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-state") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-count") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-probe") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-code0") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-code1") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-code2") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-boots") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-bootsn") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-why") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-status-try") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-zero") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-alt") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-late") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-persist") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-gap500") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-gap1000") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-gap1500") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-rewrite") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-ioava-check") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-data-status") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-pstore") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-pmsg-read") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-pmsg-age") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-text") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-prev-record") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-prev-boot") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-prev-region") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-prev-stage") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-prev-probe-retired") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-blackbox-prev-probe-trb") => BLACKBOX_WINDOW_SECS,
        Some("usb2-live-depcmd-state") => DEPCMD_STATE_WINDOW_SECS,
        Some(_) => return,
    };
    // `DWC3_DSTS_SOFFN_MASK` (`0x3fff << 3`, `core.h:487`), counted in frames.
    const SOFFN_MASK: u32 = 0x3fff << 3;
    // Long enough for the host to have started (its first SETUP follows the
    // pull-up by ~0.3 s) and short enough to leave the board's window intact.
    const SOFFN_WINDOW_SECS: u64 = 3;
    // `usb2-live-arm-late`: one run answers two questions about arming.
    // Measured 2026-09-29: mark A fired and mark B did not in a build where the
    // arm was the only difference, which first looked like a wedged loop - but
    // the step word then showed all four steps inside `try_arm_setup()` running,
    // so that was an unreadable mark at the pull-up, not a wedge.
    //
    // Window 3 s, deliberately: each mark costs ~2.2 s here (measured with
    // `usb2-live-clock-check`), so mark B plus the landing bucket already spend
    // most of the board's usable time.
    const LATE_WINDOW_SECS: u64 = 3;
    const LATE_ARM_AFTER_MILLIS: u64 = 600;
    // Reported late, so the edges are audible: 2.0 s "the loop is alive", 2.5 s
    // "a SETUP was consumed (1 pulse) or not (2)", 3.0 s "still alive".
    const CONSUME_LATE_WINDOW_SECS: u64 = 4;
    const CONSUME_REPORT_MS: u64 = 2_000;
    const CONSUME_REPORT_STEP_MS: u64 = 500;
    // Calibration word: one pulse at 2.0 s and one at 4.0 s after the drive
    // starts, nothing else at all.
    const CLOCK_CHECK_WINDOW_SECS: u64 = 5;
    const CLOCK_CHECK_FIRST_MS: u64 = 2_000;
    const CLOCK_CHECK_SECOND_MS: u64 = 4_000;
    const ARM_LAND_WINDOW_SECS: u64 = 4;
    const ARM_LAND_RESULT_MS: u64 = 1_500;
    const ARM_LAND_LANDING_MS: u64 = 3_000;
    const SETUP_TRB_WINDOW_SECS: u64 = 3;
    // One mark and a 5.9 s window (the board's watchdog after the last pet), so the
    // report has to come early: at 3.0 s it never arrived in run 586408.
    const SETUP_TRB_REPORT_MS: u64 = 1_200;
    const HANDLER_WINDOW_SECS: u64 = 4;
    const ARM_SETTLED_WINDOW_SECS: u64 = 8;
    const ARM_SETTLED_ARM_MS: u64 = 1_800;
    const ARM_RESULT_WINDOW_SECS: u64 = 3;
    const SETUP_REACHED_WINDOW_SECS: u64 = 3;
    const SETUP_REACHED_ARM_MS: u64 = 600;
    const SETUP_REACHED_REPORT_MS: u64 = 2_000;
    const ARM_STAGE_WINDOW_SECS: u64 = 3;
    const ARM_STAGE_ARM_MS: u64 = 600;
    const PRECHECK_WINDOW_SECS: u64 = 3;
    const PRECHECK_REPORT_MS: u64 = 800;
    const DATA_PHASE_WINDOW_SECS: u64 = 5;
    const DATA_PHASE_ARM_MS: u64 = 600;
    const ARM_RECOVER_WINDOW_SECS: u64 = 8;
    const ARM_RECOVER_MS: u64 = 600;
    const TRIAGE_WINDOW_SECS: u64 = 14;
    const TRIAGE_ARM_MS: u64 = 600;
    const TRIAGE_FIRST_MS: u64 = 2_200;
    const TRIAGE_STEP_MS: u64 = 1_600;
    const OBSERVE_WINDOW_SECS: u64 = 14;
    const OBSERVE_FIRST_MS: u64 = 1_500;
    const OBSERVE_STEP_MS: u64 = 1_600;
    const RESPONSE_BYTES_WINDOW_SECS: u64 = 6;
    const RESPONSE_BYTES_FIRST_MS: u64 = 1_800;
    const RESPONSE_BYTES_STEP_MS: u64 = 400;
    const SETUP_BUFFER_WINDOW_SECS: u64 = 8;
    const SETUP_BUFFER_FIRST_MS: u64 = 1_600;
    const SETUP_BUFFER_STEP_MS: u64 = 1_400;
    const FRESH_GATES_WINDOW_SECS: u64 = 9;
    const FRESH_GATES_FIRST_MS: u64 = 1_500;
    const FRESH_GATES_STEP_MS: u64 = 1_000;
    const DATA_REPORT_WINDOW_SECS: u64 = 9;
    const DATA_REPORT_FIRST_MS: u64 = 1_500;
    const DATA_REPORT_STEP_MS: u64 = 1_000;
    const DATA_CMD_WINDOW_SECS: u64 = 8;
    const DATA_CMD_FIRST_MS: u64 = 1_400;
    const DATA_CMD_STEP_MS: u64 = 900;
    // Tightened 2026-09-30 06:40: the 1.4 + k*0.9 schedule put field 4 at 4.1 s,
    // past the run's last publish (3.26 s in 1221162.0), so the DEPCMD(0) sanity
    // digit never read. 1.2 + k*0.7 puts all four fields inside 3.3 s.
    const DEPCMD_WINDOW_SECS: u64 = 8;
    const DEPCMD_FIRST_MS: u64 = 1_200;
    const DEPCMD_STEP_MS: u64 = 700;
    const EP0_PHYSICAL_WINDOW_SECS: u64 = 20;
    const FAIL_STATE_WINDOW_SECS: u64 = 9;
    const FAIL_STATE_FIRST_MS: u64 = 1_400;
    const FAIL_STATE_STEP_MS: u64 = 900;
    const GLOBAL_BLOCK_WINDOW_SECS: u64 = 10;
    const GLOBAL_BLOCK_FIRST_MS: u64 = 1_400;
    const GLOBAL_BLOCK_STEP_MS: u64 = 1_100;
    const EVENT_COUNT_WINDOW_SECS: u64 = 9;
    // Window and schedule for `usb2-live-blackbox`. Four fields, comfortably
    // inside the board's 5.9 s unfed-reset budget.
    // The calibration ladder: eight widths from 60 ms to 560 ms, a fixed gap
    // between them, emitted once. The whole sequence is about 3.2 s, which fits
    // inside the board's unfed-reset budget with the drive still running.
    const PULSE_CAL_WINDOW_SECS: u64 = 14;
    // The status-only attempt goes at 2.5 s: after the setup stage has completed (the
    // drive arms at 0.6 s and the setup TRB is consumed shortly after) and after the
    // data stage has had its turn and failed to retire.
    // When the probe words act. Three timings were tried today and the measurement chose this
    // one:
    //   * 2.5 s - the original, late enough that the host has already tried and failed, and the
    //     probes retire here;
    //   * 0.6 s - *rejected*: the run died with the host at -110, because a probe takes over the
    //     armed SETUP TRB slot and `retry_start_transfer` revokes EP0's live transfer with
    //     ENDTRANSFER. Before the host's first attempt that is fatal, which is the interference
    //     the review flagged, now measured rather than assumed;
    //   * 3.6 s for the *readout* - rejected too: fields that late are outside the run window
    //     and the words came back with no fields at all.
    const STATUS_TRY_MS: u64 = 2_500;
    // The probe words below act at STATUS_TRY_MS or later, so their fields are published on a
    // later schedule than the other black-box words. Publishing them at 1.0 s, as they were,
    // sampled the latches *before* the actions ran and reported the "not attempted" zero as a
    // result; several conclusions from those runs are being re-measured because of it.
    // Just after the probe action above, and inside the window the other words have proven
    // reachable (fields at 1.0/2.4/3.8 s all fire).
    const PROBE_FIRST_MS: u64 = 2_700;
    const PROBE_STEP_MS: u64 = 1_000;
    // The persistent re-issue starts earlier than the single-shot probes because three
    // 1200 ms polls have to fit before the board resets itself at about 5.9 s.
    const DATA_PERSIST_START_MS: u64 = 600;
    // After two 2000 ms polls that begin at 0.6 s. The loop ends by 4.6 s, so this field lands
    // after it - the one case where the action is long enough that the readout has to wait.
    const DATA_PERSIST_FIRST_MS: u64 = 4_800;
    const PULSE_CAL_FIRST_MS: u64 = 1_000;
    // 120 ms between pulses, not 150: the ladder has to finish before the board
    // resets itself at about 5.9 s without a feed, and the first ladder of eight
    // widths plus the sync marker is 2.3 s of disconnects on its own.
    const PULSE_CAL_GAP_MS: u64 = 120;
    // The first entry is a sync marker, not a class: a long pulse the capture can
    // be aligned on, because a usbmon capture from a run also contains the
    // attach and the host's own retries. Everything after it is the ladder.
    const PULSE_CAL_WIDTHS: [u64; 8] = [900, 60, 90, 130, 180, 240, 320, 420];
    const BLACKBOX_WINDOW_SECS: u64 = 10;
    // Chosen so that the black box words' later fields land *after* the 2.5 s data-stage
    // probes: fields at 1.0 / 2.4 / 3.8 / 5.2 s, which puts the TRB-state fields
    // (indices 1 and 2 of `usb2-live-data-late`) after the re-issue they describe. With the
    // previous 1.4/1.3 schedule the run ended before those fields fired and the question
    // "did the controller consume the TRB or only accept the command" went unanswered.
    const BLACKBOX_FIRST_MS: u64 = 1_000;
    const BLACKBOX_STEP_MS: u64 = 1_400;
    const EVENT_COUNT_FIRST_MS: u64 = 1_400;
    const EVENT_COUNT_STEP_MS: u64 = 900;
    // core.h line 137: `DWC3_GEVNTCOUNT(n) = 0xc40c + n*0x10`, so buffer 1 is 0xc41c.
    const GEVNTCOUNT1: usize = 0xc41c;
    const DEPCMD_STATE_WINDOW_SECS: u64 = 9;
    const DEPCMD_STATE_FIRST_MS: u64 = 1_400;
    const DEPCMD_STATE_STEP_MS: u64 = 900;
    // `GCTL`, `GCTL_DSBLCLKGTNG` and `GCTL_CORESOFTRESET` all already exist in
    // mmio.rs with the values core.h gives them, so nothing new is needed here.
    const DATA_PHASE_TRB_MS: u64 = 1_800;
    const DATA_PHASE_REJECT_MS: u64 = 2_600;

    // Marks. For the SOFFN word there are NO entry/exit marks: the first pulse
    // after the pull-up can be absorbed by the host's own first observation of
    // the connect, and the board's fail-safe competes with the window, so the
    // bucket at the end has to be the entire reading - 1 pulse for "SOFFN moved",
    // 2 for "it did not", nothing at all for "the drive never got that far".
    // Elsewhere: two on entry, two on exit.
    if !soffn_probe
        && !arm_late
        && !clock_check
        && !arm_land
        && !setup_trb_state
        && !handler_steps
        && !arm_settled
        && !arm_result
        && !setup_reached
        && !arm_stage
        && !precheck
        && !data_phase
        && !arm_recover
        && !triage
        && !observe
        && !response_bytes_word
        && !setup_buffer_word
        && !fresh_gates
        && !data_report
        && !data_cmd
        && !depcmd
        && !ep0_physical
        && !fail_state
        && !global_block
        && !event_count
        && !ring_reset
        && !blackbox_word
        && !blackbox_read
        && !pulse_cal
        && !blackbox_stage
        && !blackbox_recent
        && !blackbox_state
        && !blackbox_count
        && !blackbox_probe
        && !blackbox_code0
        && !blackbox_code1
        && !blackbox_code2
        && !blackbox_boots
        && !blackbox_bootsn
        && !blackbox_why
        && !status_try
        && !data_zero
        && !data_alt
        && !data_late
        && !data_persist
        && !ioava_check
        && !data_status
        && !pstore_ready
        && !pmsg_read
        && !pmsg_age
        && !consume_late
        && !text_log
        && !prev_record
        && !prev_boot
        && !prev_region
        && !prev_stage
        && !prev_probe_retired
        && !prev_probe_trb
        && !depcmd_state
        && !ack_event
        && !single_try
        && !trb_after
        && !_trb_after_ep0
        && !reset_first
    {
        unsafe { ccs_pulse(300) };
        unsafe { ccs_pulse(300) };
    }
    let frequency = arch_counter_frequency();
    if frequency == 0 {
        unsafe { ccs_pulse(300) };
        unsafe { ccs_pulse(300) };
        unsafe { ccs_pulse(300) };
        return;
    }
    let soffn_before = if soffn_probe {
        Some(unsafe { read(DSTS) } & SOFFN_MASK)
    } else {
        None
    };
    // NO ARMING in this loop, deliberately. Two facts from the runs of
    // 2026-09-29 say why:
    //
    //   * `EP0_SETUP_ARMED` is false when the drive starts (run 421919), because
    //     the site that used to arm it in this profile is the in-handoff arm
    //     window, which the drive runs before. Without an arm the host's SETUP
    //     cannot land - but it can still *arrive at the core*, and the SOFFN
    //     question is about frames reaching the controller, not about this loop
    //     consuming them.
    //   * every build that armed inside the drive stopped early (425574, 429216:
    //     one CCS edge, reproduced; 476502: no bucket at all), while the poll-only
    //     drive reached 250k+ passes (360466) and carried the host to the address
    //     stage. So the arm is what kills this loop, and it is out until the
    //     reason is known.
    let entry = arch_counter();
    let arm_at = entry.saturating_add(
        frequency
            .saturating_mul(LATE_ARM_AFTER_MILLIS)
            .saturating_div(1_000),
    );
    let mut arm_done = !arm_late;
    let mut consumed_any = false;
    if consume_late {
        // Arm before the host's first SETUP (~0.3 s after the pull-up), then keep
        // polling; everything below only reports what happened.
        let _ = unsafe { try_arm_setup() };
    }
    let clock_a = entry.saturating_add(
        frequency
            .saturating_mul(CLOCK_CHECK_FIRST_MS)
            .saturating_div(1_000),
    );
    let clock_b = entry.saturating_add(
        frequency
            .saturating_mul(CLOCK_CHECK_SECOND_MS)
            .saturating_div(1_000),
    );
    let mut clock_a_done = false;
    let mut clock_b_done = false;
    let mut arm_ok: Option<bool> = None;
    if arm_land {
        arm_ok = Some(unsafe { try_arm_setup() });
    }
    if setup_trb_state {
        let _ = unsafe { try_arm_setup() };
    }
    if handler_steps {
        let _ = unsafe { try_arm_setup() };
    }
    // `data-phase` arms at 600 ms rather than at entry: an arm issued immediately
    // after the pull-up reliably kills this loop (measured repeatedly), while the
    // 600 ms timing has been seen to return. The readout is scheduled after it.
    let data_arm_at = entry.saturating_add(
        frequency
            .saturating_mul(DATA_PHASE_ARM_MS)
            .saturating_div(1_000),
    );
    let mut data_arm_done = false;
    let recover_at = entry.saturating_add(
        frequency
            .saturating_mul(ARM_RECOVER_MS)
            .saturating_div(1_000),
    );
    let mut recover_done = false;
    let triage_arm_at = entry.saturating_add(
        frequency
            .saturating_mul(TRIAGE_ARM_MS)
            .saturating_div(1_000),
    );
    let mut triage_arm_done = false;
    let mut triage_field = 0u32;
    let mut triage_arm_ok: Option<bool> = None;
    let mut observe_field = 0u32;
    let mut response_field = 0u32;
    let mut setup_field = 0u32;
    let mut gates_field = 0u32;
    let mut data_report_field = 0u32;
    let mut data_cmd_field = 0u32;
    let mut fail_state_field = 0u32;
    let mut global_field = 0u32;
    let mut event_field = 0u32;
    let mut ring_reset_done = false;
    let mut blackbox_field = 0u32;
    let mut blackbox_read_field = 0u32;
    let mut pulse_cal_done = false;
    let mut blackbox_stage_field = 0u32;
    let mut blackbox_times_field = 0;
    let mut setup_bytes_field = 0;
    let mut dtrb_addr_field = 0;
    let mut dtrb_size_field = 0;
    let mut ep0_context_field = 0;
    let mut dtrb_ctrl_field = 0;
    let mut setup_count_field = 0;
    let mut gvnt0_field = 0;
    let mut setup_late_field = 0;
    let mut events_acked_field = 0;
    let mut arm_stage_field = 0;
    let mut arm_rounds_field = 0;
    let mut susphy_field = 0;
    let mut susphy_test_field = 0;
    let mut blackbox_recent_field = 0u32;
    let mut blackbox_state_field = 0u32;
    let mut blackbox_count_field = 0u32;
    let mut blackbox_probe_field = 0u32;
    let mut blackbox_code0_field = 0u32;
    let mut blackbox_code1_field = 0u32;
    let mut blackbox_code2_field = 0u32;
    let mut blackbox_boots_field = 0u32;
    let mut blackbox_bootsn_field = 0u32;
    let mut blackbox_why_field = 0u32;
    let mut status_try_done = false;
    let mut data_zero_done = false;
    let mut data_alt_done = false;
    let mut data_late_done = false;
    let mut data_gap_done = false;
    let mut data_rewrite_done = false;
    let mut data_persist_done = false;
    let mut ioava_check_field = 0u32;
    let mut data_status_done = false;
    let mut data_status_field = 0u32;
    let mut pstore_ready_field = 0u32;
    let mut pmsg_read_field = 0u32;
    let mut pmsg_age_field = 0u32;
    let mut text_log_field = 0u32;
    let mut prev_record_field = 0u32;
    let mut prev_boot_field = 0u32;
    let mut prev_region_field = 0u32;
    let mut prev_stage_field = 0u32;
    let mut prev_probe_retired_field = 0u32;
    let mut prev_probe_trb_field = 0u32;
    let mut data_persist_field = 0u32;
    let mut data_late_field = 0u32;
    let mut data_alt_field = 0u32;
    let mut data_zero_field = 0u32;
    let mut status_try_field = 0u32;
    let mut depcmd_field = 0u32;
    let mut trb_after_field = 0u32;
    let mut reset_first_done = false;
    let settled_arm_at = entry.saturating_add(
        frequency
            .saturating_mul(ARM_SETTLED_ARM_MS)
            .saturating_div(1_000),
    );
    let mut settled_arm_done = false;
    let mut result_arm_done = false;
    let reached_arm_at = entry.saturating_add(
        frequency
            .saturating_mul(SETUP_REACHED_ARM_MS)
            .saturating_div(1_000),
    );
    let reached_report_at = entry.saturating_add(
        frequency
            .saturating_mul(SETUP_REACHED_REPORT_MS)
            .saturating_div(1_000),
    );
    let arm_stage_at = entry.saturating_add(
        frequency
            .saturating_mul(ARM_STAGE_ARM_MS)
            .saturating_div(1_000),
    );
    let mut arm_stage_done = false;
    let precheck_at = entry.saturating_add(
        frequency
            .saturating_mul(PRECHECK_REPORT_MS)
            .saturating_div(1_000),
    );
    let mut precheck_done = false;
    let data_trb_at = entry.saturating_add(
        frequency
            .saturating_mul(DATA_PHASE_TRB_MS)
            .saturating_div(1_000),
    );
    let data_reject_at = entry.saturating_add(
        frequency
            .saturating_mul(DATA_PHASE_REJECT_MS)
            .saturating_div(1_000),
    );
    let mut data_trb_done = false;
    let mut data_reject_done = false;
    let mut reached_arm_done = false;
    let mut reached_report_done = false;
    let trb_report_at = entry.saturating_add(
        frequency
            .saturating_mul(SETUP_TRB_REPORT_MS)
            .saturating_div(1_000),
    );
    let mut trb_report_done = false;
    let result_at = entry.saturating_add(
        frequency
            .saturating_mul(ARM_LAND_RESULT_MS)
            .saturating_div(1_000),
    );
    let landing_at = entry.saturating_add(
        frequency
            .saturating_mul(ARM_LAND_LANDING_MS)
            .saturating_div(1_000),
    );
    let mut result_done = false;
    let mut landing_done = false;
    let mut report_stage: u32 = 0;
    let mut next_report_at = entry.saturating_add(
        frequency
            .saturating_mul(CONSUME_REPORT_MS)
            .saturating_div(1_000),
    );
    let deadline = entry.saturating_add(frequency.saturating_mul(window_secs));
    while arch_counter() < deadline {
        wdt_pet();
        // Keep the controller powered while polling. Without this the domain
        // collapses about a second after the pull-up and the arm's MMIO never
        // returns; the handoff's own arm window keeps it alive with a 200 ms
        // keepalive between attempts, which a tight polling loop cannot afford.
        usb_domain_keepalive();
        // Acknowledge the events the controller has posted, every pass.
        //
        // The drain was removed from the *arm* loop on 2026-09-29 because `read(GEVNTCOUNT0)` did not
        // return there, and the removal is why nothing in this driver has ever acknowledged the EP0
        // ring. Without that acknowledgement the controller stops posting events and, being
        // event-driven, does not advance its EP0 state machine: one setup delivered, then silence,
        // while the host re-tries 26 to 54 times.
        //
        // The read returns at the handoff on this unit (measured: `usb2-live-gvnt0` reads the register
        // back and gets zero), so the earlier measurement is context-specific rather than universal.
        // This call site is the drive, which runs after the pull-up, not the arm loop.
        unsafe { super::control::acknowledge_ep0_events() };
        if unsafe { poll_setup_buffer() } {
            consumed_any = true;
        }
        if arm_late && !arm_done && arch_counter() >= arm_at {
            arm_done = true;
            let ok = unsafe { try_arm_setup() };
            // mark B: 1 pulse = the arm returned true, 2 = it refused. Measured
            // 2026-09-29 with the step word: all four steps inside the arm ran and
            // this mark fired, so an earlier "one edge" run was an unreadable
            // mark at the pull-up, not a wedged loop.
            unsafe { ccs_pulse(300) };
            if !ok {
                unsafe { ccs_pulse(300) };
            }
        }
        if arm_settled && !settled_arm_done && arch_counter() >= settled_arm_at {
            settled_arm_done = true;
            let _ = unsafe { try_arm_setup() };
        }
        if arm_result && !result_arm_done && arch_counter() >= settled_arm_at {
            result_arm_done = true;
            let ok = unsafe { try_arm_setup() };
            // 1 pulse = the arm returned true, 2 = it refused, and no pulse at all
            // means the call never came back.
            unsafe { ccs_pulse(300) };
            if !ok {
                unsafe { ccs_pulse(300) };
            }
        }
        if reset_first
            && !reset_first_done
            && arch_counter()
                >= entry.saturating_add(frequency.saturating_mul(300).saturating_div(1_000))
        {
            reset_first_done = true;
            // One device soft reset before any arm. `device_soft_reset()` is the same
            // primitive the project's recovery uses.
            let _ = unsafe { super::control::device_soft_reset() };
        }
        if ring_reset
            && !ring_reset_done
            && arch_counter()
                >= entry.saturating_add(frequency.saturating_mul(400).saturating_div(1_000))
        {
            ring_reset_done = true;
            // Re-programme the event ring with the values it already holds, in
            // Linux's order (`dwc3_event_buffers_setup`), and never read the
            // count: those registers answer, only the count reads stall. This
            // block was declared and guarded on the first attempt but left out of
            // the file, so the word ran as a plain `trb_after` run - and the "no
            // change" reading recorded from it is therefore withdrawn along with
            // the GEVNTCOUNT1 claim above.
            let addrlo = unsafe { read(GEVNTADRLO0) };
            let addrhi = unsafe { read(GEVNTADRHI0) };
            let size = unsafe { read(GEVNTSIZ0) };
            unsafe {
                write(GEVNTADRLO0, addrlo);
                write(GEVNTADRHI0, addrhi);
                write(GEVNTSIZ0, size);
            }
        }
        if trb_after {
            super::live_digit_words::publish(
                "trb_after",
                &mut trb_after_field,
                DATA_CMD_FIRST_MS,
                DATA_CMD_STEP_MS,
                entry,
                frequency,
            );
        }

        if event_count {
            super::live_digit_words::publish(
                "event_count",
                &mut event_field,
                EVENT_COUNT_FIRST_MS,
                EVENT_COUNT_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_word {
            super::live_digit_words::publish(
                "blackbox",
                &mut blackbox_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_read {
            super::live_digit_words::publish(
                "blackbox_read",
                &mut blackbox_read_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if pulse_cal
            && !pulse_cal_done
            && arch_counter()
                >= entry.saturating_add(
                    frequency
                        .saturating_mul(PULSE_CAL_FIRST_MS)
                        .saturating_div(1_000),
                )
        {
            pulse_cal_done = true;
            super::link_recovery::runstop_width_sequence(&PULSE_CAL_WIDTHS, PULSE_CAL_GAP_MS);
        }

        if blackbox_stage {
            super::live_digit_words::publish(
                "blackbox_stage",
                &mut blackbox_stage_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_times {
            super::live_digit_words::publish(
                "blackbox_times",
                &mut blackbox_times_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if setup_bytes {
            super::live_digit_words::publish(
                "setup_bytes",
                &mut setup_bytes_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if dtrb_addr {
            super::live_digit_words::publish(
                "dtrb_addr",
                &mut dtrb_addr_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if dtrb_size {
            super::live_digit_words::publish(
                "dtrb_size",
                &mut dtrb_size_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if ep0_context {
            super::live_digit_words::publish(
                "ep0_context",
                &mut ep0_context_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if dtrb_ctrl {
            super::live_digit_words::publish(
                "dtrb_ctrl",
                &mut dtrb_ctrl_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if setup_count {
            super::live_digit_words::publish(
                "setup_count",
                &mut setup_count_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if gvnt0 {
            super::live_digit_words::publish(
                "gvnt0",
                &mut gvnt0_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if setup_late {
            super::live_digit_words::publish(
                "setup_late",
                &mut setup_late_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if events_acked {
            super::live_digit_words::publish(
                "events_acked",
                &mut events_acked_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if rearm_stage {
            super::live_digit_words::publish(
                "rearm_stage",
                &mut arm_stage_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if arm_rounds {
            super::live_digit_words::publish(
                "arm_rounds",
                &mut arm_rounds_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if susphy {
            super::live_digit_words::publish(
                "susphy",
                &mut susphy_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if susphy_test {
            super::live_digit_words::publish(
                "susphy_test",
                &mut susphy_test_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_recent {
            super::live_digit_words::publish(
                "blackbox_recent",
                &mut blackbox_recent_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_state {
            super::live_digit_words::publish(
                "blackbox_state",
                &mut blackbox_state_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_count {
            super::live_digit_words::publish(
                "blackbox_count",
                &mut blackbox_count_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_probe {
            super::live_digit_words::publish(
                "blackbox_probe",
                &mut blackbox_probe_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_code0 {
            super::live_digit_words::publish(
                "blackbox_code0",
                &mut blackbox_code0_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_code1 {
            super::live_digit_words::publish(
                "blackbox_code1",
                &mut blackbox_code1_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_code2 {
            super::live_digit_words::publish(
                "blackbox_code2",
                &mut blackbox_code2_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_boots {
            super::live_digit_words::publish(
                "blackbox_boots",
                &mut blackbox_boots_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_bootsn {
            super::live_digit_words::publish(
                "blackbox_bootsn",
                &mut blackbox_bootsn_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if blackbox_why {
            super::live_digit_words::publish(
                "blackbox_why",
                &mut blackbox_why_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if status_try
            && !status_try_done
            && arch_counter()
                >= entry.saturating_add(
                    frequency
                        .saturating_mul(STATUS_TRY_MS)
                        .saturating_div(1_000),
                )
        {
            status_try_done = true;
            let retired = unsafe { super::ep0::try_status_only() };
            record_probe_outcome(retired);
        }

        if data_zero
            && !data_zero_done
            && arch_counter()
                >= entry.saturating_add(
                    frequency
                        .saturating_mul(STATUS_TRY_MS)
                        .saturating_div(1_000),
                )
        {
            data_zero_done = true;
            let retired = unsafe { super::ep0::try_data_zero() };
            record_probe_outcome(retired);
        }

        if data_alt
            && !data_alt_done
            && arch_counter()
                >= entry.saturating_add(
                    frequency
                        .saturating_mul(STATUS_TRY_MS)
                        .saturating_div(1_000),
                )
        {
            data_alt_done = true;
            let retired = unsafe { super::ep0::try_data_alt_buffer() };
            record_probe_outcome(retired);
        }

        if data_late
            && !data_late_done
            && arch_counter()
                >= entry.saturating_add(
                    frequency
                        .saturating_mul(STATUS_TRY_MS)
                        .saturating_div(1_000),
                )
        {
            data_late_done = true;
            let retired = unsafe { super::ep0::retry_data_late() };
            record_probe_outcome(retired);
        }

        if data_gap
            && !data_gap_done
            && arch_counter()
                >= entry.saturating_add(frequency.saturating_mul(data_gap_ms).saturating_div(1_000))
        {
            data_gap_done = true;
            let started = unsafe { super::ep0::retry_data_once() };
            record_probe_outcome(started);
        }

        if data_rewrite
            && !data_rewrite_done
            && arch_counter()
                >= entry.saturating_add(frequency.saturating_mul(1_000).saturating_div(1_000))
        {
            data_rewrite_done = true;
            let done_rewriting = unsafe { super::ep0::rewrite_data_trb() };
            record_probe_outcome(done_rewriting);
        }

        if data_persist
            && !data_persist_done
            && arch_counter()
                >= entry.saturating_add(
                    frequency
                        .saturating_mul(DATA_PERSIST_START_MS)
                        .saturating_div(1_000),
                )
        {
            data_persist_done = true;
            // Two attempts, no gap between them because each attempt polls for 2000 ms: the loop
            // therefore spans 0.6 s to about 4.6 s, inside the board's unfed-reset budget, and
            // stops early as soon as one retires. (This comment used to claim three attempts of
            // 1200 ms starting at 1.5 s, which matched neither the call nor the constant; the
            // result field is published after 4.8 s so it cannot be sampled before the loop ends.)
            let retired = unsafe { super::ep0::retry_data_until_retired(2, 0) } > 0;
            record_probe_outcome(retired);
        }

        if status_try {
            super::live_digit_words::publish(
                "status_try",
                &mut status_try_field,
                PROBE_FIRST_MS,
                PROBE_STEP_MS,
                entry,
                frequency,
            );
        }

        if data_zero {
            super::live_digit_words::publish(
                "data_zero",
                &mut data_zero_field,
                PROBE_FIRST_MS,
                PROBE_STEP_MS,
                entry,
                frequency,
            );
        }

        if data_alt {
            super::live_digit_words::publish(
                "data_alt",
                &mut data_alt_field,
                PROBE_FIRST_MS,
                PROBE_STEP_MS,
                entry,
                frequency,
            );
        }

        if data_late {
            super::live_digit_words::publish(
                "data_late",
                &mut data_late_field,
                PROBE_FIRST_MS,
                PROBE_STEP_MS,
                entry,
                frequency,
            );
        }

        if data_persist {
            // This one's action starts earlier (its two 2 s polls have to fit before the board's
            // 5.9 s unfed reset) and its fields therefore start later still.
            super::live_digit_words::publish(
                "data_persist",
                &mut data_persist_field,
                DATA_PERSIST_FIRST_MS,
                1_000,
                entry,
                frequency,
            );
        }

        if ioava_check {
            super::live_digit_words::publish(
                "ioava_check",
                &mut ioava_check_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if data_status
            && !data_status_done
            && arch_counter()
                >= entry.saturating_add(
                    frequency
                        .saturating_mul(STATUS_TRY_MS)
                        .saturating_div(1_000),
                )
        {
            data_status_done = true;
            let result = unsafe { super::ep0::data_then_status() };
            record_probe_outcome(result & 1 != 0);
        }

        if data_status {
            super::live_digit_words::publish(
                "data_status",
                &mut data_status_field,
                PROBE_FIRST_MS,
                PROBE_STEP_MS,
                entry,
                frequency,
            );
        }

        if pstore_ready {
            super::live_digit_words::publish(
                "pstore_ready",
                &mut pstore_ready_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if pmsg_read {
            super::live_digit_words::publish(
                "pmsg_read",
                &mut pmsg_read_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if pmsg_age {
            super::live_digit_words::publish(
                "pmsg_age",
                &mut pmsg_age_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if text_log {
            super::live_digit_words::publish(
                "text",
                &mut text_log_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if prev_record {
            super::live_digit_words::publish(
                "prev_record",
                &mut prev_record_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if prev_boot {
            super::live_digit_words::publish(
                "prev_boot",
                &mut prev_boot_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if prev_region {
            super::live_digit_words::publish(
                "prev_region",
                &mut prev_region_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if prev_probe_retired {
            super::live_digit_words::publish("prev_probe_retired", &mut prev_probe_retired_field,
                BLACKBOX_FIRST_MS, BLACKBOX_STEP_MS, entry, frequency);
        }

        if prev_probe_trb {
            super::live_digit_words::publish("prev_probe_trb", &mut prev_probe_trb_field,
                BLACKBOX_FIRST_MS, BLACKBOX_STEP_MS, entry, frequency);
        }

        if prev_stage {
            super::live_digit_words::publish(
                "prev_stage",
                &mut prev_stage_field,
                BLACKBOX_FIRST_MS,
                BLACKBOX_STEP_MS,
                entry,
                frequency,
            );
        }

        if depcmd_state {
            super::live_digit_words::publish(
                "depcmd_state",
                &mut depcmd_field,
                DEPCMD_STATE_FIRST_MS,
                DEPCMD_STATE_STEP_MS,
                entry,
                frequency,
            );
        }

        if global_block {
            super::live_digit_words::publish(
                "global_block",
                &mut global_field,
                GLOBAL_BLOCK_FIRST_MS,
                GLOBAL_BLOCK_STEP_MS,
                entry,
                frequency,
            );
        }

        if fail_state {
            super::live_digit_words::publish(
                "fail_state",
                &mut fail_state_field,
                FAIL_STATE_FIRST_MS,
                FAIL_STATE_STEP_MS,
                entry,
                frequency,
            );
        }

        if data_cmd {
            super::live_digit_words::publish(
                "data_cmd",
                &mut data_cmd_field,
                DATA_CMD_FIRST_MS,
                DATA_CMD_STEP_MS,
                entry,
                frequency,
            );
        }

        if depcmd {
            super::live_digit_words::publish(
                "depcmd",
                &mut depcmd_field,
                DEPCMD_FIRST_MS,
                DEPCMD_STEP_MS,
                entry,
                frequency,
            );
        }

        if data_report {
            super::live_digit_words::publish(
                "data_report",
                &mut data_report_field,
                DATA_REPORT_FIRST_MS,
                DATA_REPORT_STEP_MS,
                entry,
                frequency,
            );
        }

        if fresh_gates {
            super::live_digit_words::publish(
                "fresh_gates",
                &mut gates_field,
                FRESH_GATES_FIRST_MS,
                FRESH_GATES_STEP_MS,
                entry,
                frequency,
            );
        }

        if setup_buffer_word {
            super::live_digit_words::publish(
                "setup_buffer_word",
                &mut setup_field,
                SETUP_BUFFER_FIRST_MS,
                SETUP_BUFFER_STEP_MS,
                entry,
                frequency,
            );
        }

        if response_bytes_word {
            super::live_digit_words::publish(
                "response_bytes_word",
                &mut response_field,
                RESPONSE_BYTES_FIRST_MS,
                RESPONSE_BYTES_STEP_MS,
                entry,
                frequency,
            );
        }

        if observe {
            super::live_digit_words::publish(
                "observe",
                &mut observe_field,
                OBSERVE_FIRST_MS,
                OBSERVE_STEP_MS,
                entry,
                frequency,
            );
        }

        if triage && !triage_arm_done && arch_counter() >= triage_arm_at {
            triage_arm_done = true;
            // Stash the result; the fields are published later, after the arm has
            // had time to be consumed by the host's request.
            triage_arm_ok = Some(unsafe { try_arm_setup() });
        }
        if triage && triage_arm_done && triage_field < 4 {
            let field_at = entry.saturating_add(
                frequency
                    .saturating_mul(TRIAGE_FIRST_MS + (triage_field as u64) * TRIAGE_STEP_MS)
                    .saturating_div(1_000),
            );
            if arch_counter() >= field_at {
                let digit: u32 = match triage_field {
                    0 => match triage_arm_ok {
                        Some(true) => 1,
                        Some(false) => (unsafe { arm_failure_stage() }.min(4) + 1),
                        None => 5,
                    },
                    1 => (unsafe { control_data_trb_state() } as u32 + 1),
                    2 => {
                        if unsafe { data_phase_start_rejected() } {
                            2
                        } else {
                            1
                        }
                    }
                    _ => unsafe { ep0_state_digit() },
                };
                super::link_recovery::runstop_blips_fast(digit.min(6));
                triage_field += 1;
            }
        }
        if arm_recover && !recover_done && arch_counter() >= recover_at {
            recover_done = true;
            let ok = unsafe { try_arm_setup() };
            let first = if ok {
                1
            } else {
                unsafe { arm_failure_stage() }.min(4) + 1
            };
            super::link_recovery::runstop_blips_fast(first.min(6));
            if !ok {
                // The repair the project wrote for exactly this: one controller
                // soft reset, then a re-arm. Never reachable in drive-first before.
                let _ = super::link_recovery::u0_arm_window_recovery();
                let second = if unsafe { EP0_SETUP_ARMED } { 1 } else { 2 };
                super::link_recovery::runstop_blips_fast(second.min(6));
            }
        }
        if data_phase && !data_arm_done && arch_counter() >= data_arm_at {
            data_arm_done = true;
            let _ = unsafe { try_arm_setup() };
        }
        if data_phase && !data_trb_done && arch_counter() >= data_trb_at {
            data_trb_done = true;
            let digit = unsafe { control_data_trb_state() } + 1;
            let mut left = digit.min(3);
            while left > 0 {
                unsafe { short_mark() };
                left -= 1;
            }
        }
        if data_phase && !data_reject_done && arch_counter() >= data_reject_at {
            data_reject_done = true;
            let digit = if unsafe { data_phase_start_rejected() } {
                2
            } else {
                1
            };
            let mut left = digit;
            while left > 0 {
                unsafe { short_mark() };
                left -= 1;
            }
        }
        if precheck && !precheck_done && arch_counter() >= precheck_at {
            precheck_done = true;
            let (state, ready, armed, cooldown, stage) = unsafe { arm_preconditions() };
            let digits = [
                state + 1,
                if ready { 1 } else { 2 },
                if armed { 1 } else { 2 },
                if cooldown != 0 { 2 } else { 1 },
                stage + 1,
            ];
            for digit in digits {
                // `runstop_blips_fast` is the only genuinely short host-visible
                // primitive in the tree: 100 ms stopped + 70 ms running per pair,
                // six pairs in about 1.2 s. My own `short_mark`/`medium_mark` were
                // not short at all - `readout_keepalive_delay_ms` spends ~2 s
                // whatever it is asked for - which is why the first separator build
                // read as seven 2 s marks with 55 ms gaps. Fields are separated by
                // their own spacing (1.5 s apart) instead, which is 70 ms versus
                // 300+ ms of gap.
                super::link_recovery::runstop_blips_fast(digit.min(6));
            }
        }
        if arm_stage && !arm_stage_done && arch_counter() >= arm_stage_at {
            arm_stage_done = true;
            let armed = unsafe { try_arm_setup() };
            // `stage + 1` pulses: 1 the arm succeeded, 2 the halt gate refused it,
            // 3 a status error, 4 a command that never retired.
            let stage = if armed {
                0
            } else {
                unsafe { arm_failure_stage() }
            };
            let mut left = stage.min(3) + 1;
            while left > 0 {
                unsafe { ccs_pulse(300) };
                left -= 1;
            }
        }
        if setup_reached && !reached_arm_done && arch_counter() >= reached_arm_at {
            reached_arm_done = true;
            let _ = unsafe { try_arm_setup() };
        }
        if setup_reached && !reached_report_done && arch_counter() >= reached_report_at {
            reached_report_done = true;
            // 1 pulse = the SETUP TRB was retired (the request arrived); 2 = it is
            // still owned by the controller (it never arrived).
            unsafe { ccs_pulse(300) };
            if !unsafe { setup_trb_retired() } {
                unsafe { ccs_pulse(300) };
            }
        }
        if setup_trb_state && !trb_report_done && arch_counter() >= trb_report_at {
            trb_report_done = true;
            // 1 pulse = the core retired the SETUP TRB (the request arrived);
            // 2 = it still owns it (the request never reached the endpoint).
            unsafe { ccs_pulse(300) };
            if !unsafe { setup_trb_retired() } {
                unsafe { ccs_pulse(300) };
            }
        }
        if arm_land && !result_done && arch_counter() >= result_at {
            result_done = true;
            unsafe { ccs_pulse(300) };
            if arm_ok == Some(false) {
                unsafe { ccs_pulse(300) };
            }
        }
        if arm_land && !landing_done && arch_counter() >= landing_at {
            landing_done = true;
            unsafe { ccs_pulse(300) };
            if !consumed_any {
                unsafe { ccs_pulse(300) };
            }
        }
        if clock_check && !clock_a_done && arch_counter() >= clock_a {
            clock_a_done = true;
            unsafe { ccs_pulse(300) };
        }
        if clock_check && !clock_b_done && arch_counter() >= clock_b {
            clock_b_done = true;
            unsafe { ccs_pulse(300) };
        }
        // No reports from this loop for the consume word: the two marks inside
        // `poll_setup_buffer()` are the reading (and the arm was already done
        // before the loop, so there is nothing left to publish from here).
        let _ = (report_stage, next_report_at, consumed_any);
        if cfg!(fullerene_aarch64_usb_abl_event_consume) {
            // `--abl-event-consume` swaps in the ABL-style consumer. The ordinary
            // consumer stops this loop (bracket stage 11 silent while stage 10
            // fired in the same iteration), so it is not called here.
            let _ = unsafe { poll_ep0_event_ring() };
        }
    }
    if let Some(before) = soffn_before {
        // ONE WORD, ONE PREDICATE: 1 pulse = SOF frames arrived while the consumer
        // ran (the link is live and only EP0 fails), 2 pulses = SOFFN never moved
        // (no frames at all, which puts the failure in the PHY/link layer before
        // any of this driver's logic), no pulse = the drive never finished.
        let after = unsafe { read(DSTS) } & SOFFN_MASK;
        unsafe { ccs_pulse(300) };
        if after == before {
            unsafe { ccs_pulse(300) };
        }
    } else if !arm_late
        && !clock_check
        && !arm_land
        && !setup_trb_state
        && !handler_steps
        && !arm_settled
        && !arm_result
        && !setup_reached
        && !arm_stage
        && !precheck
        && !data_phase
        && !arm_recover
        && !triage
        && !observe
        && !response_bytes_word
        && !setup_buffer_word
        && !fresh_gates
        && !data_report
        && !data_cmd
        && !depcmd
        && !ep0_physical
        && !fail_state
        && !global_block
        && !event_count
        && !ring_reset
        && !blackbox_word
        && !blackbox_read
        && !pulse_cal
        && !blackbox_stage
        && !blackbox_recent
        && !blackbox_state
        && !blackbox_count
        && !blackbox_probe
        && !blackbox_code0
        && !blackbox_code1
        && !blackbox_code2
        && !blackbox_boots
        && !blackbox_bootsn
        && !blackbox_why
        && !status_try
        && !data_zero
        && !data_alt
        && !data_late
        && !data_persist
        && !ioava_check
        && !data_status
        && !pstore_ready
        && !pmsg_read
        && !pmsg_age
        && !consume_late
        && !text_log
        && !prev_record
        && !prev_boot
        && !prev_region
        && !prev_stage
        && !prev_probe_retired
        && !prev_probe_trb
        && !depcmd_state
        && !ack_event
        && !single_try
        && !trb_after
        && !_trb_after_ep0
        && !reset_first
    {
        unsafe { ccs_pulse(300) };
        unsafe { ccs_pulse(300) };
    }
    if arm_settled {
        // ONE WORD, ONE PREDICATE: while EP0 was armed from 1.8 s, did the host's
        // request land in the buffer this loop polls? 1 pulse = yes.
        unsafe { ccs_pulse(300) };
        if !consumed_any {
            unsafe { ccs_pulse(300) };
        }
    }
    if arm_buckets {
        // ONE WORD, ONE PREDICATE: while EP0 was armed, did the host's SETUP land
        // in the buffer this loop polls? 1 pulse = yes, 2 = never. The host's
        // descriptor attempts are ~0.3 s and ~5.4 s after the pull-up, so a
        // window of a few seconds after the arm covers at least one of them.
        unsafe { ccs_pulse(300) };
        if !consumed_any {
            unsafe { ccs_pulse(300) };
        }
    }
}

/// Poll the DWC3 event ring. This is intentionally cheap enough to run from
/// the early boot loop until the normal interrupt controller owns the device.
pub fn poll() {
    unsafe {
        // Proof that the driver loop ran, written where every crate can read it. There is
        // no other crate-independent way to say "poll() executed": a `static mut` is
        // duplicated by the second `usb/` compilation.
        //
        if !trace::POLL_MARKED {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x504F_4C31); // "POL1"
            trace::POLL_MARKED = true;
        }
        // Bracket stage 9: `poll()` was entered. Emitted before any early
        // return below, so a silent stage 9 can only mean "poll() never ran".
        probe_bracket_mark(9);
        // Diagnostic quiet window (see mmio_quiet_active): after this many
        // seconds past the first Run/Stop, stop ALL controller MMIO access.
        if mmio_quiet_active() {
            return;
        }
        // Eventless SETUP path: `GEVNTCOUNT0` stays 0 on this handoff even
        // though the host's traffic reaches the controller, so the EP0 SETUP
        // buffer is polled directly to answer the host's GET_DESCRIPTOR.
        // A no-op unless `--utmi-postrun-readout usb2-live-setup-poll`.
        let _ = poll_setup_buffer();
        // Bracket stage 10: the SETUP-buffer poll returned.
        probe_bracket_mark(10);
        link_on_sample();
        let runtime = USB_RUNTIME_STATE;
        // In the no-SMMU differential the whole point is to never touch the
        // Apps-SMMU: the stream is unmatched there and the (inactive, often
        // clock-gated) SMMU aperture can fault the CPU with an asynchronous
        // external abort when its runtime clock gates later in the session,
        // which reboots the handset right in the middle of host enumeration.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_smmu)
            && !matches!(
                runtime,
                super::super::platform::bramble::UsbRuntimeState::Off
                    | super::super::platform::bramble::UsbRuntimeState::Suspended
            )
        {
            service_smmu_fault();
        }
        service_power_event();
        // Complete a PMIC Type-C parent IRQ in normal polling context, after
        // the hard IRQ path has masked the parent. This is the early-boot
        // equivalent of Linux's threaded qcom-pmic-typec handler and keeps
        // slow SPMI transactions out of the DWC3 interrupt entry.
        service_deferred_platform();
        service_usb2_runtime_power_keepalive();
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
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_arm_window_recovery)]
        if RUN_STOP_TICK != 0
            && !CONFIGURED
            && EP0_STATE == Ep0State::Setup
            && !EP0_SETUP_ARMED
            && ARM_WINDOW_RECOVERY_ATTEMPTS < ARM_WINDOW_RECOVERY_MAX_ATTEMPTS
            && arch_counter_frequency() != 0
        {
            // The direct path can publish the pull-up long before xHCI
            // reaches the USB2 port. The first recovery is intentionally
            // late, and failed recoveries are retried at a bounded cadence:
            // on this board HS attach is ~39 s after Run/Stop while the
            // descriptor request arrives ~5 s later. Do not reset a live,
            // already-armed gadget; the attempt counter advances only when
            // the scheduled recovery is actually due.
            let elapsed = arch_counter().saturating_sub(RUN_STOP_TICK);
            let retry_after = ARM_WINDOW_RECOVERY_DELAY_SECS
                + u64::from(ARM_WINDOW_RECOVERY_ATTEMPTS)
                    .saturating_mul(ARM_WINDOW_RECOVERY_RETRY_SECS);
            if elapsed >= arch_counter_frequency().saturating_mul(retry_after) {
                ARM_WINDOW_RECOVERY_ATTEMPTS = ARM_WINDOW_RECOVERY_ATTEMPTS.saturating_add(1);
                log_puts("usb gadget handoff: late automatic EP0 recovery\n");
                let status = u0_arm_window_recovery();
                U0_ARM_STATUS = status;
                trace_event(
                    TRACE_SETUP_QUEUED,
                    0x5253_434C, // "RSCL": late arm-window recovery result
                    status,
                    EP0_SETUP_ARMED as u32,
                    u32::from(ARM_WINDOW_RECOVERY_ATTEMPTS),
                    read(DSTS),
                );
            }
        }
        let mut event_seen = poll_ep0_event_ring();
        // Bracket stage 11: the event-ring read returned. See the bracket note at
        // the top of this file for the interpretation of a missing tail.
        probe_bracket_mark(11);
        // If the event FIFO is empty, give the opt-in DMA fallback one chance
        // to dispatch a controller-retired EP0 TRB.  The fallback is skipped
        // whenever a real event batch was consumed, so it cannot double-run a
        // normal completion.
        if !event_seen {
            event_seen = poll_ep0_trb_completion_fallback();
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        update_signal_latches();
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1")
            && POST_RUNSTOP_PROBE_PENDING
            && arch_counter() >= POST_RUNSTOP_PROBE_NOT_BEFORE
            && read(DCTL) & DCTL_RUN_STOP != 0
        {
            // Run/Stop returns before the host's attach debounce. Perform the
            // probe only after the calibrated delay, after the normal event
            // consumer has handled any reset/connect event, so its
            // STARTTRANSFER/ENDTRANSFER test uses the live EP0 state. Do not
            // gate the diagnostic on DSTS.DEVCTRLHLT: a stale halt bit is one
            // of the hypotheses under test, and the probe records the
            // command/armed/event bits even when the core refuses the
            // read-only command.
            POST_RUNSTOP_PROBE_PENDING = false;
            let _ = post_runstop_event_dma_probe();
        }
        // Attach-time one-bit readout (see `DEFERRED_READOUT_KIND`). This runs
        // here, in the polling owner, rather than inside the handoff, so the
        // reading reflects the state the host actually attached to and reset -
        // which is the only point at which questions about the event machinery
        // can be answered at all.
        if DEFERRED_READOUT_KIND != 0 {
            // Trigger on the host actually driving the bus, not on a fixed delay.
            // Measured timing (`354670.0`, host kernel log): the host detects the
            // attach at ~6.2 s (its debounce), logs "new high-speed USB device
            // number N", and fails the descriptor read only 588 ms later - so the
            // whole host-visible window is ~0.6 s, and the old fixed 8 s delay
            // (`POST_RUNSTOP_PROBE_DELAY_SECS`) fired *after* the host had already
            // given up. `DSTS.USBLNKST` cannot be the trigger because it reads
            // "On" from the very start (`305671.0`), but `DSTS.SOFFN` only
            // advances once the host is sending SOFs, i.e. once it is really
            // talking to us - the first SOF arrives within milliseconds of the
            // reset that precedes the SETUP.
            //
            // The first poll after the handoff only *arms* the trigger with the
            // current frame number; the action fires on the next change. Arming
            // inside the branches would be easy to forget, and run `358131.0`
            // showed what happens without it: with a sentinel initial value the
            // condition was true immediately, so the action ran ~1 s after the
            // handoff, before the host had attached.
            const SOFFN_MASK_POLL: u32 = 0x3fff << 3;
            let soffn = unsafe { read(DSTS) } & SOFFN_MASK_POLL;
            let kind_now = unsafe { DEFERRED_READOUT_KIND };
            // Retained-trace marker: the only instrument left that can prove the
            // deferred block ran at all. The host-visible channels are exhausted -
            // CCS pulses use primitives that are inert on this revision
            // (`usb_probe.rs:1331`) and the Run/Stop gates emit nothing while the
            // device is stuck pre-configuration. This lands in `USB_TRACE` in DRAM,
            // which a later boot can read back through `prev_boot_*`. One-shot: this
            // block is evaluated on every poll, so an unguarded marker would flood
            // the ring and erase the events that explain the handoff.
            if !unsafe { DEFR_MARKED } {
                unsafe {
                    DEFR_MARKED = true;
                    trace_event(
                        TRACE_PROBE_WATCHDOG,
                        0x4445_4652, // "DEFR": the deferred block is being evaluated
                        kind_now,
                        u32::from(arch_counter() >= POST_RUNSTOP_PROBE_NOT_BEFORE),
                        0,
                        read(DSTS),
                    );
                }
            }
            // Kind 5 (deliberate re-attach) wants to run *after* the host has
            // given up, so it uses the time delay; the readout kinds wait for the
            // host to actually drive the bus.
            let fire = if kind_now >= 5 {
                unsafe { arch_counter() >= POST_RUNSTOP_PROBE_NOT_BEFORE }
            } else if !DEFERRED_SOFFN_ARMED {
                DEFERRED_SOFFN_ARMED = true;
                LAST_DEFERRED_SOFFN = soffn;
                false
            } else {
                soffn != LAST_DEFERRED_SOFFN
            };
            if fire {
                LAST_DEFERRED_SOFFN = soffn;
                let kind = DEFERRED_READOUT_KIND;
                DEFERRED_READOUT_KIND = 0;
                if kind == 4 {
                    // Fix candidate: run the event-path bus-reset recovery while
                    // the host is still retrying, instead of never (nothing else
                    // can reach it when the core posts no events - `usb.rs:168`).
                    unsafe { restart_control_after_reset() };
                }
                if kind == 5 {
                    // Deliberate re-attach: the host's first attempt has already
                    // failed (it gives up ~600 ms after attaching - entry 171), so
                    // drop the pull-up, fully rebuild EP0, then re-publish. The
                    // observable is the host kernel log: a second attach followed
                    // by a successful descriptor read needs no CCS channel.
                    unsafe {
                        ep0_signal_drop_pullup();
                        readout_keepalive_delay_ms(400);
                        let _ = u0_arm_window_recovery();
                        readout_keepalive_delay_ms(200);
                        ep0_signal_publish_pullup();
                    }
                }
                if kind == 9 {
                    // Positive control: two unconditional cycles.
                    unsafe { gate_cycle_publish(2) };
                }
                if kind == 7 {
                    // Publish `diag_readout_code()` as extra attach lines.
                    unsafe { gate_cycle_publish(diag_readout_code().clamp(1, 6)) };
                }
                if kind == 8 {
                    // One extra attach line iff EP0's SETUP transfer is armed.
                    if unsafe { EP0_SETUP_ARMED } {
                        unsafe { gate_cycle_publish(1) };
                    }
                }
                if kind == 10 {
                    // One extra attach line iff the endpoint epoch was published.
                    // `gate_cycle_publish`, not `ccs_pulse`: this is the point where
                    // the plain `ccs_pulse` stops reaching the host.
                    if unsafe { ENDPOINTS_READY } {
                        unsafe { gate_cycle_publish(1) };
                    }
                }
                // The CCS pulse channel is skipped for the gate kinds: the pull-up
                // drop primitives are inert on this revision, so a pulse would be
                // invisible and only waste the window before the handset collapses.
                if kind <= 4 {
                    // Must use the no-readback gate: the plain `ccs_pulse` waits on
                    // the Run/Stop halt readback, which is exactly what stops pulses
                    // reaching the host once enumeration has started (`336520.0`,
                    // `350817.0`).
                    unsafe { ccs_pulse_no_readback(1_000) };
                    let bit = unsafe {
                        match kind {
                            1 => {
                                // Is the event ring memory non-zero? The ring retains
                                // what the core wrote, so this distinguishes "the core
                                // never posted" from "posted and consumed".
                                let base = ep0_event_dma_base();
                                cache_invalidate(base, 64);
                                let words = core::slice::from_raw_parts(base as *const u32, 16);
                                words.iter().any(|word| *word != 0)
                            }
                            2 => read(DEVTEN) != 0,
                            3 => read(DCFG) & DCFG_DEVADDR_MASK == 0,
                            _ => false,
                        }
                    };
                    if bit {
                        unsafe { ccs_pulse_no_readback(300) };
                    }
                }
            }
        }
        if !event_seen {
            drain_gsi_event_buffers();
            // The core rejects Start Transfer while the link is not ON (this
            // includes the window right after Run/Stop and the host's bus
            // reset), so the initial SETUP arm can fail. Once the link comes
            // up, arm here: the core then immediately delivers any SETUP
            // packet it latched while no TRB was armed.
            let _ = try_arm_setup();
            try_u0_blip();
            return;
        }
        drain_gsi_event_buffers();
        let _ = try_arm_setup();
        try_u0_blip();
    }
}

/// Service only the DWC3 event ring from the periodic timer interrupt.
///
/// Android-init enters user space through `launchd::run()` and does not return
/// to the boot loop that normally calls `poll()`.  The controller SPI should
/// deliver the same work through `aarch64_exception_irq`, but keeping this
/// narrow timer fallback makes EP0 progress independent of a platform IRQ
/// delivery quirk.  Do not run Type-C, power, or SMMU work here.  The one
/// optional blip is a queued Run/Stop differential whose U0 guard is kept
/// here so Android-init cannot miss it after the short initial arm window.
pub fn poll_from_timer_irq() {
    unsafe {
        if !EARLY_HANDOFF_ACTIVE || mmio_quiet_active() {
            return;
        }
        link_on_sample();
        service_usb2_runtime_power_keepalive();
        let event_seen = poll_ep0_event_ring();
        if !event_seen {
            let _ = poll_ep0_trb_completion_fallback();
        }
        let _ = try_arm_setup();
        // Android-init may reach U0 after the short post-Run/Stop arm
        // window. Keep the queued source-backed Run/Stop differential alive
        // through that later timer poll instead of silently dropping it when
        // the first 100 ms window has elapsed.
        try_u0_blip();
    }
}

/// Consume Qualcomm GSI event buffers. Android reserves event buffers 1..3 for
/// the data path; decode each record as an event word and retain it in the
/// same trace used by EP0. Unknown GSI event encodings are still acknowledged
/// without being mistaken for control transfers.
unsafe fn drain_gsi_event_buffers() {
    let configured = super::super::platform::bramble::usb_resources()
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
