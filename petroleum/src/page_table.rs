pub mod bitmap_allocator;
pub mod constants;
pub mod efi_memory;
pub mod pe;
mod utils;
pub mod mapper;
mod manager;
mod heap;
mod tests;

pub use bitmap_allocator::BitmapFrameAllocator;
pub use efi_memory::{EfiMemoryDescriptor, MemoryDescriptorValidator, process_memory_descriptors};
pub use constants::*;
pub use pe::{PeParser, PeSection, calculate_kernel_memory_size};
pub use utils::*;
pub use mapper::*;
pub use manager::*;
pub use heap::*;
pub use tests::*;

pub type BootInfoFrameAllocator = BitmapFrameAllocator;