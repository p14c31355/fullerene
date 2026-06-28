//! USB mass-storage integration — FAT mount + hotplug poll.
//!
//! The kernel owns a single [`USBContext`] that handles controller
//! discovery, port polling, device enumeration, and driver matching.
//! This module only handles VFS/FAT integration and platform-specific
//! delay functions.
//!
//! # Usage
//!
//! ```ignore
//! usb_storage::init();   // at boot
//! usb_storage::poll_usb();  // from background timer
//! ```

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use nitrogen::usb::context::USBContext;
use nitrogen::usb::disk::{Disk, set_mount_fn};

use crate::drivers::fat::{BlockDevice, FatFileSystem};
use crate::klog_fmt;

pub static USB_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
}

// ── Global USB context ────────────────────────────────────

static USB_CTX: spin::Mutex<Option<USBContext>> = spin::Mutex::new(None);
static CTRL_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Busy-wait for approximately `ms` milliseconds using RDTSC.
fn delay_ms(ms: u64) {
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    let tsc_per_ms = solvent::get_tsc_per_ms();
    let target = ms * if tsc_per_ms > 0 { tsc_per_ms } else { 1_000_000 };
    while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) < target {
        core::hint::spin_loop();
    }
}

fn with_ctx<F, R>(f: F) -> R
where
    F: FnOnce(&mut USBContext) -> R,
{
    let mut guard = USB_CTX.lock();
    let ctx = guard.as_mut().expect("USB context not initialized");
    f(ctx)
}

pub fn init() {
    let _ = crate::vfs::mkdir("/mnt");

    // Register the platform's FAT-mount callback.
    set_mount_fn(platform_mount_fat);

    // Create and initialise the USB context.
    {
        use crate::driver_context_impl::KernelDriverContext;
        let mut ctx = USBContext::new(&KernelDriverContext);
        let _ = ctx.enable();
        let mut guard = USB_CTX.lock();
        *guard = Some(ctx);
    }

    // Quick check: if a device was already mounted, log it.
    if USB_DRIVE_COUNT.load(Ordering::Relaxed) > 0 {
        klog_fmt!("USB init: device detected and mounted\n");
    } else {
        klog_fmt!("USB init: no device detected, continuing in background\n");
    }
}

pub fn poll_usb() -> bool {
    let before = USB_DRIVE_COUNT.load(Ordering::Relaxed);
    {
        let mut guard = USB_CTX.lock();
        if let Some(ctx) = guard.as_mut() {
            ctx.poll();
        }
    }
    USB_DRIVE_COUNT.load(Ordering::Relaxed) != before
}

/// Re-poll all controllers: clear state and re-enumerate from scratch.
pub fn poll_usb_all() -> bool {
    // Unmount existing drives
    let mps: Vec<String> = USB_DRIVES
        .lock()
        .iter()
        .map(|d| d.mount_point.clone())
        .collect();
    for mp in &mps {
        let _ = crate::vfs::unmount(mp);
    }
    USB_DRIVES.lock().clear();
    USB_DRIVE_COUNT.store(0, Ordering::Relaxed);

    // Re-create the USB context (full re-scan)
    use crate::driver_context_impl::KernelDriverContext;
    let mut ctx = USBContext::new(&KernelDriverContext);
    let _ = ctx.enable();
    {
        let mut guard = USB_CTX.lock();
        *guard = Some(ctx);
    }

    USB_DRIVE_COUNT.load(Ordering::Relaxed) > 0
}

// ── Platform FAT-mount callback ───────────────────────────

/// Called by [`StorageManager::try_mount`] when a mass-storage device
/// has been detected and its BOT endpoints are known.
///
/// Reads the boot sector, tries to mount a FAT filesystem, and registers
/// the mount point in [`USB_DRIVES`]. Updates the disk's block_size and
/// total_blocks fields with actual values from the BPB.
fn platform_mount_fat(disk: &mut Disk) -> bool {
    // Copy disk parameters into the block device so the closure
    // doesn't borrow `disk` across the `with_ctx` call.
    let ctrl_type = disk.ctrl_type;
    let ctrl_idx = disk.ctrl_idx;
    let dev_addr = disk.dev_addr;
    let ep_out = disk.ep_out;
    let ep_in = disk.ep_in;

    struct BotBlockDev {
        ctrl_type: &'static str,
        ctrl_idx: usize,
        dev_addr: u8,
        ep_out: u8,
        ep_in: u8,
        block_size: u32,
        total_blocks: u64,
        tag: u32,
    }
    unsafe impl Send for BotBlockDev {}

    impl BlockDevice for BotBlockDev {
        fn read_sectors(
            &mut self,
            lba: u32,
            count: u16,
            buf: &mut [u8],
        ) -> Result<(), &'static str> {
            with_ctx(|ctx| {
                ctx.bot_read(
                    self.ctrl_type,
                    self.ctrl_idx,
                    self.dev_addr,
                    self.ep_out,
                    self.ep_in,
                    lba,
                    count,
                    self.block_size,
                    buf,
                    &mut self.tag,
                )
            })
        }

        fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
            with_ctx(|ctx| {
                ctx.bot_write(
                    self.ctrl_type,
                    self.ctrl_idx,
                    self.dev_addr,
                    self.ep_out,
                    self.ep_in,
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

    // Read the boot sector to determine actual block size / total blocks
    // before creating the filesystem.
    let mut boot = [0u8; 512];
    let ok = with_ctx(|ctx| {
        ctx.bot_read(ctrl_type, ctrl_idx, dev_addr, ep_out, ep_in, 0, 1, 512, &mut boot, &mut 1)
    });
    if ok.is_err() {
        return false;
    }

    let is_exfat = &boot[3..11] == b"EXFAT   ";
    let (block_size, total_blocks) = if is_exfat {
        let bps_shift = boot[108];
        let bps = 1u32 << bps_shift;
        let total_blocks = u64::from_le_bytes([
            boot[72], boot[73], boot[74], boot[75],
            boot[76], boot[77], boot[78], boot[79],
        ]);
        (bps, total_blocks)
    } else {
        let block_size = u16::from_le_bytes([boot[11], boot[12]]) as u32;
        let total_sectors_16 = u16::from_le_bytes([boot[19], boot[20]]) as u64;
        let total_sectors_32 = u32::from_le_bytes([boot[32], boot[33], boot[34], boot[35]]) as u64;
        let total_blocks = if total_sectors_32 > 0 { total_sectors_32 } else { total_sectors_16 };
        (block_size, total_blocks)
    };

    if block_size == 0 {
        return false;
    }

    // Update disk geometry with actual values from BPB
    disk.block_size = block_size;
    disk.total_blocks = total_blocks;

    let bdev = BotBlockDev {
        ctrl_type,
        ctrl_idx,
        dev_addr,
        ep_out,
        ep_in,
        block_size,
        total_blocks,
        tag: 1,
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
                true
            } else {
                false
            }
        }
        Err(e) => {
            klog_fmt!("USB mount: {}\n", e);
            false
        }
    }
}
