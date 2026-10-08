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
// 8 MiB since the storage-stack bring-up mounts real gigabytes-scale
// filesystems (f2fs/ext4) during boot: a 512 KiB heap OOM-reset the box
// when the v100 probe mounted the 20 GB userdata partition.
const HEAP_SIZE: usize = 32 * 1024 * 1024;
pub const PAGE_SIZE: u64 = 4096;
const MAX_FRAME_RANGES: usize = 8;
const FIXED_RESERVED_RANGES: usize = 4;
const MAX_RESERVED_RANGES: usize = FIXED_RESERVED_RANGES + fdt::MAX_RESERVED_MEMORY_REGIONS + 1;
pub(crate) const MAX_RELEASED_FRAMES: usize = 1024;
const MAX_SHARED_FRAMES: usize = 1024;

#[repr(align(16))]
struct HeapStorage([u8; HEAP_SIZE]);

static mut HEAP_STORAGE: HeapStorage = HeapStorage([0; HEAP_SIZE]);

struct BumpAllocator {
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

/// Heap usage for the durable log (the v148 stall sat inside resolve_path's
/// Vec/String allocations; this tells exhaustion from corruption at a
/// glance). The bump cursor is monotonic, so this is bytes-ever-allocated.
pub fn heap_used() -> usize {
    ALLOCATOR.next.load(Ordering::Relaxed)
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    next: AtomicUsize::new(0),
};

/// Print a step, then push the RAM tee into the pmsg zone immediately so the
/// screen reader can see exactly how far the allocator entry got.
fn trace(msg: &str) {
    super::uart::puts(msg);
    crate::blackbox::tee_flush();
}

/// The one-photo status report for the ANE port: the surviving marks and the
/// CPU identity, in two short lines. Each line is flushed to the durable log
/// as soon as it is printed. `stepmarks14` measured why no peripheral reads
/// live here: the USB windows (0xff100000/0xff200000) are clock-gated by the
/// loader at handoff, and a read of one stalls the bus - the boot died
/// between the h mark and this report (count 0x0B, right at the first
/// `read32`). Register reads over a bus this kernel does not own yet belong
/// to the USB bring-up phase, not to early boot. The marks block is read
/// through the same `entry::StepMarks` definition its writer uses.
#[cfg(fullerene_aarch64_ane)]
fn report() {
    use super::entry::{STEP_MARKS_CAPACITY, StepMarks};
    use super::uart::{put_hex32, putc, puts};

    let marks = StepMarks::ADDRESS as *const StepMarks;
    puts("R:M=");
    let magic = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*marks).magic)) };
    let count = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*marks).count)) };
    let count = if magic == StepMarks::MAGIC {
        count.min(STEP_MARKS_CAPACITY as u32)
    } else {
        putc(b'?');
        0
    };
    for index in 0..count as usize {
        let byte = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*marks).bytes[index])) };
        putc(byte);
    }
    puts("\n");
    crate::blackbox::tee_flush();

    // CPU identity only - both are `mrs` reads, no bus traffic.
    let sctlr: u64;
    unsafe { core::arch::asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    let cntfrq: u64;
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack)) };
    put_hex32("R:V=", sctlr as u32);
    put_hex32(" F=", cntfrq as u32);
    puts("\n");
    crate::blackbox::tee_flush();
}
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
        unsafe { super::entry::ane_step_byte(b'h') };
        // MMU and caches are up by now: flush the RAM tee into the pmsg zone
        // instead of touching the pstore machinery on every console byte far
        // earlier in the boot.
        trace("alloc: enter\n");
        #[cfg(fullerene_aarch64_ane)]
        report();
        crate::blackbox::tee_flush();
        unsafe { super::entry::ane_step_byte(b'f') };
        let count = (info.memory_map_size as usize)
            .checked_div(core::mem::size_of::<fdt::Region>())?
            .min(MAX_FRAME_RANGES);
        trace("alloc: count ok\n");
        // Mark g was retired (2026-10-04 evening) when the mmu q/r/s split
        // took the record's 16-slot budget; the trace line stays for the
        // log context a photo shows.
        let regions = unsafe {
            core::slice::from_raw_parts(info.memory_map_address as *const fdt::Region, count)
        };
        trace("alloc: slice ok\n");
        // Mark i was retired with g (see the note at count ok above).
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
        // Mark j was retired with g (see the note at count ok above).

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
        unsafe { super::entry::ane_step_byte(b'k') };
        // The ANE diagnostic tail: the FTLX log block (blackbox) and the
        // mark/scratch page at 0x348DF000 live in an unused RAM tail that is
        // in no reserved-memory node. stepmarks20's six-minute run proved
        // that the frame allocator otherwise owns the page: it was handed out
        // to the long-running timer/task work and the log read-out fell back
        // to the stale pmsg banner. The zones are append-only diagnostics,
        // so reserve the whole 64 KiB and keep it out of the pool for good.
        #[cfg(fullerene_aarch64_ane)]
        allocator.push_reserved(
            AddressRange::new(crate::blackbox::TEXT_BLOCK_BASE, 0x1_0000)
                .unwrap_or(AddressRange::EMPTY),
        );
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
        // The allocator-side l/m/n duplicates were retired here:
        // fdt.rs's find_reserved_memory_regions now owns those three
        // letters (walk entry, node scratch, walk return), so one letter
        // narrates exactly one position again.
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

    /// Allocate `pages` consecutive 4 KiB frames and return the physical base.
    ///
    /// Carves a run straight from the region cursor (the single-frame
    /// `next_frame` walk cannot express a run), stepping over anything the
    /// reserved list covers. The bounded released list is deliberately not
    /// consulted: a contiguous run must not be stitched together from holes.
    /// Backs the petroleum frame-allocator adapter (`AneFrameAllocator`) that
    /// `PageBuf` uses for the desktop's 10 MiB back buffer.
    pub fn allocate_contiguous_frames(&mut self, pages: usize) -> Option<u64> {
        if pages == 0 {
            return None;
        }
        let bytes = (pages as u64).checked_mul(PAGE_SIZE)?;
        while self.region_index < self.region_count {
            let range = self.regions[self.region_index];
            let start = align_up(self.cursor.max(range.start))?;
            let end = start.checked_add(bytes)?;
            if end > range.end {
                self.region_index += 1;
                self.cursor = self
                    .regions
                    .get(self.region_index)
                    .map(|next| next.start)
                    .unwrap_or(0);
                continue;
            }
            let mut blocked_until = start;
            for reserved in self.reserved.iter().take(self.reserved_count) {
                if reserved.start < end && start < reserved.end {
                    blocked_until = blocked_until.max(reserved.end);
                }
            }
            if blocked_until > start {
                self.cursor = blocked_until;
                continue;
            }
            self.cursor = end;
            return Some(start);
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
