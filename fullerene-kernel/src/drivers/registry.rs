//! Driver registry + concrete driver implementations.
//!
//! This module is the **only** place in the kernel that knows about
//! individual driver names.  Every hardware driver lives behind the
//! `Driver` trait and is registered here.  Callers go through
//! `DriverRegistry::match_device` or `poll_all`.
//!
//! # Adding a new driver
//!
//! 1. Write a zero-sized struct implementing `Driver`
//! 2. Add `reg.register("name", Box::new(MyDriver))` to `build_registry()`
//!
//! No other kernel file needs to change.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
#[allow(unused_imports)]
use core::sync::atomic::AtomicBool;
use spin::Mutex;

use genome::block::BlockDevice;
use nitrogen::driver_api::{Driver, DriverBox};
use nitrogen::pci::PciDevice;
use nitrogen::DriverContext;

// ────────────────────────────────────────────────────────────
//  Re-exports (for external callers such as shell / GUI)
// ────────────────────────────────────────────────────────────

pub use nitrogen::driver_api::DriverRegistry;

// ── USB storage state (formerly drivers/usb_storage.rs) ────

// Shared USB context static used by all USB access paths
#[cfg(not(nitrogen_no_usb))]
static USB_CTX: Mutex<Option<nitrogen::usb::context::USBContext>> = Mutex::new(None);

/// Tracks how many USB disks we have registered in the block device registry.
/// Used by `poll_usb` to detect new devices without scanning the registry.
#[cfg(not(nitrogen_no_usb))]
static LAST_REGISTERED_USB_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Access the USB controller context.  Panics if not initialised.
#[cfg(not(nitrogen_no_usb))]
pub fn with_ctx<F, R>(f: F) -> R
where
    F: FnOnce(&mut nitrogen::usb::context::USBContext) -> R,
{
    let mut guard = USB_CTX.lock();
    let ctx = guard.as_mut().expect("USB context not initialized");
    f(ctx)
}

#[cfg(nitrogen_no_usb)]
/// Dummy USB context for when USB support is not compiled in.
pub struct DummyUsbContext;

#[cfg(nitrogen_no_usb)]
impl DummyUsbContext {
    pub fn disks(&self) -> &[DummyUsbDisk] {
        &[]
    }
}

#[cfg(nitrogen_no_usb)]
pub struct DummyUsbDisk {
    pub ctrl_type: &'static str,
    pub dev_addr: u8,
    pub ep_out: u8,
    pub ep_in: u8,
    pub block_size: u32,
    pub total_blocks: u64,
}

#[cfg(nitrogen_no_usb)]
pub fn with_ctx<F, R>(_f: F) -> R
where
    F: FnOnce(&mut DummyUsbContext) -> R,
{
    panic!("USB support not compiled in");
}

// ── SD card state (formerly drivers/sd_card.rs) ────────────

#[cfg(not(nitrogen_no_storage))]
pub static SD_PROBED: AtomicBool = AtomicBool::new(false);

// ────────────────────────────────────────────────────────────
//  Driver implementations
// ────────────────────────────────────────────────────────────

// -- AHCI ----------------------------------------------------

#[cfg(not(nitrogen_no_storage))]
pub struct AhciDriver;

#[cfg(not(nitrogen_no_storage))]
impl Driver for AhciDriver {
    fn pci_class(&self) -> Option<(u8, u8)> {
        Some((0x01, 0x06)) // mass-storage, SATA (AHCI)
    }
    fn probe(&self, ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        nitrogen::storage::ahci::init(ctx);
        DriverBox::None
    }
}

// -- NVMe ----------------------------------------------------

#[cfg(not(nitrogen_no_storage))]
pub struct NvmeDriver;

#[cfg(not(nitrogen_no_storage))]
impl Driver for NvmeDriver {
    fn pci_class(&self) -> Option<(u8, u8)> {
        Some((0x01, 0x08)) // mass-storage, NVM Express
    }
    fn probe(&self, ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        nitrogen::storage::nvme::init(ctx);
        DriverBox::None
    }
}

// -- USB storage (formerly usb_storage::init) -----------------

#[cfg(not(nitrogen_no_usb))]
pub struct UsbStorageDriver;

#[cfg(not(nitrogen_no_usb))]
impl Driver for UsbStorageDriver {
    fn pci_class(&self) -> Option<(u8, u8)> {
        Some((0x0C, 0x03)) // USB host controller
    }
    fn probe(&self, _ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        crate::boot_stage::draw_boot_label(b"USB STORAGE");
        nitrogen::debug::set_hint_callback(crate::boot_stage::draw_step_hint);

        let mut ctx = nitrogen::usb::context::USBContext::new(
            &crate::driver_context_impl::KernelDriverContext,
        );
        if let Err(e) = ctx.enable() {
            log::warn!("USB: enable failed: {:?}", e);
        }
        // Store in the global singleton for later polling.
        crate::drivers::registry::init_usb_ctx(ctx);
        // Initial poll to enumerate devices (no mount — use `mount` command).
        crate::boot_stage::draw_step_hint(b"usb_pol");
        crate::drivers::registry::usb_poll_and_register();
        crate::boot_stage::draw_step_hint(b"usb_reg");
        DriverBox::None
    }
}

/// Initialise the USB driver (probe phase — called from Driver).
#[cfg(not(nitrogen_no_usb))]
pub(crate) fn init_usb_ctx(ctx: nitrogen::usb::context::USBContext) {
    *USB_CTX.lock() = Some(ctx);
}

// -- SD card (formerly sd_card::init) -------------------------

#[cfg(not(nitrogen_no_storage))]
pub struct SdCardDriver;

#[cfg(not(nitrogen_no_storage))]
impl Driver for SdCardDriver {
    fn pci_class(&self) -> Option<(u8, u8)> {
        Some((0xFF, 0x00)) // vendor-specific (RTSX)
    }
    fn probe(&self, ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        crate::boot_stage::draw_boot_label(b"SD CARD");
        nitrogen::storage::rtsx::init(ctx);
        if nitrogen::storage::rtsx::is_present() {
            log::info!("SD: RTSX controller found");
        } else {
            log::info!("SD: no RTSX controller found");
        }
        DriverBox::None
    }
}

// ────────────────────────────────────────────────────────────
//  Registry construction
// ────────────────────────────────────────────────────────────

/// Populate the `DriverRegistry` with every available driver.
pub fn build_registry() -> DriverRegistry {
    let mut reg = DriverRegistry::new();
    #[cfg(not(nitrogen_no_storage))]
    {
        reg.register("ahci", Box::new(AhciDriver));
        reg.register("nvme", Box::new(NvmeDriver));
        reg.register("sd_card", Box::new(SdCardDriver));
    }
    #[cfg(not(nitrogen_no_usb))]
    reg.register("usb_storage", Box::new(UsbStorageDriver));
    // Future: virtio_gpu, iwlwifi, hda, …
    reg
}

// ────────────────────────────────────────────────────────────
//  USB polling & block device registration
// ────────────────────────────────────────────────────────────

/// `UsbBlockDevice` — a `BlockDevice` that talks to a USB mass-storage
/// device via the BOT (Bulk-Only Transport) protocol.  No block I/O
/// happens at construction time — only when `read_sectors`/`write_sectors`
/// is called (i.e. on `mount`).
#[cfg(not(nitrogen_no_usb))]
struct UsbBlockDevice {
    ctrl_type: &'static str,
    ctrl_idx: usize,
    dev_addr: u8,
    ep_out: u8,
    ep_out_mps: u16,
    ep_in: u8,
    ep_in_mps: u16,
    block_size: u32,
    total_blocks: u64,
    tag: u32,
}

#[cfg(not(nitrogen_no_usb))]
unsafe impl Send for UsbBlockDevice {}

#[cfg(not(nitrogen_no_usb))]
impl BlockDevice for UsbBlockDevice {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        with_ctx(|ctx| {
            ctx.bot_read(
                self.ctrl_type, self.ctrl_idx, self.dev_addr,
                self.ep_out, self.ep_out_mps, self.ep_in, self.ep_in_mps,
                lba, count, self.block_size, buf, &mut self.tag,
            )
        })
    }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        with_ctx(|ctx| {
            ctx.bot_write(
                self.ctrl_type, self.ctrl_idx, self.dev_addr,
                self.ep_out, self.ep_out_mps, self.ep_in, self.ep_in_mps,
                lba, count, self.block_size, buf, &mut self.tag,
            )
        })
    }
    fn sector_size(&self) -> u32 { self.block_size }
    fn total_sectors(&self) -> u64 { self.total_blocks }
}

/// Poll USB controller once and register newly-discovered devices
/// in the block device registry (no mount).
/// Returns `true` if a new device was registered.
#[cfg(not(nitrogen_no_usb))]
pub fn poll_usb() -> bool {
    let before = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed);
    {
        let mut guard = with_ctx_inner();
        if let Some(ctx) = guard.as_mut() {
            ctx.poll();
        }
    }
    register_pending_usb();
    let changed = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed) != before;
    if changed {
        let _ = crate::klog::flush_to_vfs();
    }
    changed
}

#[cfg(nitrogen_no_usb)]
pub fn poll_usb() -> bool {
    false
}

/// Initial USB poll with retries, then register block devices.
#[cfg(not(nitrogen_no_usb))]
fn usb_poll_and_register() {
    for i in 0..8 {
        {
            let mut guard = with_ctx_inner();
            if let Some(ctx) = guard.as_mut() {
                let disk_count_before = ctx.disks().len();
                log::info!(
                    "USB: poll #{}, disks before: {}",
                    i + 1, disk_count_before
                );
                ctx.poll();
                let disk_count_after = ctx.disks().len();
                log::info!(
                    "USB: poll #{}, disks after: {}",
                    i + 1, disk_count_after
                );
                if !ctx.disks().is_empty() {
                    log::info!("USB: device detected after {} retries", i + 1);
                    break;
                }
            }
        }
        nitrogen::timing::delay_ms(250);
    }
    register_pending_usb();
}

/// Full USB re-enumeration (clear + re-scan).  Does NOT mount.
#[cfg(not(nitrogen_no_usb))]
pub fn poll_usb_all() -> bool {
    // Only unregister USB-owned block devices (usbN pattern)
    let names = crate::devfs::list_block_device_names();
    for name in names {
        if name.starts_with("usb") {
            crate::devfs::unregister_block_device(&name);
        }
    }
    LAST_REGISTERED_USB_COUNT.store(0, Ordering::Relaxed);

    use crate::driver_context_impl::KernelDriverContext;
    let mut ctx = nitrogen::usb::context::USBContext::new(&KernelDriverContext);
    if let Err(e) = ctx.enable() {
        log::warn!("USB: enable failed during re-enumeration: {:?}", e);
    }
    init_usb_ctx(ctx);
    usb_poll_and_register();
    let registered = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed) > 0;
    let _ = crate::klog::flush_to_vfs();
    registered
}

/// Access the inner USB context static for poll operations.
#[cfg(not(nitrogen_no_usb))]
fn with_ctx_inner() -> spin::MutexGuard<'static, Option<nitrogen::usb::context::USBContext>> {
    USB_CTX.lock()
}

/// Register newly discovered USB disks as block devices under `/dev/usbN`.
///
/// Skips disks already known to the USB context (identified by comparing
/// the number of registered block devices against `ctx.disks().len()`).
#[cfg(not(nitrogen_no_usb))]
fn register_pending_usb() {
    let (disks, _new_count) = {
        let guard = with_ctx_inner();
        let ctx = match guard.as_ref() {
            Some(c) => c,
            None => return,
        };
        let total = ctx.disks().len();
        let already = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed);
        if total <= already {
            return;
        }
        (ctx.disks()[already..].to_vec(), total)
    };

    for disk in &disks {
        let idx = LAST_REGISTERED_USB_COUNT.fetch_add(1, Ordering::Relaxed);
        let dev_name = alloc::format!("usb{}", idx);

        let bdev = Box::new(UsbBlockDevice {
            ctrl_type: disk.ctrl_type,
            ctrl_idx: disk.ctrl_idx,
            dev_addr: disk.dev_addr,
            ep_out: disk.ep_out,
            ep_out_mps: disk.ep_out_mps,
            ep_in: disk.ep_in,
            ep_in_mps: disk.ep_in_mps,
            block_size: disk.block_size,
            total_blocks: disk.total_blocks,
            tag: 1,
        });

        crate::klog_fmt!("USB: registered /dev/{} (bulk-only)\n", dev_name);
        crate::devfs::register_block_device(dev_name.clone(), bdev);

        let _ = crate::contexts::vfs::mkdir("/dev");
        let _ = crate::contexts::vfs::create(&alloc::format!("/dev/{}", dev_name));
    }
    let _ = crate::klog::flush_to_vfs();
}

// ────────────────────────────────────────────────────────────
//  SD card — probe & register (formerly drivers/sd_card.rs)
// ────────────────────────────────────────────────────────────

/// Probe and register an SD card as a block device (no mount).
#[cfg(not(nitrogen_no_storage))]
pub fn sd_probe_and_register() -> bool {
    if SD_PROBED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        crate::klog_fmt!("SD card: already probed\n");
        return true;
    }

    if !nitrogen::storage::rtsx::is_present() {
        crate::klog_fmt!("SD card: no controller\n");
        SD_PROBED.store(false, Ordering::Release);
        return false;
    }

    match nitrogen::storage::rtsx::init_sd_card() {
        Ok(()) => crate::klog_fmt!("SD card: initialised\n"),
        Err(e) => {
            crate::klog_fmt!("SD card: init failed — {}\n", e);
            SD_PROBED.store(false, Ordering::Release);
            return false;
        }
    }

    let info = match nitrogen::storage::rtsx::sd_card_info() {
        Some(i) => i,
        None => {
            crate::klog_fmt!("SD card: no card info\n");
            SD_PROBED.store(false, Ordering::Release);
            return false;
        }
    };

    crate::klog_fmt!(
        "SD card: {:?} {} sectors {} bytes/sector\n",
        info.card_type, info.total_blocks, info.block_size
    );

    let bdev = Box::new(SdBlockDev {
        block_size: info.block_size,
        total_blocks: info.total_blocks,
    });

    let dev_name = alloc::format!("sd{}", 0);
    crate::klog_fmt!("SD card: registered /dev/{}\n", dev_name);
    crate::devfs::register_block_device(dev_name.clone(), bdev);

    let _ = crate::contexts::vfs::mkdir("/dev");
    let _ = crate::contexts::vfs::create(&alloc::format!("/dev/{}", dev_name));
    true
}

#[cfg(nitrogen_no_storage)]
pub fn sd_probe_and_register() -> bool {
    false
}

#[cfg(not(nitrogen_no_storage))]
struct SdBlockDev {
    block_size: u32,
    total_blocks: u64,
}

#[cfg(not(nitrogen_no_storage))]
unsafe impl Send for SdBlockDev {}

#[cfg(not(nitrogen_no_storage))]
impl BlockDevice for SdBlockDev {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        nitrogen::storage::rtsx::read_sectors(lba, count, buf)
    }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        nitrogen::storage::rtsx::write_sectors(lba, count, buf)
    }
    fn sector_size(&self) -> u32 { self.block_size }
    fn total_sectors(&self) -> u64 { self.total_blocks }
}

// ── Compatibility: sd_probe_and_mount → delegates to register ──
#[cfg(not(nitrogen_no_storage))]
pub fn sd_probe_and_mount() -> bool {
    let ok = sd_probe_and_register();
    let _ = crate::klog::flush_to_vfs();
    ok
}

#[cfg(nitrogen_no_storage)]
pub fn sd_probe_and_mount() -> bool {
    false
}

// ────────────────────────────────────────────────────────────
//  DriverRegistry extension — poll_all
// ────────────────────────────────────────────────────────────

/// Run periodic tasks for all registered drivers (USB poll, etc.).
///
/// Call from a background timer tick.  Returns `true` if any
/// driver reported a state change.
pub fn poll_all(_registry: &DriverRegistry) -> bool {
    let mut changed = false;
    #[cfg(not(nitrogen_no_usb))]
    {
        changed |= poll_usb();
    }
    changed
}
