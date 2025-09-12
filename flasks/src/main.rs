// fullerene/flasks/src/main.rs
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{GptConfig, disk::LogicalBlockSize, partition_types};
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    process::Command,
};
use uuid::Uuid;

use std::fs::OpenOptions;

/// A wrapper around a File that limits I/O to a specific partition offset and size.
struct PartitionIo<'a> {
    file: &'a mut fs::File,
    offset: u64,
    size: u64,
    current_pos: u64,
}

impl<'a> PartitionIo<'a> {
    fn new(file: &'a mut fs::File, offset: u64, size: u64) -> io::Result<Self> {
        Ok(PartitionIo {
            file,
            offset,
            size,
            current_pos: 0,
        })
    }
}

impl<'a> Read for PartitionIo<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.current_pos);
        let bytes_to_read = std::cmp::min(buf.len() as u64, remaining) as usize;
        if bytes_to_read == 0 {
            return Ok(0);
        }

        self.file
            .seek(SeekFrom::Start(self.offset + self.current_pos))?;
        let bytes_read = self.file.read(&mut buf[..bytes_to_read])?;
        self.current_pos += bytes_read as u64;
        Ok(bytes_read)
    }
}

impl<'a> Write for PartitionIo<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.current_pos);
        let bytes_to_write = std::cmp::min(buf.len() as u64, remaining) as usize;
        if bytes_to_write == 0 {
            return Ok(0);
        }

        self.file
            .seek(SeekFrom::Start(self.offset + self.current_pos))?;
        let bytes_written = self.file.write(&buf[..bytes_to_write])?;
        self.current_pos += bytes_written as u64;
        Ok(bytes_written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl<'a> Seek for PartitionIo<'a> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(p) => (self.size as i64 + p) as u64,
            SeekFrom::Current(p) => (self.current_pos as i64 + p) as u64,
        };

        if new_pos > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek beyond end of partition",
            ));
        }
        self.current_pos = new_pos;
        Ok(self.current_pos)
    }
}

/// Copy a file into the FAT filesystem, creating directories as needed
fn copy_to_fat<T: Read + Write + Seek>(
    fs: &FileSystem<T>,
    src: &Path,
    dest: &str,
) -> std::io::Result<()> {
    let dest_path = Path::new(dest);
    let mut dir = fs.root_dir();

    // Create intermediate directories
    if let Some(parent) = dest_path.parent() {
        for component in parent.iter() {
            let name = component.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "Non-UTF8 path component")
            })?;
            let found = dir
                .iter()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().eq_ignore_ascii_case(name));
            dir = if found {
                dir.open_dir(name)?
            } else {
                dir.create_dir(name)?
            };
        }
    }

    // Create and write file
    let mut f = dir.create_file(dest_path.file_name().unwrap().to_str().unwrap())?;
    let mut data = Vec::new();
    let mut src_f = fs::File::open(src)?;
    src_f.read_to_end(&mut data)?;
    f.write_all(&data)?;
    f.flush()?;
    Ok(())
}

fn main() -> std::io::Result<()> {
    // 1. Build fullerene-kernel
    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            "fullerene-kernel",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "fullerene-kernel build failed",
        ));
    }

    // 2. Build bellows (UEFI bootloader)
    let status = Command::new("cargo")
        .args([
            "build",
            "--package",
            "bellows",
            "--release",
            "--target",
            "x86_64-uefi.json",
            "-Z",
            "build-std=core,alloc,compiler_builtins",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "bellows build failed"));
    }

    // 3. Create disk image with GPT partition table and EFI System Partition
    let disk_img_path = Path::new("esp.img");
    if disk_img_path.exists() {
        fs::remove_file(disk_img_path)?;
    }

    // NOTE:
    // The fatfs formatter requires sizes in ranges where FAT type selection is possible.
    // Some "unfortunate" disk sizes can't be auto-selected. To avoid that, we choose a
    // safe image size and explicitly request FAT32 formatting.
    //
    // Use 128 MiB to ensure FAT32 is selectable comfortably.
    let disk_size_bytes = 128 * 1024 * 1024; // 128 MB (increased from 64MB)
    let disk_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(disk_img_path)?;

    // Set the file length BEFORE creating the GPT disk object
    disk_file.set_len(disk_size_bytes)?;
    // It's good practice to sync the file to ensure length is committed
    disk_file.sync_all()?;

    // Create GptConfig first to get usable LBA ranges
    let gpt_config = GptConfig::new()
        .writable(true)
        .logical_block_size(LogicalBlockSize::Lb512);

    // Create the disk object from the configured GptConfig
    let mut gpt_disk = gpt_config
        .create_from_device(disk_file, None)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to create GPT disk: {}", e),
            )
        })?;

    let first_lba = gpt_disk.primary_header().unwrap().first_usable;
    let last_lba = gpt_disk.primary_header().unwrap().last_usable;
    let esp_size_lba = if last_lba >= first_lba {
        last_lba - first_lba + 1
    } else {
        0
    };

    dbg!(first_lba, last_lba, esp_size_lba);

    if esp_size_lba == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Calculated ESP partition size is 0, cannot create partition.",
        ));
    }

    // Add EFI System Partition (ESP)
    let _esp_guid = Uuid::new_v4();

    // 1. add_partition
    gpt_disk
        .add_partition(
            "EFI System Partition",
            first_lba,
            partition_types::EFI,
            esp_size_lba,
            None,
        )
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to add ESP: {}", e)))?;

    // Write GPT changes to disk and retrieve the underlying File
    let mut disk_file_after_gpt = gpt_disk.write().map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to write GPT to disk: {}", e),
        )
    })?;

    drop(disk_file_after_gpt);

    let mut disk_file_after_gpt = File::options().read(true).write(true).open("esp.img")?;

    let mut disk_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(disk_img_path)?;
    let gpt_disk = GptConfig::new()
        .writable(true)
        .logical_block_size(LogicalBlockSize::Lb512)
        .open_from_device(&mut disk_file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

    println!("GPT loaded successfully: {:?}", gpt_disk);
    // Get partition info before writing, as write consumes gpt_disk
    let esp_partition_info = gpt_disk
        .partitions()
        .iter()
        .find(|(_, p)| p.part_type_guid == partition_types::EFI)
        .map(|(_, p)| p.clone())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ESP not found after creation"))?;

    // Format the EFI System Partition (ESP)
    let block_size = gpt_disk.logical_block_size().as_u64();

    let esp_offset_bytes = esp_partition_info.first_lba * block_size;
    // The volume size in bytes must be a multiple of the logical block size.
    let esp_size_bytes =
        (esp_partition_info.last_lba - esp_partition_info.first_lba + 1) * block_size;
    dbg!(esp_offset_bytes, esp_size_bytes);
    // Ensure ESP is large enough for FAT32 (choose a safe lower bound: 8 MiB)
    // (fatfs internals are happier with reasonable sizes; using 8MiB+ avoids "unfortunate disk size")
    if esp_size_bytes < 8 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("ESP too small for FAT32: {} bytes", esp_size_bytes),
        ));
    }

    // Force FAT32 to avoid auto-selection failure
    let fmt_options = FormatVolumeOptions::new()
        .volume_label(*b" FULLERENE ")
        .fat_type(FatType::Fat32);

    // Create PartitionIo which limits format to the partition region
    let mut esp_io_for_format =
        PartitionIo::new(&mut disk_file_after_gpt, esp_offset_bytes, esp_size_bytes)?;
    fatfs::format_volume(&mut esp_io_for_format, fmt_options)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("FAT format failed: {}", e)))?;

    // Use the retrieved disk_file_after_gpt for PartitionIo
    // Recreate a fresh PartitionIo because format consumed the previous writer's cursor.
    let esp_io_for_fs =
        PartitionIo::new(&mut disk_file_after_gpt, esp_offset_bytes, esp_size_bytes)?;
    let fs = FileSystem::new(esp_io_for_fs, FsOptions::new()).map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to open FAT filesystem: {}", e),
        )
    })?;

    // 5. Copy EFI files into FAT32
    let bellows_efi = Path::new("target/x86_64-uefi/release/bellows");
    let kernel_efi = Path::new("target/x86_64-uefi/release/fullerene-kernel");

    if !bellows_efi.exists() {
        panic!("bellows EFI not found: {}", bellows_efi.display());
    }
    if !kernel_efi.exists() {
        panic!("fullerene-kernel EFI not found: {}", kernel_efi.display());
    }

    copy_to_fat(&fs, bellows_efi, "EFI/BOOT/BOOTX64.EFI")?;
    copy_to_fat(&fs, kernel_efi, "kernel.efi")?;

    drop(fs); // flush filesystem

    // 6. Copy OVMF_VARS.fd if missing
    let ovmf_code = "/usr/share/OVMF/OVMF_CODE_4M.fd";
    let ovmf_vars = "./OVMF_VARS.fd";
    if !Path::new(ovmf_vars).exists() {
        fs::copy("/usr/share/OVMF/OVMF_VARS_4M.fd", ovmf_vars)?;
    }

    // 7. Run QEMU
    let qemu_args = [
        "-drive",
        &format!("if=pflash,format=raw,readonly=on,file={}", ovmf_code),
        "-drive",
        &format!("if=pflash,format=raw,file={}", ovmf_vars),
        "-drive",
        "file=esp.img,format=raw,if=ide",
        "-m",
        "512M",
        "-cpu",
        "qemu64,+smap",
        "-serial",
        "stdio",
        "-boot",
        "order=c",
    ];

    println!("Running QEMU with args: {:?}", qemu_args);

    let qemu_status = Command::new("qemu-system-x86_64")
        .args(&qemu_args)
        .status()?;
    assert!(qemu_status.success());

    Ok(())
}
