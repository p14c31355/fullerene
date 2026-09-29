//! USB2 gadget handoff that reuses Fastboot-owned DWC3 state.

use super::*;

#[cfg(fullerene_aarch64_usb_gadget_handoff_probe)]
pub(super) unsafe fn init_usb2_gadget_reuse_fastboot_ep0() -> bool {
    HANDOFF_ENTERED = true;
    // Timing latch at the handoff entry. This boundary is *provably earlier*
    // than the DEVICE-mode write, unlike `run_stop_device_no_readback` (which
    // runs after it and whose `wrapping_sub` therefore underflowed and made the
    // whole `dtimr*` ladder read FALSE for the wrong reason). What this measures
    // is "how long from entering the handoff to reaching the DEVICE write".
    HANDOFF_TICK_MS.store(now_ms_for_timing(), core::sync::atomic::Ordering::Relaxed);
    // Re-establish the Qualcomm PHY/session state without asserting the
    // pull-up yet.  EP0 must be fully configured, its event ring published,
    // and the first SETUP TRB armed before Run/Stop is allowed to advertise
    // the device; otherwise the host can issue the first descriptor request
    // while the handoff is still rebuilding DWC3 state.
    // This USB2 entry point bypasses `init_with_super_speed`; apply the same
    // explicit SMMU-disable differential here before publishing any new EP0
    // DMA object. Previously `--smmu-disable` was ignored on this path.
    if !prepare_smmu_dma_bypass() {
        handoff_progress(1);
        return false;
    }
    // Snapshot the Fastboot-owned clock/PHY state before the handoff starts
    // changing the controller-side USB2 session.
    // Bisection: pullup_mark(2) here was SILENT (2 of 2 valid runs; a third run failed to decode).
    // Window is now (:7067, :7316).
    trace_utmi_state(1);
    // Linux calls GDBGFIFOSPACE's returned field SPACE_AVAILABLE. Capture the
    // inherited free-space vector explicitly; zero is not queue occupancy.
    trace_dwc3_debug_stage(0);
    // Stage 1 is deliberately before even the initial stop/readback: it is
    // the control experiment against the already-proven bare pull-up path.
    if unsafe { stop_after_gadget_handoff_stage(1) } {
        handoff_progress(2);
        return true;
    }
    unsafe { gate_flow_blip() }; // flow-map B1: reuse entry
    if cfg!(fullerene_aarch64_usb_gadget_handoff_reuse_fastboot_dma) {
        // Capture the address while Fastboot still owns the controller. The
        // no-SMMU differential deliberately preserves that firmware stream
        // mapping; changing the address to the linker section would defeat
        // this experiment before the first STARTTRANSFER.
        let event_address =
            (read_volatile(reg(GEVNTADRHI0)) as u64) << 32 | read_volatile(reg(GEVNTADRLO0)) as u64;
        if event_address == 0
            || event_address == u64::MAX
            // DWC3 event buffers require 16-byte alignment. Stock Bramble
            // XBL programs an address ending in 0x10, so requiring a page
            // boundary would reject the firmware-owned buffer and silently
            // force the caller into the non-reuse fallback path.
            || event_address & 0xf != 0
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
            handoff_progress(3);
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
    // Capture the working Fastboot RAM clock selection before the reuse
    // helper stops the old session or the device soft reset clears GCTL.
    // Zero is a valid RAMCLKSEL value, so validity is tracked separately.
    // Bisection (valid rungs only): pullup_mark(2) here was SILENT 4/4, while :7067 below is silent
    // and :7213 above is audible (4/4) - and the code between all three is microseconds, so those
    // results are consistent only if the marks are timing elapsed time rather than locating a step.
    // Eight edges instead of two: 2.4 s of attempts at the same unconditional site. A temporal
    // threshold lets the later edges through; if it stays silent, position or state is what matters.
    // pullup_mark_after(2000, 2) was tried here (2026-09-29 ~11:30) and gave rows=2/pulses=0. That
    // result is not usable as a test of a time threshold: the 2 s delay_ms is a busy wait that
    // re-asserts no RPMh vote, so it may itself kill the controller. Removed. This site is silent
    // with two plain edges (4/4), and :7213 above is audible (4/4), so the transition is inside
    // init_usb2_bare_pullup_handoff_inner(false) at :7167 - which is where the next mark goes.
    unsafe {
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
    // Android's msm_hsphy_init() first checks the DT-provided EUD ownership
    // status and returns without touching the PHY when EUD owns it. Capture
    // that source-defined gate before the optional rail refresh and before
    // the reset/init sequence below.
    #[cfg(fullerene_aarch64_usb_disable_eud)]
    {
        let eud_result = unsafe { disable_eud_for_usb_handoff() };
        log_hex("usb gadget handoff: EUD disable SCM result=", eud_result);
        if eud_result != 0 {
            log_puts("usb gadget handoff: EUD disable rejected; retaining EUD gate\n");
        } else {
            log_hex("usb gadget handoff: EUD CSR after disable=", unsafe {
                read_volatile((BRAMBLE_EUD_BASE + BRAMBLE_EUD_CSR_EUD_EN) as *const u32) as u64
            });
        }
    }
    let hsphy_eud_status = if cfg!(fullerene_aarch64_usb_gadget_handoff_hsphy_source_exact) {
        unsafe { super::super::platform::bramble::usb_hs_phy_eud_enabled() }
    } else {
        false
    };
    let hsphy_eud_enabled = hsphy_eud_status && !cfg!(fullerene_aarch64_usb_hsphy_ignore_eud);
    if hsphy_eud_status && cfg!(fullerene_aarch64_usb_hsphy_ignore_eud) {
        log_puts("usb gadget handoff: HS PHY EUD gate overridden for A/B\n");
    }
    #[cfg(fullerene_aarch64_usb_hsphy_eud_device_mode)]
    if hsphy_eud_enabled {
        // The official EUD-owned device branch refreshes the HS-PHY rails,
        // sets PWRDOWN_B, waits 50 ms, and skips the normal analog init.
        if unsafe { super::super::platform::bramble::refresh_usb_power(false) } {
            let pwrdown = unsafe { phy::enter_eud_device_mode() };
            log_hex(
                "usb gadget handoff: EUD device-mode PWRDOWN_CTRL=",
                u64::from(pwrdown),
            );
        } else {
            log_puts("usb gadget handoff: EUD device-mode rail refresh failed\n");
        }
    }
    // Android's msm_hsphy_init() calls msm_hsphy_enable_power(true)
    // before enabling the 19.2 MHz ref clock, asserting PHY reset, and
    // programming the analog registers. The normal non-destructive path
    // preserves Fastboot's rails; this opt-in A/B re-sends the exact three
    // QUSB2 rail enable requests before the direct reset/init boundary
    // without changing the DWC3 command or response path.
    #[cfg(fullerene_aarch64_usb_refresh_hsphy_power)]
    if !hsphy_eud_enabled {
        if !unsafe { super::super::platform::bramble::refresh_usb_power(false) } {
            log_puts("usb gadget handoff: HS PHY rail refresh failed\n");
            trace_event(TRACE_USB2_PHY_RESET, 0x5057_5246, 0, 0, 0, read(DSTS)); // "PWRF"
        }
    }
    #[cfg(fullerene_aarch64_usb_android_block_reset)]
    if !unsafe { super::super::platform::bramble::android_controller_block_reset() } {
        log_puts("usb gadget handoff: Android controller block reset failed\n");
        handoff_progress(4);
        return gadget_handoff_fail(2);
    }
    #[cfg(fullerene_aarch64_usb_android_block_reset)]
    trace_event(TRACE_DWC3_RESET_BEGIN, 0x424C_4B52, 0, 0, 0, 0); // "BLKR"
    // The Android msm probe enables the HS-PHY reference clock and performs
    // the external PHY reset/init before DWC3 CSFTRST.  The original
    // `HSPHY_BEFORE_RESET` A/B was wired only into the older
    // `init_with_super_speed` path; this direct Fastboot-reuse path is the
    // attach-reaching USB2 path used by the current probe.  Keep the ordering
    // opt-in and preserve the EUD ownership early return.
    // Bisection: pullup_mark(2) here was AUDIBLE 4/4 (2026-09-29 ~11:05). Window is now
    // (:7067, :7213), 146 lines.
    let hsphy_before_core_reset = cfg!(fullerene_aarch64_usb_hsphy_before_reset)
        && !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core);
    // qpr1 calls dwc3_phy_setup() before dwc3_core_soft_reset(). Preserve its
    // USB2 controller-side pre-reset state even when the A/B below chooses a
    // different external PHY ordering; the post-reset helper restores the
    // active endpoint-command state later.
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core) {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_source_resume_clocks)]
        {
            // qpr1's dwc3_msm_resume() restores the TCXO/domain and clock
            // dependency chain before dwc3_core_init() performs the DWC3
            // device reset. Keep this separate from the older post-reset
            // branch-rearm A/B so the source location is the variable under
            // test, not another EP0/TRB permutation.
            let performance_vote = if cfg!(fullerene_aarch64_usb_gadget_handoff_core_hs_clock) {
                super::super::platform::bramble::UsbBusVote::Svs
            } else {
                super::super::platform::bramble::UsbBusVote::Nominal
            };
            let ok = unsafe {
                super::super::platform::bramble::rearm_usb2_android_resume_clocks(performance_vote)
            };
            log_hex(
                "usb gadget handoff: qpr1 resume clock order=",
                u64::from(ok),
            );
        }
        // qpr1's dwc3_phy_setup() writes the USB3 PIPE policy before it
        // programs the USB2 interface and enters dwc3_core_soft_reset().
        // Preserve that source order as a USB2-only differential.
        unsafe { configure_usb31_phy_setup_pre_reset() };
        unsafe { configure_usb2_phy_interface_pre_reset() };
    }
    if hsphy_before_core_reset && !hsphy_eud_enabled {
        if !unsafe { super::super::platform::bramble::enable_usb_hs_phy_ref_clock() } {
            log_puts("usb gadget handoff: pre-reset HS PHY ref clock enable failed\n");
        }
        if !cfg!(fullerene_aarch64_usb_skip_usb2_phy_reset)
            && !unsafe { super::super::platform::bramble::pulse_usb2_phy_reset() }
        {
            log_puts("usb gadget handoff: pre-reset HS PHY BCR reset failed\n");
            trace_event(TRACE_USB2_PHY_RESET, 0x5052_4553, 0, 0, 0, read(DSTS)); // "PRES"
        }
        unsafe {
            if cfg!(fullerene_aarch64_usb_gadget_handoff_hsphy_source_exact) {
                init_hsphy_source_exact();
            } else {
                init_hsphy();
            }
        }
    }
    #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_source_peripheral_start)]
    {
        // qpr1's dwc3_otg_start_peripheral() performs this controller-side
        // start prefix before dwc3_gadget_pullup(true), whose device-core
        // reset comes next. Publish the VBUS/session override first; the
        // optional DBM reset below must sit between that write and
        // PRTCAP=DEVICE/dis_sleep_mode(), exactly as the qpr1 source does.
        if !cfg!(any(
            fullerene_aarch64_usb_dcfg_fullspeed,
            fullerene_aarch64_usb_dcfg_lowspeed,
            fullerene_aarch64_usb_no_ss_vbus
        )) {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        }
        set_direct_usb2_vbus_override();
    }
    #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_android_dbm_reset)]
    {
        // qpr1's dwc3_otg_start_peripheral() invokes
        // dwc3_msm_block_reset(false) after the VBUS/session and PHY setup,
        // before dwc3_set_prtcap()/dwc3_gadget_pullup(). Its false branch
        // does not assert the DWC3 core reset: it only resets/enables the
        // Qualcomm DBM. Keep this source boundary immediately before the
        // device-mode/sleep transition and the direct USB2 core reset.
        let dbm_ok = super::super::platform::bramble::android_dbm_reset_and_enable();
        log_hex(
            "usb gadget handoff: USB2 Android DBM reset/enable=",
            u64::from(dbm_ok),
        );
    }
    #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_source_peripheral_start)]
    {
        configure_dwc3_device_mode();
        let usb2 = read(GUSB2PHYCFG0);
        mark_g2w_site(1008);
        write(GUSB2PHYCFG0, usb2 & !GUSB2PHYCFG_ENBLSLPM);
        let guctl1 = read(GUCTL1);
        write(GUCTL1, guctl1 & !GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST);
        let _ = read(GUCTL1);
        // qpr1 applies this DWC31 controller-link timer after DBM/device
        // mode setup and before gadget VBUS connect. The helper is enabled
        // for this source-peripheral-start path even though this experiment
        // intentionally negotiates USB2 on the external cable.
        configure_usb31_lfps_exit_timer();
    }
    // Fastboot may have stopped Run/Stop, but that is not the same ownership
    // boundary as Linux's DWC3 probe.  The default path terminates its
    // endpoint-resource epoch with a device core soft reset.  The explicit
    // preserve-core differential keeps that reset out of the experiment while
    // retaining the preceding halted-controller boundary; this tests whether
    // the reset itself destroys the live Qualcomm PHY/session handoff.
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core) {
        // Bisection: pullup_mark(2) here was AUDIBLE 3/3 (2026-09-29 ~10:35), so the transition is
        // earlier still, above the reset branch. Window is now (handoff entry, here).
        let reset_ok = if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_full_core_reset) {
            // This is a broader Fullerene controller-domain reset A/B than
            // the ordinary DCTL.CSFTRST handoff reset: after the device-core
            // reset it asserts GCTL.CORESOFTRESET and the USB2 PHY-facing
            // soft-reset, then releases both after the local settle interval.
            // Keep this boundary isolated from the normal reuse path so its
            // marker and host result can be compared without changing the
            // endpoint or descriptor state.
            // Bisection point 2 (2026-09-29 09:16): pullup_mark(2) here was SILENT - two CCS rows
            // only, the working Run/Stop. Together with milestone 11 being audible, the window is
            // now bracketed to (:7323, :7874). Bisection point 3 moves to :7608, inside it.
            log_puts("usb gadget handoff: using full USB2 DWC3 core reset\n");
            trace_marker(TRACE_DWC3_RESET_BEGIN, 0x4643_5253); // "FCRS"
            let ok = unsafe { core_soft_reset(false) };
            trace_event(
                TRACE_DWC3_RESET_BEGIN,
                0x4643_5253,
                u32::from(ok),
                read(GCTL),
                read(GUSB2PHYCFG0),
                read(DSTS),
            );
            ok
        } else {
            unsafe { device_soft_reset() }
        };
        if !reset_ok {
            log_puts("usb gadget handoff: DWC3 device reset failed\n");
            handoff_progress(5);
            return gadget_handoff_fail(2); // core reset
        }
    } else {
        trace_marker(TRACE_DWC3_RESET_BEGIN, 0x50524553); // "PRES"
        log_puts("usb gadget handoff: preserving DWC3 core state\n");
    }
    #[cfg(fullerene_aarch64_usb_hsphy_restore_suspend_n_after_reset)]
    {
        // The source-exact pre-reset PHY init leaves raw SUSPEND_N asserted,
        // but DWC3 CSFTRST can overwrite the external PHY-facing state. Test
        // the same one-bit repair at the first post-reset boundary, before
        // endpoint resources and Run/Stop, instead of waiting until the host
        // has already attempted its first SETUP packet.
        log_puts("usb gadget handoff: restoring HS PHY SUSPEND_N after reset\n");
        let value = unsafe { phy::restore_suspend_n_after_runstop() };
        trace_event(TRACE_UTMI_STATE, 0x0200_0000, value, 0, 0, 0);
    }
    unsafe { gate_flow_blip() }; // flow-map B2: core reset done
    if unsafe { stop_after_gadget_handoff_stage(2) } {
        return false;
    }
    // 4.19 resume order: utmi_clk is enabled after core_clk and before any
    // controller start.  The SS-only fastboot session never raised the mock
    // UTMI branch, so bring it up at this post-reset boundary; the core
    // branch is already running under firmware.
    if cfg!(fullerene_aarch64_usb_gadget_handoff_clock_branches_rearm)
        && !unsafe { super::super::platform::bramble::rearm_usb2_android_clock_branches() }
    {
        log_puts("usb gadget handoff: Android controller clock branch rearm failed\n");
    }
    // Bisection: pullup_mark(2) here was AUDIBLE 3/3 (2026-09-29 ~10:25). Window is now
    // (device soft reset, here). The earlier rung at :7323 was invalid: it sat in the untaken
    // branch of `if cfg!(...usb2_full_core_reset)` (flag false), so it never executed. Re-placed
    // below, immediately before that runtime branch, where it runs unconditionally.
    if !unsafe { super::super::platform::bramble::enable_usb_hs_phy_ref_clock() } {
        log_puts("usb gadget handoff: RPMh HS PHY ref clock enable failed\n");
    }
    if !unsafe { super::super::platform::bramble::enable_usb2_utmi_clock() } {
        log_puts("usb gadget handoff: GCC mock UTMI clock enable failed\n");
        trace_event(TRACE_GCC_UTMI_CLOCK, 0, 0, 0, 0, read(DSTS));
    }
    trace_utmi_state(2);
    trace_dwc3_debug_stage(1);
    let clock_delay_us = usb_clock_stable_delay_us();
    if clock_delay_us != 0 {
        log_hex(
            "usb gadget handoff: clock stabilization delay us=",
            clock_delay_us as u64,
        );
        crate::timer::delay_us(clock_delay_us as u64);
        trace_utmi_state(8);
    }
    unsafe { configure_dwc3_global_control() };
    #[cfg(fullerene_aarch64_usb_hsphy_ref_after_gctl)]
    {
        // qpr1's dwc3_core_init() resumes the legacy USB2 PHY immediately
        // after global-control setup (after core reset, before endpoint
        // construction). The normal handoff already enables this clock
        // earlier; keep this exact-order A/B separate from the later
        // Run/Stop reassertion.
        let ok = unsafe { super::super::platform::bramble::enable_usb_hs_phy_ref_clock() };
        log_hex("usb gadget handoff: HS PHY ref post-GCTL=", u64::from(ok));
    }
    // qpr1's Qualcomm glue emits DWC3_CONTROLLER_POST_RESET_EVENT from the
    // core-init tail. For a USB2-only maximum speed it performs a short
    // UTMI-as-PIPE mux turn before the gadget endpoints are constructed.
    // The direct Fastboot reuse path does not enter that notifier, so keep
    // the source-exact boundary here after CSFTRST and before EP0 commands.
    unsafe {
        select_utmi_pipe_clock_post_reset();
    }
    // The halted-controller boundary above transfers DMA ownership from the
    // old Fastboot session.  Clear every linker-owned TRB/event/table object
    // only after that boundary, then seed the allocator used by a later
    // GSI/UDC bind; clearing it before the stop could race a final bootloader
    // DMA write.
    clear_dma_memory();
    unsafe {
        // msm-4.19 dwc3_core_setup_global_control() end state for this
        // core: device port, SCALEDOWN and DISSCRAMBLE off, clock gating
        // disabled (lito DT snps,disable-clk-gating), hibernation only on
        // HIB power-option cores. The previous code preserved whatever
        // SCALEDOWN/DISSCRAMBLE state the bootloader left behind.
        let mut gctl = read(GCTL);
        gctl &= !(GCTL_PRTCAPDIR_MASK | GCTL_SCALEDOWN_MASK | GCTL_DISSCRAMBLE);
        gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG;
        if (read(GHWPARAMS1) & GHWPARAMS1_EN_PWROPT_MASK) == GHWPARAMS1_EN_PWROPT_HIB {
            gctl |= GCTL_GBLHIBERNATIONEN;
        }
        write(GCTL, gctl);
        // qpr1's reset path leaves RAMCLKSEL at its reset value. The helper is
        // retained only for the explicitly named legacy differential.
        reapply_ramclksel();
        // CSFTRST restores the controller-side PHY mux/timing state on
        // DWC3 revisions used by Bramble. Reapply the Qualcomm controller
        // programming before any endpoint command. In the preserve-core
        // differential these writes are deliberately retained as the common
        // post-halt handoff sequence; only CSFTRST itself is omitted.
        // The direct Fastboot reuse path retains the historical controller
        // timing experiment here. The Bramble qpr1 source does not contain
        // this callback, so the preserve-state A/B can leave firmware's
        // REFCLKPER/GFLADJ values untouched. The qpr1-only A/B omits this
        // extra 100-us transition because its post-reset callback above is
        // the only UTMI/Pipe mux turn in the source path.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_qpr1_utmi_post_reset_only) {
            select_utmi_pipe_clock();
        } else {
            log_puts("usb gadget handoff: qpr1 post-reset UTMI mux only\n");
        }
        update_dwc3_ref_clock();
        let mut usb2 = read(GUSB2PHYCFG0);
        usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
        mark_g2w_site(1009);
        write(GUSB2PHYCFG0, usb2);
        let mut usb3 = read(GUSB3PIPECTL0);
        usb3 |= GUSB3PIPECTL_SUSPHY;
        write(GUSB3PIPECTL0, usb3);
        // The generic GCTL path above early-returns on this DWC_usb31 core,
        // so the 4.19 usb31 reference state is applied at this post-reset
        // boundary instead: GUSB2PHYCFG UTMI timing (dwc3_hs_phy_setup
        // steady state) plus the GUCTL1/GUCTL3/GSBUSCFG1 bits from
        // setup_global_control and __dwc3_gadget_start.
        configure_usb2_phy_interface();
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_post_endpoint_global) {
            apply_usb31_gadget_reference_deltas();
        }
        // Android msm's dwc3_set_mode(DEVICE) performs a second GCTL write
        // after selecting the device port. Preserve that controller-mode
        // tail before publishing EP0 resources; the source-peripheral-start
        // A/B already exercised this operation at qpr1's pre-CSFTRST
        // boundary, so do not add a second post-reset write there.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_peripheral_start) {
            configure_dwc3_device_mode();
        }
    }
    // An SS-only fastboot session never deasserted the femto PHY block
    // reset (GCC_QUSB2PHY_PRIM_BCR), which can leave the PHY core logic held
    // in reset while the D+/D- IO state machine still answers the host reset
    // autonomously.  The 4.19 phy-core deasserts `phy_reset` before
    // `snps_hsphy_init`; pulse the USB2-only line here, before the pull-up
    // is asserted, so the host port stays unattached throughout.
    if hsphy_eud_enabled {
        // Bisection point 4 (2026-09-29 09:22): pullup_mark(2) here was SILENT - one CCS pair only.
        // Window tightened from (:7323, :7613) to (:7481, :7614), 133 lines. Bisection point 5 moves
        // to just before phy::set_normal_opmode(), which is the leading candidate for the transition.
        log_puts("usb gadget handoff: HS PHY EUD enabled; preserving PHY state\n");
    } else if hsphy_before_core_reset {
        log_puts("usb gadget handoff: HS PHY reset/init already performed before DWC3 reset\n");
    } else if !cfg!(fullerene_aarch64_usb_skip_usb2_phy_reset) {
        if !unsafe { super::super::platform::bramble::pulse_usb2_phy_reset() } {
            log_puts("usb gadget handoff: USB2 PHY BCR reset failed\n");
            trace_event(TRACE_USB2_PHY_RESET, 0, 0, 0, 0, read(DSTS));
        }
    } else {
        log_puts("usb gadget handoff: skipping USB2 PHY BCR reset A/B\n");
        trace_event(TRACE_USB2_PHY_RESET, 0x534B_4950, 0, 0, 0, read(DSTS));
    }
    // DWC3's device reset does not reset the external Femto PHY.  Reapply the
    // Linux USB2 PHY programming at the same post-reset boundary as the normal
    // Qualcomm glue path; the GCC/Type-C power-domain (core) reset stays
    // untouched, only the USB2 PHY BCR line above is pulsed.
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_preserve_core)
        && !hsphy_eud_enabled
        // In the source-ordered A/B the external PHY init already happened
        // before DWC3 CSFTRST. qpr1 calls usb_phy_set_suspend(0) after the
        // reset, which resumes clocks but does not run msm_hsphy_init() a
        // second time. Re-running the analog init here would erase the very
        // ordering distinction this option is meant to test.
        && !hsphy_before_core_reset
    {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_hsphy_source_exact)]
        unsafe {
            init_hsphy_source_exact()
        };
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_hsphy_source_exact))]
        unsafe {
            init_hsphy()
        };
    }
    #[cfg(fullerene_aarch64_usb_hsphy_normal_opmode)]
    unsafe {
        // Bisection point 5 (2026-09-29 09:24): pullup_mark(2) here was SILENT. Window tightened
        // from (:7481, :7614) to (:7522, :7619), 97 lines. Bisection point 6 moves just before
        // phy::clear_datapath_override(), splitting what is left.
        let opmode = phy::set_normal_opmode();
        log_hex(
            "usb gadget handoff: HS PHY normal OPMODE=",
            u64::from(opmode),
        );
    }
    #[cfg(fullerene_aarch64_usb_hsphy_clear_power_down)]
    if !hsphy_eud_enabled {
        unsafe {
            let pwrdown = phy::clear_power_down();
            log_hex(
                "usb gadget handoff: HS PHY PWRDOWN_B cleared, PWRDOWN_CTRL=",
                u64::from(pwrdown),
            );
        }
    }
    #[cfg(fullerene_aarch64_usb_hsphy_clear_datapath_override)]
    unsafe {
        // Bisection point 6 (2026-09-29 09:26): pullup_mark(2) here was SILENT. Window tightened
        // to (:7542, :7621), 79 lines, leaving three candidates. Bisection point 7 goes just before
        // pulse_auto_resume().
        let cfg0 = phy::clear_datapath_override();
        log_hex(
            "usb gadget handoff: HS PHY datapath override cleared, CFG0=",
            u64::from(cfg0),
        );
    }
    #[cfg(fullerene_aarch64_usb_hsphy_auto_resume_pulse)]
    unsafe {
        // Android's cable-connected msm_hsphy_set_suspend() path pulses
        // CTRL2.AUTO_RESUME for 500--1000 us before resuming the PHY. Keep
        // this RX/SOF candidate after the final HS-PHY init and before the
        // USB2 controller contract/pull-up, where it cannot be masked by a
        // later reset epoch.
        // Bisection point 7 (2026-09-29 09:28): pullup_mark(2) here was SILENT. Window tightened
        // to (:7558, :7624), 66 lines, two candidates. Bisection point 8 goes between them.
        let state = phy::pulse_auto_resume();
        log_hex(
            "usb gadget handoff: HS PHY auto-resume pulse CTRL2=",
            u64::from(state),
        );
        trace_event(TRACE_UTMI_STATE, 0x0400_0000, state, 0, 0, 0);
    }
    // The external QUSB2 PHY reset/init above can restore the DWC3-side USB2
    // interface register to its reset value.  Re-apply the UTMI contract
    // after that boundary, immediately before endpoint resources are built;
    // otherwise the source-level TRDTIM=9 write made before init_hsphy() is
    // absent from the actual stage-3 readback and the core advertises a
    // pull-up without a usable USB2 transaction interface.
    // Bisection (executed rungs only). pullup_mark(2) here was AUDIBLE 3/3 (2026-09-29 ~10:15), so
    // the window is now (:7481, :7572) and would have been (:7481, :7658) before. Bisection moves up
    // into the DWC3 core reset / clock region, which is the only substantive code in the bracket.
    unsafe { configure_usb2_phy_interface() };
    #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_source_susphy)]
    unsafe {
        // qpr1's dwc3_phy_setup() leaves SUSPHY asserted through
        // endpoint/resource construction. The Android endpoint-command
        // helper clears it only for the command and restores it after
        // completion; wire the same source-order differential into the
        // direct Fastboot-reuse path (the regular init_with_super_speed path
        // already has this A/B).
        let usb2 = read(GUSB2PHYCFG0);
        // Snapshot the register *before* the write; `usb2` is the read-modify
        // source, so it also tells us whether the read path returns anything at
        // all.
        SUSPHY_RAW_BEFORE = usb2;
        mark_g2w_site(1010);
        write(GUSB2PHYCFG0, usb2 | GUSB2PHYCFG_SUSPHY);
        // Bisection point 8 (2026-09-29 09:31): pullup_mark(2) here was SILENT. Window tightened
        // to (:7589, :7624), 35 lines. Neither pulse_auto_resume nor the SUSPHY read-modify-write
        // is the transition. Bisection point 9 goes just before the SMMU decision at :7621.
        // Bisection point 8's site, run three times consecutively (2026-09-29 ~09:39): rows=2,
        // pulses=0 on all three. Stable, not a coin flip - so the transition above is deterministic
        // and the "time only" reading was too strong. Bisection point 10 now isolates the last two
        // candidates by marking immediately after the GUSB2PHYCFG0 read.
        // Line 7158 already read the register back and threw the value away.
        // Keep it instead: if the readback does not show SUSPHY set immediately
        // after the write, then every `GUSB2PHYCFG0`-derived CCS word in this
        // project (`susphy`, `gphycfg_lo`, `gphycfg_hi`) is reading a broken
        // path rather than reporting a state - and the "SUSPHY is 0" finding is
        // an instrumentation defect, not a hardware fact.
        let after = read(GUSB2PHYCFG0);
        SUSPHY_RAW_AFTER = after;
        SUSPHY_SET_IN_HANDOFF = after & GUSB2PHYCFG_SUSPHY != 0;
        // Bisection point 10 (2026-09-29 09:42): pullup_mark(2) here was SILENT, and so was the
        // rung at :7594 three times over. Both are silent while :7623 is audible, yet no compiled
        // code lies between them - the usb2_dis_sleep_mode block in between is cfg-disabled.
        //
        // The resolution is that pullup_mark itself costs time. Each edge is a drop held 150 ms
        // followed by a rise held 150 ms, so pullup_mark(2) spans 600 ms of attempts. That makes
        // the three failing/succeeding marks a stopwatch rather than three code positions:
        //
        //     :7594   attempts from ~0 ms to ~600 ms after the SUSPHY write   all lost
        //     :7607   attempts from ~600 ms to ~1200 ms                       all lost
        //     :7623   attempts from ~1200 ms to ~1800 ms                      these reach the host
        //
        // Marks tried at this exact site, 2026-09-29:
        //   pullup_mark(2)   -> rows=2, pulses=0   (three consecutive runs, all silent)
        //   pullup_mark(8)   -> rows=2, pulses=0   (2.4 s of attempts, nothing reported)
        // With a second mark placed at :7651 (after the cfg block) the pair produced rows=4, i.e.
        // exactly one of the two fired. An 8-edge train that is silent while a 2-edge train 600 ms
        // later is audible does NOT fit a settling-time story - a settling delay would have been
        // crossed partway through the long train. It fits a host-side one instead: xhci/the hub
        // debounce or lock out a port after several connect/disconnect cycles in quick succession,
        // so edges issued in a burst may simply never be reported.
        //
        // That matters for every rung of this bisection. A silent rung may mean "the controller
        // could not raise a pull-up" OR "the host did not report it", and this instrument cannot
        // tell those apart on its own. The ladder below is therefore conditional on the host having
        // reported the edge - it is not yet a proven statement about the controller.
    }
    #[cfg(all(
        fullerene_aarch64_usb_gadget_handoff_usb2_dis_sleep_mode,
        not(fullerene_aarch64_usb_gadget_handoff_usb2_source_peripheral_start)
    ))]
    unsafe {
        // qpr1's dwc3_otg_start_peripheral() clears these sleep controls
        // before dwc3_gadget_pullup(), and therefore before the DWC3 device
        // core reset and EP0 command epoch. The normal direct handoff only
        // clears them transiently around endpoint commands; keep this exact
        // pre-reset boundary as an explicit USB2 A/B.
        let usb2 = read(GUSB2PHYCFG0);
        mark_g2w_site(1011);
        write(GUSB2PHYCFG0, usb2 & !GUSB2PHYCFG_ENBLSLPM);
        let guctl1 = read(GUCTL1);
        write(GUCTL1, guctl1 & !GUCTL1_L1_SUSP_THRLD_EN_FOR_HOST);
    }
    // The Fastboot session may have left the DWC3 stream behind an SMMU
    // mapping that only covers its own buffers.  Our TRBs/event ring are
    // intentionally identity-addressed in the 0x9b800000 DMA section.  Keep
    // the proven PHY/pull-up transition first, then install the identity map
    // before handing any new DMA object to DWC3.
    // Bisection point 9 (2026-09-29 09:33) placed a mark here and it WAS audible, once. Repeating
    // that position three times on 2026-09-29 ~09:50 gave rows=2/pulses=0 every time, and the mark
    // inside the unsafe block just above (:7620) is also silent 3/3. The only difference between
    // the two sites is the cfg-gated block between them, which is disabled in these runs - so the
    // two positions should be identical at runtime. This call re-tests point 9's exact position
    // with the other mark still in place, so one run decides it: four CCS rows means both fired
    // (the block is irrelevant and the earlier audible result was an outlier), two rows means only
    // this one fired (something about the position does matter).
    // CFG AUDIT (2026-09-29 10:05): the 34-line bracket recorded earlier was an artifact. Rungs sat
    // inside #[cfg] blocks disabled in these builds (usb_hsphy_normal_opmode,
    // usb_hsphy_clear_datapath_override, usb_hsphy_auto_resume_pulse, usb2_source_susphy), so those
    // marks were never compiled in and their silence meant nothing. Rungs that genuinely executed:
    // :6679 silent, :7323 silent, :7481 silent, :7658 audible (here), :7889 audible. The real window
    // is (:7481, :7658), about 177 lines. See the ledger.
    let smmu_ready = if cfg!(fullerene_aarch64_usb_gadget_handoff_no_smmu) {
        // Differential mode for a Fastboot-owned bypass: do not even read
        // the Apps-SMMU registers. The DMA section remains fixed inside the
        // declared Bramble pool, so this mode is valid only when firmware
        // leaves the DWC3 stream in physical=IOVA bypass.
        // Bisection point 3 (2026-09-29 09:19): pullup_mark(2) here WAS audible - four CCS rows,
        // the mark's own pair 0.265 s apart on top of the working Run/Stop. Window tightened from
        // (:7323, :7874) to (:7323, :7613). Bisection point 4 moves to :7468, inside that.
        log_puts("usb gadget handoff: Apps SMMU untouched\n");
        true
    } else {
        configure_dwc3_smmu()
    };
    if !smmu_ready {
        log_puts("usb gadget handoff: DWC3 SMMU pool map unavailable\n");
        handoff_progress(7);
        return gadget_handoff_fail(3); // SMMU
    }
    unsafe { gate_flow_blip() }; // flow-map B3: SMMU ready
    if unsafe { stop_after_gadget_handoff_stage(3) } {
        handoff_progress(8);
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
        core::ptr::write_bytes(
            ep0_trb_ptr(0).cast::<u8>(),
            0,
            2 * core::mem::size_of::<Trb>(),
        );
        core::ptr::write_bytes(ep0_response_ptr(), 0, 512);
        cache_clean(ep0_event_dma_base(), event_size);
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
            event_size as u32,
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
        write(
            DCFG,
            if cfg!(fullerene_aarch64_usb_dcfg_lowspeed) {
                DCFG_LOWSPEED
            } else if cfg!(fullerene_aarch64_usb_dcfg_fullspeed) {
                DCFG_FULLSPEED
            } else if cfg!(fullerene_aarch64_usb_dcfg_superspeed) {
                DCFG_SUPERSPEED
            } else {
                DCFG_HIGHSPEED
            },
        );
        configure_gadget_start_defaults();
        if option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT") == Some("usb2-live-ep0-armed-order") {
            // Bit 1 of 2. Recorded just *before* the gadget-start branch, so the
            // readout can tell "never reached this epoch" apart from "reached it
            // and took the false branch". See `ENDPOINT_CONFIG_BLOCK_REACHED` for
            // bit 2 and for why these are recorded rather than pulsed here.
            ENDPOINT_CONFIG_EPOCH_REACHED = true;
            handoff_progress(9);
        }
        if cfg!(fullerene_aarch64_usb_gadget_handoff_gadget_start_only_at_runstop) {
            // qpr1's dwc3_gadget_run_stop(true) performs __dwc3_gadget_start
            // only inside the final start boundary. Leave the initial
            // endpoint/resource epoch empty; restart_gadget_at_runstop()
            // below will publish it exactly once after the optional core
            // reset and event-buffer setup.
            log_puts("usb gadget handoff: deferring all EP0 start state to Run/Stop\n");
            write(DALEPENA, 0);
            ENDPOINTS_READY = false;
        } else {
            // Linux enables each endpoint only after its SETEPCONFIG and
            // SETTRANSFRESOURCE commands complete. Do not advertise EP0 before
            // the controller has accepted the corresponding resource state.
            write(DALEPENA, 0);
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
                // qpr1 walks the endpoint objects created from GHWPARAMS3's
                // num_eps field immediately after DEPSTARTCFG and before any
                // SETEPCONFIG. Mirror that available-endpoint range rather than
                // issuing commands to the unused physical endpoint slots.
                for endpoint in 0..qpr1_endpoint_count() as u32 {
                    if !set_transfer_resource(endpoint as usize) {
                        log_puts("usb gadget handoff: Android resource preallocation failed\n");
                        return gadget_handoff_fail(5); // resource allocation
                    }
                }
            }
            // The direct reuse entry has already selected DCFG High-Speed. Keep
            // EP0 at the USB2 maximum packet size unless the explicit
            // Linux/Android initial-512 A/B is requested; using the SuperSpeed
            // value unconditionally leaves a High-Speed core with a mismatched
            // control context before Connect Done can modify it.
            let ep0_packet_size = if cfg!(fullerene_aarch64_usb_ep0_initial_512) {
                INITIAL_EP0_MAX_PACKET_SIZE
            } else {
                64
            };
            if !unsafe {
                configure_endpoint_config(0, ep0_packet_size, DEPCFG_EP_TYPE_CONTROL, false, 0)
            } {
                log_puts("usb gadget handoff: USB2 EP0 OUT configure failed\n");
                return gadget_handoff_fail(5); // EP0 config
            }
            if stop_after_gadget_handoff_stage(9) {
                return false;
            }
            // This path intentionally uses the config-only helper above so the
            // Qualcomm sequence remains a single SETEPCONFIG ->
            // SETTRANSFRESOURCE pair for EP0 OUT. The outer allocation is
            // required here; unlike configure_endpoint(), the config-only helper
            // does not allocate the transfer resource itself.
            if !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
                && !cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
                && !set_transfer_resource(0)
            {
                log_puts("usb gadget handoff: USB2 EP0 OUT resource failed\n");
                return gadget_handoff_fail(5); // EP0 resource
            }
            if stop_after_gadget_handoff_stage(10) {
                return false;
            }
            // XBL's DwcConfigureEP publishes the corresponding DALEPENA bit
            // after each SETEPCONFIG -> SETTRANSFRESOURCE pair.
            write(DALEPENA, read(DALEPENA) | (1 << 0));
            trace::live_dalepena_config(0, read(DALEPENA));
            #[cfg(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0)]
            {
                // Stock XBL inserts the initial EP0 OUT request immediately after
                // the OUT pair and before configuring the EP0 IN direction. Keep
                // this as a separate ordering A/B; the explicit setup payload is
                // retained because the earlier XBL zero/self-buffer tests did not
                // attach on this handoff path.
                if !queue_xbl_setup_request() {
                    log_puts("usb gadget handoff: XBL inter-pair request queue failed\n");
                    return gadget_handoff_fail(5);
                }
                prepare_ep0_setup_trb();
                if !start_transfer(0, ep0_trb_ptr(0)) {
                    log_puts("usb gadget handoff: XBL inter-pair SETUP STARTTRANSFER failed\n");
                    return gadget_handoff_fail(12);
                }
                let slot = EP0_SETUP_REQUEST_SLOT;
                if slot == usize::MAX || !udc_mut().start(0, slot) {
                    log_puts("usb gadget handoff: XBL inter-pair request start failed\n");
                    return gadget_handoff_fail(5);
                }
                EP0_SETUP_ARMED = true;
            }
            HANDOFF_MILESTONE = 1;
            // Stage 8 isolates the first SETEPCONFIG/SETTRANSFRESOURCE pair from
            // the corresponding EP0 IN pair. It is intentionally appended to the
            // original 1..7 sequence so existing stage numbers remain stable.
            HANDOFF_MILESTONE = 4;
            if stop_after_gadget_handoff_stage(8) {
                return false;
            }
            HANDOFF_MILESTONE = 5;
            if !configure_endpoint(1, ep0_packet_size, false) {
                log_puts("usb gadget handoff: USB2 EP0 configure failed\n");
                return gadget_handoff_fail(5); // EP0 config
            }
            HANDOFF_MILESTONE = 6;
            // XBL's DwcConfigureEP publishes the corresponding DALEPENA bit
            // after each SETEPCONFIG -> SETTRANSFRESOURCE pair. Keep the two
            // physical EP0 directions on the same per-direction boundary.
            write(DALEPENA, read(DALEPENA) | (1 << 1));
            trace::live_dalepena_config(1, read(DALEPENA));
            // Stock XBL writes exactly 0x47 here: Disconnect, USB Reset, Connect
            // Done, and Suspend. Keep the narrower mask limited to the
            // event-driven XBL differential; the generic path retains its
            // broader lifecycle notifications.
            #[cfg(fullerene_aarch64_usb_gadget_handoff_xbl_post_endpoint_global)]
            {
                // Stock XBL applies the usb31 global deltas only after both EP0
                // SETEPCONFIG -> SETTRANSFRESOURCE pairs and after endpoint
                // publication. Keep this register-order differential
                // isolated from the endpoint/request A/Bs.
                apply_usb31_gadget_reference_deltas();
            }
            ENDPOINTS_READY = true;
            HANDOFF_MILESTONE = 7;
            handoff_progress(10);
            // ORDERED ONE-BIT DISAMBIGUATION (see the ledger's OPEN CONTRADICTION
            // entry): publish one pulse the instant this assignment executes, so a
            // single run can separate three cases that `usb2-live-ep0-armed` alone
            // cannot - "this else branch was entered", "it was entered and the flag
            // survived", and "it was entered but something cleared it before the
            // readout". The matching second pulse is in the
            // `usb2-live-ep0-armed-order` selector. Inert for every other build, so
            // it cannot perturb an existing run.
            if option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT")
                == Some("usb2-live-ep0-armed-order")
            {
                // Record, do not pulse. A pulse here would be unobservable: this
                // runs while the controller is still halted, and the pulse channel
                // works by cycling DCTL Run/Stop so the host re-logs the attach -
                // which only produces a line once the port is live. Publish both
                // bits together at the readout instead (ordered, one run).
                ENDPOINT_CONFIG_BLOCK_REACHED = true;
            }
            HANDOFF_MILESTONE = 11;
            // Bisection point 1 (2026-09-29 09:14): pullup_mark(2) here WAS audible - usbmon
            // recorded four CCS rows, two of them the mark's own edges 0.273 s apart (the 150 ms
            // holds), while dmesg showed only one attach line because a 0.17 s connection never
            // enumerates. So the window is already open at milestone 11, and it is closed at
            // handoff entry. Bisection point 2 moves to the DWC3 core reset, in between.
            HANDOFF_MILESTONE = 16;
            let _ = udc_mut().configure_endpoint(0, 64, false);
            let _ = udc_mut().configure_endpoint(1, 64, false);
            // Both EP0 directions, their resources, and DALEPENA have now been
            // published. This is the endpoint-config boundary, still before
            // the final Run/Stop transition.
            trace_dwc3_debug_stage(2);
            if cfg!(any(
                fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
                fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
            )) {
                // Stock XBL queues this request before Run/Stop, after both EP0
                // directions have their transfer resources.
                if !queue_xbl_setup_request() {
                    log_puts("usb gadget handoff: XBL EP0 request queue failed\n");
                    return gadget_handoff_fail(5);
                }
            }
            if stop_after_gadget_handoff_stage(5) {
                return false;
            }
            if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0) {
                trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, read(DSTS));
                prepare_ep0_setup_trb();
            }
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_stall_flush)]
            {
                // Keep the stall-flush differential effective when the proven
                // Fastboot-reuse handoff is selected as the primary path too.
                let _ = send_ep_command(0, DEPCMD_SETSTALL, 0, 0, 0);
                trace_event(
                    TRACE_SETUP_QUEUED,
                    0x5354_4C46, // "STLF"
                    0,
                    0,
                    0,
                    read(DSTS),
                );
            }
            HANDOFF_MILESTONE = 12;
            apply_ep0_txfifo_fix();
            // Stage 11 isolates the cache-cleaned SETUP buffer/TRB publication
            // from the DWC3 STARTTRANSFER command itself. The old stage 6
            // combined both operations, so a failure there could not tell us
            // whether the DMA object or the command latch was the boundary.
            if stop_after_gadget_handoff_stage(11) {
                return false;
            }
            // On Bramble, a STARTTRANSFER issued while the device is still
            // disconnected can complete with No Resource even after
            // SETTRANSFRESOURCE returned index 1. The timing A/Bs move this
            // exact command across the Run/Stop/link boundary; the default keeps
            // the historical pre-connect command for comparison.
            HANDOFF_MILESTONE = 13;
            let defer_initial_setup = cfg!(any(
                fullerene_aarch64_usb_gadget_handoff_start_after_connect,
                fullerene_aarch64_usb_gadget_handoff_start_after_reset,
                fullerene_aarch64_usb_gadget_handoff_start_at_connect_done
            ));
            if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0) {
                // The inter-pair XBL differential armed the setup TRB above.
            } else if defer_initial_setup {
                PENDING_SETUP_ARM = true;
            } else if !start_transfer(0, ep0_trb_ptr(0)) {
                log_puts("usb gadget handoff: SETUP STARTTRANSFER failed\n");
                return gadget_handoff_fail(12); // STARTTRANSFER
            }
            HANDOFF_MILESTONE = 15;
            // "M15D" - UPSTREAM of every reader. `HANDOFF_MILESTONE` itself is a `static mut`
            // and therefore exists twice across the two `usb/` compilations, so a readout that
            // wants to know "did 15 execute" cannot trust the variable; this goes to the
            // retained trace, which both crates read. Placed here (not near the readout) so
            // that a reader inside the readout block is *downstream* of it. See usb/README.md
            // §3.17.
            trace_marker(TRACE_PROBE_WATCHDOG, 0x4D31_3544); // "M15D"
            screen_mark(2); // milestone 15
            // Record the armed SETUP TRB so the USB-reset handler takes the
            // Linux-equivalent keep-the-TRB path instead of tearing it down and
            // racing the host's first post-reset SETUP token.
            if !cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0) {
                EP0_SETUP_ARMED = !defer_initial_setup;
            }
            if !cfg!(fullerene_aarch64_usb_gadget_handoff_direct)
                || cfg!(fullerene_aarch64_usb_probe_irq_controller)
            {
                enable_gadget_controller_irq();
            }
            // Linux enables the DWC3 event interrupt immediately after arming the
            // EP0 OUT SETUP TRB. Select the device-event mask at that boundary;
            // the deferred Bramble profile publishes it after its U0 retry has
            // actually armed STARTTRANSFER, while the non-deferred path writes
            // it immediately below. The probe owns no asynchronous IRQ path
            // yet, so drain the ring synchronously before the final Run/Stop.
            if !cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect) {
                write(DEVTEN, direct_gadget_devten());
            }
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
        }

        if !cfg!(any(
            fullerene_aarch64_usb_dcfg_fullspeed,
            fullerene_aarch64_usb_dcfg_lowspeed,
            fullerene_aarch64_usb_no_ss_vbus
        )) {
            qscratch_set(QSCRATCH_SS_PHY_CTRL, 1 << 24);
        }
        set_direct_usb2_vbus_override();
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_source_devten_before_runstop)]
        {
            // qpr1's __dwc3_gadget_start() enables DEVTEN immediately after
            // arming the initial EP0 OUT SETUP transfer and before the final
            // Run/Stop write. The attach-reaching `start-after-connect` A/B
            // defers STARTTRANSFER, but must not silently defer the device
            // event mask as well: USB Reset/Connect Done events are generated
            // only while their DEVTEN bits are enabled. Keep this as one
            // explicit ordering differential; the post-arm write below is
            // retained so the selected mask is identical at both boundaries.
            write(DEVTEN, direct_gadget_devten());
            let _ = read(DEVTEN);
        }
        // Bramble's DT declares maximum-speed = "super-speed". The Android
        // msm start path keeps that DCFG speed even when the negotiated link
        // later falls back to USB2; the EP0 context is changed to 64 bytes by
        // Connect Done. Keep this source-derived speed choice opt-in because
        // the existing attach-reaching baseline used a USB2 DCFG value.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_gadget_speed_after_restart) {
            configure_gadget_speed(cfg!(fullerene_aarch64_usb_dcfg_superspeed));
        }
        #[cfg(fullerene_aarch64_usb_usb2_core_reset_at_runstop)]
        {
            // qpr1's dwc3_gadget_pullup(true) always performs this device
            // core reset immediately before dwc3_gadget_run_stop(true).
            // Reproduce that boundary for the direct USB2 handoff; the
            // restart helper below rebuilds EP0 after the reset.
            // qpr1's preceding dwc3_phy_setup() leaves the USB2 PHY wake
            // bit asserted until dwc3_gadget_run_stop() clears it around
            // DCTL.RUN_STOP. Preserve that reset-time state here; the
            // shared Run/Stop helper performs the temporary clear later.
            enable_usb2_gadget_susphy();
            if !device_soft_reset() {
                log_puts("usb gadget handoff: USB2 pre-Run/Stop reset failed\n");
                return gadget_handoff_fail(7);
            }
            configure_dwc3_device_mode();
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_event_ring_at_runstop)]
        {
            // Android msm republishes the event buffer in
            // dwc3_gadget_run_stop(true), immediately before the gadget is
            // restarted and Run/Stop is asserted. Move that publication
            // boundary into the attach-reaching USB2 reuse path; the earlier
            // timing A/B only exercised the non-attaching fallback path.
            republish_ep0_event_ring_at_runstop();
        }
        #[cfg(any(
            fullerene_aarch64_usb_gadget_handoff_gadget_restart_at_runstop,
            fullerene_aarch64_usb_usb2_core_reset_at_runstop
        ))]
        if !restart_gadget_at_runstop(cfg!(fullerene_aarch64_usb_dcfg_superspeed)) {
            // Android's restart helper is best-effort from the caller's
            // perspective: Run/Stop is still attempted, and the host-visible
            // attach distinguishes a restart failure from a PHY failure.
            log_puts("usb gadget handoff: Android gadget restart incomplete\n");
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_susphy)]
        unsafe {
            // Pixel's __dwc3_gadget_start() restores USB2 SUSPHY after the
            // endpoint/SETUP start and immediately before the gadget
            // Run/Stop transition. Keep the existing direct-path A/B at that
            // same final boundary; the SuperSpeed path has its corresponding
            // placement below.
            enable_usb2_gadget_susphy();
        }
        // Gate runs skip the DEVCTRLHLT readback wait (up to 2 s on a stale
        // halt) so the handoff returns to the probe inside the biter window;
        // the stale-halt case is exactly what the diag rescue re-arm fixes.
        let gate_run = option_env!("FULLERENE_USB_PROBE_SINGLE_ATTEMPT") == Some("1");
        trace_utmi_state(4);
        trace_dwc3_debug_stage(3);
        if let Some(selector) = option_env!("FULLERENE_USB_UTMI_PRECONNECT_READOUT") {
            // Publish the live UTMI field through the physical attach time,
            // before the host can submit the first SETUP. This short delay
            // avoids the long post-readout park, which is truncated by the
            // handset's recovery watchdog on large selector values.
            let code =
                utmi_readout_code(selector).min(if selector == "utmi-gdb-link" { 16 } else { 15 });
            trace_event(TRACE_UTMI_STATE, 0x0400_0000 | code, code, 0, 0, 0);
            let delay_ms = if selector == "soffn-control" {
                // Control for the `usb2-live-soffn-count` RX claim: sample
                // DSTS.SOFFN twice while the pull-up is still down, i.e. with no
                // host traffic at all. A free-running counter advances here; a
                // received-SOF latch does not. The answer rides the attach
                // latency, which is the one channel that works before the
                // pull-up (1 s = static, 4 s = advanced).
                // `DSTS.SOFFN` is bits 16:3 - the vendor's
                // `DWC3_DSTS_SOFFN_MASK` (`0x3fff << 3`, `core.h:487`). An
                // earlier version of this control masked `0x3fff` (bits 13:0),
                // which overlaps CONNECTSPD and the bits below the field, so
                // its "advanced/static" verdict was not about SOFFN at all.
                const SOFFN_MASK: u32 = 0x3fff << 3;
                let before = (read(DSTS) & SOFFN_MASK) >> 3;
                readout_keepalive_delay_ms(500);
                let after = (read(DSTS) & SOFFN_MASK) >> 3;
                log_hex(
                    "usb gadget handoff: pre-attach SOFFN before=",
                    u64::from(before),
                );
                log_hex(
                    "usb gadget handoff: pre-attach SOFFN after=",
                    u64::from(after),
                );
                if after != before { 4_000 } else { 1_000 }
            } else if selector == "hsphy-suspend-n-safe" {
                // 1 = missing, 2 = present/0, 3 = present/1.
                match code {
                    2 => 0,
                    3 => 4_000,
                    _ => 8_000,
                }
            } else {
                u64::from(code) * 1_000
            };
            readout_keepalive_delay_ms(delay_ms);
        }
        trace_marker(TRACE_PROBE_WATCHDOG, 0x4D41_3141); // "MA1" - before the DALEPENA read
        trace::live_dalepena_before_dctl(read(DALEPENA));
        #[cfg(fullerene_aarch64_usb_gadget_handoff_min_runstop_delay)]
        {
            // qpr1's dwc3_gadget_pullup(true) enforces a minimum 50 ms
            // stop-to-start interval before advertising the gadget again.
            // The direct Fastboot handoff has a tighter reset/reconnect
            // boundary, so keep this source-derived timing differential
            // immediately before the production Run/Stop write.
            log_puts("usb gadget handoff: qpr1 minimum Run/Stop delay 50ms\n");
            super::super::timer::delay_ms(50);
        }
        screen_mark(4); // about to run the Run/Stop that raises the pull-up
        let start_readback_ok = if gate_run {
            unsafe { run_stop_device_no_readback(true) }
        } else {
            unsafe { run_stop_device(true) }
        };
        screen_mark(6); // Run/Stop returned (the host now sees the attach)
        trace_marker(TRACE_PROBE_WATCHDOG, 0x4D41_3241); // "MA2" - Run/Stop returned
        #[cfg(fullerene_aarch64_usb_hsphy_power_after_runstop)]
        {
            // The normal rail refresh belongs to the PHY reset/init epoch.
            // This opt-in repeats only the Android qpr1 HS-PHY regulator
            // enables at the first post-Run/Stop receive boundary, covering
            // a possible analog-rail collapse that a 500 ms keepalive would
            // observe only after the link had already missed its first setup.
            let ok = unsafe { super::super::platform::bramble::refresh_usb_power(false) };
            log_hex(
                "usb gadget handoff: HS PHY rails after Run/Stop=",
                u64::from(ok),
            );
            trace_event(TRACE_UTMI_STATE, 0x0600_0003, u32::from(ok), 0, 0, 0);
        }
        #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_clear_susphy_after_runstop)]
        {
            // run_stop_device() follows qpr1's guard and restores the saved
            // USB2 SUSPHY bit after the DCTL transition. Keep this A/B at the
            // first post-Run/Stop receive boundary: a live pull-up with no
            // DSTS SOF progress is consistent with the PHY being returned to
            // suspend before the host's first setup packet.
            log_puts("usb gadget handoff: clearing USB2 SUSPHY after Run/Stop\n");
            let usb2 = unsafe { read(GUSB2PHYCFG0) & !GUSB2PHYCFG_SUSPHY };
            unsafe {
                mark_g2w_site(1012);
                write(GUSB2PHYCFG0, usb2);
                let _ = read(GUSB2PHYCFG0);
            }
            trace_event(TRACE_UTMI_STATE, 0x0600_0002, usb2, 0, 0, 0);
        }
        #[cfg(fullerene_aarch64_usb_hsphy_restore_suspend_n_after_runstop)]
        {
            // qpr1's msm_hsphy_init() leaves raw SUSPEND_N asserted while
            // clearing only SUSPEND_N_SEL. The pre/post readouts localized
            // the Bramble transition to the final Run/Stop boundary, so this
            // A/B restores that one source-defined bit before EP0 traffic.
            log_puts("usb gadget handoff: restoring HS PHY SUSPEND_N after Run/Stop\n");
            let value = unsafe { phy::restore_suspend_n_after_runstop() };
            trace_event(TRACE_UTMI_STATE, 0x0600_0000, value, 0, 0, 0);
        }
        trace_marker(TRACE_PROBE_WATCHDOG, 0x4D41_3341); // "MA3" - PHY rails restored
        #[cfg(fullerene_aarch64_usb_hsphy_restore_suspend_n_selected_after_runstop)]
        {
            // qpr1's init uses the selector while asserting SUSPEND_N, then
            // clears only the selector. This opt-in A/B tests that exact
            // ownership sequence after the Bramble Run/Stop transition.
            log_puts("usb gadget handoff: restoring selected HS PHY SUSPEND_N after Run/Stop\n");
            let value = unsafe { phy::restore_suspend_n_selected_after_runstop() };
            trace_event(TRACE_UTMI_STATE, 0x0600_0001, value, 0, 0, 0);
        }
        // Capture the controller's immediate post-Run/Stop free-space state
        // before any optional post-boundary PHY or clock differential.
        trace_dwc3_debug_stage(4);
        #[cfg(fullerene_aarch64_usb_hsphy_ref_after_runstop)]
        {
            // The downstream msm_hsphy resume path re-enables the only
            // Bramble HS-PHY clock at the connect/resume boundary.  Exercise
            // that source-defined operation once more after DWC3 Run/Stop,
            // where a direct handoff differs from the Android gadget path.
            let ok = unsafe { super::super::platform::bramble::enable_usb_hs_phy_ref_clock() };
            log_hex(
                "usb gadget handoff: HS PHY ref after Run/Stop=",
                u64::from(ok),
            );
        }
        if option_env!("FULLERENE_USB_UTMI_REAPPLY_AFTER_RUNSTOP") == Some("1") {
            // Diagnostic only: Android/Linux program GUSB2PHYCFG before
            // gadget start. If the stage-3/4 value is cleared or ignored by
            // the final Run/Stop transition on this handoff, one immediate
            // post-Run/Stop write tells us whether the register can be
            // adopted at all. Do not change the normal path without the
            // explicit A/B environment flag.
            log_puts("usb gadget handoff: re-applying USB2 interface after Run/Stop\n");
            if option_env!("FULLERENE_USB_UTMI_REAPPLY_HALTED") == Some("1") {
                // Some DWC3 revisions lock GUSB2PHYCFG timing fields while
                // Run/Stop is asserted. Reapply only at a halted boundary,
                // then start the device again before the first host SETUP.
                unsafe {
                    let _ = run_stop_device(false);
                    configure_usb2_phy_interface();
                    let _ = run_stop_device(true);
                }
            } else {
                unsafe { configure_usb2_phy_interface() };
            }
        }
        if option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") == Some("hsphy-por-clear-after-runstop") {
            // Readout established that the post-Run/Stop HS-PHY POR bit is
            // still asserted on this handoff.  Clear only that source-defined
            // active-high reset bit, after the normal Run/Stop boundary and
            // before the host's first SETUP.  This is an opt-in recovery A/B;
            // all other PHY fields and the endpoint path remain unchanged.
            log_puts("usb gadget handoff: clearing HS PHY POR after Run/Stop\n");
            hsphy_update(HSPHY_UTMI_CTRL5, HSPHY_UTMI_POR, 0);
            let _ = read_volatile(hsphy_reg(HSPHY_UTMI_CTRL5));
            super::super::timer::delay_us(100);
        }
        trace_utmi_state(5);
        trace_dwc3_debug_stage(4);
        trace_marker(TRACE_PROBE_WATCHDOG, 0x4D41_3441); // "MA4" - pre-readout boundary
        // Early positive control for the pulse channel. Decode it from usbmon CCS, not attach
        // lines. It validates the carrier at this checkpoint only; after-enumeration markers may
        // be invisible to the host observer, so a missing later pulse remains unknown.
        if matches!(
            option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
            Some("2") | Some("4")
        ) {
            unsafe { ccs_pulse(300) };
        }
        if let Some(selector) = option_env!("FULLERENE_USB_UTMI_POSTRUN_READOUT") {
            // Encode the post-Run/Stop stage in the attach timestamp. This
            // is deliberately separate from the pre-connect readout so a
            // run without the guarded re-apply can show whether Run/Stop
            // itself cleared or altered the UTMI contract.
            let code =
                utmi_readout_code(selector).min(if selector == "utmi-gdb-link" { 16 } else { 15 });
            trace_event(TRACE_UTMI_STATE, 0x0500_0000 | code, code, 0, 0, 0);
            if selector == "usb2-live-park-calib" {
                // Transport calibration: park a fixed 60 s and let the host read
                // the Android-return time. A return near the natural ~26 s after
                // the attach means the park was preempted or the path was not
                // reached; a return near park + Android boot means the park
                // channel can carry a post-Run/Stop word.
                park_for_seconds(60);
            } else if selector.starts_with("usb2-live-park-") {
                // Publish the word through the probe's own PSCI reset time:
                // the value rides as (10 + code*8) seconds of park, and the
                // host reads it from the Android-return time. This transport
                // needs nothing from the DWC3, so it still works when the
                // controller cannot drive a pull-up transition at all - which
                // is the state run `188479.0` showed, where the coded
                // stop/run pairs produced no second host attach.
                park_for_seconds(12 + u64::from(code) * 6);
            } else if selector.starts_with("usb2-live-blip-") {
                // The pull-up is already asserted at this point, so a delay
                // here cannot move any host timestamp: run `178863.0` used the
                // 0/4/8 s `hsphy-suspend-n-safe` delay at this site and the
                // attach, the address-0 timeout, and the Android return were
                // all unchanged. Publish the value through a fresh stop/run
                // pair instead: the host sees a new high-speed attach whose
                // latency carries the code. Two pairs are emitted so a missing
                // second attach is interpretable - the fixed marker pair first
                // (mechanism/self-test), then the coded pair. A zero-pair
                // result is itself the readout: the controller could not drive
                // the transition, which is the dead-core signature.
                let _ = unsafe { run_stop_device(false) };
                readout_keepalive_delay_ms(300);
                let _ = unsafe { run_stop_device(true) };
                readout_keepalive_delay_ms(1_000);
                let _ = unsafe { run_stop_device(false) };
                readout_keepalive_delay_ms(300 + u64::from(code) * 1_000);
                let _ = unsafe { run_stop_device(true) };
            } else if selector == "usb2-live-speed-hs" {
                // Two bits about the controller's own view of the link, sampled
                // after the host's port reset (the marker is the delay).
                //   pulse 1: DSTS.USBLNKST == 0 ("On"). For a USB 2.0 device the
                //            core decodes tokens only in the On state; the probe
                //            has recorded non-On states (4/6/7/10/12/13) while
                //            the host was already issuing tokens, and run
                //            `261322.0` showed the core never hands a SETUP to
                //            the EP0 buffer at all.
                //   pulse 2: DSTS.CONNECTSPD == 0 (high speed). The speed is
                //            latched during a USB reset, so this says whether
                //            the core observed the host's reset.
                // Field positions are the vendor's (core.h):
                //   DWC3_DSTS_USBLNKST_MASK = 0x0f << 18
                //   DWC3_DSTS_CONNECTSPD    = 7 << 0
                let dsts = read(DSTS);
                if (dsts >> 18) & 0x0f == 0 {
                    unsafe { ccs_pulse(300) };
                }
                if dsts & DSTS_CONNECTSPD_MASK == 0 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-ep0-armed" {
                // Is the EP0 OUT endpoint actually armed? Run `264554.0`
                // established that the core is in the normal link state
                // (DSTS.USBLNKST == 0, "On") and latched high speed, i.e. it did
                // observe the host's reset - yet it never hands the host's SETUP
                // to the EP0 buffer (`261322.0`). If no transfer TRB is queued on
                // EP0 OUT the core has nowhere to put the packet, which would
                // explain both the empty buffer and the empty event ring.
                //   pulse 1: EP0_SETUP_ARMED (the handoff's Start Transfer on
                //            EP0 OUT succeeded).
                //   pulse 2: ENDPOINTS_READY (the endpoint configuration phase
                //            completed, without which `try_arm_setup` returns
                //            early and never arms anything).
                // Settle first: a pulse issued before the wait produces no host
                // line at all (measured - see the note below the second pulse).
                readout_keepalive_delay_ms(500);
                if EP0_SETUP_ARMED {
                    unsafe { ccs_pulse(300) };
                }
                // PRECONDITION, measured: the pulse only reaches the host when it
                // is preceded by a settle wait. `soffn-count` (delay 500 ms, then
                // pulse) gives 2 lines; `delay-only` (delay, no pulse) and
                // `ep0-armed` (pulse, no delay) both give 1. So the wait is not
                // decoration - without it the Run/Stop cycle produces nothing.
                readout_keepalive_delay_ms(500);
                if ENDPOINTS_READY {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-endpoints-ready" {
                // ONE predicate only, so the count is unambiguous even if two
                // closely-spaced pulses merge into one host line.
                //   1 line  = ENDPOINTS_READY is FALSE
                //   2 lines = ENDPOINTS_READY is TRUE
                readout_keepalive_delay_ms(500);
                if ENDPOINTS_READY {
                    unsafe { ccs_pulse(300) };
                }
            } else if {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x4D44_3141);
                selector == "usb2-live-ms-ge-11"
            } {
                readout_keepalive_delay_ms(500);
                if unsafe { HANDOFF_MILESTONE } >= 11 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-ms-ge-13" {
                readout_keepalive_delay_ms(500);
                if unsafe { HANDOFF_MILESTONE } >= 13 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-ms-ge-15" {
                readout_keepalive_delay_ms(500);
                if unsafe { HANDOFF_MILESTONE } >= 15 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-ms-ge-16" {
                readout_keepalive_delay_ms(500);
                if unsafe { HANDOFF_MILESTONE } >= 16 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-ms-ge-14" {
                readout_keepalive_delay_ms(500);
                if unsafe { HANDOFF_MILESTONE } >= 14 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-arm-attempted" {
                // PRE-WINDOW snapshot: this selector block runs after Run/Stop,
                // but before milestone 20 and before the arm loop below. A zero
                // pulse means only that SETUP_ARM_FAILURE_STAGE is still 4 here;
                // it says nothing about whether the later loop calls
                // `try_arm_setup`. The pulse itself toggles Run/Stop, so it is an
                // intervention, not a passive reachability witness.
                readout_keepalive_delay_ms(500);
                if unsafe { SETUP_ARM_FAILURE_STAGE } != 4 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-arm-halted" {
                // Same pre-window snapshot: stage 1 records an earlier DSTS
                // halt result; it does not classify the upcoming arm window.
                readout_keepalive_delay_ms(500);
                if unsafe { SETUP_ARM_FAILURE_STAGE } == 1 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-ep0state-setup" {
                // Pre-window EP0_STATE snapshot. It can describe an input to the
                // later guard, but cannot prove that the arm loop was entered.
                readout_keepalive_delay_ms(500);
                if EP0_STATE == Ep0State::Setup {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-armed-only" {
                // Pre-window EP0_SETUP_ARMED snapshot; with deferred arming,
                // false is expected before the arm loop runs.
                readout_keepalive_delay_ms(500);
                if EP0_SETUP_ARMED {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-ep0-armed-order" {
                // Both bits, published together at a moment when the port is live,
                // in order. The first half is recorded rather than pulsed at the
                // point of truth (see `ENDPOINT_CONFIG_BLOCK_REACHED`) because a
                // pulse there would be invisible.
                //   2 pulses = the config block ran AND ENDPOINTS_READY survived
                //   1 pulse  = the config block ran, then something cleared the flag
                //   0 pulses = the config block was never entered
                if unsafe { ENDPOINT_CONFIG_EPOCH_REACHED } {
                    unsafe { ccs_pulse(300) };
                }
                if unsafe { ENDPOINT_CONFIG_BLOCK_REACHED } {
                    unsafe { ccs_pulse(300) };
                }
                if ENDPOINTS_READY {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-handoff-progress" {
                // SINGLE BIT ONLY. The multi-pulse version of this readout was
                // retracted: `usb2-live-pulse-calibration` showed four pulses produce
                // one attach line, so counts are not readable on this channel.
                //
                // So this publishes exactly one pulse, and only for the one question
                // worth one run: **was `init_usb2_gadget_reuse_fastboot_ep0` entered
                // at all?** Everything else about the route has to be bisected one
                // bit per run (see the retraction entry in the skill).
                if unsafe { HANDOFF_ENTERED } {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-pulse-calibration" {
                // CALIBRATION of the channel itself, one pulse only.
                //
                // History: the SOFFN work established the channel with a *single*
                // pulse and A/B/B/A. A later four-pulse version of this selector
                // produced ONE attach line, which at first looked like "multi-pulse is
                // broken". But the one-bit `usb2-live-handoff-progress` run also
                // produced one line while the source says that function must be
                // entered - so the more likely reading is that *nothing* is being
                // published at this moment, and the four-pulse result was a red
                // herring.
                //
                // So issue exactly ONE unconditional pulse here. Expected: 2 attach
                // lines (1 boot attach + 1 pulse).
                //   2 lines => the channel works and the 0-pulse readings are evidence
                //   1 line  => nothing is published from this point; every zero-count
                //              reading this session is void, including "both flags
                //              false" and "the function was not entered"
                unsafe { ccs_pulse(300) };
            } else if selector == "usb2-live-force-endpoints" {
                // RE-ENUMERATION ATTEMPT with the root cause addressed.
                // Run `266423.0` measured ENDPOINTS_READY == false at about
                // attach + 1.2 s, i.e. while the host was already asking for the
                // device descriptor. `ENDPOINTS_READY = true` and
                // `DALEPENA = 0b11` are set in exactly one place - the "Connect
                // Done" event handler (`usb.rs:4790-4800`) - so with no device
                // events the core never gets an enabled control endpoint, and a
                // disabled EP0 explains every observation at once: the host's
                // SETUP is not accepted, nothing is written into the EP0 buffer
                // (`261322.0`) and no event is posted (`225813.0`).
                // So configure the control endpoints here, with the controller
                // stopped, in Linux's own order (`dwc3_gadget_start` issues its
                // endpoint commands before `DCTL.RUN_STOP`), then run.
                // The bundled `poll_setup_buffer()` (see its selector check)
                // answers the host without waiting for an event.
                unsafe {
                    let _ = run_stop_device(false);
                    let ok = configure_endpoint(0, 64, false) && configure_endpoint(1, 64, false);
                    if ok {
                        ENDPOINTS_READY = true;
                        write(DALEPENA, 0b11);
                    }
                    let _ = run_stop_device(true);
                    // Run `268613.0` (configuration only) moved the host boundary
                    // from a 5.2 s silence (`-110`) to an immediate protocol
                    // error (`-71`): the core now decodes the SETUP and stalls it,
                    // which is what an enabled EP0 with no queued transfer TRB
                    // does. Run `270244.0` showed that calling `try_arm_setup()`
                    // here is the wrong way to fix that - it blocked for longer
                    // than the 5.9 s host reset in its retry loop, losing the
                    // carrier pulses and reverting the boundary to `-110`.
                    // The handoff's own arm window already knows how to arm EP0;
                    // it only skips the work because `EP0_SETUP_ARMED` is still
                    // set from before the reconfiguration and `try_arm_setup`
                    // returns early on that flag. So just clear it and let the
                    // existing arm window do the arming.
                    EP0_SETUP_ARMED = false;
                    EP0_STATE = Ep0State::Setup;
                    if ok {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-rearm-loop" {
                // Eventless EP0 bring-up that mirrors Linux's own arming
                // lifecycle. Vendor facts (qpr1):
                //   * `dwc3_gadget_start` enables EP0/EP1 and immediately calls
                //     `dwc3_ep0_out_start()` (`gadget.c:2505-2520`);
                //   * every EP0 transfer completion re-arms it
                //     (`ep0.c:1003-1004`: `dwc->ep0state = EP0_SETUP_PHASE;
                //     dwc3_ep0_out_start(dwc);`);
                //   * `dwc3_ep0_out_start` refuses to arm while `softconnect` is
                //     false (`ep0.c:300-301`).
                // A pending transfer is flushed by the host's bus reset, so
                // arming once before that reset - which is what the handoff's
                // 400 ms arm window does, because its loop stops on
                // `EP0_SETUP_ARMED` - leaves EP0 OUT with no TRB when the host's
                // SETUP arrives ~60 ms later. That is exactly the `-71` stall
                // measured in runs `268613.0`/`271489.0`.
                // So: configure the control endpoints, then keep re-arming until
                // the deadline, answering every SETUP straight out of the EP0
                // buffer with no device event involved. `try_arm_setup` itself
                // skips the arm once the state machine leaves the Setup phase, so
                // this cannot fight the response path.
                unsafe {
                    let _ = run_stop_device(false);
                    let ok = configure_endpoint(0, 64, false) && configure_endpoint(1, 64, false);
                    if ok {
                        ENDPOINTS_READY = true;
                        write(DALEPENA, 0b11);
                    }
                    let _ = run_stop_device(true);
                    EP0_STATE = Ep0State::Setup;
                    let deadline = arch_counter()
                        .saturating_add(arch_counter_frequency().saturating_mul(2_500) / 1_000);
                    let mut answered = 0u32;
                    while arch_counter() < deadline {
                        EP0_SETUP_ARMED = false;
                        ARM_COOLDOWN = 0;
                        let _ = try_arm_setup();
                        poll_ep0_event_ring();
                        if poll_setup_buffer() {
                            answered = answered.saturating_add(1);
                        }
                        super::super::timer::delay_us(20_000);
                    }
                    if ok {
                        ccs_pulse(300);
                    }
                    if answered != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-dma-addr" {
                // Address-type audit for the DMA programming.
                // `--no-smmu` deliberately keeps Fastboot's SMMU stream mapping
                // (the harness flag description and `usb.rs:6012-6016` say so), so
                // the controller's DMA is *translated*. But `dma_iova_for()`
                // returns an IOVA only when `DMA_ADOPTED` is set and otherwise
                // returns the raw CPU address, which is identity-mapped physical
                // memory. A physical address programmed into the event ring or
                // the EP0 buffers while the SMMU translates would send the
                // core's writes elsewhere (or fault), which is exactly the shape
                // of the empty event ring (`225813.0`) and the empty SETUP
                // buffer (`261322.0`).
                //   pulse 1: DMA_ADOPTED - the handoff adopted a Fastboot IOVA window
                //   pulse 2: programmed GEVNTADR0 equals ep0_event_dma_base()
                //   pulse 3: dma_iova_for(EP0 SETUP buffer) equals the raw pointer
                unsafe {
                    if DMA_ADOPTED {
                        ccs_pulse(300);
                    }
                    let programmed =
                        ((read(GEVNTADRHI0) as u64) << 32) | u64::from(read(GEVNTADRLO0));
                    if programmed == ep0_event_dma_base() as u64 {
                        ccs_pulse(300);
                    }
                    let setup = ep0_setup_data_ptr() as usize;
                    if dma_iova_for(setup) == setup as u64 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-trb-hwo" {
                // Where does the EP0 OUT transfer actually break? Vendor layout
                // (`core.h:829-836`): the TRB's `size` field carries the transfer
                // status in bits 31:28 (`DWC3_TRB_SIZE_TRBSTS`), with
                // `DWC3_TRBSTS_SETUP_PENDING = 2`; `ctrl` bit 0 is `HWO`.
                //   pulse 1: TRB.ctrl still has HWO - the controller never
                //            consumed the transfer TRB.
                //   pulse 2: TRBSTS == 2 - the controller received a SETUP into
                //            this TRB (so the arm and the address are right and
                //            the data must be in the TRB's buffer).
                //   pulse 3: TRBSTS == 0 - the transfer completed cleanly.
                // Sampled about a second after attach, i.e. after the host's
                // first GET_DESCRIPTOR has been answered or stalled.
                unsafe {
                    let trb = ep0_trb_ptr(0);
                    cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                    let ctrl = read_volatile(addr_of!((*trb).ctrl));
                    let size = read_volatile(addr_of!((*trb).size));
                    let trbsts = (size >> 28) & 0x0f;
                    if ctrl & TRB_HWO != 0 {
                        ccs_pulse(300);
                    }
                    if trbsts == 2 {
                        ccs_pulse(300);
                    }
                    if trbsts == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-trb-buf" {
                // The controller reports SETUP_PENDING (`usb2-live-trb-hwo`,
                // run `278916.0`), so the host's SETUP did reach the EP0 OUT
                // transfer - yet `poll_setup_buffer()` never saw a non-zero
                // packet. That leaves exactly one question: does the armed TRB
                // point at the buffer the software reads?
                //   pulse 1: TRB.bpl/bph equals dma_iova_for(ep0_setup_data_ptr())
                //            - i.e. the core was told to deposit the SETUP where
                //            the software looks.
                //   pulse 2: the first byte of that buffer is non-zero *when read
                //            here*, which also tests the cache maintenance path
                //            (`cache_invalidate`) the poll uses.
                unsafe {
                    let trb = ep0_trb_ptr(0);
                    cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                    let bpl = u64::from(read_volatile(addr_of!((*trb).bpl)));
                    let bph = u64::from(read_volatile(addr_of!((*trb).bph)));
                    let trb_buffer = (bph << 32) | bpl;
                    let setup = ep0_setup_data_ptr();
                    if trb_buffer == dma_iova_for(setup as usize) {
                        ccs_pulse(300);
                    }
                    cache_invalidate(setup as usize, 8);
                    if read_volatile(setup) != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-ep0-eventless" {
                // The complete eventless EP0 control loop, now that the causal
                // chain is measured:
                //   * run `278916.0`: the core reports `TRBSTS == 2`
                //     (`SETUP_PENDING`) with `HWO` still set - it received the
                //     host's SETUP and is holding it;
                //   * run `280408.0`: the armed TRB points exactly at
                //     `ep0_setup_data_ptr()` (pulse 1), yet that buffer is still
                //     zero (pulse 2) - so the packet is *not* deposited until
                //     software acts;
                //   * vendor: `dwc3_ep0_out_start()` is called on the
                //     transfer-completion path (`ep0.c:1003-1004`) and from the
                //     reset path (`ep0.c:265`), i.e. a fresh Start Transfer is
                //     how Linux makes the core release the pending SETUP.
                // So: configure the control endpoints, arm once, then poll the
                // TRB's own status word (`core.h:832`: bits 31:28) as the
                // eventless equivalent of the completion event, re-arming when
                // the core reports a pending SETUP - and rate-limiting the
                // "transfer completed" re-arm, because the earlier unbounded
                // loop (`274800.0`) wedged the endpoint by hammering Start
                // Transfer.
                unsafe {
                    let _ = run_stop_device(false);
                    let ok = configure_endpoint(0, 64, false) && configure_endpoint(1, 64, false);
                    if ok {
                        ENDPOINTS_READY = true;
                        write(DALEPENA, 0b11);
                    }
                    let _ = run_stop_device(true);
                    EP0_STATE = Ep0State::Setup;
                    EP0_SETUP_ARMED = false;
                    ARM_COOLDOWN = 0;
                    let _ = try_arm_setup();
                    let frequency = arch_counter_frequency();
                    let deadline =
                        arch_counter().saturating_add(frequency.saturating_mul(4_000) / 1_000);
                    let mut last_arm = arch_counter();
                    let mut pending_seen = 0u32;
                    let mut answered = 0u32;
                    while arch_counter() < deadline {
                        let now = arch_counter();
                        let trb = ep0_trb_ptr(0);
                        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                        let size = read_volatile(addr_of!((*trb).size));
                        let trbsts = (size >> 28) & 0x0f;
                        if trbsts == 2 {
                            pending_seen = pending_seen.saturating_add(1);
                        }
                        // A pending SETUP needs software action immediately; a
                        // retired transfer is re-armed at a bounded rate so the
                        // endpoint is ready for the next control request.
                        let retired = trbsts == 0
                            && now.saturating_sub(last_arm) >= frequency.saturating_mul(50) / 1_000;
                        if trbsts == 2 || retired {
                            EP0_SETUP_ARMED = false;
                            ARM_COOLDOWN = 0;
                            let _ = try_arm_setup();
                            last_arm = now;
                            if poll_setup_buffer() {
                                answered = answered.saturating_add(1);
                            }
                        }
                        poll_ep0_event_ring();
                        super::super::timer::delay_us(1_000);
                    }
                    if ok {
                        ccs_pulse(300);
                    }
                    if pending_seen != 0 {
                        ccs_pulse(300);
                    }
                    if answered != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-ep0-release" {
                // Minimal eventless control loop - change NOTHING about the
                // handoff's own configuration. Runs `268613.0`/`271489.0` showed
                // a stop/reconfigure/run cycle makes EP0's Start Transfer time
                // out afterwards (`281888.0`: four seconds of loop, no carrier
                // pulses, `-110`), and the handoff's own arm is already good:
                // `278916.0` measured `TRBSTS == 2` (`SETUP_PENDING`) with `HWO`
                // set, i.e. the controller received the host's SETUP and is
                // holding it, waiting for software.
                // Vendor: Linux releases a pending SETUP by re-issuing Start
                // Transfer (`dwc3_ep0_out_start`, `ep0.c:1003-1004`), and it
                // guards re-issuing with `DWC3_EP_TRANSFER_STARTED`, so one
                // re-arm per observed pending SETUP is the faithful equivalent.
                unsafe {
                    let frequency = arch_counter_frequency();
                    let deadline =
                        arch_counter().saturating_add(frequency.saturating_mul(2_500) / 1_000);
                    let mut arms = 0u32;
                    let mut pending_seen = 0u32;
                    let mut answered = 0u32;
                    let mut last_status = 0xffu32;
                    while arch_counter() < deadline {
                        let trb = ep0_trb_ptr(0);
                        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                        let size = read_volatile(addr_of!((*trb).size));
                        let trbsts = (size >> 28) & 0x0f;
                        if trbsts == 2 {
                            pending_seen = pending_seen.saturating_add(1);
                            if last_status != 2 && arms < 8 {
                                EP0_SETUP_ARMED = false;
                                ARM_COOLDOWN = 0;
                                let _ = try_arm_setup();
                                arms = arms.saturating_add(1);
                            }
                        }
                        if poll_setup_buffer() {
                            answered = answered.saturating_add(1);
                        }
                        poll_ep0_event_ring();
                        last_status = trbsts;
                        super::super::timer::delay_us(1_000);
                    }
                    if pending_seen != 0 {
                        ccs_pulse(300);
                    }
                    if answered != 0 {
                        ccs_pulse(300);
                    }
                    if arms != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-setup-trb" {
                // Vendor fact that explains the empty buffer: Linux reads the
                // SETUP packet straight out of the TRB memory -
                // `dwc3_ep0_inspect_setup()`: `struct usb_ctrlrequest *ctrl =
                // (void *) dwc->ep0_trb;` (`ep0.c:866`) - and prepares its SETUP
                // TRB with `dwc3_ep0_prepare_one_trb(dep, dwc->ep0_trb_addr, 8,
                // ...)` (`ep0.c:304`). The controller therefore deposits the
                // 8-byte packet over the TRB, while this handoff points its TRB
                // at a separate buffer and reads *that* (`ep0_setup_data_ptr()`).
                // That is why run `280408.0` found the TRB pointing exactly at
                // the software's buffer yet empty, even though `278916.0`
                // measured `TRBSTS == 2` (`SETUP_PENDING`): the packet is sitting
                // in the TRB, not in the buffer.
                // Bridge it: when the TRB holds a plausible SETUP, copy those 8
                // bytes into the buffer the existing control state machine reads
                // and let `handle_setup()` answer the host. No reconfiguration,
                // no re-arm - both of those were shown to wedge EP0.
                // Counters go out as one pulse per unit so they stay readable.
                unsafe {
                    let frequency = arch_counter_frequency();
                    let deadline =
                        arch_counter().saturating_add(frequency.saturating_mul(2_500) / 1_000);
                    let mut seen = 0u32;
                    let mut answered = 0u32;
                    while arch_counter() < deadline {
                        let trb = ep0_trb_ptr(0);
                        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                        let size = read_volatile(addr_of!((*trb).size));
                        let trbsts = (size >> 28) & 0x0f;
                        if trbsts == 2 || trbsts == 0 {
                            let packet = trb.cast::<u8>();
                            if read_volatile(packet) != 0 {
                                seen = seen.saturating_add(1);
                                let setup = ep0_setup_data_ptr();
                                if setup != packet {
                                    core::ptr::copy_nonoverlapping(packet, setup, 8);
                                    cache_clean(setup as usize, 8);
                                }
                                if poll_setup_buffer() {
                                    answered = answered.saturating_add(1);
                                }
                            }
                        }
                        poll_ep0_event_ring();
                        super::super::timer::delay_us(1_000);
                    }
                    // ONE bit only. While EP0 is in this state each ccs_pulse
                    // takes over a second (runs `283357.0`/`285720.0` produced
                    // 2.5-2.9 s wide pulses instead of 300 ms), so counts and
                    // widths are not readable - but the *presence* of a pulse
                    // is. A pulse here means the TRB held a non-zero packet,
                    // i.e. the controller really did deposit the host's SETUP
                    // over the TRB as `ep0.c:866` implies.
                    if seen != 0 {
                        ccs_pulse(300);
                    }
                    let _ = answered;
                }
            } else if {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x4D44_3241);
                selector == "usb2-live-ep0-restart"
            } {
                // Linux's recipe for a control endpoint holding a pending SETUP
                // is `dwc3_ep0_stall_and_restart()` (`ep0.c:243-266`): stall EP0,
                // which retires the pending transfer, reset the state to the
                // setup phase, then `dwc3_ep0_out_start()` to re-arm.
                // Runs `283357.0`/`285720.0` failed because they re-armed
                // *without* retiring the pending transfer - a busy EP0 rejects
                // Start Transfer, so each arm burned its timeout, the loops
                // overshot and nothing was delivered. Run `287333.0` then showed
                // the packet is not waiting in the TRB either, so retiring the
                // pending transfer first is the step that was missing.
                unsafe {
                    let frequency = arch_counter_frequency();
                    let deadline =
                        arch_counter().saturating_add(frequency.saturating_mul(2_000) / 1_000);
                    let mut restarts = 0u32;
                    let mut answered = 0u32;
                    while arch_counter() < deadline {
                        let trb = ep0_trb_ptr(0);
                        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                        let size = read_volatile(addr_of!((*trb).size));
                        let ctrl = read_volatile(addr_of!((*trb).ctrl));
                        let trbsts = (size >> 28) & 0x0f;
                        let pending = trbsts == 2 || ctrl & TRB_HWO != 0;
                        if pending && restarts < 8 {
                            // The vendor's restart also re-establishes the
                            // endpoint-enable bookkeeping on both directions
                            // (`dep->flags = DWC3_EP_ENABLED`), which in this
                            // kernel is `ENDPOINTS_READY` plus `DALEPENA`.
                            // Run `266423.0` measured `ENDPOINTS_READY == false`
                            // in the handoff, so that part of the recipe was
                            // never satisfied.
                            ENDPOINTS_READY = true;
                            write(DALEPENA, 0b11);
                            // 1. stall EP0 so the controller retires the pending SETUP
                            let _ = send_ep_command(0, DEPCMD_SETSTALL, 0, 0, 0);
                            // 2. back to the setup phase, then re-arm (vendor order)
                            EP0_SETUP_ARMED = false;
                            ARM_COOLDOWN = 0;
                            EP0_STATE = Ep0State::Setup;
                            let _ = try_arm_setup();
                            restarts = restarts.saturating_add(1);
                            // 3. and now look for the packet
                            if poll_setup_buffer() {
                                answered = answered.saturating_add(1);
                            }
                        }
                        poll_ep0_event_ring();
                        super::super::timer::delay_us(1_000);
                    }
                    // ONE bit per run: a pulse means at least one SETUP was answered.
                    if answered != 0 {
                        ccs_pulse(300);
                    }
                    let _ = restarts;
                }
            } else if selector == "usb2-live-dma-window" {
                // Is the DMA address translation valid for the *software's own*
                // buffers? `adopt_smmu_dma_mapping()` (`smmu.rs:334-374`) adopts
                // exactly ONE live SMMU page (`DMA_ADOPTED_CPU` physical,
                // `DMA_ADOPTED_IOVA` its IOVA), and `dma_iova_for(cpu)` then
                // computes `DMA_ADOPTED_IOVA + (cpu - DMA_ADOPTED_CPU)`.
                // Any buffer outside that single page therefore gets a
                // linearly-offset address that the SMMU does not map - so the
                // controller's DMA for it lands somewhere else (or faults).
                // The event ring is fine, because its address came from the
                // firmware and *is* inside the adopted page; the EP0 SETUP buffer,
                // the EP0 TRBs and the response buffer are linker objects and are
                // not. That would explain a received SETUP whose payload appears
                // nowhere the software can read.
                //   pulse 1: EP0 TRB pointer lies within the adopted page
                //   pulse 2: EP0 SETUP buffer lies within the adopted page
                //   pulse 3: DMA_ADOPTED_IOVA == DMA_ADOPTED_CPU (identity)
                unsafe {
                    let page = DMA_ADOPTED_CPU & !0xfff;
                    let inside = |p: usize| p >= page && p < page + 0x1000;
                    if inside(ep0_trb_ptr(0) as usize) {
                        ccs_pulse(300);
                    }
                    if inside(ep0_setup_data_ptr() as usize) {
                        ccs_pulse(300);
                    }
                    if DMA_ADOPTED_IOVA == DMA_ADOPTED_CPU as u64 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-setup-inpage" {
                // THE FIX TEST. `ep0_setup_data_ptr()` now returns
                // `ep0_trb_ptr(1)` (inside the one adopted SMMU page) instead of
                // the linker-allocated `EP0_SETUP_BUFFER` (outside it), so
                // `prepare_ep0_setup_trb()` points the controller at mapped
                // memory. The TRB must be re-prepared for the new pointer, and
                // the pending transfer must be retired first or Start Transfer
                // times out (`283357.0`/`285720.0`) - hence the vendor's
                // stall-then-re-arm order (`ep0.c:243-266`).
                // ONE bit: a pulse means the SETUP was read from the in-page
                // buffer and answered.
                unsafe {
                    let frequency = arch_counter_frequency();
                    let deadline =
                        arch_counter().saturating_add(frequency.saturating_mul(2_000) / 1_000);
                    let mut answered = 0u32;
                    while arch_counter() < deadline {
                        let trb = ep0_trb_ptr(0);
                        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                        let ctrl = read_volatile(addr_of!((*trb).ctrl));
                        let size = read_volatile(addr_of!((*trb).size));
                        let pending = ((size >> 28) & 0x0f) == 2 || ctrl & TRB_HWO != 0;
                        if pending {
                            let _ = send_ep_command(0, DEPCMD_SETSTALL, 0, 0, 0);
                            EP0_SETUP_ARMED = false;
                            ARM_COOLDOWN = 0;
                            EP0_STATE = Ep0State::Setup;
                            let _ = try_arm_setup();
                        }
                        if poll_setup_buffer() {
                            answered = answered.saturating_add(1);
                        }
                        poll_ep0_event_ring();
                        super::super::timer::delay_us(1_000);
                    }
                    if answered != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-adopted" {
                // ONE question, ONE bit - no width or count coding, because the
                // multi-predicate words are ambiguous: a single pulse cannot say
                // *which* predicate fired (`usb2-live-dma-window` in `291878.0`
                // produced one pulse, and that is consistent with three different
                // readings). This matters because the whole address hypothesis
                // hinges on whether `adopt_smmu_dma_mapping()` actually ran:
                // with `DMA_ADOPTED == true` the SETUP buffer is `ep0_trb_ptr(0)`
                // and every address is already in the mapped page, while with
                // `DMA_ADOPTED == false` `dma_iova_for()` is the identity and the
                // linker buffers are correct as they are. Either way the
                // "outside the page" story needs re-testing, not assuming.
                // A pulse means `DMA_ADOPTED == true`.
                unsafe {
                    if DMA_ADOPTED {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-trbsts-pending" {
                // ONE question, ONE bit: is the EP0 OUT transfer sitting in
                // `SETUP_PENDING` (`TRBSTS == 2`)? This is the fact the whole
                // diagnosis turns on, and the earlier three-predicate word
                // (`usb2-live-trb-hwo`, run `278916.0`) could not establish it:
                // with `TRBSTS == 0` and `TRBSTS == 2` mutually exclusive, two
                // pulses there were consistent with {HWO, pending} *or*
                // {HWO, completed} - and "completed" would mean the controller
                // already handed the packet over. A pulse here means
                // `TRBSTS == 2`.
                unsafe {
                    let trb = ep0_trb_ptr(0);
                    cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                    let size = read_volatile(addr_of!((*trb).size));
                    if (size >> 28) & 0x0f == 2 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-trbsts-ok" {
                // ONE question, ONE bit: did the EP0 OUT transfer complete
                // cleanly (`TRBSTS == 0`)? `297671.0` ruled out `SETUP_PENDING`,
                // so this is the other candidate value and it decides whether the
                // controller already consumed the host's SETUP - in which case
                // the packet must be sitting somewhere the software can read, and
                // the fault is purely in the software side of the exchange.
                // A pulse means `TRBSTS == 0`.
                unsafe {
                    let trb = ep0_trb_ptr(0);
                    cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                    let size = read_volatile(addr_of!((*trb).size));
                    if (size >> 28) & 0x0f == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-setup-nonzero" {
                // ONE question, ONE bit, and it splits the last two
                // possibilities. `297671.0` + `299272.0` established that the EP0
                // OUT transfer completes cleanly (`TRBSTS != 2`, `TRBSTS == 0`),
                // so the controller did process the host's SETUP. Either the
                // packet is in the buffer the software reads and the reading path
                // is at fault, or the packet never landed there.
                // A pulse means the first byte of `ep0_setup_data_ptr()` is
                // non-zero.
                unsafe {
                    let setup = ep0_setup_data_ptr();
                    cache_invalidate(setup as usize, 8);
                    if read_volatile(setup) != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-hwo-bit" {
                // ONE question, ONE bit, and it is the cleanest discriminator
                // left. `297671.0`/`299272.0`/`300738.0` established that the
                // EP0 OUT TRB holds status 0 and the SETUP buffer is empty - but
                // `TRBSTS == 0` is also the *untouched default*, so the honest
                // reading is "the controller never used this TRB". `HWO` decides
                // between the two remaining cases:
                //   pulse present = HWO still set: the TRB is armed and the
                //     controller is waiting for a SETUP that never arrives.
                //   no pulse = HWO cleared: the controller consumed the TRB.
                unsafe {
                    let trb = ep0_trb_ptr(0);
                    cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                    if read_volatile(addr_of!((*trb).ctrl)) & TRB_HWO != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-dalepena0" {
                // ONE question, ONE bit: is EP0 OUT enabled in `DALEPENA`?
                // `302214.0` showed the EP0 OUT TRB armed with `HWO` set while the
                // controller never consumes it (`TRBSTS` default, buffer empty, no
                // event), so the break is upstream of the transfer - an endpoint
                // that is not enabled would produce exactly that. `ENDPOINTS_READY
                // == false` was suggested by `266423.0`, but that reading came
                // from a two-predicate word and is therefore unproven (entry 159).
                // A pulse means `DALEPENA` bit 0 is set.
                unsafe {
                    if read(DALEPENA) & 1 != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-usblnkst-on" {
                // ONE question, ONE bit: is the core's USB link state the "On"
                // state (`DSTS.USBLNKST == 0`, bits 21:18 - the vendor's
                // `DWC3_DSTS_USBLNKST_MASK = 0x0f << 18`, `core.h:483`), i.e. the
                // state in which a USB 2.0 device accepts and decodes tokens?
                // `264554.0` reported On, but it was a two-predicate word and is
                // therefore unproven (entry 159). With every software state now
                // measured good (`302214.0` armed, `303830.0` enabled) this is the
                // first candidate for "the controller does not take the SETUP".
                // A pulse means `USBLNKST == 0`.
                unsafe {
                    let dsts = read(DSTS);
                    if (dsts >> 18) & 0x0f == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-evring-nonzero" {
                // ONE question, ONE bit: has the core ever written an event into
                // the ring? Reading `GEVNTCOUNT0` cannot answer this, because the
                // handoff acknowledges the count (so "0" is consistent both with
                // "never posted" and with "posted and acknowledged"). The ring
                // *memory* keeps whatever the core wrote, so scan the first 64
                // bytes of the event ring for any non-zero word.
                // Caveat to carry with the result: a non-zero value could be stale
                // firmware data left in the reused ring, so a *zero* result is the
                // strong one - it means the core has written nothing at all.
                // A pulse means some word in the first 64 bytes is non-zero.
                unsafe {
                    let base = ep0_event_dma_base();
                    cache_invalidate(base, 64);
                    let mut found = false;
                    for offset in 0..16 {
                        let word = (base as *const u32).add(offset);
                        if read_volatile(word) != 0 {
                            found = true;
                            break;
                        }
                    }
                    if found {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-devctrlhlt" {
                // ONE question, ONE bit: is the device controller actually
                // running, or is `DSTS.DEVCTRLHLT` (bit 22, vendor
                // `DWC3_DSTS_DEVCTRLHLT`, `core.h:480`) set?
                // Run `307410.0` established that the event ring memory is
                // completely untouched - the core has never posted a single event
                // - while `305671.0` shows it reports the accepting link state,
                // `303830.0` shows EP0 enabled and `302214.0` shows the TRB armed
                // yet never consumed. A device controller that is halted would
                // explain all of that at once, and `DEVCTRLHLT` is the register
                // that says so. (`248672.0` reported it clear, but that reading
                // came from a count-coded word and is unproven.)
                // A pulse means `DEVCTRLHLT` is SET (the controller is halted).
                unsafe {
                    if read(DSTS) & DSTS_DEVCTRLHLT != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-prtcapdir-dev" {
                // ONE question, ONE bit: is the core in DEVICE mode?
                // Vendor: `DWC3_GCTL_PRTCAPDIR(n) = ((n) << 12)` and
                // `PRTCAP_HOST = 1`, **`PRTCAP_DEVICE = 2`**, `PRTCAP_OTG = 3`
                // (`core.h:248-251` - note DEVICE is 2, not 0).
                // A core in host or OTG mode reports a link state and has
                // endpoints enabled while never processing device tokens, which
                // is exactly the symptom set measured in `307410.0`/`308901.0`.
                // A pulse means `(GCTL >> 12) & 3 == 2` (device mode).
                unsafe {
                    if (read(GCTL) >> 12) & 0x3 == 2 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-utmi-sel" {
                // ONE question, ONE bit: is the USB2 PHY interface set to UTMI+
                // (`GUSB2PHYCFG0.ULPI_UTMI` = bit 4 clear, `core.h:287`) rather
                // than ULPI? A core configured for ULPI would not process the
                // internal UTMI traffic of this PHY.
                // A pulse means bit 4 is clear (UTMI+ selected).
                unsafe {
                    if read(GUSB2PHYCFG0) & (1 << 4) == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-csftrst-clear" {
                // ONE question, ONE bit: is `DCTL.CSFTRST` (bit 30,
                // `core.h:412`) clear? A core held in soft reset would have
                // running-looking registers but no device operation.
                // A pulse means CSFTRST is clear.
                unsafe {
                    if read(DCTL) & (1 << 30) == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-coresoftreset-clear" {
                // ONE question, ONE bit: is `GCTL.CORESOFTRESET` (bit 11,
                // `core.h:253`) clear? Linux leaves this set only during its own
                // reset sequence; a core still held there would look alive in
                // DSTS while doing nothing.
                // A pulse means CORESOFTRESET is clear.
                unsafe {
                    if read(GCTL) & (1 << 11) == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-evaddr-match" {
                // ONE question, ONE bit: does the address the core was given for
                // event buffer 0 (`GEVNTADRHI0:LO0`) equal the base the software
                // reads events from (`ep0_event_dma_base()`)?
                // This matters because run `307410.0` showed the ring memory the
                // software reads is completely untouched - and if the core is
                // posting to a *different* address, that is exactly what one would
                // see, while a fix would be purely software-side. `276604.0`
                // suggested they matched, but that came from a multi-predicate
                // word and is unproven (entry 159).
                // A pulse means the programmed address equals the software's base.
                unsafe {
                    let programmed =
                        ((read(GEVNTADRHI0) as u64) << 32) | u64::from(read(GEVNTADRLO0));
                    if programmed == ep0_event_dma_base() as u64 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-opmode0" {
                // Calibrated-channel requirement: a pulse only reaches the host
                // when a ~500 ms settle precedes it (see the calibration note).
                readout_keepalive_delay_ms(500);
                // ONE question, ONE bit: is the HS-PHY UTMI datapath driving
                // (`UTMI_CTRL0.OPMODE == 0`)? `phy.rs:440-448` documents that
                // Fastboot/charger-detection ownership can leave OPMODE=1, i.e. a
                // non-driving datapath, across a RAM-only handoff. Such a PHY
                // still reports a valid line state (so `DSTS.USBLNKST` reads "On",
                // run `305671.0`) while never passing received data up - which is
                // precisely the state after 13 one-bit measurements: everything
                // readable is correct and the controller never consumes the TRB.
                // A pulse means OPMODE is 0 (driving).
                unsafe {
                    if phy::utmi_opmode_is_normal() {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-dpath-ovr-clear" {
                readout_keepalive_delay_ms(500);
                // ONE question, ONE bit: is the charger-detection datapath
                // override dropped (`HSPHY_CFG0.UTMI_DATAPATH_CTRL_OVERRIDE_EN ==
                // 0`)? `phy.rs:451-459` documents that Fastboot can leave that
                // ownership bit latched even after OPMODE is returned to driving,
                // and it too would hold the DP/DM datapath away from the normal
                // UTMI path.
                // A pulse means the override bit is clear.
                unsafe {
                    if phy::datapath_override_cleared() {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-susphy-set" {
                // ONE question, ONE bit: is the core keeping the HS-PHY UTMI clock
                // suspended (`GUSB2PHYCFG0.SUSPHY`, vendor `core.h:286` `BIT(6)`)?
                //
                // NEW SOURCE DISTINCTION for reopening the SUSPHY family: the
                // previously closed `SUSPHY` work was an A/B that *cleared* the bit
                // once at init. This is a *runtime read* of the state that holds at
                // the moment the host's SETUP arrives - a different question, and
                // the host kernel log of run `321645.0` gives it a new signature to
                // explain:
                //   "usb 1-1: new high-speed USB device number 79 using xhci_hcd"
                //   "usb 1-1: device descriptor read/64, error -71"
                // The host reaches HIGH SPEED, so the analog chirp/handshake and
                // squelch path work; but no packet is ever received. A PHY whose
                // UTMI *digital* path has no clock (SUSPHY held set) loses exactly
                // the packets while keeping the analog link - and a SUSPHY bit that
                // something re-asserts after the init-time clear would never have
                // been caught by the A/B.
                // A pulse means SUSPHY is set (UTMI clock held suspended).
                unsafe {
                    if read(GUSB2PHYCFG0) & (1 << 6) != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-suspendn-set" {
                // ONE question, ONE bit: is the HS-PHY `CTRL2.SUSPEND_N` asserted
                // (PHY not suspended) at the moment the host's SETUP arrives?
                //
                // NEW SOURCE DISTINCTION for reopening the SUSPEND_N family: the
                // closed work (`3046227.0`, `3137358.0`, the `hsphy-suspend-n-safe`
                // encoding) was A/B *writes* at selected boundaries. This is the
                // runtime *read*. `phy.rs:391-397` states outright that the Bramble
                // transition was observed *clearing* this bit across the DWC3
                // Run/Stop boundary - and run `321645.0`'s host log now gives that a
                // signature to explain:
                //   "usb 1-1: new high-speed USB device number 79 using xhci_hcd"
                //   "usb 1-1: device descriptor read/64, error -71"
                // A suspended PHY keeps the analog link (the host reaches high
                // speed) while its UTMI digital receive path is off, so no packet is
                // ever delivered - exactly this signature.
                // A pulse means SUSPEND_N is asserted (PHY not suspended).
                unsafe {
                    if phy::suspend_n_asserted() {
                        ccs_pulse(300);
                    }
                }
            } else if {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x4D44_3341);
                selector == "usb2-live-devten-set"
            } {
                // ONE question, ONE bit: is `DEVTEN` (device event enable)
                // programmed at all at runtime?
                //
                // Why this is the decisive gap: every "the TRB is armed" reading
                // (`302214.0` `HWO`) was taken ~1 s after the handoff, while the bus
                // reset lands at attach + 6.2 s. The DWC3 *flushes* EP0's transfer
                // state on a bus reset, and the vendor re-arms EP0 from the reset
                // event handler (`dwc3_gadget_reset_interrupt` -> `dwc3_ep0_out_start`).
                // So what matters is whether EP0 is armed *after* the reset - and
                // the software only re-arms on events, while run `307410.0` showed
                // the event ring memory is *never written at all*.
                // The "Device Reset" event is generated by the core's own state
                // machine, with no host packet needed - so if the core is running
                // (`308901.0`, `DEVCTRLHLT` clear) yet posts nothing at all, the
                // prime suspect is that events are not *enabled*: `DEVTEN` left at 0
                // would suppress every event, which in turn starves the re-arm and
                // produces exactly the observed host-side silence.
                // A pulse means `DEVTEN` is non-zero (events enabled).
                unsafe {
                    if read(DEVTEN) != 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-deferred-evring" {
                // ONE question, ONE bit, asked at the *right* time: is the event
                // ring memory non-zero after the host attached and the bus reset
                // landed? The immediate readout cannot answer it (see
                // DEFERRED_READOUT_KIND) - which is why run `307410.0`'s
                // "the core never posts an event" had to be retracted. The bit is
                // published 8 s later, from the polling owner.
                //
                // Bisection aid: also emit a marker *here*, at handoff time. A
                // marker at ~1 s proves this branch ran and that the CCS channel
                // works then; the absence of the ~8 s marker then isolates the
                // failure to the polling-owner side rather than to this dispatch.
                unsafe {
                    ccs_pulse(300);
                }
                DEFERRED_READOUT_KIND = 1;
                POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                    arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
                );
            } else if selector == "usb2-live-deferred-devten" {
                // ONE question, ONE bit, asked at attach time: is `DEVTEN`
                // published at the moment the host is talking to us? The
                // immediate readout sees the pre-publish state. Same bisection
                // marker as the evring selector above.
                unsafe {
                    ccs_pulse(300);
                }
                DEFERRED_READOUT_KIND = 2;
                POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                    arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
                );
            } else if selector == "usb2-live-deferred-devaddr" {
                // ONE question, ONE bit, asked at attach time: is `DCFG.DEVADDR`
                // zero when the host is talking to us? A stale non-zero address
                // makes the core ignore the host's address-0 SETUP - the hazard the
                // code itself names at `usb.rs:12150`.
                unsafe {
                    ccs_pulse(300);
                }
                DEFERRED_READOUT_KIND = 3;
                POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                    arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
                );
            } else if selector == "usb2-live-deferred-recover" {
                // Not a measurement but a *fix candidate*: at attach time (8 s
                // after the handoff, i.e. after the bus reset), run the bus-reset
                // recovery that the event path would have run. `usb.rs:168` records
                // that `restart_control_after_reset()` is reachable only through
                // `match device_event`, so if the core posts no events the handler
                // never runs - DEVADDR is not cleared, EP0 is not reconfigured and
                // the SETUP is never accepted. The host retries GET_DESCRIPTOR
                // (baseline: one -110 then three immediate -71s), so a recovery
                // forced at this point has later attempts to catch.
                unsafe {
                    ccs_pulse(300);
                }
                DEFERRED_READOUT_KIND = 4;
                // Arm the trigger with the *current* frame number, so the action
                // fires on the next change - i.e. when the host actually starts
                // driving the bus - and not on the first poll after the handoff.
                LAST_DEFERRED_SOFFN = read(DSTS) & (0x3fff << 3);
                POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                    arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
                );
            } else if selector == "usb2-live-gate-diag" {
                // Publish `diag_readout_code()` at attach time over the ONLY channel
                // that survives the failure: DCTL Run/Stop cycles, decoded by the
                // host as extra "new high-speed USB device" lines. The code names
                // how far the first enumeration window got (1 = no SETUP reached
                // DRAM ... 6 = XferNotReady on the data phase) - see
                // `usb_probe.rs:1362-1372`, which uses the same encoding for the
                // `dstat` gate.
                DEFERRED_READOUT_KIND = 7;
                POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                    arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
                );
            } else if selector == "usb2-live-gate-ep0armed" {
                // ONE decisive bit at attach time: was EP0's SETUP transfer actually
                // armed by whatever ran after the handoff? `usb.rs:10909-10915`
                // shows the handoff itself does not arm it when
                // `--start-after-connect` is set (it only sets
                // `PENDING_SETUP_ARM`), and entry 176 shows the synchronous variant
                // suppresses the attach entirely - so the arm must come from the
                // post-handoff owner, and this is the direct check.
                // One extra attach line = armed, no extra line = not armed.
                DEFERRED_READOUT_KIND = 8;
                POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                    arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
                );
            } else if selector == "usb2-live-deferred-endpoints" {
                // ONE bit at attach time: did anything actually publish
                // `ENDPOINTS_READY = true`?
                //
                // Same shape and same deferred placement as
                // `usb2-live-gate-ep0armed` (kind 8). A pulse published from inside
                // the handoff cannot reach the host - the host must already have
                // logged the attach, and DCTL Run/Stop must be cycling. Kind 8 asks
                // whether EP0's SETUP transfer is armed; this asks whether the
                // endpoint epoch was published at all, which separates "the config
                // branch never ran" from "it ran and the arm still did not happen".
                // Positive control in this same placement: `usb2-live-gate-probe6`.
                DEFERRED_READOUT_KIND = 10;
            } else if selector == "usb2-live-gate-probe"
                || selector == "usb2-live-gate-probe6"
                || selector == "usb2-live-gate-probe7"
            {
                // POSITIVE CONTROL for the Run/Stop-cycle channel: publish two
                // cycles unconditionally. Two extra "new high-speed USB device"
                // lines prove the channel works at that point in the boot, which is
                // what makes the `usb2-live-gate-ep0armed` result interpretable.
                //
                // Timing matters more than anything else here. The handset
                // self-resets 5.5-8 s *after the attach* (`usb_probe.rs:1331`), so
                // the kernel is alive throughout the host's 588 ms enumeration
                // window but dead at +8 s. The host attaches ~6.2 s after the
                // handoff, so a 2 s action fires before the host is listening and
                // an 8 s action fires after the kernel is gone; both produce zero
                // extra lines and prove nothing. 6 s and 7 s bracket the attach.
                DEFERRED_READOUT_KIND = 9;
                let secs = match selector {
                    "usb2-live-gate-probe6" => 6,
                    "usb2-live-gate-probe7" => 7,
                    _ => 2,
                };
                POST_RUNSTOP_PROBE_NOT_BEFORE =
                    arch_counter().saturating_add(arch_counter_frequency().saturating_mul(secs));
            } else if selector == "usb2-live-deferred-early" {
                // BISECTION, not a data measurement: fire the deferred block only
                // ~2 s after the handoff - while the CCS channel is still known to
                // work (the handoff marker at ~1 s always appears) - to determine
                // whether the polling owner is running at all. If the marker shows
                // up at ~2 s, `poll()` runs and the *late* failures (no pulses at
                // ~6 s, no re-attach) are a channel/timing problem; if it does not,
                // `poll()` itself is not running after the handoff.
                DEFERRED_READOUT_KIND = 6;
                POST_RUNSTOP_PROBE_NOT_BEFORE =
                    arch_counter().saturating_add(arch_counter_frequency().saturating_mul(2));
            } else if selector == "usb2-live-reattach" {
                // Fix candidate, not a measurement: let the host's first attempt
                // fail (it gives up within ~600 ms of attaching - entry 171), then
                // *deliberately* re-attach with EP0 fully prepared. If the real
                // problem is only that the host arrives before the device is
                // ready, this gives it a clean second chance - and the observable
                // is the host kernel log (a new attach followed by a successful
                // descriptor read), which does not need the CCS channel that is
                // unusable after the first failure.
                unsafe {
                    ccs_pulse(300);
                }
                DEFERRED_READOUT_KIND = 5;
                POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                    arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
                );
            } else if selector == "usb2-live-disscramble-clear" {
                // ONE question, ONE bit: is `GCTL.DISSCRAMBLE` (BIT(3)) clear, i.e.
                // is USB2 packet scrambling ENABLED on the device side?
                //
                // Why this is a prime candidate, from the primary source:
                // vendor `core.c:783-786` does
                //     if (dwc->disable_scramble_quirk && dwc->is_fpga)
                //         reg |= DWC3_GCTL_DISSCRAMBLE;
                //     else
                //         reg &= ~DWC3_GCTL_DISSCRAMBLE;
                // i.e. on real silicon (Bramble is not an FPGA) the vendor
                // *always clears* the bit, so scrambling is on. Fullerene's GCTL
                // writes only OR in the device-mode and clock-gating bits
                // (`gctl |= GCTL_PRTCAP_DEVICE | GCTL_DSBLCLKGTNG`) and never clear
                // DISSCRAMBLE, so a value left set by Fastboot/XBL would survive
                // the handoff.
                // That failure mode matches the measured signature exactly: the
                // chirp/handshake is not scrambled, so the host still reaches
                // HIGH SPEED (`321645.0`), while every scrambled packet arrives as
                // garbage, so the core never accepts a SETUP and the host times
                // out - with EP0 enabled, the TRB armed and every readable state
                // correct (entries 160-164).
                // `DISSCRAMBLE` had never been examined in this project before.
                // A pulse means the bit is clear (scrambling enabled = vendor
                // behaviour on silicon).
                unsafe {
                    if read(GCTL) & (1 << 3) == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-phyif-zero" {
                // ONE question, ONE bit: is `GUSB2PHYCFG0.PHYIF` zero, i.e. is the
                // core using the 8-bit UTMI+ interface?
                // Vendor `core.h:289-290`: `GUSB2PHYCFG_PHYIF(n) = (n << 3)`.
                // Fullerene forces the field clear at `config.rs:441`
                // (`usb2 &= !(ULPI_UTMI | PHYIF_MASK | USBTRDTIM_MASK)`), while the
                // comment at `config.rs:411` notes that Linux instead follows the
                // DT's `snps,hsphy_interface` and therefore *leaves* PHYIF/TRDTIM
                // alone. The Bramble DT does not appear to set that property (only
                // the binding mentions it), so both should end up 8-bit - but a
                // width mismatch between core and PHY would break exactly the data
                // path while leaving line-state and the analog chirp intact, which
                // is this failure's signature. Verify rather than assume.
                // A pulse means PHYIF is 0 (8-bit).
                unsafe {
                    if read(GUSB2PHYCFG0) & GUSB2PHYCFG_PHYIF_MASK == 0 {
                        ccs_pulse(300);
                    }
                }
            } else if selector == "usb2-live-evbuf-presence" {
                // One pulse iff the event buffer size is programmed. `GEVNTCOUNT0`
                // reads 0 in every run, which is consistent both with "the core
                // posted nothing" and with "the ring has no space to post into".
                // `GEVNTSIZ0` distinguishes them, and a zero size would explain
                // zero events. (An earlier note here cited `255232.0` as evidence
                // that SOFFN advanced; that run predates the SOFFN mask fix, so
                // the reference is void - do not lean on it.)
                let size = read(GEVNTSIZ0) & GEVNTSIZ_SIZE_MASK;
                if size != 0 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-fail-stage" {
                // Snapshot `GADGET_HANDOFF_FAILURE_STAGE` in the post-Run/Stop
                // selector block. It only covers failures recorded before this
                // point; it says nothing about the later arm window or polling path.
                //   0 pulses => the handoff did not record a failure (a plain
                //               `return false` path, or a different route)
                //   1 pulse  => a failure WAS recorded; bisect with the
                //               `usb2-live-fail-ge-*` selectors for which stage
                readout_keepalive_delay_ms(500);
                if gadget_handoff_failure_stage() != 0 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-fail-ge-5" {
                // Bisect: was the recorded failure stage >= 5?
                // Stage 5 is the EP0 `configure_endpoint(1, ...)` pair
                // (mod.rs:~6875), which sits immediately *before* the
                // unconditional `ENDPOINTS_READY = true`.
                readout_keepalive_delay_ms(500);
                if gadget_handoff_failure_stage() >= 5 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-fail-eq-5" {
                // Was the recorded failure exactly stage 5?
                readout_keepalive_delay_ms(500);
                if gadget_handoff_failure_stage() == 5 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-fail-eq-12" {
                // Was the recorded failure exactly stage 12 (XBL inter-pair
                // SETUP STARTTRANSFER, mod.rs:~6856)?
                readout_keepalive_delay_ms(500);
                if gadget_handoff_failure_stage() == 12 {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-entered" {
                // ONE bit, with the settle wait: was
                // `init_usb2_gadget_reuse_fastboot_ep0` entered at all?
                // The cfg expansion proves the call site exists, so a 0 here means
                // the route takes the other branch and the whole entry point -
                // including its unconditional `ENDPOINTS_READY = true` - never runs.
                readout_keepalive_delay_ms(500);
                if unsafe { HANDOFF_ENTERED } {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-typec-detach-seen" {
                // ONE bit: did a Type-C DetachDetected event get applied? That is
                // the clear that survives once process_event is ruled out.
                readout_keepalive_delay_ms(500);
                if unsafe { TYPEC_DETACH_SEEN } {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-disconnect-seen" {
                // ONE bit: did a Disconnect event ever decode? This is the branch
                // that clears ENDPOINTS_READY after the handoff publishes it.
                readout_keepalive_delay_ms(500);
                if unsafe { DISCONNECT_SEEN } {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector.starts_with("usb2-live-events-ge-") {
                // ONE bit: were at least N event words consumed at all?
                let k: u16 = selector["usb2-live-events-ge-".len()..]
                    .parse()
                    .unwrap_or(u16::MAX);
                readout_keepalive_delay_ms(500);
                if unsafe { EVENTS_CONSUMED } >= k {
                    unsafe { ccs_pulse(300) };
                }
            } else if let Some(rest) = selector.strip_prefix("usb2-live-g2wline-ge-") {
                // Bisect the source line of the last GUSB2PHYCFG0 write. The
                // candidate clearing sites are ordered by line number, so a
                // handful of these runs brackets the offender to one site.
                let k: u32 = rest.parse().unwrap_or(u32::MAX);
                readout_keepalive_delay_ms(500);
                let packed = unsafe {
                    mmio::GUSB2PHYCFG_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed)
                };
                if packed != u32::MAX && (packed & 0xffff) >= k {
                    unsafe { ccs_pulse(300) };
                }
            } else if let Some(rest) = selector.strip_prefix("usb2-live-g2wcount-ge-") {
                // Did the register get written at all, and how many times?
                let k: u32 = rest.parse().unwrap_or(u32::MAX);
                readout_keepalive_delay_ms(500);
                if unsafe {
                    mmio::GUSB2PHYCFG_WRITE_COUNT.load(core::sync::atomic::Ordering::Relaxed)
                } >= k
                {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector.starts_with("usb2-live-milestone-ge-") {
                // Bisect the endpoint-publication region one bit per run.
                // Value = the `>= k` suffix. One pulse iff the milestone index is
                // at least k, so each run answers exactly one yes/no question.
                let k: u8 = selector["usb2-live-milestone-ge-".len()..]
                    .parse()
                    .unwrap_or(255);
                readout_keepalive_delay_ms(500);
                if unsafe { HANDOFF_MILESTONE } >= k {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-delay-only" {
                // CROSS-CHECK for the `soffn-count` result. That selector publishes
                // one pulse iff SOFFN advanced, and produced 2 attach lines twice in
                // a row - but the two lines are 2.606 s apart, and its own stop
                // delay is only 500 ms. So the second line is probably the host
                // retrying by itself, not the pulse.
                //
                // This control does *only* the wait, with no pulse at all:
                //   2 attach lines => the extra line is the delay/host-retry, and
                //                     `soffn-count`'s two lines prove nothing about RX
                //   1 attach line  => the delay alone is not enough, and the pulse
                //                     really is what makes the second line
                readout_keepalive_delay_ms(500);
            } else if selector == "usb2-live-soffn-count" {
                // Direct RX-liveness readout: DSTS.SOFFN is the frame number of
                // the last SOF the device controller received. In high speed the
                // host sends an SOF every 125 us once the link is operational, so
                // a SOFFN that advances between two samples proves the PHY-to-core
                // receive path carries host traffic; a static SOFFN means it does
                // not. THE TEST IS: exactly one pulse iff SOFFN advanced, no
                // marker, so 1 attach line on the host = static and 2 = advanced.
                // This is the cleanest available test of the receive direction,
                // which every other measurement has now isolated.
                // NO MARKER. The channel rule is "one predicate per run, pulse
                // present/absent only", and a marker pulse makes the count
                // ambiguous: `1 + 2 (advanced)` and `1 + 1 (marker) + 1 (static)`
                // are the same three attach lines on the host. So publish exactly
                // one pulse iff `SOFFN` advanced, nothing otherwise:
                //   1 attach line (baseline) = static, 2 = advanced.
                // This is the test the earlier attempts never actually ran.
                const SOFFN_MASK: u32 = 0x3fff << 3;
                let before = (read(DSTS) & SOFFN_MASK) >> 3;
                // Keep the whole branch short: a long branch lets the signal-probe's
                // own blip publisher overlap the CCS channel. In high speed the host
                // sends an SOF every 125 us and SOFFN counts 1 ms frames, so 300 ms
                // is already ~300 frames of margin.
                readout_keepalive_delay_ms(300);
                let after = (read(DSTS) & SOFFN_MASK) >> 3;
                let advanced = after != before;
                log_hex("usb gadget handoff: SOFFN before=", u64::from(before));
                log_hex("usb gadget handoff: SOFFN after=", u64::from(after));
                if advanced {
                    unsafe { ccs_pulse(300) };
                }
            } else if selector == "usb2-live-halted-count" {
                // Pulse-count readout: robust to the width distortion seen in
                // `247041.0`, where a requested 150 ms pulse measured 62 ms.
                // Three 300 ms pulses = DSTS.DEVCTRLHLT set, one = clear, after
                // the usual marker. This is the fact the probe's own "RUN/STOP
                // readback timed out; continuing" path has never published.
                let halted = read(DSTS) & DSTS_DEVCTRLHLT != 0;
                let count = if halted { 3 } else { 1 };
                for _ in 0..count {
                    unsafe { ccs_pulse(300) };
                }
            } else if {
                trace_marker(TRACE_PROBE_WATCHDOG, 0x4D44_3441);
                selector == "usb2-live-halted"
            } || selector == "usb2-live-runstop-bit"
            {
                // Single-bit, wide-pulse readouts. The width-coded words drift
                // by ~80 ms through run_stop_device, so a one-bit word uses two
                // widths that cannot be confused: 700 ms = bit set, 150 ms =
                // clear, after the usual 1000 ms marker.
                //   usb2-live-halted      -> DSTS.DEVCTRLHLT after the Run/Stop
                //                            write (1 = the device controller
                //                            never left the halted state)
                //   usb2-live-runstop-bit -> DCTL.RUN_STOP as read back
                // These are the two facts the probe's own "RUN/STOP readback
                // timed out; continuing" path has never been able to publish.
                let set = if selector == "usb2-live-halted" {
                    read(DSTS) & DSTS_DEVCTRLHLT != 0
                } else {
                    read(DCTL) & DCTL_RUN_STOP != 0
                };
                unsafe { ccs_pulse(if set { 700 } else { 150 }) };
            } else if selector == "usb2-live-susphy-active" {
                // Primary-source correction. The vendor dwc3_gadget_run_stop()
                // never touches DWC3_GUSB2PHYCFG (tmp/qpr1-msm/drivers/usb/dwc3/
                // gadget.c:2136-2200); the save/clear/restore of
                // GUSB2PHYCFG.SUSPHY|ENBLSLPM lives in dwc3_gadget_ep_cmd
                // (gadget.c:387-410) and is gated on gadget.speed <= HIGH.
                // Fullerene mirrors that pattern in prepare_run_stop_device and
                // both Run/Stop callers restore the saved bits afterwards, so if
                // the Fastboot handoff left SUSPHY set the PHY is re-suspended
                // immediately after the pull-up rises - which is exactly the
                // state that would produce "first attach, then nothing": the
                // analog pull-up is a resistor and survives, while the UTMI data
                // path that would deliver events does not.
                // Clear both bits and leave them cleared, then publish the
                // pre-clear state so the run still says whether the PHY had been
                // suspended at all: bit0 = SUSPHY was set, bit1 = ENBLSLPM was.
                let gusb2 = read(GUSB2PHYCFG0);
                write(
                    GUSB2PHYCFG0,
                    gusb2 & !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM),
                );
                EVENT_DROP_ARMED = true;
                unsafe {
                    publish_ccs_word(
                        u32::from(gusb2 & GUSB2PHYCFG_SUSPHY != 0)
                            | (u32::from(gusb2 & GUSB2PHYCFG_ENBLSLPM != 0) << 1),
                    )
                };
            } else if let Some(word) = selector.strip_prefix("usb2-live-ccs-") {
                // Publish a live controller word over the pull-up (CCS) channel
                // instead of a timestamp. See publish_ccs_word for the encoding
                // and tools/bramble_port_ccs.py for the host-side decoder. This
                // is the first carrier that survives past Run/Stop, because the
                // root hub keeps reporting the pull-up to the host.
                // NO MARKERS HERE. The first version of this probe wrote "WBFR" immediately
                // before this line and "WAFT" immediately after - and both were TAUTOLOGIES:
                // the reader runs *inside* the statement between them, so a scan for "WBFR"
                // always found the marker it had just written, and a scan for "WAFT" never
                // found a marker that had not been written yet. Measured 3 runs that appeared
                // to prove the lookup stalls; they proved nothing. The predicate a marker
                // encodes must be upstream of the code that reads it. See usb/README.md §3.17.
                let value = usb2_live_word(word);
                if word == "gctlline" || word == "gctlline2" || word == "gctlwc" {
                    unsafe { publish_ccs_word_byte(value) };
                } else {
                    unsafe { publish_ccs_word(value) };
                }
            } else {
                let delay_ms = if selector == "hsphy-suspend-n-safe" {
                    // 1 = missing, 2 = present/0, 3 = present/1.
                    match code {
                        2 => 0,
                        3 => 4_000,
                        _ => 8_000,
                    }
                } else {
                    u64::from(code) * 1_000
                };
                readout_keepalive_delay_ms(delay_ms);
            }
        }
        // Reached only if the whole POSTRUN readout block above completed.
        trace_marker(TRACE_PROBE_WATCHDOG, 0x5345_4C58); // "SELX"
        HANDOFF_MILESTONE = 14;
        // "M20D" / milestone 20 - the POSTRUN readout block is behind us. The
        // `usb2-live-milestone-ge-*` selector runs in that block above
        // before milestones 14 and 20 are assigned here. It cannot observe
        // milestone >=20 and is not a reader for arm-window reachability.
        // See usb/README.md §3.19 for the earlier milestone reads.
        // PULSE BREADCRUMB - an actuator, not a passive marker.
        //
        // Each `ccs_pulse` toggles DCTL Run/Stop. Read its host-side effect from usbmon CCS,
        // not attach-log line counts: the host's visibility changes once enumeration starts,
        // so a missing later pulse is not proof its site did not run. See usb/README.md §3.22
        // and the 2026-09-29 evidence-ledger retraction.
        if matches!(
            option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
            Some("1") | Some("4")
        ) {
            // Use the PLAIN `ccs_pulse`, not `readout_bit`. The comment at :12235 records that
            // the same publisher emitted *no* host-visible pulse through
            // `ccs_pulse_no_readback` (run 232050.0) while plain `ccs_pulse` did, and these sites
            // run after the host has begun enumerating - exactly the case that comment describes.
            // If a site is still silent after this change, the site was not reached rather than
            // mis-encoded.
            unsafe { ccs_pulse(300) };
        }
        HANDOFF_MILESTONE = 20;
        trace_marker(TRACE_PROBE_WATCHDOG, 0x4D32_3044); // "M20D"
        unsafe { trace::SHARED_POST_HANDOFF = 20 };
        // EXPERIMENT: under the explicit cfg below, attempt a SETUP transfer
        // before the timed arm window if EP0_SETUP_ARMED is false. This is an
        // intervention, not a reachability test for the later loop. The
        // usb2-live-arm-* readouts above are also pre-window snapshots; stage 4
        // there cannot establish what the loop below will do. See boundary-state.md.
        #[cfg(fullerene_aarch64_usb_force_ep0_armed)]
        unsafe {
            if !EP0_SETUP_ARMED {
                log_puts("usb gadget handoff: forcing EP0 SETUP arm\n");
                prepare_ep0_setup_trb();
                if start_transfer(0, ep0_trb_ptr(0)) {
                    EP0_SETUP_ARMED = true;
                    log_puts("usb gadget handoff: forced arm succeeded\n");
                } else {
                    log_puts("usb gadget handoff: forced arm failed\n");
                }
            }
        }
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
        // Arm the first SETUP TRB inside the handoff. The ordinary window
        // covers the usual short link-training interval; the explicit USB2
        // extended A/B keeps retrying through Bramble's measured HS attach
        // to first-descriptor gap (~5 s). A rejected Start Transfer while
        // the link is not ON is retryable, not a wedge.
        {
            let arm_window_ms = if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_long_setup_arm) {
                10_000
            } else if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_extended_setup_arm) {
                5_000
            } else {
                400
            };
            let arm_deadline = arch_counter()
                .saturating_add(arch_counter_frequency().saturating_mul(arm_window_ms) / 1_000);
            // PERTURBATIVE arm-window breadcrumbs (levels 3/9). Each `ccs_pulse(300)` toggles
            // Run/Stop and includes at least 300 ms stopped plus 200 ms after restart. The
            // deadline is set above, before the pre-loop `bc3it!(0, 0)` marker, so that marker
            // alone exceeds the default 400 ms window before the `while` condition is evaluated.
            // The historical two-pulse level-3 result is consistent with the pre-loop and
            // loop-exit markers; it does not prove the body ran. A 5 s run emitted one pulse, but
            // silence after that intervention is not a passive return witness. Do not infer
            // uninstrumented reachability from these markers. See the evidence-ledger retraction.
            // Per-iteration slots distinguish repeated sites but do not remove Run/Stop or
            // deadline perturbation. See usb/README.md §3.23 and §3.28.
            macro_rules! bc3it {
                ($iter:expr, $step:expr) => {
                    if matches!(
                        option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
                        Some("3") | Some("10") | Some("4")
                    ) {
                        let bc3_slot = ($iter * 4 + $step) as usize;
                        // 16 is BC_ONCE's length; `.len()` would take a reference to a mutable
                        // static, which is not allowed.
                        if bc3_slot < 16 && unsafe { !trace::BC_ONCE[bc3_slot] } {
                            unsafe {
                                trace::BC_ONCE[bc3_slot] = true;
                                ccs_pulse(300);
                            }
                        }
                    }
                };
            }
            let mut bc3_iter: u32 = 0;
            screen_mark(8); // EP0 arm window entered
            bc3it!(0, 0); // the loop is about to be entered
            while arch_counter() < arm_deadline && !EP0_SETUP_ARMED {
                let _ = try_arm_setup();
                bc3it!(bc3_iter, 1); // try_arm_setup() returned on this iteration
                // EXPERIMENT 2026-09-29: the event-ring drain is REMOVED from this loop.
                //
                // Measured: `poll_ep0_event_ring()` never returns, and the instruction that blocks
                // is its first controller access, `read(GEVNTCOUNT0)`. The level-7
                // discriminator put an ordinary `read(DCTL)` immediately before it and that read
                // completed (pulse counted), so the controller aperture is alive - this is not the
                // clock-collapse failure mode discussed earlier, and re-asserting the
                // power vote would not address it.
                //
                // The count read is only used to decide whether there is an event to drain, and this
                // loop's own condition already tests `!EP0_SETUP_ARMED`. The drain is therefore an
                // optimisation rather than a requirement for arming EP0, and removing it is the
                // minimal change that takes the blocking access off the path. `poll_setup_buffer()`
                // below still runs and is what consumes the host's SETUP. If this changes host
                // behaviour the drain is load-bearing and has to be reinstated behind a gated read.
                // The `usb2-live-*` dispatch above runs after Run/Stop but before milestone 20
                // and before this arm window. Its stage-4 / ARMED=false values are pre-window
                // snapshots; they cannot say whether this loop later calls `try_arm_setup`.
                // XferComplete can also clear ARMED after consumption, but that is not the timing
                // of this selector. Level-3 CCS pulses toggle Run/Stop and are not passive return
                // witnesses. See the 2026-09-29 retraction in `references/boundary-state.md`.
                let _ = unsafe { poll_setup_buffer() };
                bc3it!(bc3_iter, 2); // poll_setup_buffer() returned on this iteration
                // PROFILE OBSERVATION 2026-09-29: widening 400 ms to 5 s still ended in host -110.
                // The arm-stage selectors used in that comparison execute before this loop, so they
                // do not show whether `try_arm_setup()` returned here. Breadcrumb counts also come
                // from Run/Stop interventions and do not localise progress in this window. The
                // separate 57-62 s attach vs ~25 s vote-budget observation remains a hypothesis in
                // `references/boundary-state.md`, not proof of this function's progress.
                readout_keepalive_delay_ms(200);
                if bc3_iter < 2 {
                    bc3_iter += 1;
                }
            }
            bc3it!(3, 3); // the loop exited (slot 15)
            // Keep the arm-status readout meaningful on the direct reuse
            // path as well as on the fallback recovery path.  The latter
            // already records 0/8 in u0_arm_recovery(), but direct handoff
            // used to leave U0_ARM_STATUS at its static sentinel forever,
            // making armstat unable to distinguish a retired EP0
            // STARTTRANSFER from a command wedge.
            U0_ARM_STATUS = if EP0_SETUP_ARMED { 0 } else { 8 };
            #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_arm_window_recovery)]
            if !EP0_SETUP_ARMED {
                // The host-facing pull-up can remain quiet for tens of
                // seconds after Run/Stop. If the deferred STARTTRANSFER
                // window ended with a command wedge, repair the controller
                // once before the host's eventual HS attach rather than
                // waiting for a manual reboot or a host-side timeout.
                log_puts("usb gadget handoff: automatic EP0 arm-window recovery\n");
                let status = u0_arm_window_recovery();
                U0_ARM_STATUS = status;
                trace_event(
                    TRACE_SETUP_QUEUED,
                    0x5253_4356, // "RSCV": arm-window recovery result
                    status,
                    EP0_SETUP_ARMED as u32,
                    0,
                    read(DSTS),
                );
            }
            if cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect) {
                // In the deferred profile the real qpr1 ownership boundary is
                // reached only after Run/Stop and the U0-guarded retry. Do not
                // publish device-event interrupts before that STARTTRANSFER
                // has actually armed the EP0 OUT TRB.
                write(DEVTEN, direct_gadget_devten());
            }
        }
        // "M21D" / milestone 21 - the EP0 arm window (400 ms default) completed.
        // POST-WINDOW breadcrumb, but still an actuator: `ccs_pulse` toggles DCTL.Run/Stop.
        // Decode host-side usbmon CCS transitions, not attach-log line counts. A silent later
        // pulse is not a negative unless the root-hub observer was polling in that regime and
        // the capture has valid rows plus a same-regime control. See usb/README.md §3.22.
        if matches!(
            option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
            Some("2") | Some("4")
        ) {
            // Use plain `ccs_pulse`; an earlier `ccs_pulse_no_readback` variant had no visible
            // CCS edge. That does not make a missing later edge evidence that this site was skipped.
            unsafe { ccs_pulse(300) };
        }
        HANDOFF_MILESTONE = 21;
        trace_marker(TRACE_PROBE_WATCHDOG, 0x4D32_3144); // "M21D"
        unsafe { trace::SHARED_POST_HANDOFF = 21 };
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        if option_env!("FULLERENE_USB_SIGNAL_DMA_POST_RUNSTOP") == Some("1") {
            // Run/Stop returns before the host's attach debounce reaches U0.
            // Let the polling owner run the test after a bounded delay that
            // is still before Bramble's first descriptor request.
            POST_RUNSTOP_PROBE_PENDING = true;
            POST_RUNSTOP_PROBE_NOT_BEFORE = arch_counter().saturating_add(
                arch_counter_frequency().saturating_mul(POST_RUNSTOP_PROBE_DELAY_SECS),
            );
        }
        unsafe { trace::SHARED_POST_HANDOFF = 22 };
        // Final boundary marker; like the other CCS breadcrumbs it is a Run/Stop actuator.
        // Count only usbmon CCS transitions with valid rows and a same-regime control; attach-log
        // lines do not count individual pulses once enumeration starts. A silent pulse remains
        // unknown if the host observer was not polling. See usb/README.md §3.22.
        if matches!(
            option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
            Some("2") | Some("4")
        ) {
            // Use plain `ccs_pulse`; a previous no-readback variant had no visible CCS edge.
            // Absence of a later edge still does not prove this site was not reached.
            unsafe { ccs_pulse(300) };
        }
        unsafe { gate_flow_blip() }; // flow-map B4: final Run/Stop readback done
        if stop_after_gadget_handoff_stage(7) {
            return false;
        }
        #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
        // The reuse handoff is the primary direct Bramble path. Keep the
        // host-visible EP0 signal channel on this path as well as on the
        // full platform-init path; otherwise --signal-early-drop silently
        // skips its observation window whenever reuse reaches Run/Stop.
        // With start-after-connect, the handoff must return immediately after
        // publishing the pull-up so the normal polling owner can perform the
        // U0-guarded SETUP arm.  A synchronous observation window here would
        // touch the event ring before that boundary and can suppress the
        // first physical attach.  The polling owner performs the same latch
        // check after the handoff returns.
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_start_after_connect) {
            ep0_signal_early_drop_check();
        }
        // The probe's Type-C observer establishes Powered/Attached before
        // this point, so record the same UDC-start boundary that the normal
        // Qualcomm gadget path records. If PMIC observation was unavailable
        // this is intentionally a no-op in the state machine, but it must
        // not block EP0 testing.
        note_runtime_event(super::super::platform::bramble::UsbRuntimeEvent::ControllerStarted);
        return true;
    }
}
