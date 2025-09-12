// fullerene/flasks/src/main.rs
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    process::Command,
};
use fatfs::{FileSystem, FormatVolumeOptions, FsOptions};
use gpt::{
    disk::{LogicalBlockSize},
    partition_types,
    GptConfig,
};
use uuid::Uuid;

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
        let remaining = self.size - self.current_pos;
        let bytes_to_read = std::cmp::min(buf.len() as u64, remaining) as usize;
        if bytes_to_read == 0 {
            return Ok(0);
        }

        self.file.seek(SeekFrom::Start(self.offset + self.current_pos))?;
        let bytes_read = self.file.read(&mut buf[..bytes_to_read])?;
        self.current_pos += bytes_read as u64;
        Ok(bytes_read)
    }
}

impl<'a> Write for PartitionIo<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.size - self.current_pos;
        let bytes_to_write = std::cmp::min(buf.len() as u64, remaining) as usize;
        if bytes_to_write == 0 {
            return Ok(0);
        }

        self.file.seek(SeekFrom::Start(self.offset + self.current_pos))?;
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
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek beyond end of partition"));
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
            let name = component.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Non-UTF8 path component"))?;
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
    let data = fs::read(src)?;
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
        return Err(io::Error::new(io::ErrorKind::Other, "fullerene-kernel build failed"));
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

    let disk_size_bytes = 64 * 1024 * 1024; // 64 MB
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
    let mut gpt_disk = gpt_config.create_from_device(disk_file, None)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to create GPT disk: {}", e)))?;
        
    let first_lba = gpt_disk.primary_header().unwrap().first_usable;
    let last_lba = gpt_disk.primary_header().unwrap().last_usable;
    let esp_size_lba = if last_lba >= first_lba {
        last_lba - first_lba + 1
    } else {
        0
    };

    dbg!(esp_size_lba);

    if esp_size_lba == 0 {
        return Err(io::Error::new(io::ErrorKind::Other, "Calculated ESP partition size is 0, cannot create partition."));
    }

    // Add EFI System Partition (ESP)
    let _esp_guid = Uuid::new_v4();

    gpt_disk.add_partition(
        "EFI System Partition",
        first_lba,
        partition_types::EFI,
        esp_size_lba,
        None,
    ).map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to add ESP: {}", e)))?;

    // Get partition info before writing, as write consumes gpt_disk
    let esp_partition_info = gpt_disk.partitions().iter().find(|(_, p)| p.part_type_guid == partition_types::EFI)
        .map(|(_, p)| p.clone())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ESP not found after creation"))?;

    // Write GPT changes to disk and retrieve the underlying File
    let mut disk_file_after_gpt = gpt_disk.write()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("Failed to write GPT to disk: {}", e)))?;

    // Format the EFI System Partition (ESP)
    let esp_offset_bytes = esp_partition_info.first_lba * LogicalBlockSize::Lb512 as u64;
    let esp_size_bytes = (esp_partition_info.last_lba - esp_partition_info.first_lba + 1) * LogicalBlockSize::Lb512 as u64;

    // Use the retrieved disk_file_after_gpt for PartitionIo
    let mut esp_io = PartitionIo::new(&mut disk_file_after_gpt, esp_offset_bytes, esp_size_bytes)?;

    fatfs::format_volume(&mut esp_io, FormatVolumeOptions::new().bytes_per_cluster(4096))?;

    // 4. Open FAT filesystem (the EFI System Partition within esp.img)
    // Re-create PartitionIo for FileSystem::new as it consumes the reader/writer
    let esp_io_for_fs = PartitionIo::new(&mut disk_file_after_gpt, esp_offset_bytes, esp_size_bytes)?;
    let fs = FileSystem::new(esp_io_for_fs, FsOptions::new())?;

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