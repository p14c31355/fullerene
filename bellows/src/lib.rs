// bellows/src/lib.rs
#![no_std]
#![no_main]

use core::fmt::Write;
use uefi::prelude::*;
use uefi::table::{Boot, SystemTable};
use uefi::proto::media::file::{File, FileMode, FileAttribute};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::boot::{AllocateType, MemoryType};
use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

#[uefi::entry]
fn main() -> Status {
    // UEFI helper init
    uefi::helpers::init().unwrap();
    let mut st = uefi::system_table();
const HEAP_SIZE: usize = 128 * 1024; // 128 KiB

let heap_ptr = match st.boot_services().allocate_pool(MemoryType::LOADER_DATA, HEAP_SIZE) {
    Ok(ptr) => ptr.unwrap(),
    Err(e) => {
        writeln!(st.stdout(), "bellows: failed to allocate heap: {:?}", e.status()).ok();
        return Status::OUT_OF_RESOURCES;
    }
};

unsafe {
    ALLOCATOR.lock().init(heap_ptr, HEAP_SIZE);
}
    let stdout = st.stdout();
    stdout.reset(false).ok();
    writeln!(stdout, "bellows: UEFI bootloader started").ok();

    let boot_services = st.boot_services();

    // Locate SimpleFileSystem protocol on loaded image's device handle
    let sfs = match boot_services.open_protocol::<SimpleFileSystem>(boot_services.image_handle()) {
        Ok(proto) => proto,
        Err(_) => {
            writeln!(stdout, "bellows: failed to open SimpleFileSystem").ok();
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
    let file_handle = match volume.open(file_name, FileMode::Read, FileAttribute::READ_ONLY) {
        Ok(f) => f,
        Err(_) => {
            writeln!(stdout, "bellows: kernel.efi not found").ok();
            return Status::NOT_FOUND;
        }
    };

    writeln!(stdout, "bellows: found kernel.efi").ok();

    // get file size by seeking to end
    let mut regular = match file_handle.into_regular_file() {
        Ok(r) => r,
        Err(_) => {
            writeln!(stdout, "bellows: kernel not a regular file").ok();
            return Status::LOAD_ERROR;
        }
    };

    let size = match regular.get_boxed().get_length() {
        Ok(s) => s as usize,
        Err(_) => {
            writeln!(stdout, "bellows: cannot get size").ok();
            return Status::LOAD_ERROR;
        }
    };

    // allocate pages for the image
    let pages = (size + 0xFFF) / 0x1000;
    let buf_ptr = match boot_services.allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages) {
        Ok(p) => p,
        Err(_) => {
            writeln!(stdout, "bellows: allocate_pages failed").ok();
            return Status::OUT_OF_RESOURCES;
        }
    };

    let slice = unsafe { core::slice::from_raw_parts_mut(buf_ptr as *mut u8, pages * 0x1000) };

    // Read file
    match regular.read(slice) {
        Ok(_) => writeln!(stdout, "bellows: read {} bytes", size).ok(),
        Err(e) => {
            writeln!(stdout, "bellows: read error: {:?}", e.status()).ok();
            return Status::LOAD_ERROR;
        }
    };

    // LoadImage + StartImage
    let image = match boot_services.load_image(false, boot_services.image_handle(), None, slice) {
        Ok(img) => img,
        Err(_) => {
            writeln!(stdout, "bellows: LoadImage failed").ok();
            return Status::LOAD_ERROR;
        }
    };

    let status = match boot_services.start_image(image) {
        Ok(s) => s,
        Err(_) => {
            writeln!(stdout, "bellows: StartImage failed").ok();
            return Status::LOAD_ERROR;
        }
    };

    writeln!(stdout, "bellows: started kernel image: {:?}", status).ok();

    Status::SUCCESS
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
