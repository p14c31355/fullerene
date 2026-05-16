#![allow(static_mut_refs)]
use crate::MEMORY_MAP;
use crate::heap;
use core::ffi::c_void;
use petroleum::common::{EfiSystemTable, write_vga_string};
use petroleum::page_table::memory_map::MemoryMapDescriptor;
use petroleum::uefi_helpers::find_heap_start;
use petroleum::{debug_log_no_alloc, write_serial_bytes};

use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Mapper, PageTableFlags, mapper::MapToError},
};

/// Helper struct for UEFI initialization context
#[cfg(target_os = "uefi")]
#[repr(C)]
pub struct UefiInitContext {
    /// Kernel arguments pointer
    pub args_ptr: *const petroleum::assembly::KernelArgs,
    /// Reference to EFI system table
    pub system_table: &'static EfiSystemTable,
    /// EFI memory map data
    pub memory_map: *mut c_void,
    /// Memory map size
    pub memory_map_size: usize,
    /// Descriptor size for memory map entries
    pub descriptor_size: usize,
    /// Physical memory offset after page table reconfiguration
    pub physical_memory_offset: VirtAddr,
    /// Virtual heap start address
    pub virtual_heap_start: VirtAddr,
    /// Heap start after GDT allocation
    pub heap_start_after_gdt: VirtAddr,
    /// Heap start after stack allocation
    pub heap_start_after_stack: VirtAddr,
}

#[cfg(target_os = "uefi")]
struct BootFrameAllocator {
    next_frame: u64,
}

#[cfg(target_os = "uefi")]
impl BootFrameAllocator {
    fn new(start_frame: u64) -> Self {
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

        // The frame is zeroed by the caller (petroleum::page_table::init writes to
        // the physical address directly, which works via UEFI's identity mapping).
        // We do NOT zero here via PHYSICAL_MEMORY_OFFSET_BASE because that mapping
        // may not exist in the UEFI page table during early init.

        self.next_frame += 1;
        Some(frame)
    }
}

#[cfg(target_os = "uefi")]
impl UefiInitContext {
    /// Early initialization: serial, VGA, memory maps
    pub fn early_initialization(&mut self) -> PhysAddr {
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: early_initialization start\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Entering serial_init\n");

        // Diagnostic: Direct port write to verify I/O permissions
        unsafe {
            x86_64::instructions::port::Port::<u8>::new(0x3F8).write(b'!');
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Direct write done\n");

        petroleum::serial::serial_init();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: serial_init done\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: efi_main entered\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Printing efi_main address\n");
        let mut buf = [0u8; 16];
        let len = petroleum::serial::format_hex_to_buffer(
            crate::boot::uefi_entry::efi_main as u64,
            &mut buf,
            16,
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Kernel: efi_main located at 0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &buf[..len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: efi_main address printed\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Attempting VGA buffer access 1\n");
        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 0, b"Kernel boot (UEFI)", 0x1F00);
            write_vga_string(vga_buffer, 1, b"Early init start", 0x1F00);
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: VGA buffer access 1 successful\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Attempting VGA buffer access 2\n");
        unsafe {
            let vga_buffer = &mut *(crate::VGA_BUFFER_ADDRESS as *mut [[u16; 80]; 25]);
            write_vga_string(vga_buffer, 2, b"Serial init done", 0x1F00);
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: VGA buffer access 2 successful\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"Early setup completed\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Calling setup_kernel_location\n");

        let mut map_buf = [0u8; 16];
        let map_len =
            petroleum::serial::format_hex_to_buffer(self.memory_map as u64, &mut map_buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: memory_map ptr: 0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &map_buf[..map_len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        let kernel_virt_addr = crate::boot::uefi_entry::efi_main as u64;
        let kernel_phys_addr = kernel_virt_addr
            .wrapping_sub(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64);

        let res = petroleum::uefi_helpers::setup_kernel_location(
            self.memory_map,
            self.memory_map_size,
            kernel_phys_addr,
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: setup_kernel_location returned\n");
        res
    }

    #[cfg(target_os = "uefi")]
    pub fn memory_management_initialization(
        &mut self,
        kernel_phys_start: PhysAddr,
    ) -> (VirtAddr, PhysAddr, VirtAddr) {
        // CRITICAL: Set global physical memory offset BEFORE any page table initialization
        petroleum::set_physical_memory_offset(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE);
        self.physical_memory_offset =
            x86_64::VirtAddr::new(petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64);

        // CRITICAL: Force reset ALLOCATOR lock and HEAP_INITIALIZED to avoid garbage memory issues
        unsafe {
            let alloc_ptr = core::ptr::addr_of!(petroleum::page_table::ALLOCATOR) as *mut u32;
            core::ptr::write_volatile(alloc_ptr, 0);

            let heap_init_ptr =
                core::ptr::addr_of!(petroleum::page_table::HEAP_INITIALIZED) as *mut u8;
            core::ptr::write_volatile(heap_init_ptr, 0);

            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: Forced ALLOCATOR lock and HEAP_INITIALIZED reset\n"
            );
        }

        // CRITICAL: Initialize ALLOCATOR as early as possible to avoid implicit allocation deadlocks
        if !HEAP_INITIALIZED.load(core::sync::atomic::Ordering::SeqCst) {
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PRE-INIT] Initializing ALLOCATOR early\n"
            );

            x86_64::instructions::interrupts::disable();
            let _allocator = petroleum::page_table::ALLOCATOR.lock();
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PRE-INIT] ALLOCATOR lock check passed\n"
            );
        }

        debug_log_no_alloc!("DEBUG: Starting memory_management_initialization");
        debug_log_no_alloc!(
            "DEBUG: Offset value: ",
            self.physical_memory_offset.as_u64()
        );

        // BREAK CIRCULAR DEPENDENCY:
        // We need the memory map to initialize the frame allocator, but we need a mapper to access the memory map.
        // We use a temporary BootFrameAllocator to create a temporary mapper.
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [CircularDep] Using BootFrameAllocator for temp mapper\n"
        );
        let mut boot_allocator = BootFrameAllocator::new(0x2000000 / 4096);
        let map_addr = self.memory_map as u64;
        let _map_size = self.memory_map_size as u64;
        let _offset_val = self.physical_memory_offset.as_u64();

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [CircularDep] Mapping memory_map via early_mappings callback\n"
        );

        let _temp_mapper = unsafe {
            petroleum::page_table::init::<
                BootFrameAllocator,
                fn(&mut x86_64::structures::paging::OffsetPageTable, &mut BootFrameAllocator),
            >(
                self.physical_memory_offset,
                &mut boot_allocator,
                kernel_phys_start.as_u64(),
                None,
            )
        };
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [CircularDep] Memory map mapped successfully via early_mappings\n"
        );

        debug_log_no_alloc!("DEBUG: Calling init_memory_map...");
        self.init_memory_map();
        debug_log_no_alloc!("DEBUG: init_memory_map returned");

        // CRITICAL: Initialize global heap allocator with static BOOT_HEAP_BUFFER BEFORE
        // calling init_frame_allocator which uses Vec (needs the global allocator).
        let boot_heap_ptr =
            unsafe { core::ptr::addr_of_mut!(crate::heap::BOOT_HEAP_BUFFER) as *mut u8 };
        unsafe { petroleum::init_global_heap(boot_heap_ptr, crate::heap::HEAP_SIZE) };
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: Global heap initialized (static buffer) before frame allocator\n"
        );

        let memory_map_ref = MEMORY_MAP
            .lock()
            .as_ref()
            .expect("Memory map not initialized")
            .clone();
        debug_log_no_alloc!(
            "DEBUG: Memory map reference acquired at 0x",
            memory_map_ref.as_ptr() as usize
        );

        heap::init_frame_allocator(memory_map_ref);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Heap frame allocator initialized\n");

        let map_addr = self.memory_map as u64;
        let offset_val = self.physical_memory_offset.as_u64();

        // Check if memory_map is already a virtual address in the higher half
        if map_addr >= 0xFFFF_8000_0000_0000 {
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: memory_map is already in higher half, skipping re-mapping\n"
            );
            let _map_virt = map_addr;
            let map_size = self.memory_map_size;
            let _map_pages = ((map_size as u64) + 4095) / 4096;

            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: Memory map buffer already mapped\n"
            );
        } else {
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: memory_map is physical, mapping to higher half\n"
            );
            let map_phys = map_addr;
            let _map_virt = map_phys + offset_val;
            let map_size = self.memory_map_size;
            let map_pages = ((map_size as u64) + 4095) / 4096;

            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: Calling petroleum::page_table::init (1)...\n"
            );
            let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let frame_allocator = frame_allocator_guard
                .as_mut()
                .expect("Frame allocator should be ready now");
            let mut mapper = unsafe {
                petroleum::page_table::init::<
                    _,
                    fn(
                        &mut x86_64::structures::paging::OffsetPageTable,
                        &mut petroleum::page_table::allocator::bitmap::BitmapFrameAllocator,
                    ),
                >(
                    self.physical_memory_offset,
                    frame_allocator,
                    kernel_phys_start.as_u64(),
                    None,
                )
            };
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: petroleum::page_table::init (1) done\n"
            );
            unsafe {
                petroleum::page_table::raw::utils::map_identity_range(
                    &mut mapper,
                    frame_allocator,
                    map_phys,
                    map_pages,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                )
                .expect("failed to identity-map memory map buffer");
            }
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: Memory map buffer mapped successfully\n"
            );
        }

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Allocating TSS stacks...\n");
        let tss_stack_pages =
            (crate::gdt::GDT_TSS_STACK_COUNT * crate::gdt::GDT_TSS_STACK_SIZE) / 4096;

        let tss_phys_addr = {
            let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let frame_allocator = frame_allocator_guard
                .as_mut()
                .expect("Frame allocator not initialized");
            debug_log_no_alloc!("DEBUG: Frame allocator lock acquired for TSS");

            debug_log_no_alloc!(
                "DEBUG: Attempting to allocate contiguous frames: ",
                tss_stack_pages
            );
            match frame_allocator.allocate_contiguous_frames(tss_stack_pages) {
                Ok(phys_addr) => {
                    debug_log_no_alloc!("DEBUG: TSS frames allocated at 0x", phys_addr);
                    PhysAddr::new(phys_addr as u64)
                }
                Err(_) => {
                    panic!(
                        "Critical failure: Failed to allocate contiguous physical frames for TSS stacks."
                    );
                }
            }
        };

        let base =
            petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64 + tss_phys_addr.as_u64();
        let sz = crate::gdt::GDT_TSS_STACK_SIZE as u64;
        let tss_stacks = crate::gdt::TssStacks {
            double_fault: VirtAddr::new(base + sz),
            timer: VirtAddr::new(base + sz * 2),
            stack_fault: VirtAddr::new(base + sz * 3),
            gp_fault: VirtAddr::new(base + sz * 4),
            page_fault: VirtAddr::new(base + sz * 5),
            nmi: VirtAddr::new(base + sz * 6),
            machine_check: VirtAddr::new(base + sz * 7),
        };
        crate::gdt::init_with_stacks(tss_stacks);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: GDT initialized with TSS stacks\n");

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [uefi_init] Start mapping 1GB kernel area\n"
        );

        let kernel_virt_start = petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64;
        let kernel_phys_start_val = kernel_phys_start.as_u64();

        let mut val_buf = [0u8; 16];
        let len = petroleum::serial::format_hex_to_buffer(kernel_phys_start_val, &mut val_buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: phys_start=0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &val_buf[..len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [uefi_init] Attempting to lock FRAME_ALLOCATOR for init\n"
        );

        // Create the ONLY mapper that will be used for all initial kernel mappings
        let mut main_mapper = unsafe {
            let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [uefi_init] Lock acquired, calling init\n"
            );
            let allocator = fa_guard.as_mut().expect("Frame allocator should be ready");
            petroleum::page_table::init::<
                _,
                fn(
                    &mut x86_64::structures::paging::OffsetPageTable,
                    &mut petroleum::page_table::allocator::bitmap::BitmapFrameAllocator,
                ),
            >(self.physical_memory_offset, allocator, 0x100000, None)
        };
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [uefi_init] petroleum::page_table::init for main_mapper returned\n"
        );
        {
            let mut fa_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let allocator = fa_guard.as_mut().expect("Frame allocator should be ready");

            let kernel_phys_start_aligned = kernel_phys_start_val & !0xFFF;
            let kernel_virt_start_aligned = kernel_virt_start & !0xFFF;

            unsafe {
                petroleum::page_table::raw::map_range_with_huge_pages(
                    &mut main_mapper,
                    allocator,
                    kernel_phys_start_aligned,
                    kernel_virt_start_aligned,
                    256 * 1024,
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
                    "kernel_area",
                )
                .expect("Failed to map kernel area");
            }
            x86_64::instructions::tlb::flush_all();
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Large kernel mapping completed\n");

        debug_log_no_alloc!("Entering memory_management_initialization");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Post-GDT init phase start\n");

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: Accessing FULLERENE_FRAMEBUFFER_CONFIG...\n"
        );
        let framebuffer_config = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|mutex| {
                petroleum::write_serial_bytes!(
                    0x3F8,
                    0x3FD,
                    b"DEBUG: Locking framebuffer config mutex...\n"
                );
                let lock = mutex.lock();
                petroleum::write_serial_bytes!(
                    0x3F8,
                    0x3FD,
                    b"DEBUG: Framebuffer config mutex locked\n"
                );
                *lock
            });
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: Framebuffer config access completed\n"
        );

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: About to lock FRAME_ALLOCATOR (line 222)\n"
        );
        let config = framebuffer_config.as_ref();
        let (_fb_addr, _fb_size) = if let Some(config) = config {
            let fb_size_bytes =
                (config.width as usize * config.height as usize * config.bpp as usize) / 8;
            (
                Some(VirtAddr::new(config.address)),
                Some(fb_size_bytes as u64),
            )
        } else {
            (None, None)
        };

        debug_log_no_alloc!("DEBUG: About to lock FRAME_ALLOCATOR for page table setup");
        {
            let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
            petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: FRAME_ALLOCATOR locked\n");
            let frame_allocator = frame_allocator_guard
                .as_mut()
                .expect("Frame allocator not initialized");

            let tss_flags = x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;

            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: Mapping TSS stacks using main_mapper\n"
            );
            unsafe {
                let mut mapper = petroleum::page_table::init::<
                    _,
                    fn(
                        &mut x86_64::structures::paging::OffsetPageTable,
                        &mut petroleum::page_table::allocator::bitmap::BitmapFrameAllocator,
                    ),
                >(
                    self.physical_memory_offset,
                    frame_allocator,
                    kernel_phys_start.as_u64(),
                    None,
                );
                let tss_virt = petroleum::common::uefi::PHYSICAL_MEMORY_OFFSET_BASE as u64
                    + tss_phys_addr.as_u64();
                let _ = petroleum::page_table::raw::map_range_with_huge_pages(
                    &mut mapper,
                    frame_allocator,
                    tss_phys_addr.as_u64(),
                    tss_virt,
                    tss_stack_pages as u64,
                    tss_flags,
                    "tss_stacks",
                );
            }
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: TSS stacks mapped to higher half\n"
            );
        }

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PHASE] Setting kernel CR3...\n");
        let kernel_cr3 = x86_64::registers::control::Cr3::read();
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PHASE] CR3 value to set: 0x");
        let mut cr3_buf = [0u8; 16];
        let cr3_len = petroleum::serial::format_hex_to_buffer(
            kernel_cr3.0.start_address().as_u64(),
            &mut cr3_buf,
            16,
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &cr3_buf[..cr3_len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        crate::interrupts::syscall::set_kernel_cr3(kernel_cr3.0.start_address().as_u64());
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [PHASE] Kernel CR3 set successfully\n"
        );

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PHASE] About to find heap start\n");
        let heap_phys_start = find_heap_start(memory_map_ref);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PHASE] find_heap_start returned\n");

        let _heap_phys_start_addr = if heap_phys_start.as_u64() < 0x1000
            || heap_phys_start.as_u64() >= 0x0000_8000_0000_0000
        {
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PHASE] Using fallback heap start\n"
            );
            PhysAddr::new(petroleum::FALLBACK_HEAP_START_ADDR)
        } else {
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PHASE] Using found heap start\n"
            );
            heap_phys_start
        };

        let heap_pages = (heap::HEAP_SIZE + 4095) / 4096;
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PHASE] Heap pages needed: ");
        let mut pg_buf = [0u8; 16];
        let pg_len = petroleum::serial::format_hex_to_buffer(heap_pages as u64, &mut pg_buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &pg_buf[..pg_len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [PHASE] Attempting to lock FRAME_ALLOCATOR for heap allocation...\n"
        );
        let heap_phys_addr_val = {
            let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PHASE] FRAME_ALLOCATOR lock acquired\n"
            );
            let frame_allocator = frame_allocator_guard
                .as_mut()
                .expect("Frame allocator not initialized");
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PHASE] Calling allocate_contiguous_frames...\n"
            );
            frame_allocator
                .allocate_contiguous_frames(heap_pages)
                .expect("Failed to allocate contiguous frames for heap")
        };
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [PHASE] Heap frames allocated successfully\n"
        );

        let heap_phys_addr = PhysAddr::new(heap_phys_addr_val as u64);

        let mut addr_buf = [0u8; 16];
        let len =
            petroleum::serial::format_hex_to_buffer(heap_phys_addr.as_u64(), &mut addr_buf, 16);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: Heap frames allocated at 0x");
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, &addr_buf[..len]);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"\n");

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [PHASE] Mapping heap using main_mapper\n"
        );
        {
            let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
            let frame_allocator = frame_allocator_guard
                .as_mut()
                .expect("Frame allocator not initialized");

            let _heap_flags = x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE
                | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;

            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PHASE] Calling petroleum::page_table::init for heap mapping\n"
            );
            let _mapper = unsafe {
                petroleum::page_table::init::<
                    _,
                    fn(
                        &mut x86_64::structures::paging::OffsetPageTable,
                        &mut petroleum::page_table::allocator::bitmap::BitmapFrameAllocator,
                    ),
                >(
                    self.physical_memory_offset,
                    frame_allocator,
                    kernel_phys_start.as_u64(),
                    None,
                )
            };
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PHASE] petroleum::page_table::init returned\n"
            );
            petroleum::write_serial_bytes!(
                0x3F8,
                0x3FD,
                b"DEBUG: [PHASE] Heap already covered by 1GB mapping, skipping\n"
            );
        }
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: [PHASE] Heap allocated and mapped\n");

        self.virtual_heap_start = self.physical_memory_offset + heap_phys_addr.as_u64();
        write_serial_bytes!(0x3F8, 0x3FD, b"Heap allocated and mapped\n");

        use petroleum::page_table::{ALLOCATOR, HEAP_INITIALIZED};

        let heap_start_for_allocator = self.virtual_heap_start;
        let heap_size_for_allocator = heap::HEAP_SIZE;

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [PHASE] Global allocator already initialized via init_global_heap\n"
        );
        unsafe {
            x86_64::instructions::interrupts::disable();
        }

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: HEAP_INITIALIZED store start\n");
        HEAP_INITIALIZED.store(true, core::sync::atomic::Ordering::SeqCst);
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: HEAP_INITIALIZED store done\n");

        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: set_heap_range start\n");
        petroleum::common::memory::set_heap_range(
            heap_start_for_allocator.as_u64() as usize,
            heap_size_for_allocator,
        );
        petroleum::write_serial_bytes!(0x3F8, 0x3FD, b"DEBUG: set_heap_range done\n");

        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: memory_management_initialization about to return\n"
        );

        let res_offset = self.physical_memory_offset;
        let res_phys = heap_phys_addr;
        let res_virt = self.virtual_heap_start;

        (res_offset, res_phys, res_virt)
    }

    #[cfg(target_os = "uefi")]
    pub fn prepare_kernel_stack(
        &mut self,
        virtual_heap_start: VirtAddr,
        physical_memory_offset: VirtAddr,
    ) -> VirtAddr {
        log::info!("Setting up kernel stack");
        self.heap_start_after_gdt = virtual_heap_start;

        assert!(
            virtual_heap_start.as_u64() % 16 == 0,
            "Kernel stack must be 16-byte aligned"
        );

        let stack_phys_start = self.heap_start_after_gdt.as_u64() - physical_memory_offset.as_u64();
        let stack_pages = (2 * 1024 * 1002) / 4096;

        let mut frame_allocator_guard = crate::heap::FRAME_ALLOCATOR.lock();
        let frame_allocator = frame_allocator_guard
            .as_mut()
            .expect("Frame allocator not initialized");

        let mut mapper = unsafe {
            petroleum::page_table::init::<
                _,
                fn(
                    &mut x86_64::structures::paging::OffsetPageTable,
                    &mut petroleum::page_table::allocator::bitmap::BitmapFrameAllocator,
                ),
            >(physical_memory_offset, frame_allocator, 0x100000, None)
        };

        let stack_flags = x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;

        unsafe {
            petroleum::page_table::raw::map_range_with_huge_pages(
                &mut mapper,
                frame_allocator,
                stack_phys_start,
                self.heap_start_after_gdt.as_u64(),
                stack_pages as u64,
                stack_flags,
                "kernel_stack",
            )
            .expect("Failed to map kernel stack to higher half");
        }

        write_serial_bytes!(0x3F8, 0x3FD, b"Kernel stack allocated and mapped (wide)\n");

        let kernel_stack_top =
            (self.heap_start_after_gdt.as_u64() + crate::heap::KERNEL_STACK_SIZE as u64) & !15;

        self.heap_start_after_stack =
            self.heap_start_after_gdt + crate::heap::KERNEL_STACK_SIZE as u64;

        VirtAddr::new(kernel_stack_top)
    }

    #[cfg(target_os = "uefi")]
    pub fn setup_allocator(&mut self, virtual_heap_start: VirtAddr) {
        if petroleum::page_table::HEAP_INITIALIZED.load(core::sync::atomic::Ordering::SeqCst) {
            return;
        }

        let kernel_overhead =
            (self.heap_start_after_stack.as_u64() - virtual_heap_start.as_u64()) as usize;
        let heap_size_remaining = heap::HEAP_SIZE - kernel_overhead;

        use petroleum::page_table::ALLOCATOR;
        unsafe {
            let mut allocator = ALLOCATOR.lock();
            allocator.init(
                self.heap_start_after_stack.as_mut_ptr::<u8>(),
                heap_size_remaining,
            );
        }
    }

    /// Map standard MMIO regions (APIC, IOAPIC, VGA text buffer).
    #[cfg(target_os = "uefi")]
    fn map_standard_mmio_regions() -> usize {
        petroleum::write_serial_bytes!(
            0x3F8,
            0x3FD,
            b"DEBUG: [map_mmio] Mapping MMIO regions for APIC and IOAPIC\n"
        );

        // CRITICAL: Read physical memory offset from the global static, NOT from a
        // function parameter. The parameter may be corrupted by stack operations
        // during UMM::init (which transfers the frame allocator and calls
        // OffsetPageTable::new). The global static is always correct.
        let phys_offset_val = petroleum::common::memory::get_physical_memory_offset() as u64;
        let physical_memory_offset = x86_64::VirtAddr::new(phys_offset_val);

        // Explicitly initialize LOCAL_APIC_ADDRESS with the virtual address of the Local APIC.
        unsafe {
            petroleum::common::utils::reset_mutex_lock(&petroleum::LOCAL_APIC_ADDRESS);
        }
        let lapic_virt = 0xfee00000u64 + physical_memory_offset.as_u64();
        *petroleum::LOCAL_APIC_ADDRESS.lock() = petroleum::LocalApicAddress(lapic_virt as *mut u32);

        let frame_allocator = petroleum::page_table::constants::get_frame_allocator_mut();
        let l4 = unsafe { petroleum::page_table::active_level_4_table(physical_memory_offset) };

        let mmio_flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE;
        let std_flags =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;

        let regions = [
            (0xfee00000u64, 1u64, "Local APIC", mmio_flags),
            (0xfec00000u64, 1u64, "IO APIC", mmio_flags),
            (
                0xb8000u64,
                (0xc0000u64 - 0xb8000u64) / 4096,
                "VGA text buffer",
                std_flags,
            ),
        ];

        let mut vga_virt_addr = 0u64;

        for (phys, pages, name, flags) in regions {
            let virt = phys + physical_memory_offset.as_u64();
            if name == "VGA text buffer" {
                vga_virt_addr = virt;
            }

            unsafe {
                for i in 0..pages {
                    let v = x86_64::VirtAddr::new(virt + i * 4096);
                    let p = x86_64::PhysAddr::new(phys + i * 4096);
                    if let Err(e) = petroleum::page_table::kernel::init::map_page_4k_l1(
                        l4,
                        v,
                        p,
                        flags,
                        frame_allocator,
                        physical_memory_offset,
                    ) {
                        petroleum::debug_log!("MMIO page {} map failed: {:?}, skipping\n", i, e);
                    }
                }
            }
        }

        vga_virt_addr as usize
    }

    /// Map MMIO regions (APIC, IOAPIC, VGA text buffer, GOP framebuffer).
    ///
    /// CRITICAL: Reads physical_memory_offset from the global static
    /// (petroleum::common::memory::get_physical_memory_offset) rather than accepting
    /// a function parameter. During UMM::init, the frame allocator is transferred
    /// from heap::FRAME_ALLOCATOR to constants::FRAME_ALLOCATOR, which involves
    /// OffsetPageTable::new calls that may corrupt stack values. The global
    /// PHYSICAL_MEMORY_OFFSET is set before any of these calls and remains correct.
    #[cfg(target_os = "uefi")]
    pub fn map_mmio() -> usize {
        // Map standard MMIO regions
        let vga_virt_addr = Self::map_standard_mmio_regions();

        // Map GOP Framebuffer if available
        if let Some(config) = petroleum::FULLERENE_FRAMEBUFFER_CONFIG
            .get()
            .and_then(|m| m.lock().clone())
        {
            let fb_phys = config.address;
            let fb_virt = fb_phys + petroleum::common::memory::get_physical_memory_offset() as u64;
            let fb_size = (config.width as u64 * config.height as u64 * config.bpp as u64) / 8;
            let fb_pages = (fb_size + 4095) / 4096;

            // Use NO_CACHE (Uncacheable) for the framebuffer to match MTRR settings
            // set by UEFI firmware for PCI MMIO regions. MTRR/PAT type mismatch would
            // cause #GP on access.
            let fb_flags = PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::NO_EXECUTE
                | PageTableFlags::NO_CACHE;

            let frame_allocator = petroleum::page_table::constants::get_frame_allocator_mut();
            let phys_offset = x86_64::VirtAddr::new(
                petroleum::common::memory::get_physical_memory_offset() as u64,
            );
            let l4 = unsafe { petroleum::page_table::active_level_4_table(phys_offset) };

            // Map the framebuffer using 4KiB pages. This avoids reliance on a non‑existent
            // 2MiB mapping helper and works reliably for any framebuffer size.
            unsafe {
                for i in 0..fb_pages {
                    let v = x86_64::VirtAddr::new(fb_virt + i as u64 * 4096);
                    let p = x86_64::PhysAddr::new(fb_phys + i as u64 * 4096);
                    if let Err(e) = petroleum::page_table::kernel::init::map_page_4k_l1(
                        l4,
                        v,
                        p,
                        fb_flags,
                        frame_allocator,
                        phys_offset,
                    ) {
                        petroleum::debug_log!(
                            "Framebuffer page {} map failed: {:?}, skipping\n",
                            i,
                            e
                        );
                    }
                }
            }
        }

        // Flush TLB by reloading CR3
        let cr3_val = x86_64::registers::control::Cr3::read();
        unsafe {
            x86_64::registers::control::Cr3::write(cr3_val.0, cr3_val.1);
        }

        vga_virt_addr
    }

    #[cfg(target_os = "uefi")]
    fn init_memory_map(&self) {
        debug_log_no_alloc!("!!! ENTERING init_memory_map !!!");

        unsafe {
            let mutex_ptr = core::ptr::addr_of!(crate::heap::MEMORY_MAP) as *mut u32;
            core::ptr::write_volatile(mutex_ptr, 0);
            debug_log_no_alloc!("DEBUG: Forced MEMORY_MAP lock reset to 0");
        }

        let map_addr = self.memory_map as u64;
        let base_ptr = if map_addr >= 0xFFFF_8000_0000_0000 {
            map_addr as *const u8
        } else {
            (map_addr + self.physical_memory_offset.as_u64()) as *const u8
        };
        let descriptor_size = self.descriptor_size;

        debug_log_no_alloc!("Base ptr: 0x");
        debug_log_no_alloc!(base_ptr as u64);
        debug_log_no_alloc!("Using descriptor size: ");
        debug_log_no_alloc!(descriptor_size);

        let raw_map_size = self.memory_map_size;
        let actual_descriptor_bytes = (raw_map_size / descriptor_size) * descriptor_size;
        let max_descriptors = actual_descriptor_bytes / descriptor_size;

        unsafe {
            let mut count = 0;
            let limit = crate::heap::MAX_DESCRIPTORS.min(max_descriptors);
            for i in 0..limit {
                let offset = i * descriptor_size;
                if offset >= actual_descriptor_bytes {
                    break;
                }
                let desc_ptr = base_ptr.add(offset);
                let desc = MemoryMapDescriptor::new(desc_ptr, descriptor_size);

                if !petroleum::page_table::MemoryDescriptorValidator::is_valid(&desc) {
                    debug_log_no_alloc!("Skipping invalid descriptor ");
                    debug_log_no_alloc!(i);
                    debug_log_no_alloc!(": type 0x");
                    debug_log_no_alloc!(desc.type_() as usize);
                    continue;
                }

                crate::heap::MEMORY_MAP_BUFFER[i] = desc;
                count += 1;
            }

            debug_log_no_alloc!("Successfully parsed ");
            debug_log_no_alloc!(count);
            debug_log_no_alloc!(" descriptors");
            debug_log_no_alloc!("DEBUG: Attempting to lock MEMORY_MAP for assignment");

            if let Some(mut lock) = crate::heap::MEMORY_MAP.try_lock() {
                *lock = Some(&crate::heap::MEMORY_MAP_BUFFER[0..count]);
                debug_log_no_alloc!("DEBUG: MEMORY_MAP lock acquired and assigned");
            } else {
                debug_log_no_alloc!("DEBUG: MEMORY_MAP is ALREADY LOCKED! Deadlock detected.");
            }
        }

        debug_log_no_alloc!("!!! INIT_MMAP DONE (FIXED) !!!");
    }
}
