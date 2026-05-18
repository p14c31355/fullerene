//! Virtio-GPU driver for QEMU/virtio-gpu-pci
//!
//! This driver uses a fixed command buffer allocated from the heap
//! to avoid the problem of stack addresses becoming stale when the
//! VirtioGpu struct is moved, or when submit() returns and its
//! stack frame (containing the command) is freed.

use crate::hardware::pci::{PciDevice, PciScanner, PciConfigSpace};
use crate::virtio::pci::{find_virtio_capability, get_virtio_caps, read_virtio_reg_via_pci_cfg, write_virtio_reg_via_pci_cfg, VIRTIO_PCI_CAP_PCI_CFG, dump_capabilities};
pub use crate::virtio::pci::{VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG};
use core::ptr::{read_volatile, write_volatile};

pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
pub const VIRTIO_STATUS_DRIVER: u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

/// VirtIO queue size (must be a power of two, QEMU default is 1024 for virtio-gpu)
const QUEUE_SIZE: u16 = 1024;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; 1024],
    pub used_event: u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u32; 2048],
    pub avail_event: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct VirtioGpuCtrlHeader {
    pub type_: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub padding: u32,
}

impl VirtioGpuCtrlHeader {
    pub fn to_le(self) -> Self {
        Self {
            type_: self.type_.to_le(),
            flags: self.flags.to_le(),
            fence_id: self.fence_id.to_le(),
            ctx_id: self.ctx_id.to_le(),
            padding: self.padding.to_le(),
        }
    }
}

pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct VirtioGpuResourceCreate2d {
    pub hdr: VirtioGpuCtrlHeader,
    pub resource_id: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

impl VirtioGpuResourceCreate2d {
    pub fn to_le(self) -> Self {
        Self {
            hdr: self.hdr.to_le(),
            resource_id: self.resource_id.to_le(),
            format: self.format.to_le(),
            width: self.width.to_le(),
            height: self.height.to_le(),
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuSetScanout {
    pub hdr: VirtioGpuCtrlHeader,
    pub r: VirtioGpuRect,
    pub scanout_id: u32,
    pub resource_id: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuResourceFlush {
    pub hdr: VirtioGpuCtrlHeader,
    pub r: VirtioGpuRect,
    pub resource_id: u32,
    pub padding: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuMemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct AttachCmd {
    hdr: VirtioGpuCtrlHeader,
    resource_id: u32,
    nr_entries: u32,
    entry: VirtioGpuMemEntry,
}

// ---------------------------------------------------------------------------
// Register layout for virtio-pci common cfg (VirtIO 1.0 modern mode, QEMU)
//   le32 device_feature_select;    0x00
//   le32 device_feature;           0x04
//   le32 guest_feature_select;     0x08
//   le32 guest_feature;            0x0c
//   le16 msix_config;              0x10
//   le16 num_queues;               0x12
//   le8  device_status;            0x14
//   le8  config_generation;        0x15
//   le16 queue_select;             0x16
//   le16 queue_size;               0x18
//   le16 queue_msix_vector;        0x1a
//   le16 queue_enable;             0x1c
//   le16 queue_notify_off;         0x1e
//   le64 queue_desc;               0x20
//   le64 queue_avail;               0x28
//   le64 queue_used;               0x30
// ---------------------------------------------------------------------------
// The BAR is accessed as *mut u32 with pointer arithmetic:
// ptr.add(byte_offset >> 2) reads the dword at that byte offset.

pub struct VirtioGpu {
    device: PciDevice,
    common_bar: u8,
    type5_bar: u8,
    common_virt_absolute: *mut u32,
    notify_base: *mut u32,
    pub resource_id: u32,
    desc_table: *mut VringDesc,
    avail_ring: *mut VringAvail,
    used_ring: *mut VringUsed,
    next_desc: u16,
    /// Heap-allocated command buffer (won't move after allocation)
    cmd_buf: *mut u8,
    /// Physical address of the command buffer
    cmd_buf_phys: u64,
    cmd_buf_len: u32,
    notify_off_multiplier: u32,
    queue_notify_offs: [u16; 2],
    /// BAR to use for Type 5 access to Common Config registers
    common_bar_for_type5: u8,
}

unsafe impl Send for VirtioGpu {}
unsafe impl Sync for VirtioGpu {}

#[derive(Debug)]
pub enum VirtioGpuError {
    DeviceNotReady,
    CommandFailed,
    MappingFailed,
    InvalidDevice,
}

impl VirtioGpu {
    /// Read a 32-bit dword from the common config space.
    /// Tries Type 5 access first, falls back to direct memory-mapped access.
    fn read_common_cfg(&self, offset: u32, width: u32) -> Option<u32> {
        // Try Type 5 access via PCI Configuration Access Capability
        if let Some(val) = read_virtio_reg_via_pci_cfg(&self.device, self.common_bar_for_type5, offset, width) {
            return Some(val);
        }
        
        // Fallback: Use direct memory-mapped access via common_virt
        self.read_common_via_direct(offset, width)
    }

    /// Write a 32-bit dword to the common config space.
    /// Tries Type 5 access first, falls back to direct memory-mapped access.
    fn write_common_cfg(&self, offset: u32, value: u32, width: u32) -> Option<()> {
        // Try Type 5 access via PCI Configuration Access Capability
        if let Some(()) = write_virtio_reg_via_pci_cfg(&self.device, self.common_bar_for_type5, offset, value, width) {
            crate::serial::_print(format_args!("[VirtIO-GPU] Type5 write offset={:#x} val={:#x} width={}\n", offset, value, width));
            return Some(());
        }

        // Fallback: Use direct memory-mapped access via common_virt
        crate::serial::_print(format_args!("[VirtIO-GPU] Direct write offset={:#x} val={:#x} width={}\n", offset, value, width));
        self.write_common_via_direct(offset, value, width)
    }
    /// Read a 32-bit dword at the given byte offset (aligned to dword boundary).
    fn r32(&self, bo: usize) -> u32 {
        self.read_common_cfg(bo as u32, 4).expect("Type5 read failed")
    }

    /// Write a 32-bit dword at the given byte offset (aligned to dword boundary).
    fn w32(&self, bo: usize, v: u32) {
        self.write_common_cfg(bo as u32, v, 4).expect("Type5 write failed");
    }

    /// Read a 16-bit register at any byte offset (may be misaligned within a dword).
    fn r16(&self, bo: usize) -> u16 {
        let dword = self.read_common_cfg(bo as u32 & !3, 4).expect("Type5 read failed");
        ((dword >> ((bo & 3) * 8)) & 0xFFFF) as u16
    }

    /// Write a 16-bit register at any byte offset within a dword.
    fn w16(&self, bo: usize, v: u16) {
        let aligned = bo & !3;
        let shift = (bo & 3) * 8;
        let d = self.read_common_cfg(aligned as u32, 4).expect("Type5 read failed");
        self.write_common_cfg(aligned as u32, (d & !(0xFFFFu32 << shift)) | ((v as u32) << shift), 4).expect("Type5 write failed");
    }

    /// Read an 8-bit register at any byte offset within a dword.
    fn r8(&self, bo: usize) -> u8 {
        let dword = self.read_common_cfg((bo & !3) as u32, 4).expect("Type5 read failed");
        ((dword >> ((bo & 3) * 8)) & 0xFF) as u8
    }

    /// Write an 8-bit register at any byte offset within a dword.
    fn w8(&self, bo: usize, v: u8) {
        let aligned = bo & !3;
        let shift = (bo & 3) * 8;
        let d = self.read_common_cfg(aligned as u32, 4).expect("Type5 write failed");
        self.write_common_cfg(aligned as u32, (d & !(0xFFu32 << shift)) | ((v as u32) << shift), 4).expect("Type5 write failed");
    }

    fn status(&self) -> u8 {
        self.r8(0x14)
    }

    fn set_status(&self, s: u8) {
        self.w8(0x14, s);
    }

    fn dev_features(&self) -> u64 {
        self.w32(0x00, 0); // select features 0-31
        let f0 = self.r32(0x04);
        self.w32(0x00, 1); // select features 32-63
        let f1 = self.r32(0x04);
        (f1 as u64) << 32 | (f0 as u64)
    }

    fn set_guest_features(&self, v: u64) {
        self.w32(0x08, 0); // select features 0-31
        self.w32(0x0c, v as u32);
        self.w32(0x08, 1); // select features 32-63
        self.w32(0x0c, (v >> 32) as u32);
    }

    fn set_queue_select(&self, idx: u16) {
        self.write_common_cfg(0x16, idx as u32, 2).expect("Type5 write failed");
    }

    fn write_queue_size(&self, size: u16) {
        self.write_common_cfg(0x18, size as u32, 2).expect("Type5 write failed");
    }

    fn set_queue_enable(&self, en: bool) {
        self.write_common_cfg(0x1c, if en { 1u16 } else { 0u16 } as u32, 2).expect("Type5 write failed");
    }

    fn set_queue_desc(&self, a: u64) {
        crate::serial::_print(format_args!("[VirtIO-GPU] set_queue_desc {:#x}\n", a));
        self.write_common_cfg(0x20, a as u32, 4).expect("Type5 write failed");
        self.write_common_cfg(0x24, (a >> 32) as u32, 4).expect("Type5 write failed");
        
        let read_low = self.read_common_cfg(0x20, 4).unwrap_or(0);
        let read_high = self.read_common_cfg(0x24, 4).unwrap_or(0);
        crate::serial::_print(format_args!("[VirtIO-GPU] Verified queue_desc: wrote {:#x}, read {:#x}:{:#x}\n", a, read_high, read_low));
    }

    fn set_queue_avail(&self, a: u64) {
        crate::serial::_print(format_args!("[VirtIO-GPU] set_queue_avail {:#x}\n", a));
        self.write_common_cfg(0x28, a as u32, 4).expect("Type5 write failed");
        self.write_common_cfg(0x2c, (a >> 32) as u32, 4).expect("Type5 write failed");
        
        let read_low = self.read_common_cfg(0x28, 4).unwrap_or(0);
        let read_high = self.read_common_cfg(0x2c, 4).unwrap_or(0);
        crate::serial::_print(format_args!("[VirtIO-GPU] Verified queue_avail: wrote {:#x}, read {:#x}:{:#x}\n", a, read_high, read_low));
    }

    fn set_queue_used(&self, a: u64) {
        crate::serial::_print(format_args!("[VirtIO-GPU] set_queue_used {:#x}\n", a));
        self.write_common_cfg(0x30, a as u32, 4).expect("Type5 write failed");
        self.write_common_cfg(0x34, (a >> 32) as u32, 4).expect("Type5 write failed");
        
        let read_low = self.read_common_cfg(0x30, 4).unwrap_or(0);
        let read_high = self.read_common_cfg(0x34, 4).unwrap_or(0);
        crate::serial::_print(format_args!("[VirtIO-GPU] Verified queue_used: wrote {:#x}, read {:#x}:{:#x}\n", a, read_high, read_low));
    }

/// Read from the common config space via direct memory-mapped access.
/// Fallback when Type 5 access fails.
fn read_common_via_direct(&self, offset: u32, width: u32) -> Option<u32> {
    let ptr = unsafe { self.common_virt_absolute.offset((offset as usize / 4) as isize) };
    match width {
        1 => Some(unsafe { core::ptr::read_volatile(ptr as *const u8) as u32 }),
        2 => Some(unsafe { core::ptr::read_volatile(ptr as *const u16) as u32 }),
        4 => Some(unsafe { core::ptr::read_volatile(ptr) }),
        _ => None,
    }
}

/// Write to the common config space via direct memory-mapped access.
/// Fallback when Type 5 access fails.
fn write_common_via_direct(&self, offset: u32, value: u32, width: u32) -> Option<()> {
    let ptr = unsafe { self.common_virt_absolute.offset((offset as usize / 4) as isize) };
    match width {
        1 => unsafe { core::ptr::write_volatile(ptr as *mut u8, value as u8) },
        2 => unsafe { core::ptr::write_volatile(ptr as *mut u16, value as u16) },
        4 => unsafe { core::ptr::write_volatile(ptr, value) },
        _ => return None,
    }
    Some(())
}

pub fn init_virtio_gpu(common_virt: *mut u32, notify_virt: *mut u32, device: PciDevice, common_bar: u8) -> Option<Self> {
    crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: called\n"));
    
    // Set PCI Command Register to enable Memory, I/O, and Bus Master (0x0007)
    // This is necessary for Type 5 PCI Configuration Access to work properly
    crate::hardware::pci::PciConfigSpace::write_config_dword_raw(
        device.bus, device.device, device.function, 4, 0x0007
    );
    
    // Small delay to allow the device to process the register write
    for _ in 0..10000 { core::hint::spin_loop(); }
    
    // Dump capabilities to verify Type 5 (PCI Configuration Access) is present
    crate::virtio::pci::dump_capabilities(&device);
    
    let mut gpu = VirtioGpu::new(common_virt, notify_virt, device, common_bar)?;
    crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: new() completed, calling gpu.init()\n"));

    // Test: Read the first few dwords of Common Config via direct access
    unsafe {
        crate::serial::serial_log(format_args!("[VirtIO] Probing Common Config via direct access:\n"));
        for i in 0..8 {
            let offset = i * 4;
            let val = if offset == 0 { 0x74726976 } else {
                let ptr = (gpu.common_virt_absolute as *mut u32).add(offset / 4);
                core::ptr::read_volatile(ptr)
            };
            crate::serial::serial_log(format_args!("  offset 0x{:02x} = {:#x}\n", offset, val));
        }
    }

    // Test: Read the first few dwords of Common Config via Type5 access
    unsafe {
        crate::serial::serial_log(format_args!("[VirtIO] Type5 Probe Common Config:\n"));
        for i in 0..8 {
            if let Some(val) = read_virtio_reg_via_pci_cfg(&gpu.device, gpu.common_bar_for_type5, i * 4, 4) {
                crate::serial::serial_log(format_args!("  offset 0x{:02x} = {:#x}\n", i*4, val));
            } else {
                crate::serial::serial_log(format_args!("  offset 0x{:02x} = READ FAILED\n", i*4));
            }
        }
    }

    // Allocate queue memory BEFORE calling init() to ensure queues are ready
    // when FEATURES_OK is set. This prevents the device from rejecting FEATURES_OK.
    let desc_sz = QUEUE_SIZE as usize * core::mem::size_of::<VringDesc>();
    let (dp, dphys) = Self::alloc_queue_mem(desc_sz);
    let (ap, aphys) = Self::alloc_queue_mem(core::mem::size_of::<VringAvail>());
    let (up, uphys) = Self::alloc_queue_mem(core::mem::size_of::<VringUsed>());
    crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: about to setup_queue\n"));
    gpu.setup_queue(
        0,
        dp as *mut VringDesc,
        dphys,
        ap as *mut VringAvail,
        aphys,
        up as *mut VringUsed,
        uphys,
    );
    crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: setup_queue completed\n"));
    
    // Now call init() - the device will see that queues are configured and should
    // accept FEATURES_OK status.
    if gpu.init().is_ok() {
        crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: gpu.init() succeeded\n"));
        gpu.set_status(VIRTIO_STATUS_DRIVER_OK as u8); // Final signal
        crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: status set to DRIVER_OK\n"));
        Some(gpu)
    } else {
        None
    }
}

pub fn new(common_virt_base: *mut u32, notify_virt_base: *mut u32, device: PciDevice, common_bar: u8) -> Option<Self> {
    // Allocate command buffer from the physical frame allocator so that
    // the physical address is real, not a heap-virtual-to-physical fudge.
    let off = crate::common::memory::get_physical_memory_offset() as u64;
    let cmd_buf_size = 4096; // one whole page
    let cmd_buf_phys = crate::page_table::constants::get_frame_allocator_mut()
        .allocate_contiguous_frames(1)
        .expect("alloc_queue_mem: failed to allocate contiguous frames")
        as u64;
let cmd_buf = (cmd_buf_phys + off) as *mut u8;
unsafe {
    core::ptr::write_bytes(cmd_buf, 0, 4096);
}

    // Find the capabilities
    let caps = get_virtio_caps(&device);
    let type5_cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_PCI_CFG)?;

    // Find the Common Config capability
    let common_cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_COMMON_CFG)?;
    let common_bar_for_type5 = common_cap.bar;
    // Compute the absolute address of the common config space within the BAR
    let common_virt_absolute = unsafe { (common_virt_base as *mut u8).add(common_cap.offset as usize) } as *mut u32;

    // Find the Notify capability
    let notify_cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG)?;
    let notify_base = unsafe { (notify_virt_base as *mut u8).add(notify_cap.offset as usize) } as *mut u32;

    Some(Self {
        device,
        common_bar,
        type5_bar: type5_cap.bar,
        common_virt_absolute,
        notify_base,
        resource_id: 1,
        desc_table: core::ptr::null_mut(),
        avail_ring: core::ptr::null_mut(),
        used_ring: core::ptr::null_mut(),
        next_desc: 0,
        cmd_buf,
        cmd_buf_phys,
        cmd_buf_len: 4096,
        notify_off_multiplier: notify_cap.notify_off_multiplier,
        queue_notify_offs: [0; 2],
        // Store the BAR to use for Type 5 access to Common Config
        common_bar_for_type5,
    })
}

        pub fn init(&mut self) -> Result<(), VirtioGpuError> {
        crate::serial::_print(format_args!("[VirtIO-GPU] init: entered\n"));

        // === 1. Reset ===
        self.set_status(0);
        for _ in 0..100_000 { core::hint::spin_loop(); }
        crate::serial::_print(format_args!("[VirtIO-GPU] After reset, status = {:#x}\n", self.status()));

        // === 2. Acknowledge + Driver ===
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE as u8);
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER) as u8);
        for _ in 0..20_000 { core::hint::spin_loop(); }

        // === 3. Feature Negotiation ===
        crate::serial::_print(format_args!("[VirtIO-GPU] Negotiating features...\n"));
        let feats = self.dev_features();
        crate::serial::_print(format_args!("[VirtIO-GPU] Device features: {:#x}\n", feats));

        // We MUST set VIRTIO_F_VERSION_1 (bit 32)
        // Check if device supports VIRTIO_F_VERSION_1
        if (feats & (1 << 32)) == 0 {
            crate::serial::_print(format_args!("[VirtIO-GPU] ERROR: Device does not support VIRTIO_F_VERSION_1!\n"));
            return Err(VirtioGpuError::DeviceNotReady);
        }

        // We should only acknowledge features the device supports (feats & guest_candidate_features)
        // Here we just keep VERSION_1 and maybe others
        let guest_feats = feats & (1 << 32); 
        crate::serial::_print(format_args!("[VirtIO-GPU] Setting guest features: {:#x}\n", guest_feats));
        self.set_guest_features(guest_feats);

        for _ in 0..20_000 { core::hint::spin_loop(); }

        // === 4. FEATURES_OK ===
        self.set_status(
            (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u8,
        );
        for _ in 0..50_000 { core::hint::spin_loop(); }

        let status = self.status();
        crate::serial::_print(format_args!("[VirtIO-GPU] Status after FEATURES_OK: {:#x}\n", status));

        if (status & VIRTIO_STATUS_FEATURES_OK as u8) == 0 {
            crate::serial::_print(format_args!("[VirtIO-GPU] ERROR: FEATURES_OK rejected by device!\n"));
            return Err(VirtioGpuError::DeviceNotReady);
        }

        crate::serial::_print(format_args!("[VirtIO-GPU] init complete, status: {:#x}\n", status));
        Ok(())
    }

    fn set_queue_msix_vector(&self, vector: u16) {
        self.write_common_cfg(0x1a, vector as u32, 2).expect("Type5 write failed");
    }
    
    pub fn setup_queue(
        &mut self,
        idx: u32,
        desc: *mut VringDesc,
        desc_phys: u64,
        avail: *mut VringAvail,
        avail_phys: u64,
        used: *mut VringUsed,
        used_phys: u64,
    ) {
        crate::serial::_print(format_args!("[VirtIO-GPU] setup_queue idx={} desc={:#x} avail={:#x} used={:#x}\n", idx, desc_phys, avail_phys, used_phys));
        self.desc_table = desc;
        self.avail_ring = avail;
        self.used_ring = used;
        self.set_queue_select(idx as u16);
        
        // Read the queue_notify_off for this queue (offset 0x1E in common config)
        let notify_off = self.r16(0x1e);
        if idx < 2 {
            self.queue_notify_offs[idx as usize] = notify_off;
        }
        crate::serial::_print(format_args!("[VirtIO-GPU] queue_notify_off = {}\n", notify_off));
        
        // Read maximum queue size supported by device
        let max_size = self.r16(0x18);
        crate::serial::_print(format_args!("[VirtIO-GPU] device max queue size = {}\n", max_size));
        let q_size = if max_size > 0 && max_size < QUEUE_SIZE { max_size } else { QUEUE_SIZE };

        // Write the chosen queue size to the device.
        self.write_queue_size(q_size);
        crate::serial::_print(format_args!("[VirtIO-GPU] using queue size = {}\n", q_size));
        
        let num_queues = self.r16(0x12);
        crate::serial::_print(format_args!("[VirtIO-GPU] num_queues = {}\n", num_queues));
        
        self.set_queue_msix_vector(0); // Enable MSI-X for this queue
        
        self.set_queue_desc(desc_phys);
        self.set_queue_avail(avail_phys);
        self.set_queue_used(used_phys);
        self.set_queue_enable(true);
        self.set_status(
            (VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK) as u8,
        );
        crate::serial::_print(format_args!("[VirtIO-GPU] queue enabled, status: {:#x}\n", self.status()));
    }

    /// Allocate memory for virtio queues using the physical frame allocator.
    /// The returned memory is zeroed and physically contiguous (single frame).
    /// Returns (virtual_address, physical_address).
    pub fn alloc_queue_mem(size: usize) -> (*mut u8, u64) {
        let pages = (size + 4095) / 4096;
        let phys = crate::page_table::constants::get_frame_allocator_mut()
            .allocate_contiguous_frames(pages)
            .expect("alloc_queue_mem: failed to allocate contiguous frames")
            as u64;
        
        // Assert page alignment
        assert_eq!(phys % 4096, 0, "[VirtIO-GPU] Queue memory not page-aligned!");

        let off = crate::common::memory::get_physical_memory_offset() as u64;
        let virt = phys + off;
        // Zero the memory
        unsafe {
            core::ptr::write_bytes(virt as *mut u8, 0, pages * 4096);
        }
        (virt as *mut u8, phys)
    }

    fn read_used_idx(&self) -> u16 {
        if self.used_ring.is_null() {
            return 0;
        }
        unsafe {
            let idxp = (self.used_ring as *const u8).add(2) as *const u16;
            core::ptr::read_volatile(idxp)
        }
    }

    fn wait_used(&self, last_used_idx: u16) {
        if self.used_ring.is_null() {
            return;
        }
        unsafe {
            let idxp = (self.used_ring as *const u8).add(2) as *const u16;
            // Poll for up to a reasonable number of iterations.
            for i in 0..10_000_000 {
                let current = core::ptr::read_volatile(idxp);
                if current.wrapping_sub(last_used_idx) >= 1 {
                    crate::serial::_print(format_args!("[VirtIO-GPU] used ring updated! current={}, last={}\n", current, last_used_idx));
                    return;
                }
                if i & 0x1FFFF == 0 {
                    crate::serial::_print(format_args!("[Virtio-GPU] waiting... index at {:p} is {}\n", idxp, current));
                    core::hint::spin_loop();
                }
            }
            crate::serial::_print(format_args!(
                "[VirtIO-GPU] WARN: used ring not updated by device (last_idx={})\n", last_used_idx
            ));
        }
    }

    /// Calculate the notify offset for a given queue index.
    fn get_notify_offset(&self, queue_idx: u32) -> usize {
        (self.queue_notify_offs[queue_idx as usize] as u32 * self.notify_off_multiplier) as usize
    }

    /// Submit a command stored in the command buffer, using the device's response buffer.
    /// `cmd_type` is the VIRTIO_GPU_CMD_* constant, `cmd` points into self.cmd_buf.
    unsafe fn submit_raw(&mut self, cmd_offset: u32, cmd_len: u32) {
        if self.desc_table.is_null() || self.avail_ring.is_null() {
            crate::serial::_print(format_args!("[VirtIO-GPU] ERROR: queues not initialized\n"));
            return;
        }

        let d0 = self.next_desc;
        let d1 = (self.next_desc + 1) % QUEUE_SIZE;
        self.next_desc = (self.next_desc + 2) % QUEUE_SIZE;

        let cmd_phys = self.cmd_buf_phys + cmd_offset as u64;
        // Point to a larger response buffer. For now, assume it's at the end of the buffer.
        let resp_phys = self.cmd_buf_phys + self.cmd_buf_len as u64 - 256;

        let e0 = &mut *self.desc_table.add(d0 as usize);
        e0.addr = cmd_phys.to_le();
        e0.len = cmd_len.to_le();
        e0.flags = VRING_DESC_F_NEXT.to_le();
        e0.next = d1.to_le();

        let e1 = &mut *self.desc_table.add(d1 as usize);
        e1.addr = resp_phys.to_le();
        e1.len = 256u32.to_le();
        e1.flags = VRING_DESC_F_WRITE.to_le();
        e1.next = 0u16.to_le();

        // Memory barrier: Ensure descriptor entries are visible before updating the avail ring
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

        let av = &mut *self.avail_ring;
        let idx = av.idx;
        av.ring[(idx as usize) % QUEUE_SIZE as usize] = d0.to_le();
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        av.idx = idx.wrapping_add(1).to_le();
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let notify_off = self.get_notify_offset(0);
        let notify_ptr = (self.notify_base as *mut u8).add(notify_off) as *mut u16;
        unsafe { write_volatile(notify_ptr, 0); }
    }

    pub fn flush(&mut self, w: u32, h: u32) {
        let flush = VirtioGpuResourceFlush {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                padding: 0,
            }.to_le(),
            r: VirtioGpuRect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            resource_id: self.resource_id.to_le(),
            padding: 0,
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &flush as *const VirtioGpuResourceFlush as *const u8,
                self.cmd_buf,
                core::mem::size_of::<VirtioGpuResourceFlush>(),
            );
        }
        let before = self.read_used_idx();
        unsafe {
            self.submit_raw(0, core::mem::size_of::<VirtioGpuResourceFlush>() as u32);
        }
        self.wait_used(before);
    }

    pub fn test_minimal_command(&mut self) {
        crate::serial::_print(format_args!("[VirtIO-GPU] Running minimal command test...\n"));
        let cmd_offset = 0; // Simple offset for test
        let cmd_len = core::mem::size_of::<VirtioGpuCtrlHeader>() as u32;
        
        unsafe {
            core::ptr::write_bytes(self.cmd_buf.add(cmd_offset as usize), 0, cmd_len as usize);
        }

        unsafe {
            let last_used = self.read_used_idx();
            self.submit_raw(cmd_offset, cmd_len);
            
            crate::serial::_print(format_args!("[VirtIO-GPU] Command submitted, waiting for response...\n"));
            self.wait_used(last_used);
        }
    }

    pub fn init_display(&mut self, w: u32, h: u32, fb: u64, sz: u32) {
        crate::serial::_print(format_args!("[VirtIO-GPU] init_display {}x{}\n", w, h));
        
        // 1. GET_DISPLAY_INFO
        let get_display_info = VirtioGpuCtrlHeader {
            type_: VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            padding: 0,
        }.to_le();
        unsafe {
            core::ptr::copy_nonoverlapping(&get_display_info as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuCtrlHeader>());
        }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuCtrlHeader>() as u32); }
        self.wait_used(before);

        // 2. CTX_CREATE
        self.resource_id = 1;
        let ctx_create = VirtioGpuCtrlHeader {
                type_: 0x0200, // VIRTIO_GPU_CMD_CTX_CREATE
                flags: 0,
                fence_id: 0,
                ctx_id: 1,
                padding: 0,
            }.to_le();
        unsafe { core::ptr::copy_nonoverlapping(&ctx_create as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuCtrlHeader>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuCtrlHeader>() as u32); }
        self.wait_used(before);

        // 3. RESOURCE_CREATE_2D
        let create2d = VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                flags: 0,
                fence_id: 0,
                ctx_id: 1, // Associated with context 1
                padding: 0,
            },
            resource_id: self.resource_id,
            format: 2, // Use 2 for B8G8R8X8_UNORM
            width: w,
            height: h,
        }.to_le();
        unsafe { core::ptr::copy_nonoverlapping(&create2d as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuResourceCreate2d>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuResourceCreate2d>() as u32); }
        self.wait_used(before);

        // 4. ATTACH_BACKING
        let attach_cmd = AttachCmd {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                flags: 0,
                fence_id: 0,
                ctx_id: 1,
                padding: 0,
            },
            resource_id: self.resource_id,
            nr_entries: 1,
            entry: VirtioGpuMemEntry {
                addr: fb,
                length: sz,
                padding: 0,
            },
        };
        unsafe { core::ptr::copy_nonoverlapping(&attach_cmd as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<AttachCmd>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<AttachCmd>() as u32); }
        self.wait_used(before);

        // 5. SET_SCANOUT
        let set_scanout = VirtioGpuSetScanout {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_SET_SCANOUT,
                flags: 0,
                fence_id: 0,
                ctx_id: 1,
                padding: 0,
            },
            r: VirtioGpuRect { x: 0, y: 0, width: w, height: h },
            scanout_id: 0,
            resource_id: self.resource_id,
        };
        unsafe { core::ptr::copy_nonoverlapping(&set_scanout as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuSetScanout>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuSetScanout>() as u32); }
        self.wait_used(before);

        // 6. FLUSH
        self.flush(w, h);

        crate::serial::_print(format_args!("[VirtIO-GPU] init_display done\n"));
    }
}