//! Virtio-GPU driver for QEMU/virtio-gpu-pci
//! Implementation conforming to virtio-gpu spec v1.x

use core::ptr::{read_volatile, write_volatile};
use crate::hardware::pci::PciDevice;
use crate::virtio::pci::{find_virtio_capability, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG};

// --- Virtio Constants ---
pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
pub const VIRTIO_STATUS_DRIVER: u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;
pub const VIRTIO_STATUS_FAILED: u32 = 128;

// --- Virtqueue Structures ---
pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VringDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VringAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; 1024],
    pub used_event: u16,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VringUsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VringUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VringUsedElem; 1024],
    pub avail_event: u16,
}

// --- Virtio-GPU Structures ---
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
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
#[derive(Debug, Clone, Copy)]
pub struct VirtioGpuResourceCreate2d {
    pub hdr: VirtioGpuCtrlHeader,
    pub resource_id: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioGpuResourceAttachBacking {
    pub hdr: VirtioGpuCtrlHeader,
    pub resource_id: u32,
    pub nr_entries: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioGpuMemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioGpuResourceFlush {
    pub hdr: VirtioGpuCtrlHeader,
    pub r: VirtioGpuRect,
    pub resource_id: u32,
    pub padding: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioGpuRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioGpuSetScanout {
    pub hdr: VirtioGpuCtrlHeader,
    pub r: VirtioGpuRect,
    pub scanout_id: u32,
    pub resource_id: u32,
}

pub struct VirtioGpu {
    common_cfg: *mut u32,
    notify_base: *mut u32,
    pub resource_id: u32,
    desc_table: *mut VringDesc,
    avail_ring: *mut VringAvail,
    used_ring: *mut VringUsed,
}

#[derive(Debug)]
pub enum VirtioGpuError {
    DeviceNotReady,
    CommandFailed,
    MappingFailed,
    InvalidDevice,
}

impl VirtioGpu {
    pub fn new(device: &PciDevice) -> Result<Self, VirtioGpuError> {
        let common_cap = find_virtio_capability(device, VIRTIO_PCI_CAP_COMMON_CFG)
            .ok_or(VirtioGpuError::InvalidDevice)?;
        let notify_cap = find_virtio_capability(device, VIRTIO_PCI_CAP_NOTIFY_CFG)
            .ok_or(VirtioGpuError::InvalidDevice)?;

        let bar_phys = device.read_bar(common_cap.bar).ok_or(VirtioGpuError::MappingFailed)?;
        let notify_bar_phys = device.read_bar(notify_cap.bar).ok_or(VirtioGpuError::MappingFailed)?;
        
        let common_virt = (bar_phys + common_cap.offset as u64) as *mut u32;
        let notify_virt = (notify_bar_phys + notify_cap.offset as u64) as *mut u32;

        Ok(Self {
            common_cfg: common_virt,
            notify_base: notify_virt,
            resource_id: 1,
            desc_table: core::ptr::null_mut(),
            avail_ring: core::ptr::null_mut(),
            used_ring: core::ptr::null_mut(),
        })
    }

    fn read_common(&self, offset: usize) -> u32 {
        unsafe { read_volatile(self.common_cfg.add(offset / 4)) }
    }

    fn write_common(&self, offset: usize, value: u32) {
        unsafe { write_volatile(self.common_cfg.add(offset / 4), value) }
    }

    pub fn init(&mut self) -> Result<(), VirtioGpuError> {
        self.write_common(0x14, 0); 
        let mut status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER;
        self.write_common(0x14, status);

        self.write_common(0x0c, 0); 
        status |= VIRTIO_STATUS_FEATURES_OK;
        self.write_common(0x14, status);

        if (self.read_common(0x14) & VIRTIO_STATUS_FEATURES_OK) == 0 {
            self.write_common(0x14, VIRTIO_STATUS_FAILED);
            return Err(VirtioGpuError::DeviceNotReady);
        }

        status |= VIRTIO_STATUS_DRIVER_OK;
        self.write_common(0x14, status);
        Ok(())
    }

    pub fn setup_queue(&mut self, queue_index: u32, desc: *mut VringDesc, avail: *mut VringAvail, used: *mut VringUsed, desc_phys: u64, avail_phys: u64, used_phys: u64, _size: u32) {
        self.desc_table = desc;
        self.avail_ring = avail;
        self.used_ring = used;

        self.write_common(0x16, queue_index as u16 as u32);
        
        self.write_common(0x18, desc_phys as u32);
        self.write_common(0x1c, (desc_phys >> 32) as u32);
        
        self.write_common(0x20, avail_phys as u32);
        self.write_common(0x24, (avail_phys >> 32) as u32);
        
        self.write_common(0x28, used_phys as u32);
        self.write_common(0x2c, (used_phys >> 32) as u32);

        self.write_common(0x2d, 1);
    }

    pub fn submit_command<T: Copy>(&mut self, cmd: &T) {
        if self.desc_table.is_null() || self.avail_ring.is_null() { return; }
        unsafe {
            (*self.desc_table) = VringDesc {
                addr: cmd as *const T as u64,
                len: core::mem::size_of::<T>() as u32,
                flags: 0,
                next: 0,
            };
            let avail = &mut *self.avail_ring;
            avail.ring[(avail.idx % 1024) as usize] = 0;
            avail.idx = avail.idx.wrapping_add(1);

            write_volatile(self.notify_base, 0);
        }
    }

    pub fn create_resource_2d(&mut self, resource_id: u32, width: u32, height: u32) {
        let cmd = VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id, format: 1, width, height,
        };
        self.submit_command(&cmd);
    }

    pub fn set_scanout(&mut self, resource_id: u32, width: u32, height: u32) {
        let cmd = VirtioGpuSetScanout {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_SET_SCANOUT,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                padding: 0,
            },
            r: VirtioGpuRect { x: 0, y: 0, width, height },
            scanout_id: 0,
            resource_id,
        };
        self.submit_command(&cmd);
    }

    pub fn attach_backing(&mut self, resource_id: u32, fb_phys: u64, size: u32) {
        #[repr(C, packed)]
        #[derive(Debug, Clone, Copy)]
        struct AttachCmd { hdr: VirtioGpuCtrlHeader, resource_id: u32, nr_entries: u32, entry: VirtioGpuMemEntry }
        let cmd = AttachCmd {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id, nr_entries: 1,
            entry: VirtioGpuMemEntry { addr: fb_phys, length: size, padding: 0 },
        };
        self.submit_command(&cmd);
    }

    pub fn flush_full(&mut self, width: u32, height: u32) {
        let cmd = VirtioGpuResourceFlush {
            hdr: VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_RESOURCE_FLUSH,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                padding: 0,
            },
            r: VirtioGpuRect { x: 0, y: 0, width, height },
            resource_id: self.resource_id,
            padding: 0,
        };
        self.submit_command(&cmd);
    }
}
