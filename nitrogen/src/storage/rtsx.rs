//! Realtek RTS5249 PCIe SD-card reader.
//!
//! The controller exposes only a handful of BAR registers. Its SD engine lives
//! in a 16-bit internal register space accessed through `RTSX_HAIMR`; treating
//! those addresses as BAR byte offsets leaves the engine completely untouched.

use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::mmio::MemRegion;
use crate::pci::{PciConfigSpace, PciDevice, PciScanner};
use crate::pci_health::PciHealth;
use crate::timing::delay_ms;

const RTSX_HAIMR: usize = 0x10;
const RTSX_BIPR: usize = 0x14;
const RTSX_BIER: usize = 0x18;
const HAIMR_START: u32 = 1 << 31;
const HAIMR_WRITE: u32 = 1 << 30;
const SD_EXIST: u32 = 1 << 16;

const CARD_PWR_CTL: u16 = 0xFD50;
const CARD_SHARE_MODE: u16 = 0xFD52;
const CARD_STOP: u16 = 0xFD54;
const CARD_OE: u16 = 0xFD55;
const CARD_DATA_SOURCE: u16 = 0xFD5B;
const CARD_SELECT: u16 = 0xFD5C;
const CARD_PULL_CTL1: u16 = 0xFD60;
const CARD_PULL_CTL2: u16 = 0xFD61;
const CARD_PULL_CTL3: u16 = 0xFD62;
const CARD_PULL_CTL4: u16 = 0xFD63;
const CARD_CLK_EN: u16 = 0xFD69;
const SD_CFG1: u16 = 0xFDA0;
const SD_CFG2: u16 = 0xFDA1;
const SD_STAT1: u16 = 0xFDA3;
const SD_BUS_STAT: u16 = 0xFDA5;
const SD_CMD0: u16 = 0xFDA9;
const SD_CMD1: u16 = 0xFDAA;
const SD_BYTE_CNT_L: u16 = 0xFDAF;
const SD_BYTE_CNT_H: u16 = 0xFDB0;
const SD_BLOCK_CNT_L: u16 = 0xFDB1;
const SD_BLOCK_CNT_H: u16 = 0xFDB2;
const SD_TRANSFER: u16 = 0xFDB3;
const PWR_GATE_CTRL: u16 = 0xFE75;
const PPBUF_BASE2: u16 = 0xFA00;

const SD_CMD_START: u8 = 0x40;
const SD_TRANSFER_START: u8 = 0x80;
const SD_TRANSFER_END: u8 = 0x40;
const SD_STAT_IDLE: u8 = 0x20;
const SD_TRANSFER_ERR: u8 = 0x10;
const SD_TM_CMD_RSP: u8 = 0x08;
const SD_TM_NORMAL_READ: u8 = 0x0C;
const SD_TM_AUTO_WRITE_3: u8 = 0x01;
const SD_RSP_R0: u8 = 0x04;
const SD_RSP_R1: u8 = 0x01;
const SD_RSP_R1B: u8 = 0x09;
const SD_RSP_R2: u8 = 0x02;
const SD_RSP_R3: u8 = 0x05;

const CMD0_GO_IDLE: u8 = 0;
const CMD2_ALL_SEND_CID: u8 = 2;
const CMD3_SEND_RELATIVE_ADDR: u8 = 3;
const CMD7_SELECT_CARD: u8 = 7;
const CMD8_SEND_IF_COND: u8 = 8;
const CMD9_SEND_CSD: u8 = 9;
const CMD13_SEND_STATUS: u8 = 13;
const CMD16_SET_BLOCKLEN: u8 = 16;
const CMD17_READ_SINGLE: u8 = 17;
const CMD24_WRITE_SINGLE: u8 = 24;
const CMD55_APP_CMD: u8 = 55;
const ACMD6_SET_BUS_WIDTH: u8 = 6;
const ACMD41_SEND_OP_COND: u8 = 41;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdCardType {
    Sdsc,
    Sdhc,
    Sdxc,
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

pub struct RtsxController {
    device: PciDevice,
    mmio: MemRegion,
    sd_card: Option<SdCardInfo>,
    card_was_present: bool,
    health: PciHealth,
}

// Access to the controller is serialized by `CONTROLLER`; its pointer denotes
// a permanently mapped MMIO BAR and is never dereferenced as ordinary memory.
unsafe impl Send for RtsxController {}

impl RtsxController {
    fn read_reg(&self, address: u16) -> Result<u8, &'static str> {
        self.mmio.write32(
            RTSX_HAIMR,
            HAIMR_START | (u32::from(address & 0x3FFF) << 16),
        );
        for _ in 0..1024 {
            let value = self.mmio.read32(RTSX_HAIMR);
            if value & HAIMR_START == 0 {
                return Ok(value as u8);
            }
            core::hint::spin_loop();
        }
        Err("RTSX internal register read timed out")
    }

    fn write_reg(&self, address: u16, mask: u8, value: u8) -> Result<(), &'static str> {
        self.mmio.write32(
            RTSX_HAIMR,
            HAIMR_START
                | HAIMR_WRITE
                | (u32::from(address & 0x3FFF) << 16)
                | (u32::from(mask) << 8)
                | u32::from(value),
        );
        for _ in 0..1024 {
            let result = self.mmio.read32(RTSX_HAIMR);
            if result & HAIMR_START == 0 {
                return (result as u8 == value)
                    .then_some(())
                    .ok_or("RTSX internal register write failed");
            }
            core::hint::spin_loop();
        }
        Err("RTSX internal register write timed out")
    }

    fn write_regs(&self, writes: &[(u16, u8, u8)]) -> Result<(), &'static str> {
        writes
            .iter()
            .try_for_each(|&(register, mask, value)| self.write_reg(register, mask, value))
    }

    fn card_present(&self) -> bool {
        self.mmio.read32(RTSX_BIPR) & SD_EXIST != 0
    }

    fn prepare_device(&mut self) -> Result<(), &'static str> {
        self.device.ensure_d0();
        self.device.enable_memory_access();
        self.health
            .pre_mmio_access()
            .map_err(|_| "RTSX device is not safely accessible")
    }

    fn init_hardware(&mut self) -> Result<(), &'static str> {
        self.prepare_device()?;
        self.mmio.write32(RTSX_BIER, 0);
        if !self.card_present() {
            return Err("no SD card inserted");
        }

        self.write_regs(&[
            (CARD_SELECT, 0x07, 0x02),
            (CARD_SHARE_MODE, 0x0F, 0x04),
            (CARD_CLK_EN, 0x04, 0x04),
            (CARD_PULL_CTL1, 0xFF, 0x66),
            (CARD_PULL_CTL2, 0xFF, 0xAA),
            (CARD_PULL_CTL3, 0xFF, 0xE9),
            (CARD_PULL_CTL4, 0xFF, 0xAA),
            (CARD_PWR_CTL, 0x03, 0x02),
            (PWR_GATE_CTRL, 0x06, 0x02),
        ])?;
        delay_ms(5);
        self.write_regs(&[
            (CARD_PWR_CTL, 0x03, 0x00),
            (PWR_GATE_CTRL, 0x06, 0x06),
            (CARD_OE, 0x04, 0x04),
            (SD_CFG1, 0xC3, 0x80),
            (CARD_STOP, 0x44, 0x44),
            (SD_BUS_STAT, 0x80, 0x80),
        ])?;
        delay_ms(1);
        Ok(())
    }

    fn set_command(&self, command: u8, argument: u32) -> Result<(), &'static str> {
        let bytes = argument.to_be_bytes();
        self.write_reg(SD_CMD0, 0xFF, SD_CMD_START | command)?;
        bytes
            .iter()
            .enumerate()
            .try_for_each(|(index, &byte)| self.write_reg(SD_CMD1 + index as u16, 0xFF, byte))
    }

    fn wait_transfer(&self, required: u8) -> Result<(), &'static str> {
        for _ in 0..100_000 {
            let state = self.read_reg(SD_TRANSFER)?;
            if state & SD_TRANSFER_ERR != 0 {
                let _ = self.write_reg(CARD_STOP, 0x44, 0x44);
                return Err("SD transfer failed");
            }
            if state & required == required {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        let _ = self.write_reg(CARD_STOP, 0x44, 0x44);
        Err("SD transfer timed out")
    }

    fn command(&self, command: u8, argument: u32, response: u8) -> Result<u32, &'static str> {
        self.set_command(command, argument)?;
        self.write_regs(&[
            (SD_CFG2, 0xFF, response),
            (CARD_DATA_SOURCE, 0x01, 0x01),
            (SD_TRANSFER, 0xFF, SD_TRANSFER_START | SD_TM_CMD_RSP),
        ])?;
        self.wait_transfer(SD_TRANSFER_END | SD_STAT_IDLE)?;
        if response == SD_RSP_R0 || response == SD_RSP_R2 {
            return Ok(0);
        }

        let mut bytes = [0; 4];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = self.read_reg(SD_CMD1 + index as u16)?;
        }
        if self.read_reg(SD_STAT1)? & 0x80 != 0 && response != SD_RSP_R3 {
            return Err("SD response CRC error");
        }
        Ok(u32::from_be_bytes(bytes))
    }

    fn app_command(
        &self,
        rca: u16,
        command: u8,
        argument: u32,
        response: u8,
    ) -> Result<u32, &'static str> {
        let r1 = self.command(CMD55_APP_CMD, u32::from(rca) << 16, SD_RSP_R1)?;
        if r1 & (1 << 5) == 0 {
            return Err("card rejected APP_CMD");
        }
        self.command(command, argument, response)
    }

    fn long_response(&self) -> Result<[u8; 16], &'static str> {
        let mut raw = [0; 17];
        for (index, byte) in raw[..16].iter_mut().enumerate() {
            *byte = self.read_reg(PPBUF_BASE2 + index as u16)?;
        }
        raw[16] = 1;
        let mut response = [0; 16];
        response.copy_from_slice(&raw[1..]);
        Ok(response)
    }

    fn set_data_len(&self) -> Result<(), &'static str> {
        self.write_regs(&[
            (SD_BYTE_CNT_L, 0xFF, 0),
            (SD_BYTE_CNT_H, 0xFF, 2),
            (SD_BLOCK_CNT_L, 0xFF, 1),
            (SD_BLOCK_CNT_H, 0xFF, 0),
        ])
    }

    fn ppbuf_read(&self, buffer: &mut [u8]) -> Result<(), &'static str> {
        buffer.iter_mut().enumerate().try_for_each(|(index, byte)| {
            *byte = self.read_reg(PPBUF_BASE2 + index as u16)?;
            Ok(())
        })
    }

    fn ppbuf_write(&self, buffer: &[u8]) -> Result<(), &'static str> {
        buffer
            .iter()
            .enumerate()
            .try_for_each(|(index, &byte)| self.write_reg(PPBUF_BASE2 + index as u16, 0xFF, byte))
    }

    pub fn init_sd_card(&mut self) -> Result<(), &'static str> {
        self.init_hardware()?;
        self.command(CMD0_GO_IDLE, 0, SD_RSP_R0)?;
        delay_ms(1);

        let v2_card = self
            .command(CMD8_SEND_IF_COND, 0x1AA, SD_RSP_R1)
            .is_ok_and(|response| response & 0xFFF == 0x1AA);
        let argument = 0x00FF_8000 | if v2_card { 1 << 30 } else { 0 };
        let mut ocr = None;
        for _ in 0..1000 {
            if let Ok(response) = self.app_command(0, ACMD41_SEND_OP_COND, argument, SD_RSP_R3) {
                if response & (1 << 31) != 0 {
                    ocr = Some(response);
                    break;
                }
            }
            delay_ms(1);
        }
        let block_addressed = ocr.ok_or("ACMD41 timed out")? & (1 << 30) != 0;

        self.command(CMD2_ALL_SEND_CID, 0, SD_RSP_R2)?;
        let cid = self.long_response()?;
        let rca = (self.command(CMD3_SEND_RELATIVE_ADDR, 0, SD_RSP_R1)? >> 16) as u16;
        if rca == 0 {
            return Err("card returned RCA zero");
        }
        self.command(CMD9_SEND_CSD, u32::from(rca) << 16, SD_RSP_R2)?;
        let csd = self.long_response()?;
        let total_blocks = Self::parse_csd(&csd, block_addressed)?;

        self.command(CMD7_SELECT_CARD, u32::from(rca) << 16, SD_RSP_R1B)?;
        if !block_addressed {
            self.command(CMD16_SET_BLOCKLEN, 512, SD_RSP_R1)?;
        }
        if self
            .app_command(rca, ACMD6_SET_BUS_WIDTH, 2, SD_RSP_R1)
            .is_ok()
        {
            self.write_reg(SD_CFG1, 0x03, 0x01)?;
        }
        self.write_reg(SD_CFG1, 0xC0, 0)?;

        let card_type = if !block_addressed {
            SdCardType::Sdsc
        } else if total_blocks > 32 * 1024 * 1024 * 1024 / 512 {
            SdCardType::Sdxc
        } else {
            SdCardType::Sdhc
        };
        self.sd_card = Some(SdCardInfo {
            card_type,
            rca,
            cid,
            csd,
            block_size: 512,
            total_blocks,
        });
        self.card_was_present = true;
        log::info!(
            "RTSX: {:?} card initialized ({} sectors)",
            card_type,
            total_blocks
        );
        Ok(())
    }

    fn parse_csd(csd: &[u8; 16], block_addressed: bool) -> Result<u64, &'static str> {
        if block_addressed {
            let c_size =
                (u32::from(csd[7] & 0x3F) << 16) | (u32::from(csd[8]) << 8) | u32::from(csd[9]);
            return Ok((u64::from(c_size) + 1) * 1024);
        }

        let block_len = 1u64
            .checked_shl(u32::from(csd[5] & 0x0F))
            .ok_or("invalid CSD block length")?;
        let c_size =
            (u32::from(csd[6] & 3) << 10) | (u32::from(csd[7]) << 2) | u32::from(csd[8] >> 6);
        let multiplier = 1u64 << (u32::from((csd[9] & 3) << 1 | csd[10] >> 7) + 2);
        (u64::from(c_size) + 1)
            .checked_mul(multiplier)
            .and_then(|blocks| blocks.checked_mul(block_len))
            .map(|bytes| bytes / 512)
            .ok_or("CSD capacity overflow")
    }

    fn card_address(&self, lba: u32) -> Result<u32, &'static str> {
        match self
            .sd_card
            .as_ref()
            .ok_or("no initialized card")?
            .card_type
        {
            SdCardType::Sdsc => lba.checked_mul(512).ok_or("LBA overflow"),
            SdCardType::Sdhc | SdCardType::Sdxc => Ok(lba),
        }
    }

    fn read_sector(&self, lba: u32, buffer: &mut [u8]) -> Result<(), &'static str> {
        self.set_command(CMD17_READ_SINGLE, self.card_address(lba)?)?;
        self.set_data_len()?;
        self.write_regs(&[
            (SD_CFG2, 0xFF, SD_RSP_R1),
            (CARD_DATA_SOURCE, 0x01, 0x01),
            (SD_TRANSFER, 0xFF, SD_TRANSFER_START | SD_TM_NORMAL_READ),
        ])?;
        self.wait_transfer(SD_TRANSFER_END)?;
        self.ppbuf_read(buffer)
    }

    fn write_sector(&self, lba: u32, buffer: &[u8]) -> Result<(), &'static str> {
        let card = self.sd_card.as_ref().ok_or("no initialized card")?;
        self.command(CMD24_WRITE_SINGLE, self.card_address(lba)?, SD_RSP_R1)?;
        self.ppbuf_write(buffer)?;
        self.set_data_len()?;
        self.write_regs(&[
            (SD_CFG2, 0xFF, 0),
            (SD_TRANSFER, 0xFF, SD_TRANSFER_START | SD_TM_AUTO_WRITE_3),
        ])?;
        self.wait_transfer(SD_TRANSFER_END)?;
        for _ in 0..1000 {
            if self
                .command(CMD13_SEND_STATUS, u32::from(card.rca) << 16, SD_RSP_R1)
                .is_ok_and(|status| status & (1 << 8) != 0)
            {
                return Ok(());
            }
            delay_ms(1);
        }
        Err("card programming timed out")
    }

    pub fn read_sectors(
        &self,
        lba: u32,
        count: u16,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        let bytes = usize::from(count)
            .checked_mul(512)
            .ok_or("sector count overflow")?;
        let destination = buffer.get_mut(..bytes).ok_or("read buffer too small")?;
        destination
            .chunks_exact_mut(512)
            .enumerate()
            .try_for_each(|(index, sector)| {
                self.read_sector(lba.checked_add(index as u32).ok_or("LBA overflow")?, sector)
            })
    }

    pub fn write_sectors(&self, lba: u32, count: u16, buffer: &[u8]) -> Result<(), &'static str> {
        let bytes = usize::from(count)
            .checked_mul(512)
            .ok_or("sector count overflow")?;
        let source = buffer.get(..bytes).ok_or("write buffer too small")?;
        source
            .chunks_exact(512)
            .enumerate()
            .try_for_each(|(index, sector)| {
                self.write_sector(lba.checked_add(index as u32).ok_or("LBA overflow")?, sector)
            })
    }

    fn poll_card_detect(&mut self) -> bool {
        if self.prepare_device().is_err() {
            return false;
        }
        let present = self.card_present();
        let changed = present != self.card_was_present;
        self.card_was_present = present;
        if changed && !present {
            self.sd_card = None;
        }
        changed
    }
}

static CONTROLLER: Mutex<Option<RtsxController>> = Mutex::new(None);

pub fn init(context: &dyn DriverContext) {
    let mut scanner = PciScanner::new();
    if scanner.scan_all_buses().is_err() {
        return;
    }
    let Some(device) = scanner
        .get_devices()
        .iter()
        .find(|device| device.vendor_id == 0x10EC && device.device_id == 0x5249)
        .cloned()
    else {
        log::info!("RTSX: supported card reader not found");
        return;
    };

    device.ensure_d0();
    device.disable_pcie_aspm();
    device.enable_memory_access();
    let Some(bar0) = device.read_bar(0).filter(|&address| address != 0) else {
        log::warn!("RTSX: invalid BAR0");
        return;
    };
    let virtual_address = context.phys_to_virt(bar0);
    if context
        .map_mmio_region(bar0 as usize, virtual_address, 0x1000)
        .is_err()
    {
        log::warn!("RTSX: failed to map BAR0");
        return;
    }

    let upstream = scanner.get_devices().iter().find(|bridge| {
        bridge.class_code == 0x06
            && bridge.subclass == 0x04
            && PciConfigSpace::read_config_byte(bridge.bus, bridge.device, bridge.function, 0x19)
                == device.bus
    });
    let health = upstream.map_or_else(
        || PciHealth::new(&device),
        |bridge| {
            bridge.disable_pcie_aspm();
            PciHealth::new(&device).with_upstream_bridge(bridge.bus, bridge.device, bridge.function)
        },
    );
    let mmio = unsafe { MemRegion::new(virtual_address as *mut u8, 0x1000) };
    *CONTROLLER.lock() = Some(RtsxController {
        device,
        mmio,
        sd_card: None,
        card_was_present: false,
        health,
    });
    log::info!("RTSX: RTS5249 registered; SD initialization deferred");
}

pub fn init_sd_card() -> Result<(), &'static str> {
    CONTROLLER
        .lock()
        .as_mut()
        .ok_or("no RTS5249 controller")?
        .init_sd_card()
}

pub fn sd_card_info() -> Option<SdCardInfo> {
    CONTROLLER
        .lock()
        .as_ref()
        .and_then(|controller| controller.sd_card.clone())
}

pub fn read_sectors(lba: u32, count: u16, buffer: &mut [u8]) -> Result<(), &'static str> {
    CONTROLLER
        .lock()
        .as_ref()
        .ok_or("no RTS5249 controller")?
        .read_sectors(lba, count, buffer)
}

pub fn write_sectors(lba: u32, count: u16, buffer: &[u8]) -> Result<(), &'static str> {
    CONTROLLER
        .lock()
        .as_ref()
        .ok_or("no RTS5249 controller")?
        .write_sectors(lba, count, buffer)
}

pub fn is_present() -> bool {
    CONTROLLER.lock().is_some()
}

pub fn poll_card_detect() -> bool {
    CONTROLLER
        .lock()
        .as_mut()
        .is_some_and(RtsxController::poll_card_detect)
}

pub fn is_card_detected() -> bool {
    CONTROLLER
        .lock()
        .as_ref()
        .is_some_and(|controller| controller.card_was_present)
}
