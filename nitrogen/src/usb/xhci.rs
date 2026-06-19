//! xHCI (USB 3.0) Host Controller Driver — full implementation.
//!
//! Provides:
//! - Controller reset, start, port management
//! - Event ring processing (command + transfer completion)
//! - Command ring (Enable Slot, Address Device, Configure Endpoint)
//! - Per-endpoint transfer rings (Setup/Data/Status for control, Normal for bulk)
//! - Control transfer (slot_id, setup_packet, data buffer)
//! - Bulk transfer (slot_id, endpoint, data buffer, direction)

use crate::usb::{UsbDevice, UsbSetupPacket, UsbEndpointDesc, UsbDirection, UsbXferType};
use crate::DriverContext;
use alloc::vec::Vec;

// ── Register offsets ─────────────────────────────────────────
// Capability
const CAPLENGTH: u32 = 0x00;
const HCSPARAMS1: u32 = 0x04;
const HCCPARAMS1: u32 = 0x10;
const DBOFF: u32 = 0x14;
const RTSOFF: u32 = 0x18;

// Operational (relative to CAPLENGTH)
const USBCMD: u32 = 0x00;
const USBSTS: u32 = 0x04;
const CRCR: u32 = 0x18;
const DCBAAP: u32 = 0x30;
const CONFIG: u32 = 0x38;
const PORTSC_BASE: u32 = 0x400;

const CMD_RUN: u32 = 1 << 0;
const CMD_HCRST: u32 = 1 << 1;
const STS_HCH: u32 = 1 << 0;

// Runtime registers (relative to RTSOFF)
const IMAN_OFF: u32 = 0x00;  // interrupter 0: IMAN
const IMOD_OFF: u32 = 0x04;  // IMOD
const ERSTZ_OFF: u32 = 0x08; // Event Ring Segment Table Size
const ERSTBA_OFF: u32 = 0x10; // Event Ring Segment Table Base Address
const ERSDP_OFF: u32 = 0x18; // Event Ring Dequeue Pointer

// PORTSC
const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PED: u32 = 1 << 1;
const PORTSC_PR: u32 = 1 << 4;
const PORTSC_PP: u32 = 1 << 9;

// TRB type (bits 10..15 of flags)
const TRB_NORMAL: u8 = 1;
const TRB_SETUP: u8 = 2;
const TRB_DATA: u8 = 3;
const TRB_STATUS: u8 = 4;
const TRB_LINK: u8 = 6;
const TRB_ENABLE_SLOT: u8 = 9;
const TRB_ADDRESS_DEVICE: u8 = 10;
const TRB_CONFIGURE_ENDPOINT: u8 = 11;

// TRB flags
const TRB_C: u32 = 1 << 0;
const TRB_CHAIN: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_ENT: u32 = 1 << 11;
const TRB_DIR_IN: u32 = 1 << 16;
const TRB_TRT_MASK: u32 = 3 << 16; // Transfer Type for Setup TRB

const TRB_SIZE: usize = 16;

// ── TRB ──────────────────────────────────────────────────────
#[repr(C)]
struct Trb {
    params: [u8; 8],
    status: u32,
    flags: u32,
}

impl Trb {
    fn new(trb_type: u8, cycle: u32) -> Self {
        Self { params: [0; 8], status: 0, flags: cycle | ((trb_type as u32) << 10) }
    }
}

// ── Ring (enqueue direction: driver→HC) ─────────────────────
struct Ring {
    entries: &'static mut [Trb],
    phys: u64,
    enq: usize,
    cycle: u32,
    len: usize,
}

impl Ring {
    fn alloc(ctx: &dyn DriverContext, n: usize) -> Option<Self> {
        let size = n * TRB_SIZE;
        let pages = (size + 4095) / 4096;
        let p = ctx.allocate_contiguous_frames(pages).ok()?;
        let v = ctx.phys_to_virt(p) as *mut Trb;
        let entries = unsafe { core::slice::from_raw_parts_mut(v, n) };
        // Link back to self for circular behaviour
        if n > 1 {
            let last = &mut entries[n - 1];
            // Link TRB: type=6, TC=1 (toggle cycle on wrap), cycle=1
            last.flags = TRB_LINK_BIT | (1 << 1) | 1;
            last.params[..8].copy_from_slice(&p.to_le_bytes());
        }
        Some(Self { entries, phys: p, enq: 0, cycle: 1, len: n })
    }

    fn enqueue(&mut self, mut trb: Trb) {
        trb.flags = (trb.flags & !TRB_C) | self.cycle;
        self.entries[self.enq] = trb;
        self.enq += 1;
        if self.enq >= self.len - 1 { // wrap before link TRB
            // Update Link TRB cycle bit to match current cycle
            let link_idx = self.len - 1;
            self.entries[link_idx].flags = (self.entries[link_idx].flags & !TRB_C) | self.cycle;
            self.enq = 0;
            self.cycle ^= 1;
        }
    }
}

const TRB_LINK_BIT: u32 = (TRB_LINK as u32) << 10;

// ── Event Ring (dequeue direction: HC→driver) ────────────────
struct EventRing {
    entries: &'static mut [Trb],
    phys: u64,
    deq: usize,
    cycle: u32,
    len: usize,
}

impl EventRing {
    fn alloc(ctx: &dyn DriverContext, n: usize) -> Option<Self> {
        let size = n * TRB_SIZE;
        let pages = (size + 4095) / 4096;
        let p = ctx.allocate_contiguous_frames(pages).ok()?;
        let v = ctx.phys_to_virt(p) as *mut Trb;
        let entries = unsafe { core::slice::from_raw_parts_mut(v, n) };
        // Zero all entries (HC initial cycle bit = 1, driver expects cycle=1)
        for e in entries.iter_mut() {
            e.flags = 0;
        }
        Some(Self { entries, phys: p, deq: 0, cycle: 1, len: n })
    }

    fn has_pending(&self) -> bool {
        let flags = unsafe { core::ptr::read_volatile(&self.entries[self.deq].flags) };
        (flags & TRB_C) == self.cycle
    }

    fn pop(&mut self) -> Option<Trb> {
        if !self.has_pending() { return None; }
        let trb = unsafe { core::ptr::read_volatile(&self.entries[self.deq]) };
        self.deq += 1;
        if self.deq >= self.len {
            self.deq = 0;
            self.cycle ^= 1;
        }
        Some(trb)
    }

    fn dequeue_ptr(&self) -> u64 {
        self.phys + (self.deq as u64 * TRB_SIZE as u64) | (self.cycle as u64)
    }
}

// ── EVENT RING SEGMENT TABLE entry ───────────────────────────
// 16 bytes: base (8), size (4), rsvd (4)
#[repr(C)]
struct ErstEntry {
    base_lo: u32,
    base_hi: u32,
    size: u32,
    rsvd: u32,
}

// ── Device / Input Context (simplified to one 64-byte page) ──
// Each context is 32 dwords (128 bytes): 4 for slot, 4×31 for endpoints (more than enough)
// We use a 4KB page per device context (4096/128=32, but xHCI requires 64-byte alignment)
#[repr(C, align(64))]
struct DevCtx {
    data: [u32; 8],  // Slot context (4) + control ep ctx (4)
}

#[repr(C, align(64))]
struct InputCtx {
    drop: u32,
    add: u32,
    rsvd: [u32; 6],
    slot: [u32; 4],
    ep0: [u32; 4],
}

// ── Port speed mapping ───────────────────────────────────────
fn port_speed_to_usb(speed: u32) -> crate::usb::UsbSpeed {
    match speed {
        1 => crate::usb::UsbSpeed::Full,
        2 => crate::usb::UsbSpeed::Low,
        3 => crate::usb::UsbSpeed::High,
        _ => crate::usb::UsbSpeed::High, // SuperSpeed and above → treat as High for now
    }
}

// ── xHCI Controller ──────────────────────────────────────────
unsafe impl Send for XhciController {}

pub struct XhciController {
    mmio: *mut u8,
    op_off: u32,
    rt_off: u32,
    db_off: u32,
    n_ports: u32,
    max_slots: u32,
    ppc: bool, // Port Power Control supported
    dcbaa_phys: u64,
    dcbaa: &'static mut [u64; 256],
    slots: Vec<SlotState>,
    cmd_ring: Ring,
    ev_ring: EventRing,
    erst_phys: u64,
    ports_done: u32, // bitmask
    devices: Vec<UsbDevice>,
    ctx: *const dyn DriverContext,
    n_slots_used: u32,
}

struct SlotState {
    slot_id: u32,
    dev_addr: u8,
    ep0_ring: Ring,
    bulk_out_ring: Option<Ring>,
    bulk_in_ring: Option<Ring>,
    dev_ctx_phys: u64,
    in_ctx_phys: u64,
}

impl XhciController {
    pub fn new(mmio_base: *mut u8, ctx: &'static dyn DriverContext) -> Option<Self> {
        let caps = mmio_base;
        let caplength = unsafe { core::ptr::read_volatile(caps as *const u8) } as u32;
        let hcs1 = unsafe { core::ptr::read_volatile((caps.add(4) as *const u32)) };
        let hcc1 = unsafe { core::ptr::read_volatile((caps.add(0x10) as *const u32)) };
        let db_off_val = unsafe { core::ptr::read_volatile((caps.add(0x14) as *const u32)) };
        let rt_off_val = unsafe { core::ptr::read_volatile((caps.add(0x18) as *const u32)) };

        let n_ports = (hcs1 >> 24) & 0xFF;
        let max_slots = hcs1 & 0xFF;
        let ppc = (hcs1 & (1 << 4)) != 0; // Port Power Control supported
        let db_off = db_off_val & 0xFFFF_FFFC;
        let rt_off = rt_off_val & 0xFFFF_FFFC;
        let op_off = caplength;
        let op = unsafe { mmio_base.add(op_off as usize) };

        // ── xHCI Legacy Support Handoff ────────────────────────
        // Scan extended capabilities for USB Legacy Support (ID=1).
        // If BIOS holds ownership, we must claim it.
        {
            let mut ec_off = (((hcc1 >> 16) & 0xFFFF) as usize) * 4;
            while ec_off != 0 && ec_off < 0x400 {
                let ec_id = unsafe { core::ptr::read_volatile(caps.add(ec_off) as *const u8) };
                let ec_next = unsafe { core::ptr::read_volatile(caps.add(ec_off + 1) as *const u8) as usize } * 4;
                if ec_id == 1 {
                    // USB Legacy Support capability found
                    let bios_sem = unsafe { core::ptr::read_volatile(caps.add(ec_off + 2) as *const u8) };
                    if bios_sem & 1 != 0 {
                        // BIOS owns the controller — take ownership
                        unsafe {
                            core::ptr::write_volatile(caps.add(ec_off + 3) as *mut u8, 1);
                        }
                        // Wait for BIOS to release ownership (up to 1 second)
                        for _ in 0..1_000_000 {
                            let b = unsafe { core::ptr::read_volatile(caps.add(ec_off + 2) as *const u8) };
                            if b & 1 == 0 { break; }
                        }
                    }
                }
                ec_off = ec_next;
            }
        }

        // Check if controller is already running (firmware initialized for boot)
        let sts = unsafe { core::ptr::read_volatile((op.add(USBSTS as usize)) as *const u32) };
        let already_running = (sts & STS_HCH) == 0;

        if already_running {
            // Firmware left the controller running — don't reset, which would
            // clear port power state (PPC=0 means PP is read-only hardware-
            // managed, and HCRST would lose port power without recovery).
            // We stop the controller first to program our registers, then restart.
        } else {
            // Reset
            unsafe { core::ptr::write_volatile((op.add(USBCMD as usize)) as *mut u32, CMD_HCRST); }
            for _ in 0..200_000 {
                if unsafe { core::ptr::read_volatile((op.add(USBCMD as usize)) as *const u32) } & CMD_HCRST == 0 { break; }
            }
            for _ in 0..200_000 {
                if unsafe { core::ptr::read_volatile((op.add(USBSTS as usize)) as *const u32) } & STS_HCH != 0 { break; }
            }
        }

        // Stop the controller before programming (if running)
        if already_running {
            unsafe { core::ptr::write_volatile((op.add(USBCMD as usize)) as *mut u32, 0); }
            for _ in 0..200_000 {
                if unsafe { core::ptr::read_volatile((op.add(USBSTS as usize)) as *const u32) } & STS_HCH != 0 { break; }
            }
        }

        // Allocate DCBAA (aligned to 64 bytes)
        let dcbaa_p = ctx.allocate_contiguous_frames(1).ok()?;
        let dcbaa_v = ctx.phys_to_virt(dcbaa_p) as *mut u64;
        let dcbaa = unsafe { &mut *dcbaa_v.cast::<[u64; 256]>() };
        for e in dcbaa.iter_mut() { *e = 0; }

        // Command ring
        let cmd = Ring::alloc(ctx, 256)?;

        // Event ring + ERST
        let ev = EventRing::alloc(ctx, 256)?;
        let erst_p = ctx.allocate_contiguous_frames(1).ok()?;
        let erst_v = ctx.phys_to_virt(erst_p) as *mut ErstEntry;
        unsafe {
            (*erst_v).base_lo = ev.phys as u32;
            (*erst_v).base_hi = (ev.phys >> 32) as u32;
            (*erst_v).size = 256;
            (*erst_v).rsvd = 0;
        }

        // Set DCBAA
        unsafe { core::ptr::write_volatile((op.add(DCBAAP as usize)) as *mut u64, dcbaa_p); }

        // Set CRCR
        let crcr_val = cmd.phys | 1; // cycle = 1, ring running
        unsafe { core::ptr::write_volatile((op.add(CRCR as usize)) as *mut u64, crcr_val); }

        // Configure max slots
        unsafe { core::ptr::write_volatile((op.add(CONFIG as usize)) as *mut u32, max_slots); }

        // Set up event ring in interrupter 0
        let rt_base = unsafe { mmio_base.add(rt_off as usize) };
        unsafe {
            core::ptr::write_volatile((rt_base.add(ERSTBA_OFF as usize)) as *mut u64, erst_p);
            core::ptr::write_volatile((rt_base.add(ERSTZ_OFF as usize)) as *mut u32, 1);
        }
        // Set event ring dequeue pointer
        let deq = ev.phys | 1; // cycle = 1, EHB bit not set
        unsafe { core::ptr::write_volatile((rt_base.add(ERSDP_OFF as usize)) as *mut u64, deq); }

        // Start
        unsafe { core::ptr::write_volatile((op.add(USBCMD as usize)) as *mut u32, CMD_RUN); }

        // Unmask interrupt
        unsafe { core::ptr::write_volatile((rt_base.add(IMAN_OFF as usize)) as *mut u32, 1 << 1); }

        Some(Self {
            mmio: mmio_base, op_off, rt_off, db_off, n_ports, max_slots, ppc,
            dcbaa_phys: dcbaa_p, dcbaa,
            slots: Vec::new(),
            cmd_ring: cmd, ev_ring: ev, erst_phys: erst_p,
            ports_done: 0, devices: Vec::new(),
            ctx: ctx as *const dyn DriverContext,
            n_slots_used: 0,
        })
    }

    fn op(&self, off: u32) -> *mut u32 {
        unsafe { (self.mmio.add(self.op_off as usize).add(off as usize)) as *mut u32 }
    }

    fn rt(&self) -> *mut u32 {
        unsafe { (self.mmio.add(self.rt_off as usize)) as *mut u32 }
    }

    fn doorbell(&self, slot: u32, db_target: u32) {
        let db = unsafe { (self.mmio.add(self.db_off as usize)) as *mut u32 };
        unsafe { core::ptr::write_volatile(db.add(slot as usize), db_target); }
    }

    /// Wait for and process one event. Returns the event TRB flags.
    fn wait_event(&mut self, timeout: u32) -> Result<u32, &'static str> {
        for _ in 0..timeout {
            if let Some(ev) = self.ev_ring.pop() {
                // Update ERSDP to acknowledge event consumption
                let deq = self.ev_ring.dequeue_ptr();
                unsafe { core::ptr::write_volatile((self.rt().add(ERSDP_OFF as usize)) as *mut u64, deq); }
                return Ok(ev.flags);
            }
            if timeout > 1000 { crate::port::PortWriter::new(0x80).write_safe(0u8); }
        }
        Err("event timeout")
    }

    /// Ring the command doorbell and wait for a command completion event.
    fn send_cmd(&mut self, trb: Trb) -> Result<u32, &'static str> {
        self.cmd_ring.enqueue(trb);
        self.doorbell(0, 0);
        self.wait_event(5_000_000)
    }

    /// Allocate a device slot.
    pub fn enable_slot(&mut self) -> Result<u32, &'static str> {
        let trb = Trb::new(TRB_ENABLE_SLOT, self.cmd_ring.cycle);
        self.send_cmd(trb)?;
        let slot_id = self.n_slots_used + 1;
        self.n_slots_used += 1;

        // Allocate device context (4KB page, 64-byte aligned)
        let dev_ctx_p = unsafe { (*self.ctx).allocate_contiguous_frames(1) }.map_err(|_| "no ctx page")?;
        let in_ctx_p = unsafe { (*self.ctx).allocate_contiguous_frames(1) }.map_err(|_| "no input ctx")?;
        let dev_ctx_v = unsafe { (*self.ctx).phys_to_virt(dev_ctx_p) as *mut DevCtx };
        let in_ctx_v = unsafe { (*self.ctx).phys_to_virt(in_ctx_p) as *mut InputCtx };

        // Zero them
        unsafe {
            core::ptr::write_bytes(dev_ctx_v, 0, 1);
            core::ptr::write_bytes(in_ctx_v, 0, 1);
        }

        // Set DCBAA entry
        self.dcbaa[slot_id as usize] = dev_ctx_p;

        let dev_ctx = unsafe { &mut *dev_ctx_v };
        let in_ctx = unsafe { &mut *in_ctx_v };

        // Allocate EP0 transfer ring
        let ctx_ref = unsafe { &*self.ctx };
        let ep0_ring = Ring::alloc(ctx_ref, 64).ok_or("no ep0 ring")?;

        // Store slot state
        self.slots.push(SlotState {
            slot_id, dev_addr: 0,
            ep0_ring,
            bulk_out_ring: None,
            bulk_in_ring: None,
            dev_ctx_phys: dev_ctx_p,
            in_ctx_phys: in_ctx_p,
        });

        Ok(slot_id)
    }

    /// Address a device (after enable_slot). Sets up EP0 context in the input context.
    pub fn address_device(&mut self, slot_id: u32) -> Result<(), &'static str> {
        let dev_addr = slot_id as u8;

        // Get in_ctx_phys and ep0_ring_phys from slot before mutable borrow
        let (in_ctx_phys, ep0_ring_phys) = {
            let slot = self.slots.iter().find(|s| s.slot_id == slot_id).ok_or("bad slot")?;
            (slot.in_ctx_phys, slot.ep0_ring.phys)
        };

        // Set up Input Context
        let in_ctx_v = unsafe { (*self.ctx).phys_to_virt(in_ctx_phys) as *mut InputCtx };
        let in_ctx = unsafe { &mut *in_ctx_v };
        in_ctx.add = 3;
        in_ctx.slot[0] = 0;
        in_ctx.slot[1] = (dev_addr as u32) << 24;
        in_ctx.ep0[0] = (64 << 16) | (4 << 3); // mps=64, type=control
        in_ctx.ep0[1] = ep0_ring_phys as u32;
        in_ctx.ep0[2] = (ep0_ring_phys >> 32) as u32;

        let mut trb = Trb::new(TRB_ADDRESS_DEVICE, self.cmd_ring.cycle);
        trb.params[..4].copy_from_slice(&in_ctx_phys.to_le_bytes());
        trb.params[6] = slot_id as u8;
        trb.flags |= TRB_IOC;
        self.send_cmd(trb)?;

        // Update slot state
        if let Some(s) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) {
            s.dev_addr = dev_addr;
        }
        for dev in self.devices.iter_mut() {
            if dev.address == 0 { dev.address = dev_addr; break; }
        }
        Ok(())
    }

    /// Configure a bulk endpoint.
    pub fn configure_endpoint_bulk(&mut self, slot_id: u32, ep_addr: u8, mps: u16) -> Result<(), &'static str> {
        let slot = self.slots.iter_mut().find(|s| s.slot_id == slot_id).ok_or("bad slot")?;
        let ep_num = (ep_addr & 0x0F) as usize;
        let is_in = (ep_addr & 0x80) != 0;

        let in_ctx_v = unsafe { (*self.ctx).phys_to_virt(slot.in_ctx_phys) as *mut InputCtx };
        let in_ctx = unsafe { &mut *in_ctx_v };
        in_ctx.add = 0;
        in_ctx.drop = 0;

        // Allocate transfer ring for this bulk endpoint
        let ctx_ref = unsafe { &*self.ctx };
        let bulk_ring = Ring::alloc(ctx_ref, 64).ok_or("no ring")?;
        let b_phys = bulk_ring.phys;

        // Set context array index: for EP1 out = 2, EP1 in = 3, EP2 out = 4, etc.
        let ctx_idx = 2 + ep_num * 2 + if is_in { 1 } else { 0 };
        in_ctx.add = 1 << ctx_idx;

        // Endpoint context: type=2 (bulk), max packet size, TR dequeue
        // ep context is 4 dwords; we need to extend InputCtx or use raw access

        // Write endpoint context at the proper index in the input context page
        let ep_type = 2u32; // Bulk
        unsafe {
            let base = in_ctx_v as *mut u32;
            let ep_base = base.add(ctx_idx * 4);
            core::ptr::write_volatile(ep_base, (mps as u32) << 16 | ep_type << 3);
            core::ptr::write_volatile(ep_base.add(1), b_phys as u32);
            core::ptr::write_volatile(ep_base.add(2), (b_phys >> 32) as u32);
            core::ptr::write_volatile(ep_base.add(3), 0);
        }

        // Store the ring (use raw pointer to work around borrow)
        if is_in {
            slot.bulk_in_ring = Some(bulk_ring);
        } else {
            slot.bulk_out_ring = Some(bulk_ring);
        }

        // Build Configure Endpoint TRB
        let mut trb = Trb::new(TRB_CONFIGURE_ENDPOINT, self.cmd_ring.cycle);
        trb.params[..4].copy_from_slice(&slot.in_ctx_phys.to_le_bytes());
        trb.params[6] = slot_id as u8;
        trb.flags |= TRB_IOC;
        self.send_cmd(trb)?;
        Ok(())
    }

    // ── Port management ───────────────────────────────────────

    /// Called after start. Returns number of newly detected devices.
    pub fn poll_ports(&mut self) {
        for port in 0..self.n_ports {
            if self.ports_done & (1 << port) != 0 { continue; }

            // Ensure port is powered (some controllers lose PP after HCRST)
            let mut portsc = unsafe { core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10)) };
            if portsc & PORTSC_PP == 0 {
                unsafe { core::ptr::write_volatile(self.op(PORTSC_BASE + port * 0x10), portsc | PORTSC_PP); }
                // Wait 20ms for power to stabilize
                for _ in 0..20_000 { crate::port::PortWriter::new(0x80).write_safe(0u8); }
                portsc = unsafe { core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10)) };
            }

            let portsc = unsafe { core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10)) };

            // No device connected → will retry on next poll
            if portsc & PORTSC_CCS == 0 { continue; }

            // Device connected but port not yet enabled → reset
            if portsc & PORTSC_PED == 0 {
                unsafe { core::ptr::write_volatile(self.op(PORTSC_BASE + port * 0x10), portsc | PORTSC_PR); }
                for _ in 0..200_000 { crate::port::PortWriter::new(0x80).write_safe(0u8); }
                unsafe {
                    let v = core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10));
                    core::ptr::write_volatile(self.op(PORTSC_BASE + port * 0x10), v & !PORTSC_PR);
                }
                for _ in 0..200_000 {
                    if unsafe { core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10)) } & PORTSC_PED != 0 { break; }
                }
                if unsafe { core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10)) } & PORTSC_CCS == 0 {
                    continue;
                }
            }

            let ps = unsafe { core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10)) };
            let speed_val = (ps >> 10) & 0xF;
            let usb_speed = port_speed_to_usb(speed_val);

            self.devices.push(UsbDevice {
                address: 0, speed: usb_speed, max_packet_size_0: 64,
                vendor_id: 0, product_id: 0, device_class: 0,
                device_subclass: 0, device_protocol: 0, configurations: 0,
                endpoints: Vec::new(),
            });
            self.ports_done |= 1 << port;
        }
    }

    pub fn devices(&self) -> &[UsbDevice] { &self.devices }
    pub fn devices_mut(&mut self) -> &mut [UsbDevice] { &mut self.devices }
    pub fn n_ports(&self) -> u32 { self.n_ports }
    pub fn read_cap(&self, offset: u32) -> u32 {
        unsafe { core::ptr::read_volatile((self.mmio.add(offset as usize)) as *const u32) }
    }
    pub fn read_portsc(&self, port: u32) -> u32 {
        if port >= self.n_ports { return 0xFFFF; }
        unsafe { core::ptr::read_volatile(self.op(PORTSC_BASE + port * 0x10)) }
    }
    pub fn slot_id_for_device(&self, dev_idx: usize) -> Option<u32> {
        self.slots.get(dev_idx).map(|s| s.slot_id)
    }

    // ── Control transfer ──────────────────────────────────────

    pub fn control_transfer(
        &mut self,
        slot_id: u32,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        let is_in = (setup.bm_request_type & 0x80) != 0;
        let data_len = setup.w_length as usize;

        // Allocate dedicated staging buffer for data phase.
        // Control transfers use at most one page (w_length ≤ 4096 typically).
        let staging_phys = if data_len > 0 {
            unsafe { (*self.ctx).allocate_contiguous_frames((data_len + 4095) / 4096) }
                .map_err(|_| "no staging memory")?
        } else {
            0
        };
        let staging_virt = if staging_phys != 0 {
            unsafe { (*self.ctx).phys_to_virt(staging_phys) as *mut u8 }
        } else {
            core::ptr::null_mut()
        };

        // Copy OUT data to staging buffer
        if data_len > 0 && !is_in {
            unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, data_len); }
        }

        let ep0_cycle = {
            let slot = self.slots.iter().find(|s| s.slot_id == slot_id).ok_or("bad slot")?;
            slot.ep0_ring.cycle
        };

        // Setup TRB
        let setup_bytes = unsafe { core::slice::from_raw_parts(setup as *const UsbSetupPacket as *const u8, 8) };
        let mut s_trb = Trb::new(TRB_SETUP, ep0_cycle);
        s_trb.params[..8].copy_from_slice(setup_bytes);
        let trt = if data_len == 0 { 0u32 } else if is_in { 2 << 16 } else { 3 << 16 };
        s_trb.flags |= TRB_CHAIN | trt;

        // Enqueue on slot's ep0 ring
        if let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) {
            slot.ep0_ring.enqueue(s_trb);

            if data_len > 0 {
                let mut d_trb = Trb::new(TRB_DATA, slot.ep0_ring.cycle);
                d_trb.params[..4].copy_from_slice(&(staging_phys as u32).to_le_bytes());
                d_trb.params[4..8].copy_from_slice(&(staging_phys >> 32).to_le_bytes());
                d_trb.status = (data_len as u32) & 0x1FFFF;
                if is_in { d_trb.flags |= TRB_DIR_IN | TRB_CHAIN; } else { d_trb.flags |= TRB_CHAIN; }
                slot.ep0_ring.enqueue(d_trb);
            }

            let mut st_trb = Trb::new(TRB_STATUS, slot.ep0_ring.cycle);
            st_trb.flags |= if is_in { 0 } else { TRB_DIR_IN } | TRB_IOC;
            slot.ep0_ring.enqueue(st_trb);
        } else {
            return Err("bad slot");
        }

        self.doorbell(slot_id, 0);
        self.wait_event(5_000_000)?;

        // Copy IN data from staging buffer back to caller
        if is_in && data_len > 0 {
            unsafe { core::ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), data_len); }
        }
        Ok(data_len)
    }

    // ── Bulk transfer ─────────────────────────────────────────

    pub fn bulk_transfer(
        &mut self,
        slot_id: u32,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        _mps: u16,
    ) -> Result<usize, &'static str> {
        let len = buf.len().min(65536);

        // Allocate a dedicated contiguous-physical staging buffer
        let staging_pages = (len + 4095) / 4096;
        let staging_phys = unsafe { (*self.ctx).allocate_contiguous_frames(staging_pages) }
            .map_err(|_| "no staging memory")?;
        let staging_virt = unsafe { (*self.ctx).phys_to_virt(staging_phys) as *mut u8 };

        // Copy OUT data to staging
        if dir == UsbDirection::Out {
            unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, len); }
        }

        // Get ring cycle before mutable borrow
        let ring_cycle = {
            let slot = self.slots.iter().find(|s| s.slot_id == slot_id).ok_or("bad slot")?;
            let ring = match dir {
                UsbDirection::In => slot.bulk_in_ring.as_ref().ok_or("no bulk in ring")?,
                UsbDirection::Out => slot.bulk_out_ring.as_ref().ok_or("no bulk out ring")?,
            };
            ring.cycle
        };

        // Enqueue TRB on the ring (re-borrow slot mutably)
        if let Some(slot) = self.slots.iter_mut().find(|s| s.slot_id == slot_id) {
            let ring = match dir {
                UsbDirection::In => slot.bulk_in_ring.as_mut().ok_or("no bulk in ring")?,
                UsbDirection::Out => slot.bulk_out_ring.as_mut().ok_or("no bulk out ring")?,
            };
            let mut trb = Trb::new(TRB_NORMAL, ring.cycle);
            trb.params[..4].copy_from_slice(&(staging_phys as u32).to_le_bytes());
            trb.params[4..8].copy_from_slice(&(staging_phys >> 32).to_le_bytes());
            trb.status = (len as u32) & 0x1FFFF;
            if dir == UsbDirection::In { trb.flags |= TRB_DIR_IN; }
            trb.flags |= TRB_IOC | TRB_ENT;
            ring.enqueue(trb);
        } else {
            return Err("bad slot");
        }
        let ep_num = (endpoint & 0x0F) as u32;
        let is_in = (endpoint & 0x80) != 0;
        let dci = ep_num * 2 + if is_in { 1 } else { 0 };
        self.doorbell(slot_id, dci);
        self.wait_event(5_000_000)?;

        if dir == UsbDirection::In {
            unsafe { core::ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), len); }
        }
        Ok(len)
    }
}
