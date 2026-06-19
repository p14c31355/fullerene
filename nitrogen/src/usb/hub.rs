//! USB hub driver — port status change detection and device enumeration.
//!
//! After a hub or root port detects a connection, the hub driver:
//! 1. Resets the port
//! 2. Enables the port
//! 3. Retrieves the device descriptor (to know max packet size)
//! 4. Assigns a device address
//! 5. Retrieves the full configuration

use crate::usb::{
    UsbDevice, UsbDeviceDescriptor, UsbSetupPacket, UsbEndpointDesc,
    DESC_DEVICE, DESC_CONFIGURATION, DESC_ENDPOINT,
    REQ_GET_DESCRIPTOR, REQ_SET_ADDRESS, REQ_SET_CONFIGURATION,
    UsbConfigDescriptor,
};
use alloc::vec::Vec;

/// Maximum enumeration attempts.
const MAX_ENUM_ATTEMPTS: u32 = 3;

/// Enumerate a newly connected device on the given port.
///
/// `control_fn` is a callback that performs a control transfer:
///   fn(dev_addr, endpoint, setup_packet, buffer) -> Result<usize, &'static str>
///
/// Returns the fully enumerated UsbDevice, or an error.
pub fn enumerate_device(
    control_fn: &mut dyn FnMut(u8, u8, &UsbSetupPacket, &mut [u8]) -> Result<usize, &'static str>,
) -> Result<UsbDevice, &'static str> {
    // Step 1: Get device descriptor (only first 8 bytes for max packet size)
    let mut desc_buf = [0u8; 64];
    let setup = UsbSetupPacket {
        bm_request_type: 0x80, // device-to-host, standard, device
        b_request: REQ_GET_DESCRIPTOR,
        w_value: (DESC_DEVICE as u16) << 8,
        w_index: 0,
        w_length: 64,
    };
    let n = control_fn(0, 0, &setup, &mut desc_buf).map_err(|_| "GET_DESCRIPTOR failed")?;
    if n < 8 {
        return Err("descriptor too short");
    }

    // SAFETY: UsbDeviceDescriptor is #[repr(C, packed)]. desc_buf was filled by a
    // control-IN transfer of 64 bytes, and the first 8 bytes match the USB device
    // descriptor layout per USB spec §9.6.1.
    let dev_desc: &UsbDeviceDescriptor = unsafe { &*(desc_buf.as_ptr() as *const UsbDeviceDescriptor) };
    let max_pkt = dev_desc.b_max_packet_size_0;

    // Step 2: Assign address
    let assigned_addr = 1; // simple: first device gets address 1
    let setup = UsbSetupPacket {
        bm_request_type: 0x00, // host-to-device, standard, device
        b_request: REQ_SET_ADDRESS,
        w_value: assigned_addr as u16,
        w_index: 0,
        w_length: 0,
    };
    control_fn(0, 0, &setup, &mut []).map_err(|_| "SET_ADDRESS failed")?;

    // Delay for address to take effect
    for _ in 0..1000 {
        // SAFETY: Reading a dummy volatile u8 to introduce a small delay
        // after SET_ADDRESS, allowing the device to settle before subsequent
        // control transfers (USB2 spec §9.2.6.3 recommends 2ms).
        unsafe { core::ptr::read_volatile(&0u8); }
    }

    // Step 3: Get full device descriptor (18 bytes) at new address
    let mut dev_desc_full = [0u8; 18];
    let setup = UsbSetupPacket {
        bm_request_type: 0x80,
        b_request: REQ_GET_DESCRIPTOR,
        w_value: (DESC_DEVICE as u16) << 8,
        w_index: 0,
        w_length: 18,
    };
    control_fn(assigned_addr, 0, &setup, &mut dev_desc_full).map_err(|_| "GET_DESCRIPTOR full failed")?;
    // SAFETY: Same layout guarantee as above; dev_desc_full is exactly 18 bytes
    // (the full USB device descriptor per USB spec §9.6.1).
    let dev_desc: &UsbDeviceDescriptor = unsafe { &*(dev_desc_full.as_ptr() as *const UsbDeviceDescriptor) };

    // Step 4: Get configuration descriptor to learn total length
    let mut cfg_hdr_buf = [0u8; 9];
    let setup = UsbSetupPacket {
        bm_request_type: 0x80,
        b_request: REQ_GET_DESCRIPTOR,
        w_value: (DESC_CONFIGURATION as u16) << 8,
        w_index: 0,
        w_length: 9,
    };
    control_fn(assigned_addr, 0, &setup, &mut cfg_hdr_buf).map_err(|_| "GET_CONFIG_DESC hdr failed")?;
    // SAFETY: UsbConfigDescriptor is #[repr(C, packed)], the first 9 bytes of
    // every configuration descriptor match this layout per USB spec §9.6.3.
    let cfg_desc: &UsbConfigDescriptor = unsafe { &*(cfg_hdr_buf.as_ptr() as *const UsbConfigDescriptor) };
    let total_len = cfg_desc.w_total_length as usize;

    // Step 5: Get full configuration descriptor
    let mut cfg_buf = alloc::vec![0u8; total_len];
    let setup = UsbSetupPacket {
        bm_request_type: 0x80,
        b_request: REQ_GET_DESCRIPTOR,
        w_value: (DESC_CONFIGURATION as u16) << 8,
        w_index: 0,
        w_length: total_len as u16,
    };
    control_fn(assigned_addr, 0, &setup, &mut cfg_buf).map_err(|_| "GET_CONFIG_DESC full failed")?;

    // Step 6: Parse endpoints from the configuration descriptor
    let mut endpoints = Vec::new();
    let mut offset = cfg_desc.b_length as usize;
    while offset < total_len {
        let desc_len = cfg_buf[offset] as usize;
        if desc_len == 0 { break; }
        let desc_type = cfg_buf[offset + 1];
        if desc_type == DESC_ENDPOINT && desc_len >= 7 {
            endpoints.push(UsbEndpointDesc {
                b_endpoint_address: cfg_buf[offset + 2],
                bm_attributes: cfg_buf[offset + 3],
                w_max_packet_size: u16::from_le_bytes([cfg_buf[offset + 4], cfg_buf[offset + 5]]),
                b_interval: cfg_buf[offset + 6],
            });
        }
        offset += desc_len;
    }

    // Step 7: Set configuration
    let setup = UsbSetupPacket {
        bm_request_type: 0x00,
        b_request: REQ_SET_CONFIGURATION,
        w_value: cfg_desc.b_configuration_value as u16,
        w_index: 0,
        w_length: 0,
    };
    control_fn(assigned_addr, 0, &setup, &mut []).map_err(|_| "SET_CONFIGURATION failed")?;

    Ok(UsbDevice {
        address: assigned_addr,
        speed: crate::usb::UsbSpeed::High, // simplified
        max_packet_size_0: max_pkt,
        vendor_id: dev_desc.id_vendor,
        product_id: dev_desc.id_product,
        device_class: dev_desc.b_device_class,
        device_subclass: dev_desc.b_device_subclass,
        device_protocol: dev_desc.b_device_protocol,
        configurations: dev_desc.b_num_configurations,
        endpoints,
    })
}
