//! SD card block-device integration — RTSX card reader → FAT mount.
//!
//! Probing is split into two phases:
//!
//! 1. **`init()`** — safe PCI config-space probe, called at boot.
//!    Only touches port I/O.  Never hangs.
//! 2. **`probe_and_mount()`** — MMIO access, SD card init, FAT mount.
//!    Called on demand (from shell or hotplug).  If the hardware is
//!    unresponsive the failure is logged and the system continues.
//!
//! The user triggers probe via the `sd_mount` shell command.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::fat::{BlockDevice, FatFileSystem};
use crate::klog_fmt;

pub static SD_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static SD_DRIVES: Mutex<Vec<SdDrive>> = Mutex::new(Vec::new());
pub static SD_PROBED: AtomicBool = AtomicBool::new(false);

pub struct SdDrive {
    pub name: String,
    pub mount_point: String,
}

struct SdBlockDev {
    block_size: u32,
    total_blocks: u64,
}

unsafe impl Send for SdBlockDev {}

impl BlockDevice for SdBlockDev {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        nitrogen::storage::rtsx::read_sectors(lba, count, buf)
    }

    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        nitrogen::storage::rtsx::write_sectors(lba, count, buf)
    }

    fn sector_size(&self) -> u32 {
        self.block_size
    }

    fn total_sectors(&self) -> u64 {
        self.total_blocks
    }
}

/// Phase 1: safe PCI-config probe (port I/O only).
/// Called during kernel init.  Never touches MMIO.
pub fn init() {
    log::info!("SD: init start");
    use crate::driver_context_impl::KernelDriverContext;
    nitrogen::storage::rtsx::init(&KernelDriverContext);

    if nitrogen::storage::rtsx::is_present() {
        log::info!("SD: RTSX controller found, card init deferred to probe_and_mount");
    } else {
        log::info!("SD: no RTSX controller found — continuing without SD card support");
    }
}

/// Phase 2: MMIO access, SD card init, and FAT mount.
/// Safe to call at any time — returns an error instead of hanging.
/// The system continues booting even if this fails.
pub fn probe_and_mount() -> bool {
    // Atomically mark mount as in-progress to prevent concurrent re-entry.
    if SD_PROBED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        klog_fmt!("SD card: already mounted or mount in progress\n");
        return true;
    }

    let ok = probe_and_mount_impl();

    if !ok {
        // Allow future retry on failure.
        SD_PROBED.store(false, Ordering::Release);
    }
    ok
}

fn probe_and_mount_impl() -> bool {
    if !nitrogen::storage::rtsx::is_present() {
        klog_fmt!("SD card: no controller\n");
        return false;
    }

    // Attempt SD card initialisation (first MMIO access).
    match nitrogen::storage::rtsx::init_sd_card() {
        Ok(()) => {
            klog_fmt!("SD card: initialised\n");
        }
        Err(e) => {
            klog_fmt!("SD card: init failed — {}\n", e);
            return false;
        }
    }

    let info = match nitrogen::storage::rtsx::sd_card_info() {
        Some(i) => i,
        None => {
            klog_fmt!("SD card: no card info\n");
            return false;
        }
    };

    klog_fmt!(
        "SD card: {:?} {} sectors {} bytes/sector\n",
        info.card_type,
        info.total_blocks,
        info.block_size
    );

    let _ = crate::vfs::mkdir("/mnt");

    let bdev = SdBlockDev {
        block_size: info.block_size,
        total_blocks: info.total_blocks,
    };

    let mp = alloc::string::String::from("/mnt/sdcard-1");
    match FatFileSystem::from_device(Box::new(bdev)) {
        Ok(fs) => {
            let _ = crate::vfs::mkdir(&mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                SD_DRIVES.lock().push(SdDrive {
                    name: alloc::string::String::from("SD Card"),
                    mount_point: mp.clone(),
                });
                SD_DRIVE_COUNT.fetch_add(1, Ordering::Relaxed);
                klog_fmt!("SD card: mounted at {}\n", mp);
                true
            } else {
                klog_fmt!("SD card: mount failed\n");
                false
            }
        }
        Err(e) => {
            klog_fmt!("SD card: FAT error — {}\n", e);
            false
        }
    }
}
