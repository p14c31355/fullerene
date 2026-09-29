//! USB2 initialization and Fastboot handoff entry paths.

use super::*;

/// Take over the USB controller without resetting the PHY or clock branches.
/// Fastboot has already completed that hardware bring-up; resetting those
/// blocks during a `fastboot boot` handoff can remove the Type-C pull-up before
/// the new gadget has a chance to enumerate.
pub fn init_usb2_handoff() -> bool {
    screen_mark(0); // handoff entered - first screen timestamp of the run
    // pullup_mark(2) was here and produced no host edges (2026-09-29 09:10): at handoff entry the
    // DWC3 does not yet accept a run/stop. Removed so its 600 ms does not perturb the run; the
    // negative result is recorded in usb/README.md and the boundary-state ledger.
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
        let performance_vote = if cfg!(fullerene_aarch64_usb_gadget_handoff_core_hs_clock) {
            log_puts("usb: selecting Android HS core-clock performance state\n");
            super::super::platform::bramble::UsbBusVote::Svs
        } else {
            super::super::platform::bramble::UsbBusVote::Nominal
        };
        let performance = super::super::platform::bramble::usb_performance_state(performance_vote);
        if !super::super::platform::bramble::apply_usb_power(true, false) {
            log_puts("usb: RPMh USB PHY regulator vote unavailable; continuing\n");
        }
        let _ = super::super::platform::bramble::enable_usb30_gdsc();
        let _ = super::super::platform::bramble::apply_usb_performance(performance.vote);
        let _ = super::super::platform::bramble::usb_bus_vectors(performance.vote);
        // Latch the core id now, while the aperture answers, so no readout ever has to.
        latch_snpsid();
    }

    #[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
    {
        // A SuperSpeed direct handoff must enter the QMP/DWC3 USB3 path
        // directly.  Falling through to the generic USB2 attempt first can
        // return true after publishing only the HS pull-up, which prevents
        // `init_usb2_gadget_handoff()` from ever reaching its SuperSpeed
        // initialization and makes an SS run look like a PHY failure.
        return init_usb2_gadget_handoff();
    }

    #[cfg(all(
        fullerene_aarch64_usb_gadget_handoff_probe,
        not(fullerene_aarch64_usb_gadget_handoff_super_speed)
    ))]
    {
        if init_usb2_gadget_handoff() {
            return true;
        }
        // Attribution control: do not let another initializer turn a failed
        // direct EP0 path into an indistinguishable physical attach. Unlike
        // SINGLE_ATTEMPT, this policy does not skip Run/Stop readback.
        if option_env!("FULLERENE_AARCH64_USB_DIRECT_ONLY") == Some("1") {
            return false;
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1") {
            // Keep the post-link DMA diagnostic one-shot. Falling through to
            // the SuperSpeed fallback would publish a second controller path
            // and make a host-visible attach ambiguous.
            return false;
        }
    }

    // A QMP phase-stop run is a same-boot reachability measurement. If the
    // selected marker was not reached, do not fall through to the ordinary
    // USB2 path: that path would create the same HS attach as a reached-phase
    // fallback and make the phase-8 result ambiguous.
    if qmp_phase_probe_requested() {
        return false;
    }

    if super_speed::init_with_super_speed(false, true, false) {
        return true;
    }

    // A gate readout run must inspect the direct path's OWN failure state
    // while the ~17 s watchdog is still silent; the fallback's resets would
    // both consume the remaining window and overwrite the core state under
    // test. The caller's single-attempt limit then keeps the observation
    // window short enough to beat the bite.
    if option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") == Some("1") {
        return false;
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

        // This probe also resets the controller, so keep the historical
        // Fullerene controller-timing experiment at this boundary unless the
        // source-confirmed preserve-state A/B disables it.
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
        mark_g2w_site(1006);
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
/// Bare-pullup bisection checkpoint selector: 1 = PHY/session votes +
/// USB2 PHY wake only, 2 = +UTMI-as-PIPE clock mux, 3 = +GCTL/DCFG/DALEPENA,
/// absent = the full sequence through the Run/Stop start. The bare probe
/// parks after the checkpoint, so the host-visible attach time is the
/// cumulative cost of the executed prefix: it separates the
/// ABL-to-kernel-entry latency from the per-step controller cost.
fn bare_pullup_stop_after() -> Option<u32> {
    option_env!("FULLERENE_USB_BARE_PULLUP_STOP_AFTER").and_then(|value| value.parse::<u32>().ok())
}

pub(super) unsafe fn init_usb2_bare_pullup_handoff_inner(connect: bool) -> bool {
    unsafe {
        // Bisection: a mark at the top of this body was SILENT 2/2 (2026-09-29 ~11:55), so the
        // transition is later in the function. The next mark is in the middle of the glue writes.
        // Match dwc3_qcom_vbus_override_enable(). qpr1 writes the
        // SuperSpeed lane power-present vote only when maximum_speed is at
        // least USB_SPEED_SUPER; the DCFG_FULLSPEED A/B is the corresponding
        // no-SS branch, so do not publish an SS-side session here.
        if !cfg!(any(
            fullerene_aarch64_usb_dcfg_fullspeed,
            fullerene_aarch64_usb_dcfg_lowspeed,
            fullerene_aarch64_usb_no_ss_vbus
        )) {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        }
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // Bisection: a mark immediately before enable_power_events() was SILENT 3/3 (2026-09-29
        // ~12:10), as was the top of the body. The transition is later in the function.
        enable_power_events();
        // Fastboot may leave the core in the USB2 suspended state when it
        // tears down its gadget just before jumping to the temporary image.
        // Waking the UTMI block is still below the EP0/DMA boundary and is
        // required before DCTL.Run/Stop can produce a new pull-up.
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1007);
        write(GUSB2PHYCFG0, usb2);
        let _ = read(GUSB2PHYCFG0);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);
        let _ = read(GUSB3PIPECTL0);
        let general = read_qscratch(QSCRATCH_GENERAL_CFG);
        write_qscratch(
            QSCRATCH_GENERAL_CFG,
            general | QSCRATCH_GENERAL_CFG_XHCI_REV,
        );
        // Bisection checkpoint 1: everything above is the PHY/session side
        // (Qualcomm glue votes + USB2 PHY wake). Stopping here tests whether
        // the Fastboot-inherited controller state already advertises the
        // pull-up once the PHY votes land; an early attach then measures the
        // ABL-to-first-MMIO latency alone.
        if let Some(stop) = bare_pullup_stop_after() {
            if stop == 1 {
                return true;
            }
        }
        // The bare path intentionally skips DWC3 reset, but it still needs
        // the Qualcomm glue's UTMI-as-PIPE clock selection when the Fastboot
        // session did not leave that mux configured for the temporary image.
        select_utmi_pipe_clock();
        // Bisection checkpoint 2: + the UTMI-as-PIPE clock mux (the 2x100 us
        // clock-source transitions are the largest fixed cost so far).
        if let Some(stop) = bare_pullup_stop_after() {
            if stop == 2 {
                return true;
            }
        }

        // Bisection: a mark immediately before the GCTL device-mode write was SILENT 3/3
        // (2026-09-29 ~12:25). The transition is later in the function.
        let gctl = read(GCTL);
        write(
            GCTL,
            (gctl & !GCTL_PRTCAPDIR_MASK) | GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG,
        );
        let _ = read(GCTL);
        // Bisection: a mark immediately after the GCTL PRTCAPDIR -> DEVICE write was SILENT 3/3
        // (2026-09-29 ~13:10), so the device-mode write is not the transition after all. Window is
        // now (:6971, :6992).
        configure_dwc3_global_control();
        // Final split (2026-09-29): between configure_dwc3_global_control() and the DCFG speed write
        // - the last two candidates. Silent here means the DCFG write is the transition; audible
        // means configure_dwc3_global_control() is. Gated on !connect for the :7167 caller.
        if !connect {
            pullup_mark(2);
        }
        write(DCFG, DCFG_HIGHSPEED);
        let _ = read(DCFG);
        // Bisection: a mark immediately after the DCFG speed write was AUDIBLE 3/3 (2026-09-29
        // ~13:25). Window is now (:6971, :6976) - configure_dwc3_global_control() and the DCFG
        // write, five lines. The mark moves between them for the final split.

        // Linux disables endpoint advertising before stopping the device
        // controller. In the ABL Stop() differential, the controller remains
        // live, so leave its endpoint advertisement untouched until the
        // later handoff sequence explicitly clears it.
        if connect || !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_runstop) {
            write(DALEPENA, if connect { 0b11 } else { 0 });
            let _ = read(DALEPENA);
        }
        // Bisection checkpoint 3: + GCTL/DCFG/DALEPENA, before the VBUS
        // re-assert and the Run/Stop start. Stopping here isolates the
        // DCTL.Run/Stop wait as the only remaining cost between the last
        // plain MMIO and the host-visible attach.
        if let Some(stop) = bare_pullup_stop_after() {
            if stop == 3 {
                return true;
            }
        }

        // Bisection: a mark immediately before this glue re-assert was AUDIBLE 3/3 (2026-09-29
        // ~12:55). Window is now (:6963, :6992), and it contains the GCTL device-mode write.
        // Qualcomm's glue reasserts the VBUS override immediately before
        // enabling RUN_STOP so a stale Fastboot session cannot suppress the
        // connect-done transition.
        if !cfg!(any(
            fullerene_aarch64_usb_dcfg_fullspeed,
            fullerene_aarch64_usb_dcfg_lowspeed,
            fullerene_aarch64_usb_no_ss_vbus
        )) {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        }
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        // Bisection: a mark immediately before this block was AUDIBLE 3/3 (2026-09-29 ~12:40), so
        // the transition is earlier in the function - window is now (:6963, :7008).
        // A gadget handoff uses the same proven PHY/session preparation but
        // keeps Run/Stop clear until its event ring and EP0 commands are
        // ready. The standalone bare probe requests the pull-up immediately.
        if connect {
            // The bare probe intentionally omits endpoint setup, but it still
            // uses the same PHY-safe Run/Stop boundary as Linux.
            run_stop_device(true)
        } else if cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_runstop) {
            log_puts("usb gadget handoff: preserving Fastboot Run/Stop state\n");
            true
        } else {
            run_stop_device(false)
        }
    }
}

pub fn init_usb2_bare_pullup_handoff() -> bool {
    unsafe { init_usb2_bare_pullup_handoff_inner(true) }
}

/// TEMP(flow-map): host-visible code-point blip for the `always` self-test.
/// Toggles the physical pull-up for a short window so the host kernel log
/// timestamps the exact code point that executed. Parks and reset channels
/// are unusable while the unidentified ~17 s biter overrides them; the blip
/// completes in ~0.2 s and restores the previous Run/Stop state.
#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub(super) unsafe fn gate_flow_blip() {
    if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") != Some("always") {
        return;
    }
    unsafe {
        let running = read(DCTL) & DCTL_RUN_STOP != 0;
        if running {
            write_dctl_safe(read(DCTL) & !DCTL_RUN_STOP);
        } else {
            let dctl = run_stop_value(read(DCTL), read(GSNPSID));
            write(DCTL, dctl | DCTL_RUN_STOP);
        }
        crate::timer::delay_ms(200);
        if running {
            let dctl = run_stop_value(read(DCTL), read(GSNPSID));
            write(DCTL, dctl | DCTL_RUN_STOP);
        } else {
            write_dctl_safe(read(DCTL) & !DCTL_RUN_STOP);
        }
    }
}

#[inline(always)]
pub(super) unsafe fn set_direct_usb2_vbus_override() {
    // qpr1's dwc3_override_vbus_status(true) updates only
    // HS_PHY_CTRL.UTMI_OTG_VBUS_VALID (bit 20).  The old direct handoff
    // also ORed in SW_SESSVLD_SEL (bit 28), which is an inherited-session
    // override rather than part of the qpr1 peripheral-start contract.
    // When the source-vbus-only A/B is selected, clear that inherited bit
    // explicitly: qscratch_set() is an OR helper and cannot remove it.
    if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_vbus_only) {
        let mut value = read_qscratch(QSCRATCH_HS_PHY_CTRL);
        value &= !(1 << 28);
        value |= 1 << 20;
        write_qscratch(QSCRATCH_HS_PHY_CTRL, value);
        let _ = read_qscratch(QSCRATCH_HS_PHY_CTRL);
    } else {
        qscratch_set(QSCRATCH_HS_PHY_CTRL, (1 << 20) | (1 << 28));
    }
}

/// Reuse the physical USB2 handoff, then add the minimum DWC3 gadget state
/// needed to answer USB control transfers. The PHY and Qualcomm session
/// remain untouched; this is the early Bramble handoff path and is also
/// usable as a standalone probe.
pub fn init_usb2_gadget_handoff() -> bool {
    unsafe {
        // OUTER CALIBRATION, deliberately placed *outside* the handoff entry point.
        // Every other `usb2-live-*` readout lives inside
        // `init_usb2_gadget_reuse_fastboot_ep0`, so if that function is never called
        // its readout cannot run - which makes a zero-pulse reading there circular
        // evidence. This one sits in the caller, which must run for any handoff at
        // all, and issues ONE unconditional pulse.
        //
        //   2 attach lines => the channel works from here, so a zero-pulse reading
        //                     inside the entry point really does mean "not entered"
        //   1 attach line  => nothing is published even here; the channel is dead on
        //                     this route and every pulse reading this session is void
        if option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT") == Some("usb2-live-outer-calibration")
        {
            // stop_ms = 500 to match `usb2-live-soffn-count` exactly. The SOFFN
            // sequence is the only one on this channel that ever produced extra
            // attach lines, and it used 500; a 300 ms variant produced none, which
            // may simply have been too short for the host to re-log the attach.
            // Settle FIRST - a pulse issued before the wait produces no host line
            // (measured). This is the outermost probe in the handoff, so it answers
            // whether `init_usb2_gadget_handoff` is reached at all, which nothing
            // inside the entry point can answer.
            readout_keepalive_delay_ms(500);
            ccs_pulse(500);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
        // Keep the CLI's --no-core-reset differential effective for the
        // SuperSpeed handoff too.  Before this propagation the SS entry point
        // always passed `reset_core=true`, so run reports claiming to skip
        // CSFTRST were not testing that condition at all.
        return super_speed::init_with_super_speed(
            true,
            !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core),
            false,
        );

        #[cfg(all(
            fullerene_aarch64_usb_gadget_handoff_probe,
            not(fullerene_aarch64_usb_gadget_handoff_super_speed)
        ))]
        return gadget_handoff::init_usb2_gadget_reuse_fastboot_ep0();

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
        mark_g2w_site(1013);
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
            ep0_event_size() as u32,
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

        write(
            DCFG,
            if cfg!(fullerene_aarch64_usb_dcfg_lowspeed) {
                DCFG_LOWSPEED
            } else if cfg!(fullerene_aarch64_usb_dcfg_fullspeed) {
                DCFG_FULLSPEED
            } else {
                DCFG_HIGHSPEED
            },
        );
        configure_gadget_start_defaults();
        write(DALEPENA, 0);
        write(DEVTEN, direct_gadget_devten());

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
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct)
            || cfg!(fullerene_aarch64_usb_probe_irq_controller)
        {
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
        configure_gadget_speed(cfg!(fullerene_aarch64_usb_dcfg_superspeed));
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        qscratch_set(
            QSCRATCH_HS_PHY_CTRL,
            (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
        );
        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_defaults_at_runstop)]
        {
            // Linux's __dwc3_gadget_start() applies these controller-wide
            // defaults at the final gadget start boundary. The ordinary
            // handoff applies them before endpoint commands; replay them
            // here as a narrow ordering A/B without rebuilding EP0.
            log_puts("usb gadget handoff: replaying gadget start defaults at Run/Stop\n");
            configure_gadget_start_defaults();
        }
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

/// Host-visible progress beacon for the direct path's reset section: toggle
/// the QSCRATCH session pull-up for 500 ms at each boundary. Every
/// drop/restore pair shows up as one host-side disconnect/re-attach pair in
/// the kernel log, so the LAST beacon visible in the log names exactly how
/// far the code got - including when the code then hangs on a faulting MMIO
/// access, where the watchdog return timing alone cannot localize the stop.
/// Only active in single-attempt (gate readout) runs.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
pub(super) unsafe fn init_beacon() {
    unsafe {
        if option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") != Some("1") {
            return;
        }
        let gate = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE");
        if gate == Some("stall-map") {
            stall_map_beacon();
            return;
        }
        // Cmd-gate runs read their one bit from the return timing and must
        // evaluate inside the pre-bite window; the beacons add ~1 s each to
        // init and pushed the evaluation into the ~17 s watchdog bite.
        if gate.is_some() {
            return;
        }
        ep0_signal_drop_pullup();
        super::super::timer::delay_ms(500);
        ep0_signal_restore_pullup();
    }
}

/// stall-map beacon: one host-visible DWC3 Run/Stop disconnect/re-attach pair.
/// qpr1's DWC3 source uses DCTL.RUN_STOP for gadget pull-up control; it does
/// not define the DWC2 SDIS bit at DCTL bit 0. The beacon is honored only
/// while the core is running with the link ON, so beacons before Run/Stop are
/// silent by design.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
unsafe fn stall_map_beacon() {
    unsafe {
        runstop_blips_link_on(1);
    }
}

/// Put the Apps-SMMU into the verified physical=IOVA state used by the
/// direct Bramble DMA differential. The USB2 probe and the SuperSpeed
/// fallback have separate handoff entry points, so keep this ownership
/// transition in one helper; otherwise `--smmu-disable` can silently apply to
/// only one of them.
pub(super) unsafe fn prepare_smmu_dma_bypass() -> bool {
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
    true
}

/// Reassert the Qualcomm USB30 controller domain without resetting DWC3 or
/// replaying QMP state.  This is kept as one operation so the pre-Run/Stop
/// and post-Run/Stop A/B use exactly the same CX/bus/GDSC/RCG/branch order.
pub(super) unsafe fn reassert_ss_controller_domain() -> bool {
    unsafe {
        let cx_vote = super::super::platform::bramble::apply_usb_cx_vote(
            super::super::platform::bramble::UsbBusVote::Nominal,
        );
        let bus_vote = super::super::platform::bramble::apply_usb_bus_vote(
            super::super::platform::bramble::UsbBusVote::Nominal,
        );
        let gdsc = super::super::platform::bramble::force_enable_usb30_gdsc();
        let sources = super::super::platform::bramble::configure_usb_controller_clocks(
            super::super::platform::bramble::UsbBusVote::Nominal,
        );
        let branches = super::super::platform::bramble::rearm_usb_controller_clock_branches();
        super::super::platform::bramble::apply_usb_pm_qos(
            super::super::platform::bramble::UsbBusVote::Nominal,
        );
        log_hex(
            "usb: SS controller clock reassert mask=",
            u64::from(
                (cx_vote as u8)
                    | ((bus_vote as u8) << 1)
                    | ((gdsc as u8) << 2)
                    | ((sources as u8) << 3)
                    | ((branches as u8) << 4),
            ),
        );
        if !(cx_vote && bus_vote && gdsc && sources && branches) {
            log_puts("usb: SS controller clock reassert incomplete\n");
        }
        crate::timer::delay_us(1_000);
        cx_vote && bus_vote && gdsc && sources && branches
    }
}

/// Keep the USB2 controller domain at the active Android vote while the
/// early image owns a live direct handoff.  This is deliberately narrower
/// than the SuperSpeed reassert path: it does not retune DWC3 clocks, reset
/// the controller, or touch QMP state.  The source-backed contract also
/// re-arms the USB2 controller branches in Android's iface/core/sleep/utmi
/// order, with the HS-PHY reference clock included because the direct
/// handoff preserves Fastboot's PHY ownership.  No endpoint command or TRB
/// mutation is performed here.
#[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_runtime_power_keepalive)]
pub(super) unsafe fn service_usb2_runtime_power_keepalive() {
    unsafe {
        // Only "no Run/Stop has happened yet" is a real precondition: before the
        // handoff there is nothing to keep alive.
        //
        // The previous guard also bailed out when the core was *stopped*
        // (`DCTL.RUN_STOP == 0`) or when `QMP_PHY_READY` was set. That is backwards
        // for a keepalive: a stopped controller is exactly when RPMh is most likely
        // to have collapsed the domain, and the body below only re-votes the rails,
        // the GDSC and the clock branches - the function's own contract says it
        // "does not retune DWC3 clocks, reset the controller, or touch QMP state".
        // So it is safe with the core halted, and with the old guard the keepalive
        // could be skipped for the whole window in which it was needed.
        if RUN_STOP_TICK == 0 {
            return;
        }
        let frequency = arch_counter_frequency();
        if frequency == 0 {
            return;
        }
        let now = arch_counter();
        if USB2_RUNTIME_KEEPALIVE_NEXT != 0 && now < USB2_RUNTIME_KEEPALIVE_NEXT {
            return;
        }

        let votes = super::super::platform::bramble::refresh_usb_domain_votes(
            super::super::platform::bramble::UsbBusVote::Nominal,
            false,
        );
        let gdsc = super::super::platform::bramble::force_enable_usb30_gdsc();
        let branches = super::super::platform::bramble::rearm_usb2_android_clock_branches();
        let utmi = super::super::platform::bramble::enable_usb2_utmi_clock();
        let ref_clock = super::super::platform::bramble::enable_usb_hs_phy_ref_clock();
        super::super::platform::bramble::apply_usb_pm_qos(
            super::super::platform::bramble::UsbBusVote::Nominal,
        );
        USB2_RUNTIME_KEEPALIVE_COUNT = USB2_RUNTIME_KEEPALIVE_COUNT.wrapping_add(1);
        // Retain a sparse proof of the active-domain refresh without filling
        // the ring during a long stable session.  The first refresh and each
        // 32nd refresh carry the outcome bits; the host can correlate the
        // marker with the descriptor timeout window in Tshark.
        if USB2_RUNTIME_KEEPALIVE_COUNT == 1 || USB2_RUNTIME_KEEPALIVE_COUNT & 31 == 0 {
            trace_event(
                TRACE_USB_DEVICE_ERROR,
                0x4b41_4c56, // "KALV"
                votes as u32,
                gdsc as u32,
                ref_clock as u32,
                (branches as u32) | ((utmi as u32) << 1) | (USB2_RUNTIME_KEEPALIVE_COUNT << 8),
            );
        }
        USB2_RUNTIME_KEEPALIVE_NEXT = now.saturating_add(frequency.saturating_div(2).max(1));
    }
}

#[cfg(not(fullerene_aarch64_usb_gadget_handoff_usb2_runtime_power_keepalive))]
#[inline]
pub(super) unsafe fn service_usb2_runtime_power_keepalive() {}

/// Consume the DWC3 EP0 event ring without touching platform power, Type-C,
/// or SMMU state. Linux has an IRQ window immediately after arming the first
/// SETUP TRB; the early handoff uses this bounded synchronous equivalent before
/// the normal polling path owns the controller.
///
/// DWC3 GEVNTCOUNT is a write-to-consume register. Android msm's
/// `dwc3_event_buffers_setup()` initializes the buffer by writing zero to the
/// complete register, including when the previous Fastboot owner left a stale
/// event-buffer state behind. Factory ABL instead preserves the register's
/// EHB bit during that initial publication; the ABL event-consume A/B carries
/// that source-derived difference together with its per-event ACK order.
pub(super) unsafe fn acknowledge_ep0_event_count() {
    unsafe {
        let value = if cfg!(fullerene_aarch64_usb_abl_event_consume) {
            read(GEVNTCOUNT0) & GEVNTCOUNT_EHB
        } else {
            0
        };
        write(GEVNTCOUNT0, value);
        core::arch::asm!("dsb sy", options(nostack));
    }
}

/// Re-apply the EP0 event-buffer ownership at the Run/Stop boundary.
///
/// Android's msm-4.19 `dwc3_gadget_run_stop(true)` performs event-buffer
/// setup immediately before restarting the gadget. The direct handoff
/// normally publishes the ring earlier, after stopping Fastboot. Keep this
/// diagnostic deliberately narrow: it re-writes only the EP0 event address,
/// size, and consumed count, without clearing endpoint state or touching the
/// physical pull-up.
#[cfg(any(
    fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop,
    fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop,
    fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop,
    fullerene_aarch64_usb_usb2_core_reset_at_runstop
))]
pub(super) unsafe fn republish_ep0_event_ring_at_runstop() {
    let event_address = unsafe { ep0_event_address() };
    let event_size = unsafe { ep0_event_size() };
    unsafe {
        // The controller is still halted here, so invalidate CPU cache lines
        // before handing the cacheable ring back to DWC3. Cleaning stale CPU
        // lines could overwrite an event that the controller posted earlier.
        cache_invalidate(ep0_event_dma_base(), event_size);
        write(GEVNTADRLO0, event_address as u32);
        write(GEVNTADRHI0, (event_address >> 32) as u32);
        write(GEVNTSIZ0, event_size as u32);
        acknowledge_ep0_event_count();
        EVENT_OFFSET = 0;
        core::arch::asm!("dsb sy", options(nostack));
        trace_event(
            TRACE_EVENT_RING_READY,
            0x5253_544f, // "RSTO"
            event_address as u32,
            (event_address >> 32) as u32,
            event_size as u32,
            read(DSTS),
        );
    }
}

/// Re-run the Android msm `__dwc3_gadget_start()` EP0 portion immediately
/// before Run/Stop. Android's `dwc3_gadget_run_stop(true)` uses this restart
/// boundary after event-buffer setup, while the direct handoff normally
/// configured EP0 only once after taking over from Fastboot.
///
/// This diagnostic keeps the known direct-path platform state and reapplies
/// the selected speed after any device-core soft reset. It
/// only repeats the DWC3 gadget-start command sequence: open a resource
/// window, allocate endpoint resources, initialize both physical EP0
/// directions, and arm the initial SETUP TRB. The caller deliberately
/// continues to the final Run/Stop transition when this best-effort restart
/// is incomplete, matching Android's void `dwc3_gadget_restart()` contract.
#[cfg(any(
    fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop,
    fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop,
    fullerene_aarch64_usb_usb2_core_reset_at_runstop
))]
pub(super) unsafe fn restart_gadget_at_runstop(super_speed: bool) -> bool {
    unsafe {
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop,
            fullerene_aarch64_usb_usb2_core_reset_at_runstop
        ))]
        {
            // A device-core soft reset can restore GCTL.PRTCAPDIR to its
            // reset value. Android selects DEVICE before the reset, but this
            // helper runs after it, so repeat the source-order device-mode
            // tail before publishing gadget state.
            configure_dwc3_device_mode();
            if super_speed {
                configure_usb31_lfps_exit_timer();
            }
        }
        republish_ep0_event_ring_at_runstop();
        // The upstream `dwc3_gadget_run_stop(true)` calls
        // `__dwc3_gadget_start()` and then rewrites DCFG.SPEED immediately
        // before asserting Run/Stop. Keep the existing restart A/B's earlier
        // placement by default, but move it after the full gadget-start
        // sequence in the source-order control.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_gadget_start_only_at_runstop) {
            configure_gadget_speed(super_speed);
        }
        configure_gadget_start_defaults();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_defaults_at_runstop)]
        {
            // On DWC_usb31 qpr1's __dwc3_gadget_start() also reapplies the
            // revision-gated GUCTL/GSBUSCFG1 gadget-start deltas.  When the
            // direct USB2 A/B performs device-core reset at this boundary,
            // restore that source-owned state in the same reset-after-start
            // epoch instead of carrying the pre-reset register values into
            // the final endpoint commands.
            apply_usb31_gadget_reference_deltas();
        }
        write(DALEPENA, 0);
        ENDPOINTS_READY = false;
        EP0_SETUP_ARMED = false;
        PENDING_SETUP_ARM = false;
        EP0_RESOURCE_INDEX = [0; 2];
        EP0_STATE = Ep0State::Setup;

        if !send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0) {
            trace_event(TRACE_SETUP_QUEUED, 0x5253_4641, 0, 0, 0, read(DSTS)); // "RSFA"
            return false;
        }
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource) {
            // qpr1's start_config() assigns one resource to every endpoint
            // advertised by GHWPARAMS3 before either EP0 SETEPCONFIG command.
            for endpoint in 0..qpr1_endpoint_count() {
                if !set_transfer_resource(endpoint) {
                    trace_event(
                        TRACE_SETUP_QUEUED,
                        0x5253_5253, // "RSRS"
                        endpoint as u32,
                        0,
                        0,
                        read(DSTS),
                    );
                    return false;
                }
            }
        }

        // Android starts with the SuperSpeed EP0 descriptor and changes it
        // to 64 bytes from Connect Done for a USB2 link. When this helper is
        // used for the USB2-only handoff, Connect Done is the first host
        // boundary and may not reach the software event consumer before the
        // first SETUP. Keep the existing A/B flag consistent across both
        // endpoint-construction epochs: the explicit 512-byte experiment
        // retains the source initial state, while the baseline USB2 restart
        // publishes the post-Connect-Done 64-byte state directly.
        let restart_ep0_packet_size = if cfg!(fullerene_aarch64_usb_ep0_initial_512) {
            INITIAL_EP0_MAX_PACKET_SIZE
        } else {
            64
        };
        let epcfg0 =
            configure_endpoint_config(0, restart_ep0_packet_size, DEPCFG_EP_TYPE_CONTROL, false, 0);
        if !epcfg0 {
            trace_event(
                TRACE_SETUP_QUEUED,
                0x52534546, // "RSEF"
                epcfg0 as u32,
                0,
                0,
                read(DSTS),
            );
            return false;
        }
        // qpr1's __dwc3_gadget_ep_enable() publishes each endpoint's
        // DALEPENA bit immediately after its SETEPCONFIG command. Keep the
        // same ownership boundary instead of exposing both directions only
        // after EP0-IN configuration has completed.
        write(DALEPENA, read(DALEPENA) | (1 << 0));
        trace::live_dalepena_config(0, read(DALEPENA));
        let epcfg1 =
            configure_endpoint_config(1, restart_ep0_packet_size, DEPCFG_EP_TYPE_CONTROL, false, 0);
        if !epcfg1 {
            trace_event(
                TRACE_SETUP_QUEUED,
                0x52534546, // "RSEF"
                epcfg0 as u32,
                epcfg1 as u32,
                0,
                read(DSTS),
            );
            return false;
        }
        write(DALEPENA, read(DALEPENA) | (1 << 1));
        trace::live_dalepena_config(1, read(DALEPENA));
        ENDPOINTS_READY = true;
        let _ = udc_mut().configure_endpoint(0, restart_ep0_packet_size as u16, false);
        let _ = udc_mut().configure_endpoint(1, restart_ep0_packet_size as u16, false);
        // The canonical qpr1 path arms CONTROL_SETUP before Run/Stop. The
        // Bramble timing A/B can deliberately defer only this STARTTRANSFER
        // while retaining the Android endpoint/resource restart; the common
        // post-Run/Stop arm window then uses the same U0-guarded retry as the
        // normal direct handoff.
        let defer_setup = cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect);
        let armed = if defer_setup {
            PENDING_SETUP_ARM = true;
            false
        } else {
            start_setup()
        };
        trace_event(
            TRACE_SETUP_QUEUED,
            0x52534152, // "RSAR"
            armed as u32,
            0,
            8,
            read(DSTS),
        );
        if armed {
            poll_ep0_event_ring();
        }
        // `__dwc3_gadget_start()` enables the controller event mask after
        // arming EP0. The direct path polls the same ring, but a core reset at
        // this boundary still clears DEVTEN, so publish the selected qpr1 or
        // broad controller-event set before Run/Stop.
        write(DEVTEN, direct_gadget_devten());
        if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_gadget_start_only_at_runstop,
            fullerene_aarch64_usb_gadget_handoff_gadget_speed_after_restart
        )) {
            configure_gadget_speed(super_speed);
        }
        armed
    }
}
