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
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use nitrogen::driver_api::{Driver, DriverBox};
use nitrogen::pci::PciDevice;
use nitrogen::DriverContext;

// ────────────────────────────────────────────────────────────
//  Re-exports (for external callers such as shell / GUI)
// ────────────────────────────────────────────────────────────

pub use nitrogen::driver_api::DriverRegistry;

// ── USB storage state (formerly drivers/usb_storage.rs) ────

pub static USB_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
}

/// Access the USB controller context.  Panics if not initialised.
pub fn with_ctx<F, R>(f: F) -> R
where
    F: FnOnce(&mut nitrogen::usb::context::USBContext) -> R,
{
    use nitrogen::usb::context::USBContext;
    static USB_CTX: Mutex<Option<USBContext>> = Mutex::new(None);
    let mut guard = USB_CTX.lock();
    let ctx = guard.as_mut().expect("USB context not initialized");
    f(ctx)
}

// ── SD card state (formerly drivers/sd_card.rs) ────────────

pub static SD_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static SD_DRIVES: Mutex<Vec<SdDrive>> = Mutex::new(Vec::new());
pub static SD_PROBED: AtomicBool = AtomicBool::new(false);

pub struct SdDrive {
    pub name: String,
    pub mount_point: String,
}

// ────────────────────────────────────────────────────────────
//  Driver implementations
// ────────────────────────────────────────────────────────────

// -- AHCI ----------------------------------------------------

pub struct AhciDriver;

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

pub struct NvmeDriver;

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

pub struct UsbStorageDriver;

impl Driver for UsbStorageDriver {
    fn pci_class(&self) -> Option<(u8, u8)> {
        Some((0x0C, 0x03)) // USB host controller
    }
    fn probe(&self, _ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        crate::boot_stage::draw_boot_label(b"USB STORAGE");
        let _ = crate::contexts::vfs::mkdir("/mnt");

        use crate::driver_context_impl::KernelDriverContext;
        let mut ctx = nitrogen::usb::context::USBContext::new(&KernelDriverContext);
        let _ = ctx.enable();
        // Store in the global singleton for later polling.
        crate::drivers::registry::init_usb_ctx(ctx);
        // Initial poll + mount.
        crate::drivers::registry::usb_poll_and_mount();
        DriverBox::None
    }
}

/// Initialise the USB driver (probe phase — called from Driver).
pub(crate) fn init_usb_ctx(ctx: nitrogen::usb::context::USBContext) {
    use nitrogen::usb::context::USBContext;
    static USB_CTX: Mutex<Option<USBContext>> = Mutex::new(None);
    *USB_CTX.lock() = Some(ctx);
}

// -- SD card (formerly sd_card::init) -------------------------

pub struct SdCardDriver;

impl Driver for SdCardDriver {
    fn pci_class(&self) -> Option<(u8, u8)> {
        Some((0xFF, 0x00)) // vendor-specific (RTSX)
    }
    fn probe(&self, _ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        crate::boot_stage::draw_boot_label(b"SD CARD");
        use crate::driver_context_impl::KernelDriverContext;
        nitrogen::storage::rtsx::init(&KernelDriverContext);
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
    reg.register("ahci", Box::new(AhciDriver));
    reg.register("nvme", Box::new(NvmeDriver));
    reg.register("usb_storage", Box::new(UsbStorageDriver));
    reg.register("sd_card", Box::new(SdCardDriver));
    // Future: virtio_gpu, iwlwifi, hda, …
    reg
}

// ────────────────────────────────────────────────────────────
//  USB polling
// ────────────────────────────────────────────────────────────

/// Mount retry backoff state keyed by mount point.
static MOUNT_RETRY_STATE: Mutex<BTreeMap<String, MountRetryState>> =
    Mutex::new(BTreeMap::new());

struct MountRetryState {
    failure_count: usize,
    next_retry_tick: u64,
}

/// Poll USB controller once and mount newly-discovered devices.
/// Returns `true` if a new drive was mounted.
pub fn poll_usb() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);
    {
        let mut guard = crate::drivers::registry::with_ctx_inner();
        if let Some(ctx) = guard.as_mut() {
            ctx.poll();
        }
    }
    mount_pending();
    let changed = USB_DRIVE_COUNT.load(Ordering::Relaxed) != before;
    if changed {
        let _ = crate::klog::flush_to_vfs();
    }
    changed
}

/// Full USB re-enumeration (clear + re-scan).
fn usb_poll_and_mount() {
    // Delay-polls for devices (same as original init).
    use core::sync::atomic::Ordering;
    for i in 0..8 {
        let count_before = USB_DRIVE_COUNT.load(Ordering::Relaxed);
        {
            let mut guard = crate::drivers::registry::with_ctx_inner();
            if let Some(ctx) = guard.as_mut() {
                let disk_count_before = ctx.disks().len();
                log::info!(
                    "USB: poll #{}, drives before: count={}, disks={}",
                    i + 1, count_before, disk_count_before
                );
                ctx.poll();
                let disk_count_after = ctx.disks().len();
                log::info!(
                    "USB: poll #{}, drives after: count={}, disks={}",
                    i + 1,
                    count_before,
                    disk_count_after
                );
                if !ctx.disks().is_empty() {
                    log::info!("USB: device detected after {} retries", i + 1);
                    break;
                }
            }
        }
        nitrogen::timing::delay_ms(250);
    }
    mount_pending();
}

/// Re-poll all controllers from scratch.
pub fn poll_usb_all() -> bool {
    // Unmount existing drives.
    let mps: Vec<String> = USB_DRIVES
        .lock()
        .iter()
        .map(|d| d.mount_point.clone())
        .collect();
    for mp in &mps {
        let _ = crate::contexts::vfs::unmount(mp);
    }
    USB_DRIVES.lock().clear();
    USB_DRIVE_COUNT.store(0, Ordering::Relaxed);
    MOUNT_RETRY_STATE.lock().clear();

    use crate::driver_context_impl::KernelDriverContext;
    let mut ctx = nitrogen::usb::context::USBContext::new(&KernelDriverContext);
    let _ = ctx.enable();
    crate::drivers::registry::init_usb_ctx(ctx);
    usb_poll_and_mount();
    let mounted = USB_DRIVE_COUNT.load(Ordering::Relaxed) > 0;
    let _ = crate::klog::flush_to_vfs();
    mounted
}

/// Access the inner USB context static for poll operations.
fn with_ctx_inner() -> spin::MutexGuard<'static, Option<nitrogen::usb::context::USBContext>> {
    use nitrogen::usb::context::USBContext;
    static USB_CTX: Mutex<Option<USBContext>> = Mutex::new(None);
    USB_CTX.lock()
}

/// Mount newly enumerated USB candidates after releasing the controller lock.
fn mount_pending() {
    let current_tick = solvent::GLOBAL_TICK.load(Ordering::Relaxed);
    let mounted: Vec<String> = USB_DRIVES.lock().iter().map(|d| d.mount_point.clone()).collect();
    let candidates: Vec<nitrogen::usb::disk::Disk> =
        with_ctx_inner()
            .as_ref()
            .map(|ctx| {
                ctx.disks()
                    .iter()
                    .filter(|disk| !mounted.contains(&disk.mount_point))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

    let mut retry_state = MOUNT_RETRY_STATE.lock();
    for mut disk in candidates {
        if let Some(state) = retry_state.get(&disk.mount_point) {
            if current_tick < state.next_retry_tick {
                continue;
            }
        }
        if platform_mount_fat(&mut disk) {
            retry_state.remove(&disk.mount_point);
        } else {
            let state = retry_state
                .entry(disk.mount_point.clone())
                .or_insert(MountRetryState {
                    failure_count: 0,
                    next_retry_tick: 0,
                });
            state.failure_count += 1;
            let backoff_ticks =
                (50 * (1 << state.failure_count.min(8))).min(10_000);
            state.next_retry_tick = current_tick + backoff_ticks;
        }
    }
}

// ── Platform FAT-mount callback ─────────────────────────────

fn platform_mount_fat(disk: &mut nitrogen::usb::disk::Disk) -> bool {
    let ctrl_type = disk.ctrl_type;
    let ctrl_idx = disk.ctrl_idx;
    let dev_addr = disk.dev_addr;
    let ep_out = disk.ep_out;
    let ep_out_mps = disk.ep_out_mps;
    let ep_in = disk.ep_in;
    let ep_in_mps = disk.ep_in_mps;

    struct BotBlockDev {
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
    unsafe impl Send for BotBlockDev {}

    impl crate::drivers::fat::BlockDevice for BotBlockDev {
        fn read_sectors(
            &mut self,
            lba: u32,
            count: u16,
            buf: &mut [u8],
        ) -> Result<(), &'static str> {
            crate::drivers::registry::with_ctx(|ctx| {
                ctx.bot_read(
                    self.ctrl_type,
                    self.ctrl_idx,
                    self.dev_addr,
                    self.ep_out,
                    self.ep_out_mps,
                    self.ep_in,
                    self.ep_in_mps,
                    lba,
                    count,
                    self.block_size,
                    buf,
                    &mut self.tag,
                )
            })
        }
        fn write_sectors(
            &mut self,
            lba: u32,
            count: u16,
            buf: &[u8],
        ) -> Result<(), &'static str> {
            crate::drivers::registry::with_ctx(|ctx| {
                ctx.bot_write(
                    self.ctrl_type,
                    self.ctrl_idx,
                    self.dev_addr,
                    self.ep_out,
                    self.ep_out_mps,
                    self.ep_in,
                    self.ep_in_mps,
                    lba,
                    count,
                    self.block_size,
                    buf,
                    &mut self.tag,
                )
            })
        }
        fn sector_size(&self) -> u32 {
            self.block_size
        }
        fn total_sectors(&self) -> u64 {
            self.total_blocks
        }
    }

    let bdev = Box::new(BotBlockDev {
        ctrl_type,
        ctrl_idx,
        dev_addr,
        ep_out,
        ep_out_mps,
        ep_in,
        ep_in_mps,
        block_size: disk.block_size,
        total_blocks: disk.total_blocks,
        tag: 0,
    });

    let mp = disk.mount_point.clone();
    let _ = crate::contexts::vfs::mkdir("/mnt");

    match crate::drivers::fat::FatFileSystem::from_device(bdev) {
        Ok(fs) => {
            let mount_mp = alloc::format!("/mnt/{}", mp);
            let _ = crate::contexts::vfs::mkdir(&mount_mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mount_mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                USB_DRIVES.lock().push(UsbDrive {
                    name: alloc::format!("USB Storage ({})", mp),
                    mount_point: mount_mp.clone(),
                });
                USB_DRIVE_COUNT.fetch_add(1, Ordering::Relaxed);
                crate::klog_fmt!("USB: mounted {} at {}\n", disk.ctrl_type, mount_mp);
                true
            } else {
                crate::klog_fmt!("USB: mount failed for {}\n", mp);
                false
            }
        }
        Err(e) => {
            crate::klog_fmt!("USB: FAT error for {} — {}\n", mp, e);
            false
        }
    }
}

// ────────────────────────────────────────────────────────────
//  SD card probe & mount (formerly drivers/sd_card.rs)
// ────────────────────────────────────────────────────────────

/// Probe & mount an SD card (called from the `sd_mount` shell command).
pub fn sd_probe_and_mount() -> bool {
    if SD_PROBED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        crate::klog_fmt!("SD card: already mounted or mount in progress\n");
        return true;
    }

    let ok = sd_probe_and_mount_impl();
    if !ok {
        SD_PROBED.store(false, Ordering::Release);
    }
    let _ = crate::klog::flush_to_vfs();
    ok
}

fn sd_probe_and_mount_impl() -> bool {
    if !nitrogen::storage::rtsx::is_present() {
        crate::klog_fmt!("SD card: no controller\n");
        return false;
    }

    match nitrogen::storage::rtsx::init_sd_card() {
        Ok(()) => crate::klog_fmt!("SD card: initialised\n"),
        Err(e) => {
            crate::klog_fmt!("SD card: init failed — {}\n", e);
            return false;
        }
    }

    let info = match nitrogen::storage::rtsx::sd_card_info() {
        Some(i) => i,
        None => {
            crate::klog_fmt!("SD card: no card info\n");
            return false;
        }
    };

    crate::klog_fmt!(
        "SD card: {:?} {} sectors {} bytes/sector\n",
        info.card_type,
        info.total_blocks,
        info.block_size
    );

    let _ = crate::contexts::vfs::mkdir("/mnt");

    let bdev = SdBlockDev {
        block_size: info.block_size,
        total_blocks: info.total_blocks,
    };

    let mp = String::from("/mnt/sdcard-1");
    match crate::drivers::fat::FatFileSystem::from_device(Box::new(bdev)) {
        Ok(fs) => {
            let _ = crate::contexts::vfs::mkdir(&mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                SD_DRIVES.lock().push(SdDrive {
                    name: String::from("SD Card"),
                    mount_point: mp.clone(),
                });
                SD_DRIVE_COUNT.fetch_add(1, Ordering::Relaxed);
                crate::klog_fmt!("SD card: mounted at {}\n", mp);
                true
            } else {
                crate::klog_fmt!("SD card: mount failed\n");
                false
            }
        }
        Err(e) => {
            crate::klog_fmt!("SD card: FAT error — {}\n", e);
            false
        }
    }
}

struct SdBlockDev {
    block_size: u32,
    total_blocks: u64,
}

unsafe impl Send for SdBlockDev {}

impl crate::drivers::fat::BlockDevice for SdBlockDev {
    fn read_sectors(
        &mut self,
        lba: u32,
        count: u16,
        buf: &mut [u8],
    ) -> Result<(), &'static str> {
        nitrogen::storage::rtsx::read_sectors(lba, count, buf)
    }
    fn write_sectors(
        &mut self,
        lba: u32,
        count: u16,
        buf: &[u8],
    ) -> Result<(), &'static str> {
        nitrogen::storage::rtsx::write_sectors(lba, count, buf)
    }
    fn sector_size(&self) -> u32 {
        self.block_size
    }
    fn total_sectors(&self) -> u64 {
        self.total_blocks
    }
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
    changed |= poll_usb();
    changed
}
