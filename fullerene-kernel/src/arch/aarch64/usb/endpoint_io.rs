//! DWC3 endpoint commands, DMA cache maintenance, and GSI transfers.

use super::*;

// qpr1's dwc3_send_gadget_ep_cmd() uses a 3000-read completion budget for
// every endpoint command, including STARTTRANSFER. Keep that source contract
// here; the former 50,000-read extension was an unverified probe workaround
// and can hide a command-engine stall during the handoff.
const DWC3_EP_COMMAND_TIMEOUT: u32 = 3_000;

#[inline]
fn gsi_transfer_params(event_buffer: u32, trb: usize) -> Option<(u32, u32)> {
    let count = super::super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if event_buffer == 0 || event_buffer > count || trb & 0x3f != 0 {
        return None;
    }
    Some((
        GSI_TRB_ADDR_BIT_53 | GSI_TRB_ADDR_BIT_55 | (event_buffer << GSI_EVENT_ADDR_INDEX_SHIFT),
        trb as u32,
    ))
}

/// Set up the Qualcomm GSI event-buffer ABI before any GSI endpoint can be
/// started. Android allocates three additional event buffers and marks them
/// with both the GSI enable/index bits in GEVNTADRHI and the interrupt-mask
/// bit in GEVNTCOUNT. EP0 continues to use event buffer zero.
pub(super) unsafe fn configure_gsi_event_buffers() -> bool {
    let resources = super::super::platform::bramble::usb_resources();
    let gsi = resources.gsi;
    unsafe {
        let mut general = read_qscratch(gsi.general_cfg_offset);
        general |= GSI_CLK_EN;
        write_qscratch(gsi.general_cfg_offset, general);
        general |= GSI_RESTART_DBL_PNTR;
        write_qscratch(gsi.general_cfg_offset, general);
        general &= !GSI_RESTART_DBL_PNTR;
        write_qscratch(gsi.general_cfg_offset, general);
        if read_qscratch(gsi.general_cfg_offset) & GSI_CLK_EN == 0 {
            return false;
        }

        for index in 0..gsi.event_buffer_count as usize {
            let event = addr_of_mut!(GSI_EVENTS).cast::<EventBuffer>().add(index);
            let event_address = event as usize as u64;
            cache_clean(event as usize, EVENT_BUFFER_SIZE);
            let register = GEVNTADRLO0 + (index + 1) * GEVNT_BUFFER_STRIDE;
            write(register, event_address as u32);
            write(
                register + 4,
                (event_address >> 32) as u32
                    | (((index + 1) as u32) << GSI_EVENT_ADDR_EN_SHIFT)
                    | (((index + 1) as u32) << GSI_EVENT_ADDR_INDEX_SHIFT),
            );
            write(register + 8, EVENT_BUFFER_SIZE as u32);
            write(register + 12, GSI_EVENT_INTR_MASK);
        }
    }
    true
}

/// Enable the GSI wrapper at the point Android starts a GSI endpoint. Keeping
/// this separate from event-buffer allocation avoids asserting GSI_EN for a
/// normal gadget that has no IPA/GSI channel.
unsafe fn enable_gsi_wrapper() -> bool {
    let offset = super::super::platform::bramble::usb_resources()
        .gsi
        .general_cfg_offset;
    unsafe {
        let mut value = read_qscratch(offset);
        value |= GSI_CLK_EN;
        write_qscratch(offset, value);
        value |= GSI_EN;
        write_qscratch(offset, value);
        read_qscratch(offset) & GSI_EN != 0
    }
}

const GSI_MAX_RING_TRBS: usize = 10;

/// Build the circular DWC3 TRB ring consumed by Qualcomm's GSI wrapper. The
/// ring is caller-owned DMA memory, while buffer addresses are the contiguous
/// request pool supplied by the IPA/GSI client. This mirrors Android's
/// `gsi_prepare_trbs()` split between ring allocation and buffer storage.
unsafe fn prepare_gsi_ring(
    event_index: usize,
    endpoint: usize,
    ring_base: u64,
    buffer_base: usize,
    buffer_length: usize,
) -> bool {
    let in_direction = endpoint & 1 != 0;
    let Some(shape) = gsi_ring_shape(in_direction, GSI_DEFAULT_NUM_BUFFERS) else {
        return false;
    };
    let pool = super::super::platform::bramble::usb_resources().dma_pool;
    let ring_bytes = shape.num_trbs.saturating_mul(core::mem::size_of::<Trb>());
    let buffer_bytes = (shape.data_trbs as u64).saturating_mul(buffer_length as u64);
    if shape.num_trbs > GSI_MAX_RING_TRBS
        || !super::super::platform::bramble::dma_region_valid(
            pool,
            ring_base,
            ring_bytes as u64,
            0x400,
        )
        || !super::super::platform::bramble::dma_region_valid(
            pool,
            buffer_base as u64,
            buffer_bytes,
            64,
        )
        || buffer_length == 0
    {
        return false;
    }

    unsafe {
        let ring = ring_base as usize as *mut Trb;
        for index in 0..shape.num_trbs {
            let mut trb = Trb::default();
            if index == shape.num_trbs - 1 {
                // The GSI wrapper uses the same address[55:53] and
                // interrupter-index encoding as STARTTRANSFER.
                trb.bpl = ring_base as u32;
                trb.bph = (ring_base >> 32) as u32
                    | GSI_TRB_ADDR_BIT_53
                    | GSI_TRB_ADDR_BIT_55
                    | ((event_index as u32 + 1) << GSI_EVENT_ADDR_INDEX_SHIFT);
                trb.ctrl = TRB_LINK | TRB_HWO;
            } else if in_direction {
                // The first n+1 entries are deliberate zero-length normal
                // TRBs (ZLPs); the following n entries point at the
                // contiguous buffer pool. Android leaves HWO clear here and
                // lets the GSI path own the buffer progression.
                if index >= shape.first_buffer_trb {
                    let buffer_index = index - shape.first_buffer_trb;
                    let address = buffer_base
                        .saturating_add(buffer_index.saturating_mul(buffer_length))
                        as u64;
                    trb.bpl = address as u32;
                    trb.bph = (address >> 32) as u32;
                }
                trb.ctrl = TRB_NORMAL | TRB_IOC;
            } else if index == 0 {
                // The Bramble Android OUT ring starts with a link to the
                // second TRB, then closes with another link TRB.
                let next = ring_base + core::mem::size_of::<Trb>() as u64;
                trb.bpl = next as u32;
                trb.bph = (next >> 32) as u32;
                trb.ctrl = TRB_LINK;
            } else {
                let buffer_index = index - 1;
                let address =
                    buffer_base.saturating_add(buffer_index.saturating_mul(buffer_length)) as u64;
                trb.bpl = address as u32;
                trb.bph = (address >> 32) as u32;
                trb.size = buffer_length as u32;
                // OUT HWO is set by UPDATETRANSFER, matching Android's
                // lifecycle. Preparing a ring must not make it live early.
                trb.ctrl = TRB_NORMAL | TRB_IOC | TRB_CSP | TRB_ISP_IMI;
            }
            write_volatile(ring.add(index), trb);
        }
        cache_clean(
            ring_base as usize,
            shape.num_trbs * core::mem::size_of::<Trb>(),
        );
    }
    true
}

/// Publish the ring and doorbell addresses consumed by the IPA/GSI channel
/// setup, and prepare the complete circular TRB layout. Android does this
/// after endpoint configuration and before starting the channel; a normal
/// UDC endpoint therefore never writes to an unowned doorbell by accident.
pub unsafe fn configure_gsi_channel(
    endpoint: usize,
    event_buffer: u32,
    ring_base: u64,
    doorbell: u64,
) -> bool {
    // Do not retain the old incomplete ABI as a fake successful setup.
    // A GSI channel is meaningful only when the caller supplies the actual
    // contiguous request pool consumed by gsi_prepare_trbs().
    let _ = (endpoint, event_buffer, ring_base, doorbell);
    false
}

/// Configure one Qualcomm GSI channel with its complete DMA ownership.
/// `buffer_base..buffer_base + 4 * buffer_length` is the contiguous request
/// pool corresponding to Android's `gsi_prepare_trbs()` layout.  Both the
/// TRB ring and that pool must be in the DT-declared Apps-SMMU IOVA window.
pub unsafe fn configure_gsi_channel_with_buffers(
    endpoint: usize,
    event_buffer: u32,
    ring_base: u64,
    doorbell: u64,
    buffer_base: u64,
    buffer_length: usize,
) -> bool {
    let resources = super::super::platform::bramble::usb_resources();
    let count = resources.gsi.event_buffer_count.min(3);
    if endpoint < 2
        || event_buffer == 0
        || event_buffer > count
        || ring_base == 0
        || ring_base & 0x3ff != 0
        || doorbell == 0
        || doorbell & 0x3 != 0
        || doorbell >> 32 != 0
        || buffer_base > usize::MAX as u64
        || buffer_length == 0
    {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !prepare_gsi_ring(
            index,
            endpoint,
            ring_base,
            buffer_base as usize,
            buffer_length,
        ) {
            return false;
        }
        write_qscratch(
            resources.gsi.ring_base_low_offset + index * 4,
            ring_base as u32,
        );
        write_qscratch(
            resources.gsi.ring_base_high_offset + index * 4,
            (ring_base >> 32) as u32,
        );
        write_qscratch(
            resources.gsi.doorbell_low_offset + index * 4,
            doorbell as u32,
        );
        write_qscratch(
            resources.gsi.doorbell_high_offset + index * 4,
            (doorbell >> 32) as u32,
        );
        GSI_CHANNEL_ENDPOINT[index] = endpoint;
        GSI_CHANNEL_READY[index] = true;
        GSI_RING_BASES[index] = ring_base;
        GSI_RING_TRB_COUNTS[index] = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS)
            .map(|shape| shape.num_trbs)
            .unwrap_or(0);
        GSI_BUFFER_BASES[index] = buffer_base;
        GSI_BUFFER_LENGTHS[index] = buffer_length;
        GSI_DOORBELL_BASES[index] = doorbell;
        GSI_RESOURCE_INDEX[index] = 0;
        GSI_RING_ACTIVE[index] = false;
    }
    true
}

/// Allocate and configure a complete GSI channel from the active USB DMA
/// pool. This is the path used by a real gadget client; callers no longer
/// need to invent physical addresses for the ring or request buffers.
pub unsafe fn allocate_gsi_channel(
    endpoint: usize,
    event_buffer: u32,
    doorbell: u64,
    buffer_length: usize,
) -> Option<(*mut u8, *mut u8)> {
    let shape = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS)?;
    let ring_bytes = shape.num_trbs.checked_mul(core::mem::size_of::<Trb>())?;
    let buffer_bytes = shape.data_trbs.checked_mul(buffer_length)?;
    let ring = unsafe { allocate_usb_dma(ring_bytes, 0x400)? };
    let buffers = unsafe { allocate_usb_dma(buffer_bytes, 64)? };
    if unsafe {
        !configure_gsi_channel_with_buffers(
            endpoint,
            event_buffer,
            ring as usize as u64,
            doorbell,
            buffers as usize as u64,
            buffer_length,
        )
    } {
        return None;
    }
    Some((ring, buffers))
}

/// Ring the physical doorbell supplied by the IPA/GSI client. The Android
/// glue writes the address of the ring's final link TRB as two 32-bit MMIO
/// stores; it does not ring the DWC3 QSCRATCH register itself.
unsafe fn ring_gsi_doorbell(index: usize) -> bool {
    if index >= 3 {
        return false;
    }
    let doorbell = unsafe { GSI_DOORBELL_BASES[index] };
    let ring = unsafe { GSI_RING_BASES[index] };
    let count = unsafe { GSI_RING_TRB_COUNTS[index] };
    if doorbell == 0 || ring == 0 || count == 0 {
        return false;
    }
    let Some(link_offset) = (count - 1).checked_mul(core::mem::size_of::<Trb>()) else {
        return false;
    };
    let Some(link) = ring.checked_add(link_offset as u64) else {
        return false;
    };
    if !super::super::platform::bramble::dma_region_valid(
        super::super::platform::bramble::usb_resources().dma_pool,
        link,
        core::mem::size_of::<Trb>() as u64,
        64,
    ) {
        return false;
    }
    unsafe {
        // DWC3's GSI link TRB carries the interrupter/address-extension bits,
        // but the IPA doorbell receives the plain DMA address of that TRB.
        let db = doorbell as usize as *mut u32;
        let db_hi = doorbell.saturating_add(4) as usize as *mut u32;
        core::ptr::write_volatile(db, link as u32);
        let _ = core::ptr::read_volatile(db);
        core::ptr::write_volatile(db_hi, (link >> 32) as u32);
        let _ = core::ptr::read_volatile(db_hi);
    }
    true
}

/// Block or release the GSI write doorbell. Qualcomm runtime suspend blocks
/// writes, waits for IF_STS to idle, then halts DWC3 and drops the platform
/// vote in that order.
pub unsafe fn set_gsi_doorbell_blocked(blocked: bool) -> bool {
    let offset = super::super::platform::bramble::usb_resources()
        .gsi
        .general_cfg_offset;
    unsafe {
        let mut value = read_qscratch(offset);
        if blocked {
            value |= GSI_BLOCK_WR_GO;
        } else {
            value &= !GSI_BLOCK_WR_GO;
        }
        write_qscratch(offset, value);
        (read_qscratch(offset) & GSI_BLOCK_WR_GO != 0) == blocked
    }
}

pub(super) unsafe fn gsi_ready_to_suspend() -> bool {
    let offset = super::super::platform::bramble::usb_resources()
        .gsi
        .interface_status_offset;
    unsafe {
        for _ in 0..1500 {
            if read_qscratch(offset) & GSI_WR_CTRL_STATE == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

pub(super) unsafe fn cache_clean(address: usize, length: usize) {
    // DWC3 and the Apps SMMU consume these objects by DMA.  The probe may be
    // entered with the bootloader's caches enabled, so a no-op here would
    // leave the freshly written TRB/page table only in the CPU cache.  The
    // explicit A/B must force the maintenance even when the DT describes a
    // coherent GSI path; otherwise --dma-cache-maintenance silently compiles
    // but is not an experiment at all.
    let force_cache_maintenance = cfg!(fullerene_aarch64_usb_dma_cache_maintenance);
    if !super::super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
        && !force_cache_maintenance
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc cvac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

pub(super) unsafe fn cache_invalidate(address: usize, length: usize) {
    // Keep the invalidate side of the explicit DMA A/B symmetrical with
    // cache_clean(): observing controller-owned event/TRB writes also needs
    // the cache-line operation when the DT path advertises I/O coherency.
    let force_cache_maintenance = cfg!(fullerene_aarch64_usb_dma_cache_maintenance);
    if !super::super::platform::bramble::usb_resources()
        .gsi
        .disable_io_coherency
        && !force_cache_maintenance
    {
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
        return;
    }
    let start = address & !63;
    let end = address.saturating_add(length).saturating_add(63) & !63;
    let mut line = start;
    while line < end {
        unsafe { core::arch::asm!("dc ivac, {address}", address = in(reg) line, options(nostack)) };
        line += 64;
    }
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

/// Restore the `GUSB2PHYCFG0` bits that `send_ep_command_result` cleared for the
/// duration of an endpoint command.
///
/// Linux performs this restore immediately before `return` in
/// `dwc3_send_gadget_ep_cmd()` (`gadget.c:501-503`), so it runs on EVERY exit -
/// success, timeout and error alike. Fullerene had it on the timeout path only,
/// which left `SUSPHY` and `ENBLSLPM` cleared after the *successful* endpoint
/// commands the handoff actually issues. Measured: `GUSB2PHYCFG0.SUSPHY` read 0
/// at the post-run readout, after all EP0 traffic had finished - and the source
/// order in `init_usb2_gadget_reuse_fastboot_ep0` sets it at 7122, after every
/// other writer. One helper for both exits, so the two paths cannot drift apart
/// again the way they did between the original success-only bug and today's
/// timeout-only fix.
#[inline]
unsafe fn restore_usb2_command_guard(saved_usb2_config: u32) {
    if saved_usb2_config != 0 {
        let usb2 = read(GUSB2PHYCFG0);
        mark_g2w_site(1001);
        write(GUSB2PHYCFG0, usb2 | saved_usb2_config);
    }
}

unsafe fn send_ep_command_result(
    endpoint: usize,
    command: u32,
    param0: u32,
    param1: u32,
    param2: u32,
) -> Option<u8> {
    trace_event(
        TRACE_EP_COMMAND_ISSUE,
        command,
        endpoint as u32,
        param0,
        param1,
        param2,
    );
    let mut saved_usb2_config = 0;
    unsafe {
        // The DWC3 programming guide requires SUSPENDUSB2 and ENBLSLPM to be
        // clear while issuing endpoint commands at USB2 speeds. Linux does
        // this in dwc3_send_gadget_ep_cmd(); a Fastboot handoff commonly
        // leaves one or both bits set after tearing down its gadget.
        let command_kind = command & 0x0f;
        if cfg!(fullerene_aarch64_usb_gadget_handoff_usb2_cmd_guard)
            || command_kind == DEPCMD_ENDTRANSFER
            || read(DSTS) & DSTS_CONNECTSPD_MASK != DSTS_SUPERSPEED
        {
            let mut usb2 = read(GUSB2PHYCFG0);
            saved_usb2_config = usb2 & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
            if saved_usb2_config != 0 {
                usb2 &= !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM);
                mark_g2w_site(1002);
                write(GUSB2PHYCFG0, usb2);
                let _ = read(GUSB2PHYCFG0);
                CMD_GUARD_ENGAGED = true;
            }
        }
        // The DWC3 register names are counter-intuitive: PAR2 is at +0x00,
        // PAR1 at +0x04, and PAR0 at +0x08. Keep both the software argument
        // order and the MMIO write order identical to Linux's
        // dwc3_send_gadget_ep_cmd(). Factory ABL is a useful A/B here: its
        // endpoint-command helper writes only the parameter registers used by
        // the command kind (PAR1/PAR0 for SETEPCONFIG and STARTTRANSFER,
        // PAR0 for SETTRANSFRESOURCE, and none for the other commands). It
        // never unconditionally clears PAR2. Preserve the Linux form by
        // default, but allow the binary-derived write mask to be tested
        // without changing the normal handoff.
        #[cfg(fullerene_aarch64_usb_abl_command_params)]
        match command_kind {
            DEPCMD_SETEPCONFIG | DEPCMD_STARTTRANSFER => {
                write(dep_reg(endpoint, 0x04), param1);
                write(dep_reg(endpoint, 0x08), param0);
            }
            DEPCMD_SETTRANSFRESOURCE => {
                write(dep_reg(endpoint, 0x08), param0);
            }
            _ => {}
        }
        #[cfg(not(fullerene_aarch64_usb_abl_command_params))]
        {
            write(dep_reg(endpoint, 0x08), param0);
            write(dep_reg(endpoint, 0x04), param1);
            write(dep_reg(endpoint, 0x00), param2);
        }
        // Linux's writel() provides the MMIO ordering barrier that separates
        // the parameter writes from the command latch. Preserve that ordering
        // explicitly in this freestanding Rust path.
        core::arch::asm!("dsb sy", options(nostack));
        write(dep_reg(endpoint, 0x0c), command | DEPCMD_CMDACT);
    }
    // qpr1's dwc3_send_gadget_ep_cmd() uses a bounded 3,000-read polling
    // window. Keep this tight: a command that never retires must not leave
    // the early handoff spending an architecture-dependent amount of time in
    // a NOP loop while the host waits for EP0.
    for _ in 0..DWC3_EP_COMMAND_TIMEOUT {
        let status = unsafe { read(dep_reg(endpoint, 0x0c)) };
        if status & DEPCMD_CMDACT == 0 {
            trace_event(
                TRACE_EP_COMMAND_DONE,
                command,
                endpoint as u32,
                status,
                0,
                unsafe { read(DSTS) },
            );
            let success = status & 0xf000 == 0;
            let resource_index = ((status >> DEPCMD_PARAM_SHIFT) & 0x7f) as u8;
            if endpoint == 0 && command & 0x0f == DEPCMD_STARTTRANSFER {
                unsafe {
                    SETUP_ARM_LAST_COMMAND = status;
                }
            }
            restore_usb2_command_guard(saved_usb2_config);
            return success.then_some(resource_index);
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    // Restore on the TIMEOUT path too.
    //
    // Linux puts this restore immediately before `return` in
    // `dwc3_send_gadget_ep_cmd()` (`gadget.c:501-503`), i.e. on EVERY exit -
    // timeout, error and success alike. Fullerene had it inside the success
    // branch only, so a timed-out endpoint command left GUSB2PHYCFG with
    // SUSPHY and ENBLSLPM still cleared. Measured: the `susphy` CCS word reads
    // 0 at the post-run readout, after all EP0 traffic has finished, which is
    // exactly this bug showing itself.
    //
    // The DWC3 programming guide (3.30a / 3.31a section 3.2.2) requires both
    // bits clear *while issuing* the command and restored afterwards; leaving
    // SUSPHY clear parks the USB2 PHY in its suspend configuration, where the
    // parallel receive path can stop - a candidate mechanism for a device that
    // attaches and then receives nothing.
    restore_usb2_command_guard(saved_usb2_config);
    trace_event(
        TRACE_EP_COMMAND_TIMEOUT,
        command,
        endpoint as u32,
        unsafe { read(dep_reg(endpoint, 0x0c)) },
        0,
        unsafe { read(DSTS) },
    );
    if endpoint == 0 && command & 0x0f == DEPCMD_STARTTRANSFER {
        unsafe {
            SETUP_ARM_LAST_COMMAND = 0x8000_0000;
        }
    }
    log_puts("usb: DWC3 endpoint command timeout\n");
    None
}

#[inline]
pub(super) unsafe fn send_ep_command(
    endpoint: usize,
    command: u32,
    param0: u32,
    param1: u32,
    param2: u32,
) -> bool {
    unsafe { send_ep_command_result(endpoint, command, param0, param1, param2).is_some() }
}

/// Allocate one DWC3 transfer resource for an endpoint.
///
/// The SETTRANSFRESOURCE completion does not provide the transfer index used
/// by STARTTRANSFER. Linux obtains that index from the STARTTRANSFER
/// completion (GETTRANSFERINDEX in the EP0 path) and retains it for
/// UPDATETRANSFER/ENDTRANSFER.
pub(super) unsafe fn set_transfer_resource(endpoint: usize) -> bool {
    unsafe { send_ep_command_result(endpoint, DEPCMD_SETTRANSFRESOURCE, 1, 0, 0).is_some() }
}

pub(super) unsafe fn configure_endpoint(endpoint: usize, max_packet: u32, modify: bool) -> bool {
    unsafe { configure_endpoint_kind(endpoint, max_packet, DEPCFG_EP_TYPE_CONTROL, modify) }
}

pub(super) unsafe fn configure_endpoint_kind(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
) -> bool {
    unsafe {
        configure_endpoint_kind_with_interrupter(endpoint, max_packet, endpoint_type, modify, 0)
    }
}

unsafe fn configure_endpoint_kind_with_interrupter(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
    interrupter: u32,
) -> bool {
    if !unsafe {
        configure_endpoint_config(endpoint, max_packet, endpoint_type, modify, interrupter)
    } {
        return false;
    }
    // Linux allocates a transfer resource immediately after configuring each
    // endpoint. DEPSTARTCFG only resets the allocation window; issuing
    // SETTRANSFRESOURCE for every possible endpoint is not equivalent and can
    // make the handoff fail before the first pull-up.
    if !modify
        && !cfg!(fullerene_aarch64_usb_gadget_handoff_no_transfer_resource)
        && !cfg!(fullerene_aarch64_usb_gadget_handoff_android_resource_order)
    {
        return unsafe { set_transfer_resource(endpoint) };
    }
    true
}

pub(super) unsafe fn configure_endpoint_config(
    endpoint: usize,
    max_packet: u32,
    endpoint_type: u32,
    modify: bool,
    interrupter: u32,
) -> bool {
    let action = if modify { DEPCMD_ACTION_MODIFY } else { 0 };
    let mut param0 = action | endpoint_type | (max_packet << DEPCFG_MAX_PACKET_SHIFT);
    // Match dwc3_gadget_set_ep_config(): control endpoints request both
    // transfer-complete and transfer-not-ready notifications, while ordinary
    // data endpoints request transfer-in-progress and transfer-not-ready.
    // EP0's NRDY event is the controller's notification that it has accepted
    // the host SETUP phase, so suppressing it changes the control state
    // machine even though the first transfer is queued successfully.
    let mut param1 = if endpoint_type == DEPCFG_EP_TYPE_CONTROL {
        DEPCFG_XFER_COMPLETE_EN
    } else {
        DEPCFG_XFER_IN_PROGRESS_EN
    };
    let abl_ep_config = cfg!(fullerene_aarch64_usb_abl_ep_config) && endpoint <= 1 && !modify;
    if abl_ep_config {
        // Factory Bramble ABL's DwcConfigureEP follows the Qualcomm msm
        // DEPCFG contract: burst size 3, FIFO number on IN endpoints, and
        // endpoint address (not logical endpoint number) in P1. Its EP0
        // notification mask is XFER_COMPLETE|XFER_IN_PROGRESS (0x300), with
        // no XferNotReady bit. The previous A/B used a misread raw pair;
        // keep this flag tied to the disassembled instruction sequence.
        param0 = endpoint_type
            | (max_packet << DEPCFG_MAX_PACKET_SHIFT)
            | (3 << DEPCFG_BURST_SIZE_SHIFT);
        if endpoint & 1 != 0 {
            param0 |= ((endpoint / 2) as u32) << DEPCFG_FIFO_NUMBER_SHIFT;
        }
        param1 = DEPCFG_XFER_COMPLETE_EN
            | DEPCFG_XFER_IN_PROGRESS_EN
            | (interrupter & 0x1f) << DEPCFG_INT_NUM_SHIFT
            | (endpoint as u32) << DEPCFG_EP_NUMBER_SHIFT;
    } else {
        #[cfg(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_config)]
        if endpoint <= 1 && endpoint_type == DEPCFG_EP_TYPE_CONTROL {
            // Stock Bramble XBL's fixed EP0 configuration emits P1=0x300:
            // XFER_COMPLETE_EN | XFER_IN_PROGRESS_EN. This is a binary-derived
            // A/B for the XBL function-driver contract; keep the Linux-default
            // NRDY notification in the generic path.
            param1 |= DEPCFG_XFER_IN_PROGRESS_EN;
        }
        if endpoint <= 1
            && !(cfg!(fullerene_aarch64_usb_gadget_handoff_xbl_ep0_config)
                && endpoint_type == DEPCFG_EP_TYPE_CONTROL)
        {
            param1 |= DEPCFG_XFER_NOT_READY_EN;
        }
        param1 |= (interrupter & 0x1f) << DEPCFG_INT_NUM_SHIFT;
        param1 |= (endpoint as u32) << DEPCFG_EP_NUMBER_SHIFT;
    }
    let param2 = 0;
    unsafe { send_ep_command(endpoint, DEPCMD_SETEPCONFIG, param0, param1, param2) }
}

#[inline]
pub(super) unsafe fn apply_ep0_txfifo_fix() {
    #[cfg(fullerene_aarch64_usb_gadget_handoff_ep0_txfifo_fix)]
    {
        // Handshakes are generated internally, but every EP0 IN data packet
        // is pushed through the endpoint's TX FIFO. A Fastboot session that
        // resized or emptied FIFO 0 leaves EP1 IN unable to send any data
        // packet: the SETUP handshake still works and the host's descriptor
        // read then NAKs forever (read/64 -110). Raise a degenerate depth
        // while preserving the FIFO start address.
        let fifo = read(GTXFIFOSIZ0);
        let depth = fifo & 0x7fff;
        if depth < 32 {
            let raised = (fifo & 0xffff_0000) | 32;
            write(GTXFIFOSIZ0, raised);
            trace_event(
                TRACE_SETUP_QUEUED,
                0x5458_4631, // "TXF1" EP0 IN FIFO raised
                fifo,
                raised,
                0,
                read(DSTS),
            );
            let _ = read(GTXFIFOSIZ0);
        }
    }
}

pub(super) unsafe fn start_transfer(endpoint: usize, trb: *const Trb) -> bool {
    let address = unsafe { dma_iova_for(trb as usize) };
    unsafe {
        // DWC3's STARTTRANSFER parameters are PAR0=address[63:32] and
        // PAR1=address[31:0]. The endpoint command helper writes the named
        // param0/param1 fields to those registers respectively. Linux issues
        // STARTTRANSFER with command parameter 0 for EP0 and ordinary
        // non-isochronous endpoints; the controller returns the resource
        // index in the command completion, which is retained below.
        let Some(resource_index) = send_ep_command_result(
            endpoint,
            DEPCMD_STARTTRANSFER,
            (address >> 32) as u32,
            address as u32,
            0,
        ) else {
            return false;
        };
        if endpoint < 2 {
            EP0_RESOURCE_INDEX[endpoint] = resource_index;
        } else if endpoint < 4 {
            DATA_RESOURCE_INDEX[endpoint - 2] = resource_index;
        }
        true
    }
}

/// Retry an EP0 Start Transfer for up to `window_ms`.
///
/// The endpoint command engine rejects (or wedges) Start Transfer for a
/// bounded window after the host's bus reset, while the identical command
/// succeeds seconds later. The host keeps issuing IN tokens during the data
/// phase and tolerates the NAKs until its 5 s control timeout, so a retry
/// window measured in seconds still lands inside the host's first read
/// instead of stalling the transfer and losing the whole enumeration.
///
/// A failed re-arm can also mean the previous owner's transfer is still
/// active and its transfer resource is therefore consumed. msm-4.19 revokes
/// every active transfer with End Transfer and waits 100 us on DWC_usb31
/// before the resource is reusable; repeat that revocation between attempts
/// instead of assuming the bus reset already flushed the transfer.
pub(super) unsafe fn retry_start_transfer(
    endpoint: usize,
    trb: *const Trb,
    window_ms: u64,
) -> bool {
    let deadline = unsafe {
        arch_counter().saturating_add(arch_counter_frequency().saturating_mul(window_ms) / 1000)
    };
    loop {
        if unsafe { start_transfer(endpoint, trb) } {
            return true;
        }
        if unsafe { arch_counter() } >= deadline {
            return false;
        }
        unsafe {
            let resource_index = if endpoint < 2 {
                let index = EP0_RESOURCE_INDEX[endpoint];
                if index == 0 { 1 } else { index }
            } else {
                1
            };
            send_ep_command(
                endpoint,
                DEPCMD_ENDTRANSFER
                    | DEPCMD_CMDIOC
                    | DEPCMD_HIPRI_FORCERM
                    | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            );
        }
        super::super::timer::delay_us(200);
        unsafe {
            set_transfer_resource(endpoint);
        }
        super::super::timer::delay_us(300);
    }
}

pub(super) unsafe fn end_transfer(endpoint: usize) -> bool {
    // NOTE(bisect): the EP0-OUT index-0 rewrite is temporarily restored.
    // Passing the legitimate resource index 0 wedged the rescue path on the
    // handset (gate runs stopped reaching evaluation); re-derive the correct
    // form from a passing baseline before reapplying.
    let resource_index = if endpoint < 2 {
        let index = unsafe { EP0_RESOURCE_INDEX[endpoint] };
        if index == 0 { 1 } else { index }
    } else if endpoint < 4 {
        let index = unsafe { DATA_RESOURCE_INDEX[endpoint - 2] };
        if index == 0 { 1 } else { index }
    } else {
        1
    };
    unsafe {
        send_ep_command(
            endpoint,
            DEPCMD_ENDTRANSFER
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        )
    }
}

/// Apply qpr1's active-transfer part of `dwc3_gadget_reset_interrupt()` to
/// the control endpoint.  A bus reset terminates the wire transaction, but
/// qpr1 still revokes the DWC3 transfer resource before clearing endpoint
/// stalls.  Keep this opt-in because the normal handoff profile deliberately
/// preserves its armed EP0 across reset; the source-order reset A/B uses the
/// returned STARTTRANSFER resource index when it is available.
pub(super) unsafe fn stop_active_ep0_at_reset() -> bool {
    unsafe {
        let resource_index = EP0_RESOURCE_INDEX[0];
        if resource_index == 0 {
            return true;
        }
        let stopped = send_ep_command(
            0,
            DEPCMD_ENDTRANSFER
                | DEPCMD_CMDIOC
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        );
        EP0_RESOURCE_INDEX[0] = 0;
        // qpr1 waits 100us after ENDTRANSFER on DWC_usb31 because the
        // hardware cannot provide the older command-completion guarantee.
        if read(GSNPSID) >> 16 == DWC31_IP {
            crate::timer::delay_us(100);
        }
        stopped
    }
}

/// Apply the hardware portion of the official Qualcomm DWC3 stop cleanup.
///
/// `dwc3_gadget_run_stop(false, false)` acknowledges the GSI event buffers
/// and then sends `DWC3_CONTROLLER_NOTIFY_CLEAR_DB`; the msm glue blocks GSI
/// write-go and clears GSI_EN. The active-transfer helper is intentionally
/// not folded in here: Linux supplies its per-endpoint resource index from
/// `struct dwc3_ep`, which a fresh fastboot handoff does not inherit.
pub(super) unsafe fn clear_gsi_stop_state() {
    unsafe {
        let gsi = super::super::platform::bramble::usb_resources().gsi;
        for index in 0..gsi.event_buffer_count.min(3) as usize {
            let register = GEVNTCOUNT0 + (index + 1) * GEVNT_BUFFER_STRIDE;
            let count = read(register) & GEVNTCOUNT_MASK;
            write(register, count);
        }

        clear_gsi_doorbell_state();
    }
}

/// Reproduce Qualcomm's DWC3_CONTROLLER_NOTIFY_CLEAR_DB notification.
///
/// The Android msm callback only performs this write when its GSI event
/// buffers exist: block new GSI doorbells, then clear GSI_EN. It does not
/// touch EP0, the USB2 PHY, or the primary DWC3 event ring.
pub(super) unsafe fn clear_gsi_doorbell_state() {
    unsafe {
        let offset = super::super::platform::bramble::usb_resources()
            .gsi
            .general_cfg_offset;
        let mut value = read_qscratch(offset) | GSI_BLOCK_WR_GO;
        write_qscratch(offset, value);
        value = read_qscratch(offset) & !GSI_EN;
        write_qscratch(offset, value);
    }
}

/// Revoke every ordinary UDC data transfer before endpoint state or request
/// ownership is reset. EP0 is handled by the control-reset path separately.
pub(super) unsafe fn teardown_data_endpoints() {
    unsafe {
        if !DATA_ENDPOINTS_READY {
            return;
        }
        for endpoint in 2..=3 {
            if DATA_RESOURCE_INDEX[endpoint - 2] != 0 {
                let _ = end_transfer(endpoint);
            }
        }
        write(DALEPENA, read(DALEPENA) & !((1 << 2) | (1 << 3)));
        let _ = udc_mut().disable_endpoint(0x02);
        let _ = udc_mut().disable_endpoint(0x83);
        DATA_ENDPOINTS_READY = false;
        DATA_RESOURCE_INDEX = [0; 2];
        DATA_REQUEST_SLOTS = [usize::MAX; 2];
    }
}

/// Cancel outstanding ordinary requests at the runtime-PM boundary while
/// retaining endpoint configuration for resume. DWC3 must no longer own a
/// TRB when the UDC is marked suspended.
pub(super) unsafe fn suspend_data_transfers() -> bool {
    unsafe {
        if !DATA_ENDPOINTS_READY {
            return true;
        }
        for endpoint in 2..=3 {
            let index = endpoint - 2;
            if DATA_RESOURCE_INDEX[index] != 0 {
                if !end_transfer(endpoint) {
                    return false;
                }
            }
            let address = if endpoint == 3 { 0x83 } else { 0x02 };
            let slot = DATA_REQUEST_SLOTS[index];
            if slot != usize::MAX {
                let length = udc_mut()
                    .request(address, slot)
                    .map(|request| request.length)
                    .unwrap_or(0);
                let _ = udc_mut().complete(address, slot, 0, true);
                GadgetDriver::on_data_complete(gadget_mut(), address, 0, true);
                let _ = udc_mut().release(address, slot);
                trace_event(TRACE_TRANSFER_COMPLETE, endpoint as u32, 0, 0, length, 1);
            }
            DATA_RESOURCE_INDEX[index] = 0;
            DATA_REQUEST_SLOTS[index] = usize::MAX;
        }
        true
    }
}

/// Cancel live GSI requests without discarding their registered rings or
/// client doorbells. The function receives an explicit suspend callback and
/// can requeue after resume; no request is silently left owned by DWC3.
pub(super) unsafe fn suspend_gsi_transfers() -> bool {
    unsafe {
        for index in 0..3 {
            if !GSI_CHANNEL_READY[index] {
                continue;
            }
            let endpoint = GSI_CHANNEL_ENDPOINT[index];
            let event_buffer = (index + 1) as u32;
            let slot = GSI_REQUEST_SLOTS[index];
            if GSI_RING_ACTIVE[index] {
                if !end_gsi_transfer(endpoint, event_buffer) {
                    return false;
                }
            }
            let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
            if slot != usize::MAX {
                GadgetDriver::on_gsi_data_complete(gadget_mut(), address, 0, true);
                let _ = udc_mut().release(address, slot);
            }
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
            GSI_RING_ACTIVE[index] = false;
            GSI_RESOURCE_INDEX[index] = 0;
        }
        if GSI_GADGET_BOUND {
            GadgetDriver::on_gsi_channel_suspend(gadget_mut());
        }
        true
    }
}

/// Start a non-control transfer through Qualcomm's GSI event-buffer path.
/// event_buffer is the Android DWC3 interrupt/event-buffer index (1..=3);
/// EP0 must continue to use start_transfer and index zero.
unsafe fn start_gsi_transfer(endpoint: usize, event_buffer: u32, trb: *const Trb) -> Option<u8> {
    let Some((param0, param1)) = gsi_transfer_params(event_buffer, trb as usize) else {
        return None;
    };
    unsafe {
        if !enable_gsi_wrapper() {
            return None;
        }
        send_ep_command_result(endpoint, DEPCMD_STARTTRANSFER, param0, param1, 0)
    }
}

/// Set ownership on the OUT data TRBs and notify DWC3 of the GSI resource.
/// Android intentionally separates ring preparation from this step so a
/// channel can be armed only after its buffers and doorbell are ready.
pub unsafe fn update_gsi_transfer(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index]
            || GSI_CHANNEL_ENDPOINT[index] != endpoint
            || GSI_RING_BASES[index] == 0
            || GSI_RING_ACTIVE[index]
            || endpoint & 1 != 0
        {
            return false;
        }
        let Some(shape) = gsi_ring_shape(false, GSI_DEFAULT_NUM_BUFFERS) else {
            return false;
        };
        let ring = GSI_RING_BASES[index] as usize as *mut Trb;
        for trb_index in shape.first_buffer_trb..shape.first_buffer_trb + shape.data_trbs {
            let mut ctrl = read_volatile(addr_of!((*ring.add(trb_index)).ctrl));
            ctrl |= TRB_HWO;
            // Publish HWO behind a write barrier, as the shipped gadget.c does.
            trb_publish_barrier();
            write_volatile(addr_of_mut!((*ring.add(trb_index)).ctrl), ctrl);
        }
        cache_clean(ring as usize, shape.num_trbs * core::mem::size_of::<Trb>());
        let resource_index = GSI_RESOURCE_INDEX[index];
        if resource_index == 0
            || !send_ep_command(
                endpoint,
                DEPCMD_UPDATETRANSFER | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
                0,
                0,
                0,
            )
        {
            return false;
        }
        GSI_RING_ACTIVE[index] = true;
    }
    true
}

/// Stop a live GSI transfer before changing its ring or runtime-power state.
pub unsafe fn end_gsi_transfer(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index] || GSI_CHANNEL_ENDPOINT[index] != endpoint {
            return false;
        }
        let resource_index = GSI_RESOURCE_INDEX[index];
        if resource_index == 0 {
            return false;
        }
        let stopped = send_ep_command(
            endpoint,
            DEPCMD_ENDTRANSFER
                | DEPCMD_HIPRI_FORCERM
                | ((resource_index as u32) << DEPCMD_PARAM_SHIFT),
            0,
            0,
            0,
        );
        if stopped {
            GSI_RING_ACTIVE[index] = false;
            GSI_PENDING[index] = false;
            GSI_REQUEST_SLOTS[index] = usize::MAX;
        }
        stopped
    }
}

/// Configure a non-control bulk endpoint for the Qualcomm GSI event path.
/// This is intentionally opt-in: the normal UDC data path uses event buffer
/// zero and must not assert the global GSI enable bit merely because event
/// buffers are available.
pub unsafe fn enable_gsi_data_endpoint(
    endpoint: usize,
    event_buffer: u32,
    max_packet: u32,
) -> bool {
    let event_buffer_count = super::super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if endpoint < 2
        || endpoint >= 8
        || event_buffer == 0
        || event_buffer > event_buffer_count
        || max_packet == 0
    {
        return false;
    }
    let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
    unsafe {
        if !configure_endpoint_kind_with_interrupter(
            endpoint,
            max_packet,
            DEPCFG_EP_TYPE_BULK,
            false,
            event_buffer,
        ) {
            return false;
        }
        if !udc_mut().configure_endpoint(endpoint_address, max_packet as u16, true) {
            return false;
        }
        write(DALEPENA, read(DALEPENA) | (1 << endpoint));
    }
    true
}

/// Bind a complete GSI data endpoint in the same order as the Android client:
/// configure the DWC3 endpoint, allocate the ring/request pool, publish the
/// client doorbell, then enable the wrapper. A caller receives the owned
/// request-pool pointers and can pass the first one to `queue_gsi_transfer`.
pub unsafe fn configure_gsi_data_endpoint(
    endpoint: usize,
    event_buffer: u32,
    max_packet: u32,
    doorbell: u64,
    buffer_length: usize,
) -> Option<(*mut u8, *mut u8)> {
    if !unsafe { enable_gsi_data_endpoint(endpoint, event_buffer, max_packet) } {
        return None;
    }
    let allocation =
        unsafe { allocate_gsi_channel(endpoint, event_buffer, doorbell, buffer_length) };
    if allocation.is_none() {
        let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
        unsafe {
            let _ = udc_mut().disable_endpoint(address);
            write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
        }
        return None;
    }
    if !unsafe { enable_gsi_wrapper() } {
        unsafe {
            let _ = disable_gsi_data_endpoint(endpoint, event_buffer);
        }
        return None;
    }
    allocation
}

/// Tear down one GSI endpoint after its request has completed or been
/// cancelled. ENDTRANSFER precedes UDC removal, and the global wrapper is
/// disabled only once no channel remains published.
pub unsafe fn disable_gsi_data_endpoint(endpoint: usize, event_buffer: u32) -> bool {
    let count = super::super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count
        .min(3);
    if endpoint < 2 || endpoint >= 8 || event_buffer == 0 || event_buffer > count {
        return false;
    }
    let index = (event_buffer - 1) as usize;
    unsafe {
        if !GSI_CHANNEL_READY[index] || GSI_CHANNEL_ENDPOINT[index] != endpoint {
            return false;
        }
        if GSI_RING_ACTIVE[index] && !end_gsi_transfer(endpoint, event_buffer) {
            return false;
        }
        let address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
        let _ = udc_mut().disable_endpoint(address);
        write(DALEPENA, read(DALEPENA) & !(1 << endpoint));
        GSI_PENDING[index] = false;
        GSI_REQUEST_SLOTS[index] = usize::MAX;
        GSI_RING_ACTIVE[index] = false;
        GSI_RESOURCE_INDEX[index] = 0;
        GSI_CHANNEL_READY[index] = false;
        GSI_CHANNEL_ENDPOINT[index] = 0;

        let no_channels = !GSI_CHANNEL_READY[0] && !GSI_CHANNEL_READY[1] && !GSI_CHANNEL_READY[2];
        if no_channels {
            let offset = super::super::platform::bramble::usb_resources()
                .gsi
                .general_cfg_offset;
            let value = read_qscratch(offset) & !GSI_EN;
            write_qscratch(offset, value);
        }
    }
    true
}

/// Queue one DMA request on a previously configured GSI data endpoint. The
/// supplied buffer is treated as the beginning of the contiguous four-buffer
/// pool expected by Android's GSI ABI; callers must provide space for all
/// four `length`-sized buffers and must not reuse it until completion.
pub unsafe fn queue_gsi_transfer(
    endpoint: usize,
    event_buffer: u32,
    buffer: *const u8,
    length: usize,
) -> bool {
    let event_buffer_count = super::super::platform::bramble::usb_resources()
        .gsi
        .event_buffer_count;
    if endpoint < 2
        || endpoint >= 8
        || event_buffer == 0
        || event_buffer > event_buffer_count
        || length == 0
    {
        return false;
    }
    let trb_index = (event_buffer - 1) as usize;
    let endpoint_address = endpoint as u8 | if endpoint & 1 != 0 { 0x80 } else { 0 };
    unsafe {
        if GSI_CHANNEL_ENDPOINT[trb_index] != endpoint {
            return false;
        }
        if !GSI_CHANNEL_READY[trb_index] {
            return false;
        }
        if GSI_PENDING[trb_index] {
            return false;
        }
        let Some(shape) = gsi_ring_shape(endpoint & 1 != 0, GSI_DEFAULT_NUM_BUFFERS) else {
            return false;
        };
        let total_buffer_bytes = (shape.data_trbs as u64).saturating_mul(length as u64);
        let pool = super::super::platform::bramble::usb_resources().dma_pool;
        if buffer as usize as u64 != GSI_BUFFER_BASES[trb_index]
            || length != GSI_BUFFER_LENGTHS[trb_index]
            || !super::super::platform::bramble::dma_region_valid(
                pool,
                buffer as usize as u64,
                total_buffer_bytes,
                64,
            )
        {
            return false;
        }
        let Some(request_slot) = udc_mut().queue(endpoint_address, length as u32) else {
            return false;
        };
        if !udc_mut().start(endpoint_address, request_slot) {
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        }
        let ring_base = GSI_RING_BASES[trb_index];
        if !prepare_gsi_ring(trb_index, endpoint, ring_base, buffer as usize, length) {
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        }
        GSI_PENDING[trb_index] = true;
        GSI_REQUEST_SLOTS[trb_index] = request_slot;
        let Some(resource_index) =
            start_gsi_transfer(endpoint, event_buffer, ring_base as usize as *const Trb)
        else {
            GSI_PENDING[trb_index] = false;
            GSI_REQUEST_SLOTS[trb_index] = usize::MAX;
            let _ = udc_mut().release(endpoint_address, request_slot);
            return false;
        };
        GSI_RESOURCE_INDEX[trb_index] = resource_index;
        let transfer_updated = endpoint & 1 != 0 || update_gsi_transfer(endpoint, event_buffer);
        if transfer_updated && ring_gsi_doorbell(trb_index) {
            GSI_RING_ACTIVE[trb_index] = true;
            true
        } else {
            GSI_PENDING[trb_index] = false;
            GSI_REQUEST_SLOTS[trb_index] = usize::MAX;
            let _ = end_gsi_transfer(endpoint, event_buffer);
            let _ = udc_mut().release(endpoint_address, request_slot);
            false
        }
    }
}

/// Queue an ordinary gadget bulk request on the function's EP2 OUT or EP3
/// IN endpoint. GSI is an Android IPA optimization; Linux's normal UDC path
/// still uses DWC3's event buffer zero and must remain usable independently.
pub unsafe fn queue_bulk_transfer(endpoint: usize, buffer: *const u8, length: usize) -> bool {
    if !DATA_ENDPOINTS_READY || (endpoint != 2 && endpoint != 3) || length == 0 {
        return false;
    }
    let index = endpoint - 2;
    unsafe {
        if DATA_REQUEST_SLOTS[index] != usize::MAX {
            return false;
        }
        let address = if endpoint == 3 { 0x83 } else { 0x02 };
        let pool = super::super::platform::bramble::usb_resources().dma_pool;
        if !super::super::platform::bramble::dma_region_valid(
            pool,
            buffer as usize as u64,
            length as u64,
            64,
        ) {
            return false;
        }
        let Some(slot) = udc_mut().queue(address, length as u32) else {
            return false;
        };
        if !udc_mut().start(address, slot) {
            let _ = udc_mut().release(address, slot);
            return false;
        }
        let trb = addr_of_mut!(DATA_TRBS).cast::<Trb>().add(index);
        prepare_trb_at(trb, buffer, length, TRB_NORMAL);
        DATA_REQUEST_SLOTS[index] = slot;
        if start_transfer(endpoint, trb) {
            true
        } else {
            DATA_REQUEST_SLOTS[index] = usize::MAX;
            DATA_RESOURCE_INDEX[index] = 0;
            let _ = udc_mut().release(address, slot);
            false
        }
    }
}
