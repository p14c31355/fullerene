//! Full DWC3 gadget initialization path for USB2 and SuperSpeed.

use super::*;

pub(super) fn init_with_super_speed(super_speed: bool, reset_core: bool, reset_platform: bool) -> bool {
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
        if !prepare_smmu_dma_bypass() {
            return false;
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
        if !super::super::platform::bramble::usb_power_contract_valid(super_speed) {
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
        let performance = super::super::platform::bramble::usb_performance_state(
            super::super::platform::bramble::UsbBusVote::Nominal,
        );
        let bus_vectors = super::super::platform::bramble::usb_bus_vectors(performance.vote);
        log_hex("usb: nominal core clock=", performance.core_rate_hz as u64);
        log_hex(
            "usb: PM QoS latency us=",
            performance.pm_qos_latency_us as u64,
        );
        log_hex("usb: interconnect paths=", bus_vectors.len() as u64);
        // The lito DT wires the USB domain's two RPMh votes outside every USB
        // node: the `gcc` block's `vdd_cx-supply` (cx.lvl, DT init RETENTION)
        // and the glue node's `qcom,msm-bus,vectors-KBps`. Fastboot's votes
        // die with its exit, so both are re-asserted here for the handoff as
        // well — this is the missing vote behind the ~5-8 s post-attach
        // collapse of the USB clock branch. Unlike the clock retune below,
        // these requests do not disturb a live Fastboot clock domain: they
        // only raise resources the DT already requires for this controller.
        let _ = super::super::platform::bramble::apply_usb_cx_vote(performance.vote);
        let _ = super::super::platform::bramble::apply_usb_bus_vote(performance.vote);
        // Select the RCG source before enabling its branch clocks and before
        // publishing the corresponding interconnect vote.  Handoff mode
        // intentionally skips this write because Fastboot owns a live clock
        // domain that must not be retuned underneath the controller.
        if reset_platform {
            if !super::super::platform::bramble::apply_usb_power(true, super_speed) {
                log_puts("usb: RPMh USB PHY regulator contract unavailable\n");
                return false;
            }
            if !super::super::platform::bramble::enable_usb30_gdsc() {
                // Some Pixel bootloaders keep the GDSC under secure/RPMh
                // ownership. Treat this as a non-fatal ownership warning.
                log_puts("usb: USB3 GDSC PWR_ON not observable; preserving vote\n");
            }
            if !super::super::platform::bramble::apply_usb_performance(performance.vote) {
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
            let _ = super::super::platform::bramble::enable_usb_clock_branches();
            // Android's PHY probe enables the RPMh-backed `ref_clk_src`
            // before releasing the PHY/controller reset sequence. This fixed
            // 19.2 MHz clock is separate from the GCC mock-UTMI branch.
            let _ = super::super::platform::bramble::enable_usb_hs_phy_ref_clock();
            let _ = super::super::platform::bramble::reset_usb_blocks(super_speed);

            init_hsphy();
            if super_speed { init_qmp_phy() } else { false }
        } else {
            cfg!(fullerene_aarch64_usb_gadget_handoff_ss_preserve_phy_state)
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
        RAMCLK_CAPTURE_VALID = true;
        trace_event(
            TRACE_DWC3_REVISION_QUIRK,
            0x5243_4150,
            RAMCLK_CAPTURE,
            0,
            0,
            0,
        );
        // E0: session bits + GCTL device mode + RAMCLK captured, before the
        // bare-pullup inner (the host attach point is at or before this).
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
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

        // B1: GCTL device-mode + RAMCLK capture done, before the bare inner.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        // Use the same pre-reset ownership boundary as the proven Bramble
        // gadget probe.  The helper wakes UTMI, selects the USB2 clock path,
        // clears stale endpoint advertising, and stops the old Fastboot
        // session before CSFTRST.  Repeating those writes inline here had
        // drifted from the working handoff sequence and could leave the
        // controller reset while its PHY/session state was still suspended.
        if !reset_platform && !super_speed {
            let _ = init_usb2_bare_pullup_handoff_inner(false);
        }

        // B2: bare inner done (UTMI awake, GCTL device mode, old Fastboot
        // session stopped and the core halted) - just before core_soft_reset.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(1) {
            return true;
        }

        // ABL's public Stop() path can leave the Fastboot controller running
        // while ownership is handed to the next gadget.  The ordinary
        // SuperSpeed handoff stops it before rebuilding endpoint state, but
        // that stop itself may tear down the already-trained SS link.  Keep
        // the existing preserve-runstop flag as a narrow SS A/B: skip only
        // this old-session stop, retain all later endpoint/DMA diagnostics,
        // and let the final Run/Stop write provide the host-visible result.
        if !reset_core
            && !(super_speed && cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_runstop))
            && !stop_running_device()
        {
            return false;
        }

        // Linux initializes and resets the external USB2 PHY before issuing
        // DWC3 CSFTRST.  Fastboot normally leaves the PHY live, so retain
        // the existing post-reset initialization as the default and expose
        // the Linux ordering as a bounded handoff A/B.
        #[cfg(fullerene_aarch64_usb_hsphy_before_reset)]
        if reset_core && !super_speed && !reset_platform {
            let _ = super::super::platform::bramble::pulse_usb2_phy_reset();
            // Keep the qpr1 pre-reset ordering A/B source-faithful as well:
            // msm_hsphy_init() runs before DWC3 CSFTRST, so selecting the
            // source-exact register sequence here must not silently fall back
            // to the legacy helper used by the older ordering test.
            if cfg!(fullerene_aarch64_usb_gadget_handoff_hsphy_source_exact) {
                init_hsphy_source_exact();
            } else {
                init_hsphy();
            }
        }

        // Android's dwc3_core_soft_reset() initializes the external PHY before
        // issuing DCTL.CSFTRST.  Keep the SuperSpeed handoff at the same
        // ownership boundary: a QMP init after CSFTRST leaves DWC3 and the
        // combo PHY observing different reset epochs, and on Bramble that can
        // turn into either a watchdog or a host-visible USB2-only fallback.
        // The no-core variant has already stopped the old controller above;
        // the normal handoff needs the same stop before moving QMP first.
        if super_speed && !reset_platform {
            if reset_core && !stop_running_device() {
                return false;
            }
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_preserve_phy_state)]
            if cfg!(fullerene_aarch64_usb_gadget_handoff_ss_preserve_phy_state) {
                // Fastboot already proved that this exact cable/port/PHY
                // combination can train at SuperSpeed. Do not reset the
                // external QMP blocks or overwrite their trained state;
                // retain only the DWC3-side handoff below.
                trace_marker(TRACE_PROBE_WATCHDOG, 0x5150_5245); // "QPRE"
                QMP_PHY_READY = true;
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_android_dbm_reset)]
                {
                    // qpr1's dwc3_otg_start_peripheral() invokes
                    // dwc3_msm_block_reset(false) even when the external PHY
                    // is already trained. That call does not assert the
                    // DWC3 core reset; it only resets/enables Qualcomm DBM
                    // immediately before peripheral-mode startup. The
                    // preserve-PHY path previously skipped this source-backed
                    // controller ownership boundary because the selector was
                    // nested only in the QMP reinitialization branch.
                    if !super::super::platform::bramble::android_dbm_reset_and_enable() {
                        log_puts("usb: Android DBM reset/enable failed\n");
                        return false;
                    }
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_core_clocks)]
                {
                    // The preserve-phy branch skips the normal post-QMP
                    // reassertion block below. Keep the controller-domain A/B
                    // available here too: it reasserts only CX/Bus/GDSC/RCG/
                    // branch ownership and leaves the trained QMP untouched.
                    let _ = reassert_ss_controller_domain();
                }
            } else {
                unreachable!();
            }
            #[cfg(not(fullerene_aarch64_usb_gadget_handoff_ss_preserve_phy_state))]
            {
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_qmp_notify_disconnect)]
                {
                    // Qualcomm's stop-peripheral path notifies the SS PHY before
                    // the next PHY reset/init epoch. This opt-in isolates that
                    // source-confirmed PCS POWER_DOWN_CONTROL=0 write after the
                    // old DWC3 session has halted.
                    phy::qmp_notify_disconnect();
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_clear_vbus_override_before_qmp)]
                {
                    // Qualcomm's dwc3_override_vbus_status(false) clears the
                    // session/VBUS overrides after PHY disconnect. Keep this
                    // A/B limited to the three source-confirmed QSCRATCH bits.
                    let ss = read_qscratch(QSCRATCH_SS_PHY_CTRL);
                    write_qscratch(QSCRATCH_SS_PHY_CTRL, ss & !(1 << 24));
                    let hs = read_qscratch(QSCRATCH_HS_PHY_CTRL);
                    write_qscratch(QSCRATCH_HS_PHY_CTRL, hs & !((1 << 20) | (1 << 28)));
                    let _ = read_qscratch(QSCRATCH_SS_PHY_CTRL);
                    let _ = read_qscratch(QSCRATCH_HS_PHY_CTRL);
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_clear_usb3_susphy_before_qmp)]
                {
                    // Qualcomm's peripheral-stop path wakes the USB3 PHY after
                    // the PHY disconnect and VBUS/session override clear. Keep
                    // this A/B to the source-confirmed SUSPHY clear only.
                    let usb3 = read(GUSB3PIPECTL0) & !GUSB3PIPECTL_SUSPHY;
                    write(GUSB3PIPECTL0, usb3);
                    let _ = read(GUSB3PIPECTL0);
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reinit_hs_phy)]
                {
                    // `dwc3_core_soft_reset()` resets and initializes the legacy
                    // USB2 PHY before it resets/initializes the USB3 PHY. The
                    // no-core path previously reproduced only the QMP half;
                    // this opt-in restores the source-confirmed Bramble
                    // `dwc3_phy_setup()` SUSPHY writes before the legacy PHY
                    // boundary, then the two legacy reset calls represented by
                    // `usb_phy_reset(usb2_phy)` and the reset inside
                    // `msm_hsphy_init()`.
                    let mut usb3 = read(GUSB3PIPECTL0);
                    usb3 |= GUSB3PIPECTL_SUSPHY;
                    write(GUSB3PIPECTL0, usb3);
                    let _ = read(GUSB3PIPECTL0);
                    let mut usb2 = read(GUSB2PHYCFG0);
                    usb2 |= GUSB2PHYCFG_SUSPHY;
                    mark_g2w_site(1014);
                    write(GUSB2PHYCFG0, usb2);
                    let _ = read(GUSB2PHYCFG0);
                    if !super::super::platform::bramble::pulse_usb2_phy_reset() {
                        log_puts("usb: Fastboot HS PHY pre-init reset failed\n");
                        return false;
                    }
                    // `msm_hsphy_enable_power(true)` +
                    // `msm_hsphy_enable_clocks(true)` + `msm_hsphy_reset()` +
                    // `msm_hsphy_init()` boundary before QMP reset/init.
                    // The Android driver returns early when its DT-provided EUD
                    // status resource is asserted. Preserve that ownership rule
                    // after the preceding official usb_phy_reset() call.
                    if super::super::platform::bramble::usb_hs_phy_eud_enabled() {
                        log_puts("usb: HS PHY EUD enabled; skipping analog init\n");
                    } else {
                        if !super::super::platform::bramble::refresh_usb_power(false) {
                            log_puts("usb: Fastboot HS PHY regulator refresh failed\n");
                            return false;
                        }
                        if !super::super::platform::bramble::enable_usb_hs_phy_ref_clock() {
                            log_puts("usb: Fastboot HS PHY ref clock enable failed\n");
                            return false;
                        }
                        if !super::super::platform::bramble::pulse_usb2_phy_reset() {
                            log_puts("usb: Fastboot HS PHY init reset failed\n");
                            return false;
                        }
                        init_hsphy_source_exact();
                    }
                }
                #[cfg(not(fullerene_aarch64_usb_gadget_handoff_ss_reinit_hs_phy))]
                {
                    // Match dwc3_phy_setup(): the official core writes the USB3
                    // PIPE suspend bit before dwc3_core_soft_reset() invokes
                    // usb_phy_init(usb3), which is the QMP init boundary.
                    // Fastboot's retained value is not a valid substitute for
                    // that source-ordered write.
                    let mut usb3 = read(GUSB3PIPECTL0);
                    usb3 |= GUSB3PIPECTL_SUSPHY;
                    write(GUSB3PIPECTL0, usb3);
                    let _ = read(GUSB3PIPECTL0);
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_pre_qmp_phy_setup)]
                {
                    // qpr1's dwc3_phy_setup() executes before the external PHY
                    // init/reset boundary. Reproduce its controller-side
                    // defaults here, before QMP reset/init: clear UX_EXIT_PX,
                    // select UTMI-8/TRDTIM=9, and assert both SUSPHY bits.
                    // This is intentionally separate from the existing
                    // post-global-control UX/UTMI A/Bs.
                    let mut usb3 = read(GUSB3PIPECTL0);
                    usb3 &= !GUSB3PIPECTL_UX_EXIT_PX;
                    usb3 |= GUSB3PIPECTL_SUSPHY;
                    write(GUSB3PIPECTL0, usb3);
                    let _ = read(GUSB3PIPECTL0);
                    configure_usb2_phy_interface();
                    let mut usb2 = read(GUSB2PHYCFG0);
                    usb2 |= GUSB2PHYCFG_SUSPHY;
                    mark_g2w_site(1015);
                    write(GUSB2PHYCFG0, usb2);
                    let _ = read(GUSB2PHYCFG0);
                }
                // Android resets the combo PHY immediately before QMP init.
                // Keep the DWC3 core untouched for --no-core-reset, but restore
                // this separate global/USB3-PHY reset boundary first.
                if !super::super::platform::bramble::reset_qmp_phy_blocks() {
                    log_puts("usb: Fastboot QMP PHY reset failed\n");
                    return false;
                }
                // Match msm_ssphy_qmp_init()'s PHY-local ownership boundary:
                // reassert its core LDO, the shared 19.2 MHz reference, and only
                // the QMP AUX/COM_AUX/PIPE branches.  Do not retune the live
                // DWC3 core or mock-UTMI RCGs here; --no-core-reset must remain a
                // PHY/clock differential, not a second controller reset path.
                if !super::super::platform::bramble::refresh_usb_qmp_power() {
                    log_puts("usb: Fastboot QMP regulator refresh failed\n");
                }
                if !super::super::platform::bramble::enable_usb_hs_phy_ref_clock() {
                    log_puts("usb: Fastboot QMP ref clock enable failed\n");
                }
                if !super::super::platform::bramble::enable_usb_qmp_clock_branches() {
                    log_puts("usb: Fastboot QMP clock branch enable failed\n");
                    return false;
                }
                qmp_ready = init_qmp_phy();
                QMP_PHY_READY = qmp_ready;
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_qmp_power)]
                if qmp_ready {
                    let qmp_power = phy::qmp_reassert_power();
                    log_hex("usb: SS QMP power reassert mask=", qmp_power as u64);
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_core_clocks)]
                if qmp_ready {
                    // Fastboot's SS session can leave the DWC3 core domain
                    // collapsed while its QMP branches remain usable. Reassert
                    // only the source-derived CX/bus votes, USB30 GDSC, and the
                    // five controller branches here. No reset and no QMP branch
                    // rewrite are part of this A/B, so a changed result points to
                    // controller-domain ownership rather than PHY initialization.
                    let _ = reassert_ss_controller_domain();
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_qmp_clocks)]
                if qmp_ready {
                    // The QMP branches depend on the CX/GDSC/controller vote
                    // above on the no-core path. Reassert them only after that
                    // domain is live, matching the Android clock ownership
                    // order rather than attempting the branch write too early.
                    let ok = super::super::platform::bramble::enable_usb_qmp_clock_branches();
                    log_hex("usb: SS QMP clock reassert=", u64::from(ok));
                }
                let qmp_probe_phase = qmp_phase_probe_reached();
                if qmp_probe_phase != 0 {
                    // The phase probe intentionally stops before the next QMP
                    // access and falls back to the known USB2 pull-up. Host
                    // attach is therefore a same-boot reached/not-reached bit;
                    // it does not claim that the partial QMP state is usable.
                    trace_marker(TRACE_PROBE_WATCHDOG, 0x5150_0000 | (qmp_probe_phase & 0xff)); // "QP" + phase
                    QMP_PHY_READY = false;
                    return init_usb2_bare_pullup_handoff_inner(true);
                }
                if !qmp_ready {
                    log_puts("usb: Fastboot QMP SuperSpeed handoff unavailable\n");
                    return false;
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_android_dbm_reset)]
                {
                    // Android's `dwc3_msm_block_reset(false)` does not assert
                    // the GCC/DWC3 core reset here; it resets and enables the
                    // Qualcomm DBM immediately before peripheral-mode startup.
                    // Keep this source-ordered A/B before any SS endpoint command
                    // and before Run/Stop so it can be distinguished from the
                    // failed post-Run/Stop link-clock experiment.
                    if !super::super::platform::bramble::android_dbm_reset_and_enable() {
                        log_puts("usb: Android DBM reset/enable failed\n");
                        return false;
                    }
                }
                // `--no-core-reset` skips the normal DWC3 core/PHY soft-reset
                // sequence, including its USB3 PIPE PHYSOFTRST release. The
                // external QMP reset above does not clear this controller-side
                // bit, so expose the source-derived release as an isolated A/B.
                if !reset_core && cfg!(fullerene_aarch64_usb_ss_phy_reset_release) {
                    release_usb3_phy_reset();
                }
                // Stage 13 is the QMP-complete boundary. It intentionally
                // publishes only the minimum SS Run/Stop state so host attach can
                // distinguish QMP/link bring-up from the later SMMU, endpoint,
                // event-ring, and EP0 setup sequence.
                #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
                if (cfg!(fullerene_aarch64_usb_gadget_handoff_direct) || super_speed)
                    && stop_after_gadget_handoff_stage(13)
                {
                    return true;
                }
            }
        }

        if reset_core {
            // B3: about to assert CSFTRST (device_soft_reset inside
            // core_soft_reset). If the log stops after B3, the failure is
            // inside the soft-reset handshake or the core/PHY reset section.
            #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
            init_beacon();
            // Core state at the moment the soft reset is about to start:
            // a CSFTRST issued while the core is suspended/halted vs reset
            // vs running has different completion behavior, and the gate
            // readout names which state the handoff actually inherited.
            INIT_PRE_RESET_DSTS = read(DSTS);
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

        // An SS-only Fastboot session may leave the mock UTMI branch clock
        // gated even though the USB2 PHY can still answer the host's chirp.
        // Bring up that branch after the direct handoff reset, matching the
        // Qualcomm resume order before issuing any DWC3 endpoint command.
        if !super_speed && !reset_platform && !super::super::platform::bramble::enable_usb2_utmi_clock() {
            log_puts("usb: GCC mock UTMI clock enable failed\n");
            trace_event(TRACE_GCC_UTMI_CLOCK, 0, 0, 0, 0, read(DSTS));
        }

        // B4: core_soft_reset (CSFTRST + GCTL core reset + USB2 PHY
        // soft reset + release delays) fully returned.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        configure_dwc3_global_control();
        // CSFTRST can restore GCTL.PRTCAPDIR to its reset value on DWC_usb31.
        // Keep the controller in device mode before programming EP0, while
        // preserving the platform-specific global-control bits.
        let mut gctl = read(GCTL);
        gctl &= !(GCTL_PRTCAPDIR_MASK | GCTL_SCALEDOWN_MASK | GCTL_DISSCRAMBLE);
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        write(GCTL, gctl);
        let _ = read(GCTL);
        // DWC_usb31's revision is encoded in VER_NUMBER rather than GSNPSID;
        // qpr1 still keeps RAMCLKSEL at reset value 0 after USB reset. The
        // helper below is only the opt-in legacy captured-value differential.
        reapply_ramclksel();
        // The generic global-control helper intentionally returns early for
        // DWC_usb31, which is the IP used by Bramble. Reapply the USB2 UTMI
        // interface contract and the msm-4.19 usb31 gadget workarounds here;
        // otherwise CSFTRST leaves the DWC3-side timing at its reset/Fastboot
        // value and the host can see the pull-up without EP0 transactions.
        configure_usb2_phy_interface();
        apply_usb31_gadget_reference_deltas();
        // The USB2 handoff already applies Android msm's second
        // dwc3_set_mode(DEVICE) GCTL write. The SuperSpeed path must carry
        // the same DWC_usb31 mode tail: U2RSTECN, PWRDNSCALE=2, and
        // U2EXIT_LFPS are controller-mode state, not USB2 packet framing.
        // Apply it before the stage-14 SS boundary and before publishing any
        // endpoint resources.
        configure_dwc3_device_mode();
        // Qualcomm's Bramble glue applies this USB31 link timer before
        // VBUS/gadget connect. Keep it as the next isolated SS A/B.
        configure_usb31_lfps_exit_timer();
        configure_usb31_phy_setup();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_qmp_power_after_gctl)]
        if qmp_ready {
            // `msm_ssphy_qmp_set_suspend(usb3, 0)` first re-enables the QMP
            // PHY power consumers after `dwc3_core_setup_global_control()`.
            // This opt-in isolates that source-confirmed post-GCTL power
            // boundary from the earlier QMP init-time power writes.
            let ok = super::super::platform::bramble::refresh_usb_qmp_power();
            log_hex("usb: SS QMP post-GCTL power reassert=", u64::from(ok));
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_qmp_clocks_after_gctl)]
        if qmp_ready {
            // Bramble's usb_phy_set_suspend(usb3, 0) enables the QMP clock
            // set after dwc3_core_setup_global_control(). The existing clock
            // reassertion is intentionally earlier; this opt-in isolates the
            // source-confirmed post-global-control ownership boundary.
            let ok = super::super::platform::bramble::enable_usb_qmp_clock_branches();
            log_hex("usb: SS QMP post-GCTL clock reassert=", u64::from(ok));
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_hs_phy_ref_after_gctl)]
        if qmp_ready {
            // Bramble's phy-msm-snps-hs.c implementation of
            // usb_phy_set_suspend(usb2, 0) re-enables ref_clk_src after the
            // DWC3 global-control setup. The handoff already enables that
            // RPMh-backed clock before QMP init; this opt-in isolates the
            // source-confirmed post-global-control ownership boundary.
            let ok = super::super::platform::bramble::enable_usb_hs_phy_ref_clock();
            log_hex("usb: SS HS PHY ref post-GCTL reassert=", u64::from(ok));
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_dis_sleep_mode_before_gadget)]
        if qmp_ready {
            // Qualcomm's dwc3_otg_start_peripheral() calls
            // dwc3_dis_sleep_mode() after selecting DEVICE and before the
            // SuperSpeed gadget start. The normal path clears these bits
            // later, after SMMU setup; this opt-in isolates only that order.
            let mut usb2 = read(GUSB2PHYCFG0);
            usb2 &= !GUSB2PHYCFG_ENBLSLPM;
            mark_g2w_site(1016);
            write(GUSB2PHYCFG0, usb2);
            let guctl1 = read(GUCTL1);
            write(GUCTL1, guctl1 & !GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST);
        }
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_ss_clear_qmp_autonomous,
            fullerene_aarch64_usb_gadget_handoff_ss_clear_qmp_autonomous_exact
        ))]
        if qmp_ready {
            // Bramble's msm_ssphy_qmp_set_suspend(usb3, 0) resumes the QMP
            // clocks/power and, when the cable is connected, clears
            // autonomous mode before dwc3_gadget_run_stop(). The handoff
            // already reasserts the clocks/power above; this opt-in isolates
            // the missing connected-cable autonomous-mode transition.
            phy::qmp_set_autonomous_mode(false);
        }

        // Stage 14 is the post-QMP DWC3 global-control boundary. It keeps the
        // core reset differential unchanged, but includes the device-mode,
        // RAM-clock, USB2 timing, and USB31 reference setup that the normal
        // path performs after QMP becomes ready. The stage helper then emits
        // only DCFG=SS and Run/Stop, before SMMU or endpoint/DMA ownership.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(14) {
            return true;
        }

        // B5: post-reset global control programmed.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

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

        // Stage 15 is the first post-stage-14 tail boundary.  In the usual
        // `--no-core-reset` run the clear is a no-op, which is intentional:
        // this stop still separates the stage-14 register path from the
        // first SMMU/ownership operation below.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(15) {
            return true;
        }

        // Fastboot leaves the USB2 PHY powered, but the DWC3 handoff can
        // clear the PHY's session-valid state while stopping the old gadget.
        // Reapply the non-destructive Femto PHY programming on the USB2
        // handoff path; this does not assert the GCC PHY reset or touch the
        // Type-C power domain.
        if !super_speed
            && !reset_platform
            && !(cfg!(fullerene_aarch64_usb_hsphy_before_reset)
                && !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core)
                && cfg!(fullerene_aarch64_usb_gadget_handoff_hsphy_resume_clocks_after_reset))
        {
            init_hsphy();
        } else if !super_speed
            && !reset_platform
            && cfg!(fullerene_aarch64_usb_hsphy_before_reset)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core)
            && cfg!(fullerene_aarch64_usb_gadget_handoff_hsphy_resume_clocks_after_reset)
        {
            // qpr1 runs msm_hsphy_init() before dwc3_core_soft_reset(), then
            // calls usb_phy_set_suspend(usb2_phy, 0) after global control.
            // That resume path re-enables ref_clk_src; it does not replay the
            // analog init sequence. The ref-clock resume is already handled
            // at the post-GCTL boundary above, so repeating init_hsphy() here
            // would change the source-defined ownership/order boundary.
            log_puts("usb gadget handoff: resuming HS PHY clocks; skipping analog re-init\n");
        }

        // Core reset restores the QSCRATCH-facing state on some DWC3
        // revisions, so re-apply the Qualcomm glue votes after reset.  The
        // qpr1 peripheral-start path only writes UTMI_OTG_VBUS_VALID here;
        // keep the source-vbus-only A/B effective across this post-reset
        // boundary instead of reintroducing the inherited SW_SESSVLD_SEL.
        qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24); // LANE0_PWR_PRESENT
        if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_vbus_only) {
            set_direct_usb2_vbus_override();
        } else {
            qscratch_set(
                QSCRATCH_HS_PHY_CTRL,
                (1 << 20) | (1 << 28), // UTMI_OTG_VBUS_VALID | SW_SESSVLD_SEL
            );
        }
        qscratch_set(QSCRATCH_CGCTL, 0x18);
        // SM7250's DWC3 revision is older than 2.50a. The Qualcomm glue
        // advertises the XHCI 1.0 register layout through this QSCRATCH bit
        // during its reset callback.
        qscratch_set(QSCRATCH_GENERAL_CFG, QSCRATCH_GENERAL_CFG_XHCI_REV);

        // C1: post-reset QSCRATCH session re-asserted (the host attach point
        // when the QSCRATCH session bits own the pull-up).
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        // USB2-only starts need the same post-reset UTMI clock selection as
        // the Qualcomm glue. The DWC3 reset above invalidates the controller's
        // previous PIPE/UTMI selection, so this is required for handoff too;
        // it is a controller-side QSCRATCH mux change, not a PHY power reset.
        if !super_speed {
            select_utmi_pipe_clock();
        }

        // Retain the historical controller timing experiment after the
        // handoff reset. The Bramble qpr1 Qualcomm source has no equivalent;
        // the preserve-state A/B leaves these registers unchanged.
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
            super::super::platform::bramble::usb_resources().dma_pool.stream_id,
            super::super::platform::bramble::usb_resources().dma_pool.iova_base as u32,
            super::super::platform::bramble::usb_resources().dma_pool.size as u32,
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

        // Stage 16 proves that the selected physical/no-SMMU or Apps-SMMU
        // branch returned before any event-ring address is handed to DWC3.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(16) {
            return true;
        }

        INIT_STAGE = 3;
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if cfg!(fullerene_aarch64_usb_gadget_handoff_direct) && stop_after_gadget_handoff_stage(3) {
            return true;
        }

        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_source_susphy)]
        {
            // qpr1's dwc3_phy_setup() leaves SUSPHY asserted through the
            // endpoint/resource construction. send_ep_command_result()
            // performs the temporary Linux guard around each command and
            // restores this state afterward.
            usb2 |= GUSB2PHYCFG_SUSPHY;
        }
        mark_g2w_site(1017);
        write(GUSB2PHYCFG0, usb2);
        // Match dwc3_dis_sleep_mode(): the host-side L1 threshold helper is
        // independent of the USB2 PHY sleep bit and can survive a Fastboot
        // handoff with a stale value.
        let guctl1 = read(GUCTL1);
        write(GUCTL1, guctl1 & !GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST);
        let mut usb3 = read(GUSB3PIPECTL0);
        if qmp_ready {
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_source_susphy)]
            {
                // qpr1 dwc3_phy_setup() asserts USB3 SUSPHY after core
                // configuration and leaves it asserted through gadget-start
                // endpoint construction. The link exits suspend at connect.
                usb3 |= GUSB3PIPECTL_SUSPHY;
            }
            #[cfg(not(fullerene_aarch64_usb_gadget_handoff_ss_source_susphy))]
            {
                usb3 &= !GUSB3PIPECTL_SUSPHY;
            }
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
        // C2: event ring published and the Fastboot event count acknowledged.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
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

        // Stage 17 is after the event-ring publication, optional Qualcomm GSI
        // setup, and the in-memory gadget-state reset, but before DCFG and
        // endpoint-context commands.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(17) {
            return true;
        }

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

        // Capture the core state BEFORE the first endpoint command: the
        // post-command DSTS (below) only reflects the state after the
        // command retired or its 2M-read timeout expired.
        INIT_DEPSTART_PRE_DSTS = read(DSTS);
        let depstart_ok = send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0);
        INIT_STAGE = 4;
        INIT_DEPSTART_RAW = read(dep_reg(0, 0x0c));
        INIT_DEPSTART_DSTS = read(DSTS);
        if !depstart_ok {
            log_puts("usb: DEPSTARTCFG failed\n");
            return false;
        }
        // Android's msm DWC3 glue preallocates every hardware endpoint's
        // transfer resource immediately after DEPSTARTCFG. The earlier
        // fallback-path experiment did not cover this direct sequence, so
        // keep the direct-path differential explicit as well.
        if cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
        {
            for endpoint in 0..qpr1_endpoint_count() {
                if !set_transfer_resource(endpoint) {
                    log_puts("usb: Android direct resource preallocation failed\n");
                    return false;
                }
            }
        }
        // Linux starts EP0 at the SuperSpeed packet size and changes it at
        // Connect Done, even when the eventual link falls back to High
        // Speed. Keep the direct USB2 handoff's smaller configuration as the
        // default for the known-good Bramble path, but expose the exact
        // Linux initial state as a bounded hardware A/B.
        let ep0_packet_size = if cfg!(fullerene_aarch64_usb_ep0_initial_512) || qmp_ready {
            INITIAL_EP0_MAX_PACKET_SIZE
        } else {
            64
        };
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
            INIT_EPCFG1_RAW = read(dep_reg(1, 0x0c));
            INIT_EPCFG1_DSTS = read(DSTS);
        }
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
        write(DEVTEN, direct_gadget_devten());
        trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, read(DSTS));

        // Stage 18 stops after the endpoint contexts, DALEPENA, and DEVTEN
        // are published, before the first setup-TRB write.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(18) {
            return true;
        }

        prepare_ep0_setup_trb();

        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_eager_setup)]
        if qmp_ready {
            // qpr1's __dwc3_gadget_start() arms CONTROL_SETUP before the
            // production Run/Stop write. The normal Bramble handoff defers
            // this command because the same boundary wedges the USB2 path;
            // keep the SS-only source-order A/B separate and let the command
            // result decide whether this DWC31 accepts the earlier arm.
            let armed = start_setup();
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5353_4541, // "SSEA" source-order SS eager arm
                armed as u32,
                0,
                8,
                read(DSTS),
            );
            if !armed {
                log_puts("usb: SS eager SETUP STARTTRANSFER failed\n");
                return false;
            }
            poll_ep0_event_ring();
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_stall_flush)]
        {
            // Fastboot's interrupted session can leave a SETUP packet pending
            // in the EP0 FIFO, and this core rejects every endpoint command
            // until the pending packet is flushed - the host's first SETUP
            // token then goes unanswered until its own retry arrives seconds
            // later. Linux's dwc3_ep0_stall_and_restart() clears exactly this
            // state with an EP0 SETSTALL; the core auto-clears the stall when
            // the next SETUP token arrives, so arm the fresh SETUP TRB at the
            // same halted boundary Linux uses.
            let _ = unsafe { send_ep_command(0, DEPCMD_SETSTALL, 0, 0, 0) };
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5354_4C46, // "STLF" stall-flush arm outcome
                EP0_SETUP_ARMED as u32,
                0,
                0,
                read(DSTS),
            );
        }
        // C3: DEPSTARTCFG + both EP0 SETEPCONFIG/SETTRANSFRESOURCE commands
        // done (or failed fast), DALEPENA/DEVTEN set, setup TRB prepared.

        apply_ep0_txfifo_fix();
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        // Stage 19 is the setup-TRB/cache boundary.  DMA probing, transfer
        // commands, IRQ routing, and the final Run/Stop sequence are all
        // deliberately downstream of this stop.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(19) {
            return true;
        }

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
                let targets: [(usize, usize); 3] = [
                    (ep0_event_dma_base(), 16),
                    (ep0_trb_ptr(0) as usize, 64),
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
                super::super::timer::delay_ms(1);
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
            prepare_ep0_setup_trb();
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
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct)
            || cfg!(fullerene_aarch64_usb_probe_irq_controller)
        {
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
        // A USB2-only Bramble handoff must leave the UTMI PHY awake, matching
        // the known-good bare/reuse path. The SuperSpeed path still follows
        // Linux's gadget-start SUSPHY policy.
        if qmp_ready {
            enable_gadget_susphy();
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_clear_usb3_susphy_before_runstop)]
        if qmp_ready {
            // The normal gadget helper enables both PHY suspend bits before
            // Run/Stop. Keep an SS-only diagnostic that clears just the USB3
            // PIPE bit at the last possible boundary: lane-B reaches the
            // host's SS setup stage but never receives EP0 data, so this
            // separates a suspended PIPE from endpoint/DMA ownership.
            let before = read(GUSB3PIPECTL0);
            let after = before & !GUSB3PIPECTL_SUSPHY;
            write(GUSB3PIPECTL0, after);
            let readback = read(GUSB3PIPECTL0);
            trace_event(
                TRACE_DWC3_BOUNDARY,
                0x5355_5350,
                before,
                readback,
                read(DSTS),
                0,
            );
        }
        // C4: speed/SUSPHY configured; the next boundary is Run/Stop.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
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
                // The timeout flag is bit 31; bit 16 alone is a healthy
                // XferRscIdx=1 completion on physical EP1.
                "ep1-done" => {
                    TRACE_HARVEST_EP1 != 0xFFFF_FFFF
                        && TRACE_HARVEST_EP1 & 0x8000_0000 == 0
                        && TRACE_HARVEST_EP1 & 0xf000 == 0
                }
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
                    super::super::timer::delay_ms(secs.saturating_mul(1000));
                }
            }
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop)]
        if super_speed {
            // Android's dwc3_gadget_pullup(1) performs a device-core soft
            // reset immediately before dwc3_gadget_run_stop(1). Reproduce
            // that second reset at the same boundary; the restart helper
            // below then republishes the event ring, endpoint resources, and
            // initial SETUP transfer as __dwc3_gadget_start() does.
            if !device_soft_reset() {
                log_puts("usb: SS pre-Run/Stop device reset failed\n");
                return false;
            }
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop)]
        republish_ep0_event_ring_at_runstop();
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop,
            fullerene_aarch64_usb_gadget_handoff_ss_core_reset_at_runstop
        ))]
        if !restart_gadget_at_runstop(qmp_ready) {
            // Android's dwc3_gadget_run_stop() ignores the void restart
            // helper's internal command result and still asserts Run/Stop.
            // Keep that behavior for this diagnostic so a failed EP0 restart
            // is distinguishable from a failure to publish the pull-up.
            log_puts("usb: Android gadget restart at Run/Stop incomplete\n");
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_susphy)]
        enable_usb2_gadget_susphy();

        // Stage 20 is the last pre-Run/Stop boundary.  It includes all
        // setup/ownership work above and leaves only the actual production
        // Run/Stop transition plus post-connect arm logic unexecuted.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && gadget_handoff_stop_selected(20) {
            #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
            if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
                .filter(|value| value.starts_with("ss-"))
                .is_some()
            {
                // Capture before the selected stage helper performs its
                // diagnostic recovery. This is the actual pre-production
                // boundary; capturing after the helper would describe its
                // extra Run/Stop instead.
                SS_RUNSTOP_PRE_DCTL = read(DCTL);
                capture_ss_state_snapshot();
            }
            #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
            if let Some(selector) = option_env!("FULLERENE_USB_SIGNAL_CMD_GATE").filter(|value| {
                value.starts_with("ss-domain-")
                    || value.starts_with("ss-gctl-")
                    || value.starts_with("ss-qmp-")
                    || value.starts_with("ss-qscratch-")
                    || *value == "ss-ltssm"
                    || value.starts_with("ss-ltssm-bit")
            }) {
                // DCTL stop/run is not a reliable physical readout at this
                // boundary. Encode one domain/GCTL/QMP bit in the time at which the
                // known HS fallback is published: 0 = no extra delay, 1 =
                // 4 seconds, and 2 = missing snapshot sentinel. The 4 s
                // separation is wider than the observed host attach jitter
                // while remaining below the handset's watchdog window.
                let code = utmi_readout_code(selector);
                let delay_ms = if selector == "ss-ltssm" {
                    // Raw LTSSM values use a compact 250-ms bucket. The
                    // missing-snapshot sentinel is 16 and remains bounded
                    // below the normal watchdog recovery window.
                    u64::from(code.saturating_mul(250).min(4_000))
                } else {
                    match code {
                        0 => 0,
                        1 => 4_000,
                        _ => 8_000,
                    }
                };
                crate::timer::delay_ms(delay_ms);
            }
            if stop_after_gadget_handoff_stage(20) {
                return true;
            }
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_device_mode)]
        if super_speed {
            // The post-Run/Stop `ss-gctl=0` readout indicates that this
            // DWC_usb31 instance is not retaining PRTCAPDIR=DEVICE. Reapply
            // only the Android msm device-mode tail immediately before the
            // production transition; leave PHY, endpoint, and packet state
            // untouched for this controller-only A/B.
            configure_dwc3_device_mode();
        }
        if super_speed && cfg!(fullerene_aarch64_usb_ep0_signal_probe) {
            SS_RUNSTOP_PRE_DCTL = read(DCTL);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_defaults_at_runstop)]
        {
            // Keep the SuperSpeed boundary identical to the USB2 ordering
            // A/B: only the non-endpoint gadget-start defaults are replayed.
            log_puts("usb: replaying gadget start defaults at SS Run/Stop\n");
            configure_gadget_start_defaults();
        }
        if !run_stop_device(true) {
            log_hex("usb: DWC3 remained halted, DSTS=", read(DSTS) as u64);
            return false;
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_source_devten_after_runstop)]
        {
            // Source-order qpr1 enables DEVTEN before Run/Stop, but this
            // narrow A/B republishes the same mask at the first post-
            // transition boundary. It changes only event ingress visibility;
            // PHY, endpoint contexts, and EP0/TRB ownership are untouched.
            write(DEVTEN, direct_gadget_devten());
            let readback = read(DEVTEN);
            trace_event(
                TRACE_DWC3_BOUNDARY,
                0x4456_4152, // "DVAR": DEVTEN after Run/Stop
                readback,
                read(DSTS),
                read(DCTL),
                0,
            );
        }
        #[cfg(fullerene_aarch64_usb_usb2_susphy_after_runstop)]
        {
            // A SuperSpeed-only handoff must not leave the USB2 pull-up
            // visible after the production DCTL.Run/Stop transition.  The
            // pre-Run/Stop SUSPHY A/B is consumed by the shared Linux guard;
            // this opt-in repeats the PHY-suspend write at the first
            // post-transition boundary, before any SS state snapshot or
            // endpoint traffic is observed by the host.
            enable_usb2_gadget_susphy();
            trace_event(
                TRACE_DWC3_BOUNDARY,
                0x5532_5355, // "U2SU": post-Run/Stop USB2 SUSPHY
                read(GUSB2PHYCFG0),
                read(GUSB3PIPECTL0),
                read(DSTS),
                read(DCTL),
            );
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_clear_usb3_susphy_after_runstop)]
        if qmp_ready {
            // Android's Bramble gadget start does not re-enable USB3
            // SUSPHY at its final DCTL.Run/Stop boundary. The common
            // helper above does. Clear it only after the transition and
            // retain both values so the host-visible result can be tied to
            // the actual post-boundary readback.
            let before = read(GUSB3PIPECTL0);
            let after = before & !GUSB3PIPECTL_SUSPHY;
            write(GUSB3PIPECTL0, after);
            let readback = read(GUSB3PIPECTL0);
            trace_event(
                TRACE_DWC3_BOUNDARY,
                0x5355_5341, // "SUSA": post-Run/Stop SUSPHY A/B
                before,
                readback,
                read(DSTS),
                read(DCTL),
            );
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_link_clocks_after_runstop)]
        if super_speed {
            // Qualcomm's msm glue treats the link-clock reset as a separate
            // boundary from DWC3 CSFTRST. Replay that source-ordered
            // iface/core/sleep/utmi stop -> GCC core-reset -> release sequence
            // only after the production Run/Stop write, so this A/B answers
            // whether the post-transition domain loss is an omitted glue
            // ownership boundary. The stage-21 snapshot follows immediately;
            // no endpoint or packet operation is added to the test.
            trace_marker(TRACE_DWC3_RESET_BEGIN, 0x4C435253); // "LCRS"
            if !super::super::platform::bramble::android_controller_block_reset() {
                log_puts("usb: SS post-Run/Stop Android link-clock reset failed\n");
            }
        }
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_ss_reassert_core_clocks_after_runstop,
            fullerene_aarch64_usb_gadget_handoff_ss_reassert_domain_after_runstop
        ))]
        if super_speed {
            // The stage-21 domain readouts show that this platform can drop
            // the USB30 domain at the Run/Stop boundary even after the same
            // vote/branch sequence was applied before it.  Keep both tests
            // opt-in and immediately after the production transition so
            // they change only domain persistence, before any SS snapshot or
            // endpoint/packet activity.
            if cfg!(fullerene_aarch64_usb_gadget_handoff_ss_reassert_domain_after_runstop) {
                // This is the full Android-style keepalive prefix: resend
                // the CX/BCM and regulator votes first, then restore GDSC,
                // RCG sources, and controller branches.
                let votes = super::super::platform::bramble::refresh_usb_domain_votes(
                    super::super::platform::bramble::UsbBusVote::Nominal,
                    true,
                );
                let controller = reassert_ss_controller_domain();
                log_hex(
                    "usb: SS post-Run/Stop domain refresh mask=",
                    u64::from((votes as u8) | ((controller as u8) << 1)),
                );
            } else {
                // Narrow control: no regulator re-send, only the controller
                // domain operation used by the earlier A/B.
                let _ = reassert_ss_controller_domain();
            }
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if super_speed
            && option_env!("FULLERENE_USB_SIGNAL_CMD_GATE")
                .filter(|value| value.starts_with("ss-"))
                .is_some()
        {
            // Stage 20 is the pre-Run/Stop snapshot.  Capture again here,
            // immediately after the production transition, so stage 21
            // readouts classify the running USB31 link rather than the
            // expected pre-start SS_DIS state.  Keep this before the optional
            // device-mode A/B, which is intentionally a separate mutation.
            SS_RUNSTOP_POST_DCTL = read(DCTL);
            SS_RUNSTOP_POST_DSTS = read(DSTS);
            capture_ss_state_snapshot();
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_reassert_device_mode)]
        if super_speed {
            // The preceding pre-Run/Stop write did not change the post
            // snapshot (`ss-gctl=0`). Repeat the same narrow mode tail after
            // Run/Stop to distinguish a transition-time GCTL overwrite from
            // an unwritable/incorrect PRTCAPDIR field.
            configure_dwc3_device_mode();
        }
        if super_speed && cfg!(fullerene_aarch64_usb_ep0_signal_probe) {
            if SS_RUNSTOP_POST_DCTL == 0xffff_ffff {
                SS_RUNSTOP_POST_DCTL = read(DCTL);
                SS_RUNSTOP_POST_DSTS = read(DSTS);
            }
        }
        RUN_STOP_TICK = arch_counter();
        // Stage 21 is immediately after the production Run/Stop transition
        // and its readback.  The remaining setup-arm/poll tail is untouched.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(21) {
            return true;
        }
        // Tight SETUP-arm window: retry the ep0 OUT Start Transfer every
        // 200 us for up to 100 ms after Run/Stop. The link reaches ON within
        // a few ms (the HS chirp handshake), and the host's first SETUP
        // token arrives only after its own attach debounce plus port reset -
        // arming in this window guarantees the first descriptor read is
        // answered instead of timing out (-110) while the poll-loop guard
        // was still waiting for the link state. The SS retry A/B extends
        // this bounded window to 5 seconds because SuperSpeed training can
        // outlast the USB2-oriented default.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup)
            && !cfg!(fullerene_aarch64_usb_gadget_handoff_start_at_connect_done)
        {
            // With the Connect-Done differential, do not let this bounded
            // post-Run/Stop retry window arm EP0 first.  The Android gadget
            // path reaches its initial SETUP arm from the Connect Done
            // handler; allowing both paths would make the A/B indistinct.
            let arm_deadline = arch_counter().saturating_add(
                arch_counter_frequency().saturating_mul(
                    if cfg!(fullerene_aarch64_usb_gadget_handoff_ss_retry_setup) {
                        5_000
                    } else {
                        100
                    },
                ) / 1000,
            );
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
                super::super::timer::delay_us(200);
            }
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5441_524D, // "TARM" tight-arm outcome
                armed as u32,
                0,
                0,
                read(DSTS),
            );
            // Host-visible arm readout: a single source-aligned DWC3
            // Run/Stop pair after the window is one disconnect/re-attach pair
            // in the host log. It fires only after a successful arm and a core
            // link-state check of U0.
            if armed && option_env!("FULLERENE_USB_ARM_BLIP") == Some("1") {
                runstop_blips(1);
            }
        } else {
            // Historical XBL differential marker. Source-guided Android/Linux
            // code eagerly arms CONTROL_SETUP; this flag is not a canonical
            // initial-SETUP model and must not suppress that baseline.
            trace_event(
                TRACE_SETUP_QUEUED,
                0x58424C44, // "XBLD": historical differential marker
                0,
                0,
                8,
                read(DSTS),
            );
        }

        // Stage 22 is after the tight post-Run/Stop SETUP-arm window, before
        // deferred post-Run/Stop probing and the C5 diagnostics.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(22) {
            return true;
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1") {
            // Defer the probe to the polling owner so the U0 check observes
            // the host-facing link rather than the pre-attach Run/Stop tail.
            POST_RUNSTOP_PROBE_PENDING = true;
            POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
            );
        }

        // C5: Run/Stop active and the tight SETUP-arm window is done.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        // Defer the signal window to the normal polling owner when the
        // initial SETUP arm is intentionally U0-gated; see the corresponding
        // direct-handoff path above.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect) {
            ep0_signal_early_drop_check();
        }

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_pre_runstop_drop_check();

        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        ep0_signal_heartbeat_check();

        // Stage 23 is after all C5 post-Run/Stop diagnostics, immediately
        // before the optional start-after-connect event-ring poll.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(23) {
            return true;
        }

        #[cfg(fullerene_aarch64_usb_gadget_handoff_start_after_connect)]
        {
            // Do NOT issue the pre-link-ON STARTTRANSFER here: on this core a
            // Start Transfer issued before the link reaches ON wedges the
            // endpoint command engine, so the host's first SETUP is never
            // serviced (descriptor read/64 -110) even though Run/Stop has
            // already published the pull-up. The SETUP TRB is prepared at
            // stage 6; the poll loop's U0-guarded try_arm_setup arms it the
            // moment the link comes ON - the same proven path the default
            // mode relies on. Consume any early event to keep the ring clean.
            poll_ep0_event_ring();
        }
        log_puts("usb: Fullerene DWC3 gadget connected\n");
        // C6: init tail done (start_after_connect arm + event poll); about to
        // return to the probe entry and cross the cfg-block boundary.
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        init_beacon();
        note_runtime_event(super::super::platform::bramble::UsbRuntimeEvent::ControllerStarted);

        // Stage 24 is the last line of init_with_super_speed(), after the
        // event-ring poll and runtime-start notification but before returning
        // to usb_probe's IRQ/ownership tail.
        #[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
        if super_speed && stop_after_gadget_handoff_stage(24) {
            return true;
        }
    }
    true
}

