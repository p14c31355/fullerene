//! SCSI command set for USB mass-storage devices.
//!
//! Provides the minimal command set needed to read/write sectors:
//! - TEST_UNIT_READY
//! - INQUIRY
//! - READ_CAPACITY_10
//! - READ_10
//! - WRITE_10

/// SCSI commands opcodes.
pub const SCSI_TEST_UNIT_READY: u8 = 0x00;
pub const SCSI_INQUIRY: u8 = 0x12;
pub const SCSI_READ_CAPACITY_10: u8 = 0x25;
pub const SCSI_READ_10: u8 = 0x28;
pub const SCSI_WRITE_10: u8 = 0x2A;

/// 6-byte SCSI command block wrapper (CDB).
///
/// For READ10 / WRITE10, the CDB is 10 bytes:
/// ```text
/// Byte 0: Opcode
/// Byte 1: LUN (high nibble) | flags (low nibble)
/// Byte 2-5: Logical Block Address (big-endian)
/// Byte 6: reserved
/// Byte 7-8: Transfer Length (big-endian)
/// Byte 9: control (0)
/// ```
#[repr(C, packed)]
pub struct ScsiCdb10 {
    pub opcode: u8,
    pub flags: u8,
    pub lba: [u8; 4],
    pub reserved: u8,
    pub length: [u8; 2],
    pub control: u8,
}

impl ScsiCdb10 {
    pub fn read10(lba: u32, blocks: u16) -> Self {
        Self {
            opcode: SCSI_READ_10,
            flags: 0,
            lba: lba.to_be_bytes(),
            reserved: 0,
            length: blocks.to_be_bytes(),
            control: 0,
        }
    }

    pub fn write10(lba: u32, blocks: u16) -> Self {
        Self {
            opcode: SCSI_WRITE_10,
            flags: 0,
            lba: lba.to_be_bytes(),
            reserved: 0,
            length: blocks.to_be_bytes(),
            control: 0,
        }
    }
}

/// INQUIRY CDB (6 bytes).
#[repr(C, packed)]
pub struct ScsiInquiryCdb {
    pub opcode: u8,
    pub flags: u8,
    pub page_code: u8,
    pub allocation_length: u16,
    pub control: u8,
}

impl ScsiInquiryCdb {
    pub fn new(alloc_len: u16) -> Self {
        Self {
            opcode: SCSI_INQUIRY,
            flags: 0,
            page_code: 0,
            allocation_length: alloc_len,
            control: 0,
        }
    }
}

/// READ CAPACITY (10) CDB.
#[repr(C, packed)]
pub struct ScsiReadCapacity10Cdb {
    pub opcode: u8,
    pub flags: u8,
    pub lba: [u8; 4],
    pub reserved: [u8; 3],
    pub partial_medium_indicator: u8,
    pub control: u8,
}

impl ScsiReadCapacity10Cdb {
    pub fn new() -> Self {
        Self {
            opcode: SCSI_READ_CAPACITY_10,
            flags: 0,
            lba: [0; 4],
            reserved: [0; 3],
            partial_medium_indicator: 0,
            control: 0,
        }
    }
}

/// READ CAPACITY (10) response data (8 bytes).
#[derive(Debug, Clone, Copy)]
pub struct ScsiReadCapacity10Data {
    pub last_lba: u32,
    pub block_length: u32,
}

impl ScsiReadCapacity10Data {
    pub fn from_bytes(data: &[u8; 8]) -> Self {
        Self {
            last_lba: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
            block_length: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        }
    }

    pub fn total_blocks(&self) -> u64 {
        self.last_lba as u64 + 1
    }

    pub fn total_size(&self) -> u64 {
        self.total_blocks() * self.block_length as u64
    }
}
