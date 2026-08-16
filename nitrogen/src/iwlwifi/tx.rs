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
/// Gen1 FH write-back region at the start of every TX command. Linux keeps
/// this in a dedicated 64-byte-aligned buffer and exposes exactly 20 bytes as
/// TB0; each Fullerene per-slot DMA buffer is page-aligned and can serve the
/// same purpose in place.
const IWL_FIRST_TB_SIZE: usize = 20;

// Keep the host-side SCD programming experiment available for comparison,
// but use Linux's DQA contract by default: the transport publishes CBBC/WRPTR
// and SCD_QUEUE_CFG lets firmware configure the dynamic queue.
const DQA_HOST_DIRECT_SCD_DIAGNOSTIC: bool = false;

// Linux's gen1 DQA path does not set SCD_EN_CTRL for a dynamically allocated
// data queue. The old API-29 workaround did not move q5's read pointer on the
// affected 7265D and prevented a clean upstream-equivalent A/B run. Keep the
// switch explicit so the old behavior can be re-enabled for one hardware run.
const API29_DQA_HOST_SCD_GATE_DIAGNOSTIC: bool = false;

// Legacy 7265 management/data frames use the API-v6 TX command.  A 1 Mbps
// CCK rate is valid for the 2.4 GHz management exchange used by this driver;
// the firmware command wrapper is the important part here, since placing the
// raw 802.11 frame at byte zero makes the firmware interpret 0xb0/0x00 as a
// command opcode.
const TX_RATE_1M_CCK: u32 = 10 | (1 << 9) | (1 << 14);
const TX_RATE_6M_OFDM: u32 = 13 | (1 << 8) | (1 << 14);
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

pub(super) struct HexBytes<'a>(pub(super) &'a [u8]);

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
    /// Select the lowest mandatory legacy rate for the AP's band. The
    /// previous implementation always used 1 Mbps CCK, which is illegal on
    /// 5 GHz and leaves authentication stuck after the TX_CMD is accepted.
    fn tx_rate_n_flags(&self) -> u32 {
        let channel = self
            .wifi_conn
            .current_bssid
            .as_ref()
            .and_then(|bssid| self.scan_results.iter().find(|ap| ap.bssid == *bssid))
            .map(|ap| ap.channel)
            .unwrap_or(1);
        if channel > 14 {
            TX_RATE_6M_OFDM
        } else {
            TX_RATE_1M_CCK
        }
    }

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
        self.write_mmio32(
            CSR_GP_CNTRL,
            gp | CSR_GP_CNTRL_MAC_ACCESS_REQ | CSR_GP_CNTRL_INIT_DONE,
        );
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
        self.write_mmio32(CSR_GP_CNTRL, gp & !CSR_GP_CNTRL_MAC_ACCESS_REQ);
        mmio::write_barrier();
    }

    /// Drop the host-command wake request once the command queue is empty.
    ///
    /// Linux's `cmd_hold_nic_awake` tracks host commands only.  A pending DQA
    /// data/management descriptor must not extend that hold: the 7265's
    /// scheduler is responsible for fetching q5 after its write-pointer
    /// doorbell, and keeping MAC_ACCESS_REQ asserted until q5 is reclaimed
    /// is not the upstream power-management contract.
    fn release_mac_access_if_tx_idle(&mut self) {
        if self.tx_head == self.tx_tail {
            self.release_mac_access();
        }
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

    /// Publish the inert gen1 TX foundation while the firmware CPU is reset.
    ///
    /// This matches Linux's `iwl_pcie_tx_init()`: deactivate the scheduler
    /// FIFOs, publish the keep-warm and allocated queue rings, and select the
    /// 31-queue scheduler geometry. It deliberately does not configure SCD
    /// SRAM, activate a queue/FIFO, ring a doorbell, or enable a TX DMA
    /// channel; those operations remain behind a valid ALIVE notification.
    pub(super) fn prearm_tx_foundation_before_cpu_release(
        &mut self,
    ) -> Result<(), crate::DriverError> {
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
                "iwlwifi: refusing TX foundation pre-arm after CPU release RESET={:#010x}",
                reset,
            );
            return Err(crate::DriverError::Protocol);
        }

        self.tx_head = 0;
        self.tx_tail = 0;
        self.tx_data_head = 0;
        self.tx_data_tail = 0;
        let ring_phys = self.tx_dma_ring.dma_iova();
        let command_queue = self.command_queue();
        let auxiliary_queue = self.auxiliary_queue();
        let command_ring_phys = ring_phys + tx_tfd_ring_offset(command_queue) as u64;
        let aux_ring_phys = ring_phys + tx_tfd_ring_offset(auxiliary_queue) as u64;
        let data_ring_phys = ring_phys + TX_DATA_TFD_RING_OFFSET as u64;
        let keep_warm_phys = ring_phys + TX_KEEP_WARM_OFFSET as u64;

        self.write_prph(SCD_TXFACT, 0);
        self.write_mmio32(FH_KW_MEM_ADDR_REG, (keep_warm_phys >> 4) as u32);
        for queue in 0..IWL_NUM_OF_QUEUES {
            let queue_phys = ring_phys + tx_tfd_ring_offset(queue) as u64;
            self.write_mmio32(fh_mem_cbbc_queue(queue), (queue_phys >> 8) as u32);
        }
        self.write_prph(
            SCD_GP_CTRL,
            SCD_GP_CTRL_AUTO_ACTIVE_MODE | SCD_GP_CTRL_ENABLE_31_QUEUES,
        );
        mmio::write_barrier();
        log::info!(
            "iwlwifi: firmware boot TX foundation pre-armed RESET={:#010x} kw={:#018x} cmd_q{}={:#018x} aux_q{}={:#018x} q4={:#018x}",
            reset,
            keep_warm_phys,
            command_queue,
            command_ring_phys,
            auxiliary_queue,
            aux_ring_phys,
            data_ring_phys,
        );
        Ok(())
    }

    /// Reassert the transport-wide scheduler mode after firmware ALIVE.
    ///
    /// q0 is also forced active through `SCD_EN_CTRL`; dynamically configured
    /// queues use auto-active mode during setup and are explicitly re-enabled
    /// at their final doorbell. Preserve unrelated firmware-owned bits.
    fn ensure_scd_auto_active_after_alive(&mut self) -> Result<(), crate::DriverError> {
        let scd_gp_ctrl = self
            .read_prph(SCD_GP_CTRL)
            .ok_or(crate::DriverError::DeviceNotFound)?;
        let required = SCD_GP_CTRL_AUTO_ACTIVE_MODE | SCD_GP_CTRL_ENABLE_31_QUEUES;
        self.write_prph(SCD_GP_CTRL, scd_gp_ctrl | required);
        Ok(())
    }

    /// Start the DMA rings and firmware-owned scheduler after ALIVE.
    ///
    /// The inert Linux-compatible foundation is already present. Keep SCD
    /// SRAM access, queue/FIFO activation, doorbells, and every TX DMA enable
    /// behind validation of the firmware's actual ALIVE payload.
    fn start_legacy_dma_after_alive(&mut self) -> Result<(), crate::DriverError> {
        crate::debug::print("iwlwifi", "dma.after_alive.begin");
        let allocation_phys = self.tx_dma_ring.dma_iova();
        let command_queue = self.command_queue();
        let auxiliary_queue = self.auxiliary_queue();
        let ring_phys = allocation_phys + tx_tfd_ring_offset(command_queue) as u64;
        let aux_ring_phys = allocation_phys + tx_tfd_ring_offset(auxiliary_queue) as u64;
        let keep_warm_phys = allocation_phys + TX_KEEP_WARM_OFFSET as u64;
        let scd_bc_phys = allocation_phys + TX_SCD_BC_OFFSET as u64;

        // The hardware ALIVE interrupt is only the safe boundary at which we
        // may configure the TX scheduler. RX was pre-armed while CPU reset was
        // asserted, matching Linux, so consume the actual firmware ALIVE
        // structure before touching any TX/SCD register.
        crate::debug::print("iwlwifi", "dma.after_alive.rx_ready");
        if !cfg!(test) {
            self.wait_for_alive_rx()?;
        }
        crate::debug::print("iwlwifi", "dma.after_alive.foundation_ready");

        // The command queue is FIFO mode on gen1 hardware and still needs the
        // scheduler context/active FIFO setup after firmware alive.
        self.write_prph(SCD_TXFACT, 0);
        self.write_prph(SCD_EN_CTRL, 0);
        // Linux sets these transport-wide scheduler bits during TX init.
        // They are especially important for DQA queues. The CPU/firmware reset
        // may discard the pre-ALIVE write, so safely reassert the bits now
        // that a valid ALIVE notification has established PRPH access; the
        // final DQA doorbell restores each dynamically configured queue bit.
        self.ensure_scd_auto_active_after_alive()?;
        let scd_base = self.read_prph(SCD_SRAM_BASE_ADDR);
        if let Some(scd_base) = scd_base {
            if self.alive_scd_base_addr != 0 && self.alive_scd_base_addr != scd_base {
                log::warn!(
                    "iwlwifi: ALIVE/PRPH SCD base mismatch alive={:#010x} prph={:#010x}",
                    self.alive_scd_base_addr,
                    scd_base,
                );
            }
            // Linux clears the complete SCD SRAM region before enabling any
            // queue: queue contexts, TX status entries, and the queue-to-
            // RA/TID translation table. Clearing only active queues leaves stale
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
            // Linux's 7265 (`iwl7000_base_params`) does not set
            // `scd_chain_ext_wa`, so `iwl_pcie_tx_start()` leaves this
            // firmware/HW-controlled register untouched. In particular, do
            // not unconditionally disable it: DQA q5 is configured later by
            // firmware and must see the same scheduler default as Linux.
        } else {
            log::warn!(
                "iwlwifi: unable to read SCD SRAM base; command scheduler backing table was not configured"
            );
        }
        // Match iwl_trans_pcie_txq_enable() exactly: inactive first, publish
        // WRPTR, then RDPTR/context, and only then make q9 active. The prior
        // implementation wrote WRPTR after SCD_EN_CTRL, which the updated
        // 7265D firmware does not latch as an initial queue pointer.
        self.write_prph(scd_queue_status(command_queue), 1 << 19);
        self.write_mmio32(HBUS_TARG_WRPTR, command_queue << 8);
        self.write_prph(scd_queue_rdptr(command_queue), 0);
        if let Some(scd_base) = scd_base {
            self.write_mem32(scd_base + scd_context_queue(command_queue), 0);
            self.write_mem32(
                scd_base + scd_context_queue(command_queue) + 4,
                64 | (64 << 16),
            );
        }
        self.write_prph(SCD_QUEUECHAIN_SEL, 0);
        self.write_prph(SCD_AGGR_SEL, 0);
        self.write_prph(
            scd_queue_status(command_queue),
            SCD_QUEUE_STTS_ACTIVE
                | SCD_QUEUE_STTS_WSL
                | SCD_QUEUE_STTS_FIFO_COMMAND
                | SCD_QUEUE_STTS_MASK,
        );
        // Linux iwl_pcie_tx_start() activates all scheduler FIFOs while
        // enabling the FH DMA channels and leaves TXFACT at 0xff.  The
        // corresponding deactivation belongs to tx_init/tx_stop, not to this
        // post-ALIVE start path; q0 host commands must remain fetchable.
        self.write_prph(SCD_TXFACT, 0xFF);
        // SCD_EN_CTRL is the legacy scheduler-active gate used for the
        // command queue. DQA data queues are restored at their final doorbell
        // after firmware has populated their SCD context.
        self.write_prph(SCD_EN_CTRL, 1 << command_queue);

        {
            // The FH exposes eight physical DMA channels. SCD queues are logical
            // SCD queues and select physical channels through their FIFO
            // fields; using 9/11 as TCSR channel numbers writes outside the
            // valid FH TX channel window.
            for channel in 0..FH_TCSR_CHNL_NUM {
                self.write_mmio32(
                    FH_TCSR_CHNL_TX_CONFIG_BASE + channel * (0x20 / 4),
                    FH_TCSR_TX_CONFIG_DMA_ENABLE | FH_TCSR_TX_CONFIG_DMA_CREDIT_ENABLE,
                );
            }
            if let Some(chicken) = self.safe_read32(FH_TX_CHICKEN_BITS) {
                self.write_mmio32(
                    FH_TX_CHICKEN_BITS,
                    chicken | FH_TX_CHICKEN_BITS_SCD_AUTO_RETRY_EN,
                );
            } else {
                log::warn!("iwlwifi: unable to read FH_TX_CHICKEN_BITS; leaving it unchanged");
            }

            // prepare_firmware_dma() disables L1-active while loading the
            // image.  Linux re-enables it as the final gen1 TX-start step;
            // leaving the bit set can keep the FH DMA engine from fetching
            // the command TFD after the q9 doorbell is written.
            if let Some(pcidev_state) = self.read_prph(APMG_PCIDEV_STT_REG) {
                self.write_prph(
                    APMG_PCIDEV_STT_REG,
                    pcidev_state & !APMG_PCIDEV_STT_L1_ACT_DIS,
                );
            } else {
                log::warn!(
                    "iwlwifi: unable to read APMG_PCIDEV_STT_REG; leaving L1-active state unchanged"
                );
            }
        }
        mmio::write_barrier();
        let fh_config = self
            .safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + SCD_QUEUE_STTS_FIFO_COMMAND * (0x20 / 4));
        let scd_status = self.read_prph(scd_queue_status(command_queue));
        let scd_active = self.read_prph(SCD_EN_CTRL);
        let scd_chainext = self.read_prph(SCD_CHAINEXT_EN);
        let scd_gp_ctrl = self.read_prph(SCD_GP_CTRL);
        log::info!(
            "iwlwifi: legacy TX command queue configured: cmd_q={} cmd_fifo={} cmd_tfd={:#018x} aux_q={} aux_tfd={:#018x} aux_active=false kw={:#018x} scd_bc={:#018x} fh_cmd_cfg={:#010x} scd_status={:#010x} scd_en={:#010x} scd_chainext={:#010x} scd_gp_ctrl={:#010x} scd_txfact={:#010x}",
            command_queue,
            SCD_QUEUE_STTS_FIFO_COMMAND,
            ring_phys,
            auxiliary_queue,
            aux_ring_phys,
            keep_warm_phys,
            scd_bc_phys,
            fh_config.unwrap_or(!0),
            scd_status.unwrap_or(!0),
            scd_active.unwrap_or(!0),
            scd_chainext.unwrap_or(!0),
            scd_gp_ctrl.unwrap_or(!0),
            self.read_prph(SCD_TXFACT).unwrap_or(!0),
        );
        crate::debug::print("iwlwifi", "dma.after_alive.done");
        Ok(())
    }

    /// Activate q15 immediately before firmware is told to assign it to the
    /// auxiliary scan station. Linux does not start AUX as part of transport
    /// TX start; it enables the queue on demand from the MVM setup sequence.
    fn enable_aux_tx_queue(&mut self) -> Result<(), crate::DriverError> {
        let queue = IWL_AUX_QUEUE;
        let scd_base = self
            .read_prph(SCD_SRAM_BASE_ADDR)
            .ok_or(crate::DriverError::DeviceNotFound)?;

        self.write_prph(scd_queue_status(queue), 1 << 19);
        self.write_mmio32(HBUS_TARG_WRPTR, queue << 8);
        self.write_prph(scd_queue_rdptr(queue), 0);
        self.write_mem32(scd_base + scd_context_queue(queue), 0);
        self.write_mem32(scd_base + scd_context_queue(queue) + 4, 64 | (64 << 16));

        let chain = self.read_prph(SCD_QUEUECHAIN_SEL).unwrap_or(0);
        self.write_prph(SCD_QUEUECHAIN_SEL, chain | (1 << queue));
        let aggr = self.read_prph(SCD_AGGR_SEL).unwrap_or(0);
        self.write_prph(SCD_AGGR_SEL, aggr & !(1 << queue));
        self.write_prph(
            scd_queue_status(queue),
            SCD_QUEUE_STTS_ACTIVE
                | 5 // IWL_MVM_TX_FIFO_MCAST
                | SCD_QUEUE_STTS_WSL
                | SCD_QUEUE_STTS_MASK,
        );
        mmio::write_barrier();
        log::info!(
            "iwlwifi: legacy AUX queue activated: queue={} fifo=5 tfd={:#018x}",
            queue,
            self.tx_dma_ring.dma_iova() + tx_tfd_ring_offset(queue) as u64,
        );
        Ok(())
    }

    /// Publish a DQA queue's initial write pointer. By default the firmware,
    /// not the PCIe transport, owns the dynamic SCD context/status programming
    /// through SCD_QUEUE_CFG. A host-side direct-SCD variant remains available
    /// behind `DQA_HOST_DIRECT_SCD_DIAGNOSTIC` for A/B comparison.
    pub(super) fn enable_dqa_tx_queue(&mut self, queue: u32) -> Result<(), crate::DriverError> {
        self.enable_dqa_tx_queue_with_mode(queue, DQA_HOST_DIRECT_SCD_DIAGNOSTIC)
    }

    /// Publish a DQA queue with an explicit SCD mode for the bounded
    /// authentication fallback. The normal path remains firmware-owned;
    /// `direct_scd=true` is only a controlled diagnostic alternative.
    pub(super) fn enable_dqa_tx_queue_with_mode(
        &mut self,
        queue: u32,
        direct_scd: bool,
    ) -> Result<(), crate::DriverError> {
        if !self.fw_dqa_supported || queue >= IWL_NUM_OF_QUEUES {
            return Err(crate::DriverError::InvalidArgument);
        }
        self.wake_for_hcmd()?;
        let queue_phys = self.tx_dma_ring.dma_iova() + tx_tfd_ring_offset(queue) as u64;
        let cbbc = (queue_phys >> 8) as u32;
        self.write_mmio32(fh_mem_cbbc_queue(queue), cbbc);
        self.write_mmio32(HBUS_TARG_WRPTR, queue << 8);
        if direct_scd {
            // Diagnostic alternative to Linux's DQA path: configure the SCD
            // registers directly like the non-DQA path. Keep this branch for
            // A/B comparison without making it the production experiment.
            self.write_prph(scd_queue_status(queue), 1 << 19); // inactive
            self.write_prph(scd_queue_rdptr(queue), 0);
            if let Some(scd_base) = self.read_prph(SCD_SRAM_BASE_ADDR) {
                self.write_mem32(scd_base + scd_context_queue(queue), 0);
                self.write_mem32(scd_base + scd_context_queue(queue) + 4, 64 | (64 << 16));
            }
            let chain = self.read_prph(SCD_QUEUECHAIN_SEL).unwrap_or(0);
            self.write_prph(SCD_QUEUECHAIN_SEL, chain | (1 << queue));
            let aggr = self.read_prph(SCD_AGGR_SEL).unwrap_or(0);
            self.write_prph(SCD_AGGR_SEL, aggr & !(1 << queue));
            self.write_prph(
                scd_queue_status(queue),
                SCD_QUEUE_STTS_ACTIVE
                    | 3 // IWL_MVM_TX_FIFO_VO (management)
                    | SCD_QUEUE_STTS_WSL
                    | SCD_QUEUE_STTS_MASK,
            );
            // Add the queue to SCD_EN_CTRL. Without this, the SCD does not
            // fetch TFDs even when SCD_QUEUE_STATUS is ACTIVE. q0 and q1 are
            // both in SCD_EN_CTRL and work correctly; q5 must be added too.
            let scd_en = self.read_prph(SCD_EN_CTRL).unwrap_or(0);
            self.write_prph(SCD_EN_CTRL, scd_en | (1 << queue));
        }
        mmio::write_barrier();
        log::info!(
            "iwlwifi: DQA queue published: queue={} tfd={:#018x} cbbc={:#010x} readback={:#010x} direct_scd={} scd_en={:#010x}",
            queue,
            queue_phys,
            cbbc,
            self.safe_read32(fh_mem_cbbc_queue(queue)).unwrap_or(!0),
            direct_scd,
            self.read_prph(SCD_EN_CTRL).unwrap_or(!0),
        );
        Ok(())
    }

    /// Optionally restore the old API-29 DQA scheduler gate after firmware has
    /// configured a queue through SCD_QUEUE_CFG and ADD_STA_QUEUE.
    ///
    /// Linux does not issue a second zero-pointer doorbell at this point: the
    /// first post-configuration doorbell is the TFD's actual write pointer.
    /// This is intentionally disabled for the Linux-compatible A/B experiment:
    /// upstream leaves activation of firmware-owned DQA queues to firmware.
    /// Keep the diagnostic switch separate from the doorbell so either result
    /// can be identified without changing the pointer sequence.
    pub(super) fn ensure_api29_dqa_scheduler_gate(&mut self, queue: u32) {
        if self.fw_dqa_supported && self.fw_api_ver == IWL_FW_API29_MAX {
            let before = self.read_prph(SCD_EN_CTRL).unwrap_or(!0);
            if API29_DQA_HOST_SCD_GATE_DIAGNOSTIC {
                self.write_prph(SCD_EN_CTRL, before | (1 << queue));
            }
            let after = self.read_prph(SCD_EN_CTRL).unwrap_or(!0);
            log::info!(
                "iwlwifi: API29 DQA host SCD gate queue={} enabled={} scd_en_before={:#010x} scd_en_after={:#010x} qbit={}",
                queue,
                API29_DQA_HOST_SCD_GATE_DIAGNOSTIC,
                before,
                after,
                if after & (1 << queue) != 0 {
                    "SET"
                } else {
                    "CLEAR"
                },
            );
        }
    }

    /// Abandon a stalled traffic queue before switching to another queue.
    /// This is only called after the watchdog observed no scheduler progress;
    /// clearing the queue prevents its old TFD from racing the fallback.
    /// A diagnostic run may have added the traffic queue to SCD_EN_CTRL as a
    /// local compatibility workaround, so release that extra gate here. The
    /// rest of the queue teardown follows Linux's gen1 txq_disable sequence:
    /// deactivate the queue and clear its scheduler status entry. In
    /// particular, do not ring a zero write pointer while disabling a queue;
    /// that is a new doorbell, not a teardown operation, and can race the
    /// command queue during the fallback transition.
    pub(super) fn abandon_stalled_traffic_queue(&mut self, queue: u32) {
        self.write_prph(scd_queue_status(queue), 1 << 19);
        self.write_prph(scd_queue_rdptr(queue), 0);
        if let Some(scd_en) = self.read_prph(SCD_EN_CTRL) {
            self.write_prph(SCD_EN_CTRL, scd_en & !(1 << queue));
        }
        if self.alive_scd_base_addr != 0 {
            let status = self.alive_scd_base_addr + scd_tx_stts_queue_offset(queue);
            for offset in (0..16).step_by(4) {
                self.write_mem32(status + offset, 0);
            }
        }
        let ring = self.tx_dma_ring.virt() + tx_tfd_ring_offset(queue);
        unsafe {
            core::ptr::write_bytes(ring as *mut u8, 0, TX_TFD_RING_BYTES);
        }
        mmio::cache_flush_range(ring, TX_TFD_RING_BYTES);
        self.tx_queue.clear();
        self.tx_data_head = 0;
        self.tx_data_tail = 0;
        mmio::write_barrier();
    }

    /// Return whether the currently selected authentication queue has an
    /// outstanding descriptor that the scheduler has not consumed. `None`
    /// means the queue state could not be observed; an unknown hardware state
    /// must not be treated as permission to mutate queues.
    pub(super) fn auth_tx_fetch_stalled(&mut self) -> Option<bool> {
        if self.tx_data_head == self.tx_data_tail {
            return Some(false);
        }
        self.read_prph(scd_queue_rdptr(self.traffic_queue()))
            .map(|rptr| {
                (rptr as usize & (TX_QUEUE_SIZE - 1)) == (self.tx_data_tail & (TX_QUEUE_SIZE - 1))
            })
    }

    /// Activate the static non-DQA best-effort queue used by the 7265 MVM
    /// transport. Linux configures this queue directly in the SCD before
    /// ADD_STA; SCD_QUEUE_CFG is reserved for dynamically allocated queues.
    pub(super) fn enable_data_tx_queue(&mut self) -> Result<(), crate::DriverError> {
        let scd_base = self
            .read_prph(SCD_SRAM_BASE_ADDR)
            .ok_or(crate::DriverError::DeviceNotFound)?;

        self.write_prph(SCD_QUEUE_STATUS_DATA, 1 << 19);
        self.write_mmio32(HBUS_TARG_WRPTR, IWL_DATA_QUEUE << 8);
        self.write_prph(SCD_QUEUE_RDPTR_DATA, 0);
        self.write_mem32(scd_base + SCD_CONTEXT_QUEUE_DATA, 0);
        self.write_mem32(scd_base + SCD_CONTEXT_QUEUE_DATA + 4, 64 | (64 << 16));

        let chain = self.read_prph(SCD_QUEUECHAIN_SEL).unwrap_or(0);
        self.write_prph(SCD_QUEUECHAIN_SEL, chain | (1 << IWL_DATA_QUEUE));
        let aggr = self.read_prph(SCD_AGGR_SEL).unwrap_or(0);
        self.write_prph(SCD_AGGR_SEL, aggr & !(1 << IWL_DATA_QUEUE));
        self.write_prph(
            SCD_QUEUE_STATUS_DATA,
            SCD_QUEUE_STTS_ACTIVE
                | 1 // IWL_MVM_TX_FIFO_BE
                | SCD_QUEUE_STTS_WSL
                | SCD_QUEUE_STTS_MASK,
        );
        mmio::write_barrier();
        log::info!(
            "iwlwifi: legacy data queue activated: queue={} fifo=1 tfd={:#018x}",
            IWL_DATA_QUEUE,
            self.tx_dma_ring.dma_iova() + TX_DATA_TFD_RING_OFFSET as u64,
        );
        Ok(())
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
        const STA_KEY_FLG_WEP_KEY_MAP: u16 = 1 << 3;
        const STA_KEY_FLG_KEYID_POS: u16 = 8;
        const STA_KEY_MULTICAST: u16 = 1 << 14;

        let mut pairwise = AddStaKeyCmd {
            // The AP is the first peer station in this minimal STA mode.
            sta_id: 0,
            key_offset: 0,
            key_flags: STA_KEY_FLG_CCM | STA_KEY_FLG_WEP_KEY_MAP,
            key: [0; 32],
            rx_security_seq: [0; 16],
            tkip_rx_tsc_byte2: 0,
            reserved: 0,
            tkip_rx_ttak: [0; 5],
        };
        pairwise.key[..16].copy_from_slice(&ptk);

        let mut group = AddStaKeyCmd {
            sta_id: 0,
            key_offset: 1,
            key_flags: STA_KEY_FLG_CCM
                | STA_KEY_FLG_WEP_KEY_MAP
                | STA_KEY_MULTICAST
                | ((gtk_key_index as u16 & 0x03) << STA_KEY_FLG_KEYID_POS),
            key: [0; 32],
            rx_security_seq: [0; 16],
            tkip_rx_tsc_byte2: 0,
            reserved: 0,
            tkip_rx_ttak: [0; 5],
        };
        group.key[..16].copy_from_slice(&gtk);

        let pairwise_bytes = unsafe { super::as_bytes(&pairwise) };
        let group_bytes = unsafe { super::as_bytes(&group) };

        let command_queue = self.command_queue() as u16;
        let pairwise_sequence = (command_queue << 8) | (self.tx_head as u16 & 0xff);
        let group_sequence = (command_queue << 8) | (self.tx_head.wrapping_add(1) as u16 & 0xff);
        self.wpa_key_pending_sequences = [Some(pairwise_sequence), Some(group_sequence)];

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
        let command_queue = self.command_queue();
        let sequence = ((command_queue as u16) << 8) | (self.tx_head as u16 & 0xff);

        let used = self.tx_head.wrapping_sub(self.tx_tail);
        if used >= TX_QUEUE_SIZE {
            return Err(crate::DriverError::Busy);
        }
        let desc_idx = self.tx_head % TX_QUEUE_SIZE;
        let command_ring_offset = tx_tfd_ring_offset(command_queue);
        let desc_ptr = unsafe {
            (self.tx_dma_ring.virt() as *mut u8).add(command_ring_offset) as *mut TxDmaDesc
        };
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
        let tfd_dma = self.tx_dma_ring.dma_iova()
            + command_ring_offset as u64
            + (desc_idx * core::mem::size_of::<TxDmaDesc>()) as u64;
        let _tfd_num_tbs = desc.num_tbs;
        let _tfd_tb_addr_lo =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[0].addr_lo)) };
        let _tfd_tb_hi_n_len =
            unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[0].hi_n_len)) };
        self.write_mmio32(
            HBUS_TARG_WRPTR,
            (self.tx_head as u32 & 0xff) | (command_queue << 8),
        );
        mmio::write_barrier();
        if opcode == LegacyCmd::ScanRequest as u8 {
            log::info!(
                "iwlwifi: scan hcmd.submit q={} slot={} opcode=0x{:02x} group=0x{:02x} header={} payload={} total={} buf_dma={:#018x} tfd_dma={:#018x} wrptr={}",
                command_queue,
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
                command_queue,
                desc_idx,
                opcode,
                group,
                header_len,
                data.len(),
                total_len,
                self.tx_head & 0xff,
            );
        }
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
            let rptr = self.read_prph(scd_queue_rdptr(self.command_queue()))? & 0xff;
            self.update_tx_tail(rptr as usize);
            self.tx_tail_reached(self.tx_head).then_some(Ok(()))
        });
        match consumed {
            Some(Ok(())) => {
                self.release_mac_access_if_tx_idle();
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
                let rptr = self
                    .read_prph(scd_queue_rdptr(self.command_queue()))
                    .unwrap_or(!0);
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
                self.release_mac_access();
                Err(error)
            }
            None => {
                self.log_init_hcmd_transport(label, target as usize);
                log::error!(
                    "iwlwifi: init.hcmd.error name={} stage=consume reason=timeout consumed=false target={} rptr={:#010x} csr_int={:#010x} fh_int={:#010x} head={} tail={}",
                    label,
                    target,
                    self.read_prph(scd_queue_rdptr(self.command_queue()))
                        .unwrap_or(!0),
                    self.safe_read32(CSR_INT).unwrap_or(!0),
                    self.safe_read32(CSR_FH_INT).unwrap_or(!0),
                    self.tx_head,
                    self.tx_tail,
                );
                self.release_mac_access();
                Err(crate::DriverError::Busy)
            }
        }
    }

    /// Capture the transport state that distinguishes a rejected command
    /// from a descriptor that the FH/SCD never fetched. This is intentionally
    /// read-only and is emitted only on INIT command-consume timeout.
    fn log_init_hcmd_transport(&mut self, label: &str, target: usize) {
        let fifo = SCD_QUEUE_STTS_FIFO_COMMAND;
        let command_queue = self.command_queue();
        let slot = target.wrapping_sub(1) % TX_QUEUE_SIZE;
        let desc_ptr = unsafe {
            (self.tx_dma_ring.virt() as *const u8).add(tx_tfd_ring_offset(command_queue))
                as *const TxDmaDesc
        };
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
            "iwlwifi: init.hcmd.transport name={} slot={} target={} wrptr={:#010x} gp_cntrl={:#010x} scd_rptr={:#010x} scd_status={:#010x} scd_en={:#010x} scd_gp={:#010x} queuechain={:#010x} fh_cfg={:#010x} fh_credit={:#010x} fh_buf_sts={:#010x} fh_tx_status={:#010x} fh_tx_error={:#010x} tfd_num_tbs={} tfd_addr_lo={:#010x} tfd_hi_n_len={:#06x} wire={}",
            label,
            slot,
            target,
            self.safe_read32(HBUS_TARG_WRPTR).unwrap_or(!0),
            self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0),
            self.read_prph(scd_queue_rdptr(command_queue)).unwrap_or(!0),
            self.read_prph(scd_queue_status(command_queue))
                .unwrap_or(!0),
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

    /// Submit a runtime setup command and wait until the command queue has
    /// consumed it. The AP station must be fully published on the command
    /// queue before a TX_CMD is doorbelled on its traffic queue.
    pub(super) fn send_hcmd_and_wait(
        &mut self,
        label: &str,
        opcode: u8,
        group: u8,
        data: &[u8],
    ) -> Result<(), crate::DriverError> {
        // Unit-test devices have no firmware scheduler to advance the
        // indirect SCD read pointer. Keep the wire-building replay tests
        // deterministic; hardware follows the synchronous path below.
        if cfg!(test) {
            let _ = label;
            return self.send_hcmd(opcode, group, data);
        }
        let command_queue = self.command_queue();
        let sequence = ((command_queue as u16) << 8) | (self.tx_head as u16 & 0xff);
        if matches!(
            label,
            "CONNECT_ADD_STA"
                | "CONNECT_SCD_QUEUE_CFG"
                | "CONNECT_ADD_STA_QUEUE"
                | "CONNECT_TIME_EVENT"
        ) {
            log::info!(
                "iwlwifi: hcmd.sync.submit name={} opcode=0x{:02x} group=0x{:02x} payload_len={} payload_hex={}",
                label,
                opcode,
                group,
                data.len(),
                HexBytes(data),
            );
        }
        self.send_hcmd(opcode, group, data)?;
        let target = self.tx_head;
        let consumed = crate::timing::poll_timeout_us(100_000, || {
            let rptr = self.read_prph(scd_queue_rdptr(command_queue))? as usize;
            self.update_tx_tail(rptr);
            self.tx_tail_reached(target).then_some(())
        });
        if consumed.is_none() {
            let rptr = self.read_prph(scd_queue_rdptr(command_queue)).unwrap_or(!0);
            log::error!(
                "iwlwifi: hcmd.sync.timeout name={} stage=consume opcode=0x{:02x} sequence=0x{:04x} target={} head={} tail={} rptr={:#010x}",
                label,
                opcode,
                sequence,
                target,
                self.tx_head,
                self.tx_tail,
                rptr,
            );
            self.release_mac_access();
            return Err(crate::DriverError::Busy);
        }

        // Linux synchronous host commands complete only after the matching
        // firmware response has been drained. Merely observing q9's SCD read
        // pointer allows several setup commands to accumulate behind an
        // unread response and can stall the next descriptor indefinitely.
        const RESPONSE_TIMEOUT_US: u64 = 500_000;
        let deadline_tsc = unsafe { core::arch::x86_64::_rdtsc() }
            .saturating_add(crate::timing::ticks_per_us().saturating_mul(RESPONSE_TIMEOUT_US));
        loop {
            match self.poll_init_notification(opcode, group, Some(sequence), deadline_tsc) {
                Ok(Some(payload)) => {
                    if opcode == LegacyCmd::AddSta as u8 {
                        if payload.len() < 4 {
                            log::error!(
                                "iwlwifi: hcmd.sync.error name={} stage=status reason=short_add_sta_response payload={}",
                                label,
                                payload.len(),
                            );
                            self.release_mac_access();
                            return Err(crate::DriverError::Protocol);
                        }
                        let status =
                            u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                        log::info!(
                            "iwlwifi: add_sta.response name={} status={:#010x} status_low={:#04x} payload_len={} payload_hex={}",
                            label,
                            status,
                            status & 0xff,
                            payload.len(),
                            HexBytes(&payload),
                        );
                        if status & 0xff != 1 {
                            log::error!(
                                "iwlwifi: hcmd.sync.error name={} stage=status add_sta_status={:#010x}",
                                label,
                                status,
                            );
                            self.release_mac_access();
                            return Err(crate::DriverError::Protocol);
                        }
                    }
                    if opcode == LegacyCmd::TimeEvent as u8 {
                        if payload.len() < 16 {
                            log::error!(
                                "iwlwifi: hcmd.sync.error name={} stage=status reason=short_time_event_response payload={}",
                                label,
                                payload.len(),
                            );
                            self.release_mac_access();
                            return Err(crate::DriverError::Protocol);
                        }
                        let status =
                            u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                        log::info!(
                            "iwlwifi: time_event.response status={:#010x} id={:#010x} unique_id={:#010x} id_and_color={:#010x} payload_hex={}",
                            status,
                            u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]),
                            u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]),
                            u32::from_le_bytes([
                                payload[12],
                                payload[13],
                                payload[14],
                                payload[15]
                            ]),
                            HexBytes(&payload),
                        );
                        if status & 1 == 0 {
                            log::error!(
                                "iwlwifi: hcmd.sync.error name={} stage=status time_event_status={:#010x}",
                                label,
                                status,
                            );
                            self.release_mac_access();
                            return Err(crate::DriverError::Protocol);
                        }
                    }
                    if label == "CONNECT_SCD_QUEUE_CFG" {
                        log::info!(
                            "iwlwifi: DQA queue configuration response payload={}",
                            HexBytes(&payload),
                        );
                    }
                    if matches!(
                        label,
                        "CONNECT_PHY_CONTEXT"
                            | "CONNECT_MAC_CONTEXT"
                            | "CONNECT_ADD_STA"
                            | "CONNECT_SCD_QUEUE_CFG"
                            | "CONNECT_ADD_STA_QUEUE"
                            | "CONNECT_TIME_EVENT"
                    ) {
                        log::info!(
                            "iwlwifi: hcmd.sync.response_payload name={} opcode=0x{:02x} group=0x{:02x} sequence=0x{:04x} payload_hex={}",
                            label,
                            opcode,
                            group,
                            sequence,
                            HexBytes(&payload),
                        );
                    }
                    self.release_mac_access_if_tx_idle();
                    log::info!(
                        "iwlwifi: hcmd.sync.response name={} opcode=0x{:02x} sequence=0x{:04x} payload={} target={} rptr={}",
                        label,
                        opcode,
                        sequence,
                        payload.len(),
                        target,
                        self.tx_tail & (TX_QUEUE_SIZE - 1),
                    );
                    return Ok(());
                }
                Ok(None) => core::hint::spin_loop(),
                Err(error) => {
                    log::error!(
                        "iwlwifi: hcmd.sync.error name={} stage=response opcode=0x{:02x} sequence=0x{:04x} error={}",
                        label,
                        opcode,
                        sequence,
                        error,
                    );
                    self.release_mac_access();
                    return Err(error);
                }
            }
        }
    }

    /// Dump the complete gen1 SCD/FH state relevant to a dynamic queue.
    ///
    /// DQA queue setup is firmware-owned on API 29, so a queue can look
    /// configured from the HCMD response while still being invisible to the
    /// scheduler.  Keep this read-only snapshot at each setup boundary so a
    /// single hardware run distinguishes host publication, firmware queue
    /// configuration, station ownership, and the actual TX doorbell.
    pub(super) fn log_dqa_scheduler_snapshot(&mut self, label: &str, sta_queue_mask: u32) {
        const DIAG_QUEUES: [u32; 3] = [IWL_CMD_QUEUE, IWL_DQA_AUX_QUEUE, IWL_MGMT_QUEUE];
        const DIAG_FIFOS: [u32; 3] = [3, 5, 7];

        log::info!(
            "iwlwifi: dqa.snapshot.begin label={} sta_queue_mask={:#010x} hbus_wrptr={:#010x} csr_gp={:#010x} csr_gp1={:#010x} scd_active={:#010x} scd_ait={:#010x} scd_en={:#010x} scd_gp={:#010x} scd_interrupt_mask={:#010x} qchain={:#010x} chainext={:#010x} aggr={:#010x} txfact={:#010x} scd_dram={:#010x} scd_base={:#010x} fh_chicken={:#010x} fh_tx_status={:#010x} fh_tx_error={:#010x}",
            label,
            sta_queue_mask,
            self.safe_read32(HBUS_TARG_WRPTR).unwrap_or(!0),
            self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0),
            self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0),
            self.read_prph(SCD_ACTIVE).unwrap_or(!0),
            self.read_prph(SCD_AIT).unwrap_or(!0),
            self.read_prph(SCD_EN_CTRL).unwrap_or(!0),
            self.read_prph(SCD_GP_CTRL).unwrap_or(!0),
            self.read_prph(SCD_INTERRUPT_MASK).unwrap_or(!0),
            self.read_prph(SCD_QUEUECHAIN_SEL).unwrap_or(!0),
            self.read_prph(SCD_CHAINEXT_EN).unwrap_or(!0),
            self.read_prph(SCD_AGGR_SEL).unwrap_or(!0),
            self.read_prph(SCD_TXFACT).unwrap_or(!0),
            self.read_prph(SCD_DRAM_BASE_ADDR).unwrap_or(!0),
            self.alive_scd_base_addr,
            self.safe_read32(FH_TX_CHICKEN_BITS).unwrap_or(!0),
            self.safe_read32(FH_TSSR_TX_STATUS_REG).unwrap_or(!0),
            self.safe_read32(FH_TSSR_TX_ERROR_REG).unwrap_or(!0),
        );

        for queue in DIAG_QUEUES {
            let status = self.read_prph(scd_queue_status(queue)).unwrap_or(!0);
            let wrptr = self.read_prph(scd_queue_wrptr(queue)).unwrap_or(!0);
            let rdptr = self.read_prph(scd_queue_rdptr(queue)).unwrap_or(!0);
            let cbbc = self.safe_read32(fh_mem_cbbc_queue(queue)).unwrap_or(!0);
            let (ctx0, ctx1, trans_tbl, tx_stts) = if self.alive_scd_base_addr != 0 {
                (
                    self.read_mem32(self.alive_scd_base_addr + scd_context_queue(queue))
                        .unwrap_or(!0),
                    self.read_mem32(self.alive_scd_base_addr + scd_context_queue(queue) + 4)
                        .unwrap_or(!0),
                    self.read_mem32(self.alive_scd_base_addr + scd_trans_tbl_offset_queue(queue))
                        .unwrap_or(!0),
                    self.read_mem32(self.alive_scd_base_addr + scd_tx_stts_queue_offset(queue))
                        .unwrap_or(!0),
                )
            } else {
                (!0, !0, !0, !0)
            };
            log::info!(
                "iwlwifi: dqa.snapshot.queue label={} queue={} status={:#010x} fifo={} active={} wsl={} scd_ack={} bit7={} wrptr={:#010x} rdptr={:#010x} cbbc={:#010x} ctx0={:#010x} ctx1={:#010x} trans_tbl={:#010x} tx_stts={:#010x} queue_owned={}",
                label,
                queue,
                status,
                status & 0x7,
                (status & SCD_QUEUE_STTS_ACTIVE) != 0,
                (status & SCD_QUEUE_STTS_WSL) != 0,
                (status & SCD_QUEUE_STTS_SCD_ACK) != 0,
                (status & (1 << 7)) != 0,
                wrptr,
                rdptr,
                cbbc,
                ctx0,
                ctx1,
                trans_tbl,
                tx_stts,
                (sta_queue_mask & (1 << queue)) != 0,
            );
        }

        for fifo in DIAG_FIFOS {
            let stride = fifo * (0x20 / 4);
            log::info!(
                "iwlwifi: dqa.snapshot.fh label={} fifo={} config={:#010x} credit={:#010x} buf_status={:#010x} trb={:#010x}",
                label,
                fifo,
                self.safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + stride)
                    .unwrap_or(!0),
                self.safe_read32(FH_TCSR_CHNL_TX_CREDIT_BASE + stride)
                    .unwrap_or(!0),
                self.safe_read32(FH_TCSR_CHNL_TX_BUF_STS_BASE + stride)
                    .unwrap_or(!0),
                self.safe_read32(fh_tx_trb_channel(fifo)).unwrap_or(!0),
            );
        }

        log::info!("iwlwifi: dqa.snapshot.end label={}", label);
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

        match self.poll_init_notification(opcode, group, None, deadline_tsc)? {
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
            self.start_legacy_dma_after_alive()?;
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
            match self.poll_init_notification(opcode, group, None, deadline_tsc)? {
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

    /// Bind the station MAC context to PHY context 0. Linux creates this
    /// binding after both contexts and before any peer station is added.
    fn send_runtime_binding(&mut self) -> Result<(), crate::DriverError> {
        let binding = BindingContextCmdV1::add_single(0, 0, 0);
        let bytes = unsafe { super::as_bytes(&binding) };
        self.send_init_hcmd(
            "BINDING_CONTEXT",
            LegacyCmd::BindingContext as u8,
            GroupId::Legacy as u8,
            bytes,
        )?;
        log::info!(
            "iwlwifi: init.config name=binding_context action=add binding=0 mac=0 phy=0 payload={}",
            bytes.len(),
        );
        self.wait_init_hcmd_response(
            "BINDING_CONTEXT",
            LegacyCmd::BindingContext as u8,
            GroupId::Legacy as u8,
        )
    }

    /// Send MCC_UPDATE after BINDING_CONTEXT has been accepted and wait for its
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
                    match self.poll_init_notification(opcode, group, None, deadline_tsc)? {
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
                                    self.send_runtime_binding()?;
                                    self.send_runtime_mcc()?;
                                    self.send_runtime_scan_config()?;
                                }
                                x if x == LegacyCmd::BindingContext as u8 => {
                                    if payload.len() < 4
                                        || u32::from_le_bytes([
                                            payload[0], payload[1], payload[2], payload[3],
                                        ]) != 0
                                    {
                                        log::error!(
                                            "iwlwifi: init.config name=binding_context status=invalid payload={}",
                                            payload.len(),
                                        );
                                        return Err(crate::DriverError::Protocol);
                                    }
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
        self.start_legacy_dma_after_alive()?;

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
        // API-29 advertises DQA_SUPPORT in its capability bitmap. Linux
        // switches the command queue to q0, enables DQA, then assigns q1 to
        // the auxiliary station. Older firmware retains the static path.
        if self.fw_dqa_supported {
            let dqa = DqaEnableCmdV1::linux_7000();
            self.send_init_hcmd("DQA_ENABLE", 0, GroupId::DataPath as u8, unsafe {
                super::as_bytes(&dqa)
            })?;
            log::info!(
                "iwlwifi: init.config name=dqa_enable command_queue={}",
                IWL_DQA_CMD_QUEUE,
            );
        }

        // Firmware API 17 uses the pre-v12 station API. The scan engine
        // requires its auxiliary station before accepting an offload request.
        // In non-DQA mode Linux allocates the AUX station first, then sends
        // SCD_QUEUE_CFG naming that station, and only then sends ADD_STA.
        // The transport registers above configure DMA; this command tells
        // firmware that q15 is enabled for the already allocated station.
        const MAC_INDEX_AUX: u8 = 4;
        const AUX_STA_ID: u8 = 1;
        let aux_scd = if self.fw_dqa_supported {
            self.enable_dqa_tx_queue(IWL_DQA_AUX_QUEUE)?;
            ScdTxqCfgCmdV1::dqa_aux(AUX_STA_ID)
        } else {
            self.enable_aux_tx_queue()?;
            ScdTxqCfgCmdV1::aux(AUX_STA_ID)
        };
        let aux_scd_bytes = unsafe { super::as_bytes(&aux_scd) };
        self.send_init_hcmd(
            "SCD_QUEUE_CFG_AUX",
            LegacyCmd::ScdQueueCfg as u8,
            GroupId::Legacy as u8,
            aux_scd_bytes,
        )?;
        log::info!(
            "iwlwifi: init.config name=aux_queue queue={} owner_sta={} fifo=mcast action=enable",
            self.auxiliary_queue(),
            AUX_STA_ID,
        );

        // ADD_STA is a legacy-group command and uses the four-byte header.
        // In Linux's non-DQA path the scheduler queue is initially owned by
        // the auxiliary station; ADD_STA publishes the same station ID and
        // queue mask to firmware.
        let aux_sta = if self.fw_dqa_supported {
            AddStaCmdV7::dqa_aux(MAC_INDEX_AUX, AUX_STA_ID)
        } else {
            AddStaCmdV7::aux(MAC_INDEX_AUX, AUX_STA_ID)
        };
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
            self.read_prph(scd_queue_rdptr(self.command_queue()))
                .unwrap_or(!0),
            self.read_prph(scd_queue_status(self.command_queue()))
                .unwrap_or(!0),
        );
        if csr_int_before_echo & CSR_INT_BIT_SW_ERR != 0 {
            log::error!(
                "iwlwifi: firmware SW_ERR is latched after init HCMD submissions (MAC_CONTEXT or an earlier command was rejected)"
            );
            self.write_mmio32(CSR_INT, csr_int_before_echo);
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
        &mut self,
        ether_type: u16,
        payload: &[u8],
        protected: bool,
    ) -> Result<Vec<u8>, crate::DriverError> {
        let bssid = self
            .wifi_conn
            .current_bssid
            .ok_or(crate::DriverError::NotReady)?;
        let frame_len = 24usize
            .checked_add(if protected { 8 } else { 0 })
            .and_then(|len| len.checked_add(8))
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

        if protected {
            let pn = self.tx_pn;
            self.tx_pn = pn
                .checked_add(1)
                .ok_or(crate::DriverError::InvalidArgument)?;
            // CCMP uses PN0, PN1, reserved, ExtIV/key ID, PN2..PN5.
            frame.extend_from_slice(&[
                pn as u8,
                (pn >> 8) as u8,
                0,
                0x20, // ExtIV, key index 0 (the installed PTK)
                (pn >> 16) as u8,
                (pn >> 24) as u8,
                (pn >> 32) as u8,
                (pn >> 40) as u8,
            ]);
        }

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

        if self.tx_queue.is_empty() {
            self.release_mac_access_if_tx_idle();
            return;
        }
        if self.wake_for_hcmd().is_err() {
            return;
        }

        while let Some(tx_frame) = self.tx_queue.front() {
            if tx_frame.len() + TX_FRAME_OFFSET > MAX_FRAME_SIZE
                || tx_frame.len() + TX_FRAME_OFFSET > TFD_LENGTH_MAX
            {
                self.tx_queue.pop_front();
                continue;
            }
            if self.tx_data_head.wrapping_sub(self.tx_data_tail) >= TX_QUEUE_SIZE {
                break;
            }

            let tx_frame = self.tx_queue.pop_front().unwrap();
            let traffic_queue = self.traffic_queue();
            let sequence = ((traffic_queue as u16) << 8) | (self.tx_data_head as u16 & 0xff);
            let rate_n_flags = self.tx_rate_n_flags();
            let protected = tx_frame.len() >= 2 && tx_frame[1] & 0x40 != 0;
            let ccmp_key = if protected {
                self.wpa.key_material().map(|(ptk, _, _)| ptk)
            } else {
                None
            };
            if protected && ccmp_key.is_none() {
                log::error!("iwlwifi: protected TX frame has no active pairwise CCMP key");
                continue;
            }
            let mut wire =
                Self::build_tx_command(&tx_frame, sequence, rate_n_flags, ccmp_key.as_ref());
            if tx_frame.len() >= 2 && (tx_frame[0] & 0x0c) >> 2 == 0 {
                log::info!(
                    "iwlwifi: TX management frame subtype={} rate_n_flags={:#010x} band={}",
                    (tx_frame[0] >> 4) & 0x0f,
                    rate_n_flags,
                    if rate_n_flags == TX_RATE_6M_OFDM {
                        "5GHz"
                    } else {
                        "2.4GHz"
                    },
                );
            }
            let desc_idx = self.tx_data_head % TX_QUEUE_SIZE;
            let desc_ptr = unsafe {
                (self.tx_dma_ring.virt() as *mut u8).add(tx_tfd_ring_offset(traffic_queue))
                    as *mut TxDmaDesc
            };
            let buf = &mut self.tx_bufs[TX_QUEUE_SIZE + desc_idx];
            let dma_addr = buf.dma_iova();
            // Linux points the TX command's scratch write-back address into
            // TB0. A zero address disables that contract and, together with
            // a one-TB descriptor, leaves gen1 data queues unlike the
            // transport format used by the 7265 firmware.
            let scratch_dma = dma_addr + TX_COMMAND_HEADER_LEN as u64 + 8;
            let tx = TX_COMMAND_HEADER_LEN;
            wire[tx + 44..tx + 48].copy_from_slice(&(scratch_dma as u32).to_le_bytes());
            wire[tx + 48] = ((scratch_dma >> 32) & 0x0f) as u8;
            if wire.len() > MAX_FRAME_SIZE {
                continue;
            }
            buf.write_from(&wire);
            let auth_dma_wire = if (tx_frame[0] >> 4) & 0x0f == 11 {
                Some(buf.as_slice()[..wire.len()].to_vec())
            } else {
                None
            };

            let mac_header_len = Self::tx_mac_header_len(&tx_frame);
            let tb1_unaligned = TX_FRAME_OFFSET + mac_header_len - IWL_FIRST_TB_SIZE;
            let tb1_len = (tb1_unaligned + 3) & !3;
            let tb2_len = tx_frame.len() - mac_header_len;
            let tb2_offset = IWL_FIRST_TB_SIZE + tb1_len;
            let desc = unsafe { &mut *desc_ptr.add(desc_idx) };
            *desc = TxDmaDesc::zeroed();
            desc.num_tbs = if tb2_len == 0 { 2 } else { 3 };
            desc.tbs[0].addr_lo = dma_addr as u32;
            desc.tbs[0].hi_n_len =
                ((IWL_FIRST_TB_SIZE as u16) << 4) | ((dma_addr >> 32) as u16 & 0x0f);
            let tb1_dma = dma_addr + IWL_FIRST_TB_SIZE as u64;
            desc.tbs[1].addr_lo = tb1_dma as u32;
            desc.tbs[1].hi_n_len = ((tb1_len as u16) << 4) | ((tb1_dma >> 32) as u16 & 0x0f);
            if tb2_len != 0 {
                let tb2_dma = dma_addr + tb2_offset as u64;
                desc.tbs[2].addr_lo = tb2_dma as u32;
                desc.tbs[2].hi_n_len = ((tb2_len as u16) << 4) | ((tb2_dma >> 32) as u16 & 0x0f);
            }
            mmio::cache_flush(desc as *const TxDmaDesc as usize);

            // The legacy SCD uses the byte-count table only for
            // Scheduler-ACK/aggregate queues. Linux configures the 7265
            // management queue as FIFO/non-aggregate, so Q5 must not be
            // treated as a Scheduler-ACK queue merely because a byte-count
            // table exists in the shared TX DMA allocation. Keep the table
            // writer available for aggregate data queues, but make this
            // distinction explicit for the Q5 A/B experiment.
            let sec_ctl = wire[TX_COMMAND_HEADER_LEN + 17];
            let scd_aggr = self.read_prph(SCD_AGGR_SEL).unwrap_or(!0);
            let scheduler_ack = (scd_aggr & (1 << traffic_queue)) != 0;
            let byte_count_entry = if scheduler_ack {
                self.update_scd_byte_count(
                    traffic_queue,
                    desc_idx,
                    tx_frame.len() as u16,
                    0,
                    sec_ctl,
                )
            } else {
                0
            };

            self.tx_data_head = self.tx_data_head.wrapping_add(1);
            let handshake_frame = tx_frame.len() >= 2 && matches!(tx_frame[0] & 0xfc, 0xb0 | 0x00);
            if handshake_frame {
                self.auth_tx_sequence = Some(sequence);
            }
            mmio::write_barrier();
            self.write_mmio32(
                HBUS_TARG_WRPTR,
                (self.tx_data_head as u32 & 0xff) | (traffic_queue << 8),
            );
            mmio::write_barrier();
            if tx_frame.len() >= 2 && (tx_frame[0] & 0x0c) >> 2 == 0 {
                let scd_rptr = self.read_prph(scd_queue_rdptr(traffic_queue)).unwrap_or(!0);
                let scd_wrptr = self.read_prph(scd_queue_wrptr(traffic_queue)).unwrap_or(!0);
                let scd_status = self
                    .read_prph(scd_queue_status(traffic_queue))
                    .unwrap_or(!0);
                let fifo = scd_status & 0x7;
                let scd_en = self.read_prph(SCD_EN_CTRL).unwrap_or(!0);
                let scd_gp = self.read_prph(SCD_GP_CTRL).unwrap_or(!0);
                let scd_chain = self.read_prph(SCD_QUEUECHAIN_SEL).unwrap_or(!0);
                let scd_base = self.alive_scd_base_addr;
                let (ctx0, ctx1, trans_tbl, tx_stts) = if scd_base != 0 {
                    (
                        self.read_mem32(scd_base + scd_context_queue(traffic_queue))
                            .unwrap_or(!0),
                        self.read_mem32(scd_base + scd_context_queue(traffic_queue) + 4)
                            .unwrap_or(!0),
                        self.read_mem32(scd_base + scd_trans_tbl_offset_queue(traffic_queue))
                            .unwrap_or(!0),
                        self.read_mem32(scd_base + scd_tx_stts_queue_offset(traffic_queue))
                            .unwrap_or(!0),
                    )
                } else {
                    (!0, !0, !0, !0)
                };
                let cbbc = self
                    .safe_read32(fh_mem_cbbc_queue(traffic_queue))
                    .unwrap_or(!0);
                let station_queue_mask = if self.fw_dqa_supported {
                    1u32 << traffic_queue
                } else {
                    1u32 << IWL_DATA_QUEUE
                };
                let byte_count_addr = self.tx_dma_ring.dma_iova()
                    + TX_SCD_BC_OFFSET as u64
                    + traffic_queue as u64 * (256 + 64) * 2
                    + desc_idx as u64 * 2;
                // Dump the raw TFD bytes and the DMA byte-count entry so that
                // any TFD layout mismatch with Linux is visible without a
                // hardware debugger.  The TFD is 128 bytes; only show the
                // active TB region (reserved + num_tbs + up to 3 TBs = 22 bytes)
                // plus the byte-count entry on the DMA ring.
                let tfd_ptr = desc as *const TxDmaDesc as *const u8;
                let mut tfd_hex = alloc::string::String::new();
                for i in 0..(4 + 3 * core::mem::size_of::<TxDmaTb>()) {
                    use alloc::fmt::Write;
                    let _ = write!(tfd_hex, "{:02x} ", unsafe {
                        core::ptr::read_volatile(tfd_ptr.add(i))
                    });
                }
                let bc_ptr = (self.tx_dma_ring.virt()
                    + TX_SCD_BC_OFFSET
                    + traffic_queue as usize * (256 + 64) * 2
                    + desc_idx * 2) as *const u16;
                let bc_primary = unsafe { core::ptr::read_volatile(bc_ptr) };
                let bc_dup_ptr = (self.tx_dma_ring.virt()
                    + TX_SCD_BC_OFFSET
                    + traffic_queue as usize * (256 + 64) * 2
                    + (256 + desc_idx) * 2) as *const u16;
                let bc_duplicate = unsafe { core::ptr::read_volatile(bc_dup_ptr) };
                // Linux derives the initial DQA SSN from the 802.11 header
                // before calling iwl_trans_txq_enable_cfg().  Fullerene
                // currently sends SSN=0 in SCD_QUEUE_CFG and lets the TX
                // command firmware assign the management sequence; expose
                // both values before considering an SSN change.
                let frame_seq_ctrl = if tx_frame.len() >= 24 {
                    u16::from_le_bytes([tx_frame[22], tx_frame[23]])
                } else {
                    u16::MAX
                };
                let frame_ssn = if frame_seq_ctrl == u16::MAX {
                    u16::MAX
                } else {
                    (frame_seq_ctrl >> 4) & 0x0fff
                };
                log::info!(
                    "iwlwifi: TX management submitted queue={} slot={} frame={} wire={} frame_seq_ctrl={:#06x} frame_ssn={} bc_mode={} bc_dwords={} bc_addr={:#018x} tbs={} tb0={} tb1={} tb2={} scratch={:#018x} sta_queue_mask={:#010x} cbbc={:#010x} sw_wrptr={} hw_wrptr={:#010x} rptr={:#010x} status={:#010x} fifo={} fifo_cfg={:#010x} fifo_credit={:#010x} fifo_buf={:#010x} scd_en={:#010x} scd_gp={:#010x} qchain={:#010x} aggr={:#010x} ctx0={:#010x} ctx1={:#010x} trans_tbl={:#010x} tx_stts={:#010x} scd_dram={:#010x} scd_txfact={:#010x} fh_tx_trb={:#010x} tx_status={:#010x} tx_error={:#010x} gp_cntrl={:#010x} gp1={:#010x}",
                    traffic_queue,
                    desc_idx,
                    tx_frame.len(),
                    wire.len(),
                    frame_seq_ctrl,
                    frame_ssn,
                    if scheduler_ack {
                        "scheduler-ack"
                    } else {
                        "fifo"
                    },
                    byte_count_entry & 0x0fff,
                    byte_count_addr,
                    desc.num_tbs,
                    IWL_FIRST_TB_SIZE,
                    tb1_len,
                    tb2_len,
                    scratch_dma,
                    station_queue_mask,
                    cbbc,
                    self.tx_data_head & 0xff,
                    scd_wrptr & 0xff,
                    scd_rptr,
                    scd_status,
                    fifo,
                    self.safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + fifo * (0x20 / 4))
                        .unwrap_or(!0),
                    self.safe_read32(FH_TCSR_CHNL_TX_CREDIT_BASE + fifo * (0x20 / 4))
                        .unwrap_or(!0),
                    self.safe_read32(FH_TCSR_CHNL_TX_BUF_STS_BASE + fifo * (0x20 / 4))
                        .unwrap_or(!0),
                    scd_en,
                    scd_gp,
                    scd_chain,
                    scd_aggr,
                    ctx0,
                    ctx1,
                    trans_tbl,
                    tx_stts,
                    self.read_prph(SCD_DRAM_BASE_ADDR).unwrap_or(!0),
                    self.read_prph(SCD_TXFACT).unwrap_or(!0),
                    self.safe_read32(fh_tx_trb_channel(fifo)).unwrap_or(!0),
                    self.safe_read32(FH_TSSR_TX_STATUS_REG).unwrap_or(!0),
                    self.safe_read32(FH_TSSR_TX_ERROR_REG).unwrap_or(!0),
                    self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0),
                    self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0),
                );
                log::info!(
                    "iwlwifi: TFD raw queue={} slot={} tfd_hex={} bc_primary={:#06x} bc_duplicate={:#06x} tb0_addr={:#010x} tb0_hnlen={:#06x} tb1_addr={:#010x} tb1_hnlen={:#06x} tb2_addr={:#010x} tb2_hnlen={:#06x}",
                    traffic_queue,
                    desc_idx,
                    tfd_hex,
                    u16::from(bc_primary),
                    u16::from(bc_duplicate),
                    unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[0].addr_lo)) },
                    unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[0].hi_n_len)) },
                    unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[1].addr_lo)) },
                    unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[1].hi_n_len)) },
                    unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[2].addr_lo)) },
                    unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(desc.tbs[2].hi_n_len)) },
                );
                if let Some(dma_wire) = auth_dma_wire {
                    log::info!(
                        "iwlwifi: TX auth DMA buffer queue={} slot={} dma_wire_match={} wire_hex={}",
                        traffic_queue,
                        desc_idx,
                        dma_wire == wire,
                        HexBytes(&dma_wire),
                    );
                }
                self.log_dqa_scheduler_snapshot("after_auth_doorbell", 1u32 << traffic_queue);
            }
        }
        // Release only after the q0 host-command ring is empty. A data-frame
        // submission may be interleaved with a pending host command, and
        // dropping MAC_ACCESS_REQ in that case can suspend the command before
        // firmware consumes it.
        self.release_mac_access_if_tx_idle();
    }

    /// Publish one entry in Linux's `iwlagn_scd_bc_tbl`, including the
    /// duplicate window used when the 8-bit TFD index wraps.
    ///
    /// 7265/7000-series gen1 firmware consumes the low 12 bits as DWORDs.
    /// Linux's gen1 transport adds CRC/delimiter (and a security trailer when
    /// applicable), then rounds the result up to a DWORD before publishing it.
    fn update_scd_byte_count(
        &mut self,
        queue: u32,
        write_ptr: usize,
        frame_len: u16,
        sta_id: u8,
        sec_ctl: u8,
    ) -> u16 {
        const TFD_QUEUE_SIZE_MAX: usize = 256;
        const TFD_QUEUE_SIZE_BC_DUP: usize = 64;
        const TFD_QUEUE_BC_SIZE: usize = TFD_QUEUE_SIZE_MAX + TFD_QUEUE_SIZE_BC_DUP;
        const TX_CMD_SEC_MSK: u8 = 0x07;
        const TX_CMD_SEC_CCM: u8 = 0x02;

        let mut length = frame_len.saturating_add(4 + 4); // CRC + delimiter
        if sec_ctl & TX_CMD_SEC_MSK == TX_CMD_SEC_CCM {
            length = length.saturating_add(8); // CCMP MIC
        }
        debug_assert!(length <= 0x0fff);
        debug_assert!(queue < 32);
        debug_assert!(write_ptr < TFD_QUEUE_SIZE_MAX);
        let dwords = length.saturating_add(3) / 4;
        let entry = (dwords & 0x0fff) | ((sta_id as u16) << 12);
        let table_base = self.tx_dma_ring.virt() + TX_SCD_BC_OFFSET;
        let queue_base = table_base + queue as usize * TFD_QUEUE_BC_SIZE * 2;
        let primary = queue_base + write_ptr * 2;
        unsafe {
            core::ptr::write_unaligned(primary as *mut u16, entry.to_le());
        }
        mmio::cache_flush(primary);
        if write_ptr < TFD_QUEUE_SIZE_BC_DUP {
            let duplicate = queue_base + (TFD_QUEUE_SIZE_MAX + write_ptr) * 2;
            unsafe {
                core::ptr::write_unaligned(duplicate as *mut u16, entry.to_le());
            }
            mmio::cache_flush(duplicate);
        }
        entry
    }

    /// Build the legacy API-v6 TX command consumed by the 7265 firmware.
    ///
    /// The DMA buffer must begin with the normal four-byte command header;
    /// the fixed TX command follows it, then the 802.11 MAC frame.  Linux
    /// calls this `struct iwl_tx_cmd`. The values below mirror Linux's
    /// `iwl_mvm_set_tx_cmd()`/`iwl_mvm_set_tx_cmd_rate()` defaults for the
    /// management and non-QoS frames used by this driver.
    fn build_tx_command(
        frame: &[u8],
        sequence: u16,
        rate_n_flags: u32,
        ccmp_key: Option<&[u8; 16]>,
    ) -> Vec<u8> {
        let mac_header_len = Self::tx_mac_header_len(frame);
        let tb1_unaligned = TX_FRAME_OFFSET + mac_header_len - IWL_FIRST_TB_SIZE;
        let mac_padding = ((tb1_unaligned + 3) & !3) - tb1_unaligned;
        let mut wire = vec![0u8; TX_FRAME_OFFSET + frame.len() + mac_padding];

        // HcmdHeader.
        wire[0] = TX_CMD_OPCODE;
        wire[1] = GroupId::Legacy as u8;
        wire[2..4].copy_from_slice(&sequence.to_le_bytes());

        // API-v6 iwl_tx_cmd, relative to the command header.
        let tx = TX_COMMAND_HEADER_LEN;
        wire[tx..tx + 2].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        // TX_CMD_FLG_ACK | TX_CMD_FLG_SEQ_CTL plus Linux's BT coexistence
        // priority for 2.4 GHz management traffic.
        // Authentication/association are non-QoS management frames. Keep
        // the sequence-control bit enabled because the frame builders leave
        // sequence assignment to the firmware, as mac80211 does for these
        // requests. Authentication/association management frames use BT
        // priority 3; ordinary unicast data remains at priority 0.
        const TX_CMD_FLG_ACK: u32 = 1 << 3;
        const TX_CMD_FLG_BT_PRIO_POS: u32 = 11;
        const TX_CMD_FLG_SEQ_CTL: u32 = 1 << 13;
        const TX_CMD_FLG_MH_PAD: u32 = 1 << 20;
        let frame_type = (frame[0] & 0x0c) >> 2;
        let subtype = (frame[0] >> 4) & 0x0f;
        let bt_priority = if frame_type == 0 && subtype != 10 {
            3
        } else {
            0
        };
        let tx_flags = TX_CMD_FLG_ACK
            | TX_CMD_FLG_SEQ_CTL
            | (bt_priority << TX_CMD_FLG_BT_PRIO_POS)
            | if mac_padding != 0 {
                TX_CMD_FLG_MH_PAD
            } else {
                0
            };
        // Linux 4.14 sets offload_assist to 0 for the 7265 (sw_csum_tx is
        // false). A non-zero value may cause the firmware to attempt TX
        // offload processing that cannot complete on this hardware, which
        // can prevent the SCD from advancing the read pointer.
        let offload_assist: u16 = 0;
        wire[tx + 2..tx + 4].copy_from_slice(&offload_assist.to_le_bytes());
        wire[tx + 4..tx + 8].copy_from_slice(&tx_flags.to_le_bytes());
        wire[tx + 12..tx + 16].copy_from_slice(&rate_n_flags.to_le_bytes());
        // sta_id=0. For CCMP, Linux v4.14 copies the pairwise key into every
        // legacy TX command. KEY_FROM_TABLE is used by its GCMP path, not by
        // the 7265D CCMP path.
        wire[tx + 16] = 0;
        if frame.len() >= 2 && frame[1] & 0x40 != 0 {
            wire[tx + 17] = 0x02; // TX_CMD_SEC_CCM
            if let Some(key) = ccmp_key {
                wire[tx + 20..tx + 36].copy_from_slice(key);
            }
        }
        wire[tx + 40..tx + 44].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        // Linux uses 60 RTS retries and 15 data retries for ordinary
        // management/data frames (3 is reserved for probe responses).
        wire[tx + 49] = 60;
        wire[tx + 50] = 15;
        // IWL_MAX_TID_COUNT marks a non-QoS management frame. PM_FRAME_ASSOC
        // is needed for association requests; authentication uses the normal
        // management timeout.
        wire[tx + 51] = IWL_MAX_TID_COUNT;
        let pm_timeout = if frame.first().is_some_and(|fc| *fc & 0xfc == 0x00) {
            3u16
        } else {
            2u16
        };
        wire[tx + 52..tx + 54].copy_from_slice(&pm_timeout.to_le_bytes());

        wire[TX_FRAME_OFFSET..TX_FRAME_OFFSET + mac_header_len]
            .copy_from_slice(&frame[..mac_header_len]);
        wire[TX_FRAME_OFFSET + mac_header_len + mac_padding..]
            .copy_from_slice(&frame[mac_header_len..]);
        wire
    }

    /// Header length needed for Linux's gen1 TB1/MAC-padding split. The
    /// current TX path emits management and three-address data frames, while
    /// retaining the standard QoS/four-address/HT-control extensions.
    fn tx_mac_header_len(frame: &[u8]) -> usize {
        if frame.len() < 24 {
            return frame.len();
        }
        let frame_type = (frame[0] & 0x0c) >> 2;
        if frame_type != 2 {
            return 24;
        }
        let mut len = 24;
        if frame[1] & 0x03 == 0x03 {
            len += 6;
        }
        if frame[0] & 0x80 != 0 {
            len += 2;
            if frame[1] & 0x80 != 0 {
                len += 4;
            }
        }
        len.min(frame.len())
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

    /// Extend the data-queue hardware read pointer into its monotonic host
    /// counter.  The command and data queues have independent scheduler
    /// pointers and must not share completion accounting.
    pub(super) fn update_data_tx_tail(&mut self, hardware_tail: usize) {
        let hardware_tail = hardware_tail % TX_QUEUE_SIZE;
        let current_tail = self.tx_data_tail % TX_QUEUE_SIZE;
        let advance = (hardware_tail + TX_QUEUE_SIZE - current_tail) % TX_QUEUE_SIZE;
        let outstanding = self.tx_data_head.wrapping_sub(self.tx_data_tail);
        if advance > outstanding {
            return;
        }
        self.tx_data_tail = self.tx_data_tail.wrapping_add(advance);
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
    fn ccmp_key_commands_match_linux_v1_flags_and_trailer() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        let end = device
            .install_wpa_keys([0x11; 16], [0x22; 16], 1)
            .expect("queue CCMP keys");

        assert_eq!(end, 2);
        let pairwise = device.tx_bufs[0].as_slice();
        let group = device.tx_bufs[1].as_slice();
        assert_eq!(&pairwise[..4], &[LegacyCmd::AddStaKey as u8, 0, 0, 9]);
        assert_eq!(&group[..4], &[LegacyCmd::AddStaKey as u8, 0, 1, 9]);
        assert_eq!(u16::from_le_bytes([pairwise[6], pairwise[7]]), 0x000a);
        assert_eq!(u16::from_le_bytes([group[6], group[7]]), 0x410a);
        assert_eq!(&pairwise[8..24], &[0x11; 16]);
        assert_eq!(&group[8..24], &[0x22; 16]);
        assert_eq!(&pairwise[56..68], &[0; 12]);
        assert_eq!(&group[56..68], &[0; 12]);
    }

    #[test]
    fn api_v6_tx_command_has_linux_management_defaults() {
        let frame = [0xb0u8; 30];
        let wire = IwlWifiDevice::build_tx_command(&frame, 0x0918, TX_RATE_1M_CCK, None);
        let tx = TX_COMMAND_HEADER_LEN;

        assert_eq!(wire[0], TX_CMD_OPCODE);
        assert_eq!(wire[1], GroupId::Legacy as u8);
        assert_eq!(&wire[2..4], &0x0918u16.to_le_bytes());
        assert_eq!(
            u16::from_le_bytes([wire[tx], wire[tx + 1]]),
            frame.len() as u16
        );
        assert_eq!(
            u16::from_le_bytes([wire[tx + 2], wire[tx + 3]]),
            0 // offload_assist: Linux 4.14 sets this to 0 for 7265
        );
        assert_eq!(
            u32::from_le_bytes(wire[tx + 4..tx + 8].try_into().unwrap()),
            (1 << 3) | (3 << 11) | (1 << 13)
        );
        assert_eq!(wire[tx + 16], 0); // AP station ID
        assert_eq!(wire[tx + 49], 60); // RTS retries
        assert_eq!(wire[tx + 50], 15); // ordinary management retries
        assert_eq!(wire[tx + 51], IWL_MAX_TID_COUNT); // non-QoS
        assert_eq!(
            u16::from_le_bytes([wire[tx + 52], wire[tx + 53]]),
            2 // PM_FRAME_MGMT for authentication
        );
        assert_eq!(&wire[TX_FRAME_OFFSET..], &frame);
    }

    #[test]
    fn api_v6_tx_command_inserts_linux_qos_mac_padding() {
        let mut frame = [0u8; 34];
        frame[0] = 0x88; // QoS data
        frame[1] = 0x01; // To DS, three-address header
        frame[26..].copy_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x88, 0x8e]);

        let wire = IwlWifiDevice::build_tx_command(&frame, 0x0500, TX_RATE_1M_CCK, None);
        let tx = TX_COMMAND_HEADER_LEN;
        let offload_assist = u16::from_le_bytes(wire[tx + 2..tx + 4].try_into().unwrap());
        let flags = u32::from_le_bytes(wire[tx + 4..tx + 8].try_into().unwrap());

        assert_eq!(wire.len(), TX_FRAME_OFFSET + frame.len() + 2);
        // offload_assist is 0 on 7265 (Linux 4.14 sw_csum_tx=false).
        assert_eq!(offload_assist, 0);
        // MH_PAD is still set in tx_flags for the 2-byte padding.
        assert_ne!(flags & (1 << 20), 0);
        assert_eq!(&wire[TX_FRAME_OFFSET..TX_FRAME_OFFSET + 26], &frame[..26]);
        assert_eq!(&wire[TX_FRAME_OFFSET + 26..TX_FRAME_OFFSET + 28], &[0, 0]);
        assert_eq!(&wire[TX_FRAME_OFFSET + 28..], &frame[26..]);
    }

    #[test]
    fn protected_frames_use_increasing_ccmp_pns_and_inline_pairwise_key() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.wifi_conn.current_bssid = Some([0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);

        let first = device.build_data_frame(0x0800, &[0xaa], true).unwrap();
        let second = device.build_data_frame(0x0800, &[0xbb], true).unwrap();
        assert_eq!(&first[24..32], &[1, 0, 0, 0x20, 0, 0, 0, 0]);
        assert_eq!(&second[24..32], &[2, 0, 0, 0x20, 0, 0, 0, 0]);
        assert_eq!(&first[32..40], &[0xaa, 0xaa, 0x03, 0, 0, 0, 0x08, 0]);

        let ptk = [0x5a; 16];
        let wire = IwlWifiDevice::build_tx_command(&first, 0x0400, TX_RATE_6M_OFDM, Some(&ptk));
        assert_eq!(
            u16::from_le_bytes(
                wire[TX_COMMAND_HEADER_LEN + 2..TX_COMMAND_HEADER_LEN + 4]
                    .try_into()
                    .unwrap()
            ),
            0 // offload_assist: Linux 4.14 sets this to 0 for 7265
        );
        assert_eq!(wire[TX_COMMAND_HEADER_LEN + 17], 0x02); // CCMP, inline key
        assert_eq!(
            &wire[TX_COMMAND_HEADER_LEN + 20..TX_COMMAND_HEADER_LEN + 36],
            &ptk
        );
    }

    #[test]
    fn command_and_data_queues_use_disjoint_dma_buffers() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.fw_dqa_supported = true;
        device.send_hcmd(0x01, GroupId::Legacy as u8, &[]).unwrap();
        device.send_raw_80211_frame(&[0xb0; 30]).unwrap();

        assert_eq!(device.tx_head, 1);
        assert_eq!(device.tx_data_head, 1);
        assert_ne!(
            device.tx_bufs[0].dma_iova(),
            device.tx_bufs[TX_QUEUE_SIZE].dma_iova()
        );
        assert!(!device.tx_bufs[0].as_slice().is_empty());
        assert!(!device.tx_bufs[TX_QUEUE_SIZE].as_slice().is_empty());
        assert_eq!(
            device.safe_read32(HBUS_TARG_WRPTR),
            Some((IWL_MGMT_QUEUE << 8) | 1)
        );
        assert_eq!(
            &device.tx_bufs[TX_QUEUE_SIZE].as_slice()[2..4],
            &0x0500u16.to_le_bytes()
        );
        let management_desc =
            (device.tx_dma_ring.virt() + TX_MGMT_TFD_RING_OFFSET) as *const TxDmaDesc;
        let (num_tbs, tb0, tb1, tb2) = unsafe {
            (
                core::ptr::read_unaligned(core::ptr::addr_of!((*management_desc).num_tbs)),
                core::ptr::read_unaligned(core::ptr::addr_of!((*management_desc).tbs[0])),
                core::ptr::read_unaligned(core::ptr::addr_of!((*management_desc).tbs[1])),
                core::ptr::read_unaligned(core::ptr::addr_of!((*management_desc).tbs[2])),
            )
        };
        let data_dma = device.tx_bufs[TX_QUEUE_SIZE].dma_iova();
        let tb0_addr = tb0.addr_lo;
        let tb0_len = tb0.hi_n_len >> 4;
        let tb1_addr = tb1.addr_lo;
        let tb1_len = tb1.hi_n_len >> 4;
        let tb2_addr = tb2.addr_lo;
        let tb2_len = tb2.hi_n_len >> 4;
        assert_eq!(num_tbs, 3);
        assert_eq!(tb0_addr, data_dma as u32);
        assert_eq!(tb0_len, 20);
        assert_eq!(tb1_addr, (data_dma + 20) as u32);
        assert_eq!(tb1_len, 64);
        assert_eq!(tb2_addr, (data_dma + 84) as u32);
        assert_eq!(tb2_len, 6);
        let scratch = &device.tx_bufs[TX_QUEUE_SIZE].as_slice()
            [TX_COMMAND_HEADER_LEN + 44..TX_COMMAND_HEADER_LEN + 49];
        assert_eq!(
            u32::from_le_bytes(scratch[..4].try_into().unwrap()),
            (data_dma + 12) as u32
        );
        assert_eq!(scratch[4], ((data_dma + 12) >> 32) as u8 & 0x0f);
        let byte_count_base =
            device.tx_dma_ring.virt() + TX_SCD_BC_OFFSET + IWL_MGMT_QUEUE as usize * (256 + 64) * 2;
        let primary = unsafe { core::ptr::read_unaligned(byte_count_base as *const u16) };
        let duplicate =
            unsafe { core::ptr::read_unaligned((byte_count_base + 256 * 2) as *const u16) };
        // Q5 is Linux's FIFO/non-aggregate management queue, so the
        // Scheduler-ACK byte-count table is intentionally untouched by the
        // submit path. The table writer itself is covered below for the
        // aggregate/Scheduler-ACK path.
        assert_eq!(u16::from_le(primary), 0);
        assert_eq!(u16::from_le(duplicate), 0);
    }

    #[test]
    fn legacy_scd_byte_count_entries_store_dwords() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);

        // 30-byte protected MPDU + CRC/delimiter + CCMP MIC = 46 bytes,
        // rounded up to 12 DWORDs.
        // The station ID occupies the high nibble.
        let entry = device.update_scd_byte_count(IWL_MGMT_QUEUE, 7, 30, 3, 0x02);

        assert_eq!(entry, 0x300c);
        let queue_base =
            device.tx_dma_ring.virt() + TX_SCD_BC_OFFSET + IWL_MGMT_QUEUE as usize * (256 + 64) * 2;
        let primary = unsafe { core::ptr::read_unaligned((queue_base + 7 * 2) as *const u16) };
        let duplicate =
            unsafe { core::ptr::read_unaligned((queue_base + (256 + 7) * 2) as *const u16) };
        assert_eq!(u16::from_le(primary), 0x300c);
        assert_eq!(u16::from_le(duplicate), 0x300c);
    }

    #[test]
    fn enabling_dqa_queue_republishes_its_cbbc_before_initial_wrptr() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.fw_dqa_supported = true;
        device.write_mmio32(fh_mem_cbbc_queue(IWL_MGMT_QUEUE), 0xdead_beef);

        device.enable_dqa_tx_queue(IWL_MGMT_QUEUE).unwrap();

        let queue_phys = device.tx_dma_ring.dma_iova() + tx_tfd_ring_offset(IWL_MGMT_QUEUE) as u64;
        assert_eq!(
            device.safe_read32(fh_mem_cbbc_queue(IWL_MGMT_QUEUE)),
            Some((queue_phys >> 8) as u32)
        );
        assert_eq!(
            device.safe_read32(HBUS_TARG_WRPTR),
            Some(IWL_MGMT_QUEUE << 8)
        );
    }

    #[test]
    fn api29_dqa_gate_is_linux_owned_by_default() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.fw_dqa_supported = true;
        device.fw_api_ver = IWL_FW_API29_MAX;
        device.write_mmio32(HBUS_TARG_PRPH_RDAT, 1 << IWL_DQA_CMD_QUEUE);
        device.write_mmio32(HBUS_TARG_WRPTR, 0x1234_5678);

        device.ensure_api29_dqa_scheduler_gate(IWL_MGMT_QUEUE);

        assert_eq!(device.safe_read32(HBUS_TARG_PRPH_WDAT), Some(0));
        assert_eq!(device.safe_read32(HBUS_TARG_WRPTR), Some(0x1234_5678));
    }

    #[test]
    fn api29_dqa_host_commands_use_q0_and_its_own_tfd_ring() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.fw_dqa_supported = true;

        device.send_hcmd(0x03, GroupId::Legacy as u8, &[]).unwrap();

        assert_eq!(device.command_queue(), IWL_DQA_CMD_QUEUE);
        assert_eq!(device.auxiliary_queue(), IWL_DQA_AUX_QUEUE);
        assert_eq!(device.safe_read32(HBUS_TARG_WRPTR), Some(1));
        assert_eq!(&device.tx_bufs[0].as_slice()[2..4], &0u16.to_le_bytes());
        let dqa_desc = unsafe {
            &*((device.tx_dma_ring.virt() + tx_tfd_ring_offset(IWL_DQA_CMD_QUEUE))
                as *const TxDmaDesc)
        };
        let legacy_desc = unsafe {
            &*((device.tx_dma_ring.virt() + tx_tfd_ring_offset(IWL_LEGACY_CMD_QUEUE))
                as *const TxDmaDesc)
        };
        assert_eq!(dqa_desc.num_tbs, 1);
        assert_eq!(legacy_desc.num_tbs, 0);
    }

    #[test]
    fn host_command_releases_mac_access_when_command_queue_is_empty() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);

        device.send_hcmd(0x01, GroupId::Legacy as u8, &[]).unwrap();
        assert_ne!(
            device.safe_read32(CSR_GP_CNTRL).unwrap() & CSR_GP_CNTRL_MAC_ACCESS_REQ,
            0,
        );

        // Linux releases the host-command wake hold when q0 is empty even if
        // a DQA data queue still has an outstanding descriptor.
        device.tx_data_head = 1;
        device.tx_data_tail = 0;
        device.tx_tail = device.tx_head;
        device.release_mac_access_if_tx_idle();
        assert_eq!(
            device.safe_read32(CSR_GP_CNTRL).unwrap() & CSR_GP_CNTRL_MAC_ACCESS_REQ,
            0,
        );
    }

    #[test]
    fn pending_host_command_keeps_mac_access_held() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.write_mmio32(CSR_GP_CNTRL, CSR_GP_CNTRL_MAC_ACCESS_REQ);
        device.tx_head = 1;
        device.tx_tail = 0;

        device.release_mac_access_if_tx_idle();

        assert_ne!(
            device.safe_read32(CSR_GP_CNTRL).unwrap() & CSR_GP_CNTRL_MAC_ACCESS_REQ,
            0,
        );
    }

    #[test]
    fn alive_v3_extracts_linux_lmac_scheduler_base() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.init_firmware_completed = false;
        let mut payload = [0u8; 44];
        payload[0..2].copy_from_slice(&0xcafeu16.to_le_bytes());
        payload[20..24].copy_from_slice(&0x0080_2784u32.to_le_bytes());
        payload[40..44].copy_from_slice(&0x0081_2a54u32.to_le_bytes());

        device.record_alive_notification(&payload);

        assert_eq!(device.init_errlog_ptr, 0x0080_2784);
        assert_eq!(device.alive_scd_base_addr, 0x0081_2a54);
    }

    #[test]
    fn alive_v3_dead_status_still_preserves_init_error_table() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.init_firmware_completed = false;
        let mut payload = [0u8; 44];
        payload[0..2].copy_from_slice(&0xdeadu16.to_le_bytes());
        payload[20..24].copy_from_slice(&0x0080_e950u32.to_le_bytes());
        payload[40..44].copy_from_slice(&0x0080_d700u32.to_le_bytes());

        device.record_alive_notification(&payload);

        assert_eq!(device.init_errlog_ptr, 0x0080_e950);
        assert_eq!(device.alive_scd_base_addr, 0x0080_d700);
    }

    #[test]
    fn linux_boot_prearms_rx_and_inert_tx_foundation_while_cpu_reset_is_asserted() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.write_mmio32(CSR_RESET, 1);
        device.write_mmio32(HBUS_TARG_WRPTR, 0xCAFE_BABE);

        device.prearm_rx_before_cpu_release().unwrap();
        device.prearm_tx_foundation_before_cpu_release().unwrap();

        assert_ne!(device.safe_read32(FH_MEM_RCSR_CHNL0_CONFIG_REG), Some(0));
        let base = device.tx_dma_ring.dma_iova();
        for queue in 0..IWL_NUM_OF_QUEUES {
            assert_eq!(
                device.safe_read32(fh_mem_cbbc_queue(queue)),
                Some(((base + tx_tfd_ring_offset(queue) as u64) >> 8) as u32)
            );
        }
        assert_eq!(
            device.safe_read32(FH_KW_MEM_ADDR_REG),
            Some(((base + TX_KEEP_WARM_OFFSET as u64) >> 4) as u32)
        );
        assert_eq!(
            device.safe_read32(HBUS_TARG_PRPH_WADDR),
            Some(SCD_GP_CTRL | (3 << 24))
        );
        assert_eq!(
            device.safe_read32(HBUS_TARG_PRPH_WDAT),
            Some(SCD_GP_CTRL_AUTO_ACTIVE_MODE | SCD_GP_CTRL_ENABLE_31_QUEUES)
        );
        // Publishing ring addresses is inert: no queue doorbell and no TX
        // DMA channel may be enabled until ALIVE has been validated.
        assert_eq!(device.safe_read32(HBUS_TARG_WRPTR), Some(0xCAFE_BABE));
        assert_eq!(device.safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE), Some(0));

        device.write_mmio32(CSR_RESET, 0);
        assert_eq!(
            device.prearm_rx_before_cpu_release(),
            Err(crate::DriverError::Protocol)
        );
        assert_eq!(
            device.prearm_tx_foundation_before_cpu_release(),
            Err(crate::DriverError::Protocol)
        );
    }

    #[test]
    fn legacy_tx_activation_after_alive_preserves_prearmed_foundation() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.write_mmio32(CSR_RESET, 1);
        device.tx_head = 7;
        device.tx_tail = 3;
        device.tx_data_head = 9;
        device.tx_data_tail = 4;
        device.write_mmio32(HBUS_TARG_WRPTR, 0xCAFE_BABE);
        // Firmware boot pre-arms RX; the ALIVE transition must preserve that
        // ring while it brings up only the TX/SCD half of the transport.
        device.init_rx_dma();
        device.prearm_tx_foundation_before_cpu_release().unwrap();
        let rx_config = device.safe_read32(FH_MEM_RCSR_CHNL0_CONFIG_REG);
        device.write_mmio32(CSR_RESET, 0);

        device.start_legacy_dma_after_alive().unwrap();

        let base = device.tx_dma_ring.dma_iova();
        assert_eq!(device.tx_head, 0);
        assert_eq!(device.tx_tail, 0);
        assert_eq!(device.tx_data_head, 0);
        assert_eq!(device.tx_data_tail, 0);
        assert_eq!(device.rx_head, 0);
        assert_eq!(device.rx_tail, 0);
        assert_eq!(device.rx_posted, RX_QUEUE_SIZE - 1);
        assert_eq!(device.safe_read32(FH_MEM_RCSR_CHNL0_CONFIG_REG), rx_config);
        assert_eq!(
            device.safe_read32(FH_MEM_CBBC_CMD_QUEUE),
            Some((base >> 8) as u32)
        );
        assert_eq!(
            device.safe_read32(FH_MEM_CBBC_DATA_QUEUE),
            Some(((base + TX_DATA_TFD_RING_OFFSET as u64) >> 8) as u32)
        );
        assert_eq!(
            device.safe_read32(FH_MEM_CBBC_AUX_QUEUE),
            Some(((base + TX_AUX_TFD_RING_OFFSET as u64) >> 8) as u32)
        );
        assert_eq!(
            device.safe_read32(FH_MEM_RCSR_CHNL0_CONFIG_REG),
            Some(
                FH_RCSR_RX_CONFIG_CHNL_EN_ENABLE_VAL
                    | FH_RCSR_CHNL0_RX_IGNORE_RXF_EMPTY
                    | FH_RCSR_CHNL0_RX_CONFIG_IRQ_DEST_INT_HOST_VAL
                    | (FH_RCSR_RX_RB_TIMEOUT << FH_RCSR_RX_CONFIG_REG_IRQ_RBTH_POS)
                    | (8 << FH_RCSR_RX_CONFIG_RBDCB_SIZE_POS)
            )
        );
        assert_eq!(
            device.safe_read32(HBUS_TARG_WRPTR),
            Some(IWL_CMD_QUEUE << 8)
        );
    }

    #[test]
    fn alive_reasserts_dqa_auto_active_without_clobbering_gp_control() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        let existing = 1 << 7;
        device.write_mmio32(HBUS_TARG_PRPH_RDAT, existing);

        device.ensure_scd_auto_active_after_alive().unwrap();

        assert_eq!(
            device.safe_read32(HBUS_TARG_PRPH_WADDR),
            Some(SCD_GP_CTRL | (3 << 24))
        );
        assert_eq!(
            device.safe_read32(HBUS_TARG_PRPH_WDAT),
            Some(existing | SCD_GP_CTRL_AUTO_ACTIVE_MODE | SCD_GP_CTRL_ENABLE_31_QUEUES)
        );
    }

    #[test]
    fn legacy_non_dqa_data_queue_is_activated_without_a_firmware_command() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        let command_head = device.tx_head;

        device.enable_data_tx_queue().unwrap();

        assert_eq!(device.tx_head, command_head);
        assert_eq!(
            device.safe_read32(HBUS_TARG_WRPTR),
            Some(IWL_DATA_QUEUE << 8)
        );
        assert_eq!(
            device.safe_read32(HBUS_TARG_PRPH_WADDR),
            Some(SCD_QUEUE_STATUS_DATA | (3 << 24))
        );
        assert_eq!(
            device.safe_read32(HBUS_TARG_PRPH_WDAT),
            Some(SCD_QUEUE_STTS_ACTIVE | 1 | SCD_QUEUE_STTS_WSL | SCD_QUEUE_STTS_MASK)
        );
    }
}
