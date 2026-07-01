//! Realtek RTS5249 PCI Express Card Reader driver.
//!
//! # Design
//!
//! - `probe()` only touches PCI config space (port I/O) — never MMIO.
//!   This guarantees the boot path cannot hang even if the device is
//!   in an unresponsive state.
//! - All MMIO access is deferred to `init_sd_card()`, which the
//!   kernel calls after boot is complete enough to tolerate a failure.
//!
//! # References
//! - Linux rtsx_pci driver (drivers/misc/cardreader/rtsx_pci.c)
//! - SD Physical Layer Simplified Specification Version 8.00

use core::ptr;
use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::pci::{PciConfigSpace, PciDevice, PciScanner};

// ── RTSX Host Controller Registers (byte offsets from BAR0) ──
#[allow(dead_code)]
const RTSX_MSI_EN: u8 = 0x1C;
const RTSX_CFG: u8 = 0x20;

// ── SD Card Registers ─────────────────────────────────────────
const SD_CMD0: u8 = 0x40;
const SD_CMD1: u8 = 0x41;
const SD_CMD2: u8 = 0x42;
const SD_CMD3: u8 = 0x43;
const SD_CMD4: u8 = 0x44;
const SD_CMD5: u8 = 0x45;
const SD_CMD6: u8 = 0x46;
const SD_CMD7: u8 = 0x47;
const SD_CMD8: u8 = 0x48;
const SD_BYTE_CNT_L: u8 = 0x4C;
const SD_BYTE_CNT_H: u8 = 0x4D;
const SD_BLOCK_CNT_L: u8 = 0x4E;
const SD_BLOCK_CNT_H: u8 = 0x4F;
const SD_STAT1: u8 = 0x50;
const SD_STAT2: u8 = 0x51;
const SD_BUS_STAT: u8 = 0x52;
const SD_PAD_CTL: u8 = 0x54;
const SD_SAMPLE_POINT_CTL: u8 = 0x58;
const SD_PUSH_POINT_CTL: u8 = 0x5A;
const SD_CMD_STATE: u8 = 0x5C;
const SD_TRANSFER: u8 = 0x5E;
const SD_CFG1: u8 = 0x60;
const SD_CFG2: u8 = 0x61;
const SD_CFG3: u8 = 0x62;

// ── Card Power / Clock Registers ──────────────────────────────
const CARD_PWR_CTL: u8 = 0x70;
const CARD_CLK_EN: u8 = 0x72;
const CARD_OE: u8 = 0x74;
const CARD_CLK_SOURCE: u8 = 0x76;
const CARD_DRIVE_SEL: u8 = 0x80;
const CARD_STOP: u8 = 0x82;

// ── PPBUF base offset ─────────────────────────────────────────
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

// ── SD_CFG constants ──────────────────────────────────────────
const SD_CLK_DIVIDE_128: u8 = 0x0C;
const SD_BUS_WIDTH_1: u8 = 0x00;
const SD_CRC_CHECK_EN: u8 = 0x20;
const SD_CRC_GEN_EN: u8 = 0x40;
const SD_CALC_CRC_CMD: u8 = 0x10;
const SD_CALC_CRC_DATA: u8 = 0x20;
const SD_RSP_TIMEOUT_5S: u8 = 0x0F;
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
    #[allow(dead_code)]
    device: PciDevice,
    bar0_phys: u64,
    bar0_size: u32,
    mmio: *mut u8,
    mmio_mapped: bool,
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

    fn w16(&self, off: u8, val: u16) {
        unsafe { ptr::write_volatile(self.mmio.add(off as usize) as *mut u16, val) }
    }

    // ── PPBUF data transfer ───────────────────────────────────

    fn ppbuf_read(&self, buf: &mut [u8]) {
        assert!(buf.len() <= 512, "RTSX: ppbuf read size exceeds 512 bytes");
        for (i, chunk) in buf.chunks_mut(4).enumerate() {
            let val = unsafe { ptr::read_volatile(self.mmio.add(PPBUF_BASE + i * 4) as *const u32) };
            for (j, b) in chunk.iter_mut().enumerate().take(4) {
                *b = ((val >> (j * 8)) & 0xFF) as u8;
            }
        }
    }

    fn ppbuf_write(&self, buf: &[u8]) {
        assert!(buf.len() <= 512, "RTSX: ppbuf write size exceeds 512 bytes");
        for (i, chunk) in buf.chunks(4).enumerate() {
            let mut val: u32 = 0;
            for (j, &b) in chunk.iter().enumerate().take(4) {
                val |= (b as u32) << (j * 8);
            }
            unsafe {
                ptr::write_volatile(self.mmio.add(PPBUF_BASE + i * 4) as *mut u32, val);
            }
        }
    }

    // ── RTSX Hardware Init (MMIO access starts here) ──────────

    fn init_hardware(&self) -> bool {
        // Emergency serial debug before first MMIO
        let mut serial = crate::port::PortWriter::new(crate::port::HardwarePorts::SERIAL_DATA_PORT);
        serial.write_safe(b'R' as u32);
        serial.write_safe(b'T' as u32);
        serial.write_safe(b'S' as u32);
        serial.write_safe(b'X' as u32);
        serial.write_safe(b'\n' as u32);

        log::info!("RTSX: first MMIO read at {:p}+0x00", self.mmio);
        let test0 = self.r8(0x00);
        log::info!("RTSX: BAR0[0x00] = {:#04x}", test0);

        let cfg = self.r8(RTSX_CFG);
        if cfg == 0xFF {
            log::warn!("RTSX: device not responding (CFG=0xFF)");
            return false;
        }
        log::info!("RTSX: CFG={:#04x}", cfg);

        self.w8(RTSX_CFG, cfg | 0x01);
        for _ in 0..100_000 {
            if (self.r8(RTSX_CFG) & 0x01) == 0 {
                break;
            }
            core::hint::spin_loop();
        }

        self.w16(RTSX_MSI_EN, 0x0000);
        self.w8(CARD_PWR_CTL, CARD_PWR_ON);

        for _ in 0..200_000 {
            core::hint::spin_loop();
        }

        self.w8(CARD_CLK_EN, 0x01);
        self.w8(CARD_OE, 0x01);
        self.w8(CARD_CLK_SOURCE, 0x00);

        self.w8(SD_CFG1, SD_CLK_DIVIDE_128 | SD_BUS_WIDTH_1);
        self.w8(SD_CFG2, SD_CALC_CRC_CMD | SD_CALC_CRC_DATA | SD_RSP_TIMEOUT_5S);
        self.w8(SD_CFG3, SD_DATA_TIMEOUT_1S);
        self.w8(SD_PAD_CTL, 0x48);
        self.w16(SD_SAMPLE_POINT_CTL, 0x0007);
        self.w16(SD_PUSH_POINT_CTL, 0x0008);
        self.w8(CARD_DRIVE_SEL, 0x03);
        self.w8(CARD_STOP, 0x00);

        log::info!("RTSX: hardware init done");
        true
    }

    // ── SD Command Execution ──────────────────────────────────

    fn sd_cmd(&self, cmd: u8, arg: u32, rsp_type: u8, data_len: u16) -> Result<u32, &'static str> {
        let mut ready = false;
        for _ in 0..50_000 {
            if (self.r8(SD_CMD_STATE) & 0x01) != 0 {
                ready = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !ready {
            return Err("SD cmd busy timeout");
        }

        self.w8(SD_CMD0, cmd | rsp_type);
        self.w8(SD_CMD1, arg as u8);
        self.w8(SD_CMD2, (arg >> 8) as u8);
        self.w8(SD_CMD3, (arg >> 16) as u8);
        self.w8(SD_CMD4, (arg >> 24) as u8);
        self.w8(SD_CFG1, SD_CLK_DIVIDE_128 | SD_BUS_WIDTH_1 | SD_CRC_CHECK_EN | SD_CRC_GEN_EN);

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

        self.w8(SD_TRANSFER, SD_TRANSFER_START);

        for _ in 0..500_000 {
            if (self.r8(SD_STAT1) & SD_TRANSFER_DONE) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if (self.r8(SD_STAT1) & SD_TRANSFER_DONE) == 0 {
            return Err("SD cmd timeout");
        }
        if (self.r8(SD_STAT2) & 0x0F) != 0 {
            return Err("SD cmd error");
        }

        let rsp = (self.r8(SD_CMD5) as u32)
            | ((self.r8(SD_CMD6) as u32) << 8)
            | ((self.r8(SD_CMD7) as u32) << 16)
            | ((self.r8(SD_CMD8) as u32) << 24);

        Ok(rsp)
    }

    fn sd_acmd(&self, acmd: u8, arg: u32, rsp_type: u8) -> Result<u32, &'static str> {
        let r1 = self.sd_cmd(CMD55_APP_CMD, 0, rsp_type, 0)?;
        if (r1 & (1 << 5)) == 0 {
            return Err("APP_CMD not accepted");
        }
        self.sd_cmd(acmd, arg, rsp_type, 0)
    }

    // ── SD Card Init (called after boot) ───────────────────────

    pub fn init_sd_card(&mut self) -> Result<(), &'static str> {
        if !self.mmio_mapped {
            return Err("MMIO not mapped");
        }

        // Long delay for PCIe link and card power stabilization
        for _ in 0..2_000_000 {
            core::hint::spin_loop();
        }

        if !self.init_hardware() {
            return Err("hardware init failed");
        }

        for _ in 0..200_000 {
            core::hint::spin_loop();
        }

        let bus = self.r8(SD_BUS_STAT);
        if (bus & 0x01) == 0 {
            return Err("no card");
        }
        log::info!("RTSX: card detect OK (bus_stat={:#04x})", bus);

        for _ in 0..200_000 {
            core::hint::spin_loop();
        }

        log::info!("RTSX: CMD0");
        self.sd_cmd(CMD0_GO_IDLE, 0, 0, 0)?;
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }

        log::info!("RTSX: CMD8");
        let sdhc = match self.sd_cmd(CMD8_SEND_IF_COND, 0x1AA, SD_RSP_TYPE_R7, 0) {
            Ok(rsp) => (rsp as u8 == 0x01 && (rsp >> 8) as u8 == 0xAA),
            Err(_) => false,
        };
        log::info!("RTSX: SDHC={}", sdhc);

        let mut ocr_arg = 0x00FF_8000u32;
        if sdhc {
            ocr_arg |= 1 << 30;
        }
        log::info!("RTSX: ACMD41");
        let mut ocr = 0u32;
        let mut ok = false;
        for _ in 0..2000 {
            if let Ok(rsp) = self.sd_acmd(ACMD41_SEND_OP_COND, ocr_arg, SD_RSP_TYPE_R3) {
                if (rsp & (1 << 31)) != 0 {
                    ocr = rsp;
                    ok = true;
                    break;
                }
            }
            for _ in 0..20_000 {
                core::hint::spin_loop();
            }
        }
        if !ok {
            return Err("ACMD41 timeout");
        }
        log::info!("RTSX: ACMD41 OK, OCR={:#010x}", ocr);

        let card_type = if (ocr & (1 << 30)) != 0 {
            if (ocr & (1 << 28)) != 0 {
                SdCardType::SDXC
            } else {
                SdCardType::SDHC
            }
        } else {
            SdCardType::SDSC
        };

        self.sd_cmd(CMD2_ALL_SEND_CID, 0, SD_RSP_TYPE_R2, 0)?;
        let mut cid = [0u8; 16];
        self.ppbuf_read(&mut cid);

        let r6 = self.sd_cmd(CMD3_SEND_RELATIVE_ADDR, 0, SD_RSP_TYPE_R6, 0)?;
        let rca = ((r6 >> 16) & 0xFFFF) as u16;
        if rca == 0 {
            return Err("RCA=0");
        }
        log::info!("RTSX: RCA={:#06x}", rca);

        self.sd_cmd(CMD9_SEND_CSD, (rca as u32) << 16, SD_RSP_TYPE_R2, 0)?;
        let mut csd = [0u8; 16];
        self.ppbuf_read(&mut csd);

        let (block_size, total_blocks) = Self::parse_csd(&csd, card_type);

        self.sd_cmd(CMD7_SELECT_CARD, (rca as u32) << 16, SD_RSP_TYPE_R1B, 0)?;

        if card_type == SdCardType::SDSC {
            let _ = self.sd_cmd(CMD16_SET_BLOCKLEN, 512, SD_RSP_TYPE_R1, 0);
        }

        let bs = if card_type == SdCardType::SDSC { 512 } else { block_size };
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

        log::info!("RTSX: SD card {:?}: {} blocks, {} bytes/block", card_type, tb, bs);
        Ok(())
    }

    fn parse_csd(csd: &[u8; 16], card_type: SdCardType) -> (u32, u64) {
        match card_type {
            SdCardType::SDHC | SdCardType::SDXC => {
                let c_size = ((csd[7] & 0x3F) as u32) << 16
                    | (csd[8] as u32) << 8
                    | csd[9] as u32;
                (512, (c_size as u64 + 1) * 1024)
            }
            _ => {
                let read_bl_len = csd[5] & 0x0F;
                let bs = 1u32 << read_bl_len;
                let c_size = ((csd[6] & 0x03) as u32) << 10
                    | (csd[7] as u32) << 2
                    | ((csd[8] >> 6) & 0x03) as u32;
                let c_size_mult = (((csd[9] >> 7) & 0x01) << 2)
                    | ((csd[10] >> 6) & 0x03);
                let mult = 1u32 << (c_size_mult as u32 + 2);
                let blocks = ((c_size as u64 + 1) * mult as u64) * (bs as u64) / 512;
                (bs, blocks)
            }
        }
    }

    // ── Sector I/O ────────────────────────────────────────────

    fn read_sector(&self, lba: u32, buf: &mut [u8]) -> Result<(), &'static str> {
        let card = self.sd_card.as_ref().ok_or("no card")?;
        let addr = match card.card_type {
            SdCardType::SDSC => lba * 512,
            _ => lba,
        };

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

    fn write_sector(&self, lba: u32, buf: &[u8]) -> Result<(), &'static str> {
        let card = self.sd_card.as_ref().ok_or("no card")?;
        let addr = match card.card_type {
            SdCardType::SDSC => lba * 512,
            _ => lba,
        };

        self.ppbuf_write(buf);

        self.w8(SD_BYTE_CNT_L, 0x00);
        self.w8(SD_BYTE_CNT_H, 0x02);
        self.w8(SD_BLOCK_CNT_L, 0x01);
        self.w8(SD_BLOCK_CNT_H, 0x00);

        self.w8(SD_CMD0, CMD24_WRITE_SINGLE);
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
        let required = (count as usize)
            .checked_mul(512)
            .ok_or("sector count too large")?;
        if buf.len() < required {
            return Err("read buffer too small");
        }
        for i in 0..count as u32 {
            let off = (i * 512) as usize;
            let sector = lba.checked_add(i).ok_or("LBA overflow")?;
            self.read_sector(sector, &mut buf[off..off + 512])?;
        }
        Ok(())
    }

    pub fn write_sectors(&self, lba: u32, count: u16, buf: &[u8]) -> Result<(), &'static str> {
        let required = (count as usize)
            .checked_mul(512)
            .ok_or("sector count too large")?;
        if buf.len() < required {
            return Err("write buffer too small");
        }
        for i in 0..count as u32 {
            let off = (i * 512) as usize;
            let sector = lba.checked_add(i).ok_or("LBA overflow")?;
            self.write_sector(sector, &buf[off..off + 512])?;
        }
        Ok(())
    }
}

// ── Static controller for kernel access ──────────────────────

static CONTROLLER: Mutex<Option<RtsxController>> = Mutex::new(None);

/// Safe probe: only PCI config space (port I/O), no MMIO.
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
            log::info!("RTSX: found at {:02x}:{:02x}.{} ({:#06x}:{:#06x})",
                dev.bus, dev.device, dev.function, dev.vendor_id, dev.device_id);

            // Ensure D0 via PCI config space (safe)
            dev.ensure_d0();
            dev.disable_pcie_aspm();
            dev.enable_memory_access();

            // Disable ASPM on the upstream PCIe bridge by walking the
            // PCI topology to find the bridge whose secondary bus matches
            // the RTSX device's bus.
            let upstream_bridge = scanner.get_devices().iter().find(|b| {
                if b.class_code != 0x06 || b.subclass != 0x04 {
                    return false;
                }
                let sec_bus = PciConfigSpace::read_config_byte(b.bus, b.device, b.function, 0x19);
                sec_bus == dev.bus
            });
            if let Some(bridge) = upstream_bridge {
                log::info!("RTSX: disabling ASPM on upstream bridge {:02x}:{:02x}.{}",
                    bridge.bus, bridge.device, bridge.function);
                bridge.disable_pcie_aspm();
            } else {
                log::info!("RTSX: upstream bridge not found for bus {:#x}", dev.bus);
            }

            // Read BAR0 directly — do NOT call get_bar_info() which writes
            // 0xFFFFFFFF to the BAR (detect_bar_size) and can confuse the device.
            let bar_val = PciConfigSpace::read_config_dword(dev.bus, dev.device, dev.function, 0x10);
            if bar_val == 0 || bar_val == 0xFFFFFFFF {
                log::info!("RTSX: BAR0 invalid ({:#x})", bar_val);
                return;
            }
            if (bar_val & 0x1) != 0 {
                log::info!("RTSX: BAR0 is I/O, expected memory");
                return;
            }
            let bar0_addr = if (bar_val & 0x6) == 0x4 {
                let bar_hi =
                    PciConfigSpace::read_config_dword(dev.bus, dev.device, dev.function, 0x14);
                ((bar_hi as u64) << 32) | ((bar_val as u64) & 0xFFFF_FFF0)
            } else {
                (bar_val & 0xFFFF_FFF0) as u64
            };
            let bar0_size = 0x1000u32; // RTS5249 BAR0 is 4KB

            if bar0_addr + bar0_size as u64 > 0x1_0000_0000 {
                log::info!("RTSX: BAR0 is above 4GB, not supported by 32-bit bridge window");
                log::info!("RTSX: mapping MMIO at {:#x} size {} anyway", bar0_addr, bar0_size);
            }

            // Configure the upstream bridge's memory window to cover BAR0.
            if let Some(ref bridge) = upstream_bridge {
                let base_reg = PciConfigSpace::read_config_dword(
                    bridge.bus, bridge.device, bridge.function, 0x20);
                let mem_base = base_reg as u16;
                let mem_limit = (base_reg >> 16) as u16;
                let bar_top = bar0_addr + bar0_size as u64 - 1;
                let need_base = ((bar0_addr >> 16) & 0xFFF0) as u16;
                let need_limit = ((bar_top >> 16) & 0xFFF0) as u16;

                if mem_base != need_base || mem_limit != need_limit {
                    log::info!("RTSX: bridge window {:#06x}-{:#06x} needs {:#06x}-{:#06x}",
                        mem_base, mem_limit, need_base, need_limit);
                    let new_win = (need_limit as u32) << 16 | need_base as u32;
                    PciConfigSpace::write_config_dword_raw(
                        bridge.bus, bridge.device, bridge.function, 0x20, new_win);
                    log::info!("RTSX: bridge window updated");
                } else {
                    log::info!("RTSX: bridge window OK ({:#06x}-{:#06x})",
                        mem_base, mem_limit);
                }
            }

            log::info!("RTSX: BAR0 at {:#x} size {:#x}", bar0_addr, bar0_size);

            let mmio = ctx.phys_to_virt(bar0_addr) as *mut u8;
            if ctx.map_mmio_region(bar0_addr as usize, mmio as usize, bar0_size as usize).is_err() {
                log::info!("RTSX: MMIO mapping failed");
                return;
            }

            *CONTROLLER.lock() = Some(RtsxController {
                device: dev.clone(),
                bar0_phys: bar0_addr,
                bar0_size,
                mmio,
                mmio_mapped: true,
                sd_card: None,
            });
            log::info!("RTSX: controller registered (MMIO deferred)");
            return;
        }
    }
    log::info!("RTSX: no card reader found");
}

/// Initialise SD card (first MMIO access happens here).
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
