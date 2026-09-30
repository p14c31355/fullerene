//! USB 2.0 control-endpoint protocol shared by the Bramble gadget and QEMU
//! self-tests.
//!
//! The DWC3 register programming remains platform-specific.  Descriptor
//! selection and EP0 state transitions do not need to be, so keeping them in
//! this small no-alloc module lets QEMU exercise the part of enumeration that
//! currently fails on the phone.

/// Qualcomm's GSI request ABI allocates a circular DWC3 TRB ring per GSI
/// request. These are protocol-level constants so ring shape can be checked
/// on the host, before any MMIO or DMA address is involved.
pub const GSI_DEFAULT_NUM_BUFFERS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GsiRingShape {
    pub num_trbs: usize,
    pub first_buffer_trb: usize,
    pub data_trbs: usize,
}

/// Configuration supplied by a gadget function that has a Qualcomm GSI
/// consumer. The doorbell is deliberately supplied by that consumer: it is
/// an IPA-owned MMIO address and there is no safe controller-local default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GsiEndpointConfig {
    pub endpoint: usize,
    pub event_buffer: u32,
    pub max_packet: u32,
    pub doorbell: u64,
    pub buffer_length: usize,
}

/// Match the Bramble Android `gsi_prepare_trbs()` layout: IN has `n + 1`
/// zero-length normal TRBs followed by `n` buffer TRBs and one link TRB; OUT
/// has a leading link TRB, `n` data TRBs, and a closing link TRB.
pub const fn gsi_ring_shape(in_direction: bool, num_buffers: usize) -> Option<GsiRingShape> {
    if num_buffers == 0 {
        return None;
    }
    if in_direction {
        Some(GsiRingShape {
            num_trbs: 2 * num_buffers + 2,
            first_buffer_trb: num_buffers + 1,
            data_trbs: num_buffers,
        })
    } else {
        Some(GsiRingShape {
            num_trbs: num_buffers + 2,
            first_buffer_trb: 1,
            data_trbs: num_buffers,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlAction {
    DataIn(usize),
    StatusIn,
    StatusOut,
    SetHalt(u8),
    ClearHalt(u8),
    Setup,
    Stall,
}

/// A polled SETUP buffer is CPU-owned only after the controller retires its
/// SETUP TRB. EP0 reuses the same TRB for DATA and STATUS, so its address
/// fields alone cannot distinguish a received packet from the next stage.
pub const fn dwc3_setup_trb_complete(control: u32, size: u32) -> bool {
    control & 1 == 0 // HWO cleared
        && (control >> 4) & 0x3f == 2 // CONTROL_SETUP
        && size & 0x00ff_ffff == 0 // all eight bytes received
        && size >> 28 == 0 // TRBSTS_OK
}

/// Convert the USB descriptor's endpoint address to a DWC3 physical endpoint.
pub const fn dwc3_endpoint_number(address: u8) -> usize {
    ((address & 0x0f) as usize) * 2 + ((address >> 7) as usize)
}

/// The small interface shared by the hardware UDC and the no-hardware
/// simulator.  This is the early-boot equivalent of the Linux gadget core:
/// the controller owns TRBs and endpoint commands, while the gadget owns USB
/// requests, deferred address/configuration changes, and descriptors.
pub trait GadgetDriver {
    fn reset(&mut self);
    fn on_setup(&mut self, packet: [u8; 8], response: &mut [u8]) -> ControlAction;
    fn on_transfer_complete(&mut self) -> ControlAction;
    fn address(&self) -> u8;
    fn configured(&self) -> bool;

    /// Bind/unbind the function after SET_CONFIGURATION has committed. The
    /// default hooks keep simple control-only test gadgets source-compatible,
    /// while the hardware UDC can enforce the same lifetime boundary as
    /// Linux's gadget_driver::bind()/unbind().
    fn on_function_bind(&mut self) {}
    fn on_function_unbind(&mut self) {}

    /// Deliver a completed data request to the function layer. A no-alloc
    /// early gadget may use the default hook; a real function can consume the
    /// bounded request before the controller requeues an OUT buffer.
    fn on_data_complete(&mut self, _endpoint: u8, _actual: u32, _error: bool) {}

    /// Return a GSI binding only for functions backed by a real IPA/GSI
    /// consumer. `None` selects the ordinary DWC3 event-buffer-zero path.
    fn gsi_endpoint(&self) -> Option<GsiEndpointConfig> {
        None
    }

    /// Publish the DMA request pool after the controller has committed the
    /// GSI binding. A function may then fill/consume the pool and call the
    /// hardware queue operation through its platform adapter.
    fn on_gsi_channel_ready(
        &mut self,
        _config: GsiEndpointConfig,
        _ring: *mut u8,
        _buffers: *mut u8,
    ) {
    }

    /// Completion callback for a GSI request, kept separate from normal UDC
    /// completion because the event buffer is the ownership boundary.
    fn on_gsi_data_complete(&mut self, _endpoint: u8, _actual: u32, _error: bool) {}

    /// Notify a GSI-backed function that runtime PM revoked its outstanding
    /// request. The channel binding itself remains installed for resume.
    fn on_gsi_channel_suspend(&mut self) {}

    /// Notify a GSI-backed function that its channel may be queued again
    /// after the platform clocks, power, and DWC3 Run/Stop have resumed.
    fn on_gsi_channel_resume(&mut self) {}
}

/// Fixed-capacity request bookkeeping used by the early UDC. Linux's gadget
/// core owns a request queue per endpoint; the no-alloc boot path keeps the
/// same ownership rules with a bounded array so a completed TRB cannot be
/// silently reused while its callback is still pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestStatus {
    Free,
    Queued,
    InFlight,
    Complete,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbRequest {
    pub endpoint: u8,
    pub direction_in: bool,
    pub length: u32,
    pub actual: u32,
    pub status: RequestStatus,
}

impl UsbRequest {
    pub const EMPTY: Self = Self {
        endpoint: 0,
        direction_in: false,
        length: 0,
        actual: 0,
        status: RequestStatus::Free,
    };
}

pub const MAX_UDC_REQUESTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestQueue {
    requests: [UsbRequest; MAX_UDC_REQUESTS],
}

impl RequestQueue {
    pub const fn new() -> Self {
        Self {
            requests: [UsbRequest::EMPTY; MAX_UDC_REQUESTS],
        }
    }

    pub fn enqueue(&mut self, endpoint: u8, direction_in: bool, length: u32) -> Option<usize> {
        let slot = self
            .requests
            .iter()
            .position(|request| request.status == RequestStatus::Free)?;
        self.requests[slot] = UsbRequest {
            endpoint,
            direction_in,
            length,
            actual: 0,
            status: RequestStatus::Queued,
        };
        Some(slot)
    }

    pub fn start(&mut self, slot: usize) -> bool {
        let Some(request) = self.requests.get_mut(slot) else {
            return false;
        };
        if request.status != RequestStatus::Queued {
            return false;
        }
        request.status = RequestStatus::InFlight;
        true
    }

    pub fn complete(&mut self, slot: usize, actual: u32, error: bool) -> bool {
        let Some(request) = self.requests.get_mut(slot) else {
            return false;
        };
        if request.status != RequestStatus::InFlight {
            return false;
        }
        request.actual = actual.min(request.length);
        request.status = if error {
            RequestStatus::Error
        } else {
            RequestStatus::Complete
        };
        true
    }

    pub fn cancel_all(&mut self) {
        for request in &mut self.requests {
            if matches!(
                request.status,
                RequestStatus::Queued | RequestStatus::InFlight
            ) {
                request.status = RequestStatus::Cancelled;
            }
        }
    }

    /// Remove one request before it has completed.  This is the early-boot
    /// equivalent of usb_ep_dequeue(): the request remains observable as
    /// cancelled until the controller owner gives the slot back.
    pub fn cancel(&mut self, slot: usize) -> bool {
        let Some(request) = self.requests.get_mut(slot) else {
            return false;
        };
        if !matches!(
            request.status,
            RequestStatus::Queued | RequestStatus::InFlight
        ) {
            return false;
        }
        request.status = RequestStatus::Cancelled;
        true
    }

    pub fn release(&mut self, slot: usize) -> bool {
        let Some(request) = self.requests.get_mut(slot) else {
            return false;
        };
        if request.status == RequestStatus::Free {
            return false;
        }
        *request = UsbRequest::EMPTY;
        true
    }

    pub fn get(&self, slot: usize) -> Option<UsbRequest> {
        self.requests
            .get(slot)
            .copied()
            .filter(|request| request.status != RequestStatus::Free)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdcEndpoint {
    pub address: u8,
    pub max_packet: u16,
    pub bulk: bool,
    pub enabled: bool,
    pub halted: bool,
}

impl UdcEndpoint {
    const EMPTY: Self = Self {
        address: 0,
        max_packet: 0,
        bulk: false,
        enabled: false,
        halted: false,
    };
}

/// Minimal UDC object shared by hardware lifecycle code and protocol tests.
/// It deliberately does not own DMA buffers; the DWC3 layer maps a request
/// to a TRB only after `queue`/`start` has established ownership here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbUdc {
    endpoints: [UdcEndpoint; 8],
    queues: [RequestQueue; 8],
    pub address: u8,
    pub configured: bool,
    pub suspended: bool,
}

impl UsbUdc {
    pub const fn new() -> Self {
        Self {
            endpoints: [UdcEndpoint::EMPTY; 8],
            queues: [RequestQueue::new(); 8],
            address: 0,
            configured: false,
            suspended: false,
        }
    }

    pub fn configure_endpoint(&mut self, address: u8, max_packet: u16, bulk: bool) -> bool {
        let index = (address & 0x7f) as usize;
        if index >= self.endpoints.len() || max_packet == 0 {
            return false;
        }
        self.endpoints[index] = UdcEndpoint {
            address,
            max_packet,
            bulk,
            enabled: true,
            halted: false,
        };
        true
    }

    pub fn queue(&mut self, address: u8, length: u32) -> Option<usize> {
        let index = (address & 0x7f) as usize;
        if index >= self.endpoints.len()
            || !self.endpoints[index].enabled
            || self.endpoints[index].halted
            || self.suspended
        {
            return None;
        }
        self.queues[index].enqueue(address, address & 0x80 != 0, length)
    }

    pub fn start(&mut self, address: u8, slot: usize) -> bool {
        let index = (address & 0x7f) as usize;
        index < self.queues.len() && self.queues[index].start(slot)
    }

    pub fn complete(&mut self, address: u8, slot: usize, actual: u32, error: bool) -> bool {
        let index = (address & 0x7f) as usize;
        index < self.queues.len() && self.queues[index].complete(slot, actual, error)
    }

    pub fn cancel(&mut self, address: u8, slot: usize) -> bool {
        let index = (address & 0x7f) as usize;
        index < self.queues.len() && self.queues[index].cancel(slot)
    }

    pub fn set_halt(&mut self, address: u8, halted: bool) -> bool {
        let index = (address & 0x7f) as usize;
        let Some(endpoint) = self.endpoints.get_mut(index) else {
            return false;
        };
        if !endpoint.enabled {
            return false;
        }
        endpoint.halted = halted;
        if halted {
            self.queues[index].cancel_all();
        }
        true
    }

    pub fn release(&mut self, address: u8, slot: usize) -> bool {
        let index = (address & 0x7f) as usize;
        index < self.queues.len() && self.queues[index].release(slot)
    }

    pub fn reset(&mut self) {
        self.address = 0;
        self.configured = false;
        self.suspended = false;
        self.endpoints = [UdcEndpoint::EMPTY; 8];
        for queue in &mut self.queues {
            // A disconnect gives all queued requests back to gadget-core;
            // retain no stale slots across the next USB bus session.
            *queue = RequestQueue::new();
        }
    }

    /// Disable an endpoint and cancel all requests owned by it.  The DWC3
    /// layer performs ENDTRANSFER before calling this method; keeping the
    /// ownership transition here prevents a stale request from being
    /// accepted after SET_CONFIGURATION(0) or a disconnect.
    pub fn disable_endpoint(&mut self, address: u8) -> bool {
        let index = (address & 0x7f) as usize;
        let Some(endpoint) = self.endpoints.get_mut(index) else {
            return false;
        };
        if !endpoint.enabled {
            return false;
        }
        endpoint.enabled = false;
        self.queues[index].cancel_all();
        true
    }

    pub fn suspend(&mut self) {
        self.suspended = true;
    }

    pub fn resume(&mut self) {
        self.suspended = false;
    }

    pub fn endpoint(&self, address: u8) -> Option<UdcEndpoint> {
        self.endpoints
            .get((address & 0x7f) as usize)
            .copied()
            .filter(|endpoint| endpoint.enabled)
    }

    pub fn request(&self, address: u8, slot: usize) -> Option<UsbRequest> {
        self.queues
            .get((address & 0x7f) as usize)
            .and_then(|queue| queue.get(slot))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ep0State {
    Setup,
    Data,
    Status,
}

#[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
pub const DEVICE_DESCRIPTOR: [u8; 18] = [
    18, 1, 0x00, 0x03, 0, 0, 0, 9, 0x34, 0x12, 0x01, 0x00, 0, 1, 1, 2, 3, 1,
];

#[cfg(all(
    not(fullerene_aarch64_usb_gadget_handoff_super_speed),
    fullerene_aarch64_usb_dcfg_lowspeed
))]
pub const DEVICE_DESCRIPTOR: [u8; 18] = [
    18, 1, 0x00, 0x02, 0, 0, 0, 8, 0x34, 0x12, 0x01, 0x00, 0, 1, 1, 2, 3, 1,
];

#[cfg(all(
    not(fullerene_aarch64_usb_gadget_handoff_super_speed),
    not(fullerene_aarch64_usb_dcfg_lowspeed)
))]
pub const DEVICE_DESCRIPTOR: [u8; 18] = [
    18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x34, 0x12, 0x01, 0x00, 0, 1, 1, 2, 3, 1,
];

/// The serial string is a compact post-mortem cursor. The hardware wrapper
/// refreshes it immediately before each GET_DESCRIPTOR request, so a host
/// that can enumerate the gadget can retrieve the last trace head/event
/// without UART or a second debug transport.
pub const TRACE_DESCRIPTOR: [u8; 10] = [10, 3, b'F', 0, b'U', 0, b'T', 0, b'R', 0];
const TRACE_STATUS_CHARS: usize = 22;
const TRACE_STATUS_DESCRIPTOR_BYTES: usize = 2 + TRACE_STATUS_CHARS * 2;

/// Vendor control request used by the host-side trace reader. The response
/// starts with a 16-byte little-endian header followed by up to 15 packed
/// 32-byte `UsbTraceEntry` records, fitting in one 512-byte EP0 transfer.
pub const TRACE_CONTROL_REQUEST_TYPE: u8 = 0xc0;
pub const TRACE_CONTROL_REQUEST: u8 = 0x5a;
pub const TRACE_CONTROL_HEADER_BYTES: usize = 16;
pub const TRACE_CONTROL_ENTRY_BYTES: usize = 32;
pub const TRACE_CONTROL_PAGE_ENTRIES: usize = 15;

/// One ADB-class-compatible bulk function with a bulk IN/OUT pair. The
/// transport accepts the bounded ADB framing and the legacy FDBG diagnostic
/// framing without changing the UDC/EP0 lifecycle.
/// One CDC-ACM function: a communications interface with its notification endpoint, and a data
/// interface carrying the bulk pair the `FDBG` transport already uses.
///
/// The class changed from the Android ADB interface to CDC-ACM because Linux has a driver for the
/// latter and none for the former: the device appears as `/dev/ttyACM0` and `cat` is the whole host
/// side, with no root, no `adb` and no Android in the path. The bulk endpoints and the VID/PID are
/// unchanged, so the existing host tooling still finds the same device.
///
/// The bytes after the configuration header are copied from `crate::cdc_acm::FUNCTION_DESCRIPTOR`,
/// and a test below compares them to it: two copies of a descriptor set is exactly the kind of drift
/// that produces a device the host enumerates and then refuses.
#[cfg(not(fullerene_aarch64_usb_gadget_handoff_super_speed))]
pub const CONFIG_DESCRIPTOR: [u8; 67] = [
    9, 2, 67, 0, 2, 1, 0, 0x80, 50, // configuration: two interfaces, 67 bytes
    9, 4, 0, 0, 1, 0x02, 0x02, 0x01, 0, // communications interface, ACM
    5, 0x24, 0x00, 0x10, 0x01, // header functional descriptor, bcdCDC 1.10
    5, 0x24, 0x01, 0x00, 1, // call management, data interface 1
    4, 0x24, 0x02, 0x02, // ACM capabilities
    5, 0x24, 0x06, 0, 1, // union: interface 0 master, interface 1 slave
    7, 5, 0x81, 3, 0x08, 0x00, 0x10, // notification, interrupt IN, 8 bytes
    9, 4, 1, 0, 2, 0x0a, 0, 0, 0, // data interface
    7, 5, 0x02, 2, 0x40, 0x00, 0, // bulk OUT, EP2
    7, 5, 0x83, 2, 0x40, 0x00, 0, // bulk IN, EP3
];

// The Bramble handoff reaches the host as SuperSpeed when Fastboot leaves the
// combo PHY active. A SuperSpeed configuration needs an SS companion for each
// bulk endpoint; advertising USB2-only endpoint descriptors makes the host
// reject the configuration after EP0 enumeration.
/// The same CDC-ACM function for a SuperSpeed link. Only the *bulk* endpoints need a companion
/// descriptor; the notification endpoint is interrupt, which does not.
#[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
pub const CONFIG_DESCRIPTOR: [u8; 79] = [
    9, 2, 79, 0, 2, 1, 0, 0x80, 50, // configuration: two interfaces, 79 bytes
    9, 4, 0, 0, 1, 0x02, 0x02, 0x01, 0, // communications interface, ACM
    5, 0x24, 0x00, 0x10, 0x01, // header functional descriptor, bcdCDC 1.10
    5, 0x24, 0x01, 0x00, 1, // call management, data interface 1
    4, 0x24, 0x02, 0x02, // ACM capabilities
    5, 0x24, 0x06, 0, 1, // union: interface 0 master, interface 1 slave
    7, 5, 0x81, 3, 0x08, 0x00, 0x10, // notification, interrupt IN, 8 bytes
    9, 4, 1, 0, 2, 0x0a, 0, 0, 0, // data interface
    7, 5, 0x02, 2, 0x40, 0x00, 0, // bulk OUT, EP2
    6, 0x30, 0, 0, 0, 0, // SS endpoint companion
    7, 5, 0x83, 2, 0x40, 0x00, 0, // bulk IN, EP3
    6, 0x30, 0, 0, 0, 0, // SS endpoint companion
];

#[cfg(fullerene_aarch64_usb_gadget_handoff_super_speed)]
pub const BOS_DESCRIPTOR: [u8; 22] = [
    5, 15, 22, 0, 2, // BOS header, two device capabilities
    7, 16, 2, 0, 0, 0, 0, // USB 2.0 extension
    10, 16, 3, 0, 0x0e, 0, 1, 0, 0, 0, // SuperSpeed capability
];

#[cfg(not(fullerene_aarch64_usb_gadget_handoff_super_speed))]
pub const BOS_DESCRIPTOR: [u8; 12] = [
    5, 15, 12, 0, 1, // BOS header, USB 2.0 extension only
    7, 16, 2, 0, 0, 0, 0,
];

pub const LANGID_DESCRIPTOR: [u8; 4] = [4, 3, 0x09, 0x04];

pub const MANUFACTURER_DESCRIPTOR: [u8; 20] = [
    20, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0, b'e', 0,
];

pub const PRODUCT_DESCRIPTOR: [u8; 36] = [
    36, 3, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0, b'e', 0, b' ',
    0, b'A', 0, b'A', 0, b'r', 0, b'c', 0, b'h', 0, b'6', 0, b'4', 0,
];

pub fn descriptor(kind: u8, index: u8) -> Option<&'static [u8]> {
    match (kind, index) {
        (1, 0) => Some(&DEVICE_DESCRIPTOR),
        (2, 0) => Some(&CONFIG_DESCRIPTOR),
        (15, 0) => Some(&BOS_DESCRIPTOR),
        (3, 0) => Some(&LANGID_DESCRIPTOR),
        (3, 1) => Some(&MANUFACTURER_DESCRIPTOR),
        (3, 2) => Some(&PRODUCT_DESCRIPTOR),
        (3, 3) => Some(&TRACE_DESCRIPTOR),
        _ => None,
    }
}

const fn trace_status_default() -> [u8; TRACE_STATUS_DESCRIPTOR_BYTES] {
    let mut descriptor = [0; TRACE_STATUS_DESCRIPTOR_BYTES];
    let mut index = 0;
    while index < TRACE_DESCRIPTOR.len() {
        descriptor[index] = TRACE_DESCRIPTOR[index];
        index += 1;
    }
    descriptor
}

#[inline]
fn hex_digit(value: u32) -> u8 {
    match value & 0xf {
        0..=9 => b'0' + (value & 0xf) as u8,
        digit => b'a' + (digit - 10) as u8,
    }
}

pub struct Ep0Simulator {
    state: Ep0State,
    address: u8,
    pending_address: Option<u8>,
    configured: bool,
    pending_configured: Option<bool>,
    interface_alt: u8,
    pending_halt: Option<(u8, bool)>,
    halted_endpoints: u16,
    control_in: bool,
    control_has_data: bool,
    trace_status: [u8; TRACE_STATUS_DESCRIPTOR_BYTES],
    trace_status_len: usize,
}

impl Ep0Simulator {
    pub const fn new() -> Self {
        Self {
            state: Ep0State::Setup,
            address: 0,
            pending_address: None,
            configured: false,
            pending_configured: None,
            interface_alt: 0,
            pending_halt: None,
            halted_endpoints: 0,
            control_in: false,
            control_has_data: false,
            trace_status: trace_status_default(),
            trace_status_len: TRACE_DESCRIPTOR.len(),
        }
    }

    pub fn reset(&mut self) {
        self.state = Ep0State::Setup;
        self.address = 0;
        self.pending_address = None;
        self.configured = false;
        self.pending_configured = None;
        self.interface_alt = 0;
        self.pending_halt = None;
        self.halted_endpoints = 0;
        self.control_in = false;
        self.control_has_data = false;
        self.trace_status = trace_status_default();
        self.trace_status_len = TRACE_DESCRIPTOR.len();
    }

    /// Refresh the serial-string payload with the current retained-trace
    /// cursor. This remains a normal gadget state operation: it does not
    /// change EP0 state until the host actually requests string descriptor 3.
    pub fn set_trace_status(&mut self, head: u32, event: u32) {
        let mut ascii = [0u8; TRACE_STATUS_CHARS];
        ascii[..5].copy_from_slice(b"FUTR-");
        let mut index = 0;
        while index < 8 {
            let shift = 28 - index * 4;
            ascii[5 + index] = hex_digit((head >> shift) & 0xf);
            ascii[14 + index] = hex_digit((event >> shift) & 0xf);
            index += 1;
        }
        ascii[13] = b'-';

        let mut descriptor = [0; TRACE_STATUS_DESCRIPTOR_BYTES];
        descriptor[0] = TRACE_STATUS_DESCRIPTOR_BYTES as u8;
        descriptor[1] = 3;
        index = 0;
        while index < TRACE_STATUS_CHARS {
            descriptor[2 + index * 2] = ascii[index];
            index += 1;
        }
        self.trace_status = descriptor;
        self.trace_status_len = TRACE_STATUS_DESCRIPTOR_BYTES;
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn configured(&self) -> bool {
        self.configured
    }

    /// Cancel an interrupted control request without resetting the device's
    /// committed address or configuration. A new SETUP aborts the old request.
    pub fn abort_control(&mut self) {
        self.state = Ep0State::Setup;
        self.pending_address = None;
        self.pending_configured = None;
        self.pending_halt = None;
        self.control_in = false;
        self.control_has_data = false;
    }

    /// Let the controller's diagnostic response share the normal completion
    /// lifecycle even though its payload is assembled outside this module.
    pub fn begin_external_data_in(&mut self) {
        self.abort_control();
        self.state = Ep0State::Data;
        self.control_in = true;
        self.control_has_data = true;
    }

    pub fn on_setup(&mut self, packet: [u8; 8], response: &mut [u8]) -> ControlAction {
        self.abort_control();

        let request_type = packet[0];
        let request = packet[1];
        let value = u16::from_le_bytes([packet[2], packet[3]]);
        let index = u16::from_le_bytes([packet[4], packet[5]]);
        let requested_length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
        self.control_in = request_type & 0x80 != 0;
        self.control_has_data = requested_length != 0;

        if request_type == 0x80 && request == 6 && requested_length != 0 {
            let kind = (value >> 8) as u8;
            let index = value as u8;
            if kind == 3 && index == 3 {
                let length = requested_length
                    .min(self.trace_status_len)
                    .min(response.len());
                response[..length].copy_from_slice(&self.trace_status[..length]);
                self.state = Ep0State::Data;
                return ControlAction::DataIn(length);
            }
            if let Some(bytes) = descriptor(kind, index) {
                let length = requested_length.min(bytes.len()).min(response.len());
                response[..length].copy_from_slice(&bytes[..length]);
                self.state = Ep0State::Data;
                return ControlAction::DataIn(length);
            }
        }

        if request_type == 0x80 && request == 0 && requested_length == 2 && index == 0 {
            let length = requested_length.min(2).min(response.len());
            if length != 0 {
                response[..length].fill(0);
            }
            self.state = Ep0State::Data;
            return ControlAction::DataIn(length);
        }

        if request_type == 0x82 && request == 0 && requested_length == 2 {
            let endpoint = index as u8;
            if response.len() >= 2
                && (endpoint == 0
                    || endpoint == 0x80
                    || endpoint == 0x02
                    || endpoint == 0x83
                    || endpoint == 0x81)
            {
                response[..2].fill(0);
                if self.halted_endpoints & (1 << (endpoint & 0x0f)) != 0 {
                    response[0] = 1;
                }
                self.state = Ep0State::Data;
                return ControlAction::DataIn(2);
            }
        }

        if request_type == 0x80 && request == 8 && requested_length == 1 && index == 0 {
            let length = requested_length.min(1).min(response.len());
            if length != 0 {
                response[0] = self.configured as u8;
            }
            self.state = Ep0State::Data;
            return ControlAction::DataIn(length);
        }

        if request_type == 0
            && request == 5
            && value <= 0x7f
            && index == 0
            && requested_length == 0
            && !self.configured
        {
            self.pending_address = Some(value as u8);
            self.control_has_data = false;
            self.state = Ep0State::Status;
            return ControlAction::StatusIn;
        }

        if request_type == 0 && request == 9 && value <= 1 && index == 0 && requested_length == 0 {
            self.pending_configured = Some(value != 0);
            self.control_has_data = false;
            self.state = Ep0State::Status;
            return ControlAction::StatusIn;
        }

        if request_type == 0x81
            && request == 10
            && self.configured
            && value == 0
            && index == 0
            && requested_length == 1
            && !response.is_empty()
        {
            response[0] = self.interface_alt;
            self.state = Ep0State::Data;
            return ControlAction::DataIn(1);
        }

        if request_type == 1
            && request == 11
            && self.configured
            && value == 0
            && index == 0
            && requested_length == 0
        {
            self.state = Ep0State::Status;
            return ControlAction::StatusIn;
        }

        if (request_type == 0x02)
            && (request == 1 || request == 3)
            && self.configured
            && value == 0
            && requested_length == 0
            && matches!(index as u8, 0x02 | 0x83 | 0x81)
        {
            let endpoint = index as u8;
            self.pending_halt = Some((endpoint, request == 3));
            self.state = Ep0State::Status;
            return ControlAction::StatusIn;
        }

        self.state = Ep0State::Setup;
        ControlAction::Stall
    }

    pub fn on_transfer_complete(&mut self) -> ControlAction {
        match self.state {
            Ep0State::Data => {
                self.state = Ep0State::Status;
                if self.control_has_data && self.control_in {
                    ControlAction::StatusOut
                } else {
                    ControlAction::StatusIn
                }
            }
            Ep0State::Status => {
                self.state = Ep0State::Setup;
                if let Some(address) = self.pending_address.take() {
                    self.address = address;
                }
                if let Some(configured) = self.pending_configured.take() {
                    self.configured = configured;
                }
                let mut action = ControlAction::Setup;
                if let Some((endpoint, halted)) = self.pending_halt.take() {
                    let bit = 1 << (endpoint & 0x0f);
                    if halted {
                        self.halted_endpoints |= bit;
                    } else {
                        self.halted_endpoints &= !bit;
                    }
                    action = if halted {
                        ControlAction::SetHalt(endpoint)
                    } else {
                        ControlAction::ClearHalt(endpoint)
                    };
                }
                self.control_in = false;
                self.control_has_data = false;
                action
            }
            Ep0State::Setup => ControlAction::Setup,
        }
    }
}

impl GadgetDriver for Ep0Simulator {
    fn reset(&mut self) {
        Self::reset(self);
    }

    fn on_setup(&mut self, packet: [u8; 8], response: &mut [u8]) -> ControlAction {
        Self::on_setup(self, packet, response)
    }

    fn on_transfer_complete(&mut self) -> ControlAction {
        Self::on_transfer_complete(self)
    }

    fn address(&self) -> u8 {
        Self::address(self)
    }

    fn configured(&self) -> bool {
        Self::configured(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CONFIG_DESCRIPTOR, ControlAction, DEVICE_DESCRIPTOR, Ep0Simulator, GSI_DEFAULT_NUM_BUFFERS,
        GsiRingShape, MANUFACTURER_DESCRIPTOR, PRODUCT_DESCRIPTOR, TRACE_DESCRIPTOR,
        gsi_ring_shape,
    };

    #[test]
    fn polled_setup_requires_retired_successful_setup_trb() {
        use super::dwc3_setup_trb_complete as complete;
        assert!(complete(2 << 4, 0));
        assert!(!complete((2 << 4) | 1, 0)); // still controller-owned
        assert!(!complete(2 << 4, 8)); // no packet received
        assert!(!complete(2 << 4, 2 << 28)); // interrupted
        assert!(!complete(5 << 4, 0)); // old DATA address is not SETUP
        assert!(!complete(4 << 4, 0)); // old STATUS address is not SETUP
    }

    #[test]
    fn new_setup_aborts_uncommitted_address_and_configuration() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        assert_eq!(
            ep0.on_setup([0, 5, 42, 0, 0, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        assert_eq!(
            ep0.on_setup([0x80, 6, 0, 1, 0, 0, 18, 0], &mut response),
            ControlAction::DataIn(18)
        );
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert_eq!(ep0.address(), 0);
        assert_eq!(
            ep0.on_setup([0, 9, 1, 0, 0, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        ep0.abort_control();
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert!(!ep0.configured());
    }

    #[test]
    fn external_trace_response_has_real_status_lifecycle() {
        let mut ep0 = Ep0Simulator::new();
        ep0.begin_external_data_in();
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
    }

    #[test]
    fn endpoint_addresses_include_direction_in_physical_index() {
        assert_eq!(super::dwc3_endpoint_number(0), 0);
        assert_eq!(super::dwc3_endpoint_number(0x80), 1);
        assert_eq!(super::dwc3_endpoint_number(0x02), 4);
        assert_eq!(super::dwc3_endpoint_number(0x83), 7);
    }

    #[test]
    fn descriptor_lengths_match_their_headers() {
        for descriptor in [
            &DEVICE_DESCRIPTOR[..],
            &MANUFACTURER_DESCRIPTOR[..],
            &PRODUCT_DESCRIPTOR[..],
            &TRACE_DESCRIPTOR[..],
        ] {
            assert_eq!(descriptor[0] as usize, descriptor.len());
        }
        assert_eq!(CONFIG_DESCRIPTOR[0], 9);
        assert_eq!(
            u16::from_le_bytes([CONFIG_DESCRIPTOR[2], CONFIG_DESCRIPTOR[3]]),
            32
        );
        // The configuration now carries the CDC-ACM function: the class bytes are the
        // communications/ACM/AT triple, and the bytes after the configuration header must equal
        // `cdc_acm::FUNCTION_DESCRIPTOR` byte for byte. Two written-out copies of a descriptor set
        // is precisely the drift that yields a device the host enumerates and then refuses, so it is
        // asserted rather than trusted.
        assert_eq!(CONFIG_DESCRIPTOR[4], 2, "two interfaces");
        assert_eq!(&CONFIG_DESCRIPTOR[14..17], &[0x02, 0x02, 0x01]);
        assert_eq!(CONFIG_DESCRIPTOR.len(), 9 + crate::cdc_acm::FUNCTION_DESCRIPTOR.len());
        assert_eq!(
            &CONFIG_DESCRIPTOR[9..],
            &crate::cdc_acm::FUNCTION_DESCRIPTOR[..],
            "the configuration must carry the tested descriptor set verbatim"
        );
        assert_eq!(CONFIG_DESCRIPTOR[13], 1, "one notification endpoint");
        assert_eq!(CONFIG_DESCRIPTOR[18], 7);
        assert_eq!(CONFIG_DESCRIPTOR[19], 5);
        assert_eq!(CONFIG_DESCRIPTOR[20], 0x83);
        assert_eq!(CONFIG_DESCRIPTOR[25], 7);
        assert_eq!(CONFIG_DESCRIPTOR[26], 5);
        assert_eq!(CONFIG_DESCRIPTOR[27], 0x02);
    }

    #[test]
    fn trace_status_string_is_host_readable() {
        let mut ep0 = Ep0Simulator::new();
        ep0.set_trace_status(0x12ab_34cd, 0x0000_0026);
        let mut response = [0u8; 64];
        let action = ep0.on_setup(
            [0x80, 6, 3, 3, 0, 0, response.len() as u8, 0],
            &mut response,
        );
        assert_eq!(action, ControlAction::DataIn(46));
        assert_eq!(&response[..2], &[46, 3]);
        assert_eq!(
            &response[2..12],
            &[b'F', 0, b'U', 0, b'T', 0, b'R', 0, b'-', 0]
        );
        assert_eq!(
            &response[12..28],
            &[
                b'1', 0, b'2', 0, b'a', 0, b'b', 0, b'3', 0, b'4', 0, b'c', 0, b'd', 0,
            ]
        );
        assert_eq!(&response[28..30], &[b'-', 0]);
        assert_eq!(
            &response[30..46],
            &[
                b'0', 0, b'0', 0, b'0', 0, b'0', 0, b'0', 0, b'0', 0, b'2', 0, b'6', 0,
            ]
        );
    }

    #[test]
    fn get_device_descriptor_reaches_a_new_setup() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        assert_eq!(
            ep0.on_setup([0x80, 6, 0, 1, 0, 0, 64, 0], &mut response),
            ControlAction::DataIn(18)
        );
        assert_eq!(&response[..18], &DEVICE_DESCRIPTOR);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
    }

    #[test]
    fn set_address_and_configuration_commit_after_status() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        assert_eq!(
            ep0.on_setup([0, 5, 7, 0, 0, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        assert_eq!(ep0.address(), 0);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert_eq!(ep0.address(), 7);
        assert_eq!(
            ep0.on_setup([0, 9, 1, 0, 0, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert!(ep0.configured());
    }

    #[test]
    fn reset_returns_to_default_state() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        let _ = ep0.on_setup([0, 5, 7, 0, 0, 0, 0, 0], &mut response);
        ep0.reset();
        assert_eq!(ep0.address(), 0);
        assert!(!ep0.configured());
        assert_eq!(
            ep0.on_setup([0x80, 6, 0, 1, 0, 0, 18, 0], &mut response),
            ControlAction::DataIn(18)
        );
    }

    #[test]
    fn unsupported_control_request_stalls_instead_of_leaving_ep0_idle() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        assert_eq!(
            ep0.on_setup([0, 0x7f, 0, 0, 0, 0, 0, 0], &mut response),
            ControlAction::Stall
        );
        assert_eq!(
            ep0.on_setup([0x80, 6, 0, 1, 0, 0, 18, 0], &mut response),
            ControlAction::DataIn(18)
        );
    }

    #[test]
    fn udc_request_ownership_follows_queue_start_complete_release() {
        let mut udc = super::UsbUdc::new();
        assert!(udc.configure_endpoint(0x81, 64, true));
        let slot = udc.queue(0x81, 128).expect("request slot");
        assert_eq!(
            udc.request(0x81, slot).unwrap().status,
            super::RequestStatus::Queued
        );
        assert!(udc.start(0x81, slot));
        assert!(udc.complete(0x81, slot, 96, false));
        let request = udc.request(0x81, slot).unwrap();
        assert_eq!(request.actual, 96);
        assert_eq!(request.status, super::RequestStatus::Complete);
        assert!(udc.release(0x81, slot));
        assert!(udc.queue(0x81, 32).is_some());
    }

    #[test]
    fn udc_disconnect_cancels_inflight_requests_and_reset_clears_state() {
        let mut udc = super::UsbUdc::new();
        assert!(udc.configure_endpoint(0x02, 64, true));
        let slot = udc.queue(0x02, 64).unwrap();
        assert!(udc.start(0x02, slot));
        udc.reset();
        assert_eq!(udc.address, 0);
        assert!(!udc.configured);
        assert!(udc.endpoint(0x02).is_none());
        assert!(udc.request(0x02, slot).is_none());
    }

    #[test]
    fn udc_supports_zero_length_and_dequeue_lifecycle() {
        let mut udc = super::UsbUdc::new();
        assert!(udc.configure_endpoint(0x83, 512, true));
        let slot = udc.queue(0x83, 0).expect("ZLP request slot");
        assert!(udc.start(0x83, slot));
        assert!(udc.cancel(0x83, slot));
        assert_eq!(
            udc.request(0x83, slot).unwrap().status,
            super::RequestStatus::Cancelled
        );
        assert!(!udc.complete(0x83, slot, 0, false));
        assert!(udc.release(0x83, slot));
        assert!(udc.disable_endpoint(0x83));
        assert!(udc.queue(0x83, 1).is_none());
    }

    #[test]
    fn standard_interface_requests_require_configuration() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        assert_eq!(
            ep0.on_setup([0x81, 10, 0, 0, 0, 0, 1, 0], &mut response),
            ControlAction::Stall
        );
        let _ = ep0.on_setup([0, 9, 1, 0, 0, 0, 0, 0], &mut response);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert_eq!(
            ep0.on_setup([0x81, 10, 0, 0, 0, 0, 1, 0], &mut response),
            ControlAction::DataIn(1)
        );
        assert_eq!(response[0], 0);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
    }

    #[test]
    fn endpoint_halt_commits_after_status_and_is_reported() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0; 512];
        let _ = ep0.on_setup([0, 9, 1, 0, 0, 0, 0, 0], &mut response);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert_eq!(
            ep0.on_setup([0x02, 3, 0, 0, 0x02, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        assert_eq!(ep0.on_transfer_complete(), ControlAction::SetHalt(0x02));
        assert_eq!(
            ep0.on_setup([0x82, 0, 0, 0, 0x02, 0, 2, 0], &mut response),
            ControlAction::DataIn(2)
        );
        assert_eq!(&response[..2], &[1, 0]);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        assert_eq!(
            ep0.on_setup([0x02, 1, 0, 0, 0x02, 0, 0, 0], &mut response),
            ControlAction::StatusIn
        );
        assert_eq!(ep0.on_transfer_complete(), ControlAction::ClearHalt(0x02));
    }

    #[test]
    fn gsi_ring_shape_matches_android_in_and_out_layouts() {
        assert_eq!(
            gsi_ring_shape(true, GSI_DEFAULT_NUM_BUFFERS),
            Some(GsiRingShape {
                num_trbs: 10,
                first_buffer_trb: 5,
                data_trbs: 4,
            })
        );
        assert_eq!(
            gsi_ring_shape(false, GSI_DEFAULT_NUM_BUFFERS),
            Some(GsiRingShape {
                num_trbs: 6,
                first_buffer_trb: 1,
                data_trbs: 4,
            })
        );
        assert_eq!(gsi_ring_shape(true, 0), None);
    }
}
