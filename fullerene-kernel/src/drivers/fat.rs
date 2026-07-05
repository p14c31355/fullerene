//! FAT32 filesystem driver — read/write support for USB mass-storage.
//!
//! Implements the `FileSystem` trait over a block device.
//! Supports navigating directories, reading files, creating files,
//! and writing to existing files.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::min;
use core::str;

use crate::klog_fmt;
use crate::vfs::{FileDescriptor, FileSystem, InodeType, VNode};
use genome::fs::FsError;
// Master Boot Record at LBA 0. Partition entries at offset 0x1BE.

#[repr(C, packed)]
struct MbrPartitionEntry {
    status: u8,
    chs_first: [u8; 3],
    partition_type: u8,
    chs_last: [u8; 3],
    lba_start: u32,
    sector_count: u32,
}

const MBR_SIGNATURE: u16 = 0xAA55;
const PARTITION_FAT32: u8 = 0x0B; // FAT32 CHS
const PARTITION_FAT32_LBA: u8 = 0x0C; // FAT32 LBA
const PARTITION_FAT16: u8 = 0x06;
const PARTITION_FAT16_LBA: u8 = 0x0E;
const PARTITION_EXFAT: u8 = 0x07; // exFAT often uses 0x07

/// Detect whether LBA 0 contains an MBR and find the first FAT partition.
/// Returns `Some(lba_start)` if found, or `None` if LBA 0 is already a FAT BPB.
pub fn find_fat_partition(device: &mut dyn BlockDevice) -> Result<u32, FsError> {
    let mut boot = [0u8; 512];
    device.read_sectors(0, 1, &mut boot)?;

    // Check if LBA 0 is already a FAT32/ExFAT BPB (not an MBR)
    if is_exfat(&boot) {
        klog_fmt!("FAT: raw exFAT at LBA 0\n");
        return Ok(0);
    }
    // Check for FAT32 BPB signature (bytes 0x0B at offset 11 = 0x00 for FAT)
    // A FAT32 BPB has bytes_per_sector at offset 11-12 which is usually 512 (0x200)
    let bps = u16::from_le_bytes([boot[11], boot[12]]);
    if bps == 512 || bps == 1024 || bps == 2048 || bps == 4096 {
        // Likely a FAT BPB, not MBR
        klog_fmt!("FAT: raw FAT32 at LBA 0 (bps={})\n", bps);
        return Ok(0);
    }

    // Check MBR signature
    let sig = u16::from_le_bytes([boot[0x1FE], boot[0x1FF]]);
    if sig != MBR_SIGNATURE {
        klog_fmt!("FAT: no MBR signature at LBA 0 (0x{:04X})\n", sig);
        return Ok(0); // Assume raw filesystem
    }

    // Scan partition entries (at offset 0x1BE, 4 entries × 16 bytes)
    // Chain-loader USB drives (Ventoy / Rufus / etc.) typically have a
    // small boot/EFI partition followed by a larger data partition.  We
    // prefer the largest FAT/exFAT partition so that the actual data area
    // is mounted instead of the boot stub.
    let mut best_lba: Option<u32> = None;
    let mut best_sectors: u32 = 0;
    for i in 0..4 {
        let off = 0x1BE + i * 16;
        // SAFETY: boot[off..] has at least 16 bytes, and MbrPartitionEntry
        // is #[repr(C, packed)] so it has no padding requirement.
        let entry_ptr = boot[off..].as_ptr() as *const MbrPartitionEntry;
        let ptype = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).partition_type) };
        let lba_start = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).lba_start) };
        let sector_count = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).sector_count) };
        let is_fat = matches!(
            ptype,
            PARTITION_FAT32 | PARTITION_FAT32_LBA | PARTITION_FAT16 | PARTITION_FAT16_LBA | PARTITION_EXFAT
        );
        if is_fat {
            if sector_count > best_sectors {
                best_lba = Some(lba_start);
                best_sectors = sector_count;
            }
        }
        match ptype {
            PARTITION_FAT32 | PARTITION_FAT32_LBA => {
                klog_fmt!("FAT: MBR partition {} FAT32 at LBA {} sectors={}\n", i, lba_start, sector_count);
            }
            PARTITION_FAT16 | PARTITION_FAT16_LBA => {
                klog_fmt!("FAT: MBR partition {} FAT16 at LBA {} sectors={} (stub)\n", i, lba_start, sector_count);
            }
            PARTITION_EXFAT => {
                klog_fmt!("FAT: MBR partition {} exFAT at LBA {} sectors={}\n", i, lba_start, sector_count);
            }
            _ => {}
        }
    }
    if let Some(lba) = best_lba {
        klog_fmt!("FAT: selected partition at LBA {} ({} sectors)\n", lba, best_sectors);
        return Ok(lba);
    }
    klog_fmt!("FAT: no FAT partition found in MBR\n");
    Err(FsError::FileNotFound)
}

// ── Block device registry ────────────────────────────────────
//
// A simple registry of block devices by name so that the VFS layer can
// mount a FAT32 filesystem by name (e.g. `mount("/dev/sda", "/mnt", "fat32")`)
// without having to know about the kernel's USB storage internals.

use spin::Mutex;

static BLOCK_DEVICES: Mutex<Vec<(&'static str, Box<dyn BlockDevice>)>> = Mutex::new(Vec::new());

/// Register a named block device in the global registry.
///
/// The registry is used by the VFS layer to look up block devices
/// when mounting a `fat32` filesystem.
pub fn register_block_device(name: &'static str, device: Box<dyn BlockDevice>) {
    BLOCK_DEVICES.lock().push((name, device));
    klog_fmt!("FAT: registered block device {}\n", name);
}

/// Open a registered block device by name and return a `FatFileSystem`
/// mounted on it.
pub fn open_block_device(name: &str) -> Result<FatFileSystem, FsError> {
    let mut devices = BLOCK_DEVICES.lock();
    let pos = devices
        .iter()
        .position(|(n, _)| *n == name)
        .ok_or(FsError::FileNotFound)?;
    let (_, device) = devices.remove(pos);
    FatFileSystem::from_device(device)
}

// ── Block device abstraction ──────────────────────────────────
// The block device provides sector-level read/write.

pub trait BlockDevice: Send {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str>;
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
}

/// Error type for block device and cache operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    Device(&'static str),
    BufferTooSmall { required: usize, provided: usize },
    LbaOverflow,
    SectorNotFound,
}

impl From<&'static str> for BlockError {
    fn from(e: &'static str) -> Self {
        BlockError::Device(e)
    }
}

/// A simple direct-mapped block cache with round-robin eviction.
///
/// Caches `CAP` sectors keyed by LBA.  Each entry holds a copy of the
/// sector data.  Writes are passed through to the underlying device and
/// invalidate the matching cache line.
///
/// This is the storage stack foundation mentioned in the architecture
/// spec: `block cache → FAT32 → initramfs`.
pub struct BlockCache<D: BlockDevice> {
    inner: D,
    bps: usize,
    entries: Vec<(Option<u32>, Vec<u8>)>,
    capacity: usize,
    next_victim: usize,
}

impl<D: BlockDevice> BlockCache<D> {
    pub fn new(inner: D, capacity: usize) -> Self {
        let bps = inner.sector_size() as usize;
        let mut entries = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            entries.push((None, vec![0u8; bps]));
        }
        Self {
            inner,
            bps,
            entries,
            capacity,
            next_victim: 0,
        }
    }

    fn lookup(&self, lba: u32) -> Option<usize> {
        self.entries.iter().position(|(l, _)| *l == Some(lba))
    }

    /// Read a single sector into `buf`.
    ///
    /// Checks buffer length and LBA validity before any I/O or cache
    /// mutation, so that an error always leaves the cache and device
    /// state unchanged.
    pub fn read_sector(&mut self, lba: u32, buf: &mut [u8]) -> Result<(), BlockError> {
        if buf.len() < self.bps {
            return Err(BlockError::BufferTooSmall {
                required: self.bps,
                provided: buf.len(),
            });
        }
        if (lba as u64) >= self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }
        if let Some(idx) = self.lookup(lba) {
            buf[..self.bps].copy_from_slice(&self.entries[idx].1);
            return Ok(());
        }
        let idx = self.evict_slot();
        let entry = &mut self.entries[idx];
        self.inner.read_sectors(lba, 1, &mut entry.1)?;
        entry.0 = Some(lba);
        buf[..self.bps].copy_from_slice(&entry.1);
        Ok(())
    }

    /// Read a single sector returning a reference to the cached buffer.
    /// The returned reference is invalidated by any subsequent cache
    /// operation.
    pub fn get_sector(&mut self, lba: u32) -> Result<&[u8], BlockError> {
        if (lba as u64) >= self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }
        if let Some(idx) = self.lookup(lba) {
            return Ok(&self.entries[idx].1);
        }
        let idx = self.evict_slot();
        let entry = &mut self.entries[idx];
        self.inner.read_sectors(lba, 1, &mut entry.1)?;
        entry.0 = Some(lba);
        Ok(&self.entries[idx].1)
    }

    fn evict_slot(&mut self) -> usize {
        if self.capacity == 0 {
            panic!("BlockCache capacity is 0");
        }
        // Check if there's a free slot (never used, or was invalidated)
        if let Some(idx) = self.entries.iter().position(|(l, _)| l.is_none()) {
            return idx;
        }
        // Round-robin: take the next victim and advance the pointer
        let idx = self.next_victim;
        self.next_victim = (self.next_victim + 1) % self.capacity;
        idx
    }

    /// Write a single sector and invalidate its cache entry.
    pub fn write_sector(&mut self, lba: u32, buf: &[u8]) -> Result<(), BlockError> {
        if (lba as u64) >= self.inner.total_sectors() {
            return Err(BlockError::LbaOverflow);
        }
        if buf.len() < self.bps {
            return Err(BlockError::BufferTooSmall {
                required: self.bps,
                provided: buf.len(),
            });
        }
        if let Some(idx) = self.lookup(lba) {
            self.entries[idx].0 = None;
        }
        self.inner.write_sectors(lba, 1, buf).map_err(BlockError::Device)
    }

    pub fn sector_size(&self) -> u32 {
        self.bps as u32
    }

    pub fn total_sectors(&self) -> u64 {
        self.inner.total_sectors()
    }
}

impl<D: BlockDevice> BlockDevice for BlockCache<D> {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        let count = count as usize;
        let needed = count.checked_mul(self.bps).ok_or("count * bps overflow")?;
        if buf.len() < needed {
            return Err("buffer too small for multi-sector read");
        }
        let end_lba = (lba as u64) + (count as u64);
        if end_lba > self.inner.total_sectors() || end_lba > u32::MAX as u64 {
            return Err("LBA range exceeds device capacity or 32-bit limit");
        }
        for i in 0..count {
            let off = i * self.bps;
            let sector_buf = &mut buf[off..off + self.bps];
            self.read_sector(lba + i as u32, sector_buf)
                .map_err(|e| match e {
                    BlockError::Device(s) => s,
                    _ => "block cache error",
                })?;
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        let count = count as usize;
        let needed = count.checked_mul(self.bps).ok_or("count * bps overflow")?;
        if buf.len() < needed {
            return Err("buffer too small for multi-sector write");
        }
        let end_lba = (lba as u64) + (count as u64);
        if end_lba > self.inner.total_sectors() {
            return Err("LBA range exceeds device capacity");
        }
        for i in 0..count {
            let off = i * self.bps;
            let sector_buf = &buf[off..off + self.bps];
            self.write_sector(lba + i as u32, sector_buf)
                .map_err(|e| match e {
                    BlockError::Device(s) => s,
                    _ => "block cache error",
                })?;
        }
        Ok(())
    }

    fn sector_size(&self) -> u32 {
        self.bps as u32
    }

    fn total_sectors(&self) -> u64 {
        self.inner.total_sectors()
    }
}

/// Wraps a block device and applies an LBA offset (for partition access).
pub struct PartitionBlockDevice {
    inner: Box<dyn BlockDevice>,
    offset: u32,
}

impl BlockDevice for PartitionBlockDevice {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        self.inner.read_sectors(lba + self.offset, count, buf)
    }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        self.inner.write_sectors(lba + self.offset, count, buf)
    }
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }
    fn total_sectors(&self) -> u64 {
        self.inner
            .total_sectors()
            .saturating_sub(self.offset as u64)
    }
}

impl BlockDevice for Box<dyn BlockDevice> {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        (**self).read_sectors(lba, count, buf)
    }
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        (**self).write_sectors(lba, count, buf)
    }
    fn sector_size(&self) -> u32 {
        (**self).sector_size()
    }
    fn total_sectors(&self) -> u64 {
        (**self).total_sectors()
    }
}

// ── FAT32 Boot Sector (BPB) ──────────────────────────────────

#[repr(C, packed)]
struct FatBootSector {
    jmp_boot: [u8; 3],
    oem_name: [u8; 8],
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sector_count: u16,
    num_fats: u8,
    root_entry_count: u16,
    total_sectors_16: u16,
    media_descriptor: u8,
    sectors_per_fat_16: u16,
    sectors_per_track: u16,
    num_heads: u16,
    hidden_sectors: u32,
    total_sectors_32: u32,
    // FAT32 specific
    sectors_per_fat_32: u32,
    ext_flags: u16,
    fs_version: u16,
    root_cluster: u32,
    fs_info: u16,
    backup_boot_sector: u16,
    reserved: [u8; 12],
    drive_number: u8,
    reserved1: u8,
    boot_signature: u8,
    volume_id: u32,
    volume_label: [u8; 11],
    fs_type: [u8; 8],
}

// ── FAT32 Directory Entry ────────────────────────────────────

#[repr(C, packed)]
struct FatDirEntry {
    name: [u8; 11], // 8.3 format
    attr: u8,
    nt_res: u8,
    crt_time_tenth: u8,
    crt_time: u16,
    crt_date: u16,
    lst_acc_date: u16,
    fst_clus_hi: u16, // FAT32: high 16 bits of first cluster
    wrt_time: u16,
    wrt_date: u16,
    fst_clus_lo: u16, // low 16 bits of first cluster
    file_size: u32,
}

const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_LFN: u8 = 0x0F;

// ── exFAT Boot Sector (VBR) ─────────────────────────────────

/// exFAT Volume Boot Record (first 512 bytes, simplified).
#[repr(C, packed)]
struct ExFatBootSector {
    jmp_boot: [u8; 3],
    oem_name: [u8; 8], // "EXFAT   "
    must_be_zero: [u8; 53],
    partition_offset: u64,
    volume_length: u64,
    fat_offset: u32,          // sector offset to FAT
    fat_length: u32,          // sectors per FAT
    cluster_heap_offset: u32, // sector offset to data area
    cluster_count: u32,
    root_dir_cluster: u32, // first cluster of root dir
    volume_serial: u32,
    fs_revision: u16,
    volume_flags: u16,
    bytes_per_sector_shift: u8,    // log2(bytes per sector)
    sectors_per_cluster_shift: u8, // log2(sectors per cluster)
    number_of_fats: u8,
    drive_select: u8,
    percent_in_use: u8,
    reserved: [u8; 7],
}

// exFAT Directory Entry
//
// exFAT directory entries are 32 bytes each, grouped into sets.
// Key entry types:
const EXFAT_ENTRY_FILE_INFO: u8 = 0x85; // file info (name follows)
const EXFAT_ENTRY_STREAM_EXT: u8 = 0xC0; // stream extension (contains file size)
const EXFAT_ENTRY_FILE_NAME: u8 = 0xC1; // file name (continued)

pub fn is_exfat(boot: &[u8; 512]) -> bool {
    &boot[3..11] == b"EXFAT   "
}

// ── FAT32 / exFAT Filesystem ─────────────────────────────────

/// FAT32 / exFAT filesystem driver over a block device.
///
/// Parses the boot-sector BPB, auto-detects FAT32 vs exFAT,
/// and provides read/write access to files via the [`FileSystem`] trait.
///
/// The underlying block device is wrapped in a [`BlockCache`] so that
/// repeated reads of the FAT table and directory sectors do not
/// re-issue USB I/O.  This is the storage stack foundation
/// (`block cache → FAT32 → initramfs`).
pub struct FatFileSystem {
    device: Box<dyn BlockDevice>,
    bps: u32, // bytes per sector
    spc: u32, // sectors per cluster
    reserved_sectors: u32,
    num_fats: u32,
    sectors_per_fat: u32,
    root_cluster: u32,
    first_data_sector: u32,
    /// true = exFAT, false = FAT32
    is_exfat: bool,
    /// Number of data clusters in the volume
    data_cluster_count: u32,
    /// Open file handles: fd → (cluster, offset, size, path)
    handles: Vec<(u32, u32, u32, u32, String)>,
    next_fd: u32,
    /// Reusable sector buffer to avoid per-call allocation
    sector_buf: Vec<u8>,
}

impl FatFileSystem {
    /// Create a FAT/exFAT filesystem from a block device, auto-detecting
    /// MBR partition tables and parsing the correct boot sector.
    /// Wraps the device in a [`BlockCache`] for repeated reads.
    pub fn from_device(mut device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let lba = find_fat_partition(&mut *device)?;
        if lba > 0 {
            // Repoint the device to read from partition start.
            // We wrap the device to add an LBA offset, then wrap that in a block cache.
            let wrapped = PartitionBlockDevice {
                inner: device,
                offset: lba,
            };
            let cached = BlockCache::new(wrapped, 64);
            return Self::new(Box::new(cached));
        }
        let cached = BlockCache::new(device, 64);
        Self::new(Box::new(cached))
    }

    pub fn new(mut device: Box<dyn BlockDevice>) -> Result<Self, FsError> {
        let mut boot = [0u8; 512];
        device.read_sectors(0, 1, &mut boot)?;

        // Detect exFAT vs FAT32 from the OEM name field
        let exfat = is_exfat(&boot);

        if exfat {
            // SAFETY: ExFatBootSector is #[repr(C, packed)] and the
            // OEM name "EXFAT   " at offset 3 confirms the layout.
            let ebpb: &ExFatBootSector = unsafe { &*(boot.as_ptr() as *const ExFatBootSector) };

            let bps_shift = ebpb.bytes_per_sector_shift as u32;
            if bps_shift < 9 || bps_shift > 12 {
                return Err(FsError::InvalidInput);
            }
            let spc_shift = ebpb.sectors_per_cluster_shift as u32;
            if spc_shift > 25 {
                return Err(FsError::InvalidInput);
            }
            let bps = 1u32 << bps_shift;
            let spc = 1u32 << spc_shift;
            let reserved = ebpb.fat_offset;
            let sectors_per_fat = ebpb.fat_length;
            let num_fats = ebpb.number_of_fats as u32;
            let root_cluster = ebpb.root_dir_cluster;
            let first_data_sector = ebpb.cluster_heap_offset;
            let data_cluster_count = ebpb.cluster_count;

            Ok(Self {
                device,
                bps,
                spc,
                reserved_sectors: reserved,
                num_fats,
                sectors_per_fat,
                root_cluster,
                first_data_sector,
                is_exfat: true,
                data_cluster_count,
                handles: Vec::new(),
                next_fd: 1,
                sector_buf: vec![0u8; bps as usize],
            })
        } else {
            // SAFETY: FatBootSector is #[repr(C, packed)] and boot[..512] is valid.
            // The BPB layout matches the on-disk format read from sector 0.
            let bpb: &FatBootSector = unsafe { &*(boot.as_ptr() as *const FatBootSector) };

            let bps = bpb.bytes_per_sector as u32;
            let spc = bpb.sectors_per_cluster as u32;
            if bps < 512 || bps > 4096 || !bps.is_power_of_two() {
                return Err(FsError::InvalidInput);
            }
            if spc == 0 || !spc.is_power_of_two() {
                return Err(FsError::InvalidInput);
            }
            let reserved = bpb.reserved_sector_count as u32;
            let num_fats = bpb.num_fats as u32;
            let sectors_per_fat = bpb.sectors_per_fat_32;
            let root_cluster = bpb.root_cluster;
            let first_data_sector = reserved + num_fats * sectors_per_fat;

            // Calculate data cluster count for FAT32
            let total_sectors = if bpb.total_sectors_16 != 0 {
                bpb.total_sectors_16 as u32
            } else {
                bpb.total_sectors_32
            };
            let data_sectors = total_sectors.saturating_sub(first_data_sector);
            let data_cluster_count = data_sectors / spc;

            Ok(Self {
                device,
                bps,
                spc,
                reserved_sectors: reserved,
                num_fats,
                sectors_per_fat,
                root_cluster,
                first_data_sector,
                is_exfat: false,
                data_cluster_count,
                handles: Vec::new(),
                next_fd: 1,
                sector_buf: vec![0u8; bps as usize],
            })
        }
    }

    // ── Cluster / sector helpers ─────────────────────────────

    fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.first_data_sector + (cluster - 2) * self.spc
    }

    fn read_fat_entry(&mut self, cluster: u32) -> Result<u32, &'static str> {
        let fat_offset = cluster * 4; // FAT32: 4 bytes per entry
        let sector = self.reserved_sectors + fat_offset / self.bps;
        let offset = (fat_offset % self.bps) as usize;
        self.device.read_sectors(sector, 1, &mut self.sector_buf)?;
        let val = u32::from_le_bytes([
            self.sector_buf[offset],
            self.sector_buf[offset + 1],
            self.sector_buf[offset + 2],
            self.sector_buf[offset + 3],
        ]);
        Ok(val & 0x0FFFFFFF)
    }

    fn is_end_of_chain(cluster: u32) -> bool {
        cluster >= 0x0FFFFFF8
    }

    // ── Path resolution ─────────────────────────────────────

    fn find_entry(&mut self, path: &str) -> Result<(u32, u32, String), &'static str> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Ok((self.root_cluster, 0, String::from("/")));
        }
        let mut cluster = self.root_cluster;
        for component in path.split('/') {
            if component.is_empty() {
                continue;
            }
            match self.find_in_dir(cluster, component) {
                Some((entry_cluster, entry_size, is_dir)) => {
                    if is_dir {
                        cluster = entry_cluster;
                    } else {
                        return Ok((entry_cluster, entry_size, component.into()));
                    }
                }
                None => return Err("not found"),
            }
        }
        Ok((cluster, 0, String::from(path)))
    }

    fn find_in_dir(&mut self, dir_cluster: u32, name: &str) -> Option<(u32, u32, bool)> {
        if self.is_exfat {
            // exFAT: use the entry-set reader
            return self.read_exfat_dir(dir_cluster, name);
        }
        // FAT32: use 8.3 short-name + LFN entries
        let mut cluster = dir_cluster;
        loop {
            let sector = self.cluster_to_sector(cluster);
            let short_name = name_to_83(name);
            for entry_idx in 0..(self.spc * self.bps / 32) {
                let sec = sector + entry_idx / (self.bps / 32);
                let off = ((entry_idx % (self.bps / 32)) * 32) as usize;
                if self
                    .device
                    .read_sectors(sec, 1, &mut self.sector_buf)
                    .is_err()
                {
                    return None;
                }
                // SAFETY: FatDirEntry is #[repr(C, packed)]. `off` is derived from
                // the entry index within the sector, and `sector_buf` has at least 32 bytes remaining from `off`.
                let entry: &FatDirEntry =
                    unsafe { &*(self.sector_buf[off..].as_ptr() as *const FatDirEntry) };
                if entry.name[0] == 0 {
                    return None; // end of directory
                }
                if entry.name[0] == 0xE5 {
                    continue; // deleted entry
                }
                if entry.attr == ATTR_LFN {
                    continue;
                }
                if &entry.name[..] == short_name.as_slice() {
                    let clus = (entry.fst_clus_hi as u32) << 16 | entry.fst_clus_lo as u32;
                    return Some((clus, entry.file_size, entry.attr & ATTR_DIRECTORY != 0));
                }
            }
            // Move to next cluster in chain
            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => return None,
            }
        }
    }

    // ── exFAT helpers ────────────────────────────────────────

    /// Read the directory entries in an exFAT directory cluster chain.
    /// Calls `cb(name, cluster, size)` for each file found.
    fn read_exfat_dir(&mut self, dir_cluster: u32, target_name: &str) -> Option<(u32, u32, bool)> {
        let mut cluster = dir_cluster;
        let mut name_buf = alloc::vec::Vec::new();
        let mut entry_cluster: u32 = 0;
        let mut entry_size: u64 = 0;
        let mut entry_is_dir = false;
        let mut in_entry = false;

        loop {
            let sector_base = self.cluster_to_sector(cluster);
            let dir_size = self.spc * self.bps;
            let mut off = 0u32;
            while off < dir_size {
                let sec = sector_base + off / self.bps;
                let buf_off = (off % self.bps) as usize;
                let mut buf = alloc::vec![0u8; self.bps as usize];
                if self.device.read_sectors(sec, 1, &mut buf).is_err() {
                    return None;
                }
                let entry_type = buf[buf_off];

                if entry_type == 0 {
                    // end of directory — check last entry before giving up
                    if in_entry && !name_buf.is_empty() {
                        let name_str = core::str::from_utf8(&name_buf).unwrap_or("");
                        if name_str.eq_ignore_ascii_case(target_name) {
                            return Some((entry_cluster, entry_size as u32, entry_is_dir));
                        }
                    }
                    return None;
                }
                if entry_type == EXFAT_ENTRY_FILE_INFO {
                    // Check previous entry's name before starting a new entry
                    if in_entry && !name_buf.is_empty() {
                        let name_str = core::str::from_utf8(&name_buf).unwrap_or("");
                        if name_str.eq_ignore_ascii_case(target_name) {
                            return Some((entry_cluster, entry_size as u32, entry_is_dir));
                        }
                    }

                    let attribs = buf[buf_off + 4];
                    entry_is_dir = (attribs & ATTR_DIRECTORY) != 0;
                    entry_cluster = 0;
                    entry_size = 0;
                    name_buf.clear();
                    in_entry = true;
                } else if entry_type == EXFAT_ENTRY_STREAM_EXT && in_entry {
                    // Stream Extension holds first_cluster at bytes 20-23 and data length at bytes 24-31
                    let cl = u32::from_le_bytes([
                        buf[buf_off + 20],
                        buf[buf_off + 21],
                        buf[buf_off + 22],
                        buf[buf_off + 23],
                    ]);
                    entry_cluster = cl;
                    let sz = u64::from_le_bytes([
                        buf[buf_off + 24],
                        buf[buf_off + 25],
                        buf[buf_off + 26],
                        buf[buf_off + 27],
                        buf[buf_off + 28],
                        buf[buf_off + 29],
                        buf[buf_off + 30],
                        buf[buf_off + 31],
                    ]);
                    entry_size = sz;
                } else if entry_type == EXFAT_ENTRY_FILE_NAME && in_entry {
                    // UTF-16LE characters at offset 2-31 (15 chars per entry)
                    for i in 0..15 {
                        let lo = buf[buf_off + 2 + i * 2] as u16;
                        let hi = buf[buf_off + 3 + i * 2] as u16;
                        let cp = (hi << 8) | lo;
                        if cp == 0 {
                            break;
                        }
                        if cp <= 0x7F {
                            name_buf.push(cp as u8);
                        }
                        // Ignore non-ASCII for now (simplified)
                    }
                } else {
                    if in_entry && !name_buf.is_empty() {
                        // Check if this name matches
                        let name_str = core::str::from_utf8(&name_buf).unwrap_or("");
                        if name_str.eq_ignore_ascii_case(target_name) {
                            return Some((entry_cluster, entry_size as u32, entry_is_dir));
                        }
                    }
                    name_buf.clear();
                    in_entry = false;
                }
                off += 32;
            }
            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => return None,
            }
        }
    }

    /// Write a 32-bit entry into the FAT table.
    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), &'static str> {
        let fat_offset = cluster * 4;
        let sector = self.reserved_sectors + fat_offset / self.bps;
        let offset = (fat_offset % self.bps) as usize;
        self.device.read_sectors(sector, 1, &mut self.sector_buf)?;

        // For FAT32, preserve the high nibble (reserved bits) by merging with existing value
        let final_value = if self.is_exfat {
            value
        } else {
            let existing = u32::from_le_bytes([
                self.sector_buf[offset],
                self.sector_buf[offset + 1],
                self.sector_buf[offset + 2],
                self.sector_buf[offset + 3],
            ]);
            (existing & 0xF0000000) | (value & 0x0FFFFFFF)
        };

        self.sector_buf[offset..offset + 4].copy_from_slice(&final_value.to_le_bytes());

        // Write to all FAT copies
        for fat_idx in 0..self.num_fats {
            let fat_sector =
                self.reserved_sectors + fat_idx * self.sectors_per_fat + fat_offset / self.bps;
            self.device.write_sectors(fat_sector, 1, &self.sector_buf)?;
        }
        Ok(())
    }

    /// Scan the FAT and allocate a single free cluster.
    /// Marks it as end-of-chain and returns the cluster number.
    fn allocate_one_cluster(&mut self) -> Result<u32, &'static str> {
        let eoc = if self.is_exfat {
            0xFFFFFFFFu32
        } else {
            0x0FFFFFFFu32
        };
        // Start scanning from cluster 2 (first data cluster)
        let mut cluster = 2u32;
        let max_cluster = 2 + self.data_cluster_count;
        while cluster < max_cluster {
            let fat_offset = cluster * 4;
            let sector = self.reserved_sectors + fat_offset / self.bps;
            let offset = (fat_offset % self.bps) as usize;
            self.device.read_sectors(sector, 1, &mut self.sector_buf)?;
            let val = u32::from_le_bytes([
                self.sector_buf[offset],
                self.sector_buf[offset + 1],
                self.sector_buf[offset + 2],
                self.sector_buf[offset + 3],
            ]);
            let entry = if self.is_exfat { val } else { val & 0x0FFFFFFF };
            if entry == 0 {
                self.write_fat_entry(cluster, eoc)?;
                return Ok(cluster);
            }
            cluster += 1;
        }
        Err("no free clusters")
    }

    /// Write data to file starting at cluster (0 = unallocated).
    /// Allocates clusters on demand when the chain runs out.
    /// Skips `offset` bytes within the cluster chain before writing,
    /// mirroring the offset handling already added to `read()`.
    fn write_file_data(
        &mut self,
        cluster: &mut u32,
        offset: u32,
        data: &[u8],
    ) -> Result<(), &'static str> {
        let mut remaining = data.len() as u32;
        let mut clus = *cluster;
        let mut data_off = 0usize;

        // Allocate first cluster if this is a new file
        if clus == 0 {
            if offset > 0 {
                return Err("cannot write at non-zero offset to unallocated file");
            }
            clus = self.allocate_one_cluster()?;
            *cluster = clus;
        }

        // Skip clusters to reach the starting offset
        let cluster_bytes = self.spc * self.bps;
        let mut remaining_offset = offset;
        while remaining_offset >= cluster_bytes {
            remaining_offset -= cluster_bytes;
            match self.read_fat_entry(clus) {
                Ok(next) if !Self::is_end_of_chain(next) => clus = next,
                _ => {
                    let new_clus = self.allocate_one_cluster()?;
                    if let Err(e) = self.write_fat_entry(clus, new_clus) {
                        let _ = self.write_fat_entry(new_clus, 0);
                        return Err(e);
                    }
                    clus = new_clus;
                }
            }
        }

        loop {
            let sector_base = self.cluster_to_sector(clus);
            let start_sector_idx = remaining_offset / self.bps;
            let mut sector_off = (remaining_offset % self.bps) as usize;
            for i in start_sector_idx..self.spc {
                if remaining == 0 {
                    break;
                }
                let to_write = min(remaining, self.bps - sector_off as u32);
                let mut buf = vec![0u8; self.bps as usize];
                if to_write < self.bps {
                    self.device.read_sectors(sector_base + i, 1, &mut buf)?;
                }
                buf[sector_off..sector_off + to_write as usize]
                    .copy_from_slice(&data[data_off..data_off + to_write as usize]);
                self.device.write_sectors(sector_base + i, 1, &buf)?;
                data_off += to_write as usize;
                remaining -= to_write;
                sector_off = 0;
            }
            remaining_offset = 0;
            if remaining == 0 {
                break;
            }
            match self.read_fat_entry(clus) {
                Ok(next) if !Self::is_end_of_chain(next) => {
                    clus = next;
                }
                _ => {
                    let new_clus = self.allocate_one_cluster()?;
                    if let Err(e) = self.write_fat_entry(clus, new_clus) {
                        let _ = self.write_fat_entry(new_clus, 0);
                        return Err(e);
                    }
                    clus = new_clus;
                }
            }
        }
        Ok(())
    }
}

// ── Directory writing helpers ──────────────────────────────────

impl FatFileSystem {
    /// Find a free directory entry slot: returns (sector, offset_within_sector).
    /// Scans the directory cluster chain for an entry with name[0] == 0 or 0xE5.
    fn find_free_dir_slot(&mut self, dir_cluster: u32) -> Result<(u32, usize), &'static str> {
        let mut cluster = dir_cluster;
        loop {
            let sector_base = self.cluster_to_sector(cluster);
            let total_entries = (self.spc * self.bps / 32) as usize;
            let mut current_sec = !0u32;
            for entry_idx in 0..total_entries {
                let sec = sector_base + (entry_idx as u32) / (self.bps / 32);
                let off = ((entry_idx as u32) % (self.bps / 32) * 32) as usize;
                if sec != current_sec {
                    self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
                    current_sec = sec;
                }
                if self.sector_buf[off] == 0 || self.sector_buf[off] == 0xE5 {
                    return Ok((sec, off));
                }
            }
            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => return Err("no free dir entry"),
            }
        }
    }

    /// Write a FAT32 8.3 short directory entry at the given sector+offset.
    fn write_83_entry(
        &mut self,
        sector: u32,
        offset: usize,
        short_name: &[u8; 11],
        first_cluster: u32,
        file_size: u32,
        is_dir: bool,
    ) -> Result<(), &'static str> {
        let mut buf = vec![0u8; 32];
        buf[..11].copy_from_slice(short_name);
        buf[11] = if is_dir { 0x10 } else { 0x20 }; // attr
        buf[12] = 0; // NT reserved
        // Correct FAT32 cluster field offsets: FstClusHI at 20-21, FstClusLO at 26-27
        let hi = (first_cluster >> 16) as u16;
        let lo = first_cluster as u16;
        buf[20..22].copy_from_slice(&hi.to_le_bytes());
        buf[26..28].copy_from_slice(&lo.to_le_bytes());
        buf[28..32].copy_from_slice(&file_size.to_le_bytes());
        self.device.read_sectors(sector, 1, &mut self.sector_buf)?;
        self.sector_buf[offset..offset + 32].copy_from_slice(&buf);
        self.device.write_sectors(sector, 1, &self.sector_buf)?;
        Ok(())
    }

    /// Write a single LFN (Long File Name) entry before the 8.3 entry.
    fn write_lfn_entry(
        &mut self,
        sector: u32,
        offset: usize,
        seq: u8,
        name_chars: &[u16],
        checksum: u8,
    ) -> Result<(), &'static str> {
        let mut buf = [0u8; 32];
        buf[0] = seq; // sequence number (last fragment has ORD_LAST_LFN = 0x40)
        // Characters 1-5 at bytes 1-10
        for i in 0..5 {
            let cp = if i < name_chars.len() {
                name_chars[i]
            } else if i == name_chars.len() {
                0x0000 // Null terminator
            } else {
                0xFFFF // Padding
            };
            buf[1 + i * 2] = cp as u8;
            buf[2 + i * 2] = (cp >> 8) as u8;
        }
        buf[11] = 0x0F; // ATTR_LFN
        buf[12] = 0; // reserved
        buf[13] = checksum;
        // Characters 6-11 at bytes 14-25
        for i in 0..6 {
            let idx = 5 + i;
            let cp = if idx < name_chars.len() {
                name_chars[idx]
            } else if idx == name_chars.len() {
                0x0000 // Null terminator
            } else {
                0xFFFF // Padding
            };
            buf[14 + i * 2] = cp as u8;
            buf[15 + i * 2] = (cp >> 8) as u8;
        }
        buf[26] = 0;
        buf[27] = 0;
        // Characters 12-13 at bytes 28-31
        for i in 0..2 {
            let idx = 11 + i;
            let cp = if idx < name_chars.len() {
                name_chars[idx]
            } else if idx == name_chars.len() {
                0x0000 // Null terminator
            } else {
                0xFFFF // Padding
            };
            buf[28 + i * 2] = cp as u8;
            buf[29 + i * 2] = (cp >> 8) as u8;
        }
        self.device.read_sectors(sector, 1, &mut self.sector_buf)?;
        self.sector_buf[offset..offset + 32].copy_from_slice(&buf);
        self.device.write_sectors(sector, 1, &self.sector_buf)?;
        Ok(())
    }

    /// Write an exFAT entry set for a new file.
    fn write_exfat_entry_set(
        &mut self,
        dir_cluster: u32,
        name_utf16: &[u16],
        first_cluster: u32,
        file_size: u64,
    ) -> Result<(u32, usize), &'static str> {
        let name_entry_count = if name_utf16.is_empty() {
            1
        } else {
            (name_utf16.len() + 14) / 15
        };
        let total_entries = 2 + name_entry_count; // File Info + Stream Ext + File Name(s)

        // Find a run of consecutive free slots
        let (start_sector, start_off) = self.find_free_exfat_run(dir_cluster, total_entries)?;

        // Build File Info entry (type 0x85) - primary file metadata only
        let mut buf85 = [0u8; 32];
        buf85[0] = 0x85;
        buf85[1] = (total_entries - 1) as u8; // secondary count
        buf85[2] = 0; // checksum (will be computed later)
        buf85[4] = 0x20; // FILE_ATTRIBUTE_ARCHIVE

        // Build Stream Extension entry (type 0xC0) - contains cluster and size
        let mut buf_c0 = [0u8; 32];
        buf_c0[0] = 0xC0;
        buf_c0[1] = 0; // flags
        let name_len = name_utf16.len() as u8;
        buf_c0[3] = name_len;
        buf_c0[4..6].copy_from_slice(&0u16.to_le_bytes()); // name hash (optional)
        buf_c0[8..16].copy_from_slice(&file_size.to_le_bytes());
        buf_c0[20..24].copy_from_slice(&first_cluster.to_le_bytes());
        buf_c0[24..32].copy_from_slice(&file_size.to_le_bytes());

        // Collect all entries for checksum computation
        let mut entry_set = vec![];
        entry_set.push(buf85);
        entry_set.push(buf_c0);

        // Build File Name entries (type 0xC1)
        for chunk_idx in 0..name_entry_count {
            let mut buf_c1 = [0u8; 32];
            buf_c1[0] = 0xC1;
            buf_c1[1] = 0; // flags
            let start = chunk_idx * 15;
            for i in 0..15 {
                let cp = name_utf16.get(start + i).copied().unwrap_or(0);
                buf_c1[2 + i * 2] = cp as u8;
                buf_c1[3 + i * 2] = (cp >> 8) as u8;
            }
            entry_set.push(buf_c1);
        }

        // Compute checksum over all secondary entries (skip primary checksum field)
        let mut checksum: u16 = 0;
        for (idx, entry) in entry_set.iter().enumerate() {
            let skip_start = if idx == 0 { 2 } else { 0 };
            let skip_end = if idx == 0 { 4 } else { 0 };
            for (i, &byte) in entry.iter().enumerate() {
                if idx == 0 && i >= skip_start && i < skip_end {
                    continue;
                }
                checksum = ((checksum << 15) | (checksum >> 1)).wrapping_add(byte as u16);
            }
        }
        entry_set[0][2..4].copy_from_slice(&checksum.to_le_bytes());

        // Write all entries to disk
        for (i, entry) in entry_set.iter().enumerate() {
            let byte_off = start_off + i * 32;
            let sec = start_sector + (byte_off / self.bps as usize) as u32;
            let off = byte_off % self.bps as usize;
            self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
            self.sector_buf[off..off + 32].copy_from_slice(entry);
            self.device.write_sectors(sec, 1, &self.sector_buf)?;
        }

        let fi_byte_off = start_off;
        Ok((start_sector, fi_byte_off))
    }

    /// Find N consecutive free 32-byte entry slots in an exFAT directory.
    fn find_free_exfat_run(
        &mut self,
        dir_cluster: u32,
        count: usize,
    ) -> Result<(u32, usize), &'static str> {
        let mut cluster = dir_cluster;
        loop {
            let sector_base = self.cluster_to_sector(cluster);
            let total_entries = (self.spc * self.bps / 32) as usize;
            let mut current_sec = !0u32;
            let mut run_start = None;
            let mut run_len = 0usize;

            for entry_idx in 0..total_entries {
                let sec = sector_base + (entry_idx as u32) / (self.bps / 32);
                let off = ((entry_idx as u32) % (self.bps / 32) * 32) as usize;

                if sec != current_sec {
                    self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
                    current_sec = sec;
                }

                if self.sector_buf[off] == 0 || self.sector_buf[off] == 0xE5 {
                    if run_start.is_none() {
                        run_start = Some((sec, off));
                    }
                    run_len += 1;
                    if run_len >= count {
                        return Ok(run_start.unwrap());
                    }
                } else {
                    run_start = None;
                    run_len = 0;
                }
            }

            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => return Err("no free exfat dir run"),
            }
        }
    }
}

// ── Directory reading helpers ──────────────────────────────────

impl FatFileSystem {
    /// Read all entries in a FAT32 directory cluster chain.
    fn readdir_fat32_all(&mut self, dir_cluster: u32) -> Result<Vec<VNode>, FsError> {
        let mut entries = Vec::new();
        let mut cluster = dir_cluster;
        // LFN entries precede the main entry in reverse order
        let mut lfn_entries: Vec<[u8; 32]> = Vec::new();

        loop {
            let sector_base = self.cluster_to_sector(cluster);
            let entries_per_sector = self.bps / 32;
            let total_entries = (self.spc * entries_per_sector) as usize;

            let mut current_sector = !0u32;
            for entry_idx in 0..total_entries {
                let sec = sector_base + (entry_idx as u32) / entries_per_sector;
                let off = ((entry_idx as u32) % entries_per_sector * 32) as usize;

                if sec != current_sector {
                    self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
                    current_sector = sec;
                }

                let first_byte = self.sector_buf[off];
                if first_byte == 0 {
                    // End of directory
                    return Ok(entries);
                }
                if first_byte == 0xE5 {
                    // Deleted entry — discard any pending LFN
                    lfn_entries.clear();
                    continue;
                }

                let attr = self.sector_buf[off + 11];
                if attr == ATTR_LFN {
                    let mut raw = [0u8; 32];
                    raw.copy_from_slice(&self.sector_buf[off..off + 32]);
                    lfn_entries.push(raw);
                    continue;
                }

                // Regular entry — reconstruct name
                let name = if lfn_entries.is_empty() {
                    name_from_83(&self.sector_buf[off..off + 11])
                } else {
                    read_lfn_name(&lfn_entries)
                };
                lfn_entries.clear();

                // Skip volume label
                if attr == 0x08 {
                    continue;
                }

                let file_size = u32::from_le_bytes([
                    self.sector_buf[off + 28],
                    self.sector_buf[off + 29],
                    self.sector_buf[off + 30],
                    self.sector_buf[off + 31],
                ]);

                entries.push(VNode {
                    name,
                    size: file_size as u64,
                    is_dir: attr & ATTR_DIRECTORY != 0,
                });
            }

            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => break,
            }
        }

        Ok(entries)
    }

    /// Read all entries in an exFAT directory cluster chain.
    fn readdir_exfat_all(&mut self, dir_cluster: u32) -> Result<Vec<VNode>, FsError> {
        let mut entries = Vec::new();
        let mut cluster = dir_cluster;
        let mut current_size: u64 = 0;
        let mut current_name: Vec<u16> = Vec::new();
        let mut current_is_dir = false;
        let mut in_entry = false;

        loop {
            let sector_base = self.cluster_to_sector(cluster);
            let dir_size = self.spc * self.bps;

            let mut off = 0u32;
            while off < dir_size {
                let sec = sector_base + off / self.bps;
                let buf_off = (off % self.bps) as usize;

                if buf_off == 0 {
                    self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
                }

                let entry_type = self.sector_buf[buf_off];

                if entry_type == 0 {
                    // End of directory — flush last entry before returning
                    if in_entry && !current_name.is_empty() {
                        let name = utf16le_to_string(&current_name);
                        entries.push(VNode {
                            name,
                            size: current_size,
                            is_dir: current_is_dir,
                        });
                    }
                    return Ok(entries);
                }

                if entry_type == EXFAT_ENTRY_FILE_INFO {
                    // Flush previous entry if any
                    if in_entry && !current_name.is_empty() {
                        let name = utf16le_to_string(&current_name);
                        entries.push(VNode {
                            name,
                            size: current_size,
                            is_dir: current_is_dir,
                        });
                    }

                    let file_attributes = self.sector_buf[buf_off + 4];
                    // In exFAT, the file size is stored in the Stream Extension (0xC0),
                    // not in the File Info (0x85) entry.  Initialize to 0 here; it will
                    // be populated when we encounter the accompanying 0xC0 entry.
                    current_size = 0;
                    current_is_dir = (file_attributes & ATTR_DIRECTORY) != 0;
                    current_name.clear();
                    in_entry = true;
                } else if entry_type == EXFAT_ENTRY_STREAM_EXT && in_entry {
                    // Stream Extension: bytes 24-31 hold the data length (file size)
                    let sz = u64::from_le_bytes([
                        self.sector_buf[buf_off + 24],
                        self.sector_buf[buf_off + 25],
                        self.sector_buf[buf_off + 26],
                        self.sector_buf[buf_off + 27],
                        self.sector_buf[buf_off + 28],
                        self.sector_buf[buf_off + 29],
                        self.sector_buf[buf_off + 30],
                        self.sector_buf[buf_off + 31],
                    ]);
                    current_size = sz;
                } else if entry_type == EXFAT_ENTRY_FILE_NAME && in_entry {
                    for i in 0..15 {
                        let lo = self.sector_buf[buf_off + 2 + i * 2] as u16;
                        let hi = self.sector_buf[buf_off + 3 + i * 2] as u16;
                        let cp = (hi << 8) | lo;
                        if cp == 0 {
                            break;
                        }
                        current_name.push(cp);
                    }
                } else {
                    if in_entry && !current_name.is_empty() {
                        let name = utf16le_to_string(&current_name);
                        entries.push(VNode {
                            name,
                            size: current_size,
                            is_dir: current_is_dir,
                        });
                    }
                    current_name.clear();
                    current_is_dir = false;
                    in_entry = false;
                }

                off += 32;
            }

            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => break,
            }
        }

        // Flush last pending entry if we exit via end-of-chain
        if in_entry && !current_name.is_empty() {
            let name = utf16le_to_string(&current_name);
            entries.push(VNode {
                name,
                size: current_size,
                is_dir: current_is_dir,
            });
        }

        Ok(entries)
    }
}

impl FatFileSystem {
    fn update_dir_entry_on_close(
        &mut self,
        path: &str,
        first_cluster: u32,
        file_size: u32,
    ) -> Result<(), &'static str> {
        let path = path.trim_matches('/');
        let (parent_path, file_name) = match path.rfind('/') {
            Some(pos) => (&path[..pos], &path[pos + 1..]),
            None => ("", path),
        };
        let parent_cluster = if parent_path.is_empty() {
            self.root_cluster
        } else {
            match self.find_entry(parent_path) {
                Ok((c, _, _)) => c,
                Err(e) => return Err(e),
            }
        };
        if self.is_exfat {
            let target_utf16: Vec<u16> = file_name.encode_utf16().collect();
            self.update_exfat_entry_internal(
                parent_cluster,
                &target_utf16,
                first_cluster,
                file_size as u64,
            )
        } else {
            let short_name = name_to_83(file_name);
            self.update_83_in_dir(parent_cluster, &short_name, first_cluster, file_size)
        }
    }

    fn update_83_in_dir(
        &mut self,
        dir_cluster: u32,
        short_name: &[u8; 11],
        first_cluster: u32,
        file_size: u32,
    ) -> Result<(), &'static str> {
        let mut cluster = dir_cluster;
        loop {
            let sector_base = self.cluster_to_sector(cluster);
            let entries_per_sector = self.bps / 32;
            let mut current_sec = !0u32;
            for entry_idx in 0..(self.spc * entries_per_sector) as usize {
                let sec = sector_base + (entry_idx as u32) / entries_per_sector;
                let off = ((entry_idx as u32) % entries_per_sector * 32) as usize;
                if sec != current_sec {
                    self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
                    current_sec = sec;
                }
                if self.sector_buf[off] == 0 || self.sector_buf[off] == 0xE5 {
                    continue;
                }
                if self.sector_buf[off + 11] == 0x0F {
                    continue;
                }
                if self.sector_buf[off..off + 11] == *short_name {
                    // Correct FAT32 cluster field offsets: FstClusHI at 20-21, FstClusLO at 26-27
                    let hi = (first_cluster >> 16) as u16;
                    let lo = first_cluster as u16;
                    self.sector_buf[off + 20..off + 22].copy_from_slice(&hi.to_le_bytes());
                    self.sector_buf[off + 26..off + 28].copy_from_slice(&lo.to_le_bytes());
                    self.sector_buf[off + 28..off + 32].copy_from_slice(&file_size.to_le_bytes());
                    self.device.write_sectors(sec, 1, &self.sector_buf)?;
                    return Ok(());
                }
            }
            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => return Err("entry not found for update"),
            }
        }
    }

    fn update_exfat_entry_internal(
        &mut self,
        dir_cluster: u32,
        target_name: &[u16],
        first_cluster: u32,
        file_size: u64,
    ) -> Result<(), &'static str> {
        let mut cluster = dir_cluster;
        let mut name_buf: Vec<u16> = Vec::new();
        let mut in_entry = false;
        let mut found_fi_off = 0u32;
        let mut found_sector_base = 0u32;
        let mut found = false;

        loop {
            let sector_base = self.cluster_to_sector(cluster);
            let dir_size = self.spc * self.bps;
            let mut off = 0u32;
            while off < dir_size && !found {
                let sec = sector_base + off / self.bps;
                let buf_off = (off % self.bps) as usize;

                if buf_off == 0 {
                    self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
                }

                let entry_type = self.sector_buf[buf_off];
                if entry_type == 0 {
                    // Check if we have a pending match before treating 0x00 as "not found"
                    if in_entry && !name_buf.is_empty() && name_buf == target_name {
                        found = true;
                        found_fi_off = off - (name_buf.len() as u32 + 14) / 15 * 32 - 64;
                        found_sector_base = sector_base;
                        break;
                    }
                    return Err("entry not found for exfat update");
                }
                if entry_type == EXFAT_ENTRY_FILE_INFO {
                    if in_entry && !name_buf.is_empty() && name_buf == target_name {
                        found = true;
                        found_fi_off = off - 32 * (((name_buf.len() + 14) / 15 + 2) as u32);
                        found_sector_base = sector_base;
                        break;
                    }
                    name_buf.clear();
                    in_entry = true;
                } else if entry_type == EXFAT_ENTRY_FILE_NAME && in_entry {
                    for i in 0..15 {
                        let lo = self.sector_buf[buf_off + 2 + i * 2] as u16;
                        let hi = self.sector_buf[buf_off + 3 + i * 2] as u16;
                        let cp = (hi << 8) | lo;
                        if cp == 0 {
                            break;
                        }
                        name_buf.push(cp);
                    }
                } else if !(entry_type == EXFAT_ENTRY_STREAM_EXT && in_entry) {
                    if in_entry && !name_buf.is_empty() && name_buf == target_name {
                        found = true;
                        found_fi_off = off - (name_buf.len() as u32 + 14) / 15 * 32 - 64;
                        found_sector_base = sector_base;
                        break;
                    }
                    name_buf.clear();
                    in_entry = false;
                }
                off += 32;
            }
            if found {
                break;
            }
            match self.read_fat_entry(cluster) {
                Ok(next) if !Self::is_end_of_chain(next) => cluster = next,
                _ => return Err("entry not found for exfat update"),
            }
        }

        // ── Update Stream Extension (at fi_off + 32) ─────────────
        let fi_off = found_fi_off;
        let se_off = fi_off + 32;
        let se_sec = found_sector_base + se_off / self.bps;
        let se_buf_off = (se_off % self.bps) as usize;
        self.device.read_sectors(se_sec, 1, &mut self.sector_buf)?;
        self.sector_buf[se_buf_off + 8..se_buf_off + 16].copy_from_slice(&file_size.to_le_bytes());
        self.sector_buf[se_buf_off + 20..se_buf_off + 24]
            .copy_from_slice(&first_cluster.to_le_bytes());
        self.sector_buf[se_buf_off + 24..se_buf_off + 32].copy_from_slice(&file_size.to_le_bytes());
        self.device.write_sectors(se_sec, 1, &self.sector_buf)?;

        // ── Recompute entry-set checksum ─────────────────────────
        let se_setting_sec = found_sector_base + fi_off / self.bps;
        let se_setting_off = (fi_off % self.bps) as usize;
        self.device
            .read_sectors(se_setting_sec, 1, &mut self.sector_buf)?;
        let secondary_count = self.sector_buf[se_setting_off + 1] as usize;
        let total_entries = 1 + secondary_count;

        let mut checksum: u16 = 0;
        for entry_idx in 0..total_entries {
            let byte_off = fi_off + (entry_idx as u32) * 32;
            let sec = found_sector_base + byte_off / self.bps;
            let b_off = (byte_off % self.bps) as usize;
            let mut entry_buf = [0u8; 32];
            self.device.read_sectors(sec, 1, &mut self.sector_buf)?;
            entry_buf.copy_from_slice(&self.sector_buf[b_off..b_off + 32]);
            for j in 0..32 {
                if entry_idx == 0 && j >= 2 && j < 4 {
                    continue;
                }
                checksum = ((checksum << 15) | (checksum >> 1)).wrapping_add(entry_buf[j] as u16);
            }
        }

        let fi_sec = found_sector_base + fi_off / self.bps;
        let fi_buf_off = (fi_off % self.bps) as usize;
        self.device.read_sectors(fi_sec, 1, &mut self.sector_buf)?;
        self.sector_buf[fi_buf_off + 2..fi_buf_off + 4].copy_from_slice(&checksum.to_le_bytes());
        self.device.write_sectors(fi_sec, 1, &self.sector_buf)?;
        Ok(())
    }

    fn create_fat32(&mut self, parent_cluster: u32, name: &str) -> Option<u64> {
        let short_name = name_to_83(name);
        let name_clean = name.trim_end_matches('.');
        let needs_lfn = name_clean.len() > 12 || name_clean.contains('.') || {
            let dot = name_clean.rfind('.').unwrap_or(name_clean.len());
            dot > 8 || (name_clean.len() - dot - 1) > 3
        };
        let lfn_chars: Vec<u16> = if needs_lfn {
            name_clean.encode_utf16().collect()
        } else {
            Vec::new()
        };
        let lfn_entry_count = if lfn_chars.is_empty() {
            0
        } else {
            (lfn_chars.len() + 12) / 13
        };
        let total_entries = 1 + lfn_entry_count;

        let first_cluster = match self.allocate_one_cluster() {
            Ok(c) => c,
            Err(_) => return None,
        };

        if total_entries > 1 {
            let (slot_sector, slot_off) =
                match self.find_free_exfat_run(parent_cluster, total_entries) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = self.write_fat_entry(first_cluster, 0);
                        return None;
                    }
                };

            // Compute checksum for the short name
            let checksum = lfn_checksum(&short_name);

            // Write LFN entries in reverse order (last fragment first)
            for i in 0..lfn_entry_count {
                let chunk_idx = lfn_entry_count - 1 - i;
                let seq = (chunk_idx + 1) as u8;
                let flags = if i == 0 { seq | 0x40 } else { seq };
                let start = chunk_idx * 13;
                let end = core::cmp::min(start + 13, lfn_chars.len());
                let chars_slice: &[u16] = &lfn_chars[start..end];
                let byte_off = slot_off + i * 32;
                let sec = slot_sector + (byte_off / self.bps as usize) as u32;
                let b_off = byte_off % self.bps as usize;
                if self
                    .write_lfn_entry(sec, b_off, flags, chars_slice, checksum)
                    .is_err()
                {
                    let _ = self.write_fat_entry(first_cluster, 0);
                    return None;
                }
            }
            let byte_off = slot_off + lfn_entry_count * 32;
            let sec = slot_sector + (byte_off / self.bps as usize) as u32;
            let b_off = byte_off % self.bps as usize;
            if self
                .write_83_entry(sec, b_off, &short_name, first_cluster, 0, false)
                .is_err()
            {
                let _ = self.write_fat_entry(first_cluster, 0);
                return None;
            }
        } else {
            let (sec, off) = match self.find_free_dir_slot(parent_cluster) {
                Ok(v) => v,
                Err(_) => {
                    let _ = self.write_fat_entry(first_cluster, 0);
                    return None;
                }
            };
            if self
                .write_83_entry(sec, off, &short_name, first_cluster, 0, false)
                .is_err()
            {
                let _ = self.write_fat_entry(first_cluster, 0);
                return None;
            }
        }
        Some(first_cluster as u64)
    }

    fn create_exfat(&mut self, parent_cluster: u32, name: &str) -> Option<u64> {
        let name_utf16: Vec<u16> = name.encode_utf16().collect();
        let first_cluster = match self.allocate_one_cluster() {
            Ok(c) => c,
            Err(_) => return None,
        };
        match self.write_exfat_entry_set(parent_cluster, &name_utf16, first_cluster, 0) {
            Ok(_) => Some(first_cluster as u64),
            Err(_) => {
                // Release allocated cluster on failure
                let _ = self.write_fat_entry(first_cluster, 0);
                None
            }
        }
    }
}

impl FileSystem for FatFileSystem {
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let (cluster, size, _name) = self.find_entry(path).ok()?;
        let fd = self.next_fd;
        self.next_fd += 1;
        self.handles
            .push((fd, cluster, 0, size, String::from(path)));
        Some(FileDescriptor {
            fd,
            ino: cluster as u64,
            offset: 0,
            flags,
        })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let pos = self
            .handles
            .iter()
            .position(|h| h.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        let cluster = self.handles[pos].1;
        let offset = self.handles[pos].2;
        let file_size = self.handles[pos].3;
        let to_read = min(buf.len() as u32, file_size.saturating_sub(offset));
        let mut clus = cluster;
        let mut remaining_offset = offset;
        loop {
            let cluster_bytes = self.spc * self.bps;
            if remaining_offset < cluster_bytes {
                break;
            }
            remaining_offset -= cluster_bytes;
            match self.read_fat_entry(clus) {
                Ok(next) if !Self::is_end_of_chain(next) => clus = next,
                _ => return Ok(0),
            }
        }
        let mut remaining = to_read;
        let mut dst_off = 0usize;
        while remaining > 0 {
            let sector_base = self.cluster_to_sector(clus);
            let start_sector_idx = remaining_offset / self.bps;
            let mut sector_off = (remaining_offset % self.bps) as usize;
            for i in start_sector_idx..self.spc {
                if remaining == 0 {
                    break;
                }
                let sector = sector_base + i;
                let to_copy_in_sector =
                    min(remaining as usize, (self.bps - sector_off as u32) as usize);
                self.device.read_sectors(sector, 1, &mut self.sector_buf)?;
                buf[dst_off..dst_off + to_copy_in_sector]
                    .copy_from_slice(&self.sector_buf[sector_off..sector_off + to_copy_in_sector]);
                dst_off += to_copy_in_sector;
                remaining -= to_copy_in_sector as u32;
                sector_off = 0;
            }
            remaining_offset = 0;
            if remaining > 0 {
                match self.read_fat_entry(clus) {
                    Ok(next) if !Self::is_end_of_chain(next) => clus = next,
                    _ => break,
                }
            }
        }
        self.handles[pos].2 += dst_off as u32;
        Ok(dst_off)
    }

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, FsError> {
        let pos = self
            .handles
            .iter()
            .position(|h| h.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        let cluster = self.handles[pos].1;
        let offset = self.handles[pos].2;
        let mut new_cluster = cluster;
        self.write_file_data(&mut new_cluster, offset, data)?;
        self.handles[pos].1 = new_cluster;
        let len = data.len();
        self.handles[pos].2 += len as u32;
        self.handles[pos].3 = self.handles[pos].2.max(self.handles[pos].3);
        Ok(len)
    }

    fn close(&mut self, fd: u32) -> Result<(), FsError> {
        let pos = self
            .handles
            .iter()
            .position(|h| h.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        let cluster = self.handles[pos].1;
        let final_size = self.handles[pos].3;
        let path = self.handles[pos].4.clone();
        self.update_dir_entry_on_close(&path, cluster, final_size)?;
        self.handles.remove(pos);
        Ok(())
    }

    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), FsError> {
        let h = self
            .handles
            .iter_mut()
            .find(|h| h.0 == fd)
            .ok_or(FsError::InvalidFileDescriptor)?;
        h.2 = pos as u32;
        Ok(())
    }

    fn create(&mut self, path: &str, _kind: InodeType) -> Option<u64> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return None;
        }
        let (parent_path, file_name) = match path.rfind('/') {
            Some(pos) => (&path[..pos], &path[pos + 1..]),
            None => ("", path),
        };
        let parent_cluster = if parent_path.is_empty() {
            self.root_cluster
        } else {
            match self.find_entry(parent_path) {
                Ok((c, _, _)) => c,
                Err(_) => return None,
            }
        };
        if self.is_exfat {
            self.create_exfat(parent_cluster, file_name)
        } else {
            self.create_fat32(parent_cluster, file_name)
        }
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    fn unlink(&mut self, _path: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<VNode>, FsError> {
        let path = path.trim_matches('/');
        let cluster = if path.is_empty() {
            self.root_cluster
        } else {
            let mut cluster = self.root_cluster;
            for component in path.split('/') {
                if component.is_empty() {
                    continue;
                }
                match self.find_in_dir(cluster, component) {
                    Some((entry_cluster, _, is_dir)) => {
                        if !is_dir {
                            return Err(FsError::NotADirectory);
                        }
                        cluster = entry_cluster;
                    }
                    None => return Err(FsError::FileNotFound),
                }
            }
            cluster
        };
        if self.is_exfat {
            self.readdir_exfat_all(cluster)
        } else {
            self.readdir_fat32_all(cluster)
        }
    }

    fn exists(&mut self, path: &str) -> bool {
        self.find_entry(path).is_ok()
    }
}

// ── Utilities ────────────────────────────────────────────────

/// Convert a filename to FAT32 8.3 short name.
fn name_to_83(name: &str) -> [u8; 11] {
    let mut result = [0x20u8; 11]; // spaces
    let bytes = name.as_bytes();
    let dot = name.rfind('.').unwrap_or(name.len());
    let base = &bytes[..dot];
    let ext = if dot < name.len() {
        &bytes[dot + 1..]
    } else {
        &[]
    };
    for (i, &b) in base.iter().enumerate().take(8) {
        result[i] = b.to_ascii_uppercase();
    }
    for (i, &b) in ext.iter().enumerate().take(3) {
        result[8 + i] = b.to_ascii_uppercase();
    }
    result
}

/// Convert a FAT32 8.3 name (11 bytes) into a display String.
fn name_from_83(raw: &[u8]) -> String {
    let mut s = String::new();
    // Trim trailing spaces from base name (bytes 0-7)
    let mut base_len = 8;
    while base_len > 0 && raw[base_len - 1] == 0x20 {
        base_len -= 1;
    }
    // Trim trailing spaces from extension (bytes 8-10)
    let mut ext_len = 3;
    while ext_len > 0 && raw[8 + ext_len - 1] == 0x20 {
        ext_len -= 1;
    }
    for &b in &raw[..base_len] {
        s.push(b as char);
    }
    if ext_len > 0 {
        s.push('.');
        for &b in &raw[8..8 + ext_len] {
            s.push(b as char);
        }
    }
    s
}

/// Reconstruct a long file name from collected LFN entries.
/// LFN entries are stored in reverse order (last LFN entry first in the directory).
/// The caller should pass them in physical order, and this function reverses internally.
fn read_lfn_name(lfn_entries: &[[u8; 32]]) -> String {
    // LFN entries are in physical order: seq N (with 0x40), seq N-1, ..., seq 1
    // We need logical order: seq 1, seq 2, ..., seq N
    // Just reverse the slice.
    let mut utf16: Vec<u16> = Vec::new();
    'outer: for entry in lfn_entries.iter().rev() {
        // Characters 1-5 at bytes 1-10 (5 UTF-16LE chars)
        for i in 0..5 {
            let lo = entry[1 + i * 2] as u16;
            let hi = entry[2 + i * 2] as u16;
            let cp = (hi << 8) | lo;
            if cp == 0 || cp == 0xFFFF {
                break 'outer;
            }
            utf16.push(cp);
        }
        // Characters 6-11 at bytes 14-25 (6 UTF-16LE chars)
        for i in 0..6 {
            let lo = entry[14 + i * 2] as u16;
            let hi = entry[15 + i * 2] as u16;
            let cp = (hi << 8) | lo;
            if cp == 0 || cp == 0xFFFF {
                break 'outer;
            }
            utf16.push(cp);
        }
        // Characters 12-13 at bytes 28-31 (2 UTF-16LE chars)
        for i in 0..2 {
            let lo = entry[28 + i * 2] as u16;
            let hi = entry[29 + i * 2] as u16;
            let cp = (hi << 8) | lo;
            if cp == 0 || cp == 0xFFFF {
                break 'outer;
            }
            utf16.push(cp);
        }
    }
    utf16le_to_string(&utf16)
}

/// UTF-16LE to String conversion using the standard library.
fn utf16le_to_string(codepoints: &[u16]) -> String {
    String::from_utf16_lossy(codepoints)
}

/// Compute the LFN checksum for a FAT32 short name (8.3 format).
fn lfn_checksum(short_name: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for &byte in short_name.iter() {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(byte);
    }
    sum
}

// ── Fake block device for testing ──────────────────────────────

/// A memory-backed block device for testing cache behaviour
/// without real hardware.
#[cfg(test)]
pub struct FakeBlockDevice {
    sectors: Vec<Vec<u8>>,
    sector_size: u32,
}

#[cfg(test)]
impl FakeBlockDevice {
    pub fn new(sector_size: u32, total_sectors: u64) -> Self {
        let buf = vec![0u8; sector_size as usize];
        Self {
            sectors: vec![buf; total_sectors as usize],
            sector_size,
        }
    }

    pub fn fill_sector(&mut self, lba: u32, data: &[u8]) {
        if let Some(sector) = self.sectors.get_mut(lba as usize) {
            let len = data.len().min(sector.len());
            sector[..len].copy_from_slice(&data[..len]);
        }
    }
}

#[cfg(test)]
impl BlockDevice for FakeBlockDevice {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        let end = (lba as usize)
            .checked_add(count as usize)
            .ok_or("LBA overflow")?;
        if end > self.sectors.len() {
            return Err("LBA exceeds device");
        }
        let bps = self.sector_size as usize;
        let needed = (count as usize)
            .checked_mul(bps)
            .ok_or("count * bps overflow")?;
        if buf.len() < needed {
            return Err("buffer too small");
        }
        for i in 0..count as usize {
            let off = i * bps;
            buf[off..off + bps].copy_from_slice(&self.sectors[lba as usize + i][..bps]);
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        let end = (lba as usize)
            .checked_add(count as usize)
            .ok_or("LBA overflow")?;
        if end > self.sectors.len() {
            return Err("LBA exceeds device");
        }
        let bps = self.sector_size as usize;
        let needed = (count as usize)
            .checked_mul(bps)
            .ok_or("count * bps overflow")?;
        if buf.len() < needed {
            return Err("buffer too small");
        }
        for i in 0..count as usize {
            let off = i * bps;
            self.sectors[lba as usize + i][..bps].copy_from_slice(&buf[off..off + bps]);
        }
        Ok(())
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.sectors.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cache(sectors: u64) -> BlockCache<FakeBlockDevice> {
        let inner = FakeBlockDevice::new(512, sectors);
        let mut cache = BlockCache::new(inner, 4);
        // Fill sector 0 with known pattern
        let mut pat = [0u8; 512];
        pat[..4].copy_from_slice(b"PAT0");
        cache.inner.fill_sector(0, &pat);
        // Fill sector 1
        let mut pat1 = [0u8; 512];
        pat1[..4].copy_from_slice(b"PAT1");
        cache.inner.fill_sector(1, &pat1);
        // Fill sector 2
        let mut pat2 = [0u8; 512];
        pat2[..4].copy_from_slice(b"PAT2");
        cache.inner.fill_sector(2, &pat2);
        cache
    }

    #[test]
    fn test_cache_hit() {
        let mut cache = make_cache(10);
        let mut buf = vec![0u8; 512];
        // First read (miss) populates the cache
        cache.read_sector(0, &mut buf).unwrap();
        assert_eq!(&buf[..4], b"PAT0");
        // Second read (hit) returns from cache
        let mut buf2 = vec![0u8; 512];
        cache.read_sector(0, &mut buf2).unwrap();
        assert_eq!(&buf2[..4], b"PAT0");
    }

    #[test]
    fn test_cache_miss() {
        let mut cache = make_cache(10);
        let mut buf = vec![0u8; 512];
        cache.read_sector(1, &mut buf).unwrap();
        assert_eq!(&buf[..4], b"PAT1");
    }

    #[test]
    fn test_buffer_too_small_read() {
        let mut cache = make_cache(10);
        let mut buf = [0u8; 256];
        let err = cache.read_sector(0, &mut buf).unwrap_err();
        assert_eq!(
            err,
            BlockError::BufferTooSmall {
                required: 512,
                provided: 256
            }
        );
    }

    #[test]
    fn test_lba_overflow() {
        let mut cache = make_cache(5);
        let mut buf = vec![0u8; 512];
        let err = cache.read_sector(10, &mut buf).unwrap_err();
        assert_eq!(err, BlockError::LbaOverflow);
    }

    #[test]
    fn test_write_invalidates_cache() {
        let mut cache = make_cache(10);
        let mut buf = vec![0u8; 512];
        // Populate the cache
        cache.read_sector(0, &mut buf).unwrap();
        assert_eq!(&buf[..4], b"PAT0");
        // Write to sector 0 — should invalidate cache entry
        let write_buf = {
            let mut w = vec![0u8; 512];
            w[..4].copy_from_slice(b"NEW0");
            w
        };
        cache.write_sector(0, &write_buf).unwrap();
        // Now read back — should get the new data from device
        let mut buf2 = vec![0u8; 512];
        cache.read_sector(0, &mut buf2).unwrap();
        assert_eq!(&buf2[..4], b"NEW0");
    }

    #[test]
    fn test_round_robin_eviction() {
        let mut cache = make_cache(10);
        let mut buf = vec![0u8; 512];
        // Fill all 4 cache slots with sectors 0..3
        for i in 0..4 {
            cache.read_sector(i, &mut buf).unwrap();
        }
        // Now all slots are full. next_victim should be 0.
        // Reading sector 4 should evict slot 0 (which held sector 0).
        cache.read_sector(4, &mut buf).unwrap();
        // Reading sector 0 again should be a miss (it was evicted)
        let mut buf0 = vec![0u8; 512];
        cache.read_sector(0, &mut buf0).unwrap();
        assert_eq!(&buf0[..4], b"PAT0");
        // next_victim should now be 1. Reading sector 5 should evict slot 1.
        cache.read_sector(5, &mut buf).unwrap();
        let mut buf1 = vec![0u8; 512];
        cache.read_sector(1, &mut buf1).unwrap();
        assert_eq!(&buf1[..4], b"PAT1");
    }

    #[test]
    fn test_multi_sector_buffer_check() {
        let inner = FakeBlockDevice::new(512, 10);
        let mut cache = BlockCache::new(inner, 4);
        // Buffer too small for 2 sectors
        let mut buf = vec![0u8; 512];
        let err = cache.read_sectors(0, 2, &mut buf).unwrap_err();
        assert_eq!(err, "buffer too small for multi-sector read");
    }

    #[test]
    fn test_multi_sector_lba_overflow() {
        let inner = FakeBlockDevice::new(512, 5);
        let mut cache = BlockCache::new(inner, 4);
        // LBA 4 + 2 = 6 > 5
        let mut buf = vec![0u8; 1024];
        let err = cache.read_sectors(4, 2, &mut buf).unwrap_err();
        assert_eq!(err, "LBA range exceeds device capacity");
    }
}
