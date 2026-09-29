//! EP0 TRB preparation, setup handling, and control-transfer events.

use super::*;

unsafe fn prepare_trb(index: usize, buffer: *const u8, length: usize, kind: u32) {
    let address = unsafe { dma_iova_for(buffer as usize) };
    let trb = unsafe { ep0_trb_ptr(index) };
    let flags = if cfg!(fullerene_aarch64_usb_abl_trb_flags) {
        TRB_ABL_REQUEST_FLAGS
    } else {
        TRB_HWO
            | TRB_LST
            | TRB_IOC
            | TRB_ISP_IMI
            | if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_trb_chain) {
                TRB_CHN
            } else {
                0
            }
    };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
        // The fourth DWORD carries HWO; publish it last behind a write barrier.
        trb_publish_barrier();
        write_volatile(addr_of_mut!((*trb).ctrl), kind | flags);
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

/// Prepare the EP0 SETUP TRB and, for the source-exact A/B, its separate
/// controller-owned eight-byte setup buffer.
pub(super) unsafe fn prepare_ep0_setup_trb() {
    let kind = if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup) {
        TRB_XBL_EP0_SETUP
    } else {
        TRB_CONTROL_SETUP
    };
    unsafe {
        let setup = ep0_setup_data_ptr();
        if setup != ep0_trb_ptr(0).cast::<u8>() {
            core::ptr::write_bytes(setup, 0, 8);
            cache_clean(setup as usize, 8);
        }
        prepare_trb(0, setup as *const u8, 8, kind);
    }
}

/// Return whether the controller replaced the idle marker with a fresh SETUP.
pub(super) unsafe fn setup_packet_pending() -> bool {
    unsafe {
        let setup = ep0_setup_data_ptr();
        cache_invalidate(setup as usize, 8);
        if setup == ep0_trb_ptr(0).cast::<u8>() {
            let expected = dma_iova_for(setup as usize);
            read_volatile(setup.cast::<u32>()) != expected as u32
                || read_volatile(setup.add(4).cast::<u32>()) != (expected >> 32) as u32
        } else {
            (0..8).any(|offset| read_volatile(setup.add(offset)) != 0)
        }
    }
}

/// Clear the SETUP payload while preserving the aliased TRB0 idle DMA marker.
pub(super) unsafe fn clear_setup_packet() {
    unsafe {
        let setup = ep0_setup_data_ptr();
        if setup == ep0_trb_ptr(0).cast::<u8>() {
            let expected = dma_iova_for(setup as usize);
            write_volatile(setup.cast::<u32>(), expected as u32);
            write_volatile(setup.add(4).cast::<u32>(), (expected >> 32) as u32);
        } else {
            core::ptr::write_bytes(setup, 0, 8);
        }
        cache_clean(setup as usize, 8);
    }
}

/// Write Memory Barrier for TRB publication, matching the SHIPPED kernel.
///
/// The shipped `__dwc3_prepare_one_trb()` (`gadget.c`) replaced a plain `mb()`
/// with `wmb()` immediately before setting `DWC3_TRB_CTRL_HWO`, with this
/// comment:
///
/// > As per data book 4.2.3.2 TRB Control Bit Rules section: The controller
/// > autonomously checks the HWO field of a TRB to determine if the entire TRB
/// > is valid. Therefore, software must ensure that the rest of the TRB is
/// > valid before setting the HWO field to '1'. ... However there is a
/// > possibility of CPU re-ordering here which can cause controller to observe
/// > the HWO bit set prematurely. Add a write memory barrier to prevent CPU
/// > re-ordering.
///
/// On arm64 Linux `wmb()` is `dsb st`. `write_volatile` orders nothing by
/// itself, and `cache_clean()` is a no-op unless the DT describes a
/// non-coherent path (see `cache_clean`), so without this the core can observe
/// HWO before `bpl`/`bph`/`size` have landed - which is one way the core ends
/// up never consuming the TRB.
#[inline]
pub(super) unsafe fn trb_publish_barrier() {
    unsafe { core::arch::asm!("dsb st", options(nostack, preserves_flags)) };
}

pub(super) unsafe fn prepare_trb_at(trb: *mut Trb, buffer: *const u8, length: usize, kind: u32) {
    let address = unsafe { dma_iova_for(buffer as usize) };
    unsafe {
        write_volatile(addr_of_mut!((*trb).bpl), address as u32);
        write_volatile(addr_of_mut!((*trb).bph), (address >> 32) as u32);
        write_volatile(addr_of_mut!((*trb).size), length as u32);
        // The fourth DWORD carries HWO and must be published last.
        trb_publish_barrier();
        write_volatile(
            addr_of_mut!((*trb).ctrl),
            kind | TRB_HWO | TRB_LST | TRB_IOC | TRB_ISP_IMI,
        );
        cache_clean(trb as usize, core::mem::size_of::<Trb>());
    }
}

pub(super) unsafe fn start_setup() -> bool {
    trace_event(TRACE_SETUP_QUEUED, 0, 0, 0, 8, unsafe { read(DSTS) });
    unsafe {
        if cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
        )) {
            if !queue_xbl_setup_request() {
                return false;
            }
        }
        // Keep the source-derived TRBCTL=2 and pre-Run/Stop start order. The
        // XBL setup branch also publishes the generated TRB address as its
        // buffer field; `prepare_ep0_setup_trb()` carries that special case.
        prepare_ep0_setup_trb();
        let armed = start_transfer(0, ep0_trb_ptr(0));
        if armed && cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup) {
            let slot = EP0_SETUP_REQUEST_SLOT;
            if slot == usize::MAX || !udc_mut().start(0, slot) {
                return false;
            }
        }
        EP0_SETUP_ARMED = armed;
        armed
    }
}

/// Mirror XBL's bounded software request queue for the deferred EP0 setup.
/// The UDC queue is deliberately separate from the DMA TRB: the event-driven
/// A/B must preserve the queued -> in-flight ownership transition even though
/// the early boot path uses a fixed setup buffer.
pub(super) unsafe fn queue_xbl_setup_request() -> bool {
    unsafe {
        if !cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
        )) {
            return true;
        }
        if EP0_SETUP_REQUEST_SLOT != usize::MAX
            && udc_mut().request(0, EP0_SETUP_REQUEST_SLOT).is_some()
        {
            return true;
        }
        EP0_SETUP_REQUEST_SLOT = usize::MAX;
        let Some(slot) = udc_mut().queue(0, 8) else {
            trace_event(TRACE_USB_DEVICE_ERROR, 0x58425155, 0, 0, 8, read(DSTS)); // "XBQU"
            return false;
        };
        EP0_SETUP_REQUEST_SLOT = slot;
        true
    }
}

/// Retire the XBL-style setup request after its setup TRB completes. A stale
/// slot is harmless after a reset because `queue_xbl_setup_request()` checks
/// the UDC object before reusing it.
unsafe fn complete_xbl_setup_request(error: bool) {
    unsafe {
        if !cfg!(any(
            fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
            fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
        )) {
            return;
        }
        let slot = EP0_SETUP_REQUEST_SLOT;
        if slot != usize::MAX {
            let _ = udc_mut().complete(0, slot, if error { 0 } else { 8 }, error);
            let _ = udc_mut().release(0, slot);
            EP0_SETUP_REQUEST_SLOT = usize::MAX;
        }
    }
}

/// Exercise the live EP0 event-DMA path after Run/Stop without changing the
/// host-facing SETUP transfer. The old diagnostic ended the live request and
/// issued a synthetic STARTTRANSFER/ENDTRANSFER pair; on hardware that could
/// remove the only EP0 request before host attach. GETEPSTATE is a read-only
/// endpoint command and, with CMDIOC, still gives us a command-complete event
/// to observe in the same event buffer.
#[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
pub(super) unsafe fn post_runstop_event_dma_probe() -> bool {
    unsafe {
        let command_ok = send_ep_command(0, DEPCMD_GETEPSTATE | DEPCMD_CMDIOC, 0, 0, 0);
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
            let slot = (EVENT_OFFSET % ep0_event_size()) & !0x3;
            event_word = read_volatile((ep0_event_dma_base() + slot) as *const u32);
            poll_ep0_event_ring();
        }
        trace_event(
            TRACE_EVENT_RING_READY,
            0x504F_5354, // "POST"
            command_ok as u32,
            EP0_SETUP_ARMED as u32,
            event_word | ((delivered as u32) << 31),
            read(DSTS),
        );
        if !command_ok || !delivered {
            trace_marker(TRACE_PROBE_WATCHDOG, 0x504F_5346); // "POSF"
            log_puts("usb: post-Run/Stop event DMA probe failed\n");
            return false;
        }
        true
    }
}

/// Best-effort SETUP arming for the poll-loop guard. Unlike `rearm_setup()`
/// this never tears the endpoint down on failure: the core rejects Start
/// Transfer while the link is not ON, and the guard simply retries on the
/// next poll until the link comes up.
pub(super) unsafe fn try_arm_setup() -> bool {
    unsafe {
        if EP0_SETUP_ARMED || !ENDPOINTS_READY || EP0_STATE != Ep0State::Setup {
            return EP0_SETUP_ARMED;
        }
        if ARM_COOLDOWN != 0 {
            ARM_COOLDOWN -= 1;
            return false;
        }
        // The normal path waits for the controller's U0 report because the
        // DWC3 programming guide rejects Start Transfer in suspend/reset.
        // POST-RECOVERY CORRECTION: the gate stays closed forever on this
        // revision (DSTS reads non-U0 link states 4/6/7/10/12/13 while the
        // host is already issuing tokens), which blocked every SETUP re-arm
        // and NAKed the whole first descriptor window (-110). The bus reset
        // ends before the host's first SETUP token, so issuing the re-arm
        // here is safe; only a genuinely halted controller is skipped.
        #[cfg(not(fullerene_aarch64_usb_gadget_handoff_start_ungated))]
        {
            let dsts = read(DSTS);
            if dsts & DSTS_DEVCTRLHLT != 0 {
                SETUP_ARM_FAILURE_STAGE = 1;
                return false;
            }
        }
        // This A/B deliberately issues STARTTRANSFER without consulting the
        // firmware-inherited DSTS halt/link readback. The Bramble handoff can
        // report a stale HALT bit while the host has already reached the
        // attach boundary; let the command result, rather than that status
        // read, decide whether the EP0 SETUP TRB can be armed.
        prepare_ep0_setup_trb();
        if start_transfer(0, ep0_trb_ptr(0)) {
            SETUP_ARM_FAILURE_STAGE = 0;
            EP0_SETUP_ARMED = true;
            PENDING_SETUP_ARM = false;
            trace_event(TRACE_SETUP_QUEUED, 0x4152_4D45, 0, 0, 0, read(DSTS)); // "ARME"
            true
        } else {
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_retry_setup)]
            if retry_start_transfer(0, ep0_trb_ptr(0), 400) {
                SETUP_ARM_FAILURE_STAGE = 0;
                EP0_SETUP_ARMED = true;
                PENDING_SETUP_ARM = false;
                trace_event(TRACE_SETUP_QUEUED, 0x5254_5259, 0, 0, 0, read(DSTS)); // "RTRY"
                return true;
            }
            // Fast-fail ("No resource" completes immediately). The host's
            // first SETUP token lands ~1 ms after its bus reset ends, so the
            // retry rate must place an armed SETUP TRB inside that window
            // while still bounding the total failed-command count.
            // BISECT: the 20-poll cooldown variant never reached the data
            // phase (-110 in every run); the 200-poll cooldown is the value
            // from the era when the device answered with a data-phase -71.
            SETUP_ARM_FAILURE_STAGE = if SETUP_ARM_LAST_COMMAND & 0x8000_0000 != 0
                || SETUP_ARM_LAST_COMMAND == 0xffff_ffff
            {
                3
            } else {
                2
            };
            ARM_COOLDOWN = 200;
            false
        }
    }
}

/// Recover an EP0 completion when the DWC3 event FIFO is empty but the
/// controller has already retired the DMA TRB.
///
/// This is deliberately opt-in and read-first: it never acknowledges
/// GEVNTCOUNT and never submits a command on its own.  A missing event-ring
/// record must not prevent a host SETUP/DATA/STATUS transfer from progressing
/// when the endpoint DMA engine did complete it.  The synthetic event enters
/// the same `process_event()` path as a normal DEPEVT_XferComplete, so the
/// existing gadget state machine remains the single owner of EP0 transitions.
pub(super) unsafe fn poll_ep0_trb_completion_fallback() -> bool {
    if !cfg!(fullerene_aarch64_usb_gadget_handoff_ep0_trb_completion_fallback) {
        return false;
    }

    unsafe {
        let endpoint = match EP0_STATE {
            Ep0State::Setup => 0,
            Ep0State::Data => usize::from(CONTROL_IN),
            Ep0State::Status => {
                if CONTROL_HAS_DATA && CONTROL_IN {
                    0
                } else {
                    1
                }
            }
        };
        let trb = ep0_trb_ptr(ep0_trb_index(endpoint));
        cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
        let ctrl = read_volatile(addr_of!((*trb).ctrl));

        // A SETUP payload is actionable even if this revision failed to clear
        // HWO while delivering the packet.  The setup buffer is aliased to
        // TRB0 on the source-aligned path, so compare its first two words with
        // the expected DMA address instead of treating the normal TRB address
        // as a stale packet.  DATA/STATUS still require a retired TRB because
        // their response buffer has no equivalent packet marker.
        let mut setup_received = false;
        if EP0_STATE == Ep0State::Setup {
            setup_received = setup_packet_pending();
        }
        if (EP0_STATE == Ep0State::Setup && !setup_received)
            || (EP0_STATE != Ep0State::Setup && ctrl & TRB_HWO != 0)
        {
            return false;
        }

        // DEPEVT_XferComplete: endpoint[5:1], event[9:6] = 1, and the
        // normal LST completion status in bits[15:12].
        let raw = ((endpoint as u32) << 1) | (1 << 6) | (0x8 << 12);
        trace_event(
            TRACE_TRANSFER_COMPLETE,
            0x444D_4150, // "DMAP": completion inferred from retired TRB
            endpoint as u32,
            ctrl,
            read(GEVNTCOUNT0),
            read(DSTS),
        );
        process_event(raw);
        true
    }
}

/// Re-arm EP0 only after a successful STARTTRANSFER command. On failure the
/// endpoint is removed from DALEPENA so a host cannot continue sending SETUP
/// packets into a stale resource; the next Connect Done/USB reset can rebuild
/// the endpoint allocation.
pub(super) unsafe fn rearm_setup() -> bool {
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
pub(super) unsafe fn reset_gsi_channels() {
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
pub(super) unsafe fn restart_control_after_reset() {
    unsafe {
        // qpr1 leaves GCTL.RAMCLKSEL at the reset value after the host's bus
        // USB reset; reapply_ramclksel() is a disabled-by-default legacy A/B.
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
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)]
            {
                // qpr1's dwc3_gadget_reset_interrupt() notifies the gadget,
                // clears test mode, revokes active transfers, and clears
                // endpoint stalls in that order. The ordinary Fullerene path
                // intentionally preserves its armed control transfer; this
                // source-order A/B follows qpr1 and lets the common re-arm
                // tail below publish a fresh SETUP transfer.
                GadgetDriver::reset(gadget_mut());
                let dctl = read(DCTL);
                write(DCTL, dctl & !DCTL_TSTCTRL_MASK);
                let transfer_ok = stop_active_ep0_at_reset();
                let out_ok = send_ep_command(0, DEPCMD_CLEARSTALL, 0, 0, 0);
                let in_ok = send_ep_command(1, DEPCMD_CLEARSTALL, 0, 0, 0);
                EP0_SETUP_ARMED = false;
                trace_event(
                    TRACE_USB_RESET,
                    0x51525354, // "QRST": qpr1 reset sequence
                    transfer_ok as u32,
                    (out_ok as u32) | ((in_ok as u32) << 1),
                    dctl & DCTL_TSTCTRL_MASK,
                    read(DSTS),
                );
            }
            if !cfg!(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order) {
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)]
                {
                    // Android msm's dwc3_gadget_reset_interrupt() first notifies
                    // the gadget driver, then clears DCTL.TSTCTRL, and only then
                    // clears endpoint stalls. Keep the armed EP0 SETUP transfer
                    // and DMA addresses intact while reproducing that complete
                    // reset-time state order as one explicit hardware A/B.
                    GadgetDriver::reset(gadget_mut());
                    let dctl = read(DCTL);
                    write(DCTL, dctl & !DCTL_TSTCTRL_MASK);
                    let out_ok = send_ep_command(0, DEPCMD_CLEARSTALL, 0, 0, 0);
                    let in_ok = send_ep_command(1, DEPCMD_CLEARSTALL, 0, 0, 0);
                    trace_event(
                        TRACE_USB_RESET,
                        0x41525354, // "ARST"
                        out_ok as u32,
                        in_ok as u32,
                        dctl & DCTL_TSTCTRL_MASK,
                        read(DSTS),
                    );
                }
                #[cfg(all(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_callback_first,
                    not(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)
                ))]
                {
                    // Android msm calls usb_gadget_udc_reset() before its
                    // controller-side stop/clear-stall cleanup. Move only the
                    // existing gadget callback in this A/B; EP0 ownership and
                    // all controller commands remain otherwise unchanged.
                    GadgetDriver::reset(gadget_mut());
                    trace_event(
                        TRACE_USB_RESET,
                        0x52434246, // "RCBF"
                        1,
                        0,
                        0,
                        read(DSTS),
                    );
                }
                #[cfg(all(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_stall,
                    not(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)
                ))]
                {
                    // Android msm's dwc3_clear_stall_all_ep() clears EP0 OUT and
                    // IN after USB Reset without stopping or re-arming the
                    // preserved SETUP transfer. Keep this as an isolated A/B:
                    // the normal preserve path must not issue extra commands.
                    let out_ok = send_ep_command(0, DEPCMD_CLEARSTALL, 0, 0, 0);
                    let in_ok = send_ep_command(1, DEPCMD_CLEARSTALL, 0, 0, 0);
                    trace_event(
                        TRACE_USB_RESET,
                        0x5253544C, // "RSTL"
                        out_ok as u32,
                        in_ok as u32,
                        0,
                        read(DSTS),
                    );
                }
                #[cfg(all(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_clear_test_mode,
                    not(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order)
                ))]
                {
                    // Android msm clears DCTL.TSTCTRL in its bus-reset handler
                    // before preserving the EP0 SETUP transfer. Apply only that
                    // register correction in this A/B; Run/Stop and the EP0
                    // ownership boundary remain unchanged.
                    let dctl = read(DCTL);
                    write(DCTL, dctl & !DCTL_TSTCTRL_MASK);
                    trace_event(
                        TRACE_USB_RESET,
                        0x54455354, // "TEST"
                        dctl & DCTL_TSTCTRL_MASK,
                        0,
                        0,
                        read(DSTS),
                    );
                }
                if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_resource) {
                    // This opt-in A/B deliberately tests the opposite hardware
                    // hypothesis from the Android-compatible preserve path: a
                    // bus reset may leave the EP0 contexts intact while losing
                    // their transfer-resource allocation. Re-issue only
                    // SETTRANSFRESOURCE and keep the armed SETUP TRB, endpoint
                    // ownership, and returned STARTTRANSFER indices unchanged.
                    let out_ok = set_transfer_resource(0);
                    let in_ok = set_transfer_resource(1);
                    trace_event(
                        TRACE_USB_RESET,
                        0x52535243, // "RSRC"
                        out_ok as u32,
                        in_ok as u32,
                        1,
                        read(DSTS),
                    );
                }
                let dcfg = read(DCFG) & !DCFG_DEVADDR_MASK;
                write(DCFG, dcfg);
                unbind_function();
                teardown_data_endpoints();
                reset_gsi_channels();
                #[cfg(not(any(
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_callback_first,
                    fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order
                )))]
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
                if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_endpoints) {
                    // This broader opt-in A/B tests whether the endpoint context
                    // itself is lost across the bus reset. Unlike the normal
                    // Android-compatible preserve path, rebuild both EP0
                    // contexts, clear the old STARTTRANSFER resource indices,
                    // and let the ordinary post-reset arm retry at link ON.
                    let speed = read(DSTS) & DSTS_CONNECTSPD_MASK;
                    let max_packet = if speed == DSTS_SUPERSPEED { 512 } else { 64 };
                    write(DALEPENA, 0);
                    let rebuilt = send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0)
                        && configure_endpoint(0, max_packet, false)
                        && configure_endpoint(1, max_packet, false);
                    if rebuilt {
                        let _ = udc_mut().configure_endpoint(0, max_packet as u16, false);
                        let _ = udc_mut().configure_endpoint(1, max_packet as u16, false);
                    } else {
                        log_puts("usb: EP0 reset endpoint rebuild failed\n");
                    }
                    ENDPOINTS_READY = rebuilt;
                    EP0_RESOURCE_INDEX = [0; 2];
                    EP0_SETUP_ARMED = false;
                    PENDING_SETUP_ARM = true;
                    write(DALEPENA, if rebuilt { 0b11 } else { 0 });
                    trace_event(
                        TRACE_USB_RESET,
                        0x52455043, // "REPC"
                        rebuilt as u32,
                        max_packet,
                        0,
                        read(DSTS),
                    );
                    let _ = try_arm_setup();
                }
                // In the default path EP0_STATE, EP0_SETUP_ARMED,
                // EP0_RESOURCE_INDEX, ENDPOINTS_READY, DALEPENA, DCFG.speed, and
                // the armed SETUP TRB are preserved. Android msm's
                // dwc3_gadget_reset_interrupt() does not stop or re-arm EP0: the
                // initial SETUP transfer remains owned by the core across USB
                // reset, and Connect Done later MODIFYs the EP0 contexts for the
                // negotiated speed. Issuing ENDTRANSFER or a second STARTTRANSFER
                // here races the host's first post-reset SETUP token and loses
                // the descriptor window.
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
        if !cfg!(fullerene_aarch64_usb_gadget_handoff_ep0_reset_android_state_order) {
            GadgetDriver::reset(gadget_mut());
        }
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
        DATA_PHASE_PENDING_START = false;
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
            // Some handoff revisions flush the EP0 allocation window at USB
            // bus reset even though the controller remains in device mode.
            // Rebuild the endpoint state only for this A/B so the normal
            // reset path keeps the Linux-compatible behavior.
            if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_endpoints) {
                write(DALEPENA, 0);
                let rebuilt = send_ep_command(0, DEPCMD_DEPSTARTCFG, 0, 0, 0)
                    && configure_endpoint(0, max_packet, false)
                    && configure_endpoint(1, max_packet, false);
                if !rebuilt {
                    log_puts("usb: EP0 reset endpoint rebuild failed\n");
                    trace_event(TRACE_USB_RESET, 0x52455043, 0, 0, 0, read(DSTS));
                }
                ENDPOINTS_READY = rebuilt;
            } else if cfg!(fullerene_aarch64_usb_gadget_handoff_reset_resource) {
                // A narrower resource-only A/B for revisions where the
                // endpoint configuration survives but the allocation does
                // not.
                let out_ready = set_transfer_resource(0);
                let in_ready = set_transfer_resource(1);
                if !out_ready || !in_ready {
                    log_puts("usb: EP0 reset resource reallocation failed\n");
                    trace_event(
                        TRACE_USB_RESET,
                        0x52535243, // "RSRC"
                        out_ready as u32,
                        in_ready as u32,
                        0,
                        read(DSTS),
                    );
                }
            }
            write(DALEPENA, 0b11);
            if cfg!(any(
                fullerene_aarch64_usb_gadget_handoff_xbl_deferred_setup,
                fullerene_aarch64_usb_gadget_handoff_xbl_between_ep0
            )) {
                // USB reset cleared the software request object; the
                // historical XBL differential posts its 8-byte EP0 OUT
                // request again. This is not the canonical initial SETUP
                // arm boundary: Linux/Android arm CONTROL_SETUP eagerly.
                let _ = queue_xbl_setup_request();
            }
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
            // The protocol layer exposes one ADB-class-compatible function
            // with either an ordinary bulk pair or an explicitly supplied
            // IPA/GSI binding. Configure it only after SET_CONFIGURATION has
            // committed, matching gadget-core ordering.
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
                        DATA_OUT_BUFFER_SIZE,
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

pub(super) unsafe fn start_status(endpoint: usize) -> bool {
    let kind = if unsafe { CONTROL_HAS_DATA } {
        TRB_CONTROL_STATUS3
    } else {
        TRB_CONTROL_STATUS2
    };
    trace_event(TRACE_STATUS_QUEUED, 0, endpoint as u32, kind, 0, unsafe {
        read(DSTS)
    });
    unsafe {
        let trb_index = ep0_trb_index(endpoint);
        prepare_trb(trb_index, ep0_trb_ptr(trb_index).cast::<u8>(), 0, kind);
        // Same flaky Start Transfer window as the data phase: retry the
        // command instead of failing the status stage (SET_ADDRESS and
        // SET_CONFIGURATION become visible only after this ZLP completes).
        retry_start_transfer(endpoint, ep0_trb_ptr(trb_index), 2500)
    }
}

unsafe fn stall_control(endpoint: usize) {
    // Linux's gadget core responds to an unsupported control request with a
    // real EP0 STALL. Leaving the endpoint idle is not equivalent: hosts may
    // keep waiting for the missing handshake and never issue the next SETUP.
    let _ = unsafe { send_ep_command(endpoint, DEPCMD_SETSTALL, 0, 0, 0) };
    unsafe {
        EP0_STATE = Ep0State::Setup;
        DATA_PHASE_PENDING_START = false;
    }
}

/// Answer a host SETUP that arrived without a device event.
///
/// `handle_setup()` already latches the packet out of the EP0 SETUP DMA buffer
/// and zeroes that buffer, so a non-zero buffer is a fresh packet. The event
/// path is the normal trigger, but on this handoff `GEVNTCOUNT0` stays 0 even
/// though the host's traffic reaches the controller, so polling the buffer is
/// the only way left to answer the host's `GET_DESCRIPTOR`. Selected with
/// `--utmi-postrun-readout usb2-live-setup-poll`; it is a no-op otherwise.
pub(super) unsafe fn poll_setup_buffer() -> bool {
    // `usb2-live-force-endpoints` and `usb2-live-rearm-loop` bundle this path:
    // those selectors exist to answer the host with no device events at all,
    // which is exactly when this eventless SETUP reader is needed.
    // NO GATE: answering the host must not depend on a diagnostic selector being passed.
    // In every ordinary run this returned false immediately, `handle_setup()` never ran, and
    // the host's GET_DESCRIPTOR went unanswered. See usb/README.md §2 (defect 6).
    // Cost when idle: one `cache_invalidate` and eight `read_volatile`s per pass, and
    // `handle_setup()` only when the buffer is non-zero.
    unsafe {
        // POLL-SETUP-BUFFER BREADCRUMBS (level 9). `bc3!(1)` fires and `bc3!(2)` does not, with only
        // this call between them, and `bc8!(8)` shows `handle_setup()` was never entered - so the block
        // is one of the three DRAM-side steps below. None of them is MMIO, so whichever it is needs a
        // specific explanation (cache maintenance on a bad address, or a read that resolves through a
        // mapping that never completes). See usb/README.md §3.27.
        macro_rules! bc9 {
            ($i:expr) => {
                if matches!(
                    option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
                    Some("9") | Some("10") | Some("4")
                ) && unsafe { !trace::BC_ONCE[$i] }
                {
                    unsafe {
                        trace::BC_ONCE[$i] = true;
                        ccs_pulse(300);
                    }
                }
            };
        }
        bc9!(16); // poll_setup_buffer entered
        let setup = ep0_setup_data_ptr();
        bc9!(17); // ep0_setup_data_ptr() returned
        cache_invalidate(setup as usize, 8);
        bc9!(18); // cache_invalidate returned
        let fresh = setup_packet_pending();
        bc9!(19); // the eight-byte DRAM probe returned
        if fresh {
            trace_marker(TRACE_SETUP_RECEIVED, 0x5345_5450); // "SETP"
            handle_setup();
        }
        fresh
    }
}

unsafe fn setup_request() -> [u8; 8] {
    let mut packet = [0; 8];
    unsafe {
        let setup = ep0_setup_data_ptr();
        cache_invalidate(setup as usize, 8);
        core::ptr::copy_nonoverlapping(setup, packet.as_mut_ptr(), 8);
    }
    packet
}

pub(super) unsafe fn handle_setup() {
    let packet = unsafe { setup_request() };
    // HANDLE-SETUP BREADCRUMBS (level 8). `poll_setup_buffer()` is DRAM-only and cannot block, yet
    // it never returns after `bc3!(1)`. That leaves `handle_setup()` - reached only when the SETUP
    // buffer holds non-zero bytes, which means the host's SETUP really did arrive - and its last
    // two stages touch the controller. These four pulses say which stage does not return.
    // See usb/README.md §3.26.
    macro_rules! bc8 {
        ($i:expr) => {
            if matches!(
                option_env!("FULLERENE_USB_PULSE_BREADCRUMB"),
                Some("8") | Some("4")
            ) && unsafe { !trace::BC_ONCE[$i] }
            {
                unsafe {
                    trace::BC_ONCE[$i] = true;
                    ccs_pulse(300);
                }
            }
        };
    }
    bc8!(8); // handle_setup() entered
    #[cfg(fullerene_aarch64_usb_ep0_signal_probe)]
    unsafe {
        // The SETUP buffer is cleared immediately below so a later poll can
        // distinguish a fresh packet. Preserve a diagnostic latch before
        // clearing it; otherwise the host-visible early-drop probe can miss
        // a real DMA delivery between two 1 ms samples.
        SIGNAL_SETUP_PACKET_RECEIVED = true;
    }
    bc8!(9); // setup_request() + the DRAM-only packet parse returned
    // Zero the DMA buffer after latching the packet: a later non-zero
    // buffer then proves the core delivered a NEW SETUP packet, even while
    // the software state machine was still in the Data/Status phase (the
    // host aborts in-flight control transfers with a new SETUP - Linux
    // handles this via its setup_packet_pending logic).
    unsafe { clear_setup_packet() };
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
        bc8!(10); // trace_event(..., read(DSTS)) returned
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
    // Latch which action the callback returned: the response-phase words all read their
    bc8!(11); // GadgetDriver::on_setup() returned
    // initial values when no reply is built, so this is the one remaining unknown.
    // See usb/README.md §3.7.
    unsafe {
        LAST_CONTROL_ACTION = match action {
            ControlAction::DataIn(_) => 1,
            ControlAction::StatusIn => 2,
            ControlAction::StatusOut => 3,
            ControlAction::SetHalt(_) => 4,
            ControlAction::ClearHalt(_) => 5,
            ControlAction::Setup => 6,
            ControlAction::Stall => 7,
        };
    }
    match action {
        ControlAction::DataIn(mut length) => unsafe {
            #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_short_first_desc)]
            if request == 0x06 && (value >> 8) == 0x01 && requested_length == 64 {
                // The first device-descriptor read tolerates a short packet;
                // cap the data phase at 8 bytes so a long-packet TX failure
                // cannot hide behind the host's 5 s control timeout.
                length = length.min(8);
            }
            let response = ep0_response_ptr();
            cache_clean(response as usize, length);
            let data_trb_kind = if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_in_data) {
                // Stock XBL's EP0 IN request builder emits a NORMAL TRB
                // (TRBCTL=1), even though this is the control-transfer data
                // phase. Keep this as a narrow A/B; setup and status remain
                // on the ordinary control TRB path.
                TRB_NORMAL
            } else {
                TRB_CONTROL_DATA
            };
            let trb_index = ep0_trb_index(1);
            prepare_trb(trb_index, response, length, data_trb_kind);
            trace_event(
                TRACE_DESCRIPTOR_QUEUED,
                request as u32,
                value as u32,
                index as u32,
                length as u32,
                read(DSTS),
            );
            EP0_STATE = Ep0State::Data;
            // Android msm 4.19's __dwc3_gadget_ep0_queue() starts the DATA
            // phase immediately after preparing the response TRB. Waiting
            // for XferNotReady(DATA) here can miss the host's first IN
            // token and leave EP1 NAKing the entire control window (-110).
            // Keep the NRDY path only as recovery if this initial command is
            // rejected while the link is still settling.
            DATA_PHASE_PENDING_LEN = length;
            let started = retry_start_transfer(1, ep0_trb_ptr(trb_index), 2500);
            DATA_PHASE_PENDING_START = !started;
            if started {
                note_probe_ep0_progress();
            }
        },
        ControlAction::StatusIn => unsafe {
            EP0_STATE = Ep0State::Status;
            // The status phase suffers the same XferNotReady boundary as the
            // data phase: an immediate start_status(1) retires cleanly and is
            // ignored. The XferNotReady(CONTROL_STATUS) handler below owns
            // the arm, with the correct endpoint for the transfer direction.
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

pub(super) unsafe fn process_event(raw: u32) {
    EVENTS_CONSUMED = EVENTS_CONSUMED.saturating_add(1);
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
                DISCONNECT_SEEN = true;
                if DISCONNECT_RAW == 0 {
                    DISCONNECT_RAW = raw;
                }
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
                note_runtime_event(super::super::platform::bramble::UsbRuntimeEvent::Disconnect);
            }
            1 => {
                SIGNAL_USB_RESET_SEEN = true;
                trace_utmi_state(6);
                trace_event(TRACE_USB_RESET, 0, 0, 0, 0, raw);
                note_runtime_event(super::super::platform::bramble::UsbRuntimeEvent::BusReset);
                #[cfg(fullerene_aarch64_usb_gadget_handoff_usb3_link_training_after_reset)]
                unsafe {
                    // Android's dwc3_gadget_reset_interrupt() starts the
                    // optional QMP RX-equalization workaround before stopping
                    // active transfers. The helper is status-gated and is a
                    // no-op when the PHY is not in equalization.
                    let training = phy::qmp_start_link_training();
                    trace_event(
                        TRACE_USB_RESET,
                        0x514c_5453, // "QLTS": QMP link-training start
                        training,
                        0,
                        0,
                        read(DSTS),
                    );
                }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_usb2_clear_susphy_after_reset)]
                unsafe {
                    // qpr1's USB2 PHY suspend policy is revisited at the
                    // host-reset boundary. Keep this write isolated from the
                    // existing Run/Stop A/B so a reset-time PHY wake can be
                    // tested without changing the initial handoff.
                    let before = read(GUSB2PHYCFG0);
                    mark_g2w_site(1003);
                    write(GUSB2PHYCFG0, before & !GUSB2PHYCFG_SUSPHY);
                    let after = read(GUSB2PHYCFG0);
                    trace_event(
                        TRACE_USB_RESET,
                        0x53555352, // "SUSR": USB2 SUSPHY reset-boundary A/B
                        before,
                        after,
                        0,
                        read(DSTS),
                    );
                }
                unsafe { restart_control_after_reset() }
                #[cfg(fullerene_aarch64_usb_gadget_handoff_dalepena_after_reset)]
                unsafe {
                    // This is deliberately after the reset handler returns:
                    // it tests the publication boundary without rebuilding
                    // EP0 contexts, changing the SETUP TRB, or touching the
                    // PHY/UTMI path. If the bus reset cleared only the active
                    // endpoint mask, this is the narrowest recovery action.
                    let before = read(DALEPENA);
                    write(DALEPENA, 0b11);
                    let after = read(DALEPENA);
                    trace::live_dalepena_after_reset(before, after);
                    trace_event(
                        TRACE_USB_RESET,
                        0x4441_4C45, // "DALE"
                        before,
                        after,
                        0,
                        read(DSTS),
                    );
                }
                trace_utmi_state(7);
            }
            2 => {
                trace_event(TRACE_DEVICE_CONNECT, 0, 0, 0, 0, raw);
                let speed = unsafe { read(DSTS) & DSTS_CONNECTSPD_MASK };
                log_puts("usb: connect done, speed=");
                log_hex_value(speed as u64);
                if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_source_exact_devten) {
                    // qpr1 adds EOPFEN (the revision-2.30a suspend event bit)
                    // here, after Connect Done and before EP0 MODIFY.
                    unsafe { qpr1_enable_eopf_on_connect_done() };
                }
                unsafe { configure_android_hs_connect_done_policy(speed) };
                #[cfg(fullerene_aarch64_usb_gadget_handoff_ss_conndone_clear_hird)]
                if speed == DSTS_SUPERSPEED {
                    // qpr1's dwc3_gadget_conndone_interrupt() clears the
                    // HIRD threshold on the non-HS branch. The regular
                    // handoff leaves XBL's pre-connect value in DCTL; keep
                    // this source-aligned write isolated for an A/B.
                    unsafe {
                        let dctl = read(DCTL) & !DCTL_HIRD_THRES_MASK;
                        write(DCTL, dctl);
                        let _ = read(DCTL);
                    }
                }
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
                    // rewrite DALEPENA or re-arm while EP0 holds an armed
                    // SETUP TRB. Linux's conndone path does, however, issue a
                    // DEPCFG MODIFY for both EP0 directions so a USB2 link
                    // changes the initial 512-byte state to 64 bytes. Leaving
                    // the pre-connect packet size in place makes the host's
                    // first descriptor transaction use the wrong EP0 context.
                    if !first_connect && EP0_STATE == Ep0State::Setup && EP0_SETUP_ARMED {
                        let modified = configure_endpoint(0, max_packet, true)
                            && configure_endpoint(1, max_packet, true);
                        if modified {
                            let _ = udc_mut().configure_endpoint(0, max_packet as u16, true);
                            let _ = udc_mut().configure_endpoint(1, max_packet as u16, true);
                            note_runtime_event(
                                super::super::platform::bramble::UsbRuntimeEvent::ControllerStarted,
                            );
                        } else {
                            log_puts("usb: Connect Done EP0 MODIFY failed\n");
                            trace_event(
                                TRACE_USB_DEVICE_ERROR,
                                0x434D_4F44, // "CMOD"
                                max_packet,
                                0,
                                0,
                                read(DSTS),
                            );
                        }
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
                            super::super::platform::bramble::UsbRuntimeEvent::ControllerStarted,
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
                SIGNAL_DWC3_DEVICE_ERROR = true;
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
            if EP0_STATE == Ep0State::Setup {
                // EP0 XferComplete status 0x8 is the normal LST indication,
                // not an error. The request carries the same successful
                // setup completion in the XBL path.
                complete_xbl_setup_request(false);
            }
            // A freshly DMAed SETUP packet overrides any in-flight phase:
            // hosts abort stalled control transfers by sending a new SETUP,
            // and the completion event for the OLD transfer carries it.
            // Linux recovers via setup_packet_pending; without this the new
            // SETUP is dispatched into the stale Data/Status handler and the
            // request is silently lost (the mid-enumeration death).
            let fresh_setup = setup_packet_pending();
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
        // event for the harvest gates (request=endpoint, value=status).
        // XferNotReady is a notification for a later control phase, not the
        // initial SETUP arm point. The ordinary path has already armed EP0;
        // retain the event trace while handling DATA/STATUS below.
        trace_event(TRACE_XFER_NOT_READY, endpoint as u32, status, 0, 0, raw);
        if status == 1 && endpoint == 1 {
            unsafe {
                // CONTROL_DATA NRDY on EP1 IN is normally informational after
                // the immediate Android-style arm above. It remains a
                // recovery boundary only when the initial STARTTRANSFER was
                // rejected while the link was settling; the host keeps
                // tokening the data phase and the next event retries it.
                if DATA_PHASE_PENDING_START && EP0_STATE == Ep0State::Data {
                    let length = DATA_PHASE_PENDING_LEN;
                    let trb_index = ep0_trb_index(1);
                    prepare_trb(
                        trb_index,
                        ep0_response_ptr(),
                        length,
                        if cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_in_data) {
                            TRB_NORMAL
                        } else {
                            TRB_CONTROL_DATA
                        },
                    );
                    let queued = retry_start_transfer(1, ep0_trb_ptr(trb_index), 200);
                    trace_event(
                        TRACE_DESCRIPTOR_QUEUED,
                        0x4441_524D, // "DARM" data-phase arm outcome
                        queued as u32,
                        0,
                        length as u32,
                        read(DSTS),
                    );
                    if queued {
                        DATA_PHASE_PENDING_START = false;
                        note_probe_ep0_progress();
                    }
                }
            }
        }
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
        if endpoint == 2 {
            let data = core::slice::from_raw_parts(
                addr_of!(DATA_OUT_BUFFER.0).cast::<u8>(),
                actual as usize,
            );
            debug_transport::on_bulk_out(data, error);
        } else {
            debug_transport::on_bulk_in_complete(error);
        }
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
                DATA_OUT_BUFFER_SIZE,
            );
        }
    }
}
