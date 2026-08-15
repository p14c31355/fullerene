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
use crate::mmio::{self, DmaRegion, MemRegion, SafeReadResult};
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
    pub(super) mmio: *mut u32,
    /// Sealant-backed view of BAR0. `None` is used only by host-side model
    /// instances which must never touch hardware.
    pub(super) mmio_region: Option<MemRegion>,
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
    /// API profile selected from PCI/revision matching. The parsed image API
    /// must agree with this before HCMD dispatch.
    pub selected_fw_api: u32,
    /// LAR/MCC capabilities advertised by the loaded firmware TLVs.
    pub fw_lar_supported: bool,
    pub fw_lar_v2: bool,
    pub fw_umac_scan_supported: bool,
    /// Firmware capability bit 12: dynamic queue allocation is required.
    pub fw_dqa_supported: bool,
    pub phy_config: u32,
    pub phy_sku_tlv_len: Option<u32>,
    pub runtime_calib_flow: u32,
    pub runtime_calib_event: u32,
    /// True after the INIT image completed and its calibration data is valid
    /// for replay into the runtime image.
    pub init_firmware_completed: bool,
    /// Incremental INIT-command state retained between scheduler ticks.
    pub init_commands_started: bool,
    /// API-29 sends BT_CONFIG once before the first NVM_ACCESS command.
    pub init_bt_config_sent: bool,
    /// True after runtime setup has submitted its first command. This lets a
    /// pending MAC_CONTEXT response resume without replaying setup commands.
    pub runtime_commands_started: bool,
    pub init_nvm_index: usize,
    pub init_hw_section: Option<Vec<u8>>,
    pub init_mac_ready: bool,
    pub init_response: Option<(u8, u8, u64)>,
    /// PHY calibration database sections collected from INIT firmware and
    /// replayed to the runtime image.
    pub phy_db_sections: Vec<(u16, Vec<u8>)>,
    /// SRAM pointers supplied by the firmware image for post-crash logs.
    pub runtime_errlog_ptr: u32,
    pub init_errlog_ptr: u32,
    /// Scheduler SRAM base reported by the firmware's RX ALIVE notification.
    pub alive_scd_base_addr: u32,

    /// 802.11 state.
    pub iwl_state: IwlState,
    pub wifi_conn: wifi::WifiConnection,
    pub wpa: WpaSupplicant,
    /// True while the association requires WPA2-PSK protection.
    pub wpa_required: bool,
    /// Set only after the TX ring has reported both CCMP commands consumed.
    /// Until then, WPA data traffic is rejected fail-closed.
    pub wpa_keys_installed: bool,
    /// Next CCMP packet number for protected data frames.
    pub tx_pn: u64,
    /// End position of the queued pair/group key commands, awaiting TX-ring
    /// consumption. The data path also waits for both firmware status replies.
    pub wpa_key_command_end: Option<usize>,
    /// Exact command-queue sequences awaiting successful ADD_STA_KEY replies.
    /// Message 4 remains blocked until both replies report ADD_STA_SUCCESS.
    pub wpa_key_pending_sequences: [Option<u16>; 2],
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
    /// Late RX buffers may contain beacons after the firmware's completion
    /// notification.  Keep accepting scan frames until this reaches zero.
    pub scan_result_grace_ticks: u32,
    /// Channel from the last REPLY_RX_PHY_CMD.  5 GHz beacons lack a DS
    /// Parameter Set IE, so the channel can only be determined from this
    /// PHY metadata.
    pub last_rx_phy_channel: u16,
    /// GP2/system timestamp paired with the last RX PHY notification.
    pub last_rx_system_timestamp: u32,
    /// Service ticks since the current authentication/association request.
    /// This bounds a lost management-frame exchange instead of leaving the
    /// public connection status in Authenticating forever.
    pub connection_watchdog_ticks: u32,

    /// TX/RX queues.
    pub tx_queue: VecDeque<Vec<u8>>,
    pub rx_queue: VecDeque<Vec<u8>>,
    pub tx_dma_ring: DmaRegion,
    pub rx_dma_ring: DmaRegion,
    pub tx_head: usize,
    pub tx_tail: usize,
    pub tx_data_head: usize,
    pub tx_data_tail: usize,
    pub rx_head: usize,
    pub rx_tail: usize,
    /// Absolute RBD write pointer posted to firmware. This is distinct from
    /// `rx_tail`, which tracks the host's consumed RX buffers.
    pub rx_posted: usize,

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
    pub(super) const MMIO_BAR_SIZE: usize = 0x2000;

    #[inline]
    pub(super) fn mmio_offset(reg: u32) -> Option<usize> {
        let offset = (reg as usize).checked_mul(core::mem::size_of::<u32>())?;
        (offset < Self::MMIO_BAR_SIZE).then_some(offset)
    }

    /// Write one device register through the Sealant capability.
    ///
    /// A missing capability is a deliberate fail-closed state used by model
    /// devices. Production instances always install the capability during
    /// BAR mapping.
    #[inline]
    pub(super) fn write_mmio32(&self, reg: u32, value: u32) {
        if let Some(region) = &self.mmio_region {
            if let Some(offset) = Self::mmio_offset(reg) {
                region.write32(offset, value);
            }
        }
    }

    // ── DMA helpers ──────────────────────────────────

    #[inline]
    pub(super) fn command_queue(&self) -> u32 {
        if self.fw_dqa_supported {
            IWL_DQA_CMD_QUEUE
        } else {
            IWL_LEGACY_CMD_QUEUE
        }
    }

    #[inline]
    pub(super) fn auxiliary_queue(&self) -> u32 {
        if self.fw_dqa_supported {
            IWL_DQA_AUX_QUEUE
        } else {
            IWL_LEGACY_AUX_QUEUE
        }
    }

    /// Scheduler queue used by this minimal non-QoS station TX path.
    /// Linux allocates the first free DQA management queue for management,
    /// EAPOL, and the other non-QoS frames emitted by the current stack.
    #[inline]
    pub(super) fn traffic_queue(&self) -> u32 {
        if self.fw_dqa_supported {
            IWL_MGMT_QUEUE
        } else {
            IWL_DATA_QUEUE
        }
    }

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
        // Firmware reset also resets the device-side RBD pointers.
        self.rx_head = 0;
        self.rx_tail = 0;
        // Keep the software cursor at the actual next RBD slot. The
        // hardware register is updated only with its 8-entry-aligned form;
        // collapsing the cursor to 248 here would make the first restock
        // after wraparound publish 8 instead of the correct 16.
        self.rx_posted = RX_QUEUE_SIZE - 1;
        let rx_phys = self.rx_dma_ring.dma_iova();
        let status_phys = rx_phys + (core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE) as u64;

        // Match the legacy gen1_2 RX init sequence: stop DMA, reset both
        // hardware pointers, register the RBD/status buffers, then enable
        // channel 0 for 256 4K receive buffers.
        self.write_mmio32(FH_MEM_RCSR_CHNL0_CONFIG_REG, 0);
        self.write_mmio32(FH_MEM_RCSR_CHNL0_RBDCB_WPTR, 0);
        self.write_mmio32(FH_MEM_RCSR_CHNL0_FLUSH_RB_REQ, 0);
        self.write_mmio32(FH_RSCSR_CHNL0_RDPTR_REG, 0);
        self.write_mmio32(FH_RSCSR_CHNL0_RBDCB_WPTR_REG, 0);
        self.write_mmio32(FH_RSCSR_CHNL0_RBDCB_BASE_REG, (rx_phys >> 8) as u32);
        self.write_mmio32(FH_RSCSR_CHNL0_STTS_WPTR_REG, (status_phys >> 4) as u32);
        // Keep one slot empty so the hardware can distinguish a full ring
        // from an empty ring. Linux's gen1 transport restocks 255 entries
        // and rounds the pointer down to an 8-entry boundary.
        self.write_mmio32(FH_RSCSR_CHNL0_RBDCB_WPTR_REG, (self.rx_posted as u32) & !7);
        mmio::write_barrier();
        let rx_config = FH_RCSR_RX_CONFIG_CHNL_EN_ENABLE_VAL
            | FH_RCSR_CHNL0_RX_IGNORE_RXF_EMPTY
            | FH_RCSR_CHNL0_RX_CONFIG_IRQ_DEST_INT_HOST_VAL
            | (FH_RCSR_RX_RB_TIMEOUT << FH_RCSR_RX_CONFIG_REG_IRQ_RBTH_POS)
            | (8 << FH_RCSR_RX_CONFIG_RBDCB_SIZE_POS);
        self.write_mmio32(FH_MEM_RCSR_CHNL0_CONFIG_REG, rx_config);
        mmio::write_barrier();
        // Do not read FH registers back at this pre-firmware boundary. These
        // non-posted reads are not part of Linux's RX init contract and some
        // 7265 platforms do not complete them until the firmware CPU runs.
        log::info!(
            "iwlwifi: legacy RX DMA programmed: rbd={:#018x} status={:#018x} cfg={:#010x} rbd_base={:#010x} status_ptr={:#010x} wptr={:#010x}",
            rx_phys,
            status_phys,
            rx_config,
            (rx_phys >> 8) as u32,
            (status_phys >> 4) as u32,
            (self.rx_posted as u32) & !7,
        );
    }

    /// Arm only the gen1 RX channel while the firmware CPU is held in reset.
    ///
    /// Linux initializes RX and TX before loading firmware.  Keeping TX/SCD
    /// setup behind ALIVE avoids the unsafe pre-ALIVE scheduler accesses seen
    /// on this platform, but the firmware must have an RX ring available when
    /// it emits REPLY_ALIVE.  Restrict this early phase to the direct FH RX
    /// registers and refuse it once CPU reset has already been released.
    pub(super) fn prearm_rx_before_cpu_release(&mut self) -> Result<(), crate::DriverError> {
        if !cfg!(test) {
            self.health
                .check()
                .map_err(|_| crate::DriverError::DeviceNotFound)?;
        }
        let reset = self
            .safe_read32(CSR_RESET)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        if reset == 0 {
            log::error!(
                "iwlwifi: refusing RX pre-arm after CPU release RESET={:#010x}",
                reset,
            );
            return Err(crate::DriverError::Protocol);
        }

        self.init_rx_dma();
        log::info!(
            "iwlwifi: firmware boot RX pre-armed while CPU reset asserted RESET={:#010x}",
            reset,
        );
        Ok(())
    }

    /// Publish the MAC/radio identity consumed by firmware's early boot code.
    /// This is the 7000-series part of Linux's `iwl_mvm_nic_config()` and must
    /// run after parsing PHY_SKU but before releasing the firmware CPU.
    fn configure_legacy_nic_from_firmware(&mut self) -> Result<(), crate::DriverError> {
        if self.phy_sku_tlv_len.is_none() || self.phy_config == 0 {
            log::error!(
                "iwlwifi: firmware boot missing PHY_SKU for NIC configuration tlv_len={:?} phy_config={:#010x}",
                self.phy_sku_tlv_len,
                self.phy_config,
            );
            return Err(crate::DriverError::Protocol);
        }

        let current = self
            .safe_read32(CSR_HW_IF_CONFIG)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        let fields = legacy_nic_config_fields(self.hw_rev as u32, self.phy_config);
        let configured = (current & !CSR_HW_IF_CONFIG_NIC_MASK) | fields;
        self.write_mmio32(CSR_HW_IF_CONFIG, configured);

        // Linux also disables early-power-off reset for APMG-based devices.
        // It prevents a short platform power transition from leaving the NIC
        // firmware in reset while the host believes CPU release succeeded.
        let power = self
            .read_prph(APMG_PS_CTRL_REG)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        self.write_prph(
            APMG_PS_CTRL_REG,
            power | APMG_PS_CTRL_EARLY_PWR_OFF_RESET_DIS,
        );
        mmio::write_barrier();
        log::info!(
            "iwlwifi: firmware boot NIC configured hw_rev={:#06x} phy_config={:#010x} CSR_HW_IF_CONFIG={:#010x} APMG_PS_CTRL={:#010x}",
            self.hw_rev,
            self.phy_config,
            configured,
            power | APMG_PS_CTRL_EARLY_PWR_OFF_RESET_DIS,
        );
        Ok(())
    }

    pub(super) fn restock_rx_buffers(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        self.rx_posted = (self.rx_posted + count) % RX_QUEUE_SIZE;
        self.write_mmio32(FH_RSCSR_CHNL0_RBDCB_WPTR_REG, (self.rx_posted as u32) & !7);
        mmio::write_barrier();
    }

    // ── Safe MMIO access ────────────────────────────

    #[inline]
    pub(super) fn safe_read32(&self, reg: u32) -> Option<u32> {
        // After firmware alive, some 7265 platforms transiently report the
        // PCIe endpoint as absent while firmware changes power/link state.
        // The vendor check would reject valid MMIO accesses and stop the
        // next host command. The live path is protected by the MMIO watchdog.
        let health = if matches!(self.fw_state, FwState::Alive | FwState::Ready) {
            None
        } else {
            Some(&self.health)
        };
        let Some(region) = &self.mmio_region else {
            return None;
        };
        let Some(offset) = Self::mmio_offset(reg) else {
            return None;
        };
        match region.checked_read32(offset, health) {
            SafeReadResult::Value(v) => Some(v),
            _ => None,
        }
    }

    // ── Device initialisation ───────────────────────

    /// Scan the PCI bus for a supported legacy Intel wireless device and
    /// initialize it. Modern CNVi devices are reported explicitly but never
    /// sent through the incompatible 7265 transport.
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
            if IWL_MODERN_CNVI_DEVICE_IDS.contains(&device.device_id) {
                log::warn!(
                    "iwlwifi: modern CNVi adapter {:04x}:{:04x} detected; legacy 7265 transport is not used",
                    device.vendor_id,
                    device.device_id
                );
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
                Err(error) => {
                    log::warn!("iwlwifi: init failed: {:?}", error);
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
        let bar0_size = Self::MMIO_BAR_SIZE;
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
        // BAR0 is mapped above with the exact window used by this transport;
        // retain that mapping as a checked Sealant capability for the whole
        // device lifetime.
        let mmio_region = unsafe { MemRegion::new(mmio_virt, Self::MMIO_BAR_SIZE) };

        health
            .pre_mmio_access()
            .map_err(|_| IwlError::BarNotAvailable)?;

        let hw_rev_raw = match Self::mmio_offset(CSR_HW_REV).and_then(|offset| {
            match mmio_region.checked_read32(offset, Some(&health)) {
                mmio::SafeReadResult::Value(v) => Some(v),
                _ => None,
            }
        }) {
            Some(v) => v,
            None => return Err(IwlError::BarNotAvailable),
        };
        let hw_rev = hw_rev_raw as u16;
        log::info!(
            "iwlwifi: CSR_HW_REV raw={:#010x} type={:#06x} step_dash={:#x}",
            hw_rev_raw,
            csr_hw_rev_type(hw_rev_raw),
            hw_rev_raw & 0xf,
        );

        Self::reset_device(&mmio_region);

        let Some(offset) = Self::mmio_offset(CSR_GP_CNTRL) else {
            return Err(IwlError::BarNotAvailable);
        };
        mmio_region.write32(offset, CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE);
        mmio::write_barrier();
        if !health.is_device_present() {
            return Err(IwlError::ClockNotReady);
        }
        crate::timing::delay_us(10_000);
        health.recover().map_err(|_| IwlError::ClockNotReady)?;

        let mac = Self::read_mac(&mmio_region, Some(&health));

        if let Some(offset) = Self::mmio_offset(CSR_INT_MASK) {
            mmio_region.write32(offset, 0xFFFFFFFFu32);
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
            // Keep q9 host-command and q4 data payloads in disjoint DMA
            // slots; their queue heads advance independently.
            for _ in 0..TX_QUEUE_SIZE * 2 {
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
            mmio_region: Some(mmio_region),
            hw_rev,
            ctx,
            health,
            fw_state: FwState::NotLoaded,
            fw_build: 0,
            fw_api_ver: IWL_FW_API_VER,
            selected_fw_api: IWL_FW_API_VER,
            fw_lar_supported: false,
            fw_lar_v2: false,
            fw_umac_scan_supported: false,
            fw_dqa_supported: false,
            phy_config: 0,
            phy_sku_tlv_len: None,
            runtime_calib_flow: 0,
            runtime_calib_event: 0,
            phy_db_sections: Vec::new(),
            init_firmware_completed: false,
            init_commands_started: false,
            init_bt_config_sent: false,
            runtime_commands_started: false,
            init_nvm_index: 0,
            init_hw_section: None,
            init_mac_ready: false,
            init_response: None,
            runtime_errlog_ptr: 0,
            init_errlog_ptr: 0,
            alive_scd_base_addr: 0,
            iwl_state: IwlState::Init,
            wifi_conn: wifi::WifiConnection::new(),
            wpa: WpaSupplicant::new(),
            wpa_required: false,
            wpa_keys_installed: false,
            tx_pn: 1,
            wpa_key_command_end: None,
            wpa_key_pending_sequences: [None; 2],
            pending_wpa_message4: None,
            dhcp: None,
            scan_results: Vec::new(),
            scan_channel: 0,
            scan_pending: false,
            scan_result_grace_ticks: 0,
            last_rx_phy_channel: 0,
            last_rx_system_timestamp: 0,
            connection_watchdog_ticks: 0,
            tx_queue: VecDeque::new(),
            rx_queue: VecDeque::new(),
            tx_dma_ring,
            rx_dma_ring,
            tx_head: 0,
            tx_tail: 0,
            tx_data_head: 0,
            tx_data_tail: 0,
            rx_head: 0,
            rx_tail: 0,
            rx_posted: 0,
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
        pci_revision: u32,
        device: PciDevice,
    ) -> Option<Self> {
        let health = PciHealth::new(&device);
        Self::init_after_mmio(ctx, mmio, pci_revision as u16, device, health).ok()
    }

    fn init_after_mmio(
        ctx: &'static dyn DriverContext,
        mmio: *mut u32,
        pci_revision: u16,
        device: PciDevice,
        mut health: PciHealth,
    ) -> Result<Self, IwlError> {
        if mmio.is_null() {
            debug::print("iwlwifi", "ERR null MMIO base");
            return Err(IwlError::BarNotAvailable);
        }
        let mmio_region = unsafe { MemRegion::new(mmio as usize, Self::MMIO_BAR_SIZE) };
        debug::print("iwlwifi", "init_after_mmio: enter");
        let _ = pci_revision;
        if !health.is_device_present() {
            debug::print("iwlwifi", "ERR device_gone before reset");
            return Err(IwlError::BarNotAvailable);
        }

        debug::print("iwlwifi", "reset_device");
        Self::reset_device(&mmio_region);

        debug::print("iwlwifi", "mac_clock_req");
        let Some(offset) = Self::mmio_offset(CSR_GP_CNTRL) else {
            return Err(IwlError::BarNotAvailable);
        };
        mmio_region.write32(offset, CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE);
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

        let hw_rev_raw = match Self::mmio_offset(CSR_HW_REV).and_then(|offset| {
            match mmio_region.checked_read32(offset, Some(&health)) {
                SafeReadResult::Value(v) => Some(v),
                _ => None,
            }
        }) {
            Some(v) => v,
            None => return Err(IwlError::ClockNotReady),
        };
        let hw_rev = hw_rev_raw as u16;
        log::info!(
            "iwlwifi: CSR_HW_REV raw={:#010x} type={:#06x} step_dash={:#x}",
            hw_rev_raw,
            csr_hw_rev_type(hw_rev_raw),
            hw_rev_raw & 0xf,
        );

        debug::print("iwlwifi", "read_mac");
        let mac = Self::read_mac(&mmio_region, Some(&health));

        debug::print("iwlwifi", "mask_ints");
        if let Some(offset) = Self::mmio_offset(CSR_INT_MASK) {
            mmio_region.write32(offset, 0xFFFFFFFFu32);
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
            // Keep q9 host-command and q4 data payloads in disjoint DMA
            // slots; their queue heads advance independently.
            for _ in 0..TX_QUEUE_SIZE * 2 {
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
            mmio_region: Some(mmio_region),
            hw_rev,
            ctx,
            health,
            fw_state: FwState::NotLoaded,
            fw_build: 0,
            fw_api_ver: IWL_FW_API_VER,
            selected_fw_api: IWL_FW_API_VER,
            fw_lar_supported: false,
            fw_lar_v2: false,
            fw_umac_scan_supported: false,
            fw_dqa_supported: false,
            phy_config: 0,
            phy_sku_tlv_len: None,
            runtime_calib_flow: 0,
            runtime_calib_event: 0,
            phy_db_sections: Vec::new(),
            init_firmware_completed: false,
            init_commands_started: false,
            init_bt_config_sent: false,
            runtime_commands_started: false,
            init_nvm_index: 0,
            init_hw_section: None,
            init_mac_ready: false,
            init_response: None,
            runtime_errlog_ptr: 0,
            init_errlog_ptr: 0,
            alive_scd_base_addr: 0,
            iwl_state: IwlState::Init,
            wifi_conn: wifi::WifiConnection::new(),
            wpa: WpaSupplicant::new(),
            wpa_required: false,
            wpa_keys_installed: false,
            tx_pn: 1,
            wpa_key_command_end: None,
            wpa_key_pending_sequences: [None; 2],
            pending_wpa_message4: None,
            dhcp: None,
            scan_results: Vec::new(),
            scan_channel: 0,
            scan_pending: false,
            scan_result_grace_ticks: 0,
            last_rx_phy_channel: 0,
            last_rx_system_timestamp: 0,
            connection_watchdog_ticks: 0,
            tx_queue: VecDeque::new(),
            rx_queue: VecDeque::new(),
            tx_dma_ring,
            rx_dma_ring,
            tx_head: 0,
            tx_tail: 0,
            tx_data_head: 0,
            tx_data_tail: 0,
            rx_head: 0,
            rx_tail: 0,
            rx_posted: 0,
            tx_bufs,
            rx_bufs,
            ip_address: [0u8; 4],
            subnet_mask: [0u8; 4],
            gateway: [0u8; 4],
            dns_server: [0u8; 4],
        })
    }

    /// Reset the device with posted-write + pure TSC delays.
    pub(super) fn reset_device(region: &MemRegion) {
        let Some(offset) = Self::mmio_offset(CSR_RESET) else {
            return;
        };
        region.write32(offset, CSR_RESET_BIT_STOP_MASTER);
        crate::timing::delay_us(10_000);
        region.write32(offset, CSR_RESET_BIT_SW);
        crate::timing::delay_us(10_000);
        region.write32(offset, 0);
        crate::timing::delay_us(10_000);
    }

    /// Put the NIC back at the pre-firmware boundary so the next selected
    /// image does not inherit a wedged INIT scheduler or stale RX/TX state.
    pub(super) fn prepare_firmware_retry(&mut self) -> Result<(), crate::DriverError> {
        self.health
            .recover()
            .map_err(|_| crate::DriverError::DeviceNotFound)?;
        if let Some(region) = &self.mmio_region {
            Self::reset_device(region);
        }
        self.write_mmio32(
            CSR_GP_CNTRL,
            CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
        );
        mmio::write_barrier();
        self.fw_state = FwState::NotLoaded;
        self.init_firmware_completed = false;
        self.init_commands_started = false;
        self.init_bt_config_sent = false;
        self.runtime_commands_started = false;
        self.init_nvm_index = 0;
        self.init_hw_section = None;
        self.init_mac_ready = false;
        self.init_response = None;
        self.phy_db_sections.clear();
        self.phy_config = 0;
        self.phy_sku_tlv_len = None;
        self.fw_lar_supported = false;
        self.fw_lar_v2 = false;
        self.fw_umac_scan_supported = false;
        self.fw_dqa_supported = false;
        self.runtime_calib_flow = 0;
        self.runtime_calib_event = 0;
        self.tx_head = 0;
        self.tx_tail = 0;
        self.tx_data_head = 0;
        self.tx_data_tail = 0;
        self.rx_head = 0;
        self.rx_tail = 0;
        self.rx_posted = 0;
        Ok(())
    }

    /// Read MAC address from the NVM (non-volatile memory) via CSR registers.
    pub(super) fn read_mac(region: &MemRegion, health: Option<&PciHealth>) -> [u8; 6] {
        let checked_read = |reg: u32| -> Option<u32> {
            let offset = Self::mmio_offset(reg)?;
            match region.checked_read32(offset, health) {
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
    fn upload_firmware_and_start_cpu(
        &mut self,
        fw_data: &[u8],
        image: FirmwareImage,
    ) -> Result<(), crate::DriverError> {
        debug::print("iwlwifi", "fw: check_header");
        if fw_data.len() < FW_HEADER_SIZE {
            return Err(crate::DriverError::InvalidArgument);
        }

        self.fw_state = FwState::Loading;

        // Clear stale interrupts and the host-side RF-kill/CMD_BLOCKED
        // handshake before loading a new runtime image.  GP1 is a mailbox:
        // bit 0 is a firmware-owned MAC_SLEEP status bit, so it must not be
        // written directly by the host.
        self.write_mmio32(CSR_INT, 0xFFFF_FFFF);
        self.write_mmio32(CSR_FH_INT, 0xFFFF_FFFF);
        self.write_mmio32(
            CSR_UCODE_GP1_CLR,
            CSR_UCODE_SW_BIT_RFKILL | CSR_UCODE_GP1_BIT_CMD_BLOCKED,
        );
        self.write_mmio32(CSR_INT_MASK, 0);

        let gp = self
            .safe_read32(CSR_GP_CNTRL)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        self.write_mmio32(CSR_GP_CNTRL, gp & !0x04);
        self.write_mmio32(CSR_RESET, 0x00000080);
        crate::timing::delay_us(10_000);

        // The FH service channel is fed by the legacy 7000-series DMA clock.
        // Linux enables this clock and disables the L1-Active transition in
        // its APM init before submitting the first firmware chunk. Without
        // this setup the FH registers accept the descriptor but never raise
        // the firmware-load interrupt.
        self.prepare_firmware_dma()?;

        // The Linux 7000-series configuration enables shadow registers as
        // part of NIC initialization. In particular, queue write pointers are
        // published through this path after ALIVE; leaving it disabled can
        // make a valid q9 doorbell visible in CSR space but not to the SCD.
        self.write_mmio32(CSR_MAC_SHADOW_REG_CTRL, CSR_MAC_SHADOW_REG_CTRL_ENABLE);
        mmio::write_barrier();
        log::info!(
            "iwlwifi: legacy shadow registers enabled: value={:#010x}",
            CSR_MAC_SHADOW_REG_CTRL_ENABLE,
        );

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
        self.runtime_errlog_ptr = 0;
        self.init_errlog_ptr = 0;
        self.alive_scd_base_addr = 0;
        self.phy_config = 0;
        self.phy_sku_tlv_len = None;
        self.runtime_calib_flow = 0;
        self.runtime_calib_event = 0;
        if image == FirmwareImage::Init {
            self.phy_db_sections.clear();
            self.init_firmware_completed = false;
            self.init_commands_started = false;
            self.init_nvm_index = 0;
            self.init_hw_section = None;
            self.init_mac_ready = false;
            self.init_response = None;
        }
        log::info!(
            "iwlwifi: firmware image={:?} API v{}, build {}",
            image,
            self.fw_api_ver,
            self.fw_build
        );

        let mut off = FW_HEADER_SIZE;
        let mut section_count = 0u32;
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
                TLV_DEF_CALIB => {
                    if tlv_len == 12 {
                        let ucode_type: u32 = unsafe {
                            core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                        };
                        // IWL_UCODE_REGULAR is image index 1.
                        if ucode_type == 1 {
                            self.runtime_calib_flow = unsafe {
                                core::ptr::read_unaligned(fw_ptr.add(tlv_data_off + 4) as *const u32)
                            };
                            self.runtime_calib_event = unsafe {
                                core::ptr::read_unaligned(fw_ptr.add(tlv_data_off + 8) as *const u32)
                            };
                            log::info!(
                                "iwlwifi: firmware.phy_calibration image=runtime flow={:#010x} event={:#010x}",
                                self.runtime_calib_flow,
                                self.runtime_calib_event,
                            );
                        }
                    }
                }
                TLV_PHY_SKU => {
                    self.phy_sku_tlv_len = Some(tlv_len);
                    if tlv_len == 4 {
                        self.phy_config = unsafe {
                            core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                        };
                        log::info!("iwlwifi: firmware.phy_sku config={:#010x}", self.phy_config,);
                    }
                }
                TLV_ENABLED_CAPABILITIES => {
                    if tlv_len == 8 {
                        let api_index: u32 = unsafe {
                            core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                        };
                        let capabilities: u32 = unsafe {
                            core::ptr::read_unaligned(fw_ptr.add(tlv_data_off + 4) as *const u32)
                        };
                        // Linux treats these as a 128-bit bitmap split into
                        // u32 entries. LAR is bit 1 and LAR API v2 is bit
                        // 73 (entry 2, bit 9).
                        if api_index == 0 {
                            self.fw_lar_supported = capabilities & (1 << 1) != 0;
                            self.fw_umac_scan_supported = capabilities & (1 << 2) != 0;
                            self.fw_dqa_supported = capabilities & (1 << 12) != 0;
                        }
                        if api_index == 2 {
                            self.fw_lar_v2 = capabilities & (1 << 9) != 0;
                        }
                        log::info!(
                            "iwlwifi: firmware.capabilities api_index={} bitmap={:#010x} lar={} lar_v2={} umac_scan={} dqa={}",
                            api_index,
                            capabilities,
                            self.fw_lar_supported,
                            self.fw_lar_v2,
                            self.fw_umac_scan_supported,
                            self.fw_dqa_supported,
                        );
                    }
                }
                TLV_RUNT_ERRLOG_PTR | TLV_INIT_ERRLOG_PTR => {
                    if tlv_len == 4 {
                        let pointer: u32 = unsafe {
                            core::ptr::read_unaligned(fw_ptr.add(tlv_data_off) as *const u32)
                        };
                        if tlv_type == TLV_RUNT_ERRLOG_PTR {
                            self.runtime_errlog_ptr = pointer;
                            log::info!(
                                "iwlwifi: firmware.error_log_ptr image=runtime addr={:#010x}",
                                pointer
                            );
                        } else {
                            self.init_errlog_ptr = pointer;
                            log::info!(
                                "iwlwifi: firmware.error_log_ptr image=init addr={:#010x}",
                                pointer
                            );
                        }
                    }
                }
                TLV_SEC_INIT | TLV_SEC_RT => {
                    let wanted = match image {
                        FirmwareImage::Init => TLV_SEC_INIT,
                        FirmwareImage::Runtime => TLV_SEC_RT,
                    };
                    if tlv_type != wanted {
                        off = tlv_end;
                        continue;
                    }
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
                        // 7000-series firmware has a second SRAM address
                        // window beginning at 0x40000. Linux selects that
                        // window through LMPM_CHICK for the duration of the
                        // section transfer; without it a successful FH DMA
                        // completion can still place the image at the wrong
                        // internal address and prevent firmware alive.
                        let extended_addr =
                            (FW_MEM_EXTENDED_START..=FW_MEM_EXTENDED_END).contains(&target);
                        let previous_chick = if extended_addr {
                            let value = self
                                .read_prph(LMPM_CHICK)
                                .ok_or(crate::DriverError::DeviceNotFound)?;
                            self.write_prph(LMPM_CHICK, value | LMPM_CHICK_EXTENDED_ADDR_SPACE);
                            Some(value)
                        } else {
                            None
                        };
                        let upload_result = self.upload_section(target, section_data);
                        if let Some(value) = previous_chick {
                            self.write_prph(LMPM_CHICK, value);
                        }
                        upload_result?;
                        section_count += 1;
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

        // The 7265 is a gen1 device. Linux updates FH_UCODE_LOAD_STATUS only
        // for the newer gen2 section loader; writing the gen2 section mask on
        // this device can prevent the image from reaching its alive path.
        // Linux configures the MAC/radio identity and arms RX before starting
        // the firmware CPU. Publishing only HAP_WAKE leaves the 2025 image's
        // early boot environment observably different from upstream.
        self.configure_legacy_nic_from_firmware()?;
        // Linux's gen1 NIC initialization also arms RX before starting the
        // firmware CPU. Do this only after service-DMA upload has completed,
        // minimizing pre-ALIVE DMA exposure while still making REPLY_ALIVE
        // deliverable from the first firmware instruction onward.
        self.prearm_rx_before_cpu_release()?;
        // Linux's matching TX init phase only publishes inert host-memory
        // addresses and scheduler geometry. Queue/FIFO/DMA activation remains
        // gated on a valid ALIVE payload in `start_legacy_dma_after_alive()`.
        self.prearm_tx_foundation_before_cpu_release()?;
        self.log_fw_boot_registers("sections_ready");

        debug::print("iwlwifi", "fw: upload_done");
        log::info!("iwlwifi: firmware upload complete, starting CPU...");

        self.write_mmio32(CSR_INT, 0xFFFF_FFFF);
        // Match the gen1 Linux path: enable the normal host interrupt set
        // before releasing CPU reset. This includes SW/HW error causes.
        self.write_mmio32(CSR_INT_MASK, CSR_INI_SET_MASK);
        self.write_mmio32(CSR_RESET, 0);
        mmio::write_barrier();
        self.log_fw_boot_registers("cpu_released");
        crate::timing::delay_us(10_000);

        Ok(())
    }

    /// Capture the gen1 firmware boot hand-off state without changing any
    /// device register. This is intentionally small enough to leave enabled
    /// in real-device debug logs.
    fn log_fw_boot_registers(&self, stage: &str) {
        log::info!(
            "iwlwifi: fw.boot stage={} CSR_INT={:#010x} CSR_INT_MASK={:#010x} CSR_FH_INT={:#010x} CSR_GP={:#010x} CSR_UCODE_GP1={:#010x} RESET={:#010x} FH_LOAD={:#010x} FH_TX_CFG={:#010x}",
            stage,
            self.safe_read32(CSR_INT).unwrap_or(!0),
            self.safe_read32(CSR_INT_MASK).unwrap_or(!0),
            self.safe_read32(CSR_FH_INT).unwrap_or(!0),
            self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0),
            self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0),
            self.safe_read32(CSR_RESET).unwrap_or(!0),
            self.safe_read32(FH_UCODE_LOAD_STATUS).unwrap_or(!0),
            self.safe_read32(FH_TCSR_CHNL_TX_CONFIG_SRVC).unwrap_or(!0),
        );
    }

    /// Load firmware binary into the device.
    pub(super) fn load_firmware_inner(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        self.upload_firmware_and_start_cpu(fw_data, FirmwareImage::Runtime)?;

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
        self.write_mmio32(CSR_INT_MASK, CSR_INI_SET_MASK);

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

        self.write_mmio32(FH_TCSR_CHNL_TX_CONFIG_SRVC, 0);
        self.write_mmio32(FH_SRVC_CHNL_SRAM_ADDR, target_addr);
        self.write_mmio32(FH_TFDIB_CTRL0_SRVC, dma_addr as u32);
        self.write_mmio32(
            FH_TFDIB_CTRL1_SRVC,
            (((dma_addr >> 32) as u32) & 0xF) << FH_MEM_TFDIB_REG1_ADDR_BITSHIFT
                | byte_count as u32,
        );
        self.write_mmio32(
            FH_TCSR_CHNL_TX_BUF_STS_SRVC,
            FH_TCSR_TX_BUF_STS_TB_NUM | FH_TCSR_TX_BUF_STS_TB_IDX | FH_TCSR_TX_BUF_STS_TFDB_VALID,
        );
        self.write_mmio32(CSR_INT, CSR_INT_BIT_FH_TX);
        self.write_mmio32(CSR_FH_INT, CSR_FH_INT_BIT_TX_CHNL0);
        self.write_mmio32(CSR_INT_MASK, CSR_INT_BIT_FH_TX);
        self.write_mmio32(
            FH_TCSR_CHNL_TX_CONFIG_SRVC,
            FH_TCSR_TX_CONFIG_DMA_ENABLE | FH_TCSR_TX_CONFIG_CIRQ_HOST_ENDTFD,
        );
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
                self.write_mmio32(FH_TCSR_CHNL_TX_CONFIG_SRVC, 0);
                return Err(crate::DriverError::TimedOut);
            }

            let int_cause = self
                .safe_read32(CSR_INT)
                .ok_or(crate::DriverError::DeviceNotFound)?;
            if (int_cause & CSR_INT_BIT_FH_TX) != 0 {
                let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(0);
                self.write_mmio32(CSR_INT, int_cause);
                self.write_mmio32(CSR_FH_INT, fh_int);
                self.write_mmio32(FH_TCSR_CHNL_TX_CONFIG_SRVC, 0);
                log::info!("iwlwifi: FH firmware DMA complete");
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    /// Configure the 7265's legacy peripheral/DMA clocks before FH upload.
    fn prepare_firmware_dma(&mut self) -> Result<(), crate::DriverError> {
        let set_csr_bits = |reg: u32, bits: u32| {
            let value = self
                .safe_read32(reg)
                .ok_or(crate::DriverError::DeviceNotFound)?;
            self.write_mmio32(reg, value | bits);
            Ok::<(), crate::DriverError>(())
        };

        set_csr_bits(CSR_GIO_CHICKEN_BITS, CSR_GIO_CHICKEN_L1A_NO_L0S_RX)?;
        set_csr_bits(CSR_GIO_CHICKEN_BITS, CSR_GIO_CHICKEN_DIS_L0S_EXIT_TIMER)?;
        set_csr_bits(CSR_HW_IF_CONFIG, CSR_HW_IF_CONFIG_HAP_WAKE)?;
        set_csr_bits(CSR_DBG_HPET_MEM, CSR_DBG_HPET_MEM_VAL)?;

        let gp = self
            .safe_read32(CSR_GP_CNTRL)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        self.write_mmio32(
            CSR_GP_CNTRL,
            gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
        );
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
        self.write_mmio32(HBUS_TARG_PRPH_RADDR, address | (3 << 24));
        self.safe_read32(HBUS_TARG_PRPH_RDAT)
    }

    pub(super) fn write_prph(&mut self, address: u32, value: u32) {
        self.write_mmio32(HBUS_TARG_PRPH_WADDR, address | (3 << 24));
        self.write_mmio32(HBUS_TARG_PRPH_WDAT, value);
    }

    pub(super) fn write_mem32(&mut self, address: u32, value: u32) {
        self.write_mmio32(HBUS_TARG_MEM_WADDR, address);
        self.write_mmio32(HBUS_TARG_MEM_WDAT, value);
    }

    pub(super) fn read_mem32(&mut self, address: u32) -> Option<u32> {
        self.write_mmio32(HBUS_TARG_MEM_RADDR, address);
        self.safe_read32(HBUS_TARG_MEM_RDAT)
    }

    /// Read the compact LMAC error table written by firmware after a
    /// software assertion.  The pointer comes from the firmware TLV rather
    /// than from a guessed SRAM address.
    pub(super) fn record_alive_notification(&mut self, payload: &[u8]) {
        // MVM_ALIVE (API v3) starts with status/flags, followed by the LMAC
        // alive structure. The LMAC debug-address block starts at offset 20:
        // its error-event-table pointer is first and the SCD SRAM base is at
        // offset 40. Linux does not start the transport TX scheduler until
        // this RX notification has been parsed.
        if payload.len() < 44 {
            return;
        }
        let status = u16::from_le_bytes([payload[0], payload[1]]);
        let flags = u16::from_le_bytes([payload[2], payload[3]]);
        let ucode_major = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
        let ucode_minor = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
        let ucode_subtype = payload[12];
        let ucode_type = payload[13];
        let mac = payload[14];
        let opt = payload[15];
        let timestamp = u32::from_le_bytes([payload[16], payload[17], payload[18], payload[19]]);
        let error_log_ptr =
            u32::from_le_bytes([payload[20], payload[21], payload[22], payload[23]]);
        let scd_base_addr =
            u32::from_le_bytes([payload[40], payload[41], payload[42], payload[43]]);
        self.alive_scd_base_addr = scd_base_addr;

        let image = if self.init_firmware_completed {
            if error_log_ptr != 0 {
                self.runtime_errlog_ptr = error_log_ptr;
            }
            "runtime"
        } else {
            if error_log_ptr != 0 {
                self.init_errlog_ptr = error_log_ptr;
            }
            "init"
        };
        log::info!(
            "iwlwifi: firmware.alive.rx image={} status={:#06x} flags={:#06x} ucode={}.{} type={:#04x} subtype={:#04x} mac={:#04x} opt={:#04x} timestamp={:#010x} error_log_ptr={:#010x} scd_base={:#010x}",
            image,
            status,
            flags,
            ucode_major,
            ucode_minor,
            ucode_type,
            ucode_subtype,
            mac,
            opt,
            timestamp,
            error_log_ptr,
            scd_base_addr,
        );
    }

    pub(super) fn log_firmware_error_table(&mut self, command: &str) {
        // The firmware container may publish pointers for both images. Pick
        // the currently executing image first instead of always preferring
        // runtime while the INIT image is still reporting its failure.
        let (image, base) = if self.init_firmware_completed {
            if self.runtime_errlog_ptr != 0 {
                ("runtime", self.runtime_errlog_ptr)
            } else {
                ("init", self.init_errlog_ptr)
            }
        } else if self.init_errlog_ptr != 0 {
            ("init", self.init_errlog_ptr)
        } else {
            ("runtime", self.runtime_errlog_ptr)
        };
        if base == 0 {
            log::error!(
                "iwlwifi: firmware.error_log command={} status=pointer_missing runtime=0x00000000 init=0x00000000",
                command
            );
            return;
        }

        // LOG_ERROR_TABLE_API_S_VER_3 has 38 dwords through flow_handler.
        // The first compact log only exposed the command identity; retain the
        // execution addresses and error-specific data as well so a watchdog
        // can be distinguished from a malformed-command assertion.
        let mut words = [0u32; 38];
        for (index, word) in words.iter_mut().enumerate() {
            *word = self
                .read_mem32(base.saturating_add((index * 4) as u32))
                .unwrap_or(!0);
        }
        let error_name = match words[1] {
            0x34 => "NMI_INTERRUPT_WDG",
            0x35 => "SYSASSERT",
            0x37 => "UCODE_VERSION_MISMATCH",
            0x38 => "BAD_COMMAND",
            0x3c => "NMI_INTERRUPT_DATA_ACTION_PT",
            0x3d => "FATAL_ERROR",
            0x46 => "NMI_TRM_HW_ERR",
            0x4c => "NMI_INTERRUPT_TRM",
            0x54 => "NMI_INTERRUPT_BREAK_POINT",
            0x5c => "NMI_INTERRUPT_WDG_RXF_FULL",
            0x64 => "NMI_INTERRUPT_WDG_NO_RBD_RXF_FULL",
            0x66 => "NMI_INTERRUPT_HOST",
            0x7c => "NMI_INTERRUPT_ACTION_PT",
            0x84 => "NMI_INTERRUPT_UNKNOWN",
            0x86 => "NMI_INTERRUPT_INST_ACTION_PT",
            _ => "ADVANCED_SYSASSERT_OR_UNKNOWN",
        };
        log::error!(
            "iwlwifi: firmware.error_log command={} image={} base={:#010x} valid={:#010x} error_id={:#010x} name={} hcmd={:#010x} last_cmd_id={:#010x} isr0={:#010x} isr1={:#010x} isr2={:#010x} isr3={:#010x} isr4={:#010x}",
            command,
            image,
            base,
            words[0],
            words[1],
            error_name,
            words[23],
            words[29],
            words[24],
            words[25],
            words[26],
            words[27],
            words[28],
        );
        log::error!(
            "iwlwifi: firmware.error_detail trm_hw_status0={:#010x} trm_hw_status1={:#010x} blink2={:#010x} ilink1={:#010x} ilink2={:#010x} data1={:#010x} data2={:#010x} data3={:#010x}",
            words[2],
            words[3],
            words[4],
            words[5],
            words[6],
            words[7],
            words[8],
            words[9],
        );
        log::error!(
            "iwlwifi: firmware.error_state gp1={:#010x} gp2={:#010x} log_pc={:#010x} frame_ptr={:#010x} stack_ptr={:#010x} flow_handler={:#010x}",
            words[13],
            words[14],
            words[20],
            words[21],
            words[22],
            words[37],
        );
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
                    self.write_mmio32(CSR_INT, int_cause);
                    self.fw_state = FwState::Alive;
                    return Ok(());
                }
                if (int_cause & CSR_INT_BIT_SW_ERR) != 0 {
                    self.write_mmio32(CSR_INT, int_cause);
                    return Err(crate::DriverError::Protocol);
                }
                self.write_mmio32(CSR_INT, int_cause);
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

        self.upload_firmware_and_start_cpu(fw_data, FirmwareImage::Init)?;
        // Do not retrain the upstream PCIe link after releasing the NIC CPU
        // reset. `PciHealth::recover()` toggles bridge link state; doing that
        // while firmware is emitting its alive notification can reset or
        // disconnect the endpoint and turn a valid boot into an alive timeout.
        debug::print("iwlwifi", "fw: cpu_started");
        Ok(())
    }

    /// Start the operational image after the INIT image has completed.
    pub(super) fn start_runtime_firmware_inner(
        &mut self,
        fw_data: &[u8],
    ) -> Result<(), crate::DriverError> {
        self.health
            .recover()
            .map_err(|_| crate::DriverError::DeviceNotFound)?;
        self.upload_firmware_and_start_cpu(fw_data, FirmwareImage::Runtime)
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
            let int_mask = self.safe_read32(CSR_INT_MASK).unwrap_or(!0);
            let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
            log::warn!(
                "iwlwifi: firmware alive timeout: CSR_INT={:#010x} INT_MASK={:#010x} FH_INT={:#010x} CSR_GP={:#010x} UCODE_GP1={:#010x} RESET={:#010x} FH_LOAD={:#010x}",
                csr_int,
                int_mask,
                fh_int,
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
            self.write_mmio32(CSR_INT, int_cause);
            self.write_mmio32(CSR_INT_MASK, CSR_INI_SET_MASK);
            self.fw_state = FwState::Alive;
            debug::print("iwlwifi", "fw: alive_ok");
            return Ok(true);
        }
        if (int_cause & CSR_INT_BIT_SW_ERR) != 0 {
            self.write_mmio32(CSR_INT, int_cause);
            self.fw_state = FwState::Error;
            return Err(crate::DriverError::Protocol);
        }
        if int_cause != 0 {
            self.write_mmio32(CSR_INT, int_cause);
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
        // The scheduler currently has no 802.11 data TX queue. Returning
        // success here would make DHCP and other callers believe a frame was
        // delivered even though send_raw_80211_frame rejects it later.
        if frame
            .first()
            .is_some_and(|control| (control & 0x0C) >> 2 == 2)
        {
            return Err(NetError::SendFailed);
        }
        // NetDevice is also used by protocol helpers that may run outside
        // the device phase.  Treat this method as the compatibility adapter:
        // it only owns/enqueues the frame.  The scheduler later submits it
        // through WifiDriver::send_data_frame and publishes a CQ entry.
        if super::connection_state::enqueue_data_frame(frame) {
            Ok(())
        } else {
            Err(NetError::SendFailed)
        }
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
        pci_revision: u32,
        device: crate::pci::PciDevice,
    ) -> Option<Box<dyn crate::wifi::WifiDriver>> {
        Self::init_from_mmio(ctx, mmio_base, pci_revision, device)
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

    fn set_firmware_api_profile(&mut self, api: u32) {
        self.selected_fw_api = api;
    }

    fn prepare_firmware_retry(&mut self) -> Result<(), crate::DriverError> {
        IwlWifiDevice::prepare_firmware_retry(self)
    }

    fn start_runtime_firmware(&mut self, fw_data: &[u8]) -> Result<(), crate::DriverError> {
        IwlWifiDevice::start_runtime_firmware_inner(self, fw_data)
    }

    fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, crate::DriverError> {
        IwlWifiDevice::check_alive_nonblocking(self, start_tsc)
    }

    fn send_init_commands(&mut self) -> Result<(), crate::DriverError> {
        IwlWifiDevice::send_init_commands(self)
    }

    fn check_pci_health(&mut self) -> Result<(), crate::DriverError> {
        self.health
            .check()
            .map_err(|_| crate::DriverError::DeviceNotFound)
    }

    fn send_init_firmware_commands(&mut self) -> Result<(), crate::DriverError> {
        IwlWifiDevice::send_init_firmware_commands(self)
    }

    fn send_data_frame(&mut self, frame: &[u8]) -> Result<(), crate::DriverError> {
        self.send_raw_80211_frame(frame)
    }
}

pub fn try_create_iwl(
    ctx: &'static dyn crate::DriverContext,
    mmio: *mut u32,
    pci_revision: u32,
    device: crate::pci::PciDevice,
) -> Option<Box<dyn crate::wifi::WifiDriver>> {
    IwlWifiDevice::init_from_mmio(ctx, mmio, pci_revision, device)
        .map(|dev| Box::new(dev) as Box<dyn crate::wifi::WifiDriver>)
}

// ── Test infrastructure ──────────────────────────────────────────

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use crate::driver_context::{DriverContext, DriverContextError, PageFlags};
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Heap-backed `DriverContext` for unit tests.  Each DMA allocation is
    /// backed by a leaked `Box<[u8]>` so the "physical address" is the raw
    /// heap pointer.  `phys_to_virt` is identity, and `dma_map` returns the
    /// physical address unchanged.
    pub struct HeapDriverContext {
        backing: spin::Mutex<Vec<Box<[u8]>>>,
    }

    impl HeapDriverContext {
        pub fn new() -> Self {
            Self {
                backing: spin::Mutex::new(Vec::new()),
            }
        }

        /// Leak this context to obtain a `&'static` reference.
        pub fn leaked() -> &'static Self {
            Box::leak(Box::new(Self::new()))
        }
    }

    impl DriverContext for HeapDriverContext {
        fn phys_to_virt(&self, phys: u64) -> usize {
            phys as usize
        }

        fn zero_dma_buffer(&self, _phys: u64, _bytes: usize) {}

        fn allocate_frame(&self) -> Result<u64, DriverContextError> {
            self.allocate_contiguous_frames(1)
        }

        fn allocate_contiguous_frames(&self, count: usize) -> Result<u64, DriverContextError> {
            let size = count * 4096;
            let buf = vec![0u8; size].into_boxed_slice();
            let ptr = buf.as_ptr() as u64;
            self.backing.lock().push(buf);
            Ok(ptr)
        }

        fn map_mmio_region(&self, _: usize, _: usize, _: usize) -> Result<(), DriverContextError> {
            Ok(())
        }

        fn unmap_mmio_region(&self, _: usize, _: usize, _: usize) {}

        fn map_page(&self, _: usize, _: usize, _: PageFlags) -> Result<(), DriverContextError> {
            Ok(())
        }

        fn free_frame(&self, _: u64) {}

        fn free_contiguous_frames(&self, _: u64, _: usize) {}

        fn dma_map(&self, _: u16, phys: u64, _: usize) -> Result<u64, DriverContextError> {
            Ok(phys)
        }

        fn dma_unmap(&self, _: u64, _: usize) {}
    }

    impl IwlWifiDevice {
        /// Construct a minimal device backed by a fake MMIO buffer and
        /// heap-allocated DMA regions.  `fw_state` is set to `Ready` so
        /// `safe_read32` bypasses PCI health checks.
        pub fn new_for_test(mac: [u8; 6]) -> Self {
            let ctx = HeapDriverContext::leaked();

            // Fake MMIO: cover the same BAR window as production code.
            let mmio_vec =
                vec![0u32; Self::MMIO_BAR_SIZE / core::mem::size_of::<u32>()].into_boxed_slice();
            let mmio = Box::into_raw(mmio_vec) as *mut u32;
            // Pre-set CSR_GP_CNTRL with MAC_CLOCK_READY so `wake_for_hcmd`
            // succeeds immediately on the first poll.
            unsafe {
                *mmio.add(CSR_GP_CNTRL as usize) = CSR_GP_CNTRL_MAC_CLOCK_READY;
            }

            let pci_dev = PciDevice {
                bus: 0,
                device: 0,
                function: 0,
                handle: 0,
                vendor_id: 0x8086,
                device_id: 0x095A,
                class_code: 0x02,
                subclass: 0x80,
                prog_if: 0,
                header_type: 0,
            };
            let health = PciHealth::new(&pci_dev);

            // Allocate DMA regions.
            let mut tx_dma_ring =
                DmaRegion::alloc(ctx, TX_DMA_ALLOCATION_BYTES).expect("TX ring DMA");
            tx_dma_ring.dma_map(ctx, 0).expect("TX ring map");

            let rx_ring_size = core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE
                + core::mem::size_of::<RxDmaStatus>();
            let mut rx_dma_ring = DmaRegion::alloc(ctx, rx_ring_size).expect("RX ring DMA");
            rx_dma_ring.dma_map(ctx, 0).expect("RX ring map");

            let mut tx_bufs = Vec::new();
            // Keep q9 host-command and q4 data payloads in disjoint DMA
            // slots; their queue heads advance independently.
            for _ in 0..TX_QUEUE_SIZE * 2 {
                let mut buf = DmaRegion::alloc(ctx, MAX_FRAME_SIZE).expect("TX buf DMA");
                buf.dma_map(ctx, 0).expect("TX buf map");
                tx_bufs.push(buf);
            }

            let mut rx_bufs = Vec::new();
            let rx_virt = rx_dma_ring.virt() as *mut RxDmaDesc;
            for i in 0..RX_QUEUE_SIZE {
                let mut buf = DmaRegion::alloc(ctx, RX_BUFFER_SIZE).expect("RX buf DMA");
                let dma = buf.dma_map(ctx, 0).expect("RX buf map");
                unsafe {
                    (*rx_virt.add(i)).addr = (dma >> 8) as u32;
                    mmio::cache_flush(rx_virt.add(i) as usize);
                }
                rx_bufs.push(buf);
            }

            Self {
                mac,
                _pci_dev: pci_dev,
                mmio,
                mmio_region: Some(unsafe { MemRegion::new(mmio as usize, Self::MMIO_BAR_SIZE) }),
                hw_rev: 0x095A,
                ctx,
                health,
                fw_state: FwState::Ready,
                fw_build: 0,
                fw_api_ver: 17,
                selected_fw_api: 17,
                fw_lar_supported: false,
                fw_lar_v2: false,
                fw_umac_scan_supported: false,
                fw_dqa_supported: false,
                phy_config: 0,
                phy_sku_tlv_len: None,
                runtime_calib_flow: 0,
                runtime_calib_event: 0,
                phy_db_sections: Vec::new(),
                init_firmware_completed: true,
                init_commands_started: true,
                init_bt_config_sent: true,
                runtime_commands_started: true,
                init_nvm_index: 0,
                init_hw_section: None,
                init_mac_ready: true,
                init_response: None,
                runtime_errlog_ptr: 0,
                init_errlog_ptr: 0,
                alive_scd_base_addr: 0,
                iwl_state: IwlState::Init,
                wifi_conn: wifi::WifiConnection::new(),
                wpa: WpaSupplicant::new(),
                wpa_required: false,
                wpa_keys_installed: false,
                tx_pn: 1,
                wpa_key_command_end: None,
                wpa_key_pending_sequences: [None; 2],
                pending_wpa_message4: None,
                dhcp: None,
                scan_results: Vec::new(),
                scan_channel: 0,
                scan_pending: false,
                scan_result_grace_ticks: 0,
                last_rx_phy_channel: 0,
                last_rx_system_timestamp: 0,
                connection_watchdog_ticks: 0,
                tx_queue: VecDeque::new(),
                rx_queue: VecDeque::new(),
                tx_dma_ring,
                rx_dma_ring,
                tx_head: 0,
                tx_tail: 0,
                tx_data_head: 0,
                tx_data_tail: 0,
                rx_head: 0,
                rx_tail: 0,
                rx_posted: 0,
                tx_bufs,
                rx_bufs,
                ip_address: [0; 4],
                subnet_mask: [0; 4],
                gateway: [0; 4],
                dns_server: [0; 4],
            }
        }

        /// Read back the most recent TX frame written to the DMA ring.
        pub fn last_tx_frame(&self) -> &[u8] {
            if self.tx_data_head == 0 {
                return &[];
            }
            let idx = (self.tx_data_head - 1) % TX_QUEUE_SIZE;
            let buf = &self.tx_bufs[TX_QUEUE_SIZE + idx];
            let wire = buf.as_slice();
            if wire.len() < TX_FRAME_OFFSET {
                return &[];
            }
            let frame_len = u16::from_le_bytes([wire[4], wire[5]]) as usize;
            let end = TX_FRAME_OFFSET.saturating_add(frame_len).min(wire.len());
            &wire[TX_FRAME_OFFSET..end]
        }

        /// Read a TX frame by ring index.
        pub fn tx_frame_at(&self, index: usize) -> &[u8] {
            let idx = index % TX_QUEUE_SIZE;
            let wire = self.tx_bufs[TX_QUEUE_SIZE + idx].as_slice();
            if wire.len() < TX_FRAME_OFFSET {
                return &[];
            }
            let frame_len = u16::from_le_bytes([wire[4], wire[5]]) as usize;
            let end = TX_FRAME_OFFSET.saturating_add(frame_len).min(wire.len());
            &wire[TX_FRAME_OFFSET..end]
        }

        /// Simulate firmware consuming all queued TX descriptors by advancing
        /// both command and data queue tails to their respective heads.
        pub fn drain_tx(&mut self) {
            self.tx_tail = self.tx_head;
            self.tx_data_tail = self.tx_data_head;
        }
    }
}
