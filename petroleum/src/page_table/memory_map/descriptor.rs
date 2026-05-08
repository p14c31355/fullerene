use crate::common::EfiMemoryType;

// EFI Memory Descriptor as defined in UEFI spec
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct EfiMemoryDescriptor {
    pub type_: EfiMemoryType,
    pub padding: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attribute: u64,
}

#[derive(Clone, Copy)]
pub struct MemoryMapDescriptor {
    pub ptr: *const u8,
    pub descriptor_size: usize,
}

impl MemoryMapDescriptor {
    pub fn new(ptr: *const u8, descriptor_size: usize) -> Self {
        Self {
            ptr,
            descriptor_size,
        }
    }

    pub fn type_(&self) -> u32 {
        unsafe { core::ptr::read_unaligned(self.ptr as *const u32) }
    }

    pub fn padding(&self) -> u32 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(4) as *const u32) }
    }

    pub fn physical_start(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(8) as *const u64) }
    }

    pub fn virtual_start(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(16) as *const u64) }
    }

    pub fn number_of_pages(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(24) as *const u64) }
    }

    pub fn attribute(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self.ptr.add(self.descriptor_size - 8) as *const u64) }
    }
}

unsafe impl Send for MemoryMapDescriptor {}
unsafe impl Sync for MemoryMapDescriptor {}