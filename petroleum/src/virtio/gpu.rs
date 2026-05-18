//! Virtio-GPU driver for QEMU/virtio-gpu-pci
//!
//! This driver uses a fixed command buffer allocated from the heap
//! to avoid the problem of stack addresses becoming stale when the
//! VirtioGpu struct is moved, or when submit() returns and its
//! stack frame (containing the command) is freed.

use crate::hardware::pci::{PciDevice};
use crate::virtio::pci::{get_virtio_caps, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_PCI_CAP_PCI_CFG};
use core::ptr::{write_volatile};

pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
pub const VIRTIO_STATUS_DRIVER: u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

const QUEUE_SIZE: u16 = 1024;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct VringAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; 1024],
    pub used_event: u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringUsedElem {
    pub id: u32,
    pub len: u32,
}

#[repr(C, packed)]
pub struct VringUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VringUsedElem; 1024],
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

impl VirtioGpuRect {
    pub fn to_le(self) -> Self {
        Self {
            x: self.x.to_le(),
            y: self.y.to_le(),
            width: self.width.to_le(),
            height: self.height.to_le(),
        }
    }
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

pub struct VirtioGpu {
    device: PciDevice,
    common_bar: u8,
    type5_bar: u8,
    common_virt_absolute: *mut u32,
    notify_bar_base: *mut u8,
    notify_cap_offset: u32,
    pub resource_id: u32,
    desc_table: *mut VringDesc,
    avail_ring: *mut VringAvail,
    used_ring: *mut VringUsed,
    next_desc: u16,
    cmd_buf: *mut u8,
    cmd_buf_phys: u64,
    cmd_buf_len: u32,
    notify_off_multiplier: u32,
    queue_notify_offs: [u16; 2],
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

pub fn init_virtio_gpu(
    common_virt: *mut u32,
    notify_virt: *mut u32,
    device: PciDevice,
    common_bar: u8,
) -> Option<VirtioGpu> {
    let mut gpu = VirtioGpu::new(common_virt, notify_virt, device, common_bar)?;
    match gpu.init() {
        Ok(_) => {
            gpu.complete_init();
            Some(gpu)
        },
        Err(e) => {
            crate::serial::_print(format_args!("[VirtIO-GPU] gpu.init() failed with error: {:?}\n", e));
            None
        }
    }
}

impl VirtioGpu {
    fn read_common_cfg(&self, offset: u32, width: u32) -> Option<u32> {
        self.read_common_via_direct(offset, width)
    }

    fn write_common_cfg(&self, offset: u32, value: u32, width: u32) -> Option<()> {
        self.write_common_via_direct(offset, value, width)
    }

    fn r32(&self, bo: usize) -> u32 {
        unsafe { core::ptr::read_volatile(self.common_virt_absolute.add(bo / 4)) }
    }
    fn w32(&self, bo: usize, v: u32) {
        unsafe { core::ptr::write_volatile(self.common_virt_absolute.add(bo / 4), v); }
    }
    fn r16(&self, bo: usize) -> u16 {
        unsafe { core::ptr::read_volatile((self.common_virt_absolute as *mut u8).add(bo) as *const u16) }
    }
    fn w16(&self, bo: usize, v: u16) {
        unsafe { core::ptr::write_volatile((self.common_virt_absolute as *mut u8).add(bo) as *mut u16, v); }
    }
    fn r8(&self, bo: usize) -> u8 {
        unsafe { core::ptr::read_volatile((self.common_virt_absolute as *mut u8).add(bo) as *const u8) }
    }
    fn w8(&self, bo: usize, v: u8) {
        unsafe { core::ptr::write_volatile((self.common_virt_absolute as *mut u8).add(bo) as *mut u8, v); }
    }

    fn status(&self) -> u8 { self.r8(0x14) }
    fn set_status(&self, s: u8) { self.w8(0x14, s); }

    fn dev_features(&self) -> u64 {
        self.write_common_via_direct(0x00, 0, 4).expect("Type5 write failed"); 
        let f0 = self.read_common_via_direct(0x04, 4).expect("Type5 read failed");
        self.write_common_via_direct(0x00, 1, 4).expect("Type5 write failed"); 
        let f1 = self.read_common_via_direct(0x04, 4).expect("Type5 read failed");
        (f1 as u64) << 32 | (f0 as u64)
    }

    fn set_guest_features(&self, v: u64) {
        self.w32(0x08, 0);
        self.w32(0x0c, v as u32);
        self.w32(0x08, 1);
        self.w32(0x0c, (v >> 32) as u32);
    }

    fn set_queue_select(&self, idx: u16) { self.write_common_cfg(0x16, idx as u32, 2).expect("Type5 write failed"); }
    fn write_queue_size(&self, size: u16) { self.write_common_cfg(0x18, size as u32, 2).expect("Type5 write failed"); }
    fn set_queue_enable(&self, en: bool) { self.write_common_cfg(0x1c, if en { 1u16 } else { 0u16 } as u32, 2).expect("Type5 write failed"); }

    fn set_queue_desc(&self, a: u64) {
        self.write_common_cfg(0x20, a as u32, 4).expect("Type5 write failed");
        self.write_common_cfg(0x24, (a >> 32) as u32, 4).expect("Type5 write failed");
    }

    fn set_queue_avail(&self, a: u64) {
        self.write_common_cfg(0x28, a as u32, 4).expect("Type5 write failed");
        self.write_common_cfg(0x2c, (a >> 32) as u32, 4).expect("Type5 write failed");
    }

    fn set_queue_used(&self, a: u64) {
        self.write_common_cfg(0x30, a as u32, 4).expect("Type5 write failed");
        self.write_common_cfg(0x34, (a >> 32) as u32, 4).expect("Type5 write failed");
    }

    fn read_common_via_direct(&self, offset: u32, width: u32) -> Option<u32> {
        let ptr = unsafe { self.common_virt_absolute.offset((offset as usize / 4) as isize) };
        match width {
            1 => Some(unsafe { core::ptr::read_volatile(ptr as *const u8) as u32 }),
            2 => Some(unsafe { core::ptr::read_volatile(ptr as *const u16) as u32 }),
            4 => Some(unsafe { core::ptr::read_volatile(ptr) }),
            _ => None,
        }
    }

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

    pub fn new(common_virt_base: *mut u32, notify_virt_base: *mut u32, device: PciDevice, common_bar: u8) -> Option<Self> {
        let raw_phys = crate::page_table::constants::get_frame_allocator_mut()
            .allocate_contiguous_frames(1)
            .expect("VirtIO-GPU: failed to allocate command buffer");
        let cmd_buf_phys = if raw_phys == 0 { 0x200000 } else { raw_phys as u64 };
        let off = crate::common::memory::get_physical_memory_offset() as u64;
        let cmd_buf = (cmd_buf_phys + off) as *mut u8;
        
        unsafe { core::ptr::write_bytes(cmd_buf, 0, 4096); }

        let caps = get_virtio_caps(&device);
        let type5_cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_PCI_CFG)?;
        let common_cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_COMMON_CFG)?;
        let common_virt_absolute = unsafe { (common_virt_base as *mut u8).add(common_cap.offset as usize) } as *mut u32;

        let notify_cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG)?;
        
        Some(Self {
            device,
            common_bar,
            type5_bar: type5_cap.bar,
            common_virt_absolute,
            notify_bar_base: notify_virt_base as *mut u8,
            notify_cap_offset: notify_cap.offset,
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
            common_bar_for_type5: common_cap.bar,
        })
    }

    pub fn init(&mut self) -> Result<(), VirtioGpuError> {
        self.set_status(0);
        for _ in 0..100_000 { core::hint::spin_loop(); }
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE as u8);
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER) as u8);
        let feats = self.dev_features();
        if (feats & (1 << 32)) == 0 { return Err(VirtioGpuError::DeviceNotReady); }
        self.set_guest_features(feats & (1 << 32));
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u8);
        Ok(())
    }

    pub fn complete_init(&mut self) {
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) as u8);
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
        self.desc_table = desc;
        self.avail_ring = avail;
        self.used_ring = used;
        self.set_queue_select(idx as u16);
        let max = self.r16(0x18);
        let notify_off_reg = self.r16(0x1e);
        crate::serial::_print(format_args!("[VirtIO-GPU] setup_queue: idx={}, max={}, queue_notify_off_reg={}\n", idx, max, notify_off_reg));

        self.queue_notify_offs[idx as usize] = notify_off_reg;
        self.write_queue_size(QUEUE_SIZE);
        self.set_queue_msix_vector(0);
        self.set_queue_desc(desc_phys);
        self.set_queue_avail(avail_phys);
        self.set_queue_used(used_phys);
        
        // Verify the write
        let r_desc_low = self.read_common_cfg(0x20, 4);
        crate::serial::_print(format_args!("[VirtIO-GPU] Verified setup: wrote desc={:#x}, read={:#?}\n", desc_phys, r_desc_low));

        self.set_queue_enable(true);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let mut status = self.r8(0x14);
        status |= VIRTIO_STATUS_FEATURES_OK as u8;
        self.set_status(status);
        status |= VIRTIO_STATUS_DRIVER_OK as u8;
        self.set_status(status);
    }

    pub fn alloc_queue_mem(size: usize) -> (*mut u8, u64) {
        let pages = (size + 4095) / 4096;
        let phys = crate::page_table::constants::get_frame_allocator_mut()
            .allocate_contiguous_frames(pages)
            .expect("VirtIO-GPU: failed to allocate queue memory") as u64;
        let off = crate::common::memory::get_physical_memory_offset() as u64;
        ( (phys + off) as *mut u8, phys )
    }

    fn read_used_idx(&self) -> u16 {
        if self.used_ring.is_null() { return 0; }
        unsafe {
            let idxp = core::ptr::addr_of!((*self.used_ring).idx);
            u16::from_le(core::ptr::read_unaligned(idxp))
        }
    }

    fn wait_used(&self, last_used_idx: u16) {
        if self.used_ring.is_null() { return; }
        unsafe {
            let used = &*self.used_ring;
            for _ in 0..1_000_000 {
                let current = u16::from_le(core::ptr::read_unaligned(core::ptr::addr_of!(used.idx)));
                if current.wrapping_sub(last_used_idx) >= 1 { return; }
                core::hint::spin_loop();
            }
        }
    }

    fn get_notify_offset(&self, queue_idx: u32) -> usize {
        (self.queue_notify_offs[queue_idx as usize] as u32 * self.notify_off_multiplier) as usize
    }

    unsafe fn submit_raw(&mut self, cmd_offset: u32, cmd_len: u32) {
        let d0 = self.next_desc;
        let d1 = (self.next_desc + 1) % QUEUE_SIZE;
        self.next_desc = (self.next_desc + 2) % QUEUE_SIZE;
        
        let cmd_phys = self.cmd_buf_phys + cmd_offset as u64;
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

        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

        let av = &mut *self.avail_ring;
        let idx = u16::from_le(av.idx);
        av.ring[(idx as usize) % QUEUE_SIZE as usize] = d0.to_le();
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        av.idx = idx.wrapping_add(1).to_le();
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let notify_ptr = (self.notify_bar_base as *mut u8).add(self.notify_cap_offset as usize + self.get_notify_offset(0)) as *mut u16;
        write_volatile(notify_ptr, 0);
    }

    pub fn flush(&mut self, w: u32, h: u32) {
        let flush = VirtioGpuResourceFlush {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_FLUSH, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 }.to_le(),
            r: VirtioGpuRect { x: 0, y: 0, width: w, height: h },
            resource_id: self.resource_id.to_le(),
            padding: 0,
        };
        unsafe {
            core::ptr::copy_nonoverlapping(&flush as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuResourceFlush>());
        }
        let before = self.read_used_idx();
        crate::serial::_print(format_args!("[VirtIO-GPU] flush: before={}\n", before));
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuResourceFlush>() as u32); }
        self.wait_used(before);
        crate::serial::_print(format_args!("[VirtIO-GPU] flush: after={}\n", self.read_used_idx()));
    }

    pub fn init_display(&mut self, w: u32, h: u32, fb: u64, sz: u32) {
        let get_display_info = VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_GET_DISPLAY_INFO, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 }.to_le();
        unsafe { core::ptr::copy_nonoverlapping(&get_display_info as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuCtrlHeader>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuCtrlHeader>() as u32); }
        self.wait_used(before);
        
        self.resource_id = 1;
        let create2d = VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id: self.resource_id,
            format: 2,
            width: w,
            height: h,
        }.to_le();
        unsafe { core::ptr::copy_nonoverlapping(&create2d as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuResourceCreate2d>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuResourceCreate2d>() as u32); }
        self.wait_used(before);

        let attach_cmd = AttachCmd {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            resource_id: self.resource_id,
            nr_entries: 1,
            entry: VirtioGpuMemEntry { addr: fb, length: sz, padding: 0 },
        };
        unsafe { core::ptr::copy_nonoverlapping(&attach_cmd as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<AttachCmd>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<AttachCmd>() as u32); }
        self.wait_used(before);

        let set_scanout = VirtioGpuSetScanout {
            hdr: VirtioGpuCtrlHeader { type_: VIRTIO_GPU_CMD_SET_SCANOUT, flags: 0, fence_id: 0, ctx_id: 0, padding: 0 },
            r: VirtioGpuRect { x: 0, y: 0, width: w, height: h },
            scanout_id: 0,
            resource_id: self.resource_id,
        };
        unsafe { core::ptr::copy_nonoverlapping(&set_scanout as *const _ as *const u8, self.cmd_buf, core::mem::size_of::<VirtioGpuSetScanout>()); }
        let before = self.read_used_idx();
        unsafe { self.submit_raw(0, core::mem::size_of::<VirtioGpuSetScanout>() as u32); }
        self.wait_used(before);
        self.flush(w, h);
    }
}
