//! Register definitions, PCI identifiers, and firmware constants for
//! Intel Wireless 7265 (iwlwifi 7000 series).

// ── PCI identifiers ────────────────

pub const IWL_PCI_VENDOR: u16 = 0x8086;
pub const IWL_DEVICE_IDS: &[u16] = &[0x095b, 0x095a, 0x08b1, 0x08b2];

/// PCI requester ID format consumed by the VT-d context-table lookup.
pub const fn pci_dma_device_id(bus: u8, device: u8, function: u8) -> u16 {
    ((bus as u16) << 8) | ((device as u16) << 3) | function as u16
}

// ── CSR registers ──────────────────

pub const CSR_HW_REV: u32 = 0x028 / 4;
pub const CSR_HW_IF_CONFIG: u32 = 0x000 / 4;
pub const CSR_HW_RF_ID: u32 = 0x034 / 4;
pub const CSR_GIO: u32 = 0x03C / 4;
pub const CSR_GIO_CHICKEN_BITS: u32 = 0x100 / 4;
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
/// CSR_HW_REV type field after shifting the register right by four bits.
pub const CSR_HW_REV_TYPE_MASK: u16 = 0x0FFF;
pub const CSR_HW_REV_TYPE_7265D: u16 = 0x0210;
pub const CSR_INT_BIT_ALIVE: u32 = 1 << 0;
pub const CSR_INT_BIT_SW_ERR: u32 = 1 << 25;
pub const CSR_INT_BIT_FH_TX: u32 = 1 << 27;
pub const CSR_INT_BIT_FH_RX: u32 = 1 << 31;
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

/// Legacy TX queue 4 (the host-command queue) registers.
pub const IWL_CMD_QUEUE: u32 = 4;
pub const FH_MEM_CBBC_0_15_LOWER_BOUND: u32 = (0x1000 + 0x9D0) / 4;
pub const FH_MEM_CBBC_CMD_QUEUE: u32 = FH_MEM_CBBC_0_15_LOWER_BOUND + IWL_CMD_QUEUE;
pub const HBUS_TARG_WRPTR: u32 = (0x400 + 0x060) / 4;
pub const FH_TCSR_CHNL_TX_CONFIG_BASE: u32 = (0x1000 + 0xD00) / 4;
pub const FH_TCSR_TX_CONFIG_DMA_CREDIT_ENABLE: u32 = 0x0000_0008;
pub const FH_TX_CHICKEN_BITS: u32 = (0x1000 + 0xE98) / 4;
pub const FH_TX_CHICKEN_BITS_SCD_AUTO_RETRY_EN: u32 = 0x0000_0002;
pub const SCD_BASE: u32 = 0xA02C00;
pub const SCD_SRAM_BASE_ADDR: u32 = SCD_BASE;
pub const SCD_TXFACT: u32 = SCD_BASE + 0x10;
pub const SCD_EN_CTRL: u32 = SCD_BASE + 0x254;
pub const SCD_QUEUE_RDPTR_CMD: u32 = SCD_BASE + 0x68 + IWL_CMD_QUEUE * 4;
pub const SCD_QUEUE_STATUS_CMD: u32 = SCD_BASE + 0x10C + IWL_CMD_QUEUE * 4;
pub const SCD_CONTEXT_QUEUE_CMD: u32 = 0x600 + IWL_CMD_QUEUE * 8;
pub const SCD_QUEUE_STTS_ACTIVE: u32 = 1 << 3;
pub const SCD_QUEUE_STTS_WSL: u32 = 1 << 4;
pub const SCD_QUEUE_STTS_FIFO_COMMAND: u32 = 7;
pub const SCD_QUEUE_STTS_MASK: u32 = 0x017F_0000;
/// Firmware-written boot section status consumed before releasing the CPU.
pub const FH_UCODE_LOAD_STATUS: u32 = 0x1AF0 / 4;

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

pub const IWL_FW_API_VER: u32 = 16;
pub const IWL_FW_MAX_SECTIONS: usize = 32;

/// TX queue configuration.
pub const TX_QUEUE_SIZE: usize = 256;
pub const RX_QUEUE_SIZE: usize = 256;
/// Gen1 FH RX is configured for 4 KiB receive buffers.
pub const RX_BUFFER_SIZE: usize = 4096;
pub const MAX_FRAME_SIZE: usize = 2346;

// ── Firmware image ─────────────────

pub const IWL_FW_MAGIC: u32 = 0x0a4c5749;
pub const FW_HEADER_SIZE: usize = 88;

/// TLV entry type (modern iwlwifi firmware format).
pub const TLV_SEC_RT: u32 = 19;
pub const TLV_SEC_INIT: u32 = 20;
pub const TLV_SEC_WOWLAN: u32 = 21;
pub const TLV_DEF_CALIB: u32 = 22;
pub const FW_CPU1_CPU2_SEPARATOR_SECTION: u32 = 0xFFFF_CCCC;
pub const FW_PAGING_SEPARATOR_SECTION: u32 = 0xAAAA_BBBB;

// ── HBUS register offsets ──────────

pub const HBUS_TARG_MEM_WADDR: u32 = (0x400 + 0x010) / 4;
pub const HBUS_TARG_MEM_WDAT: u32 = (0x400 + 0x018) / 4;
