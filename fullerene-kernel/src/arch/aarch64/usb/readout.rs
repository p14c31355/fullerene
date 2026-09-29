//! DWC3 and UTMI diagnostic readout decoding and trace capture.

use super::*;

/// Scan the retained trace backwards for the last STARTTRANSFER command
/// outcome. Called at the start of every handoff attempt except the first:
/// attempt N therefore reads attempt N-1's records, which are still intact
/// because the trace survives the in-boot DMA-region clear.
pub(super) unsafe fn harvest_trace_outcome() {
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
        TRACE_HARVEST_LAST_SETUP = 0xFFFF_FFFF;
        TRACE_HARVEST_EP1_XFER = 0xFFFF_FFFF;
        TRACE_HARVEST_EP1_NRDY = 0;
        TRACE_HARVEST_POST = 0xFFFF_FFFF;
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
                // First hit of the backward scan is the newest SETUP.
                if TRACE_HARVEST_LAST_SETUP == 0xFFFF_FFFF {
                    TRACE_HARVEST_LAST_SETUP =
                        (request << 16) | (read_volatile(addr_of!((*entry).length)) & 0xffff);
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
            if event == TRACE_EVENT_RING_READY
                && read_volatile(addr_of!((*entry).request)) == 0x504F_5354
                && TRACE_HARVEST_POST == 0xFFFF_FFFF
            {
                // The post probe stores command_ok/ep0_armed in value/index
                // and the first event word in length. A nonzero event word is
                // the observed delivery marker; the record itself lets the
                // gate distinguish "probe not compiled/reached" from a
                // probe that ran and failed one of its checks.
                let command_ok = read_volatile(addr_of!((*entry).value)) & 1;
                let ep0_armed = read_volatile(addr_of!((*entry).index)) & 1;
                let delivered = (read_volatile(addr_of!((*entry).length)) >> 31) & 1;
                TRACE_HARVEST_POST = 0x1_0000 | command_ok | (ep0_armed << 1) | (delivered << 2);
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
                    // The timeout flag must not collide with the returned
                    // XferRscIdx (DEPCMD bits 22:16): physical EP1's healthy
                    // resource index is 1, so a completed EP1 STARTTRANSFER
                    // sets bit 16 and a bit-16 flag misreads it as a wedge.
                    0x8000_0000 | raw
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

/// Composite "diag" gate readout: re-harvest THIS run's live trace and fold
/// the enumeration progress into a 1..9 code. The host is a 6.8 xHCI kernel
/// using the "new scheme", so the FIRST control transfer is already the
/// 64-byte device-descriptor read and every SETUP below is one of its two
/// attempts (there is no 8-byte first read):
///   1 = no SETUP ever reached EP0 (event ring / OUT path / CPU hung)
///   2 = only the first SETUP (read/64 attempt 1)
///   3 = both attempts dispatched but the newest queued no DataIn (on_setup
///       stalled or answered with a non-data action)
///   4 = the read/64 data phase was queued but its Start Transfer failed
///   5 = the read/64 data phase armed cleanly but the core never fetched
///       the data TRB
///   6 = the core fetched the read/64 data TRB; the IN token went
///       unanswered (link / core DMA / host side)
///   7 = an EP1 transfer-complete event arrived after the arm (the data
///       phase left the core; status 0 = the host should have the bytes)
///   8 = no completion, but the data TRB's HWO was cleared over DMA: the
///       core consumed the TRB and the transfer silently died (FIFO/PHY)
///   9 = no completion and HWO still set: the core never consumed the
///       armed TRB (the doorbell/resource was lost after the command
///       retired)
///  10 = a completion happened AND the control status was queued
///       (TRACE_STATUS_QUEUED >= 1): the SET_ADDRESS status arm ran, so a
///       failure past this point is wire-level, not arm-machinery
/// DESC counts every TRACE_DESCRIPTOR_QUEUED entry, and each DataIn arm
/// writes TWO (the descriptor record plus the "DARM" marker), so two arms
/// (read/64 attempts 1 + 2) give DESC >= 4. DARM holds the NEWEST arm
/// outcome, which is the newest read/64 arm once DESC >= 4. EP1_NRDY counts
/// the XferNotReady(CONTROL_DATA) events on the data endpoint, one per
/// attempt.
pub fn diag_readout_code() -> u32 {
    unsafe {
        harvest_trace_outcome();
        let mut code = 1u32;
        if TRACE_HARVEST_SETUP > 0 {
            code = 2;
            if TRACE_HARVEST_SETUP >= 2 {
                code = 3;
                if TRACE_HARVEST_DESC >= 4 {
                    code = 4;
                    if TRACE_HARVEST_DARM == 0x1_0001 {
                        code = 5;
                        if TRACE_HARVEST_EP1_NRDY >= 2 {
                            code = 6;
                            if TRACE_HARVEST_EP1_XFER != 0xFFFF_FFFF {
                                code = 7;
                                if TRACE_HARVEST_STATUSQ >= 1 {
                                    code = 10;
                                }
                            } else {
                                // Post-mortem on the data TRB the arm
                                // queued: the core clears HWO over DMA
                                // when it consumes the TRB, so a still-set
                                // HWO means the transfer never left the
                                // doorbell. Invalidate first: the writeback
                                // is the core's, not ours.
                                let trb = ep0_trb_ptr(ep0_trb_index(1));
                                cache_invalidate(trb as usize, core::mem::size_of::<Trb>());
                                let ctrl = read_volatile(addr_of!((*trb).ctrl));
                                if ctrl & TRB_HWO == 0 {
                                    code = 8;
                                } else {
                                    code = 9;
                                }
                            }
                        }
                    }
                }
            }
        }
        code
    }
}

/// Return a host-readable nibble from the newest retained SS snapshot.
/// The probe uses this only for a timing-channel diagnostic; it does not alter
/// the USB controller or clock state.
pub(super) unsafe fn capture_ss_state_snapshot() {
    unsafe {
        SS_STATE_SNAPSHOT_DSTS = read(DSTS);
        SS_STATE_SNAPSHOT_DCTL = read(DCTL);
        SS_STATE_SNAPSHOT_PIPE = read(GUSB3PIPECTL0);
        SS_STATE_SNAPSHOT_GCTL = read(GCTL);
        SS_STATE_SNAPSHOT_QSCRATCH = read_qscratch(QSCRATCH_SS_PHY_CTRL);
        SS_STATE_SNAPSHOT_QSCRATCH_GENERAL = read_qscratch(QSCRATCH_GENERAL_CFG);
        SS_STATE_SNAPSHOT_QMP = phy::qmp_post_runstop_snapshot();
        SS_STATE_SNAPSHOT_QMP_STATUS2 = phy::qmp_status2_snapshot();
        SS_STATE_SNAPSHOT_QMP_POWER = phy::qmp_power_snapshot();
        let clocks = super::super::platform::bramble::usb_clock::read_usb_clock_register_state();
        SS_STATE_SNAPSHOT_QMP_BRANCHES = clocks.qmp_branches;
        let gdsc = core::ptr::read_volatile(
            super::super::platform::bramble::usb_resources().gdsc as *const u32,
        );
        let snpsid = read(GSNPSID);
        let ltssm_reg = if matches!(snpsid >> 16, DWC31_IP | DWC32_IP) {
            DWC31_LINK_GDBGLTSSM
        } else {
            GDBGLTSSM
        };
        SS_STATE_SNAPSHOT_LTSSM = (read(ltssm_reg) >> 22) & 0xf;
        let domain = u32::from(known_dwc_core_ip(snpsid))
            | (u32::from(gdsc & (1 << 31) != 0) << 1)
            | (u32::from(
                clocks.controller_branches[0] & 1 != 0
                    && clocks.controller_branches[0] & (1 << 31) == 0,
            ) << 2)
            | (u32::from(
                clocks.controller_branches[3] & 1 != 0
                    && clocks.controller_branches[3] & (1 << 31) == 0,
            ) << 3);
        SS_STATE_SNAPSHOT_DOMAIN = domain;
        trace_event(
            TRACE_UTMI_STATE,
            0x0300_0000 | domain,
            snpsid,
            gdsc,
            clocks.core_source_config,
            clocks.utmi_source_config,
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0400_0000 | domain,
            clocks.controller_branches[0],
            clocks.controller_branches[3],
            clocks.controller_branches[1],
            clocks.controller_branches[2],
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0500_0000 | domain,
            clocks.controller_branches[4],
            clocks.controller_branches[5],
            0,
            0,
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0600_0000 | domain,
            clocks.qmp_branches[0],
            clocks.qmp_branches[1],
            clocks.qmp_branches[3],
            0,
        );
        trace_event(
            TRACE_UTMI_STATE,
            0x0700_0000 | domain,
            SS_STATE_SNAPSHOT_QSCRATCH_GENERAL,
            SS_STATE_SNAPSHOT_QSCRATCH,
            SS_STATE_SNAPSHOT_QMP_POWER,
            SS_STATE_SNAPSHOT_LTSSM,
        );
        SS_STATE_SNAPSHOT_VALID = true;
    }
}

/// Compute one of the live, read-only 4-bit USB2 controller words.
///
/// `domain` reuses the exact field extraction of `capture_ss_state_snapshot()`
/// (core IP answered, USB30 GDSC powered, core clock branch, mock-UTMI branch).
/// `blockers` encodes the three states the vendor DWC3 source names as stopping
/// device-event generation while the controller still retires endpoint
/// commands: DCTL.CSFTRST still asserted (gadget.c:2109-2119), GCTL.
/// CORESOFTRESET still asserted (dwc3-msm.c:2028-2030), and DEVTEN never
/// programmed (dwc3_gadget_enable_irq(), gadget.c:2324-2343). Unknown names
/// return 15 so a typo cannot masquerade as a valid zero word.
/// Capture the DWC3 core id into the crate-shared retained region, once.
///
/// Safe to call at any point where the controller answers: it no-ops after the first
/// successful read. Called from the handoff's power re-assert, which is the last moment the
/// aperture is known-good before the readout path runs.
pub fn latch_snpsid() {
    unsafe {
        if trace::SHARED_SNPSID == 0 {
            trace::SHARED_SNPSID = read(GSNPSID);
        }
    }
}

#[inline]
fn cached_snpsid() -> u32 {
    // Never touch the controller from here: measured to hang when the USB clock branch has
    // collapsed. A zero cache means the latch never ran, which is a *diagnostic* result, not
    // a reason to go reading MMIO. See usb/README.md §1.6.
    unsafe { trace::SHARED_SNPSID }
}

pub(super) unsafe fn usb2_live_word(word: &str) -> u32 {
    // ---- Which GUSB2PHYCFG0 call site ran last? ----
    // Explicit tags, because `Location::caller()` recorded 0 through `#[inline]`
    // and every proxy predicate tried before this one answered a different
    // question. Ids: 1xxx = mod.rs, 2xxx = config.rs, 3xxx = control.rs.
    //
    //   g2wtag-eq-<id>  : the last writer was exactly this id
    //   g2wtag-ge-<id>  : the last writer's id is >= this (bisect within a file)
    //   g2wtag-file     : 0 = none yet, 1 = mod.rs, 2 = config.rs, 3 = control.rs
    if let Some(rest) = word.strip_prefix("g2wtag-eq-") {
        let id: u32 = rest.parse().unwrap_or(u32::MAX);
        return u32::from(G2W_SITE.load(core::sync::atomic::Ordering::Relaxed) == id);
    }
    if let Some(rest) = word.strip_prefix("g2wtag-ge-") {
        let id: u32 = rest.parse().unwrap_or(u32::MAX);
        let seen = G2W_SITE.load(core::sync::atomic::Ordering::Relaxed);
        return u32::from(seen != 0 && seen >= id);
    }
    if word == "g2wtag-file" {
        return G2W_SITE.load(core::sync::atomic::Ordering::Relaxed) / 1000;
    }
    // Route every `dwc3-*` word onto the proven CCS carrier.
    // See usb/README.md §3.9.
    if word.starts_with("dwc3-") {
        trace_dwc3_boundary();
        if word.starts_with("dwc3-debug-") || word.starts_with("dwc3-free-") {
            for _ in 0..6 {
                trace_dwc3_debug_sample();
                readout_keepalive_delay_ms(50);
            }
        }
        return trace::utmi_readout_code(word);
    }
    // "WENT" - the lookup body was entered. `usb2_live_word` runs TWICE per pass for a
    // `usb2-live-ccs-<word>` selector: once at :2275 via `utmi_readout_code`, once at :9441.
    // "WBFR" being present only proves the FIRST one finished. This marker separates entry
    // from the arm dispatch. See usb/README.md §3.17.
    trace_marker(TRACE_PROBE_WATCHDOG, 0x5745_4E54); // "WENT"
    unsafe {
        let snpsid = cached_snpsid();
        match word {
            "domain" => {
                let gdsc = core::ptr::read_volatile(
                    super::super::platform::bramble::usb_resources().gdsc as *const u32,
                );
                let clocks =
                    super::super::platform::bramble::usb_clock::read_usb_clock_register_state();
                u32::from(known_dwc_core_ip(snpsid))
                    | (u32::from(gdsc & (1 << 31) != 0) << 1)
                    | (u32::from(
                        clocks.controller_branches[0] & 1 != 0
                            && clocks.controller_branches[0] & (1 << 31) == 0,
                    ) << 2)
                    | (u32::from(
                        clocks.controller_branches[3] & 1 != 0
                            && clocks.controller_branches[3] & (1 << 31) == 0,
                    ) << 3)
            }
            "blockers" => {
                u32::from(known_dwc_core_ip(snpsid))
                    | (u32::from(read(DCTL) & DCTL_CSFTRST != 0) << 1)
                    | (u32::from(read(GCTL) & GCTL_CORESOFTRESET != 0) << 2)
                    | (u32::from(read(DEVTEN) != 0) << 3)
            }
            "runstop" => {
                // Is DCTL.RUN_STOP actually asserted? The handoff's own
                // "RUN/STOP readback timed out" path has never been able to
                // publish this.
                u32::from(read(DCTL) & DCTL_RUN_STOP != 0)
            }
            "conspd" => {
                // What speed does the *controller* believe the link is at?
                // DSTS.CONNECTSPD is bits 2:0 (0=HS, 1=FS, 2=LS, 3=SS).
                read(DSTS) & 0x7
            }
            "evtcount" => {
                // Has anything landed in the EP0 event ring? Saturate at 3 so
                // the word stays readable on a 4-bit channel.
                (read(GEVNTCOUNT0) & GEVNTCOUNT_MASK).min(3)
            }
            // Shift the field down: the CCS channel carries only 4 bits, and a raw
            // mask leaves PRTCAPDIR (bits 13:12) at 0x2000 for DEVICE, which
            // truncates to zero pulses and reads exactly like "no capability
            // set at all". Measured wrong once; keep the shift.
            "prtcap" => (read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12,
            // Who wrote GCTL last? `mmio::write()` is `#[track_caller]`, so this
            // names the source line of the most recent GCTL write without that
            // site knowing anything about it. Three nibbles: `gctlw` = line
            // bits 3:0, `gctlw2` = line bits 7:4, `gctlw3` = the PRTCAPDIR field
            // that was written (0=OTG, 1=HOST, 2=DEVICE) plus bit3 = "source
            // line exceeded 8 bits". 0xf in any of the first two means the
            // packed value has not been stored (no GCTL write yet).
            "gctlw" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { v & 0xf }
            }
            "gctlw2" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { (v >> 4) & 0xf }
            }
            "gctlw3" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                // NOTE: the store packs the field into bits 17:16, so the reader
                // must mask 0x3. A wider mask pulls in bit 18, which is bit 2 of
                // the line number - the two widths must match.
                if v == u32::MAX { 0xf } else { (v >> 16) & 0x3 }
            }
            // Full 8-bit low byte of the last GCTL writer's source line, in ONE
            // run. Deliberately a single word: splitting it across runs cannot
            // work, because each run is a separate boot with its own "last
            // writer" (see the history-probe note). `0xff` = no GCTL write yet.
            "gctlline" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xff } else { v & 0xff }
            }
            // Same value, shifted down by 8, for lines beyond 255.
            "gctlline2" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xff } else { (v >> 8) & 0xff }
            }
            // Four-bit halves of the writer line. The byte-wide `gctlline` emits
            // up to eight pulses, and each pulse is a host re-attach - which
            // breaks the "exactly one baseline attach" validity rule that all
            // the other selectors rely on (three consecutive 8-bit runs were
            // rejected on that rule). A nibble is at most four pulses, so the
            // existing rule applies unchanged. The two halves come from
            // different boots, so this is only sound if the write sequence is
            // deterministic - take each half twice and cross-check.
            // Sentinel: 0xf means "no GCTL write yet" in both halves.
            "gctllinelo" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { v & 0xf }
            }
            "gctllinehi" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                if v == u32::MAX { 0xf } else { (v >> 4) & 0xf }
            }
            // Same-boot PRTCAPDIR transition test. One pulse per predicate, so
            // the pulse count is the count of true statements:
            //   bit0: the early sample (right after the DEVICE write) was DEVICE
            //   bit1: the current field value is DEVICE  (i.e. it did NOT revert)
            //   bit2: the two differ            (the hardware changed it on its own)
            //   bit3: the early sample was never taken
            "prtcapx" => {
                let early = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                let now = (read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12;
                let early_dev = early == 2;
                let now_dev = now == 2;
                let differs = early != u32::MAX && early != now;
                u32::from(early_dev)
                    | (u32::from(now_dev) << 1)
                    | (u32::from(differs) << 2)
                    | (u32::from(early == u32::MAX) << 3)
            }
            // Single-predicate variants of `prtcapx`, one bit each, so a run
            // answers exactly one yes/no question (the house rule). `prtcapx`
            // packed four predicates and returned two true, which left three
            // possible combinations - a word that cannot be interpreted is not
            // a measurement.
            //   prtcapx0: the DEVICE write was reached and captured DEVICE
            //   prtcapx1: the field is DEVICE *now*
            //   prtcapx2: early and now differ (the hardware changed it)
            //   prtcapx3: the early sample was never taken
            // Phase-timing ladder: "the readout happens this many ms after the
            // DEVICE-mode write". One predicate per run, per the house rule -
            // `dtim16` = "delta < 16 ms", `dtim64` = "< 64 ms", `dtim256` =
            // "< 256 ms". A true reading is one pulse. Together they bracket how
            // long the handoff spends before the controller is running, inside
            // the host's ~588 ms descriptor-read window.
            // The other leg: how long after the *pull-up* (i.e. after the handset
            // presented itself, which is when the host starts its enumeration
            // attempt) does the DEVICE-mode write happen? One predicate per run:
            // `dtimr64` = "pull-up -> DEVICE write < 64 ms", `dtimr256` = "< 256 ms",
            // `dtimr1000` = "< 1000 ms".
            // Sanity predicate for the leg above. If the pull-up tick is *later*
            // than the DEVICE write tick - i.e. the latch fired on a pull-up that
            // happens after configuration - then `wrapping_sub` produces a huge
            // u32 and every `dtimr*` bound reads FALSE for the wrong reason.
            // "pullup before write" must be TRUE for any dtimr* reading to mean
            // anything.
            "dtimr_ok" => {
                let start = control::PULLUP_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(start != u32::MAX && early != u32::MAX && start <= early)
            }
            // Same guard for the write->readout leg.
            "dtim_ok" => {
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let freq = crate::timer::frequency();
                if early == u32::MAX || freq == 0 {
                    0
                } else {
                    let now = (crate::timer::counter() / (freq / 1000) as u64) as u32;
                    u32::from(early <= now)
                }
            }
            // The leg the earlier attempt got wrong: handoff entry -> DEVICE write.
            // Guarded by `dtimh_ok` (entry <= write) so an underflow cannot be
            // mistaken for a long interval again.
            "dtimh64" | "dtimh256" | "dtimh1000" => {
                let start = HANDOFF_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let limit = match word {
                    "dtimh64" => 64u32,
                    "dtimh256" => 256,
                    _ => 1000,
                };
                if start == u32::MAX || early == u32::MAX || start > early {
                    0
                } else {
                    u32::from(early.wrapping_sub(start) < limit)
                }
            }
            // Sign guard for the leg above: entry latch exists, write latch exists,
            // and entry <= write. Must be TRUE for any dtimh* reading to mean anything.
            // The real blocker candidate: `try_arm_setup` early-returns forever
            // unless `ENDPOINTS_READY` is set, and the ledger (written when it had
            // far fewer assignment sites) says it is only set in the Connect Done
            // branch - which never runs with GEVNTCOUNT0 == 0. There are now many
            // assignment sites including several in the deferred readout block, so
            // *measure* whether any of them ran rather than trusting the note.
            // One predicate per run, counted as bits:
            //   bit0 = ENDPOINTS_READY          bit1 = EP0_SETUP_ARMED
            //   bit2 = EP0_STATE == Setup       bit3 = DALEPENA bit0
            "endpoints" => {
                u32::from(ENDPOINTS_READY)
                    | (u32::from(EP0_SETUP_ARMED) << 1)
                    | (u32::from(EP0_STATE == Ep0State::Setup) << 2)
                    | (u32::from(read(DALEPENA) & 1 != 0) << 3)
            }
            // Branch-free probes: report what the selector string actually IS at
            // the moment the readout runs. `word == "armd"` did not fire even
            // though the code sits inside the right block and other words resolve
            // fine, so read the string itself instead of reasoning about a branch.
            //   word_len  = byte length of the suffix (saturating at 15)
            //   word_b0   = first byte & 0xf
            //   word_b1   = second byte & 0xf
            // For "armd": len 4, b0 = 'a' (0x61) & 0xf = 1, b1 = 'r' (0x72) & 0xf = 2.
            // NOTE: these take the suffix as an argument, so they must be evaluated
            // in the same scope `word` is bound in - hence they are plain matches
            // here rather than a helper that re-derives the selector.
            "word_len" => (word.len().min(15)) as u32,
            "word_b0" => u32::from(word.as_bytes().first().copied().unwrap_or(0) & 0xf),
            "word_b1" => u32::from(word.as_bytes().get(1).copied().unwrap_or(0) & 0xf),
            "armd" => DEFERRED_ARM_RESULT.load(core::sync::atomic::Ordering::Relaxed) & 1,
            "armd_res" => {
                (DEFERRED_ARM_RESULT.load(core::sync::atomic::Ordering::Relaxed) >> 1) & 1
            }
            // Crate-independent probe progress, read from the retained trace.
            "probe_reach" => u32::from(prev_boot_probe_reach_code() >= 6),
            "probe_stage" => prev_boot_probe_reach_code(),
            "probe_s1" => u32::from(prev_boot_probe_reach_code() >= 2),
            "probe_s2" => u32::from(prev_boot_probe_reach_code() >= 3),
            "probe_s3" => u32::from(prev_boot_probe_reach_code() >= 4),
            "probe_s4" => u32::from(prev_boot_probe_reach_code() >= 5),
            "hop_any" => u32::from(prev_boot_probe_reach_code() >= 100),
            "hop_ge1" => u32::from(prev_boot_probe_reach_code() >= 101),
            "hop_ge2" => u32::from(prev_boot_probe_reach_code() >= 102),
            "hop_ge3" => u32::from(prev_boot_probe_reach_code() >= 103),
            "hop_ge4" => u32::from(prev_boot_probe_reach_code() >= 104),
            "hop_ge5" => u32::from(prev_boot_probe_reach_code() >= 105),
            "hop_ge6" => u32::from(prev_boot_probe_reach_code() >= 106),
            "hop_ge7" => u32::from(prev_boot_probe_reach_code() >= 107),
            "hop_ge8" => u32::from(prev_boot_probe_reach_code() >= 108),
            "hop_ge9" => u32::from(prev_boot_probe_reach_code() >= 109),
            "act_known" => u32::from(unsafe { LAST_CONTROL_ACTION } != 0),
            "act_datain" => u32::from(unsafe { LAST_CONTROL_ACTION } == 1),
            "act_statusin" => u32::from(unsafe { LAST_CONTROL_ACTION } == 2),
            "act_stall" => u32::from(unsafe { LAST_CONTROL_ACTION } == 7),
            "act_other" => {
                let a = unsafe { LAST_CONTROL_ACTION };
                u32::from(a >= 3 && a <= 6)
            }
            "ph_pending" => u32::from(unsafe { DATA_PHASE_PENDING_START }),
            "ph_len" => u32::from(unsafe { DATA_PHASE_PENDING_LEN } != 0),
            "ep0_data" => u32::from(EP0_STATE == Ep0State::Data),
            "ep0_status" => u32::from(EP0_STATE == Ep0State::Status),
            // "POL1" at the top of `poll()`. Distinguishes "the driver loop never ran"
            // from "it ran and still saw nothing" - crate-independent, same retained trace
            // as `probe_reach`. See usb/README.md §3.12.
            "poll_ran" => u32::from(prev_boot_poll_ran()),
            // 1 word, 1 predicate: "SIG" alone, no HOP fold. See usb/README.md §3.13.
            "sig_only" => u32::from(prev_boot_sig_only()),
            // Brackets the handoff call itself. See usb/README.md §3.14.
            "hd_entered" => u32::from(prev_boot_handoff_entered()),
            // Did a synchronous abort fire at all this boot? See usb/README.md §3.15.
            "exc_seen" => u32::from(prev_boot_sync_exception()),
            // UPSTREAM markers only. `selx` is kept: it is written at :9504, AFTER the whole
            // readout block, so a reader inside the block is upstream of it and the reading is
            // meaningful. "wbef"/"waft"/"wstall"/"went" were removed as tautologies.
            // See usb/README.md §3.17.
            "selx" => u32::from(prev_boot_marker_eq(0x5345_4C58)), // "SELX"
            "m15d" => u32::from(prev_boot_marker_eq(0x4D31_3544)), // "M15D", set at :7757+
            // CHAIN BISECT. These are written by the *condition expressions* of the POSTRUN
            // readout chain, which Rust evaluates in order, so each one marks a real position
            // in the chain. All four are upstream of the reader at :9489, which is why - unlike
            // every marker tried before - these are not tautologies. See usb/README.md §3.18.
            "md1" => u32::from(prev_boot_marker_eq(0x4D44_3141)), // branch at :8140
            "md2" => u32::from(prev_boot_marker_eq(0x4D44_3241)), // branch at :8611
            "md3" => u32::from(prev_boot_marker_eq(0x4D44_3341)), // branch at :9061
            "md4" => u32::from(prev_boot_marker_eq(0x4D44_3441)), // branch at :9431
            // Bisect the span between milestone 15 (:7772) and the pre-readout boundary. This was
            // the last un-instrumented region; the readout block itself was exonerated by running
            // with `--utmi-postrun-readout` omitted (host still times out at ~5.5 s). All four
            // writers are upstream of the reader at the readout, so these are real predicates.
            "ma1" => u32::from(prev_boot_marker_eq(0x4D41_3141)),
            "ma2" => u32::from(prev_boot_marker_eq(0x4D41_3241)),
            "ma3" => u32::from(prev_boot_marker_eq(0x4D41_3341)),
            "ma4" => u32::from(prev_boot_marker_eq(0x4D41_3441)),
            // PREVIOUS-BOOT far-side probes. Read from the retained slot, not the ring, so these
            // are the first words that can see past the readout block. See usb/README.md §3.20.
            "ph20" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 20),
            "ph21" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 21),
            "ph22" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 22),
            // Proves the slot survives a boot; written at the top of init_usb2_handoff().
            "ph99" => u32::from(unsafe { trace::SHARED_POST_HANDOFF } >= 99),
            "hd_returned" => u32::from(prev_boot_handoff_returned()),
            // PSTA ladder, one bit each, HOP-free. See usb/README.md §3.13.
            "psta1" => u32::from(prev_boot_psta_only(0x01)),
            "psta2" => u32::from(prev_boot_psta_only(0x02)),
            "psta12" => u32::from(prev_boot_psta_only(0x12)),
            "psta3" => u32::from(prev_boot_psta_only(0x03)),
            "psta4" => u32::from(prev_boot_psta_only(0x04)),
            "ep_ready" => u32::from(ENDPOINTS_READY),
            "ep_armed" => u32::from(EP0_SETUP_ARMED),
            "ep_state" => u32::from(EP0_STATE == Ep0State::Setup),
            "ep_dalep" => u32::from(read(DALEPENA) & 1 != 0),
            // Is the arm-blip A/B actually compiled into this image?
            //
            // The rundir reports `effective_build_child_environment:
            // FULLERENE_AARCH64_USB_ARM_BLIP=1`, yet `arm_blip_queue` produced
            // no host blip even with a 75 s hold (well past its 30 s fallback).
            // `arm_blip_queue` gates on `option_env!("FULLERENE_USB_ARM_BLIP")`,
            // a COMPILE-TIME constant, so the child environment reaching the
            // build script is not the same claim as that constant being
            // `Some("1")` in the compiled kernel. Publish the constant itself.
            "blipenv" => u32::from(option_env!("FULLERENE_USB_ARM_BLIP").is_some()),
            // Did a SETUP packet actually arrive? `EP0_SETUP_ARMED` is cleared
            // by the EP0 XferComplete handler when the armed transfer is
            // *consumed* (mod.rs:5468-5479), so its false value at readout is
            // the signature of a completed transfer, not a missing one. These
            // two words test the thing that actually matters: whether the host's
            // SETUP reached the device at all.
            "ep0seen" => u32::from(ep0_setup_packet_seen()),
            "setupdr" => u32::from(TRACE_HARVEST_SETUP > 0),
            // Which arm window is compiled in? `mod.rs:9177-9188` picks
            // 10_000 / 5_000 / 400 ms from cfg!()s, and the A/B that widened it
            // (`--usb2-long-setup-arm`) produced an identical host failure. That
            // only refutes the window if the window actually changed, so publish
            // the compiled-in selection itself rather than inferring it from
            // `effective_build_child_environment`, which records only the
            // harness process's env and never the `cargo_envs` this flag rides.
            "armwin_long" => u32::from(cfg!(
                fullerene_aarch64_usb_gadget_handoff_usb2_long_setup_arm
            )),
            // Which SUSPHY A/B is compiled in? There are TWO env names and two
            // cfgs (build.rs:1100-1105):
            //   ..._USB2_SUSPHY        -> cfg(...usb2_susphy)         (--usb2-susphy)
            //   ..._USB2_SOURCE_SUSPHY -> cfg(...usb2_source_susphy)  (--usb2-source-susphy)
            // SUSPHY is SET in exactly two places (mod.rs:7116 behind
            // usb2_source_susphy, mod.rs:10522 inside init_with_super_speed)
            // and CLEARED in four (5301, 7665 A/Bs; 9418 behind
            // not(handoff_probe); 13450 unreachable unless the link reaches U0).
            // Measured: `susphy` reads 0 even WITH --usb2-source-susphy, so
            // publish which cfg is actually compiled in rather than assuming.
            "cfgsus" => u32::from(cfg!(
                fullerene_aarch64_usb_gadget_handoff_usb2_source_susphy
            )),
            // The two halves of the SUSPHY contradiction, each one bit:
            //   atsusphy = line 7139 asserted GUSB2PHYCFG0.SUSPHY
            //   guardeng = send_ep_command_result actually cleared it for a command
            // ge-1 = TRUE says the milestone at 7359 was reached (after 7139) while
            // `susphy` reads 0; these two bits separate "the guard never engaged"
            // from "7139 never ran".
            // Value read back from GUSB2PHYCFG0 *immediately after* writing
            // SUSPHY - not at the post-run readout. If this is 0 while the
            // post-run `susphy` is also 0, the readback path is broken rather
            // than the bit being cleared later.
            "atsusphy" => u32::from(unsafe { SUSPHY_SET_IN_HANDOFF }),
            "rawbef" => u32::from((unsafe { SUSPHY_RAW_BEFORE } & GUSB2PHYCFG_SUSPHY) != 0),
            // ---- Who wrote GUSB2PHYCFG0 last? ----
            // `write()` is `#[track_caller]` and records the calling source line
            // plus the SUSPHY/ENBLSLPM bits of the written value for this
            // register, the same way GCTL_LAST_WRITER does for GCTL. One
            // instrumented function names every writer without touching any of
            // them, which is what the four refutations of the read-only
            // exclusion list were asking for.
            //
            //   g2wsusphy = the LAST write had SUSPHY set
            //   g2wenbl   = the LAST write had ENBLSLPM set
            //   g2wline_ge_<k> = that write's source line is >= k (bisect)
            //   g2wcount_ge_<k> = at least k writes to the register happened
            "g2wsusphy" => u32::from(
                unsafe {
                    mmio::GUSB2PHYCFG_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed)
                } >> 16
                    & 1
                    != 0,
            ),
            // Counted behaviour, independent of the (broken) line readback:
            //   g2wset  = at least one write set SUSPHY
            //   g2wclr  = at least one write cleared SUSPHY
            // If set and clr are both 1 and susphy is 0, a clear provably ran
            // after the last set.
            // ---- A vs B: was SUSPHY present in the last READ? ----
            //   g2r_susphy  = the last read(GUSB2PHYCFG0) returned SUSPHY set
            //   g2rknown    = a read was recorded at all (not u32::MAX)
            // With g2wsusphy = FALSE:
            //   g2r_susphy = TRUE  -> an active clear (A)
            //   g2r_susphy = FALSE -> a read-modify-write of a value that was
            //                        already missing the bit (B)
            // The read that actually PRECEDED the last write. This is the
            // predicate the A/B question needs; `g2r_susphy` alone is only the
            // latest read and may postdate the write.
            //   g2rw_susphy = TRUE  + g2wsusphy = FALSE -> an active clear (A)
            //   g2rw_susphy = FALSE + g2wsusphy = FALSE -> a stale read was
            //                                             written back (B)
            "g2rw_susphy" => u32::from(
                (unsafe {
                    mmio::GUSB2PHYCFG_READ_BEFORE_LAST_WRITE
                        .load(core::sync::atomic::Ordering::Relaxed)
                } & mmio::GUSB2PHYCFG_SUSPHY)
                    != 0,
            ),
            "g2rwknown" => u32::from(
                unsafe {
                    mmio::GUSB2PHYCFG_READ_BEFORE_LAST_WRITE
                        .load(core::sync::atomic::Ordering::Relaxed)
                } != u32::MAX,
            ),
            "g2r_susphy" => u32::from(
                (unsafe {
                    mmio::GUSB2PHYCFG_LAST_READ.load(core::sync::atomic::Ordering::Relaxed)
                } & mmio::GUSB2PHYCFG_SUSPHY)
                    != 0,
            ),
            "g2rknown" => u32::from(
                unsafe { mmio::GUSB2PHYCFG_LAST_READ.load(core::sync::atomic::Ordering::Relaxed) }
                    != u32::MAX,
            ),
            "g2rge10" => u32::from(
                unsafe { mmio::GUSB2PHYCFG_READ_COUNT.load(core::sync::atomic::Ordering::Relaxed) }
                    >= 10,
            ),
            "g2wset" => u32::from(
                unsafe { mmio::GUSB2PHYCFG_SET_COUNT.load(core::sync::atomic::Ordering::Relaxed) }
                    >= 1,
            ),
            "g2wclr" => u32::from(
                unsafe {
                    mmio::GUSB2PHYCFG_CLEAR_COUNT.load(core::sync::atomic::Ordering::Relaxed)
                } >= 1,
            ),
            // Saturation markers so "how many" is distinguishable from "at least
            // one" without a multi-bit readout.
            "g2wclr10" => u32::from(
                unsafe {
                    mmio::GUSB2PHYCFG_CLEAR_COUNT.load(core::sync::atomic::Ordering::Relaxed)
                } >= 10,
            ),
            "g2wclr100" => u32::from(
                unsafe {
                    mmio::GUSB2PHYCFG_CLEAR_COUNT.load(core::sync::atomic::Ordering::Relaxed)
                } >= 100,
            ),
            "g2wenbl" => u32::from(
                unsafe {
                    mmio::GUSB2PHYCFG_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed)
                } >> 17
                    & 1
                    != 0,
            ),
            "rawaft" => u32::from((unsafe { SUSPHY_RAW_AFTER } & GUSB2PHYCFG_SUSPHY) != 0),
            // Low nibble of the raw readback, so a non-zero raw value that
            // happens to lack the SUSPHY bit is still distinguishable from an
            // all-zero readback. Multi-bit: pulse count is the popcount, so read
            // it as "0 = nothing came back" only.
            "rawbeflo" => (unsafe { SUSPHY_RAW_BEFORE } & 0xf),
            "rawaftlo" => (unsafe { SUSPHY_RAW_AFTER } & 0xf),
            "guardeng" => u32::from(unsafe { CMD_GUARD_ENGAGED }),
            "cfgsus2" => u32::from(cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_susphy)),
            "armwin_ext" => u32::from(cfg!(
                fullerene_aarch64_usb_gadget_handoff_usb2_extended_setup_arm
            )),
            // Did `arm_blip_queue` ever reach the point after the gate?
            "blipq" => u32::from(ARM_BLIP_QUEUED),
            // Was a blip ever emitted?
            "blipdone" => u32::from(ARM_BLIP_DONE),
            "dtimh_ok" => {
                let start = HANDOFF_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(start != u32::MAX && early != u32::MAX && start <= early)
            }
            "dtimr64" | "dtimr256" | "dtimr1000" => {
                let start = control::PULLUP_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let limit = match word {
                    "dtimr64" => 64u32,
                    "dtimr256" => 256,
                    _ => 1000,
                };
                if start == u32::MAX || early == u32::MAX {
                    0
                } else {
                    u32::from(early.wrapping_sub(start) < limit)
                }
            }
            "dtim16" | "dtim64" | "dtim256" => {
                let early = config::EARLY_TICK_MS.load(core::sync::atomic::Ordering::Relaxed);
                let now = if crate::timer::frequency() == 0 {
                    u32::MAX
                } else {
                    (crate::timer::counter() / (crate::timer::frequency() / 1000) as u64) as u32
                };
                let limit = match word {
                    "dtim16" => 16u32,
                    "dtim64" => 64,
                    _ => 256,
                };
                if early == u32::MAX || now == u32::MAX {
                    0
                } else {
                    u32::from(now.wrapping_sub(early) < limit)
                }
            }
            "prtcapx0" => {
                let e = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(e == 2)
            }
            "prtcapx1" => u32::from(((read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12) == 2),
            "prtcapx2" => {
                let e = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                let n = (read(GCTL) & GCTL_PRTCAPDIR_MASK) >> 12;
                u32::from(e != u32::MAX && e != n)
            }
            "prtcapx3" => {
                let e = config::EARLY_PRTCAPDIR.load(core::sync::atomic::Ordering::Relaxed);
                u32::from(e == u32::MAX)
            }
            // Writer identity AND the write count in ONE four-bit word, so a
            // single valid run is self-contained.
            //
            // The nibble split above assumed the last GCTL writer's line is the
            // same on every boot, and it is not: `gctllinelo` read 0x1 while
            // `gctllinehi` read 0x0, and no write site has low byte 0x01. Both
            // runs were valid, so both readings are real - the line varies.
            // Combining halves from different boots is therefore unsound, and
            // this word exists to stop doing it.
            //
            // Layout: bits 1:0 = (line & 0xf), bits 3:2 = the writer count saturating
            // at 3. Read it as: how many GCTL writes happened, and what does the
            // most recent one's line look like, *in the same boot*.
            "gctlwho" => {
                let v = mmio::GCTL_LAST_WRITER.load(core::sync::atomic::Ordering::Relaxed);
                let n = mmio::GCTL_WRITE_COUNT.load(core::sync::atomic::Ordering::Relaxed);
                let lo = if v == u32::MAX { 0xf } else { v & 0xf };
                (lo & 0x3) | ((n.min(3) & 0x3) << 2)
            }
            // Saturating GCTL write counter, low nibble.
            // Byte-wide so a count above 15 is distinguishable, and so the same
            // readout path as `gctlline` is exercised (a control for whether the
            // byte-wide branch is reached at all).
            "gctlwc" => mmio::GCTL_WRITE_COUNT
                .load(core::sync::atomic::Ordering::Relaxed)
                .min(0xff),
            // Proof-of-execution for the DEVICE-mode write, published in two
            // words so 4 bits are enough: `devmode` = (landed, wrote) and
            // `devmode2` = the field value read back at the write site.
            // `devmode`: bit0 = the readback matched what was written,
            // bits 3:1 = the value written (2 = DEVICE).
            "devmode" => {
                let probe = DEVICE_MODE_WRITE_PROBE.load(core::sync::atomic::Ordering::Relaxed);
                if probe == u32::MAX {
                    0xf // never executed
                } else {
                    (probe & 1) | (((probe >> 8) & 0x7) << 1)
                }
            }
            // `devmode2`: bits 3:0 = the field value read back immediately.
            "devmode2" => {
                let probe = DEVICE_MODE_WRITE_PROBE.load(core::sync::atomic::Ordering::Relaxed);
                if probe == u32::MAX {
                    0xf
                } else {
                    (probe >> 16) & 0xf
                }
            }
            "dctlrs" => u32::from(read(DCTL) & DCTL_RUN_STOP != 0),
            "gctl_lo" => read(GCTL) & 0xf,
            "runstop_and_speed" => {
                // Pack both halves of the chirp question into one word:
                // bit0 = RUN_STOP asserted, bits 2:1 = CONNECTSPD (0=HS,1=FS,2=LS).
                u32::from(read(DCTL) & DCTL_RUN_STOP != 0) | ((read(DSTS) & 0x3) << 1)
            }
            "utmic0lo" => unsafe { phy::utmi_ctrl0() & 0xf },
            "utmic0hi" => unsafe { (phy::utmi_ctrl0() >> 4) & 0xf },
            "cfg0lo" => unsafe { phy::cfg0() & 0xf },
            "cfg0hi" => unsafe { (phy::cfg0() >> 4) & 0xf },
            "suspn" => u32::from(unsafe { phy::suspend_n_asserted() }),
            "common0hi" => unsafe { (read_volatile(hsphy_reg(HSPHY_COMMON0)) >> 4) & 0xf },
            "cfg0hi" => unsafe { (phy::cfg0() >> 4) & 0xf },
            "gphycfg_lo" => unsafe { read(GUSB2PHYCFG0) & 0xf },
            "gphycfg_hi" => unsafe { (read(GUSB2PHYCFG0) >> 4) & 0xf },
            "lnkst" => {
                // DSTS.USBLNKST - what link state does the controller think it
                // is in? The phy.rs comment predicts a bogus "On" is possible.
                (read(DSTS) >> 22) & 0xf
            }
            "susphy" => {
                // GUSB2PHYCFG.SUSPHY - is the PHY being told to suspend, which
                // would stop the parallel receive path while USB2 PHY clocks
                // keep running?
                u32::from(read(GUSB2PHYCFG0) & GUSB2PHYCFG_SUSPHY != 0)
            }
            "halted" => {
                // DSTS.DEVCTRLHLT - did the controller leave the halted state?
                u32::from(read(DSTS) & DSTS_DEVCTRLHLT != 0)
            }
            _ => 15,
        }
    }
}

pub fn utmi_readout_code(selector: &str) -> u32 {
    if selector == "protocol" {
        return protocol_readout_code();
    }
    if selector == "dwc3-hib" {
        // Read-only source discriminator for qpr1's
        // dwc3_gadget_run_stop(true) KEEP_CONNECT branch. The low-power
        // option is a hardware capability field, not a software state bit;
        // publish only the categorical supported/not-supported result.
        unsafe {
            return u32::from(
                read(GHWPARAMS1) & GHWPARAMS1_EN_PWROPT_MASK == GHWPARAMS1_EN_PWROPT_HIB,
            );
        }
    }
    if selector == "hsphy-table" {
        // The PHY tuning table source is decided at DTB-install time in
        // main.rs, before any DWC3/PHY access. Bit 0-1: 0 = compiled
        // fallback, 1 = two-entry DT override, 2 = three-entry DT override.
        // Bit 8: the QMP table also came from the DT. The direct USB2 path
        // never touches the QMP poll, so the low bits are the relevant
        // classification there.
        return hsphy_table_source();
    }
    if let Some(cell) = selector.strip_prefix("hsphy-prop-") {
        // Categorical DT-observation codes (see hsphy_prop_code): tiny
        // values that survive the .min(15) clamp of the attach-delay
        // ladder. Aspects: present (0/1), len (0=absent, 1/2/3 = 8/16/24
        // bytes, 4 = other), pair0/1/2 (0=absent/incomplete, 1 = exact
        // qpr1 base value, 2 = known alternate, 3 = other).
        return phy_tables::hsphy_prop_code(cell);
    }
    if let Some(aspect) = selector.strip_prefix("hsphy-node-") {
        // Identity codes are categorical; never send a raw 0x088e3000 value
        // through the four-bit attach-delay channel.
        return phy_tables::hsphy_node_code(aspect);
    }
    if let Some(rest) = selector.strip_prefix("usb2-live-") {
        // Live, read-only 4-bit controller words for the USB2-only handoff.
        // The `ss-domain-*` selectors replay the SuperSpeed snapshot, which a
        // direct USB2 run never captures; these words are sampled at the
        // readout site itself. An optional transport prefix selects the
        // post-Run/Stop variant: `park-` publishes through the probe's own
        // PSCI reset time, `blip-` through a fresh stop/run pair; the bare
        // form is the pre-connect (attach-delay) transport.
        let word = rest
            .strip_prefix("park-")
            .or_else(|| rest.strip_prefix("blip-"))
            .unwrap_or(rest);
        // The `ccs-` transport prefix is part of the selector, not the word name.
        // Without this strip, `usb2-live-ccs-armd` arrives as `ccs-armd`, matches
        // no arm in `usb2_live_word`'s match, and falls through to the default -
        // which is why every word added here silently published a constant while
        // the readout block at `mod.rs:9104` (which *does* strip "usb2-live-ccs-")
        // was never reached. Measured: `word_len` published 1, not 8.
        let word = word.strip_prefix("ccs-").unwrap_or(word);
        // A/B: arm EP0 synchronously here, in the scope that actually executes.
        //
        // An earlier attempt put this gate in the readout block's
        // `strip_prefix("usb2-live-ccs-")` scope, which is *dead* - `usb2-live-`
        // above matches first, so that branch (and its `usb2_live_word(word)`
        // call at ~9104) is never reached. Measured: the gate never fired and the
        // log marker stayed at zero. This is the live scope; the same `word` that
        // `usb2_live_word` receives is the one tested here.
        //
        // Rationale for the arm itself: on valid plain-profile runs
        // `ENDPOINTS_READY` is true and `EP0_STATE == Setup`, yet
        // `EP0_SETUP_ARMED` is false - every `try_arm_setup` guard is false, so the
        // function would proceed if called, and none of its sixteen call sites is
        // in the handoff path. Arming here removes the dependence on
        // poll()/timer-IRQ timing beating the host's first SETUP token.
        if word == "armd" {
            let armed = unsafe { try_arm_setup() };
            DEFERRED_ARM_RESULT.store(
                1 | (u32::from(armed) << 1),
                core::sync::atomic::Ordering::Relaxed,
            );
            log_hex("usb deferred: arm attempted=", u64::from(armed));
        }
        // Read the arm result at the one point this profile is known to reach.
        //
        // `arm_blip_queue` is called only from `main.rs:930/950` (the normal
        // handoff entry) and `usb_probe.rs:2826` (behind `gadget_ready`). The
        // `--direct-handoff` profile reaches neither, which is why `blipq`
        // published FALSE while `blipenv` published TRUE - the A/B is compiled
        // in but never queued. Call it here, gated on the selector so ordinary
        // readouts are untouched:
        //
        //   --utmi-postrun-readout usb2-live-ccs-blipq    -> queue reached?
        //   --utmi-postrun-readout usb2-live-ccs-blipdone -> blip emitted?
        //
        // `arm_blip_queue` ends in `runstop_blips`, which toggles Run/Stop, and
        // handoff-path Run/Stop is measured destructive to the attach. Read
        // `blipq`/`blipdone` together with the attach count, not the attach
        // count alone.
        if word == "blipq" || word == "blipdone" {
            unsafe { arm_blip_queue() };
        }
        return unsafe { usb2_live_word(word) };
    }
    if selector.starts_with("ss-") {
        // A failed handoff can enter the generic signal path and publish the
        // same USB2 marker without ever reaching stage 13/21. Reserve 15 for
        // that case so a missing SS snapshot cannot masquerade as a valid
        // zero-valued register field.
        unsafe {
            if !SS_STATE_SNAPSHOT_VALID {
                return if selector == "ss-domain" {
                    31
                } else if selector.starts_with("ss-domain-") {
                    2
                } else if selector.starts_with("ss-gctl-") {
                    2
                } else if selector.starts_with("ss-qmp-") {
                    2
                } else if selector.starts_with("ss-qscratch-") {
                    2
                } else if selector == "ss-ltssm" {
                    16
                } else if selector.starts_with("ss-ltssm-bit") {
                    2
                } else {
                    15
                };
            }
        }
    }
    if matches!(
        selector,
        "ss-speed"
            | "ss-link"
            | "ss-pipe"
            | "ss-gctl"
            | "ss-qmp"
            | "ss-vbus"
            | "ss-dctl"
            | "ss-domain"
            | "ss-domain-core"
            | "ss-domain-gdsc"
            | "ss-domain-core-branch"
            | "ss-domain-utmi-branch"
            | "ss-gctl-device"
            | "ss-qmp-phystatus"
            | "ss-qmp-start0"
            | "ss-qmp-start1"
            | "ss-qmp-typec0"
            | "ss-qmp-rxeq"
            | "ss-qmp-com-power"
            | "ss-qmp-pcs-power"
            | "ss-qmp-aux-branch"
            | "ss-qmp-pipe-branch"
            | "ss-qmp-com-aux-branch"
            | "ss-qscratch-utmi-sel"
            | "ss-qscratch-phystatus-sw"
            | "ss-qscratch-utmi-dis"
            | "ss-ltssm"
            | "ss-ltssm-bit0"
            | "ss-ltssm-bit1"
            | "ss-ltssm-bit1-wide"
            | "ss-ltssm-bit2"
            | "ss-ltssm-bit3"
    ) {
        unsafe {
            let dsts = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_DSTS
            } else {
                read(DSTS)
            };
            let dctl = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_DCTL
            } else {
                read(DCTL)
            };
            let pipe = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_PIPE
            } else {
                read(GUSB3PIPECTL0)
            };
            let gctl = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_GCTL
            } else {
                read(GCTL)
            };
            let qscratch = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_QSCRATCH
            } else {
                read_qscratch(QSCRATCH_SS_PHY_CTRL)
            };
            let qscratch_general = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_QSCRATCH_GENERAL
            } else {
                read_qscratch(QSCRATCH_GENERAL_CFG)
            };
            let qmp = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_QMP
            } else {
                phy::qmp_post_runstop_snapshot()
            };
            let ltssm = if SS_STATE_SNAPSHOT_VALID {
                SS_STATE_SNAPSHOT_LTSSM
            } else {
                gdb_ltssm_link_state()
            };
            return match selector {
                // DWC3 DSTS.CONNECTSPD: 4 = SuperSpeed, 5 = SuperSpeed+.
                "ss-speed" => dsts & DSTS_CONNECTSPD_MASK,
                // DWC3 DSTS.USBLNKST: U0=0, RX_DET=5, SS_INACT=6,
                // POLL=7, and the remaining states follow the DWC3 table.
                "ss-link" => (dsts >> 18) & 0xf,
                // Four independent bits: USB3 PIPE SUSPHY, PIPE soft reset,
                // DCTL Run/Stop, and DSTS halted.
                "ss-pipe" => {
                    (u32::from(pipe & GUSB3PIPECTL_SUSPHY != 0))
                        | (u32::from(pipe & GUSB3PIPECTL_PHYSOFTRST != 0) << 1)
                        | (u32::from(dctl & DCTL_RUN_STOP != 0) << 2)
                        | (u32::from(dsts & DSTS_DEVCTRLHLT != 0) << 3)
                }
                // GCTL.PRTCAPDIR plus the two-bit RAMCLKSEL value.
                "ss-gctl" => ((gctl & GCTL_PRTCAPDIR_MASK) >> 12) | (gctl_ramclksel(gctl) << 2),
                // QMP PHYSTATUS, PCS_START_CONTROL[1:0], and TYPEC_CTRL[0].
                "ss-qmp" => {
                    u32::from(qmp & QMP_PHYSTATUS != 0)
                        | (((qmp >> 16) & 0x3) << 1)
                        | (((qmp >> 24) & 0x1) << 3)
                }
                "ss-vbus" => (qscratch >> 24) & 0x1,
                // Run/Stop lifecycle: pre-write DCTL, immediate post-write
                // DCTL, later stage-21 DCTL, and later DSTS halted.
                "ss-dctl" => {
                    let pre = SS_RUNSTOP_PRE_DCTL;
                    let post = SS_RUNSTOP_POST_DCTL;
                    let post_dsts = SS_RUNSTOP_POST_DSTS;
                    u32::from(pre != 0xffff_ffff && pre & DCTL_RUN_STOP != 0)
                        | (u32::from(post != 0xffff_ffff && post & DCTL_RUN_STOP != 0) << 1)
                        | (u32::from(dctl & DCTL_RUN_STOP != 0) << 2)
                        | (u32::from(post_dsts != 0xffff_ffff && post_dsts & DSTS_DEVCTRLHLT != 0)
                            << 3)
                }
                // Encode the four domain bits as N+1 so 1..=16 are valid
                // snapshots and 31 remains the missing-snapshot sentinel.
                "ss-domain" => SS_STATE_SNAPSHOT_DOMAIN + 1,
                "ss-domain-core" => SS_STATE_SNAPSHOT_DOMAIN & 1,
                "ss-domain-gdsc" => (SS_STATE_SNAPSHOT_DOMAIN >> 1) & 1,
                "ss-domain-core-branch" => (SS_STATE_SNAPSHOT_DOMAIN >> 2) & 1,
                "ss-domain-utmi-branch" => (SS_STATE_SNAPSHOT_DOMAIN >> 3) & 1,
                // DWC3 GCTL.PRTCAPDIR: 2 = DEVICE. This is separated from
                // the clock/GDSC domain bits because a live MMIO readback
                // does not prove that the controller retained its protocol
                // capability through the no-core handoff.
                "ss-gctl-device" => u32::from(gctl & GCTL_PRTCAPDIR_MASK == GCTL_PRTCAP_DEVICE),
                "ss-qmp-phystatus" => u32::from(qmp & QMP_PHYSTATUS != 0),
                "ss-qmp-start0" => (qmp >> 16) & 1,
                "ss-qmp-start1" => (qmp >> 17) & 1,
                "ss-qmp-typec0" => (qmp >> 24) & 1,
                // QMP PCS_STATUS2[3] is the official Qualcomm
                // RX_EQUALIZATION_IN_PROGRESS indicator.
                "ss-qmp-rxeq" => u32::from(SS_STATE_SNAPSHOT_QMP_STATUS2 & (1 << 3) != 0),
                "ss-qmp-com-power" => u32::from(SS_STATE_SNAPSHOT_QMP_POWER & 1 != 0),
                "ss-qmp-pcs-power" => u32::from(SS_STATE_SNAPSHOT_QMP_POWER & 2 != 0),
                "ss-qmp-aux-branch" => u32::from(
                    SS_STATE_SNAPSHOT_QMP_BRANCHES[0] & 1 != 0
                        && SS_STATE_SNAPSHOT_QMP_BRANCHES[0] & (1 << 31) == 0,
                ),
                "ss-qmp-pipe-branch" => u32::from(
                    SS_STATE_SNAPSHOT_QMP_BRANCHES[1] & 1 != 0
                        && SS_STATE_SNAPSHOT_QMP_BRANCHES[1] & (1 << 31) == 0,
                ),
                "ss-qmp-com-aux-branch" => u32::from(
                    SS_STATE_SNAPSHOT_QMP_BRANCHES[3] & 1 != 0
                        && SS_STATE_SNAPSHOT_QMP_BRANCHES[3] & (1 << 31) == 0,
                ),
                // Qualcomm's SS path leaves the UTMI-as-PIPE mux sequence
                // alone; the glue only applies it for HS/full-speed. These
                // are read-only bits from the selected same-boot snapshot so
                // a stale Fastboot HS mux can be separated from QMP state.
                "ss-qscratch-utmi-sel" => u32::from(qscratch_general & (1 << 0) != 0),
                "ss-qscratch-phystatus-sw" => u32::from(qscratch_general & (1 << 3) != 0),
                "ss-qscratch-utmi-dis" => u32::from(qscratch_general & (1 << 8) != 0),
                // Qualcomm msm's DWC3 glue reads LINKSTATE from bits 25:22
                // of the USB31 link-debug register. This is a raw four-bit
                // snapshot from the selected boundary, separate from
                // DSTS.USBLNKST. Stage 20 is pre-Run/Stop and normally reads
                // SS_DIS; use stage 21 to classify the running link.
                "ss-ltssm" => ltssm,
                "ss-ltssm-bit0" => ltssm & 1,
                "ss-ltssm-bit1" => (ltssm >> 1) & 1,
                "ss-ltssm-bit1-wide" => (ltssm >> 1) & 1,
                "ss-ltssm-bit2" => (ltssm >> 2) & 1,
                "ss-ltssm-bit3" => (ltssm >> 3) & 1,
                _ => 0,
            };
        }
    }
    trace::utmi_readout_code(selector)
}

/// Capture the controller boundary immediately before a protocol readout.
/// This is read-only: it does not acknowledge the event ring, alter the
/// endpoint command registers, or touch the USB2 PHY. Three records preserve
/// enough raw state to distinguish an empty event FIFO, a received SETUP, an
/// unconsumed SETUP TRB, and a stuck EP0 command after the host reports -71.
pub fn trace_dwc3_boundary() {
    unsafe {
        let event_count = read(GEVNTCOUNT0);
        let dsts = read(DSTS);
        let dctl = read(DCTL);
        let devten = read(DEVTEN);
        let dalepena = read(DALEPENA);
        let depcmd0 = read(dep_reg(0, 0x0c));
        let depcmd1 = read(dep_reg(1, 0x0c));
        let trb0 = ep0_trb_ptr(0);
        let trb1 = ep0_trb_ptr(ep0_trb_index(1));
        cache_invalidate(trb0 as usize, core::mem::size_of::<Trb>());
        cache_invalidate(trb1 as usize, core::mem::size_of::<Trb>());
        let trb0_ctrl = read_volatile(addr_of!((*trb0).ctrl));
        let trb1_ctrl = read_volatile(addr_of!((*trb1).ctrl));
        let setup = ep0_setup_data_ptr() as *const u8;
        cache_invalidate(setup as usize, 8);
        let setup0 = u32::from_le_bytes([
            read_volatile(setup),
            read_volatile(setup.add(1)),
            read_volatile(setup.add(2)),
            read_volatile(setup.add(3)),
        ]);
        let setup1 = u32::from_le_bytes([
            read_volatile(setup.add(4)),
            read_volatile(setup.add(5)),
            read_volatile(setup.add(6)),
            read_volatile(setup.add(7)),
        ]);
        let setup_nonzero = (setup0 | setup1) != 0;
        let compact = u32::from((event_count & GEVNTCOUNT_MASK) != 0)
            | (u32::from(setup_nonzero) << 1)
            | (u32::from((trb0_ctrl & TRB_HWO) != 0) << 2)
            | (u32::from(SIGNAL_DWC3_DEVICE_ERROR) << 3);
        trace_event(
            TRACE_DWC3_BOUNDARY,
            0x4457_4300,
            event_count,
            dsts,
            dctl,
            devten,
        );
        trace_event(
            TRACE_DWC3_BOUNDARY,
            0x4457_4301,
            dalepena,
            depcmd0,
            depcmd1,
            compact,
        );
        trace_event(
            TRACE_DWC3_BOUNDARY,
            0x4457_4302,
            trb0_ctrl,
            trb1_ctrl,
            setup0,
            setup1,
        );
    }
}

/// Sample Synopsys internal debug queues and device-mode LSP values without
/// consuming events or changing endpoint state. This mirrors Linux's
/// `dwc3_core_fifo_space()` and gadget `lsp_dump` read paths.
pub fn trace_dwc3_debug_sample() {
    unsafe {
        let mut values = [0u32; 8];
        for queue_type in 0..8u32 {
            write(
                GDBGFIFOSPACE,
                (queue_type << GDBGFIFOSPACE_TYPE_SHIFT) & GDBGFIFOSPACE_TYPE_MASK,
            );
            values[queue_type as usize] = read(GDBGFIFOSPACE) >> GDBGFIFOSPACE_SPACE_SHIFT;
        }
        trace::live_dwc3_debug_sample(values, [0; 16], read(GDBGEPINFO0), read(GDBGEPINFO1));
        trace_event(
            TRACE_DWC3_DEBUG,
            0,
            values[0],
            values[1],
            values[2],
            values[3],
        );
        trace_event(
            TRACE_DWC3_DEBUG,
            1,
            values[4],
            values[5],
            values[6],
            values[7],
        );
    }
}

pub fn trace_dwc3_debug_window_sample(queue: usize) {
    unsafe {
        if queue >= DWC3_DEBUG_QUEUE_COUNT {
            return;
        }
        write(
            GDBGFIFOSPACE,
            ((queue as u32) << GDBGFIFOSPACE_TYPE_SHIFT) & GDBGFIFOSPACE_TYPE_MASK,
        );
        let value = read(GDBGFIFOSPACE) >> GDBGFIFOSPACE_SPACE_SHIFT;
        trace::live_dwc3_debug_window_sample(queue, value);
    }
}

/// Read the DWC3 gadget debug LSP vector without touching the transfer path.
/// Linux's DWC3 debugfs gadget-LSP reader selects device endpoints 0..15
/// through GDBGLSPMUX and then reads GDBGLSP. Keep this operation at bounded
/// handoff stages; the observation loop must not continuously rewrite the
/// debug mux while a host transaction is in flight.
unsafe fn read_dwc3_gadget_lsp() -> [u32; 16] {
    let mut values = [0u32; 16];
    for endpoint in 0..16u32 {
        write(
            GDBGLSPMUX,
            (endpoint << GDBGLSPMUX_DEVSELECT_SHIFT) & 0x0000_00f0,
        );
        values[endpoint as usize] = read(GDBGLSP);
    }
    values
}

/// Return the one queue selected by a descriptor-window readout. Entry/stage
/// selectors deliberately return None so the observation loop performs no
/// continuous GDBGFIFOSPACE or GDBGLSPMUX writes after the stage latch.
pub fn dwc3_debug_window_queue() -> Option<usize> {
    match option_env!("FULLERENE_USB_SIGNAL_CMD_GATE") {
        Some("dwc3-free-descriptor-window-txfifo") => Some(DWC3_DEBUG_QUEUE_TXFIFO),
        Some("dwc3-free-descriptor-window-rxfifo") => Some(DWC3_DEBUG_QUEUE_RXFIFO),
        Some("dwc3-free-descriptor-window-txreq") => Some(DWC3_DEBUG_QUEUE_TXREQQ),
        Some("dwc3-free-descriptor-window-rxreq") => Some(DWC3_DEBUG_QUEUE_RXREQQ),
        Some("dwc3-free-descriptor-window-rxinfo") => Some(DWC3_DEBUG_QUEUE_RXINFOQ),
        Some("dwc3-free-descriptor-window-pstat") => Some(DWC3_DEBUG_QUEUE_PSTATQ),
        Some("dwc3-free-descriptor-window-descfetch") => Some(DWC3_DEBUG_QUEUE_DESCFETCHQ),
        Some("dwc3-free-descriptor-window-eventq") => Some(DWC3_DEBUG_QUEUE_EVENTQ),
        _ => None,
    }
}

/// Capture only the DWC3 queue SPACE_AVAILABLE vector at a named handoff stage.
/// The value is free space, not occupancy.
pub fn trace_dwc3_debug_stage(stage: u32) {
    unsafe {
        let mut values = [0u32; DWC3_DEBUG_QUEUE_COUNT];
        for queue_type in 0..DWC3_DEBUG_QUEUE_COUNT as u32 {
            write(
                GDBGFIFOSPACE,
                (queue_type << GDBGFIFOSPACE_TYPE_SHIFT) & GDBGFIFOSPACE_TYPE_MASK,
            );
            values[queue_type as usize] = read(GDBGFIFOSPACE) >> GDBGFIFOSPACE_SPACE_SHIFT;
        }
        let lsp = read_dwc3_gadget_lsp();
        trace::live_dwc3_debug_sample(values, lsp, read(GDBGEPINFO0), read(GDBGEPINFO1));
        trace::live_dwc3_debug_stage(stage, values);
        trace_event(
            TRACE_DWC3_DEBUG,
            0x100 | (stage & 0xff),
            values[DWC3_DEBUG_QUEUE_TXFIFO],
            values[DWC3_DEBUG_QUEUE_RXFIFO],
            values[DWC3_DEBUG_QUEUE_RXREQQ],
            values[DWC3_DEBUG_QUEUE_EVENTQ],
        );
    }
}

/// Classify the first retained EP0 STARTTRANSFER boundary for a host-visible
/// protocol-error readout. This is deliberately a command/ownership result,
/// not a claim about CRC or bit stuffing on the USB wires:
///   0 = no EP0 STARTTRANSFER record
///   1 = first EP0 STARTTRANSFER timed out with CMDACT still set
///   2 = first command completed with DWC3 status 0x1 (No Resource)
///   3 = first command completed with another non-zero DWC3 status
///   4 = first command completed with status 0
///   5 = software later received at least one SETUP packet
/// The signal probe maps this code to code+1 same-boot DCTL stop/run attach
/// cycles, so the result remains observable even when the current image never
/// reaches an enumerated trace transport.
pub fn protocol_readout_code() -> u32 {
    unsafe {
        harvest_trace_outcome();
        if TRACE_HARVEST_SETUP > 0 {
            return 5;
        }
        if TRACE_HARVEST == 0xFFFF_FFFF {
            return 0;
        }
        if TRACE_HARVEST & 0x8000_0000 != 0 {
            return 1;
        }
        match TRACE_HARVEST & 0xf000 {
            0 => 4,
            0x1000 => 2,
            _ => 3,
        }
    }
}

/// True when the newest EP1 transfer-complete event reports success (status
/// 0): the read/64 data phase left the core and the host should hold the
/// descriptor bytes. The mrad tail stable-parks on this instead of resetting,
/// because a live data phase is worth keeping the session up for.
pub fn ep1_data_phase_complete() -> bool {
    unsafe {
        harvest_trace_outcome();
        TRACE_HARVEST_EP1_XFER == 0
    }
}

/// Eval-time rescue for the read/64 -110. No blip readout has ever been
/// host-visible on this board (zero SDIS pairs across every run), so the
/// diag gate ACTS instead of reporting: it re-drives whichever stage of the
/// host's pending 64-byte GET_DESCRIPTOR is stuck, using the live trace
/// plus the live core state. The host's read/64 URB stays pending until its
/// 5 s timeout and keeps polling IN tokens, so a successful re-arm lands
/// the data and the host journal's enumeration progress is the readout
/// (1234:0001 = success, -110 again = this stage's rescue did not land).
/// Returns the branch taken (0 = no rescue):
///   0 = last SETUP was not a 64-byte GET_DESCRIPTOR, or the core is not
///       running with the link U0 (nothing pending to rescue)
///   1 = latched SETUP undelivered (the setup buffer still holds the
///       packet): re-dispatched through handle_setup
///   2 = nothing latched, state Setup: the SETUP TRB was never (re)armed,
///       so the host's SETUP is latched in the core - rearm it
///   3 = state Data: the 64-byte DataIn was dispatched but the data phase
///       never completed - ENDTRANSFER + resource re-issue + re-arm of the
///       same data TRB (the response buffer still holds the data)
///   4 = state Status: the status ZLP was lost - re-arm the status
pub fn rescue_read64() -> u32 {
    unsafe {
        harvest_trace_outcome();
        let last = TRACE_HARVEST_LAST_SETUP;
        if last == 0xFFFF_FFFF || (last >> 16) != 6 || (last & 0xffff) != 64 {
            return 0;
        }
        let dsts = read(DSTS);
        if dsts & DSTS_DEVCTRLHLT != 0 || (dsts >> 18) & 0xf != 0 {
            return 0;
        }
        let setup = ep0_setup_data_ptr();
        cache_invalidate(setup as usize, 8);
        let mut latched = false;
        for offset in 0..8 {
            if read_volatile(setup.add(offset)) != 0 {
                latched = true;
                break;
            }
        }
        if latched {
            // Mirror the fresh_setup path: the latched SETUP overrides any
            // stale phase.
            EP0_STATE = Ep0State::Setup;
            handle_setup();
            return 1;
        }
        match EP0_STATE {
            Ep0State::Setup => {
                let _ = rearm_setup();
                2
            }
            Ep0State::Data => {
                let _ = end_transfer(1);
                let _ = set_transfer_resource(1);
                let trb_index = ep0_trb_index(1);
                let mut queued = start_transfer(1, ep0_trb_ptr(trb_index));
                if !queued {
                    for _ in 0..50 {
                        super::super::timer::delay_us(200);
                        if start_transfer(1, ep0_trb_ptr(trb_index)) {
                            queued = true;
                            break;
                        }
                    }
                }
                let _ = queued;
                3
            }
            Ep0State::Status => {
                let endpoint = if CONTROL_HAS_DATA && CONTROL_IN { 0 } else { 1 };
                let _ = start_status(endpoint);
                4
            }
        }
    }
}
