// fullerene/flasks/src/disk.rs
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

// ---------------- FAT32 Partition ----------------
fn copy_to_fat<T: Read + Write + Seek>(
    dir: &fatfs::Dir<T>,
    src_file: &mut File,
    dest: &str,
) -> io::Result<()> {
    let mut f = dir.create_file(dest)?;
    src_file.seek(SeekFrom::Start(0))?;
    io::copy(src_file, &mut f)?;
    Ok(())
}

pub fn create_fat32_image(path: &Path, bellows: &mut File, kernel: &mut File) -> io::Result<File> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    file.set_len(64 * 1024 * 1024)?; // 64 MiB
    {
        // Format FAT32
        fatfs::format_volume(
            &mut file,
            FormatVolumeOptions::new().fat_type(FatType::Fat32),
        )?;
        let fs = FileSystem::new(&mut file, FsOptions::new())?;
        let root = fs.root_dir();
        root.create_dir("EFI")?;
        root.create_dir("EFI/BOOT")?;
        copy_to_fat(&root, bellows, "EFI/BOOT/BOOTX64.EFI")?;
        copy_to_fat(&root, kernel, "EFI/BOOT/KERNEL.EFI")?;
    }
    Ok(file)
}

// ---------------- ISO / El Torito ----------------
const SECTOR_SIZE: usize = 2048;

fn pad_sector(f: &mut File) -> io::Result<()> {
    let pos = f.seek(SeekFrom::Current(0))?;
    let pad = SECTOR_SIZE as u64 - (pos % SECTOR_SIZE as u64);
    if pad != SECTOR_SIZE as u64 {
        f.write_all(&vec![0u8; pad as usize])?;
    }
    Ok(())
}

// Simple CRC16 (for Validation Entry) - No longer used for El Torito checksum
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn create_iso(path: &Path, fat32_img: &Path) -> io::Result<()> {
    let mut iso = File::create(path)?;
    iso.write_all(&vec![0u8; SECTOR_SIZE * 16])?; // System Area

    // Primary Volume Descriptor
    let mut pvd = [0u8; SECTOR_SIZE];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    let mut volume_id = [0u8; 32];
    let project_name = b"FULLERENE";
    volume_id[..project_name.len()].copy_from_slice(project_name);
    // Pad with spaces
    for i in project_name.len()..32 {
        volume_id[i] = b' ';
    }
    pvd[40..72].copy_from_slice(&volume_id);

    let fat32_img_sectors = (fs::metadata(fat32_img)?.len() as u32 + SECTOR_SIZE as u32 - 1) / SECTOR_SIZE as u32;

    // Define the layout of the ISO
    // System Area: 16 sectors
    // PVD: 1 sector (LBA 16)
    // Boot Record VD: 1 sector (LBA 17)
    // Terminator VD: 1 sector (LBA 18)
    // Boot Catalog: 1 sector (LBA 19)
    // Boot Image (FAT32): fat32_img_sectors (LBA 20)

    let total_sectors = 16 + 1 + 1 + 1 + 1 + fat32_img_sectors; // System Area + PVD + BRVD + Terminator + Boot Catalog + FAT32 Image

    // Write Volume Space Size
    pvd[80..84].copy_from_slice(&total_sectors.to_le_bytes());
    pvd[84..88].copy_from_slice(&total_sectors.to_be_bytes());

    // Root Directory Record (for PVD)
    let mut root_dir_record = [0u8; 34]; 
    root_dir_record[0] = 34; // Length of Directory Record (LEN_DR)
    root_dir_record[1] = 0; // Extended Attribute Record Length (XARL)
    root_dir_record[2..6].copy_from_slice(&20u32.to_le_bytes()); // Location of Extent (LBA) - Little Endian (LBA of FAT32 image)
    root_dir_record[6..10].copy_from_slice(&20u32.to_be_bytes()); // Location of Extent (LBA) - Big Endian (LBA of FAT32 image)
    root_dir_record[10..14].copy_from_slice(&fat32_img_sectors.to_le_bytes()); // Data Length (size of directory in bytes) - Little Endian
    root_dir_record[14..18].copy_from_slice(&fat32_img_sectors.to_be_bytes()); // Data Length (size of directory in bytes) - Big Endian
    // Recording Date and Time (7 bytes) - all zeros for now
    root_dir_record[25] = 0x02; // File Flags (Directory bit set)
    root_dir_record[26] = 0; // File Unit Size
    root_dir_record[27] = 0; // Interleave Gap Size
    root_dir_record[28..30].copy_from_slice(&1u16.to_le_bytes()); // Volume Sequence Number - Little Endian
    root_dir_record[30..32].copy_from_slice(&1u16.to_be_bytes()); // Volume Sequence Number - Big Endian
    root_dir_record[32] = 1; // Length of File Identifier (LEN_FI)
    root_dir_record[33] = 0x00; // File Identifier (FI) - Root directory identifier
    pvd[156..190].copy_from_slice(&root_dir_record);

    pvd[128..132].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes()); // block size
    iso.write_all(&pvd)?;

    // Boot Record Volume Descriptor
    let mut brvd = [0u8; SECTOR_SIZE];
    brvd[0] = 0; // Type: Boot Record
    brvd[1..6].copy_from_slice(b"CD001"); // Standard Identifier
    brvd[6] = 1; // Version
    let mut el_torito_spec = [0u8; 32];
    let spec_name = b"EL TORITO SPECIFICATION";
    el_torito_spec[..spec_name.len()].copy_from_slice(spec_name);
    for i in spec_name.len()..32 {
        el_torito_spec[i] = 0x00;
    }
    brvd[7..39].copy_from_slice(&el_torito_spec); // Boot System Identifier
    brvd[71..75].copy_from_slice(&19u32.to_le_bytes()); // Boot Catalog LBA (LBA 19)
    iso.write_all(&brvd)?;

    // Volume Descriptor Terminator
    let mut term = [0u8; SECTOR_SIZE];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;
    iso.write_all(&term)?;

    // Pad up to Boot Catalog sector (LBA 19)
    while (iso.seek(SeekFrom::Current(0))? / SECTOR_SIZE as u64) < 19 { // LBA 19
        iso.write_all(&[0u8; SECTOR_SIZE])?;
    }

    // Boot Catalog (LBA 19)
    let mut cat = [0u8; SECTOR_SIZE];
    // Validation Entry (bytes 0-31)
    cat[0] = 1; // Header ID
    cat[1] = 0xEF; // Platform ID (UEFI)
    cat[2..4].copy_from_slice(&0u16.to_le_bytes()); // reserved
    cat[30] = 0x55; // Key Bytes
    cat[31] = 0xAA; // Key Bytes
    // Calculate checksum
    let mut sum: u16 = 0;
    for i in (0..32).step_by(2) {
        sum = sum.wrapping_add(u16::from_le_bytes([cat[i], cat[i + 1]]));
    }
    let checksum = 0u16.wrapping_sub(sum);
    cat[28..30].copy_from_slice(&checksum.to_le_bytes()); // Checksum

    // Initial/Default Entry (bytes 32-63)
    let mut entry = [0u8; 32];
    entry[0] = 0x88; // Boot Indicator (bootable, no emulation)
    entry[1] = 0x00; // Boot Media Type (no emulation)
    entry[2..4].copy_from_slice(&0u16.to_le_bytes()); // Load Segment (0 for UEFI)
    entry[4] = 0x00; // System Type (0 for UEFI)
    entry[5] = 0x00; // Unused
    entry[6..8].copy_from_slice(&((fat32_img_sectors * 4) as u16).to_le_bytes()); // Sector Count in 512-byte sectors
    entry[8..12].copy_from_slice(&20u32.to_le_bytes()); // LBA of Boot Image (LBA 20) - our FAT32 image
    cat[32..64].copy_from_slice(&entry);
    iso.write_all(&cat)?;

    // Pad up to Boot Image sector (LBA 20)
    while (iso.seek(SeekFrom::Current(0))? / SECTOR_SIZE as u64) < 20 { // LBA 20
        iso.write_all(&[0u8; SECTOR_SIZE])?;
    }

    // Boot Image (FAT32) (LBA 20)
    let mut f = File::open(fat32_img)?;
    io::copy(&mut f, &mut iso)?;
    pad_sector(&mut iso)?;
    Ok(())
}

// ---------------- Unified Entry ----------------
pub fn create_disk_and_iso(
    fat32_img: &Path,
    iso: &Path,
    bellows: &mut File,
    kernel: &mut File,
) -> io::Result<()> {
    let _disk = create_fat32_image(fat32_img, bellows, kernel)?;
    create_iso(iso, fat32_img)?;
    Ok(())
}