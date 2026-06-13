//! BootContext — boot-time information from UEFI/BIOS.
use petroleum::common::uefi::FullereneFramebufferConfig;
use petroleum::page_table::memory_map::MemoryMapDescriptor;

#[derive(Clone, Copy)]
pub struct MemoryMapInfo { pub entries: Option<&'static [MemoryMapDescriptor]>, pub usable_bytes: u64 }
impl MemoryMapInfo { pub const fn new() -> Self { Self { entries: None, usable_bytes: 0 } } }

#[derive(Clone, Copy)]
pub struct BootFramebufferInfo { pub config: FullereneFramebufferConfig }
impl BootFramebufferInfo {
    pub const fn new() -> Self { Self { config: FullereneFramebufferConfig { address:0,width:0,height:0,pixel_format:petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,bpp:0,stride:0 } } }
    pub fn has_valid_fb(&self) -> bool { let c=&self.config; c.address>=0x100000&&c.width>0&&c.width<=16384&&c.height>0&&c.height<=16384&&(c.bpp==8||c.bpp==16||c.bpp==24||c.bpp==32)&&c.stride>0 }
    pub fn fb_virt(&self) -> u64 { self.config.address+petroleum::common::memory::get_physical_memory_offset() as u64 }
    pub fn fb_byte_size(&self) -> u64 { self.config.stride as u64*self.config.height as u64*(self.config.bpp as u64/8) }
}

#[derive(Clone, Copy)]
pub struct AcpiInfo { pub rsdp_address: u64, pub parsed: bool }
impl AcpiInfo { pub const fn new() -> Self { Self { rsdp_address: 0, parsed: false } } }

#[derive(Clone, Copy)]
pub struct RuntimeInfo { pub kernel_args_ptr: *const petroleum::assembly::KernelArgs, pub runtime_available: bool }
impl RuntimeInfo { pub const fn new() -> Self { Self { kernel_args_ptr: core::ptr::null(), runtime_available: false } } }

pub struct BootContext {
    pub memory_map: MemoryMapInfo, pub framebuffer: BootFramebufferInfo, pub acpi: AcpiInfo, pub runtime: RuntimeInfo,
    pub framebuffer_config: FullereneFramebufferConfig, pub memory_map_entries: Option<&'static [MemoryMapDescriptor]>,
    pub rsdp_address: u64, pub kernel_args: *const petroleum::assembly::KernelArgs,
}
unsafe impl Send for BootContext {}
unsafe impl Sync for BootContext {}

impl BootContext {
    pub const fn empty() -> Self { Self { memory_map:MemoryMapInfo::new(),framebuffer:BootFramebufferInfo::new(),acpi:AcpiInfo::new(),runtime:RuntimeInfo::new(),framebuffer_config:FullereneFramebufferConfig{address:0,width:0,height:0,pixel_format:petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,bpp:0,stride:0},memory_map_entries:None,rsdp_address:0,kernel_args:core::ptr::null() } }
    pub unsafe fn new(kernel_args: *const petroleum::assembly::KernelArgs, memory_map: Option<&'static [MemoryMapDescriptor]>, rsdp_address: u64) -> Self {
        let (a,w,h,bpp) = if let Some(args) = unsafe { kernel_args.as_ref() } { (args.fb_address,args.fb_width,args.fb_height,args.fb_bpp) } else { (0,0,0,0) };
        Self { memory_map:MemoryMapInfo{entries:memory_map,usable_bytes:0},framebuffer:BootFramebufferInfo{config:FullereneFramebufferConfig{address:a,width:w,height:h,pixel_format:petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,bpp,stride:w*(bpp/8)}},acpi:AcpiInfo{rsdp_address,parsed:false},runtime:RuntimeInfo{kernel_args_ptr:kernel_args,runtime_available:true},framebuffer_config:FullereneFramebufferConfig{address:a,width:w,height:h,pixel_format:petroleum::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,bpp,stride:w*(bpp/8)},memory_map_entries:memory_map,rsdp_address,kernel_args }
    }
    pub fn has_valid_framebuffer(&self) -> bool { self.framebuffer.has_valid_fb() }
    pub fn framebuffer_virt(&self) -> u64 { self.framebuffer.fb_virt() }
    pub fn framebuffer_byte_size(&self) -> u64 { self.framebuffer.fb_byte_size() }
}

static BOOT: spin::Mutex<Option<BootContext>> = spin::Mutex::new(None);
pub fn init_boot() { *BOOT.lock() = Some(BootContext::empty()); }
pub fn get_boot() -> &'static spin::Mutex<Option<BootContext>> { &BOOT }
pub fn with_boot<F, R>(f: F) -> Option<R> where F: FnOnce(&BootContext) -> R { BOOT.lock().as_ref().map(f) }
