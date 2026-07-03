#![allow(static_mut_refs, unused_imports)]

use core::ffi::c_void;
use petroleum::common::{EfiSystemTable, write_vga_string};
use petroleum::page_table::MemoryMapDescriptor;
use petroleum::write_serial_bytes;
use x86_64::{PhysAddr, VirtAddr, structures::paging::PageTableFlags};

/// Helper to write debug messages to serial port.
#[inline(always)]
pub(super) fn debug_serial(msg: &[u8]) {
    petroleum::write_serial_bytes(0x3F8, 0x3FD, msg);
}

/// Helper struct for UEFI initialization context.
#[cfg(target_os = "uefi")]
#[repr(C)]
pub struct UefiInitContext {
    pub args_ptr: *const petroleum::assembly::KernelArgs,
    pub system_table: &'static EfiSystemTable,
    pub memory_map: *mut c_void,
    pub memory_map_size: usize,
    pub descriptor_size: usize,
    pub physical_memory_offset: VirtAddr,
    pub virtual_heap_start: VirtAddr,
    pub heap_start_after_gdt: VirtAddr,
    pub heap_start_after_stack: VirtAddr,
}

/// Type alias for the generic callback used by petroleum::page_table::init.
#[cfg(target_os = "uefi")]
type PageTableInitCb = fn(
    &mut x86_64::structures::paging::OffsetPageTable,
    &mut petroleum::page_table::allocator::bitmap::BitmapFrameAllocator,
);

/// Helper to create a temporary mapper via petroleum::page_table::init.
#[cfg(target_os = "uefi")]
pub(super) unsafe fn create_tmp_mapper(
    phys_offset: x86_64::VirtAddr,
    frame_allocator: &mut petroleum::page_table::allocator::bitmap::BitmapFrameAllocator,
    kernel_phys: u64,
) -> x86_64::structures::paging::OffsetPageTable<'static> {
    unsafe {
        petroleum::page_table::init::<_, PageTableInitCb>(
            phys_offset,
            frame_allocator,
            kernel_phys,
            None,
        )
    }
}

// ── BootFrameAllocator (circular-dependency resolver) ────────

#[cfg(target_os = "uefi")]
pub(super) struct BootFrameAllocator {
    next_frame: u64,
}

#[cfg(target_os = "uefi")]
impl BootFrameAllocator {
    pub fn new(start_frame: u64) -> Self {
        Self {
            next_frame: start_frame,
        }
    }
}

#[cfg(target_os = "uefi")]
unsafe impl x86_64::structures::paging::FrameAllocator<x86_64::structures::paging::Size4KiB>
    for BootFrameAllocator
{
    fn allocate_frame(
        &mut self,
    ) -> Option<x86_64::structures::paging::PhysFrame<x86_64::structures::paging::Size4KiB>> {
        let frame = x86_64::structures::paging::PhysFrame::containing_address(
            x86_64::PhysAddr::new(self.next_frame * 4096),
        );
        self.next_frame += 1;
        Some(frame)
    }
}

// ── UefiInitContext methods ──────────────────────────────────

#[cfg(target_os = "uefi")]
impl UefiInitContext {
    /// Early initialization: serial, VGA, memory maps.
    pub fn early_initialization(&mut self) -> PhysAddr {
        petroleum::serial::serial_init();
        debug_serial(b"Kernel: efi_main entered\n");

        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 0, b"Kernel boot (UEFI)", 0x1F00);
            write_vga_string(vga_buffer, 1, b"Early init start", 0x1F00);
        }

        let kernel_virt_addr = crate::boot::uefi_entry::efi_main as u64;
        let kernel_phys_addr = kernel_virt_addr
            .wrapping_sub(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64);

        petroleum::uefi_helpers::setup_kernel_location(
            self.memory_map,
            self.memory_map_size,
            kernel_phys_addr,
        )
    }

    /// Memory management bootstrap — delegated to `paging::bootstrap_memory`.
    #[cfg(target_os = "uefi")]
    pub fn memory_management_initialization(
        &mut self,
        kernel_phys_start: PhysAddr,
    ) -> (VirtAddr, PhysAddr, VirtAddr) {
        super::paging::bootstrap_memory(self, kernel_phys_start)
    }

    /// Prepare kernel stack region.
    #[cfg(target_os = "uefi")]
    pub fn prepare_kernel_stack(
        &mut self,
        virtual_heap_start: VirtAddr,
        physical_memory_offset: VirtAddr,
    ) -> VirtAddr {
        self.heap_start_after_gdt = virtual_heap_start;
        assert!(
            virtual_heap_start.as_u64() % 16 == 0,
            "Kernel stack must be 16-byte aligned"
        );

        let stack_phys_start = self.heap_start_after_gdt.as_u64() - physical_memory_offset.as_u64();
        let stack_pages = (2 * 1024 * 1024) / 4096;

        let mut fa = crate::heap::FRAME_ALLOCATOR.lock();
        let allocator = fa.as_mut().expect("Frame allocator not initialized");
        let mut mapper = unsafe { create_tmp_mapper(physical_memory_offset, allocator, 0x100000) };
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        unsafe {
            petroleum::page_table::raw::map_range_with_huge_pages(
                &mut mapper,
                allocator,
                stack_phys_start,
                self.heap_start_after_gdt.as_u64(),
                stack_pages as u64,
                flags,
                "kernel_stack",
            )
            .expect("Failed to map kernel stack");
        }

        let kernel_stack_top =
            (self.heap_start_after_gdt.as_u64() + crate::heap::KERNEL_STACK_SIZE as u64) & !15;
        self.heap_start_after_stack =
            self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64;
        VirtAddr::new(kernel_stack_top)
    }

    /// No-op: allocator already initialized via TOTAL_HEAP_BUFFER.
    #[cfg(target_os = "uefi")]
    pub fn setup_allocator(&mut self, _virtual_heap_start: VirtAddr) {}

    /// Validate framebuffer config and pre-initialize the APIC controller.
    #[cfg(target_os = "uefi")]
    pub fn map_mmio() -> usize {
        let phys_offset = petroleum::common::memory::get_physical_memory_offset() as u64;
        let lapic_virt = 0xfee00000u64 + phys_offset;
        crate::interrupts::apic::preinit_apic_controller(lapic_virt);

        if let Some(config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|m| m.lock().clone())
        {
            petroleum::debug_log!(
                "FB config: phys={:#x} {}x{}x{}\n",
                config.address,
                config.width,
                config.height,
                config.bpp
            );
        }
        0
    }

    /// Parse the UEFI memory map into the kernel's static buffer.
    pub(super) fn init_memory_map(&self) {
        debug_serial(b"Parsing UEFI memory map...\n");

        // Unlock potentially poisoned mutex
        unsafe {
            let mutex_ptr = core::ptr::addr_of!(crate::heap::MEMORY_MAP) as *mut u32;
            core::ptr::write_volatile(mutex_ptr, 0);
        }

        let map_addr = self.memory_map as u64;
        let base_ptr = if map_addr >= 0xFFFF_8000_0000_0000 {
            map_addr as *const u8
        } else {
            (map_addr + self.physical_memory_offset.as_u64()) as *const u8
        };
        let desc_sz = self.descriptor_size;
        let raw_size = self.memory_map_size;
        let actual_bytes = (raw_size / desc_sz) * desc_sz;
        let max = actual_bytes / desc_sz;

        unsafe {
            let mut count = 0;
            let limit = crate::heap::MAX_DESCRIPTORS.min(max);
            for i in 0..limit {
                let offset = i * desc_sz;
                if offset >= actual_bytes {
                    break;
                }
                let desc = MemoryMapDescriptor::new(base_ptr.add(offset), desc_sz);
                if !petroleum::page_table::MemoryDescriptorValidator::is_valid(&desc) {
                    continue;
                }
                crate::heap::MEMORY_MAP_BUFFER[count] = desc;
                count += 1;
            }
            // debug_serial format output omitted to avoid alloc in early boot
            debug_serial(b"Memory map parsed\n");
            if let Some(mut lock) = crate::heap::MEMORY_MAP.try_lock() {
                *lock = Some(&crate::heap::MEMORY_MAP_BUFFER[0..count]);
            }
        }
    }
}
