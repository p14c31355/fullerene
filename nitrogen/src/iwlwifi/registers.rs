//! Register definitions, PCI identifiers, and firmware constants for
//! Intel Wireless 7265 (iwlwifi 7000 series).

// ── PCI identifiers ────────────────

pub const IWL_PCI_VENDOR: u16 = 0x8086;
pub const IWL_DEVICE_IDS: &[u16] = &[0x095b, 0x095a, 0x08b1, 0x08b2];
/// Intel CNVi devices use a different transport and firmware family from the
/// legacy 7000-series implementation below. Keep this list separate so a
/// modern adapter can be diagnosed without ever being passed to the 7265
/// reset/firmware path. 4df0 is the Qu/AX101-family ID; 54f0 is a later So
/// CNVi ID also seen on platforms marketed with AX101 hardware.
pub const IWL_MODERN_CNVI_DEVICE_IDS: &[u16] = &[0x4df0, 0x54f0];

/// PCI requester ID format consumed by the VT-d context-table lookup.
pub const fn pci_dma_device_id(bus: u8, device: u8, function: u8) -> u16 {
    ((bus as u16) << 8) | ((device as u16) << 3) | function as u16
}

// ── CSR registers ──────────────────

pub const CSR_HW_REV: u32 = 0x028 / 4;
pub const CSR_HW_IF_CONFIG: u32 = 0x000;
pub const CSR_HW_RF_ID: u32 = 0x034 / 4;
pub const CSR_GIO: u32 = 0x03C / 4;
pub const CSR_GIO_CHICKEN_BITS: u32 = 0x100 / 4;
pub const CSR_MAC_SHADOW_REG_CTRL: u32 = 0x0A8 / 4;
/// Enable the 7000-series shadow-register path, matching
/// `iwl7000_base.shadow_reg_enable` in Linux.
pub const CSR_MAC_SHADOW_REG_CTRL_ENABLE: u32 = 0x800F_FFFF;
pub const CSR_DBG_HPET_MEM: u32 = 0x240 / 4;
pub const CSR_UCODE_GP1: u32 = 0x054 / 4;
pub const CSR_UCODE_GP1_SET: u32 = 0x058 / 4;
pub const CSR_UCODE_GP1_CLR: u32 = 0x05C / 4;
pub const CSR_GP_DRIVER: u32 = 0x098 / 4;
pub const CSR_LED_REG: u32 = 0x094 / 4;
pub const CSR_DRAM_INT_TBL: u32 = 0x0A0 / 4;
pub const CSR_GIO2: u32 = 0x0EC / 4;
pub const CSR_RESET: u32 = 0x020 / 4;
pub const CSR_GP_CNTRL: u32 = 0x024 / 4;
pub const CSR_EEPROM_GP: u32 = 0x02C / 4;
pub const CSR_OTP_GP: u32 = 0x030 / 4;
pub const CSR_INT: u32 = 0x008 / 4;
pub const CSR_INT_MASK: u32 = 0x00C / 4;
pub const CSR_FH_INT: u32 = 0x010 / 4;
pub const CSR_INT_PERIODIC: u32 = 0x014 / 4;

// ── Reset / power-on constants ─────

pub const CSR_RESET_BIT_SW: u32 = 1 << 7;
pub const CSR_RESET_BIT_MASTER_DISABLED: u32 = 1 << 8;
pub const CSR_RESET_BIT_STOP_MASTER: u32 = 1 << 9;
pub const CSR_GP_CNTRL_MAC_ACCESS_REQ: u32 = 1 << 3;
pub const CSR_GP_CNTRL_INIT_DONE: u32 = 1 << 2;
pub const CSR_GP_CNTRL_MAC_CLOCK_READY: u32 = 1 << 0;
pub const CSR_GP_CNTRL_GOING_TO_SLEEP: u32 = 1 << 4;
/// CSR_HW_REV type field in the register's original bit positions.
///
/// The type occupies bits 15:4.  Keep the selector input in this raw form;
/// Linux's `CSR_HW_REV_TYPE_7265D` constant is also expressed in these bit
/// positions (0x210), not as the shifted value (0x21).
pub const CSR_HW_REV_TYPE_MASK: u16 = 0xFFF0;
pub const CSR_HW_REV_TYPE_7265D: u16 = 0x0210;

/// Decode the printable/type value from a raw CSR_HW_REV register value.
pub const fn csr_hw_rev_type(raw: u32) -> u16 {
    ((raw & 0x0000_FFF0) >> 4) as u16
}
pub const CSR_INT_BIT_ALIVE: u32 = 1 << 0;
pub const CSR_INT_BIT_RESET_DONE: u32 = 1 << 2;
pub const CSR_INT_BIT_SW_RX: u32 = 1 << 3;
pub const CSR_INT_BIT_RF_KILL: u32 = 1 << 7;
pub const CSR_INT_BIT_WAKEUP: u32 = 1 << 1;
pub const CSR_INT_BIT_RX_PERIODIC: u32 = 1 << 28;
pub const CSR_INT_BIT_HW_ERR: u32 = 1 << 29;
pub const CSR_INT_BIT_SCD: u32 = 1 << 26;
pub const CSR_INT_BIT_SW_ERR: u32 = 1 << 25;
pub const CSR_INT_BIT_FH_TX: u32 = 1 << 27;
pub const CSR_INT_BIT_FH_RX: u32 = 1 << 31;
/// Runtime interrupt set used by the legacy 7000-series transport.
///
/// Command responses are reported through SW_RX as well as the FH RX
/// aggregate. Keep the mask aligned with the upstream gen1 transport instead
/// of enabling reserved CSR bits with 0xffff_ffff.
pub const CSR_INI_SET_MASK: u32 = CSR_INT_BIT_FH_RX
    | CSR_INT_BIT_HW_ERR
    | CSR_INT_BIT_FH_TX
    | CSR_INT_BIT_SW_ERR
    | CSR_INT_BIT_RF_KILL
    | CSR_INT_BIT_SW_RX
    | CSR_INT_BIT_WAKEUP
    | CSR_INT_BIT_RESET_DONE
    | CSR_INT_BIT_ALIVE
    | CSR_INT_BIT_RX_PERIODIC;
pub const CSR_FH_INT_BIT_TX_CHNL0: u32 = 1 << 0;
pub const CSR_FH_INT_BIT_RX_CHNL0: u32 = 1 << 16;
pub const CSR_FH_INT_RX_MASK: u32 = 1 << 16;
pub const CSR_FH_INT_TX_MASK: u32 = 1 << 0;
pub const CSR_UCODE_SW_BIT_RFKILL: u32 = 1 << 1;
pub const CSR_UCODE_GP1_BIT_CMD_BLOCKED: u32 = 1 << 2;
pub const CSR_HW_IF_CONFIG_HAP_WAKE: u32 = 0x0008_0000;
pub const CSR_GIO_CHICKEN_L1A_NO_L0S_RX: u32 = 0x0080_0000;
pub const CSR_GIO_CHICKEN_DIS_L0S_EXIT_TIMER: u32 = 0x2000_0000;
pub const CSR_DBG_HPET_MEM_VAL: u32 = 0xFFFF_0000;

/// Legacy 7265 RX status/RBD registers. The old 0x0b8/0x0c0 offsets are not
/// RX-ring registers on this generation.
pub const FH_RSCSR_CHNL0_STTS_WPTR_REG: u32 = (0x1000 + 0xBC0) / 4;
pub const FH_RSCSR_CHNL0_RBDCB_BASE_REG: u32 = (0x1000 + 0xBC4) / 4;
pub const FH_RSCSR_CHNL0_RBDCB_WPTR_REG: u32 = (0x1000 + 0xBC8) / 4;
pub const FH_RSCSR_CHNL0_RDPTR_REG: u32 = (0x1000 + 0xBCC) / 4;
pub const FH_MEM_RCSR_CHNL0_CONFIG_REG: u32 = (0x1000 + 0xC00) / 4;
pub const FH_MEM_RCSR_CHNL0_RBDCB_WPTR: u32 = (0x1000 + 0xC08) / 4;
pub const FH_MEM_RCSR_CHNL0_FLUSH_RB_REQ: u32 = (0x1000 + 0xC10) / 4;
pub const FH_RCSR_RX_CONFIG_CHNL_EN_ENABLE_VAL: u32 = 0x8000_0000;
pub const FH_RCSR_CHNL0_RX_IGNORE_RXF_EMPTY: u32 = 0x0000_0004;
pub const FH_RCSR_CHNL0_RX_CONFIG_IRQ_DEST_INT_HOST_VAL: u32 = 0x0000_1000;
pub const FH_RCSR_RX_CONFIG_REG_IRQ_RBTH_POS: u32 = 4;
pub const FH_RCSR_RX_CONFIG_RBDCB_SIZE_POS: u32 = 20;
pub const FH_RCSR_RX_RB_TIMEOUT: u32 = 0x11;

/// Non-DQA MVM command queue used by the API-v17 firmware.
///
/// Linux's `IWL_MVM_CMD_QUEUE` is queue 9. Queue 4 is a data queue in this
/// layout; using it for HCMDs can appear to work for simple commands but
/// corrupts the scheduler state when ADD_STA configures the AUX station.
pub const IWL_CMD_QUEUE: u32 = 9;
/// Minimal managed-station data queue. Linux keeps TX_CMD frames off the
/// command queue and assigns the first ordinary data queue to the AP peer.
pub const IWL_DATA_QUEUE: u32 = 4;
/// Auxiliary queue used by the firmware scan station in the 7265's non-DQA
/// layout. Linux selects q11 for `mvm->aux_queue` in this layout;
/// q8 is the separate off-channel reservation, not the station's TX queue.
pub const IWL_AUX_QUEUE: u32 = 11;
/// Number of logical scheduler queues in Linux's 7000-series configuration.
/// This is distinct from the eight physical FH DMA channels.
pub const IWL_NUM_OF_QUEUES: u32 = 31;
pub const FH_MEM_CBBC_0_15_LOWER_BOUND: u32 = (0x1000 + 0x9D0) / 4;
pub const FH_MEM_CBBC_16_19_LOWER_BOUND: u32 = (0x1000 + 0xBF0) / 4;
pub const FH_MEM_CBBC_20_31_LOWER_BOUND: u32 = (0x1000 + 0xB20) / 4;

/// Locate the legacy circular-buffer base register for a logical TX queue.
/// Linux uses three discontiguous register windows for queues 0..31.
pub const fn fh_mem_cbbc_queue(queue: u32) -> u32 {
    if queue < 16 {
        FH_MEM_CBBC_0_15_LOWER_BOUND + queue
    } else if queue < 20 {
        FH_MEM_CBBC_16_19_LOWER_BOUND + (queue - 16)
    } else {
        FH_MEM_CBBC_20_31_LOWER_BOUND + (queue - 20)
    }
}

pub const FH_MEM_CBBC_CMD_QUEUE: u32 = fh_mem_cbbc_queue(IWL_CMD_QUEUE);
pub const FH_MEM_CBBC_DATA_QUEUE: u32 = fh_mem_cbbc_queue(IWL_DATA_QUEUE);
pub const FH_MEM_CBBC_AUX_QUEUE: u32 = fh_mem_cbbc_queue(IWL_AUX_QUEUE);
pub const FH_KW_MEM_ADDR_REG: u32 = (0x1000 + 0x97C) / 4;
pub const HBUS_TARG_WRPTR: u32 = (0x400 + 0x060) / 4;
/// AX210/So/QuZ RFH free-RBD producer doorbell for RX queue 0.
pub const RFH_Q0_FRBDCB_WIDX_TRG: u32 = 0x1C80 / 4;
pub const FH_TCSR_CHNL_TX_CONFIG_BASE: u32 = (0x1000 + 0xD00) / 4;
/// The FH has eight physical TX DMA channels. Logical scheduler queues
/// (including command q9 and the auxiliary q11) select one of these channels
/// through their SCD FIFO, so they must not be used as TCSR channel numbers.
pub const FH_TCSR_CHNL_NUM: u32 = 8;
pub const FH_TCSR_CHNL_TX_CREDIT_BASE: u32 = FH_TCSR_CHNL_TX_CONFIG_BASE + 1;
pub const FH_TCSR_CHNL_TX_BUF_STS_BASE: u32 = FH_TCSR_CHNL_TX_CONFIG_BASE + 2;
pub const FH_TCSR_TX_CONFIG_DMA_CREDIT_ENABLE: u32 = 0x0000_0008;
pub const FH_TX_CHICKEN_BITS: u32 = (0x1000 + 0xE98) / 4;
pub const FH_TX_CHICKEN_BITS_SCD_AUTO_RETRY_EN: u32 = 0x0000_0002;
/// FH scheduler diagnostics for the legacy TX DMA engine.
pub const FH_TSSR_TX_STATUS_REG: u32 = (0x1000 + 0xEB0) / 4;
pub const FH_TSSR_TX_ERROR_REG: u32 = (0x1000 + 0xEB8) / 4;
pub const FH_TX_TRB_CHNL0: u32 = (0x1000 + 0x958) / 4;
pub const SCD_BASE: u32 = 0xA02C00;
pub const SCD_SRAM_BASE_ADDR: u32 = SCD_BASE;
pub const SCD_DRAM_BASE_ADDR: u32 = SCD_BASE + 0x08;
/// Chain extension is enabled by default on this generation, but is known to
/// interact badly with the legacy scheduler. Linux disables it during TX
/// queue setup, including for 7265 devices.
pub const SCD_CHAINEXT_EN: u32 = SCD_BASE + 0x244;
pub const SCD_TXFACT: u32 = SCD_BASE + 0x10;
pub const SCD_GP_CTRL: u32 = SCD_BASE + 0x1A8;
pub const SCD_EN_CTRL: u32 = SCD_BASE + 0x254;
pub const SCD_QUEUECHAIN_SEL: u32 = SCD_BASE + 0xE8;
pub const SCD_AGGR_SEL: u32 = SCD_BASE + 0x248;
/// Shared SCD SRAM range cleared by the legacy PCIe TX start sequence.
/// It covers queue contexts, TX status entries, and the queue-to-RA/TID
/// translation table for the 31-queue 7000-series layout.
pub const SCD_CONTEXT_MEM_LOWER_BOUND: u32 = 0x600;
pub const SCD_TRANS_TBL_MEM_LOWER_BOUND: u32 = 0x7E0;
/// Linux's `SCD_TRANS_TBL_OFFSET_QUEUE()` rounds pairs of queue entries down
/// to a dword boundary. Passing the queue count gives the exclusive clear end.
pub const fn scd_trans_tbl_offset_queue(queue: u32) -> u32 {
    (SCD_TRANS_TBL_MEM_LOWER_BOUND + queue * 2) & 0xFFFC
}
pub const SCD_TRANS_TBL_MEM_UPPER_BOUND: u32 = scd_trans_tbl_offset_queue(IWL_NUM_OF_QUEUES);
pub const SCD_QUEUE_RDPTR_CMD: u32 = SCD_BASE + 0x68 + IWL_CMD_QUEUE * 4;
pub const SCD_QUEUE_STATUS_CMD: u32 = SCD_BASE + 0x10C + IWL_CMD_QUEUE * 4;
pub const SCD_CONTEXT_QUEUE_CMD: u32 = 0x600 + IWL_CMD_QUEUE * 8;
pub const SCD_QUEUE_RDPTR_DATA: u32 = SCD_BASE + 0x68 + IWL_DATA_QUEUE * 4;
pub const SCD_QUEUE_STATUS_DATA: u32 = SCD_BASE + 0x10C + IWL_DATA_QUEUE * 4;
pub const SCD_CONTEXT_QUEUE_DATA: u32 = 0x600 + IWL_DATA_QUEUE * 8;
pub const SCD_QUEUE_RDPTR_AUX: u32 = SCD_BASE + 0x68 + IWL_AUX_QUEUE * 4;
pub const SCD_QUEUE_STATUS_AUX: u32 = SCD_BASE + 0x10C + IWL_AUX_QUEUE * 4;
pub const SCD_CONTEXT_QUEUE_AUX: u32 = 0x600 + IWL_AUX_QUEUE * 8;
pub const SCD_QUEUE_STTS_ACTIVE: u32 = 1 << 3;
pub const SCD_QUEUE_STTS_WSL: u32 = 1 << 4;
pub const SCD_QUEUE_STTS_FIFO_COMMAND: u32 = 7;
pub const SCD_QUEUE_STTS_MASK: u32 = 0x017F_0000;
pub const SCD_GP_CTRL_AUTO_ACTIVE_MODE: u32 = 1 << 18;
pub const SCD_GP_CTRL_ENABLE_31_QUEUES: u32 = 1 << 0;

// The legacy SCD byte-count table has one 16-bit entry for each of 256 TFDs
// plus 64 duplicate entries, for each of the 32 possible queues. Keep it in
// the same contiguous DMA allocation as the command TFD ring, but outside the
// ring and keep-warm areas.
pub const TX_TFD_RING_BYTES: usize = 128 * TX_QUEUE_SIZE;
/// The auxiliary station has its own TFD ring even though the host does not
/// submit scan frames directly. Linux allocates one ring per scheduler queue;
/// keeping q11 separate prevents firmware from interpreting q9 descriptors as
/// scan traffic.
pub const TX_AUX_TFD_RING_OFFSET: usize = TX_TFD_RING_BYTES;
/// Ordinary data queue ring. It is kept separate from both the HCMD and AUX
/// rings because each scheduler queue owns an independent read pointer.
pub const TX_DATA_TFD_RING_OFFSET: usize = TX_AUX_TFD_RING_OFFSET + TX_TFD_RING_BYTES;
pub const TX_KEEP_WARM_OFFSET: usize = TX_DATA_TFD_RING_OFFSET + TX_TFD_RING_BYTES;
pub const TX_KEEP_WARM_BYTES: usize = 0x1000;
pub const TX_SCD_BC_OFFSET: usize = TX_KEEP_WARM_OFFSET + TX_KEEP_WARM_BYTES;
pub const TX_SCD_BC_BYTES: usize = 32 * (256 + 64) * 2;
pub const TX_DMA_ALLOCATION_BYTES: usize = TX_SCD_BC_OFFSET + TX_SCD_BC_BYTES;
/// Firmware-written boot section status consumed before releasing the CPU.
pub const FH_UCODE_LOAD_STATUS: u32 = 0x1AF0 / 4;

/// Extended SRAM address window used by 7000-series firmware sections.
pub const FW_MEM_EXTENDED_START: u32 = 0x0004_0000;
pub const FW_MEM_EXTENDED_END: u32 = 0x0005_7FFF;
pub const LMPM_CHICK: u32 = 0x00A0_1FF8;
pub const LMPM_CHICK_EXTENDED_ADDR_SPACE: u32 = 1 << 0;

// Legacy 7000-series firmware upload service channel. These are the
// byte-offsets from Linux's iwl-fh.h, converted to dword MMIO indices.
pub const FH_SRVC_CHNL_SRAM_ADDR: u32 = (0x1000 + 0x9C8) / 4;
pub const FH_TFDIB_CTRL0_SRVC: u32 = (0x1000 + 0x900 + 0x8 * 9) / 4;
pub const FH_TFDIB_CTRL1_SRVC: u32 = FH_TFDIB_CTRL0_SRVC + 1;
pub const FH_TCSR_CHNL_TX_CONFIG_SRVC: u32 = (0x1000 + 0xD00 + 0x20 * 9) / 4;
pub const FH_TCSR_CHNL_TX_BUF_STS_SRVC: u32 = FH_TCSR_CHNL_TX_CONFIG_SRVC + 2;
pub const FH_TCSR_TX_CONFIG_DMA_ENABLE: u32 = 0x8000_0000;
pub const FH_TCSR_TX_CONFIG_CIRQ_HOST_ENDTFD: u32 = 0x0010_0000;
pub const FH_TCSR_TX_BUF_STS_TFDB_VALID: u32 = 0x0000_0003;
pub const FH_TCSR_TX_BUF_STS_TB_NUM: u32 = 1 << 20;
pub const FH_TCSR_TX_BUF_STS_TB_IDX: u32 = 1 << 12;
pub const FH_MEM_TFDIB_REG1_ADDR_BITSHIFT: u32 = 28;
pub const FH_MEM_TB_MAX_LENGTH: usize = 0x0002_0000;

// Internal peripheral access used for the 7000-series APMG power/DMA setup.
pub const HBUS_TARG_PRPH_WADDR: u32 = (0x400 + 0x044) / 4;
pub const HBUS_TARG_PRPH_RADDR: u32 = (0x400 + 0x048) / 4;
pub const HBUS_TARG_PRPH_WDAT: u32 = (0x400 + 0x04C) / 4;
pub const HBUS_TARG_PRPH_RDAT: u32 = (0x400 + 0x050) / 4;
pub const APMG_CLK_EN_REG: u32 = 0x3004;
pub const APMG_PCIDEV_STT_REG: u32 = 0x3010;
pub const APMG_CLK_VAL_DMA_CLK_RQT: u32 = 0x0000_0200;
pub const APMG_PCIDEV_STT_L1_ACT_DIS: u32 = 0x0000_0800;

// ── Firmware constants ─────────────

pub const IWL_FW_API_VER: u32 = 17;
/// API range supported by the 7265D firmware family. Linux advertises
/// 22..=29 for this device; the captured machine loaded API 29.
pub const IWL_FW_API29_MIN: u32 = 22;
pub const IWL_FW_API29_MAX: u32 = 29;
pub const IWL_FW_MAX_SECTIONS: usize = 32;

/// TX queue configuration.
pub const TX_QUEUE_SIZE: usize = 256;
pub const RX_QUEUE_SIZE: usize = 256;
/// Gen1 FH RX is configured for 4 KiB receive buffers.
pub const RX_BUFFER_SIZE: usize = 4096;
// Host commands also carry the largest API-v17 PHY calibration database
// section (just over 3 KiB), so the command DMA buffers must be a full page.
pub const MAX_FRAME_SIZE: usize = 4096;

// ── Firmware image ─────────────────

pub const IWL_FW_MAGIC: u32 = 0x0a4c5749;
pub const FW_HEADER_SIZE: usize = 88;

/// TLV entry type (modern iwlwifi firmware format).
pub const TLV_SEC_RT: u32 = 19;
pub const TLV_SEC_INIT: u32 = 20;
pub const TLV_SEC_WOWLAN: u32 = 21;
pub const TLV_DEF_CALIB: u32 = 22;
pub const TLV_PHY_SKU: u32 = 23;
/// Firmware capability bitmap entries (`api_index`, `api_capa`).
pub const TLV_ENABLED_CAPABILITIES: u32 = 30;
/// Firmware TLVs containing the runtime/init error-log SRAM addresses.
pub const TLV_RUNT_ERRLOG_PTR: u32 = 10;
pub const TLV_INIT_ERRLOG_PTR: u32 = 13;
pub const FW_CPU1_CPU2_SEPARATOR_SECTION: u32 = 0xFFFF_CCCC;
pub const FW_PAGING_SEPARATOR_SECTION: u32 = 0xAAAA_BBBB;

// ── HBUS register offsets ──────────

pub const HBUS_TARG_MEM_WADDR: u32 = (0x400 + 0x010) / 4;
pub const HBUS_TARG_MEM_WDAT: u32 = (0x400 + 0x018) / 4;
pub const HBUS_TARG_MEM_RADDR: u32 = (0x400 + 0x00C) / 4;
pub const HBUS_TARG_MEM_RDAT: u32 = (0x400 + 0x01C) / 4;
