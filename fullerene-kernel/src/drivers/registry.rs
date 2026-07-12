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

#[cfg(not(nitrogen_no_usb))]
pub static USB_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(not(nitrogen_no_usb))]
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

// Shared USB context static used by all USB access paths
#[cfg(not(nitrogen_no_usb))]
static USB_CTX: Mutex<Option<nitrogen::usb::context::USBContext>> = Mutex::new(None);

#[cfg(not(nitrogen_no_usb))]
pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
}

#[cfg(nitrogen_no_usb)]
pub static USB_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(nitrogen_no_usb)]
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

#[cfg(nitrogen_no_usb)]
pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
}

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
pub static SD_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(not(nitrogen_no_storage))]
pub static SD_DRIVES: Mutex<Vec<SdDrive>> = Mutex::new(Vec::new());
#[cfg(not(nitrogen_no_storage))]
pub static SD_PROBED: AtomicBool = AtomicBool::new(false);

#[cfg(not(nitrogen_no_storage))]
pub struct SdDrive {
    pub name: String,
    pub mount_point: String,
}

#[cfg(nitrogen_no_storage)]
pub static SD_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(nitrogen_no_storage)]
pub static SD_DRIVES: Mutex<Vec<SdDrive>> = Mutex::new(Vec::new());

#[cfg(nitrogen_no_storage)]
pub struct SdDrive {
    pub name: String,
    pub mount_point: String,
}

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
        let _ = crate::contexts::vfs::mkdir("/mnt");

        let mut ctx = nitrogen::usb::context::USBContext::new(
            &crate::driver_context_impl::KernelDriverContext,
        );
        let _ = ctx.enable();
        // Store in the global singleton for later polling.
        crate::drivers::registry::init_usb_ctx(ctx);
        // Initial poll + mount.
        crate::drivers::registry::usb_poll_and_mount();
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
//  USB polling
// ────────────────────────────────────────────────────────────

#[cfg(not(nitrogen_no_usb))]
/// Mount retry backoff state keyed by mount point.
static MOUNT_RETRY_STATE: Mutex<BTreeMap<String, MountRetryState>> =
    Mutex::new(BTreeMap::new());

#[cfg(not(nitrogen_no_usb))]
struct MountRetryState {
    failure_count: usize,
    next_retry_tick: u64,
}

/// Poll USB controller once and mount newly-discovered devices.
/// Returns `true` if a new drive was mounted.
#[cfg(not(nitrogen_no_usb))]
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

#[cfg(nitrogen_no_usb)]
pub fn poll_usb() -> bool {
    false
}

/// Full USB re-enumeration (clear + re-scan).
#[cfg(not(nitrogen_no_usb))]
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
#[cfg(not(nitrogen_no_usb))]
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
#[cfg(not(nitrogen_no_usb))]
fn with_ctx_inner() -> spin::MutexGuard<'static, Option<nitrogen::usb::context::USBContext>> {
    USB_CTX.lock()
}

/// Mount newly enumerated USB candidates after releasing the controller lock.
#[cfg(not(nitrogen_no_usb))]
fn mount_pending() {
    let current_tick = solvent::GLOBAL_TICK.load(Ordering::Relaxed);
    let mounted: Vec<String> = USB_DRIVES.lock().iter().map(|d| d.mount_point.clone()).collect();
    let candidates: Vec<nitrogen::usb::disk::Disk> =
        with_ctx_inner()
            .as_ref()
            .map(|ctx| {
                ctx.disks()
                    .iter()
                    .filter(|disk| !mounted.contains(&alloc::format!("/mnt/{}", disk.mount_point)))
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

#[cfg(not(nitrogen_no_usb))]
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

    // Determine partition offset by scanning for FAT partition (MBR or raw BPB).
    let mut partition_lba = 0u32;
    {
        let mut mbr = [0u8; 512];
        let mut tag = 0u32;
        let ok = crate::drivers::registry::with_ctx(|ctx| {
            ctx.bot_read(
                ctrl_type, ctrl_idx, dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps,
                0, 1, 512, &mut mbr, &mut tag,
            )
        });
        if ok.is_ok() {
            let is_exfat_at_0 = &mbr[3..11] == b"EXFAT   ";
            let bps_at_0 = u16::from_le_bytes([mbr[11], mbr[12]]);
            let is_fat_bpb_at_0 =
                bps_at_0 == 512 || bps_at_0 == 1024 || bps_at_0 == 2048 || bps_at_0 == 4096;

            if !is_exfat_at_0 && !is_fat_bpb_at_0 {
                let sig = u16::from_le_bytes([mbr[0x1FE], mbr[0x1FF]]);
                if sig == 0xAA55 {
                    let mut best_lba: Option<u32> = None;
                    let mut best_sectors: u64 = 0;
                    for i in 0..4 {
                        let off = 0x1BE + i * 16;
                        let ptype = mbr[off + 4];
                        let lba_start = u32::from_le_bytes([
                            mbr[off + 8], mbr[off + 9], mbr[off + 10], mbr[off + 11],
                        ]);
                        let sector_count = u32::from_le_bytes([
                            mbr[off + 12], mbr[off + 13], mbr[off + 14], mbr[off + 15],
                        ]) as u64;
                        let is_known_fat = ptype == 0x01 || ptype == 0x04 || ptype == 0x06 || ptype == 0x0B || ptype == 0x0C || ptype == 0x0E;
                        let is_ambiguous_07 = ptype == 0x07;

                        if is_known_fat || is_ambiguous_07 {
                            let mut accept = is_known_fat;
                            if is_ambiguous_07 {
                                let mut boot_check = [0u8; 512];
                                let mut tag2 = 0u32;
                                let read_ok = crate::drivers::registry::with_ctx(|ctx| {
                                    ctx.bot_read(
                                        ctrl_type, ctrl_idx, dev_addr, ep_out, ep_out_mps,
                                        ep_in, ep_in_mps, lba_start, 1, 512,
                                        &mut boot_check, &mut tag2,
                                    )
                                });
                                if read_ok.is_ok() {
                                    let is_exfat_sig = &boot_check[3..11] == b"EXFAT   ";
                                    let bps = u16::from_le_bytes([boot_check[11], boot_check[12]]);
                                    let is_fat_bps =
                                        bps == 512 || bps == 1024 || bps == 2048 || bps == 4096;
                                    accept = is_exfat_sig || is_fat_bps;
                                }
                            }
                            if accept && sector_count > best_sectors {
                                best_lba = Some(lba_start);
                                best_sectors = sector_count;
                            }
                        }
                    }
                    partition_lba = best_lba.unwrap_or(0);

                    // GPT support
                    if partition_lba == 0 {
                        let gpt_protective = {
                            let off = 0x1BE;
                            mbr[off + 4] == 0xEE
                        };
                        if gpt_protective {
                            let mut gpt_hdr = [0u8; 512];
                            let mut tag3 = 0u32;
                            let ok = crate::drivers::registry::with_ctx(|ctx| {
                                ctx.bot_read(
                                    ctrl_type, ctrl_idx, dev_addr, ep_out, ep_out_mps,
                                    ep_in, ep_in_mps, 1, 1, 512,
                                    &mut gpt_hdr, &mut tag3,
                                )
                            });
                            if ok.is_ok() && &gpt_hdr[0..8] == b"EFI PART" {
                                let entries_lba = u64::from_le_bytes([
                                    gpt_hdr[72], gpt_hdr[73], gpt_hdr[74], gpt_hdr[75],
                                    gpt_hdr[76], gpt_hdr[77], gpt_hdr[78], gpt_hdr[79],
                                ]);
                                let num_entries = u32::from_le_bytes([
                                    gpt_hdr[80], gpt_hdr[81], gpt_hdr[82], gpt_hdr[83],
                                ]);
                                let entry_size =
                                    u32::from_le_bytes([gpt_hdr[84], gpt_hdr[85], gpt_hdr[86], gpt_hdr[87]])
                                        .max(128);

                                let mut best_lba_gpt: u32 = 0;
                                let mut best_size_gpt: u64 = 0;
                                let max_entries = num_entries.min(128);
                                let entries_per_sector = if entry_size > 0 && entry_size <= 512 {
                                    512 / entry_size
                                } else {
                                    1
                                };

                                for idx in 0..max_entries {
                                    let sector_idx = idx / entries_per_sector;
                                    let entry_in_sector = idx % entries_per_sector;
                                    let sector_lba = entries_lba + sector_idx as u64;

                                    let mut sector = [0u8; 512];
                                    let mut tag4 = 0u32;
                                    let ok = crate::drivers::registry::with_ctx(|ctx| {
                                        ctx.bot_read(
                                            ctrl_type, ctrl_idx, dev_addr, ep_out, ep_out_mps,
                                            ep_in, ep_in_mps, sector_lba as u32, 1, 512,
                                            &mut sector, &mut tag4,
                                        )
                                    });
                                    if ok.is_err() {
                                        break;
                                    }

                                    let entry_off = (entry_in_sector * entry_size) as usize;
                                    if entry_off + 128 > 512 {
                                        break;
                                    }
                                    let entry = &sector[entry_off..entry_off + 128];

                                    if entry[..16] == [0u8; 16] {
                                        continue;
                                    }
                                    let start_lba = u64::from_le_bytes([
                                        entry[32], entry[33], entry[34], entry[35],
                                        entry[36], entry[37], entry[38], entry[39],
                                    ]);
                                    let end_lba = u64::from_le_bytes([
                                        entry[40], entry[41], entry[42], entry[43],
                                        entry[44], entry[45], entry[46], entry[47],
                                    ]);
                                    let size_sectors = end_lba.saturating_sub(start_lba) + 1;
                                    if start_lba <= u32::MAX as u64 && size_sectors > best_size_gpt {
                                        best_size_gpt = size_sectors;
                                        best_lba_gpt = start_lba as u32;
                                    }
                                }
                                partition_lba = best_lba_gpt;
                            }
                        }
                    }
                }
            }
        }
    }

    // Read the actual boot sector at partition offset
    let mut boot = [0u8; 512];
    let mut tag5 = 0u32;
    let ok = crate::drivers::registry::with_ctx(|ctx| {
        ctx.bot_read(
            ctrl_type, ctrl_idx, dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps,
            partition_lba, 1, 512, &mut boot, &mut tag5,
        )
    });
    if ok.is_err() {
        return false;
    }

    let is_exfat = &boot[3..11] == b"EXFAT   ";
    let (block_size, total_blocks) = if is_exfat {
        let bps_shift = boot[108];
        if bps_shift < 9 || bps_shift > 12 {
            return false;
        }
        let bps = 1u32 << bps_shift;
        let total_blocks = u64::from_le_bytes([
            boot[72], boot[73], boot[74], boot[75], boot[76], boot[77], boot[78], boot[79],
        ]);
        (bps, total_blocks)
    } else {
        let block_size = u16::from_le_bytes([boot[11], boot[12]]) as u32;
        let total_sectors_16 = u16::from_le_bytes([boot[19], boot[20]]) as u64;
        let total_sectors_32 = u32::from_le_bytes([boot[32], boot[33], boot[34], boot[35]]) as u64;
        let total_blocks = if total_sectors_32 > 0 {
            total_sectors_32
        } else {
            total_sectors_16
        };
        (block_size, total_blocks)
    };

    if block_size == 0 {
        return false;
    }

    // Update disk geometry with actual values from partition boot sector
    disk.block_size = block_size;
    disk.total_blocks = total_blocks;

    let bdev = Box::new(BotBlockDev {
        ctrl_type,
        ctrl_idx,
        dev_addr,
        ep_out,
        ep_out_mps,
        ep_in,
        ep_in_mps,
        block_size,
        total_blocks,
        tag: 1,
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
#[cfg(not(nitrogen_no_storage))]
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

#[cfg(not(nitrogen_no_storage))]
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

#[cfg(nitrogen_no_storage)]
pub fn sd_probe_and_mount() -> bool {
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
    #[cfg(not(nitrogen_no_usb))]
    {
        changed |= poll_usb();
    }
    changed
}
