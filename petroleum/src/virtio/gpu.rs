//! Virtio-GPU driver for QEMU/virtio-gpu-pci
//!
//! This driver uses a fixed command buffer allocated from the heap
//! to avoid the problem of stack addresses becoming stale when the
//! VirtioGpu struct is moved, or when submit() returns and its
//! stack frame (containing the command) is freed.

use crate::hardware::pci::{PciDevice, PciScanner};
use crate::virtio::pci::find_virtio_capability;
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
    cfg: *mut u32,
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
}

#[derive(Debug)]
pub enum VirtioGpuError {
    DeviceNotReady,
    CommandFailed,
    MappingFailed,
    InvalidDevice,
}

impl VirtioGpu {
    /// Read a 32-bit dword at the given byte offset (aligned to dword boundary).
    /// The common cfg is accessed as u32 array: byte_offset >> 2 gives the dword index.
    fn r32(&self, bo: usize) -> u32 {
        unsafe {
            let ptr = (self.cfg as *mut u8).add(bo) as *mut u32;
            core::ptr::read_volatile(ptr)
        }
    }
    /// Write a 32-bit dword at the given byte offset (aligned to dword boundary).
    fn w32(&self, bo: usize, v: u32) {
        unsafe {
            let ptr = (self.cfg as *mut u8).add(bo) as *mut u32;
            core::ptr::write_volatile(ptr, v)
        }
    }
    /// Read a 16-bit register at any byte offset (may be misaligned within a dword).
    fn r16(&self, bo: usize) -> u16 {
        let dword = self.r32(bo & !3);
        ((dword >> ((bo & 3) * 8)) & 0xFFFF) as u16
    }
    /// Write a 16-bit register at any byte offset within a dword.
    fn w16(&self, bo: usize, v: u16) {
        let aligned = bo & !3;
        let shift = (bo & 3) * 8;
        let d = self.r32(aligned);
        self.w32(aligned, (d & !(0xFFFFu32 << shift)) | ((v as u32) << shift));
    }
    /// Read an 8-bit register at any byte offset within a dword.
    fn r8(&self, bo: usize) -> u8 {
        let dword = self.r32(bo & !3);
        ((dword >> ((bo & 3) * 8)) & 0xFF) as u8
    }
    /// Write an 8-bit register at any byte offset within a dword.
    fn w8(&self, bo: usize, v: u8) {
        let aligned = bo & !3;
        let shift = (bo & 3) * 8;
        let d = self.r32(aligned);
        self.w32(aligned, (d & !(0xFFu32 << shift)) | ((v as u32) << shift));
    }
    fn status(&self) -> u8 {
        self.r8(0x14)
    }
    fn set_status(&self, s: u8) {
        self.w8(0x14, s);
    }
    fn dev_features(&self) -> u32 {
        self.w32(0x00, 0);
        self.r32(0x04)
    }
    fn set_guest_features(&self, v: u32) {
        self.w32(0x08, 0);
        self.w32(0x0c, v);
    }
    fn set_queue_select(&self, idx: u16) {
        self.w16(0x16, idx);
    }
    fn write_queue_size(&self, size: u16) {
        self.w16(0x18, size);
    }
    fn set_queue_enable(&self, en: bool) {
        self.w16(0x1c, if en { 1u16 } else { 0u16 });
    }
    fn set_queue_desc(&self, a: u64) {
        self.w32(0x20, a as u32);
        self.w32(0x24, (a >> 32) as u32);
    }
    fn set_queue_avail(&self, a: u64) {
        self.w32(0x28, a as u32);
        self.w32(0x2c, (a >> 32) as u32);
    }
    fn set_queue_used(&self, a: u64) {
        self.w32(0x30, a as u32);
        self.w32(0x34, (a >> 32) as u32);
    }

pub fn init_virtio_gpu(common_virt: *mut u32, notify_virt: *mut u32) -> Option<Self> {
    crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: called\n"));
    let mut gpu = VirtioGpu::new(common_virt, notify_virt);
    crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: new() completed, calling gpu.init()\n"));

    // Test: Read the first few dwords of Common Config before init
    unsafe {
        crate::serial::serial_log(format_args!("[VirtIO] Probing Common Config:\n"));
        for i in 0..8 {
            let val = core::ptr::read_volatile(common_virt.offset(i as isize));
            crate::serial::serial_log(format_args!("  offset 0x{:02x} = {:#x}\n", i*4, val));
        }
    }

    if gpu.init().is_ok() {
        crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: gpu.init() succeeded\n"));
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
        gpu.set_status(VIRTIO_STATUS_DRIVER_OK as u8); // Final signal
        crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: status set to DRIVER_OK\n"));
        // Read the notify offset multiplier after signaling DRIVER_OK
        unsafe {
            gpu.notify_off_multiplier = core::ptr::read_volatile(gpu.notify_base as *mut u32);
        }
        crate::serial::_print(format_args!("[VirtIO-GPU] init_virtio_gpu: notify_off_multiplier read: {}\n", gpu.notify_off_multiplier));
        Some(gpu)
    } else {
        None
    }
}

    pub fn new(common_virt: *mut u32, notify_virt: *mut u32) -> Self {
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

        Self {
            cfg: common_virt,
            notify_base: notify_virt,
            resource_id: 1,
            desc_table: core::ptr::null_mut(),
            avail_ring: core::ptr::null_mut(),
            used_ring: core::ptr::null_mut(),
            next_desc: 0,
            cmd_buf,
            cmd_buf_phys,
            cmd_buf_len: 4096,
            notify_off_multiplier: 0,
        }
    }

    pub fn init(&mut self) -> Result<(), VirtioGpuError> {
        crate::serial::_print(format_args!("[VirtIO-GPU] init: entered\n"));
        crate::serial::_print(format_args!("[VirtIO-GPU] init: Resetting device...\n"));
        self.set_status(0);
        crate::serial::_print(format_args!("[VirtIO-GPU] init: status set to 0\n"));

        crate::serial::_print(format_args!("[VirtIO-GPU] init: Acknowledge/Driver status...\n"));
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE as u8);
        crate::serial::_print(format_args!("[VirtIO-GPU] init: status set to ACKNOWLEDGE\n"));
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER) as u8);
        crate::serial::_print(format_args!("[VirtIO-GPU] init: status set to ACKNOWLEDGE|DRIVER\n"));

        crate::serial::_print(format_args!("[VirtIO-GPU] init: Negotiating features...\n"));
        let feats = self.dev_features();
        crate::serial::_print(format_args!("[VirtIO-GPU] init: dev_features: {:#x}\n", feats));
        self.set_guest_features(feats);
        crate::serial::_print(format_args!("[VirtIO-GPU] init: guest_features set\n"));
        
        crate::serial::_print(format_args!("[VirtIO-GPU] init: Committing features (FEATURES_OK)...\n"));
        self.set_status(
            (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u8,
        );
        crate::serial::_print(format_args!("[VirtIO-GPU] init: status set to ACKNOWLEDGE|DRIVER|FEATURES_OK\n"));

        // Verify if FEATURES_OK was accepted
        let status = self.status();
        crate::serial::_print(format_args!("[VirtIO-GPU] init: read back status: {:#x}\n", status));
        if (status & VIRTIO_STATUS_FEATURES_OK as u8) == 0 {
            crate::serial::_print(format_args!("[VirtIO-GPU] ERROR: FEATURES_OK not set\n"));
            return Err(VirtioGpuError::DeviceNotReady);
        }
        
        crate::serial::_print(format_args!("[VirtIO-GPU] init complete, status: {:#x}\n", status));
        Ok(())
    }

    fn set_queue_msix_vector(&self, vector: u16) {
        self.w16(0x1a, vector);
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
        
        let size = self.r16(0x18);
        crate::serial::_print(format_args!("[VirtIO-GPU] queue size read at 0x18: {}\n", size));
        
        let q_size = if size > 0 { size } else { QUEUE_SIZE };
        self.write_queue_size(q_size);
        
        self.set_queue_msix_vector(0xFFFF); // Disable MSI-X for this queue
        
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

    /// Submit a command stored in the command buffer, using the device's response buffer.
    /// `cmd_type` is the VIRTIO_GPU_CMD_* constant, `cmd` points into self.cmd_buf.
    fn get_notify_offset(&self, queue_idx: u16) -> usize {
        self.set_queue_select(queue_idx);
        let queue_notify_off = self.r16(0x1e) as usize;
        
        // Read the notify_off_multiplier from Common Config (offset 0x00)
        // This is the correct location according to the VirtIO specification
        let notify_off_multiplier = self.r32(0x00) as usize;
        
        queue_notify_off * notify_off_multiplier
    }

    unsafe fn submit_raw(&mut self, cmd_type: u32, cmd_offset: u32, cmd_len: u32) {
        if self.desc_table.is_null() || self.avail_ring.is_null() {
            crate::serial::_print(format_args!("[VirtIO-GPU] ERROR: queues not initialized\n"));
            return;
        }

        // Write the command header into the command buffer
        let hdr = &mut *(self.cmd_buf.add(cmd_offset as usize) as *mut VirtioGpuCtrlHeader);
        hdr.type_ = cmd_type;
        hdr.flags = 0;
        hdr.fence_id = 0;
        hdr.ctx_id = 0;
        hdr.padding = 0;

        let d0 = self.next_desc;
        let d1 = (self.next_desc + 1) % QUEUE_SIZE;
        self.next_desc = (self.next_desc + 2) % QUEUE_SIZE;

        let cmd_phys = self.cmd_buf_phys + cmd_offset as u64;
        let resp_phys = self.cmd_buf_phys + self.cmd_buf_len as u64 - 64;

        let e0 = &mut *self.desc_table.add(d0 as usize);
        e0.addr = cmd_phys;
        e0.len = cmd_len;
        e0.flags = VRING_DESC_F_NEXT;
        e0.next = d1;

        let e1 = &mut *self.desc_table.add(d1 as usize);
        e1.addr = resp_phys;
        e1.len = 64;
        e1.flags = VRING_DESC_F_WRITE;
        e1.next = 0;

        let av = &mut *self.avail_ring;
        let idx = av.idx;
        av.ring[(idx as usize) % QUEUE_SIZE as usize] = d0;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        av.idx = idx.wrapping_add(1);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let notify_off = self.get_notify_offset(0);
        let notify_ptr = (self.notify_base as *mut u8).add(notify_off) as *mut u16;
        crate::serial::_print(format_args!("[VirtIO-GPU] notifying device at {:p} (offset {}) for cmd={:#x}\n", notify_ptr, notify_off, cmd_type));
        
        // Small delay
        for _ in 0..10000 { core::hint::spin_loop(); }
        write_volatile(notify_ptr, 0);
    }

    pub fn init_display(&mut self, w: u32, h: u32, fb: u64, sz: u32) {
        crate::serial::_print(format_args!("[VirtIO-GPU] init_display {}x{}\n", w, h));
        self.resource_id = 2; // Try different ID

        // Command 1: create 2D resource
        let create2d = VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                padding: 0,
            },
            resource_id: self.resource_id,
            format: 1,
            width: w,
            height: h,
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &create2d as *const VirtioGpuResourceCreate2d as *const u8,
                self.cmd_buf,
                core::mem::size_of::<VirtioGpuResourceCreate2d>(),
            );
        }
        let before = self.read_used_idx();
        unsafe {
            self.submit_raw(
                VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                0,
                core::mem::size_of::<VirtioGpuResourceCreate2d>() as u32,
            );
        }
        self.wait_used(before);

        // Command 2: attach backing
        let attach_cmd = AttachCmd {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
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
        unsafe {
            core::ptr::copy_nonoverlapping(
                &attach_cmd as *const AttachCmd as *const u8,
                self.cmd_buf,
                core::mem::size_of::<AttachCmd>(),
            );
        }
        let before = self.read_used_idx();
        unsafe {
            self.submit_raw(
                VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                0,
                core::mem::size_of::<AttachCmd>() as u32,
            );
        }
        self.wait_used(before);

        // Command 3: set scanout
        let set_scanout = VirtioGpuSetScanout {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_SET_SCANOUT,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                padding: 0,
            },
            r: VirtioGpuRect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            scanout_id: 0,
            resource_id: self.resource_id,
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &set_scanout as *const VirtioGpuSetScanout as *const u8,
                self.cmd_buf,
                core::mem::size_of::<VirtioGpuSetScanout>(),
            );
        }
        let before = self.read_used_idx();
        unsafe {
            self.submit_raw(
                VIRTIO_GPU_CMD_SET_SCANOUT,
                0,
                core::mem::size_of::<VirtioGpuSetScanout>() as u32,
            );
        }
        self.wait_used(before);

        // Command 4: flush
        let flush = VirtioGpuResourceFlush {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                padding: 0,
            },
            r: VirtioGpuRect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            resource_id: self.resource_id,
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
            self.submit_raw(
                VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                0,
                core::mem::size_of::<VirtioGpuResourceFlush>() as u32,
            );
        }
        self.wait_used(before);

        crate::serial::_print(format_args!("[VirtIO-GPU] init_display done\n"));
    }
}