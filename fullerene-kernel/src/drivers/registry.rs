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
#[cfg(not(nitrogen_no_storage))]
use alloc::vec::Vec;
#[cfg(not(nitrogen_no_storage))]
use core::sync::atomic::AtomicBool as DriverAtomicBool;
#[cfg(not(nitrogen_no_storage))]
use core::sync::atomic::AtomicU64;
#[cfg(not(nitrogen_no_usb))]
use core::sync::atomic::AtomicUsize;
#[cfg(any(not(nitrogen_no_usb), not(nitrogen_no_storage)))]
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(nitrogen_no_storage))]
use core::{cell::UnsafeCell, mem::MaybeUninit};
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

// Drivers are exposed through a small software request/completion pair.  The
// queue is intentionally independent of any device's hardware DMA queues:
// it is the kernel-to-driver bridge used by ioctl and shell-facing requests.
#[cfg(not(nitrogen_no_storage))]
const DRIVER_QUEUE_DEPTH: usize = 8;

#[cfg(not(nitrogen_no_storage))]
#[derive(Debug)]
enum DriverRequestKind {
    InitializeNvme {
        device: PciDevice,
    },
    InitializeAhci {
        device: PciDevice,
    },
    Mmio {
        device: PciDevice,
        bar: u8,
        offset: u32,
        width: u8,
        write: bool,
        value: u64,
    },
    BlockIo {
        target: BlockTarget,
        lba: u64,
        count: u16,
        write: bool,
        buffer: Vec<u8>,
    },
}

/// Driver-independent block target carried by a storage SQ entry.
///
/// The request owns all data crossing the queue. Hardware-specific controller
/// state remains inside Nitrogen and is never exposed as a raw pointer.
#[cfg(not(nitrogen_no_storage))]
#[derive(Debug)]
enum BlockTarget {
    Ahci {
        controller_index: usize,
        port_index: u8,
    },
    Usb {
        ctrl_type: &'static str,
        ctrl_idx: usize,
        dev_addr: u8,
        ep_out: u8,
        ep_out_mps: u16,
        ep_in: u8,
        ep_in_mps: u16,
        tag: u32,
    },
    Sd,
}

#[cfg(not(nitrogen_no_storage))]
#[derive(Debug)]
struct DriverRequest {
    request_id: u64,
    kind: DriverRequestKind,
}

#[cfg(not(nitrogen_no_storage))]
#[derive(Debug)]
struct DriverCompletion {
    request_id: u64,
    error: Option<nitrogen::DriverError>,
    controller_index: Option<usize>,
    value: u64,
    buffer: Option<Vec<u8>>,
    tag: Option<u32>,
}

#[cfg(not(nitrogen_no_storage))]
struct DriverSlot<T> {
    sequence: AtomicUsize,
    entry: UnsafeCell<MaybeUninit<T>>,
}

#[cfg(not(nitrogen_no_storage))]
impl<T> DriverSlot<T> {
    const fn new(sequence: usize) -> Self {
        Self {
            sequence: AtomicUsize::new(sequence),
            entry: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

#[cfg(not(nitrogen_no_storage))]
unsafe impl<T: Send> Sync for DriverSlot<T> {}
#[cfg(not(nitrogen_no_storage))]
unsafe impl<T: Send> Send for DriverSlot<T> {}

#[cfg(not(nitrogen_no_storage))]
struct DriverRing<T> {
    entries: [DriverSlot<T>; DRIVER_QUEUE_DEPTH],
    producer_pos: AtomicUsize,
    consumer_pos: AtomicUsize,
}

#[cfg(not(nitrogen_no_storage))]
unsafe impl<T: Send> Sync for DriverRing<T> {}
#[cfg(not(nitrogen_no_storage))]
unsafe impl<T: Send> Send for DriverRing<T> {}

/// Bounded lock-free MPSC ring used for the generic driver SQ and CQ.
///
/// Producers reserve distinct slots with a lock-free CAS retry loop. Each
/// producer then publishes its slot by releasing the slot sequence number;
/// the single consumer acquires that sequence before reading the entry. A
/// full queue returns immediately, so the algorithm never waits on a mutex or
/// spin lock. The CAS loop is a lock-free reservation operation: if one
/// producer is delayed, another producer can still make progress.
#[cfg(not(nitrogen_no_storage))]
impl<T> DriverRing<T> {
    const fn new() -> Self {
        Self {
            entries: [
                DriverSlot::new(0),
                DriverSlot::new(1),
                DriverSlot::new(2),
                DriverSlot::new(3),
                DriverSlot::new(4),
                DriverSlot::new(5),
                DriverSlot::new(6),
                DriverSlot::new(7),
            ],
            producer_pos: AtomicUsize::new(0),
            consumer_pos: AtomicUsize::new(0),
        }
    }

    fn push(&self, value: T) -> Result<(), T> {
        let mut producer = self.producer_pos.load(Ordering::Relaxed);
        loop {
            let slot = &self.entries[producer & (DRIVER_QUEUE_DEPTH - 1)];
            let sequence = slot.sequence.load(Ordering::Acquire);
            let difference = sequence.wrapping_sub(producer) as isize;
            if difference == 0 {
                match self.producer_pos.compare_exchange_weak(
                    producer,
                    producer.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        unsafe { (*slot.entry.get()).write(value) };
                        slot.sequence
                            .store(producer.wrapping_add(1), Ordering::Release);
                        return Ok(());
                    }
                    Err(next) => producer = next,
                }
            } else if difference < 0 {
                return Err(value);
            } else {
                producer = self.producer_pos.load(Ordering::Relaxed);
            }
        }
    }

    /// Remove the oldest entry.
    ///
    /// The ring has a single-consumer contract. Callers must prevent
    /// concurrent `pop` calls; the synchronous adapter enforces this with
    /// `DRIVER_IN_FLIGHT`.
    fn pop(&self) -> Option<T> {
        let consumer = self.consumer_pos.load(Ordering::Relaxed);
        let slot = &self.entries[consumer & (DRIVER_QUEUE_DEPTH - 1)];
        let sequence = slot.sequence.load(Ordering::Acquire);
        let difference = sequence.wrapping_sub(consumer.wrapping_add(1)) as isize;
        if difference != 0 {
            return None;
        }

        self.consumer_pos
            .store(consumer.wrapping_add(1), Ordering::Relaxed);
        let value = unsafe { (*slot.entry.get()).assume_init_read() };
        slot.sequence
            .store(consumer.wrapping_add(DRIVER_QUEUE_DEPTH), Ordering::Release);
        Some(value)
    }
}

#[cfg(not(nitrogen_no_storage))]
struct DriverQueuePair {
    submission: DriverRing<DriverRequest>,
    completion: DriverRing<DriverCompletion>,
}

#[cfg(not(nitrogen_no_storage))]
impl DriverQueuePair {
    const fn new() -> Self {
        Self {
            submission: DriverRing::new(),
            completion: DriverRing::new(),
        }
    }

    fn submit(&self, request: DriverRequest) -> Result<(), DriverRequest> {
        self.submission.push(request)
    }

    fn take_submission(&self) -> Option<DriverRequest> {
        self.submission.pop()
    }

    fn complete(&self, completion: DriverCompletion) -> Result<(), DriverCompletion> {
        self.completion.push(completion)
    }

    fn take_completion(&self, request_id: u64) -> Option<DriverCompletion> {
        let completion = self.completion.pop()?;
        if completion.request_id == request_id {
            Some(completion)
        } else {
            // The ring has no push-front operation, so callers must request
            // completions in submission order. A mismatch means the
            // single-consumer invariant enforced by DRIVER_IN_FLIGHT broke.
            log::error!(
                "driver queue: dropped completion {} while waiting for {}",
                completion.request_id,
                request_id
            );
            None
        }
    }
}

#[cfg(not(nitrogen_no_storage))]
static DRIVER_QUEUES: DriverQueuePair = DriverQueuePair::new();
#[cfg(not(nitrogen_no_storage))]
static NEXT_DRIVER_REQUEST: AtomicU64 = AtomicU64::new(1);
#[cfg(not(nitrogen_no_storage))]
// The generic rings remain MPSC. This gate only serializes the synchronous
// adapter's CQ consumer so one caller cannot steal another caller's response.
static DRIVER_IN_FLIGHT: DriverAtomicBool = DriverAtomicBool::new(false);

#[cfg(not(nitrogen_no_storage))]
fn driver_queues() -> &'static DriverQueuePair {
    &DRIVER_QUEUES
}

#[cfg(all(test, not(nitrogen_no_storage)))]
mod driver_queue_tests {
    use super::*;

    #[test]
    fn submission_and_completion_are_independent_fifo_rings() {
        let pair = DriverQueuePair::new();
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
        pair.submit(DriverRequest {
            request_id: 7,
            kind: DriverRequestKind::InitializeNvme { device },
        })
        .unwrap();
        let request = pair.take_submission().unwrap();
        assert_eq!(request.request_id, 7);
        assert!(pair.take_submission().is_none());

        pair.complete(DriverCompletion {
            request_id: 7,
            error: None,
            controller_index: Some(0),
            value: 0,
            buffer: None,
            tag: None,
        })
        .unwrap();
        let completion = pair.take_completion(7).unwrap();
        assert_eq!(completion.controller_index, Some(0));
        assert!(pair.take_completion(7).is_none());
    }

    #[test]
    fn submission_ring_rejects_when_full_without_overwriting_entries() {
        let ring = DriverRing::<DriverRequest>::new();
        for request_id in 0..DRIVER_QUEUE_DEPTH as u64 {
            ring.push(DriverRequest {
                request_id,
                kind: DriverRequestKind::InitializeNvme {
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
                },
            })
            .unwrap();
        }
        let rejected = ring.push(DriverRequest {
            request_id: 99,
            kind: DriverRequestKind::InitializeNvme {
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
            },
        });
        assert_eq!(rejected.unwrap_err().request_id, 99);
        assert_eq!(ring.pop().unwrap().request_id, 0);
    }

    #[test]
    fn mpsc_ring_transfers_entries_concurrently_without_locking() {
        use std::collections::BTreeSet;
        use std::sync::Arc;
        use std::thread;

        const PRODUCERS: u64 = 4;
        const PER_PRODUCER: u64 = 2_500;
        const COUNT: u64 = PRODUCERS * PER_PRODUCER;
        let ring = Arc::new(DriverRing::<u64>::new());
        let mut producers = Vec::new();
        for producer_id in 0..PRODUCERS {
            let producer_ring = Arc::clone(&ring);
            producers.push(thread::spawn(move || {
                for sequence in 0..PER_PRODUCER {
                    let value = producer_id * PER_PRODUCER + sequence;
                    loop {
                        if producer_ring.push(value).is_ok() {
                            break;
                        }
                        thread::yield_now();
                    }
                }
            }));
        }

        let mut values = BTreeSet::new();
        while values.len() < COUNT as usize {
            loop {
                if let Some(value) = ring.pop() {
                    assert!(values.insert(value));
                    break;
                }
                thread::yield_now();
            }
        }

        for producer in producers {
            producer.join().unwrap();
        }
        assert_eq!(values.len(), COUNT as usize);
        assert!(ring.pop().is_none());
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

// -- Generic driver request service -------------------------------

#[cfg(not(nitrogen_no_storage))]
enum DriverResult {
    Controller(usize),
    Value(u64),
    Block { buffer: Vec<u8>, tag: Option<u32> },
}

#[cfg(not(nitrogen_no_storage))]
fn execute_block_io(
    target: BlockTarget,
    lba: u64,
    count: u16,
    write: bool,
    mut buffer: Vec<u8>,
) -> Result<(Vec<u8>, Option<u32>), nitrogen::DriverError> {
    match target {
        BlockTarget::Ahci {
            controller_index,
            port_index,
        } => {
            if write {
                nitrogen::storage::ahci::write_sectors(
                    controller_index,
                    port_index,
                    lba,
                    count,
                    &buffer,
                )?;
                buffer.clear();
            } else {
                nitrogen::storage::ahci::read_sectors(
                    controller_index,
                    port_index,
                    lba,
                    count,
                    &mut buffer,
                )?;
            }
            Ok((buffer, None))
        }
        BlockTarget::Usb {
            ctrl_type,
            ctrl_idx,
            dev_addr,
            ep_out,
            ep_out_mps,
            ep_in,
            ep_in_mps,
            tag,
        } => {
            let lba = u32::try_from(lba).map_err(|_| nitrogen::DriverError::InvalidArgument)?;
            let mut tag = tag;
            with_ctx(|ctx| {
                if write {
                    ctx.bot_write(
                        ctrl_type,
                        ctrl_idx,
                        dev_addr,
                        ep_out,
                        ep_out_mps,
                        ep_in,
                        ep_in_mps,
                        lba,
                        count,
                        (buffer.len() / count.max(1) as usize) as u32,
                        &buffer,
                        &mut tag,
                    )
                } else {
                    ctx.bot_read(
                        ctrl_type,
                        ctrl_idx,
                        dev_addr,
                        ep_out,
                        ep_out_mps,
                        ep_in,
                        ep_in_mps,
                        lba,
                        count,
                        (buffer.len() / count.max(1) as usize) as u32,
                        &mut buffer,
                        &mut tag,
                    )
                }
            })
            .map_err(|_| nitrogen::DriverError::Io)?;
            if write {
                buffer.clear();
            }
            Ok((buffer, Some(tag)))
        }
        BlockTarget::Sd => {
            let lba = u32::try_from(lba).map_err(|_| nitrogen::DriverError::InvalidArgument)?;
            if write {
                nitrogen::storage::rtsx::write_sectors(lba, count, &buffer)?;
                buffer.clear();
            } else {
                nitrogen::storage::rtsx::read_sectors(lba, count, &mut buffer)?;
            }
            Ok((buffer, None))
        }
    }
}

#[cfg(not(nitrogen_no_storage))]
fn execute_driver_request(request: DriverRequest) -> DriverCompletion {
    let request_id = request.request_id;
    let failure_device = match &request.kind {
        DriverRequestKind::InitializeNvme { device }
        | DriverRequestKind::InitializeAhci { device }
        | DriverRequestKind::Mmio { device, .. } => Some(device.clone()),
        DriverRequestKind::BlockIo { .. } => None,
    };
    let result = match request.kind {
        DriverRequestKind::InitializeNvme { device } => nitrogen::storage::nvme::init_device(
            &crate::driver_context_impl::KernelDriverContext,
            device,
        )
        .map(DriverResult::Controller),
        DriverRequestKind::InitializeAhci { device } => nitrogen::storage::ahci::init_device(
            &crate::driver_context_impl::KernelDriverContext,
            device,
        )
        .map(DriverResult::Controller),
        DriverRequestKind::Mmio {
            device,
            bar,
            offset,
            width,
            write,
            value,
        } => nitrogen::storage::nvme::request_mmio(&device, bar, offset, width, write, value)
            .map(DriverResult::Value),
        DriverRequestKind::BlockIo {
            target,
            lba,
            count,
            write,
            buffer,
        } => execute_block_io(target, lba, count, write, buffer)
            .map(|(buffer, tag)| DriverResult::Block { buffer, tag }),
    };

    // The concrete driver has already performed its failure cleanup before
    // returning here.  Only now may the supervisor terminate an explicitly
    // bound driver process; an ioctl caller is never used as a fallback owner.
    if let Err(error) = &result {
        if error.is_fatal() {
            if let Some(device) = failure_device {
                crate::drivers::supervisor::kill_failed_driver(&device, *error);
            }
        }
    }

    match result {
        Ok(DriverResult::Controller(index)) => DriverCompletion {
            request_id,
            error: None,
            controller_index: Some(index),
            value: 0,
            buffer: None,
            tag: None,
        },
        Ok(DriverResult::Value(value)) => DriverCompletion {
            request_id,
            error: None,
            controller_index: None,
            value,
            buffer: None,
            tag: None,
        },
        Ok(DriverResult::Block { buffer, tag }) => DriverCompletion {
            request_id,
            error: None,
            controller_index: None,
            value: buffer.len() as u64,
            buffer: Some(buffer),
            tag,
        },
        Err(error) => DriverCompletion {
            request_id,
            error: Some(error),
            controller_index: None,
            value: 0,
            buffer: None,
            tag: None,
        },
    }
}

#[cfg(not(nitrogen_no_storage))]
fn submit_driver_request(
    kind: DriverRequestKind,
) -> Result<DriverCompletion, nitrogen::DriverError> {
    // Synchronous ioctl callers share one CQ dispatcher, but contention is a
    // lock-free failure rather than a lock acquisition or a spin wait. The
    // underlying SQ/CQ themselves accept multiple producers.
    if DRIVER_IN_FLIGHT
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Err(nitrogen::DriverError::Busy);
    }

    let result = (|| {
        let request_id = NEXT_DRIVER_REQUEST.fetch_add(1, Ordering::Relaxed);
        driver_queues()
            .submit(DriverRequest { request_id, kind })
            .map_err(|_| nitrogen::DriverError::Busy)?;

        // Consume one generic SQ entry, execute it in the matched driver, and
        // publish exactly one generic CQ entry for the interactive caller.
        let request = driver_queues()
            .take_submission()
            .ok_or(nitrogen::DriverError::Io)?;
        let completion = execute_driver_request(request);
        driver_queues()
            .complete(completion)
            .map_err(|_| nitrogen::DriverError::Io)?;
        driver_queues()
            .take_completion(request_id)
            .ok_or(nitrogen::DriverError::Io)
    })();
    DRIVER_IN_FLIGHT.store(false, Ordering::Release);
    result
}

/// Submit one owned block I/O request through the generic storage SQ/CQ.
///
/// Read data is returned in the completion-owned buffer. Writes return an
/// empty buffer, and USB BOT returns its next command tag alongside it.
#[cfg(not(nitrogen_no_storage))]
fn submit_block_io(
    target: BlockTarget,
    lba: u64,
    count: u16,
    write: bool,
    buffer: Vec<u8>,
) -> Result<(Vec<u8>, Option<u32>), nitrogen::DriverError> {
    let completion = submit_driver_request(DriverRequestKind::BlockIo {
        target,
        lba,
        count,
        write,
        buffer,
    })?;
    completion.error.map_or_else(
        || Ok((completion.buffer.unwrap_or_default(), completion.tag)),
        Err,
    )
}

/// Submit one explicit NVMe initialization request through the generic
/// kernel-owned SQ and return its CQ completion.  Initialization remains
/// explicit; the boot driver probe does not reset or activate NVMe devices.
#[cfg(not(nitrogen_no_storage))]
pub fn initialize_nvme(device: PciDevice) -> Result<usize, nitrogen::DriverError> {
    let completion = submit_driver_request(DriverRequestKind::InitializeNvme { device })?;
    if let Some(index) = completion.controller_index {
        log::info!("NVMe: initialized nvme{} through SQ/CQ", index);
        Ok(index)
    } else {
        Err(completion.error.unwrap_or(nitrogen::DriverError::Io))
    }
}

/// Submit one explicit AHCI initialization request through the generic
/// kernel-owned SQ and return its CQ completion.
#[cfg(not(nitrogen_no_storage))]
pub fn initialize_ahci(device: PciDevice) -> Result<usize, nitrogen::DriverError> {
    let completion = submit_driver_request(DriverRequestKind::InitializeAhci { device })?;
    if let Some(index) = completion.controller_index {
        register_ahci_block_devices(index);
        let disk_count = nitrogen::storage::ahci::device_count(index);
        log::info!(
            "AHCI: initialized ahci{} through SQ/CQ ({} ATA disk(s))",
            index,
            disk_count
        );
        Ok(index)
    } else {
        Err(completion.error.unwrap_or(nitrogen::DriverError::Io))
    }
}

#[cfg(not(nitrogen_no_storage))]
fn register_ahci_block_devices(controller_index: usize) {
    for info in nitrogen::storage::ahci::devices()
        .into_iter()
        .filter(|info| info.controller_index == controller_index)
    {
        let name = alloc::format!("sata{}p{}", info.controller_index, info.port_index);
        if crate::devfs::block_device_exists(&name) {
            continue;
        }
        crate::devfs::register_block_device(
            name.clone(),
            Box::new(AhciBlockDevice {
                controller_index: info.controller_index,
                port_index: info.port_index,
                sector_size: info.sector_size,
                total_sectors: info.total_sectors,
            }),
        );
        log::info!(
            "AHCI: registered /dev/{} ({} bytes/sector, {} sectors)",
            name,
            info.sector_size,
            info.total_sectors
        );
    }
}

#[cfg(not(nitrogen_no_storage))]
struct AhciBlockDevice {
    controller_index: usize,
    port_index: u8,
    sector_size: u32,
    total_sectors: u64,
}

#[cfg(not(nitrogen_no_storage))]
impl BlockDevice for AhciBlockDevice {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        let required = (count as usize)
            .checked_mul(self.sector_size as usize)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < required {
            return Err(BlockError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if end > self.total_sectors {
            return Err(BlockError::LbaOverflow);
        }
        let (data, _) = submit_block_io(
            BlockTarget::Ahci {
                controller_index: self.controller_index,
                port_index: self.port_index,
            },
            lba,
            count,
            false,
            alloc::vec![0; required],
        )
        .map_err(ahci_block_error)?;
        if data.len() < required {
            return Err(BlockError::Device);
        }
        buf[..required].copy_from_slice(&data[..required]);
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let required = (count as usize)
            .checked_mul(self.sector_size as usize)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < required {
            return Err(BlockError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if end > self.total_sectors {
            return Err(BlockError::LbaOverflow);
        }
        submit_block_io(
            BlockTarget::Ahci {
                controller_index: self.controller_index,
                port_index: self.port_index,
            },
            lba,
            count,
            true,
            buf[..required].to_vec(),
        )
        .map(|_| ())
        .map_err(ahci_block_error)
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors
    }
}

#[cfg(not(nitrogen_no_storage))]
fn ahci_block_error(error: nitrogen::DriverError) -> BlockError {
    match error {
        nitrogen::DriverError::Busy => BlockError::Busy,
        nitrogen::DriverError::InvalidArgument => BlockError::LbaOverflow,
        nitrogen::DriverError::TimedOut
        | nitrogen::DriverError::Io
        | nitrogen::DriverError::DeviceFault
        | nitrogen::DriverError::Protocol
        | nitrogen::DriverError::DeviceNotFound => BlockError::Device,
        _ => BlockError::Device,
    }
}

/// Submit an interactive MMIO request through the same generic driver SQ/CQ
/// used by NVMe initialization.  The NVMe driver currently owns BAR0; future
/// drivers can add their own request dispatch without changing this queue ABI.
#[cfg(not(nitrogen_no_storage))]
pub fn request_mmio(
    device: PciDevice,
    bar: u8,
    offset: u32,
    width: u8,
    write: bool,
    value: u64,
) -> Result<u64, nitrogen::DriverError> {
    let completion = submit_driver_request(DriverRequestKind::Mmio {
        device,
        bar,
        offset,
        width,
        write,
        value,
    })?;
    completion.error.map_or(Ok(completion.value), Err)
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
        let required = (count as usize)
            .checked_mul(self.block_size as usize)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < required {
            return Err(BlockError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        let (data, tag) = submit_block_io(
            BlockTarget::Usb {
                ctrl_type: self.ctrl_type,
                ctrl_idx: self.ctrl_idx,
                dev_addr: self.dev_addr,
                ep_out: self.ep_out,
                ep_out_mps: self.ep_out_mps,
                ep_in: self.ep_in,
                ep_in_mps: self.ep_in_mps,
                tag: self.tag,
            },
            u64::from(lba),
            count,
            false,
            alloc::vec![0; required],
        )
        .map_err(|_| BlockError::Device)?;
        if data.len() < required {
            return Err(BlockError::Device);
        }
        if let Some(tag) = tag {
            self.tag = tag;
        }
        buf[..required].copy_from_slice(&data[..required]);
        Ok(())
    }
    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let lba = u32::try_from(lba).map_err(|_| BlockError::LbaOverflow)?;
        let required = (count as usize)
            .checked_mul(self.block_size as usize)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < required {
            return Err(BlockError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        let (_, tag) = submit_block_io(
            BlockTarget::Usb {
                ctrl_type: self.ctrl_type,
                ctrl_idx: self.ctrl_idx,
                dev_addr: self.dev_addr,
                ep_out: self.ep_out,
                ep_out_mps: self.ep_out_mps,
                ep_in: self.ep_in,
                ep_in_mps: self.ep_in_mps,
                tag: self.tag,
            },
            u64::from(lba),
            count,
            true,
            buf[..required].to_vec(),
        )
        .map_err(|_| BlockError::Device)?;
        if let Some(tag) = tag {
            self.tag = tag;
        }
        Ok(())
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
        let required = (count as usize)
            .checked_mul(self.block_size as usize)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < required {
            return Err(BlockError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        let (data, _) = submit_block_io(
            BlockTarget::Sd,
            u64::from(lba),
            count,
            false,
            alloc::vec![0; required],
        )
        .map_err(sd_block_error)?;
        if data.len() < required {
            return Err(BlockError::Device);
        }
        buf[..required].copy_from_slice(&data[..required]);
        Ok(())
    }
    fn write_sectors(&mut self, lba: u64, count: u16, buf: &[u8]) -> Result<(), BlockError> {
        let lba = u32::try_from(lba).map_err(|_| BlockError::LbaOverflow)?;
        let required = (count as usize)
            .checked_mul(self.block_size as usize)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < required {
            return Err(BlockError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        submit_block_io(
            BlockTarget::Sd,
            u64::from(lba),
            count,
            true,
            buf[..required].to_vec(),
        )
        .map(|_| ())
        .map_err(sd_block_error)
    }
    fn sector_size(&self) -> u32 {
        self.block_size
    }
    fn total_sectors(&self) -> u64 {
        self.total_blocks
    }
}

#[cfg(not(nitrogen_no_storage))]
fn sd_block_error(error: nitrogen::DriverError) -> BlockError {
    match error {
        nitrogen::DriverError::Busy => BlockError::Busy,
        nitrogen::DriverError::InvalidArgument => BlockError::LbaOverflow,
        _ => BlockError::Device,
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
