use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

// ---------------- Partition IO ----------------
pub struct PartitionIo {
    file: File,
    offset: u64,
    size: u64,
}
impl PartitionIo {
    pub fn new(mut file: File, offset: u64, size: u64) -> io::Result<Self> {
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self { file, offset, size })
    }
    pub fn into_inner(self) -> File {
        self.file
    }
}
impl Read for PartitionIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.file.stream_position()? - self.offset;
        let remaining = self.size.saturating_sub(pos);
        if remaining == 0 {
            return Ok(0);
        }
        let n = std::cmp::min(buf.len() as u64, remaining);
        self.file.read(&mut buf[..n as usize])
    }
}
impl Write for PartitionIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let pos = self.file.stream_position()? - self.offset;
        let remaining = self.size.saturating_sub(pos);
        if remaining == 0 {
            return Ok(0);
        }
        let n = std::cmp::min(buf.len() as u64, remaining);
        self.file.write(&buf[..n as usize])
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for PartitionIo {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(p) => (self.size as i64 + p) as u64,
            SeekFrom::Current(p) => {
                let cur = self.file.stream_position()? - self.offset;
                (cur as i64 + p) as u64
            }
        };
        if new > self.size {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond partition"));
        }
        self.file.seek(SeekFrom::Start(self.offset + new))?;
        Ok(new)
    }
}

// ---------------- FAT helper ----------------
fn copy_to_fat<T: Read + Write + Seek>(dir: &fatfs::Dir<T>, src_file: &mut File, dest: &str) -> io::Result<()> {
    let mut f = dir.create_file(dest)?;
    src_file.seek(SeekFrom::Start(0))?;
    io::copy(src_file, &mut f)?;
    Ok(())
}

// ---------------- Disk Image (.img) ----------------
fn create_disk_image(path: &Path, bellows: &mut File, kernel: &mut File) -> io::Result<File> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    let mut file = OpenOptions::new().read(true).write(true).create(true).open(path)?;
    file.set_len(256 * 1024 * 1024)?; // 256 MiB

    let lb_size = LogicalBlockSize::Lb512;
    let sector_size = lb_size.as_u64();
    let part = {
        let mut gpt = GptConfig::default()
            .writable(true)
            .logical_block_size(lb_size)
            .create_from_device(&mut file, None)
            .unwrap();
        let size = (64 * 1024 * 1024) / sector_size; // 64 MiB
        let id = gpt.add_partition("EFI", size, partition_types::EFI, 0, None).unwrap();
        let part = gpt.partitions()[&id].clone();
        gpt.write().unwrap();
        part
    };

    let mut part_io = PartitionIo::new(file, part.first_lba * sector_size, (part.last_lba - part.first_lba + 1) * sector_size)?;
    {
        fatfs::format_volume(&mut part_io, FormatVolumeOptions::new().fat_type(FatType::Fat32))?;
        let fs = FileSystem::new(&mut part_io, FsOptions::new())?;
        let root = fs.root_dir();
        root.create_dir("EFI")?;
        root.create_dir("EFI/BOOT")?;
        copy_to_fat(&root, bellows, "EFI/BOOT/BOOTX64.EFI")?;
        copy_to_fat(&root, kernel, "EFI/BOOT/KERNEL.EFI")?;
    }

    Ok(part_io.into_inner())
}

// ---------------- ISO (.iso, El Torito UEFI) ----------------
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

fn create_iso(path: &Path, disk_img: &Path) -> io::Result<()> {
    let mut iso = File::create(path)?;
    iso.write_all(&vec![0u8; SECTOR_SIZE * 16])?; // system area

    // Primary Volume Descriptor (PVD)
    let mut pvd = [0u8; SECTOR_SIZE];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[40..49].copy_from_slice(b"FULLERENE");
    iso.write_all(&pvd)?;

    // Boot Record Volume Descriptor (BRVD)
    let mut brvd = [0u8; SECTOR_SIZE];
    brvd[0] = 0;
    brvd[1..6].copy_from_slice(b"CD001");
    brvd[6] = 1;
    brvd[7..39].copy_from_slice(b"EL TORITO SPECIFICATION\0\0\0\0\0\0\0\0\0");
    brvd[71..75].copy_from_slice(&BOOT_CATALOG_SECTOR.to_le_bytes());
    iso.write_all(&brvd)?;

    // Volume Descriptor Terminator
    let mut term = [0u8; SECTOR_SIZE];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;
    iso.write_all(&term)?;

    // Boot Catalog
    while (iso.seek(SeekFrom::Current(0))? / SECTOR_SIZE as u64) < BOOT_CATALOG_SECTOR as u64 {
        iso.write_all(&[0u8; SECTOR_SIZE])?;
    }
    let mut cat = [0u8; SECTOR_SIZE];
    cat[0] = 1;      // Validation Entry
    cat[30] = 0x55;
    cat[31] = 0xAA;
    cat[32] = 0x88;  // Boot Indicator
    cat[33] = 0xEF;  // EFI Media Type
    cat[40..44].copy_from_slice(&BOOT_IMAGE_SECTOR.to_le_bytes()); // Pointer to boot image
    iso.write_all(&cat)?;

    // Boot Image (embed disk_img)
    while (iso.seek(SeekFrom::Current(0))? / SECTOR_SIZE as u64) < BOOT_IMAGE_SECTOR as u64 {
        iso.write_all(&[0u8; SECTOR_SIZE])?;
    }
    let mut f = File::open(disk_img)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    iso.write_all(&buf)?;
    pad_sector(&mut iso)?;
    Ok(())
}

// ---------------- Unified Entry ----------------
pub fn create_disk_and_iso(
    img: &Path,
    iso: &Path,
    bellows: &mut File,
    kernel: &mut File,
) -> io::Result<()> {
    let _disk = create_disk_image(img, bellows, kernel)?;
    create_iso(iso, img)?;
    Ok(())
}
