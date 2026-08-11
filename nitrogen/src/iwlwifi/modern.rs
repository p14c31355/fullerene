//! Intel CNVi/Gen2 firmware discovery and image-layout validation.
//!
//! AX101 is not a 7265-compatible PCIe endpoint.  Its firmware is loaded by
//! the Gen2 context-info path: the host supplies DMA addresses for LMAC,
//! UMAC, paging, and (on AX210-family devices) the IML image.  Keep parsing
//! independent of MMIO so malformed firmware can never reach the device.

use alloc::vec::Vec;

const FW_HEADER_LEN: usize = 88;
const TLV_HEADER_LEN: usize = 8;
const TLV_SEC_RT: u32 = 19;
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
pub const GEN2_RX_QUEUE_ENTRIES: usize = 512;

/// DMA addresses corresponding to the three firmware section groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareDmaMap {
    pub lmac: Vec<u64>,
    pub umac: Vec<u64>,
    pub paging: Vec<u64>,
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
                _ => {}
            }
            cursor = end;
        }

        let image = Self {
            api,
            build,
            sections,
            iml,
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
