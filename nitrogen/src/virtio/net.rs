//! Virtio-net driver.
//!
//! Implements `bonder::NetDevice` for the virtio-net PCI device.
//!
//! ## Architecture
//!
//! - PCI scanning for devices with class=0x02, subclass=0x00
//! - Two virtqueues: RX (0) and TX (1)
//! - virtio_net_hdr (10 bytes) prepended to each frame
//! - Device config space provides the MAC address
//! - Pre-allocated per-descriptor RX buffers for polling
//!
//! ## Limitations
//!
//! - No mergeable RX buffers, no offload negotiation
//! - Single-descriptor TX and RX
//! - The caller must provide pre-allocated physical memory for queues and buffers

use alloc::boxed::Box;
use core::sync::atomic;

use bonder::{NetDevice, NetError};

use crate::pci::PciDevice;
use crate::virtio::cap::{
    VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    get_virtio_caps,
};

// ── virtio-net constants ──────────────────────────────────────────────

/// virtio-net header preceding each frame (10 bytes, no mergeable buffers).
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    // Only present when VIRTIO_NET_F_MRG_RXBUF is negotiated (adds 2 bytes).
    // We don't negotiate that feature, so this field is omitted here.
}

impl VirtioNetHdr {
    pub const SIZE: usize = 10;
}

/// Per-buffer size for RX descriptors (virtio_net_hdr + MTU).
/// Aligned RX buffer size per descriptor (virtio_net_hdr + MTU, rounded to 2048).
const RX_BUF_SIZE_ALIGNED: usize = 2048;

// ── virtqueue types ───────────────────────────────────────────────────

const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
const VIRTIO_STATUS_DRIVER: u32 = 2;
const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
const VIRTIO_STATUS_FEATURES_OK: u32 = 8;

const VRING_DESC_F_WRITE: u16 = 2;

/// Maximum ring size; the device may offer a smaller value.
const QUEUE_SIZE: u16 = 64;

#[repr(C)]
#[derive(Clone, Copy)]
struct VringDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VringAvail {
    flags: u16,
    idx: u16,
    ring: [u16; QUEUE_SIZE as usize],
    used_event: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VringUsedElem {
    id: u32,
    len: u32,
}

#[repr(C)]
struct VringUsed {
    flags: u16,
    idx: u16,
    ring: [VringUsedElem; QUEUE_SIZE as usize],
    avail_event: u16,
}

// ── VringResources ────────────────────────────────────────────────────

/// Pre-allocated memory for a single virtqueue.
struct VringResources {
    desc: *mut VringDesc,
    avail: *mut VringAvail,
    used: *mut VringUsed,
    desc_phys: u64,
    avail_phys: u64,
    used_phys: u64,
    next_desc: u16,
    actual_size: u16,
    // Keep boxes alive to prevent deallocation
    _desc_box: Box<[VringDesc; QUEUE_SIZE as usize]>,
    _avail_box: Box<VringAvail>,
    _used_box: Box<VringUsed>,
}

// ── VirtioNetDevice ───────────────────────────────────────────────────

/// Virtio-net NIC driver implementing `bonder::NetDevice`.
///
/// The caller must ensure that MMIO BARs are already identity-mapped
/// before invoking `init`. After initialisation the device is ready for
/// `send_frame` / `poll_frame`.
pub struct VirtioNetDevice {
    mac: [u8; 6],
    _pci_dev: PciDevice,
    // TX virtqueue
    tx: VringResources,
    tx_last_used: u16,
    // RX virtqueue
    rx: VringResources,
    rx_last_used: u16,
    // Per-descriptor RX buffers (aligned 2048 bytes each)
    rx_bufs: Box<[[u8; RX_BUF_SIZE_ALIGNED]; QUEUE_SIZE as usize]>,
    rx_bufs_phys: u64,
    // Common/shared MMIO pointers
    common_virt: *mut u32,
    notify_bar_base: *mut u8,
    notify_cap_offset: u32,
    notify_off_multiplier: u32,
    queue_notify_offs: [u16; 2],
    // TX payload buffer
    tx_buf: Box<[u8; 1536]>,
    tx_buf_phys: u64,
}

unsafe impl Send for VirtioNetDevice {}

// ── public API ────────────────────────────────────────────────────────

impl VirtioNetDevice {
    /// Find a virtio-net device on the PCI bus and initialise it.
    ///
    /// Returns `None` if no suitable device is found or initialisation fails.
    pub fn probe_and_init() -> Option<Self> {
        let mut scanner = crate::pci::PciScanner::new();
        let _ = scanner.scan_all_buses();

        for device in scanner.get_devices() {
            if device.class_code == 0x02 && device.subclass == 0x00 {
                if device.vendor_id != 0x1AF4 {
                    continue;
                }
                match device.device_id {
                    0x1000 | 0x1041 => {} // virtio-net (modern or transitional)
                    _ => continue,
                }

                log::info!(
                    "virtio-net: found at {:02x}:{:02x}.{:01x} (vid={:#06x} did={:#06x})",
                    device.bus,
                    device.device,
                    device.function,
                    device.vendor_id,
                    device.device_id,
                );

                match Self::init(device.clone()) {
                    Ok(s) => return Some(s),
                    Err(_) => {
                        log::warn!(
                            "virtio-net: init failed for {:02x}:{:02x}.{:01x}",
                            device.bus,
                            device.device,
                            device.function
                        );
                        continue;
                    }
                }
            }
        }

        log::info!("virtio-net: no device found");
        None
    }

    /// Initialise a previously discovered PCI device.
    fn init(device: PciDevice) -> Result<Self, VirtioNetError> {
        let caps = get_virtio_caps(&device);

        let common_cap = caps
            .iter()
            .find(|c| c.cfg_type == VIRTIO_PCI_CAP_COMMON_CFG)
            .ok_or(VirtioNetError::NoCapability)?;
        let notify_cap = caps
            .iter()
            .find(|c| c.cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG)
            .ok_or(VirtioNetError::NoCapability)?;
        let device_cfg_cap = caps
            .iter()
            .find(|c| c.cfg_type == VIRTIO_PCI_CAP_DEVICE_CFG)
            .ok_or(VirtioNetError::NoCapability)?;

        // Enable memory-space access and bus-mastering
        device.enable_memory_access();

        let common_bar_addr = device
            .read_bar(common_cap.bar)
            .ok_or(VirtioNetError::BarNotAvailable)?;
        let notify_bar_addr = device
            .read_bar(notify_cap.bar)
            .ok_or(VirtioNetError::BarNotAvailable)?;
        let device_cfg_bar_addr = device
            .read_bar(device_cfg_cap.bar)
            .ok_or(VirtioNetError::BarNotAvailable)?;

        let common_virt = (common_bar_addr + common_cap.offset as u64) as *mut u32;
        let notify_bar_base = notify_bar_addr as *mut u8;
        let device_cfg_virt =
            (device_cfg_bar_addr + device_cfg_cap.offset as u64) as *mut u8;

        let notify_cap_offset = notify_cap.offset;
        let notify_off_multiplier = notify_cap.notify_off_multiplier;

        // ── basic init sequence ──────────────────────────────────────
        let common_mut = |off: u32, val: u32| unsafe {
            core::ptr::write_volatile(common_virt.add(off as usize), val);
        };
        let common_get = |off: u32| unsafe {
            core::ptr::read_volatile(common_virt.add(off as usize))
        };

        // Reset
        common_mut(0x14 / 4, 0);
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }
        // ACKNOWLEDGE
        common_mut(0x14 / 4, VIRTIO_STATUS_ACKNOWLEDGE);
        // ACKNOWLEDGE | DRIVER
        common_mut(0x14 / 4, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);

        // Negotiate features: accept only VIRTIO_F_VERSION_1 (bit 32)
        common_mut(0x00 / 4, 0);
        let _ = common_get(0x04 / 4);
        common_mut(0x00 / 4, 1);
        let _ = common_get(0x04 / 4);

        // Guest features: VIRTIO_F_VERSION_1 only
        common_mut(0x08 / 4, 0);
        common_mut(0x0C / 4, 0);
        common_mut(0x08 / 4, 1);
        common_mut(0x0C / 4, 1);

        // FEATURES_OK
        common_mut(
            0x14 / 4,
            VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
        );

        let status = common_get(0x14 / 4);
        if (status & VIRTIO_STATUS_FEATURES_OK) == 0 {
            log::warn!("virtio-net: FEATURES_OK not set by device (status={:#x})", status);
            return Err(VirtioNetError::DeviceNotReady);
        }

        // Read MAC address from device config space
        let mac: [u8; 6] = unsafe {
            [
                core::ptr::read_volatile(device_cfg_virt.add(0)),
                core::ptr::read_volatile(device_cfg_virt.add(1)),
                core::ptr::read_volatile(device_cfg_virt.add(2)),
                core::ptr::read_volatile(device_cfg_virt.add(3)),
                core::ptr::read_volatile(device_cfg_virt.add(4)),
                core::ptr::read_volatile(device_cfg_virt.add(5)),
            ]
        };

        log::info!(
            "virtio-net: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        );

        // ── allocate virtqueue memory ────────────────────────────────

        let tx_desc_box =
            Box::new([VringDesc { addr: 0, len: 0, flags: 0, next: 0 }; QUEUE_SIZE as usize]);
        let tx_avail_box = Box::new(VringAvail {
            flags: 0,
            idx: 0,
            ring: [0; QUEUE_SIZE as usize],
            used_event: 0,
        });
        let tx_used_box = Box::new(VringUsed {
            flags: 0,
            idx: 0,
            ring: [VringUsedElem { id: 0, len: 0 }; QUEUE_SIZE as usize],
            avail_event: 0,
        });

        let rx_desc_box =
            Box::new([VringDesc { addr: 0, len: 0, flags: 0, next: 0 }; QUEUE_SIZE as usize]);
        let rx_avail_box = Box::new(VringAvail {
            flags: 0,
            idx: 0,
            ring: [0; QUEUE_SIZE as usize],
            used_event: 0,
        });
        let rx_used_box = Box::new(VringUsed {
            flags: 0,
            idx: 0,
            ring: [VringUsedElem { id: 0, len: 0 }; QUEUE_SIZE as usize],
            avail_event: 0,
        });

        // Per-descriptor RX buffers (one 2048-byte buffer per descriptor slot)
        let rx_bufs: Box<[[u8; RX_BUF_SIZE_ALIGNED]; QUEUE_SIZE as usize]> =
            Box::new([[0u8; RX_BUF_SIZE_ALIGNED]; QUEUE_SIZE as usize]);
        let rx_bufs_phys = rx_bufs.as_ptr() as *const u8 as u64;

        // Get raw pointers (stable: via &* deref then cast)
        let tx_desc_ptr = &*tx_desc_box as *const VringDesc as *mut VringDesc;
        let tx_avail_ptr = &*tx_avail_box as *const VringAvail as *mut VringAvail;
        let tx_used_ptr = &*tx_used_box as *const VringUsed as *mut VringUsed;
        let rx_desc_ptr = &*rx_desc_box as *const VringDesc as *mut VringDesc;
        let rx_avail_ptr = &*rx_avail_box as *const VringAvail as *mut VringAvail;
        let rx_used_ptr = &*rx_used_box as *const VringUsed as *mut VringUsed;

        let tx_desc_phys = tx_desc_ptr as u64;
        let tx_avail_phys = tx_avail_ptr as u64;
        let tx_used_phys = tx_used_ptr as u64;
        let rx_desc_phys = rx_desc_ptr as u64;
        let rx_avail_phys = rx_avail_ptr as u64;
        let rx_used_phys = rx_used_ptr as u64;

        // TX payload buffer
        let tx_buf: Box<[u8; 1536]> = Box::new([0u8; 1536]);
        let tx_buf_phys = &*tx_buf as *const u8 as u64;

        let mut tx = VringResources {
            desc: tx_desc_ptr,
            avail: tx_avail_ptr,
            used: tx_used_ptr,
            desc_phys: tx_desc_phys,
            avail_phys: tx_avail_phys,
            used_phys: tx_used_phys,
            next_desc: 0,
            actual_size: QUEUE_SIZE,
            _desc_box: tx_desc_box,
            _avail_box: tx_avail_box,
            _used_box: tx_used_box,
        };

        let mut rx = VringResources {
            desc: rx_desc_ptr,
            avail: rx_avail_ptr,
            used: rx_used_ptr,
            desc_phys: rx_desc_phys,
            avail_phys: rx_avail_phys,
            used_phys: rx_used_phys,
            next_desc: 0,
            actual_size: QUEUE_SIZE,
            _desc_box: rx_desc_box,
            _avail_box: rx_avail_box,
            _used_box: rx_used_box,
        };

        let mut queue_notify_offs = [0u16; 2];

        // ── setup queue 0 (RX) ───────────────────────────────────────
        setup_virtqueue(common_virt, 0, &mut rx, &mut queue_notify_offs);

        // ── setup queue 1 (TX) ───────────────────────────────────────
        setup_virtqueue(common_virt, 1, &mut tx, &mut queue_notify_offs);

        // ── DRIVER_OK ────────────────────────────────────────────────
        common_mut(
            0x14 / 4,
            VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK,
        );

        let mut dev = Self {
            mac,
            _pci_dev: device,
            tx,
            tx_last_used: 0,
            rx,
            rx_last_used: 0,
            rx_bufs,
            rx_bufs_phys,
            common_virt,
            notify_bar_base,
            notify_cap_offset,
            notify_off_multiplier,
            queue_notify_offs,
            tx_buf,
            tx_buf_phys,
        };

        // ── fill RX available ring with per-descriptor buffer addrs ──
        dev.fill_rx_ring();

        log::info!("virtio-net: initialised successfully");

        Ok(dev)
    }

    // ── internal helpers ─────────────────────────────────────────────

    fn notify(&self, queue_idx: u16) {
        let off = self.queue_notify_offs[queue_idx as usize] as usize;
        let mult = self.notify_off_multiplier as usize;
        let notify_offset = off * mult;
        let notify_ptr = unsafe {
            self.notify_bar_base
                .add(self.notify_cap_offset as usize)
                .add(notify_offset) as *mut u32
        };
        unsafe {
            core::ptr::write_volatile(notify_ptr, queue_idx as u32);
        }
        // MMIO read-back fence
        unsafe {
            let _ = core::ptr::read_volatile(self.common_virt);
        }
    }

    /// Fill the RX available ring: each descriptor points to its own
    /// pre-allocated 2048-byte physical buffer.
    fn fill_rx_ring(&mut self) {
        let qsz = self.rx.actual_size;
        let avail = unsafe { &mut *self.rx.avail };

        avail.flags = 0u16.to_le();
        avail.idx = 0u16.to_le();

        for i in 0..qsz {
            let desc = unsafe { &mut *self.rx.desc.add(i as usize) };
            let buf_phys = self.rx_bufs_phys + (i as u64) * (RX_BUF_SIZE_ALIGNED as u64);
            desc.addr = buf_phys.to_le();
            desc.len = (RX_BUF_SIZE_ALIGNED as u32).to_le();
            desc.flags = VRING_DESC_F_WRITE.to_le();
            desc.next = 0u16.to_le();
            avail.ring[i as usize] = i.to_le();
        }

        // Mark all descriptors as available
        avail.idx = qsz.to_le();

        atomic::fence(atomic::Ordering::SeqCst);
        self.notify(0);
    }

    fn submit_tx(&mut self, phys: u64, len: u32) {
        let desc_idx = self.tx.next_desc % self.tx.actual_size;
        self.tx.next_desc = self.tx.next_desc.wrapping_add(1);

        let desc = unsafe { &mut *self.tx.desc.add(desc_idx as usize) };
        desc.addr = phys.to_le();
        desc.len = len.to_le();
        desc.flags = 0u16.to_le();
        desc.next = 0u16.to_le();

        let avail = unsafe { &mut *self.tx.avail };
        let idx = u16::from_le(avail.idx);
        let ring_idx = (idx % self.tx.actual_size) as usize;
        avail.ring[ring_idx] = desc_idx.to_le();
        avail.flags = 0u16.to_le();
        atomic::fence(atomic::Ordering::Release);
        avail.idx = idx.wrapping_add(1).to_le();

        atomic::fence(atomic::Ordering::SeqCst);
        self.notify(1);

        // Wait for used ring update
        for _ in 0..10_000_000 {
            let used_idx = unsafe {
                u16::from_le(core::ptr::read_volatile(core::ptr::addr_of!((*self.tx.used).idx)))
            };
            if used_idx != self.tx_last_used {
                self.tx_last_used = used_idx;
                break;
            }
            core::hint::spin_loop();
        }
    }
}

impl NetDevice for VirtioNetDevice {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        let total = VirtioNetHdr::SIZE + frame.len();
        if total > 1536 {
            return Err(NetError::FrameTooLarge);
        }

        // Copy virtio_net_hdr (zeroed) + frame into the TX buffer
        self.tx_buf[..VirtioNetHdr::SIZE].fill(0);
        self.tx_buf[VirtioNetHdr::SIZE..total].copy_from_slice(frame);

        self.submit_tx(self.tx_buf_phys, total as u32)?;

        Ok(())
    }

    fn poll_frame(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        let used = unsafe { &*self.rx.used };
        let used_idx = u16::from_le(unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(used.idx))
        });

        // No new frames
        if used_idx == self.rx_last_used {
            return Ok(None);
        }

        // Consume one entry
        let slot = (self.rx_last_used % self.rx.actual_size) as usize;
        let elem = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(used.ring[slot]))
        };
        let total = u32::from_le(elem.len) as usize;

        // Advance consumer index
        self.rx_last_used = self.rx_last_used.wrapping_add(1);

        if total <= VirtioNetHdr::SIZE {
            // Empty or header-only frame — re-make the descriptor available
            self.refill_rx_slot(slot);
            return Ok(None);
        }

        let data_offset = VirtioNetHdr::SIZE;
        let data_len = total - data_offset;
        let copy_len = data_len.min(buf.len());

        // Copy payload from the RX buffer (skipping virtio_net_hdr)
        let rx_buf_slice = &self.rx_bufs[slot];
        buf[..copy_len].copy_from_slice(&rx_buf_slice[data_offset..data_offset + copy_len]);

        // Re-make this descriptor available for the device
        self.refill_rx_slot(slot);

        Ok(Some(copy_len))
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

// ── VirtioNetDevice: internal helpers (continued) ─────────────────────

impl VirtioNetDevice {
    /// Re-fill a single RX descriptor slot so the device can reuse it.
    fn refill_rx_slot(&mut self, slot: usize) {
        let buf_phys = self.rx_bufs_phys + (slot as u64) * (RX_BUF_SIZE_ALIGNED as u64);
        let desc = unsafe { &mut *self.rx.desc.add(slot) };
        desc.addr = buf_phys.to_le();
        desc.len = (RX_BUF_SIZE_ALIGNED as u32).to_le();
        desc.flags = VRING_DESC_F_WRITE.to_le();
        desc.next = 0u16.to_le();

        // Put this descriptor in the available ring
        let avail = unsafe { &mut *self.rx.avail };
        let idx = u16::from_le(avail.idx);
        let ring_slot = (idx % self.rx.actual_size) as usize;
        avail.ring[ring_slot] = slot as u16;
        atomic::fence(atomic::Ordering::Release);
        avail.idx = idx.wrapping_add(1).to_le();

        atomic::fence(atomic::Ordering::SeqCst);

        // Notify the device that a new descriptor is available
        // Use the free-function version to avoid borrow issues
        notify_device(
            self.common_virt,
            self.notify_bar_base,
            self.notify_cap_offset,
            self.notify_off_multiplier,
            self.queue_notify_offs,
            0,
        );
    }
}

// ── free functions ────────────────────────────────────────────────────

/// Set up a virtqueue on the device.
fn setup_virtqueue(
    common_virt: *mut u32,
    idx: u16,
    res: &mut VringResources,
    queue_notify_offs: &mut [u16; 2],
) {
    let common_mut = |off: u32, val: u32| unsafe {
        core::ptr::write_volatile(common_virt.add(off as usize), val);
    };
    let common_get = |off: u32| unsafe {
        core::ptr::read_volatile(common_virt.add(off as usize))
    };

    // Select queue
    common_mut(0x16 / 4, idx as u32);
    // Flush
    let _ = common_get(0x16 / 4);
    for _ in 0..1_000 {
        core::hint::spin_loop();
    }

    let max_size = common_get(0x18 / 4) as u16;
    let actual = if max_size == 0 { QUEUE_SIZE } else { max_size.min(QUEUE_SIZE) };
    res.actual_size = actual;

    // Record notify offset
    let qnotify = common_get(0x1E / 4) as u16;
    queue_notify_offs[idx as usize] = qnotify;

    // Write queue size
    common_mut(0x18 / 4, actual as u32);

    // MSI-X vector (0 = none)
    common_mut(0x1A / 4, 0);

    // Descriptor table
    common_mut(0x20 / 4, res.desc_phys as u32);
    common_mut(0x24 / 4, (res.desc_phys >> 32) as u32);

    // Available ring
    common_mut(0x28 / 4, res.avail_phys as u32);
    common_mut(0x2C / 4, (res.avail_phys >> 32) as u32);

    // Used ring
    common_mut(0x30 / 4, res.used_phys as u32);
    common_mut(0x34 / 4, (res.used_phys >> 32) as u32);

    // Enable
    common_mut(0x1C / 4, 1);

    atomic::fence(atomic::Ordering::SeqCst);

    log::info!("virtio-net: queue {} setup (size={})", idx, res.actual_size);
}

/// Notify the device (free function for use where `&self` isn't available).
fn notify_device(
    common_virt: *mut u32,
    notify_bar_base: *mut u8,
    notify_cap_offset: u32,
    notify_off_multiplier: u32,
    queue_notify_offs: [u16; 2],
    queue_idx: u16,
) {
    let off = queue_notify_offs[queue_idx as usize] as usize;
    let mult = notify_off_multiplier as usize;
    let notify_offset = off * mult;
    let notify_ptr = unsafe {
        notify_bar_base
            .add(notify_cap_offset as usize)
            .add(notify_offset) as *mut u32
    };
    unsafe {
        core::ptr::write_volatile(notify_ptr, queue_idx as u32);
    }
    // MMIO read-back fence
    unsafe {
        let _ = core::ptr::read_volatile(common_virt);
    }
}

// ── error type ─────────────────────────────────────────────────────────

#[derive(Debug)]
enum VirtioNetError {
    NoCapability,
    BarNotAvailable,
    DeviceNotReady,
}