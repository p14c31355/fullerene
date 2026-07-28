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
    pub address: usize,
    pub descriptor_size: usize,
}

impl MemoryMapDescriptor {
    pub const fn new(address: usize, descriptor_size: usize) -> Self {
        Self {
            address,
            descriptor_size,
        }
    }

    pub fn type_(&self) -> u32 {
        unsafe { core::ptr::read_unaligned(self.address as *const u32) }
    }

    pub fn padding(&self) -> u32 {
        unsafe { core::ptr::read_unaligned((self.address + 4) as *const u32) }
    }

    pub fn physical_start(&self) -> u64 {
        unsafe { core::ptr::read_unaligned((self.address + 8) as *const u64) }
    }

    pub fn virtual_start(&self) -> u64 {
        unsafe { core::ptr::read_unaligned((self.address + 16) as *const u64) }
    }

    pub fn number_of_pages(&self) -> u64 {
        unsafe { core::ptr::read_unaligned((self.address + 24) as *const u64) }
    }

    pub fn attribute(&self) -> u64 {
        unsafe {
            core::ptr::read_unaligned(
                (self.address + self.descriptor_size.saturating_sub(8)) as *const u64,
            )
        }
    }
}

unsafe impl Send for MemoryMapDescriptor {}
unsafe impl Sync for MemoryMapDescriptor {}
