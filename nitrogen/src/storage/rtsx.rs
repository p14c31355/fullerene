//! Realtek RTS5249 PCIe SD-card reader.
//!
//! The controller exposes only a handful of BAR registers. Its SD engine lives
//! in a 16-bit internal register space accessed through `RTSX_HAIMR`; treating
//! those addresses as BAR byte offsets leaves the engine completely untouched.

use core::mem::size_of;

use spin::Mutex;

use crate::driver_context::DriverContext;
use crate::mmio::{DmaRegion, MemRegion};
use crate::pci::{PciConfigSpace, PciDevice, PciScanner};
use crate::pci_health::PciHealth;
use crate::timing::delay_ms;

const RTSX_HCBAR: usize = 0x00;
const RTSX_HCBCTLR: usize = 0x04;
const RTSX_HDBAR: usize = 0x08;
const RTSX_HDBCTLR: usize = 0x0C;
const RTSX_HAIMR: usize = 0x10;
const RTSX_BIPR: usize = 0x14;
const RTSX_BIER: usize = 0x18;
const HAIMR_START: u32 = 1 << 31;
const HAIMR_WRITE: u32 = 1 << 30;
const SD_EXIST: u32 = 1 << 16;
const TRANS_OK_INT: u32 = 1 << 29;
const TRANS_FAIL_INT: u32 = 1 << 28;
const HOST_COMMAND_START: u32 = 1 << 31;
const HOST_COMMAND_AUTO_RESPONSE: u32 = 1 << 30;
const HOST_COMMAND_STOP: u32 = 1 << 28;
const HOST_DMA_STOP: u32 = 1 << 28;
const HOST_DMA_DEVICE_TO_HOST: u32 = 1 << 29;
const HOST_DMA_START: u32 = 1 << 31;
const HOST_COMMAND_BUFFER_SIZE: usize = 1024;
const HOST_COMMAND_CHUNK: usize = HOST_COMMAND_BUFFER_SIZE / size_of::<u32>();
const DATA_BUFFER_SIZE: usize = 512;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCommandKind {
    Read,
    Write,
    Check,
}

type RegisterCommand = (HostCommandKind, u16, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataPath {
    Sdma,
    HostPpbuf,
    Pio,
}

impl DataPath {
    const fn preferred(host_commands: bool, data_buffer: bool) -> Self {
        if host_commands && data_buffer {
            Self::Sdma
        } else if host_commands {
            Self::HostPpbuf
        } else {
            Self::Pio
        }
    }
}

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
const DMACTL: u16 = 0xFE2C;
const IRQSTAT0: u16 = 0xFE21;
const DMATC0: u16 = 0xFE28;
const DMATC1: u16 = 0xFE29;
const DMATC2: u16 = 0xFE2A;
const DMATC3: u16 = 0xFE2B;
const RBCTL: u16 = 0xFE34;
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
const SD_TM_AUTO_READ_3: u8 = 0x05;
const SD_TM_AUTO_WRITE_3: u8 = 0x01;
const SD_NO_CALCULATE_CRC7: u8 = 0x80;
const SD_NO_CHECK_WAIT_CRC_TO: u8 = 0x20;
const SD_NO_CHECK_CRC7: u8 = 0x04;
const DMA_FROM_CARD: u8 = 0x23;
const DMA_TO_CARD: u8 = 0x21;
const SD_WRITE_CONFIG: u8 = SD_NO_CALCULATE_CRC7 | SD_NO_CHECK_WAIT_CRC_TO | SD_NO_CHECK_CRC7;
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
    host_commands: Option<DmaRegion>,
    data_buffer: Option<DmaRegion>,
    data_path: DataPath,
}

// Access to the controller is serialized by `CONTROLLER`; its pointer denotes
// a permanently mapped MMIO BAR and is never dereferenced as ordinary memory.
unsafe impl Send for RtsxController {}

impl RtsxController {
    fn host_command(kind: HostCommandKind, address: u16, mask: u8, value: u8) -> u32 {
        ((kind as u32) << 30)
            | (u32::from(address & 0x3FFF) << 16)
            | (u32::from(mask) << 8)
            | u32::from(value)
    }

    fn read_reg(&self, address: u16) -> Result<u8, &'static str> {
        self.mmio.write32(
            RTSX_HAIMR,
            HAIMR_START | (u32::from(address & 0x3FFF) << 16),
        );
        match crate::timing::poll_timeout_us(10_000, || {
            let value = self.mmio.read32(RTSX_HAIMR);
            if value & HAIMR_START == 0 {
                Some(value as u8)
            } else {
                None
            }
        }) {
            Some(v) => Ok(v),
            None => Err("RTSX internal register read timed out"),
        }
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
        match crate::timing::poll_timeout_us(10_000, || {
            let result = self.mmio.read32(RTSX_HAIMR);
            if result & HAIMR_START == 0 {
                Some(result)
            } else {
                None
            }
        }) {
            Some(result) => {
                if result as u8 == value {
                    Ok(())
                } else {
                    Err("RTSX internal register write failed")
                }
            }
            None => Err("RTSX internal register write timed out"),
        }
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
        if !self.device.prepare_mmio() {
            return Err("RTSX PCI command or power transition failed");
        }
        self.health
            .pre_mmio_access()
            .map_err(|_| "RTSX device is not safely accessible")
    }

    fn init_hardware(&mut self) -> Result<(), &'static str> {
        crate::debug::hint(b"sd_pci");
        self.prepare_device()?;
        self.data_path =
            DataPath::preferred(self.host_commands.is_some(), self.data_buffer.is_some());
        crate::debug::hint(b"sd_mmio");
        self.mmio.write32(RTSX_BIER, 0);
        crate::debug::hint(b"sd_bier");
        if !self.card_present() {
            return Err("no SD card inserted");
        }
        crate::debug::hint(b"sd_card");

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
        match crate::timing::poll_timeout_us(500_000, || match self.read_reg(SD_TRANSFER) {
            Ok(state) => {
                if state & SD_TRANSFER_ERR != 0 {
                    Some(Err("SD transfer failed"))
                } else if state & required == required {
                    Some(Ok(()))
                } else {
                    None
                }
            }
            Err(_) => Some(Err("SD transfer register read error")),
        }) {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => {
                self.stop_transfer();
                Err(e)
            }
            None => {
                self.stop_transfer();
                Err("SD transfer timed out")
            }
        }
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

    fn run_host_commands(&mut self, count: usize) -> Result<(), &'static str> {
        let bytes = count
            .checked_mul(size_of::<u32>())
            .filter(|&bytes| bytes != 0 && bytes <= HOST_COMMAND_BUFFER_SIZE)
            .ok_or("RTSX host command overflow")?;
        let iova = {
            let commands = self
                .host_commands
                .as_ref()
                .ok_or("RTSX host command buffer unavailable")?;
            commands.flush_for_device();
            u32::try_from(commands.dma_iova()).map_err(|_| "RTSX command address exceeds 32-bit")?
        };

        self.mmio.write32(RTSX_BIPR, TRANS_OK_INT | TRANS_FAIL_INT);
        self.mmio.write_batch_then_barrier(&[
            (RTSX_HCBAR, iova),
            (
                RTSX_HCBCTLR,
                HOST_COMMAND_START | HOST_COMMAND_AUTO_RESPONSE | bytes as u32,
            ),
        ]);
        let result = crate::timing::poll_timeout_us(250_000, || {
            let status = self.mmio.read32(RTSX_BIPR);
            (status & TRANS_FAIL_INT != 0)
                .then_some(Err("RTSX host command failed"))
                .or_else(|| (status & TRANS_OK_INT != 0).then_some(Ok(())))
        });
        self.mmio.write32(RTSX_BIPR, TRANS_OK_INT | TRANS_FAIL_INT);
        let result = result.unwrap_or(Err("RTSX host command timed out"));
        if result.is_err() {
            self.stop_transfer();
        }
        result
    }

    fn stop_transfer(&self) {
        // Mirror Linux's rtsx_pci_stop_cmd recovery. Stopping only HCBCTLR
        // leaves the DMA/ring-buffer state dirty and makes the next mount
        // capable of stalling after a timed-out host command.
        self.mmio.write_batch_then_barrier(&[
            (RTSX_HCBCTLR, HOST_COMMAND_STOP),
            (RTSX_HDBCTLR, HOST_DMA_STOP),
        ]);
        for &(register, mask, value) in &[
            (DMACTL, 0x80, 0x80),
            (RBCTL, 0x80, 0x80),
            (CARD_STOP, 0x44, 0x44),
        ] {
            let _ = self.write_reg(register, mask, value);
        }
        self.mmio.write32(RTSX_BIPR, TRANS_OK_INT | TRANS_FAIL_INT);
    }

    fn run_register_commands(&mut self, commands: &[RegisterCommand]) -> Result<(), &'static str> {
        self.load_register_commands(commands)?;
        self.run_host_commands(commands.len())
    }

    fn load_register_commands(&mut self, commands: &[RegisterCommand]) -> Result<(), &'static str> {
        commands
            .len()
            .checked_mul(size_of::<u32>())
            .filter(|&bytes| bytes != 0 && bytes <= HOST_COMMAND_BUFFER_SIZE)
            .ok_or("RTSX host command overflow")?;
        {
            let command_buffer = self
                .host_commands
                .as_mut()
                .ok_or("RTSX host command buffer unavailable")?;
            for (slot, &(kind, address, mask, value)) in command_buffer
                .as_mut_slice()
                .chunks_exact_mut(size_of::<u32>())
                .zip(commands)
            {
                slot.copy_from_slice(&Self::host_command(kind, address, mask, value).to_le_bytes());
            }
        }
        Ok(())
    }

    fn dma_iova(region: &Option<DmaRegion>) -> Result<u32, &'static str> {
        u32::try_from(
            region
                .as_ref()
                .ok_or("RTSX DMA buffer unavailable")?
                .dma_iova(),
        )
        .map_err(|_| "RTSX DMA address exceeds 32-bit")
    }

    fn data_dma_control(device_to_host: bool) -> u32 {
        HOST_DMA_START
            | if device_to_host {
                HOST_DMA_DEVICE_TO_HOST
            } else {
                0
            }
            | DATA_BUFFER_SIZE as u32
    }

    fn run_data_dma(
        &mut self,
        commands: &[RegisterCommand],
        device_to_host: bool,
    ) -> Result<(), &'static str> {
        self.load_register_commands(commands)?;
        let bytes = (commands.len() * size_of::<u32>()) as u32;
        let command_iova = Self::dma_iova(&self.host_commands)?;
        let data_iova = Self::dma_iova(&self.data_buffer)?;

        self.host_commands
            .as_ref()
            .ok_or("RTSX host command buffer unavailable")?
            .flush_for_device();
        if device_to_host {
            self.data_buffer
                .as_ref()
                .ok_or("RTSX data buffer unavailable")?
                .flush_for_device();
        }

        self.mmio.write32(RTSX_BIPR, TRANS_OK_INT | TRANS_FAIL_INT);
        self.mmio.write_batch_then_barrier(&[
            (RTSX_HCBAR, command_iova),
            (
                RTSX_HCBCTLR,
                HOST_COMMAND_START | HOST_COMMAND_AUTO_RESPONSE | bytes,
            ),
            (RTSX_HDBAR, data_iova),
            (RTSX_HDBCTLR, Self::data_dma_control(device_to_host)),
        ]);
        let result = crate::timing::poll_timeout_us(1_000_000, || {
            let status = self.mmio.read32(RTSX_BIPR);
            (status & TRANS_FAIL_INT != 0)
                .then_some(Err("RTSX data DMA failed"))
                .or_else(|| (status & TRANS_OK_INT != 0).then_some(Ok(())))
        })
        .unwrap_or(Err("RTSX data DMA timed out"));
        self.mmio.write32(RTSX_BIPR, TRANS_OK_INT | TRANS_FAIL_INT);
        if result.is_err() {
            self.stop_transfer();
        }
        result
    }

    fn read_sector_commands(argument: u32) -> [RegisterCommand; 12] {
        let [arg0, arg1, arg2, arg3] = argument.to_be_bytes();
        [
            (
                HostCommandKind::Write,
                SD_CMD0,
                0xFF,
                SD_CMD_START | CMD17_READ_SINGLE,
            ),
            (HostCommandKind::Write, SD_CMD1, 0xFF, arg0),
            (HostCommandKind::Write, SD_CMD1 + 1, 0xFF, arg1),
            (HostCommandKind::Write, SD_CMD1 + 2, 0xFF, arg2),
            (HostCommandKind::Write, SD_CMD1 + 3, 0xFF, arg3),
            (HostCommandKind::Write, SD_BYTE_CNT_L, 0xFF, 0),
            (HostCommandKind::Write, SD_BYTE_CNT_H, 0xFF, 2),
            (HostCommandKind::Write, SD_BLOCK_CNT_L, 0xFF, 1),
            (HostCommandKind::Write, SD_BLOCK_CNT_H, 0xFF, 0),
            (HostCommandKind::Write, SD_CFG2, 0xFF, SD_RSP_R1),
            (HostCommandKind::Write, CARD_DATA_SOURCE, 0x01, 0x01),
            (
                HostCommandKind::Write,
                SD_TRANSFER,
                0xFF,
                SD_TRANSFER_START | SD_TM_NORMAL_READ,
            ),
        ]
    }

    fn write_sector_commands() -> [RegisterCommand; 7] {
        [
            (HostCommandKind::Write, SD_BYTE_CNT_L, 0xFF, 0),
            (HostCommandKind::Write, SD_BYTE_CNT_H, 0xFF, 2),
            (HostCommandKind::Write, SD_BLOCK_CNT_L, 0xFF, 1),
            (HostCommandKind::Write, SD_BLOCK_CNT_H, 0xFF, 0),
            (HostCommandKind::Write, SD_CFG2, 0xFF, 0),
            (HostCommandKind::Write, CARD_DATA_SOURCE, 0x01, 0x01),
            (
                HostCommandKind::Write,
                SD_TRANSFER,
                0xFF,
                SD_TRANSFER_START | SD_TM_AUTO_WRITE_3,
            ),
        ]
    }

    fn data_dma_commands(dma_control: u8, sd_config: u8, transfer: u8) -> [RegisterCommand; 14] {
        [
            (HostCommandKind::Write, SD_BLOCK_CNT_L, 0xFF, 1),
            (HostCommandKind::Write, SD_BLOCK_CNT_H, 0xFF, 0),
            (HostCommandKind::Write, SD_BYTE_CNT_L, 0xFF, 0),
            (HostCommandKind::Write, SD_BYTE_CNT_H, 0xFF, 2),
            (HostCommandKind::Write, IRQSTAT0, 0x80, 0x80),
            (HostCommandKind::Write, DMATC3, 0xFF, 0),
            (HostCommandKind::Write, DMATC2, 0xFF, 0),
            (HostCommandKind::Write, DMATC1, 0xFF, 2),
            (HostCommandKind::Write, DMATC0, 0xFF, 0),
            (HostCommandKind::Write, DMACTL, 0x33, dma_control),
            (HostCommandKind::Write, CARD_DATA_SOURCE, 0x01, 0),
            (HostCommandKind::Write, SD_CFG2, 0xFF, sd_config),
            (
                HostCommandKind::Write,
                SD_TRANSFER,
                0xFF,
                SD_TRANSFER_START | transfer,
            ),
            (
                HostCommandKind::Check,
                SD_TRANSFER,
                SD_TRANSFER_END,
                SD_TRANSFER_END,
            ),
        ]
    }

    fn read_sector_dma_commands() -> [RegisterCommand; 14] {
        Self::data_dma_commands(DMA_FROM_CARD, SD_NO_CHECK_WAIT_CRC_TO, SD_TM_AUTO_READ_3)
    }

    fn write_sector_dma_commands() -> [RegisterCommand; 14] {
        Self::data_dma_commands(DMA_TO_CARD, SD_WRITE_CONFIG, SD_TM_AUTO_WRITE_3)
    }

    fn ppbuf_read_fast(&mut self, buffer: &mut [u8]) -> Result<(), &'static str> {
        for (chunk_index, output) in buffer.chunks_mut(HOST_COMMAND_CHUNK).enumerate() {
            let first_register = PPBUF_BASE2 + (chunk_index * HOST_COMMAND_CHUNK) as u16;
            {
                let command_buffer = self
                    .host_commands
                    .as_mut()
                    .ok_or("RTSX host command buffer unavailable")?;
                for (index, command) in command_buffer
                    .as_mut_slice()
                    .chunks_exact_mut(size_of::<u32>())
                    .take(output.len())
                    .enumerate()
                {
                    command.copy_from_slice(
                        &Self::host_command(
                            HostCommandKind::Read,
                            first_register + index as u16,
                            0,
                            0,
                        )
                        .to_le_bytes(),
                    );
                }
            }
            self.run_host_commands(output.len())?;
            let command_buffer = self
                .host_commands
                .as_ref()
                .ok_or("RTSX host command buffer unavailable")?;
            command_buffer.flush_for_cpu();
            // AUTO_RESPONSE packs one byte per READ_REG at the start of the
            // command buffer; responses do not remain in their u32 slots.
            // This matches Linux rtsx_pci_read_ppbuf/get_cmd_data semantics.
            output.copy_from_slice(&command_buffer.as_slice()[..output.len()]);
        }
        Ok(())
    }

    fn ppbuf_read_pio(&self, buffer: &mut [u8]) -> Result<(), &'static str> {
        buffer.iter_mut().enumerate().try_for_each(|(index, byte)| {
            *byte = self.read_reg(PPBUF_BASE2 + index as u16)?;
            Ok(())
        })
    }

    fn ppbuf_write_fast(&mut self, buffer: &[u8]) -> Result<(), &'static str> {
        for (chunk_index, input) in buffer.chunks(HOST_COMMAND_CHUNK).enumerate() {
            let first_register = PPBUF_BASE2 + (chunk_index * HOST_COMMAND_CHUNK) as u16;
            {
                let command_buffer = self
                    .host_commands
                    .as_mut()
                    .ok_or("RTSX host command buffer unavailable")?;
                for (index, (command, &value)) in command_buffer
                    .as_mut_slice()
                    .chunks_exact_mut(size_of::<u32>())
                    .zip(input)
                    .enumerate()
                {
                    command.copy_from_slice(
                        &Self::host_command(
                            HostCommandKind::Write,
                            first_register + index as u16,
                            0xFF,
                            value,
                        )
                        .to_le_bytes(),
                    );
                }
            }
            self.run_host_commands(input.len())?;
        }
        Ok(())
    }

    fn ppbuf_write_pio(&self, buffer: &[u8]) -> Result<(), &'static str> {
        buffer
            .iter()
            .enumerate()
            .try_for_each(|(index, &byte)| self.write_reg(PPBUF_BASE2 + index as u16, 0xFF, byte))
    }

    fn read_sector_pio(&self, argument: u32, buffer: &mut [u8]) -> Result<(), &'static str> {
        self.set_command(CMD17_READ_SINGLE, argument)?;
        self.set_data_len()?;
        self.write_regs(&[
            (SD_CFG2, 0xFF, SD_RSP_R1),
            (CARD_DATA_SOURCE, 0x01, 0x01),
            (SD_TRANSFER, 0xFF, SD_TRANSFER_START | SD_TM_NORMAL_READ),
        ])?;
        self.wait_transfer(SD_TRANSFER_END)?;
        self.ppbuf_read_pio(buffer)
    }

    fn read_sector_host_ppbuf(
        &mut self,
        argument: u32,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        self.run_register_commands(&Self::read_sector_commands(argument))?;
        self.wait_transfer(SD_TRANSFER_END)?;
        self.ppbuf_read_fast(buffer)
    }

    fn write_sector_pio(&self, buffer: &[u8]) -> Result<(), &'static str> {
        self.ppbuf_write_pio(buffer)?;
        self.set_data_len()?;
        self.write_regs(&[
            (SD_CFG2, 0xFF, SD_WRITE_CONFIG),
            (SD_TRANSFER, 0xFF, SD_TRANSFER_START | SD_TM_AUTO_WRITE_3),
        ])?;
        self.wait_transfer(SD_TRANSFER_END)
    }

    fn write_sector_host_ppbuf(&mut self, buffer: &[u8]) -> Result<(), &'static str> {
        self.ppbuf_write_fast(buffer)?;
        self.run_register_commands(&Self::write_sector_commands())?;
        self.wait_transfer(SD_TRANSFER_END)
    }

    pub fn init_sd_card(&mut self) -> Result<(), &'static str> {
        // A registered card is already in transfer state. Sending CMD0 and
        // ACMD41 again without a removal/power-cycle resets the protocol under
        // a live block device and is known to time out on RTS5249 hardware.
        if let Some(rca) = self.sd_card.as_ref().map(|card| card.rca) {
            self.prepare_device()?;
            if self.card_present()
                && self
                    .command(CMD13_SEND_STATUS, u32::from(rca) << 16, SD_RSP_R1)
                    .is_ok_and(|status| status & (1 << 8) != 0)
            {
                self.card_was_present = true;
                log::info!("RTSX: reusing initialized SD card");
                return Ok(());
            }
            self.sd_card = None;
        }

        // Overall timeout: must complete within 5 seconds on real hardware.
        let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let duration_ticks = 5_000_000u64.saturating_mul(crate::timing::ticks_per_us());

        macro_rules! check_timeout {
            () => {
                if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start_tsc) >= duration_ticks
                {
                    return Err("SD init timed out");
                }
            };
        }

        self.init_hardware()?;
        check_timeout!();
        self.command(CMD0_GO_IDLE, 0, SD_RSP_R0)?;
        check_timeout!();
        delay_ms(1);

        let v2_card = self
            .command(CMD8_SEND_IF_COND, 0x1AA, SD_RSP_R1)
            .is_ok_and(|response| response & 0xFFF == 0x1AA);
        check_timeout!();
        let argument = 0x00FF_8000 | if v2_card { 1 << 30 } else { 0 };
        let ocr = crate::timing::poll_timeout_us(2_000_000, || {
            match self.app_command(0, ACMD41_SEND_OP_COND, argument, SD_RSP_R3) {
                Ok(response) if response & (1 << 31) != 0 => Some(response),
                _ => None,
            }
        })
        .ok_or("ACMD41 timed out")?;
        let block_addressed = ocr & (1 << 30) != 0;

        self.command(CMD2_ALL_SEND_CID, 0, SD_RSP_R2)?;
        check_timeout!();
        let cid = self.long_response()?;
        check_timeout!();
        let rca = (self.command(CMD3_SEND_RELATIVE_ADDR, 0, SD_RSP_R1)? >> 16) as u16;
        check_timeout!();
        if rca == 0 {
            return Err("card returned RCA zero");
        }
        self.command(CMD9_SEND_CSD, u32::from(rca) << 16, SD_RSP_R2)?;
        check_timeout!();
        let csd = self.long_response()?;
        check_timeout!();
        let total_blocks = Self::parse_csd(&csd, block_addressed)?;

        self.command(CMD7_SELECT_CARD, u32::from(rca) << 16, SD_RSP_R1B)?;
        check_timeout!();
        if !block_addressed {
            self.command(CMD16_SET_BLOCKLEN, 512, SD_RSP_R1)?;
            check_timeout!();
        }
        if self
            .app_command(rca, ACMD6_SET_BUS_WIDTH, 2, SD_RSP_R1)
            .is_ok()
        {
            check_timeout!();
            self.write_reg(SD_CFG1, 0x03, 0x01)?;
            check_timeout!();
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

    fn read_sector(&mut self, lba: u32, buffer: &mut [u8]) -> Result<(), &'static str> {
        let argument = self.card_address(lba)?;
        loop {
            match self.data_path {
                DataPath::Sdma => {
                    self.command(CMD17_READ_SINGLE, argument, SD_RSP_R1)?;
                    match self.run_data_dma(&Self::read_sector_dma_commands(), true) {
                        Ok(()) => {
                            self.data_buffer
                                .as_ref()
                                .ok_or("RTSX data buffer unavailable")?
                                .read_into(buffer);
                            return Ok(());
                        }
                        Err(error) => {
                            self.data_path = DataPath::HostPpbuf;
                            log::warn!("RTSX: {error}; falling back to batched PPBUF");
                        }
                    }
                }
                DataPath::HostPpbuf => match self.read_sector_host_ppbuf(argument, buffer) {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        self.stop_transfer();
                        self.data_path = DataPath::Pio;
                        log::warn!("RTSX: {error}; falling back to bounded PPBUF PIO");
                    }
                },
                DataPath::Pio => return self.read_sector_pio(argument, buffer),
            }
        }
    }

    fn write_sector(&mut self, lba: u32, buffer: &[u8]) -> Result<(), &'static str> {
        let rca = self.sd_card.as_ref().ok_or("no initialized card")?.rca;
        let argument = self.card_address(lba)?;
        loop {
            self.command(CMD24_WRITE_SINGLE, argument, SD_RSP_R1)?;
            match self.data_path {
                DataPath::Sdma => {
                    self.data_buffer
                        .as_mut()
                        .ok_or("RTSX data buffer unavailable")?
                        .write_from(buffer);
                    match self.run_data_dma(&Self::write_sector_dma_commands(), false) {
                        Ok(()) => break,
                        Err(error) => {
                            self.data_path = DataPath::HostPpbuf;
                            log::warn!("RTSX: {error}; falling back to batched PPBUF");
                        }
                    }
                }
                DataPath::HostPpbuf => match self.write_sector_host_ppbuf(buffer) {
                    Ok(()) => break,
                    Err(error) => {
                        self.stop_transfer();
                        self.data_path = DataPath::Pio;
                        log::warn!("RTSX: {error}; falling back to bounded PPBUF PIO");
                    }
                },
                DataPath::Pio => {
                    self.write_sector_pio(buffer)?;
                    break;
                }
            }
        }
        if crate::timing::poll_timeout_us(2_000_000, || {
            self.command(CMD13_SEND_STATUS, u32::from(rca) << 16, SD_RSP_R1)
                .ok()
                .filter(|status| status & (1 << 8) != 0)
        })
        .is_some()
        {
            return Ok(());
        }
        Err("card programming timed out")
    }

    pub fn read_sectors(
        &mut self,
        lba: u32,
        count: u16,
        buffer: &mut [u8],
    ) -> Result<(), &'static str> {
        self.prepare_device()?;
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

    pub fn write_sectors(
        &mut self,
        lba: u32,
        count: u16,
        buffer: &[u8],
    ) -> Result<(), &'static str> {
        self.prepare_device()?;
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

    if !device.prepare_mmio() {
        log::warn!("RTSX: PCI command or power transition failed");
        return;
    }
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
            // L1Sub is NOT disabled — ECAM MMIO is unsafe on bare metal.
            PciHealth::new(&device).with_upstream_bridge(bridge.bus, bridge.device, bridge.function)
        },
    );
    let mmio = unsafe { MemRegion::new(virtual_address as *mut u8, 0x1000) };
    let device_id =
        (u16::from(device.bus) << 8) | (u16::from(device.device) << 3) | u16::from(device.function);
    let allocate_dma = |size| {
        DmaRegion::alloc(context, size).and_then(|mut buffer| {
            match buffer.dma_map(context, device_id) {
                Ok(iova) if u32::try_from(iova).is_ok() => Some(buffer),
                _ => {
                    buffer.free(context);
                    None
                }
            }
        })
    };
    let host_commands = allocate_dma(HOST_COMMAND_BUFFER_SIZE);
    let data_buffer = if host_commands.is_some() {
        allocate_dma(DATA_BUFFER_SIZE)
    } else {
        None
    };
    let data_path = DataPath::preferred(host_commands.is_some(), data_buffer.is_some());
    if host_commands.is_none() {
        log::warn!("RTSX: host command DMA unavailable; falling back to slow PIO");
    } else if data_buffer.is_none() {
        log::warn!("RTSX: data DMA unavailable; falling back to PPBUF");
    }
    *CONTROLLER.lock() = Some(RtsxController {
        device,
        mmio,
        sd_card: None,
        card_was_present: false,
        health,
        host_commands,
        data_buffer,
        data_path,
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
        .as_mut()
        .ok_or("no RTS5249 controller")?
        .read_sectors(lba, count, buffer)
}

pub fn write_sectors(lba: u32, count: u16, buffer: &[u8]) -> Result<(), &'static str> {
    CONTROLLER
        .lock()
        .as_mut()
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

#[cfg(test)]
mod tests {
    use super::{
        DataPath, HostCommandKind, RtsxController, SD_TM_AUTO_READ_3, SD_TRANSFER, SD_TRANSFER_END,
        SD_TRANSFER_START,
    };

    #[test]
    fn host_command_uses_realtek_wire_format() {
        assert_eq!(
            RtsxController::host_command(HostCommandKind::Read, 0xFA00, 0, 0),
            0x3A00_0000
        );
        assert_eq!(
            RtsxController::host_command(HostCommandKind::Write, 0xFA00, 0xFF, 0x5A),
            0x7A00_FF5A
        );
        assert_eq!(
            RtsxController::host_command(
                HostCommandKind::Check,
                SD_TRANSFER,
                SD_TRANSFER_END,
                SD_TRANSFER_END,
            ),
            0xBDB3_4040
        );
    }

    #[test]
    fn sector_dma_setup_uses_ring_buffer_and_completion_check() {
        let commands = RtsxController::read_sector_dma_commands();

        assert_eq!(commands.len(), 14);
        assert_eq!(
            commands.last(),
            Some(&(
                HostCommandKind::Check,
                SD_TRANSFER,
                SD_TRANSFER_END,
                SD_TRANSFER_END,
            ))
        );
        assert_eq!(commands[12].3, SD_TRANSFER_START | SD_TM_AUTO_READ_3);
        assert_eq!(RtsxController::write_sector_dma_commands()[11].3, 0xA4);
        assert_eq!(RtsxController::data_dma_control(true), 0xA000_0200);
        assert_eq!(RtsxController::data_dma_control(false), 0x8000_0200);
    }

    #[test]
    fn data_path_prefers_bounded_acceleration() {
        assert_eq!(DataPath::preferred(true, true), DataPath::Sdma);
        assert_eq!(DataPath::preferred(true, false), DataPath::HostPpbuf);
        assert_eq!(DataPath::preferred(false, false), DataPath::Pio);
    }
}
