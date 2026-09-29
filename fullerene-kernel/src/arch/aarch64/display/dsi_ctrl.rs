//! DSI controller (`qcom,dsi-ctrl-hw-v2.4`) - port of the vendor driver's
//! command-mode path for Lito.
//!
//! Source: `tmp/display-src/vq_dsi_host.c` (bramble 4.19 qpr1,
//! `dsi_ctrl_config` `:823-924`, `dsi_timing_setup` `:926-991`,
//! `dsi_cmd_dma_add` `:1151`, `dsi_cmd_dma_tx` `:1266`). Register offsets and
//! bitfields come from the vendor's committed `dsi.xml.h`; see
//! `docs/DISPLAY_REGISTERS.md`.

/// DSI controller 0 window (vendor DT `lito-sde.dtsi:445`).
pub const DSI_CTRL_BASE: usize = 0x0ae9_4000;

/// Register offsets (`dsi.xml.h`).
pub mod reg {
    pub const CTRL: usize = 0x000;
    pub const CMD_DMA_CTRL: usize = 0x038;
    pub const CMD_CFG0: usize = 0x03c;
    pub const CMD_CFG1: usize = 0x040;
    pub const DMA_BASE: usize = 0x044;
    pub const DMA_LEN: usize = 0x048;
    pub const CMD_MDP_STREAM_CTRL: usize = 0x054;
    pub const CMD_MDP_STREAM_TOTAL: usize = 0x058;
    pub const TRIG_CTRL: usize = 0x080;
    pub const TRIG_DMA: usize = 0x08c;
    pub const LANE_CTRL: usize = 0x0a8;
    pub const LANE_SWAP_CTRL: usize = 0x0ac;
    pub const CLKOUT_TIMING_CTRL: usize = 0x0c0;
    pub const EOT_PACKET_CTRL: usize = 0x0c8;
    pub const ERR_INT_MASK0: usize = 0x108;
    pub const INTR_CTRL: usize = 0x10c;
    pub const RESET: usize = 0x114;
    pub const CLK_CTRL: usize = 0x118;
    pub const PHY_RESET: usize = 0x128;
}

/// Bitfields (`dsi.xml.h`).
pub mod bits {
    pub const CTRL_ENABLE: u32 = 0x0000_0001;
    /// `DSI_CTRL_CMD_MODE_EN` (`dsi.xml.h:133`) - without this the controller is
    /// never placed in command mode and will not accept the panel's DCS traffic.
    pub const CTRL_CMD_MODE_EN: u32 = 0x0000_0004;
    pub const CTRL_LANE0: u32 = 0x0000_0010;
    pub const CLK_CTRL_ENABLE_CLKS: u32 = 0x0000_003f;
    pub const CMD_DMA_CTRL_LOW_POWER: u32 = 0x0400_0000;
    pub const CMD_DMA_CTRL_FROM_FRAME_BUFFER: u32 = 0x1000_0000;
    pub const TRIG_CTRL_TE: u32 = 0x8000_0000;
    pub const TRIG_CTRL_BLOCK_DMA_WITHIN_FRAME: u32 = 0x0000_1000;
    pub const CMD_CFG1_INSERT_DCS_COMMAND: u32 = 0x0001_0000;
    pub const EOT_PACKET_CTRL_TX_EOT_APPEND: u32 = 0x0000_0001;
    pub const LANE_CTRL_CLKLN_HS_FORCE_REQUEST: u32 = 0x1000_0000;
    /// `DSI_TRIG_CTRL_DMA_TRIGGER(TRIGGER_SW)` - trigger mode 1.
    pub const DMA_TRIGGER_SW: u32 = 1;
}

/// MIPI DCS commands used by the panel path.
pub const DCS_WRITE_MEMORY_START: u32 = 0x2c;
pub const DCS_WRITE_MEMORY_CONTINUE: u32 = 0x3c;
/// `MIPI_DSI_DCS_LONG_WRITE`.
pub const DSI_DCS_LONG_WRITE: u32 = 0x39;

/// Parameters for the controller's command-mode configuration.
#[derive(Debug, Clone, Copy)]
pub struct CtrlConfig {
    /// DSI lanes in use (`sofef01` uses 4).
    pub lanes: u32,
    /// `clk_post` from the PHY timing calculation.
    pub clk_post: u32,
    /// `clk_pre` from the PHY timing calculation.
    pub clk_pre: u32,
    /// Data-lane swap select (`dlane_swap`), 0 for the default mapping.
    pub dlane_swap: u32,
    /// True when the clock lane runs continuously.
    pub continuous_clock: bool,
    /// True when the panel expects EOT packets.
    pub eot_packet: bool,
}

/// One command ready to be packed for the MSM command engine. Mirrors the DT's
/// packed descriptor (`type last vc ack wait dlen payload`).
#[derive(Debug, Clone, Copy)]
pub struct Packet {
    /// MIPI DSI data type (0x05 short write 0-param, 0x15 short write 1-param,
    /// 0x39 long write).
    pub dtype: u8,
    pub vc: u8,
    pub payload: &'static [u8],
}

impl Packet {
    /// Long-packet types carry a word count and a payload.
    pub const fn is_long(&self) -> bool {
        self.dtype == 0x39
    }

    /// Build the MSM in-memory command format into `out`
    /// (`dsi_cmd_dma_add`, `vq_dsi_host.c:1179-1195`). Returns the transfer
    /// length, a multiple of 4, or `None` if `out` is too small.
    ///
    /// Layout: `[0]=header1 [1]=header2 [2]=header0 [3]=flags payload... 0xff pad`,
    /// where for a short packet `header1`/`header2` are data0/data1 and for a long
    /// packet they are the word count low/high.
    pub fn build(&self, out: &mut [u8]) -> Option<usize> {
        let (h0, h1, h2, size) = if self.is_long() {
            let wc = self.payload.len() as u16;
            (
                (self.dtype as u16) | ((self.vc as u16) << 6),
                (wc & 0xff) as u8,
                (wc >> 8) as u8,
                4 + self.payload.len() + 2, // header + payload + checksum
            )
        } else {
            let d0 = self.payload.first().copied().unwrap_or(0);
            let d1 = self.payload.get(1).copied().unwrap_or(0);
            ((self.dtype as u16) | ((self.vc as u16) << 6), d0, d1, 4)
        };
        let len = (size + 3) & !0x3;
        if out.len() < len {
            return None;
        }
        out[0] = h1;
        out[1] = h2;
        out[2] = h0 as u8;
        out[3] = 1 << 7; // last packet
        if self.is_long() {
            out[3] |= 1 << 6;
        }
        let n = self.payload.len();
        if n > 0 {
            out[4..4 + n].copy_from_slice(self.payload);
        }
        for b in out[size..len].iter_mut() {
            *b = 0xff;
        }
        Some(len)
    }
}

#[cfg(target_arch = "aarch64")]
pub mod hw {
    use super::{CtrlConfig, DCS_WRITE_MEMORY_CONTINUE, DCS_WRITE_MEMORY_START, Packet, bits, reg};

    /// Command/DMA staging buffer. Identity-mapped, so its address is the
    /// physical address the DSI DMA engine needs. 16 KiB keeps a full-screen
    /// pixel push to a few hundred transfers instead of thousands.
    const TX_BUF_LEN: usize = 16384;
    #[repr(align(64))]
    struct TxBuf([u8; TX_BUF_LEN]);
    static mut TX_BUF: TxBuf = TxBuf([0; TX_BUF_LEN]);

    #[inline]
    fn wr(off: usize, value: u32) {
        unsafe { core::ptr::write_volatile((super::DSI_CTRL_BASE + off) as *mut u32, value) };
    }

    #[inline]
    fn rd(off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((super::DSI_CTRL_BASE + off) as *const u32) }
    }

    /// Read back the controller's `CTRL` register, for the host-side self-report.
    pub fn read_ctrl() -> u32 {
        rd(reg::CTRL)
    }

    /// Read back `CLK_CTRL`, for the host-side self-report.
    pub fn read_clk_ctrl() -> u32 {
        rd(reg::CLK_CTRL)
    }

    /// `dsi_sw_reset()` (`vq_dsi_host.c:993-1001`): clocks on, assert the reset,
    /// sleep `DSI_RESET_TOGGLE_DELAY_MS = 20`, deassert.
    ///
    /// The vendor *sleeps* rather than polling the reset bit, so this port does the
    /// same - polling a self-clearing toggle is not what the driver does.
    pub fn sw_reset() {
        wr(reg::CLK_CTRL, bits::CLK_CTRL_ENABLE_CLKS);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        wr(reg::RESET, 1);
        crate::timer::delay_us(20_000);
        wr(reg::RESET, 0);
    }

    /// `dsi_ctrl_config()` (`vq_dsi_host.c:823-924`), command-mode branch.
    pub fn ctrl_config(cfg: &CtrlConfig) {
        // CMD_CFG0: DST_FORMAT for RGB888 is 0 (RGB888 is the native format),
        // no RGB swap.
        wr(reg::CMD_CFG0, 0);
        // CMD_CFG1: write-memory start/continue plus DCS insertion.
        let cfg1 = (DCS_WRITE_MEMORY_START & 0xff)
            | ((DCS_WRITE_MEMORY_CONTINUE & 0xff) << 8)
            | bits::CMD_CFG1_INSERT_DCS_COMMAND;
        wr(reg::CMD_CFG1, cfg1);

        wr(
            reg::CMD_DMA_CTRL,
            bits::CMD_DMA_CTRL_FROM_FRAME_BUFFER | bits::CMD_DMA_CTRL_LOW_POWER,
        );

        // TRIG_CTRL: dedicated TE pin, software DMA trigger, 6G >= v1.2 blocks
        // the DMA within the frame.
        let trig = bits::TRIG_CTRL_TE
            | (bits::DMA_TRIGGER_SW & 0x7)
            | bits::TRIG_CTRL_BLOCK_DMA_WITHIN_FRAME;
        wr(reg::TRIG_CTRL, trig);

        // CLKOUT_TIMING_CTRL takes the PHY's shared timings.
        let clkout = (cfg.clk_pre & 0x3f) | ((cfg.clk_post & 0x3f) << 8);
        wr(reg::CLKOUT_TIMING_CTRL, clkout);

        let eot = if cfg.eot_packet {
            0
        } else {
            bits::EOT_PACKET_CTRL_TX_EOT_APPEND
        };
        wr(reg::EOT_PACKET_CTRL, eot);

        // Only ack-err-status generates interrupts.
        wr(reg::ERR_INT_MASK0, 0x13ff_3fe0);

        wr(reg::CLK_CTRL, bits::CLK_CTRL_ENABLE_CLKS);

        let mut ctrl = bits::CTRL_ENABLE & 0; // CLK_EN comes from the lane field below
        ctrl |= 1 << 4; // DSI_CTRL_CLK_EN
        ctrl |= (bits::CTRL_LANE0 << cfg.lanes) - bits::CTRL_LANE0;
        wr(reg::LANE_SWAP_CTRL, cfg.dlane_swap & 0x7);
        if cfg.continuous_clock {
            wr(reg::LANE_CTRL, bits::LANE_CTRL_CLKLN_HS_FORCE_REQUEST);
        }
        // `dsi_op_mode_config(video_mode=false, enable=true)` (`vq_dsi_host.c:1003-1026`):
        // command mode must be enabled explicitly, and it also enables the
        // CMD_MDP_DONE interrupt. Without CMD_MODE_EN the controller will not accept
        // the panel's DCS traffic at all.
        ctrl |= bits::CTRL_CMD_MODE_EN;
        ctrl |= bits::CTRL_ENABLE;
        wr(reg::CTRL, ctrl);
        // DSI_IRQ_MASK_CMD_MDP_DONE (0x200, `dsi.xml.h`) enabled in INTR_CTRL.
        wr(reg::INTR_CTRL, 0x200);
    }

    /// `dsi_timing_setup()` command-mode branch (`vq_dsi_host.c:976-990`).
    pub fn timing_setup(hdisplay: u32, vdisplay: u32, bpp: u32) {
        let wc = hdisplay * bpp / 8 + 1;
        wr(
            reg::CMD_MDP_STREAM_CTRL,
            ((wc & 0xffff) << 16) | super::DSI_DCS_LONG_WRITE,
        );
        wr(
            reg::CMD_MDP_STREAM_TOTAL,
            (hdisplay & 0xfff) | ((vdisplay & 0xfff) << 16),
        );
    }

    /// `dsi_cmd_dma_tx()` (`vq_dsi_host.c:1266`): point the DMA at the staging
    /// buffer and trigger a software transfer.
    pub fn cmd_tx(packet: &Packet) -> bool {
        cmd_tx_raw(packet.dtype, packet.vc, packet.payload)
    }

    /// DCS/DSI *read* path with an explicit response length.
    ///
    /// Ported from the vendor host driver: `vq_dsi_host.c:2049`
    /// (`msm_dsi_host_cmd_rx`) and `:1299` (`dsi_cmd_dma_rx`). The vendor branches on
    /// the requested length (`:2060`): for `rlen <= 2` it sends `pkt_size = rlen`
    /// (a *short* read), and only for longer reads does it use a fixed
    /// `data_byte = 10`. Sending a fixed 10 for a one-byte DCS read - which is what
    /// this port did first - makes the panel wait to fill a longer response, and the
    /// read-back registers come back empty.
    ///
    /// `None` means the transfer was not accepted. `Some(0)` or `Some(0xffff_ffff)`
    /// means the panel returned nothing.
    pub fn cmd_rx_len(dcs_command: u8, rlen: u8) -> Option<u32> {
        const RDBK_DATA: usize = 0x68;
        const RDBK_DATA_CTRL: usize = 0x1d0;
        const RDBK_DATA_CTRL_CLR: u32 = 0x0000_0001;

        // 1. Maximum return packet size: the *requested* length for a short read
        //    (`vq_dsi_host.c:2062`), otherwise the vendor's chunk size.
        let pkt_size: u16 = if rlen <= 2 { rlen as u16 } else { 10 };
        if !cmd_tx_raw(0x37, 0, &[pkt_size as u8, (pkt_size >> 8) as u8]) {
            return None;
        }
        // 2. Clear the read-back registers.
        let base = super::DSI_CTRL_BASE as *mut u32;
        unsafe {
            core::ptr::write_volatile(base.add(RDBK_DATA_CTRL / 4), RDBK_DATA_CTRL_CLR);
            core::ptr::write_volatile(base.add(RDBK_DATA_CTRL / 4), 0);
        }
        // 3. The read command itself.
        if !cmd_tx_raw(0x06, 0, &[dcs_command]) {
            return None;
        }
        // 4./5. Completion has already been waited for; collect the response.
        let word = rd(RDBK_DATA);
        Some(word.swap_bytes())
    }

    /// Convenience wrapper: a one-byte DCS read (the DDB bytes and most read status
    /// commands are one byte each).
    pub fn cmd_rx(dcs_command: u8) -> Option<u32> {
        cmd_rx_len(dcs_command, 1)
    }

    /// Clean the data cache for `[address, address + length)` to the point of
    /// coherency, then a full `dsb sy`.
    ///
    /// The DSI controller fetches command and pixel data by *physical* address, so
    /// anything still sitting dirty in the CPU cache is invisible to it - it reads
    /// whatever stale bytes happen to be in DRAM. This is the exact failure mode the
    /// display workstream already recorded for the framebuffer
    /// (`docs/CONTEXT_STATUS.md` entries 186-187: "the panel reads physical DRAM, so
    /// cached pixels would never show"). Linux gets this from the DMA API
    /// (`dma_sync_single_for_device`); this port writes the packet with plain stores,
    /// so it has to do the clean itself.
    ///
    /// Same shape as `usb::cache_clean`, which is private to that module.
    unsafe fn clean_to_dram(address: usize, length: usize) {
        const LINE: usize = 64;
        let start = address & !(LINE - 1);
        let end = address.saturating_add(length).saturating_add(LINE - 1) & !(LINE - 1);
        let mut line = start;
        while line < end {
            unsafe {
                core::arch::asm!("dc cvac, {a}", a = in(reg) line, options(nostack));
            }
            line += LINE;
        }
        unsafe {
            core::arch::asm!("dsb sy", options(nostack));
        }
    }

    /// Same as `cmd_tx` but from a borrowed payload, so callers can stream pixel
    /// data without leaking a buffer.
    pub fn cmd_tx_raw(dtype: u8, vc: u8, payload: &[u8]) -> bool {
        let is_long = dtype == 0x39;
        let len = unsafe {
            let buf = core::ptr::addr_of_mut!(TX_BUF.0) as *mut u8;
            let slice = core::slice::from_raw_parts_mut(buf, TX_BUF_LEN);
            let h0 = (dtype as u16) | ((vc as u16) << 6);
            let (h1, h2, size) = if is_long {
                let wc = payload.len() as u16;
                ((wc & 0xff) as u8, (wc >> 8) as u8, 4 + payload.len() + 2)
            } else {
                (
                    payload.first().copied().unwrap_or(0),
                    payload.get(1).copied().unwrap_or(0),
                    4,
                )
            };
            let len = (size + 3) & !0x3;
            if slice.len() < len {
                return false;
            }
            slice[0] = h1;
            slice[1] = h2;
            slice[2] = h0 as u8;
            slice[3] = 1 << 7;
            if is_long {
                slice[3] |= 1 << 6;
                slice[4..4 + payload.len()].copy_from_slice(payload);
            }
            for b in slice[size..len].iter_mut() {
                *b = 0xff;
            }
            len
        };
        let addr = unsafe { core::ptr::addr_of!(TX_BUF.0) as usize };
        wr(reg::DMA_BASE, addr as u32);
        wr(reg::DMA_LEN, len as u32);
        // The controller reads this buffer by physical address; the packet was just
        // written with plain stores and may still be dirty in cache. Clean it out to
        // DRAM before the engine is told to fetch it.
        unsafe { clean_to_dram(addr, len) };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        wr(reg::TRIG_DMA, 1);
        // The command engine clears TRIG_DMA when the transfer is accepted. Report
        // the truth: a timeout here means the engine never took the transfer, which
        // is exactly the bisection question this has to answer.
        for _ in 0..100 {
            if rd(reg::TRIG_DMA) & 1 == 0 {
                return true;
            }
            crate::timer::delay_us(100);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_packet_matches_driver_layout() {
        // DT: `05 01 00 00 00 00 01 29` -> DCS 0x29, no parameter.
        let p = Packet {
            dtype: 0x05,
            vc: 0,
            payload: &[0x29],
        };
        let mut buf = [0u8; 16];
        let len = p.build(&mut buf).expect("fits");
        assert_eq!(len, 4);
        assert_eq!(&buf[..4], &[0x29, 0x00, 0x05, 0x80]);
    }

    #[test]
    fn long_packet_carries_word_count_and_pads() {
        // DT: `39 01 00 00 00 00 03 F0 5A 5A` -> long write, 3 bytes.
        let p = Packet {
            dtype: 0x39,
            vc: 0,
            payload: &[0xF0, 0x5A, 0x5A],
        };
        let mut buf = [0xffu8; 16];
        let len = p.build(&mut buf).expect("fits");
        // size = 4 + 3 + 2 = 9, padded to 12.
        assert_eq!(len, 12);
        assert_eq!(&buf[..7], &[0x03, 0x00, 0x39, 0xc0, 0xF0, 0x5A, 0x5A]);
        assert_eq!(&buf[9..12], &[0xff, 0xff, 0xff]);
    }
}
