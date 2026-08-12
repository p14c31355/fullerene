//! Gen2 transport for Intel AX101/AX210-family CNVi devices.
//!
//! This module follows the Linux Gen2 self-load and unified-MVM init path up
//! through NVM discovery.  The later scan/association engine remains separate
//! from the legacy 7265 implementation and is not silently routed through it.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use bonder::wifi::{self, AccessPoint, Ssid, WifiStatus};
use bonder::wpa::WpaSupplicant;

use crate::DriverContext;
use crate::mmio::{self, DmaRegion, SafeReadResult};
use crate::pci::PciDevice;
use crate::pci_health::PciHealth;

use super::modern::{
    self, CONTEXT_INFO_V2_SIZE, ContextInfoAddresses, FirmwareDmaMap, GEN2_BYTE_COUNT_TABLE_BYTES,
    GEN2_COMMAND_QUEUE, GEN2_FIRST_TB_SIZE, GEN2_RX_BUFFER_SIZE, GEN2_TX_QUEUE_BYTES, ModernFamily,
    ModernFirmware, PRPH_SCRATCH_SIZE, RxTransferDesc,
};
use super::registers::{
    CSR_DBG_HPET_MEM, CSR_DBG_HPET_MEM_VAL, CSR_GIO, CSR_GIO_CHICKEN_BITS,
    CSR_GIO_CHICKEN_L1A_NO_L0S_RX, CSR_GP_CNTRL, CSR_GP_CNTRL_INIT_DONE, CSR_HW_IF_CONFIG,
    CSR_HW_IF_CONFIG_HAP_WAKE, CSR_HW_REV, CSR_INT, CSR_INT_BIT_ALIVE, CSR_INT_BIT_FH_RX,
    CSR_INT_BIT_HW_ERR, CSR_INT_BIT_SW_ERR, CSR_INT_MASK, CSR_MAC_SHADOW_REG_CTRL,
    HBUS_TARG_PRPH_RDAT, HBUS_TARG_PRPH_WADDR, HBUS_TARG_PRPH_WDAT, HBUS_TARG_WRPTR,
    RFH_Q0_FRBDCB_WIDX_TRG, pci_dma_device_id,
};

const UMAC_PRPH_OFFSET: u32 = 0x300000;
const UREG_CPU_INIT_RUN: u32 = 0x00a0_5c44;
const CSR_MAC_ADDR: u32 = 0x380;
#[cfg(test)]
const MMIO_MAP_SIZE: usize = 0x2000;
const COMMAND_QUEUE_SIZE: usize = 128;
const CONTEXT_INFO_BOOT_CTRL: u32 = modern::CSR_CTXT_INFO_BOOT_CTRL;
const CONTEXT_INFO_ADDR: u32 = modern::CSR_CTXT_INFO_ADDR;
const IML_DATA_ADDR: u32 = modern::CSR_IML_DATA_ADDR;
const IML_SIZE_ADDR: u32 = modern::CSR_IML_SIZE_ADDR;
const AUTO_FUNC_BOOT_ENA: u32 = modern::CSR_AUTO_FUNC_BOOT_ENA;
const SYSTEM_GROUP: u8 = 0x02;
const REGULATORY_AND_NVM_GROUP: u8 = 0x0c;
const LONG_GROUP: u8 = 0x01;
const INIT_EXTENDED_CFG_CMD: u8 = 0x03;
const NVM_ACCESS_COMPLETE_CMD: u8 = 0x00;
const ADD_STA_CMD: u8 = 0x18;
const MAC_CONTEXT_CMD: u8 = 0x28;
const PHY_CONTEXT_CMD: u8 = 0x08;
const SCAN_CONFIG_CMD: u8 = 0x0c;
const SCAN_REQUEST_UMAC_CMD: u8 = 0x0d;
const SCAN_COMPLETE_UMAC_NOTIF: u8 = 0x0f;
const RX_MPDU_CMD: u8 = 0xc1;
const PHY_CONFIGURATION_CMD: u8 = 0x6a;
const INIT_COMPLETE_NOTIF: u8 = 0x04;
const INIT_COMPLETE_GROUP: u8 = LONG_GROUP;
const REPLY_ERROR: u8 = 0x02;
const TX_CMD: u8 = 0x1c;
const CSR_GIO_REG_VAL_L0S_DISABLED: u32 = 0x0000_0002;
const CSR_MAC_SHADOW_REG_CTRL_VAL: u32 = 0x800f_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingCommand {
    opcode: u8,
    group_id: u8,
    sequence: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionStage {
    Idle,
    WaitingPhy(PendingCommand),
    WaitingApSta(PendingCommand),
    WaitingMgmtQueue(PendingCommand),
    WaitingDataQueue(PendingCommand),
    WaitingAuth,
    WaitingAssoc,
    WaitingMacContext(PendingCommand),
    WaitingWpaMsg1,
    WaitingWpaMsg3,
    WaitingPairwiseKey(PendingCommand),
    WaitingGroupKey(PendingCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModernInitStage {
    NeedInitConfig,
    WaitingInitConfig(PendingCommand),
    NeedNvmComplete,
    WaitingNvmComplete(PendingCommand),
    NeedPhyConfig,
    WaitingPhyConfig(PendingCommand),
    WaitingInitComplete,
    NeedNvmInfo,
    WaitingNvmInfo(PendingCommand),
    NeedDqaEnable,
    WaitingDqaEnable(PendingCommand),
    NeedAuxSta,
    WaitingAuxSta(PendingCommand),
    NeedScanConfig,
    WaitingScanConfig(PendingCommand),
    NeedMacContext,
    WaitingMacContext(PendingCommand),
    Ready,
}

struct ModernResources {
    ctx: &'static dyn DriverContext,
    fw_sections: Vec<DmaRegion>,
    iml: Option<DmaRegion>,
    tx_queue: Option<DmaRegion>,
    data_tx_queue: Option<DmaRegion>,
    data_byte_count: Option<DmaRegion>,
    mgmt_tx_queue: Option<DmaRegion>,
    mgmt_byte_count: Option<DmaRegion>,
    rx_free: Option<DmaRegion>,
    rx_used: Option<DmaRegion>,
    rx_status: Option<DmaRegion>,
    rx_bufs: Vec<DmaRegion>,
    command_bufs: Vec<Option<DmaRegion>>,
    // One entry per possible (queue, slot). Slot reuse replaces and frees the
    // previous DMA buffer instead of growing an append-only completion list.
    tx_command_bufs: Vec<Option<DmaRegion>>,
    prph_info: Option<DmaRegion>,
    prph_scratch: Option<DmaRegion>,
    context_info: Option<DmaRegion>,
}

impl ModernResources {
    fn new(ctx: &'static dyn DriverContext) -> Self {
        Self {
            ctx,
            fw_sections: Vec::new(),
            iml: None,
            tx_queue: None,
            data_tx_queue: None,
            data_byte_count: None,
            mgmt_tx_queue: None,
            mgmt_byte_count: None,
            rx_free: None,
            rx_used: None,
            rx_status: None,
            rx_bufs: Vec::new(),
            command_bufs: (0..COMMAND_QUEUE_SIZE).map(|_| None).collect(),
            tx_command_bufs: (0..32 * 256).map(|_| None).collect(),
            prph_info: None,
            prph_scratch: None,
            context_info: None,
        }
    }

    fn map(&self, region: &mut DmaRegion, device: u16) -> Result<(), crate::DriverError> {
        region.dma_map(self.ctx, device).map(|_| ())
    }

    fn dma_device_id(device: &PciDevice) -> u16 {
        pci_dma_device_id(device.bus, device.device, device.function)
    }
}

impl Drop for ModernResources {
    fn drop(&mut self) {
        for region in &mut self.fw_sections {
            region.free(self.ctx);
        }
        for region in &mut self.rx_bufs {
            region.free(self.ctx);
        }
        for region in [
            &mut self.iml,
            &mut self.tx_queue,
            &mut self.data_tx_queue,
            &mut self.data_byte_count,
            &mut self.mgmt_tx_queue,
            &mut self.mgmt_byte_count,
            &mut self.rx_free,
            &mut self.rx_used,
            &mut self.rx_status,
            &mut self.prph_info,
            &mut self.prph_scratch,
            &mut self.context_info,
        ] {
            if let Some(region) = region.take() {
                let mut region = region;
                region.free(self.ctx);
            }
        }
        for region in &mut self.command_bufs {
            if let Some(region) = region.take() {
                let mut region = region;
                region.free(self.ctx);
            }
        }
        for region in &mut self.tx_command_bufs {
            if let Some(mut region) = region.take() {
                region.free(self.ctx);
            }
        }
    }
}

/// Minimal stateful Gen2 device. It owns all DMA objects required for Linux's
/// self-load path and exposes the common WifiDriver lifecycle boundary.
pub struct ModernIwlWifiDevice {
    mac: [u8; 6],
    pci: PciDevice,
    mmio: *mut u32,
    ctx: &'static dyn DriverContext,
    health: PciHealth,
    hw_rev: u16,
    fw_api: u32,
    fw_state: WifiStatus,
    alive: bool,
    resources: Option<ModernResources>,
    scan_results: Vec<AccessPoint>,
    connected: Option<Ssid>,
    ip: [u8; 4],
    _rx_queue: VecDeque<Vec<u8>>,
    tx_write_ptr: u16,
    mgmt_tx_queue_id: Option<u16>,
    mgmt_tx_write_ptr: u16,
    data_tx_queue_id: Option<u16>,
    data_tx_write_ptr: u16,
    rx_read_ptr: u16,
    rx_write_ptr: u16,
    rx_queue_entries: usize,
    init_stage: ModernInitStage,
    phy_config: u32,
    calib_flow: u32,
    calib_event: u32,
    phy_command_version: u8,
    phy_context_command_version: u8,
    nvm_command_version: u8,
    nvm_info_command_version: u8,
    nvm_info_notification_version: u8,
    dqa_enable_command_version: u8,
    scd_queue_config_command_version: u8,
    add_sta_command_version: u8,
    scan_config_command_version: u8,
    scan_request_command_version: u8,
    mac_context_command_version: u8,
    sta_key_command_version: u8,
    aux_sta_id: u8,
    scan_active: bool,
    pending_scan_command: Option<PendingCommand>,
    connection_stage: ConnectionStage,
    connection_ssid: Option<Ssid>,
    connection_password: Option<String>,
    connection_bssid: Option<[u8; 6]>,
    connection_beacon_interval: u16,
    wpa: Option<WpaSupplicant>,
    pending_eapol: Option<Vec<u8>>,
    nvm_info: Option<modern::NvmInfo>,
}

unsafe impl Send for ModernIwlWifiDevice {}

impl Drop for ModernIwlWifiDevice {
    fn drop(&mut self) {
        self.resources.take();
    }
}

impl ModernIwlWifiDevice {
    pub fn init_from_mmio(
        ctx: &'static dyn DriverContext,
        mmio: *mut u32,
        _pci_revision: u32,
        pci: PciDevice,
    ) -> Option<Self> {
        let health = PciHealth::new(&pci);
        if mmio.is_null() || !health.is_device_present() {
            return None;
        }
        let rx_queue_entries = ModernFamily::from_device_id(pci.device_id)?.rx_queue_entries();
        let hw_rev = read32(mmio, CSR_HW_REV, Some(&health))? as u16;
        let mac = read_mac(mmio, Some(&health));
        Some(Self {
            mac,
            pci,
            mmio,
            ctx,
            health,
            hw_rev,
            fw_api: 0,
            fw_state: WifiStatus::Disconnected,
            alive: false,
            resources: None,
            scan_results: Vec::new(),
            connected: None,
            ip: [0; 4],
            _rx_queue: VecDeque::new(),
            tx_write_ptr: 0,
            mgmt_tx_queue_id: None,
            mgmt_tx_write_ptr: 0,
            data_tx_queue_id: None,
            data_tx_write_ptr: 0,
            rx_read_ptr: 0,
            rx_write_ptr: (rx_queue_entries - 1) as u16,
            rx_queue_entries,
            init_stage: ModernInitStage::NeedInitConfig,
            phy_config: 0,
            calib_flow: 0,
            calib_event: 0,
            phy_command_version: 3,
            phy_context_command_version: 4,
            nvm_command_version: 1,
            nvm_info_command_version: 1,
            nvm_info_notification_version: 4,
            dqa_enable_command_version: 0,
            scd_queue_config_command_version: modern::SCD_QUEUE_CONFIG_COMMAND_VERSION,
            add_sta_command_version: 0,
            scan_config_command_version: 5,
            scan_request_command_version: 17,
            mac_context_command_version: 0,
            sta_key_command_version: modern::ADD_STA_KEY_DEFAULT_VERSION,
            aux_sta_id: 1,
            scan_active: false,
            pending_scan_command: None,
            connection_stage: ConnectionStage::Idle,
            connection_ssid: None,
            connection_password: None,
            connection_bssid: None,
            connection_beacon_interval: 0,
            wpa: None,
            pending_eapol: None,
            nvm_info: None,
        })
    }

    fn family(&self) -> Result<ModernFamily, crate::DriverError> {
        ModernFamily::from_device_id(self.pci.device_id).ok_or(crate::DriverError::NotSupported)
    }

    fn prepare_nic(&mut self) -> Result<(), crate::DriverError> {
        self.health
            .pre_mmio_access()
            .map_err(|_| crate::DriverError::DeviceNotFound)?;
        let set_bits = [
            (CSR_GIO, CSR_GIO_REG_VAL_L0S_DISABLED),
            (CSR_GIO_CHICKEN_BITS, CSR_GIO_CHICKEN_L1A_NO_L0S_RX),
            (CSR_DBG_HPET_MEM, CSR_DBG_HPET_MEM_VAL),
            (CSR_HW_IF_CONFIG, CSR_HW_IF_CONFIG_HAP_WAKE),
        ];
        for (reg, bits) in set_bits {
            let old = read32(self.mmio, reg, Some(&self.health))
                .ok_or(crate::DriverError::DeviceNotFound)?;
            write32(self.mmio, reg, old | bits);
        }

        // Linux's iwl_pcie_gen2_apm_init() enters D0A* by setting INIT_DONE
        // after the link/power workarounds above. A software reset belongs to
        // the stop/recovery path, not to the first Gen2 activation; doing it
        // here would add an unverified state transition before self-load.
        let gp = read32(self.mmio, CSR_GP_CNTRL, Some(&self.health))
            .ok_or(crate::DriverError::DeviceNotFound)?;
        write32(self.mmio, CSR_GP_CNTRL, gp | CSR_GP_CNTRL_INIT_DONE);
        // Linux enables the shadow register path for Gen2 after the NIC is
        // active. It prevents a sleeping device from losing host-side
        // doorbells while the command/RX rings are being populated.
        write32(
            self.mmio,
            CSR_MAC_SHADOW_REG_CTRL,
            CSR_MAC_SHADOW_REG_CTRL_VAL,
        );
        mmio::write_barrier();

        let start = unsafe { core::arch::x86_64::_rdtsc() };
        let timeout = crate::timing::ticks_per_us().saturating_mul(25_000);
        loop {
            let value = read32(self.mmio, CSR_GP_CNTRL, Some(&self.health))
                .ok_or(crate::DriverError::DeviceNotFound)?;
            if value & 1 != 0 {
                return Ok(());
            }
            if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= timeout {
                return Err(crate::DriverError::TimedOut);
            }
            core::hint::spin_loop();
        }
    }

    fn allocate_resources(
        &mut self,
        firmware: &ModernFirmware<'_>,
        rx_queue_entries: usize,
    ) -> Result<ModernResources, crate::DriverError> {
        let mut resources = ModernResources::new(self.ctx);
        let device_id = ModernResources::dma_device_id(&self.pci);
        let mut lmac = Vec::new();
        let mut umac = Vec::new();
        let mut paging = Vec::new();
        let groups = firmware
            .groups()
            .map_err(|_| crate::DriverError::Protocol)?;

        for (index, section) in firmware.sections.iter().enumerate() {
            if section.offset == 0xffff_cccc || section.offset == 0xaaaa_bbbb {
                continue;
            }
            let mut dma = DmaRegion::alloc(self.ctx, section.data.len())
                .ok_or(crate::DriverError::OutOfMemory)?;
            resources.map(&mut dma, device_id)?;
            dma.write_from(section.data);
            if index < groups.lmac.end {
                lmac.push(dma.dma_iova());
            } else if index < groups.umac.end {
                umac.push(dma.dma_iova());
            } else {
                paging.push(dma.dma_iova());
            }
            resources.fw_sections.push(dma);
        }

        let map = FirmwareDmaMap::new(firmware, &lmac, &umac, &paging)
            .map_err(|_| crate::DriverError::Protocol)?;
        let iml_data = firmware.iml.ok_or(crate::DriverError::Protocol)?;
        let mut iml =
            DmaRegion::alloc(self.ctx, iml_data.len()).ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut iml, device_id)?;
        iml.write_from(iml_data);
        resources.iml = Some(iml);

        let mut tx_queue = DmaRegion::alloc(self.ctx, GEN2_TX_QUEUE_BYTES)
            .ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut tx_queue, device_id)?;
        resources.tx_queue = Some(tx_queue);

        // Dynamic data/mgmt queues use the same Gen2 TFD shape as the
        // command queue, but each scheduler queue has its own byte-count
        // table. Allocate them before DQA is enabled so queue-config can be
        // issued without doing DMA allocation in the connection state
        // machine.
        for (queue, byte_count, slot) in [
            (
                &mut resources.data_tx_queue,
                &mut resources.data_byte_count,
                0,
            ),
            (
                &mut resources.mgmt_tx_queue,
                &mut resources.mgmt_byte_count,
                1,
            ),
        ] {
            let mut tfd_queue = DmaRegion::alloc(self.ctx, GEN2_TX_QUEUE_BYTES)
                .ok_or(crate::DriverError::OutOfMemory)?;
            tfd_queue.dma_map(self.ctx, device_id)?;
            *queue = Some(tfd_queue);

            let mut byte_count_region = DmaRegion::alloc(self.ctx, GEN2_BYTE_COUNT_TABLE_BYTES)
                .ok_or(crate::DriverError::OutOfMemory)?;
            byte_count_region.dma_map(self.ctx, device_id)?;
            *byte_count = Some(byte_count_region);
            log::debug!("iwlwifi: AX101 preallocated DQA queue class={}", slot);
        }

        let mut rx_free = DmaRegion::alloc(
            self.ctx,
            core::mem::size_of::<RxTransferDesc>() * rx_queue_entries,
        )
        .ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut rx_free, device_id)?;
        let mut rx_used = DmaRegion::alloc(self.ctx, 32 * rx_queue_entries)
            .ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut rx_used, device_id)?;
        let mut rx_status = DmaRegion::alloc(self.ctx, 2).ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut rx_status, device_id)?;

        // Linux leaves one ring slot empty so the producer and consumer
        // indices remain distinguishable.  The Context Info ring itself is
        // still sized to the full power-of-two count.
        for rbid in 1..rx_queue_entries {
            let mut buffer = DmaRegion::alloc(self.ctx, GEN2_RX_BUFFER_SIZE)
                .ok_or(crate::DriverError::OutOfMemory)?;
            let address = buffer.dma_map(self.ctx, device_id)?;
            let offset = (rbid - 1) * core::mem::size_of::<RxTransferDesc>();
            let bytes = rx_free.as_mut_slice();
            bytes[offset..offset + 2].copy_from_slice(&(rbid as u16).to_le_bytes());
            bytes[offset + 8..offset + 16].copy_from_slice(&address.to_le_bytes());
            resources.rx_bufs.push(buffer);
        }
        rx_free.flush_for_device();
        resources.rx_free = Some(rx_free);
        resources.rx_used = Some(rx_used);
        resources.rx_status = Some(rx_status);

        let mut prph_info =
            DmaRegion::alloc(self.ctx, 4096).ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut prph_info, device_id)?;
        resources.prph_info = Some(prph_info);

        let mut scratch =
            DmaRegion::alloc(self.ctx, PRPH_SCRATCH_SIZE).ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut scratch, device_id)?;
        let scratch_bytes = modern::encode_prph_scratch(
            self.hw_rev,
            resources.rx_free.as_ref().unwrap().dma_iova(),
            &map,
        )
        .map_err(|_| crate::DriverError::Protocol)?;
        scratch.write_from(&scratch_bytes);
        let scratch_dma = scratch.dma_iova();
        resources.prph_scratch = Some(scratch);

        let mut context = DmaRegion::alloc(self.ctx, CONTEXT_INFO_V2_SIZE)
            .ok_or(crate::DriverError::OutOfMemory)?;
        resources.map(&mut context, device_id)?;
        let prph_info_dma = resources.prph_info.as_ref().unwrap().dma_iova();
        let rx_status_dma = resources.rx_status.as_ref().unwrap().dma_iova();
        let used_rx_dma = resources.rx_used.as_ref().unwrap().dma_iova();
        let tx_queue_dma = resources.tx_queue.as_ref().unwrap().dma_iova();
        let context_bytes = modern::encode_context_info_v2(
            ContextInfoAddresses {
                prph_info: prph_info_dma,
                rx_status: rx_status_dma,
                tr_tail: prph_info_dma + 2048,
                cr_tail: prph_info_dma + 3072,
                tx_queue: tx_queue_dma,
                used_rx: used_rx_dma,
                prph_scratch: scratch_dma,
            },
            COMMAND_QUEUE_SIZE,
            rx_queue_entries,
        )
        .map_err(|_| crate::DriverError::Protocol)?;
        context.write_from(&context_bytes);
        resources.context_info = Some(context);
        Ok(resources)
    }

    fn kick_context_info(
        mmio: *mut u32,
        resources: &ModernResources,
    ) -> Result<(), crate::DriverError> {
        let context = resources
            .context_info
            .as_ref()
            .ok_or(crate::DriverError::NotReady)?
            .dma_iova();
        let iml = resources
            .iml
            .as_ref()
            .ok_or(crate::DriverError::NotReady)?
            .dma_iova();
        write64_bytes(mmio, CONTEXT_INFO_ADDR, context);
        write64_bytes(mmio, IML_DATA_ADDR, iml);
        write32_bytes(
            mmio,
            IML_SIZE_ADDR,
            resources.iml.as_ref().unwrap().len() as u32,
        );
        // Matches iwl_enable_fw_load_int_ctx_info(trans, false) for the
        // non-MSI-X path: the firmware self-load reports ALIVE through CSR
        // and the subsequent notification is delivered on FH_RX.
        write32(mmio, CSR_INT_MASK, CSR_INT_BIT_ALIVE | CSR_INT_BIT_FH_RX);
        write32_bytes(mmio, CONTEXT_INFO_BOOT_CTRL, AUTO_FUNC_BOOT_ENA);
        mmio::write_barrier();
        write_prph(mmio, UMAC_PRPH_OFFSET + UREG_CPU_INIT_RUN, 1);
        mmio::write_barrier();
        Ok(())
    }

    /// Submit a small wide host command through the Gen2 command queue.
    ///
    /// Linux keeps the first 20 bytes in a dedicated bidirectional buffer;
    /// using one mapped command buffer here gives the same TFD split while
    /// keeping the buffer alive until the firmware has returned a response.
    fn submit_wide_command(
        &mut self,
        opcode: u8,
        group_id: u8,
        version: u8,
        payload: &[u8],
    ) -> Result<u16, crate::DriverError> {
        let sequence = (GEN2_COMMAND_QUEUE << 8) | (self.tx_write_ptr & 0xff);
        let tx_slot = (self.tx_write_ptr as usize) & (COMMAND_QUEUE_SIZE - 1);
        if self
            .resources
            .as_ref()
            .ok_or(crate::DriverError::NotReady)?
            .command_bufs[tx_slot]
            .is_some()
        {
            return Err(crate::DriverError::Busy);
        }
        let wire = modern::encode_wide_command(opcode, group_id, sequence, version, payload)
            .map_err(|_| crate::DriverError::Protocol)?;
        let first_len = core::cmp::min(GEN2_FIRST_TB_SIZE, wire.len());
        let command_buf_len = wire.len().max(64);
        let mut command_buf =
            DmaRegion::alloc(self.ctx, command_buf_len).ok_or(crate::DriverError::OutOfMemory)?;
        let device_id = ModernResources::dma_device_id(&self.pci);
        let resources = self
            .resources
            .as_mut()
            .ok_or(crate::DriverError::NotReady)?;
        resources.map(&mut command_buf, device_id)?;
        command_buf.write_from(&wire);

        let mut tbs = [(0u64, 0u16); 2];
        tbs[0] = (command_buf.dma_iova(), first_len as u16);
        let tb_count = if wire.len() > first_len {
            tbs[1] = (
                command_buf.dma_iova() + first_len as u64,
                (wire.len() - first_len) as u16,
            );
            2
        } else {
            1
        };
        let tfd =
            modern::encode_tfh_tfd(&tbs[..tb_count]).map_err(|_| crate::DriverError::Protocol)?;
        resources.command_bufs[tx_slot] = Some(command_buf);
        let tx_queue = resources
            .tx_queue
            .as_mut()
            .ok_or(crate::DriverError::NotReady)?;
        let tfd_offset = tx_slot * core::mem::size_of::<modern::TfhTfd>();
        tx_queue.as_mut_slice()[tfd_offset..tfd_offset + tfd.len()].copy_from_slice(&tfd);
        tx_queue.flush_for_device();
        mmio::write_barrier();
        self.tx_write_ptr = self.tx_write_ptr.wrapping_add(1) & (COMMAND_QUEUE_SIZE as u16 - 1);
        write32(
            self.mmio,
            HBUS_TARG_WRPTR,
            modern::encode_gen2_tx_doorbell(GEN2_COMMAND_QUEUE, self.tx_write_ptr),
        );
        mmio::write_barrier();
        log::info!(
            "iwlwifi: AX101 MVM hcmd submitted group=0x{:02x} opcode=0x{:02x} seq=0x{:04x} payload={} tbs={} wrptr={}",
            group_id,
            opcode,
            sequence,
            payload.len(),
            tb_count,
            self.tx_write_ptr,
        );
        Ok(sequence)
    }

    fn release_command_buffer(&mut self, sequence: u16) {
        let slot = usize::from(sequence & 0xff) & (COMMAND_QUEUE_SIZE - 1);
        let Some(resources) = self.resources.as_mut() else {
            return;
        };
        if let Some(mut buffer) = resources.command_bufs[slot].take() {
            buffer.free(self.ctx);
        }
    }

    /// Tell the AX210-family RFH that the initial set of free RBDs is ready.
    /// Linux defers this until the firmware has configured the RFH during the
    /// alive transition; doing it at the same boundary avoids touching the
    /// Gen2 RFH registers during the ROM self-load.
    fn kick_initial_rx_ring(&self) {
        write32(
            self.mmio,
            RFH_Q0_FRBDCB_WIDX_TRG,
            (self.rx_write_ptr as u32) & !7,
        );
        mmio::write_barrier();
    }

    /// Drain one used-RBD entry and immediately recycle its buffer.
    fn poll_rx_packet(&mut self) -> Result<Option<(u8, u8, u16, Vec<u8>)>, crate::DriverError> {
        if let Some(packet) = self._rx_queue.pop_front() {
            let decoded =
                modern::decode_rx_packet(&packet).map_err(|_| crate::DriverError::Protocol)?;
            return Ok(Some((
                decoded.opcode,
                decoded.group_id,
                decoded.sequence,
                decoded.payload.to_vec(),
            )));
        }
        let ring_mask = (self.rx_queue_entries - 1) as u16;
        let resources = self
            .resources
            .as_mut()
            .ok_or(crate::DriverError::NotReady)?;
        let mut status = [0u8; 2];
        resources
            .rx_status
            .as_ref()
            .ok_or(crate::DriverError::NotReady)?
            .flush_for_cpu();
        resources
            .rx_status
            .as_ref()
            .ok_or(crate::DriverError::NotReady)?
            .read_into(&mut status);
        let closed = u16::from_le_bytes(status) & ring_mask;
        if self.rx_read_ptr == closed {
            return Ok(None);
        }

        let read_index = self.rx_read_ptr as usize;
        let used = resources
            .rx_used
            .as_ref()
            .ok_or(crate::DriverError::NotReady)?;
        used.flush_for_cpu();
        let used_offset = read_index * core::mem::size_of::<modern::RxCompletionDesc>();
        let used_bytes = used.as_slice();
        let rbid = u16::from_le_bytes([used_bytes[used_offset + 4], used_bytes[used_offset + 5]]);
        let buffer_index = rbid.checked_sub(1).map(usize::from).ok_or_else(|| {
            log::error!("iwlwifi: AX101 RX completion has invalid rbid=0");
            crate::DriverError::Protocol
        })?;
        if buffer_index >= resources.rx_bufs.len() {
            log::error!(
                "iwlwifi: AX101 RX completion has out-of-range rbid={} buffers={}",
                rbid,
                resources.rx_bufs.len()
            );
            return Err(crate::DriverError::Protocol);
        }

        let mut packet = Vec::new();
        packet.resize(modern::GEN2_RX_BUFFER_SIZE, 0);
        resources.rx_bufs[buffer_index].flush_for_cpu();
        resources.rx_bufs[buffer_index].read_into(&mut packet);
        let buffer_dma = resources.rx_bufs[buffer_index].dma_iova();

        // Reinsert the consumed RBD at the producer index, exactly like
        // iwl_pcie_rxmq_restock(). One slot remains empty by construction.
        let write_index = self.rx_write_ptr as usize;
        let free = resources
            .rx_free
            .as_mut()
            .ok_or(crate::DriverError::NotReady)?;
        let free_offset = write_index * core::mem::size_of::<modern::RxTransferDesc>();
        let free_bytes = free.as_mut_slice();
        free_bytes[free_offset..free_offset + 2].copy_from_slice(&rbid.to_le_bytes());
        free_bytes[free_offset + 8..free_offset + 16].copy_from_slice(&buffer_dma.to_le_bytes());
        free.flush_for_device();

        self.rx_read_ptr = (self.rx_read_ptr + 1) & ring_mask;
        self.rx_write_ptr = (self.rx_write_ptr + 1) & ring_mask;
        if self.rx_write_ptr & 7 == 0 {
            write32(self.mmio, RFH_Q0_FRBDCB_WIDX_TRG, self.rx_write_ptr as u32);
            mmio::write_barrier();
        }

        for packet in modern::split_rx_buffer(&packet).map_err(|_| crate::DriverError::Protocol)? {
            self._rx_queue.push_back(packet);
        }
        let Some(packet) = self._rx_queue.pop_front() else {
            return Ok(None);
        };
        let decoded =
            modern::decode_rx_packet(&packet).map_err(|_| crate::DriverError::Protocol)?;
        Ok(Some((
            decoded.opcode,
            decoded.group_id,
            decoded.sequence,
            decoded.payload.to_vec(),
        )))
    }

    fn poll_expected_command(
        &mut self,
        expected: PendingCommand,
    ) -> Result<Option<Vec<u8>>, crate::DriverError> {
        for _ in 0..8 {
            let Some((opcode, group_id, sequence, payload)) = self.poll_rx_packet()? else {
                return Ok(None);
            };
            if opcode == REPLY_ERROR {
                log::error!(
                    "iwlwifi: AX101 MVM command rejected group=0x{:02x} opcode=0x{:02x} payload_len={}",
                    expected.group_id,
                    expected.opcode,
                    payload.len()
                );
                return Err(crate::DriverError::Protocol);
            }
            if opcode == expected.opcode
                && group_id == expected.group_id
                && (sequence & 0xff) == (expected.sequence & 0xff)
            {
                self.release_command_buffer(sequence);
                return Ok(Some(payload));
            }
            log::debug!(
                "iwlwifi: AX101 MVM RX deferred group=0x{:02x} opcode=0x{:02x} seq=0x{:04x}",
                group_id,
                opcode,
                sequence
            );
        }
        Ok(None)
    }

    fn poll_init_complete(&mut self) -> Result<Option<()>, crate::DriverError> {
        for _ in 0..8 {
            let Some((opcode, group_id, _sequence, payload)) = self.poll_rx_packet()? else {
                return Ok(None);
            };
            if opcode == REPLY_ERROR {
                log::error!(
                    "iwlwifi: AX101 MVM init notification rejected payload_len={}",
                    payload.len()
                );
                return Err(crate::DriverError::Protocol);
            }
            if opcode == INIT_COMPLETE_NOTIF && group_id == INIT_COMPLETE_GROUP {
                return Ok(Some(()));
            }
            log::debug!(
                "iwlwifi: AX101 MVM RX while waiting for init-complete group=0x{:02x} opcode=0x{:02x}",
                group_id,
                opcode
            );
        }
        Ok(None)
    }

    fn handle_runtime_packet(&mut self, opcode: u8, group_id: u8, sequence: u16, payload: &[u8]) {
        if opcode == TX_CMD && group_id == 0 {
            self.reclaim_tx_buffer(sequence);
            return;
        }
        if let Some(expected) = self.pending_scan_command {
            if opcode == expected.opcode
                && group_id == expected.group_id
                && (sequence & 0xff) == (expected.sequence & 0xff)
            {
                self.release_command_buffer(sequence);
                self.pending_scan_command = None;
                return;
            }
        }
        if opcode == SCAN_COMPLETE_UMAC_NOTIF && group_id == LONG_GROUP {
            self.scan_active = false;
            if self.fw_state == WifiStatus::Scanning {
                self.fw_state = WifiStatus::Disconnected;
            }
            return;
        }

        // AX210-family RX_MPDU v3 has Linux's 64-byte DW2..DW17 descriptor
        // followed by the 802.11 MPDU. The common beacon parser already
        // validates the frame's management subtype and tagged-parameter
        // bounds.
        if opcode != RX_MPDU_CMD {
            return;
        }
        let Ok(frame) = modern::decode_ax210_rx_mpdu(payload) else {
            return;
        };
        let Some(beacon) = wifi::parse_beacon(frame) else {
            return;
        };
        let Some(ssid) = beacon.ssid else {
            return;
        };
        let ap = AccessPoint {
            ssid,
            bssid: beacon.header.addr3,
            channel: beacon.ds_channel.unwrap_or(0),
            // The RX descriptor's energy is device-specific; until the full
            // PHY status decoder is wired, retain a conservative usable RSSI.
            rssi: -50,
            security: wifi::security_from_beacon(beacon.capability, beacon.rsn.as_ref()),
            beacon_interval: beacon.beacon_interval,
        };
        if !self
            .scan_results
            .iter()
            .any(|existing| existing.bssid == ap.bssid)
        {
            self.scan_results.push(ap);
        }
    }

    fn reclaim_tx_buffer(&mut self, sequence: u16) {
        let queue_id = sequence >> 8;
        let index = sequence & 0xff;
        let Some(resources) = self.resources.as_mut() else {
            return;
        };
        let slot = usize::from(queue_id) * 256 + usize::from(index);
        if slot < resources.tx_command_bufs.len() {
            if let Some(mut buffer) = resources.tx_command_bufs[slot].take() {
                buffer.free(self.ctx);
            }
        }
    }

    fn submit_dqa_queue_config(
        &mut self,
        management: bool,
    ) -> Result<PendingCommand, crate::DriverError> {
        let resources = self
            .resources
            .as_ref()
            .ok_or(crate::DriverError::NotReady)?;
        let (byte_count_addr, tfd_queue_addr) = if management {
            (
                resources
                    .mgmt_byte_count
                    .as_ref()
                    .ok_or(crate::DriverError::NotReady)?
                    .dma_iova(),
                resources
                    .mgmt_tx_queue
                    .as_ref()
                    .ok_or(crate::DriverError::NotReady)?
                    .dma_iova(),
            )
        } else {
            (
                resources
                    .data_byte_count
                    .as_ref()
                    .ok_or(crate::DriverError::NotReady)?
                    .dma_iova(),
                resources
                    .data_tx_queue
                    .as_ref()
                    .ok_or(crate::DriverError::NotReady)?
                    .dma_iova(),
            )
        };
        let tid = if management { 16 } else { 0 };
        let queue_size = if management {
            modern::TX_QUEUE_SIZE_MGMT
        } else {
            modern::TX_QUEUE_SIZE_DEFAULT
        };
        let payload = modern::encode_scd_queue_config_cmd_add(
            1 << 2, // AP station ID 2
            tid,
            0, // Linux passes no legacy enable/short-format flags for API v3
            queue_size,
            byte_count_addr,
            tfd_queue_addr,
        )
        .map_err(|_| crate::DriverError::Protocol)?;
        let sequence = self.submit_wide_command(
            modern::SCD_QUEUE_CONFIG_CMD,
            modern::DATA_PATH_GROUP,
            self.scd_queue_config_command_version,
            &payload,
        )?;
        Ok(PendingCommand {
            opcode: modern::SCD_QUEUE_CONFIG_CMD,
            group_id: modern::DATA_PATH_GROUP,
            sequence,
        })
    }

    fn submit_tx_queue_frame(
        &mut self,
        frame_len: usize,
        payload: &[u8],
        management: bool,
    ) -> Result<u16, crate::DriverError> {
        let (queue_id, write_ptr, queue_size) = if management {
            (
                self.mgmt_tx_queue_id.ok_or(crate::DriverError::NotReady)?,
                self.mgmt_tx_write_ptr,
                modern::TX_QUEUE_SIZE_MGMT,
            )
        } else {
            (
                self.data_tx_queue_id.ok_or(crate::DriverError::NotReady)?,
                self.data_tx_write_ptr,
                modern::TX_QUEUE_SIZE_DEFAULT,
            )
        };
        let index = usize::from(write_ptr) & (queue_size - 1);
        let sequence = (queue_id << 8) | index as u16;
        let wire = modern::encode_narrow_command(TX_CMD, 0, sequence, payload);
        let first_len = core::cmp::min(GEN2_FIRST_TB_SIZE, wire.len());
        let mut command_buf = DmaRegion::alloc(self.ctx, wire.len().max(64))
            .ok_or(crate::DriverError::OutOfMemory)?;
        let device_id = ModernResources::dma_device_id(&self.pci);
        command_buf.dma_map(self.ctx, device_id)?;
        command_buf.write_from(&wire);

        let mut tbs = [(0u64, 0u16); 2];
        tbs[0] = (command_buf.dma_iova(), first_len as u16);
        let tb_count = if wire.len() > first_len {
            tbs[1] = (
                command_buf.dma_iova() + first_len as u64,
                (wire.len() - first_len) as u16,
            );
            2
        } else {
            1
        };
        let tfd =
            modern::encode_tfh_tfd(&tbs[..tb_count]).map_err(|_| crate::DriverError::Protocol)?;
        let byte_count = frame_len as u16;
        let resources = self
            .resources
            .as_mut()
            .ok_or(crate::DriverError::NotReady)?;
        let (tx_queue, byte_table) = if management {
            (
                resources
                    .mgmt_tx_queue
                    .as_mut()
                    .ok_or(crate::DriverError::NotReady)?,
                resources
                    .mgmt_byte_count
                    .as_mut()
                    .ok_or(crate::DriverError::NotReady)?,
            )
        } else {
            (
                resources
                    .data_tx_queue
                    .as_mut()
                    .ok_or(crate::DriverError::NotReady)?,
                resources
                    .data_byte_count
                    .as_mut()
                    .ok_or(crate::DriverError::NotReady)?,
            )
        };
        let tfd_offset = index * core::mem::size_of::<modern::TfhTfd>();
        tx_queue.as_mut_slice()[tfd_offset..tfd_offset + tfd.len()].copy_from_slice(&tfd);
        byte_table.as_mut_slice()[index * 2..index * 2 + 2]
            .copy_from_slice(&byte_count.to_le_bytes());
        tx_queue.flush_for_device();
        byte_table.flush_for_device();
        let tx_slot = usize::from(queue_id) * 256 + index;
        if let Some(mut previous) = resources.tx_command_bufs[tx_slot].replace(command_buf) {
            previous.free(self.ctx);
        }

        let next = write_ptr.wrapping_add(1) & (queue_size as u16 - 1);
        if management {
            self.mgmt_tx_write_ptr = next;
        } else {
            self.data_tx_write_ptr = next;
        }
        mmio::write_barrier();
        write32(
            self.mmio,
            HBUS_TARG_WRPTR,
            modern::encode_gen2_tx_doorbell(queue_id, next),
        );
        mmio::write_barrier();
        log::info!(
            "iwlwifi: AX101 DQA TX queue={} index={} frame_len={} tbs={} wrptr={}",
            queue_id,
            index,
            frame_len,
            tb_count,
            next,
        );
        Ok(sequence)
    }

    fn submit_data_frame(
        &mut self,
        frame: &[u8],
        encrypted: bool,
    ) -> Result<u16, crate::DriverError> {
        // IWL_TX_FLAGS_HIGH_PRI is 0x0004. EAPOL message 2 is sent before
        // keys exist and therefore also carries ENCRYPT_DIS (0x0002).
        // API89's new TX API has the firmware generate the CCMP IV and
        // encrypt the raw MPDU with the installed station key.
        let flags = 0x0004 | if encrypted { 0 } else { 0x0002 };
        let payload =
            modern::encode_tx_cmd_v10(frame, flags, 0).map_err(|_| crate::DriverError::Protocol)?;
        self.submit_tx_queue_frame(frame.len(), &payload, false)
    }

    fn submit_management_frame(&mut self, frame: &[u8]) -> Result<u16, crate::DriverError> {
        let payload = modern::encode_tx_cmd_v10(frame, 0x0004 | 0x0002, 0)
            .map_err(|_| crate::DriverError::Protocol)?;
        self.submit_tx_queue_frame(frame.len(), &payload, true)
    }

    fn submit_sta_key(
        &mut self,
        key: &[u8; 16],
        key_index: u8,
        multicast: bool,
    ) -> Result<PendingCommand, crate::DriverError> {
        // Linux's CCMP path always marks the key as mapped in the station
        // table. Pairwise and group keys occupy separate firmware slots.
        const STA_KEY_FLG_CCM: u16 = 2;
        const STA_KEY_FLG_WEP_KEY_MAP: u16 = 1 << 3;
        const STA_KEY_FLG_KEYID_POS: u16 = 8;
        const STA_KEY_MULTICAST: u16 = 1 << 14;
        let mut flags = STA_KEY_FLG_CCM
            | STA_KEY_FLG_WEP_KEY_MAP
            | ((u16::from(key_index) & 3) << STA_KEY_FLG_KEYID_POS);
        if multicast {
            flags |= STA_KEY_MULTICAST;
        }
        let key_offset = if multicast { 1 + key_index } else { 0 };
        let payload = modern::encode_add_sta_key_cmd_v2(2, key_offset, flags, key, 0, 0);
        let sequence = self.submit_wide_command(
            modern::ADD_STA_KEY_CMD,
            LONG_GROUP,
            self.sta_key_command_version,
            &payload,
        )?;
        Ok(PendingCommand {
            opcode: modern::ADD_STA_KEY_CMD,
            group_id: LONG_GROUP,
            sequence,
        })
    }

    fn build_eapol_data_frame(&self, eapol: &[u8]) -> Result<Vec<u8>, crate::DriverError> {
        let bssid = self.connection_bssid.ok_or(crate::DriverError::NotReady)?;
        if eapol.len() > u16::MAX as usize {
            return Err(crate::DriverError::Protocol);
        }
        let mut frame = Vec::with_capacity(24 + 8 + eapol.len());
        // Non-QoS data, STA -> AP. The CCMP header is inserted by firmware
        // after the pairwise key has been installed.
        frame.extend_from_slice(&[0x08, 0x00, 0x00, 0x00]);
        frame.extend_from_slice(&bssid);
        frame.extend_from_slice(&self.mac);
        frame.extend_from_slice(&bssid);
        frame.extend_from_slice(&[0x00, 0x00]);
        frame.extend_from_slice(&[0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00, 0x88, 0x8e]);
        frame.extend_from_slice(eapol);
        Ok(frame)
    }

    fn eapol_payload(frame: &[u8]) -> Option<&[u8]> {
        if frame.len() < 32 || (frame[0] >> 2) & 3 != 2 {
            return None;
        }
        let header_len = if frame[0] & 0x80 != 0 { 26 } else { 24 };
        if frame.len() < header_len + 8
            || &frame[header_len..header_len + 8]
                != &[0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00, 0x88, 0x8e]
        {
            return None;
        }
        Some(&frame[header_len + 8..])
    }

    fn submit_ap_station(&mut self) -> Result<PendingCommand, crate::DriverError> {
        let bssid = self.connection_bssid.ok_or(crate::DriverError::NotReady)?;
        let sta_id = 2;
        let payload = if self.add_sta_command_version >= 12 {
            modern::encode_add_sta_cmd_v10(0, 0, bssid, sta_id, 0, 0)
        } else {
            modern::encode_add_sta_cmd_v7(0, 0, bssid, sta_id)
        };
        let sequence = self.submit_wide_command(
            ADD_STA_CMD,
            LONG_GROUP,
            self.add_sta_command_version,
            &payload,
        )?;
        Ok(PendingCommand {
            opcode: ADD_STA_CMD,
            group_id: LONG_GROUP,
            sequence,
        })
    }

    fn handle_connection_packet(
        &mut self,
        opcode: u8,
        group_id: u8,
        sequence: u16,
        payload: &[u8],
    ) -> Result<bool, crate::DriverError> {
        if let ConnectionStage::WaitingMgmtQueue(expected) = self.connection_stage {
            if opcode == expected.opcode
                && group_id == expected.group_id
                && (sequence & 0xff) == (expected.sequence & 0xff)
            {
                self.release_command_buffer(sequence);
                let response = modern::decode_tx_queue_config_response(payload)
                    .map_err(|_| crate::DriverError::Protocol)?;
                if response.flags != 0 || response.queue_number > 0x1f {
                    return Err(crate::DriverError::Protocol);
                }
                self.mgmt_tx_queue_id = Some(response.queue_number);
                self.mgmt_tx_write_ptr =
                    response.write_pointer & (modern::TX_QUEUE_SIZE_MGMT as u16 - 1);
                let expected = self.submit_dqa_queue_config(false)?;
                self.connection_stage = ConnectionStage::WaitingDataQueue(expected);
                return Ok(true);
            }
        }

        if let ConnectionStage::WaitingDataQueue(expected) = self.connection_stage {
            if opcode == expected.opcode
                && group_id == expected.group_id
                && (sequence & 0xff) == (expected.sequence & 0xff)
            {
                self.release_command_buffer(sequence);
                let response = modern::decode_tx_queue_config_response(payload)
                    .map_err(|_| crate::DriverError::Protocol)?;
                if response.flags != 0 || response.queue_number > 0x1f {
                    return Err(crate::DriverError::Protocol);
                }
                self.data_tx_queue_id = Some(response.queue_number);
                self.data_tx_write_ptr =
                    response.write_pointer & (modern::TX_QUEUE_SIZE_DEFAULT as u16 - 1);
                let bssid = self.connection_bssid.ok_or(crate::DriverError::NotReady)?;
                let frame = wifi::build_auth_frame(bssid, self.mac, 1);
                self.submit_management_frame(&frame)?;
                self.connection_stage = ConnectionStage::WaitingAuth;
                self.fw_state = WifiStatus::Authenticating;
                return Ok(true);
            }
        }

        if opcode == REPLY_ERROR
            && matches!(
                self.connection_stage,
                ConnectionStage::WaitingPhy(_)
                    | ConnectionStage::WaitingApSta(_)
                    | ConnectionStage::WaitingMgmtQueue(_)
                    | ConnectionStage::WaitingDataQueue(_)
                    | ConnectionStage::WaitingMacContext(_)
                    | ConnectionStage::WaitingPairwiseKey(_)
                    | ConnectionStage::WaitingGroupKey(_)
            )
        {
            self.release_command_buffer(sequence);
            self.connection_stage = ConnectionStage::Idle;
            self.fw_state = WifiStatus::Error;
            return Err(crate::DriverError::Protocol);
        }

        if group_id != LONG_GROUP {
            return Ok(false);
        }

        if let ConnectionStage::WaitingPhy(expected) = self.connection_stage {
            if opcode == expected.opcode && (sequence & 0xff) == (expected.sequence & 0xff) {
                self.release_command_buffer(sequence);
                let expected = self.submit_ap_station()?;
                self.connection_stage = ConnectionStage::WaitingApSta(expected);
                return Ok(true);
            }
        }

        if let ConnectionStage::WaitingApSta(expected) = self.connection_stage {
            if opcode == expected.opcode && (sequence & 0xff) == (expected.sequence & 0xff) {
                self.release_command_buffer(sequence);
                if payload.len() < 4
                    || u32::from_le_bytes(payload[..4].try_into().unwrap()) & 0xff != 1
                {
                    self.connection_stage = ConnectionStage::Idle;
                    self.fw_state = WifiStatus::Error;
                    return Err(crate::DriverError::Protocol);
                }
                let expected = self.submit_dqa_queue_config(true)?;
                self.connection_stage = ConnectionStage::WaitingMgmtQueue(expected);
                return Ok(true);
            }
        }

        if let ConnectionStage::WaitingMacContext(expected) = self.connection_stage {
            if opcode == expected.opcode && (sequence & 0xff) == (expected.sequence & 0xff) {
                self.release_command_buffer(sequence);
                if self.wpa.is_some() {
                    self.connection_stage = ConnectionStage::WaitingWpaMsg1;
                    self.fw_state = WifiStatus::Handshake;
                } else {
                    self.connected = self.connection_ssid.clone();
                    self.connection_stage = ConnectionStage::Idle;
                    self.fw_state = WifiStatus::Connected;
                }
                return Ok(true);
            }
        }

        if let ConnectionStage::WaitingPairwiseKey(expected) = self.connection_stage {
            if opcode == expected.opcode && (sequence & 0xff) == (expected.sequence & 0xff) {
                self.release_command_buffer(sequence);
                if payload.len() < 4
                    || u32::from_le_bytes(payload[..4].try_into().unwrap()) & 0xff != 1
                {
                    self.connection_stage = ConnectionStage::Idle;
                    self.fw_state = WifiStatus::Error;
                    return Err(crate::DriverError::Protocol);
                }
                let (_, gtk, gtk_index) = self
                    .wpa
                    .as_ref()
                    .and_then(WpaSupplicant::key_material)
                    .ok_or(crate::DriverError::Protocol)?;
                let expected = self.submit_sta_key(&gtk, gtk_index, true)?;
                self.connection_stage = ConnectionStage::WaitingGroupKey(expected);
                return Ok(true);
            }
        }

        if let ConnectionStage::WaitingGroupKey(expected) = self.connection_stage {
            if opcode == expected.opcode && (sequence & 0xff) == (expected.sequence & 0xff) {
                self.release_command_buffer(sequence);
                if payload.len() < 4
                    || u32::from_le_bytes(payload[..4].try_into().unwrap()) & 0xff != 1
                {
                    self.connection_stage = ConnectionStage::Idle;
                    self.fw_state = WifiStatus::Error;
                    return Err(crate::DriverError::Protocol);
                }
                let eapol = self
                    .pending_eapol
                    .take()
                    .ok_or(crate::DriverError::Protocol)?;
                let frame = self.build_eapol_data_frame(&eapol)?;
                self.submit_data_frame(&frame, true)?;
                self.wpa
                    .as_mut()
                    .ok_or(crate::DriverError::NotReady)?
                    .complete_handshake()
                    .map_err(|_| crate::DriverError::Protocol)?;
                self.connected = self.connection_ssid.clone();
                self.connection_stage = ConnectionStage::Idle;
                self.fw_state = WifiStatus::Connected;
                return Ok(true);
            }
        }

        if opcode != RX_MPDU_CMD {
            return Ok(false);
        }
        let frame = match modern::decode_ax210_rx_mpdu(payload) {
            Ok(frame) => frame,
            Err(_) => return Ok(false),
        };
        if frame.len() < 30 {
            return Ok(false);
        }
        let subtype = (frame[0] >> 4) & 0x0f;
        let Some(bssid) = self.connection_bssid else {
            return Ok(false);
        };
        if frame[10..16] != bssid {
            return Ok(false);
        }

        if let Some(eapol) = Self::eapol_payload(frame) {
            match self.connection_stage {
                ConnectionStage::WaitingWpaMsg1 => {
                    let msg2 = self
                        .wpa
                        .as_mut()
                        .ok_or(crate::DriverError::NotReady)?
                        .handle_message_1(eapol)
                        .map_err(|_| crate::DriverError::Protocol)?;
                    let tx = self.build_eapol_data_frame(&msg2)?;
                    self.submit_data_frame(&tx, false)?;
                    self.connection_stage = ConnectionStage::WaitingWpaMsg3;
                    return Ok(true);
                }
                ConnectionStage::WaitingWpaMsg3 => {
                    let (msg4, ptk, _gtk, _gtk_index) = {
                        let wpa = self.wpa.as_mut().ok_or(crate::DriverError::NotReady)?;
                        let msg4 = wpa
                            .handle_message_3(eapol)
                            .map_err(|_| crate::DriverError::Protocol)?;
                        let (ptk, gtk, gtk_index) =
                            wpa.key_material().ok_or(crate::DriverError::Protocol)?;
                        (msg4, ptk, gtk, gtk_index)
                    };
                    self.pending_eapol = Some(msg4);
                    let expected = self.submit_sta_key(&ptk, 0, false)?;
                    self.connection_stage = ConnectionStage::WaitingPairwiseKey(expected);
                    return Ok(true);
                }
                _ => {}
            }
        }

        match self.connection_stage {
            ConnectionStage::WaitingAuth if subtype == 11 => {
                let status = u16::from_le_bytes([frame[28], frame[29]]);
                if status != 0 {
                    self.connection_stage = ConnectionStage::Idle;
                    self.fw_state = WifiStatus::Error;
                    return Ok(true);
                }
                let ssid = self
                    .connection_ssid
                    .as_ref()
                    .ok_or(crate::DriverError::NotReady)?;
                let assoc = wifi::build_assoc_request_with_security(
                    bssid,
                    self.mac,
                    ssid,
                    self.connection_password.is_some(),
                );
                self.submit_management_frame(&assoc)?;
                self.connection_stage = ConnectionStage::WaitingAssoc;
                self.fw_state = WifiStatus::Associating;
                Ok(true)
            }
            ConnectionStage::WaitingAssoc if subtype == 1 => {
                let status = u16::from_le_bytes([frame[26], frame[27]]);
                if status == 0 {
                    let mac = modern::encode_mac_context_cmd_sta_action(
                        0,
                        0,
                        self.mac,
                        bssid,
                        u32::from(self.connection_beacon_interval),
                        true,
                        2, // FW_CTXT_ACTION_MODIFY
                    );
                    let sequence = self.submit_wide_command(
                        MAC_CONTEXT_CMD,
                        LONG_GROUP,
                        self.mac_context_command_version,
                        &mac,
                    )?;
                    self.connection_stage = ConnectionStage::WaitingMacContext(PendingCommand {
                        opcode: MAC_CONTEXT_CMD,
                        group_id: LONG_GROUP,
                        sequence,
                    });
                    self.fw_state = WifiStatus::Associating;
                } else {
                    self.connection_stage = ConnectionStage::Idle;
                    self.fw_state = WifiStatus::Error;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub fn start_firmware(&mut self, data: &[u8]) -> Result<(), crate::DriverError> {
        let family = self.family()?;
        let blob = family.firmware();
        let firmware = ModernFirmware::parse(data).map_err(|_| crate::DriverError::Protocol)?;
        firmware
            .validate_api(blob)
            .map_err(|_| crate::DriverError::Protocol)?;
        let calib = firmware.default_calib.ok_or(crate::DriverError::Protocol)?;
        self.phy_config = firmware.phy_config.ok_or(crate::DriverError::Protocol)?;
        self.calib_flow = calib.flow;
        self.calib_event = calib.event;
        self.phy_command_version = firmware.command_version(PHY_CONFIGURATION_CMD, LONG_GROUP, 3);
        self.phy_context_command_version = firmware.command_version(PHY_CONTEXT_CMD, LONG_GROUP, 4);
        self.nvm_command_version =
            firmware.command_version(NVM_ACCESS_COMPLETE_CMD, REGULATORY_AND_NVM_GROUP, 1);
        self.nvm_info_command_version = firmware.command_version(
            modern::NVM_GET_INFO_CMD,
            modern::REGULATORY_AND_NVM_GROUP,
            1,
        );
        self.nvm_info_notification_version = firmware.notification_version(
            modern::NVM_GET_INFO_CMD,
            modern::REGULATORY_AND_NVM_GROUP,
            4,
        );
        self.dqa_enable_command_version =
            firmware.command_version(modern::DQA_ENABLE_CMD, modern::DATA_PATH_GROUP, 0);
        self.scd_queue_config_command_version = firmware.command_version(
            modern::SCD_QUEUE_CONFIG_CMD,
            modern::DATA_PATH_GROUP,
            modern::SCD_QUEUE_CONFIG_COMMAND_VERSION,
        );
        self.add_sta_command_version = firmware.command_version(ADD_STA_CMD, LONG_GROUP, 0);
        self.scan_config_command_version = firmware.command_version(SCAN_CONFIG_CMD, LONG_GROUP, 5);
        self.scan_request_command_version =
            firmware.command_version(SCAN_REQUEST_UMAC_CMD, LONG_GROUP, 17);
        self.mac_context_command_version = firmware.command_version(MAC_CONTEXT_CMD, LONG_GROUP, 0);
        self.sta_key_command_version = firmware.command_version(
            modern::ADD_STA_KEY_CMD,
            LONG_GROUP,
            modern::ADD_STA_KEY_DEFAULT_VERSION,
        );
        if self.scan_request_command_version < 17 {
            return Err(crate::DriverError::NotSupported);
        }
        if self.resources.is_some() {
            return Err(crate::DriverError::Busy);
        }
        self.prepare_nic()?;
        let resources = self.allocate_resources(&firmware, family.rx_queue_entries())?;
        self.fw_api = firmware.api;
        self.resources = Some(resources);
        self.tx_write_ptr = 0;
        self.rx_read_ptr = 0;
        self.rx_write_ptr = (self.rx_queue_entries - 1) as u16;
        self.scan_active = false;
        self.wpa = None;
        self.pending_eapol = None;
        self.connection_stage = ConnectionStage::Idle;
        self.init_stage = ModernInitStage::NeedInitConfig;
        self.fw_state = WifiStatus::Authenticating;
        Self::kick_context_info(self.mmio, self.resources.as_ref().unwrap())?;
        log::info!(
            "iwlwifi: Gen2 context-info boot kicked device={:04x} firmware_api={} hw_rev={:#06x}",
            self.pci.device_id,
            self.fw_api,
            self.hw_rev,
        );
        Ok(())
    }

    pub fn check_alive(&mut self, start_tsc: u64) -> Result<bool, crate::DriverError> {
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        if now.wrapping_sub(start_tsc) >= crate::timing::ticks_per_us().saturating_mul(5_000_000) {
            return Err(crate::DriverError::TimedOut);
        }
        let cause = read32(self.mmio, CSR_INT, Some(&self.health))
            .ok_or(crate::DriverError::DeviceNotFound)?;
        if cause & CSR_INT_BIT_ALIVE != 0 {
            write32(self.mmio, CSR_INT, cause);
            self.alive = true;
            self.fw_state = WifiStatus::Disconnected;
            self.kick_initial_rx_ring();
            return Ok(true);
        }
        if cause & (CSR_INT_BIT_SW_ERR | CSR_INT_BIT_HW_ERR) != 0 {
            write32(self.mmio, CSR_INT, cause);
            return Err(crate::DriverError::DeviceFault);
        }
        if cause != 0 {
            write32(self.mmio, CSR_INT, cause);
        }
        Ok(false)
    }
}

impl crate::wifi::WifiDriver for ModernIwlWifiDevice {
    fn create(
        ctx: &'static dyn DriverContext,
        mmio_base: *mut u32,
        pci_revision: u32,
        device: PciDevice,
    ) -> Option<Box<dyn crate::wifi::WifiDriver>> {
        Self::init_from_mmio(ctx, mmio_base, pci_revision, device)
            .map(|device| Box::new(device) as Box<dyn crate::wifi::WifiDriver>)
    }

    fn tick(&mut self) {
        if !self.alive || !matches!(self.init_stage, ModernInitStage::Ready) {
            return;
        }
        for _ in 0..32 {
            let packet = match self.poll_rx_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(error) => {
                    log::error!("iwlwifi: AX101 runtime RX failed: {:?}", error);
                    self.fw_state = WifiStatus::Error;
                    break;
                }
            };
            match self.handle_connection_packet(packet.0, packet.1, packet.2, &packet.3) {
                Ok(true) => continue,
                Ok(false) => self.handle_runtime_packet(packet.0, packet.1, packet.2, &packet.3),
                Err(error) => {
                    log::error!("iwlwifi: AX101 connection RX failed: {:?}", error);
                    self.fw_state = WifiStatus::Error;
                    break;
                }
            }
        }
    }

    fn get_status(&self) -> WifiStatus {
        self.fw_state
    }

    fn hardware_revision(&self) -> u16 {
        self.hw_rev
    }

    fn start_scan(&mut self) -> bool {
        if !self.alive || !matches!(self.init_stage, ModernInitStage::Ready) || self.scan_active {
            return false;
        }
        // Keep the first runtime path deterministic and within the channels
        // present on every NVM profile used by this device family. Beacons
        // are sufficient for discovery; directed probes are added once the
        // connection path supplies a target SSID.
        let mut channels = Vec::new();
        for channel in 1..=11 {
            channels.push((channel, 1)); // PHY_BAND_24
        }
        for channel in [36, 40, 44, 48] {
            channels.push((channel, 0)); // PHY_BAND_5
        }
        let payload = match modern::encode_umac_scan_request_v17(self.mac, &channels, None) {
            Ok(payload) => payload,
            Err(_) => return false,
        };
        let Ok(sequence) = self.submit_wide_command(
            SCAN_REQUEST_UMAC_CMD,
            LONG_GROUP,
            self.scan_request_command_version,
            &payload,
        ) else {
            return false;
        };
        self.scan_results.clear();
        self.pending_scan_command = Some(PendingCommand {
            opcode: SCAN_REQUEST_UMAC_CMD,
            group_id: LONG_GROUP,
            sequence,
        });
        self.scan_active = true;
        self.fw_state = WifiStatus::Scanning;
        true
    }

    fn get_scan_results(&self) -> Vec<AccessPoint> {
        self.scan_results.clone()
    }

    fn connect(&mut self, ssid: &Ssid, psk: Option<&str>) -> bool {
        if !self.alive
            || !matches!(self.init_stage, ModernInitStage::Ready)
            || self.scan_active
            || !matches!(self.connection_stage, ConnectionStage::Idle)
        {
            return false;
        }
        let Some(ap) = self
            .scan_results
            .iter()
            .find(|ap| ap.ssid == *ssid)
            .cloned()
        else {
            return false;
        };
        let secured = matches!(
            ap.security,
            wifi::Security::WpaPsk | wifi::Security::Wpa2Psk
        );
        if secured && psk.is_none() {
            return false;
        }
        if !secured && ap.security != wifi::Security::Open {
            log::warn!(
                "iwlwifi: AX101 security mode {} is not supported by the WPA2-PSK path",
                ap.security.name()
            );
            return false;
        }

        self.connection_ssid = Some(ssid.clone());
        self.connection_password = psk.map(String::from);
        self.connection_bssid = Some(ap.bssid);
        self.connection_beacon_interval = ap.beacon_interval;
        self.pending_eapol = None;
        self.wpa = None;
        if let Some(password) = psk {
            let mut wpa = WpaSupplicant::new();
            wpa.init(password, ssid.as_str(), ap.bssid, self.mac);
            self.wpa = Some(wpa);
        }
        let band = if ap.channel <= 14 { 1 } else { 0 };
        let phy =
            modern::encode_phy_context_cmd_v4(0, 0, 1, ap.channel as u32, band, 0, 0, 0, 0, 0, 0);
        let Ok(sequence) = self.submit_wide_command(
            PHY_CONTEXT_CMD,
            LONG_GROUP,
            self.phy_context_command_version,
            &phy,
        ) else {
            return false;
        };
        self.connection_stage = ConnectionStage::WaitingPhy(PendingCommand {
            opcode: PHY_CONTEXT_CMD,
            group_id: LONG_GROUP,
            sequence,
        });
        self.fw_state = WifiStatus::Authenticating;
        true
    }

    fn disconnect(&mut self) {
        self.connected = None;
        self.connection_stage = ConnectionStage::Idle;
        self.connection_ssid = None;
        self.connection_password = None;
        self.connection_bssid = None;
        self.connection_beacon_interval = 0;
        self.pending_scan_command = None;
        self.wpa = None;
        self.pending_eapol = None;
        self.fw_state = WifiStatus::Disconnected;
    }

    fn device_available(&self) -> bool {
        self.alive
    }

    fn connected_ssid(&self) -> Option<&Ssid> {
        self.connected.as_ref()
    }

    fn ip_address(&self) -> [u8; 4] {
        self.ip
    }

    fn load_firmware(&mut self, data: &[u8]) -> Result<(), crate::DriverError> {
        self.start_firmware(data)?;
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        loop {
            if self.check_alive(start)? {
                break;
            }
            core::hint::spin_loop();
        }

        // The common WifiDriver contract is synchronous: its caller installs
        // the driver only after this method returns, and then starts calling
        // tick(). Complete the same unified-MVM init state machine that the
        // non-blocking connection-state path advances between ticks. Keep a
        // separate deadline so a missing command completion cannot turn this
        // public API into an unbounded spin loop.
        let init_start = unsafe { core::arch::x86_64::_rdtsc() };
        let init_timeout = crate::timing::ticks_per_us().saturating_mul(5_000_000);
        loop {
            match self.send_init_commands() {
                Ok(()) => return Ok(()),
                Err(crate::DriverError::Pending) => {}
                Err(error) => return Err(error),
            }
            if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(init_start) >= init_timeout {
                return Err(crate::DriverError::TimedOut);
            }
            core::hint::spin_loop();
        }
    }

    fn start_firmware(&mut self, data: &[u8]) -> Result<(), crate::DriverError> {
        ModernIwlWifiDevice::start_firmware(self, data)
    }

    fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, crate::DriverError> {
        self.check_alive(start_tsc)
    }

    fn send_init_commands(&mut self) -> Result<(), crate::DriverError> {
        if !self.alive {
            return Err(crate::DriverError::NotReady);
        }

        match self.init_stage {
            ModernInitStage::NeedInitConfig => {
                log::info!(
                    "iwlwifi: AX101 firmware alive mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}; submitting Linux INIT_EXTENDED_CFG",
                    self.mac[0],
                    self.mac[1],
                    self.mac[2],
                    self.mac[3],
                    self.mac[4],
                    self.mac[5],
                );
                // IWL_INIT_NVM | IWL_INIT_PHY. AX101 is the HR1 1x1
                // configuration in Linux's RF table, so firmware must wait
                // for both command families before emitting INIT_COMPLETE.
                let init_flags = (1u32 << 1 | 1u32 << 2).to_le_bytes();
                let sequence =
                    self.submit_wide_command(INIT_EXTENDED_CFG_CMD, SYSTEM_GROUP, 0, &init_flags)?;
                self.init_stage = ModernInitStage::WaitingInitConfig(PendingCommand {
                    opcode: INIT_EXTENDED_CFG_CMD,
                    group_id: SYSTEM_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingInitConfig(expected) => {
                if self.poll_expected_command(expected)?.is_some() {
                    self.init_stage = ModernInitStage::NeedNvmComplete;
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::NeedNvmComplete => {
                let sequence = self.submit_wide_command(
                    NVM_ACCESS_COMPLETE_CMD,
                    REGULATORY_AND_NVM_GROUP,
                    self.nvm_command_version,
                    &[0; 4],
                )?;
                self.init_stage = ModernInitStage::WaitingNvmComplete(PendingCommand {
                    opcode: NVM_ACCESS_COMPLETE_CMD,
                    group_id: REGULATORY_AND_NVM_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingNvmComplete(expected) => {
                if self.poll_expected_command(expected)?.is_some() {
                    self.init_stage = ModernInitStage::NeedPhyConfig;
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::NeedPhyConfig => {
                // Linux API version 3 layout: phy_cfg, calibration flow and
                // event bitmaps, followed by four PHY filter words. The
                // API89 image carries no host-selected filter overrides.
                let mut phy = [0u8; 28];
                phy[0..4].copy_from_slice(&self.phy_config.to_le_bytes());
                phy[4..8].copy_from_slice(&self.calib_flow.to_le_bytes());
                phy[8..12].copy_from_slice(&self.calib_event.to_le_bytes());
                let sequence = self.submit_wide_command(
                    PHY_CONFIGURATION_CMD,
                    LONG_GROUP,
                    self.phy_command_version,
                    &phy,
                )?;
                self.init_stage = ModernInitStage::WaitingPhyConfig(PendingCommand {
                    opcode: PHY_CONFIGURATION_CMD,
                    group_id: LONG_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingPhyConfig(expected) => {
                if self.poll_expected_command(expected)?.is_some() {
                    self.init_stage = ModernInitStage::WaitingInitComplete;
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingInitComplete => {
                if self.poll_init_complete()?.is_some() {
                    self.init_stage = ModernInitStage::NeedNvmInfo;
                    return Err(crate::DriverError::Pending);
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::NeedNvmInfo => {
                let sequence = self.submit_wide_command(
                    modern::NVM_GET_INFO_CMD,
                    modern::REGULATORY_AND_NVM_GROUP,
                    self.nvm_info_command_version,
                    &[0; 4],
                )?;
                self.init_stage = ModernInitStage::WaitingNvmInfo(PendingCommand {
                    opcode: modern::NVM_GET_INFO_CMD,
                    group_id: modern::REGULATORY_AND_NVM_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingNvmInfo(expected) => {
                if let Some(payload) = self.poll_expected_command(expected)? {
                    let info =
                        modern::decode_nvm_info(&payload, self.nvm_info_notification_version)
                            .map_err(|_| crate::DriverError::Protocol)?;
                    log::info!(
                        "iwlwifi: AX101 NVM ready version={} board={} chains=tx{:x}/rx{:x} channels={}",
                        info.nvm_version,
                        info.board_type,
                        info.tx_chains,
                        info.rx_chains,
                        info.n_channels,
                    );
                    self.nvm_info = Some(info);
                    self.init_stage = ModernInitStage::NeedDqaEnable;
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::NeedDqaEnable => {
                // AX210/AX101's API89 command table has a DATA_PATH group.
                // Linux enables DQA before creating the AUX station; without
                // this gate, later TX_CMD frames would be placed on the
                // legacy command queue instead of a station/TID queue.
                let payload = modern::encode_dqa_enable_cmd(GEN2_COMMAND_QUEUE as u32);
                let sequence = self.submit_wide_command(
                    modern::DQA_ENABLE_CMD,
                    modern::DATA_PATH_GROUP,
                    self.dqa_enable_command_version,
                    &payload,
                )?;
                self.init_stage = ModernInitStage::WaitingDqaEnable(PendingCommand {
                    opcode: modern::DQA_ENABLE_CMD,
                    group_id: modern::DATA_PATH_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingDqaEnable(expected) => {
                if self.poll_expected_command(expected)?.is_some() {
                    self.init_stage = ModernInitStage::NeedAuxSta;
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::NeedAuxSta => {
                // API89's command table selects ADD_STA v7. Linux allocates
                // the first non-reserved station ID for this internal AUX
                // station; ID 0 is reserved for the station interface.
                let payload = modern::encode_add_sta_cmd_v7(4, 0, [0; 6], self.aux_sta_id);
                let sequence = self.submit_wide_command(
                    ADD_STA_CMD,
                    LONG_GROUP,
                    self.add_sta_command_version,
                    &payload,
                )?;
                self.init_stage = ModernInitStage::WaitingAuxSta(PendingCommand {
                    opcode: ADD_STA_CMD,
                    group_id: LONG_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingAuxSta(expected) => {
                if let Some(payload) = self.poll_expected_command(expected)? {
                    if payload.len() < 4
                        || u32::from_le_bytes(payload[..4].try_into().unwrap()) & 0xff != 1
                    {
                        log::error!(
                            "iwlwifi: AX101 AUX STA rejected status_len={}",
                            payload.len()
                        );
                        return Err(crate::DriverError::Protocol);
                    }
                    self.init_stage = ModernInitStage::NeedScanConfig;
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::NeedScanConfig => {
                if self.scan_config_command_version < 5 {
                    return Err(crate::DriverError::NotSupported);
                }
                let info = self.nvm_info.ok_or(crate::DriverError::NotReady)?;
                let payload =
                    modern::encode_scan_config_v5(info.tx_chains, info.rx_chains, self.aux_sta_id);
                let sequence = self.submit_wide_command(
                    SCAN_CONFIG_CMD,
                    LONG_GROUP,
                    self.scan_config_command_version,
                    &payload,
                )?;
                self.init_stage = ModernInitStage::WaitingScanConfig(PendingCommand {
                    opcode: SCAN_CONFIG_CMD,
                    group_id: LONG_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingScanConfig(expected) => {
                if self.poll_expected_command(expected)?.is_some() {
                    self.init_stage = ModernInitStage::NeedMacContext;
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::NeedMacContext => {
                let payload =
                    modern::encode_mac_context_cmd_sta(0, 0, self.mac, [0xff; 6], 0, false);
                let sequence = self.submit_wide_command(
                    MAC_CONTEXT_CMD,
                    LONG_GROUP,
                    self.mac_context_command_version,
                    &payload,
                )?;
                self.init_stage = ModernInitStage::WaitingMacContext(PendingCommand {
                    opcode: MAC_CONTEXT_CMD,
                    group_id: LONG_GROUP,
                    sequence,
                });
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::WaitingMacContext(expected) => {
                if self.poll_expected_command(expected)?.is_some() {
                    self.init_stage = ModernInitStage::Ready;
                    self.fw_state = WifiStatus::Disconnected;
                    log::info!("iwlwifi: AX101 MVM unified initialization complete");
                    return Ok(());
                }
                Err(crate::DriverError::Pending)
            }
            ModernInitStage::Ready => Ok(()),
        }
    }

    fn send_init_firmware_commands(&mut self) -> Result<(), crate::DriverError> {
        Err(crate::DriverError::NotSupported)
    }

    fn send_data_frame(&mut self, frame: &[u8]) -> Result<(), crate::DriverError> {
        if self.fw_state != WifiStatus::Connected {
            return Err(crate::DriverError::NotReady);
        }
        self.submit_data_frame(frame, true)?;
        Ok(())
    }

    fn check_pci_health(&mut self) -> Result<(), crate::DriverError> {
        self.health
            .check()
            .map_err(|_| crate::DriverError::DeviceNotFound)
    }
}

pub fn try_create_iwl_modern(
    ctx: &'static dyn DriverContext,
    mmio: *mut u32,
    pci_revision: u32,
    device: PciDevice,
) -> Option<Box<dyn crate::wifi::WifiDriver>> {
    ModernIwlWifiDevice::init_from_mmio(ctx, mmio, pci_revision, device)
        .map(|device| Box::new(device) as Box<dyn crate::wifi::WifiDriver>)
}

fn read32(mmio: *mut u32, offset: u32, health: Option<&PciHealth>) -> Option<u32> {
    match unsafe { mmio::checked_read_u32(mmio.add(offset as usize) as usize, health) } {
        SafeReadResult::Value(value) => Some(value),
        _ => None,
    }
}

fn write32(mmio: *mut u32, offset: u32, value: u32) {
    unsafe { core::ptr::write_volatile(mmio.add(offset as usize), value) };
}

fn write32_bytes(mmio: *mut u32, byte_offset: u32, value: u32) {
    write32(mmio, byte_offset / 4, value);
}

fn write64_bytes(mmio: *mut u32, byte_offset: u32, value: u64) {
    write32_bytes(mmio, byte_offset, value as u32);
    write32_bytes(mmio, byte_offset + 4, (value >> 32) as u32);
}

fn write_prph(mmio: *mut u32, address: u32, value: u32) {
    write32(mmio, HBUS_TARG_PRPH_WADDR, address | (3 << 24));
    write32(mmio, HBUS_TARG_PRPH_WDAT, value);
    let _ = read32(mmio, HBUS_TARG_PRPH_RDAT, None);
}

fn read_mac(mmio: *mut u32, health: Option<&PciHealth>) -> [u8; 6] {
    let Some(low) = read32(mmio, CSR_MAC_ADDR / 4, health) else {
        return [0x02, 0, 0, 0, 0, 1];
    };
    let Some(high) = read32(mmio, CSR_MAC_ADDR / 4 + 1, health) else {
        return [0x02, 0, 0, 0, 0, 1];
    };
    let mac = [
        low as u8,
        (low >> 8) as u8,
        (low >> 16) as u8,
        (low >> 24) as u8,
        high as u8,
        (high >> 8) as u8,
    ];
    if mac == [0; 6] || mac == [0xff; 6] {
        [0x02, 0, 0, 0, 0, 1]
    } else {
        mac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gen2_descriptor_constants_are_not_legacy_sized() {
        assert_eq!(core::mem::size_of::<modern::TfhTfd>(), 256);
        assert_eq!(GEN2_TX_QUEUE_BYTES, 65536);
        assert_eq!(GEN2_RX_BUFFER_SIZE, 4096);
        assert_eq!(ModernFamily::So.rx_queue_entries(), 4096);
        assert_eq!(ModernFamily::Quz.rx_queue_entries(), 2048);
        assert_eq!(modern::AX101_DEVICE_ID, 0x54f0);
        assert_eq!(MMIO_MAP_SIZE, 0x2000);
    }
}
