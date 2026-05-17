//! Virtio-GPU driver for QEMU/virtio-gpu-pci
//! Implementation conforming to virtio-gpu spec v1.x

use core::ptr::{read_volatile, write_volatile};
use crate::hardware::pci::{PciDevice, PciScanner};
use crate::virtio::pci::{find_virtio_capability};
pub use crate::virtio::pci::{VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG};


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
    next_desc: u16,
}

#[derive(Debug)]
pub enum VirtioGpuError {
    DeviceNotReady,
    CommandFailed,
    MappingFailed,
    InvalidDevice,
}

impl VirtioGpu {
    pub fn new(device: &PciDevice, common_virt: *mut u32, notify_virt: *mut u32) -> Result<Self, VirtioGpuError> {
        crate::serial::_print(format_args!("[VirtIO-GPU] new: common_virt={:p}, notify_virt={:p}\n", common_virt, notify_virt));
        Ok(Self {
            common_cfg: common_virt,
            notify_base: notify_virt,
            resource_id: 1,
            desc_table: core::ptr::null_mut(),
            avail_ring: core::ptr::null_mut(),
            used_ring: core::ptr::null_mut(),
            next_desc: 0,
        })
    }


    pub fn init_virtio_gpu(common_virt: *mut u32, notify_virt: *mut u32) -> Option<Self> {
        let mut scanner = PciScanner::new();
        if scanner.scan_all_buses().is_err() { return None; }
        let devices = scanner.get_devices();
        
        for device in devices {
            if device.vendor_id == 0x1af4 && device.device_id == 0x1050 {
                crate::serial::_print(format_args!("[VirtIO-GPU] Found device: {:#x}:{:#x}\n", device.vendor_id, device.device_id));
                let mut gpu = VirtioGpu::new(device, common_virt, notify_virt).ok()?;
                if gpu.init().is_ok() {
                    // Allocate VirtQueue memory
                    let desc_size = 1024 * core::mem::size_of::<VringDesc>();
                    let avail_size = 1024 * 2 + 6; // simplified
                    let used_size = 1024 * 8 + 6;  // simplified
                    
                    let (desc_ptr, desc_phys) = Self::alloc_queue_mem(desc_size);
                    let (avail_ptr, avail_phys) = Self::alloc_queue_mem(avail_size);
                    let (used_ptr, used_phys) = Self::alloc_queue_mem(used_size);
                    
                    gpu.setup_queue(0, desc_ptr as *mut VringDesc, avail_ptr as *mut VringAvail, used_ptr as *mut VringUsed, desc_phys, avail_phys, used_phys, 1024);
                    
                    return Some(gpu);
                }
            }
        }
        None
    }

    pub fn alloc_queue_mem(size: usize) -> (*mut u8, u64) {
        // FIXME: Use proper kernel allocator.
        // For now, allocate a dummy buffer. In a real kernel, use `allocate_pages`.
        let layout = unsafe { core::alloc::Layout::from_size_align_unchecked(size, 4096) };
        let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        (ptr, ptr as u64)
    }

    fn read_common(&self, offset: usize) -> u32 {
        unsafe { read_volatile(self.common_cfg.add(offset / 4)) }
    }

    fn write_common(&self, offset: usize, value: u32) {
        unsafe { write_volatile(self.common_cfg.add(offset / 4), value) }
    }

    pub fn init(&mut self) -> Result<(), VirtioGpuError> {
        crate::serial::_print(format_args!("[VirtIO-GPU] init start\n"));
        // Simple handshake: Acknowledge and Driver
        crate::serial::_print(format_args!("[VirtIO-GPU] write status ACK|DRIVER\n"));
        self.write_common(0x14, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

        // Notify Features OK
        crate::serial::_print(format_args!("[VirtIO-GPU] write status ACK|DRIVER|FEATURES_OK\n"));
        self.write_common(0x14, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);

        // Check Features OK bit
        crate::serial::_print(format_args!("[VirtIO-GPU] read status\n"));
        if (self.read_common(0x14) & VIRTIO_STATUS_FEATURES_OK) == 0 {
            crate::serial::_print(format_args!("[VirtIO-GPU] FEATURES_OK not set\n"));
            return Err(VirtioGpuError::DeviceNotReady);
        }

        // Driver OK
        crate::serial::_print(format_args!("[VirtIO-GPU] write status ACK|DRIVER|FEATURES_OK|DRIVER_OK\n"));
        self.write_common(0x14, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK);

        crate::serial::_print(format_args!("[VirtIO-GPU] init complete\n"));
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

    pub fn wait_for_completion(&self, desc_index: u16) {
        if self.used_ring.is_null() { return; }
        unsafe {
            let used = &*self.used_ring;
            // Simple polling: wait until the descriptor index appears in the used ring
            loop {
                for i in 0..1024 {
                    if used.ring[i].id == desc_index as u32 {
                        return;
                    }
                }
                core::hint::spin_loop();
            }
        }
    }

    pub fn submit_command<T: Copy>(&mut self, cmd: &T) -> u16 {
        if self.desc_table.is_null() || self.avail_ring.is_null() { return 0; }
        
        let desc_index = self.next_desc;
        self.next_desc = (self.next_desc + 1) % 1024;

        unsafe {
            let desc = &mut (*self.desc_table.add(desc_index as usize));
            desc.addr = cmd as *const T as u64;
            desc.len = core::mem::size_of::<T>() as u32;
            desc.flags = 0;
            desc.next = 0;

            let avail = &mut *self.avail_ring;
            let idx = (avail.idx % 1024) as usize;
            avail.ring[idx] = desc_index;
            avail.idx = avail.idx.wrapping_add(1);

            write_volatile(self.notify_base, 0);
        }
        desc_index
    }

    pub fn create_resource_2d(&mut self, resource_id: u32, width: u32, height: u32) -> u16 {
        let cmd = VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id, format: 1, width, height,
        };
        self.submit_command(&cmd)
    }

    pub fn set_scanout(&mut self, resource_id: u32, width: u32, height: u32) -> u16 {
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
        self.submit_command(&cmd)
    }

    pub fn attach_backing(&mut self, resource_id: u32, fb_phys: u64, size: u32) -> u16 {
        #[repr(C, packed)]
        #[derive(Debug, Clone, Copy)]
        struct AttachCmd { hdr: VirtioGpuCtrlHeader, resource_id: u32, nr_entries: u32, entry: VirtioGpuMemEntry }
        let cmd = AttachCmd {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id, nr_entries: 1,
            entry: VirtioGpuMemEntry { addr: fb_phys, length: size, padding: 0 },
        };
        self.submit_command(&cmd)
    }

    pub fn flush_full(&mut self, width: u32, height: u32) -> u16 {
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
        self.submit_command(&cmd)
    }

    pub fn init_display(&mut self, width: u32, height: u32, fb_phys: u64, size: u32) {
        crate::serial::_print(format_args!("[VirtIO-GPU] Starting display initialization: {}x{}\n", width, height));
        
        let desc1 = self.create_resource_2d(self.resource_id, width, height);
        self.wait_for_completion(desc1);
        
        let desc2 = self.attach_backing(self.resource_id, fb_phys, size);
        self.wait_for_completion(desc2);
        
        let desc3 = self.set_scanout(self.resource_id, width, height);
        self.wait_for_completion(desc3);
        
        let desc4 = self.flush_full(width, height);
        self.wait_for_completion(desc4);

        crate::serial::_print(format_args!("[VirtIO-GPU] Display initialization complete.\n"));
    }
}
