//! Small, transport-independent HID report decoder.
//!
//! Linux keeps HID transport (USB, I2C, Bluetooth, ...) separate from HID
//! report parsing and input-device drivers.  Fullerene follows the same split:
//! this module knows how to turn a report descriptor and one input report into
//! touchpad fields, while the bus driver owns how the bytes are obtained.

use alloc::vec::Vec;

/// Intel LPSS I2C functions used by Linux for Alder Lake-N touch devices.
/// The attached GemiBook report identifies function 0 as `8086:54e8`.
pub const INTEL_LPSS_I2C_DEVICE_IDS: &[u16] = &[0x54e8, 0x54e9, 0x54ea, 0x54eb];
/// ACPI HID reported by the GemiBook XPro N150 for its I2C HID device.
pub const GEMIBOOK_TOUCHPAD_ACPI_HID: &[u8; 7] = b"AMR1399";
/// Linux's ACPI-backed I2C-HID device name observed on the N150.
pub const GEMIBOOK_LINUX_I2C_HID_NAME: &[u8; 11] = b"AMR13992:00";
/// HID identity reported by the Linux live image.
pub const GEMIBOOK_TOUCHPAD_HID_VENDOR_ID: u16 = 0x36b6;
pub const GEMIBOOK_TOUCHPAD_HID_PRODUCT_ID: u16 = 0xc001;

const GENERIC_DESKTOP_PAGE: u16 = 0x01;
const BUTTON_PAGE: u16 = 0x09;
const DIGITIZER_PAGE: u16 = 0x0d;

const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_BUTTON_LEFT: u16 = 0x01;
const USAGE_BUTTON_RIGHT: u16 = 0x02;
const USAGE_TIP_SWITCH: u16 = 0x42;
const USAGE_CONTACT_ID: u16 = 0x51;
const USAGE_CONTACT_COUNT: u16 = 0x54;

/// A single input field in a HID report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidInputField {
    pub report_id: u8,
    pub usage_page: u16,
    pub usage: u16,
    pub bit_offset: u16,
    pub bit_size: u8,
    pub logical_minimum: i32,
    pub logical_maximum: i32,
}

/// Parsed input fields from a HID report descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidReportDescriptor {
    fields: Vec<HidInputField>,
    max_input_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidDescriptorError {
    Truncated,
    InvalidItem,
    ReportTooLarge,
}

#[derive(Clone, Copy)]
struct GlobalState {
    usage_page: u16,
    logical_minimum: i32,
    logical_maximum: i32,
    report_size: u8,
    report_count: u8,
    report_id: u8,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self {
            usage_page: 0,
            logical_minimum: 0,
            logical_maximum: 0,
            report_size: 0,
            report_count: 0,
            report_id: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct LocalState {
    usages: [u16; 16],
    usage_count: usize,
    usage_minimum: Option<u16>,
    usage_maximum: Option<u16>,
}

impl Default for LocalState {
    fn default() -> Self {
        Self {
            usages: [0; 16],
            usage_count: 0,
            usage_minimum: None,
            usage_maximum: None,
        }
    }
}

impl LocalState {
    fn usage_for(&self, index: usize) -> u16 {
        if index < self.usage_count {
            return self.usages[index];
        }
        self.usage_minimum
            .zip(self.usage_maximum)
            .and_then(|(minimum, maximum)| {
                minimum.checked_add(index as u16).filter(|v| *v <= maximum)
            })
            .unwrap_or(0)
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

impl HidReportDescriptor {
    /// Parse the input items needed by generic HID pointers and touchpads.
    ///
    /// Output and feature items are skipped as fields, but input bit offsets
    /// are kept correct.  Long items are ignored according to the HID item
    /// framing rules, just as Linux HID core ignores unknown long tags.
    pub fn parse(bytes: &[u8]) -> Result<Self, HidDescriptorError> {
        let mut offset = 0usize;
        let mut global = GlobalState::default();
        let mut local = LocalState::default();
        let mut report_offsets = [0u16; 256];
        let mut fields = Vec::new();
        let mut max_input_bytes = 0usize;

        while offset < bytes.len() {
            let prefix = bytes[offset];
            offset += 1;
            if prefix == 0xfe {
                let length = *bytes.get(offset).ok_or(HidDescriptorError::Truncated)? as usize;
                offset = offset
                    .checked_add(2 + length)
                    .ok_or(HidDescriptorError::Truncated)?;
                if offset > bytes.len() {
                    return Err(HidDescriptorError::Truncated);
                }
                continue;
            }

            let size = match prefix & 0x03 {
                0 => 0,
                1 => 1,
                2 => 2,
                3 => 4,
                _ => unreachable!(),
            };
            let item_type = (prefix >> 2) & 0x03;
            let tag = (prefix >> 4) & 0x0f;
            let data = bytes
                .get(offset..offset + size)
                .ok_or(HidDescriptorError::Truncated)?;
            offset += size;

            let unsigned = read_unsigned(data)?;
            let signed = read_signed(data)?;
            match (item_type, tag) {
                (1, 0x0) => global.usage_page = unsigned as u16,
                (1, 0x1) => global.logical_minimum = signed,
                (1, 0x2) => global.logical_maximum = signed,
                (1, 0x7) => global.report_size = unsigned as u8,
                (1, 0x8) => {
                    if size != 1 || unsigned == 0 || unsigned > u8::MAX as u32 {
                        return Err(HidDescriptorError::InvalidItem);
                    }
                    global.report_id = unsigned as u8;
                    report_offsets[global.report_id as usize] = 8;
                    local.clear();
                }
                (1, 0x9) => global.report_count = unsigned as u8,
                (2, 0x0) => {
                    if local.usage_count < local.usages.len() {
                        local.usages[local.usage_count] = unsigned as u16;
                        local.usage_count += 1;
                    }
                }
                (2, 0x1) => local.usage_minimum = Some(unsigned as u16),
                (2, 0x2) => local.usage_maximum = Some(unsigned as u16),
                (0, 0x8) => {
                    let input_offset = &mut report_offsets[global.report_id as usize];
                    let field_bits = (global.report_size as u32)
                        .checked_mul(global.report_count as u32)
                        .ok_or(HidDescriptorError::ReportTooLarge)?;
                    let end_bits = (*input_offset as u32)
                        .checked_add(field_bits)
                        .ok_or(HidDescriptorError::ReportTooLarge)?;
                    if end_bits > (u16::MAX as u32) {
                        return Err(HidDescriptorError::ReportTooLarge);
                    }
                    let input_is_constant = unsigned & 0x01 != 0;
                    if !input_is_constant {
                        for index in 0..global.report_count as usize {
                            fields.push(HidInputField {
                                report_id: global.report_id,
                                usage_page: global.usage_page,
                                usage: local.usage_for(index),
                                bit_offset: *input_offset
                                    + index as u16 * global.report_size as u16,
                                bit_size: global.report_size,
                                logical_minimum: global.logical_minimum,
                                logical_maximum: global.logical_maximum,
                            });
                        }
                    }
                    *input_offset = end_bits as u16;
                    max_input_bytes = max_input_bytes.max((end_bits as usize + 7) / 8);
                    local.clear();
                }
                // Collection, End Collection, Output and Feature do not
                // change the input bit layout here.  Physical/unit globals
                // also do not affect extraction; they are nevertheless
                // valid and common in precision-touchpad descriptors.
                (0, 0x9) | (0, 0xa) | (0, 0xc) | (1, 0x3..=0x6) | (1, 0xa) | (1, 0xb) => {}
                _ => return Err(HidDescriptorError::InvalidItem),
            }
        }

        Ok(Self {
            fields,
            max_input_bytes,
        })
    }

    pub fn fields(&self) -> &[HidInputField] {
        &self.fields
    }

    pub fn max_input_bytes(&self) -> usize {
        self.max_input_bytes
    }

    /// Read one signed/unsigned field from an input report.
    pub fn value(&self, field: HidInputField, report: &[u8]) -> Option<i32> {
        let mut bit_offset = field.bit_offset as usize;
        if field.report_id != 0 {
            if report.first().copied()? != field.report_id {
                return None;
            }
            bit_offset += 0;
        }
        let end = bit_offset.checked_add(field.bit_size as usize)?;
        if end > report.len().checked_mul(8)? || field.bit_size == 0 || field.bit_size > 32 {
            return None;
        }
        let mut value = 0u32;
        for bit in 0..field.bit_size as usize {
            let source = bit_offset + bit;
            value |= (((report[source / 8] >> (source % 8)) & 1) as u32) << bit;
        }
        if field.logical_minimum < 0 && field.bit_size != 0 {
            let sign = 1u32 << (field.bit_size - 1);
            if value & sign != 0 {
                if field.bit_size < 32 {
                    value |= u32::MAX << field.bit_size;
                }
            }
        }
        Some(value as i32)
    }
}

fn read_unsigned(data: &[u8]) -> Result<u32, HidDescriptorError> {
    match data.len() {
        0 => Ok(0),
        1 => Ok(data[0] as u32),
        2 => Ok(u16::from_le_bytes([data[0], data[1]]) as u32),
        4 => Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]])),
        _ => Err(HidDescriptorError::InvalidItem),
    }
}

fn read_signed(data: &[u8]) -> Result<i32, HidDescriptorError> {
    match data.len() {
        0 => Ok(0),
        1 => Ok(i8::from_le_bytes([data[0]]) as i32),
        2 => Ok(i16::from_le_bytes([data[0], data[1]]) as i32),
        4 => Ok(i32::from_le_bytes([data[0], data[1], data[2], data[3]])),
        _ => Err(HidDescriptorError::InvalidItem),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchpadFieldMap {
    pub x: HidInputField,
    pub y: HidInputField,
    pub left_button: Option<HidInputField>,
    pub right_button: Option<HidInputField>,
    pub tip_switch: Option<HidInputField>,
    pub contact_id: Option<HidInputField>,
    pub contact_count: Option<HidInputField>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchpadReport {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
    pub in_contact: bool,
}

impl HidReportDescriptor {
    /// Find the standard HID usages used by Windows/Linux precision touchpads.
    pub fn touchpad_fields(&self) -> Option<TouchpadFieldMap> {
        let find = |page, usage| {
            self.fields
                .iter()
                .copied()
                .find(|field| field.usage_page == page && field.usage == usage)
        };
        let tip_switch = find(DIGITIZER_PAGE, USAGE_TIP_SWITCH);
        let contact_id = find(DIGITIZER_PAGE, USAGE_CONTACT_ID);
        let contact_count = find(DIGITIZER_PAGE, USAGE_CONTACT_COUNT);
        // The N150 descriptor contains a mouse collection (report ID 6)
        // before the touchpad collections (report ID 1).  Select coordinates
        // and buttons from the digitizer report instead of accidentally
        // binding the relative mouse X/Y fields.
        let touchpad_report_id = tip_switch
            .or(contact_id)
            .or(contact_count)
            .map(|field| field.report_id);
        let find_touchpad = |page, usage| {
            touchpad_report_id
                .and_then(|report_id| {
                    self.fields.iter().copied().find(|field| {
                        field.report_id == report_id
                            && field.usage_page == page
                            && field.usage == usage
                    })
                })
                .or_else(|| find(page, usage))
        };
        Some(TouchpadFieldMap {
            x: find_touchpad(GENERIC_DESKTOP_PAGE, USAGE_X)?,
            y: find_touchpad(GENERIC_DESKTOP_PAGE, USAGE_Y)?,
            left_button: find_touchpad(BUTTON_PAGE, USAGE_BUTTON_LEFT),
            right_button: find_touchpad(BUTTON_PAGE, USAGE_BUTTON_RIGHT),
            tip_switch,
            contact_id,
            contact_count,
        })
    }

    pub fn decode_touchpad(
        &self,
        fields: TouchpadFieldMap,
        report: &[u8],
    ) -> Option<TouchpadReport> {
        let x = self.value(fields.x, report)?;
        let y = self.value(fields.y, report)?;
        let left = fields
            .left_button
            .and_then(|field| self.value(field, report))
            .unwrap_or(0)
            != 0;
        let right = fields
            .right_button
            .and_then(|field| self.value(field, report))
            .unwrap_or(0)
            != 0;
        let in_contact = fields
            .tip_switch
            .and_then(|field| self.value(field, report).map(|v| v != 0))
            .or_else(|| {
                fields
                    .contact_count
                    .and_then(|field| self.value(field, report).map(|v| v > 0))
            })
            .unwrap_or(left || right);
        Some(TouchpadReport {
            x,
            y,
            buttons: (left as u8) | ((right as u8) << 1),
            in_contact,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GEMIBOOK_LINUX_I2C_HID_NAME, GEMIBOOK_TOUCHPAD_ACPI_HID, GEMIBOOK_TOUCHPAD_HID_PRODUCT_ID,
        GEMIBOOK_TOUCHPAD_HID_VENDOR_ID, HidReportDescriptor, INTEL_LPSS_I2C_DEVICE_IDS,
        TouchpadReport,
    };

    // One report-IDed absolute pointer with left/right buttons and a tip bit.
    const DESCRIPTOR: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x75, 0x06, 0x95, 0x01, 0x81, 0x01,
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x00, 0x00, 0x26, 0xff, 0x0f, 0x75, 0x10, 0x95,
        0x02, 0x81, 0x02, 0x05, 0x0d, 0x09, 0x42, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01,
        0x81, 0x02, 0xc0,
    ];

    // The first digitizer collection from the N150 report descriptor.  In
    // the complete descriptor this follows a report-ID 6 relative mouse
    // collection and uses report ID 1 for touch data.
    const N150_TOUCHPAD_DESCRIPTOR: &[u8] = &[
        0x05, 0x0d, 0x09, 0x22, 0xa1, 0x02, 0x85, 0x01, 0x09, 0x47, 0x09, 0x42, 0x15, 0x00, 0x25,
        0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x02, 0x81, 0x03, 0x09, 0x51, 0x25, 0x0f,
        0x75, 0x04, 0x95, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x75, 0x10, 0x55, 0x0e, 0x65,
        0x11, 0x35, 0x00, 0x46, 0xd8, 0x04, 0x27, 0xac, 0x06, 0x00, 0x00, 0x81, 0x02, 0x09, 0x31,
        0x46, 0x02, 0x03, 0x27, 0x24, 0x04, 0x00, 0x00, 0x81, 0x02, 0xc0,
    ];

    #[test]
    fn parses_standard_touchpad_fields_and_report_id() {
        let descriptor = HidReportDescriptor::parse(DESCRIPTOR).unwrap();
        let fields = descriptor.touchpad_fields().unwrap();
        assert_eq!(fields.x.report_id, 1);
        assert_eq!(descriptor.max_input_bytes(), 7);
        let report = [1, 0x01, 0x34, 0x00, 0x01, 0x02, 0x01];
        assert_eq!(
            descriptor.decode_touchpad(fields, &report),
            Some(TouchpadReport {
                x: 0x0034,
                y: 0x0201,
                buttons: 1,
                in_contact: true,
            })
        );
    }

    #[test]
    fn rejects_truncated_descriptor() {
        assert!(HidReportDescriptor::parse(&[0x05]).is_err());
    }

    #[test]
    fn records_n150_linux_transport_identity() {
        assert!(INTEL_LPSS_I2C_DEVICE_IDS.contains(&0x54e8));
        assert_eq!(GEMIBOOK_TOUCHPAD_ACPI_HID, b"AMR1399");
        assert_eq!(GEMIBOOK_LINUX_I2C_HID_NAME, b"AMR13992:00");
        assert_eq!(GEMIBOOK_TOUCHPAD_HID_VENDOR_ID, 0x36b6);
        assert_eq!(GEMIBOOK_TOUCHPAD_HID_PRODUCT_ID, 0xc001);
    }

    #[test]
    fn parses_n150_precision_touchpad_collection() {
        let descriptor = HidReportDescriptor::parse(N150_TOUCHPAD_DESCRIPTOR).unwrap();
        let fields = descriptor.touchpad_fields().unwrap();
        assert_eq!(fields.x.report_id, 1);
        assert_eq!(fields.y.report_id, 1);
        assert_eq!(fields.contact_id.unwrap().report_id, 1);
        assert_eq!(descriptor.max_input_bytes(), 6);

        let report = [1, 0b0010_0011, 0x34, 0x12, 0x78, 0x56];
        assert_eq!(
            descriptor.decode_touchpad(fields, &report).unwrap().x,
            0x1234
        );
        assert_eq!(
            descriptor.decode_touchpad(fields, &report).unwrap().y,
            0x5678
        );
    }
}
