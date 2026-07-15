//! PCI Device Abstraction
//!
//! This module provides PCI device abstraction and configuration space access
//! for unified hardware management. No kernel or boot crate dependencies — only
//! `x86_64`, `alloc`, and `log`.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::port::PortWriter;

// ── ECAM (Enhanced Configuration Access Mechanism) ──────────────
//
// PCIe Configuration Mechanism #1 (I/O ports 0xCF8/0xCFC) can only
// address offsets 0x00..0xFC (256 bytes).  Extended capabilities
// (L1Sub, AER, etc.) live at offsets ≥ 0x100 and require ECAM, which
// maps the full 4 KiB per-device config space into MMIO.
//
// These statics are populated once by the kernel after it parses the
// MCFG ACPI table.

/// Serializes the address/data pair used by PCI configuration mechanism #1.
static PCI_CONFIG_LOCK: AtomicBool = AtomicBool::new(false);

/// Acquire the PCI config space lock.
///
/// A retry limit prevents deadlock if the lock is re-entered from the same
/// context (e.g. an NMI that interrupted a locked region).  In normal
/// operation the lock is held for a single I/O transaction so contention
/// is negligible.
#[inline]
fn pci_config_lock_acquire() {
    let mut retries = 0u32;
    while PCI_CONFIG_LOCK.compare_exchange_weak(
        false,
        true,
        Ordering::Acquire,
        Ordering::Relaxed,
    ).is_err() {
        retries += 1;
        if retries > 100_000 {
            log::warn!("PCI: config lock contention exceeded 100k spins");
            break;
        }
        core::hint::spin_loop();
    }
}

/// Release the PCI config space lock.
#[inline]
fn pci_config_lock_release() {
    PCI_CONFIG_LOCK.store(false, Ordering::Release);
}

/// Abandon a configuration transaction interrupted by fatal NMI recovery.
/// The watchdog redirects execution away from the interrupted context, so it
/// cannot resume and release the lock itself.
pub fn reset_config_lock_for_recovery() {
    PCI_CONFIG_LOCK.store(false, Ordering::Release);
}

static ECAM_BASE: AtomicU64 = AtomicU64::new(0);
static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);
static ECAM_START_BUS: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
static ECAM_END_BUS: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Store the ECAM MMIO base (physical), the phys→virt offset, and the
/// bus range covered by the ECAM segment.
///
/// Must be called once during boot, after the MCFG table is parsed.
/// Without this, extended config space (offsets ≥ 0x100) cannot be
/// accessed — L1Sub and AER configuration will be skipped.
pub fn set_ecam_info(ecam_base: u64, phys_offset: u64, start_bus: u8, end_bus: u8) {
    ECAM_BASE.store(ecam_base, core::sync::atomic::Ordering::Relaxed);
    PHYS_OFFSET.store(phys_offset, core::sync::atomic::Ordering::Relaxed);
    ECAM_START_BUS.store(start_bus, core::sync::atomic::Ordering::Relaxed);
    ECAM_END_BUS.store(end_bus, core::sync::atomic::Ordering::Relaxed);
}

/// Convert a physical address to a virtual pointer using the stored offset.
fn ecam_phys_to_virt(phys: u64) -> usize {
    let offset = PHYS_OFFSET.load(core::sync::atomic::Ordering::Relaxed);
    (phys + offset) as usize
}

/// Return the virtual address for the ECAM register of `bus:dev.func` at `offset`.
///
/// Layout per the PCIe spec:
///   offset = (bus << 20) | (device << 15) | (function << 12) | register_offset
///
/// The bus number is adjusted by subtracting the MCFG start_bus before computing
/// the ECAM offset, so that bus addresses are relative to the ECAM window base.
fn ecam_addr(bus: u8, device: u8, function: u8, offset: u16) -> usize {
    let base = ECAM_BASE.load(core::sync::atomic::Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    let start_bus = ECAM_START_BUS.load(core::sync::atomic::Ordering::Relaxed);
    let end_bus = ECAM_END_BUS.load(core::sync::atomic::Ordering::Relaxed);
    // Reject buses outside the ECAM window — avoids out-of-bounds MMIO.
    if bus < start_bus || bus > end_bus {
        return 0;
    }
    let bus_offset = (bus - start_bus) as u64;
    let phys = base
        + (bus_offset << 20)
        + ((device as u64 & 0x1F) << 15)
        + ((function as u64 & 0x7) << 12)
        + (offset as u64 & 0xFFF);
    ecam_phys_to_virt(phys)
}

/// Pre-flight check: is the device present on the PCI bus?
///
/// Uses port I/O (CF8/CFC) which always completes, even for unresponsive
/// devices.  Returns `true` if the vendor ID is valid (> 0x0000 and < 0xFFFF).
fn ext_dev_present(bus: u8, device: u8, function: u8) -> bool {
    let vendor = PciConfigSpace::read_config_word(bus, device, function, 0);
    vendor != 0xFFFF && vendor != 0x0000
}

/// Read a DWORD from extended PCIe config space (offset ≥ 0x100) via ECAM.
///
/// Returns 0xFFFF_FFFF if ECAM is not configured, the device is absent,
/// or the capability is not present.
///
/// # ⚠️ Hang risk for endpoint devices
///
/// `ext_dev_present` uses port I/O (CF8/CFC) to check the vendor ID, which
/// always completes even when the device is in L1.  However, a device in
/// ASPM L1 **cannot** complete ECAM MMIO reads — the CPU will hang forever.
///
/// **This function must only be called for upstream bridge devices**, whose
/// ECAM reads always complete because the bridge is never in L1 relative to
/// the CPU.  Calling this for an endpoint that may be in L1 will cause a
/// system hang.
///
/// For endpoint extended config access, L1 must first be disabled on the
/// upstream bridge so the link returns to L0.  Prefer `PciDevice` methods
/// that use port I/O (standard config space, offset < 0x100) for endpoints.
pub fn read_ext_dword(bus: u8, device: u8, function: u8, offset: u16) -> u32 {
    let va = ecam_addr(bus, device, function, offset);
    if va == 0 || !ext_dev_present(bus, device, function) {
        return 0xFFFF_FFFF;
    }
    // Only allow ECAM reads for bridge devices (class 0x06, subclass 0x04)
    let class_rev = PciConfigSpace::read_config_dword(bus, device, function, 8);
    let class_code = (class_rev >> 24) as u8;
    let subclass = (class_rev >> 16) as u8;
    if class_code != 0x06 || subclass != 0x04 {
        return 0xFFFF_FFFF;
    }
    unsafe { core::ptr::read_volatile(va as *const u32) }
}

/// Write a DWORD to extended PCIe config space (offset ≥ 0x100) via ECAM.
///
/// Writes are posted transactions and never hang, so no pre-flight check
/// is needed.  No-op if ECAM is not configured.
pub fn write_ext_dword(bus: u8, device: u8, function: u8, offset: u16, value: u32) {
    let va = ecam_addr(bus, device, function, offset);
    if va == 0 {
        return;
    }
    unsafe { core::ptr::write_volatile(va as *mut u32, value) }
}

#[derive(Debug, Clone, Copy)]
pub struct PciBar {
    pub index: u8,
    pub address: u64,
    pub size: u32,
    pub is_io: bool,
    pub is_64bit: bool,
    pub is_prefetchable: bool,
}

/// PCI Configuration Space Header
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PciConfigSpace {
    pub vendor_id: u16,
    pub device_id: u16,
    pub command: u16,
    pub status: u16,
    pub revision_id: u8,
    pub prog_if: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub cache_line_size: u8,
    pub latency_timer: u8,
    pub header_type: u8,
    pub bist: u8,
}

impl PciConfigSpace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read_from_device(bus: u8, device: u8, function: u8) -> Option<Self> {
        if !Self::device_exists(bus, device, function) {
            return None;
        }

        let mut config = Self::new();
        config.read_config_space(bus, device, function);
        Some(config)
    }

    fn device_exists(bus: u8, device: u8, function: u8) -> bool {
        Self::read_config_word(bus, device, function, 0) != 0xFFFF
    }

    fn read_config_space(&mut self, bus: u8, device: u8, function: u8) {
        self.vendor_id = Self::read_config_word(bus, device, function, 0);
        self.device_id = Self::read_config_word(bus, device, function, 2);
        self.command = Self::read_config_word(bus, device, function, 4);
        self.status = Self::read_config_word(bus, device, function, 6);
        self.revision_id = Self::read_config_byte(bus, device, function, 8);
        self.prog_if = Self::read_config_byte(bus, device, function, 9);
        self.subclass = Self::read_config_byte(bus, device, function, 10);
        self.class_code = Self::read_config_byte(bus, device, function, 11);
        self.cache_line_size = Self::read_config_byte(bus, device, function, 12);
        self.latency_timer = Self::read_config_byte(bus, device, function, 13);
        self.header_type = Self::read_config_byte(bus, device, function, 14);
        self.bist = Self::read_config_byte(bus, device, function, 15);
    }

    fn build_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        0x80000000u32
            | ((bus as u32) << 16)
            | ((device as u32) << 11)
            | ((function as u32) << 8)
            | (offset as u32 & 0xFC)
    }

    pub fn read_config_byte(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
        let dword = Self::read_config_dword(bus, device, function, offset);
        (dword >> ((offset & 3) * 8)) as u8
    }

    pub fn read_config_word(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
        let dword = Self::read_config_dword(bus, device, function, offset);
        let shift = if offset % 4 < 2 { 0 } else { 16 };
        ((dword >> shift) & 0xFFFF) as u16
    }

    pub fn read_config_dword(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        pci_config_lock_acquire();
        let value = Self::read_config_dword_unlocked(bus, device, function, offset);
        pci_config_lock_release();
        value
    }

    fn read_config_dword_unlocked(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_reader = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_DATA);

        addr_writer.write_safe(address);
        data_reader.read_safe()
    }

    pub fn enable_memory_access(&mut self, bus: u8, device: u8, function: u8) {
        let command = self.command | 0x06;
        Self::write_config_word_raw(bus, device, function, 4, command);
        self.command = command;
    }

    pub fn write_config_dword(
        &mut self,
        bus: u8,
        device: u8,
        function: u8,
        offset: u8,
        value: u32,
    ) {
        Self::write_config_dword_raw(bus, device, function, offset, value);
    }

    /// Write a raw DWORD to PCI configuration space (internal, unlocked).
    ///
    /// Caller must hold PCI_CONFIG_LOCK.
    fn write_config_dword_unlocked(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let address = Self::build_config_address(bus, device, function, offset);
        let mut addr_writer = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_ADDRESS);
        let mut data_writer = PortWriter::new(crate::port::HardwarePorts::PCI_CONFIG_DATA);

        addr_writer.write_safe(address);
        data_writer.write_safe(value);
    }

    /// Write a raw WORD to PCI configuration space (internal, unlocked).
    ///
    /// Caller must hold PCI_CONFIG_LOCK.  Uses a DWORD read-modify-write so that
    /// the write-one-to-clear Status register in the adjacent half-word is not
    /// corrupted.  Native 16-bit I/O writes to the CFC port are not supported by
    /// all chipsets and may cause a system hang.
    fn write_config_word_unlocked(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
        let aligned = offset & !3;
        let shift = if offset % 4 < 2 { 0 } else { 16 };
        let existing = Self::read_config_dword_unlocked(bus, device, function, aligned);
        let masked = existing & !(0xFFFFu32 << shift);
        Self::write_config_dword_unlocked(
            bus, device, function, aligned,
            masked | ((value as u32) << shift),
        );
    }

    /// Write a raw WORD to PCI configuration space.
    ///
    /// Serializes CF8/CFC access with all other configuration transactions.
    pub fn write_config_word_raw(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
        pci_config_lock_acquire();
        Self::write_config_word_unlocked(bus, device, function, offset, value);
        pci_config_lock_release();
    }

    /// Write a raw DWORD to PCI configuration space.
    ///
    /// This is a low-level mechanism. Use `write_config_dword` on `PciConfigSpace`
    /// when you need to update the cached header fields as well.
    ///
    /// Serializes CF8/CFC access with all other configuration transactions.
    pub fn write_config_dword_raw(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        pci_config_lock_acquire();
        Self::write_config_dword_unlocked(bus, device, function, offset, value);
        pci_config_lock_release();
    }
}

/// PCI Device abstraction - public struct for external use
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub handle: usize,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    /// Header type (bits [6:0]): 0x00 = endpoint, 0x01 = PCI-to-PCI bridge,
    /// 0x02 = CardBus bridge.  Bit 7 indicates multi-function.
    pub header_type: u8,
}

impl PciDevice {
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        PrivatePciDevice::new(bus, device, function).map(PrivatePciDevice::into_public)
    }

    /// Enable memory-space access and bus-mastering for this device.
    /// The caller should invoke this once after obtaining a `PciDevice`
    /// and before performing MMIO or DMA operations.
    pub fn enable_memory_access(&self) -> bool {
        let cmd = PciConfigSpace::read_config_word(self.bus, self.device, self.function, 4);
        PciConfigSpace::write_config_word_raw(self.bus, self.device, self.function, 4, cmd | 0x06);
        PciConfigSpace::read_config_word(self.bus, self.device, self.function, 4) & 0x06 == 0x06
    }

    pub fn read_bar(&self, bar_index: u8) -> Option<u64> {
        if let Some(dev) = PrivatePciDevice::new(self.bus, self.device, self.function) {
            dev.read_bar(bar_index)
        } else {
            None
        }
    }

    /// Number of BAR registers for this device type.
    ///
    /// Type 0 (endpoint): up to 6 BARs at offsets 0x10..0x27.
    /// Type 1 (bridge):   up to 2 BARs at offsets 0x10..0x17.
    ///                     Offsets 0x18+ hold I/O / Memory / Prefetchable
    ///                     base-limit registers — probing them as BARs
    ///                     corrupts bus routing and causes system hangs.
    pub fn max_bars(&self) -> u8 {
        match self.header_type & 0x7F {
            0x01 => 2, // PCI-to-PCI bridge
            0x02 => 2, // CardBus bridge
            _ => 6,    // endpoint (including 0x00)
        }
    }

    /// Read the firmware-programmed BAR without issuing a size probe.
    pub fn read_bar_info(&self, index: u8) -> Option<PciBar> {
        if index >= self.max_bars() {
            return None;
        }
        let offset = 0x10 + (index * 4);
        let value = PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);
        if value == u32::MAX {
            return None;
        }
        let is_io = (value & 0x1) != 0;
        let is_64bit = !is_io && ((value & 0x6) == 0x4);
        let is_prefetchable = !is_io && ((value & 0x8) != 0);

        let mut address = if is_io {
            (value & 0xFFFFFFFC) as u64
        } else {
            (value & 0xFFFFFFF0) as u64
        };

        if is_64bit && index < 5 {
            let high_value =
                PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset + 4);
            address |= (high_value as u64) << 32;
        }

        Some(PciBar {
            index,
            address,
            size: 0,
            is_io,
            is_64bit,
            is_prefetchable,
        })
    }

    /// Read BAR metadata, destructively probing only its size field.
    pub fn get_bar_info(&self, index: u8) -> Option<PciBar> {
        let mut bar = self.read_bar_info(index)?;
        bar.size = self.detect_bar_size(index);
        (bar.size != 0).then_some(bar)
    }

    /// Ensure the PCI Power Management capability is set to D0.
    pub fn ensure_d0(&self) -> bool {
        let cap_ptr = PciConfigSpace::read_config_byte(self.bus, self.device, self.function, 0x34);
        if cap_ptr == 0 {
            return true;
        }
        let mut off = cap_ptr;
        let mut visited = [false; 256];
        loop {
            if !(0x40..=0xF8).contains(&off) {
                break;
            }
            if visited[off as usize] {
                log::warn!("PCI: capability list cycle detected at offset {:#x}", off);
                break;
            }
            visited[off as usize] = true;

            let cap_id =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off);
            if cap_id == 0x01 {
                let pmcsr =
                    PciConfigSpace::read_config_word(self.bus, self.device, self.function, off + 4);
                let pstate = pmcsr & 0x3;
                if pstate != 0 {
                    log::info!(
                        "PCI: device {:02x}:{:02x}.{} in D{} → requesting D0",
                        self.bus,
                        self.device,
                        self.function,
                        pstate
                    );
                    PciConfigSpace::write_config_word_raw(
                        self.bus,
                        self.device,
                        self.function,
                        off + 4,
                        pmcsr & !0x3,
                    );
                    crate::timing::delay_ms(10);
                }
                return PciConfigSpace::read_config_word(
                    self.bus,
                    self.device,
                    self.function,
                    off + 4,
                ) & 0x3 == 0;
            }
            let next =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off + 1);
            if next == 0 || next as usize == off as usize {
                break;
            }
            off = next;
        }
        true
    }

    /// Establish the PCI configuration prerequisites for MMIO and DMA.
    /// Power is restored before decoding is enabled, matching the PCI core
    /// ordering used by mature operating systems.
    pub fn prepare_mmio(&self) -> bool {
        if !self.ensure_d0() {
            return false;
        }
        self.disable_pcie_aspm();
        self.enable_memory_access()
    }

    /// Disable ASPM (Active State Power Management) on the PCIe link.
    ///
    /// Clears ASPM bits in the PCIe Link Control register using **port I/O only**
    /// (standard config space, offset < 0x100).  This is always safe — port I/O
    /// never hangs even when the endpoint is in L1.
    ///
    /// Does NOT touch L1 PM Substates (L1.1 / L1.2).  L1Sub lives in extended
    /// config space (offset ≥ 0x100) which requires ECAM MMIO — a non-posted
    /// ECAM read to an endpoint in L1 hangs the CPU forever.  Bridge-side ECAM
    /// access is permissible (bridges are never in L1 relative to the CPU), so
    /// L1Sub can be disabled on the upstream bridge via `disable_l1_substates()`.
    /// WiFi init now disables L1Sub on the bridge to prevent endpoint MMIO hangs.
    pub fn disable_pcie_aspm(&self) {
        let cap_ptr = PciConfigSpace::read_config_byte(self.bus, self.device, self.function, 0x34);
        if cap_ptr == 0 {
            return;
        }
        let mut off = cap_ptr;
        let mut visited = [false; 256];
        loop {
            if !(0x40..=0xFC).contains(&off) {
                break;
            }
            if visited[off as usize] {
                log::warn!("PCI: capability list cycle detected at offset {:#x}", off);
                break;
            }
            visited[off as usize] = true;
            let cap_id =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off);
            if cap_id == 0x10 {
                let lnk_ctrl = PciConfigSpace::read_config_word(
                    self.bus,
                    self.device,
                    self.function,
                    off + 0x10,
                );
                let aspm = lnk_ctrl & 0x3;
                if aspm != 0 {
                    log::info!(
                        "PCI: disabling ASPM on {:02x}:{:02x}.{} (was {})",
                        self.bus,
                        self.device,
                        self.function,
                        aspm
                    );
                    PciConfigSpace::write_config_word_raw(
                        self.bus,
                        self.device,
                        self.function,
                        off + 0x10,
                        lnk_ctrl & !0x3,
                    );
                }
                // L1Sub is NOT disabled on the endpoint here — that would
                // require ECAM MMIO which hangs if the link is in L1.
                // Bridge-side L1Sub can be disabled separately via
                // `disable_l1_substates()` during WiFi bridge init.
                return;
            }
            let next =
                PciConfigSpace::read_config_byte(self.bus, self.device, self.function, off + 1);
            if next == 0 || next as usize == off as usize {
                break;
            }
            off = next;
        }
    }

    /// Walk the PCIe Extended Capability list and disable L1 PM
    /// Substates (ASPM L1.1 / L1.2) on the given device.
    ///
    /// Extended capabilities start at offset 0x100 in config space.
    /// Each entry is 4 bytes: bits [15:0] = Capability ID,
    /// bits [19:16] = Version, bits [31:20] = Next Capability Offset.
    /// Offset 0 terminates the list.
    ///
    /// # ⚠️ Caller responsibility
    ///
    /// This function uses ECAM MMIO.  It is **only safe to call for
    /// upstream bridge devices**.  Calling it for an endpoint that may
    /// be in ASPM L1 will cause a permanent CPU hang because the endpoint
    /// cannot complete a non-posted ECAM read while in L1.
    ///
    /// L1Sub is negotiated between the bridge and endpoint — disabling
    /// it on the bridge alone is sufficient to prevent the link from
    /// entering L1.1/L1.2.  There is no need to call this on the endpoint.
    ///
    /// Note: The WiFi/RTSX/PciHealth recovery paths intentionally leave
    /// L1Sub enabled, as Linux tolerates ASPM L1 + L1Sub on the same
    /// chipset without hangs. This function is provided for explicit
    /// bridge-side L1Sub control when needed.
    ///
    /// Silently no-ops if ECAM has not been configured by the kernel.
    pub fn disable_l1_substates(bus: u8, device: u8, function: u8) {
        let mut off: u16 = 0x100;
        let mut iterations = 0;
        const MAX_ITERATIONS: u8 = 48;
        while off != 0 && iterations < MAX_ITERATIONS {
            iterations += 1;
            // Extended capabilities live at offsets ≥ 0x100 — must use ECAM.
            let cap_hdr = read_ext_dword(bus, device, function, off);
            if cap_hdr == 0xFFFF_FFFF {
                // ECAM not configured or device absent — skip
                return;
            }
            let cap_id = (cap_hdr & 0xFFFF) as u16;
            let next_off = ((cap_hdr >> 20) & 0xFFF) as u16;

            if cap_id == 0x001E {
                // L1 PM Substates Capability
                // L1SubCtl1 is at offset cap+0x08 (2 dwords in).
                let ctl1 = read_ext_dword(bus, device, function, off + 8);
                // Bits [2:1]: ASPM L1.2 Enable (bit 2), ASPM L1.1 Enable (bit 1)
                let l1sub_enabled = ctl1 & 0x6;
                if l1sub_enabled != 0 {
                    log::info!(
                        "PCI: disabling L1Sub on {:02x}:{:02x}.{} (was {:#x})",
                        bus, device, function, l1sub_enabled,
                    );
                    write_ext_dword(
                        bus, device, function, off + 8,
                        ctl1 & !0x6u32,
                    );
                }
                return;
            }

            off = next_off;
        }
    }

    pub fn detect_bar_size(&self, bar_index: u8) -> u32 {
        if bar_index >= self.max_bars() {
            return 0;
        }
        let offset = 0x10 + (bar_index * 4);
        pci_config_lock_acquire();
        let original_value = PciConfigSpace::read_config_dword_unlocked(
            self.bus, self.device, self.function, offset,
        );
        let command = PciConfigSpace::read_config_dword_unlocked(
            self.bus, self.device, self.function, 4,
        ) as u16;
        PciConfigSpace::write_config_word_unlocked(
            self.bus, self.device, self.function, 4, command & !0x3,
        );
        PciConfigSpace::write_config_dword_unlocked(
            self.bus, self.device, self.function, offset, u32::MAX,
        );
        let size_mask = PciConfigSpace::read_config_dword_unlocked(
            self.bus, self.device, self.function, offset,
        );
        PciConfigSpace::write_config_dword_unlocked(
            self.bus, self.device, self.function, offset, original_value,
        );
        PciConfigSpace::write_config_word_unlocked(
            self.bus, self.device, self.function, 4, command,
        );
        pci_config_lock_release();

        if size_mask == 0 || size_mask == 0xFFFFFFFF {
            return 0;
        }

        if (size_mask & 0x1) != 0 {
            !(size_mask & 0xFFFFFFFC) + 1
        } else {
            !(size_mask & 0xFFFFFFF0) + 1
        }
    }
}

struct PrivatePciDevice {
    bus: u8,
    device: u8,
    function: u8,
    config: PciConfigSpace,
}

impl PrivatePciDevice {
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        // CRITICAL: Do NOT call read_config_space() here.
        // On real hardware (InsydeH2O), reading all 16 config bytes
        // in sequence can cause master aborts on certain offsets,
        // hanging the CPU.  We only read the vendor/device ID to
        // confirm presence, and leave the rest to the caller.
        let vendor = PciConfigSpace::read_config_word(bus, device, function, 0);
        if vendor == 0xFFFF || vendor == 0x0000 {
            return None;
        }
        let device_id = PciConfigSpace::read_config_word(bus, device, function, 2);

        // Read the class code, subclass, prog_if, and revision_id in a single safe read
        let class_rev = PciConfigSpace::read_config_dword(bus, device, function, 8);

        // Read header type (offset 0x0E) — needed to distinguish endpoints
        // from bridges so BAR probing doesn't clobber bridge control registers.
        let header_type_raw = PciConfigSpace::read_config_byte(bus, device, function, 0x0E);

        // Build minimal config — other fields will be read on demand.
        let mut config = PciConfigSpace::new();
        config.vendor_id = vendor;
        config.device_id = device_id;
        config.revision_id = class_rev as u8;
        config.prog_if = (class_rev >> 8) as u8;
        config.subclass = (class_rev >> 16) as u8;
        config.class_code = (class_rev >> 24) as u8;
        config.header_type = header_type_raw;
        Some(Self {
            bus,
            device,
            function,
            config,
        })
    }

    pub fn into_public(self) -> PciDevice {
        PciDevice {
            bus: self.bus,
            device: self.device,
            function: self.function,
            handle: Self::build_handle(self.bus, self.device, self.function),
            vendor_id: self.config.vendor_id,
            device_id: self.config.device_id,
            class_code: self.config.class_code,
            subclass: self.config.subclass,
            prog_if: self.config.prog_if,
            header_type: self.config.header_type,
        }
    }

    pub fn read_bar(&self, bar_index: u8) -> Option<u64> {
        if bar_index > 5 {
            return None;
        }

        let offset = 0x10 + (bar_index * 4);
        let bar_low =
            PciConfigSpace::read_config_dword(self.bus, self.device, self.function, offset);

        if bar_low == 0 {
            return None;
        }

        if (bar_low & 0x1) != 0 {
            return None;
        }

        let is_64bit = (bar_low & 0x6) == 0x4;

        if is_64bit {
            if bar_index >= 5 {
                return None;
            }
            let high_offset = offset + 4;
            let bar_high = PciConfigSpace::read_config_dword(
                self.bus,
                self.device,
                self.function,
                high_offset,
            );
            Some(((bar_high as u64) << 32) | ((bar_low & 0xFFFFFFF0) as u64))
        } else {
            Some((bar_low & 0xFFFFFFF0) as u64)
        }
    }

    fn build_handle(bus: u8, device: u8, function: u8) -> usize {
        ((bus as usize) << 16) | ((device as usize) << 8) | (function as usize)
    }
}

#[derive(Default)]
pub struct PciScanner {
    devices: alloc::vec::Vec<PciDevice>,
}

impl PciScanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scan_all_buses(&mut self) -> Result<(), ()> {
        self.devices.clear();

        fn bus_exists(bus: u8) -> bool {
            let vendor = PciConfigSpace::read_config_word(bus, 0, 0, 0);
            if vendor == 0xFFFF || vendor == 0x0000 {
                return false;
            }
            true
        }

        fn device_exists(bus: u8, device: u8, function: u8) -> bool {
            let vendor = PciConfigSpace::read_config_word(bus, device, function, 0);
            vendor != 0xFFFF && vendor != 0x0000
        }

        let mut buses_to_scan: [bool; 256] = [false; 256];
        buses_to_scan[0] = true;

        crate::debug::print("pci", "scan_bus0_start");
        for device in 0..=31u8 {
            if !device_exists(0, device, 0) {
                continue;
            }
            crate::debug::print("pci", "b0_dev_found");
            for function in 0..=7u8 {
                if function > 0 {
                    let header_type_fn0 =
                        PciConfigSpace::read_config_byte(0, device, 0, 0x0E);
                    if (header_type_fn0 & 0x80) == 0 {
                        break;
                    }
                }
                if !device_exists(0, device, function) {
                    continue;
                }
                crate::debug::print("pci", "b0_push");
                if let Some(pci_device) = PciDevice::new(0, device, function) {
                    let cc = pci_device.class_code;
                    let sc = pci_device.subclass;
                    self.devices.push(pci_device);

                    if cc == 0x06 && sc == 0x04 {
                        let secondary_bus =
                            PciConfigSpace::read_config_byte(0, device, function, 0x19);
                        if secondary_bus > 0 && secondary_bus < 255 {
                            buses_to_scan[secondary_bus as usize] = true;
                        }
                    }
                }
            }
        }
        crate::debug::print("pci", "scan_bus0_done");

        // Second pass: scan discovered child buses
        for bus in 1..=255u8 {
            if !buses_to_scan[bus as usize] {
                continue;
            }
            if !bus_exists(bus) {
                buses_to_scan[bus as usize] = false;
                continue;
            }
            for device in 0..=31u8 {
                if !device_exists(bus, device, 0) {
                    continue;
                }
                for function in 0..=7u8 {
                    if function > 0 {
                        let header_type =
                            PciConfigSpace::read_config_byte(bus, device, 0, 0x0E);
                        if (header_type & 0x80) == 0 {
                            break;
                        }
                    }
                    if !device_exists(bus, device, function) {
                        continue;
                    }
                    if let Some(pci_device) = PciDevice::new(bus, device, function) {
                        let cc = pci_device.class_code;
                        let sc = pci_device.subclass;
                        self.devices.push(pci_device);

                        if cc == 0x06 && sc == 0x04 {
                            let secondary_bus =
                                PciConfigSpace::read_config_byte(bus, device, function, 0x19);
                            if secondary_bus > bus && !buses_to_scan[secondary_bus as usize] {
                                buses_to_scan[secondary_bus as usize] = true;
                            }
                        }
                    }
                }
            }
        }

        crate::debug::print("pci", "scan_done");
        Ok(())
    }

    pub fn get_devices(&self) -> &[PciDevice] {
        &self.devices
    }
}
