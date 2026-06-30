//! SD card block-device integration — RTSX card reader → FAT mount.
//!
//! The kernel probes the RTSX PCI card reader, initialises the SD card,
//! wraps it as a [`BlockDevice`], and mounts any FAT/exFAT filesystem
//! found at `/mnt/sdcard-1`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use crate::drivers::fat::{BlockDevice, FatFileSystem};
use crate::klog_fmt;

pub static SD_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static SD_DRIVES: Mutex<Vec<SdDrive>> = Mutex::new(Vec::new());

pub struct SdDrive {
    pub name: String,
    pub mount_point: String,
}

/// BlockDevice wrapper around the RTSX SD card reader.
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

/// Initialise the SD card reader and attempt to mount any filesystem.
pub fn init() {
    let _ = crate::vfs::mkdir("/mnt");

    // Initialise RTSX PCI hardware
    use crate::driver_context_impl::KernelDriverContext;
    nitrogen::storage::rtsx::init(&KernelDriverContext);

    if !nitrogen::storage::rtsx::is_present() {
        klog_fmt!("SD card: no RTSX controller found\n");
        return;
    }

    // Wait a bit for card to be ready
    for _ in 0..500_000 {
        core::hint::spin_loop();
    }

    // Initialise the SD card
    match nitrogen::storage::rtsx::init_sd_card() {
        Ok(()) => {
            klog_fmt!("SD card: initialised\n");
        }
        Err(e) => {
            klog_fmt!("SD card: init failed — {}\n", e);
            return;
        }
    }

    // Get card info
    let info = match nitrogen::storage::rtsx::sd_card_info() {
        Some(i) => i,
        None => {
            klog_fmt!("SD card: no card info\n");
            return;
        }
    };

    klog_fmt!("SD card: type={:?} sectors={} sector_size={}\n",
        info.card_type, info.total_blocks, info.block_size);

    let bdev = SdBlockDev {
        block_size: info.block_size,
        total_blocks: info.total_blocks,
    };

    let mp = alloc::format!("/mnt/sdcard-1");
    match FatFileSystem::from_device(Box::new(bdev)) {
        Ok(fs) => {
            let _ = crate::vfs::mkdir(&mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                SD_DRIVES.lock().push(SdDrive {
                    name: alloc::format!("SD Card"),
                    mount_point: mp.clone(),
                });
                SD_DRIVE_COUNT.fetch_add(1, Ordering::Relaxed);
                klog_fmt!("SD card: mounted at {}\n", mp);
            } else {
                klog_fmt!("SD card: mount failed\n");
            }
        }
        Err(e) => {
            klog_fmt!("SD card: FAT mount error — {}\n", e);
        }
    }
}
