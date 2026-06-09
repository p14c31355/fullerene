#![allow(dead_code)] // Many CSR constants / fields are placeholders for firmware-loaded path

//! Intel Wireless 7265 (iwlwifi 7000 series) driver.
//!
//! Implements `bonder::NetDevice` for the Intel Wireless 7265 PCIe Wi-Fi adapter.
//!
//! ## Hardware
//!
//! - Vendor: 0x8086 (Intel)
//! - Device: 0x095b (Wireless 7265)
//! - Class: 0x02 (Network), Subclass: 0x80 (Other)
//! - Interface: PCIe, MSI-X or legacy INTx interrupts
//!
//! ## Architecture
//!
//! ```text
//! PCI config space → BAR0 (MMIO) → CSR registers
//!                                 → TX DMA ring
//!                                 → RX DMA ring
//! ```
//!
//! ## Limitations
//!
//! - Firmware loading is NOT implemented (requires iwlwifi-7265-*.ucode)
//! - Without firmware, the device stays in a minimal initialisation state
//! - TX/RX are stubs until firmware is loaded and alive
//! - Only PCIe gen1/2 negotiation is attempted
//!
//! ## References
//!
//! - Intel 7265 datasheet (public sections)
//! - Linux iwlwifi driver (drivers/net/wireless/intel/iwlwifi/)

use alloc::boxed::Box;

use bonder::{NetDevice, NetError};

use crate::pci::{PciDevice, PciScanner};

// ── PCI identifiers ───────────────────────────────────────────────────

const IWL_PCI_VENDOR: u16 = 0x8086;
const IWL_PCI_DEVICE_7265: u16 = 0x095b;

/// Additional 7265-family device IDs for future expansion.
const IWL_DEVICE_IDS: &[u16] = &[0x095b, 0x095a, 0x08b1, 0x08b2];

// ── CSR (Control and Status Register) offsets ─────────────────────────
//
// All offsets are 32-bit aligned (offset / 4 to index into a `*mut u32`).

/// Hardware revision register
const CSR_HW_REV: u32 = 0x028 / 4;
/// RF ID register
const CSR_HW_RF_ID: u32 = 0x034 / 4;
/// General Purpose Control
const CSR_GIO: u32 = 0x03C / 4;
/// UCode General Purpose Control
const CSR_UCODE_GP1: u32 = 0x054 / 4;
/// GPIO Driver
const CSR_GP_DRIVER: u32 = 0x098 / 4;
/// LED register
const CSR_LED_REG: u32 = 0x094 / 4;
/// DRAM base address table
const CSR_DRAM_INT_TBL: u32 = 0x0A0 / 4;
/// General Purpose Control 2
const CSR_GIO2: u32 = 0x0EC / 4;

/// Hardware configuration: reset bit
const CSR_RESET: u32 = 0x020 / 4;
/// Hardware configuration: clock
const CSR_GP_CNTRL: u32 = 0x024 / 4;
/// Hardware configuration: EEPROM status
const CSR_EEPROM_GP: u32 = 0x02C / 4;
/// Hardware configuration: OTP status
const CSR_OTP_GP: u32 = 0x030 / 4;

/// Interrupt Cause Register
const CSR_INT: u32 = 0x008 / 4;
/// Interrupt Mask Register
const CSR_INT_MASK: u32 = 0x00C / 4;
/// Force Interrupt Register (write to assert test interrupt)
const CSR_FH_INT: u32 = 0x010 / 4;
/// Interrupt Status (read to clear)
const CSR_INT_PERIODIC: u32 = 0x014 / 4;

// ── Reset / power-on constants ────────────────────────────────────────

const CSR_RESET_BIT_SW: u32 = 1 << 7;
const CSR_RESET_BIT_MASTER_DISABLED: u32 = 1 << 8;
const CSR_RESET_BIT_STOP_MASTER: u32 = 1 << 9;

const CSR_GP_CNTRL_MAC_ACCESS_EN: u32 = 1 << 4;
const CSR_GP_CNTRL_MAC_CLOCK_READY: u32 = 1 << 0;

// ── IwlWifiDevice ─────────────────────────────────────────────────────

/// Intel Wireless 7265 NIC driver.
///
/// Implements `bonder::NetDevice`. Requires firmware to be loaded
/// externally (through Linux/iwldvm or iwlmvm) for full operation.
pub struct IwlWifiDevice {
    /// MAC address read from EEPROM / NVM
    mac: [u8; 6],
    /// PCI device for config space access
    _pci_dev: PciDevice,
    /// MMIO BAR0 virtual address (identity-mapped)
    mmio: *mut u32,
    /// Hardware revision
    hw_rev: u16,
    /// TX buffer (single-frame copy)
    tx_buf: Box<[u8; 1536]>,
}

unsafe impl Send for IwlWifiDevice {}

// ── public API ────────────────────────────────────────────────────────

impl IwlWifiDevice {
    /// Scan the PCI bus for an Intel Wireless 7265 and initialise it.
    ///
    /// Returns `None` if no suitable device is found or initialisation fails.
    pub fn probe_and_init() -> Option<Self> {
        let mut scanner = PciScanner::new();
        let _ = scanner.scan_all_buses();

        for device in scanner.get_devices() {
            if device.class_code != 0x02 || device.subclass != 0x80 {
                continue;
            }
            if device.vendor_id != IWL_PCI_VENDOR {
                continue;
            }
            if !IWL_DEVICE_IDS.contains(&device.device_id) {
                continue;
            }

            log::info!(
                "iwlwifi: found device {:04x}:{:04x} at {:02x}:{:02x}.{:01x}",
                device.vendor_id,
                device.device_id,
                device.bus,
                device.device,
                device.function,
            );

            match Self::init(device.clone()) {
                Ok(s) => return Some(s),
                Err(_) => {
                    log::warn!(
                        "iwlwifi: init failed for {:02x}:{:02x}.{:01x}",
                        device.bus,
                        device.device,
                        device.function
                    );
                    continue;
                }
            }
        }

        log::info!("iwlwifi: no device found");
        None
    }

    /// Initialise a previously discovered PCI device.
    fn init(device: PciDevice) -> Result<Self, IwlError> {
        // Enable memory-space access and bus-mastering
        device.enable_memory_access();

        // Read BAR0 (MMIO)
        let bar0_addr = device.read_bar(0).ok_or(IwlError::BarNotAvailable)?;

        let mmio = bar0_addr as *mut u32;

        // ── read hardware revision ──────────────────────────────────
        let hw_rev_raw = unsafe { core::ptr::read_volatile(mmio.add(CSR_HW_REV as usize)) };
        let hw_rev = ((hw_rev_raw >> 4) & 0xFFFF) as u16;

        log::info!(
            "iwlwifi: HW_REV={:#06x} (raw={:#010x})",
            hw_rev,
            hw_rev_raw,
        );

        // ── stop and reset the device ───────────────────────────────
        unsafe {
            // Write to RESET register: stop master, software reset
            let reset_val = CSR_RESET_BIT_STOP_MASTER;
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), reset_val);

            // Wait for master to be disabled
            for _ in 0..100_000 {
                let r = core::ptr::read_volatile(mmio.add(CSR_RESET as usize));
                if (r & CSR_RESET_BIT_MASTER_DISABLED) != 0 {
                    break;
                }
                core::hint::spin_loop();
            }

            // Software reset
            core::ptr::write_volatile(
                mmio.add(CSR_RESET as usize),
                CSR_RESET_BIT_SW,
            );
            for _ in 0..200_000 {
                core::hint::spin_loop();
            }

            // Clear reset
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), 0);
            for _ in 0..200_000 {
                core::hint::spin_loop();
            }
        }

        // ── enable MAC clock ────────────────────────────────────────
        unsafe {
            core::ptr::write_volatile(
                mmio.add(CSR_GP_CNTRL as usize),
                CSR_GP_CNTRL_MAC_ACCESS_EN,
            );
        }

        // Wait for clock to stabilise
        for _ in 0..50_000 {
            let gp = unsafe { core::ptr::read_volatile(mmio.add(CSR_GP_CNTRL as usize)) };
            if (gp & CSR_GP_CNTRL_MAC_CLOCK_READY) != 0 {
                break;
            }
            core::hint::spin_loop();
        }

        let gp_final = unsafe { core::ptr::read_volatile(mmio.add(CSR_GP_CNTRL as usize)) };
        log::info!(
            "iwlwifi: GP_CNTRL={:#010x} (MAC_CLOCK_READY={})",
            gp_final,
            (gp_final & CSR_GP_CNTRL_MAC_CLOCK_READY) != 0,
        );

        // ── read MAC address from NVM/OTP ───────────────────────────
        // The MAC is stored in the OTP/NVM at specific offsets.
        // For now, read from the first 6 bytes of the EEPROM indirect area.
        // In a full driver, this would parse the NVM sections properly.
        let mac: [u8; 6] = unsafe {
            // Try reading from hardware; if unavailable, use a dummy
            let eeprom_ctrl = core::ptr::read_volatile(mmio.add(CSR_EEPROM_GP as usize));
            if eeprom_ctrl != 0 && eeprom_ctrl != 0xFFFF_FFFF {
                // Read MAC from NVM (simplified — real driver uses more complex addressing)
                let mac_lo = core::ptr::read_volatile(mmio.add((CSR_DRAM_INT_TBL + 0) as usize));
                let mac_hi = core::ptr::read_volatile(mmio.add((CSR_DRAM_INT_TBL + 1) as usize));
                [
                    mac_lo as u8,
                    (mac_lo >> 8) as u8,
                    (mac_lo >> 16) as u8,
                    (mac_lo >> 24) as u8,
                    mac_hi as u8,
                    (mac_hi >> 8) as u8,
                ]
            } else {
                // Dummy MAC: locally administered unicast
                [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
            }
        };

        log::info!(
            "iwlwifi: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        );

        // ── mask all interrupts (for now) ───────────────────────────
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        // ── allocate TX buffer ──────────────────────────────────────
        let tx_buf: Box<[u8; 1536]> = Box::new([0u8; 1536]);

        log::info!("iwlwifi: initialised (firmware NOT loaded — TX/RX stubs)");

        Ok(Self {
            mac,
            _pci_dev: device,
            mmio,
            hw_rev,
            tx_buf,
        })
    }
}

// ── NetDevice implementation ──────────────────────────────────────────

impl NetDevice for IwlWifiDevice {
    fn send_frame(&mut self, _frame: &[u8]) -> Result<(), NetError> {
        // Firmware is required for TX. Without it, we cannot send.
        // TODO: implement TX DMA ring with firmware alive check
        Err(NetError::SendFailed)
    }

    fn poll_frame(&mut self, _buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        // Firmware is required for RX. Without it, we cannot receive.
        // TODO: implement RX DMA ring with firmware alive check
        Ok(None)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

// ── internal helpers ──────────────────────────────────────────────────

impl IwlWifiDevice {
    /// Read a 32-bit CSR register.
    #[allow(dead_code)]
    fn csr_read(&self, offset: u32) -> u32 {
        unsafe { core::ptr::read_volatile(self.mmio.add(offset as usize)) }
    }

    /// Write a 32-bit CSR register.
    #[allow(dead_code)]
    fn csr_write(&self, offset: u32, value: u32) {
        unsafe {
            core::ptr::write_volatile(self.mmio.add(offset as usize), value);
        }
    }

    /// Check if the hardware is ready (master enabled, no reset active).
    #[allow(dead_code)]
    fn is_hw_ready(&self) -> bool {
        let reset = self.csr_read(CSR_RESET);
        if (reset & CSR_RESET_BIT_MASTER_DISABLED) != 0 {
            return false;
        }
        if (reset & CSR_RESET_BIT_SW) != 0 {
            return false;
        }
        true
    }
}

// ── interrupt stubs ───────────────────────────────────────────────────

/// Interrupt cause bits (for future use).
#[allow(dead_code)]
const INT_CSR_FH_RX: u32 = 1 << 18;
#[allow(dead_code)]
const INT_CSR_FH_TX: u32 = 1 << 19;
#[allow(dead_code)]
const INT_CSR_ALIVE: u32 = 1 << 0;
#[allow(dead_code)]
const INT_CSR_SW_ERR: u32 = 1 << 25;
#[allow(dead_code)]
const INT_CSR_RF_KILL: u32 = 1 << 7;

/// Check and acknowledge interrupts (for future ISR use).
#[allow(dead_code)]
fn check_interrupts(mmio: *mut u32) -> u32 {
    let int_cause = unsafe { core::ptr::read_volatile(mmio.add(CSR_INT as usize)) };
    if int_cause == 0 || int_cause == 0xFFFF_FFFF {
        return 0;
    }
    // Acknowledge: write 1 to clear
    unsafe {
        core::ptr::write_volatile(mmio.add(CSR_INT as usize), int_cause);
    }
    // Clear periodic interrupt status
    unsafe {
        let _ = core::ptr::read_volatile(mmio.add(CSR_INT_PERIODIC as usize));
    }
    int_cause
}

// ── error type ────────────────────────────────────────────────────────

#[derive(Debug)]
enum IwlError {
    BarNotAvailable,
}