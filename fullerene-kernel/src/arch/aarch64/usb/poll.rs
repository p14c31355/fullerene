//! Early-boot and timer-driven DWC3 event polling.

use super::*;

/// Poll the DWC3 event ring. This is intentionally cheap enough to run from
/// the early boot loop until the normal interrupt controller owns the device.
pub fn poll() {
    unsafe {
        // Proof that the driver loop ran, written where every crate can read it. There is
        // no other crate-independent way to say "poll() executed": a `static mut` is
        // duplicated by the second `usb/` compilation.
        //
        // NOT latched, deliberately. The first version guarded this with a `static mut
        // bool` and it read false on a second run: `.bss` is not guaranteed to be cleared
        // between `fastboot boot` attempts the way the retained region is not either, and
        // a latch that starts true can never write its marker. Writing every pass instead
        // is safe here because the ring is 256 entries and `prev_boot_poll_ran()` asks
        // "is there a POL1 anywhere in the ring" - a marker repeated every pass survives
        // even a full wrap, and a marker that is never written stays absent.
        // See usb/README.md §3.12.
        trace_marker(TRACE_PROBE_WATCHDOG, 0x504F_4C31); // "POL1"
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
