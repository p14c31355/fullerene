//! Realtek RTS5249 PCI Express Card Reader driver.
//!
//! Implements SD/MMC card access via the RTS5249 PCIe card reader.
//! Uses direct MMIO register access for SD commands and the PPBUF
//! (Ping-Pong Buffer at BAR0+0x400) for data transfer.
//!
//! # References
//! - Linux rtsx_pci driver (drivers/misc/cardreader/rtsx_pci.c)
//! - SD Physical Layer Simplified Specification Version 8.00

use core::ptr;
use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::pci::{PciDevice, PciScanner};

// ── RTSX Host Controller Registers (byte offsets from BAR0) ──

#[allow(dead_code)]
const RTSX_MSI_EN: u8 = 0x1C;     // [16-bit] MSI Enable
const RTSX_CFG: u8 = 0x20;        // [32-bit] Configuration

// ── SD Card Registers ─────────────────────────────────────────

const SD_CMD0: u8 = 0x40;         // [8-bit]  Cmd Index + Resp Type
const SD_CMD1: u8 = 0x41;         // [8-bit]  Arg byte 0 (LSB)
const SD_CMD2: u8 = 0x42;         // [8-bit]  Arg byte 1
const SD_CMD3: u8 = 0x43;         // [8-bit]  Arg byte 2
const SD_CMD4: u8 = 0x44;         // [8-bit]  Arg byte 3 (MSB)
const SD_CMD5: u8 = 0x45;         // [8-bit]  Resp byte 0
const SD_CMD6: u8 = 0x46;         // [8-bit]  Resp byte 1
const SD_CMD7: u8 = 0x47;         // [8-bit]  Resp byte 2
const SD_CMD8: u8 = 0x48;         // [8-bit]  Resp byte 3

const SD_BYTE_CNT_L: u8 = 0x4C;  // [8-bit]  Byte Count Low
const SD_BYTE_CNT_H: u8 = 0x4D;  // [8-bit]  Byte Count High
const SD_BLOCK_CNT_L: u8 = 0x4E; // [8-bit]  Block Count Low
const SD_BLOCK_CNT_H: u8 = 0x4F; // [8-bit]  Block Count High

const SD_STAT1: u8 = 0x50;        // [8-bit]  Status 1
const SD_STAT2: u8 = 0x51;        // [8-bit]  Status 2
const SD_BUS_STAT: u8 = 0x52;     // [8-bit]  Bus Status
const SD_PAD_CTL: u8 = 0x54;      // [8-bit]  Pad Control

const SD_SAMPLE_POINT_CTL: u8 = 0x58; // [16-bit] Sample Point
const SD_PUSH_POINT_CTL: u8 = 0x5A;   // [16-bit] Push Point

const SD_CMD_STATE: u8 = 0x5C;    // [8-bit]  Cmd State
const SD_TRANSFER: u8 = 0x5E;     // [8-bit]  Transfer

const SD_CFG1: u8 = 0x60;         // [8-bit]  Config 1
const SD_CFG2: u8 = 0x61;         // [8-bit]  Config 2
const SD_CFG3: u8 = 0x62;         // [8-bit]  Config 3

// ── Card Power / Clock Registers ──────────────────────────────

const CARD_PWR_CTL: u8 = 0x70;    // [8-bit]  Card Power Control
const CARD_CLK_EN: u8 = 0x72;     // [8-bit]  Card Clock Enable
const CARD_OE: u8 = 0x74;         // [8-bit]  Card Output Enable
const CARD_CLK_SOURCE: u8 = 0x76; // [8-bit]  Card Clock Source

const CARD_DRIVE_SEL: u8 = 0x80;  // [8-bit]  Card Drive Select
const CARD_STOP: u8 = 0x82;       // [8-bit]  Card Stop

// ── PPBUF base offset (data transfer window) ──────────────────

const PPBUF_BASE: usize = 0x400;

// ── SD_STAT1 bits ─────────────────────────────────────────────
const SD_TRANSFER_DONE: u8 = 0x04;
const SD_DATA_DONE: u8 = 0x08;

// ── SD_TRANSFER bits ──────────────────────────────────────────
const SD_TRANSFER_START: u8 = 0x80;
const SD_TRANSFER_WRITE: u8 = 0x01;

// ── SD_CMD0 response type bits ────────────────────────────────
const SD_RSP_TYPE_R1: u8 = 0x00;
const SD_RSP_TYPE_R1B: u8 = 0x40;
const SD_RSP_TYPE_R2: u8 = 0x20;
const SD_RSP_TYPE_R3: u8 = 0x10;
const SD_RSP_TYPE_R6: u8 = 0x02;
const SD_RSP_TYPE_R7: u8 = 0x01;

// ── SD_CFG1 bits ──────────────────────────────────────────────
const SD_CLK_DIVIDE_128: u8 = 0x0C;
const SD_BUS_WIDTH_1: u8 = 0x00;
const SD_CRC_CHECK_EN: u8 = 0x20;
const SD_CRC_GEN_EN: u8 = 0x40;

// ── SD_CFG2 bits ──────────────────────────────────────────────
const SD_CALC_CRC_CMD: u8 = 0x10;
const SD_CALC_CRC_DATA: u8 = 0x20;
const SD_RSP_TIMEOUT_5S: u8 = 0x0F;

// ── SD_CFG3 bits ──────────────────────────────────────────────
const SD_DATA_TIMEOUT_1S: u8 = 0x0E;

// ── CARD_PWR_CTL values ───────────────────────────────────────
const CARD_PWR_ON: u8 = 0x07;

// ── SD command indices ────────────────────────────────────────
const CMD0_GO_IDLE: u8 = 0;
const CMD2_ALL_SEND_CID: u8 = 2;
const CMD3_SEND_RELATIVE_ADDR: u8 = 3;
const CMD7_SELECT_CARD: u8 = 7;
const CMD8_SEND_IF_COND: u8 = 8;
const CMD9_SEND_CSD: u8 = 9;
const CMD16_SET_BLOCKLEN: u8 = 16;
const CMD17_READ_SINGLE: u8 = 17;
const CMD24_WRITE_SINGLE: u8 = 24;
const CMD55_APP_CMD: u8 = 55;
const ACMD41_SEND_OP_COND: u8 = 41;

// ── SD Card Type ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SdCardType {
    Unknown,
    SDSC,
    SDHC,
    SDXC,
}

// ── SD Card Info ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SdCardInfo {
    pub card_type: SdCardType,
    pub rca: u16,
    pub cid: [u8; 16],
    pub csd: [u8; 16],
    pub block_size: u32,
    pub total_blocks: u64,
}

// ── Controller ────────────────────────────────────────────────

pub struct RtsxController {
    device: PciDevice,
    mmio: *mut u8,
    sd_card: Option<SdCardInfo>,
}

unsafe impl Send for RtsxController {}
unsafe impl Sync for RtsxController {}

impl RtsxController {
    // ── Low-level register access ─────────────────────────────

    fn r8(&self, off: u8) -> u8 {
        unsafe { ptr::read_volatile(self.mmio.add(off as usize)) }
    }

    fn w8(&self, off: u8, val: u8) {
        unsafe { ptr::write_volatile(self.mmio.add(off as usize), val) }
    }

    fn r16(&self, off: u8) -> u16 {
        unsafe { ptr::read_volatile(self.mmio.add(off as usize) as *const u16) }
    }

    fn w16(&self, off: u8, val: u16) {
        unsafe { ptr::write_volatile(self.mmio.add(off as usize) as *mut u16, val) }
    }

    fn r32(&self, off: u8) -> u32 {
        unsafe { ptr::read_volatile(self.mmio.add(off as usize) as *const u32) }
    }

    fn w32(&self, off: u8, val: u32) {
        unsafe { ptr::write_volatile(self.mmio.add(off as usize) as *mut u32, val) }
    }

    // ── PPBUF data transfer ───────────────────────────────────

    /// Read from PPBUF (BAR0 + 0x400) — data from SD card after a read.
    fn ppbuf_read(&self, buf: &mut [u8]) {
        let base = PPBUF_BASE;
        for (i, chunk) in buf.chunks_mut(4).enumerate() {
            let val = unsafe { ptr::read_volatile(self.mmio.add(base + i * 4) as *const u32) };
            for (j, b) in chunk.iter_mut().enumerate() {
                if j < 4 {
                    *b = ((val >> (j * 8)) & 0xFF) as u8;
                }
            }
        }
    }

    /// Write to PPBUF — data to send to SD card before a write.
    fn ppbuf_write(&self, buf: &[u8]) {
        let base = PPBUF_BASE;
        for (i, chunk) in buf.chunks(4).enumerate() {
            let mut val: u32 = 0;
            for (j, &b) in chunk.iter().enumerate() {
                if j < 4 {
                    val |= (b as u32) << (j * 8);
                }
            }
            unsafe {
                ptr::write_volatile(self.mmio.add(base + i * 4) as *mut u32, val);
            }
        }
    }

    // ── RTSX Hardware Init ────────────────────────────────────

    fn init_hardware(&self) -> bool {
        // Soft reset
        self.w8(RTSX_CFG, self.r8(RTSX_CFG) | 0x01);
        for _ in 0..10_000 {
            if (self.r8(RTSX_CFG) & 0x01) == 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // Disable MSI (use INTx)
        self.w16(RTSX_MSI_EN, 0x0000);

        // Card power on
        self.w8(CARD_PWR_CTL, CARD_PWR_ON);
        for _ in 0..100_000 {
            core::hint::spin_loop();
        }

        // Enable card clock + output
        self.w8(CARD_CLK_EN, 0x01);
        self.w8(CARD_OE, 0x01);

        // Set clock source
        self.w8(CARD_CLK_SOURCE, 0x00);

        // Init SD registers
        self.w8(SD_CFG1, SD_CLK_DIVIDE_128 | SD_BUS_WIDTH_1);
        self.w8(SD_CFG2, SD_CALC_CRC_CMD | SD_CALC_CRC_DATA | SD_RSP_TIMEOUT_5S);
        self.w8(SD_CFG3, SD_DATA_TIMEOUT_1S);
        self.w8(SD_PAD_CTL, 0x48);
        self.w16(SD_SAMPLE_POINT_CTL, 0x0007);
        self.w16(SD_PUSH_POINT_CTL, 0x0008);
        self.w8(CARD_DRIVE_SEL, 0x03);
        self.w8(CARD_STOP, 0x00);

        log::info!("RTSX: hardware init complete");
        true
    }

    // ── SD Command Execution ──────────────────────────────────

    /// Send an SD command and wait for completion.  Returns the 32-bit response.
    fn sd_cmd(&self, cmd: u8, arg: u32, rsp_type: u8, data_len: u16) -> Result<u32, &'static str> {
        // Wait for cmd state machine idle
        for _ in 0..10_000 {
            if (self.r8(SD_CMD_STATE) & 0x01) != 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // Write command + argument
        self.w8(SD_CMD0, cmd | rsp_type);
        self.w8(SD_CMD1, arg as u8);
        self.w8(SD_CMD2, (arg >> 8) as u8);
        self.w8(SD_CMD3, (arg >> 16) as u8);
        self.w8(SD_CMD4, (arg >> 24) as u8);

        // Configure CFG1 with CRC and bus width
        self.w8(SD_CFG1, SD_CLK_DIVIDE_128 | SD_BUS_WIDTH_1 | SD_CRC_CHECK_EN | SD_CRC_GEN_EN);

        // Set byte/block count for data commands
        if data_len > 0 {
            self.w8(SD_BYTE_CNT_L, (data_len & 0xFF) as u8);
            self.w8(SD_BYTE_CNT_H, (data_len >> 8) as u8);
            self.w8(SD_BLOCK_CNT_L, 0x01);
            self.w8(SD_BLOCK_CNT_H, 0x00);
        } else {
            self.w8(SD_BYTE_CNT_L, 0x00);
            self.w8(SD_BYTE_CNT_H, 0x00);
            self.w8(SD_BLOCK_CNT_L, 0x00);
            self.w8(SD_BLOCK_CNT_H, 0x00);
        }

        // Trigger transfer
        self.w8(SD_TRANSFER, SD_TRANSFER_START);

        // Wait for completion
        for _ in 0..500_000 {
            if (self.r8(SD_STAT1) & SD_TRANSFER_DONE) != 0 {
                break;
            }
            core::hint::spin_loop();
        }

        if (self.r8(SD_STAT1) & SD_TRANSFER_DONE) == 0 {
            return Err("SD cmd timeout");
        }

        // Check for errors
        if (self.r8(SD_STAT2) & 0x0F) != 0 {
            return Err("SD cmd error");
        }

        // Read response
        let rsp = (self.r8(SD_CMD5) as u32)
            | ((self.r8(SD_CMD6) as u32) << 8)
            | ((self.r8(SD_CMD7) as u32) << 16)
            | ((self.r8(SD_CMD8) as u32) << 24);

        Ok(rsp)
    }

    /// Send an ACMD (CMD55 + ACMD).
    fn sd_acmd(&self, acmd: u8, arg: u32, rsp_type: u8) -> Result<u32, &'static str> {
        let r1 = self.sd_cmd(CMD55_APP_CMD, 0, SD_RSP_TYPE_R1, 0)?;
        if (r1 & (1 << 5)) == 0 {
            return Err("APP_CMD not accepted");
        }
        self.sd_cmd(acmd, arg, rsp_type, 0)
    }

    /// Init SD card: detect, power up, get parameters.
    pub fn init_sd_card(&mut self) -> Result<(), &'static str> {
        // Check card detect
        let bus = self.r8(SD_BUS_STAT);
        if (bus & 0x01) == 0 {
            return Err("no card");
        }
        log::info!("RTSX: card detect OK");

        for _ in 0..100_000 {
            core::hint::spin_loop();
        }

        // CMD0: reset
        log::info!("RTSX: CMD0");
        self.sd_cmd(CMD0_GO_IDLE, 0, 0, 0)?;
        for _ in 0..1_000 {
            core::hint::spin_loop();
        }

        // CMD8: check SDHC/SDXC
        log::info!("RTSX: CMD8");
        let sdhc = match self.sd_cmd(CMD8_SEND_IF_COND, 0x1AA, SD_RSP_TYPE_R7, 0) {
            Ok(rsp) => {
                let check = (rsp >> 8) as u8;
                let voltage = rsp as u8;
                if voltage == 0x01 && check == 0xAA {
                    log::info!("RTSX: SDHC/SDXC card");
                    true
                } else {
                    log::info!("RTSX: SDSC (CMD8 mismatch)");
                    false
                }
            }
            Err(_) => {
                log::info!("RTSX: CMD8 unsupported — SDSC or MMC");
                false
            }
        };

        // ACMD41: negotiate voltage+capacity
        let mut ocr_arg = 0x00FF_8000u32; // 2.7-3.6V
        if sdhc {
            ocr_arg |= 1 << 30; // HCS
        }
        log::info!("RTSX: ACMD41");
        let mut ocr = 0u32;
        let mut ok = false;
        for _ in 0..1000 {
            if let Ok(rsp) = self.sd_acmd(ACMD41_SEND_OP_COND, ocr_arg, SD_RSP_TYPE_R3) {
                if (rsp & (1 << 31)) != 0 {
                    ocr = rsp;
                    ok = true;
                    break;
                }
            }
            for _ in 0..10_000 {
                core::hint::spin_loop();
            }
        }
        if !ok {
            return Err("ACMD41 timeout");
        }
        log::info!("RTSX: ACMD41 OK, OCR={:#010x}", ocr);

        let card_type = if (ocr & (1 << 30)) != 0 {
            SdCardType::SDHC
        } else {
            SdCardType::SDSC
        };
        // SDXC also sets bit 30; distinguish by OCR
        let card_type = if card_type == SdCardType::SDHC && (ocr & (1 << 28)) != 0 {
            SdCardType::SDXC
        } else {
            card_type
        };

        // CMD2: get CID
        log::info!("RTSX: CMD2");
        let _ = self.sd_cmd(CMD2_ALL_SEND_CID, 0, SD_RSP_TYPE_R2, 0)?;
        let mut cid = [0u8; 16];
        for i in 0..8 {
            cid[i] = self.r8(SD_CMD5 + i as u8);
        }

        // CMD3: get RCA
        log::info!("RTSX: CMD3");
        let r6 = self.sd_cmd(CMD3_SEND_RELATIVE_ADDR, 0, SD_RSP_TYPE_R6, 0)?;
        let rca = ((r6 >> 16) & 0xFFFF) as u16;
        if rca == 0 {
            return Err("RCA=0");
        }
        log::info!("RTSX: RCA={:#06x}", rca);

        // CMD9: get CSD
        log::info!("RTSX: CMD9");
        let _ = self.sd_cmd(CMD9_SEND_CSD, (rca as u32) << 16, SD_RSP_TYPE_R2, 0)?;
        let mut csd = [0u8; 16];
        for i in 0..8 {
            csd[i] = self.r8(SD_CMD5 + i as u8);
        }

        // Parse CSD
        let (block_size, total_blocks) = self.parse_csd(&csd, card_type);

        // CMD7: select card
        log::info!("RTSX: CMD7");
        self.sd_cmd(CMD7_SELECT_CARD, (rca as u32) << 16, SD_RSP_TYPE_R1B, 0)?;

        // Set block size to 512 for SDSC
        if card_type == SdCardType::SDSC {
            let _ = self.sd_cmd(CMD16_SET_BLOCKLEN, 512, SD_RSP_TYPE_R1, 0);
        }

        let bs = if card_type == SdCardType::SDSC { 512 } else { block_size };
        // For SDHC/SDXC, total_blocks is in 512-byte units already
        let tb = if card_type == SdCardType::SDSC {
            total_blocks * (block_size as u64) / 512
        } else {
            total_blocks
        };

        self.sd_card = Some(SdCardInfo {
            card_type,
            rca,
            cid,
            csd,
            block_size: bs,
            total_blocks: tb,
        });

        log::info!("RTSX: SD card ready — type={:?} blocks={} size={}",
            card_type, tb, bs);
        Ok(())
    }

    fn parse_csd(&self, csd: &[u8; 16], card_type: SdCardType) -> (u32, u64) {
        let csd_ver = (csd[14] >> 6) & 0x3;
        log::info!("RTSX: CSD ver={}", csd_ver);

        match card_type {
            SdCardType::SDHC | SdCardType::SDXC => {
                let c_size = ((csd[7] & 0x3F) as u32) << 16
                    | (csd[8] as u32) << 8
                    | csd[9] as u32;
                let blocks = (c_size as u64 + 1) * 1024;
                (512, blocks)
            }
            _ => {
                let read_bl_len = csd[5] & 0x0F;
                let bs = 1u32 << read_bl_len;
                let c_size = ((csd[6] & 0x03) as u32) << 10
                    | (csd[7] as u32) << 2
                    | ((csd[8] >> 6) & 0x03) as u32;
            let c_size_mult = (((csd[9] >> 7) & 0x01) << 2)
                    | (((csd[10] >> 6) & 0x03));
            let mult = 1u32 << (c_size_mult as u32 + 2);
                let blocks = ((c_size as u64 + 1) * mult as u64) * (bs as u64) / 512;
                (bs, blocks)
            }
        }
    }

    // ── Sector I/O ────────────────────────────────────────────

    /// Read one 512-byte sector at LBA.
    fn read_sector(&self, lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let card = self.sd_card.as_ref().ok_or("no card")?;
        let addr = match card.card_type {
            SdCardType::SDSC => lba * 512,
            _ => lba,
        };

        // Configure data length
        self.w8(SD_BYTE_CNT_L, 0x00);
        self.w8(SD_BYTE_CNT_H, 0x02);
        self.w8(SD_BLOCK_CNT_L, 0x01);
        self.w8(SD_BLOCK_CNT_H, 0x00);

        self.sd_cmd(CMD17_READ_SINGLE, addr, SD_RSP_TYPE_R1, 512)?;

        for _ in 0..500_000 {
            if (self.r8(SD_STAT1) & SD_DATA_DONE) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if (self.r8(SD_STAT1) & SD_DATA_DONE) == 0 {
            return Err("read data timeout");
        }

        self.ppbuf_read(buf);
        Ok(())
    }

    /// Write one 512-byte sector at LBA.
    fn write_sector(&self, lba: u32, buf: &[u8]) -> Result<(), &'static str> {
        let card = self.sd_card.as_ref().ok_or("no card")?;
        let addr = match card.card_type {
            SdCardType::SDSC => lba * 512,
            _ => lba,
        };

        // Write data to PPBUF first (for write data commands)
        self.ppbuf_write(buf);

        // Configure data length
        self.w8(SD_BYTE_CNT_L, 0x00);
        self.w8(SD_BYTE_CNT_H, 0x02);
        self.w8(SD_BLOCK_CNT_L, 0x01);
        self.w8(SD_BLOCK_CNT_H, 0x00);

        // Send CMD24 with write direction
        self.w8(SD_CMD0, CMD24_WRITE_SINGLE | SD_RSP_TYPE_R1);
        self.w8(SD_CMD1, addr as u8);
        self.w8(SD_CMD2, (addr >> 8) as u8);
        self.w8(SD_CMD3, (addr >> 16) as u8);
        self.w8(SD_CMD4, (addr >> 24) as u8);

        self.w8(SD_CFG1, SD_CLK_DIVIDE_128 | SD_BUS_WIDTH_1 | SD_CRC_CHECK_EN | SD_CRC_GEN_EN);
        self.w8(SD_TRANSFER, SD_TRANSFER_START | SD_TRANSFER_WRITE);

        for _ in 0..500_000 {
            if (self.r8(SD_STAT1) & SD_DATA_DONE) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if (self.r8(SD_STAT1) & SD_DATA_DONE) == 0 {
            return Err("write data timeout");
        }

        Ok(())
    }

    // ── Public API ────────────────────────────────────────────

    pub fn sd_card_info(&self) -> Option<SdCardInfo> {
        self.sd_card.clone()
    }

    pub fn read_sectors(&self, lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
        for i in 0..count as u32 {
            let off = (i * 512) as usize;
            self.read_sector(lba + i, &mut buf[off..off + 512])?;
        }
        Ok(())
    }

    pub fn write_sectors(&self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        for i in 0..count as u32 {
            let off = (i * 512) as usize;
            self.write_sector(lba + i, &buf[off..off + 512])?;
        }
        Ok(())
    }

    pub fn probe(ctx: &dyn DriverContext, device: PciDevice) -> Option<Self> {
        let bar0 = device.get_bar_info(0)?;
        if bar0.is_io {
            return None;
        }
        let mmio = ctx.phys_to_virt(bar0.address) as *mut u8;
        ctx.map_mmio_region(bar0.address as usize, mmio as usize, bar0.size as usize).ok()?;

        let ctrl = Self { device, mmio, sd_card: None };
        if !ctrl.init_hardware() {
            return None;
        }

        for _ in 0..500_000 {
            core::hint::spin_loop();
        }

        Some(ctrl)
    }
}

// ── Static controller for kernel access ──────────────────────

static CONTROLLER: Mutex<Option<RtsxController>> = Mutex::new(None);

pub fn init(ctx: &dyn DriverContext) {
    let mut scanner = PciScanner::new();
    if scanner.scan_all_buses().is_err() {
        log::info!("RTSX: PCI scan failed");
        return;
    }

    for dev in scanner.get_devices() {
        if dev.vendor_id == 0x10EC
            && (dev.device_id == 0x5249 || dev.device_id == 0x5250 || dev.device_id == 0x5260)
        {
            log::info!("RTSX: found RTS5249 at {:02x}:{:02x}.{}",
                dev.bus, dev.device, dev.function);
            dev.enable_memory_access();
            if let Some(ctrl) = RtsxController::probe(ctx, dev.clone()) {
                *CONTROLLER.lock() = Some(ctrl);
                log::info!("RTSX: controller initialised");
            }
            return;
        }
    }
    log::info!("RTSX: no card reader found");
}

pub fn init_sd_card() -> Result<(), &'static str> {
    let mut guard = CONTROLLER.lock();
    if let Some(ref mut ctrl) = *guard {
        ctrl.init_sd_card()
    } else {
        Err("no RTSX controller")
    }
}

pub fn sd_card_info() -> Option<SdCardInfo> {
    let guard = CONTROLLER.lock();
    guard.as_ref().and_then(|c| c.sd_card_info())
}

pub fn read_sectors(lba: u32, count: u16, buf: &mut [u8]) -> Result<(), &'static str> {
    let guard = CONTROLLER.lock();
    guard.as_ref().ok_or("no controller")?.read_sectors(lba, count, buf)
}

pub fn write_sectors(lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
    let guard = CONTROLLER.lock();
    guard.as_ref().ok_or("no controller")?.write_sectors(lba, count, buf)
}

pub fn is_present() -> bool {
    CONTROLLER.lock().is_some()
}
