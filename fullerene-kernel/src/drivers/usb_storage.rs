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
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use nitrogen::usb::context::USBContext;
use nitrogen::usb::disk::Disk;

use crate::drivers::fat::{BlockDevice, FatFileSystem};
use crate::klog_fmt;

pub static USB_DRIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
pub static USB_DRIVES: Mutex<Vec<UsbDrive>> = Mutex::new(Vec::new());

pub struct UsbDrive {
    pub name: String,
    pub mount_point: String,
}

/// Tracks retry state for failed mount attempts.
struct MountRetryState {
    /// Number of consecutive failures for this disk.
    failure_count: usize,
    /// Tick counter when the next retry is allowed.
    next_retry_tick: u64,
}

/// Mount retry backoff state, keyed by mount point.
static MOUNT_RETRY_STATE: Mutex<BTreeMap<String, MountRetryState>> = Mutex::new(BTreeMap::new());

// ── Global USB context ────────────────────────────────────

static USB_CTX: spin::Mutex<Option<USBContext>> = spin::Mutex::new(None);

/// Access the controller context outside enumeration callbacks.
pub(crate) fn with_ctx<F, R>(f: F) -> R
where
    F: FnOnce(&mut USBContext) -> R,
{
    let mut guard = USB_CTX.lock();
    let ctx = guard.as_mut().expect("USB context not initialized");
    f(ctx)
}

pub fn init() {
    log::info!("USB: init start");
    let _ = crate::vfs::mkdir("/mnt");

    // Create and initialise the USB context.
    {
        use crate::driver_context_impl::KernelDriverContext;
        let mut ctx = USBContext::new(&KernelDriverContext);

        log::info!("USB: enabling context (PCI scan + controller init)");
        let init_ok = ctx.enable();
        log::info!("USB: ctx.enable() = {:?}", init_ok);

        // Retry polling multiple times — real xHCI hardware may need
        // extra time for port power-up, link training, and device enumeration.
        for i in 0..8 {
            let count_before = USB_DRIVE_COUNT.load(Ordering::Relaxed);
            let disk_count_before = ctx.disks().len();

            log::info!("USB: poll #{}, drives before: count={}, disks={}",
                i + 1, count_before, disk_count_before);
            ctx.poll();

            let count_after = USB_DRIVE_COUNT.load(Ordering::Relaxed);
            let disk_count_after = ctx.disks().len();
            log::info!(
                "USB: poll #{}, drives after: count={}, disks={}",
                i + 1, count_after, disk_count_after
            );

            if !ctx.disks().is_empty() {
                log::info!("USB: device detected after {} retries", i + 1);
                for d in ctx.disks() {
                    log::info!(
                        "USB:   -> ctrl={} dev_addr={} block_size={} total_blocks={}",
                        d.ctrl_type,
                        d.dev_addr,
                        d.block_size,
                        d.total_blocks
                    );
                }
                break;
            }
            nitrogen::timing::delay_ms(250);
        }

        *USB_CTX.lock() = Some(ctx);
    } // closes the inner block

    mount_pending();

    // Quick check: if a device was already mounted, log it.
    if USB_DRIVE_COUNT.load(Ordering::Relaxed) > 0 {
        log::info!("USB: device detected and mounted");
    } else {
        log::info!("USB: no device detected after 8 retries — continuing without USB storage");
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
    mount_pending();
    let changed = USB_DRIVE_COUNT.load(Ordering::Relaxed) != before;
    if changed {
        let _ = crate::klog::flush_to_vfs();
    }
    changed
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

    // Clear retry state on explicit rescan.
    MOUNT_RETRY_STATE.lock().clear();

    // Re-create the USB context (full re-scan)
    use crate::driver_context_impl::KernelDriverContext;
    let mut ctx = USBContext::new(&KernelDriverContext);
    let _ = ctx.enable();
    {
        let mut guard = USB_CTX.lock();
        *guard = Some(ctx);
    }
    mount_pending();
    let mounted = USB_DRIVE_COUNT.load(Ordering::Relaxed) > 0;
    let _ = crate::klog::flush_to_vfs();
    mounted
}

/// Mount newly enumerated candidates after releasing the controller lock.
fn mount_pending() {
    let current_tick = solvent::GLOBAL_TICK.load(core::sync::atomic::Ordering::Relaxed);
    let mounted: Vec<String> = USB_DRIVES.lock().iter().map(|d| d.mount_point.clone()).collect();
    let candidates: Vec<Disk> = USB_CTX
        .lock()
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
        // Check if this disk is in backoff.
        if let Some(state) = retry_state.get(&disk.mount_point) {
            if current_tick < state.next_retry_tick {
                // Still in backoff; skip this candidate.
                continue;
            }
        }

        // Attempt mount.
        if platform_mount_fat(&mut disk) {
            // Success: remove any retry state.
            retry_state.remove(&disk.mount_point);
        } else {
            // Failure: record or increment backoff.
            let state = retry_state.entry(disk.mount_point.clone())
                .or_insert(MountRetryState {
                    failure_count: 0,
                    next_retry_tick: 0,
                });
            state.failure_count += 1;
            // Exponential backoff: 50ms * 2^failure_count, capped at 10s.
            let backoff_ticks = (50 * (1 << state.failure_count.min(7))).min(10_000);
            state.next_retry_tick = current_tick + backoff_ticks;
        }
    }
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

        fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
            with_ctx(|ctx| {
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
    // This logic mirrors `find_fat_partition` to discover the correct boot sector LBA.
    let mut mbr = [0u8; 512];
    let ok = with_ctx(|ctx| {
        ctx.bot_read(
            ctrl_type, ctrl_idx, dev_addr, ep_out, ep_out_mps, ep_in, ep_in_mps, 0, 1, 512,
            &mut mbr, &mut 1,
        )
    });
    if ok.is_err() {
        return false;
    }

    // Check if LBA 0 is exFAT or FAT BPB (no MBR)
    let mut partition_lba = 0u32;
    let is_exfat_at_0 = &mbr[3..11] == b"EXFAT   ";
    let bps_at_0 = u16::from_le_bytes([mbr[11], mbr[12]]);
    let is_fat_bpb_at_0 =
        bps_at_0 == 512 || bps_at_0 == 1024 || bps_at_0 == 2048 || bps_at_0 == 4096;

    if !is_exfat_at_0 && !is_fat_bpb_at_0 {
        // Check MBR signature
        let sig = u16::from_le_bytes([mbr[0x1FE], mbr[0x1FF]]);
        if sig == 0xAA55 {
            // Scan MBR partition table for FAT/exFAT partition types.
            // We do NOT stop at the first matching entry — chain-loader
            // USB drives (Ventoy / Rufus / etc.) typically have a small
            // boot/EFI partition followed by an exFAT data partition.  We
            // prefer the largest FAT/exFAT partition so that the actual
            // data area is mounted instead of the boot stub.
            let mut best_lba: Option<u32> = None;
            let mut best_sectors: u64 = 0;
            for i in 0..4 {
                let off = 0x1BE + i * 16;
                let ptype = mbr[off + 4];
                let lba_start =
                    u32::from_le_bytes([mbr[off + 8], mbr[off + 9], mbr[off + 10], mbr[off + 11]]);
                let sector_count = u32::from_le_bytes([
                    mbr[off + 12],
                    mbr[off + 13],
                    mbr[off + 14],
                    mbr[off + 15],
                ]) as u64;
                // FAT32, FAT16, exFAT partition types
                // 0x0B/0x0C = FAT32, 0x06/0x0E = FAT16, 0x07 = NTFS/exFAT (ambiguous)
                // Prioritize known FAT/exFAT types; validate ambiguous 0x07 by checking boot sector
                let is_known_fat = ptype == 0x0B || ptype == 0x0C || ptype == 0x06 || ptype == 0x0E;
                let is_ambiguous_07 = ptype == 0x07;

                if is_known_fat || is_ambiguous_07 {
                    let mut accept = is_known_fat; // Known FAT types are always accepted

                    // For ambiguous 0x07, validate by reading boot sector
                    if is_ambiguous_07 {
                        let mut boot_check = [0u8; 512];
                        let read_ok = with_ctx(|ctx| {
                            ctx.bot_read(
                                ctrl_type,
                                ctrl_idx,
                                dev_addr,
                                ep_out,
                                ep_out_mps,
                                ep_in,
                                ep_in_mps,
                                lba_start,
                                1,
                                512,
                                &mut boot_check,
                                &mut 1,
                            )
                        });
                        if read_ok.is_ok() {
                            // Check for exFAT signature at offset 3-10
                            let is_exfat_sig = &boot_check[3..11] == b"EXFAT   ";
                            // Check for FAT BPB (valid bytes-per-sector at offset 11-12)
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

            // GPT support: detect GPT Protective MBR and follow the GUID
            // Partition Table. Ventoy and similar tools use GPT instead of
            // MBR; the protective MBR contains only one entry of type 0xEE.
            if partition_lba == 0 {
                let gpt_protective = (|| {
                    let off = 0x1BE;
                    let ptype = mbr[off + 4];
                    ptype == 0xEE
                })();
                if gpt_protective {
                    // Read the GPT header at LBA 1
                    let mut gpt_hdr = [0u8; 512];
                    let ok = with_ctx(|ctx| {
                        ctx.bot_read(
                            ctrl_type,
                            ctrl_idx,
                            dev_addr,
                            ep_out,
                            ep_out_mps,
                            ep_in,
                            ep_in_mps,
                            1,
                            1,
                            512,
                            &mut gpt_hdr,
                            &mut 1,
                        )
                    });
                    if ok.is_ok() && &gpt_hdr[0..8] == b"EFI PART" {
                        // GPT Header Layout (offsets from start of sector):
                        //   44..51 = First Usable LBA
                        //   52..59 = Last Usable LBA
                        //   72..79 = Partition Entries Starting LBA
                        //   80..83 = Number of Partition Entries
                        //   84..87 = Size of Each Partition Entry
                        let _first_usable = u64::from_le_bytes([
                            gpt_hdr[44],
                            gpt_hdr[45],
                            gpt_hdr[46],
                            gpt_hdr[47],
                            gpt_hdr[48],
                            gpt_hdr[49],
                            gpt_hdr[50],
                            gpt_hdr[51],
                        ]);
                        let _last_usable = u64::from_le_bytes([
                            gpt_hdr[52],
                            gpt_hdr[53],
                            gpt_hdr[54],
                            gpt_hdr[55],
                            gpt_hdr[56],
                            gpt_hdr[57],
                            gpt_hdr[58],
                            gpt_hdr[59],
                        ]);
                        let entries_lba = u64::from_le_bytes([
                            gpt_hdr[72],
                            gpt_hdr[73],
                            gpt_hdr[74],
                            gpt_hdr[75],
                            gpt_hdr[76],
                            gpt_hdr[77],
                            gpt_hdr[78],
                            gpt_hdr[79],
                        ]);
                        let _num_entries = u32::from_le_bytes([
                            gpt_hdr[80],
                            gpt_hdr[81],
                            gpt_hdr[82],
                            gpt_hdr[83],
                        ]);
                        let num_entries = u32::from_le_bytes([
                            gpt_hdr[80],
                            gpt_hdr[81],
                            gpt_hdr[82],
                            gpt_hdr[83],
                        ]);
                        let entry_size = u32::from_le_bytes([
                            gpt_hdr[84],
                            gpt_hdr[85],
                            gpt_hdr[86],
                            gpt_hdr[87],
                        ])
                        .max(128);
                        // Read the GPT entries array. Each sector holds multiple entries;
                        // iterate through entries using entry_size, reading sectors as needed.
                        // Scan up to num_entries or 128 entries (whichever is smaller).
                        let mut best_lba_gpt: u32 = 0;
                        let mut best_size_gpt: u64 = 0;
                        let max_entries = num_entries.min(128);
                        let entries_per_sector = 512 / entry_size;

                        for idx in 0..max_entries {
                            let sector_idx = idx / entries_per_sector;
                            let entry_in_sector = idx % entries_per_sector;
                            let sector_lba = entries_lba + sector_idx as u64;

                            // Read sector containing this entry
                            let mut sector = [0u8; 512];
                            let ok = with_ctx(|ctx| {
                                ctx.bot_read(
                                    ctrl_type,
                                    ctrl_idx,
                                    dev_addr,
                                    ep_out,
                                    ep_out_mps,
                                    ep_in,
                                    ep_in_mps,
                                    sector_lba as u32,
                                    1,
                                    512,
                                    &mut sector,
                                    &mut 1,
                                )
                            });
                            if ok.is_err() {
                                break;
                            }

                            // Extract entry from sector buffer
                            let entry_off = (entry_in_sector * entry_size) as usize;
                            if entry_off + 128 > 512 {
                                break; // Entry spans sectors (unsupported)
                            }
                            let entry = &sector[entry_off..entry_off + 128];

                            // Type GUID at offset 0..15. If all zeros -> unused entry.
                            if entry[..16] == [0u8; 16] {
                                continue;
                            }
                            // First usable LBA at offset 32..39, last usable LBA at 40..47.
                            let start_lba = u64::from_le_bytes([
                                entry[32], entry[33], entry[34], entry[35], entry[36], entry[37],
                                entry[38], entry[39],
                            ]);
                            let end_lba = u64::from_le_bytes([
                                entry[40], entry[41], entry[42], entry[43], entry[44], entry[45],
                                entry[46], entry[47],
                            ]);
                            let size_sectors = end_lba.saturating_sub(start_lba) + 1;
                            // Heuristic: pick the largest partition. Most
                            // multi-boot tools create a small EFI/boot
                            // partition followed by the data partition.
                            // Reject partitions whose start_lba exceeds u32::MAX
                            if start_lba <= u32::MAX as u64 && size_sectors > best_size_gpt {
                                best_size_gpt = size_sectors;
                                best_lba_gpt = start_lba as u32;
                            }
                        }
                        partition_lba = best_lba_gpt;
                        log::info!("USB: GPT detected, using {}", partition_lba);
                    }
                }
            }
        }
    }

    // Read the actual boot sector (at partition start if partitioned)
    let mut boot = [0u8; 512];
    let ok = with_ctx(|ctx| {
        ctx.bot_read(
            ctrl_type,
            ctrl_idx,
            dev_addr,
            ep_out,
            ep_out_mps,
            ep_in,
            ep_in_mps,
            partition_lba,
            1,
            512,
            &mut boot,
            &mut 1,
        )
    });
    if ok.is_err() {
        return false;
    }

    let is_exfat = &boot[3..11] == b"EXFAT   ";
    let (block_size, total_blocks) = if is_exfat {
        let bps_shift = boot[108];
        // Validate shift value before using it (exFAT spec: 9-12 for 512-4096 bytes/sector)
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

    let bdev = BotBlockDev {
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
    };

    let mp = disk.mount_point.clone();
    match FatFileSystem::from_device(Box::new(bdev)) {
        Ok(fs) => {
            let _ = crate::vfs::mkdir(&mp);
            if crate::contexts::vfs::with_vfs(|v| v.mount(&mp, Box::new(fs)))
                .is_some_and(|r| r.is_ok())
            {
                USB_DRIVES.lock().push(UsbDrive {
                    name: disk.name.clone(),
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
