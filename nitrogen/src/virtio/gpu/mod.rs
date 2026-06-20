//! Virtio-GPU driver for QEMU/virtio-gpu-pci
//!
//! This is a **pure hardware mechanism** driver. It does NOT call any frame
//! allocator or page-table code. The caller provides pre-allocated physical
//! memory and corresponding virtual addresses for command/response buffers
//! and virtqueue memory.
//!
//! Design principle: Nitrogen provides the register-level programming; the
//! caller owns all memory management policy.

pub mod init;
use crate::pci::PciDevice;
use crate::virtio::cap::{
    VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_PCI_CAP_PCI_CFG, get_virtio_caps,
};

pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
pub const VIRTIO_STATUS_DRIVER: u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;

pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

const QUEUE_SIZE: u16 = 64;

/// Max spin-loop iterations for waiting on a VirtIO used-ring update.
///
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
    pub ring: [u16; QUEUE_SIZE as usize],
    pub used_event: u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VringUsedElem {
    pub id: u32,
    pub len: u32,
}
#[repr(C)]
pub struct VringUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VringUsedElem; QUEUE_SIZE as usize],
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
macro_rules! impl_to_le {
    ($ty:ident { $($field:ident),+ $(,)? }) => {
        impl $ty {
            pub fn to_le(self) -> Self { Self { $($field: self.$field.to_le()),+ } }
        }
    };
}
impl_to_le!(VirtioGpuCtrlHeader {
    type_,
    flags,
    fence_id,
    ctx_id,
    padding
});

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
impl_to_le!(VirtioGpuResourceCreate2d {
    hdr,
    resource_id,
    format,
    width,
    height
});

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl_to_le!(VirtioGpuRect {
    x,
    y,
    width,
    height
});

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuSetScanout {
    pub hdr: VirtioGpuCtrlHeader,
    pub r: VirtioGpuRect,
    pub scanout_id: u32,
    pub resource_id: u32,
}
impl_to_le!(VirtioGpuSetScanout {
    hdr,
    r,
    scanout_id,
    resource_id
});
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuResourceFlush {
    pub hdr: VirtioGpuCtrlHeader,
    pub r: VirtioGpuRect,
    pub resource_id: u32,
    pub padding: u32,
}
impl_to_le!(VirtioGpuResourceFlush {
    hdr,
    r,
    resource_id,
    padding
});

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuMemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}
impl_to_le!(VirtioGpuMemEntry {
    addr,
    length,
    padding
});

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AttachCmd {
    pub hdr: VirtioGpuCtrlHeader,
    pub resource_id: u32,
    pub nr_entries: u32,
    pub entry: VirtioGpuMemEntry,
}
impl_to_le!(AttachCmd {
    hdr,
    resource_id,
    nr_entries,
    entry
});

/// VirtIO GPU hardware driver.
///
/// The caller is responsible for allocating all physical memory
/// (command buffer, response buffer, virtqueue descriptor/avail/used)
/// and providing both physical and virtual addresses.
pub struct VirtioGpu {
    #[allow(dead_code)]
    device: PciDevice,
    #[allow(dead_code)]
    common_bar: u8,
    #[allow(dead_code)]
    type5_bar: u8,
    common_virt_absolute: *mut u32,
    /// Base address of the notify BAR (raw BAR start, NOT including notify_cap.offset).
    /// notify address is computed as: notify_bar_base + notify_cap_offset + queue_notify_off
    notify_bar_base: *mut u8,
    /// Offset from notify BAR base to the start of the notify register region,
    /// as specified by VIRTIO_PCI_CAP_NOTIFY_CFG.
    notify_cap_offset: u32,
    pub resource_id: u32,
    desc_table: *mut VringDesc,
    avail_ring: *mut VringAvail,
    used_ring: *mut VringUsed,
    next_desc: u16,
    cmd_buf: *mut u8,
    cmd_buf_phys: u64,
    #[allow(dead_code)]
    cmd_buf_len: u32,
    /// Separately allocated response buffer (physical address).
    #[allow(dead_code)]
    resp_buf: *mut u8,
    resp_buf_phys: u64,
    resp_buf_len: u32,
    notify_off_multiplier: u32,
    queue_notify_offs: [u16; 2],
    #[allow(dead_code)]
    common_bar_for_type5: u8,
}

unsafe impl Send for VirtioGpu {}

#[derive(Debug)]
pub enum VirtioGpuError {
    DeviceNotReady,
    CommandFailed,
    MappingFailed,
    InvalidDevice,
}

/// Initialise a VirtIO GPU from a previously discovered PCI device.
///
/// The caller must provide:
/// - `common_virt_base` / `notify_virt_base`: virtual addresses of the MMIO BARs
///   (already mapped into the kernel's address space).
/// - `device` : the PCI device found during scanning.
/// - `common_bar` : the BAR index holding the common config capability.
/// - Pre-allocated buffers for commands and responses.
pub fn init_virtio_gpu(
    common_virt_base: *mut u32,
    notify_virt_base: *mut u32,
    device: PciDevice,
    common_bar: u8,
    cmd_buf: *mut u8,
    cmd_buf_phys: u64,
    cmd_buf_len: u32,
    resp_buf: *mut u8,
    resp_buf_phys: u64,
    resp_buf_len: u32,
) -> Option<VirtioGpu> {
    let mut gpu = VirtioGpu::new(
        common_virt_base,
        notify_virt_base,
        device,
        common_bar,
        cmd_buf,
        cmd_buf_phys,
        cmd_buf_len,
        resp_buf,
        resp_buf_phys,
        resp_buf_len,
    )?;
    match gpu.init() {
        Ok(_) => Some(gpu),
        Err(e) => {
            log::info!("[VirtIO-GPU] gpu.init() failed with error: {:?}", e);
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
        self.read_common_cfg(bo as u32, 4).unwrap_or(0)
    }
    fn w32(&self, bo: usize, v: u32) {
        self.write_common_cfg(bo as u32, v, 4);
    }
    fn r16(&self, bo: usize) -> u16 {
        self.read_common_cfg(bo as u32, 2).unwrap_or(0) as u16
    }
    fn r8(&self, bo: usize) -> u8 {
        self.read_common_cfg(bo as u32, 1).unwrap_or(0) as u8
    }
    fn w8(&self, bo: usize, v: u8) {
        self.write_common_cfg(bo as u32, v as u32, 1);
    }

    fn status(&self) -> u8 {
        self.r8(0x14)
    }
    fn set_status(&self, s: u8) {
        self.w8(0x14, s);
    }

    fn dev_features(&self) -> u64 {
        log::info!(
            "[VirtIO-GPU] dev_features: common_virt_absolute={:#p}",
            self.common_virt_absolute
        );
        self.write_common_cfg(0x00, 0, 4);
        let f0 = self.read_common_cfg(0x04, 4).unwrap_or(0);
        self.write_common_cfg(0x00, 1, 4);
        let f1 = self.read_common_cfg(0x04, 4).unwrap_or(0);
        log::info!(
            "[VirtIO-GPU] dev_features: f0={:#010x}, f1={:#010x}",
            f0,
            f1
        );
        (f1 as u64) << 32 | (f0 as u64)
    }

    fn set_guest_features(&self, v: u64) {
        self.w32(0x08, 0);
        self.w32(0x0c, v as u32);
        self.w32(0x08, 1);
        self.w32(0x0c, (v >> 32) as u32);
    }

    fn set_queue_select(&self, idx: u16) {
        log::info!("[VirtIO-GPU] set_queue_select: {}", idx);
        self.write_common_cfg(0x16, idx as u32, 2)
            .expect("Direct write failed");
    }
    fn write_queue_size(&self, size: u16) {
        log::info!("[VirtIO-GPU] write_queue_size: {}", size);
        self.write_common_cfg(0x18, size as u32, 2)
            .expect("Direct write failed");
    }
    fn set_queue_enable(&self, en: bool) {
        log::info!("[VirtIO-GPU] set_queue_enable: {}", en);
        self.write_common_cfg(0x1c, if en { 1u16 } else { 0u16 } as u32, 2)
            .expect("Direct write failed");
    }

    fn set_queue_desc(&self, a: u64) {
        log::info!("[VirtIO-GPU] set_queue_desc: {:#x}", a);
        self.write_common_cfg(0x20, a as u32, 4)
            .expect("Direct write failed");
        self.write_common_cfg(0x24, (a >> 32) as u32, 4)
            .expect("Direct write failed");
    }

    fn set_queue_avail(&self, a: u64) {
        log::info!("[VirtIO-GPU] set_queue_avail: {:#x}", a);
        self.write_common_cfg(0x28, a as u32, 4)
            .expect("Direct write failed");
        self.write_common_cfg(0x2c, (a >> 32) as u32, 4)
            .expect("Direct write failed");
    }

    fn set_queue_used(&self, a: u64) {
        log::info!("[VirtIO-GPU] set_queue_used: {:#x}", a);
        self.write_common_cfg(0x30, a as u32, 4)
            .expect("Direct write failed");
        self.write_common_cfg(0x34, (a >> 32) as u32, 4)
            .expect("Direct write failed");
    }

    fn read_common_via_direct(&self, offset: u32, width: u32) -> Option<u32> {
        let ptr = unsafe { (self.common_virt_absolute as *mut u8).add(offset as usize) };
        let val = match width {
            1 => Some(unsafe { core::ptr::read_volatile(ptr as *const u8) as u32 }),
            2 => Some(unsafe { core::ptr::read_volatile(ptr as *const u16) as u32 }),
            4 => Some(unsafe { core::ptr::read_volatile(ptr as *const u32) }),
            _ => None,
        };
        if let Some(v) = val {
            if offset != 0x14 {
                log::info!(
                    "[VirtIO-GPU] read_common: off={:#x}, width={}, val={:#x}",
                    offset,
                    width,
                    v
                );
            }
        }
        val
    }

    fn write_common_via_direct(&self, offset: u32, value: u32, width: u32) -> Option<()> {
        let ptr = unsafe { (self.common_virt_absolute as *mut u8).add(offset as usize) };
        match width {
            1 => unsafe { core::ptr::write_volatile(ptr as *mut u8, value as u8) },
            2 => unsafe { core::ptr::write_volatile(ptr as *mut u16, value as u16) },
            4 => unsafe { core::ptr::write_volatile(ptr as *mut u32, value) },
            _ => return None,
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Some(())
    }

    pub fn new(
        common_virt_base: *mut u32,
        notify_virt_base: *mut u32,
        device: PciDevice,
        common_bar: u8,
        cmd_buf: *mut u8,
        cmd_buf_phys: u64,
        cmd_buf_len: u32,
        resp_buf: *mut u8,
        resp_buf_phys: u64,
        resp_buf_len: u32,
    ) -> Option<Self> {
        let caps = get_virtio_caps(&device);
        let type5_cap = caps.iter().find(|c| c.cfg_type == VIRTIO_PCI_CAP_PCI_CFG)?;
        let common_cap = caps
            .iter()
            .find(|c| c.cfg_type == VIRTIO_PCI_CAP_COMMON_CFG)?;
        let notify_cap = caps
            .iter()
            .find(|c| c.cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG)?;

        // common_virt_absolute = common_virt_base + common_cap.offset
        let common_virt_absolute =
            unsafe { (common_virt_base as *mut u8).add(common_cap.offset as usize) } as *mut u32;

        let n_off = notify_cap.offset;
        let n_mult = notify_cap.notify_off_multiplier;
        log::info!(
            "[VirtIO-GPU] new: notify_cap_offset={:#x}, multiplier={}",
            n_off,
            n_mult
        );

        Some(Self {
            device,
            common_bar,
            type5_bar: type5_cap.bar,
            common_virt_absolute,
            notify_bar_base: notify_virt_base as *mut u8,
            notify_cap_offset: n_off,
            resource_id: 1,
            desc_table: core::ptr::null_mut(),
            avail_ring: core::ptr::null_mut(),
            used_ring: core::ptr::null_mut(),
            next_desc: 0,
            cmd_buf,
            cmd_buf_phys,
            cmd_buf_len,
            resp_buf,
            resp_buf_phys,
            resp_buf_len,
            notify_off_multiplier: n_mult,
            queue_notify_offs: [0; 2],
            common_bar_for_type5: common_cap.bar,
        })
    }

    pub fn init(&mut self) -> Result<(), VirtioGpuError> {
        self.set_status(0);
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
        self.set_status(VIRTIO_STATUS_ACKNOWLEDGE as u8);
        self.set_status((VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER) as u8);
        let feats = self.dev_features();
        log::info!("[VirtIO-GPU] Negotiating features: {:#x}", feats);

        let guest_feats = 1u64 << 32; // VIRTIO_F_VERSION_1
        self.set_guest_features(guest_feats);

        // Write the FEATURES_OK bit
        self.set_status(
            (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u8,
        );

        // The device must report back that features were accepted.
        let status = self.status();
        if (status & VIRTIO_STATUS_FEATURES_OK as u8) == 0 {
            log::info!("[VirtIO-GPU] ERROR: FEATURES_OK not set by device");
            return Err(VirtioGpuError::DeviceNotReady);
        }
        Ok(())
    }

    pub fn complete_init(&mut self) {
        self.set_status(
            (VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK) as u8,
        );
    }

    fn set_queue_msix_vector(&self, vector: u16) {
        self.write_common_cfg(0x1a, vector as u32, 2)
            .expect("Type5 write failed");
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

        unsafe {
            core::ptr::write_bytes(
                desc,
                0,
                QUEUE_SIZE as usize * core::mem::size_of::<VringDesc>(),
            );
            core::ptr::write_bytes(avail as *mut u8, 0, core::mem::size_of::<VringAvail>());
            core::ptr::write_bytes(used as *mut u8, 0, core::mem::size_of::<VringUsed>());
        }

        self.set_queue_select(idx as u16);
        // Flush posted PCI MMIO write: read back queue_select to ensure
        // the write has reached the device before we read queue_size.
        let _ = self.r16(0x16);
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        let mut max_size = self.r16(0x18);
        if max_size == 0 {
            max_size = QUEUE_SIZE;
            log::info!(
                "[VirtIO-GPU] WARNING: device_max is 0, forcing to {}",
                QUEUE_SIZE
            );
        }
        let actual_size = max_size.min(QUEUE_SIZE);

        log::info!(
            "[VirtIO-GPU] setup_queue: idx={}, device_max={}, using={}",
            idx,
            max_size,
            actual_size
        );

        self.queue_notify_offs[idx as usize] = self.r16(0x1e);
        self.write_queue_size(actual_size);
        self.set_queue_msix_vector(0);
        self.set_queue_desc(desc_phys);
        self.set_queue_avail(avail_phys);
        self.set_queue_used(used_phys);
        self.set_queue_enable(true);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.complete_init();
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Verify queue_enable took effect
        let qena_check = self.r16(0x1c);
        log::info!(
            "[VirtIO-GPU] Queue enable verification: qena={:#x}",
            qena_check
        );

        let qsz = self.r16(0x18);
        let qena = self.r16(0x1c);
        let qnoff = self.r16(0x1e);
        let desc_lo = self.r32(0x20);
        let desc_hi = self.r32(0x24);
        let driver_lo = self.r32(0x28);
        let driver_hi = self.r32(0x2c);
        let device_lo = self.r32(0x30);
        let device_hi = self.r32(0x34);
        let qdesc = ((desc_hi as u64) << 32) | desc_lo as u64;
        let qdriver = ((driver_hi as u64) << 32) | driver_lo as u64;
        let qdevice = ((device_hi as u64) << 32) | device_lo as u64;
        log::info!(
            "[VirtIO-GPU] Queue {} verify: qsz={}, qena={:#x}, qnoff={}",
            idx,
            qsz,
            qena,
            qnoff
        );
        log::info!(
            "[VirtIO-GPU]   desc={:#x}, avail={:#x}, used={:#x}",
            qdesc,
            qdriver,
            qdevice
        );
        log::info!(
            "[VirtIO-GPU] Queue {} fully enabled. status={:#x}",
            idx,
            self.status()
        );
    }

    fn read_used_idx(&self) -> u16 {
        if self.used_ring.is_null() {
            log::info!("[VirtIO-GPU] wait_used: used_ring is null!");
            return 0;
        }
        unsafe {
            let idxp = core::ptr::addr_of!((*self.used_ring).idx);
            u16::from_le(core::ptr::read_volatile(idxp))
        }
    }

    fn wait_used(&self, last_used_idx: u16) -> bool {
        if self.used_ring.is_null() {
            log::info!("[VirtIO-GPU] wait_used: used_ring is null!");
            return false;
        }
        let dev_status = self.status();
        if dev_status != 0xf {
            log::info!(
                "[VirtIO-GPU] WARNING: device status={:#x} before wait",
                dev_status
            );
        }
        for i in 0..30_000_000 {
            let current = self.read_used_idx();
            if current.wrapping_sub(last_used_idx) >= 1 {
                log::info!(
                    "[VirtIO-GPU] wait_used OK! before={}, after={}, spins={}",
                    last_used_idx,
                    current,
                    i
                );
                return true;
            }
            if i % 1000000 == 0 {
                let s = self.status();
                log::info!(
                    "[VirtIO-GPU] wait_used poll: used_idx={}, status={:#x}",
                    current,
                    s
                );
            }
            if i % 10000 == 0 {
                core::hint::spin_loop();
            }
        }
        log::info!(
            "[VirtIO-GPU] wait_used TIMEOUT! (used_idx still {}), final status={:#x}",
            self.read_used_idx(),
            self.status()
        );
        false
    }

    fn get_notify_offset(&self, queue_idx: u16) -> usize {
        let off = self.queue_notify_offs[queue_idx as usize] as usize;
        let mult = self.notify_off_multiplier as usize;
        let offset = off * mult;
        log::info!(
            "[VirtIO-GPU] get_notify_offset: q={}, off={}, mult={}, res={:#x}",
            queue_idx,
            off,
            mult,
            offset
        );
        offset
    }

    fn debug_submit_raw(&self, cmd_phys: u64, resp_phys: u64, d0: u16, d1: u16, ring_idx: usize) {
        log::info!(
            "[VirtIO-GPU] submit: cmd_phys={:#x}, resp_phys={:#x}, d0={}, d1={}, ring_idx={}",
            cmd_phys,
            resp_phys,
            d0,
            d1,
            ring_idx
        );
        if !self.desc_table.is_null() {
            let desc0 = unsafe { &*self.desc_table.add(d0 as usize) };
            let desc1 = unsafe { &*self.desc_table.add(d1 as usize) };
            log::info!(
                "[VirtIO-GPU] desc0: addr={:#x}, len={}, flags={:#x}, next={}",
                desc0.addr,
                desc0.len,
                desc0.flags,
                desc0.next
            );
            log::info!(
                "[VirtIO-GPU] desc1: addr={:#x}, len={}, flags={:#x}, next={}",
                desc1.addr,
                desc1.len,
                desc1.flags,
                desc1.next
            );
        }
        if !self.avail_ring.is_null() {
            let av = unsafe { &*self.avail_ring };
            let flags = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(av.flags)) };
            let idx = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(av.idx)) };
            let ring_val =
                unsafe { core::ptr::read_volatile(core::ptr::addr_of!(av.ring[ring_idx])) };
            log::info!(
                "[VirtIO-GPU] avail: flags={:#x}, idx={}, ring[{}]={}",
                flags,
                idx,
                ring_idx,
                ring_val
            );
        }
    }

    /// Memory fence for DMA coherence.
    fn dma_fence(&self) {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }

    unsafe fn submit_raw(&mut self, cmd_offset: u32, cmd_len: u32) {
        unsafe {
            log::info!(
                "[VirtIO-GPU] submit_raw: next_desc={}, cmd_offset={}, cmd_len={}",
                self.next_desc,
                cmd_offset,
                cmd_len
            );
            let d0 = self.next_desc;
            let d1 = (self.next_desc + 1) % QUEUE_SIZE;
            self.next_desc = (self.next_desc + 2) % QUEUE_SIZE;

            let cmd_phys = self.cmd_buf_phys + cmd_offset as u64;
            let resp_phys = self.resp_buf_phys;

            let desc0 = &mut *self.desc_table.add(d0 as usize);
            desc0.addr = cmd_phys.to_le();
            desc0.len = cmd_len.to_le();
            desc0.flags = VRING_DESC_F_NEXT.to_le();
            desc0.next = d1.to_le();

            let desc1 = &mut *self.desc_table.add(d1 as usize);
            desc1.addr = resp_phys.to_le();
            desc1.len = self.resp_buf_len.to_le();
            desc1.flags = VRING_DESC_F_WRITE.to_le();
            desc1.next = 0;

            self.dma_fence();

            let av = &mut *self.avail_ring;
            let idx = u16::from_le(av.idx);
            av.flags = 0u16.to_le();
            let ring_idx = (idx % QUEUE_SIZE) as usize;
            av.ring[ring_idx] = d0.to_le();
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            av.idx = idx.wrapping_add(1).to_le();

            self.debug_submit_raw(cmd_phys, resp_phys, d0, d1, ring_idx);
            self.dma_fence();

            let notify_off = self.get_notify_offset(0);
            let notify_ptr = self
                .notify_bar_base
                .add(self.notify_cap_offset as usize)
                .add(notify_off) as *mut u32;
            core::ptr::write_volatile(notify_ptr, 0u32.to_le());
            core::ptr::read_volatile(self.common_virt_absolute);
        }
    }

    /// Copy `cmd` to cmd_buf, submit it to VirtIO, and wait for completion.
    /// Returns true if the command completed successfully (no timeout).
    unsafe fn submit_gpu_cmd<T>(&mut self, cmd: &T) -> bool {
        unsafe {
            core::ptr::copy_nonoverlapping(
                cmd as *const T as *const u8,
                self.cmd_buf,
                core::mem::size_of::<T>(),
            );
        }
        let before = self.read_used_idx();
        unsafe {
            self.submit_raw(0, core::mem::size_of::<T>() as u32);
        }
        self.wait_used(before)
    }

    pub fn flush(&mut self, w: u32, h: u32) {
        if self.desc_table.is_null() || self.cmd_buf.is_null() {
            return;
        }
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
        }
        .to_le();
        unsafe {
            core::ptr::copy_nonoverlapping(
                &flush as *const _ as *const u8,
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

    pub fn init_display(&mut self, w: u32, h: u32, fb: u64, sz: u32) -> Result<(), VirtioGpuError> {
        if self.desc_table.is_null() {
            log::info!("[VirtIO-GPU] ERROR: Queue not set up!");
            return Err(VirtioGpuError::CommandFailed);
        }

        macro_rules! submit_or_fail {
            ($cmd:expr, $name:literal) => {
                unsafe {
                    if !self.submit_gpu_cmd(&$cmd.to_le()) {
                        log::info!(concat!("[VirtIO-GPU] ERROR: ", $name, " timed out"));
                        return Err(VirtioGpuError::CommandFailed);
                    }
                }
            };
        }

        submit_or_fail!(
            VirtioGpuCtrlHeader {
                type_: VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
                flags: 0,
                fence_id: 0,
                ctx_id: 0,
                padding: 0
            },
            "GET_DISPLAY_INFO"
        );

        self.resource_id = 1;
        submit_or_fail!(
            VirtioGpuResourceCreate2d {
                hdr: VirtioGpuCtrlHeader {
                    type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0
                },
                resource_id: self.resource_id,
                format: 2,
                width: w,
                height: h
            },
            "RESOURCE_CREATE_2D"
        );
        submit_or_fail!(
            AttachCmd {
                hdr: VirtioGpuCtrlHeader {
                    type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0
                },
                resource_id: self.resource_id,
                nr_entries: 1,
                entry: VirtioGpuMemEntry {
                    addr: fb,
                    length: sz,
                    padding: 0
                }
            },
            "RESOURCE_ATTACH_BACKING"
        );
        submit_or_fail!(
            VirtioGpuSetScanout {
                hdr: VirtioGpuCtrlHeader {
                    type_: VIRTIO_GPU_CMD_SET_SCANOUT,
                    flags: 0,
                    fence_id: 0,
                    ctx_id: 0,
                    padding: 0
                },
                r: VirtioGpuRect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h
                },
                scanout_id: 0,
                resource_id: self.resource_id
            },
            "SET_SCANOUT"
        );

        self.flush(w, h);
        Ok(())
    }
}
