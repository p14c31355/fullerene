//! Qualcomm UFS platform contract and guarded bring-up for AArch64.
//!
//! A Qualcomm UFS host is not a single MMIO window: the platform driver also
//! owns the UFS PHY, clocks, resets, regulators, interrupt routing, and
//! sometimes an IOMMU context. Keep the DT contract, Linux-aligned pure HCI
//! encoding, and side-effectful backends separate so a backend cannot touch
//! the platform without an exact contract.

use super::fdt;

#[cfg(fullerene_aarch64_bramble)]
use genome::block::{BlockDevice, BlockError};

const UFS_CONTROLLER: &[u8] = b"qcom,ufshc";
const UFS_PHY: &[u8] = b"qcom,ufs-phy-qmp-v4-lito";
const RPMH_SET_ACTIVE: u32 = 1;
const RPMH_SET_ALL: u32 = 3;

/// Fixed early-boot UFSHCI arena. It is deliberately a distinct linker
/// section from the USB gadget DMA pool; no SMMU or cache attribute is
/// inferred from the USB contract.
#[repr(C, align(1024))]
struct UfsDmaArena {
    transfer_list: [u8; 1024],
    task_list: [u8; 1024],
    // The reserved device-management slot uses a 4 KiB response UCD.
    devman_descriptor: [u8; 8192],
    command_descriptor: [u8; 2048],
    // One legacy PRD may describe up to 256 KiB. Keeping the whole transfer
    // in the reserved arena lets the block adapter serve bounded multi-block
    // reads without allocating DMA memory from the general heap.
    data: [u8; 256 * 1024],
}

#[unsafe(link_section = ".ufs_dma")]
#[used]
static mut UFS_DMA_ARENA: UfsDmaArena = UfsDmaArena {
    transfer_list: [0; 1024],
    task_list: [0; 1024],
    devman_descriptor: [0; 8192],
    command_descriptor: [0; 2048],
    data: [0; 256 * 1024],
};

#[allow(dead_code)]
struct UfsDmaLayout;

#[allow(dead_code)]
impl UfsDmaLayout {
    const TRANSFER_LIST_BYTES: usize = 1024;
    const TASK_LIST_BYTES: usize = 1024;
    const DEVMAN_DESCRIPTOR_BYTES: usize = 8192;
    const COMMAND_DESCRIPTOR_BYTES: usize = 2048;
    const DATA_BYTES: usize = 256 * 1024;

    /// Return the physical/identity address of the arena only for a future
    /// backend that has separately proved the UFS DMA ownership contract.
    #[cfg(fullerene_aarch64_bramble)]
    unsafe fn addresses() -> (u64, u64, u64, u64, u64) {
        (
            core::ptr::addr_of!(UFS_DMA_ARENA.transfer_list) as u64,
            core::ptr::addr_of!(UFS_DMA_ARENA.task_list) as u64,
            core::ptr::addr_of!(UFS_DMA_ARENA.devman_descriptor) as u64,
            core::ptr::addr_of!(UFS_DMA_ARENA.command_descriptor) as u64,
            core::ptr::addr_of!(UFS_DMA_ARENA.data) as u64,
        )
    }
}

/// UFSHCI layout and descriptor encoding. This is intentionally a pure
/// little-endian layer: the future driver can put these bytes in a DMA-safe
/// allocation only after the platform power/PHY contract has been claimed.
#[allow(dead_code)]
pub(crate) mod hci {
    pub(crate) const REG_CONTROLLER_CAPABILITIES: u32 = 0x00;
    pub(crate) const REG_UFS_VERSION: u32 = 0x08;
    pub(crate) const REG_INTERRUPT_STATUS: u32 = 0x20;
    pub(crate) const REG_INTERRUPT_ENABLE: u32 = 0x24;
    pub(crate) const REG_CONTROLLER_STATUS: u32 = 0x30;
    pub(crate) const REG_CONTROLLER_ENABLE: u32 = 0x34;
    pub(crate) const REG_UTP_TRANSFER_REQ_INT_AGG_CONTROL: u32 = 0x4c;
    pub(crate) const REG_UTP_TRANSFER_REQ_LIST_BASE_L: u32 = 0x50;
    pub(crate) const REG_UTP_TRANSFER_REQ_LIST_BASE_H: u32 = 0x54;
    pub(crate) const REG_UTP_TRANSFER_REQ_DOOR_BELL: u32 = 0x58;
    pub(crate) const REG_UTP_TRANSFER_REQ_LIST_CLEAR: u32 = 0x5c;
    pub(crate) const REG_UTP_TRANSFER_REQ_LIST_RUN_STOP: u32 = 0x60;
    pub(crate) const REG_UTP_TASK_REQ_LIST_BASE_L: u32 = 0x70;
    pub(crate) const REG_UTP_TASK_REQ_LIST_BASE_H: u32 = 0x74;
    pub(crate) const REG_UTP_TASK_REQ_DOOR_BELL: u32 = 0x78;
    pub(crate) const REG_UTP_TASK_REQ_LIST_CLEAR: u32 = 0x7c;
    pub(crate) const REG_UTP_TASK_REQ_LIST_RUN_STOP: u32 = 0x80;
    pub(crate) const REG_UIC_COMMAND: u32 = 0x90;
    pub(crate) const REG_UIC_COMMAND_ARG_1: u32 = 0x94;
    pub(crate) const REG_UIC_COMMAND_ARG_2: u32 = 0x98;
    pub(crate) const REG_UIC_COMMAND_ARG_3: u32 = 0x9c;
    pub(crate) const REGISTER_SPACE_SIZE: u32 = 0xa0;
    pub(crate) const INTERRUPT_UIC_ERROR: u32 = 1 << 2;
    pub(crate) const INTERRUPT_UIC_COMMAND_COMPLETION: u32 = 1 << 10;
    pub(crate) const INTERRUPT_UIC_LINK_LOST: u32 = 1 << 7;
    pub(crate) const UIC_COMMAND_DME_LINK_STARTUP: u32 = 0x16;
    pub(crate) const UIC_COMMAND_RESULT_MASK: u32 = 0xff;

    pub(crate) const STATUS_DEVICE_PRESENT: u32 = 1 << 0;
    pub(crate) const STATUS_TRANSFER_LIST_READY: u32 = 1 << 1;
    pub(crate) const STATUS_TASK_LIST_READY: u32 = 1 << 2;
    pub(crate) const STATUS_UIC_COMMAND_READY: u32 = 1 << 3;
    pub(crate) const STATUS_READY: u32 =
        STATUS_TRANSFER_LIST_READY | STATUS_TASK_LIST_READY | STATUS_UIC_COMMAND_READY;
    pub(crate) const CONTROLLER_ENABLE: u32 = 1;
    pub(crate) const TRANSFER_LIST_RUN_STOP: u32 = 1;
    pub(crate) const CAPABILITY_TRANSFER_SLOTS_MASK: u32 = 0x1f;
    pub(crate) const CAPABILITY_64BIT_ADDRESSING: u32 = 1 << 24;
    pub(crate) const PRD_DATA_BYTE_COUNT_MAX: u32 = 256 * 1024;
    pub(crate) const PRD_DATA_BYTE_COUNT_GRANULARITY: u32 = 4;
    pub(crate) const ALIGNED_UPIU_SIZE: u16 = 512;
    pub(crate) const ALIGNED_DEVMAN_RSP_SIZE: u16 = 4096;
    pub(crate) const UPIU_HEADER_SIZE: usize = 12;
    pub(crate) const UPIU_QUERY_SIZE: usize = 20;
    pub(crate) const UPIU_GENERAL_REQUEST_SIZE: usize = UPIU_HEADER_SIZE + UPIU_QUERY_SIZE;
    pub(crate) const COMMAND_DESCRIPTOR_ALIGNMENT: u64 = 128;
    pub(crate) const TRANSFER_REQUEST_LIST_ALIGNMENT: u64 = 1024;
    pub(crate) const TRANSFER_REQUEST_DESCRIPTOR_SIZE: usize = 32;
    pub(crate) const COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET: usize = 0;
    pub(crate) const COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET: usize =
        COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET + ALIGNED_UPIU_SIZE as usize;
    pub(crate) const COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET: usize =
        COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET + ALIGNED_UPIU_SIZE as usize;
    pub(crate) const DEVMAN_COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET: usize =
        ALIGNED_UPIU_SIZE as usize;
    pub(crate) const DEVMAN_COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET: usize =
        DEVMAN_COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET + ALIGNED_DEVMAN_RSP_SIZE as usize;

    pub(crate) const UTP_CMD_TYPE_SCSI: u8 = 0x0;
    pub(crate) const UTP_CMD_TYPE_UFS: u8 = 0x1;
    pub(crate) const UTP_CMD_TYPE_DEV_MANAGE: u8 = 0x2;
    pub(crate) const UTP_REQ_DESC_INT_CMD: u32 = 0x0100_0000;
    pub(crate) const UTP_REQ_DESC_CRYPTO_ENABLE_CMD: u32 = 0x0080_0000;
    pub(crate) const UTP_NO_DATA_TRANSFER: u32 = 0x0000_0000;
    pub(crate) const UTP_HOST_TO_DEVICE: u32 = 0x0200_0000;
    pub(crate) const UTP_DEVICE_TO_HOST: u32 = 0x0400_0000;
    pub(crate) const OCS_SUCCESS: u8 = 0x0;
    pub(crate) const OCS_INVALID_COMMAND_STATUS: u8 = 0xf;
    pub(crate) const MASK_OCS: u8 = 0xf;

    pub(crate) const UPIU_TRANSACTION_NOP_OUT: u8 = 0x00;
    pub(crate) const UPIU_TRANSACTION_COMMAND: u8 = 0x01;
    pub(crate) const UPIU_TRANSACTION_QUERY_REQ: u8 = 0x16;
    pub(crate) const UPIU_TRANSACTION_NOP_IN: u8 = 0x20;
    pub(crate) const UPIU_TRANSACTION_RESPONSE: u8 = 0x21;
    pub(crate) const UPIU_TRANSACTION_QUERY_RSP: u8 = 0x36;
    pub(crate) const UPIU_CMD_FLAGS_NONE: u8 = 0x00;
    pub(crate) const UPIU_CMD_FLAGS_WRITE: u8 = 0x20;
    pub(crate) const UPIU_CMD_FLAGS_READ: u8 = 0x40;
    pub(crate) const UPIU_COMMAND_SET_TYPE_SCSI: u8 = 0x0;
    pub(crate) const UPIU_COMMAND_SET_TYPE_UFS: u8 = 0x1;
    pub(crate) const UPIU_COMMAND_SET_TYPE_QUERY: u8 = 0x2;
    pub(crate) const UPIU_QUERY_FUNC_STANDARD_READ_REQUEST: u8 = 0x01;
    pub(crate) const UPIU_QUERY_FUNC_STANDARD_WRITE_REQUEST: u8 = 0x81;
    pub(crate) const UPIU_QUERY_OPCODE_READ_DESC: u8 = 0x01;
    pub(crate) const UPIU_QUERY_OPCODE_WRITE_DESC: u8 = 0x02;
    pub(crate) const QUERY_DESC_MIN_SIZE: u16 = 2;
    pub(crate) const QUERY_DESC_MAX_SIZE: u16 = 255;
    pub(crate) const SCSI_READ10_OPCODE: u8 = 0x28;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct ControllerCapabilities {
        pub raw: u32,
    }

    impl ControllerCapabilities {
        pub(crate) const fn transfer_slots(self) -> u8 {
            ((self.raw & CAPABILITY_TRANSFER_SLOTS_MASK) + 1) as u8
        }

        pub(crate) const fn supports_64bit_addressing(self) -> bool {
            self.raw & CAPABILITY_64BIT_ADDRESSING != 0
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct ControllerStatus {
        pub raw: u32,
    }

    impl ControllerStatus {
        pub(crate) const fn device_present(self) -> bool {
            self.raw & STATUS_DEVICE_PRESENT != 0
        }

        pub(crate) const fn lists_ready(self) -> bool {
            self.raw & STATUS_READY == STATUS_READY
        }
    }

    /// The common 16-byte UTRD header.
    ///
    /// The four dwords are little-endian on the wire. In particular, the
    /// command type and data direction are bitfields in DW0, while OCS is the
    /// low nibble of DW2. They are not a protocol command tag or a UPIU
    /// header; the controller associates a tag with the transfer-list slot.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct RequestHeader {
        pub command_type: u8,
        pub data_direction: u32,
        pub ehs_length: u8,
        pub interrupt: bool,
        pub enable_crypto: bool,
        pub overall_command_status: u8,
    }

    impl RequestHeader {
        pub(crate) const BYTE_SIZE: usize = 16;

        pub(crate) fn new(command_type: u8, data_direction: u32, interrupt: bool) -> Option<Self> {
            let valid_direction = matches!(
                data_direction,
                UTP_NO_DATA_TRANSFER | UTP_HOST_TO_DEVICE | UTP_DEVICE_TO_HOST
            );
            (command_type < 16 && valid_direction).then_some(Self {
                command_type,
                data_direction,
                ehs_length: 0,
                interrupt,
                enable_crypto: false,
                overall_command_status: OCS_INVALID_COMMAND_STATUS,
            })
        }

        pub(crate) fn to_le_bytes(self) -> [u8; Self::BYTE_SIZE] {
            let mut bytes = [0; Self::BYTE_SIZE];
            let mut dword_0 = self.data_direction
                | ((self.command_type as u32 & 0xf) << 28)
                | ((self.ehs_length as u32) << 8);
            if self.interrupt {
                dword_0 |= UTP_REQ_DESC_INT_CMD;
            }
            if self.enable_crypto {
                dword_0 |= UTP_REQ_DESC_CRYPTO_ENABLE_CMD;
            }
            bytes[0..4].copy_from_slice(&dword_0.to_le_bytes());
            bytes[8..12].copy_from_slice(
                &(u32::from(self.overall_command_status) & u32::from(MASK_OCS)).to_le_bytes(),
            );
            bytes
        }
    }

    /// The 32-byte legacy single-queue UTP transfer request descriptor.
    ///
    /// Length and offset fields use UFSHCI double-word units on the normal
    /// path. A controller quirk may select byte-granular fields; that choice
    /// belongs to the controller backend and is intentionally not hidden by
    /// this byte serializer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct TransferRequestDescriptor {
        pub header: RequestHeader,
        pub command_desc_base_addr: u64,
        pub response_upiu_length: u16,
        pub response_upiu_offset: u16,
        pub prd_table_length: u16,
        pub prd_table_offset: u16,
    }

    impl TransferRequestDescriptor {
        pub(crate) const BYTE_SIZE: usize = TRANSFER_REQUEST_DESCRIPTOR_SIZE;

        pub(crate) fn to_le_bytes(self) -> [u8; Self::BYTE_SIZE] {
            let mut bytes = [0; Self::BYTE_SIZE];
            bytes[0..16].copy_from_slice(&self.header.to_le_bytes());
            bytes[16..24].copy_from_slice(&self.command_desc_base_addr.to_le_bytes());
            bytes[24..26].copy_from_slice(&self.response_upiu_length.to_le_bytes());
            bytes[26..28].copy_from_slice(&self.response_upiu_offset.to_le_bytes());
            bytes[28..30].copy_from_slice(&self.prd_table_length.to_le_bytes());
            bytes[30..32].copy_from_slice(&self.prd_table_offset.to_le_bytes());
            bytes
        }
    }

    /// One UFSHCI physical-region descriptor (PRD).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct PhysicalRegionDescriptor {
        pub address: u64,
        pub byte_count: u32,
    }

    impl PhysicalRegionDescriptor {
        pub(crate) const BYTE_SIZE: usize = 16;

        /// Construct a PRD whose encoded count is `(byte_count - 1)`.
        ///
        /// UFSHCI requires a non-zero, four-byte-granular segment no larger
        /// than 256 KiB. The alignment of `address` is an IOMMU/DMA contract,
        /// so it is checked by the allocator/backend rather than here.
        pub(crate) fn new(address: u64, byte_count: u32) -> Option<Self> {
            (byte_count != 0
                && byte_count <= PRD_DATA_BYTE_COUNT_MAX
                && byte_count % PRD_DATA_BYTE_COUNT_GRANULARITY == 0)
                .then_some(Self {
                    address,
                    byte_count,
                })
        }

        pub(crate) fn to_le_bytes(self) -> [u8; Self::BYTE_SIZE] {
            let mut bytes = [0; Self::BYTE_SIZE];
            bytes[0..8].copy_from_slice(&self.address.to_le_bytes());
            bytes[12..16].copy_from_slice(&(self.byte_count - 1).to_le_bytes());
            bytes
        }
    }

    /// The 12-byte big-endian UPIU header used inside a UTP command
    /// descriptor. `function` is the query or task-management function byte;
    /// it is zero for a normal SCSI command.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct UpiuHeader {
        pub transaction_code: u8,
        pub flags: u8,
        pub lun: u8,
        pub task_tag: u8,
        pub initiator_id: u8,
        pub command_set_type: u8,
        pub function: u8,
        pub response: u8,
        pub status: u8,
        pub ehs_length: u8,
        pub device_information: u8,
        pub data_segment_length: u16,
    }

    impl UpiuHeader {
        pub(crate) const BYTE_SIZE: usize = UPIU_HEADER_SIZE;

        pub(crate) fn to_be_bytes(self) -> [u8; Self::BYTE_SIZE] {
            let mut bytes = [0; Self::BYTE_SIZE];
            bytes[0] = self.transaction_code;
            bytes[1] = self.flags;
            bytes[2] = self.lun;
            bytes[3] = self.task_tag;
            bytes[4] = (self.command_set_type & 0xf) | ((self.initiator_id & 0xf) << 4);
            bytes[5] = self.function;
            bytes[6] = self.response;
            bytes[7] = self.status;
            bytes[8] = self.ehs_length;
            bytes[9] = self.device_information;
            bytes[10..12].copy_from_slice(&self.data_segment_length.to_be_bytes());
            bytes
        }
    }

    /// The 20-byte query-specific OSF area following a UPIU header.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct UpiuQuery {
        pub opcode: u8,
        pub idn: u8,
        pub index: u8,
        pub selector: u8,
        pub length: u16,
        pub value: u32,
    }

    impl UpiuQuery {
        pub(crate) const BYTE_SIZE: usize = UPIU_QUERY_SIZE;

        pub(crate) fn to_be_bytes(self) -> [u8; Self::BYTE_SIZE] {
            let mut bytes = [0; Self::BYTE_SIZE];
            bytes[0] = self.opcode;
            bytes[1] = self.idn;
            bytes[2] = self.index;
            bytes[3] = self.selector;
            bytes[6..8].copy_from_slice(&self.length.to_be_bytes());
            bytes[8..12].copy_from_slice(&self.value.to_be_bytes());
            bytes
        }
    }

    /// Prepare the fixed-size NOP device-management command used to prove
    /// that link startup reached the UTP transfer path. The caller still
    /// owns DMA address translation, cache maintenance, and doorbell order.
    pub(crate) fn prepare_nop_out(
        command_descriptor_base: u64,
        descriptor: &mut [u8],
        task_tag: u8,
    ) -> Option<TransferRequestDescriptor> {
        if command_descriptor_base % COMMAND_DESCRIPTOR_ALIGNMENT != 0
            || descriptor.len() < DEVMAN_COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET
        {
            return None;
        }
        let header = RequestHeader::new(UTP_CMD_TYPE_UFS, UTP_NO_DATA_TRANSFER, true)?;
        descriptor.fill(0);
        descriptor[COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET
            ..COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET + UPIU_HEADER_SIZE]
            .copy_from_slice(
                &UpiuHeader {
                    transaction_code: UPIU_TRANSACTION_NOP_OUT,
                    flags: UPIU_CMD_FLAGS_NONE,
                    lun: 0,
                    task_tag,
                    initiator_id: 0,
                    command_set_type: 0,
                    function: 0,
                    response: 0,
                    status: 0,
                    ehs_length: 0,
                    device_information: 0,
                    data_segment_length: 0,
                }
                .to_be_bytes(),
            );
        Some(TransferRequestDescriptor {
            header,
            command_desc_base_addr: command_descriptor_base,
            response_upiu_length: ALIGNED_DEVMAN_RSP_SIZE / 4,
            response_upiu_offset: (DEVMAN_COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET / 4) as u16,
            prd_table_length: 0,
            prd_table_offset: (DEVMAN_COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET / 4) as u16,
        })
    }

    /// Prepare Linux's standard READ DESCRIPTOR query in the reserved
    /// device-management UCD. Descriptor data is returned in the response
    /// UPIU data segment at byte 32, not through a PRD.
    pub(crate) fn prepare_query_read_desc(
        command_descriptor_base: u64,
        descriptor: &mut [u8],
        task_tag: u8,
        idn: u8,
        index: u8,
        length: u16,
    ) -> Option<TransferRequestDescriptor> {
        if command_descriptor_base % COMMAND_DESCRIPTOR_ALIGNMENT != 0
            || !(QUERY_DESC_MIN_SIZE..=QUERY_DESC_MAX_SIZE).contains(&length)
            || descriptor.len() < DEVMAN_COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET
        {
            return None;
        }
        let header = RequestHeader::new(UTP_CMD_TYPE_UFS, UTP_NO_DATA_TRANSFER, true)?;
        descriptor.fill(0);
        descriptor[COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET
            ..COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET + UPIU_HEADER_SIZE]
            .copy_from_slice(
                &UpiuHeader {
                    transaction_code: UPIU_TRANSACTION_QUERY_REQ,
                    flags: UPIU_CMD_FLAGS_NONE,
                    lun: 0,
                    task_tag,
                    initiator_id: 0,
                    command_set_type: 0,
                    function: UPIU_QUERY_FUNC_STANDARD_READ_REQUEST,
                    response: 0,
                    status: 0,
                    ehs_length: 0,
                    device_information: 0,
                    data_segment_length: 0,
                }
                .to_be_bytes(),
            );
        descriptor[COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET + UPIU_HEADER_SIZE
            ..COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET + UPIU_GENERAL_REQUEST_SIZE]
            .copy_from_slice(
                &UpiuQuery {
                    opcode: UPIU_QUERY_OPCODE_READ_DESC,
                    idn,
                    index,
                    selector: 0,
                    length,
                    value: 0,
                }
                .to_be_bytes(),
            );
        Some(TransferRequestDescriptor {
            header,
            command_desc_base_addr: command_descriptor_base,
            response_upiu_length: ALIGNED_DEVMAN_RSP_SIZE / 4,
            response_upiu_offset: (DEVMAN_COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET / 4) as u16,
            prd_table_length: 0,
            prd_table_offset: (DEVMAN_COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET / 4) as u16,
        })
    }

    /// Copy a successful READ DESCRIPTOR response from the response UPIU's
    /// data segment. The caller supplies the DMA-synchronized response area.
    pub(crate) fn copy_query_read_desc_response(
        response: &[u8],
        descriptor: &mut [u8],
    ) -> Option<usize> {
        if response.len() < UPIU_GENERAL_REQUEST_SIZE
            || response[0] != UPIU_TRANSACTION_QUERY_RSP
            || response[6] != 0
        {
            return None;
        }
        let length = u16::from_be_bytes([response[10], response[11]]) as usize;
        if !(QUERY_DESC_MIN_SIZE as usize..=QUERY_DESC_MAX_SIZE as usize).contains(&length)
            || length > descriptor.len()
            || UPIU_GENERAL_REQUEST_SIZE + length > response.len()
        {
            return None;
        }
        descriptor[..length].copy_from_slice(
            &response[UPIU_GENERAL_REQUEST_SIZE..UPIU_GENERAL_REQUEST_SIZE + length],
        );
        Some(length)
    }

    /// Prepare a single-PRD SCSI READ(10) request in a normal UCD. This is a
    /// pure command builder; it does not imply that the supplied data address
    /// is DMA-visible to the UFS controller.
    pub(crate) fn prepare_scsi_read10(
        command_descriptor_base: u64,
        descriptor: &mut [u8],
        data_address: u64,
        task_tag: u8,
        lun: u8,
        lba: u32,
        block_count: u16,
        block_size: u32,
    ) -> Option<TransferRequestDescriptor> {
        if command_descriptor_base % COMMAND_DESCRIPTOR_ALIGNMENT != 0
            || block_count == 0
            || descriptor.len()
                < COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET + PhysicalRegionDescriptor::BYTE_SIZE
        {
            return None;
        }
        let transfer_bytes = u32::from(block_count).checked_mul(block_size)?;
        let prd = PhysicalRegionDescriptor::new(data_address, transfer_bytes)?;
        let header = RequestHeader::new(UTP_CMD_TYPE_UFS, UTP_DEVICE_TO_HOST, true)?;
        descriptor.fill(0);
        descriptor[COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET
            ..COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET + UPIU_HEADER_SIZE]
            .copy_from_slice(
                &UpiuHeader {
                    transaction_code: UPIU_TRANSACTION_COMMAND,
                    flags: UPIU_CMD_FLAGS_READ,
                    lun,
                    task_tag,
                    initiator_id: 0,
                    command_set_type: UPIU_COMMAND_SET_TYPE_SCSI,
                    function: 0,
                    response: 0,
                    status: 0,
                    ehs_length: 0,
                    device_information: 0,
                    data_segment_length: 0,
                }
                .to_be_bytes(),
            );
        let command_offset = COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET + UPIU_HEADER_SIZE;
        descriptor[command_offset..command_offset + 4]
            .copy_from_slice(&transfer_bytes.to_be_bytes());
        let cdb_offset = command_offset + 4;
        descriptor[cdb_offset] = SCSI_READ10_OPCODE;
        descriptor[cdb_offset + 2..cdb_offset + 6].copy_from_slice(&lba.to_be_bytes());
        descriptor[cdb_offset + 7..cdb_offset + 9].copy_from_slice(&block_count.to_be_bytes());
        descriptor[COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET
            ..COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET + PhysicalRegionDescriptor::BYTE_SIZE]
            .copy_from_slice(&prd.to_le_bytes());
        Some(TransferRequestDescriptor {
            header,
            command_desc_base_addr: command_descriptor_base,
            response_upiu_length: ALIGNED_UPIU_SIZE / 4,
            response_upiu_offset: (COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET / 4) as u16,
            prd_table_length: 1,
            prd_table_offset: (COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET / 4) as u16,
        })
    }

    pub(crate) fn scsi_response_success(response: &[u8], task_tag: u8) -> bool {
        response.len() >= UPIU_HEADER_SIZE
            && response[0] == UPIU_TRANSACTION_RESPONSE
            && response[3] == task_tag
            && response[7] == 0
    }

    pub(crate) fn nop_response_success(response: &[u8], task_tag: u8) -> bool {
        response.len() >= UPIU_HEADER_SIZE
            && response[0] == UPIU_TRANSACTION_NOP_IN
            && response[3] == task_tag
    }

    pub(crate) const fn layout_is_supported() -> bool {
        REGISTER_SPACE_SIZE == 0xa0
            && ALIGNED_UPIU_SIZE == 512
            && UPIU_HEADER_SIZE == 12
            && UPIU_QUERY_SIZE == 20
            && UPIU_GENERAL_REQUEST_SIZE == 32
            && COMMAND_DESCRIPTOR_ALIGNMENT == 128
            && TRANSFER_REQUEST_LIST_ALIGNMENT == 1024
            && TRANSFER_REQUEST_DESCRIPTOR_SIZE == 32
            && COMMAND_DESCRIPTOR_REQUEST_UPIU_OFFSET == 0
            && COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET == 512
            && COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET == 1024
            && DEVMAN_COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET == 512
            && DEVMAN_COMMAND_DESCRIPTOR_PRD_TABLE_OFFSET == 4608
            && ALIGNED_DEVMAN_RSP_SIZE == 4096
            && QUERY_DESC_MIN_SIZE == 2
            && QUERY_DESC_MAX_SIZE == 255
            && PRD_DATA_BYTE_COUNT_GRANULARITY == 4
            && PRD_DATA_BYTE_COUNT_MAX == 256 * 1024
            && PhysicalRegionDescriptor::BYTE_SIZE == 16
    }

    const _: () = assert!(layout_is_supported());
}

const UFS_DESC_TYPE_DEVICE: u8 = 0x00;
const UFS_DESC_TYPE_UNIT: u8 = 0x02;
const UFS_DEVICE_DESC_NUM_LU_OFFSET: usize = 0x06;
const UFS_UNIT_DESC_INDEX_OFFSET: usize = 0x02;
const UFS_UNIT_DESC_ENABLE_OFFSET: usize = 0x03;
const UFS_UNIT_DESC_BLOCK_SIZE_OFFSET: usize = 0x0a;
const UFS_UNIT_DESC_BLOCK_COUNT_OFFSET: usize = 0x0b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UfsGeometry {
    pub lun: u8,
    pub block_size: u32,
    pub total_blocks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DescriptorError {
    TooShort,
    InvalidLength,
    WrongType,
    InvalidField,
}

fn descriptor_payload(descriptor: &[u8], expected_type: u8) -> Result<&[u8], DescriptorError> {
    if descriptor.len() < 2 {
        return Err(DescriptorError::TooShort);
    }
    let length = descriptor[0] as usize;
    if length < 2 || length > descriptor.len() {
        return Err(DescriptorError::InvalidLength);
    }
    if descriptor[1] != expected_type {
        return Err(DescriptorError::WrongType);
    }
    Ok(&descriptor[..length])
}

pub(crate) fn parse_device_num_lu(descriptor: &[u8]) -> Result<u8, DescriptorError> {
    let descriptor = descriptor_payload(descriptor, UFS_DESC_TYPE_DEVICE)?;
    descriptor
        .get(UFS_DEVICE_DESC_NUM_LU_OFFSET)
        .copied()
        .filter(|count| *count != 0)
        .ok_or(DescriptorError::InvalidField)
}

pub(crate) fn parse_unit_geometry(
    descriptor: &[u8],
    lun: u8,
) -> Result<UfsGeometry, DescriptorError> {
    let descriptor = descriptor_payload(descriptor, UFS_DESC_TYPE_UNIT)?;
    let unit_index = *descriptor
        .get(UFS_UNIT_DESC_INDEX_OFFSET)
        .ok_or(DescriptorError::InvalidField)?;
    let enabled = *descriptor
        .get(UFS_UNIT_DESC_ENABLE_OFFSET)
        .ok_or(DescriptorError::InvalidField)?;
    let block_exponent = *descriptor
        .get(UFS_UNIT_DESC_BLOCK_SIZE_OFFSET)
        .ok_or(DescriptorError::InvalidField)?;
    let count_end = UFS_UNIT_DESC_BLOCK_COUNT_OFFSET + core::mem::size_of::<u64>();
    if unit_index != lun || enabled == 0 || !(9..=20).contains(&block_exponent) {
        return Err(DescriptorError::InvalidField);
    }
    let block_count = u64::from_be_bytes(
        descriptor[UFS_UNIT_DESC_BLOCK_COUNT_OFFSET..count_end]
            .try_into()
            .map_err(|_| DescriptorError::InvalidField)?,
    );
    let block_size = 1u32
        .checked_shl(u32::from(block_exponent))
        .ok_or(DescriptorError::InvalidField)?;
    if block_count == 0 {
        return Err(DescriptorError::InvalidField);
    }
    Ok(UfsGeometry {
        lun,
        block_size,
        total_blocks: block_count,
    })
}

/// Bring-up states exposed to the inventory layer. `init()` still publishes
/// only `Described`; `execute_platform()` can advance to `ControllerReady`
/// once a concrete, owned backend completes the guarded sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum BringupStage {
    Absent = 0,
    Described = 1,
    PowerReady = 2,
    PhyReady = 3,
    ControllerReady = 4,
    LinkReady = 5,
    BlockReady = 6,
}

/// The ordering boundary copied from the Qualcomm Linux split between the
/// PHY driver's `power_on()` and the UFS-QCOM HCE notification. The sequence
/// is consumed by `execute_platform()` through a narrow backend,
/// so order can be tested without permitting arbitrary MMIO from the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum PlatformStep {
    EnableControllerClocks = 0,
    EnableQphyRpmhResource = 1,
    EnablePhyRails = 2,
    PowerOnPhyAnalog = 3,
    EnablePhyInterfaceClocks = 4,
    EnableReferenceClocks = 5,
    AssertControllerPhyReset = 6,
    ApplyRateACalibration = 7,
    ApplySecondLaneCalibration = 8,
    DeassertControllerPhyReset = 9,
    StartSerdes = 10,
    WaitPcsReady = 11,
    SelectUniproMode = 12,
    EnableLaneClocks = 13,
    EnableController = 14,
    EnableDeviceRefRail = 15,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlatformSequence {
    pub steps: [PlatformStep; 16],
    pub count: u8,
}

impl PlatformSequence {
    const LITO: Self = Self {
        steps: [
            PlatformStep::EnableControllerClocks,
            PlatformStep::EnableQphyRpmhResource,
            PlatformStep::EnablePhyRails,
            PlatformStep::PowerOnPhyAnalog,
            PlatformStep::EnablePhyInterfaceClocks,
            PlatformStep::EnableReferenceClocks,
            PlatformStep::EnableDeviceRefRail,
            PlatformStep::AssertControllerPhyReset,
            PlatformStep::ApplyRateACalibration,
            PlatformStep::ApplySecondLaneCalibration,
            PlatformStep::DeassertControllerPhyReset,
            PlatformStep::StartSerdes,
            PlatformStep::WaitPcsReady,
            PlatformStep::SelectUniproMode,
            PlatformStep::EnableLaneClocks,
            PlatformStep::EnableController,
        ],
        count: 16,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlatformError {
    InvalidContract,
    UnmappedRegulators,
    PowerStateUnsupported,
    Step(PlatformStep),
    PcsReadyTimeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkStartupError {
    InvalidContract,
    UnmappedRegulators,
    PowerStateUnsupported,
    ControllerNotReady,
    CommandTimeout,
    CommandFailed(u8),
    DeviceAbsent,
}

/// The hardware-dependent side of the Linux-aligned sequence. Keeping this
/// interface narrower than a generic MMIO bus is deliberate: a caller must
/// prove the DT contract first, and a host-side mock can verify the complete
/// order without touching a Qualcomm register or RPMh TCS.
pub(crate) trait PlatformOps {
    fn enable_clock(&mut self, gate: ClockGate) -> bool;
    fn enable_qphy_rpmh_resource(&mut self, resource: &[u8; 8]) -> bool;
    fn enable_phy_rails(&mut self) -> bool;
    fn power_on_phy_analog(&mut self) -> bool;
    fn enable_phy_interface_clocks(&mut self) -> bool;
    fn enable_reference_clock(&mut self, spec: ProviderSpec) -> bool;
    fn set_controller_phy_reset(&mut self, asserted: bool) -> bool;
    fn write_phy(&mut self, offset: u32, value: u8) -> bool;
    fn read_phy(&mut self, offset: u32) -> u32;
    fn delay_us(&mut self, microseconds: u32);
    fn select_unipro_mode(&mut self) -> bool;
    fn enable_controller(&mut self) -> bool;
    fn enable_device_ref_rail(&mut self) -> bool;
}

/// The minimal controller surface needed after HCE: UFSHCI UIC commands are
/// completed through the controller interrupt-status register, but this early
/// boot path polls that status before installing a GIC-backed IRQ handler.
pub(crate) trait ControllerOps {
    fn read_controller(&mut self, offset: u32) -> u32;
    fn write_controller(&mut self, offset: u32, value: u32);
    fn delay_us(&mut self, microseconds: u32);
}

/// DMA addresses and ownership evidence required before programming UFSHCI.
/// The DT's lack of an `iommus` property is not itself treated as proof of
/// identity DMA, so a platform backend must provide both evidence bits
/// explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DmaContract {
    pub transfer_list: u64,
    pub task_list: u64,
    pub devman_descriptor: u64,
    pub command_descriptor: u64,
    pub data: u64,
    pub dma_visible: bool,
    pub cache_maintained: bool,
}

impl DmaContract {
    pub(crate) const fn ready(self) -> bool {
        self.transfer_list != 0
            && self.task_list != 0
            && self.devman_descriptor != 0
            && self.command_descriptor != 0
            && self.data != 0
            && self.transfer_list % hci::TRANSFER_REQUEST_LIST_ALIGNMENT == 0
            && self.task_list % hci::TRANSFER_REQUEST_LIST_ALIGNMENT == 0
            && self.devman_descriptor % hci::COMMAND_DESCRIPTOR_ALIGNMENT == 0
            && self.command_descriptor % hci::COMMAND_DESCRIPTOR_ALIGNMENT == 0
            && self.dma_visible
            && self.cache_maintained
    }

    pub(crate) fn any_address_above_32_bits(self) -> bool {
        self.transfer_list > u64::from(u32::MAX)
            || self.task_list > u64::from(u32::MAX)
            || self.devman_descriptor > u64::from(u32::MAX)
            || self.command_descriptor > u64::from(u32::MAX)
            || self.data > u64::from(u32::MAX)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferEngineError {
    DmaContractUnproven,
    AddressWidthUnsupported,
    ControllerNotReady,
    InvalidSlot,
    CompletionTimeout,
    ControllerFault(u32),
}

const INTERRUPT_TRANSFER_REQ_COMPLETION: u32 = 1 << 0;
const INTERRUPT_TASK_REQ_COMPLETION: u32 = 1 << 9;
const INTERRUPT_UTP_ERROR: u32 = 1 << 12;
const INTERRUPT_FATAL_ERROR: u32 = (1 << 11) | (1 << 16) | (1 << 17) | (1 << 18);
const TRANSFER_ENGINE_INTERRUPT_MASK: u32 = INTERRUPT_TRANSFER_REQ_COMPLETION
    | INTERRUPT_TASK_REQ_COMPLETION
    | INTERRUPT_UTP_ERROR
    | INTERRUPT_FATAL_ERROR
    | hci::INTERRUPT_UIC_ERROR
    | hci::INTERRUPT_UIC_LINK_LOST;

/// Configure the legacy single-doorbell UFSHCI request engine after link
/// startup. This function is deliberately not called by generic boot: the
/// explicit DMA contract is a separate proof obligation from the DT contract.
pub(crate) fn configure_transfer_engine<O: ControllerOps>(
    ops: &mut O,
    dma: DmaContract,
) -> Result<hci::ControllerCapabilities, TransferEngineError> {
    if !dma.ready() {
        return Err(TransferEngineError::DmaContractUnproven);
    }
    let capabilities = hci::ControllerCapabilities {
        raw: ops.read_controller(hci::REG_CONTROLLER_CAPABILITIES),
    };
    if !capabilities.supports_64bit_addressing() && dma.any_address_above_32_bits() {
        return Err(TransferEngineError::AddressWidthUnsupported);
    }
    if ops.read_controller(hci::REG_CONTROLLER_STATUS) & hci::STATUS_READY != hci::STATUS_READY {
        return Err(TransferEngineError::ControllerNotReady);
    }

    // Linux's make_hba_operational() disables aggregation in the one-command
    // bring-up path, clears stale status, enables completion/error sources,
    // then publishes both list bases before run/stop.
    ops.write_controller(hci::REG_UTP_TRANSFER_REQ_INT_AGG_CONTROL, 0);
    let pending = ops.read_controller(hci::REG_INTERRUPT_STATUS) & TRANSFER_ENGINE_INTERRUPT_MASK;
    if pending != 0 {
        ops.write_controller(hci::REG_INTERRUPT_STATUS, pending);
    }
    let interrupt_enable =
        ops.read_controller(hci::REG_INTERRUPT_ENABLE) | TRANSFER_ENGINE_INTERRUPT_MASK;
    ops.write_controller(hci::REG_INTERRUPT_ENABLE, interrupt_enable);
    ops.write_controller(
        hci::REG_UTP_TRANSFER_REQ_LIST_BASE_L,
        dma.transfer_list as u32,
    );
    ops.write_controller(
        hci::REG_UTP_TRANSFER_REQ_LIST_BASE_H,
        (dma.transfer_list >> 32) as u32,
    );
    ops.write_controller(hci::REG_UTP_TASK_REQ_LIST_BASE_L, dma.task_list as u32);
    ops.write_controller(
        hci::REG_UTP_TASK_REQ_LIST_BASE_H,
        (dma.task_list >> 32) as u32,
    );
    ops.write_controller(
        hci::REG_UTP_TASK_REQ_LIST_RUN_STOP,
        hci::TRANSFER_LIST_RUN_STOP,
    );
    ops.write_controller(
        hci::REG_UTP_TRANSFER_REQ_LIST_RUN_STOP,
        hci::TRANSFER_LIST_RUN_STOP,
    );
    Ok(capabilities)
}

/// Install one serialized UTRD into a legacy transfer list. Cache cleaning
/// and DMA address translation remain outside this pure memory operation.
pub(crate) fn install_transfer_slot(
    transfer_list: &mut [u8],
    slot: usize,
    request: hci::TransferRequestDescriptor,
) -> bool {
    if slot >= 32 {
        return false;
    }
    let offset = slot * hci::TRANSFER_REQUEST_DESCRIPTOR_SIZE;
    let end = offset + hci::TRANSFER_REQUEST_DESCRIPTOR_SIZE;
    if end > transfer_list.len() {
        return false;
    }
    transfer_list[offset..end].copy_from_slice(&request.to_le_bytes());
    true
}

pub(crate) fn transfer_slot_ocs(transfer_list: &[u8], slot: usize) -> Option<u8> {
    if slot >= 32 {
        return None;
    }
    let offset = slot * hci::TRANSFER_REQUEST_DESCRIPTOR_SIZE;
    (offset + 12 <= transfer_list.len()).then_some(transfer_list[offset + 8] & hci::MASK_OCS)
}

/// Ring one legacy transfer slot after the caller has installed and cache-
/// cleaned its UTRD/UCD/PRDT objects.
pub(crate) fn ring_transfer_request<O: ControllerOps>(
    ops: &mut O,
    dma: DmaContract,
    slot: usize,
) -> Result<(), TransferEngineError> {
    if !dma.ready() {
        return Err(TransferEngineError::DmaContractUnproven);
    }
    if slot >= 32 {
        return Err(TransferEngineError::InvalidSlot);
    }
    ops.write_controller(hci::REG_UTP_TRANSFER_REQ_DOOR_BELL, 1 << slot);
    Ok(())
}

pub(crate) fn poll_transfer_completion<O: ControllerOps>(
    ops: &mut O,
    slot: usize,
    timeout_us: u32,
) -> Result<(), TransferEngineError> {
    if slot >= 32 {
        return Err(TransferEngineError::InvalidSlot);
    }
    let mut elapsed = 0;
    while elapsed <= timeout_us {
        let status = ops.read_controller(hci::REG_INTERRUPT_STATUS);
        if status & (INTERRUPT_FATAL_ERROR | INTERRUPT_UTP_ERROR) != 0 {
            ops.write_controller(
                hci::REG_INTERRUPT_STATUS,
                status & TRANSFER_ENGINE_INTERRUPT_MASK,
            );
            return Err(TransferEngineError::ControllerFault(status));
        }
        if status & INTERRUPT_TRANSFER_REQ_COMPLETION != 0 {
            ops.write_controller(hci::REG_INTERRUPT_STATUS, INTERRUPT_TRANSFER_REQ_COMPLETION);
            return Ok(());
        }
        if elapsed == timeout_us {
            return Err(TransferEngineError::CompletionTimeout);
        }
        ops.delay_us(10);
        elapsed = elapsed.saturating_add(10);
    }
    Err(TransferEngineError::CompletionTimeout)
}

/// Submit one already-serialized transfer-list slot. The caller must clean
/// the UTRD/UCD/PRDT cache lines before calling and invalidate them after this
/// function returns; the generic queue layer cannot infer either policy.
pub(crate) fn submit_transfer_slot<O: ControllerOps>(
    ops: &mut O,
    dma: DmaContract,
    transfer_list: &mut [u8],
    slot: usize,
    request: hci::TransferRequestDescriptor,
    timeout_us: u32,
) -> Result<(), TransferEngineError> {
    if !dma.ready() {
        return Err(TransferEngineError::DmaContractUnproven);
    }
    if !install_transfer_slot(transfer_list, slot, request) {
        return Err(TransferEngineError::InvalidSlot);
    }
    ring_transfer_request(ops, dma, slot)?;
    poll_transfer_completion(ops, slot, timeout_us)
}

const QPHY_RPMH_RESOURCE: [u8; 8] = *b"qphy.lvl";
const PCS_READY_TIMEOUT_US: u32 = 1_000_000;
const PCS_READY_POLL_US: u32 = 10;

fn run_step(result: bool, step: PlatformStep) -> Result<(), PlatformError> {
    result.then_some(()).ok_or(PlatformError::Step(step))
}

fn apply_calibration_table<O: PlatformOps>(
    ops: &mut O,
    table: &[CalibrationEntry],
) -> Result<(), PlatformError> {
    for entry in table {
        run_step(
            ops.write_phy(entry.offset, entry.value),
            PlatformStep::ApplyRateACalibration,
        )?;
    }
    Ok(())
}

/// Execute the source-ordered Qualcomm PHY/controller boundary. This is the
/// first function allowed to request side effects, but it still requires the
/// exact merged Bramble/Lito DT contract. Link startup, DMA, interrupts, and
/// block translation intentionally remain outside this transaction.
pub(crate) fn execute_platform<O: PlatformOps>(
    profile: PlatformContract,
    ops: &mut O,
    rate_b: bool,
) -> Result<BringupStage, PlatformError> {
    if platform_sequence(profile).is_none() {
        return Err(PlatformError::InvalidContract);
    }

    for index in 0..profile.controller_clock_count as usize {
        let spec = profile.controller_clocks[index];
        if spec.provider == RPMH_CLOCK_PHANDLE {
            continue;
        }
        let Some(gate) = lito_clock_gate(spec) else {
            return Err(PlatformError::Step(PlatformStep::EnableControllerClocks));
        };
        run_step(ops.enable_clock(gate), PlatformStep::EnableControllerClocks)?;
    }

    run_step(
        ops.enable_qphy_rpmh_resource(&QPHY_RPMH_RESOURCE),
        PlatformStep::EnableQphyRpmhResource,
    )?;
    run_step(ops.enable_phy_rails(), PlatformStep::EnablePhyRails)?;
    run_step(ops.power_on_phy_analog(), PlatformStep::PowerOnPhyAnalog)?;
    run_step(
        ops.enable_phy_interface_clocks(),
        PlatformStep::EnablePhyInterfaceClocks,
    )?;

    // The host ref_clk and PHY ref_clk_src are separate Linux clock handles,
    // even though both ultimately use the same RPMh CXO source on Bramble.
    run_step(
        ops.enable_reference_clock(profile.controller_clocks[6]),
        PlatformStep::EnableReferenceClocks,
    )?;
    for spec in profile.phy_clocks {
        run_step(
            ops.enable_reference_clock(spec),
            PlatformStep::EnableReferenceClocks,
        )?;
    }

    run_step(
        ops.enable_device_ref_rail(),
        PlatformStep::EnableDeviceRefRail,
    )?;

    run_step(
        ops.set_controller_phy_reset(true),
        PlatformStep::AssertControllerPhyReset,
    )?;
    ops.delay_us(1_000);
    run_step(
        ops.write_phy(LITO_UFS_PHY_SW_RESET, 0x01),
        PlatformStep::ApplyRateACalibration,
    )?;
    apply_calibration_table(ops, LITO_RATE_A_NO_G4)?;
    if profile.lanes_per_direction == Some(2) {
        for entry in LITO_SECOND_LANE_NO_G4 {
            run_step(
                ops.write_phy(entry.offset, entry.value),
                PlatformStep::ApplySecondLaneCalibration,
            )?;
        }
    }
    if rate_b {
        for entry in LITO_RATE_B {
            run_step(
                ops.write_phy(entry.offset, entry.value),
                PlatformStep::ApplyRateACalibration,
            )?;
        }
    }
    run_step(
        ops.write_phy(LITO_UFS_PHY_SW_RESET, 0x00),
        PlatformStep::ApplyRateACalibration,
    )?;
    run_step(
        ops.set_controller_phy_reset(false),
        PlatformStep::DeassertControllerPhyReset,
    )?;
    ops.delay_us(1_000);

    let start = (ops.read_phy(LITO_UFS_PHY_START) & !SERDES_START_MASK) | SERDES_START_MASK;
    run_step(
        ops.write_phy(LITO_UFS_PHY_START, start as u8),
        PlatformStep::StartSerdes,
    )?;

    let mut elapsed = 0;
    while elapsed <= PCS_READY_TIMEOUT_US {
        if ops.read_phy(LITO_UFS_PHY_PCS_READY_STATUS) & PCS_READY_MASK != 0 {
            break;
        }
        if elapsed == PCS_READY_TIMEOUT_US {
            return Err(PlatformError::PcsReadyTimeout);
        }
        ops.delay_us(PCS_READY_POLL_US);
        elapsed = elapsed.saturating_add(PCS_READY_POLL_US);
    }

    run_step(ops.select_unipro_mode(), PlatformStep::SelectUniproMode)?;
    for index in 7..10 {
        let Some(gate) = lito_clock_gate(profile.controller_clocks[index]) else {
            return Err(PlatformError::Step(PlatformStep::EnableLaneClocks));
        };
        run_step(ops.enable_clock(gate), PlatformStep::EnableLaneClocks)?;
    }
    run_step(ops.enable_controller(), PlatformStep::EnableController)?;

    unsafe {
        STAGE = BringupStage::ControllerReady;
    }
    Ok(BringupStage::ControllerReady)
}

const UIC_COMMAND_TIMEOUT_US: u32 = 500_000;
const UIC_COMMAND_POLL_US: u32 = 10;

/// Issue the Linux UFSHCI DME_LINK_STARTUP command and verify both the UIC
/// result and the interconnect's device-present indication. This is kept
/// separate from PHY power-on so a failed link cannot be reported as storage
/// readiness. Retries and link power-mode negotiation remain later policy.
pub(crate) fn execute_link_startup<O: ControllerOps>(
    profile: PlatformContract,
    ops: &mut O,
) -> Result<BringupStage, LinkStartupError> {
    if platform_sequence(profile).is_none() {
        return Err(LinkStartupError::InvalidContract);
    }

    let mut elapsed = 0;
    while elapsed <= UIC_COMMAND_TIMEOUT_US {
        if ops.read_controller(hci::REG_CONTROLLER_STATUS) & hci::STATUS_UIC_COMMAND_READY != 0 {
            break;
        }
        if elapsed == UIC_COMMAND_TIMEOUT_US {
            return Err(LinkStartupError::ControllerNotReady);
        }
        ops.delay_us(UIC_COMMAND_POLL_US);
        elapsed = elapsed.saturating_add(UIC_COMMAND_POLL_US);
    }

    let uic_interrupts = hci::INTERRUPT_UIC_ERROR
        | hci::INTERRUPT_UIC_COMMAND_COMPLETION
        | hci::INTERRUPT_UIC_LINK_LOST;
    let pending = ops.read_controller(hci::REG_INTERRUPT_STATUS) & uic_interrupts;
    if pending != 0 {
        // UFSHCI interrupt status is write-one-to-clear. Only clear UIC bits;
        // unrelated transfer/task completion bits belong to later ownership.
        ops.write_controller(hci::REG_INTERRUPT_STATUS, pending);
    }
    let enabled = ops.read_controller(hci::REG_INTERRUPT_ENABLE) | uic_interrupts;
    ops.write_controller(hci::REG_INTERRUPT_ENABLE, enabled);
    ops.write_controller(hci::REG_UIC_COMMAND_ARG_1, 0);
    ops.write_controller(hci::REG_UIC_COMMAND_ARG_2, 0);
    ops.write_controller(hci::REG_UIC_COMMAND_ARG_3, 0);
    ops.write_controller(
        hci::REG_UIC_COMMAND,
        hci::UIC_COMMAND_DME_LINK_STARTUP & 0xff,
    );

    elapsed = 0;
    while elapsed <= UIC_COMMAND_TIMEOUT_US {
        let status = ops.read_controller(hci::REG_INTERRUPT_STATUS);
        if status & (hci::INTERRUPT_UIC_ERROR | hci::INTERRUPT_UIC_LINK_LOST) != 0 {
            let result = (ops.read_controller(hci::REG_UIC_COMMAND_ARG_2)
                & hci::UIC_COMMAND_RESULT_MASK) as u8;
            ops.write_controller(hci::REG_INTERRUPT_STATUS, status & uic_interrupts);
            return Err(LinkStartupError::CommandFailed(result));
        }
        if status & hci::INTERRUPT_UIC_COMMAND_COMPLETION != 0 {
            let result = (ops.read_controller(hci::REG_UIC_COMMAND_ARG_2)
                & hci::UIC_COMMAND_RESULT_MASK) as u8;
            ops.write_controller(hci::REG_INTERRUPT_STATUS, status & uic_interrupts);
            if result != 0 {
                return Err(LinkStartupError::CommandFailed(result));
            }
            if ops.read_controller(hci::REG_CONTROLLER_STATUS) & hci::STATUS_DEVICE_PRESENT == 0 {
                return Err(LinkStartupError::DeviceAbsent);
            }
            unsafe {
                STAGE = BringupStage::LinkReady;
            }
            return Ok(BringupStage::LinkReady);
        }
        if elapsed == UIC_COMMAND_TIMEOUT_US {
            return Err(LinkStartupError::CommandTimeout);
        }
        ops.delay_us(UIC_COMMAND_POLL_US);
        elapsed = elapsed.saturating_add(UIC_COMMAND_POLL_US);
    }

    Err(LinkStartupError::CommandTimeout)
}

/// Real Bramble backend. It is compiled only for the Bramble image and is
/// invoked only by the explicit `FULLERENE_AARCH64_UFS_EXECUTE=1` build opt-in
/// in `main.rs`; generic QEMU has no path to these Qualcomm writes.
#[cfg(fullerene_aarch64_bramble)]
pub(crate) struct BramblePlatformOps {
    profile: PlatformContract,
    controller_base: u64,
    phy_base: u64,
    gcc_base: u64,
    xo_enabled: bool,
}

#[cfg(fullerene_aarch64_bramble)]
impl BramblePlatformOps {
    pub(crate) fn new(profile: PlatformContract) -> Result<Self, PlatformError> {
        if !profile.resource_contract_valid() {
            return Err(PlatformError::InvalidContract);
        }
        if !profile.power_contract_valid() {
            return Err(PlatformError::UnmappedRegulators);
        }
        if !profile.power_transaction_ready() {
            return Err(PlatformError::PowerStateUnsupported);
        }
        let Some(gcc_region) = profile.gcc_region else {
            return Err(PlatformError::InvalidContract);
        };
        Ok(Self {
            profile,
            controller_base: profile.controller[0].base,
            phy_base: profile.phy[0].base,
            gcc_base: gcc_region.base,
            xo_enabled: false,
        })
    }

    #[inline(always)]
    unsafe fn read32(base: u64, offset: u32) -> u32 {
        unsafe { core::ptr::read_volatile((base + offset as u64) as *const u32) }
    }

    #[inline(always)]
    unsafe fn write32(base: u64, offset: u32, value: u32) {
        unsafe { core::ptr::write_volatile((base + offset as u64) as *mut u32, value) };
    }

    #[inline(always)]
    unsafe fn barrier() {
        unsafe { core::arch::asm!("dmb sy", options(nostack, preserves_flags)) };
    }

    unsafe fn configure_source(&self, gate: ClockGate) -> bool {
        let Some(source_offset) = gate.source_offset else {
            return true;
        };
        let (parent, divider) = match gate.id {
            // gcc_ufs_phy_axi_clk_src: GPLL0_OUT_MAIN / 3 = 200 MHz.
            0x67 | 0x81 => (1, 2),
            // gcc_ufs_phy_unipro_core_clk_src: GPLL0_OUT_MAIN / 4 = 150 MHz.
            0x70 => (1, 3),
            // gcc_ufs_phy_ice_core_clk_src: GPLL0_OUT_MAIN / 2 = 300 MHz.
            0x69 | 0x8a => (1, 1),
            // gcc_ufs_phy_phy_aux_clk_src: BI_TCXO / 1 = 19.2 MHz.
            0x6b => (0, 0),
            _ => return false,
        };
        const SOURCE_DIV_MASK: u32 = 0xff;
        const SOURCE_SEL_MASK: u32 = 0x7 << 8;
        const COMMAND_UPDATE: u32 = 1;
        let config_offset = source_offset + 4;
        let mut config = unsafe { Self::read32(self.gcc_base, config_offset) };
        config &= !(SOURCE_DIV_MASK | SOURCE_SEL_MASK);
        config |= divider & SOURCE_DIV_MASK;
        config |= (parent << 8) & SOURCE_SEL_MASK;
        unsafe { Self::write32(self.gcc_base, config_offset, config) };
        let _ = unsafe { Self::read32(self.gcc_base, config_offset) };

        let command = unsafe { Self::read32(self.gcc_base, source_offset) } | COMMAND_UPDATE;
        unsafe { Self::write32(self.gcc_base, source_offset, command) };
        unsafe { Self::barrier() };
        for _ in 0..500_000u32 {
            if unsafe { Self::read32(self.gcc_base, source_offset) } & COMMAND_UPDATE == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    unsafe fn set_clock(&self, gate: ClockGate) -> bool {
        if unsafe { !self.configure_source(gate) } {
            return false;
        }
        let mut value = unsafe { Self::read32(self.gcc_base, gate.branch_offset) };
        value |= gate.enable_mask;
        unsafe { Self::write32(self.gcc_base, gate.branch_offset, value) };
        unsafe { Self::barrier() };
        for _ in 0..500_000u32 {
            if unsafe { Self::read32(self.gcc_base, gate.branch_offset) } & gate.enable_mask != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    unsafe fn enable_gdsc(&self, address: u64) -> bool {
        const PWR_ON: u32 = 1 << 31;
        const HW_CONTROL: u32 = 1 << 1;
        const SW_OVERRIDE: u32 = 1 << 2;
        const SW_COLLAPSE: u32 = 1 << 0;
        const WAIT_MASK: u32 = (0xf << 20) | (0xf << 16) | (0xf << 12);
        const WAIT_VALUE: u32 = (0x2 << 20) | (0x8 << 16) | (0x2 << 12);
        let register = address as *mut u32;
        let mut value = unsafe { core::ptr::read_volatile(register) };
        value &= !(HW_CONTROL | SW_OVERRIDE | WAIT_MASK);
        value |= WAIT_VALUE;
        unsafe { core::ptr::write_volatile(register, value) };
        let _ = unsafe { core::ptr::read_volatile(register) };
        value &= !SW_COLLAPSE;
        unsafe { core::ptr::write_volatile(register, value) };
        let _ = unsafe { core::ptr::read_volatile(register) };
        unsafe { Self::barrier() };
        for _ in 0..1_000_000u32 {
            if unsafe { core::ptr::read_volatile(register) } & PWR_ON != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

#[cfg(fullerene_aarch64_bramble)]
impl PlatformOps for BramblePlatformOps {
    fn enable_clock(&mut self, gate: ClockGate) -> bool {
        unsafe { self.set_clock(gate) }
    }

    fn enable_qphy_rpmh_resource(&mut self, resource: &[u8; 8]) -> bool {
        unsafe { super::platform::bramble::send_rpmh_resource_value(resource, 1) }
    }

    fn enable_phy_rails(&mut self) -> bool {
        let Some(hba_region) = self.profile.vdd_hba_power.region else {
            return false;
        };
        if unsafe { !self.enable_gdsc(hba_region.base) } {
            return false;
        }
        let rails = [
            (self.profile.phy_vdda_power, None, 7),
            (self.profile.phy_vdda_pll_power, None, 7),
            (
                self.profile.vcc_power,
                self.profile.vcc_power.consumer_min_uv,
                7,
            ),
            (
                self.profile.vccq2_power,
                self.profile.vccq2_power.provider_min_uv,
                6,
            ),
        ];
        for (rail, voltage_uv, mode) in rails {
            let Some(resource_id) = rail.resource_id else {
                return false;
            };
            let Some(set_mask) = rail.qcom_set else {
                return false;
            };
            if unsafe {
                !super::platform::bramble::send_rpmh_regulator_request(
                    &resource_id,
                    voltage_uv,
                    mode,
                    true,
                    set_mask,
                )
            } {
                return false;
            }
        }
        true
    }

    fn power_on_phy_analog(&mut self) -> bool {
        unsafe {
            for offset in [
                LITO_UFS_PHY_RX0_INTERFACE_MODE,
                LITO_UFS_PHY_RX1_INTERFACE_MODE,
            ] {
                let value =
                    Self::read32(self.phy_base, offset) & !LITO_UFS_PHY_RX_INTERFACE_CLOCK_EDGE;
                Self::write32(self.phy_base, offset, value);
            }
            Self::write32(self.phy_base, LITO_UFS_PHY_POWER_DOWN_CONTROL, 1);
            Self::barrier();
        }
        true
    }

    fn enable_phy_interface_clocks(&mut self) -> bool {
        // Lito v4 has no tx_iface_clk/rx_iface_clk properties in the active
        // PHY node; the generic Linux driver treats both as optional.
        true
    }

    fn enable_reference_clock(&mut self, spec: ProviderSpec) -> bool {
        if spec.provider == RPMH_CLOCK_PHANDLE && spec.id == 0 {
            if self.xo_enabled {
                return true;
            }
            let ok = unsafe { super::platform::bramble::enable_rpmh_xo_clock() };
            if ok {
                self.xo_enabled = true;
            }
            return ok;
        }
        let Some(gate) = lito_clock_gate(spec) else {
            return false;
        };
        unsafe { self.set_clock(gate) }
    }

    fn set_controller_phy_reset(&mut self, asserted: bool) -> bool {
        unsafe {
            let mut value = Self::read32(self.controller_base, LITO_UFS_CONTROLLER_CFG1);
            if asserted {
                value |= LITO_UFS_CONTROLLER_CFG1_PHY_SOFT_RESET;
            } else {
                value &= !LITO_UFS_CONTROLLER_CFG1_PHY_SOFT_RESET;
            }
            Self::write32(self.controller_base, LITO_UFS_CONTROLLER_CFG1, value);
            Self::barrier();
            Self::read32(self.controller_base, LITO_UFS_CONTROLLER_CFG1)
                & LITO_UFS_CONTROLLER_CFG1_PHY_SOFT_RESET
                == if asserted {
                    LITO_UFS_CONTROLLER_CFG1_PHY_SOFT_RESET
                } else {
                    0
                }
        }
    }

    fn write_phy(&mut self, offset: u32, value: u8) -> bool {
        unsafe { Self::write32(self.phy_base, offset, value as u32) };
        true
    }

    fn read_phy(&mut self, offset: u32) -> u32 {
        unsafe { Self::read32(self.phy_base, offset) }
    }

    fn delay_us(&mut self, microseconds: u32) {
        super::timer::delay_us(microseconds as u64);
    }

    fn select_unipro_mode(&mut self) -> bool {
        unsafe {
            let value = Self::read32(self.controller_base, LITO_UFS_CONTROLLER_CFG1)
                | LITO_UFS_CONTROLLER_CFG1_UNIPRO_SEL;
            Self::write32(self.controller_base, LITO_UFS_CONTROLLER_CFG1, value);
            Self::barrier();
            Self::read32(self.controller_base, LITO_UFS_CONTROLLER_CFG1)
                & LITO_UFS_CONTROLLER_CFG1_UNIPRO_SEL
                != 0
        }
    }

    fn enable_controller(&mut self) -> bool {
        unsafe {
            Self::write32(self.controller_base, LITO_UFS_HCE, hci::CONTROLLER_ENABLE);
            Self::barrier();
            for _ in 0..500_000u32 {
                if Self::read32(self.controller_base, LITO_UFS_HCE) & hci::CONTROLLER_ENABLE != 0 {
                    return true;
                }
                core::hint::spin_loop();
            }
        }
        false
    }

    fn enable_device_ref_rail(&mut self) -> bool {
        let rail = self.profile.vddp_ref_clk_power;
        let Some(resource_id) = rail.resource_id else {
            return false;
        };
        let Some(set_mask) = rail.qcom_set else {
            return false;
        };
        unsafe {
            super::platform::bramble::send_rpmh_regulator_request(
                &resource_id,
                Some(1_200_000),
                7,
                true,
                set_mask,
            )
        }
    }
}

#[cfg(fullerene_aarch64_bramble)]
impl ControllerOps for BramblePlatformOps {
    fn read_controller(&mut self, offset: u32) -> u32 {
        unsafe { Self::read32(self.controller_base, offset) }
    }

    fn write_controller(&mut self, offset: u32, value: u32) {
        unsafe {
            Self::write32(self.controller_base, offset, value);
            Self::barrier();
        }
    }

    fn delay_us(&mut self, microseconds: u32) {
        super::timer::delay_us(microseconds as u64);
    }
}

#[cfg(fullerene_aarch64_bramble)]
const UFS_TRANSFER_TIMEOUT_US: u32 = 1_500_000;

#[cfg(fullerene_aarch64_bramble)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadOnlyProbeError {
    DmaContractUnproven,
    Platform(PlatformError),
    Link(LinkStartupError),
    Transfer(TransferEngineError),
    CommandStatus(u8),
    InvalidResponse,
    Descriptor(DescriptorError),
}

#[cfg(fullerene_aarch64_bramble)]
unsafe fn arena_transfer_list() -> &'static mut [u8] {
    core::slice::from_raw_parts_mut(
        core::ptr::addr_of_mut!(UFS_DMA_ARENA.transfer_list).cast::<u8>(),
        UfsDmaLayout::TRANSFER_LIST_BYTES,
    )
}

#[cfg(fullerene_aarch64_bramble)]
unsafe fn arena_task_list() -> &'static mut [u8] {
    core::slice::from_raw_parts_mut(
        core::ptr::addr_of_mut!(UFS_DMA_ARENA.task_list).cast::<u8>(),
        UfsDmaLayout::TASK_LIST_BYTES,
    )
}

#[cfg(fullerene_aarch64_bramble)]
unsafe fn arena_devman_descriptor() -> &'static mut [u8] {
    core::slice::from_raw_parts_mut(
        core::ptr::addr_of_mut!(UFS_DMA_ARENA.devman_descriptor).cast::<u8>(),
        UfsDmaLayout::DEVMAN_DESCRIPTOR_BYTES,
    )
}

#[cfg(fullerene_aarch64_bramble)]
unsafe fn arena_command_descriptor() -> &'static mut [u8] {
    core::slice::from_raw_parts_mut(
        core::ptr::addr_of_mut!(UFS_DMA_ARENA.command_descriptor).cast::<u8>(),
        UfsDmaLayout::COMMAND_DESCRIPTOR_BYTES,
    )
}

#[cfg(fullerene_aarch64_bramble)]
unsafe fn arena_data() -> &'static mut [u8] {
    core::slice::from_raw_parts_mut(
        core::ptr::addr_of_mut!(UFS_DMA_ARENA.data).cast::<u8>(),
        UfsDmaLayout::DATA_BYTES,
    )
}

#[cfg(fullerene_aarch64_bramble)]
unsafe fn cache_line_size() -> usize {
    let ctr: u64;
    core::arch::asm!(
        "mrs {ctr}, ctr_el0",
        ctr = out(reg) ctr,
        options(nostack, preserves_flags)
    );
    4usize << ((ctr >> 16) & 0xf) as usize
}

#[cfg(fullerene_aarch64_bramble)]
unsafe fn cache_maintain_range(address: u64, length: usize, clean: bool, invalidate: bool) {
    if length == 0 {
        return;
    }
    let line = unsafe { cache_line_size() }.max(4);
    let mask = (line - 1) as u64;
    let start = address & !mask;
    let end = address.saturating_add(length as u64).saturating_add(mask) & !mask;
    let mut current = start;
    while current < end {
        if clean && invalidate {
            core::arch::asm!("dc civac, {address}", address = in(reg) current, options(nostack));
        } else if clean {
            core::arch::asm!("dc cvac, {address}", address = in(reg) current, options(nostack));
        } else if invalidate {
            core::arch::asm!("dc ivac, {address}", address = in(reg) current, options(nostack));
        }
        current = current.saturating_add(line as u64);
    }
    core::arch::asm!("dsb sy", options(nostack, preserves_flags));
}

#[cfg(fullerene_aarch64_bramble)]
fn bramble_dma_contract() -> Result<DmaContract, ReadOnlyProbeError> {
    // The active DTB has no UFS `iommus` property, but absence is not proof of
    // identity DMA. Require a deliberate build-time assertion for this first
    // physical read-only trial; the cache instructions below are the other
    // half of the contract.
    if option_env!("FULLERENE_AARCH64_UFS_DMA_IDENTITY") != Some("1") {
        return Err(ReadOnlyProbeError::DmaContractUnproven);
    }
    let (transfer_list, task_list, devman_descriptor, command_descriptor, data) =
        unsafe { UfsDmaLayout::addresses() };
    let dma = DmaContract {
        transfer_list,
        task_list,
        devman_descriptor,
        command_descriptor,
        data,
        dma_visible: true,
        cache_maintained: true,
    };
    dma.ready()
        .then_some(dma)
        .ok_or(ReadOnlyProbeError::DmaContractUnproven)
}

#[cfg(fullerene_aarch64_bramble)]
pub(crate) struct BrambleUfsBlockDevice {
    backend: BramblePlatformOps,
    dma: DmaContract,
    lun: u8,
    block_size: u32,
    total_blocks: u64,
    slot: usize,
    next_tag: u8,
}

#[cfg(fullerene_aarch64_bramble)]
static BRAMBLE_UFS_DEVICE: spin::Mutex<Option<BrambleUfsBlockDevice>> = spin::Mutex::new(None);

/// A filesystem-facing handle for the installed read-only backend. The
/// controller-owning object stays in the global mutex; this zero-sized handle
/// prevents the VFS from taking ownership of platform state or DMA buffers.
#[cfg(fullerene_aarch64_bramble)]
pub(crate) struct BrambleUfsReadOnlyHandle;

#[cfg(fullerene_aarch64_bramble)]
impl BlockDevice for BrambleUfsReadOnlyHandle {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        read_bramble_blocks(lba, count, buf)
    }

    fn write_sectors(&mut self, _lba: u64, _count: u16, _buf: &[u8]) -> Result<(), BlockError> {
        Err(BlockError::Device)
    }

    fn sector_size(&self) -> u32 {
        bramble_block_info().map_or(0, |(sector_size, _)| sector_size)
    }

    fn total_sectors(&self) -> u64 {
        bramble_block_info().map_or(0, |(_, total_sectors)| total_sectors)
    }
}

#[cfg(fullerene_aarch64_bramble)]
impl BrambleUfsBlockDevice {
    fn submit_transfer(
        &mut self,
        request: hci::TransferRequestDescriptor,
        descriptor_address: u64,
        descriptor_bytes: usize,
        data_address: Option<u64>,
        data_bytes: usize,
    ) -> Result<(), ReadOnlyProbeError> {
        let installed = unsafe { install_transfer_slot(arena_transfer_list(), self.slot, request) };
        if !installed {
            return Err(ReadOnlyProbeError::Transfer(
                TransferEngineError::InvalidSlot,
            ));
        }
        unsafe {
            cache_maintain_range(
                self.dma.transfer_list,
                hci::TRANSFER_REQUEST_DESCRIPTOR_SIZE,
                true,
                false,
            );
            cache_maintain_range(descriptor_address, descriptor_bytes, true, false);
            if let Some(address) = data_address {
                cache_maintain_range(address, data_bytes, true, true);
            }
        }
        let result = ring_transfer_request(&mut self.backend, self.dma, self.slot).and_then(|_| {
            poll_transfer_completion(&mut self.backend, self.slot, UFS_TRANSFER_TIMEOUT_US)
        });
        unsafe {
            cache_maintain_range(
                self.dma.transfer_list,
                hci::TRANSFER_REQUEST_DESCRIPTOR_SIZE,
                false,
                true,
            );
            cache_maintain_range(descriptor_address, descriptor_bytes, false, true);
            if let Some(address) = data_address {
                cache_maintain_range(address, data_bytes, false, true);
            }
        }
        result.map_err(ReadOnlyProbeError::Transfer)?;
        let status = unsafe { transfer_slot_ocs(arena_transfer_list(), self.slot) }.ok_or(
            ReadOnlyProbeError::Transfer(TransferEngineError::InvalidSlot),
        )?;
        if status != hci::OCS_SUCCESS {
            return Err(ReadOnlyProbeError::CommandStatus(status));
        }
        Ok(())
    }

    fn next_tag(&mut self) -> u8 {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        tag
    }

    fn submit_nop(&mut self) -> Result<(), ReadOnlyProbeError> {
        let tag = self.next_tag();
        let request = unsafe {
            hci::prepare_nop_out(self.dma.devman_descriptor, arena_devman_descriptor(), tag)
        }
        .ok_or(ReadOnlyProbeError::InvalidResponse)?;
        self.submit_transfer(
            request,
            self.dma.devman_descriptor,
            UfsDmaLayout::DEVMAN_DESCRIPTOR_BYTES,
            None,
            0,
        )?;
        let response = unsafe {
            core::slice::from_raw_parts(
                (self.dma.devman_descriptor
                    + hci::DEVMAN_COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET as u64)
                    as *const u8,
                hci::UPIU_HEADER_SIZE,
            )
        };
        if hci::nop_response_success(response, tag) {
            Ok(())
        } else {
            Err(ReadOnlyProbeError::InvalidResponse)
        }
    }

    fn query_descriptor(
        &mut self,
        idn: u8,
        index: u8,
    ) -> Result<([u8; hci::QUERY_DESC_MAX_SIZE as usize], usize), ReadOnlyProbeError> {
        let tag = self.next_tag();
        let request = unsafe {
            hci::prepare_query_read_desc(
                self.dma.devman_descriptor,
                arena_devman_descriptor(),
                tag,
                idn,
                index,
                hci::QUERY_DESC_MAX_SIZE,
            )
        }
        .ok_or(ReadOnlyProbeError::InvalidResponse)?;
        self.submit_transfer(
            request,
            self.dma.devman_descriptor,
            UfsDmaLayout::DEVMAN_DESCRIPTOR_BYTES,
            None,
            0,
        )?;
        let response = unsafe {
            core::slice::from_raw_parts(
                (self.dma.devman_descriptor
                    + hci::DEVMAN_COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET as u64)
                    as *const u8,
                hci::ALIGNED_DEVMAN_RSP_SIZE as usize,
            )
        };
        let mut descriptor = [0_u8; hci::QUERY_DESC_MAX_SIZE as usize];
        let length = hci::copy_query_read_desc_response(response, &mut descriptor)
            .ok_or(ReadOnlyProbeError::InvalidResponse)?;
        Ok((descriptor, length))
    }
}

#[cfg(fullerene_aarch64_bramble)]
impl BlockDevice for BrambleUfsBlockDevice {
    fn read_sectors(&mut self, lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
        if count == 0 {
            return Ok(());
        }
        let required = (count as usize)
            .checked_mul(self.block_size as usize)
            .ok_or(BlockError::LbaOverflow)?;
        if buf.len() < required {
            return Err(BlockError::BufferTooSmall {
                required,
                provided: buf.len(),
            });
        }
        let end = lba
            .checked_add(count as u64)
            .ok_or(BlockError::LbaOverflow)?;
        if end > self.total_blocks
            || lba > u64::from(u32::MAX)
            || required > UfsDmaLayout::DATA_BYTES
        {
            return Err(BlockError::LbaOverflow);
        }
        let tag = self.next_tag();
        let request = unsafe {
            hci::prepare_scsi_read10(
                self.dma.command_descriptor,
                arena_command_descriptor(),
                self.dma.data,
                tag,
                self.lun,
                lba as u32,
                count,
                self.block_size,
            )
        }
        .ok_or(BlockError::Device)?;
        self.submit_transfer(
            request,
            self.dma.command_descriptor,
            UfsDmaLayout::COMMAND_DESCRIPTOR_BYTES,
            Some(self.dma.data),
            required,
        )
        .map_err(|_| BlockError::Device)?;
        let response = unsafe {
            core::slice::from_raw_parts(
                (self.dma.command_descriptor + hci::COMMAND_DESCRIPTOR_RESPONSE_UPIU_OFFSET as u64)
                    as *const u8,
                hci::ALIGNED_UPIU_SIZE as usize,
            )
        };
        if !hci::scsi_response_success(response, tag) {
            return Err(BlockError::Device);
        }
        let data = unsafe { arena_data() };
        buf[..required].copy_from_slice(&data[..required]);
        Ok(())
    }

    fn write_sectors(&mut self, _lba: u64, _count: u16, _buf: &[u8]) -> Result<(), BlockError> {
        // The first storage registration is intentionally read-only. No
        // WRITE(10), descriptor write, format, or partition operation is
        // reachable through this adapter.
        Err(BlockError::Device)
    }

    fn sector_size(&self) -> u32 {
        self.block_size
    }

    fn total_sectors(&self) -> u64 {
        self.total_blocks
    }
}

#[cfg(fullerene_aarch64_bramble)]
pub(crate) fn install_bramble_block_device(device: BrambleUfsBlockDevice) -> bool {
    let mut registered = BRAMBLE_UFS_DEVICE.lock();
    if registered.is_some() {
        return false;
    }
    *registered = Some(device);
    true
}

#[cfg(fullerene_aarch64_bramble)]
pub(crate) fn bramble_read_only_handle() -> Option<BrambleUfsReadOnlyHandle> {
    bramble_block_info().map(|_| BrambleUfsReadOnlyHandle)
}

#[cfg(fullerene_aarch64_bramble)]
pub(crate) fn read_bramble_blocks(lba: u64, count: u16, buf: &mut [u8]) -> Result<(), BlockError> {
    BRAMBLE_UFS_DEVICE
        .lock()
        .as_mut()
        .ok_or(BlockError::Device)?
        .read_sectors(lba, count, buf)
}

pub(crate) fn bramble_block_info() -> Option<(u32, u64)> {
    #[cfg(fullerene_aarch64_bramble)]
    {
        return BRAMBLE_UFS_DEVICE
            .lock()
            .as_ref()
            .map(|device| (device.sector_size(), device.total_sectors()));
    }
    #[cfg(not(fullerene_aarch64_bramble))]
    {
        None
    }
}

#[cfg(fullerene_aarch64_bramble)]
pub(crate) fn execute_bramble_read_only(
    profile: PlatformContract,
    rate_b: bool,
) -> Result<(BrambleUfsBlockDevice, UfsGeometry), ReadOnlyProbeError> {
    let dma = bramble_dma_contract()?;
    let mut backend = BramblePlatformOps::new(profile).map_err(ReadOnlyProbeError::Platform)?;
    execute_platform(profile, &mut backend, rate_b).map_err(ReadOnlyProbeError::Platform)?;
    execute_link_startup(profile, &mut backend).map_err(ReadOnlyProbeError::Link)?;

    unsafe {
        arena_transfer_list().fill(0);
        arena_task_list().fill(0);
        cache_maintain_range(
            dma.transfer_list,
            UfsDmaLayout::TRANSFER_LIST_BYTES,
            true,
            false,
        );
        cache_maintain_range(dma.task_list, UfsDmaLayout::TASK_LIST_BYTES, true, false);
    }
    configure_transfer_engine(&mut backend, dma).map_err(ReadOnlyProbeError::Transfer)?;

    let mut device = BrambleUfsBlockDevice {
        backend,
        dma,
        lun: 0,
        block_size: 0,
        total_blocks: 0,
        slot: 0,
        next_tag: 0,
    };
    device.submit_nop()?;
    let (device_descriptor, device_descriptor_len) = device.query_descriptor(0x00, 0)?;
    let device_num_lu = parse_device_num_lu(&device_descriptor[..device_descriptor_len])
        .map_err(ReadOnlyProbeError::Descriptor)?;
    if device_num_lu == 0 {
        return Err(ReadOnlyProbeError::Descriptor(
            DescriptorError::InvalidField,
        ));
    }
    let (unit_descriptor, unit_descriptor_len) = device.query_descriptor(0x02, 0)?;
    let geometry = parse_unit_geometry(&unit_descriptor[..unit_descriptor_len], 0)
        .map_err(ReadOnlyProbeError::Descriptor)?;
    device.lun = geometry.lun;
    device.block_size = geometry.block_size;
    device.total_blocks = geometry.total_blocks;
    unsafe {
        STAGE = BringupStage::BlockReady;
    }
    Ok((device, geometry))
}

#[cfg(fullerene_aarch64_bramble)]
pub(crate) fn execute_bramble_platform(
    profile: PlatformContract,
    rate_b: bool,
) -> Result<BringupStage, PlatformError> {
    let mut backend = BramblePlatformOps::new(profile)?;
    execute_platform(profile, &mut backend, rate_b)
}

#[cfg(fullerene_aarch64_bramble)]
pub(crate) fn execute_bramble_link_startup(
    profile: PlatformContract,
) -> Result<BringupStage, LinkStartupError> {
    let mut backend = BramblePlatformOps::new(profile).map_err(|error| match error {
        PlatformError::UnmappedRegulators => LinkStartupError::UnmappedRegulators,
        PlatformError::PowerStateUnsupported => LinkStartupError::PowerStateUnsupported,
        _ => LinkStartupError::InvalidContract,
    })?;
    execute_link_startup(profile, &mut backend)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PropertyShape {
    pub present: bool,
    pub bytes: u32,
}

/// One provider-local DT clock/reset specifier.  The first cell is a DT
/// phandle, not a globally meaningful clock number; retaining both cells is
/// mandatory before any later GCC/RPMh operation can be made safe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProviderSpec {
    pub provider: u32,
    pub id: u32,
}

/// Supply information resolved from the active DTB. A consumer phandle is
/// not enough for an RPMh vote: the child regulator name, its parent
/// `qcom,resource-name`, provider limits, and consumer load/voltage request
/// must all agree before the backend can claim the rail.
#[derive(Clone, Copy)]
pub(crate) struct SupplyContract {
    pub phandle: Option<u32>,
    pub regulator_name: Option<fdt::StringValue>,
    pub resource_name: Option<fdt::StringValue>,
    pub region: Option<fdt::Region>,
    pub provider_min_uv: Option<u32>,
    pub provider_max_uv: Option<u32>,
    pub provider_init_uv: Option<u32>,
    pub consumer_min_uv: Option<u32>,
    pub consumer_max_uv: Option<u32>,
    pub consumer_max_load_ua: Option<u32>,
    pub qcom_set: Option<u32>,
    pub resource_id: Option<[u8; 8]>,
}

const EMPTY_SUPPLY_CONTRACT: SupplyContract = SupplyContract {
    phandle: None,
    regulator_name: None,
    resource_name: None,
    region: None,
    provider_min_uv: None,
    provider_max_uv: None,
    provider_init_uv: None,
    consumer_min_uv: None,
    consumer_max_uv: None,
    consumer_max_load_ua: None,
    qcom_set: None,
    resource_id: None,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClockGate {
    pub provider: u32,
    pub id: u32,
    pub branch_offset: u32,
    pub enable_mask: u32,
    pub source_offset: Option<u32>,
}

/// GCC register locations from the primary Lito clock driver.  The clock
/// number alone is not enough to program a branch: HW-control clocks share a
/// register with their software-controlled sibling and use a different bit.
pub(crate) fn lito_clock_gate(spec: ProviderSpec) -> Option<ClockGate> {
    if spec.provider != GCC_PHANDLE {
        return None;
    }
    let (branch_offset, enable_mask, source_offset) = match spec.id {
        0x67 => (0x77010, 1 << 0, Some(0x77024)), // UFS PHY AXI
        0x81 => (0x770cc, 1 << 0, Some(0x77024)), // aggregate UFS AXI
        0x66 => (0x77018, 1 << 0, None),          // UFS PHY AHB
        0x70 => (0x7705c, 1 << 0, Some(0x77084)), // UniPro core
        0x69 => (0x77064, 1 << 0, Some(0x7706c)), // ICE core
        0x8a => (0x77064, 1 << 1, Some(0x7706c)), // ICE HW control
        0x6f => (0x7701c, 1 << 0, None),          // TX symbol lane 0
        0x6d => (0x77020, 1 << 0, None),          // RX symbol lane 0
        0x6e => (0x770b8, 1 << 0, None),          // RX symbol lane 1
        0x65 => (0x8c000, 1 << 0, None),          // UFS device ref clock
        0x6b => (0x7709c, 1 << 0, Some(0x770a0)), // PHY auxiliary clock
        _ => return None,
    };
    Some(ClockGate {
        provider: spec.provider,
        id: spec.id,
        branch_offset,
        enable_mask,
        source_offset,
    })
}

pub(crate) const LITO_GCC_BASE: u64 = 0x0010_0000;
pub(crate) const LITO_GCC_SIZE: u64 = 0x001f_0000;
pub(crate) const LITO_UFS_RESET_OFFSET: u64 = 0x77000;
pub(crate) const LITO_RPMH_RSC_BASE: u64 = 0x1820_0000;
pub(crate) const LITO_UFS_PHY_COM_BASE: u32 = 0x000;
pub(crate) const LITO_UFS_PHY_BASE: u32 = 0xc00;
pub(crate) const LITO_UFS_PHY_START: u32 = LITO_UFS_PHY_BASE;
pub(crate) const LITO_UFS_PHY_POWER_DOWN_CONTROL: u32 = LITO_UFS_PHY_BASE + 0x04;
pub(crate) const LITO_UFS_PHY_SW_RESET: u32 = LITO_UFS_PHY_BASE + 0x08;
pub(crate) const LITO_UFS_PHY_PCS_READY_STATUS: u32 = LITO_UFS_PHY_BASE + 0x180;
pub(crate) const LITO_UFS_PHY_LINECFG_DISABLE: u32 = LITO_UFS_PHY_BASE + 0x148;
const LITO_UFS_PHY_RX0_INTERFACE_MODE: u32 = 0x734;
const LITO_UFS_PHY_RX1_INTERFACE_MODE: u32 = 0xb34;
const LITO_UFS_PHY_RX_INTERFACE_CLOCK_EDGE: u32 = 1 << 5;
const LITO_UFS_CONTROLLER_CFG1: u32 = 0xdc;
const LITO_UFS_CONTROLLER_CFG1_UNIPRO_SEL: u32 = 1 << 0;
const LITO_UFS_CONTROLLER_CFG1_PHY_SOFT_RESET: u32 = 1 << 1;
const LITO_UFS_HCE: u32 = 0x34;

const SERDES_START_MASK: u32 = 0x1;
const PCS_READY_MASK: u32 = 0x1;

/// One byte written to one QMP register during the Lito calibration tables.
/// These are copied as data from the Android primary PHY source; the executor
/// supplies the ordering, reset pulse, and barriers around them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CalibrationEntry {
    pub offset: u32,
    pub value: u8,
}

const LITO_RATE_A_NO_G4: &[CalibrationEntry] = &[
    CalibrationEntry {
        offset: 0xc04,
        value: 0x01,
    },
    CalibrationEntry {
        offset: 0x094,
        value: 0xd9,
    },
    CalibrationEntry {
        offset: 0x158,
        value: 0x11,
    },
    CalibrationEntry {
        offset: 0x15c,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0x0a4,
        value: 0x01,
    },
    CalibrationEntry {
        offset: 0x10c,
        value: 0x02,
    },
    CalibrationEntry {
        offset: 0x058,
        value: 0x0f,
    },
    CalibrationEntry {
        offset: 0x124,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0x1bc,
        value: 0x11,
    },
    CalibrationEntry {
        offset: 0x0bc,
        value: 0x82,
    },
    CalibrationEntry {
        offset: 0x074,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0x07c,
        value: 0x16,
    },
    CalibrationEntry {
        offset: 0x084,
        value: 0x36,
    },
    CalibrationEntry {
        offset: 0x0ac,
        value: 0xff,
    },
    CalibrationEntry {
        offset: 0x0b0,
        value: 0x0c,
    },
    CalibrationEntry {
        offset: 0x1ac,
        value: 0xac,
    },
    CalibrationEntry {
        offset: 0x1b0,
        value: 0x1e,
    },
    CalibrationEntry {
        offset: 0x0c4,
        value: 0x98,
    },
    CalibrationEntry {
        offset: 0x078,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0x080,
        value: 0x16,
    },
    CalibrationEntry {
        offset: 0x088,
        value: 0x36,
    },
    CalibrationEntry {
        offset: 0x0b4,
        value: 0x32,
    },
    CalibrationEntry {
        offset: 0x0b8,
        value: 0x0f,
    },
    CalibrationEntry {
        offset: 0x1b4,
        value: 0xdd,
    },
    CalibrationEntry {
        offset: 0x1b8,
        value: 0x23,
    },
    CalibrationEntry {
        offset: 0x568,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0x56c,
        value: 0x03,
    },
    CalibrationEntry {
        offset: 0x570,
        value: 0x01,
    },
    CalibrationEntry {
        offset: 0x574,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0x484,
        value: 0xf5,
    },
    CalibrationEntry {
        offset: 0x48c,
        value: 0x3f,
    },
    CalibrationEntry {
        offset: 0x43c,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0x440,
        value: 0x09,
    },
    CalibrationEntry {
        offset: 0x4c0,
        value: 0x0c,
    },
    CalibrationEntry {
        offset: 0x720,
        value: 0x24,
    },
    CalibrationEntry {
        offset: 0x71c,
        value: 0x0f,
    },
    CalibrationEntry {
        offset: 0x724,
        value: 0x1e,
    },
    CalibrationEntry {
        offset: 0x728,
        value: 0x18,
    },
    CalibrationEntry {
        offset: 0x630,
        value: 0x0a,
    },
    CalibrationEntry {
        offset: 0x634,
        value: 0x5a,
    },
    CalibrationEntry {
        offset: 0x644,
        value: 0xf1,
    },
    CalibrationEntry {
        offset: 0x63c,
        value: 0x80,
    },
    CalibrationEntry {
        offset: 0x648,
        value: 0x80,
    },
    CalibrationEntry {
        offset: 0x608,
        value: 0x0c,
    },
    CalibrationEntry {
        offset: 0x614,
        value: 0x04,
    },
    CalibrationEntry {
        offset: 0x680,
        value: 0x1b,
    },
    CalibrationEntry {
        offset: 0x6ec,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0x6f0,
        value: 0x04,
    },
    CalibrationEntry {
        offset: 0x6f4,
        value: 0x1d,
    },
    CalibrationEntry {
        offset: 0x714,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0x700,
        value: 0x10,
    },
    CalibrationEntry {
        offset: 0x6f8,
        value: 0xc0,
    },
    CalibrationEntry {
        offset: 0x6fc,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0x75c,
        value: 0x64,
    },
    CalibrationEntry {
        offset: 0x760,
        value: 0x64,
    },
    CalibrationEntry {
        offset: 0x764,
        value: 0x24,
    },
    CalibrationEntry {
        offset: 0x768,
        value: 0x3f,
    },
    CalibrationEntry {
        offset: 0x76c,
        value: 0x1f,
    },
    CalibrationEntry {
        offset: 0x770,
        value: 0xe0,
    },
    CalibrationEntry {
        offset: 0x774,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0x778,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0x77c,
        value: 0x3b,
    },
    CalibrationEntry {
        offset: 0x780,
        value: 0xb1,
    },
    CalibrationEntry {
        offset: 0x784,
        value: 0xe0,
    },
    CalibrationEntry {
        offset: 0x788,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0x78c,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0x790,
        value: 0x3b,
    },
    CalibrationEntry {
        offset: 0x794,
        value: 0xb1,
    },
    CalibrationEntry {
        offset: 0x7a8,
        value: 0x0c,
    },
    CalibrationEntry {
        offset: 0x6d8,
        value: 0x04,
    },
    CalibrationEntry {
        offset: 0xd58,
        value: 0x6d,
    },
    CalibrationEntry {
        offset: 0xc30,
        value: 0x0a,
    },
    CalibrationEntry {
        offset: 0xc38,
        value: 0x02,
    },
    CalibrationEntry {
        offset: 0xdd8,
        value: 0x43,
    },
    CalibrationEntry {
        offset: 0xdc4,
        value: 0x1f,
    },
    CalibrationEntry {
        offset: 0xd50,
        value: 0xff,
    },
    CalibrationEntry {
        offset: 0xc2c,
        value: 0x03,
    },
    CalibrationEntry {
        offset: 0xc0c,
        value: 0x16,
    },
    CalibrationEntry {
        offset: 0xc10,
        value: 0xd8,
    },
    CalibrationEntry {
        offset: 0xd60,
        value: 0xaa,
    },
    CalibrationEntry {
        offset: 0xd68,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0xc74,
        value: 0x03,
    },
    CalibrationEntry {
        offset: 0xcb4,
        value: 0x03,
    },
    CalibrationEntry {
        offset: 0xd54,
        value: 0x0e,
    },
];

const LITO_SECOND_LANE_NO_G4: &[CalibrationEntry] = &[
    CalibrationEntry {
        offset: 0x968,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0x96c,
        value: 0x03,
    },
    CalibrationEntry {
        offset: 0x970,
        value: 0x01,
    },
    CalibrationEntry {
        offset: 0x974,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0x884,
        value: 0xf5,
    },
    CalibrationEntry {
        offset: 0x88c,
        value: 0x3f,
    },
    CalibrationEntry {
        offset: 0x83c,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0x840,
        value: 0x09,
    },
    CalibrationEntry {
        offset: 0x8c0,
        value: 0x0c,
    },
    CalibrationEntry {
        offset: 0xb20,
        value: 0x24,
    },
    CalibrationEntry {
        offset: 0xb1c,
        value: 0x0f,
    },
    CalibrationEntry {
        offset: 0xb24,
        value: 0x1e,
    },
    CalibrationEntry {
        offset: 0xb28,
        value: 0x18,
    },
    CalibrationEntry {
        offset: 0xa30,
        value: 0x0a,
    },
    CalibrationEntry {
        offset: 0xa34,
        value: 0x5a,
    },
    CalibrationEntry {
        offset: 0xa44,
        value: 0xf1,
    },
    CalibrationEntry {
        offset: 0xa3c,
        value: 0x80,
    },
    CalibrationEntry {
        offset: 0xa48,
        value: 0x80,
    },
    CalibrationEntry {
        offset: 0xa08,
        value: 0x0c,
    },
    CalibrationEntry {
        offset: 0xa14,
        value: 0x04,
    },
    CalibrationEntry {
        offset: 0xa80,
        value: 0x1b,
    },
    CalibrationEntry {
        offset: 0xaec,
        value: 0x06,
    },
    CalibrationEntry {
        offset: 0xaf0,
        value: 0x04,
    },
    CalibrationEntry {
        offset: 0xaf4,
        value: 0x1d,
    },
    CalibrationEntry {
        offset: 0xb14,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0xb00,
        value: 0x10,
    },
    CalibrationEntry {
        offset: 0xaf8,
        value: 0xc0,
    },
    CalibrationEntry {
        offset: 0xafc,
        value: 0x00,
    },
    CalibrationEntry {
        offset: 0xb5c,
        value: 0x64,
    },
    CalibrationEntry {
        offset: 0xb60,
        value: 0x64,
    },
    CalibrationEntry {
        offset: 0xb64,
        value: 0x24,
    },
    CalibrationEntry {
        offset: 0xb68,
        value: 0x3f,
    },
    CalibrationEntry {
        offset: 0xb6c,
        value: 0x1f,
    },
    CalibrationEntry {
        offset: 0xb70,
        value: 0xe0,
    },
    CalibrationEntry {
        offset: 0xb74,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0xb78,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0xb7c,
        value: 0x3b,
    },
    CalibrationEntry {
        offset: 0xb80,
        value: 0xb1,
    },
    CalibrationEntry {
        offset: 0xb84,
        value: 0xe0,
    },
    CalibrationEntry {
        offset: 0xb88,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0xb8c,
        value: 0xc8,
    },
    CalibrationEntry {
        offset: 0xb90,
        value: 0x3b,
    },
    CalibrationEntry {
        offset: 0xb94,
        value: 0xb1,
    },
    CalibrationEntry {
        offset: 0xba8,
        value: 0x0c,
    },
    CalibrationEntry {
        offset: 0xad8,
        value: 0x04,
    },
    CalibrationEntry {
        offset: 0xde0,
        value: 0x02,
    },
];

const LITO_RATE_B: &[CalibrationEntry] = &[CalibrationEntry {
    offset: 0x10c,
    value: 0x06,
}];

pub(crate) fn platform_sequence(profile: PlatformContract) -> Option<PlatformSequence> {
    profile
        .resource_contract_valid()
        .then_some(PlatformSequence::LITO)
}

const EMPTY_PROVIDER_SPEC: ProviderSpec = ProviderSpec { provider: 0, id: 0 };

#[derive(Clone, Copy)]
pub(crate) struct PlatformContract {
    pub controller: [fdt::Region; 2],
    pub controller_regions: u8,
    pub phy: [fdt::Region; 2],
    pub phy_regions: u8,
    pub gcc_region: Option<fdt::Region>,
    /// The second cell of the Qualcomm `interrupts = <0 spi flags>` tuple.
    pub irq_spi: Option<u32>,
    pub clocks: PropertyShape,
    pub clock_names: PropertyShape,
    pub controller_clocks: [ProviderSpec; 10],
    pub controller_clock_count: u8,
    pub phy_clocks: [ProviderSpec; 3],
    pub phy_clock_count: u8,
    pub phy_clock_names: PropertyShape,
    pub qphy_resource_name: PropertyShape,
    pub clock_names_hash: Option<u32>,
    pub phy_clock_names_hash: Option<u32>,
    pub reset_names_hash: Option<u32>,
    pub qphy_resource_name_hash: Option<u32>,
    pub resets: PropertyShape,
    pub reset_names: PropertyShape,
    pub core_reset: Option<ProviderSpec>,
    pub lanes_per_direction: Option<u32>,
    pub dev_ref_clk_freq: Option<u32>,
    pub power_domains: PropertyShape,
    pub iommus: PropertyShape,
    pub vdd_hba_supply: PropertyShape,
    pub vcc_supply: PropertyShape,
    pub vccq_supply: PropertyShape,
    pub vccq2_supply: PropertyShape,
    pub phy_vdda_supply: PropertyShape,
    pub phy_vdda_pll_supply: PropertyShape,
    pub vdd_hba_power: SupplyContract,
    pub vcc_power: SupplyContract,
    pub vccq2_power: SupplyContract,
    pub vddp_ref_clk_supply: PropertyShape,
    pub vddp_ref_clk_power: SupplyContract,
    pub phy_vdda_power: SupplyContract,
    pub phy_vdda_pll_power: SupplyContract,
}

const EMPTY_REGION: fdt::Region = fdt::Region { base: 0, size: 0 };
const EMPTY_PROPERTY: PropertyShape = PropertyShape {
    present: false,
    bytes: 0,
};

impl PlatformContract {
    /// Bits returned by [`Self::missing_required_mask`].
    pub const MISSING_CONTROLLER_REGION: u32 = 1 << 0;
    pub const MISSING_PHY_REGION: u32 = 1 << 1;
    pub const MISSING_INTERRUPT: u32 = 1 << 2;
    pub const MISSING_CLOCKS: u32 = 1 << 3;
    pub const MISSING_CLOCK_NAMES: u32 = 1 << 4;
    pub const MISSING_RESETS: u32 = 1 << 5;
    pub const MISSING_RESET_NAMES: u32 = 1 << 6;
    pub const MISSING_RESOURCE_CONTRACT: u32 = 1 << 7;

    pub(crate) const fn empty() -> Self {
        Self {
            controller: [EMPTY_REGION; 2],
            controller_regions: 0,
            phy: [EMPTY_REGION; 2],
            phy_regions: 0,
            gcc_region: None,
            irq_spi: None,
            clocks: EMPTY_PROPERTY,
            clock_names: EMPTY_PROPERTY,
            controller_clocks: [EMPTY_PROVIDER_SPEC; 10],
            controller_clock_count: 0,
            phy_clocks: [EMPTY_PROVIDER_SPEC; 3],
            phy_clock_count: 0,
            phy_clock_names: EMPTY_PROPERTY,
            qphy_resource_name: EMPTY_PROPERTY,
            clock_names_hash: None,
            phy_clock_names_hash: None,
            reset_names_hash: None,
            qphy_resource_name_hash: None,
            resets: EMPTY_PROPERTY,
            reset_names: EMPTY_PROPERTY,
            core_reset: None,
            lanes_per_direction: None,
            dev_ref_clk_freq: None,
            power_domains: EMPTY_PROPERTY,
            iommus: EMPTY_PROPERTY,
            vdd_hba_supply: EMPTY_PROPERTY,
            vcc_supply: EMPTY_PROPERTY,
            vccq_supply: EMPTY_PROPERTY,
            vccq2_supply: EMPTY_PROPERTY,
            phy_vdda_supply: EMPTY_PROPERTY,
            phy_vdda_pll_supply: EMPTY_PROPERTY,
            vdd_hba_power: EMPTY_SUPPLY_CONTRACT,
            vcc_power: EMPTY_SUPPLY_CONTRACT,
            vccq2_power: EMPTY_SUPPLY_CONTRACT,
            vddp_ref_clk_supply: EMPTY_PROPERTY,
            vddp_ref_clk_power: EMPTY_SUPPLY_CONTRACT,
            phy_vdda_power: EMPTY_SUPPLY_CONTRACT,
            phy_vdda_pll_power: EMPTY_SUPPLY_CONTRACT,
        }
    }

    pub(crate) fn missing_required_mask(self) -> u32 {
        let mut missing = 0;
        if self.controller_regions < 2 {
            missing |= Self::MISSING_CONTROLLER_REGION;
        }
        if self.phy_regions < 2 {
            missing |= Self::MISSING_PHY_REGION;
        }
        if self.irq_spi.is_none() {
            missing |= Self::MISSING_INTERRUPT;
        }
        if !self.clocks.present {
            missing |= Self::MISSING_CLOCKS;
        }
        if !self.clock_names.present {
            missing |= Self::MISSING_CLOCK_NAMES;
        }
        if !self.resets.present {
            missing |= Self::MISSING_RESETS;
        }
        if !self.reset_names.present {
            missing |= Self::MISSING_RESET_NAMES;
        }
        if !self.resource_contract_valid() {
            missing |= Self::MISSING_RESOURCE_CONTRACT;
        }
        missing
    }

    /// Validate the provider-local IDs and string-list shapes from the
    /// Bramble/Lito DT.  These values are intentionally exact: accepting a
    /// clock number without its provider phandle would turn a later MMIO
    /// write into an arbitrary operation on another Qualcomm clock controller.
    pub(crate) fn resource_contract_valid(self) -> bool {
        self.controller_regions == 2
            && self.phy_regions == 2
            && region_matches(self.gcc_region, LITO_GCC_BASE, LITO_GCC_SIZE)
            && self.irq_spi == Some(265)
            && self.controller_clock_count == LITO_CONTROLLER_CLOCKS.len() as u8
            && self.phy_clock_count == LITO_PHY_CLOCKS.len() as u8
            && self.controller_clocks == LITO_CONTROLLER_CLOCKS
            && self.phy_clocks == LITO_PHY_CLOCKS
            && self.core_reset == Some(LITO_CORE_RESET)
            && self.clocks
                == PropertyShape {
                    present: true,
                    bytes: 80,
                }
            && self.clock_names
                == PropertyShape {
                    present: true,
                    bytes: 143,
                }
            && self.phy_clock_names
                == PropertyShape {
                    present: true,
                    bytes: 32,
                }
            && self.qphy_resource_name
                == PropertyShape {
                    present: true,
                    bytes: 9,
                }
            && self.resets
                == PropertyShape {
                    present: true,
                    bytes: 8,
                }
            && self.reset_names
                == PropertyShape {
                    present: true,
                    bytes: 11,
                }
            && self.lanes_per_direction == Some(2)
            && self.dev_ref_clk_freq == Some(0)
            && self.clock_names_hash == Some(LITO_CONTROLLER_CLOCK_NAMES_HASH)
            && self.phy_clock_names_hash == Some(LITO_PHY_CLOCK_NAMES_HASH)
            && self.reset_names_hash == Some(LITO_RESET_NAMES_HASH)
            && self.qphy_resource_name_hash == Some(LITO_QPHY_RESOURCE_NAME_HASH)
    }

    pub(crate) fn platform_ready(self) -> bool {
        self.missing_required_mask() == 0
    }

    pub(crate) fn has_unmapped_regulators(self) -> bool {
        !self.power_contract_valid()
    }

    /// Validate the exact merged-DTB power graph. The names and resource IDs
    /// are checked together so an overlay cannot redirect a valid-looking
    /// consumer name to a different RPMh accelerator.
    pub(crate) fn power_contract_valid(self) -> bool {
        supply_string_is(self.vdd_hba_power.regulator_name, b"ufs_phy_gdsc")
            && region_matches(self.vdd_hba_power.region, 0x177004, 0x4)
            && supply_string_is(self.vcc_power.regulator_name, b"pm8150a_l7")
            && supply_string_is(self.vcc_power.resource_name, b"ldoc7")
            && self.vcc_power.resource_id == Some(*b"ldoc7\0\0\0")
            && self.vcc_power.provider_min_uv == Some(2_704_000)
            && self.vcc_power.provider_max_uv == Some(3_304_000)
            && self.vcc_power.consumer_min_uv == Some(2_950_000)
            && self.vcc_power.consumer_max_uv == Some(2_960_000)
            && self.vcc_power.consumer_max_load_ua == Some(800_000)
            && self.vcc_power.qcom_set == Some(3)
            && supply_string_is(self.vccq2_power.regulator_name, b"pm8150_s4")
            && supply_string_is(self.vccq2_power.resource_name, b"smpa4")
            && self.vccq2_power.resource_id == Some(*b"smpa4\0\0\0")
            && self.vccq2_power.provider_min_uv == Some(1_800_000)
            && self.vccq2_power.provider_max_uv == Some(1_800_000)
            && self.vccq2_power.consumer_max_load_ua == Some(800_000)
            && self.vccq2_power.qcom_set == Some(3)
            && supply_string_is(self.vddp_ref_clk_power.regulator_name, b"pm8150_l9")
            && supply_string_is(self.vddp_ref_clk_power.resource_name, b"ldoa9")
            && self.vddp_ref_clk_power.resource_id == Some(*b"ldoa9\0\0\0")
            && self.vddp_ref_clk_power.provider_min_uv == Some(1_152_000)
            && self.vddp_ref_clk_power.provider_max_uv == Some(1_320_000)
            && self.vddp_ref_clk_power.consumer_max_load_ua == Some(100)
            && self.vddp_ref_clk_power.qcom_set == Some(3)
            && supply_string_is(self.phy_vdda_power.regulator_name, b"pm8150_l5")
            && supply_string_is(self.phy_vdda_power.resource_name, b"ldoa5")
            && self.phy_vdda_power.resource_id == Some(*b"ldoa5\0\0\0")
            && self.phy_vdda_power.provider_min_uv == Some(720_000)
            && self.phy_vdda_power.provider_max_uv == Some(1_056_000)
            && self.phy_vdda_power.consumer_max_load_ua == Some(90_200)
            && self.phy_vdda_power.qcom_set == Some(3)
            && supply_string_is(self.phy_vdda_pll_power.regulator_name, b"pm8150_l9")
            && supply_string_is(self.phy_vdda_pll_power.resource_name, b"ldoa9")
            && self.phy_vdda_pll_power.resource_id == Some(*b"ldoa9\0\0\0")
            && self.phy_vdda_pll_power.provider_min_uv == Some(1_152_000)
            && self.phy_vdda_pll_power.provider_max_uv == Some(1_320_000)
            && self.phy_vdda_pll_power.consumer_max_load_ua == Some(19_000)
            && self.phy_vdda_pll_power.qcom_set == Some(3)
    }

    /// The Bramble RPMh transport can now target both active and sleep TCS
    /// families. Keep the backend closed for a malformed set mask even when
    /// the names and provider ranges look correct.
    pub(crate) fn power_transaction_ready(self) -> bool {
        let valid_set = |set| matches!(set, Some(RPMH_SET_ACTIVE) | Some(RPMH_SET_ALL));
        self.power_contract_valid()
            && valid_set(self.phy_vdda_power.qcom_set)
            && valid_set(self.phy_vdda_pll_power.qcom_set)
            && valid_set(self.vcc_power.qcom_set)
            && valid_set(self.vccq2_power.qcom_set)
            && valid_set(self.vddp_ref_clk_power.qcom_set)
    }
}

const GCC_PHANDLE: u32 = 0x4f;
const RPMH_CLOCK_PHANDLE: u32 = 0x56;

const LITO_CONTROLLER_CLOCKS: [ProviderSpec; 10] = [
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x67,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x81,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x66,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x70,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x69,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x8a,
    },
    ProviderSpec {
        provider: RPMH_CLOCK_PHANDLE,
        id: 0,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x6f,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x6d,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x6e,
    },
];

const LITO_PHY_CLOCKS: [ProviderSpec; 3] = [
    ProviderSpec {
        provider: RPMH_CLOCK_PHANDLE,
        id: 0,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x65,
    },
    ProviderSpec {
        provider: GCC_PHANDLE,
        id: 0x6b,
    },
];

const LITO_CORE_RESET: ProviderSpec = ProviderSpec {
    provider: GCC_PHANDLE,
    id: 0x0c,
};

const LITO_CONTROLLER_CLOCK_NAMES_HASH: u32 = 0xeb3f65a6;
const LITO_PHY_CLOCK_NAMES_HASH: u32 = 0x87f348c3;
const LITO_RESET_NAMES_HASH: u32 = 0xc83d5336;
const LITO_QPHY_RESOURCE_NAME_HASH: u32 = 0xea4c294d;

static mut PLATFORM: Option<PlatformContract> = None;
static mut STAGE: BringupStage = BringupStage::Absent;

/// Decode the active DT resources without reading or writing any device
/// register. Both controller and PHY windows are retained for the later
/// driver; accepting the first window alone would hide an incomplete DT.
pub(crate) fn describe(address: u64) -> Option<PlatformContract> {
    let mut contract = PlatformContract::empty();
    contract.controller_regions =
        collect_regions(address, UFS_CONTROLLER, &mut contract.controller);
    contract.phy_regions = collect_regions(address, UFS_PHY, &mut contract.phy);
    contract.gcc_region = fdt::find_phandle_region(address, GCC_PHANDLE);
    if contract.controller_regions == 0 && contract.phy_regions == 0 {
        return None;
    }

    contract.irq_spi = fdt::find_compatible_property_u32(address, UFS_CONTROLLER, b"interrupts", 1)
        .or_else(|| {
            fdt::find_compatible_property_u32(address, UFS_CONTROLLER, b"interrupts-extended", 1)
        });
    contract.clocks = property(address, UFS_CONTROLLER, b"clocks");
    contract.clock_names = property(address, UFS_CONTROLLER, b"clock-names");
    contract.controller_clock_count = collect_provider_specs(
        address,
        UFS_CONTROLLER,
        b"clocks",
        &mut contract.controller_clocks,
    );
    contract.clock_names_hash =
        fdt::find_compatible_property_fnv1a(address, UFS_CONTROLLER, b"clock-names");
    contract.phy_clock_count =
        collect_provider_specs(address, UFS_PHY, b"clocks", &mut contract.phy_clocks);
    contract.phy_clock_names = property(address, UFS_PHY, b"clock-names");
    contract.qphy_resource_name = property(address, UFS_PHY, b"qcom,rpmh-resource-name");
    contract.phy_clock_names_hash =
        fdt::find_compatible_property_fnv1a(address, UFS_PHY, b"clock-names");
    contract.resets = property(address, UFS_CONTROLLER, b"resets");
    contract.reset_names = property(address, UFS_CONTROLLER, b"reset-names");
    contract.reset_names_hash =
        fdt::find_compatible_property_fnv1a(address, UFS_CONTROLLER, b"reset-names");
    contract.qphy_resource_name_hash =
        fdt::find_compatible_property_fnv1a(address, UFS_PHY, b"qcom,rpmh-resource-name");
    contract.core_reset = provider_spec(address, UFS_CONTROLLER, b"resets", 0);
    contract.lanes_per_direction =
        fdt::find_compatible_property_u32(address, UFS_CONTROLLER, b"lanes-per-direction", 0);
    contract.dev_ref_clk_freq =
        fdt::find_compatible_property_u32(address, UFS_CONTROLLER, b"dev-ref-clk-freq", 0);
    contract.power_domains = property(address, UFS_CONTROLLER, b"power-domains");
    contract.iommus = property(address, UFS_CONTROLLER, b"iommus");
    contract.vdd_hba_supply = property(address, UFS_CONTROLLER, b"vdd-hba-supply");
    contract.vcc_supply = property(address, UFS_CONTROLLER, b"vcc-supply");
    contract.vccq_supply = property(address, UFS_CONTROLLER, b"vccq-supply");
    contract.vccq2_supply = property(address, UFS_CONTROLLER, b"vccq2-supply");
    contract.vddp_ref_clk_supply = property(address, UFS_CONTROLLER, b"qcom,vddp-ref-clk-supply");
    contract.phy_vdda_supply = property(address, UFS_PHY, b"vdda-phy-supply");
    contract.phy_vdda_pll_supply = property(address, UFS_PHY, b"vdda-pll-supply");
    contract.vdd_hba_power =
        describe_supply(address, UFS_CONTROLLER, b"vdd-hba-supply", None, None, None);
    contract.vcc_power = describe_supply(
        address,
        UFS_CONTROLLER,
        b"vcc-supply",
        Some(b"vcc-voltage-level"),
        None,
        Some(b"vcc-max-microamp"),
    );
    contract.vccq2_power = describe_supply(
        address,
        UFS_CONTROLLER,
        b"vccq2-supply",
        None,
        None,
        Some(b"vccq2-max-microamp"),
    );
    contract.vddp_ref_clk_power = describe_supply(
        address,
        UFS_CONTROLLER,
        b"qcom,vddp-ref-clk-supply",
        None,
        None,
        Some(b"qcom,vddp-ref-clk-max-microamp"),
    );
    contract.phy_vdda_power = describe_supply(
        address,
        UFS_PHY,
        b"vdda-phy-supply",
        None,
        None,
        Some(b"vdda-phy-max-microamp"),
    );
    contract.phy_vdda_pll_power = describe_supply(
        address,
        UFS_PHY,
        b"vdda-pll-supply",
        None,
        None,
        Some(b"vdda-pll-max-microamp"),
    );
    Some(contract)
}

fn region_matches(region: Option<fdt::Region>, base: u64, size: u64) -> bool {
    match region {
        Some(region) => region.base == base && region.size == size,
        None => false,
    }
}

fn collect_regions(address: u64, compatible: &[u8], output: &mut [fdt::Region; 2]) -> u8 {
    let mut count = 0u8;
    for (index, slot) in output.iter_mut().enumerate() {
        if let Some(region) = fdt::find_compatible_nth(address, compatible, index) {
            *slot = region;
            count = count.saturating_add(1);
        }
    }
    count
}

fn provider_spec(
    address: u64,
    compatible: &[u8],
    property: &[u8],
    pair_index: usize,
) -> Option<ProviderSpec> {
    Some(ProviderSpec {
        provider: fdt::find_compatible_property_u32(
            address,
            compatible,
            property,
            pair_index.saturating_mul(2),
        )?,
        id: fdt::find_compatible_property_u32(
            address,
            compatible,
            property,
            pair_index.saturating_mul(2).saturating_add(1),
        )?,
    })
}

fn collect_provider_specs<const N: usize>(
    address: u64,
    compatible: &[u8],
    property: &[u8],
    output: &mut [ProviderSpec; N],
) -> u8 {
    let mut count = 0u8;
    for (index, slot) in output.iter_mut().enumerate() {
        let Some(spec) = provider_spec(address, compatible, property, index) else {
            break;
        };
        *slot = spec;
        count = count.saturating_add(1);
    }
    count
}

fn property(address: u64, compatible: &[u8], name: &[u8]) -> PropertyShape {
    fdt::find_compatible_node_property_observation(address, compatible, name, 0)
        .map(|observation| PropertyShape {
            present: observation.property_present,
            bytes: observation.property_length,
        })
        .unwrap_or_default()
}

fn describe_supply(
    address: u64,
    source_node: &[u8],
    property_name: &[u8],
    consumer_voltage_property: Option<&[u8]>,
    consumer_voltage_max_property: Option<&[u8]>,
    consumer_load_property: Option<&[u8]>,
) -> SupplyContract {
    let Some(phandle) = fdt::find_compatible_property_u32(address, source_node, property_name, 0)
    else {
        return EMPTY_SUPPLY_CONTRACT;
    };
    let regulator_name = fdt::find_phandle_property_string(
        address,
        source_node,
        property_name,
        0,
        b"regulator-name",
    );
    let resource_name =
        fdt::find_phandle_parent_property_string(address, phandle, b"qcom,resource-name");
    let consumer_min_uv = consumer_voltage_property.and_then(|property| {
        fdt::find_compatible_property_u32(address, UFS_CONTROLLER, property, 0)
    });
    let consumer_max_uv = consumer_voltage_property.and_then(|property| {
        consumer_voltage_max_property
            .and_then(|max_property| {
                fdt::find_compatible_property_u32(address, source_node, max_property, 0)
            })
            .or_else(|| fdt::find_compatible_property_u32(address, source_node, property, 1))
    });
    let consumer_max_load_ua = consumer_load_property
        .and_then(|property| fdt::find_compatible_property_u32(address, source_node, property, 0));
    let resource_id = regulator_name.and_then(ufs_rpmh_resource_id);
    SupplyContract {
        phandle: Some(phandle),
        regulator_name,
        resource_name,
        region: fdt::find_phandle_region(address, phandle),
        provider_min_uv: fdt::find_phandle_property_u32(
            address,
            phandle,
            b"regulator-min-microvolt",
            0,
        ),
        provider_max_uv: fdt::find_phandle_property_u32(
            address,
            phandle,
            b"regulator-max-microvolt",
            0,
        ),
        provider_init_uv: fdt::find_phandle_property_u32(address, phandle, b"qcom,init-voltage", 0),
        consumer_min_uv,
        consumer_max_uv,
        consumer_max_load_ua,
        qcom_set: fdt::find_phandle_property_u32(address, phandle, b"qcom,set", 0),
        resource_id,
    }
}

fn supply_string_is(value: Option<fdt::StringValue>, expected: &[u8]) -> bool {
    value.is_some_and(|value| {
        value.len == expected.len() && value.bytes[..value.len] == expected[..]
    })
}

fn ufs_rpmh_resource_id(name: fdt::StringValue) -> Option<[u8; 8]> {
    let bytes = &name.bytes[..name.len];
    match bytes {
        b"pm8150_l5" => Some(*b"ldoa5\0\0\0"),
        b"pm8150_l9" => Some(*b"ldoa9\0\0\0"),
        b"pm8150a_l7" => Some(*b"ldoc7\0\0\0"),
        b"pm8150_s4" => Some(*b"smpa4\0\0\0"),
        _ => None,
    }
}

/// Snapshot the DT-only stage at boot. The returned profile is `Copy`: no
/// heap, lock, or live hardware reference is introduced before the AArch64
/// driver has a proper ownership path.
pub(crate) fn init(dtb_address: Option<u64>) -> Option<PlatformContract> {
    let profile = dtb_address.and_then(describe);
    let stage = profile
        .filter(|profile| profile.platform_ready())
        .map(|_| BringupStage::Described)
        .unwrap_or(BringupStage::Absent);
    unsafe {
        PLATFORM = profile;
        STAGE = stage;
    }
    profile
}

pub(crate) fn profile() -> Option<PlatformContract> {
    unsafe { PLATFORM }
}

pub(crate) fn stage() -> BringupStage {
    unsafe { STAGE }
}

pub(crate) fn platform_ready() -> bool {
    profile().is_some_and(PlatformContract::platform_ready)
}
