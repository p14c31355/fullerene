use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::boxed::Box;

use fullerene_abi::boot::{self, BootInfo};

use super::fdt;

const HEAP_SIZE: usize = 128 * 1024;
pub const PAGE_SIZE: u64 = 4096;
const MAX_FRAME_RANGES: usize = 8;
const MAX_RESERVED_RANGES: usize = 4;

#[repr(align(16))]
struct HeapStorage([u8; HEAP_SIZE]);

static mut HEAP_STORAGE: HeapStorage = HeapStorage([0; HEAP_SIZE]);

struct BumpAllocator {
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    next: AtomicUsize::new(0),
};

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = unsafe { core::ptr::addr_of_mut!(HEAP_STORAGE.0) as usize };
        let end = base.saturating_add(HEAP_SIZE);
        let align_mask = layout.align().saturating_sub(1);

        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = match base
                .saturating_add(current)
                .checked_add(align_mask)
                .map(|address| address & !align_mask)
            {
                Some(address) if address < end => address,
                _ => return core::ptr::null_mut(),
            };
            let offset = aligned - base;
            let new_next = match offset.checked_add(layout.size()) {
                Some(value) if aligned.saturating_add(layout.size()) <= end => value,
                _ => return core::ptr::null_mut(),
            };
            if self
                .next
                .compare_exchange(current, new_next, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    super::uart::puts("aarch64 allocator exhausted\n");
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

pub fn smoke() {
    let value = Box::new(0x_f00d_u64);
    assert_eq!(*value, 0x_f00d);
}

#[derive(Clone, Copy)]
struct AddressRange {
    start: u64,
    end: u64,
}

impl AddressRange {
    const EMPTY: Self = Self { start: 0, end: 0 };

    fn new(start: u64, size: u64) -> Option<Self> {
        let end = start.checked_add(size)?;
        let start = align_up(start)?;
        let end = end & !(PAGE_SIZE - 1);
        (start < end).then_some(Self { start, end })
    }

    fn contains(&self, address: u64) -> bool {
        address >= self.start && address < self.end
    }
}

/// A bounded early physical-frame allocator built from the arm64 DTB memory
/// map. It deliberately returns physical addresses only; the caller must use
/// the active MMU mapping before dereferencing a frame. The implementation is
/// single-core during early boot and is intended to become the backing source
/// for the architecture-neutral page-table allocator.
pub struct PhysicalFrameAllocator {
    regions: [AddressRange; MAX_FRAME_RANGES],
    region_count: usize,
    region_index: usize,
    cursor: u64,
    reserved: [AddressRange; MAX_RESERVED_RANGES],
    reserved_count: usize,
}

impl PhysicalFrameAllocator {
    pub fn from_boot_info(info: &BootInfo) -> Option<Self> {
        if info.flags & boot::flags::MEMORY_MAP == 0
            || info.memory_map_address == 0
            || info.memory_map_descriptor_size as usize != core::mem::size_of::<fdt::Region>()
        {
            return None;
        }
        let count = (info.memory_map_size as usize)
            .checked_div(core::mem::size_of::<fdt::Region>())?
            .min(MAX_FRAME_RANGES);
        let regions = unsafe {
            core::slice::from_raw_parts(info.memory_map_address as *const fdt::Region, count)
        };
        let mut allocator = Self {
            regions: [AddressRange::EMPTY; MAX_FRAME_RANGES],
            region_count: 0,
            region_index: 0,
            cursor: 0,
            reserved: [AddressRange::EMPTY; MAX_RESERVED_RANGES],
            reserved_count: 0,
        };

        // The linker image, DTB, and fixed USB DMA/trace sections must not be
        // handed to a future page-table or userspace allocator.
        unsafe extern "C" {
            static __image_start: u8;
            static __image_end: u8;
            static __usb_dma_start: u8;
            static __usb_dma_end: u8;
            static __usb_trace_start: u8;
            static __usb_trace_end: u8;
        }
        allocator.push_reserved(symbol_range(
            core::ptr::addr_of!(__image_start) as u64,
            core::ptr::addr_of!(__image_end) as u64,
        ));
        allocator.push_reserved(symbol_range(
            core::ptr::addr_of!(__usb_dma_start) as u64,
            core::ptr::addr_of!(__usb_dma_end) as u64,
        ));
        allocator.push_reserved(symbol_range(
            core::ptr::addr_of!(__usb_trace_start) as u64,
            core::ptr::addr_of!(__usb_trace_end) as u64,
        ));
        if let Some(header) = fdt::inspect(info.fdt_address) {
        allocator.push_reserved(AddressRange::new(
                info.fdt_address,
                header.total_size as u64,
            ).unwrap_or(AddressRange::EMPTY));
        }

        for region in regions {
            if let Some(range) = AddressRange::new(region.base, region.size) {
                if allocator.region_count < MAX_FRAME_RANGES {
                    allocator.regions[allocator.region_count] = range;
                    allocator.region_count += 1;
                }
            }
        }
        allocator.cursor = allocator
            .regions
            .first()
            .map(|range| range.start)
            .unwrap_or(0);
        Some(allocator)
    }

    fn push_reserved(&mut self, range: AddressRange) {
        if range.start < range.end && self.reserved_count < MAX_RESERVED_RANGES {
            self.reserved[self.reserved_count] = range;
            self.reserved_count += 1;
        }
    }

    pub fn next_frame(&mut self) -> Option<u64> {
        while self.region_index < self.region_count {
            let range = self.regions[self.region_index];
            let candidate = align_up(self.cursor)?;
            if candidate >= range.end {
                self.region_index += 1;
                self.cursor = self
                    .regions
                    .get(self.region_index)
                    .map(|next| next.start)
                    .unwrap_or(0);
                continue;
            }
            self.cursor = candidate.saturating_add(PAGE_SIZE);
            if self
                .reserved
                .iter()
                .take(self.reserved_count)
                .any(|reserved| reserved.contains(candidate))
            {
                continue;
            }
            return Some(candidate);
        }
        None
    }

    pub fn first_available_frame(&mut self) -> Option<u64> {
        let saved_index = self.region_index;
        let saved_cursor = self.cursor;
        let frame = self.next_frame();
        self.region_index = saved_index;
        self.cursor = saved_cursor;
        frame
    }
}

fn symbol_range(start: u64, end: u64) -> AddressRange {
    AddressRange {
        start: start & !(PAGE_SIZE - 1),
        end: end.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1),
    }
}

fn align_up(value: u64) -> Option<u64> {
    value.checked_add(PAGE_SIZE - 1).map(|value| value & !(PAGE_SIZE - 1))
}
