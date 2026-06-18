//! EHCI (USB 2.0) Host Controller Driver with working async schedule.
//!
//! Provides:
//! - Controller reset, start, stop
//! - Port status detection and reset
//! - Async schedule management (qH list insert/remove)
//! - Control transfers (SETUP → DATA → STATUS via qTD chain)
//! - Bulk transfers (single qTD)
//! - Completion polling with timeout

use crate::usb::{UsbDevice, UsbSpeed, UsbSetupPacket, UsbDirection};
use crate::DriverContext;
use alloc::vec::Vec;

// ── Operational register offsets ─────────────────────────────
const USBCMD: u32 = 0x00;
const USBSTS: u32 = 0x04;
const USBINTR: u32 = 0x08;
const FRINDEX: u32 = 0x0C;
const CTRLDSSEGMENT: u32 = 0x10;
const PERIODICLISTBASE: u32 = 0x14;
const ASYNCLISTADDR: u32 = 0x18;
const PORTSC_BASE: u32 = 0x44;

// ── USBCMD bits ──────────────────────────────────────────────
const CMD_RUN: u32 = 1 << 0;
const CMD_HCRESET: u32 = 1 << 1;
const CMD_ASSE: u32 = 1 << 5;
const CMD_PSE: u32 = 1 << 4;

// ── USBSTS bits ──────────────────────────────────────────────
const STS_HCHALTED: u32 = 1 << 0;
const STS_PCD: u32 = 1 << 2;

// ── PORTSC bits ──────────────────────────────────────────────
const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PE: u32 = 1 << 2;
const PORTSC_RESET: u32 = 1 << 8;

// ── qH constants ─────────────────────────────────────────────
const QH_HORZ_TERMINATE: u32 = 0x01;
const QH_HORZ_TYPE_QH: u32 = 0x02; // bit 1 = 1 → qH

// qH endpoint characteristics fields
const fn qh_ep_address(addr: u8) -> u32 { addr as u32 }
const fn qh_ep_endpoint(ep: u8) -> u32 { (ep as u32) << 8 }
const fn qh_ep_eps(speed: UsbSpeed) -> u32 {
    match speed {
        UsbSpeed::Full => 0 << 12,
        UsbSpeed::Low => 1 << 12,
        UsbSpeed::High => 2 << 12,
    }
}
const QH_EP_DTC: u32 = 1 << 14;   // doorbell / toggle control
const QH_EP_RL: u32 = 8 << 23;    // nak reload count

// qH endpoint capabilities
const fn qh_cap_max_packet(mps: u16) -> u32 { mps as u32 }

// qTD token fields
const QTD_ACTIVE: u32 = 1 << 7;
const QTD_HALTED: u32 = 1 << 6;
const QTD_PID_OUT: u32 = 0 << 8;
const QTD_PID_IN: u32 = 1 << 8;
const QTD_PID_SETUP: u32 = 2 << 8;
const QTD_CERR: u32 = 3 << 10; // 3 error counts
const QTD_IOC: u32 = 1 << 15;

const fn qtd_total_bytes(n: u32) -> u32 {
    if n == 0 { 0x8000 } else { (n << 16) & 0x7FFF_0000 } // bit 31 = 0 if n>0
}

const QTD_TERMINATE: u32 = 0x01;

// ── Queue Head (32-byte aligned, 48 bytes) ───────────────────
#[repr(C, align(32))]
pub struct QueueHead {
    pub horz_link: u32,
    pub ep_chars: u32,
    pub ep_caps: u32,
    pub current_qtd: u32,
    // Overlay area (9 dwords = 36 bytes)
    pub next_qtd: u32,
    pub alt_next_qtd: u32,
    pub token: u32,
    pub buf0: u32,
    pub buf1: u32,
    pub buf2: u32,
    pub buf3: u32,
    pub buf4: u32,
}

// ── Queue Element Descriptor (32-byte aligned, 32 bytes) ─────
#[repr(C, align(32))]
pub struct Qtd {
    pub next_qtd: u32,
    pub alt_next_qtd: u32,
    pub token: u32,
    pub buf0: u32,
    pub buf1: u32,
    pub buf2: u32,
    pub buf3: u32,
    pub buf4: u32,
}

// ── EHCI Controller ──────────────────────────────────────────

// SAFETY: EHCI is used only on the main kernel thread (single-threaded kernel).
unsafe impl Send for EhciController {}

pub struct EhciController {
    mmio_base: *mut u8,
    op_offset: u32,
    n_ports: u32,
    /// Physical+virtual address of the async list head qH.
    async_head_phys: u64,
    async_head: &'static mut QueueHead,
    /// A pool of qTDs for transfers (pre-allocated from a page).
    qtd_pool_phys: u64,
    qtd_pool: &'static mut [Qtd],
    qtd_pool_used: usize,
    /// Allocated qH entries for endpoints (from a second page).
    qh_pool_phys: u64,
    qh_pool: &'static mut [QueueHead],
    qh_pool_used: usize,
    devices: Vec<UsbDevice>,
    ctx: *const dyn DriverContext,
    next_address: u8,
}

impl EhciController {
    /// Create and initialize an EHCI controller.
    ///
    /// `mmio_base` must be a valid virtual mapping of the EHCI capability
    /// registers (BAR0 from PCI config space).
    pub fn new(mmio_base: *mut u8, ctx: &'static dyn DriverContext) -> Option<Self> {
        let caplength = unsafe { core::ptr::read_volatile(mmio_base as *const u8) } as u32;
        let hcsparams = unsafe { core::ptr::read_volatile(mmio_base.add(4) as *const u32) };
        let n_ports = ((hcsparams >> 24) & 0x0F).max(1);
        let op_offset = caplength;

        // Reset the controller
        let op_base = unsafe { mmio_base.add(op_offset as usize) };
        unsafe {
            core::ptr::write_volatile((op_base.add(USBCMD as usize)) as *mut u32, CMD_HCRESET);
        }
        let mut wait = 0u32;
        while wait < 250_000 {
            let sts = unsafe { core::ptr::read_volatile((op_base.add(USBSTS as usize)) as *const u32) };
            if sts & CMD_HCRESET == 0 { break; }
            wait += 1;
        }
        if wait >= 250_000 { return None; }

        // Allocate a page for the async head qH
        let head_phys = ctx.allocate_frame().ok()?;
        let head_virt = ctx.phys_to_virt(head_phys) as *mut QueueHead;

        // Allocate a page for qTD pool (4096 / 32 = 128 qTDs)
        let qtd_pool_phys = ctx.allocate_frame().ok()?;
        let qtd_pool_virt = ctx.phys_to_virt(qtd_pool_phys) as *mut Qtd;

        // Allocate a page for qH pool (4096 / 48 = 85 qHs)
        let qh_pool_phys = ctx.allocate_frame().ok()?;
        let qh_pool_virt = ctx.phys_to_virt(qh_pool_phys) as *mut QueueHead;

        // Initialize async head qH (circular list, points to itself when idle)
        let async_head = unsafe { &mut *head_virt };
        unsafe {
            core::ptr::write_volatile(&mut async_head.horz_link,
                (head_phys as u32) | QH_HORZ_TYPE_QH); // self-loop (idle)
            core::ptr::write_volatile(&mut async_head.ep_chars, 0);
            core::ptr::write_volatile(&mut async_head.ep_caps, 0);
            core::ptr::write_volatile(&mut async_head.current_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut async_head.next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut async_head.alt_next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut async_head.token, 0);
        }

        // Zero out qTD pool
        let qtd_slice = unsafe {
            core::slice::from_raw_parts_mut(qtd_pool_virt, 128)
        };
        for q in qtd_slice.iter_mut() {
            unsafe {
                core::ptr::write_volatile(&mut q.next_qtd, 0);
                core::ptr::write_volatile(&mut q.alt_next_qtd, 0);
                core::ptr::write_volatile(&mut q.token, 0);
                core::ptr::write_volatile(&mut q.buf0, 0);
                core::ptr::write_volatile(&mut q.buf1, 0);
                core::ptr::write_volatile(&mut q.buf2, 0);
                core::ptr::write_volatile(&mut q.buf3, 0);
                core::ptr::write_volatile(&mut q.buf4, 0);
            }
        }

        let qh_slice = unsafe {
            core::slice::from_raw_parts_mut(qh_pool_virt, 85)
        };
        for q in qh_slice.iter_mut() {
            unsafe {
                core::ptr::write_volatile(&mut q.horz_link, 0);
                core::ptr::write_volatile(&mut q.ep_chars, 0);
                core::ptr::write_volatile(&mut q.ep_caps, 0);
                core::ptr::write_volatile(&mut q.current_qtd, QTD_TERMINATE);
                core::ptr::write_volatile(&mut q.next_qtd, QTD_TERMINATE);
                core::ptr::write_volatile(&mut q.alt_next_qtd, QTD_TERMINATE);
                core::ptr::write_volatile(&mut q.token, 0);
            }
        }

        Some(Self {
            mmio_base,
            op_offset,
            n_ports,
            async_head_phys: head_phys,
            async_head,
            qtd_pool_phys,
            qtd_pool: qtd_slice,
            qtd_pool_used: 0,
            qh_pool_phys: qh_pool_phys,
            qh_pool: qh_slice,
            qh_pool_used: 0,
            devices: Vec::new(),
            ctx,
            next_address: 1,
        })
    }

    /// Start the host controller and enable async schedule.
    pub fn start(&mut self) {
        let op_base = unsafe { self.mmio_base.add(self.op_offset as usize) };
        unsafe {
            core::ptr::write_volatile(
                (op_base.add(ASYNCLISTADDR as usize)) as *mut u32,
                self.async_head_phys as u32,
            );
        }
        unsafe {
            let cmd = core::ptr::read_volatile((op_base.add(USBCMD as usize)) as *const u32);
            core::ptr::write_volatile(
                (op_base.add(USBCMD as usize)) as *mut u32,
                cmd | CMD_RUN | CMD_ASSE,
            );
        }
        // Wait for HCHalted to clear
        for _ in 0..100_000 {
            let sts = unsafe { core::ptr::read_volatile((op_base.add(USBSTS as usize)) as *const u32) };
            if sts & STS_HCHALTED == 0 { break; }
        }
    }

    fn alloc_qtd(&mut self) -> Option<(&'static mut Qtd, u64)> {
        if self.qtd_pool_used >= 128 { return None; }
        let idx = self.qtd_pool_used;
        self.qtd_pool_used += 1;
        let phys = self.qtd_pool_phys + (idx as u64) * 32;
        let ptr = &mut self.qtd_pool[idx] as *mut Qtd;
        Some(unsafe { (&mut *ptr, phys) })
    }

    fn alloc_qh(&mut self) -> Option<(&'static mut QueueHead, u64)> {
        if self.qh_pool_used >= 85 { return None; }
        let idx = self.qh_pool_used;
        self.qh_pool_used += 1;
        let phys = self.qh_pool_phys + (idx as u64) * 48;
        let ptr = &mut self.qh_pool[idx] as *mut QueueHead;
        Some(unsafe { (&mut *ptr, phys) })
    }

    /// Insert a qH into the async list (after the head).
    fn insert_qh(&mut self, qh_phys: u64) {
        // The list is circular: head → qH1 → qH2 → ... → head
        // Insert after head: head → new → head's_old_next
        let head_next = unsafe { core::ptr::read_volatile(&self.async_head.horz_link) };
        unsafe {
            core::ptr::write_volatile(&mut self.async_head.horz_link,
                (qh_phys as u32) | QH_HORZ_TYPE_QH);
        }
        // Find the last qH before the head loop-back and set its horz_link to new
        // Actually, simpler: set new's horz_link to head's old next
        unsafe {
            core::ptr::write_volatile(
                &mut (*((qh_phys as usize) as *mut QueueHead)).horz_link,
                head_next,
            );
        }
    }

    /// Remove a qH from the async list.
    fn remove_qh(&mut self, qh_phys: u64) {
        // Walk the list from head to find the one pointing to 'qh_phys'
        let mut prev = self.async_head_phys;
        loop {
            let prev_qh = unsafe { &*(prev as usize as *const QueueHead) };
            let next_link = unsafe { core::ptr::read_volatile(&prev_qh.horz_link) };
            let next_phys = next_link & !0x1F; // strip type bits, keep alignment
            if next_phys == qh_phys as u32 {
                // Found it. Point prev to qh's next.
                let qh = unsafe { &*((qh_phys as usize) as *const QueueHead) };
                let qh_next = unsafe { core::ptr::read_volatile(&qh.horz_link) };
                unsafe {
                    core::ptr::write_volatile(
                        &mut (*((prev as usize) as *mut QueueHead)).horz_link,
                        qh_next,
                    );
                }
                return;
            }
            if next_phys == self.async_head_phys as u32 { break; } // back to head → not found
            prev = next_phys as u64;
        }
    }

    /// Wait for a qTD to complete (active bit cleared).
    /// Returns Ok(()) on success, Err on timeout or error.
    fn wait_qtd(&self, qtd: &Qtd, timeout_us: u32) -> Result<(), &'static str> {
        let mut wait = 0u32;
        while wait < timeout_us {
            let token = unsafe { core::ptr::read_volatile(&qtd.token) };
            if token & QTD_ACTIVE == 0 {
                if token & QTD_HALTED != 0 {
                    return Err("qTD halted");
                }
                return Ok(());
            }
            // Small delay (~1us per iteration with port I/O)
            if wait & 0xFF == 0 {
                crate::port::PortWriter::new(0x80).write_safe(0u8);
            }
            wait += 1;
        }
        Err("qTD timeout")
    }

    // ── Control Transfer ─────────────────────────────────────

    /// Perform a USB control transfer.
    ///
    /// Creates a SETUP qTD, optional DATA qTD, and a STATUS qTD,
    /// links them, inserts into the async schedule, waits for completion.
    pub fn control_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        let is_in = (setup.bm_request_type & 0x80) != 0;
        let data_len = setup.w_length as usize;

        // Allocate qH for this endpoint
        let (qh, qh_phys) = self.alloc_qh().ok_or("no qH")?;

        // Setup qH for control endpoint
        let speed = UsbSpeed::High; // EHCI only handles HS
        unsafe {
            core::ptr::write_volatile(&mut qh.ep_chars,
                qh_ep_address(dev_addr)
                | qh_ep_endpoint(endpoint)
                | qh_ep_eps(speed)
                | QH_EP_DTC
                | QH_EP_RL);
            core::ptr::write_volatile(&mut qh.ep_caps,
                qh_cap_max_packet(64)); // HS default MPS
            core::ptr::write_volatile(&mut qh.current_qtd, QTD_TERMINATE);
        }

        // Allocate qTDs
        let (qtd_setup, qtd_setup_phys) = self.alloc_qtd().ok_or("no qTD")?;
        let mut qtd_data: Option<(&mut Qtd, u64)> = if data_len > 0 {
            Some(self.alloc_qtd().ok_or("no qTD")?)
        } else {
            None
        };
        let (qtd_status, qtd_status_phys) = self.alloc_qtd().ok_or("no qTD")?;

        // Build SETUP qTD (8 bytes)
        let setup_bytes = unsafe {
            core::slice::from_raw_parts(
                setup as *const UsbSetupPacket as *const u8,
                8,
            )
        };
        // Setup data must be in a physical buffer. Use the first 8 bytes of
        // the qTD's buffer space itself (or a small staging area).
        // We need the physical address of the setup data.
        // Since we're in a single-address-space environment, we can use
        // the virtual address directly by converting to physical via ctx.
        // For simplicity, we copy to a known page.
        let setup_page_phys = self.qtd_pool_phys + 128 * 32; // use one past the pool
        let setup_page_virt = unsafe { (*self.ctx).phys_to_virt(setup_page_phys) } as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(setup_bytes.as_ptr(), setup_page_virt, 8);
        }

        let next_after_setup = qtd_data.as_ref().map(|d| d.1 as u32).unwrap_or(qtd_status_phys as u32);
        unsafe {
            core::ptr::write_volatile(&mut qtd_setup.next_qtd, next_after_setup);
            core::ptr::write_volatile(&mut qtd_setup.alt_next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_setup.token,
                QTD_ACTIVE | QTD_PID_SETUP | QTD_CERR | qtd_total_bytes(8));
            core::ptr::write_volatile(&mut qtd_setup.buf0, setup_page_phys as u32);
            core::ptr::write_volatile(&mut qtd_setup.buf1, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_setup.buf2, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_setup.buf3, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_setup.buf4, QTD_TERMINATE);
        }

        // Build DATA qTD (if any)
        if data_len > 0 {
            let (data_qh_ptr, _) = qtd_data.as_mut().unwrap();
            if !is_in {
                unsafe {
                    core::ptr::copy_nonoverlapping(buf.as_ptr(), setup_page_virt, data_len.min(4096));
                }
            }
            let pid = if is_in { QTD_PID_IN } else { QTD_PID_OUT };
            unsafe {
                core::ptr::write_volatile(&mut data_qh_ptr.next_qtd, qtd_status_phys as u32);
                core::ptr::write_volatile(&mut data_qh_ptr.alt_next_qtd, QTD_TERMINATE);
                core::ptr::write_volatile(&mut data_qh_ptr.token,
                    QTD_ACTIVE | pid | QTD_CERR | qtd_total_bytes(data_len as u32));
                core::ptr::write_volatile(&mut data_qh_ptr.buf0, setup_page_phys as u32);
                core::ptr::write_volatile(&mut data_qh_ptr.buf1, QTD_TERMINATE);
                core::ptr::write_volatile(&mut data_qh_ptr.buf2, QTD_TERMINATE);
                core::ptr::write_volatile(&mut data_qh_ptr.buf3, QTD_TERMINATE);
                core::ptr::write_volatile(&mut data_qh_ptr.buf4, QTD_TERMINATE);
            }
        }

        // Build STATUS qTD (0 bytes, opposite direction)
        let status_pid = if is_in || data_len == 0 { QTD_PID_OUT } else { QTD_PID_IN };
        unsafe {
            core::ptr::write_volatile(&mut qtd_status.next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_status.alt_next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_status.token,
                QTD_ACTIVE | status_pid | QTD_CERR | qtd_total_bytes(0));
            core::ptr::write_volatile(&mut qtd_status.buf0, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_status.buf1, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_status.buf2, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_status.buf3, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd_status.buf4, QTD_TERMINATE);
        }

        // Link qH to first qTD
        unsafe {
            core::ptr::write_volatile(&mut qh.next_qtd, qtd_setup_phys as u32);
            core::ptr::write_volatile(&mut qh.alt_next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qh.token,
                QTD_ACTIVE | QTD_PID_SETUP | QTD_CERR);
        }

        // Insert qH into async schedule
        self.insert_qh(qh_phys);

        // Wait for completion (poll all 3 qTDs)
        let mut timeout = 5_000_000u32; // 5 seconds
        let result = self.wait_qtd(&qtd_setup, timeout);
        if result.is_err() { self.remove_qh(qh_phys); return result.map(|_| 0); }

        if data_len > 0 {
            let (data_qh_ptr, _) = qtd_data.as_ref().unwrap();
            let r = self.wait_qtd(data_qh_ptr, timeout);
            if r.is_err() { self.remove_qh(qh_phys); return r.map(|_| 0); }

            if is_in {
                unsafe {
                    let n = data_len.min(4096);
                    core::ptr::copy_nonoverlapping(setup_page_virt, buf.as_mut_ptr(), n);
                }
            }
        }

        let r = self.wait_qtd(&qtd_status, timeout);
        if r.is_err() { self.remove_qh(qh_phys); return r.map(|_| 0); }

        // Remove qH from async schedule
        self.remove_qh(qh_phys);
        Ok(data_len)
    }

    // ── Bulk Transfer ────────────────────────────────────────

    /// Perform a USB bulk transfer (single qTD, up to 20480 bytes).
    pub fn bulk_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        max_packet: u16,
    ) -> Result<usize, &'static str> {
        let len = buf.len().min(20480);

        // Allocate qH
        let (qh, qh_phys) = self.alloc_qh().ok_or("no qH")?;
        unsafe {
            core::ptr::write_volatile(&mut qh.ep_chars,
                qh_ep_address(dev_addr)
                | qh_ep_endpoint(endpoint & 0x0F)
                | qh_ep_eps(UsbSpeed::High)
                | QH_EP_DTC
                | QH_EP_RL);
            core::ptr::write_volatile(&mut qh.ep_caps, qh_cap_max_packet(max_packet));
            core::ptr::write_volatile(&mut qh.current_qtd, QTD_TERMINATE);
        }

        // Allocate qTD
        let (qtd, qtd_phys) = self.alloc_qtd().ok_or("no qTD")?;

        // For OUT, copy data to staging. For IN, read back after.
        let staging_phys = self.qtd_pool_phys + 128 * 32;
        let staging_virt = unsafe { (*self.ctx).phys_to_virt(staging_phys) } as *mut u8;
        if dir == UsbDirection::Out {
            unsafe {
                core::ptr::copy_nonoverlapping(buf.as_ptr(), staging_virt, len);
            }
        }

        let pid = match dir {
            UsbDirection::In => QTD_PID_IN,
            UsbDirection::Out => QTD_PID_OUT,
        };

        unsafe {
            core::ptr::write_volatile(&mut qtd.next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd.alt_next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd.token,
                QTD_ACTIVE | pid | QTD_CERR | qtd_total_bytes(len as u32));
            core::ptr::write_volatile(&mut qtd.buf0, staging_phys as u32);
            core::ptr::write_volatile(&mut qtd.buf1, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd.buf2, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd.buf3, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qtd.buf4, QTD_TERMINATE);
        }

        unsafe {
            core::ptr::write_volatile(&mut qh.next_qtd, qtd_phys as u32);
            core::ptr::write_volatile(&mut qh.alt_next_qtd, QTD_TERMINATE);
            core::ptr::write_volatile(&mut qh.token, QTD_ACTIVE | pid | QTD_CERR);
        }

        self.insert_qh(qh_phys);
        let r = self.wait_qtd(qtd, 5_000_000);
        if r.is_err() { self.remove_qh(qh_phys); return r.map(|_| 0); }

        // For IN, copy data back
        if dir == UsbDirection::In {
            unsafe {
                core::ptr::copy_nonoverlapping(staging_virt, buf.as_mut_ptr(), len);
            }
        }

        self.remove_qh(qh_phys);
        Ok(len)
    }

    // ── Port management ──────────────────────────────────────

    /// Poll all ports for newly connected devices.
    ///
    /// Handles two cases:
    /// - **Firmware-enabled port** (UEFI booted from USB): `PORTSC_PE` is already set.
    ///   We register the device without resetting.
    /// - **Hotplug**: `PORTSC_CCS` is set but `PORTSC_PE` is clear.
    ///   We reset the port, enable it, then register.
    pub fn poll_ports(&mut self) {
        let op_base = unsafe { self.mmio_base.add(self.op_offset as usize) };
        for port in 0..self.n_ports {
            let paddr = (op_base as usize + PORTSC_BASE as usize + (port * 4) as usize) as *mut u32;
            let portsc = unsafe { core::ptr::read_volatile(paddr) };
            if portsc & PORTSC_CCS == 0 { continue; }

            // Check if this port was already processed
            let already_known = self.devices.iter().any(|d| {
                d.vendor_id == 0 && d.speed == UsbSpeed::High
            });
            if already_known { continue; }

            if portsc & PORTSC_PE == 0 {
                // Hotplug: reset the port
                unsafe { core::ptr::write_volatile(paddr, portsc | PORTSC_RESET); }
                for _ in 0..50_000 { let _ = unsafe { core::ptr::read_volatile(paddr) }; }
                unsafe { core::ptr::write_volatile(paddr, portsc & !PORTSC_RESET); }
                for _ in 0..10_000 {
                    if unsafe { core::ptr::read_volatile(paddr) } & PORTSC_PE != 0 { break; }
                }
            }

            let speed = UsbSpeed::from_portsc(unsafe { core::ptr::read_volatile(paddr) });
            if speed != UsbSpeed::High { continue; }

            // Register device (address will be assigned during enumeration)
            self.devices.push(UsbDevice {
                address: 0, speed, max_packet_size_0: 64,
                vendor_id: 0, product_id: 0, device_class: 0,
                device_subclass: 0, device_protocol: 0, configurations: 0,
                endpoints: Vec::new(),
            });
        }
    }

    pub fn devices(&self) -> &[UsbDevice] { &self.devices }
    pub fn devices_mut(&mut self) -> &mut [UsbDevice] { &mut self.devices }
}
