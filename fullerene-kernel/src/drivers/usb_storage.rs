//! USB mass-storage driver — real EHCI enumeration + FAT mount + hotplug poll.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use nitrogen::usb::ehci::EhciController;
use nitrogen::usb::{UsbDirection, UsbSetupPacket, UsbXferType};
use nitrogen::DriverContext;
use spin::Mutex;

use crate::drivers::fat::{BlockDevice, FatFileSystem};

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
    ehci: *mut EhciController,
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
        let ehci = unsafe { &mut *self.ehci };
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
        let ehci = unsafe { &mut *self.ehci };
        let mut cdb = [0u8; 10];
        cdb[0] = 0x2A;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7..9].copy_from_slice(&count.to_be_bytes());
        self.bot_xfer(&cdb, Some(&mut buf.to_vec()), false, ehci)
    }

    fn sector_size(&self) -> u32 { self.block_size }
    fn total_sectors(&self) -> u64 { self.total_blocks }
}

// ── EHCI controller storage ──────────────────────────────────

static EHCI_CONTROLLERS: Mutex<Vec<EhciController>> = Mutex::new(Vec::new());
static EHCI_INITIALIZED: AtomicBool = AtomicBool::new(false);
use core::sync::atomic::AtomicBool;

pub fn init() {
    let _ = crate::vfs::mkdir("/mnt");
    init_controllers();
}

fn init_controllers() {
    if EHCI_INITIALIZED.swap(true, Ordering::SeqCst) { return; }
    use nitrogen::pci::PciConfigSpace;
    use crate::driver_context_impl::KernelDriverContext;

    let mut controllers = EHCI_CONTROLLERS.lock();
    for bus in 0u8..=0 {
        for slot in 0u8..32 {
            for func in 0u8..8 {
                let cfg = match PciConfigSpace::read_from_device(bus, slot, func) {
                    Some(c) => c,
                    None => { if func == 0 { break; } continue; }
                };
                if cfg.class_code != 0x0C || cfg.subclass != 0x03 || cfg.prog_if != 0x20 { continue; }

                let bar0 = PciConfigSpace::read_config_dword(bus, slot, func, 0x10) & 0xFFFF_FFF0;
                if bar0 == 0 { continue; }
                let mmio_virt = KernelDriverContext.phys_to_virt(bar0 as u64) as *mut u8;
                if mmio_virt.is_null() { continue; }

                let cmd = PciConfigSpace::read_config_word(bus, slot, func, 4);
                PciConfigSpace::write_config_word_raw(bus, slot, func, 4, cmd | 0x06);

                if let Some(hc) = EhciController::new(mmio_virt, &KernelDriverContext) {
                    controllers.push(hc);
                }
            }
        }
    }
}

/// Poll EHCI ports. Returns true if a new drive was mounted.
pub fn poll_usb() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);
    let mut ctlrs = EHCI_CONTROLLERS.lock();

    for ehci in ctlrs.iter_mut() {
        ehci.start();
        let old = ehci.devices().len();
        ehci.poll_ports();
        if ehci.devices().len() <= old { continue; }

        let ndevs = ehci.devices().len();
        for idx in old..ndevs {
            let eptr: *mut EhciController = ehci as *mut EhciController;

            // Enumerate
            let dev = unsafe {
                let p = &mut *eptr;
                let mut ctrl = |a, ep, s: &UsbSetupPacket, b: &mut [u8]| p.control_transfer(a, ep, s, b);
                nitrogen::usb::hub::enumerate_device(&mut ctrl)
            };
            let dev = match dev { Ok(d) => d, Err(_) => continue };
            if !dev.is_mass_storage() { continue; }

            // Find bulk endpoints
            let mut bulk_out = 0u8; let mut bulk_in = 0u8;
            for ep in &dev.endpoints {
                if ep.xfer_type() != UsbXferType::Bulk { continue; }
                match ep.direction() {
                    UsbDirection::Out => { bulk_out = ep.b_endpoint_address; }
                    UsbDirection::In => { bulk_in = ep.b_endpoint_address; }
                }
            }
            if bulk_out == 0 || bulk_in == 0 { continue; }

            // Build block device with default 512-byte block size
            let bdev = UsbBlockDevice {
                dev_addr: dev.address, bulk_out, bulk_in,
                block_size: 512, total_blocks: 0, tag: 1, ehci: eptr,
            };

            let mp = alloc::format!("/mnt/usb-{}", USB_DRIVES.lock().len() + 1);
            match FatFileSystem::new(Box::new(bdev)) {
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
                Err(e) => { petroleum::serial::serial_log(format_args!("USB mount: {}\n", e)); }
            }
        }
    }
    USB_DRIVE_COUNT.load(Ordering::Relaxed) != before
}
