//! Virtio-GPU driver for QEMU/virtio-gpu-pci

use core::ptr::{read_volatile, write_volatile};
use crate::hardware::pci::{PciDevice, PciScanner};
use crate::virtio::pci::{find_virtio_capability};
pub use crate::virtio::pci::{VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG};

pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
pub const VIRTIO_STATUS_DRIVER: u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringDesc { pub addr: u64, pub len: u32, pub flags: u16, pub next: u16 }
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringAvail { pub flags: u16, pub idx: u16, pub ring: [u16; 1024], pub used_event: u16 }
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringUsed { pub flags: u16, pub idx: u16, pub ring: [u32; 2048], pub avail_event: u16 }

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuCtrlHeader { pub type_: u32, pub flags: u32, pub fence_id: u64, pub ctx_id: u32, pub padding: u32 }

pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuResourceCreate2d {
    pub hdr: VirtioGpuCtrlHeader, pub resource_id: u32, pub format: u32, pub width: u32, pub height: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuRect { pub x: u32, pub y: u32, pub width: u32, pub height: u32 }
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuSetScanout {
    pub hdr: VirtioGpuCtrlHeader, pub r: VirtioGpuRect, pub scanout_id: u32, pub resource_id: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuResourceFlush {
    pub hdr: VirtioGpuCtrlHeader, pub r: VirtioGpuRect, pub resource_id: u32, pub padding: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuMemEntry { pub addr: u64, pub length: u32, pub padding: u32 }
#[repr(C)]
#[derive(Clone, Copy)]
struct AttachCmd {
    hdr: VirtioGpuCtrlHeader, resource_id: u32, nr_entries: u32, entry: VirtioGpuMemEntry,
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
//   le64 queue_avail;              0x28
//   le64 queue_used;               0x30
// ---------------------------------------------------------------------------

pub struct VirtioGpu {
    cfg: *mut u32,               // byte-addressable through dword index
    notify_base: *mut u32,
    pub resource_id: u32,
    desc_table: *mut VringDesc,
    avail_ring: *mut VringAvail,
    used_ring: *mut VringUsed,
    next_desc: u16,
    resp_buf: [u8; 64],
    resp_phys: u64,
}

#[derive(Debug)]
pub enum VirtioGpuError { DeviceNotReady, CommandFailed, MappingFailed, InvalidDevice }

impl VirtioGpu {
    // Access common cfg as u32 dwords: byte_offset >> 2 gives dword index.
    fn r32(&self, bo: usize) -> u32 {
        unsafe { read_volatile(self.cfg.add(bo >> 2)) }
    }
    fn w32(&self, bo: usize, v: u32) {
        unsafe { write_volatile(self.cfg.add(bo >> 2), v) }
    }
    fn status(&self) -> u8 { self.r32(0x14) as u8 }
    fn set_status(&self, s: u8) {
        let d = self.r32(0x14);
        self.w32(0x14, (d & !0xFF) | s as u32);
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
        let d = self.r32(0x16);
        self.w32(0x16, (d & 0xFFFF) | (idx as u32) << 16);
    }
    fn set_queue_enable(&self, en: bool) {
        let d = self.r32(0x1c);
        self.w32(0x1c, (d & 0xFFFF0000) | if en { 1u32 } else { 0u32 });
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
        let mut gpu = VirtioGpu::new(common_virt, notify_virt);
        if gpu.init().is_ok() {
            let desc_sz = 1024 * core::mem::size_of::<VringDesc>();
            let (dp, dphys) = Self::alloc_queue_mem(desc_sz);
            let (ap, aphys) = Self::alloc_queue_mem(core::mem::size_of::<VringAvail>());
            let (up, uphys) = Self::alloc_queue_mem(core::mem::size_of::<VringUsed>());
            gpu.setup_queue(0, dp as *mut VringDesc, dphys, ap as *mut VringAvail, aphys, up as *mut VringUsed, uphys);
            Some(gpu)
        } else {
            None
        }
    }

    pub fn new(common_virt: *mut u32, notify_virt: *mut u32) -> Self {
        let resp = [0u8; 64];
        let off = crate::common::memory::get_physical_memory_offset() as u64;
        Self {
            cfg: common_virt,
            notify_base: notify_virt,
            resource_id: 1,
            desc_table: core::ptr::null_mut(),
            avail_ring: core::ptr::null_mut(),
            used_ring: core::ptr::null_mut(),
            next_desc: 0,
            resp_buf: resp,
            resp_phys: (&resp as *const u8 as u64).wrapping_sub(off),
        }
    }

    pub fn init(&mut self) -> Result<(), VirtioGpuError> {
        self.w32(0x14, 0);
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE as u8);
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER) as u8);
        let feats = self.dev_features();
        self.set_guest_features(feats);
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u8);
        if (self.status() & VIRTIO_STATUS_FEATURES_OK as u8) == 0 {
            return Err(VirtioGpuError::DeviceNotReady);
        }
        Ok(())
    }

    pub fn setup_queue(&mut self, idx: u32,
                       desc: *mut VringDesc, desc_phys: u64,
                       avail: *mut VringAvail, avail_phys: u64,
                       used: *mut VringUsed, used_phys: u64) {
        self.desc_table = desc;
        self.avail_ring = avail;
        self.used_ring = used;
        self.set_queue_select(idx as u16);
        self.set_queue_desc(desc_phys);
        self.set_queue_avail(avail_phys);
        self.set_queue_used(used_phys);
        self.set_queue_enable(true);
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER |
                         VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) as u8);
    }

    pub fn alloc_queue_mem(size: usize) -> (*mut u8, u64) {
        let layout = unsafe { core::alloc::Layout::from_size_align_unchecked(size, 4096) };
        let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        let off = crate::common::memory::get_physical_memory_offset() as u64;
        (ptr, (ptr as u64).wrapping_sub(off))
    }

    fn wait_used(&self, desc_head: u16) {
        if self.used_ring.is_null() { return; }
        unsafe {
            let idxp = (self.used_ring as *const u8).add(2) as *const u16;
            loop {
                if core::ptr::read_volatile(idxp) > desc_head { return; }
                core::hint::spin_loop();
            }
        }
    }

    pub fn init_display(&mut self, w: u32, h: u32, fb: u64, sz: u32) {
        crate::serial::_print(format_args!("[VirtIO-GPU] init_display {}x{}\n", w, h));
        let d1 = self.submit(&VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id: self.resource_id, format: 1, width: w, height: h,
        });
        self.wait_used(d1);
        let d2 = self.submit(&AttachCmd {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id: self.resource_id, nr_entries: 1,
            entry: VirtioGpuMemEntry { addr: fb, length: sz, padding: 0 },
        });
        self.wait_used(d2);
        let d3 = self.submit(&VirtioGpuSetScanout {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_SET_SCANOUT, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            r: VirtioGpuRect { x: 0, y: 0, width: w, height: h },
            scanout_id: 0, resource_id: self.resource_id,
        });
        self.wait_used(d3);
        let d4 = self.submit(&VirtioGpuResourceFlush {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_FLUSH, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            r: VirtioGpuRect { x: 0, y: 0, width: w, height: h },
            resource_id: self.resource_id, padding: 0,
        });
        self.wait_used(d4);
        crate::serial::_print(format_args!("[VirtIO-GPU] init_display done\n"));
    }

    pub fn submit<T: Copy>(&mut self, cmd: &T) -> u16 {
        if self.desc_table.is_null() || self.avail_ring.is_null() { return 0; }
        let d0 = self.next_desc;
        let d1 = (self.next_desc + 1) % 1024;
        self.next_desc = (self.next_desc + 2) % 1024;

        unsafe {
            let off = crate::common::memory::get_physical_memory_offset() as u64;
            let e0 = &mut *self.desc_table.add(d0 as usize);
            e0.addr = (cmd as *const T as u64).wrapping_sub(off);
            e0.len = core::mem::size_of::<T>() as u32;
            e0.flags = VRING_DESC_F_NEXT;
            e0.next = d1;

            let e1 = &mut *self.desc_table.add(d1 as usize);
            e1.addr = self.resp_phys;
            e1.len = 24;
            e1.flags = VRING_DESC_F_WRITE;
            e1.next = 0;

            let av = &mut *self.avail_ring;
            av.ring[(av.idx % 1024) as usize] = d0;
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            av.idx = av.idx.wrapping_add(1);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

            write_volatile(self.notify_base, 0);
        }
        d0
    }
}