use core::sync::atomic::{AtomicBool, Ordering};
use crate::common::memory::set_heap_range;
use x86_64::PhysAddr;
use crate::page_table::efi_memory::MemoryMapDescriptor;

pub const MAX_DESCRIPTORS: usize = 2048;

/// Buffer for memory map descriptors to avoid heap allocation during init
pub static mut MEMORY_MAP_BUFFER: [MemoryMapDescriptor; MAX_DESCRIPTORS] = [const {
    MemoryMapDescriptor {
        ptr: core::ptr::null(),
        descriptor_size: 0,
    }
}; MAX_DESCRIPTORS];

pub static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);

#[cfg(all(not(feature = "std"), not(test)))]
#[global_allocator]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

#[cfg(all(not(feature = "std"), test))]
pub static ALLOCATOR: linked_list_allocator::LockedHeap =
    linked_list_allocator::LockedHeap::empty();

pub fn init_global_heap(ptr: *mut u8, size: usize) {
    #[cfg(all(not(feature = "std"), not(test)))]
    if !HEAP_INITIALIZED.load(Ordering::SeqCst) {
        let mut buf = [0u8; 16];
        let len = crate::serial::format_hex_to_buffer(ptr as u64, &mut buf, 16);
        unsafe {
            crate::write_serial_bytes(0x3F8, 0x3FD, b"DEBUG: [init_global_heap] ptr: 0x");
            crate::write_serial_bytes(0x3F8, 0x3FD, &buf[..len]);
        }
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        unsafe {
            ALLOCATOR.lock().init(ptr, size);
        }
        set_heap_range(ptr as usize, size);
        HEAP_INITIALIZED.store(true, Ordering::SeqCst);
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [init_global_heap] completed\n");
    }
}

/// Allocate heap memory from EFI memory map
pub fn allocate_heap_from_map(start_addr: PhysAddr, heap_size: usize) -> PhysAddr {
    const FRAME_SIZE: u64 = 4096;
    let _heap_frames = (heap_size + FRAME_SIZE as usize - 1) / FRAME_SIZE as usize;

    let aligned_start = if start_addr.as_u64() % FRAME_SIZE == 0 {
        start_addr
    } else {
        PhysAddr::new((start_addr.as_u64() / FRAME_SIZE + 1) * FRAME_SIZE)
    };

    aligned_start
}