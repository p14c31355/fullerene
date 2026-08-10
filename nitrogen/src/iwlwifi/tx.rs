//! Host-command and transmit-ring handling for [`IwlWifiDevice`].

use crate::mmio;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use super::device::IwlWifiDevice;
use super::registers::*;
use super::types::*;

/// The legacy TFD stores the buffer length in bits 4..15 of `hi_n_len`.
const TFD_LENGTH_MAX: usize = 0x0fff;

// Legacy 7265 management/data frames use the API-v6 TX command.  A 1 Mbps
// CCK rate is valid for the 2.4 GHz management exchange used by this driver;
// the firmware command wrapper is the important part here, since placing the
// raw 802.11 frame at byte zero makes the firmware interpret 0xb0/0x00 as a
// command opcode.
const TX_RATE_1M_CCK: u32 = 10 | (1 << 9) | (1 << 14);
const TX_CMD_OPCODE: u8 = 0x1c;

const _: () = assert!(
    core::mem::size_of::<HcmdHeaderWide>() + core::mem::size_of::<ScanRequestCmd>()
        <= MAX_FRAME_SIZE
);
const _: () = assert!(
    core::mem::size_of::<HcmdHeaderWide>() + core::mem::size_of::<ScanRequestCmd>()
        <= TFD_LENGTH_MAX
);
const _: () = assert!(core::mem::size_of::<MacContextCmd>() == 148);

struct HexBytes<'a>(&'a [u8]);

impl fmt::Display for HexBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index != 0 {
                f.write_str(" ")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl IwlWifiDevice {
    /// Keep the MAC awake while the firmware consumes a host command.
    ///
    /// The 7000-series Linux transport sets MAC_ACCESS_REQ and waits for
    /// MAC_CLOCK_READY before touching internal scheduler registers. Without
    /// this handshake, a live 7265 may enter power-save during a command and
    /// subsequent CSR reads look like a disappeared PCIe endpoint.
    fn wake_for_hcmd(&mut self) -> Result<(), crate::DriverError> {
        // Do not make the request depend on a successful read first.  On the
        // affected 7265 systems the first CSR read after firmware alive can
        // transiently return all-ones even though a CSR write still reaches
        // the endpoint.  Use zero as the conservative fallback and report
        // the raw read result so the next boot log distinguishes that case
        // from a genuine handshake timeout.
        let initial_gp = self.safe_read32(CSR_GP_CNTRL);
        log::debug!(
            "iwlwifi: MAC access request before host command initial_gp={:?}",
            initial_gp,
        );
        let gp = initial_gp.unwrap_or(0);
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
            );
        }
        mmio::write_barrier();

        let ready = crate::timing::poll_timeout_us(15_000, || {
            let value = self.safe_read32(CSR_GP_CNTRL)?;
            (value & CSR_GP_CNTRL_MAC_CLOCK_READY != 0 && value & CSR_GP_CNTRL_GOING_TO_SLEEP == 0)
                .then_some(())
        });
        if ready.is_none() {
            self.release_mac_access();
            log::error!(
                "iwlwifi: MAC access handshake timed out before host command GP_CNTRL={:#010x}",
                self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0),
            );
            return Err(crate::DriverError::DeviceNotFound);
        }
        Ok(())
    }

    /// Release the MAC wake request after the command queue becomes empty.
    ///
    /// The request is deliberately held across descriptor submission and
    /// firmware consumption, but leaving it set across commands prevents
    /// the 7265 power-management state machine from completing its transition
    /// after PHY_CONFIGURATION.
    fn release_mac_access(&mut self) {
        let Some(gp) = self.safe_read32(CSR_GP_CNTRL) else {
            log::warn!("iwlwifi: cannot release MAC access: GP_CNTRL unavailable");
            return;
        };
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(CSR_GP_CNTRL as usize),
                gp & !CSR_GP_CNTRL_MAC_ACCESS_REQ,
            );
        }
        mmio::write_barrier();
    }

    /// Replay the calibration sections collected from INIT firmware to the
    /// operational image. This is the API-v17 equivalent of Linux's
    /// iwl_send_phy_db_data() sequence.
    fn send_runtime_phy_db(&mut self) -> Result<(), crate::DriverError> {
        if !self.init_firmware_completed {
            log::info!("iwlwifi: runtime PHY DB skipped reason=init_firmware_not_completed");
            return Ok(());
        }
        let sections = core::mem::take(&mut self.phy_db_sections);
        let result = (|| {
            if sections.is_empty() {
                log::warn!("iwlwifi: runtime PHY DB empty after INIT calibration");
                return Ok(());
            }
            for (section_type, data) in &sections {
                if data.len() > u16::MAX as usize {
                    return Err(crate::DriverError::InvalidArgument);
                }
                let mut payload = Vec::with_capacity(4 + data.len());
                payload.extend_from_slice(&section_type.to_le_bytes());
                payload.extend_from_slice(&(data.len() as u16).to_le_bytes());
                payload.extend_from_slice(data);
                self.send_init_hcmd(
                    "PHY_DB",
                    LegacyCmd::PhyDb as u8,
                    GroupId::Legacy as u8,
                    &payload,
                )?;
                log::debug!(
                    "iwlwifi: runtime.phy_db section={} bytes={}",
                    section_type,
                    data.len(),
                );
            }
            Ok(())
        })();
        self.phy_db_sections = sections;
        result
    }

    fn init_tx_cmd_queue(&mut self) {
        // The INIT and runtime images use the same host allocation but each
        // firmware reset starts its scheduler pointers at zero.
        self.tx_head = 0;
        self.tx_tail = 0;
        let ring_phys = self.tx_dma_ring.dma_iova();
        let aux_ring_phys = ring_phys + TX_AUX_TFD_RING_OFFSET as u64;
        let keep_warm_phys = ring_phys + TX_KEEP_WARM_OFFSET as u64;
        let scd_bc_phys = ring_phys + TX_SCD_BC_OFFSET as u64;

        // The command queue is FIFO mode on gen1 hardware and still needs the
        // scheduler context/active FIFO setup after firmware alive.
        self.write_prph(SCD_TXFACT, 0);
        self.write_prph(SCD_EN_CTRL, 0);
        if let Some(scd_base) = self.read_prph(SCD_SRAM_BASE_ADDR) {
            // Linux clears the complete SCD SRAM region before enabling any
            // queue: queue contexts, TX status entries, and the queue-to-
            // RA/TID translation table. Clearing only q9/q11 leaves stale
            // state after a warm reboot; SCD_QUEUE_CFG is the first command
            // that makes the firmware consume that state and can then make
            // the 7265 disappear from PCIe.
            for offset in (SCD_CONTEXT_MEM_LOWER_BOUND..SCD_TRANS_TBL_MEM_UPPER_BOUND).step_by(4) {
                self.write_mem32(scd_base + offset, 0);
            }

            // Reset the scheduler's host-memory backing pointer after alive.
            // The table is also needed by the legacy scheduler even though
            // the command queue itself is non-aggregated.
            self.write_prph(SCD_DRAM_BASE_ADDR, (scd_bc_phys >> 10) as u32);
            // The chain-extension path is enabled by default on gen1, but it
            // is unreliable on the 7265 legacy scheduler. Keep the command
            // queue on the ordinary TFD path, as upstream does.
            self.write_prph(SCD_CHAINEXT_EN, 0);
            self.write_mem32(scd_base + SCD_CONTEXT_QUEUE_CMD, 0);
            self.write_mem32(scd_base + SCD_CONTEXT_QUEUE_CMD + 4, 64 | (64 << 16));
            // The scan engine uses the internal station's q11. Configure it
            // before ADD_STA_AUX, just as Linux does; the firmware validates
            // the station's tfd_queue_msk against this scheduler entry.
            self.write_mem32(scd_base + SCD_CONTEXT_QUEUE_AUX, 0);
            self.write_mem32(scd_base + SCD_CONTEXT_QUEUE_AUX + 4, 64 | (64 << 16));
        } else {
            log::warn!(
                "iwlwifi: unable to read SCD SRAM base; command scheduler backing table was not configured"
            );
        }
        // Linux enables automatic queue activation before publishing the
        // queue status. Otherwise the SCD can retain the post-reset inactive
        // state even though the status register looks active.
        let scd_gp = self.read_prph(SCD_GP_CTRL).unwrap_or(0);
        self.write_prph(SCD_GP_CTRL, scd_gp | SCD_GP_CTRL_AUTO_ACTIVE_MODE);
        self.write_prph(SCD_QUEUE_RDPTR_CMD, 0);
        self.write_prph(SCD_QUEUE_RDPTR_AUX, 0);
        // Match Linux's iwl_trans_pcie_txq_enable(): stop the command queue
        // before rewriting its context/status.  Clearing SCD_EN_CTRL alone
        // is not sufficient on a warm 7265 reset; the queue can retain its
        // old active state and then refuse a later large host command.
        self.write_prph(SCD_QUEUE_STATUS_CMD, 1 << 19);
        self.write_prph(
            SCD_QUEUE_STATUS_AUX,
            1 << 19, // SCD_QUEUE_STTS_REG_POS_SCD_ACT_EN: inactive while configuring
        );
        self.write_prph(SCD_QUEUECHAIN_SEL, 1 << IWL_AUX_QUEUE);
        self.write_prph(SCD_AGGR_SEL, 0);
        self.write_prph(
            SCD_QUEUE_STATUS_AUX,
            SCD_QUEUE_STTS_ACTIVE
                | 5 // IWL_MVM_TX_FIFO_MCAST
                | SCD_QUEUE_STTS_WSL
                | SCD_QUEUE_STTS_MASK,
        );
        self.write_prph(
            SCD_QUEUE_STATUS_CMD,
            SCD_QUEUE_STTS_ACTIVE
                | SCD_QUEUE_STTS_WSL
                | SCD_QUEUE_STTS_FIFO_COMMAND
                | SCD_QUEUE_STTS_MASK,
        );
        self.write_prph(SCD_TXFACT, 0xFF);
        self.write_prph(SCD_EN_CTRL, 1 << IWL_CMD_QUEUE);

        unsafe {
            // The keep-warm buffer must be a separate 4 KiB-aligned DMA
            // region. It occupies the page immediately after the TFD ring in
            // the single contiguous allocation.
            core::ptr::write_volatile(
                self.mmio.add(FH_KW_MEM_ADDR_REG as usize),
                (keep_warm_phys >> 4) as u32,
            );
            // The 7265 uses a gen1 128-byte TFD and the API-v17 MVM command
            // queue 9. The previous code rang 0x0bc, which is not
            // HBUS_TARG_WRPTR on this device and therefore never submitted
            // the scan command.
            core::ptr::write_volatile(
                self.mmio.add(FH_MEM_CBBC_CMD_QUEUE as usize),
                (ring_phys >> 8) as u32,
            );
            core::ptr::write_volatile(
                self.mmio.add(FH_MEM_CBBC_AUX_QUEUE as usize),
                (aux_ring_phys >> 8) as u32,
            );
            core::ptr::write_volatile(self.mmio.add(HBUS_TARG_WRPTR as usize), IWL_CMD_QUEUE << 8);
            // The FH exposes eight physical DMA channels. q9/q11 are logical
            // SCD queues and select physical channels through their FIFO
            // fields; using 9/11 as TCSR channel numbers writes outside the
            // valid FH TX channel window.
            for channel in 0..FH_TCSR_CHNL_NUM {
                core::ptr::write_volatile(
                    self.mmio
                        .add((FH_TCSR_CHNL_TX_CONFIG_BASE + channel * (0x20 / 4)) as usize),
                    FH_TCSR_TX_CONFIG_DMA_ENABLE | FH_TCSR_TX_CONFIG_DMA_CREDIT_ENABLE,
                );
            }
            let chicken = core::ptr::read_volatile(self.mmio.add(FH_TX_CHICKEN_BITS as usize));
            core::ptr::write_volatile(
                self.mmio.add(FH_TX_CHICKEN_BITS as usize),
                chicken | FH_TX_CHICKEN_BITS_SCD_AUTO_RETRY_EN,
            );
        }
        mmio::write_barrier();
        let fh_config = self
            .safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + SCD_QUEUE_STTS_FIFO_COMMAND * (0x20 / 4));
        let scd_status = self.read_prph(SCD_QUEUE_STATUS_CMD);
        let scd_active = self.read_prph(SCD_EN_CTRL);
        let scd_chainext = self.read_prph(SCD_CHAINEXT_EN);
        log::info!(
            "iwlwifi: legacy TX queues configured: cmd_q={} cmd_fifo={} cmd_tfd={:#018x} aux_q={} aux_fifo=5 aux_tfd={:#018x} kw={:#018x} scd_bc={:#018x} fh_cmd_cfg={:#010x} scd_status={:#010x} aux_status={:#010x} scd_en={:#010x} scd_chainext={:#010x}",
            IWL_CMD_QUEUE,
            SCD_QUEUE_STTS_FIFO_COMMAND,
            ring_phys,
            IWL_AUX_QUEUE,
            aux_ring_phys,
            keep_warm_phys,
            scd_bc_phys,
            fh_config.unwrap_or(!0),
            scd_status.unwrap_or(!0),
            self.read_prph(SCD_QUEUE_STATUS_AUX).unwrap_or(!0),
            scd_active.unwrap_or(!0),
            scd_chainext.unwrap_or(!0),
        );
    }

    /// Queue WPA2-PSK CCMP pairwise and group key installation commands.
    ///
    /// iwlwifi performs CCMP in the NIC/firmware.  Keeping the keys only in
    /// the supplicant is not sufficient: raw data frames must never leave the
    /// device until these two ADD_STA_KEY commands have been consumed by
    /// firmware.
    ///
    /// IMPORTANT: This function only queues the commands asynchronously.  The
    /// returned TX-ring position must be retained and checked from the device
    /// tick before enabling the protected data path.
    pub(super) fn install_wpa_keys(
        &mut self,
        ptk: [u8; 16],
        gtk: [u8; 16],
        gtk_key_index: u8,
    ) -> Result<usize, crate::DriverError> {
        const STA_KEY_FLG_CCM: u16 = 2;
        const STA_KEY_FLG_KEYID_POS: u16 = 8;
        const STA_KEY_MULTICAST: u16 = 1 << 14;

        let mut pairwise = AddStaKeyCmd {
            // The AP is the first peer station in this minimal STA mode.
            sta_id: 0,
            key_offset: 0,
            key_flags: STA_KEY_FLG_CCM,
            key: [0; 32],
            rx_security_seq: [0; 16],
        };
        pairwise.key[..16].copy_from_slice(&ptk);

        let mut group = AddStaKeyCmd {
            sta_id: 0,
            key_offset: 1,
            key_flags: STA_KEY_FLG_CCM
                | STA_KEY_MULTICAST
                | ((gtk_key_index as u16 & 0x03) << STA_KEY_FLG_KEYID_POS),
            key: [0; 32],
            rx_security_seq: [0; 16],
        };
        group.key[..16].copy_from_slice(&gtk);

        let pairwise_bytes = unsafe { super::as_bytes(&pairwise) };
        let group_bytes = unsafe { super::as_bytes(&group) };

        self.send_hcmd(
            LegacyCmd::AddStaKey as u8,
            GroupId::Legacy as u8,
            pairwise_bytes,
        )?;
        self.send_hcmd(
            LegacyCmd::AddStaKey as u8,
            GroupId::Legacy as u8,
            group_bytes,
        )?;
        Ok(self.tx_head)
    }

    pub(super) fn send_hcmd(
        &mut self,
        opcode: u8,
        group: u8,
        data: &[u8],
    ) -> Result<(), crate::DriverError> {
        let header_len = if group == GroupId::Legacy as u8 {
            core::mem::size_of::<HcmdHeader>()
        } else {
            core::mem::size_of::<HcmdHeaderWide>()
        };
        let total_len = header_len
            .checked_add(data.len())
            .ok_or(crate::DriverError::InvalidArgument)?;
        if total_len > MAX_FRAME_SIZE || total_len > TFD_LENGTH_MAX {
            return Err(crate::DriverError::InvalidArgument);
        }

        // Once firmware has reported alive, retraining the PCIe link in the
        // generic health path is unsafe: the 7265 can legitimately report a
        // transient link-down state while its command scheduler is running,
        // and retraining at that point resets/disconnects the endpoint.  Keep
        // the strong recovery check for pre-firmware access, but use the
        // lock-free vendor-presence check for live firmware MMIO cycles.
        let present = if matches!(self.fw_state, FwState::Alive | FwState::Ready) {
            true
        } else {
            self.health.pre_mmio_access().is_ok()
        };
        if !present {
            return Err(crate::DriverError::DeviceNotFound);
        }
        self.wake_for_hcmd()?;
        let sequence = ((IWL_CMD_QUEUE as u16) << 8) | (self.tx_head as u16 & 0xff);

        let used = self.tx_head.wrapping_sub(self.tx_tail);
        if used >= TX_QUEUE_SIZE {
            self.release_mac_access();
            return Err(crate::DriverError::Busy);
        }
        let desc_idx = self.tx_head % TX_QUEUE_SIZE;
        let desc_ptr = self.tx_dma_ring.virt() as *mut TxDmaDesc;
        let cmd_buf = &mut self.tx_bufs[desc_idx];
        let mut full_data = alloc::vec::Vec::with_capacity(total_len);
        if group == GroupId::Legacy as u8 {
            let hcmd_header = HcmdHeader {
                opcode,
                group_id: group,
                sequence,
            };
            full_data.extend_from_slice(unsafe { super::as_bytes(&hcmd_header) });
        } else {
            let hcmd_header = HcmdHeaderWide {
                opcode,
                group_id: group,
                sequence,
                length: data.len() as u16,
                reserved: 0,
                version: 0,
            };
            full_data.extend_from_slice(unsafe { super::as_bytes(&hcmd_header) });
        }
        full_data.extend_from_slice(data);
        cmd_buf.write_from(&full_data);

        let dma_addr = cmd_buf.dma_iova();
        let desc = unsafe { &mut *desc_ptr.add(desc_idx) };
        *desc = TxDmaDesc::zeroed();
        desc.num_tbs = 1;
        desc.tbs[0].addr_lo = dma_addr as u32;
        let hi_n_len = ((total_len as u16) << 4) | ((dma_addr >> 32) as u16 & 0x0f);
        desc.tbs[0].hi_n_len = hi_n_len;
        mmio::cache_flush(desc as *const TxDmaDesc as usize);

        self.tx_head = self.tx_head.wrapping_add(1);
        mmio::write_barrier();
        let tfd_dma =
            self.tx_dma_ring.dma_iova() + (desc_idx * core::mem::size_of::<TxDmaDesc>()) as u64;
        let _tfd_num_tbs = desc.num_tbs;
        let _tfd_tb_addr_lo =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[0].addr_lo)) };
        let _tfd_tb_hi_n_len =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[0].hi_n_len)) };
        unsafe {
            core::ptr::write_volatile(
                self.mmio.add(HBUS_TARG_WRPTR as usize),
                (self.tx_head as u32 & 0xff) | (IWL_CMD_QUEUE << 8),
            );
        }
        mmio::write_barrier();
        if opcode == LegacyCmd::ScanRequest as u8 {
            log::info!(
                "iwlwifi: scan hcmd.submit q={} slot={} opcode=0x{:02x} group=0x{:02x} header={} payload={} total={} buf_dma={:#018x} tfd_dma={:#018x} wrptr={}",
                IWL_CMD_QUEUE,
                desc_idx,
                opcode,
                group,
                header_len,
                data.len(),
                total_len,
                dma_addr,
                tfd_dma,
                self.tx_head & 0xff,
            );
            let fifo = SCD_QUEUE_STTS_FIFO_COMMAND;
            log::info!(
                "iwlwifi: scan hcmd wire prefix={} fifo={} cfg={:#010x} credit={:#010x} buf_sts={:#010x}",
                HexBytes(&full_data[..core::cmp::min(16, full_data.len())]),
                fifo,
                self.safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + fifo * (0x20 / 4))
                    .unwrap_or(!0),
                self.safe_read32(FH_TCSR_CHNL_TX_CREDIT_BASE + fifo * (0x20 / 4))
                    .unwrap_or(!0),
                self.safe_read32(FH_TCSR_CHNL_TX_BUF_STS_BASE + fifo * (0x20 / 4))
                    .unwrap_or(!0),
            );
        } else {
            log::debug!(
                "iwlwifi: hcmd.submit q={} slot={} opcode=0x{:02x} group=0x{:02x} header={} payload={} total={} wrptr={}",
                IWL_CMD_QUEUE,
                desc_idx,
                opcode,
                group,
                header_len,
                data.len(),
                total_len,
                self.tx_head & 0xff,
            );
        }
        self.release_mac_access();
        Ok(())
    }

    /// Submit an initialization command and wait until the firmware command
    /// scheduler consumes it.  Linux sends these setup commands
    /// synchronously; queueing the whole burst lets firmware API 17 report a
    /// generic SW_ERR at the next command boundary, without identifying the
    /// command that was actually rejected.
    fn send_init_hcmd(
        &mut self,
        label: &str,
        opcode: u8,
        group: u8,
        data: &[u8],
    ) -> Result<(), crate::DriverError> {
        log::debug!(
            "iwlwifi: init.hcmd.begin name={} opcode=0x{:02x} group=0x{:02x} payload={} head={} tail={} used={}",
            label,
            opcode,
            group,
            data.len(),
            self.tx_head,
            self.tx_tail,
            self.tx_head.wrapping_sub(self.tx_tail),
        );
        if let Err(error) = self.send_hcmd(opcode, group, data) {
            log::error!(
                "iwlwifi: init.hcmd.error name={} stage=submit error={} head={} tail={}",
                label,
                error,
                self.tx_head,
                self.tx_tail,
            );
            return Err(error);
        }
        let target = self.tx_head as u32 & 0xff;
        let consumed = crate::timing::poll_timeout_us(100_000, || {
            let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
            // A live 7265 normally never returns all ones from CSR_INT. It
            // means the PCIe endpoint has gone away, not that firmware
            // raised a valid HW_ERR bit. Classify it separately so the error
            // path does not pretend that the firmware error-table pointers
            // are meaningful.
            if csr_int == !0 {
                return Some(Err(crate::DriverError::DeviceNotFound));
            }
            if csr_int & (CSR_INT_BIT_SW_ERR | CSR_INT_BIT_HW_ERR) != 0 {
                return Some(Err(crate::DriverError::Protocol));
            }
            let rptr = self.read_prph(SCD_QUEUE_RDPTR_CMD)? & 0xff;
            self.update_tx_tail(rptr as usize);
            self.tx_tail_reached(self.tx_head).then_some(Ok(()))
        });
        match consumed {
            Some(Ok(())) => {
                self.release_mac_access();
                log::debug!(
                    "iwlwifi: init.hcmd.ok name={} target={} rptr={} head={} tail={}",
                    label,
                    target,
                    self.tx_tail & 0xff,
                    self.tx_head,
                    self.tx_tail,
                );
                Ok(())
            }
            Some(Err(error)) => {
                let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
                let rptr = self.read_prph(SCD_QUEUE_RDPTR_CMD).unwrap_or(!0);
                let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
                let device_gone = csr_int == !0 || rptr == !0 || fh_int == !0;
                let reason = if device_gone {
                    "PCIe_DEVICE_GONE"
                } else if csr_int & CSR_INT_BIT_HW_ERR != 0 {
                    "HW_ERR"
                } else if csr_int & CSR_INT_BIT_SW_ERR != 0 {
                    "SW_ERR"
                } else {
                    "protocol"
                };
                log::error!(
                    "iwlwifi: init.hcmd.error name={} stage=firmware reason={} consumed=false target={} rptr={:#010x} csr_int={:#010x} fh_int={:#010x} head={} tail={}",
                    label,
                    reason,
                    target,
                    rptr,
                    csr_int,
                    fh_int,
                    self.tx_head,
                    self.tx_tail,
                );
                if device_gone {
                    log::error!(
                        "iwlwifi: PCIe endpoint disappeared while waiting for init command {}; firmware error table not readable",
                        label
                    );
                } else {
                    self.log_firmware_error_table(label);
                }
                Err(error)
            }
            None => {
                self.log_init_hcmd_transport(label, target as usize);
                log::error!(
                    "iwlwifi: init.hcmd.error name={} stage=consume reason=timeout consumed=false target={} rptr={:#010x} csr_int={:#010x} fh_int={:#010x} head={} tail={}",
                    label,
                    target,
                    self.read_prph(SCD_QUEUE_RDPTR_CMD).unwrap_or(!0),
                    self.safe_read32(CSR_INT).unwrap_or(!0),
                    self.safe_read32(CSR_FH_INT).unwrap_or(!0),
                    self.tx_head,
                    self.tx_tail,
                );
                Err(crate::DriverError::Busy)
            }
        }
    }

    /// Capture the transport state that distinguishes a rejected command
    /// from a descriptor that the FH/SCD never fetched. This is intentionally
    /// read-only and is emitted only on INIT command-consume timeout.
    fn log_init_hcmd_transport(&mut self, label: &str, target: usize) {
        let fifo = SCD_QUEUE_STTS_FIFO_COMMAND;
        let slot = target.wrapping_sub(1) % TX_QUEUE_SIZE;
        let desc_ptr = self.tx_dma_ring.virt() as *const TxDmaDesc;
        let (num_tbs, addr_lo, hi_n_len) = unsafe {
            let desc = desc_ptr.add(slot);
            (
                core::ptr::read_unaligned(core::ptr::addr_of!((*desc).num_tbs)),
                core::ptr::read_unaligned(core::ptr::addr_of!((*desc).tbs[0].addr_lo)),
                core::ptr::read_unaligned(core::ptr::addr_of!((*desc).tbs[0].hi_n_len)),
            )
        };
        let mut wire = [0u8; 16];
        if slot < self.tx_bufs.len() {
            self.tx_bufs[slot].read_into(&mut wire);
        }
        log::error!(
            "iwlwifi: init.hcmd.transport name={} slot={} target={} wrptr={:#010x} scd_rptr={:#010x} scd_status={:#010x} scd_en={:#010x} scd_gp={:#010x} queuechain={:#010x} fh_cfg={:#010x} fh_credit={:#010x} fh_buf_sts={:#010x} fh_tx_status={:#010x} fh_tx_error={:#010x} tfd_num_tbs={} tfd_addr_lo={:#010x} tfd_hi_n_len={:#06x} wire={}",
            label,
            slot,
            target,
            self.safe_read32(HBUS_TARG_WRPTR).unwrap_or(!0),
            self.read_prph(SCD_QUEUE_RDPTR_CMD).unwrap_or(!0),
            self.read_prph(SCD_QUEUE_STATUS_CMD).unwrap_or(!0),
            self.read_prph(SCD_EN_CTRL).unwrap_or(!0),
            self.read_prph(SCD_GP_CTRL).unwrap_or(!0),
            self.read_prph(SCD_QUEUECHAIN_SEL).unwrap_or(!0),
            self.safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + fifo * (0x20 / 4))
                .unwrap_or(!0),
            self.safe_read32(FH_TCSR_CHNL_TX_CREDIT_BASE + fifo * (0x20 / 4))
                .unwrap_or(!0),
            self.safe_read32(FH_TCSR_CHNL_TX_BUF_STS_BASE + fifo * (0x20 / 4))
                .unwrap_or(!0),
            self.safe_read32(FH_TSSR_TX_STATUS_REG).unwrap_or(!0),
            self.safe_read32(FH_TSSR_TX_ERROR_REG).unwrap_or(!0),
            num_tbs,
            addr_lo,
            hi_n_len,
            HexBytes(&wire),
        );
    }

    /// Wait for the firmware response to a synchronous runtime setup command.
    ///
    /// Advancing the SCD read pointer only proves that the DMA engine fetched
    /// the descriptor.  Linux's synchronous command path also waits for the
    /// matching firmware notification, which is where command validation
    /// failures are reported.  Keep this separate from `send_init_hcmd`
    /// because NVM and PHY init commands have responses consumed by the
    /// scheduler-driven INIT state machine.
    fn wait_init_hcmd_response(
        &mut self,
        label: &str,
        opcode: u8,
        group: u8,
    ) -> Result<(), crate::DriverError> {
        const RESPONSE_TIMEOUT_US: u64 = 500_000;
        let deadline_tsc = match self.init_response {
            Some((pending_opcode, pending_group, deadline_tsc))
                if pending_opcode == opcode && pending_group == group =>
            {
                deadline_tsc
            }
            Some(_) => return Err(crate::DriverError::Protocol),
            None => {
                let deadline_tsc = unsafe { core::arch::x86_64::_rdtsc() }.saturating_add(
                    crate::timing::ticks_per_us().saturating_mul(RESPONSE_TIMEOUT_US),
                );
                self.init_response = Some((opcode, group, deadline_tsc));
                deadline_tsc
            }
        };

        match self.poll_init_notification(opcode, group, deadline_tsc)? {
            Some(payload) => {
                self.init_response = None;
                log::info!(
                    "iwlwifi: init.hcmd.response name={} opcode=0x{:02x} group=0x{:02x} payload={}",
                    label,
                    opcode,
                    group,
                    payload.len(),
                );
                Ok(())
            }
            None => {
                log::debug!(
                    "iwlwifi: init.hcmd.pending name={} opcode=0x{:02x} group=0x{:02x}",
                    label,
                    opcode,
                    group,
                );
                Err(crate::DriverError::Pending)
            }
        }
    }

    /// Run one scheduler-sized step of the short-lived INIT firmware sequence.
    ///
    /// Command responses are polled on later ticks.  In particular, do not
    /// spin here: a missing INIT response must not monopolize the driver
    /// scheduler while the PCI health monitor is unable to run.
    /// Shared non-unified INIT sequence for the 7000-series firmware.
    ///
    /// API-17 keeps the historical order. API-29 uses the same NVM wire
    /// format, but upstream sends the valid TX antenna mask before PHY
    /// calibration; the `api29` flag expresses that protocol distinction
    /// without changing the existing API-17 path.
    pub(super) fn send_init_firmware_commands_profile(
        &mut self,
        api29: bool,
    ) -> Result<(), crate::DriverError> {
        const NVM_SECTIONS: [u16; 8] = [0, 1, 3, 4, 5, 8, 11, 12];
        const NVM_OFFSET: u16 = 0;
        const NVM_LENGTH: u16 = 2048;
        const RESPONSE_TIMEOUT_US: u64 = 500_000;

        if !self.init_commands_started {
            log::info!(
                "iwlwifi: init.firmware_commands.begin fw_api={} fw_build={}",
                self.fw_api_ver,
                self.fw_build,
            );
            self.init_tx_cmd_queue();
            self.init_rx_dma();
            self.init_commands_started = true;
            self.init_bt_config_sent = false;
            self.init_nvm_index = 0;
            self.init_hw_section = None;
            self.init_mac_ready = false;
            self.init_response = None;
        }

        if api29 && !self.init_bt_config_sent {
            let bt_config = BtCoexConfigCmd::network_default();
            let bt_config_bytes = unsafe { super::as_bytes(&bt_config) };
            self.send_init_hcmd(
                "BT_CONFIG_INIT_API29",
                LegacyCmd::BtConfig as u8,
                GroupId::Legacy as u8,
                bt_config_bytes,
            )?;
            self.init_bt_config_sent = true;
            log::info!(
                "iwlwifi: init.api29.config name=bt_config mode={} modules={:#x}",
                BtCoexConfigCmd::BT_COEX_NW,
                BtCoexConfigCmd::BT_COEX_MPLUT_ENABLED
                    | BtCoexConfigCmd::BT_COEX_SYNC2SCO_ENABLED
                    | BtCoexConfigCmd::BT_COEX_HIGH_BAND_RET,
            );
            return Err(crate::DriverError::Pending);
        }

        if let Some((opcode, group, deadline_tsc)) = self.init_response {
            match self.poll_init_notification(opcode, group, deadline_tsc)? {
                None => return Err(crate::DriverError::Pending),
                Some(response) => {
                    self.init_response = None;
                    if opcode == LegacyCmd::NvmAccess as u8 {
                        let section = NVM_SECTIONS
                            .get(self.init_nvm_index)
                            .copied()
                            .ok_or(crate::DriverError::Protocol)?;
                        if response.len() < 8 {
                            return Err(crate::DriverError::Protocol);
                        }
                        let response_offset = u16::from_le_bytes([response[0], response[1]]);
                        let response_length =
                            u16::from_le_bytes([response[2], response[3]]) as usize;
                        let response_type = u16::from_le_bytes([response[4], response[5]]);
                        let status = u16::from_le_bytes([response[6], response[7]]);
                        log::debug!(
                            "iwlwifi: nvm.response section={} offset={} length={} type={} status={}",
                            section,
                            response_offset,
                            response_length,
                            response_type,
                            status,
                        );
                        if status != 0 || response_offset != NVM_OFFSET || response_type != section
                        {
                            if section == 0 {
                                log::error!(
                                    "iwlwifi: nvm.section section=0 failed status={} offset={} type={}",
                                    status,
                                    response_offset,
                                    response_type,
                                );
                                return Err(crate::DriverError::Protocol);
                            }
                            log::debug!(
                                "iwlwifi: nvm.section section={} unavailable status={} offset={} type={}",
                                section,
                                status,
                                response_offset,
                                response_type,
                            );
                        } else {
                            let available = response.len().saturating_sub(8);
                            let returned = core::cmp::min(response_length, available);
                            let data = response[8..8 + returned].to_vec();
                            log::debug!(
                                "iwlwifi: nvm.section section={} bytes={}",
                                section,
                                data.len(),
                            );
                            if section == 0 {
                                self.init_hw_section = Some(data);
                            }
                        }
                        self.init_nvm_index += 1;
                    } else if opcode == LegacyCmd::InitCompleteNotif as u8 {
                        self.init_firmware_completed = true;
                        self.init_commands_started = false;
                        log::info!("iwlwifi: init.firmware_commands.complete");
                        return Ok(());
                    }
                }
            }
        }

        if let Some(&section) = NVM_SECTIONS.get(self.init_nvm_index) {
            let mut command = [0u8; 8];
            command[0] = 0; // IWL_NVM_READ
            command[1] = 0; // NVM_ACCESS_TARGET_CACHE
            command[2..4].copy_from_slice(&section.to_le_bytes());
            command[4..6].copy_from_slice(&NVM_OFFSET.to_le_bytes());
            command[6..8].copy_from_slice(&NVM_LENGTH.to_le_bytes());
            self.send_init_hcmd(
                "NVM_ACCESS",
                LegacyCmd::NvmAccess as u8,
                GroupId::Legacy as u8,
                &command,
            )?;
            self.init_response = Some((
                LegacyCmd::NvmAccess as u8,
                GroupId::Legacy as u8,
                unsafe { core::arch::x86_64::_rdtsc() }.saturating_add(
                    crate::timing::ticks_per_us().saturating_mul(RESPONSE_TIMEOUT_US),
                ),
            ));
            return Err(crate::DriverError::Pending);
        }

        if !self.init_mac_ready {
            let hw = self
                .init_hw_section
                .take()
                .ok_or(crate::DriverError::Protocol)?;
            const HW_ADDR_OFFSET: usize = 0x15 * 2;
            if hw.len() < HW_ADDR_OFFSET + 6 {
                return Err(crate::DriverError::Protocol);
            }
            let mac = [
                hw[HW_ADDR_OFFSET + 1],
                hw[HW_ADDR_OFFSET],
                hw[HW_ADDR_OFFSET + 3],
                hw[HW_ADDR_OFFSET + 2],
                hw[HW_ADDR_OFFSET + 5],
                hw[HW_ADDR_OFFSET + 4],
            ];
            if mac == [0; 6] || mac == [0xff; 6] || (mac[0] & 1) != 0 {
                log::error!(
                    "iwlwifi: NVM returned invalid MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    mac[0],
                    mac[1],
                    mac[2],
                    mac[3],
                    mac[4],
                    mac[5],
                );
                return Err(crate::DriverError::Protocol);
            }
            self.mac = mac;
            self.init_mac_ready = true;
            log::info!(
                "iwlwifi: nvm.mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0],
                mac[1],
                mac[2],
                mac[3],
                mac[4],
                mac[5],
            );
        }

        if self.phy_config == 0 {
            log::error!(
                "iwlwifi: init.firmware_commands name=phy_configuration status=missing_firmware_phy_sku tlv_len={:?}",
                self.phy_sku_tlv_len
            );
            return Err(crate::DriverError::Protocol);
        }
        if api29 {
            // Linux's non-unified 7000-series path sends TX antenna
            // configuration before triggering INIT PHY calibration. Keep
            // this in the API-29 branch; the established API-17 sequence is
            // intentionally left byte-for-byte ordered as before.
            let ant_cfg: [u8; 4] = [0x03, 0, 0, 0];
            self.send_init_hcmd(
                "TX_ANT_CONFIG_INIT_API29",
                LegacyCmd::TxAntConfig as u8,
                GroupId::Legacy as u8,
                &ant_cfg,
            )?;
            log::info!(
                "iwlwifi: init.api29.config name=tx_antenna mask=0x{:02x}",
                ant_cfg[0]
            );
        }
        let phy_config = PhyConfigurationCmd {
            phy_config: self.phy_config,
            calib_flow_trigger: self.runtime_calib_flow,
            calib_event_trigger: self.runtime_calib_event,
        };
        let phy_bytes = unsafe { super::as_bytes(&phy_config) };
        self.send_init_hcmd(
            "PHY_CONFIGURATION_INIT",
            LegacyCmd::PhyConfiguration as u8,
            GroupId::Legacy as u8,
            phy_bytes,
        )?;
        self.init_response = Some((
            LegacyCmd::InitCompleteNotif as u8,
            GroupId::Legacy as u8,
            unsafe { core::arch::x86_64::_rdtsc() }
                .saturating_add(crate::timing::ticks_per_us().saturating_mul(RESPONSE_TIMEOUT_US)),
        ));
        Err(crate::DriverError::Pending)
    }

    /// Send MCC_UPDATE after MAC_CONTEXT has been accepted and wait for its
    /// firmware response before submitting SCAN_CONFIG. Linux treats MCC as a
    /// synchronous command; descriptor consumption alone is not sufficient.
    fn send_runtime_mcc(&mut self) -> Result<(), crate::DriverError> {
        // Linux checks the firmware LAR capability before sending this
        // command. A non-LAR firmware must proceed directly to scan setup;
        // waiting for an MCC response from it can only end in a timeout.
        if !self.fw_lar_supported {
            log::info!(
                "iwlwifi: init.config name=mcc_update status=skipped reason=lar_unsupported"
            );
            return Ok(());
        }

        let mcc = u16::from_be_bytes(*b"ZZ");
        let (wire_version, payload_len) = if self.fw_lar_v2 {
            let command = MccUpdateCmdV2 {
                mcc,
                source_id: 0,
                reserved: 0,
                key: 0,
                reserved2: [0; 20],
            };
            let bytes = unsafe { super::as_bytes(&command) };
            self.send_init_hcmd(
                "MCC_UPDATE",
                LegacyCmd::MccUpdate as u8,
                GroupId::Legacy as u8,
                bytes,
            )?;
            (2, bytes.len())
        } else {
            let command = MccUpdateCmdV1 {
                mcc,
                source_id: 0,
                reserved: 0,
            };
            let bytes = unsafe { super::as_bytes(&command) };
            self.send_init_hcmd(
                "MCC_UPDATE",
                LegacyCmd::MccUpdate as u8,
                GroupId::Legacy as u8,
                bytes,
            )?;
            (1, bytes.len())
        };
        log::info!(
            "iwlwifi: init.config name=mcc_update country=ZZ source=old_fw lar_wire_version={} payload={}",
            wire_version,
            payload_len,
        );
        // MCC_UPDATE_CMD is listed in Linux's always-long response table even
        // though the request uses the legacy command group. RX matching also
        // accepts the legacy namespace for older 7000-series firmware.
        self.wait_init_hcmd_response(
            "MCC_UPDATE",
            LegacyCmd::MccUpdate as u8,
            GroupId::Long as u8,
        )
    }

    /// Send the LMAC scan configuration after MCC_UPDATE completed.
    fn send_runtime_scan_config(&mut self) -> Result<(), crate::DriverError> {
        const AUX_STA_ID: u8 = 1;
        // Linux only sends SCAN_CFG_CMD when the firmware advertises UMAC
        // scan support. The 7265D-27/29 images expose LMAC scan only, so
        // their SCAN_OFFLOAD_REQUEST_CMD already carries the channel list
        // and this command must be omitted.
        if !self.fw_umac_scan_supported {
            log::info!(
                "iwlwifi: init.config name=scan_config status=skipped reason=umac_scan_unsupported"
            );
            return Ok(());
        }
        // SCAN_CFG_CMD configures the LMAC scan engine with channel lists,
        // rates, and dwell times. It is a long-group command with opcode 0x0c.
        let scan_cfg = ScanConfigV1::new(self.mac, AUX_STA_ID);
        let scan_cfg_bytes = unsafe { super::as_bytes(&scan_cfg) };
        self.send_init_hcmd(
            "SCAN_CONFIG",
            LegacyCmd::ScanConfig as u8,
            GroupId::Long as u8,
            scan_cfg_bytes,
        )?;
        log::info!(
            "iwlwifi: init.config name=scan_config opcode=0x{:02x} group=0x{:02x} channels={} payload={}",
            LegacyCmd::ScanConfig as u8,
            GroupId::Long as u8,
            SCAN_CONFIG_CHANNEL_COUNT,
            scan_cfg_bytes.len(),
        );
        // Linux sends SCAN_CFG_CMD synchronously. Do not expose the device as
        // scan-ready until the firmware has accepted this configuration;
        // otherwise its REPLY_ERROR arrives later, mixed with the first scan
        // request, and the original failure is obscured.
        self.wait_init_hcmd_response(
            "SCAN_CONFIG",
            LegacyCmd::ScanConfig as u8,
            GroupId::Long as u8,
        )
    }

    /// Existing API-17 runtime command sequence. Kept as a separately named
    /// entry point so the API-29 dispatcher cannot accidentally fall through
    /// firmware selection while the 7000-series legacy wire format
    /// remains shared.
    pub(super) fn send_init_commands_legacy(&mut self) -> Result<(), crate::DriverError> {
        if self.runtime_commands_started {
            match self.init_response {
                Some((opcode, group, deadline_tsc)) => {
                    match self.poll_init_notification(opcode, group, deadline_tsc)? {
                        None => return Err(crate::DriverError::Pending),
                        Some(payload) => {
                            self.init_response = None;
                            log::info!(
                                "iwlwifi: init.hcmd.response opcode=0x{:02x} group=0x{:02x} payload={}",
                                opcode,
                                group,
                                payload.len(),
                            );
                            match opcode {
                                x if x == LegacyCmd::MacContext as u8 => {
                                    self.send_runtime_mcc()?;
                                    self.send_runtime_scan_config()?;
                                }
                                x if x == LegacyCmd::MccUpdate as u8 => {
                                    self.send_runtime_scan_config()?;
                                }
                                x if x == LegacyCmd::ScanConfig as u8 => {}
                                _ => return Err(crate::DriverError::Protocol),
                            }
                        }
                    }
                }
                None => return Err(crate::DriverError::Protocol),
            }
            self.runtime_commands_started = false;
            self.fw_state = FwState::Ready;
            log::info!("iwlwifi: init.commands.result status=operational");
            return Ok(());
        }
        self.runtime_commands_started = true;
        log::info!(
            "iwlwifi: init.commands.begin fw_api={} fw_build={} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.fw_api_ver,
            self.fw_build,
            self.mac[0],
            self.mac[1],
            self.mac[2],
            self.mac[3],
            self.mac[4],
            self.mac[5],
        );
        self.init_tx_cmd_queue();
        self.init_rx_dma();

        // The 7265 modules used here expose two RF chains. Keep the valid TX
        // mask aligned with the two-chain RX scan selection.
        let ant_cfg: [u8; 4] = [0x03, 0, 0, 0];
        self.send_init_hcmd(
            "TX_ANT_CONFIG",
            LegacyCmd::TxAntConfig as u8,
            GroupId::Legacy as u8,
            &ant_cfg,
        )?;
        log::info!(
            "iwlwifi: init.config name=tx_antenna mask=0x{:02x}",
            ant_cfg[0]
        );

        self.send_runtime_phy_db()?;

        // Linux configures the runtime PHY before creating any MAC context.
        // API-v17 supplies the PHY SKU and calibration triggers in the
        // firmware TLVs parsed during upload; omitting this command leaves
        // MAC_CONTEXT rejected even though the transport is healthy.
        if self.phy_config == 0 {
            log::error!(
                "iwlwifi: init.config name=phy_configuration status=missing_firmware_phy_sku tlv_len={:?}",
                self.phy_sku_tlv_len
            );
            return Err(crate::DriverError::Protocol);
        }
        let phy_config = PhyConfigurationCmd {
            phy_config: self.phy_config,
            calib_flow_trigger: self.runtime_calib_flow,
            calib_event_trigger: self.runtime_calib_event,
        };
        let phy_config_bytes = unsafe { super::as_bytes(&phy_config) };
        self.send_init_hcmd(
            "PHY_CONFIGURATION",
            LegacyCmd::PhyConfiguration as u8,
            GroupId::Legacy as u8,
            phy_config_bytes,
        )?;
        log::info!(
            "iwlwifi: init.config name=phy_configuration phy_config={:#010x} calib_flow={:#010x} calib_event={:#010x} payload={}",
            self.phy_config,
            self.runtime_calib_flow,
            self.runtime_calib_event,
            phy_config_bytes.len(),
        );
        // Firmware API 17 uses the pre-v12 station API. The scan engine
        // requires its auxiliary station before accepting an offload request.
        // In non-DQA mode Linux allocates the AUX station first, then sends
        // SCD_QUEUE_CFG naming that station, and only then sends ADD_STA.
        // The transport registers above configure DMA; this command tells
        // firmware that q11 is enabled for the already allocated station.
        const MAC_INDEX_AUX: u8 = 4;
        const AUX_STA_ID: u8 = 1;
        let aux_scd = ScdTxqCfgCmdV1::aux(AUX_STA_ID);
        let aux_scd_bytes = unsafe { super::as_bytes(&aux_scd) };
        self.send_init_hcmd(
            "SCD_QUEUE_CFG_AUX",
            LegacyCmd::ScdQueueCfg as u8,
            GroupId::Legacy as u8,
            aux_scd_bytes,
        )?;
        log::info!(
            "iwlwifi: init.config name=aux_queue queue={} owner_sta={} fifo=mcast action=enable",
            IWL_AUX_QUEUE,
            AUX_STA_ID,
        );

        // ADD_STA is a legacy-group command and uses the four-byte header.
        // In Linux's non-DQA path the scheduler queue is initially owned by
        // the auxiliary station; ADD_STA publishes the same station ID and
        // queue mask to firmware.
        let aux_sta = AddStaCmdV7::aux(MAC_INDEX_AUX, AUX_STA_ID);
        let aux_sta_bytes = unsafe { super::as_bytes(&aux_sta) };
        self.send_init_hcmd(
            "ADD_STA_AUX",
            LegacyCmd::AddSta as u8,
            GroupId::Legacy as u8,
            aux_sta_bytes,
        )?;
        log::info!(
            "iwlwifi: init.config name=aux_scan_station sta_id={} group=0x{:02x} opcode=0x{:02x}",
            AUX_STA_ID,
            GroupId::Legacy as u8,
            LegacyCmd::AddSta as u8,
        );

        // API v1 uses the compact four-byte channel description.  This
        // minimal driver only binds one 2.4 GHz station/scan context.  The
        // 7265 firmware accepts PHY context 0 here but leaves the command
        // scheduler stopped when unused contexts 1/2 are added during the
        // same startup burst; those contexts can be created later when a
        // second interface actually needs them.
        let phy_id = 0u8;
        let phy = PhyContextCmdV1::add(phy_id);
        let phy_bytes = unsafe { super::as_bytes(&phy) };
        self.send_init_hcmd(
            "PHY_CONTEXT",
            LegacyCmd::PhyContext as u8,
            GroupId::Legacy as u8,
            phy_bytes,
        )?;
        log::info!(
            "iwlwifi: init.config name=phy_context action=add id={} opcode=0x{:02x} payload={}",
            phy_id,
            LegacyCmd::PhyContext as u8,
            core::mem::size_of::<PhyContextCmdV1>(),
        );

        // MAC_CONTEXT_CMD: without this the firmware never delivers 802.11
        // frames (beacons, probe responses) to the host.  Scan-complete
        // notifications still arrive (they are command responses) but
        // beacons travel the REPLY_RX_MPDU_CMD data path which requires an
        // active MAC context.  This was the root cause of "scan complete
        // with 0 APs" — the scan ran, beacons were received by the radio,
        // but the firmware dropped them because no MAC context existed.
        let mac_ctx = MacContextCmd::sta(self.mac);
        let mac_ctx_bytes = unsafe { super::as_bytes(&mac_ctx) };
        const _: () = assert!(core::mem::size_of::<MacContextCmd>() == 148);
        log::info!(
            "iwlwifi: init.config name=mac_context action=add mac_type=bss_sta(5) id_color=0 filter=0x44"
        );
        log::info!(
            "iwlwifi: init.config name=mac_context rates_cck=0x0000000f rates_ofdm=0x00000015"
        );
        for (index, ac) in (0..5).map(|index| (index, mac_ctx.qos_ac(index))) {
            let (cw_min, cw_max, aifsn, fifo, txop) = ac.values();
            log::info!(
                "iwlwifi: init.config name=mac_context ac{}=cw_min:{},cw_max:{},aifsn:{},fifo:0x{:02x},txop:{}",
                index,
                cw_min,
                cw_max,
                aifsn,
                fifo,
                txop,
            );
        }
        log::info!(
            "iwlwifi: init.config name=mac_context sta=unassociated beacon_interval:100 dtim_interval:0 listen_interval:10"
        );
        // Keep each raw-payload record short enough for serial consoles and
        // old log readers. The region labels make the API-v1 layout visible
        // without requiring a separate decoder.
        for (region, region_offset, region_bytes) in [
            ("common", 0usize, &mac_ctx_bytes[0..60]),
            ("qos_ac", 60usize, &mac_ctx_bytes[60..100]),
            ("sta", 100usize, &mac_ctx_bytes[100..148]),
        ] {
            for (chunk_index, chunk) in region_bytes.chunks(16).enumerate() {
                log::info!(
                    "iwlwifi: init.payload name=mac_context region={} offset={} chunk={} len={} hex={}",
                    region,
                    region_offset + chunk_index * 16,
                    chunk_index,
                    chunk.len(),
                    HexBytes(chunk),
                );
            }
        }
        self.send_init_hcmd(
            "MAC_CONTEXT",
            LegacyCmd::MacContext as u8,
            GroupId::Legacy as u8,
            mac_ctx_bytes,
        )?;
        self.wait_init_hcmd_response(
            "MAC_CONTEXT",
            LegacyCmd::MacContext as u8,
            GroupId::Legacy as u8,
        )?;
        self.send_runtime_mcc()?;
        self.send_runtime_scan_config()?;
        log::info!(
            "iwlwifi: init.config name=mac_context status=accepted mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} filter=0x{:08x} payload={}",
            self.mac[0],
            self.mac[1],
            self.mac[2],
            self.mac[3],
            self.mac[4],
            self.mac[5],
            (1u32 << 2) | (1u32 << 6),
            core::mem::size_of::<MacContextCmd>(),
        );

        let csr_int_before_echo = self.safe_read32(CSR_INT).unwrap_or(!0);
        let csr_fh_int_before_echo = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
        log::info!(
            "iwlwifi: command transport before optional echo probe: CSR_INT={:#010x} FH_INT={:#010x} UCODE_GP1={:#010x} GP_DRIVER={:#010x} RESET={:#010x} GP_CNTRL={:#010x} SCD_RDPTR={} SCD_STATUS={:#010x}",
            csr_int_before_echo,
            csr_fh_int_before_echo,
            self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0),
            self.safe_read32(CSR_GP_DRIVER).unwrap_or(!0),
            self.safe_read32(CSR_RESET).unwrap_or(!0),
            self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0),
            self.read_prph(SCD_QUEUE_RDPTR_CMD).unwrap_or(!0),
            self.read_prph(SCD_QUEUE_STATUS_CMD).unwrap_or(!0),
        );
        if csr_int_before_echo & CSR_INT_BIT_SW_ERR != 0 {
            log::error!(
                "iwlwifi: firmware SW_ERR is latched after init HCMD submissions (MAC_CONTEXT or an earlier command was rejected)"
            );
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), csr_int_before_echo);
            }
            self.fw_state = FwState::Error;
            return Err(crate::DriverError::Protocol);
        }
        if csr_int_before_echo & CSR_INT_BIT_HW_ERR != 0 {
            log::error!(
                "iwlwifi: HW_ERR was already set before ECHO probe; initial HCMD submissions triggered the failure"
            );
            self.fw_state = FwState::Error;
            return Err(crate::DriverError::Protocol);
        }

        // ECHO is a valid legacy command in the general firmware API, but on
        // this 7265-17 target it causes CSR_INT_BIT_HW_ERR during the init
        // sequence. Do not issue this diagnostic probe in the production init
        // path; the scan command is the useful end-to-end transport test.
        log::info!("iwlwifi: init.commands.transport_echo status=skipped");

        self.fw_state = FwState::Ready;
        log::info!("iwlwifi: init.commands.result status=operational");
        Ok(())
    }

    /// Dispatch INIT commands by the parsed firmware API.
    pub fn send_init_firmware_commands(&mut self) -> Result<(), crate::DriverError> {
        let parsed_api29 = (IWL_FW_API29_MIN..=IWL_FW_API29_MAX).contains(&self.fw_api_ver);
        if self.selected_fw_api == IWL_FW_API29_MAX {
            if !parsed_api29 {
                log::error!(
                    "iwlwifi: firmware profile mismatch selected_api=29 parsed_api={}",
                    self.fw_api_ver
                );
                return Err(crate::DriverError::Protocol);
            }
            return super::api29::send_init_firmware_commands(self);
        }
        if parsed_api29 {
            log::error!(
                "iwlwifi: API-29 firmware cannot enter API-17 command path selected_api={}",
                self.selected_fw_api
            );
            return Err(crate::DriverError::Protocol);
        }
        self.send_init_firmware_commands_profile(false)
    }

    /// Dispatch runtime commands by the parsed firmware API.
    pub fn send_init_commands(&mut self) -> Result<(), crate::DriverError> {
        let parsed_api29 = (IWL_FW_API29_MIN..=IWL_FW_API29_MAX).contains(&self.fw_api_ver);
        if self.selected_fw_api == IWL_FW_API29_MAX {
            if !parsed_api29 {
                log::error!(
                    "iwlwifi: firmware profile mismatch selected_api=29 parsed_api={}",
                    self.fw_api_ver
                );
                return Err(crate::DriverError::Protocol);
            }
            return super::api29::send_runtime_commands(self);
        }
        if parsed_api29 {
            log::error!(
                "iwlwifi: API-29 firmware cannot enter API-17 runtime path selected_api={}",
                self.selected_fw_api
            );
            return Err(crate::DriverError::Protocol);
        }
        self.send_init_commands_legacy()
    }

    /// Send a complete IPv4 packet in an 802.11 data frame with LLC/SNAP.
    ///
    /// Callers must provide the IPv4 header as well as its payload.  In
    /// particular, a bare DHCP packet must go through `send_dhcp_payload`.
    pub fn send_ip_payload(&mut self, payload: &[u8]) -> Result<(), crate::DriverError> {
        if payload.len() < 20
            || payload[0] >> 4 != 4
            || (payload[0] & 0x0f) < 5
            || (payload[0] & 0x0f) as usize * 4 > payload.len()
        {
            return Err(crate::DriverError::InvalidArgument);
        }
        let ihl = (payload[0] & 0x0f) as usize * 4;
        let total_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
        if total_len < ihl || total_len > payload.len() {
            return Err(crate::DriverError::InvalidArgument);
        }
        let protected = self.wpa_keys_installed;
        self.send_data_frame(0x0800, &payload[..total_len], protected)
    }

    /// Encapsulate a DHCP packet in IPv4/UDP/LLC and send it.
    pub fn send_dhcp_payload(&mut self, payload: &[u8]) -> Result<(), crate::DriverError> {
        let udp_len = 8usize
            .checked_add(payload.len())
            .ok_or(crate::DriverError::InvalidArgument)?;
        let ip_len = 20usize
            .checked_add(udp_len)
            .ok_or(crate::DriverError::InvalidArgument)?;
        if ip_len > u16::MAX as usize {
            return Err(crate::DriverError::InvalidArgument);
        }

        let mut packet = Vec::with_capacity(ip_len);
        packet.extend_from_slice(&[
            0x45,
            0x00, // IPv4, IHL=5, DSCP/ECN
            (ip_len >> 8) as u8,
            ip_len as u8,
            0x00,
            0x00, // identification
            0x00,
            0x00, // flags/fragment offset
            64,   // TTL
            17,   // UDP
            0x00,
            0x00, // checksum placeholder
            0x00,
            0x00,
            0x00,
            0x00, // source 0.0.0.0
            0xff,
            0xff,
            0xff,
            0xff, // destination 255.255.255.255
        ]);
        let checksum = ipv4_checksum(&packet[..20]);
        packet[10..12].copy_from_slice(&checksum.to_be_bytes());

        packet.extend_from_slice(&[
            0x00,
            0x44, // source port 68
            0x00,
            0x43, // destination port 67
            (udp_len >> 8) as u8,
            udp_len as u8,
            0x00,
            0x00, // UDP checksum is optional for IPv4 DHCP
        ]);
        packet.extend_from_slice(payload);
        let protected = self.wpa_keys_installed;
        self.send_data_frame(0x0800, &packet, protected)
    }

    /// Wrap an EAPOL-Key PDU in the 802.11 data and LLC/SNAP headers required
    /// on the air.  EAPOL itself is intentionally unprotected during the
    /// four-way handshake; only ordinary data frames require CCMP keys.
    pub(super) fn send_eapol_frame(&mut self, pdu: &[u8]) -> Result<(), crate::DriverError> {
        if pdu.len() < 4 || pdu[1] != 3 {
            return Err(crate::DriverError::InvalidArgument);
        }
        let declared_len = u16::from_be_bytes([pdu[2], pdu[3]]) as usize;
        if declared_len < 95 || 4 + declared_len > pdu.len() {
            return Err(crate::DriverError::InvalidArgument);
        }
        let frame = self.build_data_frame(0x888E, pdu, false)?;
        self.send_raw_80211_frame(&frame)
    }

    fn send_data_frame(
        &mut self,
        ether_type: u16,
        payload: &[u8],
        protected: bool,
    ) -> Result<(), crate::DriverError> {
        let frame = self.build_data_frame(ether_type, payload, protected)?;
        self.send_raw_80211_frame(&frame)
    }

    fn build_data_frame(
        &self,
        ether_type: u16,
        payload: &[u8],
        protected: bool,
    ) -> Result<Vec<u8>, crate::DriverError> {
        let bssid = self
            .wifi_conn
            .current_bssid
            .ok_or(crate::DriverError::NotReady)?;
        let frame_len = 24usize
            .checked_add(8)
            .and_then(|len| len.checked_add(payload.len()))
            .ok_or(crate::DriverError::InvalidArgument)?;
        if frame_len > MAX_FRAME_SIZE {
            return Err(crate::DriverError::InvalidArgument);
        }

        let mut frame = Vec::with_capacity(frame_len);

        // Frame control: data + ToDS.  EAPOL is unprotected; ordinary data
        // callers pass protected=true only after CCMP key activation.
        let protected_bit = if protected { 0x40 } else { 0x00 };
        frame.push(0x08);
        frame.push(0x01 | protected_bit);
        // Duration
        frame.extend_from_slice(&[0x00, 0x00]);
        // Addr1: BSSID (destination = AP)
        frame.extend_from_slice(&bssid);
        // Addr2: source (client MAC)
        frame.extend_from_slice(&self.mac);
        // Addr3: BSSID
        frame.extend_from_slice(&bssid);
        // Sequence control
        frame.extend_from_slice(&[0x00, 0x00]);

        // LLC/SNAP header.
        frame.extend_from_slice(&[
            0xAA,
            0xAA,
            0x03, // LLC header
            0x00,
            0x00,
            0x00, // SNAP OUI
            (ether_type >> 8) as u8,
            ether_type as u8,
        ]);

        // Append the IP payload
        frame.extend_from_slice(payload);

        Ok(frame)
    }

    pub fn send_raw_80211_frame(&mut self, frame: &[u8]) -> Result<(), crate::DriverError> {
        // Validate that we have a proper 802.11 frame.  EAPOL-Key PDUs must
        // already be wrapped by send_eapol_frame; bare payloads are rejected.
        if frame.len() < 2 {
            return Err(crate::DriverError::InvalidArgument);
        }
        if frame.len() > MAX_FRAME_SIZE.saturating_sub(TX_FRAME_OFFSET)
            || frame.len() + TX_FRAME_OFFSET > TFD_LENGTH_MAX
        {
            return Err(crate::DriverError::InvalidArgument);
        }

        // Identify frame type based on well-known 802.11 patterns
        let frame_control = frame[0];
        let frame_type = (frame_control & 0x0C) >> 2;

        let is_80211_management = frame.len() >= 24
            && frame_type == 0 // Management frame type
            && matches!(frame[0] & 0xFC, 0x00 | 0xB0 | 0xC0); // assoc, auth, deauth subtypes
        let is_80211_data = frame_type == 2; // Data frame type

        // Data frames must carry a valid LLC/SNAP header.  EAPOL frames
        // (ether_type 0x888E) are permitted during the 4-way handshake even
        // before CCMP keys are installed.  All other data frames require
        // the protected path to be active.
        if is_80211_data {
            if self.wpa_required && !self.wpa_keys_installed {
                let subtype = (frame_control >> 4) & 0x0F;
                let header_len = if subtype & 0x08 != 0 { 26 } else { 24 };
                let is_eapol = frame.len() >= header_len + 8
                    && frame[header_len] == 0xAA
                    && frame[header_len + 1] == 0xAA
                    && frame[header_len + 2] == 0x03
                    && frame[header_len + 6] == 0x88
                    && frame[header_len + 7] == 0x8E;
                if !is_eapol {
                    return Err(crate::DriverError::NotSupported);
                }
            }
        } else if self.wpa_required && !is_80211_management {
            return Err(crate::DriverError::NotSupported);
        }

        self.tx_queue.push_back(frame.to_vec());
        self.process_tx_queue();
        Ok(())
    }

    pub(super) fn process_tx_queue(&mut self) {
        let present = if matches!(self.fw_state, FwState::Alive | FwState::Ready) {
            true
        } else {
            self.health.pre_mmio_access().is_ok()
        };
        if !present {
            return;
        }

        while let Some(tx_frame) = self.tx_queue.front() {
            if tx_frame.len() + TX_FRAME_OFFSET > MAX_FRAME_SIZE
                || tx_frame.len() + TX_FRAME_OFFSET > TFD_LENGTH_MAX
            {
                self.tx_queue.pop_front();
                continue;
            }
            if self.tx_head.wrapping_sub(self.tx_tail) >= TX_QUEUE_SIZE {
                break;
            }

            let tx_frame = self.tx_queue.pop_front().unwrap();
            let sequence = ((IWL_CMD_QUEUE as u16) << 8) | (self.tx_head as u16 & 0xff);
            let wire = Self::build_tx_command(&tx_frame, sequence);
            let desc_idx = self.tx_head % TX_QUEUE_SIZE;
            let desc_ptr = self.tx_dma_ring.virt() as *mut TxDmaDesc;
            let buf = &mut self.tx_bufs[desc_idx];
            buf.write_from(&wire);

            let dma_addr = buf.dma_iova();
            let desc = unsafe { &mut *desc_ptr.add(desc_idx) };
            *desc = TxDmaDesc::zeroed();
            desc.num_tbs = 1;
            desc.tbs[0].addr_lo = dma_addr as u32;
            desc.tbs[0].hi_n_len = ((wire.len() as u16) << 4) | ((dma_addr >> 32) as u16 & 0x0f);
            mmio::cache_flush(desc as *const TxDmaDesc as usize);

            self.tx_head = self.tx_head.wrapping_add(1);
            mmio::write_barrier();
            unsafe {
                core::ptr::write_volatile(
                    self.mmio.add(HBUS_TARG_WRPTR as usize),
                    (self.tx_head as u32 & 0xff) | (IWL_CMD_QUEUE << 8),
                );
            }
            mmio::write_barrier();
        }
    }

    /// Build the legacy API-v6 TX command consumed by the 7265 firmware.
    ///
    /// The DMA buffer must begin with the normal four-byte command header;
    /// the fixed TX command follows it, then the 802.11 MAC frame.  Linux
    /// calls this `struct iwl_tx_cmd`. The values below mirror Linux's
    /// `iwl_mvm_set_tx_cmd()`/`iwl_mvm_set_tx_cmd_rate()` defaults for the
    /// management and non-QoS frames used by this driver.
    fn build_tx_command(frame: &[u8], sequence: u16) -> Vec<u8> {
        let mut wire = vec![0u8; TX_FRAME_OFFSET + frame.len()];

        // HcmdHeader.
        wire[0] = TX_CMD_OPCODE;
        wire[1] = GroupId::Legacy as u8;
        wire[2..4].copy_from_slice(&sequence.to_le_bytes());

        // API-v6 iwl_tx_cmd, relative to the command header.
        let tx = TX_COMMAND_HEADER_LEN;
        wire[tx..tx + 2].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        // TX_CMD_FLG_ACK | TX_CMD_FLG_SEQ_CTL | TX_CMD_FLG_BT_DIS.
        // Authentication/association are non-QoS management frames. Keep
        // the sequence-control bit enabled because the frame builders leave
        // sequence assignment to the firmware, as mac80211 does for these
        // requests. Disable BT arbitration for this low-rate 2.4 GHz
        // exchange so a SCO activity cannot defer the authentication frame.
        const TX_CMD_FLG_ACK: u32 = 1 << 3;
        const TX_CMD_FLG_BT_DIS: u32 = 1 << 12;
        const TX_CMD_FLG_SEQ_CTL: u32 = 1 << 13;
        wire[tx + 4..tx + 8].copy_from_slice(
            &(TX_CMD_FLG_ACK | TX_CMD_FLG_BT_DIS | TX_CMD_FLG_SEQ_CTL).to_le_bytes(),
        );
        wire[tx + 12..tx + 16].copy_from_slice(&TX_RATE_1M_CCK.to_le_bytes());
        // sta_id=0, no firmware-side encryption. The AP peer is registered
        // before this command is queued by connect().
        wire[tx + 16] = 0;
        wire[tx + 40..tx + 44].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        // Linux uses 60 RTS retries and 15 data retries for ordinary
        // management/data frames (3 is reserved for probe responses).
        wire[tx + 49] = 60;
        wire[tx + 50] = 15;
        // IWL_MAX_TID_COUNT marks a non-QoS management frame. PM_FRAME_ASSOC
        // is needed for association requests; authentication uses the normal
        // management timeout.
        wire[tx + 51] = 16;
        let pm_timeout = if frame.first().is_some_and(|fc| *fc & 0xfc == 0x00) {
            3u16
        } else {
            2u16
        };
        wire[tx + 52..tx + 54].copy_from_slice(&pm_timeout.to_le_bytes());

        wire[TX_FRAME_OFFSET..].copy_from_slice(frame);
        wire
    }

    /// Return whether the monotonic hardware TX tail has reached or passed the
    /// command sequence's end position.
    pub(super) fn tx_tail_reached(&self, target: usize) -> bool {
        (self.tx_tail.wrapping_sub(target) as isize) >= 0
    }

    /// Extend the hardware's ring index into the host's monotonic TX-tail
    /// counter.  This keeps queue accounting and completion checks correct
    /// across ring wraparound.
    pub(super) fn update_tx_tail(&mut self, hardware_tail: usize) {
        let hardware_tail = hardware_tail % TX_QUEUE_SIZE;
        let current_tail = self.tx_tail % TX_QUEUE_SIZE;
        let advance = (hardware_tail + TX_QUEUE_SIZE - current_tail) % TX_QUEUE_SIZE;
        let outstanding = self.tx_head.wrapping_sub(self.tx_tail);
        if advance > outstanding {
            // A backwards jump is not valid progress.  Leave the monotonic
            // counter unchanged so a reset/stale register cannot activate
            // WPA keys prematurely.
            return;
        }
        self.tx_tail = self.tx_tail.wrapping_add(advance);
    }
}

fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in header.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_v6_tx_command_has_linux_management_defaults() {
        let frame = [0xb0u8; 30];
        let wire = IwlWifiDevice::build_tx_command(&frame, 0x0918);
        let tx = TX_COMMAND_HEADER_LEN;

        assert_eq!(wire[0], TX_CMD_OPCODE);
        assert_eq!(wire[1], GroupId::Legacy as u8);
        assert_eq!(&wire[2..4], &0x0918u16.to_le_bytes());
        assert_eq!(
            u16::from_le_bytes([wire[tx], wire[tx + 1]]),
            frame.len() as u16
        );
        assert_eq!(
            u32::from_le_bytes(wire[tx + 4..tx + 8].try_into().unwrap()),
            (1 << 3) | (1 << 12) | (1 << 13)
        );
        assert_eq!(wire[tx + 16], 0); // AP station ID
        assert_eq!(wire[tx + 49], 60); // RTS retries
        assert_eq!(wire[tx + 50], 15); // ordinary management retries
        assert_eq!(wire[tx + 51], 16); // IWL_MAX_TID_COUNT / non-QoS
        assert_eq!(
            u16::from_le_bytes([wire[tx + 52], wire[tx + 53]]),
            2 // PM_FRAME_MGMT for authentication
        );
        assert_eq!(&wire[TX_FRAME_OFFSET..], &frame);
    }
}
