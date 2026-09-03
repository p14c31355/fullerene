//! Linux usbcore `hub_port_init()` + `usb_enumerate_device()` enumeration
//! host, driven against Fullerene's `Ep0Simulator` gadget.
//!
//! Source: Linux v6.6 `drivers/usb/core/hub.c` (hub_port_init 4795, 
//! get_bMaxPacketSize0 4731, hub_set_address 4645, usb_enumerate_device 2423),
//! `drivers/usb/core/message.c` (usb_get_descriptor 781, usb_get_string 833,
//! usb_string 968, usb_get_device_descriptor 1056), and
//! `drivers/usb/core/config.c` (usb_get_configuration 861,
//! usb_get_bos_descriptor 978). Fetched from
//! git.kernel.org and mirrored in tmp/linux-{hub,message,config}.c.
//!
//! The transport is modeled as a byte-accurate control transfer: the host
//! submits the SETUP packet and drains the data stage through the same
//! `ControlAction`/response-buffer protocol the real hardware wrapper uses,
//! so a PASS exercises exactly the EP0 code the Bramble gadget runs.

use super::usb_protocol::{ControlAction, Ep0Simulator};

const USB_REQ_GET_DESCRIPTOR: u8 = 6;
const USB_REQ_SET_ADDRESS: u8 = 5;
const USB_REQ_SET_CONFIGURATION: u8 = 9;
const USB_REQ_GET_STATUS: u8 = 0;

const USB_DT_DEVICE: u8 = 1;
const USB_DT_CONFIG: u8 = 2;
const USB_DT_STRING: u8 = 3;
const USB_DT_BOS: u8 = 15;

/// Mirror of the Linux hub-side device bookkeeping that enumeration reads.
struct VirtualHostDevice {
    /// bMaxPacketSize0 learned from the first (64-byte) descriptor read.
    ep0_max_packet: u16,
    /// The devnum the host assigned through SET_ADDRESS.
    devnum: u8,
    /// Full 18-byte device descriptor cached by usb_get_device_descriptor().
    device_descriptor: Option<[u8; 18]>,
    /// Configuration descriptor buffer from usb_get_configuration().
    config_descriptor: Vec<u8>,
    /// BOS descriptor buffer from usb_get_bos_descriptor().
    bos_descriptor: Vec<u8>,
    /// usb_cache_string() results keyed by descriptor index.
    strings: [(u8, Vec<u8>); 3],
}

impl VirtualHostDevice {
    fn new() -> Self {
        Self {
            ep0_max_packet: 64,
            devnum: 0,
            device_descriptor: None,
            config_descriptor: Vec::new(),
            bos_descriptor: Vec::new(),
            strings: [(0, Vec::new()), (0, Vec::new()), (0, Vec::new())],
        }
    }
}

/// Result of a full hub enumeration pass.
struct EnumerationOutcome {
    ok: bool,
    trace: heapless_trace::Trace,
}

/// Fixed-capacity log so the test stays no-alloc besides descriptor buffers.
mod heapless_trace {
    pub struct Trace {
        lines: [Option<&'static str>; 24],
        count: usize,
    }

    impl Trace {
        pub const fn new() -> Self {
            Self {
                lines: [None; 24],
                count: 0,
            }
        }

        pub fn push(&mut self, line: &'static str) {
            if self.count < self.lines.len() {
                self.lines[self.count] = Some(line);
                self.count += 1;
            }
        }

        pub fn lines(&self) -> impl Iterator<Item = &'static str> {
            self.lines[..self.count].iter().map(|line| line.unwrap())
        }
    }
}

/// One host-to-device control transfer against the gadget.
///
/// `usb_control_msg()` shape: SETUP always precedes the data stage, and a
/// zero-length status stage always terminates the transfer. The data stage
/// direction follows the request; the response buffer is the device's
/// endpoint buffer.
fn control_transfer(
    ep0: &mut Ep0Simulator,
    setup: [u8; 8],
    response: &mut [u8],
) -> Result<usize, &'static str> {
    let action = ep0.on_setup(setup, response);
    match action {
        ControlAction::DataIn(len) => {
            // Data stage done: the device reports the completion and the host
            // then drives the zero-length OUT status stage.
            assert_eq!(
                ep0.on_transfer_complete(),
                ControlAction::StatusOut,
                "IN data stage must be followed by an OUT status stage"
            );
            assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
            Ok(len)
        }
        ControlAction::StatusIn => {
            // No-data control transfer: status stage is IN from the device.
            assert_eq!(
                ep0.on_transfer_complete(),
                ControlAction::Setup,
                "SET_ADDRESS/SET_CONFIGURATION status must return to Setup"
            );
            Ok(0)
        }
        ControlAction::Stall => Err("endpoint stalled the request"),
        other => Err(match other {
            ControlAction::StatusOut => "unexpected OUT status for a host read",
            _ => "unexpected EP0 action",
        }),
    }
}

/// Linux hub.c get_bMaxPacketSize0(): read the device descriptor with a
/// 64-byte buffer (GET_DESCRIPTOR_BUFSIZE) and validate bMaxPacketSize0.
fn get_b_max_packet_size0(
    ep0: &mut Ep0Simulator,
    response: &mut [u8],
    trace: &mut heapless_trace::Trace,
) -> Result<u16, &'static str> {
    let setup = [
        0x80,
        USB_REQ_GET_DESCRIPTOR,
        0,
        USB_DT_DEVICE,
        0,
        0,
        64,
        0,
    ];
    let len = control_transfer(ep0, setup, response)?;
    if len < 8 {
        trace.push("device descriptor read/64: too short");
        return Err("device descriptor read/64, error -EPROTO");
    }
    let maxp0 = response[7] as u16;
    match maxp0 {
        8 | 16 | 32 | 64 | 9 => {
            trace.push("get_bMaxPacketSize0: valid bMaxPacketSize0");
            Ok(maxp0)
        }
        _ => Err("Invalid ep0 maxpacket"),
    }
}

/// Linux hub.c hub_set_address(): SET_ADDRESS then the device leaves the
/// default state. The hub issues the request to address 0.
fn hub_set_address(
    ep0: &mut Ep0Simulator,
    devnum: u8,
    response: &mut [u8],
) -> Result<(), &'static str> {
    let setup = [0, USB_REQ_SET_ADDRESS, devnum, 0, 0, 0, 0, 0];
    control_transfer(ep0, setup, response)?;
    Ok(())
}

/// Linux message.c usb_get_descriptor(): GET_DESCRIPTOR(type, index) with
/// three retries on zero-length results.
fn usb_get_descriptor(
    ep0: &mut Ep0Simulator,
    dtype: u8,
    index: u8,
    size: u16,
    response: &mut [u8],
    _trace: &mut heapless_trace::Trace,
) -> Result<usize, &'static str> {
    let mut last: Result<usize, &'static str> = Err("no attempt");
    for _ in 0..3 {
        let setup = [
            0x80,
            USB_REQ_GET_DESCRIPTOR,
            index,
            dtype,
            0,
            0,
            (size & 0xff) as u8,
            (size >> 8) as u8,
        ];
        match control_transfer(ep0, setup, response) {
            Ok(0) => last = Err("zero-length descriptor read"),
            Ok(len) => {
                if len > 1 && response[1] != dtype {
                    last = Err("descriptor type mismatch (-ENODATA)");
                } else {
                    return Ok(len);
                }
            }
            Err(err) => last = Err(err),
        }
    }
    last
}

/// Linux message.c usb_get_device_descriptor(): the 18-byte whole read.
fn usb_get_device_descriptor(
    ep0: &mut Ep0Simulator,
    response: &mut [u8],
    trace: &mut heapless_trace::Trace,
) -> Result<[u8; 18], &'static str> {
    let len = usb_get_descriptor(ep0, USB_DT_DEVICE, 0, 18, response, trace)?;
    if len != 18 {
        return Err("device descriptor read/all, error -EMSGSIZE");
    }
    let mut descriptor = [0u8; 18];
    descriptor.copy_from_slice(&response[..18]);
    Ok(descriptor)
}

/// Linux config.c usb_get_configuration(): read the 9-byte configuration
/// header, then re-read wTotalLength bytes.
fn usb_get_configuration(
    ep0: &mut Ep0Simulator,
    device: &mut VirtualHostDevice,
    response: &mut [u8],
    trace: &mut heapless_trace::Trace,
) -> Result<(), &'static str> {
    let len = usb_get_descriptor(ep0, USB_DT_CONFIG, 0, 9, response, trace)?;
    if len < 9 {
        return Err("unable to read config index 0 descriptor/start");
    }
    let total_length =
        u16::from_le_bytes([response[2], response[3]]).max(9) as usize;
    let len = usb_get_descriptor(
        ep0,
        USB_DT_CONFIG,
        0,
        total_length as u16,
        response,
        trace,
    )?;
    if len < total_length {
        return Err("unable to read config index 0 descriptor/all");
    }
    device.config_descriptor = response[..len].to_vec();
    Ok(())
}

/// Linux config.c usb_get_bos_descriptor(): read the BOS header, then the
/// whole wTotalLength set. Only reached for bcdUSB >= 0x0201.
fn usb_get_bos_descriptor(
    ep0: &mut Ep0Simulator,
    device: &mut VirtualHostDevice,
    response: &mut [u8],
    trace: &mut heapless_trace::Trace,
) -> Result<(), &'static str> {
    let len = usb_get_descriptor(ep0, USB_DT_BOS, 0, 5, response, trace)?;
    if len < 5 || response[0] < 5 {
        return Err("unable to get BOS descriptor or descriptor too short");
    }
    let total_length = u16::from_le_bytes([response[2], response[3]]) as usize;
    if total_length < 5 {
        return Err("BOS wTotalLength too short");
    }
    let len = usb_get_descriptor(
        ep0,
        USB_DT_BOS,
        0,
        total_length as u16,
        response,
        trace,
    )?;
    if len < total_length {
        return Err("unable to get BOS descriptor set");
    }
    device.bos_descriptor = response[..len].to_vec();
    Ok(())
}

/// Linux message.c usb_get_string() + usb_string(): read the language table
/// first, then each string with the first listed langid.
fn usb_get_string(
    ep0: &mut Ep0Simulator,
    index: u8,
    langid: u16,
    size: u16,
    response: &mut [u8],
    _trace: &mut heapless_trace::Trace,
) -> Result<usize, &'static str> {
    let setup = [
        0x80,
        USB_REQ_GET_DESCRIPTOR,
        index,
        USB_DT_STRING,
        (langid & 0xff) as u8,
        (langid >> 8) as u8,
        (size & 0xff) as u8,
        (size >> 8) as u8,
    ];
    let mut last: Result<usize, &'static str> = Err("no attempt");
    for _ in 0..3 {
        match control_transfer(ep0, setup, response) {
            Ok(0) => last = Err("zero-length string read"),
            Ok(len) => {
                if len > 1 && response[1] != USB_DT_STRING {
                    last = Err("string type mismatch (-ENODATA)");
                } else {
                    return Ok(len);
                }
            }
            Err(err) => last = Err(err),
        }
    }
    last
}

/// usb_get_langid() equivalent: string descriptor 0 selects the language.
fn usb_get_langid(
    ep0: &mut Ep0Simulator,
    response: &mut [u8],
    trace: &mut heapless_trace::Trace,
) -> Result<u16, &'static str> {
    let len = usb_get_string(ep0, 0, 0, 255, response, trace).or_else(|_| {
        trace.push("string descriptor 0 read error, retrying short");
        usb_get_string(ep0, 0, 0, 2, response, trace)
    })?;
    if len < 4 {
        // Linux defaults to English on a malformed table.
        return Ok(0x0409);
    }
    Ok(u16::from_le_bytes([response[2], response[3]]))
}

/// Linux message.c usb_cache_string(): cache one UTF-16LE string descriptor.
fn usb_cache_string(
    ep0: &mut Ep0Simulator,
    index: u8,
    langid: u16,
    response: &mut [u8],
    trace: &mut heapless_trace::Trace,
) -> Option<Vec<u8>> {
    if index == 0 {
        return None;
    }
    let len = usb_get_string(ep0, index, langid, 255, response, trace).ok()?;
    if len < 2 || response[1] != USB_DT_STRING {
        return None;
    }
    Some(response[..len].to_vec())
}

/// Linux hub.c hub_port_init() + usb_new_device()→usb_enumerate_device(),
/// the exact sequence a Linux host runs after a port reset:
///  1. (new scheme) GET_DESCRIPTOR(Device) 64 bytes at address 0
///  2. SET_ADDRESS
///  3. usb_get_device_descriptor(): 18-byte GET_DESCRIPTOR(Device)
///  4. BOS read when bcdUSB >= 0x0201
///  5. usb_get_configuration(): 9-byte header then wTotalLength bytes
///  6. usb_cache_string() for iManufacturer/iProduct/iSerialNumber
///  7. SET_CONFIGURATION(1)
fn enumerate_fullerene_gadget(
    ep0: &mut Ep0Simulator,
    device: &mut VirtualHostDevice,
) -> EnumerationOutcome {
    let mut trace = heapless_trace::Trace::new();
    let mut response = [0u8; 512];

    // Step 1: the new-scheme 64-byte descriptor read at address 0.
    match get_b_max_packet_size0(ep0, &mut response, &mut trace) {
        Ok(maxp0) => device.ep0_max_packet = maxp0,
        Err(_) => {
            trace.push("hub_port_init: maxpacket read failed");
            return EnumerationOutcome { ok: false, trace };
        }
    }

    // Step 2: SET_ADDRESS. Linux picks devnum >= 2 (root hub is 1); use 7 so
    // the address is visible in the trace.
    if hub_set_address(ep0, 7, &mut response).is_err() {
        trace.push("device not accepting address 7");
        return EnumerationOutcome { ok: false, trace };
    }
    device.devnum = 7;
    trace.push("SET_ADDRESS 7 accepted");

    // Step 3: the full 18-byte device descriptor read at the new address.
    let descriptor = match usb_get_device_descriptor(ep0, &mut response, &mut trace) {
        Ok(descriptor) => descriptor,
        Err(_) => {
            trace.push("device descriptor read/all failed");
            return EnumerationOutcome { ok: false, trace };
        }
    };
    let bcd_usb = u16::from_le_bytes([descriptor[2], descriptor[3]]);
    device.device_descriptor = Some(descriptor);
    trace.push("device descriptor read/all ok");

    // Step 4: BOS for bcdUSB >= 0x0201 (usb_get_bos_descriptor).
    if bcd_usb >= 0x0201 {
        if usb_get_bos_descriptor(ep0, device, &mut response, &mut trace).is_ok() {
            trace.push("BOS descriptor set ok");
        } else {
            // Linux treats a BOS failure as non-fatal (lpm_capable stays
            // false), so continue.
            trace.push("BOS read failed; continuing without LPM");
        }
    }

    // Step 5: configuration descriptors.
    if usb_get_configuration(ep0, device, &mut response, &mut trace).is_err() {
        trace.push("can't read configurations");
        return EnumerationOutcome { ok: false, trace };
    }
    trace.push("configuration descriptor ok");

    // Step 6: strings. Linux caches iManufacturer/iProduct/iSerialNumber.
    let langid = match usb_get_langid(ep0, &mut response, &mut trace) {
        Ok(langid) => langid,
        Err(_) => {
            trace.push("string descriptor 0 read failed; strings skipped");
            0
        }
    };
    if langid != 0 {
        let descriptor = device.device_descriptor.unwrap();
        for (slot, index) in device.strings.iter_mut().zip([
            descriptor[14], // iManufacturer
            descriptor[15], // iProduct
            descriptor[16], // iSerialNumber
        ]) {
            if let Some(string) = usb_cache_string(ep0, index, langid, &mut response, &mut trace) {
                *slot = (index, string);
            }
        }
        trace.push("string descriptors cached");
    }

    // Step 7: SET_CONFIGURATION(1) through usb_control_msg.
    let setup = [0, USB_REQ_SET_CONFIGURATION, 1, 0, 0, 0, 0, 0];
    if control_transfer(ep0, setup, &mut response).is_err() {
        trace.push("SET_CONFIGURATION rejected");
        return EnumerationOutcome { ok: false, trace };
    }
    if !ep0.configured() {
        trace.push("gadget did not commit configuration");
        return EnumerationOutcome { ok: false, trace };
    }
    trace.push("SET_CONFIGURATION committed");

    // The hub also validates GET_STATUS after configuration.
    let setup = [0x80, USB_REQ_GET_STATUS, 0, 0, 0, 0, 2, 0];
    match control_transfer(ep0, setup, &mut response) {
        Ok(2) => trace.push("GET_STATUS ok"),
        _ => {
            trace.push("GET_STATUS failed");
            return EnumerationOutcome { ok: false, trace };
        }
    }

    EnumerationOutcome { ok: true, trace }
}

/// The Linux-visible identity of the enumerated device: idVendor/idProduct
/// must be Fullerene's `1234:0001` and the cached strings must include the
/// serial string the device descriptor's iSerialNumber points at.
fn linux_identity_matches(device: &VirtualHostDevice) -> bool {
    let Some(descriptor) = device.device_descriptor else {
        return false;
    };
    let id_vendor = u16::from_le_bytes([descriptor[8], descriptor[9]]);
    let id_product = u16::from_le_bytes([descriptor[10], descriptor[11]]);
    // Descriptor layout: idVendor at offset 8, idProduct at 10,
    // iSerialNumber at 16. The strings array caches (index, bytes) pairs.
    let serial_index = descriptor[16];
    id_vendor == 0x1234
        && id_product == 0x0001
        && serial_index != 0
        && device
            .strings
            .iter()
            .any(|(index, _)| *index == serial_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Linux hub enumeration sequence must fully configure Fullerene's
    /// EP0 gadget and read back its `1234:0001` identity.
    #[test]
    fn linux_hub_enumeration_reaches_fullerene_1234_0001() {
        let mut ep0 = Ep0Simulator::new();
        let mut device = VirtualHostDevice::new();

        let outcome = enumerate_fullerene_gadget(&mut ep0, &mut device);
        for line in outcome.trace.lines() {
            eprintln!("linux-enum: {line}");
        }
        assert!(outcome.ok, "Linux hub enumeration failed");

        assert_eq!(ep0.address(), 7, "SET_ADDRESS must commit");
        assert!(ep0.configured(), "SET_CONFIGURATION must commit");

        let descriptor = device.device_descriptor.expect("device descriptor");
        assert_eq!(descriptor[0], 18, "bLength");
        assert_eq!(descriptor[1], USB_DT_DEVICE, "bDescriptorType");
        let id_vendor = u16::from_le_bytes([descriptor[8], descriptor[9]]);
        let id_product = u16::from_le_bytes([descriptor[10], descriptor[11]]);
        assert_eq!(id_vendor, 0x1234, "idVendor must be Fullerene's 1234");
        assert_eq!(id_product, 0x0001, "idProduct must be Fullerene's 0001");

        let config = &device.config_descriptor;
        assert_eq!(config[1], USB_DT_CONFIG, "config descriptor type");
        assert_eq!(
            u16::from_le_bytes([config[2], config[3]]),
            config.len() as u16,
            "wTotalLength must match the delivered bytes"
        );

        // Linux reads BOS only when bcdUSB >= 0x0201; the High-Speed
        // descriptor advertises 0x0200, so the BOS buffer stays empty here.
        let bcd_usb = u16::from_le_bytes([descriptor[2], descriptor[3]]);
        if bcd_usb >= 0x0201 {
            let bos = &device.bos_descriptor;
            assert_eq!(bos[1], USB_DT_BOS, "BOS descriptor type");
            assert_eq!(
                u16::from_le_bytes([bos[2], bos[3]]),
                bos.len() as u16,
                "BOS wTotalLength must match the delivered bytes"
            );
        } else {
            assert!(
                device.bos_descriptor.is_empty(),
                "BOS must not be fetched below bcdUSB 0x0201"
            );
        }

        assert!(linux_identity_matches(&device));
    }

    /// The old enumeration scheme (SET_ADDRESS first, then an 8-byte
    /// descriptor read) must also work: Linux falls back to it when the new
    /// scheme fails.
    #[test]
    fn linux_old_scheme_enumeration_also_reaches_1234_0001() {
        let mut ep0 = Ep0Simulator::new();
        let mut device = VirtualHostDevice::new();
        let mut response = [0u8; 512];

        // Old scheme: SET_ADDRESS first.
        assert!(hub_set_address(&mut ep0, 7, &mut response).is_ok());
        device.devnum = 7;

        // Then an 8-byte descriptor read for maxpacket (get_bMaxPacketSize0
        // with size 8), completed through its data and status stages.
        let setup = [0x80, USB_REQ_GET_DESCRIPTOR, 0, USB_DT_DEVICE, 0, 0, 8, 0];
        let action = ep0.on_setup(setup, &mut response);
        assert_eq!(
            action,
            ControlAction::DataIn(8),
            "8-byte read must return 8 bytes"
        );
        assert_eq!(&response[..2], &[18, 1]);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);

        // Finish through the shared path.
        let outcome = enumerate_fullerene_gadget(&mut ep0, &mut device);
        for line in outcome.trace.lines() {
            eprintln!("linux-enum-old: {line}");
        }
        assert!(outcome.ok, "old-scheme continuation failed");
        assert_eq!(ep0.address(), 7);
        assert!(ep0.configured());
    }

    /// A USB reset between transactions must return the gadget to the
    /// default state without losing enumerability: the hub re-enumerates
    /// after a port reset and must still reach 1234:0001.
    #[test]
    fn gadget_recovers_enumerability_after_a_usb_reset() {
        let mut ep0 = Ep0Simulator::new();
        let mut device = VirtualHostDevice::new();

        let first = enumerate_fullerene_gadget(&mut ep0, &mut device);
        for line in first.trace.lines() {
            eprintln!("linux-enum-first: {line}");
        }
        assert!(first.ok, "first enumeration failed");

        // USB reset: the gadget returns to default state (address 0,
        // unconfigured) per the gadget core contract.
        ep0.reset();
        assert_eq!(ep0.address(), 0);
        assert!(!ep0.configured());

        let second = enumerate_fullerene_gadget(&mut ep0, &mut device);
        for line in second.trace.lines() {
            eprintln!("linux-enum-second: {line}");
        }
        assert!(second.ok, "re-enumeration after USB reset failed");
        assert_eq!(ep0.address(), 7);
        assert!(ep0.configured());
    }

    /// Zero-length status handling: a control read whose wLength equals the
    /// descriptor length exactly must still terminate with a status stage
    /// (Linux usb_get_descriptor reads exactly wTotalLength bytes).
    #[test]
    fn exact_length_config_read_terminates_cleanly() {
        let mut ep0 = Ep0Simulator::new();
        let mut response = [0u8; 512];

        let setup = [0x80, USB_REQ_GET_DESCRIPTOR, 0, USB_DT_CONFIG, 0, 0, 32, 0];
        assert_eq!(
            ep0.on_setup(setup, &mut response),
            ControlAction::DataIn(32)
        );
        assert_eq!(ep0.on_transfer_complete(), ControlAction::StatusOut);
        assert_eq!(ep0.on_transfer_complete(), ControlAction::Setup);
        // The gadget must be ready for the next SETUP immediately.
        assert_eq!(
            ep0.on_setup([0x80, USB_REQ_GET_DESCRIPTOR, 0, USB_DT_DEVICE, 0, 0, 18, 0], &mut response),
            ControlAction::DataIn(18)
        );
    }
}
