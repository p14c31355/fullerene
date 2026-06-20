//! USB Mass-Storage Driver — Bulk-Only Transport (BOT) + SCSI.
//!
//! Implements the USB mass-storage class specification for bulk-only
//! transport. After a mass-storage device is enumerated, this driver
//! handles:
//!
//! - Command Block Wrapper (CBW) submission
//! - Data transfer (IN / OUT)
//! - Command Status Wrapper (CSW) retrieval
//! - SCSI command dispatch (read/write sectors)
//!
//! # Protocol
//!
//! ```text
//! Host → Device:  CBW (31 bytes, bulk OUT endpoint)
//! Host ↔ Device:  Data (bulk IN/OUT, optional)
//! Device → Host:  CSW (13 bytes, bulk IN endpoint)
//! ```

use crate::usb::{UsbDevice, UsbDirection, UsbXferType};
use crate::usb::scsi::{
    ScsiCdb10, ScsiReadCapacity10Cdb, ScsiReadCapacity10Data,
};

// ── CBW (Command Block Wrapper) — 31 bytes ────────────────────
#[repr(C, packed)]
#[allow(non_snake_case)]
pub struct Cbw {
    pub dCBWSignature: u32,     // 0x43425355 ("USBC")
    pub dCBWTag: u32,           // command tag
    pub dCBWDataTransferLength: u32, // bytes to transfer
    pub bmCBWFlags: u8,         // 0x80 = IN, 0x00 = OUT
    pub bCBWLUN: u8,            // LUN (usually 0)
    pub bCBWCBLength: u8,       // CB length (6-16)
    pub CBWCB: [u8; 16],        // command block
}

impl Cbw {
    pub const SIGNATURE: u32 = 0x43425355;

    pub fn new(tag: u32, data_len: u32, dir_in: bool, cdb: &[u8]) -> Self {
        let mut cb = [0u8; 16];
        let len = cdb.len().min(16);
        cb[..len].copy_from_slice(&cdb[..len]);
        Self {
            dCBWSignature: Self::SIGNATURE,
            dCBWTag: tag,
            dCBWDataTransferLength: data_len,
            bmCBWFlags: if dir_in { 0x80 } else { 0x00 },
            bCBWLUN: 0,
            bCBWCBLength: cdb.len().min(16) as u8,
            CBWCB: cb,
        }
    }
}

// ── CSW (Command Status Wrapper) — 13 bytes ───────────────────
#[repr(C, packed)]
#[allow(non_snake_case)]
pub struct Csw {
    pub dCSWSignature: u32,     // 0x53425355 ("USBS")
    pub dCSWTag: u32,
    pub dCSWDataResidue: u32,
    pub bCSWStatus: u8,         // 0 = success, 1 = failure, 2 = phase error
}

impl Csw {
    pub const SIGNATURE: u32 = 0x53425355;
    pub const STATUS_SUCCESS: u8 = 0;
    pub const STATUS_FAILED: u8 = 1;
    pub const STATUS_PHASE_ERROR: u8 = 2;
}

// ── Bulk callback type ────────────────────────────────────────
// The caller provides a function that does a bulk transfer.
pub type BulkXferFn = dyn FnMut(u8, u8, &mut [u8], UsbDirection, u16) -> Result<usize, &'static str>;

// ── Mass Storage Device ───────────────────────────────────────

/// USB Mass Storage class driver using Bulk-Only Transport (BOT).
///
/// Wraps a [`UsbDevice`] with bulk IN/OUT endpoints and provides
/// SCSI command dispatch via CBW/CSW protocol.
pub struct UsbMassStorage {
    pub device: UsbDevice,
    pub bulk_out_ep: u8,
    pub bulk_in_ep: u8,
    pub max_packet_out: u16,
    pub max_packet_in: u16,
    pub tag: u32,
    pub block_size: u32,
    pub total_blocks: u64,
}

impl UsbMassStorage {
    /// Create a mass-storage driver for a USB device.
    ///
    /// Scans the device's endpoints for bulk IN/OUT pairs.
    pub fn new(device: UsbDevice) -> Option<Self> {
        let mut bulk_out = None;
        let mut bulk_in = None;
        let mut mps_out = 0u16;
        let mut mps_in = 0u16;

        for ep in &device.endpoints {
            if ep.xfer_type() != UsbXferType::Bulk {
                continue;
            }
            match ep.direction() {
                UsbDirection::Out => {
                    bulk_out = Some(ep.b_endpoint_address);
                    mps_out = ep.w_max_packet_size;
                }
                UsbDirection::In => {
                    bulk_in = Some(ep.b_endpoint_address);
                    mps_in = ep.w_max_packet_size;
                }
            }
        }

        Some(Self {
            bulk_out_ep: bulk_out?,
            bulk_in_ep: bulk_in?,
            max_packet_out: mps_out,
            max_packet_in: mps_in,
            device,
            tag: 1,
            block_size: 512,
            total_blocks: 0,
        })
    }

    /// Perform a complete BOT command: CBW → Data → CSW.
    ///
    /// `xfer` is a closure that performs bulk transfers.
    /// Returns `Ok(())` on CSW success, or `Err`.
    pub fn exec_command(
        &mut self,
        xfer: &mut BulkXferFn,
        cdb: &[u8],
        data: Option<&mut [u8]>,
        dir_in: bool,
    ) -> Result<(), &'static str> {
        let data_len = data.as_ref().map(|d| d.len() as u32).unwrap_or(0);
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1);

        // Step 1: Send CBW on bulk OUT
        let cbw = Cbw::new(tag, data_len, dir_in, cdb);
        // SAFETY: Cbw is #[repr(C, packed)] with no padding, so reinterpreting
        // its bytes as &[u8] is valid. size_of::<Cbw>() == 31 per the USB BOT spec.
        let cbw_bytes = unsafe {
            core::slice::from_raw_parts(
                &cbw as *const Cbw as *const u8,
                core::mem::size_of::<Cbw>(),
            )
        };
        let mut cbw_buf = alloc::vec![0u8; core::mem::size_of::<Cbw>()];
        cbw_buf.copy_from_slice(cbw_bytes);
        xfer(
            self.device.address,
            self.bulk_out_ep,
            &mut cbw_buf,
            UsbDirection::Out,
            self.max_packet_out,
        )?;

        // Step 2: Data phase (if any)
        if let Some(buf) = data {
            let dir = if dir_in { UsbDirection::In } else { UsbDirection::Out };
            let ep = if dir_in { self.bulk_in_ep } else { self.bulk_out_ep };
            let mps = if dir_in { self.max_packet_in } else { self.max_packet_out };
            xfer(self.device.address, ep, buf, dir, mps)?;
        }

        // Step 3: Receive CSW on bulk IN
        let mut csw_buf = [0u8; 13];
        xfer(
            self.device.address,
            self.bulk_in_ep,
            &mut csw_buf,
            UsbDirection::In,
            self.max_packet_in,
        )?;

        // SAFETY: Csw is #[repr(C, packed)] and csw_buf was filled by a bulk-IN
        // transfer of exactly 13 bytes (CSW size per BOT spec).
        let csw: &Csw = unsafe { &*(csw_buf.as_ptr() as *const Csw) };
        if csw.dCSWSignature != Csw::SIGNATURE {
            return Err("bad CSW signature");
        }
        if csw.bCSWStatus != Csw::STATUS_SUCCESS {
            return Err("CSW reported error");
        }
        Ok(())
    }

    /// Read block size and total blocks via READ_CAPACITY_10.
    pub fn read_capacity(&mut self, xfer: &mut BulkXferFn) -> Result<(), &'static str> {
        let cdb = ScsiReadCapacity10Cdb::new();
        // SAFETY: ScsiReadCapacity10Cdb is #[repr(C, packed)]. The CDB is exactly
        // 10 bytes per the SCSI READ_CAPACITY_10 spec.
        let cdb_bytes = unsafe {
            core::slice::from_raw_parts(
                &cdb as *const ScsiReadCapacity10Cdb as *const u8,
                10,
            )
        };
        let mut data = [0u8; 8];
        self.exec_command(xfer, cdb_bytes, Some(&mut data), true)?;

        let cap = ScsiReadCapacity10Data::from_bytes(&data);
        self.block_size = cap.block_length;
        self.total_blocks = cap.total_blocks();
        Ok(())
    }

    /// Read one or more sectors starting at LBA.
    pub fn read_sectors(
        &mut self,
        xfer: &mut BulkXferFn,
        lba: u32,
        blocks: u16,
        buf: &mut [u8],
    ) -> Result<(), &'static str> {
        let expected = (blocks as u32) * self.block_size;
        if buf.len() < expected as usize {
            return Err("buffer too small");
        }
        let cdb = ScsiCdb10::read10(lba, blocks);
        // SAFETY: ScsiCdb10 is #[repr(C, packed)], exactly 10 bytes per SCSI READ_10 spec.
        let cdb_bytes = unsafe {
            core::slice::from_raw_parts(
                &cdb as *const ScsiCdb10 as *const u8,
                10,
            )
        };
        self.exec_command(xfer, cdb_bytes, Some(buf), true)
    }

    /// Write one or more sectors starting at LBA.
    pub fn write_sectors(
        &mut self,
        xfer: &mut BulkXferFn,
        lba: u32,
        blocks: u16,
        buf: &[u8],
    ) -> Result<(), &'static str> {
        let expected = (blocks as u32) * self.block_size;
        if buf.len() < expected as usize {
            return Err("buffer too small");
        }
        let cdb = ScsiCdb10::write10(lba, blocks);
        // SAFETY: ScsiCdb10 is #[repr(C, packed)], exactly 10 bytes per SCSI WRITE_10 spec.
        let cdb_bytes = unsafe {
            core::slice::from_raw_parts(
                &cdb as *const ScsiCdb10 as *const u8,
                10,
            )
        };
        let mut buf_mut = alloc::vec![0u8; buf.len()];
        buf_mut.copy_from_slice(buf);
        self.exec_command(xfer, cdb_bytes, Some(&mut buf_mut), false)
    }
}
