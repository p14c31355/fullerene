// bellows/src/lib.rs
#![no_std]
#![no_main]

use core::fmt::Write;
use uefi::prelude::*;
use uefi::proto::media::file::{File, FileMode, FileAttribute, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::boot::{AllocateType, MemoryType};

#[entry]
fn efi_main() -> Status {
    // init helper for printing (from uefi-services)
    uefi::helpers::init().unwrap();

    let st = uefi::table::set_system_table::get();
    let boot_services = st.boot_services();

    let stdout = boot_services.stdout();
    stdout.reset(false).unwrap();
    writeln!(stdout, "bellows: UEFI bootloader started").ok();

    // Locate SimpleFileSystem protocol on loaded image's device handle
    let sfs = boot_services.open_protocol::<SimpleFileSystem>(boot_services.image_handle())
        .or_else(|_| {
            // fallback: try to locate any filesystem by handle search
            // (simple approach) 
            Err(())
        });

    let sfs = match sfs {
        Ok(proto) => proto,
        Err(_) => {
            writeln!(stdout, "bellows: failed to open SimpleFileSystem on handle").ok();
            return Status::LOAD_ERROR;
        }
    };

    // open root volume
    let mut volume = match unsafe { (&*sfs.get()).open_volume() } {
        Ok(v) => v,
        Err(e) => {
            writeln!(stdout, "bellows: open_volume failed: {:?}", e.status()).ok();
            return Status::LOAD_ERROR;
        }
    };

    // Try to open "kernel.efi" in the root of the ESP
    let file_name = cstr16!("kernel.efi");
    match volume.open(file_name, FileMode::Read, FileAttribute::READ_ONLY) {
        Ok(file_handle) => {
            writeln!(stdout, "bellows: found kernel.efi").ok();

            // read file into memory (simple approach: get file size then allocate pages)
            if let Ok(mut regular) = file_handle.into_type().and_then(|t| match t {
                uefi::proto::media::file::FileType::Regular(r) => Ok(r),
                _ => Err(()),
            }) {
                // get file size by seeking to end
                let size = match regular.get_boxed().get_length() {
                    Ok(s) => s as usize,
                    Err(_) => {
                        writeln!(stdout, "bellows: cannot get size").ok();
                        return Status::LOAD_ERROR;
                    }
                };

                // allocate pages for image
                let pages = (size + 0xFFF) / 0x1000;
                let buf_ptr = boot_services
                    .allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
                    .expect("allocate_pages failed")
                    .unwrap();
                let slice = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, pages * 0x1000) };

                // read the file fully
                match regular.read(slice) {
                    Ok(_read) => {
                        writeln!(stdout, "bellows: read {} bytes", size).ok();

                        // Now ask UEFI to load the image from the buffer
                        // We need to use LoadImage with a Handle to the parent image.
                        let image = boot_services
                            .load_image(false, boot_services.image_handle(), None, slice)
                            .expect("LoadImage failed")
                            .unwrap();

                        // Start the loaded image
                        let status = boot_services.start_image(image).unwrap();
                        writeln!(stdout, "bellows: started kernel image: {:?}", status).ok();

                        return Status::SUCCESS;
                    }
                    Err(e) => {
                        writeln!(stdout, "bellows: read error: {:?}", e.status()).ok();
                        return Status::LOAD_ERROR;
                    }
                }
            } else {
                writeln!(stdout, "bellows: kernel not a regular file").ok();
                return Status::LOAD_ERROR;
            }
        }
        Err(_) => {
            writeln!(stdout, "bellows: kernel.efi not found on ESP").ok();
            // keep interactive so user sees message, then exit to UEFI shell or halt
            return Status::NOT_FOUND;
        }
    }
}
