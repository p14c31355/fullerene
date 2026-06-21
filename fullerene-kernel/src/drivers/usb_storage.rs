//! USB mass-storage integration — FAT mount + hotplug poll.
//!
//! USB controller discovery, enumeration, and BOT protocol are in
//! [`nitrogen::usb::usb_bus`].  This module only handles VFS/FAT
//! integration and platform-specific polling delays.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use nitrogen::usb::usb_bus::{UsbBus, bot_read_sectors, bot_write_sectors, enumerate_mass_storage};
use nitrogen::usb::{UsbDirection, UsbXferType, UsbDevice};
use nitrogen::usb::host_controller::HostController;
use spin::Mutex;

use crate::drivers::fat::{BlockDevice, FatFileSystem};
use crate::klog_fmt;

pub static USB_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
}

// ── Controller storage ──────────────────────────────────────

static USB_BUS: spin::Mutex<Option<UsbBus>> = spin::Mutex::new(None);
static CTRL_INITIALIZED: AtomicBool = AtomicBool::new(false);

fn with_bus<F, R>(f: F) -> R
where
    F: FnOnce(&mut UsbBus) -> R,
{
    let mut guard = USB_BUS.lock();
    let bus = guard.as_mut().expect("USB bus not initialized");
    f(bus)
}

pub fn init() {
    let _ = crate::vfs::mkdir("/mnt");
    init_controllers();

    // Phase 1: Immediate poll
    poll_usb();

    // Phase 2: Short delay → re-poll for xHCI devices needing
    // additional time after HCRST.
    delay(300_000);
    if poll_usb() {
        debug_usb();
        return;
    }

    // Phase 3: Longer delay → re-poll
    delay(500_000);
    if poll_usb() {
        debug_usb();
        return;
    }

    // Phase 4: Force re-poll with fresh ports_done
    delay(200_000);
    poll_usb_all();
    debug_usb();
}

fn delay(iterations: u32) {
    for _ in 0..iterations {
        nitrogen::port::PortWriter::<u8>::new(0x80).write_safe(0u8);
    }
}

fn init_controllers() {
    if CTRL_INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
    use crate::driver_context_impl::KernelDriverContext;
    let mut guard = USB_BUS.lock();
    let mut bus = UsbBus::new();
    bus.init_controllers(&KernelDriverContext);
    *guard = Some(bus);
}

/// Debug dump USB controller state
pub fn debug_usb() {
    klog_fmt!("=== USB DEBUG ===\n");
    with_bus(|bus| {
    for (i, ehci) in bus.ehci.iter().enumerate() {
        klog_fmt!("EHCI[{}]: {} ports\n", i, ehci.n_ports());
        for p in 0..(ehci.n_ports().min(4)) {
            let ps = ehci.read_portsc(p);
            klog_fmt!("  PORTSC[{}]=0x{:08X} CCS={} PE={}\n", p, ps, ps & 1, (ps >> 2) & 1);
        }
    }

    for (i, xhci) in bus.xhci.iter().enumerate() {
        klog_fmt!("xHCI[{}] ppc={} n_ports={} max_slots={} ports_done={:#x} legacy={}\n",
            i, xhci.ppc_enabled(), xhci.n_ports(), xhci.max_slots(),
            xhci.ports_done_mask(), xhci.legacy_handoff_done());
        for p in 0..xhci.n_ports() {
            let ps = xhci.read_portsc(p);
            if ps == 0xFFFF { continue; }
            klog_fmt!("xHCI PORTSC[{}]={:#x} CCS={} PED={} PLS={} PP={} PR={} speed={}\n",
                p, ps, ps & 1, (ps >> 1) & 1, (ps >> 5) & 0xF,
                (ps >> 9) & 1, (ps >> 4) & 1, (ps >> 10) & 0xF);
        }
    }
    });
    klog_fmt!("=== USB END ===\n");
}

// ── Polling ────────────────────────────────────────────────

pub fn poll_usb() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);
    let (ehci_pending, xhci_pending) = with_bus(|bus| bus.poll());

    // Mount new devices without holding the bus lock
    for (ctrl_idx, idx) in ehci_pending {
        mount_ehci_device(ctrl_idx, idx);
    }
    for (ctrl_idx, idx) in xhci_pending {
        mount_xhci_device(ctrl_idx, idx);
    }

    USB_DRIVE_COUNT.load(Ordering::Relaxed) != before
}

pub fn poll_usb_all() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);

    // Unmount existing drives
    let mps: Vec<String> = USB_DRIVES.lock().iter().map(|d| d.mount_point.clone()).collect();
    for mp in &mps {
        let _ = crate::vfs::unmount(mp);
    }
    USB_DRIVES.lock().clear();
    USB_DRIVE_COUNT.store(0, Ordering::Relaxed);

    let pending = with_bus(|bus| bus.poll_all());

    for (ctrl_idx, dev_idx) in pending {
        mount_xhci_device(ctrl_idx, dev_idx);
    }

    USB_DRIVE_COUNT.load(Ordering::Relaxed) != before
}

// ── Mount helpers ──────────────────────────────────────────

fn mount_ehci_device(ctrl_index: usize, dev_idx: usize) {
    let dev;
    let mut bulk_out = 0u8;
    let mut bulk_in = 0u8;
    {
        let mut guard = USB_BUS.lock();
        let bus = guard.as_mut().unwrap();
        let ehci = &mut bus.ehci[ctrl_index];
        ehci.reset_pools();

        let result = {
            let mut ctrl_fn = |addr, ep, setup: &nitrogen::usb::UsbSetupPacket, buf: &mut [u8]| {
                ehci.control_transfer(addr, ep, setup, buf)
            };
            nitrogen::usb::hub::enumerate_device(&mut ctrl_fn)
        };
        dev = match result {
            Ok(d) => d,
            Err(_) => return,
        };
        if !dev.is_mass_storage() {
            return;
        }

        if let Some(slot) = ehci.devices_mut().get_mut(dev_idx) {
            *slot = dev.clone();
        }

        for ep in &dev.endpoints {
            if ep.xfer_type() != UsbXferType::Bulk {
                continue;
            }
            match ep.direction() {
                UsbDirection::Out => bulk_out = ep.b_endpoint_address,
                UsbDirection::In => bulk_in = ep.b_endpoint_address,
            }
        }
    }

    if bulk_out == 0 || bulk_in == 0 {
        return;
    }

    mount_fat("EHCI", dev.address as u32, bulk_out, bulk_in, "EHCI", ctrl_index);
}

fn mount_xhci_device(ctrl_index: usize, dev_idx: usize) {
    let slot_id: u32;
    let ep_out: u8;
    let ep_in: u8;
    {
        let mut guard = USB_BUS.lock();
        let bus = guard.as_mut().unwrap();
        let xhci = &mut bus.xhci[ctrl_index];

        // Enable slot + address device
        slot_id = match xhci.enable_slot() {
            Ok(id) => id,
            Err(_) => return,
        };
        if xhci.address_device(slot_id).is_err() {
            return;
        }

        // Enumerate using the generic helper
        let dev_addr = slot_id as u8;
        let result = enumerate_mass_storage(&mut **xhci, dev_addr, dev_idx);
        let (out_ep, in_ep, _blk) = match result {
            Ok(v) => v,
            Err(e) => {
                klog_fmt!("USB xHCI enum err: {}\n", e);
                return;
            }
        };
        ep_out = out_ep;
        ep_in = in_ep;

        // Configure bulk endpoints
        if xhci.configure_endpoint_bulk(slot_id, ep_out, 512).is_err() {
            return;
        }
        if xhci.configure_endpoint_bulk(slot_id, ep_in, 512).is_err() {
            return;
        }
    }

    mount_fat("xHCI", slot_id, ep_out, ep_in, "xHCI", ctrl_index);
}

fn mount_fat(label: &str, dev_id: u32, ep_out: u8, ep_in: u8, ctrl_type: &'static str, ctrl_idx: usize) {
    struct BotBlockDev {
        dev_id: u32,
        ep_out: u8,
        ep_in: u8,
        block_size: u32,
        total_blocks: u64,
        tag: u32,
        ctrl_type: &'static str,
        ctrl_idx: usize,
    }
    unsafe impl Send for BotBlockDev {}

    impl BlockDevice for BotBlockDev {
        fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
            let mut guard = USB_BUS.lock();
            let bus = guard.as_mut().unwrap();
            if self.ctrl_type == "xHCI" {
                bot_read_sectors(&mut *bus.xhci[self.ctrl_idx], self.dev_id as u8, self.ep_out, self.ep_in,
                    lba, count, self.block_size, buf, &mut self.tag)
            } else {
                bot_read_sectors(&mut *bus.ehci[self.ctrl_idx], self.dev_id as u8, self.ep_out, self.ep_in,
                    lba, count, self.block_size, buf, &mut self.tag)
            }
        }

        fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
            let mut guard = USB_BUS.lock();
            let bus = guard.as_mut().unwrap();
            if self.ctrl_type == "xHCI" {
                bot_write_sectors(&mut *bus.xhci[self.ctrl_idx], self.dev_id as u8, self.ep_out, self.ep_in,
                    lba, count, self.block_size, buf, &mut self.tag)
            } else {
                bot_write_sectors(&mut *bus.ehci[self.ctrl_idx], self.dev_id as u8, self.ep_out, self.ep_in,
                    lba, count, self.block_size, buf, &mut self.tag)
            }
        }

        fn sector_size(&self) -> u32 { self.block_size }
        fn total_sectors(&self) -> u64 { self.total_blocks }
    }

    let bdev = BotBlockDev {
        dev_id,
        ep_out,
        ep_in,
        block_size: 512,
        total_blocks: 0,
        tag: 1,
        ctrl_type,
        ctrl_idx,
    };

    let mp = alloc::format!("/mnt/usb-{}", USB_DRIVES.lock().len() + 1);
    match FatFileSystem::from_device(Box::new(bdev)) {
        Ok(fs) => {
            let _ = crate::vfs::mkdir(&mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                let n = USB_DRIVES.lock().len() + 1;
                USB_DRIVES.lock().push(UsbDrive {
                    name: alloc::format!("USB Drive {}", n),
                    mount_point: mp,
                });
                USB_DRIVE_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(e) => {
            klog_fmt!("USB {} mount: {}\n", label, e);
        }
    }
}