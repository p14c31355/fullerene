//! USB mass-storage driver — real EHCI enumeration + FAT mount + hotplug poll.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use nitrogen::usb::ehci::EhciController;
use nitrogen::usb::xhci::XhciController;
use nitrogen::usb::{UsbDirection, UsbSetupPacket, UsbXferType};
use nitrogen::DriverContext;
use spin::Mutex;

use crate::drivers::fat::{BlockDevice, FatFileSystem};
use crate::klog_fmt;

pub static USB_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
}

// ── Inline BOT block device (no closures, no UsbMassStorage) ─

struct UsbBlockDevice {
    dev_addr: u8,
    bulk_out: u8,
    bulk_in: u8,
    block_size: u32,
    total_blocks: u64,
    tag: u32,
    ctrl_index: usize,
}

unsafe impl Send for UsbBlockDevice {}

impl UsbBlockDevice {
    fn bot_xfer(
        &mut self,
        cdb: &[u8],
        data: Option<&mut [u8]>,
        dir_in: bool,
        ehci: &mut EhciController,
    ) -> Result<(), &'static str> {
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1);

        let mut cbw = [0u8; 31];
        cbw[..4].copy_from_slice(&0x43425355u32.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        let dlen = data.as_ref().map(|d| d.len() as u32).unwrap_or(0);
        cbw[8..12].copy_from_slice(&dlen.to_le_bytes());
        cbw[12] = if dir_in { 0x80 } else { 0x00 };
        cbw[13] = 0; // bCBWLUN
        cbw[14] = cdb.len().min(16) as u8;
        cbw[15..15 + cdb.len().min(16)].copy_from_slice(&cdb[..cdb.len().min(16)]);

        ehci.bulk_transfer(self.dev_addr, self.bulk_out, &mut cbw, UsbDirection::Out, 512)?;
        if let Some(buf) = data {
            let ep = if dir_in { self.bulk_in } else { self.bulk_out };
            let dir = if dir_in { UsbDirection::In } else { UsbDirection::Out };
            ehci.bulk_transfer(self.dev_addr, ep, buf, dir, 512)?;
        }
        let mut csw = [0u8; 13];
        ehci.bulk_transfer(self.dev_addr, self.bulk_in, &mut csw, UsbDirection::In, 512)?;
        let sig = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
        if sig != 0x53425355 { return Err("bad CSW"); }
        let csw_tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
        if csw_tag != tag { return Err("CSW tag mismatch"); }
        if csw[12] != 0 { return Err("CSW err"); }
        Ok(())
    }
}

impl BlockDevice for UsbBlockDevice {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        let mut ehis = EHCI_CONTROLLERS.lock();
        let ehci = ehis[self.ctrl_index].as_mut();
        let mut cdb = [0u8; 10];
        cdb[0] = 0x28;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7..9].copy_from_slice(&count.to_be_bytes());
        let mut data = vec![0u8; (count as u32 * self.block_size) as usize];
        self.bot_xfer(&cdb, Some(&mut data), true, ehci)?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        let mut ehis = EHCI_CONTROLLERS.lock();
        let ehci = ehis[self.ctrl_index].as_mut();
        let mut cdb = [0u8; 10];
        cdb[0] = 0x2A;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7..9].copy_from_slice(&count.to_be_bytes());
        self.bot_xfer(&cdb, Some(&mut buf.to_vec()), false, ehci)
    }

    fn sector_size(&self) -> u32 { self.block_size }
    fn total_sectors(&self) -> u64 { self.total_blocks }
}

// ── Controller storage ───────────────────────────────────────

static EHCI_CONTROLLERS: Mutex<Vec<Box<EhciController>>> = Mutex::new(Vec::new());
static XHCI_CONTROLLERS: Mutex<Vec<Box<XhciController>>> = Mutex::new(Vec::new());
static CTRL_INITIALIZED: AtomicBool = AtomicBool::new(false);

pub fn init() {
    let _ = crate::vfs::mkdir("/mnt");
    init_controllers();

    // Phase 1: Immediate poll — catches EHCI devices and xHCI devices
    // that were ready right after controller reset.
    poll_usb();

    // Phase 2: Delayed re-poll for xHCI devices.
    // After HCRST (controller reset) the xHCI ports need time to
    // re-negotiate SuperSpeed / HighSpeed links (typically 1-2 s).
    // A second poll after a spin delay catches devices that weren't
    // ready during the first poll.
    for _ in 0..1_500_000 {
        nitrogen::port::PortWriter::<u8>::new(0x80).write_safe(0u8);
    }
    if poll_usb() {
        return;
    }

    // Phase 3: One more attempt for stubborn controllers/firmware.
    for _ in 0..3_000_000 {
        core::hint::spin_loop();
    }
    poll_usb();

    // Phase 4: Force re-poll with fresh ports_done — catches devices
    // that were connected during boot (same USB drive) but missed due to
    // timing or already-done filtering.
    for _ in 0..500_000 { core::hint::spin_loop(); }
    poll_usb_all();

    debug_usb();
}

fn init_controllers() {
    if CTRL_INITIALIZED.swap(true, Ordering::SeqCst) { return; }
    use nitrogen::pci::PciScanner;
    use crate::driver_context_impl::KernelDriverContext;

    let mut ehis = EHCI_CONTROLLERS.lock();
    let mut xhis = XHCI_CONTROLLERS.lock();

    // Use the comprehensive PCI scanner that handles multi-bus topologies
    // and avoids probing non-existent buses (which can hang real hardware).
    let mut scanner = PciScanner::new();
    let _ = scanner.scan_all_buses();
    for dev in scanner.get_devices() {
        if dev.class_code != 0x0C || dev.subclass != 0x03 { continue; }

        // Read BAR0 via the PCI device helper (handles 32/64-bit BARs).
        let mmio_base = match dev.read_bar(0) {
            Some(addr) => addr,
            None => { continue; }
        };

        let mmio_virt = KernelDriverContext.phys_to_virt(mmio_base) as *mut u8;
        if mmio_virt.is_null() { continue; }

        // Enable memory space + bus mastering
        dev.enable_memory_access();

        // Determine prog_if by reading it directly (PciDevice doesn't cache it).
        let prog_if = nitrogen::pci::PciConfigSpace::read_config_byte(dev.bus, dev.device, dev.function, 0x09);

        match prog_if {
            0x20 => { // EHCI
                if let Some(hc) = EhciController::new(mmio_virt, &KernelDriverContext) {
                    ehis.push(Box::new(hc));
                    klog_fmt!("USB: EHCI at {}:{}.{}\n", dev.bus, dev.device, dev.function);
                }
            }
            0x30 => { // xHCI
                if let Some(hc) = XhciController::new(mmio_virt, &KernelDriverContext) {
                    xhis.push(Box::new(hc));
                    klog_fmt!("USB: xHCI at {}:{}.{}\n", dev.bus, dev.device, dev.function);
                } else {
                    klog_fmt!("USB: xHCI at {}:{}.{} FAILED init\n", dev.bus, dev.device, dev.function);
                }
            }
            _ => {}
        }
    }
}

/// Debug dump USB controller state
pub fn debug_usb() {
    klog_fmt!("=== USB DEBUG ===\n");
    {
        let ehis = EHCI_CONTROLLERS.lock();
        for (i, ehci) in ehis.iter().enumerate() {
            klog_fmt!("EHCI[{}]: {} ports\n", i, ehci.n_ports());
            for p in 0..ehci.n_ports().min(4) {
                let ps = ehci.read_portsc(p);
                klog_fmt!("  PORTSC[{}]=0x{:08X} CCS={} PE={}\n", p, ps, ps&1, (ps>>2)&1);
            }
        }
    }
    {
        let xhis = XHCI_CONTROLLERS.lock();
        for (i, xhci) in xhis.iter().enumerate() {
            let x = xhci;
            klog_fmt!("xHCI[{}] ppc={} n_ports={} max_slots={} ports_done={:#x} legacy={}\n",
                i, x.ppc_enabled(), x.n_ports(), x.max_slots(),
                x.ports_done_mask(), x.legacy_handoff_done());
            let usbcmd = x.read_op_reg(0x00);
            let usbsts = x.read_op_reg(0x04);
            klog_fmt!("xHCI USBCMD={:#x} USBSTS={:#x} HCHalted={}\n",
                usbcmd, usbsts, (usbsts & 1) != 0);
            for p in 0..x.n_ports() {
                let ps = x.read_portsc(p);
                if ps == 0xFFFF { continue; }
                klog_fmt!("xHCI PORTSC[{}]={:#x} CCS={} PED={} PLS={} PP={} PR={} speed={}\n",
                    p, ps, ps&1, (ps>>1)&1, (ps>>5)&0xF, (ps>>9)&1, (ps>>4)&1, (ps>>10)&0xF);
            }
        }
    }
    klog_fmt!("=== USB END ===\n");
}

/// Poll all USB controllers (EHCI + xHCI). Returns true if a new drive was mounted.
pub fn poll_usb() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);

    // Phase 1: Poll controllers while holding locks to detect new devices.
    // Phase 2: Mount new devices after releasing locks (avoid deadlock with
    //          BlockDevice::read_sectors which also acquires the same lock).
    let mut ehci_pending: Vec<(usize, usize)> = Vec::new();
    let mut xhci_pending: Vec<(usize, usize)> = Vec::new();

    // ── EHCI ──
    {
        let mut ehis = EHCI_CONTROLLERS.lock();
        for (ctrl_idx, ehci_box) in ehis.iter_mut().enumerate() {
            let ehci = ehci_box.as_mut();
            ehci.start();
            let old = ehci.devices().len();
            let n_ports = ehci.n_ports();
            for p in 0..2.min(n_ports) {
                let ps = ehci.read_portsc(p);
                klog_fmt!("EHCI PORTSC[{}]: 0x{:08X} (CCS={} PE={})\n",
                    p, ps, (ps>>0)&1, (ps>>2)&1);
            }
            ehci.poll_ports();
            let new = ehci.devices().len();
            klog_fmt!("EHCI poll: {} ports old={} new={}\n", n_ports, old, new);
            if new > old {
                for idx in old..new {
                    ehci_pending.push((ctrl_idx, idx));
                }
            }
        }
    }

    // ── xHCI ──
    {
        let mut xhis = XHCI_CONTROLLERS.lock();
        for (ctrl_idx, xhci_box) in xhis.iter_mut().enumerate() {
            let xhci = xhci_box.as_mut();

            // Clear ports_done before every poll so previously-skipped
            // ports (e.g. the boot USB drive) get re-evaluated.
            // Skip clearing if we already have USB drives mounted (deduplication).
            if USB_DRIVE_COUNT.load(Ordering::Relaxed) == 0 {
                xhci.clear_ports_done();
            }

            let old = xhci.devices().len();

            let hcs1 = xhci.read_cap(4);
            let hcc1 = xhci.read_cap(0x10);
            let usbcmd = xhci.read_op_reg(0x00);
            let usbsts = xhci.read_op_reg(0x04);
            klog_fmt!("xHCI HCSPARAMS1=0x{:08X} HCCPARAMS1=0x{:08X}\n", hcs1, hcc1);
            klog_fmt!("xHCI USBCMD=0x{:08X} USBSTS=0x{:08X} running={} slots={} ports={} ppc={} legacy={}\n",
                usbcmd, usbsts, xhci.is_running(), hcs1 & 0xFF, (hcs1>>24)&0xFF,
                xhci.ppc_enabled(), xhci.legacy_handoff_done());

            for p in 0..xhci.n_ports() {
                let ps = xhci.read_portsc(p);
                if ps != 0xFFFF {
                    klog_fmt!("xHCI PORTSC[{}]=0x{:08X} CCS={} PED={} PR={} PP={} PLS={} WPR={} speed={}\n",
                        p, ps, ps&1, (ps>>1)&1, (ps>>4)&1, (ps>>9)&1,
                        (ps>>5)&0xF, (ps>>20)&1, (ps>>10)&0xF);
                }
            }

            xhci.poll_ports();
            let new = xhci.devices().len();
            klog_fmt!("xHCI poll: {} ports old={} new={}\n", xhci.n_ports(), old, new);

            if new > old {
                for idx in old..new {
                    xhci_pending.push((ctrl_idx, idx));
                }
            }
        }
    }

    // Phase 2: Mount new devices WITHOUT holding controller locks.
    // (FatFileSystem::from_device → read_sectors → locks the controller
    //  internally, so holding the lock here causes a deadlock.)
    for (ctrl_idx, idx) in ehci_pending {
        mount_ehci_device(ctrl_idx, idx);
    }
    for (ctrl_idx, idx) in xhci_pending {
        mount_xhci_device(ctrl_idx, idx);
    }

    USB_DRIVE_COUNT.load(Ordering::Relaxed) != before
}

/// Force re-poll of all xHCI ports (clears ports_done mask).
/// Returns true if a new drive was mounted.
pub fn poll_usb_all() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);
    let mut pending: Vec<(usize, usize)> = Vec::new();
    {
        // Unmount existing USB filesystems from VFS before clearing state
        for drive in USB_DRIVES.lock().iter() {
            let _ = crate::vfs::unmount(&drive.mount_point);
        }
        USB_DRIVES.lock().clear();
        USB_DRIVE_COUNT.store(0, Ordering::Relaxed);
        let mut xhis = XHCI_CONTROLLERS.lock();
        for (ctrl_idx, xhci) in xhis.iter_mut().enumerate() {
            // Disable all active slots to free device context / input context
            // pages and transfer rings before re-enumerating.
            xhci.disable_all_slots();
            xhci.clear_ports_done();
            xhci.clear_devices();
            xhci.poll_ports();
            for dev_idx in 0..xhci.devices().len() {
                pending.push((ctrl_idx, dev_idx));
            }
        }
    }
    for (ctrl_idx, dev_idx) in pending {
        mount_xhci_device(ctrl_idx, dev_idx);
    }
    USB_DRIVE_COUNT.load(Ordering::Relaxed) != before
}

/// Enumerate and mount a USB device on an xHCI controller.
fn mount_xhci_device(ctrl_index: usize, dev_idx: usize) {
    // Phase A: Enumerate the device while holding the controller lock.
    let slot_id;
    let dev_class;
    let dev_subclass;
    let dev_protocol;
    let bulk_out_ep: u8;
    let bulk_in_ep: u8;
    {
        let mut xhis = XHCI_CONTROLLERS.lock();
        let xhci = xhis[ctrl_index].as_mut();

        // Step 1: Enable slot
        slot_id = match xhci.enable_slot() {
            Ok(id) => id,
            Err(_) => return,
        };

        // Step 2: Address device (assigns address, sets up EP0)
        if xhci.address_device(slot_id).is_err() { return; }

        // Step 3: Get device descriptor via control transfer.
        // Fix: the device descriptor bDeviceClass is at offset 4,
        // not offset 12 (the old code read bcdDevice instead).
        let mut desc_buf = [0u8; 64];
        let setup = UsbSetupPacket {
            bm_request_type: 0x80,
            b_request: 6, // GET_DESCRIPTOR
            w_value: (1u16) << 8, // DEVICE descriptor
            w_index: 0,
            w_length: 64,
        };
        let desc_len = match xhci.control_transfer(slot_id, &setup, &mut desc_buf) {
            Ok(len) => len,
            Err(_) => return,
        };
        if desc_len < 18 { return; }
        dev_class = desc_buf[4];   // bDeviceClass    (was offset 12 - bug)
        dev_subclass = desc_buf[5]; // bDeviceSubClass  (was offset 13)
        dev_protocol = desc_buf[6]; // bDeviceProtocol  (was offset 14)
        let num_cfgs = desc_buf[17]; // bNumConfigurations

        // Mass-storage check: many USB flash drives report bDeviceClass=0x00
        // and specify the Mass-Storage class at the interface level.
        // Accept devices that are MSC at device-level OR have at least one
        // configuration (interface-level class is checked after CONFIG read).
        if dev_class != 0x08 && num_cfgs == 0 { return; }

        // Set configuration (configuration value = 1)
        let setup_cfg = UsbSetupPacket {
            bm_request_type: 0x00,
            b_request: 9, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        };
        if xhci.control_transfer(slot_id, &setup_cfg, &mut []).is_err() { return; }

        // Step 4: Read configuration descriptor to discover interface class
        // and endpoint addresses (instead of hardcoding 0x02/0x82).
        let mut cfg_buf = [0u8; 256];
        let setup_cfg_read = UsbSetupPacket {
            bm_request_type: 0x80,
            b_request: 6, // GET_DESCRIPTOR
            w_value: (2u16) << 8, // CONFIGURATION descriptor, index 0
            w_index: 0,
            w_length: 256,
        };
        let cfg_res = xhci.control_transfer(slot_id, &setup_cfg_read, &mut cfg_buf);
        if cfg_res.is_err() {
            // Fall back to device-level class check if CONFIG read fails
            if dev_class != 0x08 { return; }
            // else use hardcoded endpoints as last resort
            bulk_out_ep = 0x02;
            bulk_in_ep = 0x82;
        } else {
            let cfg_len = cfg_res.unwrap();
            if cfg_len < 9 { return; }
            // Parse configuration descriptor (header is 9 bytes)
            let total_len = u16::from_le_bytes([cfg_buf[2], cfg_buf[3]]) as usize;
            let mut offset: usize = 9; // skip config header
            let mut iface_class_ok = false;
            let mut found_out: Option<(u8, u16)> = None;
            let mut found_in: Option<(u8, u16)> = None;

            let limit = total_len.min(cfg_len).min(256);
            while offset + 2 <= limit {
                let dlen = cfg_buf[offset] as usize;
                if dlen < 2 || offset + dlen > limit { break; }
                let dtype = cfg_buf[offset + 1];

                match dtype {
                    4 => { // INTERFACE descriptor
                        if dlen >= 9 {
                            let iface_class = cfg_buf[offset + 5];
                            let iface_subclass = cfg_buf[offset + 6];
                            let iface_protocol = cfg_buf[offset + 7];
                            if iface_class == 0x08 {
                                // Mass Storage interface with SCSI/BOT
                                iface_class_ok = true;
                                klog_fmt!("xHCI: MSC iface class={:02X} sub={:02X} prot={:02X}\n",
                                    iface_class, iface_subclass, iface_protocol);
                            }
                        }
                    }
                    5 => { // ENDPOINT descriptor
                        if dlen >= 7 {
                            let ep_addr = cfg_buf[offset + 2];
                            let ep_attr = cfg_buf[offset + 3];
                            let xfer_type = ep_attr & 0x03;
                            let mps = u16::from_le_bytes([cfg_buf[offset + 4], cfg_buf[offset + 5]]) & 0x07FF;
                            if xfer_type == 2 { // Bulk
                                if ep_addr & 0x80 != 0 {
                                    found_in = Some((ep_addr, mps));
                                } else {
                                    found_out = Some((ep_addr, mps));
                                }
                                klog_fmt!("xHCI: bulk EP addr=0x{:02X} mps={}\n", ep_addr, mps);
                            }
                        }
                    }
                    _ => {}
                }
                offset += dlen;
            }

            // If device class wasn't 0x08, check interface class
            if dev_class != 0x08 && !iface_class_ok {
                klog_fmt!("xHCI: not a mass-storage device (dev_class={:02X}, iface_msc={})\n",
                    dev_class, iface_class_ok);
                return;
            }

            let (out_addr, out_mps) = found_out.unwrap_or((0x02, 512));
            let (in_addr, in_mps) = found_in.unwrap_or((0x82, 512));
            bulk_out_ep = out_addr;
            bulk_in_ep = in_addr;

            if xhci.configure_endpoint_bulk(slot_id, bulk_out_ep, out_mps).is_err() {
                klog_fmt!("xHCI: configure bulk OUT 0x{:02X} failed\n", bulk_out_ep);
                return;
            }
            if xhci.configure_endpoint_bulk(slot_id, bulk_in_ep, in_mps).is_err() {
                klog_fmt!("xHCI: configure bulk IN 0x{:02X} failed\n", bulk_in_ep);
                return;
            }
        }

        // Update the device entry
        if let Some(dev) = xhci.devices_mut().get_mut(dev_idx) {
            dev.device_class = dev_class;
            dev.device_subclass = dev_subclass;
            dev.device_protocol = dev_protocol;
        }

        klog_fmt!("xHCI: device enumerated slot={} class={:02X} ep_out=0x{:02X} ep_in=0x{:02X}\n",
            slot_id, dev_class, bulk_out_ep, bulk_in_ep);
    } // Lock is dropped here

    // Phase B: Build block device and mount (no lock held — FatFileSystem
    //          will acquire it internally when reading sectors).
    struct XhciBlockDev {
        slot_id: u32, bulk_out: u8, bulk_in: u8,
        block_size: u32, total_blocks: u64, tag: u32,
        ctrl_index: usize,
    }
    unsafe impl Send for XhciBlockDev {}
    impl BlockDevice for XhciBlockDev {
        fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
            let mut xhis = XHCI_CONTROLLERS.lock();
            let xhci = xhis[self.ctrl_index].as_mut();
            let mut cdb = [0u8; 10];
            cdb[0] = 0x28;
            cdb[2..6].copy_from_slice(&lba.to_be_bytes());
            cdb[7..9].copy_from_slice(&count.to_be_bytes());
            let dlen = (count as u32) * self.block_size;
            let blen = buf.len();
            let mut data = vec![0u8; dlen as usize];
            // BOT CBW → xfer → CSW via xHCI bulk transfers
            let mut cbw = [0u8; 31];
            let tag = self.tag; self.tag += 1;
            cbw[..4].copy_from_slice(&0x43425355u32.to_le_bytes());
            cbw[4..8].copy_from_slice(&tag.to_le_bytes());
            cbw[8..12].copy_from_slice(&dlen.to_le_bytes());
            cbw[12] = 0x80;
            cbw[13] = 0;
            cbw[14] = 10;
            cbw[15..25].copy_from_slice(&cdb);
            xhci.bulk_transfer(self.slot_id, self.bulk_out, &mut cbw, UsbDirection::Out, 512)?;
            xhci.bulk_transfer(self.slot_id, self.bulk_in, &mut data, UsbDirection::In, 512)?;
            let mut csw = [0u8; 13];
            xhci.bulk_transfer(self.slot_id, self.bulk_in, &mut csw, UsbDirection::In, 512)?;
            let sig = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
            if sig != 0x53425355 { return Err("bad CSW"); }
            let csw_tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
            if csw_tag != tag { return Err("CSW tag mismatch"); }
            if csw[12] != 0 { return Err("CSW err"); }
            let n = data.len().min(blen);
            buf[..n].copy_from_slice(&data[..n]);
            Ok(())
        }
        fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
            let mut xhis = XHCI_CONTROLLERS.lock();
            let xhci = xhis[self.ctrl_index].as_mut();
            let mut cdb = [0u8; 10];
            cdb[0] = 0x2A; // WRITE_10
            cdb[2..6].copy_from_slice(&lba.to_be_bytes());
            cdb[7..9].copy_from_slice(&count.to_be_bytes());
            let dlen = (count as u32) * self.block_size;
            let tag = self.tag; self.tag += 1;
            let mut cbw = [0u8; 31];
            cbw[..4].copy_from_slice(&0x43425355u32.to_le_bytes());
            cbw[4..8].copy_from_slice(&tag.to_le_bytes());
            cbw[8..12].copy_from_slice(&dlen.to_le_bytes());
            cbw[12] = 0x00;
            cbw[13] = 0;
            cbw[14] = 10;
            cbw[15..25].copy_from_slice(&cdb);
            xhci.bulk_transfer(self.slot_id, self.bulk_out, &mut cbw, UsbDirection::Out, 512)?;
            let mut wbuf = buf.to_vec();
            xhci.bulk_transfer(self.slot_id, self.bulk_out, &mut wbuf, UsbDirection::Out, 512)?;
            let mut csw = [0u8; 13];
            xhci.bulk_transfer(self.slot_id, self.bulk_in, &mut csw, UsbDirection::In, 512)?;
            let sig = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
            if sig != 0x53425355 { return Err("bad CSW"); }
            let csw_tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
            if csw_tag != tag { return Err("CSW tag mismatch"); }
            if csw[12] != 0 { return Err("CSW err"); }
            Ok(())
        }
        fn sector_size(&self) -> u32 { self.block_size }
        fn total_sectors(&self) -> u64 { self.total_blocks }
    }

    let bdev = XhciBlockDev {
        slot_id, bulk_out: bulk_out_ep, bulk_in: bulk_in_ep,
        block_size: 512, total_blocks: 0, tag: 1,
        ctrl_index,
    };

    let mp = alloc::format!("/mnt/usb-{}", USB_DRIVES.lock().len() + 1);
    match FatFileSystem::from_device(Box::new(bdev)) {
        Ok(fs) => {
            let _ = crate::vfs::mkdir(&mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                let n = USB_DRIVES.lock().len() + 1;
                USB_DRIVES.lock().push(UsbDrive { name: alloc::format!("USB Drive {}", n), mount_point: mp });
                USB_DRIVE_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(e) => { klog_fmt!("USB xHCI mount: {}\n", e); }
    }
}

/// Enumerate and mount a USB device on an EHCI controller.
fn mount_ehci_device(ctrl_index: usize, dev_idx: usize) {
    // Phase A: Enumerate the device while holding the controller lock.
    let dev;
    let mut bulk_out = 0u8;
    let mut bulk_in = 0u8;
    {
        let mut ehis = EHCI_CONTROLLERS.lock();
        let ehci = ehis[ctrl_index].as_mut();

        // Reset qH/qTD pools so control/bulk transfers always have free entries.
        ehci.reset_pools();

        let result = unsafe {
            let p: &mut EhciController = &mut *ehci;
            let mut ctrl = |a, ep, s: &UsbSetupPacket, b: &mut [u8]| p.control_transfer(a, ep, s, b);
            nitrogen::usb::hub::enumerate_device(&mut ctrl)
        };
        dev = match result { Ok(d) => d, Err(_) => return };
        if !dev.is_mass_storage() { return; }

        // Write back enumerated device metadata into the controller's device list
        // so address / endpoints are available for subsequent bulk transfers.
        if let Some(slot) = ehci.devices_mut().get_mut(dev_idx) {
            *slot = dev.clone();
        }

        for ep in &dev.endpoints {
            if ep.xfer_type() != UsbXferType::Bulk { continue; }
            match ep.direction() {
                UsbDirection::Out => { bulk_out = ep.b_endpoint_address; }
                UsbDirection::In => { bulk_in = ep.b_endpoint_address; }
            }
        }
    } // Lock is dropped here

    if bulk_out == 0 || bulk_in == 0 { return; }

    // Phase B: Build block device and mount (no lock held).
    let bdev = UsbBlockDevice {
        dev_addr: dev.address, bulk_out, bulk_in,
        block_size: 512, total_blocks: 0, tag: 1, ctrl_index,
    };

    let mp = alloc::format!("/mnt/usb-{}", USB_DRIVES.lock().len() + 1);
    match FatFileSystem::from_device(Box::new(bdev)) {
        Ok(fs) => {
            let _ = crate::vfs::mkdir(&mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                let n = USB_DRIVES.lock().len() + 1;
                USB_DRIVES.lock().push(UsbDrive { name: alloc::format!("USB Drive {}", n), mount_point: mp });
                USB_DRIVE_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(e) => { klog_fmt!("USB mount: {}\n", e); }
    }
}