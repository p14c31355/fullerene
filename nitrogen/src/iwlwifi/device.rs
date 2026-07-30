//! Core device definition, initialisation, DMA helpers, and firmware
//! loading for the Intel Wireless 7265 (iwlwifi 7000 series) driver.

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use bonder::dhcp::DhcpClient;
use bonder::wifi::{self, AccessPoint, Ssid};
use bonder::wpa::WpaSupplicant;
use bonder::{NetDevice, NetError};

use crate::DriverContext;
use crate::debug;
use crate::mmio::{self, DmaRegion, SafeReadResult};
use crate::pci::{PciDevice, PciScanner};
use crate::pci_health::PciHealth;

use super::registers::*;
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
    /// True while the association requires WPA2-PSK protection.
    pub wpa_required: bool,
    /// Set only after the TX ring has reported both CCMP commands consumed.
    /// Until then, WPA data traffic is rejected fail-closed.
    pub wpa_keys_installed: bool,
    /// End position of the queued pair/group key commands, awaiting TX-ring
    /// consumption.  A command response path is not available in this
    /// firmware interface, so the data path stays blocked until the ring has
    /// consumed the commands at minimum.
    pub wpa_key_command_end: Option<usize>,
    /// EAPOL Message 4 is held until the key commands have been consumed.
    pub pending_wpa_message4: Option<Vec<u8>>,
    pub dhcp: Option<DhcpClient>,

    /// Scan results.
    pub scan_results: Vec<AccessPoint>,
    /// Tick-count scan watchdog.  Firmware scan dwell is expressed in TUs,
    /// so a full-channel scan can take seconds; the watchdog bounds a wedged
    /// firmware while allowing enough time for late-arriving beacons.
    pub scan_channel: u32,
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

    pub(super) fn rx_status(&self) -> &RxDmaStatus {
        unsafe {
            &*((self.rx_dma_ring.virt() + core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE)
                as *const RxDmaStatus)
        }
    }

    pub(super) fn init_rx_dma(&mut self) {
        let rx_phys = self.rx_dma_ring.dma_iova();
        let status_phys = rx_phys + (core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE) as u64;

        unsafe {
            // Match the legacy gen1_2 RX init sequence: stop DMA, reset both
            // hardware pointers, register the RBD/status buffers, then enable
            // channel 0 for 256 4K receive buffers.
            core::ptr::write_volatile(self.mmio.add(FH_MEM_RCSR_CHNL0_CONFIG_REG as usize), 0);
            core::ptr::write_volatile(self.mmio.add(FH_MEM_RCSR_CHNL0_RBDCB_WPTR as usize), 0);
            core::ptr::write_volatile(self.mmio.add(FH_MEM_RCSR_CHNL0_FLUSH_RB_REQ as usize), 0);
            core::ptr::write_volatile(self.mmio.add(FH_RSCSR_CHNL0_RDPTR_REG as usize), 0);
            core::ptr::write_volatile(self.mmio.add(FH_RSCSR_CHNL0_RBDCB_WPTR_REG as usize), 0);
            core::ptr::write_volatile(
                self.mmio.add(FH_RSCSR_CHNL0_RBDCB_BASE_REG as usize),
                (rx_phys >> 8) as u32,
            );
            core::ptr::write_volatile(
                self.mmio.add(FH_RSCSR_CHNL0_STTS_WPTR_REG as usize),
                (status_phys >> 4) as u32,
            );
            core::ptr::write_volatile(
                self.mmio.add(FH_RSCSR_CHNL0_RBDCB_WPTR_REG as usize),
                // All 256 RBDs have already been populated. On this
                // generation a fully posted ring wraps the write pointer to
                // zero; Linux uses the same value after restocking.
                0,
            );
            mmio::write_barrier();
            core::ptr::write_volatile(
                self.mmio.add(FH_MEM_RCSR_CHNL0_CONFIG_REG as usize),
                FH_RCSR_RX_CONFIG_CHNL_EN_ENABLE_VAL
                    | FH_RCSR_CHNL0_RX_IGNORE_RXF_EMPTY
                    | FH_RCSR_CHNL0_RX_CONFIG_IRQ_DEST_INT_HOST_VAL
                    | (FH_RCSR_RX_RB_TIMEOUT << FH_RCSR_RX_CONFIG_REG_IRQ_RBTH_POS)
                    | (8 << FH_RCSR_RX_CONFIG_RBDCB_SIZE_POS),
            );
        }
        mmio::write_barrier();
        let rx_config = self.safe_read32(FH_MEM_RCSR_CHNL0_CONFIG_REG).unwrap_or(!0);
        let rx_rbd_base = self
            .safe_read32(FH_RSCSR_CHNL0_RBDCB_BASE_REG)
            .unwrap_or(!0);
        let rx_status_ptr = self.safe_read32(FH_RSCSR_CHNL0_STTS_WPTR_REG).unwrap_or(!0);
        let rx_wptr = self
            .safe_read32(FH_RSCSR_CHNL0_RBDCB_WPTR_REG)
            .unwrap_or(!0);
        let int_mask = self.safe_read32(CSR_INT_MASK).unwrap_or(!0);
        log::info!(
            "iwlwifi: legacy RX DMA enabled: rbd={:#018x} status={:#018x} cfg={:#010x} rbd_base={:#010x} status_ptr={:#010x} wptr={:#010x} int_mask={:#010x}",
            rx_phys,
            status_phys,
            rx_config,
            rx_rbd_base,
            rx_status_ptr,
            rx_wptr,
            int_mask,
        );
    }

    // ── Safe MMIO access ────────────────────────────

    #[inline]
    pub(super) fn safe_read32(&self, reg: u32) -> Option<u32> {
        let addr = unsafe { self.mmio.add(reg as usize) } as *const u32;
        match unsafe { mmio::checked_read_u32(addr as usize, Some(&self.health)) } {
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
        health
            .pre_mmio_access()
            .map_err(|_| IwlError::BarNotAvailable)?;

        if !device.prepare_mmio() {
            return Err(IwlError::BarNotAvailable);
        }

        let bar0_addr = device.read_bar(0).ok_or(IwlError::BarNotAvailable)?;
        let mmio_virt = ctx.phys_to_virt(bar0_addr);

        // BAR0 is firmware-assigned. Avoid a destructive all-ones BAR size
        // probe on a live Wi-Fi endpoint; the CSR/FH register window used by
        // firmware boot fits in the first two pages.
        let bar0_size = 0x2000;
        log::info!(
            "iwlwifi: mapping BAR0 {:#x} -> virt {:#p} ({} bytes)",
            bar0_addr,
            mmio_virt as *mut u8,
            bar0_size
        );
        ctx.map_mmio_region(bar0_addr as usize, mmio_virt, bar0_size)
            .map_err(|_| {
                log::info!("iwlwifi: failed to map BAR0 MMIO");
                IwlError::BarNotAvailable
            })?;

        let mmio = mmio_virt as *mut u32;

        health
            .pre_mmio_access()
            .map_err(|_| IwlError::BarNotAvailable)?;

        let hw_rev_raw = match unsafe {
            mmio::checked_read_u32(mmio.add(CSR_HW_REV as usize) as usize, Some(&health))
        } {
            mmio::SafeReadResult::Value(v) => v,
            _ => return Err(IwlError::BarNotAvailable),
        };
        let hw_rev = ((hw_rev_raw >> 4) & 0xFFFF) as u16;
        log::info!("iwlwifi: HW_REV={:#06x}", hw_rev);

        Self::reset_device(mmio);

        unsafe {
            core::ptr::write_volatile(
                mmio.add(CSR_GP_CNTRL as usize),
                CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
            );
        }
        mmio::write_barrier();
        if !health.is_device_present() {
            return Err(IwlError::ClockNotReady);
        }
        crate::timing::delay_us(10_000);
        health.recover().map_err(|_| IwlError::ClockNotReady)?;

        let mac = Self::read_mac(mmio, Some(&health));

        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        let mut tx_dma_ring = DmaRegion::alloc(ctx, TX_DMA_ALLOCATION_BYTES)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(
                    ctx,
                    pci_dma_device_id(device.bus, device.device, device.function),
                )
                .map_err(|_| {
                    r.free(ctx);
                    IwlError::DmaAllocFailed
                })
                .map(|_| r)
            })?;
        let mut rx_dma_ring = match DmaRegion::alloc(
            ctx,
            core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE + core::mem::size_of::<RxDmaStatus>(),
        ) {
            Some(r) => r,
            None => {
                tx_dma_ring.free(ctx);
                return Err(IwlError::DmaAllocFailed);
            }
        };
        if rx_dma_ring
            .dma_map(
                ctx,
                pci_dma_device_id(device.bus, device.device, device.function),
            )
            .is_err()
        {
            rx_dma_ring.free(ctx);
            tx_dma_ring.free(ctx);
            return Err(IwlError::DmaAllocFailed);
        }
        let mut tx_bufs = Vec::new();
        let mut rx_bufs = Vec::new();
        let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;

        let init_result = (|| -> Result<(), IwlError> {
            for _ in 0..TX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                if buf
                    .dma_map(
                        ctx,
                        pci_dma_device_id(device.bus, device.device, device.function),
                    )
                    .is_err()
                {
                    buf.free(ctx);
                    return Err(IwlError::DmaAllocFailed);
                }
                tx_bufs.push(buf);
            }
            for i in 0..RX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, RX_BUFFER_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                let dma = match buf.dma_map(
                    ctx,
                    pci_dma_device_id(device.bus, device.device, device.function),
                ) {
                    Ok(d) => d,
                    Err(_) => {
                        buf.free(ctx);
                        return Err(IwlError::DmaAllocFailed);
                    }
                };
                unsafe {
                    (*rx_virt.add(i)).addr = (dma >> 8) as u32;
                    mmio::cache_flush(rx_virt.add(i) as usize);
                }
                rx_bufs.push(buf);
            }
            Ok(())
        })();

        if let Err(e) = init_result {
            for mut buf in tx_bufs {
                buf.free(ctx);
            }
            for mut buf in rx_bufs {
                buf.free(ctx);
            }
            tx_dma_ring.free(ctx);
            rx_dma_ring.free(ctx);
            return Err(e);
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
            wpa_required: false,
            wpa_keys_installed: false,
            wpa_key_command_end: None,
            pending_wpa_message4: None,
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
        _hw_rev: u16,
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
            core::ptr::write_volatile(
                mmio.add(CSR_GP_CNTRL as usize),
                CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
            );
        }
        mmio::write_barrier();
        if !health.is_device_present() {
            debug::print("iwlwifi", "ERR device_gone_before_clock");
            return Err(IwlError::ClockNotReady);
        }
        crate::timing::delay_us(10_000);
        health.recover().map_err(|_| {
            debug::print("iwlwifi", "ERR recover_before_read_mac");
            IwlError::ClockNotReady
        })?;

        let hw_rev_raw = match unsafe {
            mmio::checked_read_u32(mmio.add(CSR_HW_REV as usize) as usize, Some(&health))
        } {
            SafeReadResult::Value(v) => v,
            _ => return Err(IwlError::ClockNotReady),
        };
        let hw_rev = ((hw_rev_raw >> 4) & 0xFFFF) as u16;
        log::info!(
            "iwlwifi: CSR HW_REV type={:#06x}",
            hw_rev & CSR_HW_REV_TYPE_MASK
        );

        debug::print("iwlwifi", "read_mac");
        let mac = Self::read_mac(mmio, Some(&health));

        debug::print("iwlwifi", "mask_ints");
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_INT_MASK as usize), 0xFFFFFFFFu32);
        }

        debug::print("iwlwifi", "alloc_tx_ring");
        let mut tx_dma_ring = DmaRegion::alloc(ctx, TX_DMA_ALLOCATION_BYTES)
            .ok_or(IwlError::DmaAllocFailed)
            .and_then(|mut r| {
                r.dma_map(
                    ctx,
                    pci_dma_device_id(device.bus, device.device, device.function),
                )
                .map_err(|_| {
                    r.free(ctx);
                    IwlError::DmaAllocFailed
                })
                .map(|_| r)
            })?;
        debug::print("iwlwifi", "alloc_rx_ring");
        let mut rx_dma_ring = match DmaRegion::alloc(
            ctx,
            core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE + core::mem::size_of::<RxDmaStatus>(),
        ) {
            Some(r) => r,
            None => {
                tx_dma_ring.free(ctx);
                return Err(IwlError::DmaAllocFailed);
            }
        };
        if rx_dma_ring
            .dma_map(
                ctx,
                pci_dma_device_id(device.bus, device.device, device.function),
            )
            .is_err()
        {
            rx_dma_ring.free(ctx);
            tx_dma_ring.free(ctx);
            return Err(IwlError::DmaAllocFailed);
        }
        let mut tx_bufs = Vec::new();
        let mut rx_bufs = Vec::new();
        let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;

        debug::print("iwlwifi", "alloc_tx_bufs");
        let init_result = (|| -> Result<(), IwlError> {
            for _ in 0..TX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, MAX_FRAME_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                if buf
                    .dma_map(
                        ctx,
                        pci_dma_device_id(device.bus, device.device, device.function),
                    )
                    .is_err()
                {
                    buf.free(ctx);
                    return Err(IwlError::DmaAllocFailed);
                }
                tx_bufs.push(buf);
            }
            debug::print("iwlwifi", "alloc_rx_bufs");
            for i in 0..RX_QUEUE_SIZE {
                let mut buf =
                    DmaRegion::alloc(ctx, RX_BUFFER_SIZE).ok_or(IwlError::DmaAllocFailed)?;
                let dma = match buf.dma_map(
                    ctx,
                    pci_dma_device_id(device.bus, device.device, device.function),
                ) {
                    Ok(d) => d,
                    Err(_) => {
                        buf.free(ctx);
                        return Err(IwlError::DmaAllocFailed);
                    }
                };
                unsafe {
                    (*rx_virt.add(i)).addr = (dma >> 8) as u32;
                    mmio::cache_flush(rx_virt.add(i) as usize);
                }
                rx_bufs.push(buf);
            }
            Ok(())
        })();

        if let Err(e) = init_result {
            debug::print("iwlwifi", "ERR init_result");
            for mut buf in tx_bufs {
                buf.free(ctx);
            }
            for mut buf in rx_bufs {
                buf.free(ctx);
            }
            tx_dma_ring.free(ctx);
            rx_dma_ring.free(ctx);
            return Err(e);
        }

        debug::print("iwlwifi", "rx_dma_deferred_until_alive");

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
            wpa_required: false,
            wpa_keys_installed: false,
            wpa_key_command_end: None,
            pending_wpa_message4: None,
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

    /// Reset the device with posted-write + pure TSC delays.
    pub(super) fn reset_device(mmio: *mut u32) {
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), CSR_RESET_BIT_STOP_MASTER);
        }
        crate::timing::delay_us(10_000);
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), CSR_RESET_BIT_SW);
        }
        crate::timing::delay_us(10_000);
        unsafe {
            core::ptr::write_volatile(mmio.add(CSR_RESET as usize), 0);
        }
        crate::timing::delay_us(10_000);
    }

    /// Read MAC address from the NVM (non-volatile memory) via CSR registers.
    pub(super) fn read_mac(mmio: *mut u32, health: Option<&PciHealth>) -> [u8; 6] {
        let checked_read = |reg: u32| -> Option<u32> {
            let addr = unsafe { mmio.add(reg as usize) } as *const u32;
            match unsafe { mmio::checked_read_u32(addr as usize, health) } {
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

            if let (Some(mac_lo), Some(mac_hi)) = (
                checked_read(mac_addr_shadow),
                checked_read(mac_addr_shadow + 1),
            ) {
                let mac = [
                    mac_lo as u8,
                    (mac_lo >> 8) as u8,
                    (mac_lo >> 16) as u8,
                    (mac_lo >> 24) as u8,
                    mac_hi as u8,
                    (mac_hi >> 8) as u8,
                ];
                if mac != [0; 6] && mac != [0xFF; 6] {
                    return mac;
                }
            }
        }

        if let (Some(mac_lo), Some(mac_hi)) = (checked_read(0x0D4 / 4), checked_read(0x0D8 / 4)) {
            let fallback = [
                mac_lo as u8,
                (mac_lo >> 8) as u8,
                (mac_lo >> 16) as u8,
                (mac_lo >> 24) as u8,
                mac_hi as u8,
                (mac_hi >> 8) as u8,
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

    /// Common firmware upload and CPU start sequence shared by load_firmware and start_firmware.
    /// Does NOT wait for alive signal - caller must handle that if needed.
    fn upload_firmware_and_start_cpu(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        debug::print("iwlwifi", "fw: check_header");
        if fw_data.len() < FW_HEADER_SIZE {
            return Err(crate::DriverError::InvalidArgument);
        }

        self.fw_state = FwState::Loading;

        // Clear stale interrupts and the host-side RF-kill/CMD_BLOCKED
        // handshake before loading a new runtime image.  GP1 is a mailbox:
        // bit 0 is a firmware-owned MAC_SLEEP status bit, so it must not be
        // written directly by the host.
        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), 0xFFFF_FFFF);
            core::ptr::write_volatile(self.mmio.add(CSR_FH_INT as usize), 0xFFFF_FFFF);
            core::ptr::write_volatile(
                self.mmio.add(CSR_UCODE_GP1_CLR as usize),
                CSR_UCODE_SW_BIT_RFKILL | CSR_UCODE_GP1_BIT_CMD_BLOCKED,
            );
            core::ptr::write_volatile(self.mmio.add(CSR_INT_MASK as usize), 0);
        }

        let gp = self
            .safe_read32(CSR_GP_CNTRL)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_GP_CNTRL as usize), gp & !0x04);
            core::ptr::write_volatile(self.mmio.add(CSR_RESET as usize), 0x00000080);
        }
        crate::timing::delay_us(10_000);

        // The FH service channel is fed by the legacy 7000-series DMA clock.
        // Linux enables this clock and disables the L1-Active transition in
        // its APM init before submitting the first firmware chunk. Without
        // this setup the FH registers accept the descriptor but never raise
        // the firmware-load interrupt.
        self.prepare_firmware_dma()?;

        debug::print("iwlwifi", "fw: header_parse");
        let fw_ptr = fw_data.as_ptr();

        let zero: u32 = unsafe { core::ptr::read_unaligned(fw_ptr as *const u32) };
        if zero != 0 {
            return Err(crate::DriverError::Protocol);
        }

        let magic: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(4) as *const u32) };
        if magic != IWL_FW_MAGIC {
            return Err(crate::DriverError::Protocol);
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
            self.fw_api_ver,
            self.fw_build
        );

        let mut off = FW_HEADER_SIZE;
        let mut section_count = 0u32;
        // Linux encodes the sections loaded into CPU1 as a growing mask in
        // FH_UCODE_LOAD_STATUS (1, 3, 7, ...), then writes 0xffff when the
        // CPU1 image is complete. The 7265 firmware checks this mailbox
        // before it emits the alive interrupt.
        let mut section_status = 1u32;

        while off + 8 <= fw_data.len() {
            let tlv_type: u32 = unsafe { core::ptr::read_unaligned(fw_ptr.add(off) as *const u32) };
            let tlv_len: u32 =
                unsafe { core::ptr::read_unaligned(fw_ptr.add(off + 4) as *const u32) };
            let tlv_data_off = off + 8;
            let tlv_end = match tlv_data_off.checked_add(tlv_len as usize) {
                Some(end) => end,
                None => break,
            };

            if tlv_end > fw_data.len() {
                break;
            }

            match tlv_type {
                // A regular boot uses only SEC_RT.  SEC_INIT, SEC_WOWLAN,
                // and DEF_CALIB belong to other firmware images/metadata;
                // writing them into runtime SRAM prevents the CPU from
                // reaching the alive notification.
                TLV_SEC_RT => {
                    if tlv_len < 4 {
                        off = tlv_end;
                        continue;
                    }
                    let target: u32 = unsafe {
                        core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                    };
                    if target == FW_CPU1_CPU2_SEPARATOR_SECTION
                        || target == FW_PAGING_SEPARATOR_SECTION
                    {
                        off = tlv_end;
                        continue;
                    }
                    let data_size = tlv_len - 4;
                    if data_size > 0 {
                        let section_data =
                            &fw_data[tlv_data_off + 4..tlv_data_off + 4 + data_size as usize];
                        self.upload_section(target, section_data)?;
                        section_count += 1;
                        unsafe {
                            core::ptr::write_volatile(
                                self.mmio.add(FH_UCODE_LOAD_STATUS as usize),
                                section_status,
                            );
                        }
                        mmio::write_barrier();
                        section_status = (section_status << 1) | 1;
                        log::info!(
                            "iwlwifi: uploaded section {} at {:#010x} ({} bytes)",
                            section_count,
                            target,
                            data_size
                        );
                    }
                }
                _ => {}
            }
            off = tlv_end.saturating_add(3) & !3;
        }

        if section_count == 0 {
            return Err(crate::DriverError::Protocol);
        }

        unsafe {
            core::ptr::write_volatile(self.mmio.add(FH_UCODE_LOAD_STATUS as usize), 0xFFFF);
        }
        mmio::write_barrier();
        log::info!(
            "iwlwifi: firmware sections ready: FH_UCODE_LOAD_STATUS={:#010x}",
            0xFFFF_u32
        );

        debug::print("iwlwifi", "fw: upload_done");
        log::info!("iwlwifi: firmware upload complete, starting CPU...");

        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), 0xFFFF_FFFF);
            // Arm the alive interrupt before releasing reset. The firmware
            // can signal alive immediately after the CPU starts.
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                CSR_INT_BIT_ALIVE | CSR_INT_BIT_FH_RX,
            );
            core::ptr::write_volatile(self.mmio.add(CSR_RESET as usize), 0);
        }
        crate::timing::delay_us(10_000);

        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_UCODE_GP1_CLR as usize),
                CSR_UCODE_SW_BIT_RFKILL | CSR_UCODE_GP1_BIT_CMD_BLOCKED,
            );
        }

        let gp = self
            .safe_read32(CSR_GP_CNTRL)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
            );
        }

        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_INT_MASK as usize),
                CSR_INT_BIT_ALIVE | CSR_INT_BIT_FH_RX,
            );
        }

        Ok(())
    }

    /// Load firmware binary into the device.
    pub(super) fn load_firmware_inner(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        self.upload_firmware_and_start_cpu(fw_data)?;

        debug::print("iwlwifi", "fw: wait_alive");
        let alive = self.wait_for_alive();
        if alive.is_err() {
            let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
            let csr_gp = self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0);
            let csr_ucode = self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0);
            let csr_reset = self.safe_read32(CSR_RESET).unwrap_or(!0);
            let fh_load = self.safe_read32(FH_UCODE_LOAD_STATUS).unwrap_or(!0);
            log::info!(
                "iwlwifi: CSR_INT={:#010x} CSR_GP={:#010x} UCODE_GP1={:#010x} RESET={:#010x} FH_LOAD={:#010x}",
                csr_int,
                csr_gp,
                csr_ucode,
                csr_reset,
                fh_load
            );
        }
        alive?;

        debug::print("iwlwifi", "fw: alive_ok");
        unsafe {
            core::ptr::write_volatile(self.mmio.add(CSR_INT_MASK as usize), CSR_INI_SET_MASK);
        }

        self.fw_state = FwState::Ready;
        debug::print("iwlwifi", "fw: ready");
        log::info!("iwlwifi: firmware alive and ready");

        debug::print("iwlwifi", "fw: init_cmds");
        self.send_init_commands()?;
        debug::print("iwlwifi", "fw: init_cmds_done");

        Ok(())
    }

    /// Upload a firmware section through the legacy FH service DMA channel.
    ///
    /// The 7265 firmware expects every DMA chunk to complete before the next
    /// chunk is submitted. A direct HBUS write can look successful from the
    /// host side while leaving the CPU boot image incomplete, which results in
    /// an alive timeout after the reset is released.
    fn upload_section(&mut self, target_addr: u32, data: &[u8]) -> Result<(), crate::DriverError> {
        let mut dma = DmaRegion::alloc(self.ctx, FH_MEM_TB_MAX_LENGTH)
            .ok_or(crate::DriverError::DmaMappingFailed)?;
        let dma_device_id = pci_dma_device_id(
            self._pci_dev.bus,
            self._pci_dev.device,
            self._pci_dev.function,
        );
        if dma.dma_map(self.ctx, dma_device_id).is_err() {
            dma.free(self.ctx);
            return Err(crate::DriverError::DmaMappingFailed);
        }

        let result = (|| {
            let mut offset = 0usize;
            while offset < data.len() {
                let count = core::cmp::min(FH_MEM_TB_MAX_LENGTH, data.len() - offset);
                log::info!(
                    "iwlwifi: FH firmware DMA target={:#010x} bytes={}",
                    target_addr.saturating_add(offset as u32),
                    count
                );
                dma.write_from(&data[offset..offset + count]);
                self.upload_firmware_chunk(
                    target_addr.saturating_add(offset as u32),
                    dma.dma_iova(),
                    count,
                )?;
                offset += count;
            }
            Ok(())
        })();
        dma.free(self.ctx);
        result
    }

    /// Program and synchronously drain one FH service-DMA transfer.
    fn upload_firmware_chunk(
        &mut self,
        target_addr: u32,
        dma_addr: u64,
        byte_count: usize,
    ) -> Result<(), crate::DriverError> {
        if byte_count == 0 || byte_count > FH_MEM_TB_MAX_LENGTH {
            return Err(crate::DriverError::InvalidArgument);
        }

        unsafe {
            core::ptr::write_volatile(self.mmio.add(FH_TCSR_CHNL_TX_CONFIG_SRVC as usize), 0);
            core::ptr::write_volatile(self.mmio.add(FH_SRVC_CHNL_SRAM_ADDR as usize), target_addr);
            core::ptr::write_volatile(self.mmio.add(FH_TFDIB_CTRL0_SRVC as usize), dma_addr as u32);
            core::ptr::write_volatile(
                self.mmio.add(FH_TFDIB_CTRL1_SRVC as usize),
                (((dma_addr >> 32) as u32) & 0xF) << FH_MEM_TFDIB_REG1_ADDR_BITSHIFT
                    | byte_count as u32,
            );
            core::ptr::write_volatile(
                self.mmio.add(FH_TCSR_CHNL_TX_BUF_STS_SRVC as usize),
                FH_TCSR_TX_BUF_STS_TB_NUM
                    | FH_TCSR_TX_BUF_STS_TB_IDX
                    | FH_TCSR_TX_BUF_STS_TFDB_VALID,
            );
            core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), CSR_INT_BIT_FH_TX);
            core::ptr::write_volatile(self.mmio.add(CSR_FH_INT as usize), CSR_FH_INT_BIT_TX_CHNL0);
            core::ptr::write_volatile(self.mmio.add(CSR_INT_MASK as usize), CSR_INT_BIT_FH_TX);
            core::ptr::write_volatile(
                self.mmio.add(FH_TCSR_CHNL_TX_CONFIG_SRVC as usize),
                FH_TCSR_TX_CONFIG_DMA_ENABLE | FH_TCSR_TX_CONFIG_CIRQ_HOST_ENDTFD,
            );
        }
        mmio::write_barrier();

        let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let timeout_tsc = crate::timing::ticks_per_us().saturating_mul(5_000_000);
        loop {
            let now = unsafe { core::arch::x86_64::_rdtsc() };
            if now.wrapping_sub(start_tsc) >= timeout_tsc {
                let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
                let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
                let tx_cfg = self.safe_read32(FH_TCSR_CHNL_TX_CONFIG_SRVC).unwrap_or(!0);
                let tx_status = self.safe_read32(FH_TCSR_CHNL_TX_BUF_STS_SRVC).unwrap_or(!0);
                log::warn!(
                    "iwlwifi: FH firmware DMA timeout: CSR_INT={:#010x} FH_INT={:#010x} TX_CFG={:#010x} TX_STS={:#010x} dma={:#018x} bytes={}",
                    csr_int,
                    fh_int,
                    tx_cfg,
                    tx_status,
                    dma_addr,
                    byte_count,
                );
                unsafe {
                    core::ptr::write_volatile(
                        self.mmio.add(FH_TCSR_CHNL_TX_CONFIG_SRVC as usize),
                        0,
                    );
                }
                return Err(crate::DriverError::TimedOut);
            }

            let int_cause = self
                .safe_read32(CSR_INT)
                .ok_or(crate::DriverError::DeviceNotFound)?;
            if (int_cause & CSR_INT_BIT_FH_TX) != 0 {
                let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(0);
                unsafe {
                    core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                    core::ptr::write_volatile(self.mmio.add(CSR_FH_INT as usize), fh_int);
                    core::ptr::write_volatile(
                        self.mmio.add(FH_TCSR_CHNL_TX_CONFIG_SRVC as usize),
                        0,
                    );
                }
                log::info!("iwlwifi: FH firmware DMA complete");
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    /// Configure the 7265's legacy peripheral/DMA clocks before FH upload.
    fn prepare_firmware_dma(&mut self) -> Result<(), crate::DriverError> {
        let set_csr_bits = |mmio: *mut u32, reg: u32, bits: u32| {
            let value = self
                .safe_read32(reg)
                .ok_or(crate::DriverError::DeviceNotFound)?;
            unsafe { core::ptr::write_volatile(mmio.add(reg as usize), value | bits) };
            Ok::<(), crate::DriverError>(())
        };

        set_csr_bits(
            self.mmio,
            CSR_GIO_CHICKEN_BITS,
            CSR_GIO_CHICKEN_L1A_NO_L0S_RX,
        )?;
        set_csr_bits(
            self.mmio,
            CSR_GIO_CHICKEN_BITS,
            CSR_GIO_CHICKEN_DIS_L0S_EXIT_TIMER,
        )?;
        set_csr_bits(self.mmio, CSR_HW_IF_CONFIG, CSR_HW_IF_CONFIG_HAP_WAKE)?;
        set_csr_bits(self.mmio, CSR_DBG_HPET_MEM, CSR_DBG_HPET_MEM_VAL)?;

        let gp = self
            .safe_read32(CSR_GP_CNTRL)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
            );
        }
        mmio::write_barrier();

        self.write_prph(APMG_CLK_EN_REG, APMG_CLK_VAL_DMA_CLK_RQT);
        crate::timing::delay_us(20);
        let pcidev_state = self
            .read_prph(APMG_PCIDEV_STT_REG)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        self.write_prph(
            APMG_PCIDEV_STT_REG,
            pcidev_state | APMG_PCIDEV_STT_L1_ACT_DIS,
        );
        mmio::write_barrier();
        log::info!("iwlwifi: FH DMA clock enabled");
        Ok(())
    }

    pub(super) fn read_prph(&mut self, address: u32) -> Option<u32> {
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(HBUS_TARG_PRPH_RADDR as usize),
                address | (3 << 24),
            );
        }
        self.safe_read32(HBUS_TARG_PRPH_RDAT)
    }

    pub(super) fn write_prph(&mut self, address: u32, value: u32) {
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(HBUS_TARG_PRPH_WADDR as usize),
                address | (3 << 24),
            );
            core::ptr::write_volatile(self.mmio.add(HBUS_TARG_PRPH_WDAT as usize), value);
        }
    }

    pub(super) fn write_mem32(&mut self, address: u32, value: u32) {
        unsafe {
            core::ptr::write_volatile(self.mmio.add(HBUS_TARG_MEM_WADDR as usize), address);
            core::ptr::write_volatile(self.mmio.add(HBUS_TARG_MEM_WDAT as usize), value);
        }
    }

    /// Wait for the firmware "alive" response with a TSC-based timeout.
    fn wait_for_alive(&mut self) -> Result<(), crate::DriverError> {
        let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let timeout_tsc = crate::timing::ticks_per_us().saturating_mul(5_000_000);
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
                    return Err(crate::DriverError::DeviceNotFound);
                }
            }

            let int_cause = match self.safe_read32(CSR_INT) {
                Some(v) => v,
                None => return Err(crate::DriverError::DeviceNotFound),
            };
            if int_cause != 0 {
                if (int_cause & CSR_INT_BIT_ALIVE) != 0 {
                    unsafe {
                        core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                    }
                    self.fw_state = FwState::Alive;
                    return Ok(());
                }
                if (int_cause & CSR_INT_BIT_SW_ERR) != 0 {
                    unsafe {
                        core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                    }
                    return Err(crate::DriverError::Protocol);
                }
                unsafe {
                    core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                }
            }

            core::hint::spin_loop();
        }

        self.fw_state = FwState::Error;
        Err(crate::DriverError::TimedOut)
    }

    /// Start firmware upload and CPU boot without waiting for alive.
    pub(super) fn start_firmware_inner(
        &mut self,
        fw_data: &[u8],
    ) -> Result<(), crate::DriverError> {
        self.health
            .recover()
            .map_err(|_| crate::DriverError::DeviceNotFound)?;

        self.upload_firmware_and_start_cpu(fw_data)?;
        // Do not retrain the upstream PCIe link after releasing the NIC CPU
        // reset. `PciHealth::recover()` toggles bridge link state; doing that
        // while firmware is emitting its alive notification can reset or
        // disconnect the endpoint and turn a valid boot into an alive timeout.
        debug::print("iwlwifi", "fw: cpu_started");
        Ok(())
    }

    /// Check if firmware has signaled alive (non-blocking poll).
    pub(super) fn check_alive_nonblocking_inner(
        &mut self,
        start_tsc: u64,
    ) -> Result<bool, crate::DriverError> {
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        let elapsed = now.wrapping_sub(start_tsc);
        let timeout_tsc = crate::timing::ticks_per_us().saturating_mul(5_000_000);

        if elapsed >= timeout_tsc {
            let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
            let csr_gp = self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0);
            let csr_ucode = self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0);
            let csr_reset = self.safe_read32(CSR_RESET).unwrap_or(!0);
            let fh_load = self.safe_read32(FH_UCODE_LOAD_STATUS).unwrap_or(!0);
            log::warn!(
                "iwlwifi: firmware alive timeout: CSR_INT={:#010x} CSR_GP={:#010x} UCODE_GP1={:#010x} RESET={:#010x} FH_LOAD={:#010x}",
                csr_int,
                csr_gp,
                csr_ucode,
                csr_reset,
                fh_load
            );
            debug::print("iwlwifi", "fw: alive_timeout");
            self.fw_state = FwState::Error;
            return Err(crate::DriverError::TimedOut);
        }

        if !self.health.is_device_present() {
            self.fw_state = FwState::Error;
            return Err(crate::DriverError::DeviceNotFound);
        }

        let int_cause = match self.safe_read32(CSR_INT) {
            Some(v) => v,
            None => return Err(crate::DriverError::DeviceNotFound),
        };
        if (int_cause & CSR_INT_BIT_ALIVE) != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
                core::ptr::write_volatile(self.mmio.add(CSR_INT_MASK as usize), CSR_INI_SET_MASK);
            }
            self.fw_state = FwState::Alive;
            debug::print("iwlwifi", "fw: alive_ok");
            return Ok(true);
        }
        if (int_cause & CSR_INT_BIT_SW_ERR) != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }
            self.fw_state = FwState::Error;
            return Err(crate::DriverError::Protocol);
        }
        if int_cause != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }
        }

        Ok(false)
    }
}

impl NetDevice for IwlWifiDevice {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if self.fw_state != FwState::Ready {
            return Err(NetError::NotInitialized);
        }
        if frame.len() > MAX_FRAME_SIZE {
            return Err(NetError::FrameTooLarge);
        }
        self.send_raw_80211_frame(frame)
            .map_err(|_| NetError::SendFailed)
    }

    fn poll_frame(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        if self.fw_state != FwState::Ready {
            return Ok(None);
        }
        if let Some(rx_data) = self.rx_queue.pop_front() {
            if rx_data.len() > buf.len() {
                return Err(NetError::BufferTooSmall);
            }
            let len = rx_data.len();
            buf[..len].copy_from_slice(&rx_data);
            return Ok(Some(len));
        }
        Ok(None)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

impl crate::wifi::WifiDriver for IwlWifiDevice {
    fn create(
        ctx: &'static dyn crate::DriverContext,
        mmio_base: *mut u32,
        hw_rev: u32,
        device: crate::pci::PciDevice,
    ) -> Option<Box<dyn crate::wifi::WifiDriver>> {
        Self::init_from_mmio(ctx, mmio_base, hw_rev, device)
            .map(|dev| Box::new(dev) as Box<dyn crate::wifi::WifiDriver>)
    }

    fn tick(&mut self) {
        self.tick();
    }

    fn get_status(&self) -> bonder::wifi::WifiStatus {
        self.wifi_conn.status
    }

    fn hardware_revision(&self) -> u16 {
        self.hw_rev
    }

    fn start_scan(&mut self) -> bool {
        self.start_scan().is_ok()
    }

    fn get_scan_results(&self) -> Vec<AccessPoint> {
        self.scan_results.clone()
    }

    fn connect(&mut self, ssid: &Ssid, psk: Option<&str>) -> bool {
        self.connect(ssid, psk).is_ok()
    }

    fn disconnect(&mut self) {
        self.disconnect();
    }

    fn device_available(&self) -> bool {
        self.fw_state == FwState::Ready
    }

    fn connected_ssid(&self) -> Option<&Ssid> {
        self.wifi_conn.current_ssid.as_ref()
    }

    fn ip_address(&self) -> [u8; 4] {
        self.ip_address
    }

    fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        IwlWifiDevice::load_firmware(self, fw_data)
    }

    fn start_firmware(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        IwlWifiDevice::start_firmware(self, fw_data)
    }

    fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, crate::DriverError> {
        IwlWifiDevice::check_alive_nonblocking(self, start_tsc)
    }

    fn send_init_commands(&mut self) -> Result<(), crate::DriverError> {
        IwlWifiDevice::send_init_commands(self)
    }
}

pub fn try_create_iwl(
    ctx: &'static dyn crate::DriverContext,
    mmio: *mut u32,
    hw_rev: u32,
    device: crate::pci::PciDevice,
) -> Option<Box<dyn crate::wifi::WifiDriver>> {
    IwlWifiDevice::init_from_mmio(ctx, mmio, hw_rev, device)
        .map(|dev| Box::new(dev) as Box<dyn crate::wifi::WifiDriver>)
}
