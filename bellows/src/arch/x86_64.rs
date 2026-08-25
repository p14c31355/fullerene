//! x86_64 UEFI handoff policy for Bellows.

use fullerene_abi::boot::{self, BootArchitecture, BootInfo, BootPlatform};

pub fn make_boot_info(
    kernel_base: u64,
    kernel_size: u64,
    kernel_entry: u64,
    memory_map_address: u64,
    memory_map_size: u64,
    memory_map_descriptor_size: u64,
    framebuffer: Option<(u64, u32, u32, u32, u32)>,
) -> BootInfo {
    let mut info = BootInfo::new(BootArchitecture::X86_64, BootPlatform::PcUefi);
    info.kernel_base = kernel_base;
    info.kernel_size = kernel_size;
    info.kernel_entry = kernel_entry;
    info.memory_map_address = memory_map_address;
    info.memory_map_size = memory_map_size;
    info.memory_map_descriptor_size = memory_map_descriptor_size;
    info.flags |= boot::flags::MEMORY_MAP;

    if let Some((address, width, height, stride, bpp)) = framebuffer {
        info.framebuffer_address = address;
        info.framebuffer_size = u64::from(stride).saturating_mul(u64::from(height));
        info.framebuffer_width = width;
        info.framebuffer_height = height;
        info.framebuffer_stride = stride;
        info.framebuffer_bpp = bpp;
        if address != 0 && info.framebuffer_size != 0 {
            info.flags |= boot::flags::FRAMEBUFFER;
        }
    }

    info
}
