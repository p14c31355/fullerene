//! Register definitions, PCI identifiers, and firmware constants for
//! Intel Wireless 7265 (iwlwifi 7000 series).

// ── PCI identifiers ────────────────

pub const IWL_PCI_VENDOR: u16 = 0x8086;
pub const IWL_DEVICE_IDS: &[u16] = &[0x095b, 0x095a, 0x08b1, 0x08b2];

// ── CSR registers ──────────────────

pub const CSR_HW_REV: u32 = 0x028 / 4;
pub const CSR_HW_RF_ID: u32 = 0x034 / 4;
pub const CSR_GIO: u32 = 0x03C / 4;
pub const CSR_UCODE_GP1: u32 = 0x054 / 4;
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
pub const CSR_GP_CNTRL_MAC_CLOCK_READY: u32 = 1 << 0;

/// FH register for RX ring base address (BADR).
pub const FH_RSCSR_CHNL0_RBDCB_BASE: u32 = 0x0B8 / 4;
/// FH register for RX ring read pointer (head index, updated by hardware).
pub const FH_RSCSR_CHNL0_RBDCB_RPTR_REG: u32 = 0x0C0 / 4;
/// FH register for TX ring head index (written by hardware on completion).
pub const FH_TX_CHNL0_WPTR: u32 = 0x0A0 / 4;

// ── Firmware constants ─────────────

pub const IWL_FW_API_VER: u32 = 16;
pub const IWL_FW_MAX_SECTIONS: usize = 32;

/// TX queue configuration.
pub const TX_QUEUE_SIZE: usize = 256;
pub const RX_QUEUE_SIZE: usize = 256;
pub const MAX_FRAME_SIZE: usize = 2346;

// ── Firmware image ─────────────────

pub const IWL_FW_MAGIC: u32 = 0x0a4c5749;
pub const FW_HEADER_SIZE: usize = 88;

/// TLV entry type (modern iwlwifi firmware format).
pub const TLV_INST: u32 = 19;
pub const TLV_DATA: u32 = 20;
pub const TLV_INIT: u32 = 21;
pub const TLV_INIT_DATA: u32 = 22;
pub const TLV_SECDER: u32 = 29;
pub const TLV_SECDER_USNIFFER: u32 = 30;

// ── HBUS register offsets ──────────

pub const HBUS_TARG_MEM_WADDR: u32 = (0x400 + 0x010) / 4;
pub const HBUS_TARG_MEM_WDAT: u32 = (0x400 + 0x018) / 4;
