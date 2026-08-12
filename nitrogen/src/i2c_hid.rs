//! Polling HID-over-I2C support for ACPI-described DesignWare controllers.
//!
//! The Linux stack presents this device as an Intel DesignWare I2C adapter
//! followed by `i2c_hid_acpi` and `hid-multitouch`.  The platform description
//! (PCI BDF, I²C address, timing and HID descriptor
//! register) is supplied by the platform layer.  The HID identity and report
//! format are discovered from the device, like Linux's i2c_hid_acpi and
//! i2c-hid-core layers.  Machines without such a description keep using the
//! existing PS/2 path unchanged.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::hid::{
    HidReportDescriptor, I2cHidPlatformConfig, MAX_TOUCH_CONTACTS, TouchContact, TouchpadReport,
};
use crate::mmio::MemRegion;
use crate::pci::{PciConfigSpace, PciDevice};
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
const HID_ADDRESS_RETRY_DELAY_US: u64 = 400;
const I2C_INPUT_POLL_INTERVAL_US: u64 = 1_000;
const MAX_INPUT_REPORTS_PER_POLL: usize = 8;

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
    ctx: &'static dyn DriverContext,
    physical_base: usize,
    virtual_base: usize,
    mapping_size: usize,
    target: u16,
    tx_fifo_depth: u32,
    speed_mode: u32,
}

impl DesignWareI2c {
    fn new(
        ctx: &'static dyn DriverContext,
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
        // Do not use the WB physical-memory direct map for controller
        // registers. A dedicated virtual alias lets the kernel map this BAR
        // as UC, which is required for LPSS/DW register reads and writes.
        let virt = ctx
            .mmio_virtual_address(bar.address, MMIO_SIZE)
            .map_err(|_| I2cHidError::MappingFailed)?;
        if ctx
            .map_mmio_region(bar.address as usize, virt, MMIO_SIZE)
            .is_err()
        {
            // mmio_virtual_address reserves the range before returning it.
            ctx.unmap_mmio_region(bar.address as usize, virt, MMIO_SIZE);
            return Err(I2cHidError::MappingFailed);
        }
        // SAFETY: the PCI BAR was validated and mapped as an uncached 4 KiB
        // MMIO region immediately above.
        let mmio = unsafe { MemRegion::new(virt, MMIO_SIZE) };
        let lpss_caps = mmio.read32(LPSS_PRIV_CAPS);
        let lpss_type = (lpss_caps & LPSS_PRIV_CAPS_TYPE_MASK) >> LPSS_PRIV_CAPS_TYPE_SHIFT;
        if lpss_type != LPSS_PRIV_CAPS_TYPE_I2C {
            let status = alloc::format!("LPSS type{}; DW probe", lpss_type);
            publish_status(&status);
            log::warn!(
                "[nitrogen] LPSS caps type {} (caps=0x{:08x}); validating DW component before fallback",
                lpss_type,
                lpss_caps
            );
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
            // Keep the physical BAR, the dedicated MMIO alias and the PCI
            // decode bits beside the bad register value.  This is the only
            // useful distinction available on machines without a serial
            // console: a wrong BAR and a wrong page-table mapping otherwise
            // look exactly like an unsupported DesignWare revision.
            let command =
                PciConfigSpace::read_config_word(device.bus, device.device, device.function, 4);
            let bar_low =
                PciConfigSpace::read_config_dword(device.bus, device.device, device.function, 0x10);
            let status = alloc::format!(
                "DW={:08x} LP={:08x} B={:x} T={:08x} V={:x} C={:04x}",
                component_type,
                lpss_caps,
                bar.address,
                bar_low,
                virt,
                command
            );
            log::warn!(
                "[nitrogen] DesignWare probe mismatch: comp=0x{:08x} caps=0x{:08x} bar=0x{:x} virt=0x{:x} command=0x{:04x} c0=0x{:08x} f4=0x{:08x} f8=0x{:08x}",
                component_type,
                lpss_caps,
                bar.address,
                virt,
                command,
                mmio.read32(IC_CON),
                mmio.read32(IC_COMP_PARAM_1),
                mmio.read32(IC_COMP_VERSION),
            );
            publish_status(&status);
            ctx.unmap_mmio_region(bar.address as usize, virt, MMIO_SIZE);
            return Err(I2cHidError::UnsupportedController);
        }
        let param = mmio.read32(IC_COMP_PARAM_1);
        let tx_fifo_depth = (((param >> 16) & 0xff) + 1).max(1);
        let mut controller = Self {
            mmio,
            ctx,
            physical_base: bar.address as usize,
            virtual_base: virt,
            mapping_size: MMIO_SIZE,
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
        log::info!(
            "[nitrogen] I2C-HID DW configured: target=0x{:02x} fifo={} speed={}Hz hcnt={} lcnt={} sda_hold=0x{:08x}",
            self.target,
            self.tx_fifo_depth,
            profile.bus_speed_hz,
            self.mmio.read32(IC_FS_SCL_HCNT),
            self.mmio.read32(IC_FS_SCL_LCNT),
            self.mmio.read32(IC_SDA_HOLD),
        );
        crate::debug_status!(
            "I2C-HID",
            "CTRL 0x{:02x} {}k",
            self.target,
            profile.bus_speed_hz / 1_000
        );
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

    /// Match Linux's i2c_hid_probe_address() SMBus-byte probe.  This is a
    /// plain one-byte read: no register prefix is sent before the HID
    /// descriptor transaction.  Some controllers need a short wake delay
    /// after the first address probe, so retry once exactly as Linux does.
    fn probe_address(&mut self) -> Result<(), I2cHidError> {
        let mut byte = [0u8; 1];
        match self.transfer(&[], &mut byte) {
            Ok(()) => Ok(()),
            Err(first_error) => {
                delay_us(HID_ADDRESS_RETRY_DELAY_US);
                self.transfer(&[], &mut byte).map_err(|_| first_error)
            }
        }
    }

    fn write_command(&mut self, register: u16, command: &[u8]) -> Result<(), I2cHidError> {
        let register = register.to_le_bytes();
        let mut bytes = Vec::with_capacity(register.len() + command.len());
        bytes.extend_from_slice(&register);
        bytes.extend_from_slice(command);
        self.transfer(&bytes, &mut [])
    }

    fn read_input_report(&mut self, output: &mut [u8]) -> Result<usize, I2cHidError> {
        if output.len() < 2 {
            return Err(I2cHidError::InvalidDescriptor);
        }
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

/// Send SET_POWER(ON) with the same wake-up retry Linux uses for devices that
/// NAK the first transaction after a clock edge or a deep-sleep transition.
fn power_on(bus: &mut DesignWareI2c, command_register: u16) -> Result<(), I2cHidError> {
    match bus.write_command(command_register, &[0x00, 0x08]) {
        Ok(()) => {}
        Err(first_error) => {
            delay_us(HID_ADDRESS_RETRY_DELAY_US);
            bus.write_command(command_register, &[0x00, 0x08])
                .map_err(|_| first_error)?;
        }
    }
    // The HID-over-I2C stack follows Windows here: allow the device to finish
    // its wake-up before RESET or the first descriptor transaction.
    delay_ms(60);
    Ok(())
}

impl Drop for DesignWareI2c {
    fn drop(&mut self) {
        self.mmio.write32(IC_INTR_MASK, 0);
        self.mmio.write32(IC_ENABLE, 0);
        self.ctx
            .unmap_mmio_region(self.physical_base, self.virtual_base, self.mapping_size);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchpadInput {
    pub report: TouchpadReport,
    pub x_min: i32,
    pub x_max: i32,
    pub y_min: i32,
    pub y_max: i32,
    /// Relative mouse reports are emitted as report ID 6 on the N150.  The
    /// absolute digitizer path remains available for report ID 1. A stored
    /// snapshot may contain a relative delta together with contact slots
    /// retained from the preceding absolute report.
    pub relative: Option<(i16, i16)>,
    /// Contact slots from the absolute digitizer report.  A slot remains
    /// present with `in_contact == false` in release reports.
    pub contacts: [Option<TouchContact>; MAX_TOUCH_CONTACTS],
    pub contact_count: u8,
}

struct I2cHidTouchpad {
    bus: DesignWareI2c,
    descriptor: HidI2cDescriptor,
    report: HidReportDescriptor,
    fields: crate::hid::TouchpadFieldMap,
}

static TOUCHPAD: Mutex<Option<I2cHidTouchpad>> = Mutex::new(None);
static LATEST_INPUT: Mutex<Option<TouchpadInput>> = Mutex::new(None);
static STATUS: Mutex<Option<(String, String)>> = Mutex::new(None);
static LAST_SERVICE_TSC: AtomicU64 = AtomicU64::new(0);
static INPUT_PENDING: AtomicBool = AtomicBool::new(false);
static INPUT_INTERRUPT_MODE: AtomicBool = AtomicBool::new(false);
static INPUT_INTERRUPT_REARM: Mutex<Option<fn()>> = Mutex::new(None);
static POLL_CONSECUTIVE_FAILURES: AtomicU32 = AtomicU32::new(0);
static POLL_ERROR_REPORTED: AtomicBool = AtomicBool::new(false);
static FIRST_INPUT_REPORTED: AtomicBool = AtomicBool::new(false);
static INPUT_SERVICE_COUNT: AtomicU64 = AtomicU64::new(0);
static INPUT_SERVICE_TOTAL_TSC: AtomicU64 = AtomicU64::new(0);
static INPUT_SERVICE_MAX_TSC: AtomicU64 = AtomicU64::new(0);

/// Publish a persistent framebuffer-visible status for the I2C-HID path.
/// The normal debug ring is transient and can be displaced by later boot
/// messages from VFS or Wi-Fi.
pub fn publish_status(message: &str) {
    *STATUS.lock() = Some((String::from("I2C-HID"), String::from(message)));
    crate::debug::print("I2C-HID", message);
}

/// Return the latest I2C-HID status for the taskbar compositor.
pub fn status_snapshot() -> Option<(String, String)> {
    STATUS.lock().clone()
}

/// Publish a visible result when the expected LPSS PCI function is absent.
pub fn publish_absent() {
    publish_status("PCI 54e8 absent");
}

/// Probe and initialise one ACPI-described HID-over-I2C touch device.
///
/// The platform config identifies the controller and bus wiring.  HID
/// vendor/product IDs are read from the wire descriptor and are not used to
/// select the driver.
pub fn init_i2c_hid(
    ctx: &'static dyn DriverContext,
    device: &PciDevice,
    profile: I2cHidPlatformConfig,
) -> Result<(), I2cHidError> {
    let mut bus = DesignWareI2c::new(ctx, device, profile)?;
    bus.probe_address().map_err(|error| {
        log::warn!(
            "[nitrogen] I2C-HID address probe failed: target=0x{:02x} error={:?}",
            profile.i2c_address,
            error
        );
        crate::debug_status!("I2C-HID", "ADDR ERR {:?}", error);
        error
    })?;
    log::info!(
        "[nitrogen] I2C-HID address acknowledged: target=0x{:02x} descriptor_reg=0x{:02x}",
        profile.i2c_address,
        profile.hid_descriptor_register
    );
    crate::debug_status!("I2C-HID", "ADDR OK 0x{:02x}", profile.i2c_address);
    let mut hid_bytes = [0u8; MAX_HID_DESCRIPTOR];
    bus.read_register(
        profile.hid_descriptor_register,
        &mut hid_bytes[..HID_DESCRIPTOR_LENGTH],
    )
    .map_err(|error| {
        log::warn!(
            "[nitrogen] I2C-HID descriptor read failed: reg=0x{:02x} len={} error={:?}",
            profile.hid_descriptor_register,
            HID_DESCRIPTOR_LENGTH,
            error
        );
        crate::debug_status!("I2C-HID", "DESC ERR {:?}", error);
        error
    })?;
    let descriptor =
        HidI2cDescriptor::parse(&hid_bytes[..HID_DESCRIPTOR_LENGTH]).ok_or_else(|| {
            log::warn!(
                "[nitrogen] invalid I2C-HID descriptor: first bytes={:02x?}",
                &hid_bytes[..HID_DESCRIPTOR_LENGTH.min(8)]
            );
            crate::debug_status!("I2C-HID", "DESC INVALID");
            I2cHidError::InvalidDescriptor
        })?;
    log::info!(
        "[nitrogen] I2C-HID descriptor: report_reg=0x{:04x} report_len={} input_reg=0x{:04x} input_max={} command_reg=0x{:04x} data_reg=0x{:04x} id={:04x}:{:04x} version=0x{:04x}",
        descriptor.report_desc_register,
        descriptor.report_desc_length,
        descriptor.input_register,
        descriptor.max_input_length,
        descriptor.command_register,
        descriptor.data_register,
        descriptor.vendor_id,
        descriptor.product_id,
        descriptor.version_id
    );
    crate::debug_status!(
        "I2C-HID",
        "DESC {:04x}:{:04x} IN{}",
        descriptor.vendor_id,
        descriptor.product_id,
        descriptor.max_input_length
    );
    // Follow Linux's power-on/reset sequence.  IRQ 81 is not wired into the
    // current Fullerene APIC input path yet, so the reset completion wait is
    // bounded and the normal polling path is used below.
    power_on(&mut bus, descriptor.command_register)?;
    bus.write_command(descriptor.command_register, &[0x00, 0x01])?;
    let reset_ack = bus.wait_reset_completion(descriptor.max_input_length as usize);
    if !reset_ack {
        // Linux continues after its one-second reset wait and reports the
        // missing IRQ acknowledgement, so keep the same recovery behavior
        // while still using the polling transport here.
        log::warn!("[nitrogen] I2C-HID reset acknowledgement timed out");
        crate::debug_status!("I2C-HID", "RESET TIMEOUT");
    } else {
        log::info!("[nitrogen] I2C-HID reset acknowledgement received");
        crate::debug_status!("I2C-HID", "RESET ACK");
    }
    power_on(&mut bus, descriptor.command_register)?;

    let report_length = descriptor.report_desc_length as usize;
    if report_length > MAX_REPORT_DESCRIPTOR {
        return Err(I2cHidError::InvalidDescriptor);
    }
    let mut report_bytes = Vec::new();
    report_bytes.resize(report_length, 0);
    bus.read_register(descriptor.report_desc_register, &mut report_bytes)
        .map_err(|error| {
            log::warn!(
                "[nitrogen] I2C-HID report descriptor read failed: reg=0x{:04x} len={} error={:?}",
                descriptor.report_desc_register,
                report_length,
                error
            );
            I2cHidError::InvalidReportDescriptor
        })?;
    let report = HidReportDescriptor::parse(&report_bytes).map_err(|error| {
        log::warn!(
            "[nitrogen] I2C-HID report descriptor parse failed: {:?}",
            error
        );
        crate::debug_status!("I2C-HID", "REPORT INVALID");
        I2cHidError::InvalidReportDescriptor
    })?;
    let fields = report.touchpad_fields().ok_or_else(|| {
        crate::debug_status!("I2C-HID", "TOUCH INVALID");
        I2cHidError::InvalidReportDescriptor
    })?;
    log::info!(
        "[nitrogen] I2C-HID report descriptor parsed: max_input_bytes={} report_id={} x_bits={} y_bits={}",
        report.max_input_bytes(),
        fields.x.report_id,
        fields.x.bit_size,
        fields.y.bit_size
    );
    crate::debug_status!(
        "I2C-HID",
        "READY x{} y{}",
        fields.x.bit_size,
        fields.y.bit_size
    );
    crate::debug_status!(
        "I2C-HID",
        "READY x{} y{} RST={}",
        fields.x.bit_size,
        fields.y.bit_size,
        if reset_ack { "OK" } else { "TO" }
    );
    let status = alloc::format!(
        "READY x{} y{} RST={}",
        fields.x.bit_size,
        fields.y.bit_size,
        if reset_ack { "OK" } else { "TO" }
    );
    publish_status(&status);
    let state = I2cHidTouchpad {
        bus,
        descriptor,
        report,
        fields,
    };
    *TOUCHPAD.lock() = Some(state);
    *LATEST_INPUT.lock() = None;
    POLL_ERROR_REPORTED.store(false, Ordering::Release);
    POLL_CONSECUTIVE_FAILURES.store(0, Ordering::Release);
    FIRST_INPUT_REPORTED.store(false, Ordering::Release);
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
pub fn init_n150(ctx: &'static dyn DriverContext, device: &PciDevice) -> Result<(), I2cHidError> {
    init_i2c_hid(ctx, device, crate::hid::GEMIBOOK_N150_I2C_HID)
}

fn store_input(input: TouchpadInput) {
    let mut slot = LATEST_INPUT.lock();
    if let Some((x, y)) = input.relative {
        if let Some(previous) = slot.as_mut() {
            // Preserve every relative packet until the desktop consumes it.
            // The scheduler is slower than a busy HID device, so replacing
            // the old packet here would create both lag and post-release
            // motion.  Keep the preceding absolute contact snapshot as well:
            // the N150 may interleave digitizer and relative-mouse reports.
            let (previous_x, previous_y) = previous.relative.unwrap_or((0, 0));
            previous.relative = Some((previous_x.saturating_add(x), previous_y.saturating_add(y)));
            previous.report.buttons = input.report.buttons;
            previous.report.in_contact = input.report.in_contact;
            return;
        }
    }
    *slot = Some(input);
}

/// Record an input interrupt without touching the I2C controller from ISR
/// context. The kernel masks the level-triggered GSI before calling this.
pub fn handle_interrupt() {
    INPUT_PENDING.store(true, Ordering::Release);
}

/// Enable interrupt-driven input after the platform has routed the ACPI GSI.
/// A pending service is requested once so a report already waiting in the
/// device FIFO is not lost during the hand-off from boot polling.
pub fn enable_interrupt_mode() {
    INPUT_PENDING.store(true, Ordering::Release);
    INPUT_INTERRUPT_MODE.store(true, Ordering::Release);
}

/// Install the kernel-side callback that unmasks the level-triggered GSI
/// after a normal-context transfer has drained the device FIFO.
pub fn install_interrupt_rearm(callback: fn()) {
    *INPUT_INTERRUPT_REARM.lock() = Some(callback);
}

fn rearm_interrupt() {
    if let Some(callback) = *INPUT_INTERRUPT_REARM.lock() {
        callback();
    }
}

/// Return whether the HID path is using the platform interrupt.
pub fn interrupt_mode_enabled() -> bool {
    INPUT_INTERRUPT_MODE.load(Ordering::Acquire)
}

/// Service HID input in normal scheduler context and drain a bounded number
/// of reports. With a working IRQ this does no I2C work while the device is
/// idle; polling remains as a compatibility fallback when IRQ routing is not
/// available on a platform.
pub fn service_input() -> bool {
    if !is_initialized() {
        return false;
    }
    let interrupt_mode = INPUT_INTERRUPT_MODE.load(Ordering::Acquire);
    if interrupt_mode {
        if !INPUT_PENDING.swap(false, Ordering::AcqRel) {
            return false;
        }
    }
    let now = unsafe { core::arch::x86_64::_rdtsc() };
    if !interrupt_mode {
        let previous = LAST_SERVICE_TSC.load(Ordering::Relaxed);
        let failures = POLL_CONSECUTIVE_FAILURES.load(Ordering::Relaxed).min(4);
        let interval_us = I2C_INPUT_POLL_INTERVAL_US.saturating_mul(1u64 << failures);
        if previous != 0
            && now.wrapping_sub(previous)
                < crate::timing::ticks_per_us().saturating_mul(interval_us)
        {
            return false;
        }
    }
    LAST_SERVICE_TSC.store(now, Ordering::Relaxed);
    let length = {
        let guard = TOUCHPAD.lock();
        let Some(device) = guard.as_ref() else {
            rearm_interrupt();
            return false;
        };
        (device.descriptor.max_input_length as usize).min(MAX_INPUT_REPORT)
    };
    if length < 2 {
        rearm_interrupt();
        return true;
    }
    let service_start = unsafe { core::arch::x86_64::_rdtsc() };
    for _ in 0..MAX_INPUT_REPORTS_PER_POLL {
        let mut bytes = [0u8; MAX_INPUT_REPORT];
        let report_length = match {
            let mut guard = TOUCHPAD.lock();
            let Some(device) = guard.as_mut() else {
                rearm_interrupt();
                return false;
            };
            device.bus.read_input_report(&mut bytes[..length])
        } {
            Ok(length) => length,
            Err(error) => {
                POLL_CONSECUTIVE_FAILURES.fetch_add(1, Ordering::AcqRel);
                if !POLL_ERROR_REPORTED.swap(true, Ordering::AcqRel) {
                    log::warn!(
                        "[nitrogen] I2C-HID input read failed: max_len={} error={:?}",
                        length,
                        error
                    );
                    crate::debug_status!("I2C-HID", "INPUT ERR {:?}", error);
                    let status = alloc::format!("INPUT ERR {:?}", error);
                    publish_status(&status);
                }
                break;
            }
        };
        POLL_CONSECUTIVE_FAILURES.store(0, Ordering::Release);
        // Zero is the normal HID-over-I2C idle response, not a malformed
        // packet. It also terminates the bounded drain once the queue is
        // empty, so old motion cannot be replayed indefinitely.
        if report_length == 0 {
            break;
        }
        if report_length < 2 || report_length > length {
            if !POLL_ERROR_REPORTED.swap(true, Ordering::AcqRel) {
                log::warn!(
                    "[nitrogen] I2C-HID input length invalid: embedded={} transfer_len={}",
                    report_length,
                    length
                );
                crate::debug_status!("I2C-HID", "INPUT LEN {}", report_length);
                let status = alloc::format!("INPUT LEN {}", report_length);
                publish_status(&status);
            }
            break;
        }
        let payload = &bytes[2..report_length];
        let (decoded, relative, contacts, contact_count, x_min, x_max, y_min, y_max) = {
            let guard = TOUCHPAD.lock();
            let Some(device) = guard.as_ref() else {
                rearm_interrupt();
                return false;
            };
            let decoded = device.report.decode_touchpad(device.fields, payload);
            let (contacts, contact_count) = if decoded.is_some() {
                device
                    .report
                    .decode_touchpad_contacts(device.fields, payload)
            } else {
                ([None; MAX_TOUCH_CONTACTS], 0)
            };
            let relative = if decoded.is_none() {
                device.report.decode_relative_mouse(payload)
            } else {
                None
            };
            (
                decoded,
                relative,
                contacts,
                contact_count,
                device.fields.x.logical_minimum,
                device.fields.x.logical_maximum,
                device.fields.y.logical_minimum,
                device.fields.y.logical_maximum,
            )
        };
        let input_kind = if decoded.is_some() {
            "ABS"
        } else if relative.is_some() {
            "REL"
        } else {
            "ERR"
        };
        if !FIRST_INPUT_REPORTED.swap(true, Ordering::AcqRel) {
            log::info!(
                "[nitrogen] I2C-HID first input: len={} report_id={} payload={:02x?}",
                report_length,
                bytes.get(2).copied().unwrap_or(0),
                &bytes[2..report_length.min(34)]
            );
            crate::debug_status!(
                "I2C-HID",
                "INPUT len{} id{} {}",
                report_length,
                bytes.get(2).copied().unwrap_or(0),
                input_kind
            );
            let status = alloc::format!(
                "INPUT len{} id{} {}",
                report_length,
                bytes.get(2).copied().unwrap_or(0),
                input_kind
            );
            publish_status(&status);
        }
        if let Some(report) = decoded {
            store_input(TouchpadInput {
                report,
                x_min,
                x_max,
                y_min,
                y_max,
                relative: None,
                contacts,
                contact_count,
            });
        } else if let Some((x, y, buttons)) = relative {
            store_input(TouchpadInput {
                report: TouchpadReport {
                    x: 0,
                    y: 0,
                    buttons,
                    in_contact: buttons != 0,
                },
                x_min,
                x_max,
                y_min,
                y_max,
                relative: Some((x, y)),
                contacts: [None; MAX_TOUCH_CONTACTS],
                contact_count: 0,
            });
        } else if !POLL_ERROR_REPORTED.swap(true, Ordering::AcqRel) {
            log::warn!(
                "[nitrogen] I2C-HID input report could not be decoded: report_id={} len={}",
                bytes.get(2).copied().unwrap_or(0),
                report_length
            );
            crate::debug_status!(
                "I2C-HID",
                "DECODE ERR id{}",
                bytes.get(2).copied().unwrap_or(0)
            );
            let status = alloc::format!("DECODE ERR id{}", bytes.get(2).copied().unwrap_or(0));
            publish_status(&status);
        }
    }
    let elapsed = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(service_start);
    INPUT_SERVICE_COUNT.fetch_add(1, Ordering::Relaxed);
    INPUT_SERVICE_TOTAL_TSC.fetch_add(elapsed, Ordering::Relaxed);
    let mut maximum = INPUT_SERVICE_MAX_TSC.load(Ordering::Relaxed);
    while elapsed > maximum {
        match INPUT_SERVICE_MAX_TSC.compare_exchange_weak(
            maximum,
            elapsed,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(current) => maximum = current,
        }
    }
    if interrupt_mode {
        rearm_interrupt();
    }
    true
}

/// Compatibility entry point for callers that still perform input servicing
/// from their own event-loop pump.
pub fn poll_input() {
    let _ = service_input();
}

pub fn consume_input() -> Option<TouchpadInput> {
    core::mem::take(&mut *LATEST_INPUT.lock())
}

pub fn is_initialized() -> bool {
    TOUCHPAD.lock().is_some()
}

/// Return `(service_count, total_tsc, maximum_service_tsc)` for diagnostics.
pub fn input_service_metrics() -> (u64, u64, u64) {
    (
        INPUT_SERVICE_COUNT.load(Ordering::Relaxed),
        INPUT_SERVICE_TOTAL_TSC.load(Ordering::Relaxed),
        INPUT_SERVICE_MAX_TSC.load(Ordering::Relaxed),
    )
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
