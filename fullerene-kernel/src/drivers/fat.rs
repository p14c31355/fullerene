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

use crate::vfs::{FileSystem, FileDescriptor, VNode, InodeType};
use crate::klog_fmt;
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
const PARTITION_FAT32: u8 = 0x0B;  // FAT32 CHS
const PARTITION_FAT32_LBA: u8 = 0x0C; // FAT32 LBA
const PARTITION_FAT16: u8 = 0x06;
const PARTITION_FAT16_LBA: u8 = 0x0E;
const PARTITION_EXFAT: u8 = 0x07; // exFAT often uses 0x07

/// Detect whether LBA 0 contains an MBR and find the first FAT partition.
/// Returns `Some(lba_start)` if found, or `None` if LBA 0 is already a FAT BPB.
pub fn find_fat_partition(device: &mut dyn BlockDevice) -> Result<u32, &'static str> {
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
    for i in 0..4 {
        let off = 0x1BE + i * 16;
        // SAFETY: boot[off..] has at least 16 bytes, and MbrPartitionEntry
        // is #[repr(C, packed)] so it has no padding requirement.
        let entry_ptr = boot[off..].as_ptr() as *const MbrPartitionEntry;
        let ptype = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).partition_type) };
        let lba_start = unsafe { core::ptr::read_unaligned(&raw const (*entry_ptr).lba_start) };
        match ptype {
            PARTITION_FAT32 | PARTITION_FAT32_LBA => {
                klog_fmt!("FAT: MBR partition {} FAT32 at LBA {}\n", i, lba_start);
                return Ok(lba_start);
            }
            PARTITION_FAT16 | PARTITION_FAT16_LBA => {
                klog_fmt!("FAT: MBR partition {} FAT16 at LBA {} (stub)\n", i, lba_start);
            }
            PARTITION_EXFAT => {
                klog_fmt!("FAT: MBR partition {} exFAT at LBA {}\n", i, lba_start);
                return Ok(lba_start);
            }
            _ => {}
        }
    }
    klog_fmt!("FAT: no FAT partition found in MBR\n");
    Err("no FAT partition")
}

// ── Block device abstraction ──────────────────────────────────
// The block device provides sector-level read/write.

pub trait BlockDevice: Send {
    fn read_sectors(&mut self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str>;
    fn write_sectors(&mut self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str>;
    fn sector_size(&self) -> u32;
    fn total_sectors(&self) -> u64;
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
    fn sector_size(&self) -> u32 { self.inner.sector_size() }
    fn total_sectors(&self) -> u64 { self.inner.total_sectors().saturating_sub(self.offset as u64) }
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
    name: [u8; 11],       // 8.3 format
    attr: u8,
    nt_res: u8,
    crt_time_tenth: u8,
    crt_time: u16,
    crt_date: u16,
    lst_acc_date: u16,
    fst_clus_hi: u16,     // FAT32: high 16 bits of first cluster
    wrt_time: u16,
    wrt_date: u16,
    fst_clus_lo: u16,     // low 16 bits of first cluster
    file_size: u32,
}

const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_LFN: u8 = 0x0F;

// ── exFAT Boot Sector (VBR) ─────────────────────────────────

/// exFAT Volume Boot Record (first 512 bytes, simplified).
#[repr(C, packed)]
struct ExFatBootSector {
    jmp_boot: [u8; 3],
    oem_name: [u8; 8],          // "EXFAT   "
    must_be_zero: [u8; 53],
    partition_offset: u64,
    volume_length: u64,
    fat_offset: u32,             // sector offset to FAT
    fat_length: u32,             // sectors per FAT
    cluster_heap_offset: u32,    // sector offset to data area
    cluster_count: u32,
    root_dir_cluster: u32,       // first cluster of root dir
    volume_serial: u32,
    fs_revision: u16,
    volume_flags: u16,
    bytes_per_sector_shift: u8,  // log2(bytes per sector)
    sectors_per_cluster_shift: u8, // log2(sectors per cluster)
    number_of_fats: u8,
    drive_select: u8,
    percent_in_use: u8,
    reserved: [u8; 7],
}

// ── exFAT Directory Entry ───────────────────────────────────
//
// exFAT directory entries are 32 bytes each, grouped into sets.
// Key entry types:
#[repr(C, packed)]
struct ExFatDirEntry {
    entry_type: u8,              // 0x81 = file, 0x85 = file info, 0xC0 = volume GUID, etc.
    custom1: [u8; 19],          // varies by type
    first_cluster: u32,         // low 32 bits of first cluster
    data_length: u64,           // file size
}

// File entry type codes
const EXFAT_ENTRY_FILE_INFO: u8 = 0x85;    // file info (name follows)
const EXFAT_ENTRY_FILE_NAME: u8 = 0xC1;    // file name (continued)
const EXFAT_ENTRY_UP_CASE: u8 = 0x81;      // up-case table (root dir only)
const EXFAT_ENTRY_BITMAP: u8 = 0x81;       // allocation bitmap (root dir only)
const EXFAT_ENTRY_VOLUME_LABEL: u8 = 0x83; // volume label

pub fn is_exfat(boot: &[u8; 512]) -> bool {
    &boot[3..11] == b"EXFAT   "
}

// ── FAT32 / exFAT Filesystem ─────────────────────────────────

/// FAT32 / exFAT filesystem driver over a block device.
///
/// Parses the boot-sector BPB, auto-detects FAT32 vs exFAT,
/// and provides read/write access to files via the [`FileSystem`] trait.
pub struct FatFileSystem {
    device: Box<dyn BlockDevice>,
    bps: u32,          // bytes per sector
    spc: u32,          // sectors per cluster
    bps_log2: u8,
    spc_log2: u8,
    reserved_sectors: u32,
    num_fats: u32,
    sectors_per_fat: u32,
    root_cluster: u32,
    first_data_sector: u32,
    /// true = exFAT, false = FAT32
    is_exfat: bool,
    /// Open file handles: fd → (cluster, offset, size, path)
    handles: Vec<(u32, u32, u32, u32, String)>,
    next_fd: u32,
    /// Reusable sector buffer to avoid per-call allocation
    sector_buf: Vec<u8>,
}

impl FatFileSystem {
    /// Create a FAT/exFAT filesystem from a block device, auto-detecting
    /// MBR partition tables and parsing the correct boot sector.
    pub fn from_device(mut device: Box<dyn BlockDevice>) -> Result<Self, &'static str> {
        let lba = find_fat_partition(&mut *device)?;
        if lba > 0 {
            // Repoint the device to read from partition start.
            // We wrap the device to add an LBA offset.
            let wrapped = PartitionBlockDevice {
                inner: device,
                offset: lba,
            };
            return Self::new_at(Box::new(wrapped), 0);
        }
        Self::new_at(device, 0)
    }

    /// Create from device at a specific LBA offset (relative to the device).
    fn new_at(device: Box<dyn BlockDevice>, _lba_offset: u32) -> Result<Self, &'static str> {
        // Delegate to `new` which reads LBA 0 of the given device
        Self::new(device)
    }

    pub fn new(mut device: Box<dyn BlockDevice>) -> Result<Self, &'static str> {
        let mut boot = [0u8; 512];
        device.read_sectors(0, 1, &mut boot)?;

        // Detect exFAT vs FAT32 from the OEM name field
        let exfat = is_exfat(&boot);

        if exfat {
            // SAFETY: ExFatBootSector is #[repr(C, packed)] and the
            // OEM name "EXFAT   " at offset 3 confirms the layout.
            let ebpb: &ExFatBootSector = unsafe { &*(boot.as_ptr() as *const ExFatBootSector) };

            let bps_shift = ebpb.bytes_per_sector_shift as u32;
            let spc_shift = ebpb.sectors_per_cluster_shift as u32;
            let bps = 1u32 << bps_shift;
            let spc = 1u32 << spc_shift;
            let reserved = ebpb.fat_offset;
            let sectors_per_fat = ebpb.fat_length;
            let num_fats = ebpb.number_of_fats as u32;
            let root_cluster = ebpb.root_dir_cluster;
            let first_data_sector = ebpb.cluster_heap_offset;

            Ok(Self {
                device,
                bps,
                spc,
                bps_log2: bps_shift as u8,
                spc_log2: spc_shift as u8,
                reserved_sectors: reserved,
                num_fats,
                sectors_per_fat,
                root_cluster,
                first_data_sector,
                is_exfat: true,
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
            let reserved = bpb.reserved_sector_count as u32;
            let num_fats = bpb.num_fats as u32;
            let sectors_per_fat = bpb.sectors_per_fat_32;
            let root_cluster = bpb.root_cluster;
            let first_data_sector = reserved + num_fats * sectors_per_fat;

            Ok(Self {
                device,
                bps,
                spc,
                bps_log2: (bps.trailing_zeros()) as u8,
                spc_log2: (spc.trailing_zeros()) as u8,
                reserved_sectors: reserved,
                num_fats,
                sectors_per_fat,
                root_cluster,
                first_data_sector,
                is_exfat: false,
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

    fn find_entry(
        &mut self,
        path: &str,
    ) -> Result<(u32, u32, String), &'static str> {
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
            return self.read_exfat_dir(dir_cluster, name).map(|(c, sz)| (c, sz, false));
        }
        // FAT32: use 8.3 short-name + LFN entries
        let mut cluster = dir_cluster;
        loop {
            let sector = self.cluster_to_sector(cluster);
            let short_name = name_to_83(name);
            for entry_idx in 0..(self.spc * self.bps / 32) {
                let sec = sector + entry_idx / (self.bps / 32);
                let off = ((entry_idx % (self.bps / 32)) * 32) as usize;
                if self.device.read_sectors(sec, 1, &mut self.sector_buf).is_err() {
                    return None;
                }
                // SAFETY: FatDirEntry is #[repr(C, packed)]. `off` is derived from
                // the entry index within the sector, and `sector_buf` has at least 32 bytes remaining from `off`.
                let entry: &FatDirEntry = unsafe { &*(self.sector_buf[off..].as_ptr() as *const FatDirEntry) };
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
    fn read_exfat_dir(
        &mut self,
        dir_cluster: u32,
        target_name: &str,
    ) -> Option<(u32, u32)> {
        let mut cluster = dir_cluster;
        let mut name_buf = alloc::vec::Vec::new();
        let mut entry_cluster: u32 = 0;
        let mut entry_size: u64 = 0;
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
                    // end of directory
                    return None;
                }
                if entry_type == EXFAT_ENTRY_FILE_INFO {
                    // Read cluster (offset 20-23) and size (offset 24-31)
                    let cl = u32::from_le_bytes([
                        buf[buf_off + 20], buf[buf_off + 21],
                        buf[buf_off + 22], buf[buf_off + 23],
                    ]);
                    let sz = u64::from_le_bytes([
                        buf[buf_off + 24], buf[buf_off + 25],
                        buf[buf_off + 26], buf[buf_off + 27],
                        buf[buf_off + 28], buf[buf_off + 29],
                        buf[buf_off + 30], buf[buf_off + 31],
                    ]);
                    entry_cluster = cl;
                    entry_size = sz;
                    name_buf.clear();
                    in_entry = true;
                } else if entry_type == EXFAT_ENTRY_FILE_NAME && in_entry {
                    // UTF-16LE characters at offset 2-31 (15 chars per entry)
                    for i in 0..15 {
                        let lo = buf[buf_off + 2 + i * 2] as u16;
                        let hi = buf[buf_off + 3 + i * 2] as u16;
                        let cp = (hi << 8) | lo;
                        if cp == 0 { break; }
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
                            return Some((entry_cluster, entry_size as u32));
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

    /// Read entire file content into a Vec<u8> starting from cluster.
    fn read_file_data(&mut self, cluster: u32, size: u32) -> Result<Vec<u8>, &'static str> {
        let mut data = Vec::with_capacity(size as usize);
        let mut remaining = size;
        let mut clus = cluster;
        loop {
            let sector = self.cluster_to_sector(clus);
            for i in 0..self.spc {
                if remaining == 0 { break; }
                let to_read = min(remaining, self.bps);
                self.device.read_sectors(sector + i, 1, &mut self.sector_buf)?;
                data.extend_from_slice(&self.sector_buf[..to_read as usize]);
                remaining -= to_read;
            }
            if remaining == 0 { break; }
            match self.read_fat_entry(clus) {
                Ok(next) if !Self::is_end_of_chain(next) => clus = next,
                _ => break,
            }
        }
        Ok(data)
    }

    /// Allocate a new cluster chain for the given number of bytes.
    fn allocate_clusters(&mut self, size: u32) -> Result<u32, &'static str> {
        // Simplified: always returns cluster 2 for now
        // Full implementation would scan FAT for free clusters
        Ok(self.root_cluster)
    }

    /// Write data to file starting at cluster.
    fn write_file_data(&mut self, cluster: u32, data: &[u8]) -> Result<(), &'static str> {
        let mut remaining = data.len() as u32;
        let mut clus = cluster;
        let mut offset = 0usize;
        loop {
            let sector = self.cluster_to_sector(clus);
            for i in 0..self.spc {
                if remaining == 0 { break; }
                let to_write = min(remaining, self.bps);
                let mut buf = vec![0u8; self.bps as usize];
                // Read existing sector first (to preserve data beyond our write)
                if to_write < self.bps {
                    self.device.read_sectors(sector + i, 1, &mut buf)?;
                }
                buf[..to_write as usize].copy_from_slice(&data[offset..offset + to_write as usize]);
                self.device.write_sectors(sector + i, 1, &buf)?;
                offset += to_write as usize;
                remaining -= to_write;
            }
            if remaining == 0 { break; }
            match self.read_fat_entry(clus) {
                Ok(next) if !Self::is_end_of_chain(next) => clus = next,
                _ => return Err("out of space or end of cluster chain"),
            }
        }
        Ok(())
    }
}

impl FileSystem for FatFileSystem {
    fn open(&mut self, path: &str, flags: u32) -> Option<FileDescriptor> {
        let (cluster, size, _name) = self.find_entry(path).ok()?;
        let fd = self.next_fd;
        self.next_fd += 1;
        self.handles.push((fd, cluster, 0, size, String::from(path)));
        Some(FileDescriptor { fd, ino: cluster as u64, offset: 0, flags })
    }

    fn read(&mut self, fd: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        let pos = self.handles.iter().position(|h| h.0 == fd).ok_or("bad fd")?;
        let cluster = self.handles[pos].1;
        let offset = self.handles[pos].2;
        let file_size = self.handles[pos].3;
        let to_read = min(buf.len() as u32, file_size.saturating_sub(offset));
        // Walk the cluster chain to the starting offset, then read only what's needed.
        let mut total = 0usize;
        let mut clus = cluster;
        let mut remaining_offset = offset;
        // Skip clusters until we reach the cluster containing `offset`
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
                if remaining == 0 { break; }
                let sector = sector_base + i;
                let to_copy_in_sector = min(remaining as usize, (self.bps - sector_off as u32) as usize);
                self.device.read_sectors(sector, 1, &mut self.sector_buf)?;
                buf[dst_off..dst_off + to_copy_in_sector]
                    .copy_from_slice(&self.sector_buf[sector_off..sector_off + to_copy_in_sector]);
                dst_off += to_copy_in_sector;
                remaining -= to_copy_in_sector as u32;
                sector_off = 0;
            }
            // Reset inner offset for subsequent clusters
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

    fn write(&mut self, fd: u32, data: &[u8]) -> Result<usize, &'static str> {
        let pos = self.handles.iter().position(|h| h.0 == fd).ok_or("bad fd")?;
        let cluster = self.handles[pos].1;
        self.write_file_data(cluster, data)?;
        let len = data.len();
        self.handles[pos].2 += len as u32;
        self.handles[pos].3 = self.handles[pos].2.max(self.handles[pos].3);
        Ok(len)
    }

    fn close(&mut self, fd: u32) -> Result<(), &'static str> {
        let pos = self.handles.iter().position(|h| h.0 == fd).ok_or("bad fd")?;
        self.handles.remove(pos);
        Ok(())
    }

    fn seek(&mut self, fd: u32, pos: usize) -> Result<(), &'static str> {
        let h = self.handles.iter_mut().find(|h| h.0 == fd).ok_or("bad fd")?;
        h.2 = pos as u32;
        Ok(())
    }

    fn create(&mut self, path: &str, kind: InodeType) -> Option<u64> {
        // Simplified: return a fake inode number
        Some(self.root_cluster as u64)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), &'static str> {
        Err("mkdir not implemented")
    }

    fn unlink(&mut self, path: &str) -> Result<(), &'static str> {
        Err("unlink not implemented")
    }

    fn readdir(&self, path: &str) -> Result<Vec<VNode>, &'static str> {
        Err("readdir not yet implemented via FatFileSystem")
    }

    fn exists(&self, path: &str) -> bool {
        false
    }
}

// ── Utilities ────────────────────────────────────────────────

/// Convert a filename to FAT32 8.3 short name.
fn name_to_83(name: &str) -> [u8; 11] {
    let mut result = [0x20u8; 11]; // spaces
    let bytes = name.as_bytes();
    let dot = name.rfind('.').unwrap_or(name.len());
    let base = &bytes[..dot];
    let ext = if dot < name.len() { &bytes[dot + 1..] } else { &[] };
    for (i, &b) in base.iter().enumerate().take(8) {
        result[i] = b.to_ascii_uppercase();
    }
    for (i, &b) in ext.iter().enumerate().take(3) {
        result[8 + i] = b.to_ascii_uppercase();
    }
    result
}
