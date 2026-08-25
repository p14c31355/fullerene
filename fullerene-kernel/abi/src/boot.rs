//! Stable data exchanged between a Fullerene bootloader and kernel.
//!
//! Boot addresses are represented as `u64` rather than pointers or `usize` so
//! the same layout can be produced by a 32-bit-capable loader and consumed by
//! either the x86_64 or AArch64 kernel. Zero means that an optional payload is
//! not present.

pub const BOOT_INFO_MAGIC: u64 = 0x4642_4f4f_5449_4e46; // "FBOOTINF"
pub const BOOT_INFO_VERSION: u32 = 1;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootArchitecture {
    Unknown = 0,
    X86_64 = 1,
    Aarch64 = 2,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPlatform {
    Unknown = 0,
    PcUefi = 1,
    QemuVirt = 2,
    Bramble = 3,
}

pub mod flags {
    pub const FDT: u64 = 1 << 0;
    pub const INITRD: u64 = 1 << 1;
    pub const CMDLINE: u64 = 1 << 2;
    pub const MEMORY_MAP: u64 = 1 << 3;
    pub const FRAMEBUFFER: u64 = 1 << 4;
}

/// Versioned, pointer-free handoff record shared by Bellows and Fullerene.
///
/// The record contains addresses and lengths only. The pointed-to objects keep
/// their native format: FDT for `fdt_address`, the bootloader memory-map
/// descriptor format for `memory_map_address`, and UTF-8 bytes for cmdline.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    pub size: u32,
    pub architecture: BootArchitecture,
    pub platform: BootPlatform,
    pub flags: u64,

    pub kernel_base: u64,
    pub kernel_size: u64,
    pub kernel_entry: u64,

    pub fdt_address: u64,

    pub initrd_address: u64,
    pub initrd_size: u64,

    pub cmdline_address: u64,
    pub cmdline_size: u64,

    pub memory_map_address: u64,
    pub memory_map_size: u64,
    pub memory_map_descriptor_size: u64,

    pub framebuffer_address: u64,
    pub framebuffer_size: u64,
    pub framebuffer_width: u32,
    pub framebuffer_height: u32,
    pub framebuffer_stride: u32,
    pub framebuffer_bpp: u32,
}

impl BootInfo {
    pub const BYTE_SIZE: usize = core::mem::size_of::<Self>();

    pub const fn new(architecture: BootArchitecture, platform: BootPlatform) -> Self {
        Self {
            magic: BOOT_INFO_MAGIC,
            version: BOOT_INFO_VERSION,
            size: Self::BYTE_SIZE as u32,
            architecture,
            platform,
            flags: 0,
            kernel_base: 0,
            kernel_size: 0,
            kernel_entry: 0,
            fdt_address: 0,
            initrd_address: 0,
            initrd_size: 0,
            cmdline_address: 0,
            cmdline_size: 0,
            memory_map_address: 0,
            memory_map_size: 0,
            memory_map_descriptor_size: 0,
            framebuffer_address: 0,
            framebuffer_size: 0,
            framebuffer_width: 0,
            framebuffer_height: 0,
            framebuffer_stride: 0,
            framebuffer_bpp: 0,
        }
    }

    pub const fn is_valid(&self) -> bool {
        self.magic == BOOT_INFO_MAGIC
            && self.version == BOOT_INFO_VERSION
            && self.size as usize >= Self::BYTE_SIZE
    }
}

const _: () = {
    assert!(core::mem::size_of::<BootInfo>() == BootInfo::BYTE_SIZE);
    assert!(core::mem::align_of::<BootInfo>() == 8);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_info_has_a_stable_header() {
        let info = BootInfo::new(BootArchitecture::Aarch64, BootPlatform::Bramble);
        assert!(info.is_valid());
        assert_eq!(info.size as usize, BootInfo::BYTE_SIZE);
        assert_eq!(info.flags, 0);
    }

    #[test]
    fn optional_payloads_are_flagged_explicitly() {
        let mut info = BootInfo::new(BootArchitecture::X86_64, BootPlatform::PcUefi);
        info.flags = flags::MEMORY_MAP | flags::FRAMEBUFFER;
        assert_ne!(info.flags & flags::MEMORY_MAP, 0);
        assert_eq!(info.flags & flags::FDT, 0);
    }
}
