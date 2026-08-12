//! Intel CNVi/Gen2 firmware discovery and image-layout validation.
//!
//! AX101 is not a 7265-compatible PCIe endpoint.  Its firmware is loaded by
//! the Gen2 context-info path: the host supplies DMA addresses for LMAC,
//! UMAC, paging, and (on AX210-family devices) the IML image.  Keep parsing
//! independent of MMIO so malformed firmware can never reach the device.

use alloc::vec;
use alloc::vec::Vec;

const FW_HEADER_LEN: usize = 88;
const TLV_HEADER_LEN: usize = 8;
const TLV_SEC_RT: u32 = 19;
const TLV_DEF_CALIB: u32 = 22;
const TLV_PHY_SKU: u32 = 23;
const TLV_CMD_VERSIONS: u32 = 48;
const TLV_IML: u32 = 52;
const CPU1_CPU2_SEPARATOR: u32 = 0xffff_cccc;
const PAGING_SEPARATOR: u32 = 0xaaaa_bbbb;

/// Gen2 Context Info v2 CSR offsets used by AX101/AX210-family devices.
pub const CSR_CTXT_INFO_BOOT_CTRL: u32 = 0x000;
pub const CSR_CTXT_INFO_ADDR: u32 = 0x118;
pub const CSR_IML_DATA_ADDR: u32 = 0x120;
pub const CSR_IML_SIZE_ADDR: u32 = 0x128;
pub const CSR_AUTO_FUNC_BOOT_ENA: u32 = 1 << 1;

const IWL_MAX_DRAM_ENTRY: usize = 64;
const IWL_NUM_DRAM_FSEQ_ENTRIES: usize = 8;
const PRPH_SCRATCH_MTR_MODE: u32 = 1 << 17;
const PRPH_SCRATCH_MTR_FORMAT_256B: u32 = 0x000c_0000;
const PRPH_SCRATCH_RB_SIZE_4K: u32 = 1 << 16;

/// Size of `struct iwl_prph_scratch::dram.common` in bytes. The FSEQ tail
/// is omitted from the context-info length for the API89 image.
pub const PRPH_SCRATCH_COMMON_SIZE: usize = 1660;
pub const PRPH_SCRATCH_SIZE: usize = core::mem::size_of::<PrphScratch>();
pub const CONTEXT_INFO_V2_SIZE: usize = core::mem::size_of::<ContextInfoV2>();
pub const GEN2_RX_BUFFER_SIZE: usize = 4096;

/// Linux's regulatory/NVM command group.
pub const REGULATORY_AND_NVM_GROUP: u8 = 0x0c;
/// `NVM_GET_INFO` subcommand.
pub const NVM_GET_INFO_CMD: u8 = 0x02;

/// Intel PCI vendor ID.
pub const INTEL_VENDOR_ID: u16 = 0x8086;
/// AX101-family CNVi device ID used by the N150 fixture in Ventoy.
pub const AX101_DEVICE_ID: u16 = 0x54f0;
/// AX101 subsystem ID recorded from the target machine.
pub const AX101_SUBSYSTEM_DEVICE_ID: u16 = 0x0244;

const FW_AX101_SO_89: &[u8] =
    include_bytes!("../../../bonder/iwlwifi/intel/iwlwifi/iwlwifi-so-a0-hr-b0-89.ucode");
const FW_QUZ_77: &[u8] =
    include_bytes!("../../../bonder/iwlwifi/intel/iwlwifi/iwlwifi-QuZ-a0-hr-b0-77.ucode");

/// Hardware family selected before any BAR/MMIO access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModernFamily {
    /// So/AX101 integrated CNVi, device ID 0x54f0.
    So,
    /// QuZ integrated CNVi, device ID 0x4df0.
    Quz,
}

impl ModernFamily {
    pub const fn from_device_id(device_id: u16) -> Option<Self> {
        match device_id {
            0x54f0 => Some(Self::So),
            0x4df0 => Some(Self::Quz),
            _ => None,
        }
    }

    pub const fn firmware(self) -> FirmwareBlob {
        match self {
            Self::So => FirmwareBlob {
                name: "iwlwifi-so-a0-hr-b0-89.ucode",
                data: FW_AX101_SO_89,
                api_min: 89,
                api_max: 89,
            },
            Self::Quz => FirmwareBlob {
                name: "iwlwifi-QuZ-a0-hr-b0-77.ucode",
                data: FW_QUZ_77,
                api_min: 77,
                api_max: 77,
            },
        }
    }

    /// RX ring sizes from the Linux family tables used by the API-era
    /// firmware carried in this tree.  AX101/So uses the AX210-sized ring;
    /// QuZ remains on the 22000-sized ring.
    pub const fn rx_queue_entries(self) -> usize {
        match self {
            Self::So => 4096,
            Self::Quz => 2048,
        }
    }
}

/// A firmware candidate selected from the Linux-compatible family mapping.
#[derive(Debug, Clone, Copy)]
pub struct FirmwareBlob {
    pub name: &'static str,
    pub data: &'static [u8],
    pub api_min: u32,
    pub api_max: u32,
}

/// One `IWL_UCODE_TLV_SEC_RT` image chunk.
#[derive(Debug, Clone, Copy)]
pub struct FirmwareSection<'a> {
    /// Device SRAM address encoded in the section prefix.
    pub offset: u32,
    /// Section bytes after the four-byte SRAM offset.
    pub data: &'a [u8],
}

/// A validated Gen2 firmware image.
#[derive(Debug)]
pub struct ModernFirmware<'a> {
    pub api: u32,
    pub build: u32,
    pub sections: Vec<FirmwareSection<'a>>,
    pub iml: Option<&'a [u8]>,
    pub phy_config: Option<u32>,
    pub default_calib: Option<CalibrationTriggers>,
    pub command_versions: Vec<CommandVersion>,
}

/// Default calibration bitmaps carried by `IWL_UCODE_TLV_DEF_CALIB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalibrationTriggers {
    pub flow: u32,
    pub event: u32,
}

/// One entry from Linux's `IWL_UCODE_TLV_CMD_VERSIONS` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandVersion {
    pub opcode: u8,
    pub group_id: u8,
    pub command: u8,
    pub notification: u8,
}

/// The stable prefix of Linux's `REGULATORY_NVM_GET_INFO_RSP` response.
/// Channel profiles are intentionally not copied here; the driver only uses
/// the metadata to gate the later MVM setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvmInfo {
    pub flags: u32,
    pub nvm_version: u16,
    pub board_type: u8,
    pub n_hw_addrs: u8,
    pub mac_sku_flags: u32,
    pub tx_chains: u32,
    pub rx_chains: u32,
    pub lar_enabled: u32,
    pub n_channels: u32,
}

/// Section groups in the order consumed by Linux's `iwl_pcie_init_fw_sec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareGroups {
    pub lmac: core::ops::Range<usize>,
    pub umac: core::ops::Range<usize>,
    pub paging: core::ops::Range<usize>,
}

/// Linux's `iwl_context_info_dram_nonfseq`, retained as a wire layout.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct ContextInfoDramMap {
    pub umac_img: [u64; IWL_MAX_DRAM_ENTRY],
    pub lmac_img: [u64; IWL_MAX_DRAM_ENTRY],
    pub virtual_img: [u64; IWL_MAX_DRAM_ENTRY],
}

impl Default for ContextInfoDramMap {
    fn default() -> Self {
        // All-zero DMA pointers are the documented unused-entry value.
        unsafe { core::mem::zeroed() }
    }
}

/// Linux's v2 peripheral scratch control block.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct PrphScratchCtrl {
    pub mac_id: u16,
    pub version: u16,
    pub size_dw: u16,
    pub version_reserved: u16,
    pub control_flags: u32,
    pub control_flags_ext: u32,
    pub pnvm_base_addr: u64,
    pub pnvm_size: u32,
    pub pnvm_reserved: u32,
    pub hwm_base_addr: u64,
    pub hwm_size: u32,
    pub hwm_debug_token_config: u32,
    pub free_rbd_addr: u64,
    pub rbd_reserved: u32,
    pub uefi_base_addr: u64,
    pub uefi_size: u32,
    pub uefi_reserved: u32,
    pub step_mbx_addr_0: u32,
    pub step_mbx_addr_1: u32,
}

/// Linux's v2 peripheral scratch structure.  FSEQ is deliberately present
/// even when unused; `prph_scratch_size` tells firmware which prefix is live.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct PrphScratch {
    pub ctrl: PrphScratchCtrl,
    pub fseq_override: u32,
    pub step_analog_params: u32,
    pub reserved: [u32; 8],
    pub dram: ContextInfoDramMap,
    pub fseq_img: [u64; IWL_NUM_DRAM_FSEQ_ENTRIES],
}

/// Linux's `IPC_CONTEXT_INFO_S` for AX210-family Gen2 devices.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct ContextInfoV2 {
    pub version: u16,
    pub size_dw: u16,
    pub config: u32,
    pub prph_info_base_addr: u64,
    pub cr_head_idx_arr_base_addr: u64,
    pub tr_tail_idx_arr_base_addr: u64,
    pub cr_tail_idx_arr_base_addr: u64,
    pub tr_head_idx_arr_base_addr: u64,
    pub cr_idx_arr_size: u16,
    pub tr_idx_arr_size: u16,
    pub mtr_base_addr: u64,
    pub mcr_base_addr: u64,
    pub mtr_size: u16,
    pub mcr_size: u16,
    pub mtr_doorbell_vec: u16,
    pub mcr_doorbell_vec: u16,
    pub mtr_msi_vec: u16,
    pub mcr_msi_vec: u16,
    pub mtr_opt_header_size: u8,
    pub mtr_opt_footer_size: u8,
    pub mcr_opt_header_size: u8,
    pub mcr_opt_footer_size: u8,
    pub msg_rings_ctrl_flags: u16,
    pub prph_info_msi_vec: u16,
    pub prph_scratch_base_addr: u64,
    pub prph_scratch_size: u32,
    pub reserved: u32,
}

/// AX210/AX101 Gen2 transfer descriptor for the free-RBD ring.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct RxTransferDesc {
    pub rbid: u16,
    pub reserved: [u16; 3],
    pub addr: u64,
}

/// AX210/AX101 Gen2 completion descriptor for the used-RBD ring.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct RxCompletionDesc {
    pub reserved1: u32,
    pub rbid: u16,
    pub flags: u8,
    pub reserved2: [u8; 25],
}

/// The packet header at the front of a Gen2 receive buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxPacket<'a> {
    pub opcode: u8,
    pub group_id: u8,
    pub sequence: u16,
    pub payload: &'a [u8],
}

/// Gen2 TX buffer pointer. The packed 10-byte shape is required by firmware.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct TfhTb {
    pub tb_len: u16,
    pub addr: u64,
}

/// Gen2 TX frame descriptor. One 256-entry queue therefore occupies 64 KiB.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct TfhTfd {
    pub num_tbs: u16,
    pub tbs: [TfhTb; 25],
    pub pad: u32,
}

impl Default for TfhTfd {
    fn default() -> Self {
        unsafe { core::mem::zeroed() }
    }
}

pub const GEN2_TX_QUEUE_ENTRIES: usize = 256;
pub const GEN2_TX_QUEUE_BYTES: usize = core::mem::size_of::<TfhTfd>() * GEN2_TX_QUEUE_ENTRIES;
pub const GEN2_TFD_TB_COUNT: usize = 25;
pub const GEN2_COMMAND_QUEUE: u16 = 0;
pub const GEN2_FIRST_TB_SIZE: usize = 20;
// Linux's HR RF configuration starts with IWL_NUM_RBDS_HE (256 * 8) and
// iwl_trans_get_num_rbds() doubles it for AX210/So/Ty because the firmware
// cannot place multiple frames in one receive buffer.
pub const GEN2_RX_QUEUE_ENTRIES: usize = 4096;

/// Encode the 8-byte wide host-command header used by the Gen2 MVM queue.
/// `length` is the payload length, matching Linux's `HcmdHeaderWide`.
pub fn encode_wide_command(
    opcode: u8,
    group_id: u8,
    sequence: u16,
    version: u8,
    payload: &[u8],
) -> Result<Vec<u8>, FirmwareError> {
    let length = u16::try_from(payload.len()).map_err(|_| FirmwareError::CommandTooLong)?;
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(&[opcode, group_id]);
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&[0, version]);
    bytes.extend_from_slice(payload);
    Ok(bytes)
}

/// Encode one Linux `struct iwl_tfh_tfd` entry for the Gen2 TX ring.
pub fn encode_tfh_tfd(tbs: &[(u64, u16)]) -> Result<Vec<u8>, FirmwareError> {
    if tbs.is_empty() || tbs.len() > GEN2_TFD_TB_COUNT {
        return Err(FirmwareError::InvalidTransferBufferCount);
    }
    let mut bytes = vec![0u8; core::mem::size_of::<TfhTfd>()];
    bytes[..2].copy_from_slice(&(tbs.len() as u16).to_le_bytes());
    for (index, &(addr, length)) in tbs.iter().enumerate() {
        let offset = 2 + index * core::mem::size_of::<TfhTb>();
        bytes[offset..offset + 2].copy_from_slice(&length.to_le_bytes());
        bytes[offset + 2..offset + 10].copy_from_slice(&addr.to_le_bytes());
    }
    Ok(bytes)
}

/// Decode one `iwl_rx_packet` from a Gen2 receive buffer.
pub fn decode_rx_packet(bytes: &[u8]) -> Result<RxPacket<'_>, FirmwareError> {
    if bytes.len() < 12 {
        return Err(FirmwareError::InvalidRxPacket);
    }
    // Linux's length field covers the command header and payload, but not
    // the leading four-byte len/flags word.
    let frame_len = (u32::from_le_bytes(bytes[..4].try_into().unwrap()) & 0x3fff) as usize;
    let frame_end = 4usize
        .checked_add(frame_len)
        .ok_or(FirmwareError::InvalidRxPacket)?;
    if frame_len < 8 || frame_end > bytes.len() {
        return Err(FirmwareError::InvalidRxPacket);
    }
    Ok(RxPacket {
        opcode: bytes[4],
        group_id: bytes[5],
        sequence: u16::from_le_bytes([bytes[6], bytes[7]]),
        payload: &bytes[12..frame_end],
    })
}

/// DMA addresses corresponding to the three firmware section groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareDmaMap {
    pub lmac: Vec<u64>,
    pub umac: Vec<u64>,
    pub paging: Vec<u64>,
}

/// Addresses consumed by Linux's `IPC_CONTEXT_INFO_S`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextInfoAddresses {
    pub prph_info: u64,
    pub rx_status: u64,
    pub tr_tail: u64,
    pub cr_tail: u64,
    pub tx_queue: u64,
    pub used_rx: u64,
    pub prph_scratch: u64,
}

/// Encode the v2 peripheral scratch page exactly as the packed Linux structs
/// lay it out. Keeping this byte-oriented avoids creating unaligned Rust
/// references to `#[repr(packed)]` fields.
pub fn encode_prph_scratch(
    mac_id: u16,
    rx_free: u64,
    map: &FirmwareDmaMap,
) -> Result<Vec<u8>, FirmwareError> {
    if map.lmac.len() > IWL_MAX_DRAM_ENTRY
        || map.umac.len() > IWL_MAX_DRAM_ENTRY
        || map.paging.len() > IWL_MAX_DRAM_ENTRY
    {
        return Err(FirmwareError::SectionAddressCountMismatch);
    }
    let mut bytes = alloc::vec![0u8; PRPH_SCRATCH_SIZE];
    put_u16(&mut bytes, 0, mac_id);
    put_u16(&mut bytes, 2, 0);
    put_u16(&mut bytes, 4, (PRPH_SCRATCH_SIZE / 4) as u16);
    put_u32(&mut bytes, 8, default_prph_scratch_control_flags());
    put_u32(&mut bytes, 12, 0);
    put_u64(&mut bytes, 48, rx_free);

    // ctrl_cfg (84), fseq_override (4), step_analog_params (4), reserved[8]
    // (32) precede dram.common at offset 124.
    const DRAM_OFFSET: usize = 124;
    for (index, address) in map.umac.iter().copied().enumerate() {
        put_u64(&mut bytes, DRAM_OFFSET + index * 8, address);
    }
    for (index, address) in map.lmac.iter().copied().enumerate() {
        put_u64(
            &mut bytes,
            DRAM_OFFSET + IWL_MAX_DRAM_ENTRY * 8 + index * 8,
            address,
        );
    }
    for (index, address) in map.paging.iter().copied().enumerate() {
        put_u64(
            &mut bytes,
            DRAM_OFFSET + IWL_MAX_DRAM_ENTRY * 16 + index * 8,
            address,
        );
    }
    Ok(bytes)
}

/// Encode the v2 boot context. `cmd_queue_size` and `rx_queue_size` are
/// circular-buffer entry counts; Linux stores their logarithmic encodings.
pub fn encode_context_info_v2(
    addresses: ContextInfoAddresses,
    cmd_queue_size: usize,
    rx_queue_size: usize,
) -> Result<Vec<u8>, FirmwareError> {
    let cmd_log = queue_log2(cmd_queue_size).ok_or(FirmwareError::InvalidQueueSize)?;
    let rx_log = queue_log2(rx_queue_size).ok_or(FirmwareError::InvalidQueueSize)?;
    if cmd_log < 3 {
        return Err(FirmwareError::InvalidQueueSize);
    }

    let mut bytes = alloc::vec![0u8; CONTEXT_INFO_V2_SIZE];
    put_u16(&mut bytes, 0, 0);
    put_u16(&mut bytes, 2, (CONTEXT_INFO_V2_SIZE / 4) as u16);
    put_u64(&mut bytes, 8, addresses.prph_info);
    put_u64(&mut bytes, 16, addresses.rx_status);
    put_u64(&mut bytes, 24, addresses.tr_tail);
    put_u64(&mut bytes, 32, addresses.cr_tail);
    put_u64(&mut bytes, 52, addresses.tx_queue);
    put_u64(&mut bytes, 60, addresses.used_rx);
    put_u16(&mut bytes, 68, (cmd_log - 3) as u16);
    put_u16(&mut bytes, 70, rx_log as u16);
    put_u64(&mut bytes, 88, addresses.prph_scratch);
    put_u32(&mut bytes, 96, PRPH_SCRATCH_COMMON_SIZE as u32);
    Ok(bytes)
}

fn queue_log2(size: usize) -> Option<usize> {
    if size == 0 || !size.is_power_of_two() {
        return None;
    }
    Some(size.trailing_zeros() as usize)
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

impl FirmwareDmaMap {
    /// Build the address map in the same group order Linux uses.  The caller
    /// owns the DMA allocations; this function only validates cardinality.
    pub fn new(
        firmware: &ModernFirmware<'_>,
        lmac: &[u64],
        umac: &[u64],
        paging: &[u64],
    ) -> Result<Self, FirmwareError> {
        let groups = firmware.groups()?;
        if lmac.len() != groups.lmac.len()
            || umac.len() != groups.umac.len()
            || paging.len() != groups.paging.len()
        {
            return Err(FirmwareError::SectionAddressCountMismatch);
        }
        Ok(Self {
            lmac: lmac.to_vec(),
            umac: umac.to_vec(),
            paging: paging.to_vec(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareError {
    TooShort,
    BadMagic,
    InvalidTlvBounds,
    SectionWithoutOffset,
    MissingCpuSeparator,
    MissingPagingSeparator,
    UnexpectedSeparator,
    EmptyImageGroup,
    MissingIml,
    ApiOutOfRange,
    SectionAddressCountMismatch,
    InvalidQueueSize,
    CommandTooLong,
    InvalidTransferBufferCount,
    InvalidRxPacket,
    InvalidMetadata,
    InvalidNvmInfo,
}

/// Values Linux programs in the AX210-family v2 scratch control word for a
/// 4 KiB receive buffer and 256-byte message-transfer descriptors.
pub const fn default_prph_scratch_control_flags() -> u32 {
    PRPH_SCRATCH_MTR_MODE | PRPH_SCRATCH_MTR_FORMAT_256B | PRPH_SCRATCH_RB_SIZE_4K
}

impl<'a> ModernFirmware<'a> {
    /// Parse the Linux TLV stream without allocating or touching hardware.
    pub fn parse(data: &'a [u8]) -> Result<Self, FirmwareError> {
        if data.len() < FW_HEADER_LEN {
            return Err(FirmwareError::TooShort);
        }
        if &data[4..8] != b"IWL\n" {
            return Err(FirmwareError::BadMagic);
        }

        let api = le32(&data[72..76]);
        let build = le32(&data[76..80]);
        let mut cursor = FW_HEADER_LEN;
        let mut sections = Vec::new();
        let mut iml = None;
        let mut phy_config = None;
        let mut default_calib = None;
        let mut command_versions = Vec::new();

        while cursor < data.len() {
            let end = cursor
                .checked_add(TLV_HEADER_LEN)
                .and_then(|v| v.checked_add(le32_at(data, cursor + 4)? as usize))
                .ok_or(FirmwareError::InvalidTlvBounds)?;
            if end > data.len() {
                return Err(FirmwareError::InvalidTlvBounds);
            }

            let typ = le32(&data[cursor..cursor + 4]);
            let len = le32(&data[cursor + 4..cursor + 8]) as usize;
            let payload_start = cursor + TLV_HEADER_LEN;
            let payload = &data[payload_start..payload_start + len];
            match typ {
                TLV_SEC_RT => {
                    if len < 4 {
                        return Err(FirmwareError::SectionWithoutOffset);
                    }
                    sections.push(FirmwareSection {
                        offset: le32(&payload[..4]),
                        data: &payload[4..],
                    });
                }
                TLV_IML => {
                    if iml.replace(payload).is_some() {
                        return Err(FirmwareError::InvalidTlvBounds);
                    }
                }
                TLV_DEF_CALIB => {
                    if len != 12 {
                        return Err(FirmwareError::InvalidMetadata);
                    }
                    // Linux indexes this TLV by ucode_type. Type 0 is the
                    // regular/runtime image selected for AX101.
                    if le32(&payload[..4]) == 0 {
                        default_calib = Some(CalibrationTriggers {
                            flow: le32(&payload[4..8]),
                            event: le32(&payload[8..12]),
                        });
                    }
                }
                TLV_PHY_SKU => {
                    if len != 4 {
                        return Err(FirmwareError::InvalidMetadata);
                    }
                    phy_config = Some(le32(payload));
                }
                TLV_CMD_VERSIONS => {
                    if len % 4 != 0 {
                        return Err(FirmwareError::InvalidMetadata);
                    }
                    for entry in payload.chunks_exact(4) {
                        command_versions.push(CommandVersion {
                            opcode: entry[0],
                            group_id: entry[1],
                            command: entry[2],
                            notification: entry[3],
                        });
                    }
                }
                _ => {}
            }
            cursor = end;
        }

        let image = Self {
            api,
            build,
            sections,
            iml,
            phy_config,
            default_calib,
            command_versions,
        };
        image.groups()?;
        Ok(image)
    }

    /// Return the exact three ranges expected by Linux's Gen2 loader.
    pub fn groups(&self) -> Result<FirmwareGroups, FirmwareError> {
        let first = self
            .sections
            .iter()
            .position(|section| section.offset == CPU1_CPU2_SEPARATOR)
            .ok_or(FirmwareError::MissingCpuSeparator)?;
        let second = self
            .sections
            .iter()
            .position(|section| section.offset == PAGING_SEPARATOR)
            .ok_or(FirmwareError::MissingPagingSeparator)?;
        if first == 0 || second <= first + 1 || second + 1 >= self.sections.len() {
            return Err(FirmwareError::EmptyImageGroup);
        }
        if self.sections[..first]
            .iter()
            .chain(self.sections[first + 1..second].iter())
            .chain(self.sections[second + 1..].iter())
            .any(|section| {
                section.offset == CPU1_CPU2_SEPARATOR || section.offset == PAGING_SEPARATOR
            })
        {
            return Err(FirmwareError::UnexpectedSeparator);
        }
        Ok(FirmwareGroups {
            lmac: 0..first,
            umac: first + 1..second,
            paging: second + 1..self.sections.len(),
        })
    }

    /// Validate a device's advertised API against the selected Linux image.
    pub fn validate_api(&self, blob: FirmwareBlob) -> Result<(), FirmwareError> {
        if self.api < blob.api_min || self.api > blob.api_max {
            return Err(FirmwareError::ApiOutOfRange);
        }
        Ok(())
    }

    /// Look up a command version using Linux's fallback semantics.
    pub fn command_version(&self, opcode: u8, group_id: u8, default: u8) -> u8 {
        self.command_versions
            .iter()
            .find(|entry| entry.opcode == opcode && entry.group_id == group_id)
            .map(|entry| entry.command)
            .filter(|version| *version != 99)
            .unwrap_or(default)
    }

    /// Look up the response/notification version paired with a command.
    pub fn notification_version(&self, opcode: u8, group_id: u8, default: u8) -> u8 {
        self.command_versions
            .iter()
            .find(|entry| entry.opcode == opcode && entry.group_id == group_id)
            .map(|entry| entry.notification)
            .filter(|version| *version != 99)
            .unwrap_or(default)
    }
}

/// Decode the fixed metadata prefix of `NVM_GET_INFO` for Linux response
/// versions 3, 4, and 5.  Exact sizes are checked because Linux rejects a
/// response whose advertised version and payload disagree.
pub fn decode_nvm_info(payload: &[u8], notification_version: u8) -> Result<NvmInfo, FirmwareError> {
    let expected_len = match notification_version {
        5 => 488,
        4 => 468,
        _ => 148,
    };
    if payload.len() != expected_len {
        return Err(FirmwareError::InvalidNvmInfo);
    }
    let n_channels = match notification_version {
        4 | 5 => le32(&payload[24..28]),
        _ => 51,
    };
    Ok(NvmInfo {
        flags: le32(&payload[0..4]),
        nvm_version: u16::from_le_bytes([payload[4], payload[5]]),
        board_type: payload[6],
        n_hw_addrs: payload[7],
        mac_sku_flags: le32(&payload[8..12]),
        tx_chains: le32(&payload[12..16]),
        rx_chains: le32(&payload[16..20]),
        lar_enabled: le32(&payload[20..24]),
        n_channels,
    })
}

/// Select the firmware corresponding to an Intel modern CNVi device ID.
pub const fn select_firmware(device_id: u16) -> Option<FirmwareBlob> {
    match ModernFamily::from_device_id(device_id) {
        Some(family) => Some(family.firmware()),
        None => None,
    }
}

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le32_at(bytes: &[u8], start: usize) -> Option<u32> {
    bytes.get(start..start + 4).map(le32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_maps_to_so_api89() {
        let blob = select_firmware(AX101_DEVICE_ID).expect("AX101 firmware");
        assert_eq!(blob.name, "iwlwifi-so-a0-hr-b0-89.ucode");
        assert_eq!((blob.api_min, blob.api_max), (89, 89));
        let fw = ModernFirmware::parse(blob.data).expect("valid AX101 image");
        assert_eq!(fw.api, 89);
        assert!(fw.iml.is_some());
        assert_eq!(fw.phy_config, Some(0x0033_0018));
        assert_eq!(
            fw.default_calib,
            Some(CalibrationTriggers {
                flow: 0x5f16_01d3,
                event: 0x5b06_958b,
            })
        );
        assert_eq!(fw.command_version(0x6a, 0x01, 0), 3);
        assert_eq!(fw.command_version(0x00, 0x0c, 0), 1);
        assert_eq!(
            fw.command_version(NVM_GET_INFO_CMD, REGULATORY_AND_NVM_GROUP, 0),
            1
        );
        assert_eq!(
            fw.notification_version(NVM_GET_INFO_CMD, REGULATORY_AND_NVM_GROUP, 3),
            4
        );
        fw.validate_api(blob).expect("API selected correctly");
    }

    #[test]
    fn target_subsystem_fixture_is_explicit() {
        assert_eq!(INTEL_VENDOR_ID, 0x8086);
        assert_eq!(AX101_SUBSYSTEM_DEVICE_ID, 0x0244);
    }

    #[test]
    fn linux_gen2_group_counts_match_ax101_image() {
        let blob = ModernFamily::So.firmware();
        let fw = ModernFirmware::parse(blob.data).expect("valid image");
        let groups = fw.groups().expect("three image groups");
        assert_eq!(groups.lmac.len(), 15);
        assert_eq!(groups.umac.len(), 15);
        assert_eq!(groups.paging.len(), 20);
        assert!(
            fw.sections[groups.lmac]
                .iter()
                .all(|s| s.data.len() <= 32768)
        );
    }

    #[test]
    fn context_info_v2_layout_matches_linux_wire_sizes() {
        assert_eq!(core::mem::size_of::<ContextInfoDramMap>(), 1536);
        assert_eq!(core::mem::size_of::<PrphScratchCtrl>(), 84);
        assert_eq!(core::mem::size_of::<PrphScratch>(), 1724);
        assert_eq!(core::mem::size_of::<ContextInfoV2>(), 104);
        assert_eq!(core::mem::size_of::<RxTransferDesc>(), 16);
        assert_eq!(core::mem::size_of::<RxCompletionDesc>(), 32);
        assert_eq!(core::mem::size_of::<TfhTb>(), 10);
        assert_eq!(core::mem::size_of::<TfhTfd>(), 256);
        assert_eq!(GEN2_TX_QUEUE_BYTES, 64 * 1024);
        assert_eq!(default_prph_scratch_control_flags(), 0x000f_0000);
        assert_eq!(CSR_CTXT_INFO_ADDR, 0x118);
        assert_eq!(CSR_IML_DATA_ADDR, 0x120);
    }

    #[test]
    fn dma_map_preserves_linux_group_order_and_counts() {
        let blob = ModernFamily::So.firmware();
        let fw = ModernFirmware::parse(blob.data).expect("valid image");
        let lmac: Vec<_> = (0..15).map(|i| 0x1000 + i * 0x1000).collect();
        let umac: Vec<_> = (0..15).map(|i| 0x2000 + i * 0x1000).collect();
        let paging: Vec<_> = (0..20).map(|i| 0x3000 + i * 0x1000).collect();
        let map = FirmwareDmaMap::new(&fw, &lmac, &umac, &paging).expect("valid DMA map");
        assert_eq!(map.lmac, lmac);
        assert_eq!(map.umac, umac);
        assert_eq!(map.paging, paging);
        assert!(matches!(
            FirmwareDmaMap::new(&fw, &lmac[..14], &umac, &paging),
            Err(FirmwareError::SectionAddressCountMismatch)
        ));
    }

    #[test]
    fn context_info_encoders_write_linux_offsets() {
        let blob = ModernFamily::So.firmware();
        let fw = ModernFirmware::parse(blob.data).expect("valid image");
        let lmac: Vec<_> = (0..15).map(|i| 0x1000 + i as u64 * 0x1000).collect();
        let umac: Vec<_> = (0..15).map(|i| 0x2000 + i as u64 * 0x1000).collect();
        let paging: Vec<_> = (0..20).map(|i| 0x3000 + i as u64 * 0x1000).collect();
        let map = FirmwareDmaMap::new(&fw, &lmac, &umac, &paging).expect("valid DMA map");
        let scratch = encode_prph_scratch(0x1234, 0x1111, &map).expect("scratch");
        assert_eq!(scratch.len(), PRPH_SCRATCH_SIZE);
        assert_eq!(&scratch[0..2], &0x1234u16.to_le_bytes());
        assert_eq!(&scratch[48..56], &0x1111u64.to_le_bytes());
        assert_eq!(&scratch[124..132], &0x2000u64.to_le_bytes());
        assert_eq!(
            &scratch[124 + 64 * 8..124 + 65 * 8],
            &0x1000u64.to_le_bytes()
        );
        assert_eq!(
            &scratch[124 + 128 * 8..124 + 129 * 8],
            &0x3000u64.to_le_bytes()
        );

        let context = encode_context_info_v2(
            ContextInfoAddresses {
                prph_info: 1,
                rx_status: 2,
                tr_tail: 3,
                cr_tail: 4,
                tx_queue: 5,
                used_rx: 6,
                prph_scratch: 7,
            },
            128,
            GEN2_RX_QUEUE_ENTRIES,
        )
        .expect("context");
        assert_eq!(context.len(), CONTEXT_INFO_V2_SIZE);
        assert_eq!(&context[52..60], &5u64.to_le_bytes());
        assert_eq!(&context[60..68], &6u64.to_le_bytes());
        assert_eq!(&context[68..70], &4u16.to_le_bytes());
        assert_eq!(&context[70..72], &12u16.to_le_bytes());
        assert_eq!(&context[88..96], &7u64.to_le_bytes());
        assert_eq!(
            &context[96..100],
            &(PRPH_SCRATCH_COMMON_SIZE as u32).to_le_bytes()
        );
        assert!(matches!(
            encode_context_info_v2(
                ContextInfoAddresses {
                    prph_info: 0,
                    rx_status: 0,
                    tr_tail: 0,
                    cr_tail: 0,
                    tx_queue: 0,
                    used_rx: 0,
                    prph_scratch: 0,
                },
                127,
                512,
            ),
            Err(FirmwareError::InvalidQueueSize)
        ));
    }

    #[test]
    fn wide_command_and_gen2_tfd_match_linux_wire_order() {
        let command = encode_wide_command(0x03, 0x02, 0, 0, &[0x02, 0, 0, 0])
            .expect("INIT_EXTENDED_CFG command");
        assert_eq!(&command[..8], &[0x03, 0x02, 0, 0, 4, 0, 0, 0]);
        assert_eq!(&command[8..], &[0x02, 0, 0, 0]);

        let tfd = encode_tfh_tfd(&[(0x1122_3344_5566_7788, 12), (0xaabb_ccdd_eeff_0011, 64)])
            .expect("TFD");
        assert_eq!(tfd.len(), core::mem::size_of::<TfhTfd>());
        assert_eq!(&tfd[..2], &[2, 0]);
        assert_eq!(&tfd[2..4], &[12, 0]);
        assert_eq!(&tfd[4..12], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&tfd[12..14], &[64, 0]);
        assert_eq!(&tfd[14..22], &0xaabb_ccdd_eeff_0011u64.to_le_bytes());
        assert!(matches!(
            encode_tfh_tfd(&[]),
            Err(FirmwareError::InvalidTransferBufferCount)
        ));

        let mut rx = vec![0u8; 16];
        rx[..4].copy_from_slice(&12u32.to_le_bytes());
        rx[4..8].copy_from_slice(&[0x03, 0x02, 0, 0]);
        rx[12..].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
        let packet = decode_rx_packet(&rx).expect("RX packet");
        assert_eq!(packet.opcode, 0x03);
        assert_eq!(packet.group_id, 0x02);
        assert_eq!(packet.payload, &[0xaa, 0xbb, 0xcc, 0xdd]);
    }

    #[test]
    fn nvm_info_decoder_matches_linux_response_layouts() {
        let mut response = vec![0u8; 488];
        response[0..4].copy_from_slice(&1u32.to_le_bytes());
        response[4..6].copy_from_slice(&0x1234u16.to_le_bytes());
        response[6] = 7;
        response[7] = 1;
        response[8..12].copy_from_slice(&0x1fu32.to_le_bytes());
        response[12..16].copy_from_slice(&1u32.to_le_bytes());
        response[16..20].copy_from_slice(&1u32.to_le_bytes());
        response[20..24].copy_from_slice(&1u32.to_le_bytes());
        response[24..28].copy_from_slice(&115u32.to_le_bytes());
        let info = decode_nvm_info(&response, 5).expect("API v5 NVM response");
        assert_eq!(info.nvm_version, 0x1234);
        assert_eq!(info.n_channels, 115);
        assert!(matches!(
            decode_nvm_info(&response[..148], 5),
            Err(FirmwareError::InvalidNvmInfo)
        ));
    }

    #[test]
    fn linux_family_rx_ring_sizes_are_preserved() {
        assert_eq!(ModernFamily::So.rx_queue_entries(), 4096);
        assert_eq!(ModernFamily::Quz.rx_queue_entries(), 2048);
        assert_eq!(GEN2_COMMAND_QUEUE, 0);
    }

    #[test]
    fn quz_is_kept_separate_from_ax101() {
        let blob = select_firmware(0x4df0).expect("QuZ firmware");
        assert_eq!(blob.api_max, 77);
        let fw = ModernFirmware::parse(blob.data).expect("valid QuZ image");
        assert_eq!(fw.api, 77);
        assert_ne!(blob.name, select_firmware(AX101_DEVICE_ID).unwrap().name);
    }

    #[test]
    fn malformed_tlv_is_rejected_before_section_use() {
        let mut bytes = [0u8; FW_HEADER_LEN + TLV_HEADER_LEN];
        bytes[4..8].copy_from_slice(b"IWL\n");
        bytes[72..76].copy_from_slice(&89u32.to_le_bytes());
        bytes[FW_HEADER_LEN + 4..FW_HEADER_LEN + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            ModernFirmware::parse(&bytes),
            Err(FirmwareError::InvalidTlvBounds)
        ));
    }
}
