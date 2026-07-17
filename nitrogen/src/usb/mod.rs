//! USB subsystem — host controller, hub, and mass-storage drivers.
//!
//! # Architecture
//!
//! ```text
//! USBContext
//!  ├── ControllerManager   (PCI scan, init, polling)
//!  ├── StorageManager      (discovered block-device metadata)
//!  ├── HostController (trait)
//!  │    ├── XhciContext
//!  │    └── EhciContext
//!  └── DriverManager       (class-based driver attach)
//! ```

// Top‑level API
pub mod context;

// Common host-controller abstraction
pub mod host_controller;

// DMA allocation helpers
mod dma;

// EHCI sub-context modules
pub mod ehci;

// xHCI sub-context modules
pub mod xhci;

// USB class drivers
pub mod disk;
pub mod hub;
pub mod msd;
pub mod scsi;
pub mod usb_bus;

/// MMIO window mapped for a USB host-controller BAR.
pub(crate) const HOST_CONTROLLER_BAR_SIZE: usize = 0x1_0000;

/// USB device speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbSpeed {
    Low,        // 1.5 Mbps
    Full,       // 12 Mbps
    High,       // 480 Mbps
    SuperSpeed, // 5 Gbps (USB 3.x)
}

impl UsbSpeed {
    pub fn from_portsc(portsc: u32) -> Self {
        match (portsc >> 26) & 3 {
            0 => UsbSpeed::Full,
            1 => UsbSpeed::Low,
            2 => UsbSpeed::High,
            _ => UsbSpeed::Full,
        }
    }
}

/// Standard USB device request (8-byte setup packet).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct UsbSetupPacket {
    pub bm_request_type: u8,
    pub b_request: u8,
    pub w_value: u16,
    pub w_index: u16,
    pub w_length: u16,
}

/// Standard request codes.
pub const REQ_GET_DESCRIPTOR: u8 = 6;
pub const REQ_SET_ADDRESS: u8 = 5;
pub const REQ_SET_CONFIGURATION: u8 = 9;

/// Descriptor types.
pub const DESC_DEVICE: u8 = 1;
pub const DESC_CONFIGURATION: u8 = 2;
pub const DESC_STRING: u8 = 3;
pub const DESC_INTERFACE: u8 = 4;
pub const DESC_ENDPOINT: u8 = 5;

/// Standard endpoint directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbDirection {
    Out = 0,
    In = 1,
}

/// Standard transfer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbXferType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

/// Endpoint descriptor (from device).
#[derive(Debug, Clone, Copy)]
pub struct UsbEndpointDesc {
    pub b_endpoint_address: u8,
    pub bm_attributes: u8,
    pub w_max_packet_size: u16,
    pub b_interval: u8,
}

impl UsbEndpointDesc {
    pub fn direction(&self) -> UsbDirection {
        if self.b_endpoint_address & 0x80 != 0 {
            UsbDirection::In
        } else {
            UsbDirection::Out
        }
    }
    pub fn number(&self) -> u8 {
        self.b_endpoint_address & 0x0F
    }
    pub fn xfer_type(&self) -> UsbXferType {
        match self.bm_attributes & 3 {
            0 => UsbXferType::Control,
            1 => UsbXferType::Isochronous,
            2 => UsbXferType::Bulk,
            _ => UsbXferType::Interrupt,
        }
    }
}

/// USB device descriptor (from device).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct UsbDeviceDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub bcd_usb: u16,
    pub b_device_class: u8,
    pub b_device_subclass: u8,
    pub b_device_protocol: u8,
    pub b_max_packet_size_0: u8,
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub i_manufacturer: u8,
    pub i_product: u8,
    pub i_serial_number: u8,
    pub b_num_configurations: u8,
}

/// Configuration descriptor header.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct UsbConfigDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub w_total_length: u16,
    pub b_num_interfaces: u8,
    pub b_configuration_value: u8,
    pub i_configuration: u8,
    pub bm_attributes: u8,
    pub b_max_power: u8,
}

/// Interface descriptor.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct UsbInterfaceDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub b_interface_number: u8,
    pub b_alternate_setting: u8,
    pub b_num_endpoints: u8,
    pub b_interface_class: u8,
    pub b_interface_subclass: u8,
    pub b_interface_protocol: u8,
    pub i_interface: u8,
}

/// Mass-storage class codes.
pub const MSC_CLASS: u8 = 0x08;
pub const MSC_SUBCLASS_SCSI: u8 = 0x06;
pub const MSC_PROTOCOL_BOT: u8 = 0x50;

/// Common endpoint addresses for mass storage (bulk-only).
pub const EP_BULK_OUT: u8 = 0x02; // typical
pub const EP_BULK_IN: u8 = 0x82; // typical (bit 7 = IN)

/// A USB device discovered on the bus.
#[derive(Debug, Clone)]
pub struct UsbDevice {
    pub address: u8,
    pub speed: UsbSpeed,
    pub max_packet_size_0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub configurations: u8,
    pub endpoints: alloc::vec::Vec<UsbEndpointDesc>,
    /// Root-hub port index this device is connected to.
    pub port_index: u32,
}

impl UsbDevice {
    pub fn is_mass_storage(&self) -> bool {
        self.device_class == MSC_CLASS
    }
}
