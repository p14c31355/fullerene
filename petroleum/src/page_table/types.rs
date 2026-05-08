use x86_64::structures::paging::{PageTableFlags, PhysFrame, Size4KiB};

pub trait PageTableHelper {
    fn map_page(
        &mut self,
        virtual_addr: usize,
        physical_addr: usize,
        flags: PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<()>;
    fn unmap_page(
        &mut self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PhysFrame>;
    fn translate_address(&self, virtual_addr: usize)
    -> crate::common::logging::SystemResult<usize>;
    fn set_page_flags(
        &mut self,
        virtual_addr: usize,
        flags: PageTableFlags,
    ) -> crate::common::logging::SystemResult<()>;
    fn get_page_flags(
        &self,
        virtual_addr: usize,
    ) -> crate::common::logging::SystemResult<PageTableFlags>;
    fn flush_tlb(&mut self, virtual_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn flush_tlb_all(&mut self) -> crate::common::logging::SystemResult<()>;
    fn create_page_table(
        &mut self,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize>;
    fn destroy_page_table(
        &mut self,
        table_addr: usize,
        frame_allocator: &mut crate::page_table::constants::BootInfoFrameAllocator,
    ) -> crate::common::logging::SystemResult<()>;
    fn clone_page_table(
        &mut self,
        source_table: usize,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> crate::common::logging::SystemResult<usize>;
    fn switch_page_table(&mut self, table_addr: usize) -> crate::common::logging::SystemResult<()>;
    fn current_page_table(&self) -> usize;
}

pub trait MemoryDescriptorValidator {
    fn is_valid(&self) -> bool;
    fn get_type(&self) -> u32;
    fn get_physical_start(&self) -> u64;
    fn get_page_count(&self) -> u64;
    fn is_memory_available(&self) -> bool;
}