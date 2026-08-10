//! Minimal polling HID-over-I2C support for the GemiBook XPro N150.
//!
//! The Linux stack presents this device as an Intel DesignWare I2C adapter
//! followed by `i2c_hid_acpi` and `hid-multitouch`.  This module keeps the
//! same layering, but deliberately enables only the exact PCI function and
//! HID identity found in the supplied N150 diagnostics.  Other machines keep
//! using the existing PS/2 path unchanged.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::hid::{
    GEMIBOOK_N150_I2C_HID, GEMIBOOK_TOUCHPAD_HID_PRODUCT_ID, GEMIBOOK_TOUCHPAD_HID_VENDOR_ID,
    HidReportDescriptor, TouchpadReport,
};
use crate::mmio::MemRegion;
use crate::pci::PciDevice;
use crate::timing::{delay_ms, poll_timeout_us};

const MMIO_SIZE: usize = 0x1000;
// Intel LPSS exposes the DesignWare block at BAR+0x000 and its private
// wrapper registers at BAR+0x200.  Linux initializes the wrapper before it
// probes the child DesignWare adapter.
const LPSS_PRIV_RESETS: usize = 0x204;
const LPSS_PRIV_REMAP_ADDR: usize = 0x240;
const LPSS_PRIV_CAPS: usize = 0x2fc;
const LPSS_PRIV_CAPS_TYPE_MASK: u32 = 0xf0;
const LPSS_PRIV_CAPS_TYPE_SHIFT: u32 = 4;
const LPSS_PRIV_CAPS_TYPE_I2C: u32 = 0;
const LPSS_PRIV_RESETS_IDMA: u32 = 1 << 2;
const LPSS_PRIV_RESETS_FUNC: u32 = 0x3;

const COMP_TYPE: usize = 0xfc;
const COMP_TYPE_VALUE: u32 = 0x4457_0140;

const IC_CON: usize = 0x00;
const IC_TAR: usize = 0x04;
const IC_DATA_CMD: usize = 0x10;
const IC_SS_SCL_HCNT: usize = 0x14;
const IC_SS_SCL_LCNT: usize = 0x18;
const IC_FS_SCL_HCNT: usize = 0x1c;
const IC_FS_SCL_LCNT: usize = 0x20;
const IC_INTR_MASK: usize = 0x30;
const IC_RX_TL: usize = 0x38;
const IC_TX_TL: usize = 0x3c;
const IC_RAW_INTR_STAT: usize = 0x34;
const IC_CLR_INTR: usize = 0x40;
const IC_CLR_TX_ABRT: usize = 0x54;
const IC_CLR_STOP_DET: usize = 0x60;
const IC_ENABLE: usize = 0x6c;
const IC_STATUS: usize = 0x70;
const IC_TXFLR: usize = 0x74;
const IC_RXFLR: usize = 0x78;
const IC_ENABLE_STATUS: usize = 0x9c;
const IC_COMP_PARAM_1: usize = 0xf4;
const IC_COMP_VERSION: usize = 0xf8;
const IC_SDA_HOLD: usize = 0x7c;

const CON_MASTER: u32 = 1 << 0;
const CON_SPEED_FAST: u32 = 2 << 1;
const CON_RESTART: u32 = 1 << 5;
const CON_SLAVE_DISABLE: u32 = 1 << 6;

const DATA_READ: u32 = 1 << 8;
const DATA_STOP: u32 = 1 << 9;
const DATA_RESTART: u32 = 1 << 10;

const STATUS_MASTER_ACTIVITY: u32 = 1 << 5;
const INTR_TX_ABORT: u32 = 1 << 6;
const INTR_STOP_DET: u32 = 1 << 9;

// Intel's bxt_i2c_info is also used for Alder Lake-M 8086:54e8 by Linux.
// These values are the software node properties and fixed LPSS root clock
// from that platform description.
const LPSS_BXT_I2C_CLOCK_KHZ: u64 = 133_000;
const LPSS_BXT_SDA_HOLD_NS: u64 = 42;
const LPSS_BXT_SDA_FALL_NS: u64 = 171;
const LPSS_BXT_SCL_FALL_NS: u64 = 208;
const DW_IC_SDA_HOLD_MIN_VERSION: u32 = 0x3131312a;

const fn div_round_closest(value: u64, divisor: u64) -> u64 {
    (value + divisor / 2) / divisor
}

const fn bxt_scl_hcnt() -> u32 {
    // Linux: i2c_dw_scl_hcnt(133000, 600ns, 171ns, offset=0).
    div_round_closest(
        LPSS_BXT_I2C_CLOCK_KHZ * (600 + LPSS_BXT_SDA_FALL_NS),
        1_000_000,
    )
    .saturating_sub(3) as u32
}

const fn bxt_scl_lcnt() -> u32 {
    // Linux: i2c_dw_scl_lcnt(133000, 1300ns, 208ns, offset=0).
    div_round_closest(
        LPSS_BXT_I2C_CLOCK_KHZ * (1_300 + LPSS_BXT_SCL_FALL_NS),
        1_000_000,
    )
    .saturating_sub(1) as u32
}

const fn bxt_ss_hcnt() -> u32 {
    div_round_closest(
        LPSS_BXT_I2C_CLOCK_KHZ * (4_000 + LPSS_BXT_SDA_FALL_NS),
        1_000_000,
    )
    .saturating_sub(3) as u32
}

const fn bxt_ss_lcnt() -> u32 {
    div_round_closest(
        LPSS_BXT_I2C_CLOCK_KHZ * (4_700 + LPSS_BXT_SCL_FALL_NS),
        1_000_000,
    )
    .saturating_sub(1) as u32
}

const fn bxt_sda_hold() -> u32 {
    // Linux converts the 42 ns property to clock cycles and sets the RX
    // hold workaround bit when the DesignWare IP supports SDA_HOLD.
    (div_round_closest(LPSS_BXT_I2C_CLOCK_KHZ * LPSS_BXT_SDA_HOLD_NS, 1_000_000) as u32) | (1 << 16)
}

const MAX_HID_DESCRIPTOR: usize = 64;
const HID_DESCRIPTOR_LENGTH: usize = 30;
const MAX_REPORT_DESCRIPTOR: usize = 4096;
const MAX_INPUT_REPORT: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cHidError {
    NotTarget,
    InvalidBar,
    MappingFailed,
    UnsupportedController,
    Timeout,
    TransferAborted(u32),
    InvalidDescriptor,
    InvalidReportDescriptor,
    UnsupportedDevice,
}

/// HID-over-I2C descriptor, in the wire order defined by the specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HidI2cDescriptor {
    hid_desc_length: u16,
    report_desc_length: u16,
    report_desc_register: u16,
    input_register: u16,
    max_input_length: u16,
    command_register: u16,
    vendor_id: u16,
    product_id: u16,
}

impl HidI2cDescriptor {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HID_DESCRIPTOR_LENGTH {
            return None;
        }
        let word = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let descriptor = Self {
            hid_desc_length: word(0),
            report_desc_length: word(4),
            report_desc_register: word(6),
            input_register: word(8),
            max_input_length: word(10),
            command_register: word(16),
            vendor_id: word(20),
            product_id: word(22),
        };
        (descriptor.hid_desc_length as usize >= HID_DESCRIPTOR_LENGTH
            && descriptor.report_desc_length != 0
            && descriptor.max_input_length as usize >= 2)
            .then_some(descriptor)
    }
}

/// A single DesignWare I2C controller configured for one 7-bit HID target.
struct DesignWareI2c {
    mmio: MemRegion,
    target: u16,
    tx_fifo_depth: u32,
}

impl DesignWareI2c {
    fn new(ctx: &dyn DriverContext, device: &PciDevice) -> Result<Self, I2cHidError> {
        let profile = GEMIBOOK_N150_I2C_HID;
        if device.vendor_id != 0x8086
            || device.device_id != profile.pci_device_id
            || device.bus != profile.pci_bus
            || device.device != profile.pci_device
            || device.function != profile.pci_function
        {
            return Err(I2cHidError::NotTarget);
        }
        let bar = device.read_bar_info(0).ok_or(I2cHidError::InvalidBar)?;
        if bar.is_io || bar.address == 0 || bar.address as usize % 4096 != 0 {
            return Err(I2cHidError::InvalidBar);
        }
        if !device.prepare_mmio() {
            return Err(I2cHidError::MappingFailed);
        }
        let virt = ctx.phys_to_virt(bar.address);
        ctx.map_mmio_region(bar.address as usize, virt, MMIO_SIZE)
            .map_err(|_| I2cHidError::MappingFailed)?;
        // SAFETY: the PCI BAR was validated and mapped as an uncached 4 KiB
        // MMIO region immediately above.
        let mmio = unsafe { MemRegion::new(virt, MMIO_SIZE) };
        let lpss_caps = mmio.read32(LPSS_PRIV_CAPS);
        let lpss_type = (lpss_caps & LPSS_PRIV_CAPS_TYPE_MASK) >> LPSS_PRIV_CAPS_TYPE_SHIFT;
        if lpss_type != LPSS_PRIV_CAPS_TYPE_I2C {
            return Err(I2cHidError::UnsupportedController);
        }

        // This is the ordering used by intel_lpss_init_dev(): put the LPSS
        // function in reset, deassert the function and IDMA resets, then
        // provide the child DesignWare block with its PCI BAR remap address.
        // Without this wrapper setup the DesignWare component may be visible
        // but transactions never reach the I2C pins (the observed symptom was
        // a touchpad that initialized nowhere and never moved the cursor).
        mmio.write32(LPSS_PRIV_RESETS, 0);
        mmio.write32(
            LPSS_PRIV_RESETS,
            LPSS_PRIV_RESETS_FUNC | LPSS_PRIV_RESETS_IDMA,
        );
        mmio.write64(LPSS_PRIV_REMAP_ADDR, bar.address);

        let component_type = mmio.read32(COMP_TYPE);
        if component_type != COMP_TYPE_VALUE {
            return Err(I2cHidError::UnsupportedController);
        }
        let param = mmio.read32(IC_COMP_PARAM_1);
        let tx_fifo_depth = (((param >> 16) & 0xff) + 1).max(1);
        let mut controller = Self {
            mmio,
            target: profile.i2c_address,
            tx_fifo_depth,
        };
        controller.configure(profile.bus_speed_hz)?;
        Ok(controller)
    }

    fn configure(&mut self, _bus_speed_hz: u32) -> Result<(), I2cHidError> {
        self.mmio.write32(IC_ENABLE, 0);
        poll_timeout_us(10_000, || {
            (self.mmio.read32(IC_ENABLE_STATUS) & 1 == 0).then_some(())
        })
        .ok_or(I2cHidError::Timeout)?;
        let _ = self.mmio.read32(IC_CLR_INTR);
        let _ = self.mmio.read32(IC_CLR_TX_ABRT);

        // Match Linux's intel-lpss bxt_i2c_info and DesignWare timing
        // calculation for the 400 kHz ACPI bus.  Firmware HCNT/LCNT values
        // are not trusted here: this exact LPSS function is clocked at
        // 133 MHz, and the old 100 MHz fallback produced an invalid bus
        // waveform on the N150.
        self.mmio.write32(IC_FS_SCL_HCNT, bxt_scl_hcnt());
        self.mmio.write32(IC_FS_SCL_LCNT, bxt_scl_lcnt());
        self.mmio.write32(IC_SS_SCL_HCNT, bxt_ss_hcnt());
        self.mmio.write32(IC_SS_SCL_LCNT, bxt_ss_lcnt());
        if self.mmio.read32(IC_COMP_VERSION) >= DW_IC_SDA_HOLD_MIN_VERSION {
            self.mmio.write32(IC_SDA_HOLD, bxt_sda_hold());
        }
        self.mmio.write32(IC_TX_TL, self.tx_fifo_depth / 2);
        self.mmio.write32(IC_RX_TL, 0);
        self.mmio.write32(IC_TAR, self.target as u32);
        self.mmio.write32(
            IC_CON,
            CON_MASTER | CON_SPEED_FAST | CON_RESTART | CON_SLAVE_DISABLE,
        );
        self.mmio.write32(IC_INTR_MASK, 0);
        self.mmio.write32(IC_ENABLE, 1);
        poll_timeout_us(10_000, || {
            (self.mmio.read32(IC_ENABLE_STATUS) & 1 != 0).then_some(())
        })
        .ok_or(I2cHidError::Timeout)
    }

    fn abort_source(&self) -> Option<I2cHidError> {
        let status = self.mmio.read32(IC_RAW_INTR_STAT);
        if status & INTR_TX_ABORT != 0 {
            let source = self.mmio.read32(0x80);
            let _ = self.mmio.read32(IC_CLR_TX_ABRT);
            return Some(I2cHidError::TransferAborted(source));
        }
        None
    }

    fn wait_tx_space(&self) -> Result<(), I2cHidError> {
        poll_timeout_us(10_000, || {
            if let Some(error) = self.abort_source() {
                return Some(Err(error));
            }
            (self.mmio.read32(IC_TXFLR) < self.tx_fifo_depth).then_some(Ok(()))
        })
        .unwrap_or(Err(I2cHidError::Timeout))
    }

    fn wait_rx_data(&self) -> Result<u8, I2cHidError> {
        poll_timeout_us(10_000, || {
            if let Some(error) = self.abort_source() {
                return Some(Err(error));
            }
            if self.mmio.read32(IC_RXFLR) != 0 {
                return Some(Ok(self.mmio.read32(IC_DATA_CMD) as u8));
            }
            None
        })
        .unwrap_or(Err(I2cHidError::Timeout))
    }

    fn transfer(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), I2cHidError> {
        if write.is_empty() && read.is_empty() {
            return Ok(());
        }
        self.mmio.write32(IC_INTR_MASK, 0);
        let mut wrote = 0usize;
        for byte in write {
            self.wait_tx_space()?;
            let is_last_write = wrote + 1 == write.len() && read.is_empty();
            self.mmio.write32(
                IC_DATA_CMD,
                u32::from(*byte) | if is_last_write { DATA_STOP } else { 0 },
            );
            wrote += 1;
        }
        let read_len = read.len();
        for (index, byte) in read.iter_mut().enumerate() {
            self.wait_tx_space()?;
            let restart = index == 0 && !write.is_empty();
            let stop = index + 1 == read_len;
            self.mmio.write32(
                IC_DATA_CMD,
                DATA_READ
                    | if restart { DATA_RESTART } else { 0 }
                    | if stop { DATA_STOP } else { 0 },
            );
            *byte = self.wait_rx_data()?;
        }
        poll_timeout_us(10_000, || {
            if let Some(error) = self.abort_source() {
                return Some(Err(error));
            }
            let status = self.mmio.read32(IC_RAW_INTR_STAT);
            if status & INTR_STOP_DET != 0 {
                let _ = self.mmio.read32(IC_CLR_STOP_DET);
                return Some(Ok(()));
            }
            (self.mmio.read32(IC_STATUS) & STATUS_MASTER_ACTIVITY == 0).then_some(Ok(()))
        })
        .unwrap_or(Err(I2cHidError::Timeout))
    }

    fn read_register(&mut self, register: u16, output: &mut [u8]) -> Result<(), I2cHidError> {
        let register = register.to_le_bytes();
        self.transfer(&register, output)
    }

    fn write_command(&mut self, register: u16, command: &[u8]) -> Result<(), I2cHidError> {
        let register = register.to_le_bytes();
        let mut bytes = Vec::with_capacity(register.len() + command.len());
        bytes.extend_from_slice(&register);
        bytes.extend_from_slice(command);
        self.transfer(&bytes, &mut [])
    }

    fn read_input_report(
        &mut self,
        input_register: u16,
        output: &mut [u8],
    ) -> Result<usize, I2cHidError> {
        // Linux uses a plain receive for the interrupt-driven path. Some
        // firmware revisions also require the HID input-register address;
        // try the Linux path first and fall back to the explicit register
        // form when the returned length is not a valid packet.
        let direct_ok = self.transfer(&[], output).is_ok();
        let mut length = if direct_ok {
            u16::from_le_bytes([output[0], output[1]]) as usize
        } else {
            0
        };
        if length < 2 || length > output.len() {
            // Do not propagate the direct-read NACK before trying the
            // register-addressed form; some controllers only expose the
            // pending HID packet through this transaction shape.
            self.read_register(input_register, output)?;
            length = u16::from_le_bytes([output[0], output[1]]) as usize;
        }
        Ok(length)
    }
}

impl Drop for DesignWareI2c {
    fn drop(&mut self) {
        self.mmio.write32(IC_INTR_MASK, 0);
        self.mmio.write32(IC_ENABLE, 0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchpadInput {
    pub report: TouchpadReport,
    pub x_min: i32,
    pub x_max: i32,
    pub y_min: i32,
    pub y_max: i32,
}

struct N150Touchpad {
    bus: DesignWareI2c,
    descriptor: HidI2cDescriptor,
    report: HidReportDescriptor,
    fields: crate::hid::TouchpadFieldMap,
}

static TOUCHPAD: Mutex<Option<N150Touchpad>> = Mutex::new(None);
static LATEST_INPUT: Mutex<Option<TouchpadInput>> = Mutex::new(None);
static LAST_POLL_TSC: AtomicU64 = AtomicU64::new(0);

/// Probe and initialise only the N150's `00:15.0` I2C controller.
pub fn init_n150(ctx: &dyn DriverContext, device: &PciDevice) -> Result<(), I2cHidError> {
    let mut bus = DesignWareI2c::new(ctx, device)?;
    let mut hid_bytes = [0u8; MAX_HID_DESCRIPTOR];
    bus.read_register(
        GEMIBOOK_N150_I2C_HID.hid_descriptor_register,
        &mut hid_bytes[..HID_DESCRIPTOR_LENGTH],
    )?;
    let descriptor = HidI2cDescriptor::parse(&hid_bytes).ok_or(I2cHidError::InvalidDescriptor)?;
    if descriptor.vendor_id != GEMIBOOK_TOUCHPAD_HID_VENDOR_ID
        || descriptor.product_id != GEMIBOOK_TOUCHPAD_HID_PRODUCT_ID
    {
        return Err(I2cHidError::UnsupportedDevice);
    }

    // Follow Linux's power-on/reset sequence.  IRQ 81 is not wired into the
    // current Fullerene APIC input path yet, so the reset completion wait is
    // bounded and the normal polling path is used below.
    bus.write_command(descriptor.command_register, &[0x00, 0x08])?;
    delay_ms(60);
    bus.write_command(descriptor.command_register, &[0x00, 0x01])?;
    delay_ms(100);
    bus.write_command(descriptor.command_register, &[0x00, 0x08])?;
    delay_ms(60);

    // Consume the reset-complete input packet when the firmware exposes one.
    // Linux normally does this from IRQ 81; the polling implementation must
    // clear it explicitly before waiting for the first finger report.
    let reset_length = (descriptor.max_input_length as usize).min(MAX_INPUT_REPORT);
    if reset_length >= 2 {
        let mut reset_report = [0u8; MAX_INPUT_REPORT];
        let _ = bus.read_input_report(descriptor.input_register, &mut reset_report[..reset_length]);
    }

    let report_length = descriptor.report_desc_length as usize;
    if report_length > MAX_REPORT_DESCRIPTOR {
        return Err(I2cHidError::InvalidDescriptor);
    }
    let mut report_bytes = Vec::new();
    report_bytes.resize(report_length, 0);
    bus.read_register(descriptor.report_desc_register, &mut report_bytes)
        .map_err(|_| I2cHidError::InvalidReportDescriptor)?;
    let report = HidReportDescriptor::parse(&report_bytes)
        .map_err(|_| I2cHidError::InvalidReportDescriptor)?;
    let fields = report
        .touchpad_fields()
        .ok_or(I2cHidError::InvalidReportDescriptor)?;
    let state = N150Touchpad {
        bus,
        descriptor,
        report,
        fields,
    };
    *TOUCHPAD.lock() = Some(state);
    *LATEST_INPUT.lock() = None;
    log::info!(
        "[nitrogen] N150 I2C-HID touchpad initialized ({:04x}:{:04x}, input max {})",
        descriptor.vendor_id,
        descriptor.product_id,
        descriptor.max_input_length
    );
    Ok(())
}

/// Poll one HID input packet.  The interrupt line is intentionally left for a
/// later APIC/GSI integration; polling keeps this first hardware path
/// independent of the existing legacy IRQ routing.
pub fn poll_input() {
    if !is_initialized() {
        return;
    }
    let now = unsafe { core::arch::x86_64::_rdtsc() };
    let previous = LAST_POLL_TSC.load(Ordering::Relaxed);
    if previous != 0
        && now.wrapping_sub(previous) < crate::timing::ticks_per_us().saturating_mul(5_000)
    {
        return;
    }
    LAST_POLL_TSC.store(now, Ordering::Relaxed);
    let mut guard = TOUCHPAD.lock();
    let Some(device) = guard.as_mut() else { return };
    let length = (device.descriptor.max_input_length as usize).min(MAX_INPUT_REPORT);
    if length < 2 {
        return;
    }
    let mut bytes = [0u8; MAX_INPUT_REPORT];
    let report_length = match device
        .bus
        .read_input_report(device.descriptor.input_register, &mut bytes[..length])
    {
        Ok(length) => length,
        Err(_) => return,
    };
    if report_length < 2 || report_length > length {
        return;
    }
    if let Some(report) = device
        .report
        .decode_touchpad(device.fields, &bytes[2..report_length])
    {
        *LATEST_INPUT.lock() = Some(TouchpadInput {
            report,
            x_min: device.fields.x.logical_minimum,
            x_max: device.fields.x.logical_maximum,
            y_min: device.fields.y.logical_minimum,
            y_max: device.fields.y.logical_maximum,
        });
    }
}

pub fn consume_input() -> Option<TouchpadInput> {
    core::mem::take(&mut *LATEST_INPUT.lock())
}

pub fn is_initialized() -> bool {
    TOUCHPAD.lock().is_some()
}

#[cfg(test)]
mod tests {
    use super::{HidI2cDescriptor, bxt_scl_hcnt, bxt_scl_lcnt, bxt_sda_hold};

    #[test]
    fn matches_linux_bxt_i2c_timing_profile() {
        assert_eq!(bxt_scl_hcnt(), 100);
        assert_eq!(bxt_scl_lcnt(), 200);
        assert_eq!(bxt_sda_hold(), 0x1_0006);
    }

    #[test]
    fn parses_wire_descriptor_offsets() {
        let mut bytes = [0u8; 30];
        bytes[0..2].copy_from_slice(&30u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&512u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&0x100u16.to_le_bytes());
        bytes[8..10].copy_from_slice(&0x200u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&128u16.to_le_bytes());
        bytes[16..18].copy_from_slice(&0x300u16.to_le_bytes());
        bytes[20..22].copy_from_slice(&0x36b6u16.to_le_bytes());
        bytes[22..24].copy_from_slice(&0xc001u16.to_le_bytes());
        let descriptor = HidI2cDescriptor::parse(&bytes).unwrap();
        assert_eq!(descriptor.report_desc_register, 0x100);
        assert_eq!(descriptor.input_register, 0x200);
        assert_eq!(descriptor.command_register, 0x300);
        assert_eq!(descriptor.vendor_id, 0x36b6);
    }
}
