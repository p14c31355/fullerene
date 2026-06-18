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
    poll_usb();
}

fn init_controllers() {
    if CTRL_INITIALIZED.swap(true, Ordering::SeqCst) { return; }
    use nitrogen::pci::PciConfigSpace;
    use crate::driver_context_impl::KernelDriverContext;

    let mut ehis = EHCI_CONTROLLERS.lock();
    let mut xhis = XHCI_CONTROLLERS.lock();

    for bus in 0u8..=0 {
        for slot in 0u8..32 {
            for func in 0u8..8 {
                let cfg = match PciConfigSpace::read_from_device(bus, slot, func) {
                    Some(c) => c,
                    None => { if func == 0 { break; } continue; }
                };
                if cfg.class_code != 0x0C || cfg.subclass != 0x03 { continue; }

                // Read BAR0 (MMIO base). Handle both 32-bit and 64-bit BARs.
                let bar0_raw = PciConfigSpace::read_config_dword(bus, slot, func, 0x10);
                if bar0_raw & 1 != 0 { continue; } // Skip I/O space BARs
                let bar_type = (bar0_raw >> 1) & 3;
                let mmio_base = if bar_type == 2 {
                    // 64-bit MMIO BAR: read upper dword at offset 0x14
                    let low = bar0_raw & 0xFFFF_FFF0;
                    let high = PciConfigSpace::read_config_dword(bus, slot, func, 0x14);
                    (low as u64) | ((high as u64) << 32)
                } else {
                    (bar0_raw & 0xFFFF_FFF0) as u64
                };
                if mmio_base == 0 { continue; }

                let mmio_virt = KernelDriverContext.phys_to_virt(mmio_base) as *mut u8;
                if mmio_virt.is_null() { continue; }

                PciConfigSpace::write_config_word_raw(bus, slot, func, 4,
                    PciConfigSpace::read_config_word(bus, slot, func, 4) | 0x06);

                match cfg.prog_if {
                    0x20 => { // EHCI
                        if let Some(hc) = EhciController::new(mmio_virt, &KernelDriverContext) {
                            ehis.push(Box::new(hc));
                            klog_fmt!("USB: EHCI at {}:{}.{}\n", bus, slot, func);
                        }
                    }
                    0x30 => { // xHCI
                        if let Some(hc) = XhciController::new(mmio_virt, &KernelDriverContext) {
                            xhis.push(Box::new(hc));
                             klog_fmt!("USB: xHCI at {}:{}.{}\n", bus, slot, func);
                         } else {
                             klog_fmt!("USB: xHCI at {}:{}.{} FAILED init\n", bus, slot, func);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Poll all USB controllers (EHCI + xHCI). Returns true if a new drive was mounted.
pub fn poll_usb() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);

    // ── EHCI ──
    {
        let mut ehis = EHCI_CONTROLLERS.lock();
        for (ctrl_idx, ehci_box) in ehis.iter_mut().enumerate() {
            let ehci = ehci_box.as_mut();
            ehci.start();
            let old = ehci.devices().len();
            let n_ports = ehci.n_ports();
            // Dump first 2 EHCI PORTSC for debug
            for p in 0..2.min(n_ports) {
                let ps = ehci.read_portsc(p);
                klog_fmt!("EHCI PORTSC[{}]: 0x{:08X} (CCS={} PE={})\n",
                    p, ps, (ps>>0)&1, (ps>>2)&1);
            }
            ehci.poll_ports();
            let new = ehci.devices().len();
            klog_fmt!("EHCI poll: {} ports old={} new={}\n", n_ports, old, new);
            if new <= old { continue; }
            for idx in old..ehci.devices().len() {
                mount_ehci_device(ehci, idx, ctrl_idx);
            }
        }
    }

    // ── xHCI ──
    {
        let mut xhis = XHCI_CONTROLLERS.lock();
        for (ctrl_idx, xhci_box) in xhis.iter_mut().enumerate() {
            let xhci = xhci_box.as_mut();
            let old = xhci.devices().len();
            let hcs1 = xhci.read_cap(4);
            klog_fmt!("xHCI HCSPARAMS1=0x{:08X} (slots={} ports={} PPC={})\n",
                hcs1, hcs1 & 0xFF, (hcs1>>24)&0xFF, (hcs1>>4)&1);
            // Check if HCRST was skipped (PPC=0 + port power preserved)
            let ps0 = xhci.read_portsc(0);
            klog_fmt!("xHCI PORTSC[0] after init: 0x{:08X} (PP={} CCS={})\n",
                ps0, (ps0>>9)&1, ps0&1);
            let hcc1 = xhci.read_cap(0x10);
            klog_fmt!("xHCI HCCPARAMS1=0x{:08X} (64bit={} xECP=0x{:x})\n",
                hcc1, hcc1 & 1, (hcc1>>16)&0xFFFF);
            // Dump first 3 PORTSC registers for debug
            for p in 0..3.min(xhci.n_ports()) {
                let ps = xhci.read_portsc(p);
                if ps != 0xFFFF {
                    klog_fmt!("xHCI PORTSC[{}]: 0x{:08X} (CCS={} PED={} PP={} speed={})\n",
                        p, ps, (ps>>0)&1, (ps>>1)&1, (ps>>9)&1, (ps>>10)&0xF);
                }
            }
            xhci.poll_ports();
            let new = xhci.devices().len();
            klog_fmt!("xHCI poll: {} ports old={} new={}\n", xhci.n_ports(), old, new);
            if new <= old { continue; }
            for idx in old..new {
                mount_xhci_device(xhci, idx, ctrl_idx);
            }
        }
    }

    USB_DRIVE_COUNT.load(Ordering::Relaxed) != before
}

/// Enumerate and mount a USB device on an xHCI controller.
fn mount_xhci_device(xhci: &mut XhciController, dev_idx: usize, ctrl_index: usize) {
    // Step 1: Enable slot
    let slot_id = match xhci.enable_slot() {
        Ok(id) => id,
        Err(_) => return,
    };

    // Step 2: Address device (assigns address, sets up EP0)
    if xhci.address_device(slot_id).is_err() { return; }

    // Step 3: Get device descriptor via control transfer
    let mut desc_buf = [0u8; 64];
    let setup = UsbSetupPacket {
        bm_request_type: 0x80,
        b_request: 6, // GET_DESCRIPTOR
        w_value: (1u16) << 8, // DEVICE descriptor
        w_index: 0,
        w_length: 64,
    };
    if xhci.control_transfer(slot_id, &setup, &mut desc_buf).is_err() { return; }
    let dev_class = desc_buf[12];
    let dev_subclass = desc_buf[13];
    let dev_protocol = desc_buf[14];
    if dev_class != 0x08 { return; } // not mass storage

    // Step 4: Get configuration descriptor (simplified: assume EP0, EP1 bulk out, EP2 bulk in)
    // In a full implementation we'd iterate the configuration descriptor.
    // For now, use standard USB class: bulk endpoints at address 0x02 (out) and 0x82 (in)

    // Set configuration (configuration value = 1)
    let setup = UsbSetupPacket {
        bm_request_type: 0x00,
        b_request: 9, // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    // Use control transfer through a raw pointer to avoid borrow conflicts
    let xptr: *mut XhciController = xhci as *mut XhciController;
    if unsafe { (*xptr).control_transfer(slot_id, &setup, &mut []) }.is_err() { return; }

    // Configure bulk endpoints
    if unsafe { (*xptr).configure_endpoint_bulk(slot_id, 0x02, 512) }.is_err() { return; }
    if unsafe { (*xptr).configure_endpoint_bulk(slot_id, 0x82, 512) }.is_err() { return; }

    // Update the device entry
    if let Some(dev) = xhci.devices_mut().get_mut(dev_idx) {
        dev.device_class = dev_class;
        dev.device_subclass = dev_subclass;
        dev.device_protocol = dev_protocol;
    }

    // Build block device with inline BOT protocol
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
            cbw[..4].copy_from_slice(&0x43425355u32.to_le_bytes());
            cbw[4..8].copy_from_slice(&self.tag.to_le_bytes()); self.tag += 1;
            cbw[8..12].copy_from_slice(&dlen.to_le_bytes());
            cbw[12] = 0x80;
            cbw[14] = 10;
            cbw[15..25].copy_from_slice(&cdb);
            xhci.bulk_transfer(self.slot_id, self.bulk_out, &mut cbw, UsbDirection::Out, 512)?;
            xhci.bulk_transfer(self.slot_id, self.bulk_in, &mut data, UsbDirection::In, 512)?;
            let mut csw = [0u8; 13];
            xhci.bulk_transfer(self.slot_id, self.bulk_in, &mut csw, UsbDirection::In, 512)?;
            let sig = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
            if sig != 0x53425355 { return Err("bad CSW"); }
            let tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
            if tag != self.tag - 1 { return Err("CSW tag mismatch"); }
            if csw[12] != 0 { return Err("CSW err"); }
            let n = data.len().min(blen);
            buf[..n].copy_from_slice(&data[..n]);
            Ok(())
        }
        fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
            Err("xhci write not impl")
        }
        fn sector_size(&self) -> u32 { self.block_size }
        fn total_sectors(&self) -> u64 { self.total_blocks }
    }

    // Allocate on heap (Box) — closures are avoided by using the struct
    let bdev = XhciBlockDev {
        slot_id, bulk_out: 0x02, bulk_in: 0x82,
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
fn mount_ehci_device(ehci: &mut EhciController, idx: usize, ctrl_index: usize) {
    let dev = unsafe {
        // SAFETY: enumerate_device takes a closure that performs control transfers.
        // We guarantee the closure outlives this call.
        let p: &mut EhciController = &mut *ehci;
        let mut ctrl = |a, ep, s: &UsbSetupPacket, b: &mut [u8]| p.control_transfer(a, ep, s, b);
        nitrogen::usb::hub::enumerate_device(&mut ctrl)
    };
    let dev = match dev { Ok(d) => d, Err(_) => return };
    if !dev.is_mass_storage() { return; }

    let mut bulk_out = 0u8; let mut bulk_in = 0u8;
    for ep in &dev.endpoints {
        if ep.xfer_type() != UsbXferType::Bulk { continue; }
        match ep.direction() {
            UsbDirection::Out => { bulk_out = ep.b_endpoint_address; }
            UsbDirection::In => { bulk_in = ep.b_endpoint_address; }
        }
    }
    if bulk_out == 0 || bulk_in == 0 { return; }

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