//! Destructive Fullerene installation onto a registered block device.
//!
//! The installer deliberately owns only the small amount of FAT32/GPT-adjacent
//! layout needed for a bootable UEFI ESP. It does not reuse the mounted VFS:
//! the target disk is exclusively reserved, formatted, and populated through
//! the block-device capability layer.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

const SECTOR_SIZE: usize = 512;
const PARTITION_START: u64 = 2048;
const RESERVED_SECTORS: u32 = 32;
const FAT_COUNT: u32 = 2;
const ROOT_CLUSTER: u32 = 2;
const EOF_CLUSTER: u32 = 0x0FFF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerError {
    NoSuchDevice,
    DeviceBusy,
    UnsupportedSectorSize,
    DeviceTooSmall,
    PayloadUnavailable,
    PayloadTooLarge,
    OutOfMemory,
    DeviceIo,
    InvalidLayout,
}

impl fmt::Display for InstallerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoSuchDevice => "target device not found",
            Self::DeviceBusy => "target device is mounted or busy",
            Self::UnsupportedSectorSize => "installer requires 512-byte sectors",
            Self::DeviceTooSmall => "target device is too small for a Fullerene ESP",
            Self::PayloadUnavailable => "boot payload is unavailable in this boot path",
            Self::PayloadTooLarge => "boot payload is too large for the target FAT32 volume",
            Self::OutOfMemory => "installer ran out of memory",
            Self::DeviceIo => "target block I/O failed",
            Self::InvalidLayout => "target FAT32 layout could not be created",
        })
    }
}

#[derive(Debug, Clone)]
pub struct InstallerDevice {
    pub name: String,
    pub sector_size: u32,
    pub total_sectors: u64,
    pub available: bool,
}

pub fn list_devices() -> Vec<InstallerDevice> {
    crate::devfs::list_block_device_names()
        .into_iter()
        .filter_map(|name| {
            let (sector_size, total_sectors) = crate::devfs::block_device_info(&name)?;
            Some(InstallerDevice {
                available: crate::devfs::block_device_available(&name),
                name,
                sector_size,
                total_sectors,
            })
        })
        .collect()
}

/// Format a target device and install the bootloader and kernel payload.
///
/// This is intentionally a direct, destructive operation. Callers must show
/// the target name and obtain an explicit confirmation before invoking it.
pub fn install(device_name: &str) -> Result<u64, InstallerError> {
    let device_name = device_name.trim_start_matches("/dev/");
    if !crate::devfs::block_device_exists(device_name) {
        return Err(InstallerError::NoSuchDevice);
    }
    if !crate::devfs::block_device_available(device_name) {
        return Err(InstallerError::DeviceBusy);
    }
    let (sector_size, total_sectors) =
        crate::devfs::block_device_info(device_name).ok_or(InstallerError::NoSuchDevice)?;
    if sector_size as usize != SECTOR_SIZE {
        return Err(InstallerError::UnsupportedSectorSize);
    }
    let partition_sectors = total_sectors
        .checked_sub(PARTITION_START)
        .ok_or(InstallerError::DeviceTooSmall)?;
    if partition_sectors < 131_072 || partition_sectors > u32::MAX as u64 {
        return Err(InstallerError::DeviceTooSmall);
    }

    let (bootloader, kernel) = boot_payloads().ok_or(InstallerError::PayloadUnavailable)?;
    let payload_bytes = bootloader
        .len()
        .checked_add(kernel.len())
        .ok_or(InstallerError::PayloadTooLarge)?;
    if payload_bytes > (u32::MAX as usize) {
        return Err(InstallerError::PayloadTooLarge);
    }

    let layout = FatLayout::new(partition_sectors)?;
    let required_clusters = file_clusters(&layout, bootloader.len())
        .checked_add(file_clusters(&layout, kernel.len()))
        .ok_or(InstallerError::PayloadTooLarge)?
        .checked_add(3)
        .ok_or(InstallerError::PayloadTooLarge)?;
    if required_clusters as u64 > layout.cluster_count as u64 {
        return Err(InstallerError::PayloadTooLarge);
    }

    write_mbr(device_name, partition_sectors)?;
    write_fat_boot_sectors(device_name, &layout)?;
    zero_fat_tables(device_name, &layout)?;

    let mut next_cluster = 5u32;
    let efi_cluster = 3u32;
    let boot_cluster = 4u32;
    let bootloader_chain = allocate_chain(&mut next_cluster, &layout, bootloader.len())?;
    let kernel_chain = allocate_chain(&mut next_cluster, &layout, kernel.len())?;

    let chains = [
        (ROOT_CLUSTER, vec![efi_cluster]),
        (efi_cluster, vec![boot_cluster]),
        (boot_cluster, Vec::new()),
        (
            bootloader_chain[0],
            bootloader_chain.iter().copied().skip(1).collect(),
        ),
        (
            kernel_chain[0],
            kernel_chain.iter().copied().skip(1).collect(),
        ),
    ];
    write_fat_tables(device_name, &layout, &chains)?;
    write_directory(
        device_name,
        &layout,
        ROOT_CLUSTER,
        &[("EFI       ", 0x10, efi_cluster, 0)],
    )?;
    write_directory(
        device_name,
        &layout,
        efi_cluster,
        &[("BOOT       ", 0x10, boot_cluster, 0)],
    )?;
    write_directory(
        device_name,
        &layout,
        boot_cluster,
        &[
            ("BOOTX64 EFI", 0x20, bootloader_chain[0], bootloader.len()),
            ("KERNEL  EFI", 0x20, kernel_chain[0], kernel.len()),
        ],
    )?;
    write_file(device_name, &layout, &bootloader_chain, bootloader)?;
    write_file(device_name, &layout, &kernel_chain, kernel)?;

    Ok(payload_bytes as u64)
}

struct FatLayout {
    partition_sectors: u64,
    sectors_per_cluster: u32,
    fat_sectors: u32,
    data_start: u64,
    cluster_count: u32,
}

impl FatLayout {
    fn new(partition_sectors: u64) -> Result<Self, InstallerError> {
        let mut sectors_per_cluster = 1u32;
        let mut fat_sectors = 1u32;
        for _ in 0..12 {
            let data_sectors = partition_sectors
                .checked_sub(RESERVED_SECTORS as u64 + FAT_COUNT as u64 * fat_sectors as u64)
                .ok_or(InstallerError::InvalidLayout)?;
            let clusters = data_sectors / sectors_per_cluster as u64;
            if clusters > 0x0FFF_FFF5 && sectors_per_cluster < 128 {
                sectors_per_cluster *= 2;
                continue;
            }
            let next_fat = ((clusters + 2) * 4).div_ceil(SECTOR_SIZE as u64) as u32;
            // A FAT may contain a small amount of intentional slack. Requiring
            // exact equality oscillates for layouts where the sector rounding
            // alternates between N and N+1 sectors.
            if next_fat <= fat_sectors {
                if !(65_525..=0x0FFF_FFF5).contains(&clusters) {
                    return Err(InstallerError::DeviceTooSmall);
                }
                let data_start = PARTITION_START
                    + RESERVED_SECTORS as u64
                    + FAT_COUNT as u64 * fat_sectors as u64;
                return Ok(Self {
                    partition_sectors,
                    sectors_per_cluster,
                    fat_sectors,
                    data_start,
                    cluster_count: clusters as u32,
                });
            }
            fat_sectors = next_fat;
        }
        Err(InstallerError::InvalidLayout)
    }

    fn cluster_lba(&self, cluster: u32) -> u64 {
        self.data_start + (cluster as u64 - 2) * self.sectors_per_cluster as u64
    }
}

fn file_clusters(layout: &FatLayout, bytes: usize) -> usize {
    bytes.div_ceil(layout.sectors_per_cluster as usize * SECTOR_SIZE)
}

fn allocate_chain(
    next: &mut u32,
    layout: &FatLayout,
    bytes: usize,
) -> Result<Vec<u32>, InstallerError> {
    let count = file_clusters(layout, bytes);
    let end = next
        .checked_add(count as u32)
        .ok_or(InstallerError::PayloadTooLarge)?;
    if end > layout.cluster_count.saturating_add(2) {
        return Err(InstallerError::PayloadTooLarge);
    }
    let chain = (*next..end).collect();
    *next = end;
    Ok(chain)
}

fn write_mbr(device: &str, partition_sectors: u64) -> Result<(), InstallerError> {
    let mut sector = [0u8; SECTOR_SIZE];
    let entry = &mut sector[0x1BE..0x1CE];
    entry[0] = 0x80;
    entry[4] = 0xEF; // EFI System Partition (MBR type)
    entry[8..12].copy_from_slice(&(PARTITION_START as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&(partition_sectors as u32).to_le_bytes());
    sector[510..512].copy_from_slice(&0xAA55u16.to_le_bytes());
    write_sector(device, 0, &sector)
}

fn write_fat_boot_sectors(device: &str, layout: &FatLayout) -> Result<(), InstallerError> {
    let mut boot = [0u8; SECTOR_SIZE];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"FULLEREN");
    boot[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    boot[13] = layout.sectors_per_cluster as u8;
    boot[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    boot[16] = FAT_COUNT as u8;
    boot[21] = 0xF8;
    boot[32..36].copy_from_slice(&(layout.partition_sectors as u32).to_le_bytes());
    boot[36..40].copy_from_slice(&layout.fat_sectors.to_le_bytes());
    boot[44..48].copy_from_slice(&ROOT_CLUSTER.to_le_bytes());
    boot[48..50].copy_from_slice(&1u16.to_le_bytes());
    boot[50..52].copy_from_slice(&6u16.to_le_bytes());
    boot[64] = 0x80;
    boot[66] = 0x29;
    boot[67..71].copy_from_slice(&0x46554C52u32.to_le_bytes());
    boot[71..82].copy_from_slice(b"FULLERENE  ");
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510..512].copy_from_slice(&0xAA55u16.to_le_bytes());
    write_sector(device, PARTITION_START, &boot)?;
    write_sector(device, PARTITION_START + 6, &boot)?;

    let mut fsinfo = [0u8; SECTOR_SIZE];
    fsinfo[0..4].copy_from_slice(&0x41615252u32.to_le_bytes());
    fsinfo[484..488].copy_from_slice(&0x61417272u32.to_le_bytes());
    fsinfo[488..492].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    fsinfo[492..496].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    fsinfo[508..512].copy_from_slice(&0xAA550000u32.to_le_bytes());
    write_sector(device, PARTITION_START + 1, &fsinfo)
}

fn zero_fat_tables(device: &str, layout: &FatLayout) -> Result<(), InstallerError> {
    let zero = [0u8; SECTOR_SIZE * 8];
    for fat in 0..FAT_COUNT {
        let start =
            PARTITION_START + RESERVED_SECTORS as u64 + fat as u64 * layout.fat_sectors as u64;
        write_zero(device, start, layout.fat_sectors as u64, &zero)?;
    }
    Ok(())
}

fn write_fat_tables(
    device: &str,
    layout: &FatLayout,
    chains: &[(u32, Vec<u32>)],
) -> Result<(), InstallerError> {
    let last_cluster = chains
        .iter()
        .flat_map(|(_, chain)| {
            chain
                .iter()
                .copied()
                .chain(core::iter::once(*chain.last().unwrap_or(&2)))
        })
        .max()
        .unwrap_or(2);
    let fat_entries_per_sector = SECTOR_SIZE / 4;
    let mut sector_index = 0u32;
    while sector_index <= last_cluster / fat_entries_per_sector as u32 {
        let mut sector = [0u8; SECTOR_SIZE];
        for entry in 0..fat_entries_per_sector {
            let cluster = sector_index * fat_entries_per_sector as u32 + entry as u32;
            let value = fat_value(cluster, chains);
            sector[entry * 4..entry * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        for fat in 0..FAT_COUNT {
            let lba = PARTITION_START
                + RESERVED_SECTORS as u64
                + fat as u64 * layout.fat_sectors as u64
                + sector_index as u64;
            write_sector(device, lba, &sector)?;
        }
        sector_index += 1;
    }
    Ok(())
}

fn fat_value(cluster: u32, chains: &[(u32, Vec<u32>)]) -> u32 {
    if cluster == 0 {
        return 0x0FFF_FFF8;
    }
    if cluster == 1 {
        return EOF_CLUSTER;
    }

    // Directory chains and file chains use adjacent records. Resolve anchors
    // first so EFI's directory cluster is not mistaken for ROOT's continuation.
    for (anchor, chain) in chains {
        if cluster == *anchor {
            return chain.first().copied().unwrap_or(EOF_CLUSTER);
        }
    }
    for (_directory_cluster, chain) in chains {
        for (index, current) in chain.iter().enumerate() {
            if cluster == *current {
                return chain.get(index + 1).copied().unwrap_or(EOF_CLUSTER);
            }
        }
    }
    0
}

fn write_directory(
    device: &str,
    layout: &FatLayout,
    cluster: u32,
    entries: &[(&str, u8, u32, usize)],
) -> Result<(), InstallerError> {
    let mut sector = [0u8; SECTOR_SIZE];
    for (index, (name, attr, first_cluster, size)) in entries.iter().enumerate() {
        let offset = index * 32;
        if offset + 32 > sector.len() || name.len() != 11 {
            return Err(InstallerError::InvalidLayout);
        }
        sector[offset..offset + 11].copy_from_slice(name.as_bytes());
        sector[offset + 11] = *attr;
        sector[offset + 20..offset + 22]
            .copy_from_slice(&((*first_cluster >> 16) as u16).to_le_bytes());
        sector[offset + 26..offset + 28].copy_from_slice(&(*first_cluster as u16).to_le_bytes());
        sector[offset + 28..offset + 32].copy_from_slice(&(*size as u32).to_le_bytes());
    }
    write_sector(device, layout.cluster_lba(cluster), &sector)
}

fn write_file(
    device: &str,
    layout: &FatLayout,
    chain: &[u32],
    data: &[u8],
) -> Result<(), InstallerError> {
    let cluster_bytes = layout.sectors_per_cluster as usize * SECTOR_SIZE;
    for (index, cluster) in chain.iter().enumerate() {
        let start = index * cluster_bytes;
        let end = (start + cluster_bytes).min(data.len());
        let mut chunk = vec![0u8; cluster_bytes];
        chunk[..end.saturating_sub(start)].copy_from_slice(&data[start..end]);
        write_blocks(device, layout.cluster_lba(*cluster), &chunk)?;
    }
    Ok(())
}

fn write_zero(
    device: &str,
    mut lba: u64,
    mut sectors: u64,
    zero: &[u8; SECTOR_SIZE * 8],
) -> Result<(), InstallerError> {
    while sectors != 0 {
        let count = sectors.min(8) as u16;
        write_blocks(device, lba, &zero[..count as usize * SECTOR_SIZE])?;
        lba += count as u64;
        sectors -= count as u64;
    }
    Ok(())
}

fn write_sector(device: &str, lba: u64, sector: &[u8; SECTOR_SIZE]) -> Result<(), InstallerError> {
    write_blocks(device, lba, sector)
}

fn write_blocks(device: &str, lba: u64, data: &[u8]) -> Result<(), InstallerError> {
    let count = data.len().div_ceil(SECTOR_SIZE);
    if count == 0 || count > u16::MAX as usize || data.len() % SECTOR_SIZE != 0 {
        return Err(InstallerError::InvalidLayout);
    }
    crate::devfs::write_block_device(device, lba, count as u16, data)
        .map_err(|_| InstallerError::DeviceIo)
}

fn boot_payloads() -> Option<(&'static [u8], &'static [u8])> {
    let args_ptr = crate::contexts::boot::with_boot(|boot| boot.kernel_args)?;
    let args = unsafe { args_ptr.as_ref()? };
    let offset = petroleum::common::memory::get_physical_memory_offset() as u64;
    let bootloader = payload_slice(
        args.bootloader_image_ptr,
        args.bootloader_image_size,
        offset,
    )?;
    let kernel = payload_slice(args.kernel_image_ptr, args.kernel_image_size, offset)?;
    Some((bootloader, kernel))
}

fn payload_slice(ptr: u64, size: u64, offset: u64) -> Option<&'static [u8]> {
    if ptr == 0 || size == 0 || size > 64 * 1024 * 1024 {
        return None;
    }
    let virtual_ptr = ptr.checked_add(offset)? as *const u8;
    Some(unsafe { core::slice::from_raw_parts(virtual_ptr, size as usize) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat_chains_prioritize_directory_anchors() {
        let chains = [
            (ROOT_CLUSTER, vec![3]),
            (3, vec![4]),
            (4, Vec::new()),
            (5, vec![6]),
            (7, Vec::new()),
        ];

        assert_eq!(fat_value(0, &chains), 0x0FFF_FFF8);
        assert_eq!(fat_value(1, &chains), EOF_CLUSTER);
        assert_eq!(fat_value(2, &chains), 3);
        assert_eq!(fat_value(3, &chains), 4);
        assert_eq!(fat_value(4, &chains), EOF_CLUSTER);
        assert_eq!(fat_value(5, &chains), 6);
        assert_eq!(fat_value(6, &chains), EOF_CLUSTER);
        assert_eq!(fat_value(7, &chains), EOF_CLUSTER);
        assert_eq!(fat_value(8, &chains), 0);
    }

    #[test]
    fn fat32_layout_is_available_at_installer_minimum() {
        let layout = FatLayout::new(131_072).expect("minimum installer disk");
        assert!(layout.cluster_count >= 65_525);
        assert!(layout.data_start > PARTITION_START);
        assert!(layout.cluster_lba(ROOT_CLUSTER) < PARTITION_START + layout.partition_sectors);
    }
}
