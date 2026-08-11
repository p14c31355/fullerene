//! Polling HID-over-I2C support for ACPI-described DesignWare controllers.
//!
//! The Linux stack presents this device as an Intel DesignWare I2C adapter
//! followed by `i2c_hid_acpi` and `hid-multitouch`.  This module keeps the
//! The platform description (PCI BDF, I²C address, timing and HID descriptor
//! register) is supplied by the platform layer.  The HID identity and report
//! format are discovered from the device, like Linux's i2c_hid_acpi and
//! i2c-hid-core layers.  Machines without such a description keep using the
//! existing PS/2 path unchanged.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::hid::{HidReportDescriptor, I2cHidPlatformConfig, TouchpadReport};
use crate::mmio::MemRegion;
use crate::pci::PciDevice;
use crate::timing::{delay_ms, delay_us, poll_timeout_us};

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
const CON_SPEED_STANDARD: u32 = 1 << 1;
const CON_SPEED_FAST: u32 = 2 << 1;
const CON_RESTART: u32 = 1 << 5;
const CON_SLAVE_DISABLE: u32 = 1 << 6;

const DATA_READ: u32 = 1 << 8;
const DATA_STOP: u32 = 1 << 9;
const DATA_RESTART: u32 = 1 << 10;

const STATUS_MASTER_ACTIVITY: u32 = 1 << 5;
const STATUS_ACTIVITY: u32 = 1 << 0;
const INTR_TX_ABORT: u32 = 1 << 6;
const INTR_STOP_DET: u32 = 1 << 9;
const I2C_BUSY_TIMEOUT_US: u64 = 1_000;

const DW_IC_SDA_HOLD_MIN_VERSION: u32 = 0x3131312a;

const fn div_round_closest(value: u64, divisor: u64) -> u64 {
    (value + divisor / 2) / divisor
}

const fn scl_hcnt(clock_khz: u64, period_ns: u64, fall_ns: u64) -> u32 {
    div_round_closest(clock_khz * (period_ns + fall_ns), 1_000_000).saturating_sub(3) as u32
}

const fn scl_lcnt(clock_khz: u64, period_ns: u64, fall_ns: u64) -> u32 {
    div_round_closest(clock_khz * (period_ns + fall_ns), 1_000_000).saturating_sub(1) as u32
}

const fn sda_hold(clock_khz: u64, hold_ns: u64) -> u32 {
    // Linux converts the ACPI/software-node hold time to clock cycles and
    // sets the RX hold workaround bit when the DesignWare IP supports it.
    (div_round_closest(clock_khz * hold_ns, 1_000_000) as u32) | (1 << 16)
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
    output_register: u16,
    max_output_length: u16,
    command_register: u16,
    data_register: u16,
    vendor_id: u16,
    product_id: u16,
    version_id: u16,
}

impl HidI2cDescriptor {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != HID_DESCRIPTOR_LENGTH {
            return None;
        }
        let word = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        // Linux i2c-hid-core requires HID-over-I2C version 1.00 and the
        // specification fixes this descriptor at exactly 30 bytes.
        if word(0) != HID_DESCRIPTOR_LENGTH as u16 || word(2) != 0x0100 {
            return None;
        }
        let descriptor = Self {
            hid_desc_length: word(0),
            report_desc_length: word(4),
            report_desc_register: word(6),
            input_register: word(8),
            max_input_length: word(10),
            output_register: word(12),
            max_output_length: word(14),
            command_register: word(16),
            data_register: word(18),
            vendor_id: word(20),
            product_id: word(22),
            version_id: word(24),
        };
        (descriptor.report_desc_length != 0 && descriptor.max_input_length as usize >= 2)
            .then_some(descriptor)
    }
}

/// A single DesignWare I2C controller configured for one 7-bit HID target.
struct DesignWareI2c {
    mmio: MemRegion,
    target: u16,
    tx_fifo_depth: u32,
    speed_mode: u32,
}

impl DesignWareI2c {
    fn new(
        ctx: &dyn DriverContext,
        device: &PciDevice,
        profile: I2cHidPlatformConfig,
    ) -> Result<Self, I2cHidError> {
        if !profile.matches_pci(
            device.vendor_id,
            device.device_id,
            device.bus,
            device.device,
            device.function,
        ) {
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
            speed_mode: if profile.bus_speed_hz > 100_000 {
                CON_SPEED_FAST
            } else {
                CON_SPEED_STANDARD
            },
        };
        controller.configure(profile)?;
        Ok(controller)
    }

    fn configure(&mut self, profile: I2cHidPlatformConfig) -> Result<(), I2cHidError> {
        self.mmio.write32(IC_ENABLE, 0);
        poll_timeout_us(10_000, || {
            (self.mmio.read32(IC_ENABLE_STATUS) & 1 == 0).then_some(())
        })
        .ok_or(I2cHidError::Timeout)?;
        let _ = self.mmio.read32(IC_CLR_INTR);
        let _ = self.mmio.read32(IC_CLR_TX_ABRT);

        // Match Linux's i2c_dw_scl_{h,l}cnt calculations.  The root clock
        // and electrical fall/hold times come from the platform description;
        // the controller is otherwise independent of the attached HID.
        self.mmio.write32(
            IC_FS_SCL_HCNT,
            scl_hcnt(profile.root_clock_khz, 600, profile.sda_fall_ns),
        );
        self.mmio.write32(
            IC_FS_SCL_LCNT,
            scl_lcnt(profile.root_clock_khz, 1_300, profile.scl_fall_ns),
        );
        self.mmio.write32(
            IC_SS_SCL_HCNT,
            scl_hcnt(profile.root_clock_khz, 4_000, profile.sda_fall_ns),
        );
        self.mmio.write32(
            IC_SS_SCL_LCNT,
            scl_lcnt(profile.root_clock_khz, 4_700, profile.scl_fall_ns),
        );
        if self.mmio.read32(IC_COMP_VERSION) >= DW_IC_SDA_HOLD_MIN_VERSION {
            self.mmio.write32(
                IC_SDA_HOLD,
                sda_hold(profile.root_clock_khz, profile.sda_hold_ns),
            );
        }
        self.mmio.write32(IC_TX_TL, self.tx_fifo_depth / 2);
        self.mmio.write32(IC_RX_TL, 0);
        self.mmio.write32(IC_TAR, self.target as u32);
        self.mmio.write32(
            IC_CON,
            CON_MASTER | self.speed_mode | CON_RESTART | CON_SLAVE_DISABLE,
        );
        self.mmio.write32(IC_INTR_MASK, 0);
        // Linux enables the adapter for each individual I2C transfer and
        // disables it again when the transfer completes.  Leave it disabled
        // here; `begin_transfer` owns the transaction lifetime.
        Ok(())
    }

    fn begin_transfer(&self) -> Result<(), I2cHidError> {
        // Match i2c_dw_wait_bus_not_busy() before i2c_dw_xfer_init().
        poll_timeout_us(I2C_BUSY_TIMEOUT_US, || {
            (self.mmio.read32(IC_STATUS) & STATUS_ACTIVITY == 0).then_some(())
        })
        .ok_or(I2cHidError::Timeout)?;

        self.mmio.write32(IC_ENABLE, 0);
        poll_timeout_us(10_000, || {
            (self.mmio.read32(IC_ENABLE_STATUS) & 1 == 0).then_some(())
        })
        .ok_or(I2cHidError::Timeout)?;
        self.mmio.write32(IC_TAR, self.target as u32);
        self.mmio.write32(
            IC_CON,
            CON_MASTER | self.speed_mode | CON_RESTART | CON_SLAVE_DISABLE,
        );
        self.mmio.write32(IC_INTR_MASK, 0);
        self.mmio.write32(IC_ENABLE, 1);
        // Linux performs this dummy read for DesignWare implementations with
        // an enable-status register before clearing stale transaction state.
        let _ = self.mmio.read32(IC_ENABLE_STATUS);
        let _ = self.mmio.read32(IC_CLR_INTR);
        let _ = self.mmio.read32(IC_CLR_TX_ABRT);
        poll_timeout_us(10_000, || {
            (self.mmio.read32(IC_ENABLE_STATUS) & 1 != 0).then_some(())
        })
        .ok_or(I2cHidError::Timeout)
    }

    fn end_transfer(&self) {
        // Linux checks that the master is no longer active before disabling
        // the adapter; disabling while SCL is still held can strand the
        // DesignWare state machine on the next transaction.
        let _ = poll_timeout_us(10_000, || {
            (self.mmio.read32(IC_STATUS) & (STATUS_ACTIVITY | STATUS_MASTER_ACTIVITY) == 0)
                .then_some(())
        });
        self.mmio.write32(IC_INTR_MASK, 0);
        self.mmio.write32(IC_ENABLE, 0);
        let _ = poll_timeout_us(10_000, || {
            (self.mmio.read32(IC_ENABLE_STATUS) & 1 == 0).then_some(())
        });
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
        if let Err(error) = self.begin_transfer() {
            self.end_transfer();
            return Err(error);
        }
        let result = self.transfer_started(write, read);
        self.end_transfer();
        result
    }

    fn transfer_started(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), I2cHidError> {
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

    fn read_input_report(&mut self, output: &mut [u8]) -> Result<usize, I2cHidError> {
        // Linux reads an input packet with I2C_M_RD only.  The input register
        // is not sent as a prefix; it is a descriptor field used by command
        // transactions, not by interrupt/input reception.  A zero length is
        // meaningful: i2c-hid uses it to acknowledge RESET completion.
        self.transfer(&[], output)?;
        Ok(u16::from_le_bytes([output[0], output[1]]) as usize)
    }

    fn wait_reset_completion(&mut self, max_input_length: usize) -> bool {
        let length = max_input_length.min(MAX_INPUT_REPORT);
        if length < 2 {
            return false;
        }
        let mut packet = [0u8; MAX_INPUT_REPORT];
        poll_timeout_us(1_000_000, || {
            match self.read_input_report(&mut packet[..length]) {
                // Linux treats a zero-sized input packet as the reset
                // completion acknowledgement.
                Ok(0) => return Some(true),
                // A non-empty packet may be queued while the controller is
                // waking. Consume it and keep waiting for the ack packet.
                Ok(_) | Err(_) => {}
            }
            delay_us(1_000);
            None
        })
        .unwrap_or(false)
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

/// Probe and initialise one ACPI-described HID-over-I2C touch device.
///
/// The platform config identifies the controller and bus wiring.  HID
/// vendor/product IDs are read from the wire descriptor and are not used to
/// select the driver.
pub fn init_i2c_hid(
    ctx: &dyn DriverContext,
    device: &PciDevice,
    profile: I2cHidPlatformConfig,
) -> Result<(), I2cHidError> {
    let mut bus = DesignWareI2c::new(ctx, device, profile)?;
    let mut hid_bytes = [0u8; MAX_HID_DESCRIPTOR];
    bus.read_register(
        profile.hid_descriptor_register,
        &mut hid_bytes[..HID_DESCRIPTOR_LENGTH],
    )?;
    let descriptor = HidI2cDescriptor::parse(&hid_bytes).ok_or(I2cHidError::InvalidDescriptor)?;
    // Follow Linux's power-on/reset sequence.  IRQ 81 is not wired into the
    // current Fullerene APIC input path yet, so the reset completion wait is
    // bounded and the normal polling path is used below.
    bus.write_command(descriptor.command_register, &[0x00, 0x08])?;
    delay_ms(60);
    bus.write_command(descriptor.command_register, &[0x00, 0x01])?;
    if !bus.wait_reset_completion(descriptor.max_input_length as usize) {
        // Linux continues after its one-second reset wait and reports the
        // missing IRQ acknowledgement, so keep the same recovery behavior
        // while still using the polling transport here.
        log::warn!("[nitrogen] I2C-HID reset acknowledgement timed out");
    }
    bus.write_command(descriptor.command_register, &[0x00, 0x08])?;
    delay_ms(60);

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
        "[nitrogen] I2C-HID touchpad initialized ({:04x}:{:04x}, input max {})",
        descriptor.vendor_id,
        descriptor.product_id,
        descriptor.max_input_length
    );
    Ok(())
}

/// Compatibility wrapper for callers that still select the supplied N150
/// platform description explicitly.
pub fn init_n150(ctx: &dyn DriverContext, device: &PciDevice) -> Result<(), I2cHidError> {
    init_i2c_hid(ctx, device, crate::hid::GEMIBOOK_N150_I2C_HID)
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
    let report_length = match device.bus.read_input_report(&mut bytes[..length]) {
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
    use super::{HidI2cDescriptor, scl_hcnt, scl_lcnt, sda_hold};

    #[test]
    fn matches_linux_bxt_i2c_timing_profile() {
        assert_eq!(scl_hcnt(133_000, 600, 171), 100);
        assert_eq!(scl_lcnt(133_000, 1_300, 208), 200);
        assert_eq!(sda_hold(133_000, 42), 0x1_0006);
    }

    #[test]
    fn parses_wire_descriptor_offsets() {
        let mut bytes = [0u8; 30];
        bytes[0..2].copy_from_slice(&30u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&0x0100u16.to_le_bytes());
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

    #[test]
    fn rejects_non_1_00_or_non_30_byte_hid_descriptors() {
        let mut bytes = [0u8; 30];
        bytes[0..2].copy_from_slice(&30u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&0x0100u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
        assert!(HidI2cDescriptor::parse(&bytes).is_some());

        let mut extended = [0u8; 31];
        extended[..30].copy_from_slice(&bytes);
        assert!(HidI2cDescriptor::parse(&extended).is_none());

        bytes[0..2].copy_from_slice(&31u16.to_le_bytes());
        assert!(HidI2cDescriptor::parse(&bytes).is_none());
        bytes[0..2].copy_from_slice(&30u16.to_le_bytes());
        bytes[2..4].copy_from_slice(&0x0101u16.to_le_bytes());
        assert!(HidI2cDescriptor::parse(&bytes).is_none());
    }
}
