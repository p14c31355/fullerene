//! Driver registry + concrete driver implementations.
//!
//! This module is the **only** place in the kernel that knows about
//! individual driver names.  Every hardware driver lives behind the
//! `Driver` trait and is registered here.  Callers go through
//! `DriverRegistry::match_device` or `poll_all`.
//!
//! # Adding a new driver
//!
//! 1. Write a zero-sized struct implementing `Driver`
//! 2. Add `reg.register("name", Box::new(MyDriver))` to `build_registry()`
//!
//! No other kernel file needs to change.

#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use alloc::boxed::Box;
#[cfg(not(nitrogen_no_usb))]
use core::sync::atomic::AtomicUsize;
#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use spin::Mutex;

#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use genome::block::{BlockDevice, BlockError};
#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use nitrogen::DriverContext;
#[cfg(not(nitrogen_no_usb))]
use nitrogen::driver_api::UsbHostDriver;
#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use nitrogen::driver_api::{Driver, DriverBox};
#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use nitrogen::pci::PciDevice;

// ────────────────────────────────────────────────────────────
//  Re-exports (for external callers such as shell / GUI)
// ────────────────────────────────────────────────────────────

pub use nitrogen::driver_api::DriverRegistry;
#[cfg(not(nitrogen_no_storage))]
use nitrogen::driver_api::StorageDriver;

// ── USB storage state (formerly drivers/usb_storage.rs) ────

// Shared USB context static used by all USB access paths
#[cfg(not(nitrogen_no_usb))]
static USB_CTX: Mutex<Option<nitrogen::usb::context::USBContext>> = Mutex::new(None);

/// Tracks how many USB disks we have registered in the block device registry.
/// Used by `poll_usb` to detect new devices without scanning the registry.
#[cfg(not(nitrogen_no_usb))]
static LAST_REGISTERED_USB_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Access the USB controller context.  Panics if not initialised.
#[cfg(not(nitrogen_no_usb))]
pub fn with_ctx<F, R>(f: F) -> R
where
    F: FnOnce(&mut nitrogen::usb::context::USBContext) -> R,
{
    try_with_ctx(f).expect("USB context not initialized")
}

/// Access the USB controller context when a host driver was discovered.
#[cfg(not(nitrogen_no_usb))]
pub fn try_with_ctx<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut nitrogen::usb::context::USBContext) -> R,
{
    USB_CTX.lock().as_mut().map(f)
}

#[cfg(nitrogen_no_usb)]
/// Dummy USB context for when USB support is not compiled in.
pub struct DummyUsbContext;

#[cfg(nitrogen_no_usb)]
impl DummyUsbContext {
    pub fn is_enabled(&self) -> bool {
        false
    }

    pub fn disks(&self) -> &[DummyUsbDisk] {
        &[]
    }
}

#[cfg(nitrogen_no_usb)]
pub struct DummyUsbDisk {
    pub ctrl_type: &'static str,
    pub dev_addr: u8,
    pub ep_out: u8,
    pub ep_in: u8,
    pub block_size: u32,
    pub total_blocks: u64,
}

#[cfg(nitrogen_no_usb)]
pub fn with_ctx<F, R>(_f: F) -> R
where
    F: FnOnce(&mut DummyUsbContext) -> R,
{
    panic!("USB support not compiled in");
}

#[cfg(nitrogen_no_usb)]
pub fn try_with_ctx<F, R>(_f: F) -> Option<R>
where
    F: FnOnce(&mut DummyUsbContext) -> R,
{
    None
}

// ── SD card state (formerly drivers/sd_card.rs) ────────────

#[cfg(not(nitrogen_no_storage))]
static SD_PROBED: AtomicBool = AtomicBool::new(false);

// NVMe is deliberately exposed through a small software request/completion
// pair first.  The submission queue accepts only initialization requests at
// this stage; data-path commands are not silently accepted before the block
// adapter exists.
#[cfg(not(nitrogen_no_storage))]
const NVME_QUEUE_DEPTH: usize = 8;

#[cfg(not(nitrogen_no_storage))]
#[derive(Debug)]
struct NvmeInitRequest {
    request_id: u64,
    device: PciDevice,
}

#[cfg(not(nitrogen_no_storage))]
#[derive(Debug, Clone, Copy)]
struct NvmeInitCompletion {
    request_id: u64,
    error: Option<nitrogen::DriverError>,
    controller_index: Option<usize>,
}

#[cfg(not(nitrogen_no_storage))]
struct NvmeRing<T> {
    entries: alloc::vec::Vec<Option<T>>,
    head: usize,
    tail: usize,
}

#[cfg(not(nitrogen_no_storage))]
impl<T> NvmeRing<T> {
    fn new() -> Self {
        Self {
            entries: (0..NVME_QUEUE_DEPTH).map(|_| None).collect(),
            head: 0,
            tail: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    fn is_full(&self) -> bool {
        self.tail.wrapping_add(1) % self.entries.len() == self.head
    }

    fn push(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        let slot = self.tail;
        self.entries[slot] = Some(value);
        self.tail = (self.tail + 1) % self.entries.len();
        Ok(())
    }

    fn pop(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let slot = self.head;
        let value = self.entries[slot].take();
        self.head = (self.head + 1) % self.entries.len();
        value
    }
}

#[cfg(not(nitrogen_no_storage))]
struct NvmeQueuePair {
    submission: NvmeRing<NvmeInitRequest>,
    completion: NvmeRing<NvmeInitCompletion>,
}

#[cfg(not(nitrogen_no_storage))]
impl NvmeQueuePair {
    fn new() -> Self {
        Self {
            submission: NvmeRing::new(),
            completion: NvmeRing::new(),
        }
    }

    /// Enqueue the only request type currently supported by the NVMe service.
    fn submit_initialize(&mut self, request: NvmeInitRequest) -> Result<(), NvmeInitRequest> {
        self.submission.push(request)
    }

    fn take_submission(&mut self) -> Option<NvmeInitRequest> {
        self.submission.pop()
    }

    fn complete(&mut self, completion: NvmeInitCompletion) -> Result<(), NvmeInitCompletion> {
        self.completion.push(completion)
    }

    fn take_completion(&mut self, request_id: u64) -> Option<NvmeInitCompletion> {
        let completion = self.completion.pop()?;
        if completion.request_id == request_id {
            Some(completion)
        } else {
            // There is only one synchronous initializer today. Preserve FIFO
            // semantics if this changes by putting an unrelated completion
            // back at the head on the next iteration is not possible, so
            // callers must request completions in submission order.
            None
        }
    }
}

#[cfg(not(nitrogen_no_storage))]
static NVME_QUEUES: spin::Once<Mutex<NvmeQueuePair>> = spin::Once::new();
#[cfg(not(nitrogen_no_storage))]
static NEXT_NVME_REQUEST: AtomicU64 = AtomicU64::new(1);
#[cfg(not(nitrogen_no_storage))]
static NVME_PROCESS_LOCK: spin::Once<Mutex<()>> = spin::Once::new();

#[cfg(not(nitrogen_no_storage))]
fn nvme_queues() -> &'static Mutex<NvmeQueuePair> {
    NVME_QUEUES.call_once(|| Mutex::new(NvmeQueuePair::new()))
}

#[cfg(not(nitrogen_no_storage))]
fn nvme_process_lock() -> &'static Mutex<()> {
    NVME_PROCESS_LOCK.call_once(|| Mutex::new(()))
}

#[cfg(all(test, not(nitrogen_no_storage)))]
mod nvme_queue_tests {
    use super::*;

    #[test]
    fn submission_and_completion_are_independent_fifo_rings() {
        let mut pair = NvmeQueuePair::new();
        let device = PciDevice {
            bus: 0,
            device: 1,
            function: 0,
            handle: 0,
            vendor_id: 0x8086,
            device_id: 0x5845,
            class_code: 0x01,
            subclass: 0x08,
            prog_if: 0,
            header_type: 0,
        };
        pair.submit_initialize(NvmeInitRequest {
            request_id: 7,
            device,
        })
        .unwrap();
        let request = pair.take_submission().unwrap();
        assert_eq!(request.request_id, 7);
        assert!(pair.take_submission().is_none());

        pair.complete(NvmeInitCompletion {
            request_id: 7,
            error: None,
            controller_index: Some(0),
        })
        .unwrap();
        let completion = pair.take_completion(7).unwrap();
        assert_eq!(completion.controller_index, Some(0));
        assert!(pair.take_completion(7).is_none());
    }

    #[test]
    fn submission_ring_rejects_when_full_without_overwriting_entries() {
        let mut ring = NvmeRing::new();
        for request_id in 0..(NVME_QUEUE_DEPTH - 1) as u64 {
            ring.push(NvmeInitRequest {
                request_id,
                device: PciDevice {
                    bus: 0,
                    device: 0,
                    function: 0,
                    handle: 0,
                    vendor_id: 0,
                    device_id: 0,
                    class_code: 0,
                    subclass: 0,
                    prog_if: 0,
                    header_type: 0,
                },
            })
            .unwrap();
        }
        let rejected = ring.push(NvmeInitRequest {
            request_id: 99,
            device: PciDevice {
                bus: 0,
                device: 0,
                function: 0,
                handle: 0,
                vendor_id: 0,
                device_id: 0,
                class_code: 0,
                subclass: 0,
                prog_if: 0,
                header_type: 0,
            },
        });
        assert_eq!(rejected.unwrap_err().request_id, 99);
        assert_eq!(ring.pop().unwrap().request_id, 0);
    }
}

// ────────────────────────────────────────────────────────────
//  Driver implementations
// ────────────────────────────────────────────────────────────

// -- USB storage (formerly usb_storage::init) -----------------

#[cfg(not(nitrogen_no_usb))]
pub struct UsbStorageDriver(AtomicBool);

#[cfg(not(nitrogen_no_usb))]
impl UsbStorageDriver {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }
}

#[cfg(not(nitrogen_no_usb))]
impl Driver for UsbStorageDriver {
    fn pci_class(&self) -> Option<(u8, u8)> {
        Some((0x0C, 0x03)) // USB host controller
    }
    fn probe(&self, _ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        if self
            .0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            DriverBox::UsbHost(Box::new(UsbHostCtl))
        } else {
            DriverBox::None
        }
    }
}

#[cfg(not(nitrogen_no_usb))]
struct UsbHostCtl;

#[cfg(not(nitrogen_no_usb))]
impl UsbHostDriver for UsbHostCtl {
    fn init(&mut self) -> Result<(), nitrogen::DriverError> {
        nitrogen::debug::set_hint_callback(crate::boot_stage::draw_step_hint);
        init_usb_ctx(nitrogen::usb::context::USBContext::new(
            &crate::driver_context_impl::KernelDriverContext,
        ));
        log::info!("USB: service registered; controller activation deferred");
        Ok(())
    }
    fn poll(&self) {
        poll_usb();
    }
}

/// Initialise the USB driver (probe phase — called from Driver).
#[cfg(not(nitrogen_no_usb))]
pub(crate) fn init_usb_ctx(ctx: nitrogen::usb::context::USBContext) {
    *USB_CTX.lock() = Some(ctx);
}

// -- SD card (formerly sd_card::init) -------------------------

#[cfg(not(nitrogen_no_storage))]
pub struct SdCardDriver;

#[cfg(not(nitrogen_no_storage))]
impl Driver for SdCardDriver {
    fn pci_id(&self) -> (u16, u16) {
        (0x10EC, 0x5249)
    }
    fn probe(&self, _ctx: &dyn DriverContext, _device: &PciDevice) -> DriverBox {
        DriverBox::Storage(Box::new(SdCardStorageCtl))
    }
}

#[cfg(not(nitrogen_no_storage))]
struct SdCardStorageCtl;

#[cfg(not(nitrogen_no_storage))]
impl StorageDriver for SdCardStorageCtl {
    fn init(&mut self) -> Result<(), nitrogen::DriverError> {
        crate::boot_stage::draw_boot_label(b"SD CARD");
        nitrogen::storage::rtsx::init(&crate::driver_context_impl::KernelDriverContext);
        if nitrogen::storage::rtsx::is_present() {
            log::info!("SD: RTSX controller found");
        } else {
            log::info!("SD: no RTSX controller found");
        }
        Ok(())
    }
    fn read_blocks(
        &self,
        _lba: u64,
        _count: usize,
        _buf: &mut [u8],
    ) -> Result<(), nitrogen::DriverError> {
        Err(nitrogen::DriverError::NotSupported)
    }
    fn write_blocks(
        &self,
        _lba: u64,
        _count: usize,
        _buf: &[u8],
    ) -> Result<(), nitrogen::DriverError> {
        Err(nitrogen::DriverError::NotSupported)
    }
    fn block_size(&self) -> u32 {
        0
    }
    fn total_blocks(&self) -> u64 {
        0
    }
}

// -- NVMe initialization service --------------------------------

/// Submit one explicit NVMe initialization request through the kernel-owned
/// SQ and return its CQ completion.  This is intentionally not registered in
/// the boot driver attach pipeline: until a real block adapter exists, an
/// NVMe controller is initialized only by an explicit device operation.
#[cfg(not(nitrogen_no_storage))]
pub fn initialize_nvme(device: PciDevice) -> Result<usize, nitrogen::DriverError> {
    let _process_guard = nvme_process_lock().lock();
    let request_id = NEXT_NVME_REQUEST.fetch_add(1, Ordering::Relaxed);
    {
        let mut queues = nvme_queues().lock();
        queues
            .submit_initialize(NvmeInitRequest { request_id, device })
            .map_err(|_| nitrogen::DriverError::Busy)?;
    }

    // Consume one SQ entry and produce exactly one CQ entry. Keeping the
    // queue boundary explicit makes the later interrupt-driven path a
    // replacement for this processing step, not a second API.
    let request = nvme_queues()
        .lock()
        .take_submission()
        .ok_or(nitrogen::DriverError::Io)?;
    let result = nitrogen::storage::nvme::init_device(
        &crate::driver_context_impl::KernelDriverContext,
        request.device,
    );
    let (controller_index, error) = match result {
        Ok(index) => (Some(index), None),
        Err(error) => (None, Some(error)),
    };
    nvme_queues()
        .lock()
        .complete(NvmeInitCompletion {
            request_id: request.request_id,
            error,
            controller_index,
        })
        .map_err(|_| nitrogen::DriverError::Io)?;
    let completion = nvme_queues()
        .lock()
        .take_completion(request_id)
        .ok_or(nitrogen::DriverError::Io)?;
    if let Some(index) = completion.controller_index {
        log::info!("NVMe: initialized nvme{} through SQ/CQ", index);
        Ok(index)
    } else {
        Err(completion.error.unwrap_or(nitrogen::DriverError::Io))
    }
}

// ────────────────────────────────────────────────────────────
//  Registry construction
// ────────────────────────────────────────────────────────────

/// Populate the `DriverRegistry` with every available driver.
pub fn build_registry() -> DriverRegistry {
    let reg = DriverRegistry::new();
    #[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
    let mut reg = reg;
    #[cfg(not(nitrogen_no_storage))]
    reg.register("sd_card", Box::new(SdCardDriver));
    #[cfg(not(nitrogen_no_usb))]
    reg.register("usb_storage", Box::new(UsbStorageDriver::new()));
    // Future: virtio_gpu, iwlwifi, hda, …
    reg
}

// ────────────────────────────────────────────────────────────
//  USB polling & block device registration
// ────────────────────────────────────────────────────────────

/// `UsbBlockDevice` — a `BlockDevice` that talks to a USB mass-storage
/// device via the BOT (Bulk-Only Transport) protocol.  No block I/O
/// happens at construction time — only when `read_sectors`/`write_sectors`
/// is called (i.e. on `mount`).
#[cfg(not(nitrogen_no_usb))]
struct UsbBlockDevice {
    ctrl_type: &'static str,
    ctrl_idx: usize,
    dev_addr: u8,
    ep_out: u8,
    ep_out_mps: u16,
    ep_in: u8,
    ep_in_mps: u16,
    block_size: u32,
    total_blocks: u64,
    tag: u32,
}

#[cfg(not(nitrogen_no_usb))]
unsafe impl Send for UsbBlockDevice {}

#[cfg(not(nitrogen_no_usb))]
impl BlockDevice for UsbBlockDevice {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let lba = u32::try_from(lba).map_err(|_| BlockError::LbaOverflow)?;
        with_ctx(|ctx| {
            ctx.bot_read(
                self.ctrl_type,
                self.ctrl_idx,
                self.dev_addr,
                self.ep_out,
                self.ep_out_mps,
                self.ep_in,
                self.ep_in_mps,
                lba,
                count,
                self.block_size,
                buf,
                &mut self.tag,
            )
        })
        .map_err(|_| BlockError::Device)
    }
    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let lba = u32::try_from(lba).map_err(|_| BlockError::LbaOverflow)?;
        with_ctx(|ctx| {
            ctx.bot_write(
                self.ctrl_type,
                self.ctrl_idx,
                self.dev_addr,
                self.ep_out,
                self.ep_out_mps,
                self.ep_in,
                self.ep_in_mps,
                lba,
                count,
                self.block_size,
                buf,
                &mut self.tag,
            )
        })
        .map_err(|_| BlockError::Device)
    }
    fn sector_size(&self) -> u32 {
        self.block_size
    }
    fn total_sectors(&self) -> u64 {
        self.total_blocks
    }
}

/// Poll an already-active USB controller and register newly-discovered
/// devices in the block device registry (no mount).
///
/// This function is safe to call from the desktop scheduler: it never turns
/// deferred controller registration into BAR MMIO activation.
/// Returns `true` if a new device was registered.
#[cfg(not(nitrogen_no_usb))]
pub fn poll_usb() -> bool {
    let before = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed);
    {
        let mut guard = with_ctx_inner();
        if let Some(ctx) = guard.as_mut() {
            if !ctx.is_enabled() {
                return false;
            }
            crate::boot_stage::draw_step_hint(b"usb_poll");
            ctx.poll();
        }
    }
    register_pending_usb();
    let changed = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed) != before;
    if changed {
        let _ = crate::klog::flush_to_vfs();
    }
    changed
}

#[cfg(nitrogen_no_usb)]
pub fn poll_usb() -> bool {
    false
}

/// Explicitly activate the USB controller service.
///
/// A non-posted MMIO read cannot be made recoverable in software, so callers
/// must never invoke this from boot, rendering, or input-dispatch paths.
#[cfg(not(nitrogen_no_usb))]
fn activate_usb() -> bool {
    // Do not hold USB_CTX across enable(): a broken non-posted MMIO read may
    // never return. Pollers must be able to observe None and keep the GUI
    // responsive while explicit activation is in progress.
    let Some(mut ctx) = ({
        let mut guard = with_ctx_inner();
        guard.take()
    }) else {
        log::warn!("USB: no host-controller service registered");
        return false;
    };

    let result = if ctx.is_enabled() {
        Ok(())
    } else {
        crate::boot_stage::draw_step_hint(b"usb_init");
        ctx.enable()
    };
    *with_ctx_inner() = Some(ctx);

    match result {
        Ok(()) => true,
        Err(e) => {
            log::warn!("USB: enable failed: {:?}", e);
            false
        }
    }
}

/// Initial USB poll with retries, then register block devices.
#[cfg(not(nitrogen_no_usb))]
fn usb_poll_and_register() {
    if !activate_usb() {
        return;
    }
    for i in 0..8 {
        log::info!("USB: poll #{}", i + 1);
        if poll_usb() || LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed) > 0 {
            log::info!("USB: device detected after {} polls", i + 1);
            break;
        }
        nitrogen::timing::delay_ms(250);
    }
}

/// Full USB re-enumeration (clear + re-scan).  Does NOT mount.
#[cfg(not(nitrogen_no_usb))]
pub fn rescan_usb_all() -> bool {
    let names = crate::devfs::list_block_device_names();
    if names
        .iter()
        .any(|name| name.starts_with("usb") && !crate::devfs::block_device_available(name))
    {
        log::warn!("USB: refusing re-enumeration while a USB block device is mounted");
        return false;
    }

    // Only unregister USB-owned block devices (usbN pattern).
    for name in names {
        if name.starts_with("usb") {
            crate::devfs::unregister_block_device(&name);
        }
    }
    LAST_REGISTERED_USB_COUNT.store(0, Ordering::Relaxed);

    init_usb_ctx(nitrogen::usb::context::USBContext::new(
        &crate::driver_context_impl::KernelDriverContext,
    ));
    usb_poll_and_register();
    let registered = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed) > 0;
    let _ = crate::klog::flush_to_vfs();
    registered
}

#[cfg(nitrogen_no_usb)]
pub fn rescan_usb_all() -> bool {
    false
}

/// Access the inner USB context static for poll operations.
#[cfg(not(nitrogen_no_usb))]
fn with_ctx_inner()
-> spin::MutexGuard<'static, Option<nitrogen::usb::context::USBContext>, spin::relax::Spin> {
    USB_CTX.lock()
}

/// Register newly discovered USB disks as block devices under `/dev/usbN`.
///
/// Skips disks already known to the USB context (identified by comparing
/// the number of registered block devices against `ctx.disks().len()`).
#[cfg(not(nitrogen_no_usb))]
fn register_pending_usb() {
    let (disks, _new_count) = {
        let guard = with_ctx_inner();
        let ctx = match guard.as_ref() {
            Some(c) => c,
            None => return,
        };
        let total = ctx.disks().len();
        let already = LAST_REGISTERED_USB_COUNT.load(Ordering::Relaxed);
        if total <= already {
            return;
        }
        (ctx.disks()[already..].to_vec(), total)
    };

    for disk in &disks {
        let idx = LAST_REGISTERED_USB_COUNT.fetch_add(1, Ordering::Relaxed);
        let dev_name = alloc::format!("usb{}", idx);

        let bdev = Box::new(UsbBlockDevice {
            ctrl_type: disk.ctrl_type,
            ctrl_idx: disk.ctrl_idx,
            dev_addr: disk.dev_addr,
            ep_out: disk.ep_out,
            ep_out_mps: disk.ep_out_mps,
            ep_in: disk.ep_in,
            ep_in_mps: disk.ep_in_mps,
            block_size: disk.block_size,
            total_blocks: disk.total_blocks,
            tag: 1,
        });

        crate::klog_fmt!("USB: registered /dev/{} (bulk-only)\n", dev_name);
        crate::devfs::register_block_device(dev_name.clone(), bdev);
    }
    let _ = crate::klog::flush_to_vfs();
}

// ────────────────────────────────────────────────────────────
//  SD card — probe & register (formerly drivers/sd_card.rs)
// ────────────────────────────────────────────────────────────

/// Probe and register an SD card as a block device (no mount).
#[cfg(not(nitrogen_no_storage))]
pub fn sd_probe_and_register() -> bool {
    if crate::devfs::block_device_exists("sd0") {
        crate::klog_fmt!("SD card: /dev/sd0 already registered\n");
        return true;
    }
    if SD_PROBED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        crate::klog_fmt!("SD card: already probed\n");
        return true;
    }

    if !nitrogen::storage::rtsx::is_present() {
        crate::klog_fmt!("SD card: no controller\n");
        SD_PROBED.store(false, Ordering::Release);
        return false;
    }

    match nitrogen::storage::rtsx::init_sd_card() {
        Ok(()) => crate::klog_fmt!("SD card: initialised\n"),
        Err(e) => {
            crate::klog_fmt!("SD card: init failed — {}\n", e);
            SD_PROBED.store(false, Ordering::Release);
            return false;
        }
    }

    let info = match nitrogen::storage::rtsx::sd_card_info() {
        Some(i) => i,
        None => {
            crate::klog_fmt!("SD card: no card info\n");
            SD_PROBED.store(false, Ordering::Release);
            return false;
        }
    };

    crate::klog_fmt!(
        "SD card: {:?} {} sectors {} bytes/sector\n",
        info.card_type,
        info.total_blocks,
        info.block_size
    );

    let bdev = Box::new(SdBlockDev {
        block_size: info.block_size,
        total_blocks: info.total_blocks,
    });

    let dev_name = alloc::format!("sd{}", 0);
    crate::klog_fmt!("SD card: registered /dev/{}\n", dev_name);
    crate::devfs::register_block_device(dev_name.clone(), bdev);
    true
}

#[cfg(nitrogen_no_storage)]
pub fn sd_probe_and_register() -> bool {
    false
}

#[cfg(not(nitrogen_no_storage))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdRescanResult {
    Registered,
    AlreadyRegistered,
    Mounted,
    Unavailable,
}

/// Reconcile the SD device node without destructively reinitializing a live card.
#[cfg(not(nitrogen_no_storage))]
pub fn rescan_sd() -> SdRescanResult {
    if crate::devfs::block_device_exists("sd0") {
        return if crate::devfs::block_device_available("sd0") {
            SdRescanResult::AlreadyRegistered
        } else {
            SdRescanResult::Mounted
        };
    }

    SD_PROBED.store(false, Ordering::Release);
    if sd_probe_and_register() {
        SdRescanResult::Registered
    } else {
        SdRescanResult::Unavailable
    }
}

#[cfg(not(nitrogen_no_storage))]
struct SdBlockDev {
    block_size: u32,
    total_blocks: u64,
}

#[cfg(not(nitrogen_no_storage))]
unsafe impl Send for SdBlockDev {}

#[cfg(not(nitrogen_no_storage))]
impl BlockDevice for SdBlockDev {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let lba = u32::try_from(lba).map_err(|_| BlockError::LbaOverflow)?;
        nitrogen::storage::rtsx::read_sectors(lba, count, buf).map_err(|error| match error {
            nitrogen::DriverError::Busy => BlockError::Busy,
            _ => BlockError::Device,
        })
    }
    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let lba = u32::try_from(lba).map_err(|_| BlockError::LbaOverflow)?;
        nitrogen::storage::rtsx::write_sectors(lba, count, buf).map_err(|error| match error {
            nitrogen::DriverError::Busy => BlockError::Busy,
            _ => BlockError::Device,
        })
    }
    fn sector_size(&self) -> u32 {
        self.block_size
    }
    fn total_sectors(&self) -> u64 {
        self.total_blocks
    }
}

// ────────────────────────────────────────────────────────────
//  DriverRegistry extension — poll_all
// ────────────────────────────────────────────────────────────

/// Run periodic tasks for all registered drivers (USB poll, etc.).
///
/// Call from a background timer tick.  Returns `true` if any
/// driver reported a state change.
pub fn poll_all(_registry: &DriverRegistry) -> bool {
    #[cfg(not(nitrogen_no_usb))]
    return poll_usb();
    #[cfg(nitrogen_no_usb)]
    false
}
