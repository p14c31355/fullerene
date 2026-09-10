use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::boxed::Box;

use fullerene_abi::boot::{self, BootInfo};

use super::fdt;

// The native VFS keeps the file-backed child image resident while SPAWN
// stages one checked user copy. Keep both bounded buffers available during
// the first VFS-to-process handoff.
// Android-init now keeps the Rust PID-1 image, property-area metadata, and
// the bounded virtual filesystems resident at the same time. 256 KiB was
// enough for the original launchd probe but made a valid initramfs fail as
// soon as the property-service protocol handler grew past that boundary.
const HEAP_SIZE: usize = 512 * 1024;
pub const PAGE_SIZE: u64 = 4096;
const MAX_FRAME_RANGES: usize = 8;
const MAX_RESERVED_RANGES: usize = 32;
const MAX_RELEASED_FRAMES: usize = 1024;
const MAX_SHARED_FRAMES: usize = 1024;

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

#[derive(Clone, Copy)]
struct SharedFrame {
    physical: u64,
    references: u16,
}

impl SharedFrame {
    const EMPTY: Self = Self {
        physical: 0,
        references: 0,
    };
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
    released: [u64; MAX_RELEASED_FRAMES],
    released_count: usize,
    shared: [SharedFrame; MAX_SHARED_FRAMES],
}

static mut GLOBAL_FRAME_ALLOCATOR: Option<PhysicalFrameAllocator> = None;

/// Move the DTB-backed allocator behind the AArch64 runtime boundary so
/// process-creation syscalls can allocate frames after boot handoff.
pub(crate) fn install_global(allocator: PhysicalFrameAllocator) {
    unsafe {
        *core::ptr::addr_of_mut!(GLOBAL_FRAME_ALLOCATOR) = Some(allocator);
    }
}

pub(crate) fn with_global<F, R>(function: F) -> Option<R>
where
    F: FnOnce(&mut PhysicalFrameAllocator) -> R,
{
    unsafe {
        (*core::ptr::addr_of_mut!(GLOBAL_FRAME_ALLOCATOR))
            .as_mut()
            .map(function)
    }
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
            released: [0; MAX_RELEASED_FRAMES],
            released_count: 0,
            shared: [SharedFrame::EMPTY; MAX_SHARED_FRAMES],
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
            static __ufs_dma_start: u8;
            static __ufs_dma_end: u8;
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
        allocator.push_reserved(symbol_range(
            core::ptr::addr_of!(__ufs_dma_start) as u64,
            core::ptr::addr_of!(__ufs_dma_end) as u64,
        ));
        if info.fdt_address != 0 {
            let mut reserved_regions = [fdt::Region { base: 0, size: 0 }; MAX_RESERVED_RANGES];
            let reserved_count =
                fdt::find_reserved_memory_regions(info.fdt_address, &mut reserved_regions);
            for region in reserved_regions.iter().take(reserved_count) {
                allocator.push_reserved(
                    AddressRange::new(region.base, region.size).unwrap_or(AddressRange::EMPTY),
                );
            }
        }
        if let Some(header) = fdt::inspect(info.fdt_address) {
            allocator.push_reserved(
                AddressRange::new(info.fdt_address, header.total_size as u64)
                    .unwrap_or(AddressRange::EMPTY),
            );
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
        if self.released_count != 0 {
            self.released_count -= 1;
            return Some(self.released[self.released_count]);
        }
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

    /// Return a page to the bounded free list for reuse by later mappings.
    ///
    /// This is intentionally a small LIFO cache rather than a complete page
    /// allocator. It closes the first lifetime hole in the AArch64 bring-up:
    /// unmapping a bounded user mapping must make its frames available again.
    pub fn release_frame(&mut self, frame: u64) -> bool {
        self.release_frames(core::slice::from_ref(&frame))
    }

    /// Add one reference for a page shared by multiple address spaces.
    ///
    /// The first call changes the implicit single-owner page into an
    /// explicitly tracked two-owner page. Later calls extend that count for
    /// another fork. Private pages remain outside this table, so the common
    /// map/unmap path keeps the same bounded cost as before.
    pub(crate) fn retain_shared_frame(&mut self, frame: u64) -> bool {
        if frame & (PAGE_SIZE - 1) != 0
            || frame == 0
            || self
                .reserved
                .iter()
                .take(self.reserved_count)
                .any(|reserved| reserved.contains(frame))
            || self.released[..self.released_count].contains(&frame)
        {
            return false;
        }
        if let Some(shared) = self
            .shared
            .iter_mut()
            .find(|shared| shared.references != 0 && shared.physical == frame)
        {
            if shared.references == u16::MAX {
                return false;
            }
            shared.references += 1;
            return true;
        }
        let Some(shared) = self.shared.iter_mut().find(|shared| shared.references == 0) else {
            return false;
        };
        *shared = SharedFrame {
            physical: frame,
            references: 2,
        };
        true
    }

    /// Return the currently tracked number of owners of a shared page.
    pub(crate) fn shared_frame_references(&self, frame: u64) -> Option<u16> {
        self.shared
            .iter()
            .find(|shared| shared.references != 0 && shared.physical == frame)
            .map(|shared| shared.references)
    }

    /// Turn a COW page with one remaining owner back into an ordinary private
    /// page without putting it on the free list.
    pub(crate) fn make_frame_private(&mut self, frame: u64) -> bool {
        let Some(shared) = self
            .shared
            .iter_mut()
            .find(|shared| shared.references != 0 && shared.physical == frame)
        else {
            return false;
        };
        if shared.references != 1 {
            return false;
        }
        *shared = SharedFrame::EMPTY;
        true
    }

    /// Return a set of pages atomically from one retired address space.
    ///
    /// Checking the complete batch before mutating the free list prevents a
    /// partial address-space teardown when the bounded cache is full.
    pub(crate) fn release_frames(&mut self, frames: &[u64]) -> bool {
        let mut frames_to_release = 0usize;
        for (index, frame) in frames.iter().copied().enumerate() {
            if frame & (PAGE_SIZE - 1) != 0
                || frame == 0
                || self
                    .reserved
                    .iter()
                    .take(self.reserved_count)
                    .any(|reserved| reserved.contains(frame))
                || frames[..index].contains(&frame)
            {
                return false;
            }
            if let Some(shared) = self
                .shared
                .iter()
                .find(|shared| shared.references != 0 && shared.physical == frame)
            {
                if shared.references == 1 {
                    frames_to_release += 1;
                }
            } else {
                if self.released[..self.released_count].contains(&frame) {
                    return false;
                }
                frames_to_release += 1;
            }
        }
        if frames_to_release > MAX_RELEASED_FRAMES.saturating_sub(self.released_count) {
            return false;
        }
        let mut released_count = self.released_count;
        for frame in frames.iter().copied() {
            if let Some(shared) = self
                .shared
                .iter_mut()
                .find(|shared| shared.references != 0 && shared.physical == frame)
            {
                if shared.references > 1 {
                    shared.references -= 1;
                } else {
                    *shared = SharedFrame::EMPTY;
                    self.released[released_count] = frame;
                    released_count += 1;
                }
            } else {
                self.released[released_count] = frame;
                released_count += 1;
            }
        }
        self.released_count = released_count;
        true
    }

    pub fn first_available_frame(&mut self) -> Option<u64> {
        let saved_index = self.region_index;
        let saved_cursor = self.cursor;
        let saved_released_count = self.released_count;
        let frame = self.next_frame();
        self.region_index = saved_index;
        self.cursor = saved_cursor;
        self.released_count = saved_released_count;
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
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
}
