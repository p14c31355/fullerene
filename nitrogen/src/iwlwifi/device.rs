//! Core device definition, initialisation, DMA helpers, and firmware
//! loading for the Intel Wireless 7265 (iwlwifi 7000 series) driver.

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::collections::VecDeque;

use bonder::wifi::{self, AccessPoint};
use bonder::wpa::WpaSupplicant;
use bonder::dhcp::DhcpClient;

use crate::pci::{PciDevice, PciScanner};
use crate::pci_health::PciHealth;
use crate::mmio::{self, DmaRegion, SafeReadResult};
use crate::debug;
use crate::DriverContext;

use super::regs::*;
use super::types::*;

// ── IwlWifiDevice ─────────────────

/// Intel Wireless 7265 NIC driver.
pub struct IwlWifiDevice {
    /// MAC address from NVM/EEPROM.
    pub mac: [u8; 6],
    /// PCI config access.
    pub _pci_dev: PciDevice,
    /// MMIO BAR0.
    pub mmio: *mut u32,
    /// Hardware revision.
    pub hw_rev: u16,

    /// Driver context for DMA.
    pub ctx: &'static dyn DriverContext,
    /// PCIe health monitor for pre-MMIO access checks.
    pub health: PciHealth,

    /// Firmware state.
    pub fw_state: FwState,
    pub fw_build: u32,
    pub fw_api_ver: u32,

    /// 802.11 state.
    pub iwl_state: IwlState,
    pub wifi_conn: wifi::WifiConnection,
    pub wpa: WpaSupplicant,
    pub dhcp: Option<DhcpClient>,

    /// Scan results.
    pub scan_results: Vec<AccessPoint>,
    pub scan_channel: u8,
    pub scan_pending: bool,

    /// TX/RX queues.
    pub tx_queue: VecDeque<Vec<u8>>,
    pub rx_queue: VecDeque<Vec<u8>>,
    pub tx_dma_ring: DmaRegion,
    pub rx_dma_ring: DmaRegion,
    pub tx_head: usize,
    pub tx_tail: usize,
    pub rx_head: usize,
    pub rx_tail: usize,

    /// DMA buffers.
    pub tx_bufs: Vec<DmaRegion>,
    pub rx_bufs: Vec<DmaRegion>,

    /// IP configuration (from DHCP).
    pub ip_address: [u8; 4],
    pub subnet_mask: [u8; 4],
    pub gateway: [u8; 4],
    pub dns_server: [u8; 4],
}

unsafe impl Send for IwlWifiDevice {}

impl Drop for IwlWifiDevice {
    fn drop(&mut self) {
        for mut buf in self.tx_bufs.drain(..) {
            buf.free(self.ctx);
        }
        for mut buf in self.rx_bufs.drain(..) {
            buf.free(self.ctx);
        }
        self.tx_dma_ring.free(self.ctx);
        self.rx_dma_ring.free(self.ctx);
    }
}

impl IwlWifiDevice {
    // ── DMA helpers ──────────────────────────────────

    pub(super) fn tx_desc_mut(&mut self, idx: usize) -> &mut TxDmaDesc {
        unsafe { &mut *(self.tx_dma_ring.virt() as *mut TxDmaDesc).add(idx) }
    }

    #[allow(dead_code)]
    pub(super) fn tx_desc(&self, idx: usize) -> &TxDmaDesc {
        unsafe { &*(self.tx_dma_ring.virt() as *const TxDmaDesc).add(idx) }
    }

    pub(super) fn rx_desc_mut(&mut self, idx: usize) -> &mut RxDmaDesc {
        unsafe { &mut *(self.rx_dma_ring.virt() as *mut RxDmaDesc).add(idx) }
    }

    pub(super) fn rx_desc(&self, idx: usize) -> &RxDmaDesc {
        unsafe { &*(self.rx_dma_ring.virt() as *const RxDmaDesc).add(idx) }
    }

    // ── Safe MMIO access ────────────────────────────

    #[inline]
    pub(super) fn safe_read32(&self, reg: u32) -> Option<u32> {
        let addr = unsafe { self.mmio.add(reg as usize) } as *const u32;
        match mmio::checked_read_u32(addr, Some(&self.health)) {
            SafeReadResult::Value(v) => Some(v),
            _ => None,
        }
    }

    // ── Device initialisation ───────────────────────

    /// Scan the PCI bus for an Intel Wireless 7265 and initialize it.
    pub fn probe_and_init(ctx: &'static dyn DriverContext) -> Option<Self> {
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

            match Self::init(device.clone(), ctx) {
                Ok(s) => return Some(s),
                Err(_) => {
                    log::warn!("iwlwifi: init failed");
                    continue;
                }
            }
        }

        log::info!("iwlwifi: no device found");
        None
    }

    fn init(device: PciDevice, ctx: &'static dyn DriverContext) -> Result<Self, IwlError> {
        let mut health = PciHealth::new(&device);
        health.pre_mmio_access().map_err(|_| IwlError::BarNotAvailable)?;

        device.ensure_d0();
        device.disable_pcie_aspm();
        device.enable_memory_access();

        let bar0_addr = device.read_bar(0).ok_or(IwlError::BarNotAvailable)?;
        let mmio_virt = ctx.phys_to_virt(bar0_addr);

        let bar0_size = device
            .get_bar_info(0)
            .map(|info| info.size as usize)
            .unwrap_or(0x1000);
        log::info!(
            "iwlwifi: mapping BAR0 {:#x} -> virt {:#p} ({} bytes)",
            bar0_addr, mmio_virt as *mut u8, bar0_size
        );
        ctx.map_mmio_region(bar0_addr as usize, mmio_virt, bar0_size)
            .map_err(|_| {
                log::info!("iwlwifi: failed to map BAR0 MMIO");
                IwlError::BarNotAvailable
            })?;

        let mmio = mmio_virt as *mut u32;

        health.pre_mmio_access().map_err(|_| IwlError::BarNotAvailable)?;

        let hw_rev_raw = match mmio::checked_read_u32(
            unsafe { mmio.add(CSR_HW_REV as usize) } as *const u32,
            Some(&health),
        ) {
            mmio::SafeReadResult::Value(v) => v,
            _ => return Err(IwlError::BarNotAvailable),
        };
        let hw_rev = ((hw_rev_raw >> 4) & 0xFFFF) as u16;
        log::info!("iwlwifi: HW_REV={:#06x}", hw_rev);

        Self::reset_device(mmio);

        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_GP_CNTRL as usize), CSR_GP_CNTRL_MAC_ACCESS_REQ);
        }
        mmio::write_barrier();
        if !health.is_device_present() {
            return Err(IwlError::ClockNotReady);
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
        health.recover().map_err(|_| IwlError::ClockNotReady)?;

        let mac = Self::read_mac(mmio, Some(&health));

        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        let mut tx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<TxDmaDesc>() * TX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        let mut rx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); tx_dma_ring.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        let mut tx_bufs = Vec::new();
        let mut rx_bufs = Vec::new();
        let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;

        let init_result = (|| -> Result<(), IwlError> {
            for _ in 0..TX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                buf.dma_map(ctx, device.device_id).map_err(|_| IwlError::DmaAllocFailed)?;
                tx_bufs.push(buf);
            }
            for i in 0..RX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                let dma = buf
                    .dma_map(ctx, device.device_id)
                    .map_err(|_| IwlError::DmaAllocFailed)?;
                unsafe {
                    (*rx_virt.add(i)).addr_lo = dma as u32;
                    (*rx_virt.add(i)).addr_hi = (dma >> 32) as u32;
                    (*rx_virt.add(i)).len = MAX_FRAME_SIZE as u16;
                    mmio::cache_flush(rx_virt.add(i) as *const u8);
                }
                rx_bufs.push(buf);
            }
            Ok(())
        })();

        if let Err(e) = init_result {
            for mut buf in tx_bufs { buf.free(ctx); }
            for mut buf in rx_bufs { buf.free(ctx); }
            tx_dma_ring.free(ctx);
            rx_dma_ring.free(ctx);
            return Err(e);
        }

        let rx_phys = rx_dma_ring.dma_iova();

        unsafe {
            core::ptr::write_volatile(mmio.add(FH_TX_CHNL0_WPTR as usize), 0);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_BASE as usize), rx_phys as u32);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_RPTR_REG as usize), 0);
        }

        log::info!("iwlwifi: hardware initialized (firmware not loaded)");

        Ok(Self {
            mac,
            _pci_dev: device,
            mmio,
            hw_rev,
            ctx,
            health,
            fw_state: FwState::NotLoaded,
            fw_build: 0,
            fw_api_ver: IWL_FW_API_VER,
            iwl_state: IwlState::Init,
            wifi_conn: wifi::WifiConnection::new(),
            wpa: WpaSupplicant::new(),
            dhcp: None,
            scan_results: Vec::new(),
            scan_channel: 0,
            scan_pending: false,
            tx_queue: VecDeque::new(),
            rx_queue: VecDeque::new(),
            tx_dma_ring,
            rx_dma_ring,
            tx_head: 0,
            tx_tail: 0,
            rx_head: 0,
            rx_tail: 0,
            tx_bufs,
            rx_bufs,
            ip_address: [0u8; 4],
            subnet_mask: [0u8; 4],
            gateway: [0u8; 4],
            dns_server: [0u8; 4],
        })
    }

    /// Initialize the device from an already-mapped MMIO base.
    pub fn init_from_mmio(
        ctx: &'static dyn DriverContext,
        mmio: *mut u32,
        hw_rev: u32,
        device: PciDevice,
    ) -> Option<Self> {
        let health = PciHealth::new(&device);
        Self::init_after_mmio(ctx, mmio, hw_rev as u16, device, health).ok()
    }

    fn init_after_mmio(
        ctx: &'static dyn DriverContext,
        mmio: *mut u32,
        hw_rev: u16,
        device: PciDevice,
        mut health: PciHealth,
    ) -> Result<Self, IwlError> {
        debug::print("iwlwifi", "init_after_mmio: enter");
        if !health.is_device_present() {
            debug::print("iwlwifi", "ERR device_gone before reset");
            return Err(IwlError::BarNotAvailable);
        }

        debug::print("iwlwifi", "reset_device");
        Self::reset_device(mmio);

        debug::print("iwlwifi", "mac_clock_req");
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_GP_CNTRL as usize), CSR_GP_CNTRL_MAC_ACCESS_REQ);
        }
        mmio::write_barrier();
        if !health.is_device_present() {
            debug::print("iwlwifi", "ERR device_gone_before_clock");
            return Err(IwlError::ClockNotReady);
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
        health.recover().map_err(|_| {
            debug::print("iwlwifi", "ERR recover_before_read_mac");
            IwlError::ClockNotReady
        })?;

        debug::print("iwlwifi", "read_mac");
        let mac = Self::read_mac(mmio, Some(&health));

        debug::print("iwlwifi", "mask_ints");
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        debug::print("iwlwifi", "alloc_tx_ring");
        let mut tx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<TxDmaDesc>() * TX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        debug::print("iwlwifi", "alloc_rx_ring");
        let mut rx_dma_ring = DmaRegion::alloc(ctx, core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(ctx, device.device_id)
                    .map_err(|_| { r.free(ctx); tx_dma_ring.free(ctx); IwlError::DmaAllocFailed })
                    .map(|_| r)
            })?;
        let mut tx_bufs = Vec::new();
        let mut rx_bufs = Vec::new();
        let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;

        debug::print("iwlwifi", "alloc_tx_bufs");
        let init_result = (|| -> Result<(), IwlError> {
            for _ in 0..TX_QUEUE_SIZE {
                let mut buf = DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                buf.dma_map(ctx, device.device_id).map_err(|_| IwlError::DmaAllocFailed)?;
                tx_bufs.push(buf);
            }
            debug::print("iwlwifi", "alloc_rx_bufs");
            for i in 0..RX_QUEUE_SIZE {
                let mut buf = DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                let dma = buf.dma_map(ctx, device.device_id).map_err(|_| IwlError::DmaAllocFailed)?;
                unsafe {
                    (*rx_virt.add(i)).addr_lo = dma as u32;
                    (*rx_virt.add(i)).addr_hi = (dma >> 32) as u32;
                    (*rx_virt.add(i)).len = MAX_FRAME_SIZE as u16;
                    mmio::cache_flush(rx_virt.add(i) as *const u8);
                }
                rx_bufs.push(buf);
            }
            Ok(())
        })();

        if let Err(e) = init_result {
            debug::print("iwlwifi", "ERR init_result");
            for mut buf in tx_bufs { buf.free(ctx); }
            for mut buf in rx_bufs { buf.free(ctx); }
            tx_dma_ring.free(ctx);
            rx_dma_ring.free(ctx);
            return Err(e);
        }

        debug::print("iwlwifi", "program_fh");
        let rx_phys = rx_dma_ring.dma_iova();
        unsafe {
            core::ptr::write_volatile(mmio.add(FH_TX_CHNL0_WPTR as usize), 0);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_BASE as usize), rx_phys as u32);
            core::ptr::write_volatile(mmio.add(FH_RSCSR_CHNL0_RBDCB_RPTR_REG as usize), 0);
        }

        Ok(Self {
            mac,
            _pci_dev: device,
            mmio,
            hw_rev,
            ctx,
            health,
            fw_state: FwState::NotLoaded,
            fw_build: 0,
            fw_api_ver: IWL_FW_API_VER,
            iwl_state: IwlState::Init,
            wifi_conn: wifi::WifiConnection::new(),
            wpa: WpaSupplicant::new(),
            dhcp: None,
            scan_results: Vec::new(),
            scan_channel: 1,
            scan_pending: false,
            tx_queue: VecDeque::new(),
            rx_queue: VecDeque::new(),
            tx_dma_ring,
            rx_dma_ring,
            tx_head: 0,
            tx_tail: 0,
            rx_head: 0,
            rx_tail: 0,
            tx_bufs,
            rx_bufs,
            ip_address: [0u8; 4],
            subnet_mask: [0u8; 4],
            gateway: [0u8; 4],
            dns_server: [0u8; 4],
        })
    }

    /// Reset the device with posted-write + pure TSC delays.
    pub fn reset_device(mmio: *mut u32) {
        unsafe {
            core::ptr::write_volatile(
                mmio.add(CSR_RESET as usize),
                CSR_RESET_BIT_STOP_MASTER,
            );
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), CSR_RESET_BIT_SW);
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), 0);
        }
        {
            let start = unsafe { core::arch::x86_64::_rdtsc() };
            loop {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 10_000_000 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
    }

    /// Read MAC address from the NVM (non-volatile memory) via CSR registers.
    pub fn read_mac(mmio: *mut u32, health: Option<&PciHealth>) -> [u8; 6] {
        let checked_read = |reg: u32| -> Option<u32> {
            let addr = unsafe { mmio.add(reg as usize) } as *const u32;
            match mmio::checked_read_u32(addr, health) {
                SafeReadResult::Value(v) => Some(v),
                _ => None,
            }
        };

        let eeprom_gp = match checked_read(CSR_EEPROM_GP) {
            Some(v) => v,
            None => return [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
        };

        if (eeprom_gp & 0x08) != 0 {
            let otp_gp = match checked_read(CSR_OTP_GP) {
                Some(v) => v,
                None => return [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            };
            let mac_addr_shadow = if (otp_gp & 0x01) != 0 {
                0x0A0 / 4
            } else {
                0x0D4 / 4
            };

            if let (Some(mac_lo), Some(mac_hi)) = (checked_read(mac_addr_shadow), checked_read(mac_addr_shadow + 1)) {
                let mac = [
                    mac_lo as u8, (mac_lo >> 8) as u8,
                    (mac_lo >> 16) as u8, (mac_lo >> 24) as u8,
                    mac_hi as u8, (mac_hi >> 8) as u8,
                ];
                if mac != [0; 6] && mac != [0xFF; 6] {
                    return mac;
                }
            }
        }

        if let (Some(mac_lo), Some(mac_hi)) = (checked_read(0x0D4 / 4), checked_read(0x0D8 / 4)) {
            let fallback = [
                mac_lo as u8, (mac_lo >> 8) as u8,
                (mac_lo >> 16) as u8, (mac_lo >> 24) as u8,
                mac_hi as u8, (mac_hi >> 8) as u8,
            ];
            if fallback != [0; 6] && fallback != [0xFF; 6] {
                return fallback;
            }
        }

        [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
    }

    fn crc32(data: &[u8]) -> u32 {
        const POLY: u32 = 0xEDB88320;
        let mut crc = 0xFFFFFFFFu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if (crc & 1) != 0 {
                    (crc >> 1) ^ POLY
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    // ── Firmware loading ──────────────────────────

    /// Load firmware binary into the device.
    pub fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        debug::print("iwlwifi", "fw: check_header");
        if fw_data.len() < FW_HEADER_SIZE {
            return Err("Firmware data too short");
        }

        self.fw_state = FwState::Loading;

        let gp = self.safe_read32(CSR_GP_CNTRL).ok_or("Device unresponsive")?;
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp & !0x04,
            );
            core::ptr::write_volatile(
                self.mmio.add(CSR_RESET as usize),
                0x00000080,
            );
            for _ in 0..500 {
                core::hint::spin_loop();
            }
        }

        debug::print("iwlwifi", "fw: header_parse");
        let fw_ptr = fw_data.as_ptr();

        let zero: u32 = unsafe { core::ptr::read_unaligned(fw_ptr as *const u32) };
        if zero != 0 {
            return Err("Invalid firmware header (zero != 0)");
        }

        let magic: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(4) as *const u32) };
        if magic != IWL_FW_MAGIC {
            return Err("Invalid firmware magic");
        }

        log::info!("iwlwifi: loading firmware payload...");

        let mut desc_buf = [0u8; 64];
        unsafe {
            core::ptr::copy_nonoverlapping(fw_ptr.add(8), desc_buf.as_mut_ptr(), 64);
        }
        let build_str = core::ffi::CStr::from_bytes_until_nul(&desc_buf)
            .map(|c| c.to_str().unwrap_or("<invalid>"))
            .unwrap_or("<unknown>");
        log::info!("iwlwifi: firmware build: {}", build_str);

        self.fw_api_ver = unsafe { core::ptr::read_unaligned(fw_ptr.add(72) as *const u32) };
        self.fw_build = unsafe { core::ptr::read_unaligned(fw_ptr.add(76) as *const u32) };
        log::info!(
            "iwlwifi: firmware API v{}, build {}",
            self.fw_api_ver, self.fw_build
        );

        let mut off = FW_HEADER_SIZE;
        let mut section_count = 0u32;

        while off + 8 <= fw_data.len() {
            let tlv_type: u32 = unsafe {
                core::ptr::read_unaligned(fw_ptr.add(off) as *const u32)
            };
            let tlv_len: u32 = unsafe {
                core::ptr::read_unaligned(fw_ptr.add(off + 4) as *const u32)
            };
            let tlv_data_off = off + 8;
            let tlv_end = match tlv_data_off.checked_add(tlv_len as usize) {
                Some(end) => end,
                None => break,
            };

            if tlv_end > fw_data.len() {
                break;
            }

            match tlv_type {
                TLV_INST | TLV_DATA | TLV_INIT | TLV_INIT_DATA => {
                    if tlv_len < 4 {
                        off = tlv_end;
                        continue;
                    }
                    let target: u32 = unsafe {
                        core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                    };
                    let data_size = tlv_len - 4;
                    if data_size > 0 {
                        let section_data = &fw_data[tlv_data_off + 4..tlv_data_off + 4 + data_size as usize];
                        self.upload_section(target, section_data)?;
                        section_count += 1;
                        log::info!(
                            "iwlwifi: uploaded section {} at {:#010x} ({} bytes)",
                            section_count, target, data_size
                        );
                    }
                }
                _ => {}
            }
            off = tlv_end;
        }

        if section_count == 0 {
            return Err("No firmware sections uploaded");
        }

        debug::print("iwlwifi", "fw: upload_done");
        log::info!("iwlwifi: firmware upload complete, starting CPU...");

        let _pending = self.safe_read32(CSR_INT).unwrap_or(0);
        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), _pending);
        }

        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_RESET as usize), 0);
        }
        for _ in 0..200 {
            core::hint::spin_loop();
        }

        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_UCODE_GP1 as usize),
                0x00000001,
            );
        }

        let gp = self.safe_read32(CSR_GP_CNTRL).ok_or("Device unresponsive")?;
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | 0x04,
            );
        }

        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                !(1u32 << 0),
            );
        }

        debug::print("iwlwifi", "fw: wait_alive");
        let alive = self.wait_for_alive();
        if alive.is_err() {
            let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
            let csr_gp = self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0);
            let csr_ucode = self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0);
            let csr_reset = self.safe_read32(CSR_RESET).unwrap_or(!0);
            log::info!(
                "iwlwifi: CSR_INT={:#010x} CSR_GP={:#010x} UCODE_GP1={:#010x} RESET={:#010x}",
                csr_int, csr_gp, csr_ucode, csr_reset
            );
        }
        alive?;

        debug::print("iwlwifi", "fw: alive_ok");
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                0xFFFFFFFFu32,
            );
        }

        self.fw_state = FwState::Ready;
        debug::print("iwlwifi", "fw: ready");
        log::info!("iwlwifi: firmware alive and ready");

        debug::print("iwlwifi", "fw: init_cmds");
        self.send_init_commands()?;
        debug::print("iwlwifi", "fw: init_cmds_done");

        Ok(())
    }

    /// Upload a single firmware section to the device SRAM via HBUS direct writes.
    fn upload_section(&mut self, target_addr: u32, data: &[u8]) -> Result<(), &'static str> {
        let dwords = data.len() / 4;
        let extra = data.len() % 4;

        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(HBUS_TARG_MEM_WADDR as usize),
                target_addr,
            );

            for i in 0..dwords {
                let val = u32::from_le_bytes([
                    data[i * 4],
                    data[i * 4 + 1],
                    data[i * 4 + 2],
                    data[i * 4 + 3],
                ]);
                core::ptr::write_volatile(
                    self.mmio.add(HBUS_TARG_MEM_WDAT as usize),
                    val,
                );
            }

            if extra > 0 {
                let mut last = [0u8; 4];
                last[..extra].copy_from_slice(&data[dwords * 4..]);
                let val = u32::from_le_bytes(last);
                core::ptr::write_volatile(
                    self.mmio.add(HBUS_TARG_MEM_WDAT as usize),
                    val,
                );
            }
        }

        Ok(())
    }

    /// Wait for the firmware "alive" response with a TSC-based timeout.
    fn wait_for_alive(&mut self) -> Result<(), &'static str> {
        let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let timeout_tsc: u64 = 5_000_000_000;
        let mut last_pci_check: u64 = 0;
        let pci_check_interval: u64 = 100_000_000;

        loop {
            let now = unsafe { core::arch::x86_64::_rdtsc() };
            let elapsed = now.wrapping_sub(start_tsc);
            if elapsed >= timeout_tsc {
                break;
            }

            if now.wrapping_sub(last_pci_check) >= pci_check_interval {
                last_pci_check = now;
                if !self.health.is_device_present() {
                    return Err("Device disappeared from PCI bus during alive wait");
                }
            }

            let int_cause = match self.safe_read32(CSR_INT) {
                Some(v) => v,
                None => return Err("Device unresponsive (PCI master abort)"),
            };
            if int_cause != 0 {
                if (int_cause & 0x01) != 0 {
                    unsafe {
                        core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                    }
                    self.fw_state = FwState::Alive;
                    return Ok(());
                }
                if (int_cause & (1 << 25)) != 0 {
                    unsafe {
                        core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                    }
                    return Err("Firmware error");
                }
                unsafe {
                    core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                }
            }

            let ucode_gp1 = match self.safe_read32(CSR_UCODE_GP1) {
                Some(v) => v,
                None => return Err("Device unresponsive (PCI master abort)"),
            };
            if (ucode_gp1 & 0x01) == 0 {
                self.fw_state = FwState::Alive;
                return Ok(());
            }

            core::hint::spin_loop();
        }

        self.fw_state = FwState::Error;
        Err("Timeout waiting for firmware alive")
    }

    /// Start firmware upload and CPU boot without waiting for alive.
    pub fn start_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        self.health.recover().map_err(|_| "Device not accessible for firmware upload")?;

        debug::print("iwlwifi", "fw: check_header");
        if fw_data.len() < FW_HEADER_SIZE {
            return Err("Firmware data too short");
        }

        self.fw_state = FwState::Loading;

        let gp = self.safe_read32(CSR_GP_CNTRL).ok_or("Device unresponsive")?;
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp & !0x04,
            );
            core::ptr::write_volatile(
                self.mmio.add(CSR_RESET as usize),
                0x00000080,
            );
            for _ in 0..500 {
                core::hint::spin_loop();
            }
        }

        debug::print("iwlwifi", "fw: header_parse");
        let fw_ptr = fw_data.as_ptr();

        let zero: u32 = unsafe { core::ptr::read_unaligned(fw_ptr as *const u32) };
        if zero != 0 {
            return Err("Invalid firmware header (zero != 0)");
        }

        let magic: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(4) as *const u32) };
        if magic != IWL_FW_MAGIC {
            return Err("Invalid firmware magic");
        }

        log::info!("iwlwifi: loading firmware payload...");

        let mut desc_buf = [0u8; 64];
        unsafe {
            core::ptr::copy_nonoverlapping(fw_ptr.add(8), desc_buf.as_mut_ptr(), 64);
        }
        let build_str = core::ffi::CStr::from_bytes_until_nul(&desc_buf)
            .map(|c| c.to_str().unwrap_or("<invalid>"))
            .unwrap_or("<unknown>");
        log::info!("iwlwifi: firmware build: {}", build_str);

        self.fw_api_ver = unsafe { core::ptr::read_unaligned(fw_ptr.add(72) as *const u32) };
        self.fw_build = unsafe { core::ptr::read_unaligned(fw_ptr.add(76) as *const u32) };
        log::info!(
            "iwlwifi: firmware API v{}, build {}",
            self.fw_api_ver, self.fw_build
        );

        let mut off = FW_HEADER_SIZE;
        let mut section_count = 0;
        while off + 8 <= fw_data.len() {
            let tlv_type: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(off) as *const u32) };
            let tlv_len: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(off + 4) as *const u32) };
            let tlv_data_off = off + 8;
            let tlv_end = match tlv_data_off.checked_add(tlv_len as usize) {
                Some(end) => end,
                None => break,
            };
            if tlv_end > fw_data.len() {
                break;
            }
            match tlv_type {
                TLV_INST | TLV_DATA | TLV_INIT | TLV_INIT_DATA => {
                    if tlv_len < 4 {
                        off = tlv_end;
                        continue;
                    }
                    let target: u32 = unsafe {
                        core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                    };
                    let data_size = tlv_len - 4;
                    if data_size > 0 {
                        let section_data = &fw_data[tlv_data_off + 4..tlv_data_off + 4 + data_size as usize];
                        self.upload_section(target, section_data)?;
                        section_count += 1;
                        log::info!(
                            "iwlwifi: uploaded section {} at {:#010x} ({} bytes)",
                            section_count, target, data_size
                        );
                    }
                }
                _ => {}
            }
            off = tlv_end;
        }

        if section_count == 0 {
            return Err("No firmware sections uploaded");
        }

        debug::print("iwlwifi", "fw: upload_done");
        log::info!("iwlwifi: firmware upload complete, starting CPU...");

        self.health.recover().map_err(|_| {
            self.fw_state = FwState::Error;
            "Device not accessible after firmware upload"
        })?;

        let _pending = self.safe_read32(CSR_INT).unwrap_or(0);
        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), _pending);
            core::ptr::write_volatile(self.mmio.add(CSR_RESET as usize), 0);
        }
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_UCODE_GP1 as usize),
                0x00000001,
            );
        }
        let gp = self.safe_read32(CSR_GP_CNTRL).ok_or("Device unresponsive")?;
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | 0x04,
            );
        }
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                !(1u32 << 0),
            );
        }

        debug::print("iwlwifi", "fw: cpu_started");
        Ok(())
    }

    /// Check if firmware has signaled alive (non-blocking poll).
    pub fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, &'static str> {
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        let elapsed = now.wrapping_sub(start_tsc);
        const TIMEOUT_TSC: u64 = 5_000_000_000;

        if elapsed >= TIMEOUT_TSC {
            self.fw_state = FwState::Error;
            return Err("Timeout waiting for firmware alive");
        }

        if !self.health.is_device_present() {
            self.fw_state = FwState::Error;
            return Err("Device not accessible for MMIO");
        }

        let int_cause = match self.safe_read32(CSR_INT) {
            Some(v) => v,
            None => return Err("Device unresponsive (PCI master abort)"),
        };
        if (int_cause & (1 << 0)) != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                core::ptr::write_volatile(
                    self.mmio.add(CSR_INT_MASK as usize),
                    0xFFFFFFFFu32,
                );
            }
            self.fw_state = FwState::Alive;
            debug::print("iwlwifi", "fw: alive_ok");
            return Ok(true);
        }
        if (int_cause & (1 << 25)) != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }
            self.fw_state = FwState::Error;
            return Err("Firmware error");
        }
        if int_cause != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }
        }

        let ucode_gp1 = match self.safe_read32(CSR_UCODE_GP1) {
            Some(v) => v,
            None => return Err("Device unresponsive (PCI master abort)"),
        };
        if (ucode_gp1 & 0x01) == 0 {
            unsafe {
                core::ptr::write_volatile(
                    self.mmio.add(CSR_INT_MASK as usize),
                    0xFFFFFFFFu32,
                );
            }
            self.fw_state = FwState::Alive;
            debug::print("iwlwifi", "fw: alive_ok");
            return Ok(true);
        }

        Ok(false)
    }
}
