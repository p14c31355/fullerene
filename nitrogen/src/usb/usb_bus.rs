//! USB Bus — controller discovery, enumeration, and BOT mass-storage protocol.
//!
//! This module owns the list of active host controllers and provides
//! the high-level USB operations that the kernel needs:
//! - PCI scanning and controller initialisation
//! - Port polling and device enumeration
//! - Bulk-Only Transport (BOT) for mass-storage devices
//!
//! All controller-specific details are hidden behind [`super::host_controller::HostController`].

use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::DriverContext;
use crate::usb::{
    UsbDevice, UsbDirection, UsbSetupPacket, UsbXferType,
    REQ_GET_DESCRIPTOR, REQ_SET_CONFIGURATION,
    DESC_DEVICE, DESC_CONFIGURATION,
};
use super::host_controller::HostController;
use super::ehci_context::EhciContext;
use super::xhci_context::XhciContext;

// ============================================================================
//  CBW / CSW (Bulk-Only Transport)
// ============================================================================

/// Command Block Wrapper (31 bytes, BOT spec §5.1).
#[repr(C, packed)]
struct Cbw {
    signature: u32,     // 0x43425355 ("USBC")
    tag: u32,
    data_len: u32,
    flags: u8,          // 0x80 = IN, 0x00 = OUT
    lun: u8,
    cb_len: u8,
    cb: [u8; 16],
}

const CBW_SIGNATURE: u32 = 0x43425355;

/// Command Status Wrapper (13 bytes, BOT spec §5.2).
#[repr(C, packed)]
struct Csw {
    signature: u32,     // 0x53425355 ("USBS")
    tag: u32,
    residue: u32,
    status: u8,         // 0 = success
}

const CSW_SIGNATURE: u32 = 0x53425355;

// ============================================================================
//  BOT transfer (generic over HostController)
// ============================================================================

/// Borrowed buffer for BOT data phase, distinguishing IN vs OUT direction.
///
/// This enum avoids undefined behavior: an `&[u8]` buffer for OUT transfers
/// is never cast to `&mut [u8]`.  `In` accepts `&mut [u8]` and `Out` accepts `&[u8]`.
pub enum BotBuffer<'a> {
    In(&'a mut [u8]),
    Out(&'a [u8]),
}

impl<'a> BotBuffer<'a> {
    pub fn len(&self) -> usize {
        match self {
            BotBuffer::In(buf) => buf.len(),
            BotBuffer::Out(buf) => buf.len(),
        }
    }

    pub fn is_in(&self) -> bool {
        matches!(self, BotBuffer::In(_))
    }
}

/// Execute a single BOT command (CBW → Data → CSW).
///
/// `host` is any host controller implementing [`HostController`].
/// `dev_addr` is the USB device address, `ep_out`/`ep_in` are bulk endpoints.
pub fn bot_exec_command(
    host: &mut dyn HostController,
    dev_addr: u8,
    ep_out: u8,
    ep_in: u8,
    cdb: &[u8],
    data: Option<BotBuffer<'_>>,
    tag: &mut u32,
) -> Result<(), &'static str> {
    let t = *tag;
    *tag = tag.wrapping_add(1);

    let dlen = data.as_ref().map(|d| d.len() as u32).unwrap_or(0);
    let dir_in = data.as_ref().map(|d| d.is_in()).unwrap_or(false);

    // ── Phase 1: Send CBW ─────────────────────────────────
    let mut cbw_raw = [0u8; 31];
    cbw_raw[..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
    cbw_raw[4..8].copy_from_slice(&t.to_le_bytes());
    cbw_raw[8..12].copy_from_slice(&dlen.to_le_bytes());
    cbw_raw[12] = if dir_in { 0x80 } else { 0x00 };
    cbw_raw[13] = 0; // LUN
    cbw_raw[14] = cdb.len().min(16) as u8;
    cbw_raw[15..15 + cdb.len().min(16)].copy_from_slice(&cdb[..cdb.len().min(16)]);

    host.bulk_transfer(dev_addr, ep_out, &mut cbw_raw, UsbDirection::Out, 512)?;

    // ── Phase 2: Data (optional) ──────────────────────────
    if let Some(buf) = data {
        let ep = if dir_in { ep_in } else { ep_out };
        match buf {
            BotBuffer::In(buf) => {
                host.bulk_transfer(dev_addr, ep, buf, UsbDirection::In, 512)?;
            }
            BotBuffer::Out(buf) => {
                // SAFETY: bulk_transfer for OUT only reads the buffer, so creating a
                // temporary mutable slice from an immutable one is sound.  The enum
                // wrapper guarantees this path only runs for OUT transfers.
                let len = buf.len();
                let mut tmp = alloc::vec![0u8; len];
                tmp.copy_from_slice(buf);
                host.bulk_transfer(dev_addr, ep, &mut tmp, UsbDirection::Out, 512)?;
            }
        }
    }

    // ── Phase 3: Receive CSW ──────────────────────────────
    let mut csw_raw = [0u8; 13];
    host.bulk_transfer(dev_addr, ep_in, &mut csw_raw, UsbDirection::In, 512)?;

    let sig = u32::from_le_bytes([csw_raw[0], csw_raw[1], csw_raw[2], csw_raw[3]]);
    if sig != CSW_SIGNATURE {
        return Err("bad CSW signature");
    }
    let csw_tag = u32::from_le_bytes([csw_raw[4], csw_raw[5], csw_raw[6], csw_raw[7]]);
    if csw_tag != t {
        return Err("CSW tag mismatch");
    }
    if csw_raw[12] != 0 {
        return Err("CSW reported error");
    }
    Ok(())
}

/// Read sectors from a mass-storage device via BOT.
pub fn bot_read_sectors(
    host: &mut dyn HostController,
    dev_addr: u8,
    ep_out: u8,
    ep_in: u8,
    lba: u32,
    count: u16,
    block_size: u32,
    buf: &mut [u8],
    tag: &mut u32,
) -> Result<(), &'static str> {
    let dlen = count as u32 * block_size;
    let mut cdb = [0u8; 10];
    cdb[0] = 0x28; // READ_10
    cdb[2..6].copy_from_slice(&lba.to_be_bytes());
    cdb[7..9].copy_from_slice(&count.to_be_bytes());
    let mut data = alloc::vec![0u8; dlen as usize];
    bot_exec_command(host, dev_addr, ep_out, ep_in, &cdb, Some(BotBuffer::In(&mut data)), tag)?;
    let n = dlen.min(buf.len() as u32) as usize;
    buf[..n].copy_from_slice(&data[..n]);
    Ok(())
}

/// Write sectors to a mass-storage device via BOT.
pub fn bot_write_sectors(
    host: &mut dyn HostController,
    dev_addr: u8,
    ep_out: u8,
    ep_in: u8,
    lba: u32,
    count: u16,
    block_size: u32,
    buf: &[u8],
    tag: &mut u32,
) -> Result<(), &'static str> {
    let mut cdb = [0u8; 10];
    cdb[0] = 0x2A; // WRITE_10
    cdb[2..6].copy_from_slice(&lba.to_be_bytes());
    cdb[7..9].copy_from_slice(&count.to_be_bytes());
    bot_exec_command(host, dev_addr, ep_out, ep_in, &cdb, Some(BotBuffer::Out(buf)), tag)
}

// ============================================================================
//  Device enumeration (generic over HostController)
// ============================================================================

/// Enumerate a mass-storage device on the given host controller.
///
/// Returns `(dev_addr, ep_out, ep_in, block_size)` or an error.
/// Uses the host controller's `control_transfer` and `bulk_transfer`.
pub fn enumerate_mass_storage(
    host: &mut dyn HostController,
    dev_addr: u8,
    dev_idx: usize,
) -> Result<(u8, u8, u32), &'static str> {
    // Step 1: Get device descriptor (64 bytes for safety)
    let mut desc_buf = [0u8; 64];
    let setup = UsbSetupPacket {
        bm_request_type: 0x80,
        b_request: REQ_GET_DESCRIPTOR,
        w_value: (DESC_DEVICE as u16) << 8,
        w_index: 0,
        w_length: 64,
    };
    let len = host.control_transfer(dev_addr, &setup, &mut desc_buf)?;
    if len < 18 {
        return Err("descriptor too short");
    }

    let dev_class = desc_buf[4];
    let _dev_subclass = desc_buf[5];
    let _dev_protocol = desc_buf[6];
    let num_cfgs = desc_buf[17];

    // Accept MSC at device level OR at least one configuration
    if dev_class != 0x08 && num_cfgs == 0 {
        return Err("not MSC");
    }

    // Step 2: Set configuration (value = 1)
    let setup_cfg = UsbSetupPacket {
        bm_request_type: 0x00,
        b_request: REQ_SET_CONFIGURATION,
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    host.control_transfer(dev_addr, &setup_cfg, &mut [])?;

    // Step 3: Read configuration descriptor
    let mut cfg_buf = [0u8; 256];
    let setup_cfg_read = UsbSetupPacket {
        bm_request_type: 0x80,
        b_request: REQ_GET_DESCRIPTOR,
        w_value: (DESC_CONFIGURATION as u16) << 8,
        w_index: 0,
        w_length: 256,
    };
    let cfg_res = host.control_transfer(dev_addr, &setup_cfg_read, &mut cfg_buf);

    let (ep_out, ep_in) = if let Ok(cfg_len) = cfg_res {
        if cfg_len < 9 {
            return Err("config too short");
        }
        parse_endpoints(&cfg_buf, cfg_len)
    } else {
        if dev_class != 0x08 {
            return Err("not MSC");
        }
        // Fallback: hardcoded endpoints
        (0x02u8, 0x82u8)
    };

    // Update device metadata
    if let Some(dev) = host.devices_mut().get_mut(dev_idx) {
        dev.device_class = dev_class;
        dev.device_subclass = desc_buf[5];
        dev.device_protocol = desc_buf[6];
    }

    Ok((ep_out, ep_in, 512))
}

/// Parse bulk IN/OUT endpoints from a configuration descriptor buffer.
fn parse_endpoints(cfg_buf: &[u8], cfg_len: usize) -> (u8, u8) {
    let total_len = u16::from_le_bytes([cfg_buf[2], cfg_buf[3]]) as usize;
    let mut offset: usize = 9;
    let mut found_out: Option<u8> = None;
    let mut found_in: Option<u8> = None;

    let limit = total_len.min(cfg_len).min(cfg_buf.len());
    while offset + 2 <= limit {
        let dlen = cfg_buf[offset] as usize;
        if dlen < 2 || offset + dlen > limit {
            break;
        }
        let dtype = cfg_buf[offset + 1];
        if dtype == 5 && dlen >= 7 {
            // ENDPOINT descriptor
            let ep_addr = cfg_buf[offset + 2];
            let ep_attr = cfg_buf[offset + 3];
            if (ep_attr & 0x03) == 2 {
                // Bulk endpoint
                if ep_addr & 0x80 != 0 {
                    found_in = Some(ep_addr);
                } else {
                    found_out = Some(ep_addr);
                }
            }
        }
        offset += dlen;
    }

    let out = found_out.unwrap_or(0x02);
    let in_ep = found_in.unwrap_or(0x82);
    (out, in_ep)
}

// EHCI-specific enumeration is in hub.rs (enumerate_device).
// Callers should use hub::enumerate_device() with an appropriate
// control-transfer callback.

// ============================================================================
//  UsbBus — unified bus manager
// ============================================================================

/// USB bus manager holding all active host controllers.
pub struct UsbBus {
    /// EHCI controllers.
    pub ehci: Vec<Box<EhciContext>>,
    /// xHCI controllers.
    pub xhci: Vec<Box<XhciContext>>,
}

impl UsbBus {
    pub fn new() -> Self {
        Self {
            ehci: Vec::new(),
            xhci: Vec::new(),
        }
    }

    /// Scan PCI bus and initialise all found USB controllers.
    pub fn init_controllers(&mut self, ctx: &'static dyn DriverContext) {
        use crate::pci::PciScanner;
        let mut scanner = PciScanner::new();
        let _ = scanner.scan_all_buses();
        for dev in scanner.get_devices() {
            if dev.class_code != 0x0C || dev.subclass != 0x03 {
                continue;
            }
            let mmio_base = match dev.read_bar(0) {
                Some(addr) => addr,
                None => continue,
            };
            let mmio_virt = ctx.phys_to_virt(mmio_base) as *mut u8;
            if mmio_virt.is_null() {
                continue;
            }
            dev.enable_memory_access();

            let prog_if = crate::pci::PciConfigSpace::read_config_byte(
                dev.bus, dev.device, dev.function, 0x09,
            );
            match prog_if {
                0x20 => {
                    if let Some(hc) = EhciContext::new(mmio_virt, ctx) {
                        self.ehci.push(Box::new(hc));
                    }
                }
                0x30 => {
                    if let Some(hc) = XhciContext::new(mmio_virt, ctx) {
                        self.xhci.push(Box::new(hc));
                    }
                }
                _ => {}
            }
        }
    }

    /// Poll all controllers for new devices.
    /// Returns the total number of newly discovered devices.
    pub fn poll(&mut self) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
        let mut ehci_pending: Vec<(usize, usize)> = Vec::new();
        let mut xhci_pending: Vec<(usize, usize)> = Vec::new();

        // ── EHCI ──
        for (ctrl_idx, ehci) in self.ehci.iter_mut().enumerate() {
            let _ = ehci.start();
            let old = ehci.devices().len();
            let new_devs = ehci.poll_ports();
            let new = ehci.devices().len();
            if new_devs > 0 || new > old {
                for idx in old..new {
                    ehci_pending.push((ctrl_idx, idx));
                }
            }
        }

        // ── xHCI ──
        for (ctrl_idx, xhci) in self.xhci.iter_mut().enumerate() {
            xhci.clear_hse_and_recover();
            let old = xhci.devices().len();
            let new_devs = xhci.poll_ports();
            let new = xhci.devices().len();
            if new_devs > 0 || new > old {
                for idx in old..new {
                    xhci_pending.push((ctrl_idx, idx));
                }
            }
        }

        (ehci_pending, xhci_pending)
    }

    /// Force re-poll of all xHCI controllers (clears done flags, re-enumerates).
    /// Note: This method currently only polls xHCI controllers. EHCI controllers
    /// are not re-enumerated by this method.
    pub fn poll_all_xhci(&mut self) -> Vec<(usize, usize)> {
        let mut pending: Vec<(usize, usize)> = Vec::new();
        for (ctrl_idx, xhci) in self.xhci.iter_mut().enumerate() {
            xhci.clear_hse_and_recover();
            xhci.disable_all_slots();
            xhci.clear_devices();
            xhci.poll_ports();
            for dev_idx in 0..xhci.devices().len() {
                pending.push((ctrl_idx, dev_idx));
            }
        }
        pending
    }

    /// Deprecated alias for poll_all_xhci().
    pub fn poll_all(&mut self) -> Vec<(usize, usize)> {
        self.poll_all_xhci()
    }
}