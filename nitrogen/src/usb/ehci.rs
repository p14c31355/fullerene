//! EHCI (USB 2.0) Host Controller Driver.
//!
//! Provides:
//! - Controller reset and start
//! - Port status detection
//! - Async schedule management (control/bulk transfers)
//! - qTD (queue element descriptor) management
//! - Device address assignment
//!
//! # Register Map (MMIO)
//!
//! ```text
//! Capability Registers (offset 0x00):
//!   CAPLENGTH   — offset to operational registers
//!   HCSPARAMS   — structural parameters (port count etc.)
//!   HCCPARAMS   — capability parameters (64-bit, etc.)
//!
//! Operational Registers (offset CAPLENGTH):
//!   USBCMD      — run/stop, reset, async schedule enable
//!   USBSTS      — interrupt status, HCHalted
//!   USBINTR     — interrupt enable
//!   FRINDEX     — micro-frame counter
//!   CTRLDSSEGMENT — high 32 bits of schedule base (64-bit)
//!   PERIODICLISTBASE — periodic list base address
//!   ASYNCLISTADDR   — async list base address
//!   PORTSC(i)   — port status and control
//! ```

use crate::usb::{UsbDevice, UsbSpeed, UsbSetupPacket, UsbEndpointDesc, UsbDirection};
use crate::DriverContext;
use alloc::vec::Vec;

// ── Register offsets (operational, relative to CAPLENGTH) ─────
const USBCMD: u32 = 0x00;
const USBSTS: u32 = 0x04;
const USBINTR: u32 = 0x08;
const FRINDEX: u32 = 0x0C;
const CTRLDSSEGMENT: u32 = 0x10;
const PERIODICLISTBASE: u32 = 0x14;
const ASYNCLISTADDR: u32 = 0x18;
const PORTSC_BASE: u32 = 0x44; // first port, each port is 4 bytes

// ── USBCMD bits ──────────────────────────────────────────────
const CMD_RUN: u32 = 1 << 0;
const CMD_HCRESET: u32 = 1 << 1;
const CMD_ASSE: u32 = 1 << 5;  // Async Schedule Enable
const CMD_PSE: u32 = 1 << 4;   // Periodic Schedule Enable

// ── USBSTS bits ──────────────────────────────────────────────
const STS_HCHALTED: u32 = 1 << 0;
const STS_PCD: u32 = 1 << 2;   // Port Change Detect

// ── PORTSC bits ──────────────────────────────────────────────
const PORTSC_CCS: u32 = 1 << 0;   // Current Connect Status
const PORTSC_PE: u32 = 1 << 2;    // Port Enable
const PORTSC_RESET: u32 = 1 << 8; // Port Reset
const PORTSC_PR: u32 = 1 << 8;    // Port Reset (same bit)
const PORTSC_LINESTATE_MASK: u32 = 3 << 10;
const PORTSC_LINESTATE_SHIFT: u32 = 10;

// ── Async List Schedule ──────────────────────────────────────
//
// Queue Head (qH) — 48 bytes, 32-byte aligned
#[repr(C, align(32))]
pub struct QueueHead {
    /// Horizontal link pointer (next qH or terminator)
    pub horz_link: u32,
    /// Endpoint characteristics:
    ///   bits 0-6: device address
    ///   bits 8-10: endpoint number
    ///   bits 11-12: EPS (0=full, 1=low, 2=high)
    ///   bit 14: DT (data toggle control)
    ///   bits 16-20: Mult (high-bandwidth)
    ///   bits 23-26: NAK reload counter
    pub ep_chars: u32,
    /// Endpoint capabilities:
    ///   bits 0-11: max packet length
    ///   bits 12-14: C (control endpoint flags)
    ///   bits 16-26: hub address
    ///   bits 27-31: port number
    pub ep_caps: u32,
    /// Current qTD pointer (physical address of next qTD, or terminator)
    pub current_qtd: u32,
    /// qTD list (scratch / overlay area — 4 × 32-bit dwords)
    pub next_qtd: u32,
    pub alt_next_qtd: u32,
    pub token: u32,
    pub buf0: u32,
    pub buf1: u32,
    pub buf2: u32,
    pub buf3: u32,
    pub buf4: u32,
    /// Extended buffer pointers (for 64-bit)
    pub ext_buf0: u32,
    pub ext_buf1: u32,
    pub ext_buf2: u32,
    pub ext_buf3: u32,
    pub ext_buf4: u32,
}

const QH_HORZ_TERMINATE: u32 = 1;

const QH_EP_ADDR_MASK: u32 = 0x7F;
const QH_EP_ENDPT_SHIFT: u32 = 8;
const QH_EP_EPS_SHIFT: u32 = 12;
const QH_EP_EPS_FULL: u32 = 0;
const QH_EP_EPS_LOW: u32 = 1;
const QH_EP_EPS_HIGH: u32 = 2;
const QH_EP_DTC: u32 = 1 << 14; // doorbell / toggle control

const QH_CAP_MAX_PKT_MASK: u32 = 0x07FF;

// ── Queue Element Descriptor (qTD) — 32 bytes, 32-byte aligned ──
#[repr(C, align(32))]
pub struct Qtd {
    pub next_qtd: u32,       // physical address of next qTD
    pub alt_next_qtd: u32,   // alternate next (for errors)
    pub token: u32,          // status, PID, length, ioc, C_page, cerr, toggle
    pub buf0: u32,           // buffer page 0 (physical)
    pub buf1: u32,           // buffer page 1
    pub buf2: u32,           // buffer page 2
    pub buf3: u32,           // buffer page 3
    pub buf4: u32,           // buffer page 4
}

const QTD_TOKEN_STATUS_MASK: u32 = 0xFF;
const QTD_TOKEN_ACTIVE: u32 = 1 << 7;
const QTD_TOKEN_HALTED: u32 = 1 << 6;
const QTD_TOKEN_DATABUFERR: u32 = 1 << 5;
const QTD_TOKEN_BABBLE: u32 = 1 << 4;
const QTD_TOKEN_XACTERR: u32 = 1 << 3;
const QTD_TOKEN_PID_SHIFT: u32 = 8;
const QTD_TOKEN_PID_OUT: u32 = 0;
const QTD_TOKEN_PID_IN: u32 = 1;
const QTD_TOKEN_PID_SETUP: u32 = 2;
const QTD_TOKEN_ERROR_SHIFT: u32 = 10;
const QTD_TOKEN_CERR_SHIFT: u32 = 10;
const QTD_TOKEN_CERR_MASK: u32 = 3 << QTD_TOKEN_CERR_SHIFT;
const QTD_TOKEN_IOC: u32 = 1 << 15;
const QTD_TOKEN_TOTAL_BYTES_SHIFT: u32 = 16;
const QTD_TOKEN_TOTAL_BYTES_MASK: u32 = 0x7FFF << QTD_TOKEN_TOTAL_BYTES_SHIFT;

// ── EHCI Controller ──────────────────────────────────────────

/// EHCI (USB 2.0) Host Controller driver.
///
/// Manages MMIO register access, port routing, and async/periodic
/// schedule lists for control/bulk/interrupt transfers.
pub struct EhciController {
    /// MMIO base physical address (capability registers).
    mmio_base: *mut u8,
    /// Offset to operational registers (from CAPLENGTH).
    op_offset: u32,
    /// Number of downstream ports.
    n_ports: u32,
    /// Physical address of the async list head (dummy qH).
    async_head_phys: u64,
    /// Virtual address of the async head qH.
    async_head: &'static mut QueueHead,
    /// Connected devices.
    devices: Vec<UsbDevice>,
    /// The DriverContext for allocation.
    driver_ctx: &'static dyn DriverContext,
    /// Next device address.
    next_address: u8,
}

impl EhciController {
    /// Create and initialize an EHCI controller at the given MMIO physical address.
    ///
    /// `mmio_phys` is the physical base of the capability registers.
    /// The caller must have already mapped this region.
    pub fn new(mmio_base: *mut u8, ctx: &'static dyn DriverContext) -> Option<Self> {
        // Read capabilities
        let caplength = unsafe { core::ptr::read_volatile(mmio_base as *const u8) } as u32;
        let hcsparams = unsafe {
            core::ptr::read_volatile((mmio_base.add(4)) as *const u32)
        };
        let n_ports = (hcsparams >> 24) & 0x0F;
        let hccparams = unsafe {
            core::ptr::read_volatile((mmio_base.add(8)) as *const u32)
        };
        let _has_64bit = (hccparams & 1) != 0;

        let op_offset = caplength;
        let op_base = unsafe { mmio_base.add(op_offset as usize) };

        // Reset controller
        unsafe {
            core::ptr::write_volatile(op_base as *mut u32, CMD_HCRESET);
        }
        // Wait up to 250ms for reset to complete
        for _ in 0..250_000 {
            let sts = unsafe { core::ptr::read_volatile((op_base.add(USBSTS as usize)) as *const u32) };
            if sts & CMD_HCRESET == 0 {
                break;
            }
            crate::port::PortWriter::new(0x80).write_safe(0u8); // small delay
        }

        // Verify controller halted
        let sts = unsafe { core::ptr::read_volatile((op_base.add(USBSTS as usize)) as *const u32) };
        if sts & STS_HCHALTED == 0 {
            return None; // reset failed
        }

        // Allocate async list head qH
        let async_head_phys = ctx.allocate_frame().ok()?;
        let async_head_virt = ctx.phys_to_virt(async_head_phys) as *mut QueueHead;
        let async_head = unsafe { &mut *async_head_virt };
        // Initialize as a dummy qH that terminates immediately
        unsafe {
            core::ptr::write_volatile(&mut async_head.horz_link, QH_HORZ_TERMINATE);
            core::ptr::write_volatile(&mut async_head.ep_chars, 0);
            core::ptr::write_volatile(&mut async_head.ep_caps, 0);
            core::ptr::write_volatile(&mut async_head.current_qtd, 1); // terminator
            core::ptr::write_volatile(&mut async_head.next_qtd, 1);
            core::ptr::write_volatile(&mut async_head.alt_next_qtd, 1);
            core::ptr::write_volatile(&mut async_head.token, 0);
        }

        Some(Self {
            mmio_base,
            op_offset,
            n_ports: n_ports.max(1),
            async_head_phys,
            async_head,
            devices: Vec::new(),
            driver_ctx: ctx,
            next_address: 1,
        })
    }

    pub fn start(&mut self) {
        let op_base = unsafe { self.mmio_base.add(self.op_offset as usize) };

        // Set async list base to the head qH
        let async_list_phys = self.async_head_phys as u32;
        unsafe {
            core::ptr::write_volatile(
                (op_base.add(ASYNCLISTADDR as usize)) as *mut u32,
                async_list_phys,
            );
        }

        // Enable async schedule, start the controller
        unsafe {
            let cmd = core::ptr::read_volatile((op_base.add(USBCMD as usize)) as *const u32);
            core::ptr::write_volatile(
                (op_base.add(USBCMD as usize)) as *mut u32,
                cmd | CMD_RUN | CMD_ASSE,
            );
        }

        // Wait for HCHalted to clear
        for _ in 0..100_000 {
            let sts = unsafe {
                core::ptr::read_volatile((op_base.add(USBSTS as usize)) as *const u32)
            };
            if sts & STS_HCHALTED == 0 {
                break;
            }
        }
    }

    /// Poll port status and detect newly connected devices.
    pub fn poll_ports(&mut self) {
        let op_base = unsafe { self.mmio_base.add(self.op_offset as usize) };
        for port in 0..self.n_ports {
            let portsc_addr = (op_base as usize + PORTSC_BASE as usize + (port * 4) as usize) as *mut u32;
            let portsc = unsafe { core::ptr::read_volatile(portsc_addr) };

            if portsc & PORTSC_CCS == 0 {
                continue; // nothing connected
            }
            if portsc & PORTSC_PE != 0 {
                continue; // already enabled
            }

            // Port reset
            unsafe {
                core::ptr::write_volatile(portsc_addr, portsc | PORTSC_RESET);
            }
            // Wait for reset (EHCI spec: 50ms)
            for _ in 0..50_000 {
                let _ = unsafe { core::ptr::read_volatile(portsc_addr) };
            }
            unsafe {
                core::ptr::write_volatile(portsc_addr, portsc & !PORTSC_RESET);
            }

            // Wait for port enable after reset
            for _ in 0..10_000 {
                let ps = unsafe { core::ptr::read_volatile(portsc_addr) };
                if ps & PORTSC_PE != 0 {
                    break;
                }
            }

            let portsc_after = unsafe { core::ptr::read_volatile(portsc_addr) };
            let speed = UsbSpeed::from_portsc(portsc_after);
            // If the device is low/full speed, EHCI won't own it (companion controller does).
            // For now, we only handle high-speed devices.
            if speed != UsbSpeed::High {
                continue;
            }

            // Create a minimal device entry; full enumeration happens via control transfers.
            let dev = UsbDevice {
                address: 0,
                speed,
                max_packet_size_0: 64, // default for high-speed
                vendor_id: 0,
                product_id: 0,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                configurations: 0,
                endpoints: Vec::new(),
            };
            self.devices.push(dev);
        }
    }

    pub fn devices(&self) -> &[UsbDevice] {
        &self.devices
    }

    pub fn devices_mut(&mut self) -> &mut [UsbDevice] {
        &mut self.devices
    }

    /// Perform a control transfer (setup + data + status).
    ///
    /// `buf` is used for data (written for IN, read for OUT).
    /// Returns Ok(bytes transferred) or Err.
    pub fn control_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        setup: &UsbSetupPacket,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        // Note: a full EHCI control transfer implementation would:
        // 1. Create a qH for the endpoint
        // 2. Create 3 qTDs: SETUP → DATA (optional) → STATUS
        // 3. Link into async schedule
        // 4. Wait for completion
        //
        // For now, this is a stub that will be completed with async infrastructure.
        Err("async schedule not yet implemented")
    }

    /// Perform a bulk transfer (IN or OUT).
    pub fn bulk_transfer(
        &mut self,
        dev_addr: u8,
        endpoint: u8,
        buf: &mut [u8],
        dir: UsbDirection,
        max_packet: u16,
    ) -> Result<usize, &'static str> {
        // Similar to control transfer but uses bulk PID.
        Err("bulk transfer not yet implemented")
    }
}
