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
const BOOT_CATALOG_SECTOR: u32 = 20;
const BOOT_IMAGE_SECTOR: u32 = 21;

fn pad_sector(f: &mut File) -> io::Result<()> {
    let pos = f.seek(SeekFrom::Current(0))?;
    let pad = SECTOR_SIZE as u64 - (pos % SECTOR_SIZE as u64);
    if pad != SECTOR_SIZE as u64 {
        f.write_all(&vec![0u8; pad as usize])?;
    }
    Ok(())
}

// Simple CRC16 (for Validation Entry)
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
    pvd[40..48].copy_from_slice(b"FULLERENE");
    pvd[128..132].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes()); // block size
    iso.write_all(&pvd)?;

    // Boot Record Volume Descriptor
    let mut brvd = [0u8; SECTOR_SIZE];
    brvd[0] = 0;
    brvd[1..6].copy_from_slice(b"CD001");
    brvd[6] = 1;
    brvd[7..39].copy_from_slice(b"EL TORITO SPECIFICATION".as_bytes());
    brvd[71..75].copy_from_slice(&BOOT_CATALOG_SECTOR.to_le_bytes());
    iso.write_all(&brvd)?;

    // Volume Descriptor Terminator
    let mut term = [0u8; SECTOR_SIZE];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;
    iso.write_all(&term)?;

    // Pad up to Boot Catalog sector
    while (iso.seek(SeekFrom::Current(0))? / SECTOR_SIZE as u64) < BOOT_CATALOG_SECTOR as u64 {
        iso.write_all(&[0u8; SECTOR_SIZE])?;
    }

    // Boot Catalog
    let mut cat = [0u8; SECTOR_SIZE];
    // Validation Entry
    cat[0] = 1; // header id
    cat[1] = 0; // platform (x86)
    let crc = crc16(&cat[0..30]);
    cat[28..30].copy_from_slice(&crc.to_le_bytes());
    cat[30] = 0x55;
    cat[31] = 0xAA;

    // Default Entry
    let mut entry = [0u8; 32];
    entry[0] = 0x88; // bootable, no emulation
    entry[1] = 0x00; // media type (no emulation)
    entry[4..6].copy_from_slice(&0u16.to_le_bytes()); // load segment
    entry[6] = 0xEF; // system type = EFI
    entry[8..10].copy_from_slice(&(1u16).to_le_bytes()); // sector count (dummy)
    entry[28..32].copy_from_slice(&BOOT_IMAGE_SECTOR.to_le_bytes()); // boot image LBA

    cat[32..64].copy_from_slice(&entry);
    iso.write_all(&cat)?;

    // Pad up to Boot Image sector
    while (iso.seek(SeekFrom::Current(0))? / SECTOR_SIZE as u64) < BOOT_IMAGE_SECTOR as u64 {
        iso.write_all(&[0u8; SECTOR_SIZE])?;
    }

    // Boot Image (FAT32)
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
