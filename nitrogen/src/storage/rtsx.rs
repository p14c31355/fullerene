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
const RTSX_CFG_RESET: u8 = 0x01;

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
    /// Upstream PCIe bridge coordinates (bus, dev, func).  When set,
    /// we re-assert D0/ASPM disable on the bridge before each MMIO
    /// session to avoid hangs caused by L1 substate transitions.
    upstream_bridge: Option<(u8, u8, u8)>,
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

    /// Check device accessibility via config space (port I/O, safe).
    /// Returns `Err` with a reason if the device cannot be safely accessed
    /// via MMIO.  Checks:
    ///
    /// 1. Vendor ID sanity (device must be on the bus).
    /// 2. Power Management state (must be D0 — D3hot/D3cold reads hang).
    /// 3. PCIe link status (Negotiated Speed must be non-zero).
    fn ensure_device_accessible(&self) -> Result<(), &'static str> {
        let bus = self.device.bus;
        let dev = self.device.device;
        let func = self.device.function;

        // 1. Vendor check
        let vendor = crate::pci::PciConfigSpace::read_config_word(bus, dev, func, 0x00);
        if vendor == 0xFFFF || vendor == 0x0000 || vendor != self.device.vendor_id {
            log::warn!("RTSX: device not on PCI bus (vendor={:#06x})", vendor);
            return Err("device off PCI bus");
        }

        // 2. Walk capabilities list to find PM cap (0x01) and PCIe cap (0x10).
        let cap_ptr = crate::pci::PciConfigSpace::read_config_byte(bus, dev, func, 0x34);
        if cap_ptr == 0 {
            log::warn!("RTSX: no PCI capabilities list");
            return Err("no capabilities");
        }
        let mut off = cap_ptr;
        let mut found_pm = false;
        let mut found_pcie = false;
        for _ in 0..48 {
            // Tighten bounds check: off + 0x12 must not overflow config space (256 bytes).
            // PCIe capability reads at off+0x12, so off must be <= 0xED (256 - 19).
            if off < 0x40 || off > 0xED {
                break;
            }
            let cap_id = crate::pci::PciConfigSpace::read_config_byte(bus, dev, func, off);

            match cap_id {
                0x01 => {
                    // Power Management capability
                    found_pm = true;
                    // PMCSR at cap_offset + 4, bits 1:0 = power state.
                    let pmcsr = crate::pci::PciConfigSpace::read_config_word(
                        bus, dev, func, off + 4);
                    let pstate = pmcsr & 0x3;
                    if pstate != 0 {
                        log::warn!("RTSX: device not in D0 (state={})", pstate);
                        return Err("device not in D0");
                    }
                    log::info!("RTSX: power state D0 confirmed");
                }
                0x10 => {
                    // PCI Express capability
                    found_pcie = true;
                    let lnk_sts = crate::pci::PciConfigSpace::read_config_word(
                        bus, dev, func, off + 0x12);
                    // Negotiated Link Speed in bits 15:12; zero = link down.
                    let speed = (lnk_sts >> 12) & 0xF;
                    if speed == 0 {
                        log::warn!("RTSX: PCIe link down (lnk_sts={:#06x})", lnk_sts);
                        return Err("PCIe link down");
                    }
                    log::info!("RTSX: PCIe link up (speed={})", speed);
                }
                _ => {}
            }

            if found_pm && found_pcie {
                return Ok(());
            }

            let next = crate::pci::PciConfigSpace::read_config_byte(
                bus, dev, func, off + 1);
            // Self-loop check: some broken hardware can return the same
            // offset as the next pointer.
            if next == 0 || next == off {
                break;
            }
            off = next;
        }

        if !found_pm {
            log::warn!("RTSX: Power Management capability not found");
            return Err("PM cap not found");
        }
        if !found_pcie {
            log::warn!("RTSX: PCIe capability not found");
            return Err("PCIe cap not found");
        }
        Ok(())
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
        // Before any MMIO access, verify the device is alive via PCI config
        // space (port I/O, never hangs).  If the device is in D3cold or the
        // PCIe link is down, this is our last safe bail-out point before
        // touching MMIO registers that could hang the bus.
        let vendor = crate::pci::PciConfigSpace::read_config_word(
            self.device.bus, self.device.device, self.device.function, 0x00);
        if vendor != self.device.vendor_id || vendor == 0xFFFF || vendor == 0x0000 {
            log::warn!("RTSX: device not on PCI bus (vendor={:#06x})", vendor);
            return false;
        }

        // First MMIO access must be a WRITE (posted) — PCIe reads
        // (non-posted) can hang if the link is in an unstable state.
        // We have already verified the device exists via PCI config space
        // and re-asserted D0.  Skip any MMIO reads during init_hardware;
        // register writes are posted and cannot hang the bus.
        log::info!("RTSX: first MMIO write at {:p}+0x1C", self.mmio);
        self.w16(RTSX_MSI_EN, 0x0000);

        // Wait for PCIe link to wake (L1→L0 transition).
        for _ in 0..2000 {
            core::hint::spin_loop();
        }

        // Send several additional posted writes to flush the host bridge's
        // posted-write buffer and ensure the PCIe link has fully exited L1.
        // Some Realtek controllers need extra wake-up time before the first
        // non-posted read; without this, sd_bus_stat reads can hang on
        // cold-boot when ASPM put the link into L1 substate.
        self.w8(CARD_CLK_EN, 0x00);
        self.w8(CARD_CLK_EN, 0x00);
        for _ in 0..50_000 {
            core::hint::spin_loop();
        }

        // Perform soft-reset (RTSX_CFG bit 0).  This resets the internal
        // chip state machine to a known state.  Without it, the controller
        // may accept MMIO writes but never respond to SD commands, causing
        // the first non-posted MMIO read to hang the CPU indefinitely.
        // The Linux rtsx_pci driver performs this reset unconditionally.
        log::info!("RTSX: soft-reset");
        self.w8(RTSX_CFG, RTSX_CFG_RESET);
        // Wait for the internal chip reset to complete.  The chip typically
        // takes ~100-200 microseconds; we use a generous spin loop.
        for _ in 0..200_000 {
            core::hint::spin_loop();
        }
        // Clear the reset bit manually.  The auto-clear may not be reliable
        // on all revisions; writing 0x00 explicitly matches the Linux
        // rtsx_pci driver behaviour.  We do NOT read back here — reading
        // while the chip is still resetting can cause an undefined response
        // and potentially hang the CPU.
        self.w8(RTSX_CFG, 0x00);
        for _ in 0..50_000 {
            core::hint::spin_loop();
        }

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

    fn mmio_alive(&self) -> bool {
        self.r8(SD_CMD_STATE) != 0xFF
    }

    fn sd_cmd(&self, cmd: u8, arg: u32, rsp_type: u8, data_len: u16) -> Result<u32, &'static str> {
        let mut ready = false;
        for i in 0..50_000 {
            let state = self.r8(SD_CMD_STATE);
            if state == 0xFF {
                if i >= 1_000 {
                    return Err("SD cmd: controller not responding");
                }
            } else if (state & 0x01) != 0 {
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
            let stat1 = self.r8(SD_STAT1);
            if stat1 == 0xFF && !self.mmio_alive() {
                return Err("SD cmd: controller vanished during transfer");
            }
            if (stat1 & SD_TRANSFER_DONE) != 0 {
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
        let r1 = self.sd_cmd(CMD55_APP_CMD, 0, SD_RSP_TYPE_R1, 0)?;
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

        // Verify device is alive via PCI config space (port I/O, never hangs).
        // Read vendor ID at config offset 0x00.
        let vendor = crate::pci::PciConfigSpace::read_config_word(
            self.device.bus, self.device.device, self.device.function, 0x00);
        if vendor != self.device.vendor_id {
            return Err("controller disappeared from PCI bus");
        }

        // Re-assert D0 and re-enable memory access before any MMIO.
        // The PCIe link may have entered a lower power state since boot;
        // re-programming the device's config space wakes it up.
        self.device.ensure_d0();
        self.device.enable_memory_access();

        // Re-disable ASPM on the device and the upstream bridge.  The
        // PCIe link may have entered L1 substate since boot; if the link
        // is in L1 when we issue the first non-posted MMIO read, the host
        // will wait for the link to wake up — and on some Realtek
        // controllers with buggy ASPM, this wait can hang the CPU
        // indefinitely.  Disabling ASPM forces the link back to L0.
        self.device.disable_pcie_aspm();
        if let Some((b, d, f)) = self.upstream_bridge {
            let bridge = PciDevice::new(b, d, f);
            if let Some(bridge) = bridge {
                log::info!("RTSX: re-disabling ASPM on upstream bridge {:02x}:{:02x}.{}", b, d, f);
                bridge.disable_pcie_aspm();
            }
        }

        // Verify the PCIe link is up *before* any MMIO access.  The very
        // first MMIO write (even though it's "posted") will block on
        // x86_64 when the memory type is Uncached, so if the link is in
        // L1 substate or D3cold, the write will hang the CPU indefinitely.
        // Checking the Negotiated Link Speed via PCI config space (port I/O)
        // is safe because it goes through the host bridge without needing
        // the downstream device to respond.
        match self.ensure_device_accessible() {
            Ok(()) => {}
            Err(e) => {
                log::warn!("RTSX: device not accessible before MMIO: {}", e);
                return Err(e);
            }
        }
        log::info!("RTSX: PCIe link confirmed up, proceeding with MMIO");

        if !self.init_hardware() {
            return Err("hardware init failed");
        }

        // After init_hardware() completes, the device should be responsive.
        // The link has already been verified before the first MMIO write,
        // and the posted writes have had a chance to settle.  We don't need
        // a second accessibility check here — the ensure_device_accessible()
        // call above is sufficient.

        for _ in 0..200_000 {
            core::hint::spin_loop();
        }

        // Re-check the vendor ID via PCI config space immediately before
        // the first non-posted MMIO read.  If the link dropped between the
        // accessibility check above and this point, the upcoming MMIO read
        // would hang the CPU indefinitely.  Bailing out here keeps the
        // system responsive instead of hanging.
        let vendor_now = crate::pci::PciConfigSpace::read_config_word(
            self.device.bus, self.device.device, self.device.function, 0x00);
        if vendor_now != self.device.vendor_id || vendor_now == 0xFFFF || vendor_now == 0 {
            return Err("device vanished from PCI bus before MMIO read");
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
        for _ in 0..200 {
            if !self.mmio_alive() {
                return Err("ACMD41: controller not responding");
            }
            if let Ok(rsp) = self.sd_acmd(ACMD41_SEND_OP_COND, ocr_arg, SD_RSP_TYPE_R3) {
                if (rsp & (1 << 31)) != 0 {
                    ocr = rsp;
                    ok = true;
                    break;
                }
            }
            for _ in 0..1_000 {
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

        // sd_cmd already waited for TRANSFER_DONE — data is in PPBUF.
        // Do NOT wait for DATA_DONE separately; some RTSX revisions never
        // set it for reads, causing a 500K-spin hang.
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

            // Configure the upstream bridge's 32-bit memory window to cover BAR0.
            // Expand the existing window (using min/max) rather than overwriting,
            // so other devices behind the bridge are not broken.
            // The 32-bit window at config offset 0x20 cannot address above 4GB.
            if let Some(ref bridge) = upstream_bridge {
                if bar0_addr + bar0_size as u64 <= 0x1_0000_0000 {
                    let base_reg = PciConfigSpace::read_config_dword(
                        bridge.bus, bridge.device, bridge.function, 0x20);
                    let mem_base = base_reg as u16;
                    let mem_limit = (base_reg >> 16) as u16;
                    let bar_top = bar0_addr + bar0_size as u64 - 1;
                    let need_base = ((bar0_addr >> 16) & 0xFFF0) as u16;
                    let need_limit = ((bar_top >> 16) & 0xFFF0) as u16;

                    let window_enabled = mem_base <= mem_limit;
                    let already_covered = window_enabled && mem_base <= need_base && mem_limit >= need_limit;
                    if !already_covered {
                        let new_base = if window_enabled { mem_base.min(need_base) } else { need_base };
                        let new_limit = if window_enabled { mem_limit.max(need_limit) } else { need_limit };
                        log::info!("RTSX: bridge window {:#06x}-{:#06x} expanded to {:#06x}-{:#06x}",
                            mem_base, mem_limit, new_base, new_limit);
                        let new_win = (new_limit as u32) << 16 | new_base as u32;
                        PciConfigSpace::write_config_dword_raw(
                            bridge.bus, bridge.device, bridge.function, 0x20, new_win);
                    }
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
                upstream_bridge: upstream_bridge.map(|b| (b.bus, b.device, b.function)),
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
