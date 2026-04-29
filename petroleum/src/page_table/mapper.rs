use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        Mapper, OffsetPageTable, PageTableFlags, Size4KiB, PhysFrame, Page, PageTable, FrameAllocator,
    },
};
use crate::page_table::constants::{BootInfoFrameAllocator};
use crate::page_table::pe::{PeSection, derive_pe_flags, PeParser};

pub trait MemoryMappable {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_identity(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;

    fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>>;
}

pub struct MemoryMapper<'a> {
    pub mapper: &'a mut OffsetPageTable<'static>,
    pub frame_allocator: &'a mut BootInfoFrameAllocator,
    pub phys_offset: VirtAddr,
}

impl<'a> MemoryMapper<'a> {
    pub fn new(
        mapper: &'a mut OffsetPageTable<'static>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
        }
    }
}

impl<'a> MemoryMappable for MemoryMapper<'a> {
    fn map_region_with_flags(
        &mut self,
        phys_start: u64,
        virt_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        unsafe {
            crate::map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                phys_start,
                virt_start,
                num_pages,
                flags
            );
        }
        Ok(())
    }

    fn map_to_identity(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        unsafe {
            crate::identity_map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                phys_start,
                num_pages,
                flags
            );
        }
        Ok(())
    }

    fn map_to_higher_half(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        let virt_start = self.phys_offset.as_u64() + phys_start;
        unsafe {
            crate::map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                phys_start,
                virt_start,
                num_pages,
                flags
            );
        }
        Ok(())
    }
}

impl<'a> MemoryMapper<'a> {
    pub fn map_vga(&mut self) {
        use crate::page_table::constants::{VGA_MEMORY_END, VGA_MEMORY_START};
        const VGA_PAGES: u64 = (VGA_MEMORY_END - VGA_MEMORY_START + 4095) / 4096;
        let flags = crate::page_flags_const!(READ_WRITE_NO_EXEC);
        let _ = self.map_region_dual(VGA_MEMORY_START, VGA_PAGES, flags);
    }

    pub fn map_boot_code(&mut self) {
        use crate::page_table::constants::{BOOT_CODE_PAGES, BOOT_CODE_START};
        let flags = crate::page_flags_const!(READ_WRITE);
        unsafe {
            let _ = crate::map_range_with_log_macro!(
                self.mapper,
                self.frame_allocator,
                BOOT_CODE_START,
                self.phys_offset.as_u64() + BOOT_CODE_START,
                BOOT_CODE_PAGES,
                flags
            );

            for i in 0..BOOT_CODE_PAGES {
                let virt_addr = self.phys_offset.as_u64() + BOOT_CODE_START + (i * 4096);
                crate::page_table::utils::force_update_page_flags_no_flush(
                    self.mapper,
                    x86_64::VirtAddr::new(virt_addr),
                    flags,
                );
            }
            x86_64::instructions::tlb::flush_all();
            crate::debug_log_no_alloc!("Boot code flags forcefully updated to READ_WRITE (global TLB flush)");
        }
    }

    fn map_region_dual(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
        self.map_to_higher_half(phys_start, num_pages, flags)?;
        self.map_to_identity(phys_start, num_pages, flags)?;
        Ok(())
    }

    pub fn map_framebuffer(&mut self, addr: Option<VirtAddr>, size: Option<u64>) {
        if let (Some(addr), Some(size)) = (addr, size) {
            // Sanity check for size to prevent overflow and excessive mapping
            if size == 0 || size > 1024 * 1024 * 1024 * 10 { // 10 GiB limit
                return;
            }
            let pages = size.wrapping_add(4095) / 4096;
            let flags = crate::page_flags_const!(READ_WRITE);
            // addr is already the physical address from UEFI config
            let phys_start = addr.as_u64();
            let _ = self.map_region_dual(phys_start, pages, flags);
        }
    }
}

pub unsafe fn map_pe_section(
    mapper: &mut OffsetPageTable,
    section: PeSection,
    pe_base_phys: u64,
    phys_offset: VirtAddr,
    frame_allocator: &mut BootInfoFrameAllocator,
) {
    let flags = derive_pe_flags(section.characteristics);
    let section_start_phys = pe_base_phys + section.pointer_to_raw_data as u64;
    let section_start_virt = phys_offset.as_u64() + section.virtual_address as u64;
    let section_size = section.virtual_size as u64;
    let pages = section_size.div_ceil(4096);
    for p in 0..pages {
        let phys_addr = crate::calc_offset_addr!(section_start_phys, p);
        let virt_addr = crate::calc_offset_addr!(section_start_virt, p);
        crate::map_with_offset!(mapper, frame_allocator, phys_addr, virt_addr, flags, "panic");
    }
}

pub fn derive_memory_descriptor_flags<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(desc: &T) -> PageTableFlags {
    use x86_64::structures::paging::PageTableFlags as Flags;
    if desc.get_type() == crate::common::EfiMemoryType::EfiRuntimeServicesCode as u32 {
        Flags::PRESENT
    } else {
        Flags::PRESENT | Flags::WRITABLE | Flags::NO_EXECUTE
    }
}

pub unsafe fn map_available_memory_to_higher_half<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[T],
) {
    memory_map.iter().for_each(|desc| {
        if desc.is_valid() {
            let phys_start = desc.get_physical_start();
            let pages = desc.get_page_count();
            let flags = derive_memory_descriptor_flags(desc);
            crate::safe_map_to_higher_half!(
                mapper,
                frame_allocator,
                phys_offset,
                phys_start,
                pages,
                flags
            );
        }
    });
}

pub fn map_stack_to_higher_half<T: crate::page_table::efi_memory::MemoryDescriptorValidator>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    memory_map: &[T],
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let rsp = crate::get_current_stack_pointer!();
    for desc in memory_map.iter() {
        if desc.is_valid() {
            let start = desc.get_physical_start();
            let end = start + desc.get_page_count() * 4096;
            if rsp >= start && rsp < end {
                crate::safe_map_to_higher_half!(
                    mapper,
                    frame_allocator,
                    phys_offset,
                    desc.get_physical_start(),
                    desc.get_page_count(),
                    PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
                )?;
                break;
            }
        }
    }
    Ok(())
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "sysv64" fn landing_zone(
    _load_gdt: Option<fn()>,
    _load_idt: Option<fn()>,
    _phys_offset: VirtAddr,
    _level_4_table_frame: PhysFrame,
    _frame_allocator: *mut BootInfoFrameAllocator,
    _logic_fn_high: usize,
    _kernel_entry: usize,
) {
    unsafe {
        core::arch::naked_asm!(
            // 1. Immediate生存確認 (No stack usage)
            "mov dx, 0x3f8", "mov al, 0x4c", "out dx, al", // 'L'
            "mov dx, 0x3f8", "mov al, 0x4d", "out dx, al", // 'M'
            "mov dx, 0x3f8", "mov al, 0x4e", "out dx, al", // 'N'
            "mov dx, 0x3f8", "mov al, 0x58", "out dx, al", // 'X'

            // 2. IDT Load
            // System V ABI: load_idt is in RSI
            "mov dx, 0x3f8", "mov al, 0x59", "out dx, al", // 'Y'
            "test rsi, rsi", 
            "jz 1f",
            "mov dx, 0x3f8", "mov al, 0x49", "out dx, al", // 'I'
            "sub rsp, 8",
            "call rsi",
            "add rsp, 8",
            "mov dx, 0x3f8", "mov al, 0x4a", "out dx, al", // 'J'
            "1:",

            // 3. GDT Load
            // System V ABI: load_gdt is in RDI
            "mov dx, 0x3f8", "mov al, 0x5a", "out dx, al", // 'Z'
            "test rdi, rdi",
            "jz 2f",
            "mov dx, 0x3f8", "mov al, 0x4b", "out dx, al", // 'K'
            "sub rsp, 8",
            "call rdi",
            "add rsp, 8",
            "mov dx, 0x3f8", "mov al, 0x4f", "out dx, al", // 'O'
            "2:",

            // 4. Transition back to Rust logic
            "mov dx, 0x3f8", "mov al, 0x57", "out dx, al", // 'W'
            // Save RSP before alignment to restore it before returning
            "mov r10, rsp",
            // Ensure RSP is 16-byte aligned for the Rust function prologue
            "and rsp, -16",
            "call r12",
            // Logic function is noreturn, but just in case it returns, we hlt instead of ret
            "hlt",
        );
    }
}

#[repr(C)]
pub struct KernelArgs {
    pub handle: usize,
    pub system_table: usize,
    pub map_ptr: usize,
    pub map_size: usize,
    pub kernel_phys_start: u64,
}

/// Global pointer to kernel arguments, set during the high-half transition.
#[unsafe(no_mangle)]
pub static mut KERNEL_ARGS: *const KernelArgs = core::ptr::null();

#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "sysv64" fn landing_zone_logic(
    load_gdt: *const (),
    load_idt: *const (),
    phys_offset_raw: u64,
    l4_frame_raw: u64,
    frame_allocator: *mut BootInfoFrameAllocator,
    kernel_entry: usize,
    kernel_args: *const KernelArgs,
) {
    unsafe {
        KERNEL_ARGS = kernel_args;
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Logic: Start\n");
        let l4_phys = l4_frame_raw;
        let local_phys_offset = VirtAddr::new(phys_offset_raw);
        let local_frame_allocator = frame_allocator;

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"High-half transition: landing zone logic reached!\n");
        
        crate::flush_tlb_and_verify!();
        
        crate::debug_log_no_alloc!("TLB flushed in landing zone");

        let l4_virt = local_phys_offset + l4_phys;
        
        let mut mapper = x86_64::structures::paging::OffsetPageTable::new(
            &mut *(l4_virt.as_mut_ptr() as *mut PageTable),
            local_phys_offset,
        );

        let _ = mapper.map_to(
            x86_64::structures::paging::Page::<Size4KiB>::containing_address(l4_virt),
            x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(l4_phys)),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
            &mut *local_frame_allocator,
        );
        
        crate::debug_log_no_alloc!("L4 table mapped to high-half in landing zone");
        crate::debug_log_no_alloc!("Landing zone completed. Jumping directly to kernel entry...");
        
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Landing zone jumping to kernel entry!\n");
        
        if kernel_entry != 0 {
            let args = &*kernel_args;
            core::arch::asm!(
                "mov rcx, {handle}",
                "mov rdx, {st}",
                "mov r8, {map}",
                "mov r9, {size}",
                "jmp {}", 
                in(reg) kernel_entry,
                handle = in(reg) args.handle,
                st = in(reg) args.system_table,
                map = in(reg) args.map_ptr,
                size = in(reg) args.map_size,
                options(noreturn)
            );
        }
    }
}

pub unsafe fn map_to_higher_half_with_log(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
    phys_start: u64,
    num_pages: u64,
    flags: PageTableFlags,
) -> Result<(), x86_64::structures::paging::mapper::MapToError<Size4KiB>> {
    let virt_start = phys_offset.as_u64() + phys_start;
    crate::map_range_with_log_macro!(
        mapper,
        frame_allocator,
        phys_start,
        virt_start,
        num_pages,
        flags
    );
    Ok(())
}

struct KernelMapper<'a, 'b> {
    mapper: &'a mut OffsetPageTable<'b>,
    frame_allocator: &'a mut BootInfoFrameAllocator,
    phys_offset: VirtAddr,
}

impl<'a, 'b> KernelMapper<'a, 'b> {
    fn new(
        mapper: &'a mut OffsetPageTable<'b>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
        }
    }

    unsafe fn map_pe_sections(&mut self, kernel_phys_start: PhysAddr) -> bool {
        if let Some(parser) = unsafe { PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
            let pe_base_phys = parser.pe_base as u64;
            if let Some(sections) = unsafe { parser.sections() } {
                for section in sections.into_iter().filter(|s| s.virtual_size > 0) {
                    unsafe {
                        self.map_single_pe_section(section, pe_base_phys);
                    }
                }
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    unsafe fn map_fallback_kernel_region(&mut self, kernel_phys_start: PhysAddr) {
        use crate::page_table::pe::FALLBACK_KERNEL_SIZE;
        let kernel_size = FALLBACK_KERNEL_SIZE;
        let kernel_pages = kernel_size.div_ceil(4096);
        let flags = crate::page_flags_const!(READ_WRITE);
        unsafe {
            crate::page_table::utils::map_identity_range(
                self.mapper,
                self.frame_allocator,
                kernel_phys_start.as_u64(),
                kernel_pages,
                flags,
            )
            .expect("Failed to map fallback kernel range");
        }
    }

    unsafe fn map_single_pe_section(&mut self, section: PeSection, pe_base_phys: u64) {
        unsafe { map_pe_section(
            self.mapper,
            section,
            pe_base_phys,
            self.phys_offset,
            self.frame_allocator,
        ); }
    }
}

#[derive(Clone, Copy)]
pub struct MappingConfig {
    pub phys_start: u64,
    pub virt_start: u64,
    pub num_pages: u64,
    pub flags: PageTableFlags,
}

pub unsafe fn map_memory_descriptors_with_config<T: crate::page_table::efi_memory::MemoryDescriptorValidator, F>(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[T],
    config_fn: F,
) where
    F: Fn(&T) -> Option<MappingConfig>,
{
    for desc in memory_map.iter() {
        if let Some(config) = config_fn(desc) {
            unsafe {
                crate::map_range_with_log_macro!(
                    mapper,
                    frame_allocator,
                    config.phys_start,
                    config.virt_start,
                    config.num_pages,
                    config.flags
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_mid: u8,
    access: u8,
    flags: u8,
    base_high: u8,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
struct GdtDescriptor {
    limit: u16,
    base: u64,
}

#[repr(C, packed)]
struct TransitionGdt {
    descriptor: GdtDescriptor,
    entries: [GdtEntry; 3],
}

static mut TRANSITION_GDT: TransitionGdt = TransitionGdt {
    descriptor: GdtDescriptor {
        limit: (core::mem::size_of::<[GdtEntry; 3]>() - 1) as u16,
        base: 0,
    },
    entries: [
        GdtEntry { limit_low: 0, base_low: 0, base_mid: 0, access: 0, flags: 0, base_high: 0 }, // 0x00: Null
        GdtEntry { // 0x08: Kernel Code
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x9A, // Present, Ring 0, Code, Exec/Read
            flags: 0xAF, // Long mode, 64-bit
            base_high: 0,
        },
        GdtEntry { // 0x10: Kernel Data
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            access: 0x92, // Present, Ring 0, Data, Read/Write
            flags: 0,
            base_high: 0,
        },
    ],
};


pub struct PageTableInitializer<'a, T: crate::page_table::efi_memory::MemoryDescriptorValidator> {
    pub mapper: &'a mut OffsetPageTable<'static>,
    pub frame_allocator: &'a mut BootInfoFrameAllocator,
    pub phys_offset: VirtAddr,
    pub current_phys_offset: VirtAddr,
    pub memory_map: &'a [T],
    pub uefi_map_phys: u64,
    pub uefi_map_size: u64,
}

impl<'a, T: crate::page_table::efi_memory::MemoryDescriptorValidator> PageTableInitializer<'a, T> {
    pub fn new(
        mapper: &'a mut OffsetPageTable<'static>,
        frame_allocator: &'a mut BootInfoFrameAllocator,
        phys_offset: VirtAddr,
        current_phys_offset: VirtAddr,
        memory_map: &'a [T],
        uefi_map_phys: u64,
        uefi_map_size: u64,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            phys_offset,
            current_phys_offset,
            memory_map,
            uefi_map_phys,
            uefi_map_size,
        }
    }

    pub fn setup_transition_mappings(
        &mut self,
        kernel_phys_start: PhysAddr,
        level_4_table_frame: PhysFrame,
    ) -> u64 {
        crate::debug_log_no_alloc!("Setting up transition mappings for CR3 switch");
        
        // Removed the blanket 4GiB identity mapping using 1GiB pages.
        // This blanket mapping could overlap with sensitive MMIO regions (e.g., APIC) 
        // or conflict with specific 4KiB mappings, potentially causing hangs or #GP.
        // We now rely on map_essential_regions and map_current_stack_identity 
        // to provide the necessary transition mappings.

        let kernel_size = self.map_essential_regions(kernel_phys_start, level_4_table_frame);
        crate::debug_log_no_alloc!("Essential regions mapped");
        
        // CRITICAL: Ensure the current stack (RSP) is identity-mapped.
        // Instead of relying on map_current_stack_identity(), we map a generous 
        // range around the current RSP to prevent #PF/#UD during transition.
        unsafe {
            // 1. Map current stack (RSP)
            let rsp: u64;
            core::arch::asm!("mov {}, rsp", out(reg) rsp);
            let rsp_phys = rsp.wrapping_sub(self.current_phys_offset.as_u64());
            let stack_phys_start = rsp_phys.wrapping_sub(2 * 1024 * 1024) & !0xFFF;
            let stack_pages = (4 * 1024 * 1024) / 4096;
            
            self.map_identity_config_4kiB(stack_phys_start, stack_pages, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(self.current_phys_offset, stack_phys_start, stack_pages, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(self.phys_offset, stack_phys_start, stack_pages, crate::page_flags_const!(READ_WRITE));
            crate::debug_log_no_alloc!("Current stack region identity, current-offset, AND high-half mapped: 0x{:x}", stack_phys_start);

            // 2. Map current instruction pointer (RIP)
            // This is critical to ensure that the code executing the transition is mapped in the new page table.
            let rip: u64;
            core::arch::asm!("lea {}, [rip]", out(reg) rip);
            let rip_phys = rip.wrapping_sub(self.current_phys_offset.as_u64());
            let code_phys_start = rip_phys.wrapping_sub(2 * 1024 * 1024) & !0xFFF;
            let code_pages = (4 * 1024 * 1024) / 4096;

            self.map_identity_config_4kiB(code_phys_start, code_pages, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(self.current_phys_offset, code_phys_start, code_pages, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(self.phys_offset, code_phys_start, code_pages, crate::page_flags_const!(READ_WRITE));
            crate::debug_log_no_alloc!("Current code region identity, current-offset, AND high-half mapped: 0x{:x}", code_phys_start);
        }
        
        // Removed map_available_memory_identity() as it is too slow and unnecessary for the CR3 switch transition.
        // Only essential regions, kernel, and stack need to be identity mapped.
        
        // CRITICAL: Identity map a wide range of low physical memory to prevent #UD/#PF 
        // immediately after CR3 switch.
        // CRITICAL: Identity map a wide range of low physical memory to prevent #UD/#PF 
        // immediately after CR3 switch.
        // We use 4KiB pages here instead of huge pages because the subsequent 
        // `setup_higher_half_mappings` performs fine-grained 4KiB mappings.
        // If we use huge pages here, any attempt to map a 4KiB page within that 
        // range will fail with `ParentEntryHugePage`.
        unsafe {
            // CRITICAL: Ensure the current execution point (RIP) is identity-mapped.
            // We map a massive range starting from 0 to cover almost any possible 
            // UEFI load address (up to 4GiB).
            let low_mem_start = 0u64;
            let low_mem_size = 4 * 1024 * 1024 * 1024; // Increased to 4GiB
            let region_pages = low_mem_size / 4096;
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            
            self.map_identity_config_4kiB(
                low_mem_start,
                region_pages,
                flags,
            );
            
            self.map_at_offset_config_4kiB(
                self.phys_offset,
                low_mem_start,
                region_pages,
                flags,
            );
            
            crate::debug_log_no_alloc!("Low physical memory (4GiB) identity AND high-half mapped for transition (4KiB pages)");
        }

        // CRITICAL: Ensure the transition GDT and its descriptor are identity mapped.
        // This allows the CPU to load and use the GDT immediately after the CR3 switch,
        // regardless of where the bootloader is located in memory.
        unsafe {
            let gdt_virt_addr = core::ptr::addr_of!(TRANSITION_GDT) as *const _ as u64;
            // Map the entire TransitionGdt structure to BOTH identity and high-half.
            // This is critical because lgdt [rdi] uses the high-half address after CR3 switch.
            let gdt_phys_addr = (gdt_virt_addr.wrapping_sub(self.current_phys_offset.as_u64())) & !0xFFF;
            
            self.map_identity_config_4kiB(gdt_phys_addr, 1, crate::page_flags_const!(READ_WRITE));
            self.map_at_offset_config_4kiB(
                self.phys_offset,
                gdt_phys_addr,
                1,
                crate::page_flags_const!(READ_WRITE),
            );
            
            crate::debug_log_no_alloc!("Transition GDT identity AND high-half mapped at phys: 0x{:x}", gdt_phys_addr);
        }
        
        crate::debug_log_no_alloc!("Transition mappings completed");
        kernel_size
    }

    fn map_essential_regions(
        &mut self,
        kernel_phys_start: PhysAddr,
        level_4_table_frame: PhysFrame,
    ) -> u64 {
        unsafe {
            // 1. Identity map the first 512MB to ensure all early page tables and 
            // common UEFI regions are accessible during the transition.
            // We use 4KiB pages here to avoid `ParentEntryHugePage` errors when 
            // subsequent fine-grained mappings are applied to the same region.
            for i in 0..(512 * 1024 * 1024 / (2 * 1024 * 1024)) {
                let start = i * 2 * 1024 * 1024;
                self.map_identity_config_4kiB(
                    start,
                    (2 * 1024 * 1024) / 4096,
                    crate::page_flags_const!(READ_WRITE),
                );
            }

            let bitmap_virt_start =
                (&raw const crate::page_table::bitmap_allocator::BITMAP_STATIC) as *const _ as usize as u64;
            let bitmap_phys_start = bitmap_virt_start.wrapping_sub(self.current_phys_offset.as_u64());
            let bitmap_pages = ((131072 * 8) + 4095) / 4096;
            self.map_identity_config_4kiB(bitmap_phys_start, bitmap_pages, crate::page_flags_const!(READ_WRITE_NO_EXEC));
            self.map_identity_config_4kiB(
                level_4_table_frame.start_address().as_u64(),
                1,
                crate::page_flags_const!(READ_WRITE_NO_EXEC),
            );
            self.map_identity_config_4kiB(4096, crate::page_table::constants::UEFI_COMPAT_PAGES, crate::page_flags_const!(READ_WRITE_NO_EXEC));

            // IMPORTANT: Map the original UEFI memory map buffer. 
            // MemoryMapDescriptor holds pointers into this buffer.
            let uefi_map_pages = (self.uefi_map_size + 4095) / 4096;
            self.map_identity_config_4kiB(
                self.uefi_map_phys,
                uefi_map_pages,
                crate::page_flags_const!(READ_WRITE_NO_EXEC),
            );
            
            // Try to find the actual PE base to ensure the entire kernel image is mapped
            let (pe_base, kernel_size) = if let Some(parser) = unsafe { crate::page_table::pe::PeParser::new(kernel_phys_start.as_u64() as *const u8) } {
                let base = parser.pe_base as u64;
                let size = parser.size_of_image().unwrap_or(crate::page_table::pe::FALLBACK_KERNEL_SIZE);
                (base, size)
            } else {
                (kernel_phys_start.as_u64(), crate::page_table::pe::FALLBACK_KERNEL_SIZE)
            };
            let kernel_pages = kernel_size.div_ceil(4096);
            
            // Identity mapping for absolute low-address compatibility
            self.map_identity_config_4kiB(pe_base, kernel_pages, crate::page_flags_const!(READ_WRITE));
            
            // Current offset mapping to keep the CPU executing after CR3 switch
            self.map_at_offset_config_4kiB(
                self.current_phys_offset,
                pe_base,
                kernel_pages,
                crate::page_flags_const!(READ_WRITE),
            );

            // High-half mapping for the kernel image to prevent #PF immediately after switch
            self.map_at_offset_config_4kiB(
                self.phys_offset,
                pe_base,
                kernel_pages,
                crate::page_flags_const!(READ_WRITE),
            );
            
            self.map_identity_config_4kiB(crate::page_table::constants::BOOT_CODE_START, crate::page_table::constants::BOOT_CODE_PAGES, crate::page_flags_const!(READ_WRITE));
            kernel_size
        }
    }

    unsafe fn map_identity_config(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) {
        crate::identity_map_range_with_log_macro!(
            self.mapper,
            self.frame_allocator,
            phys_start,
            num_pages,
            flags
        );
    }

    unsafe fn map_identity_config_4kiB(
        &mut self,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) {
        let _ = crate::page_table::utils::map_range_4kiB(
            self.mapper,
            self.frame_allocator,
            phys_start,
            phys_start,
            num_pages,
            flags,
            "panic",
        );
    }

    unsafe fn map_at_offset_config_4kiB(
        &mut self,
        offset: VirtAddr,
        phys_start: u64,
        num_pages: u64,
        flags: PageTableFlags,
    ) {
        let virt_start = offset.as_u64() + phys_start;
        let _ = crate::page_table::utils::map_range_4kiB(
            self.mapper,
            self.frame_allocator,
            phys_start,
            virt_start,
            num_pages,
            flags,
            "panic",
        );
    }

    fn map_current_stack_identity(&mut self) {
        crate::map_current_stack!(
            self.mapper,
            self.frame_allocator,
            self.memory_map,
            crate::page_flags_const!(READ_WRITE_NO_EXEC)
        );
    }

    pub fn setup_higher_half_mappings(
        &mut self,
        kernel_phys_start: PhysAddr,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
    ) {
        crate::debug_log_no_alloc!("Setting up higher-half mappings");
        let mut kernel_mapper =
            KernelMapper::new(self.mapper, self.frame_allocator, self.phys_offset);
        if !unsafe { kernel_mapper.map_pe_sections(kernel_phys_start) } {
            unsafe {
                kernel_mapper.map_fallback_kernel_region(kernel_phys_start);
            }
        }
        crate::debug_log_no_alloc!("Kernel segments mapped to higher half");
        unsafe {
            crate::debug_log_no_alloc!("Mapping available memory to higher half...");
            self.map_available_memory_to_higher_half();
            crate::debug_log_no_alloc!("Mapping UEFI runtime to higher half...");
            self.map_uefi_runtime_to_higher_half();
            crate::debug_log_no_alloc!("Mapping stack to higher half...");
            self.map_stack_to_higher_half();
        }
        crate::debug_log_no_alloc!("Special regions mapped");

        let mut memory_mapper =
            MemoryMapper::new(self.mapper, self.frame_allocator, self.phys_offset);
        // Explicitly map framebuffer first to ensure it is present before other mappings
        memory_mapper.map_framebuffer(fb_addr, fb_size);
        memory_mapper.map_vga();
        memory_mapper.map_boot_code();
        crate::debug_log_no_alloc!("Additional regions mapped");
        crate::debug_log_no_alloc!("Higher-half mappings completed");
    }

    unsafe fn map_uefi_runtime_to_higher_half(&mut self) {
        map_available_memory_to_higher_half(
            self.mapper,
            self.frame_allocator,
            self.phys_offset,
            self.memory_map,
        );
    }

    unsafe fn map_available_memory_to_higher_half(&mut self) {
        map_available_memory_to_higher_half(
            self.mapper,
            self.frame_allocator,
            self.phys_offset,
            self.memory_map,
        );
    }

    unsafe fn map_stack_to_higher_half(&mut self) {
        crate::page_table::utils::map_stack_to_higher_half(
            self.mapper,
            self.frame_allocator,
            self.phys_offset,
            self.current_phys_offset,
            self.memory_map,
        )
        .expect("Failed to map stack region to higher half");
    }

    unsafe fn map_available_memory_identity(&mut self) {
        for desc in self.memory_map.iter() {
            if desc.is_valid() {
                let should_identity_map = desc.is_memory_available()
                    || (desc.get_type()
                        == crate::common::EfiMemoryType::EfiRuntimeServicesCode as u32
                        || desc.get_type()
                            == crate::common::EfiMemoryType::EfiRuntimeServicesData as u32)
                    || desc.get_type() == crate::common::EfiMemoryType::EfiBootServicesCode as u32
                    || desc.get_type() == crate::common::EfiMemoryType::EfiBootServicesData as u32;
                if should_identity_map {
                    let phys_start = desc.get_physical_start();
                    let pages = desc.get_page_count();
                    let flags = if desc.get_type()
                        == crate::common::EfiMemoryType::EfiRuntimeServicesCode as u32
                    {
                        PageTableFlags::PRESENT
                    } else {
                        PageTableFlags::PRESENT
                            | PageTableFlags::WRITABLE
                            | PageTableFlags::NO_EXECUTE
                    };
                    let _: core::result::Result<
                        (),
                        x86_64::structures::paging::mapper::MapToError<Size4KiB>,
                    > = crate::identity_map_range_with_log_macro!(
                        self.mapper,
                        self.frame_allocator,
                        phys_start,
                        pages,
                        flags
                    );
                }
            }
        }
    }
}

#[repr(C)]
pub struct TransitionContext {
    pub cr3: u64,
    pub load_gdt: *const (),
    pub load_idt: *const (),
    pub phys_offset: u64,
    pub l4_frame: u64,
    pub allocator: *const BootInfoFrameAllocator,
    pub logic_fn_high: usize,
    pub kernel_entry: usize,
    pub kernel_args_virt: u64,
    pub cs_selector: u64,
    pub landing_zone_high: usize,
    pub offset_diff: u64,
    pub gdt_ptr: *const u8,
}

impl TransitionContext {
    pub fn prepare(
        phys_offset: VirtAddr,
        current_physical_memory_offset: VirtAddr,
        level_4_table_frame: PhysFrame,
        frame_allocator: &mut BootInfoFrameAllocator,
        load_gdt: Option<fn()>,
        load_idt: Option<fn()>,
        gdt_ptr: Option<*const u8>,
        kernel_entry: Option<usize>,
        kernel_args_phys: Option<u64>,
    ) -> Self {
        let current_offset = current_physical_memory_offset.as_u64();
        let target_offset = phys_offset.as_u64();
        let offset_diff = target_offset.wrapping_sub(current_offset);

        unsafe {
            let gdt_ptr_static = core::ptr::addr_of_mut!(TRANSITION_GDT);
            let entries_virt_addr = core::ptr::addr_of!((*gdt_ptr_static).entries) as *const _ as u64;
            let gdt_phys_base = entries_virt_addr.wrapping_sub(current_offset);
            let gdt_high_base = gdt_phys_base.wrapping_add(target_offset);
            (*gdt_ptr_static).descriptor.base = gdt_high_base;
        }

        let final_gdt_ptr_virt = gdt_ptr.unwrap_or(unsafe {
            core::ptr::addr_of!((*core::ptr::addr_of!(TRANSITION_GDT)).descriptor) as *const _ as *const u8
        });
        let final_gdt_ptr_high = (final_gdt_ptr_virt as u64)
            .wrapping_sub(current_offset)
            .wrapping_add(target_offset) as *const u8;

        Self {
            cr3: level_4_table_frame.start_address().as_u64(),
            load_gdt: load_gdt.map_or(core::ptr::null(), |f| f as *const ()),
            load_idt: load_idt.map_or(core::ptr::null(), |f| f as *const ()),
            phys_offset: target_offset,
            l4_frame: level_4_table_frame.start_address().as_u64(),
            allocator: frame_allocator as *const _,
            logic_fn_high: ((landing_zone_logic as *const () as usize) as u64)
                .wrapping_sub(current_offset)
                .wrapping_add(target_offset) as usize,
            kernel_entry: kernel_entry.unwrap_or(0),
            kernel_args_virt: kernel_args_phys.map_or(0, |phys| phys + target_offset),
            cs_selector: 0x08,
            landing_zone_high: ((landing_zone as *const () as usize) as u64)
                .wrapping_sub(current_offset)
                .wrapping_add(target_offset) as usize,
            offset_diff,
            gdt_ptr: final_gdt_ptr_high,
        }
    }
}

pub fn perform_world_switch(ctx: TransitionContext) -> ! {
    unsafe {
        core::arch::asm!(
            // 1. Debug output
            "mov dx, 0x3f8", "mov al, 0x31", "out dx, al",

            // 2. Switch CR3
            "mov cr3, {cr3}",
            "mov dx, 0x3f8", "mov al, 0x32", "out dx, al",

            // 3. Load GDT
            "lgdt [{gdt_ptr}]",
            "mov dx, 0x3f8", "mov al, 0x33", "out dx, al",

            // 4. Shift RSP
            "add rsp, {offset_diff}",
            "mov dx, 0x3f8", "mov al, 0x36", "out dx, al",

            // 5. Set up arguments for landing_zone (System V ABI)
            "mov rdi, {load_gdt}",
            "mov rsi, {load_idt}",
            "mov rdx, {phys_offset}",
            "mov rcx, {l4_frame}",
            "mov r8, {allocator}",
            "mov r9, {kernel_entry}",

            // Push 7th argument (KernelArgs)
            "push {kernel_args_virt}",

            // Align stack to 16 bytes
            "and rsp, -16",

            // Set up the high-half address of the logic function in r12
            "mov r12, {logic_fn_high}",

            // Final debug and absolute jump to high-half landing zone
            "mov dx, 0x3f8", "mov al, 0x35", "out dx, al",
            "push {cs_selector}",
            "push {landing_zone_high}",
            "retfq",

            cr3 = in(reg) ctx.cr3,
            load_gdt = in(reg) ctx.load_gdt,
            load_idt = in(reg) ctx.load_idt,
            phys_offset = in(reg) ctx.phys_offset,
            l4_frame = in(reg) ctx.l4_frame,
            allocator = in(reg) ctx.allocator,
            logic_fn_high = in(reg) ctx.logic_fn_high,
            kernel_entry = in(reg) ctx.kernel_entry,
            kernel_args_virt = in(reg) ctx.kernel_args_virt,
            cs_selector = in(reg) ctx.cs_selector,
            landing_zone_high = in(reg) ctx.landing_zone_high,
            offset_diff = in(reg) ctx.offset_diff,
            gdt_ptr = in(reg) ctx.gdt_ptr,
            options(noreturn),
        );
        core::hint::unreachable_unchecked()
    }
}

pub struct PageTableReinitializer {
    pub phys_offset: VirtAddr,
}

impl PageTableReinitializer {
    pub fn new() -> Self {
        Self {
            phys_offset: crate::page_table::constants::HIGHER_HALF_OFFSET,
        }
    }

    pub fn reinitialize<T, F>(
        &mut self,
        kernel_phys_start: PhysAddr,
        fb_addr: Option<VirtAddr>,
        fb_size: Option<u64>,
        frame_allocator: &mut BootInfoFrameAllocator,
        memory_map: &[T],
        uefi_map_phys: u64,
        uefi_map_size: u64,
        current_physical_memory_offset: VirtAddr,
        load_gdt: Option<fn()>,
        load_idt: Option<fn()>,
        extra_mappings: Option<F>,
        gdt_ptr: Option<*const u8>,
        kernel_entry: Option<usize>,
        kernel_args_phys: Option<u64>,
    ) -> VirtAddr 
    where 
        T: crate::page_table::efi_memory::MemoryDescriptorValidator,
        F: FnOnce(&mut OffsetPageTable, &mut BootInfoFrameAllocator, VirtAddr),
    {
        crate::debug_log_no_alloc!("Page table reinitialization starting");
        let level_4_table_frame =
            self.create_page_table(frame_allocator, current_physical_memory_offset);
        let mut mapper = self.setup_new_mapper(
            level_4_table_frame,
            current_physical_memory_offset,
            frame_allocator,
        );
        let mut initializer =
            PageTableInitializer::new(
                &mut mapper,
                frame_allocator,
                self.phys_offset,
                current_physical_memory_offset,
                memory_map,
                uefi_map_phys,
                uefi_map_size,
            );
        
        // 1. Setup transition mappings (including current_physical_memory_offset)
        let _kernel_size =
            unsafe { initializer.setup_transition_mappings(kernel_phys_start, level_4_table_frame) };
        
        // 2. Setup higher-half mappings
        initializer.setup_higher_half_mappings(kernel_phys_start, fb_addr, fb_size);

        if let Some(mapping_fn) = extra_mappings {
            unsafe {
                mapping_fn(&mut mapper, frame_allocator, self.phys_offset);
            }
        }
        
        // 3. Recursive mapping
        self.setup_recursive_mapping(&mut mapper, level_4_table_frame);
        
        // 4. CRITICAL: Pre-map the new L4 table to the new phys_offset using the OLD mapper.
        // This solves the "chicken and egg" problem where the new mapper needs L4 mapped to work.
        unsafe {
            let l4_phys = level_4_table_frame.start_address().as_u64();
            let l4_virt = self.phys_offset.as_u64() + l4_phys;
            crate::map_range_with_log_macro!(
                &mut mapper,
                frame_allocator,
                l4_phys,
                l4_virt,
                1,
                crate::page_flags_const!(READ_WRITE_NO_EXEC)
            );
            crate::debug_log_no_alloc!("Pre-mapped L4 table to new phys_offset: 0x", l4_virt as usize);
        }

        // 5. Switch CR3
        self.perform_page_table_switch(
            level_4_table_frame,
            frame_allocator,
            current_physical_memory_offset,
            load_gdt,
            load_idt,
            gdt_ptr,
            kernel_entry,
            kernel_args_phys,
        );
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"Page table switch: returned to reinitialize\n");
        
        self.phys_offset
    }

    fn create_page_table(
        &self,
        frame_allocator: &mut BootInfoFrameAllocator,
        current_physical_memory_offset: VirtAddr,
    ) -> PhysFrame {
        crate::debug_log_no_alloc!("Allocating new L4 page table frame");
        let level_4_table_frame = match frame_allocator.allocate_frame() {
            Some(frame) => frame,
            None => panic!("Failed to allocate L4 page table frame"),
        };
        unsafe {
            let table_phys = level_4_table_frame.start_address().as_u64();
            let table_virt = current_physical_memory_offset + table_phys;
            let table_ptr = table_virt.as_mut_ptr() as *mut PageTable;
            *table_ptr = PageTable::new();
        }
        crate::debug_log_no_alloc!("New L4 page table created and zeroed");
        level_4_table_frame
    }

    fn setup_new_mapper(
        &self,
        level_4_table_frame: PhysFrame,
        current_physical_memory_offset: VirtAddr,
        frame_allocator: &mut BootInfoFrameAllocator,
    ) -> OffsetPageTable<'static> {
        crate::debug_log_no_alloc!("Setting up new page table mapper");
        let temp_phys_addr = level_4_table_frame.start_address().as_u64();
        let temp_virt_addr = current_physical_memory_offset + temp_phys_addr;
        let temp_page = Page::<Size4KiB>::containing_address(temp_virt_addr);
        crate::debug_log_no_alloc!(
            "Using existing phys offset mapping at: 0x",
            temp_virt_addr.as_u64() as usize
        );
        if temp_virt_addr.as_u64() < 0x800000000000 {
            unsafe {
                return OffsetPageTable::new(
                    &mut *(temp_virt_addr.as_mut_ptr() as *mut PageTable),
                    current_physical_memory_offset,
                );
            }
        }
        panic!(
            "Cannot create any mapping for L4 table frame - UEFI huge page coverage is complete"
        );
    }

    fn setup_recursive_mapping(
        &self,
        mapper: &mut OffsetPageTable,
        level_4_table_frame: PhysFrame,
    ) {
        unsafe {
            let table = mapper.level_4_table() as *const PageTable as *mut PageTable;
            (&mut *table
                .cast::<x86_64::structures::paging::page_table::PageTableEntry>()
                .add(511))
                .set_addr(
                    level_4_table_frame.start_address(),
                    crate::page_flags_const!(READ_WRITE),
                );
        }
    }

    fn perform_page_table_switch(
        &self,
        level_4_table_frame: PhysFrame,
        frame_allocator: &mut BootInfoFrameAllocator,
        current_physical_memory_offset: VirtAddr,
        load_gdt: Option<fn()>,
        load_idt: Option<fn()>,
        gdt_ptr: Option<*const u8>,
        kernel_entry: Option<usize>,
        kernel_args_phys: Option<u64>,
    ) {
        x86_64::instructions::interrupts::disable();
        crate::debug_log_no_alloc!("About to switch CR3 to new table: 0x", level_4_table_frame.start_address().as_u64() as usize);
        
        let ctx = TransitionContext::prepare(
            self.phys_offset,
            current_physical_memory_offset,
            level_4_table_frame,
            frame_allocator,
            load_gdt,
            load_idt,
            gdt_ptr,
            kernel_entry,
            kernel_args_phys,
        );

        // The current perform_page_table_switch has significant side-effects: 
        // It maps the current RIP and the landing zone area into the new page table.
        // Since TransitionContext::prepare is a pure calculation, we must keep 
        // these mapping operations here.
        unsafe {
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: inside unsafe block, getting RIP\n");
            let rip: u64;
            core::arch::asm!("lea {}, [rip]", out(reg) rip);
            
            let rip_phys = rip.wrapping_sub(current_physical_memory_offset.as_u64());
            let rip_region_start = (rip_phys.wrapping_sub(2 * 1024 * 1024)) & !0xFFF;
            let rip_region_pages = (4 * 1024 * 1024) / 4096;
            
            let l4_phys_u64 = level_4_table_frame.start_address().as_u64();
            let l4_virt = VirtAddr::new(current_physical_memory_offset.as_u64() + l4_phys_u64);
            let mut new_mapper = x86_64::structures::paging::OffsetPageTable::new(
                &mut *(l4_virt.as_mut_ptr() as *mut x86_64::structures::paging::PageTable),
                VirtAddr::new(current_physical_memory_offset.as_u64()),
            );

            for i in 0..rip_region_pages {
                let p_phys = rip_region_start + (i * 4096);
                let v_addr = VirtAddr::new(p_phys.wrapping_add(current_physical_memory_offset.as_u64()));
                let page = x86_64::structures::paging::Page::<Size4KiB>::containing_address(v_addr);
                let _ = new_mapper.unmap(page);
                let _ = new_mapper.map_to(
                    page,
                    x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(p_phys)),
                    x86_64::structures::paging::PageTableFlags::PRESENT | x86_64::structures::paging::PageTableFlags::WRITABLE,
                    frame_allocator,
                );
            }
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: RIP region mapped\n");
            crate::debug_log_no_alloc!("Current RIP region (4MB) explicitly mapped to current virtual address in new page table");

            let kernel_base_virt = landing_zone as *const () as usize as u64;
            let kernel_base_phys = kernel_base_virt.wrapping_sub(current_physical_memory_offset.as_u64());
            let region_start_phys = (kernel_base_phys.wrapping_sub(1024 * 1024)) & !0xFFF;
            let region_pages = (64 * 1024 * 1024) / 4096;
            
            for i in 0..region_pages {
                let p_phys = region_start_phys + (i * 4096);
                let v_low = VirtAddr::new(p_phys.wrapping_add(current_physical_memory_offset.as_u64()));
                let page_low = x86_64::structures::paging::Page::<Size4KiB>::containing_address(v_low);
                let _ = new_mapper.unmap(page_low);
                let _ = new_mapper.map_to(
                    page_low,
                    x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(p_phys)),
                    x86_64::structures::paging::PageTableFlags::PRESENT | x86_64::structures::paging::PageTableFlags::WRITABLE,
                    frame_allocator,
                );
                let v_high = VirtAddr::new(p_phys.wrapping_add(self.phys_offset.as_u64()));
                let page_high = x86_64::structures::paging::Page::<Size4KiB>::containing_address(v_high);
                let _ = new_mapper.unmap(page_high);
                let _ = new_mapper.map_to(
                    page_high,
                    x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(p_phys)),
                    x86_64::structures::paging::PageTableFlags::PRESENT | x86_64::structures::paging::PageTableFlags::WRITABLE,
                    frame_allocator,
                );
            }
            crate::write_serial_bytes!(0x3F8, 0x3FD, b"Debug: Landing zone region mapped\n");
            crate::mem_debug!("landing_zone region (2MB) mapped at low and high", "\n");
        }

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"CR3 switch: about to enter asm! block\n");
        perform_world_switch(ctx);
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"CR3 switch: returned from asm! block\n");
    }
}

pub fn reinit_page_table_with_allocator<T, F>(
    kernel_phys_start: PhysAddr,
    fb_addr: Option<VirtAddr>,
    fb_size: Option<u64>,
    frame_allocator: &mut BootInfoFrameAllocator,
    memory_map: &[T],
    uefi_map_phys: u64,
    uefi_map_size: u64,
    current_physical_memory_offset: VirtAddr,
    load_gdt: Option<fn()>,
    load_idt: Option<fn()>,
    extra_mappings: Option<F>,
    gdt_ptr: Option<*const u8>,
    kernel_entry: Option<usize>,
    kernel_args_phys: Option<u64>,
) -> VirtAddr 
where 
    T: crate::page_table::efi_memory::MemoryDescriptorValidator,
    F: FnOnce(&mut OffsetPageTable, &mut BootInfoFrameAllocator, VirtAddr),
{
    let mut reinitializer = PageTableReinitializer::new();
        reinitializer.reinitialize(
            kernel_phys_start,
            fb_addr,
            fb_size,
            frame_allocator,
            memory_map,
            uefi_map_phys,
            uefi_map_size,
            current_physical_memory_offset,
            load_gdt,
            load_idt,
            extra_mappings,
            gdt_ptr,
            kernel_entry,
            kernel_args_phys,
        )
}

pub unsafe fn unmap_identity_range(
    mapper: &mut OffsetPageTable,
    start_addr: u64,
    num_pages: u64,
) {
    for i in 0..num_pages {
        let addr = start_addr + (i * 4096);
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
        let _ = mapper.unmap(page);
    }
}
