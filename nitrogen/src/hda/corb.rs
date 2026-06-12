//! CORB (Command Output Ring Buffer) / RIRB (Response Input Ring Buffer)
//! verb engine for HDA controllers.
//!
//! ## Register offsets (relative to MMIO base)
//! | Offset | Size | Name      |
//! |--------|------|-----------|
//! | 0x40   | 4    | CORBLBASE |
//! | 0x44   | 4    | CORBUBASE |
//! | 0x48   | 2    | CORBWP    |
//! | 0x4A   | 2    | CORBRP    |
//! | 0x4C   | 1    | CORBCTL   |
//! | 0x4D   | 1    | CORBSTS   |
//! | 0x4E   | 1    | CORBSIZE  |
//! | 0x50   | 4    | RIRBLBASE |
//! | 0x54   | 4    | RIRBUBASE |
//! | 0x58   | 2    | RIRBWP    |
//! | 0x5C   | 1    | RIRBCTL   |
//! | 0x5D   | 1    | RIRBSTS   |
//! | 0x5E   | 1    | RIRBSIZE  |

use crate::hda::DmaRegion;
use core::sync::atomic;

/// CORB/RIRB register offsets within the HDA MMIO space.
const CORBLBASE: usize = 0x0040;
const CORBUBASE: usize = 0x0044;
const CORBWP: usize = 0x0048;
const CORBRP: usize = 0x004A;
const CORBCTL: usize = 0x004C;
const RIRBLBASE: usize = 0x0050;
const RIRBUBASE: usize = 0x0054;
const RIRBWP: usize = 0x0058;
const RIRBCTL: usize = 0x005C;

/// Default CORB / RIRB entry counts.
pub const CORB_ENTRIES: usize = 256;
pub const RIRB_ENTRIES: usize = 256;

/// HDA verb identifiers.
pub mod verbs {
    // ── 4‑bit verbs (payload in lower 16 bits) ──
    pub const SET_FMT: u32 = 0x002;
    pub const SET_AMP_GAIN_MUTE: u32 = 0x003;
    pub const SET_PIN_CTL: u32 = 0x707;
    pub const SET_STREAM: u32 = 0x706;
    pub const SET_EAPD: u32 = 0x70C;
    pub const SET_CONNECTION_SELECT: u32 = 0x701;

    // ── 12‑bit verbs (payload in lower 8 bits) ──
    pub const GET_PARAM: u32 = 0xF00;
    pub const GET_CONNECTION_LIST_ENTRY: u32 = 0xF02;
    pub const GET_PIN_SENSE: u32 = 0xF09;
    pub const GET_AMP_GAIN_MUTE: u32 = 0x00B;
    pub const GET_POWER_STATE: u32 = 0xF05;
    pub const GET_CONFIG_DEFAULT: u32 = 0xF1C;
    pub const GET_SUBSYSTEM_ID: u32 = 0xF20;
    pub const GET_PIN_CTL: u32 = 0xF07;
    pub const GET_EAPD: u32 = 0xF0C;
}

/// Parameter IDs for `VERB_GET_PARAM`.
pub mod params {
    pub const VENDOR_ID: u16 = 0x00;
    pub const REVISION_ID: u16 = 0x02;
    pub const SUBORDINATE_COUNT: u16 = 0x04;
    pub const AUDIO_WIDGET_CAP: u16 = 0x09;
    pub const PCM: u16 = 0x0A;
    pub const STREAM: u16 = 0x0B;
    pub const PIN_CAP: u16 = 0x0C;
    pub const INPUT_AMP_CAP: u16 = 0x0D;
    pub const OUTPUT_AMP_CAP: u16 = 0x12;
    pub const CONNECTION_LIST_LEN: u16 = 0x0E;
    pub const POWER_STATE: u16 = 0x0F;
}

/// CORB / RIRB DMA engine.
pub struct CorbEngine {
    corb_virt: *mut u32,
    rirb_virt: *mut u64,
    corb_entries: usize,
}

impl CorbEngine {
    /// Create a new CORB engine.  `corb_entries` must be 2, 16, or 256.
    pub const fn new(corb_virt: *mut u32, rirb_virt: *mut u64, corb_entries: usize) -> Self {
        Self {
            corb_virt,
            rirb_virt,
            corb_entries,
        }
    }

    /// Initialise CORB/RIRB DMA engines on the controller.
    ///
    /// Programs the base addresses, resets write/read pointers, and
    /// enables DMA.  Returns `true` on success.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base pointer.  `corb_region` and
    /// `rirb_region` must point to valid, zeroed DMA pages.
    pub unsafe fn init(
        mmio: *mut u8,
        corb_region: &DmaRegion,
        rirb_region: &DmaRegion,
        corb_entries: usize,
    ) -> bool {
        // Determine CORBSIZE code (0 = 2, 1 = 16, 2 = 256)
        let corb_sz_code: u8 = if corb_entries >= 256 {
            2
        } else if corb_entries >= 16 {
            1
        } else {
            0
        };

        // Check GCAP 64-bit support
        let gcap = mmio_read32(mmio, 0x0000);
        let gcap64 = gcap & 1 != 0;
        let corb_phys = corb_region.phys;
        let rirb_phys = rirb_region.phys;

        if !gcap64 && ((corb_phys >> 32) != 0 || (rirb_phys >> 32) != 0) {
            log::error!(
                "HDA: physical addresses exceed 32-bit limit but controller does not support 64-bit addressing"
            );
            return false;
        }

        // Stop DMA engines (8-bit writes to avoid clobbering adjacent registers)
        mmio_write8(mmio, CORBCTL, 0);
        mmio_write8(mmio, RIRBCTL, 0);

        log::info!(
            "HDA: CORB phys=0x{:016x} RIRB phys=0x{:016x}",
            corb_phys,
            rirb_phys
        );

        // Program CORB base
        mmio_write32(mmio, CORBLBASE, corb_phys as u32);
        mmio_write32(mmio, CORBUBASE, (corb_phys >> 32) as u32);

        // Reset CORB read pointer (bit 15 = CORBRPRST)
        mmio_write16(mmio, CORBRP, 0x8000);
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        mmio_write16(mmio, CORBRP, 0);
        mmio_write16(mmio, CORBWP, 0);

        // Program CORB size first, then enable CORB DMA (CORBRUN = bit 1)
        mmio_write8(mmio, CORBCTL + 2, corb_sz_code);
        mmio_write8(mmio, CORBCTL, 0x02);

        // Program RIRB base
        mmio_write32(mmio, RIRBLBASE, rirb_phys as u32);
        mmio_write32(mmio, RIRBUBASE, (rirb_phys >> 32) as u32);

        // Reset RIRB write pointer
        mmio_write16(mmio, RIRBWP, 0x8000);
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        if mmio_read16(mmio, RIRBWP) & 0x8000 != 0 {
            mmio_write16(mmio, RIRBWP, 0);
        }

        // Program RIRB size first, then enable RIRB DMA
        mmio_write8(mmio, RIRBCTL + 2, corb_sz_code);
        mmio_write8(mmio, RIRBCTL, 0x02);

        // Log register state
        let corb_ctl = mmio_read8(mmio, CORBCTL);
        let corb_sz_rb = mmio_read8(mmio, CORBCTL + 2);
        let rirb_ctl = mmio_read8(mmio, RIRBCTL);
        let rirb_sz_rb = mmio_read8(mmio, RIRBCTL + 2);
        let corb_rp = mmio_read16(mmio, CORBRP);
        let corb_wp = mmio_read16(mmio, CORBWP);
        let rirb_wp = mmio_read16(mmio, RIRBWP);
        log::info!(
            "HDA: CORB CTL=0x{:02x} SZ={} RP=0x{:04x} WP=0x{:04x}  RIRB CTL=0x{:02x} SZ={} WP=0x{:04x}",
            corb_ctl,
            corb_sz_rb,
            corb_rp,
            corb_wp,
            rirb_ctl,
            rirb_sz_rb,
            rirb_wp
        );
        log::info!("HDA: CORB/RIRB enabled (size={} entries)", corb_entries);

        // Short settling delay for Intel PCH controllers
        for _ in 0..50000 {
            core::hint::spin_loop();
        }
        true
    }

    /// Send a verb to the codec and return the 32‑bit solicited response,
    /// or `0xFFFF_FFFF` on timeout.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base for the controller.
    /// `self` must have been initialised with valid CORB/RIRB buffers.
    pub unsafe fn send_verb(
        &self,
        mmio: *mut u8,
        codec: u8,
        node: u8,
        verb: u32,
        payload: u16,
    ) -> u32 {
        let corb = self.corb_virt;
        let rirb = self.rirb_virt;
        if corb.is_null() || rirb.is_null() {
            return 0xFFFF_FFFF;
        }
        let corb_n = self.corb_entries;

        // Encode the verb command word
        // 4‑bit verbs: Verb ID → bits [19:16], 16‑bit payload → bits [15:0]
        // 12‑bit verbs: Verb ID → bits [19:8], 8‑bit payload → bits [7:0]
        let cmd_val = if verb > 0xF {
            (verb << 8) | (payload as u32 & 0xFF)
        } else {
            (verb << 16) | (payload as u32 & 0xFFFF)
        };
        let cmd = ((codec as u32) << 28) | ((node as u32) << 20) | cmd_val;

        // Wait for space in CORB
        let corb_mask = corb_n - 1;
        let mut has_space = false;
        for _ in 0..1000 {
            let wp = mmio_read16(mmio, CORBWP) as usize & corb_mask;
            let rp = mmio_read16(mmio, CORBRP) as usize & corb_mask;
            if (wp + 1) % corb_n != rp {
                has_space = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !has_space {
            log::warn!(
                "HDA: CORB full timeout, codec={} node={:#x} verb={:#03x}",
                codec,
                node,
                verb
            );
            return 0xFFFF_FFFF;
        }

        // Ensure CORB/RIRB DMA engines are running
        let corb_sts = mmio_read8(mmio, CORBCTL + 1);
        if corb_sts & 0x01 != 0 {
            // CORBMEI — clear by writing 1 (RW1C)
            mmio_write8(mmio, CORBCTL + 1, 0x01);
            log::info!("HDA: CORBMEI cleared");
        }
        let rirb_sts = mmio_read8(mmio, RIRBCTL + 1);
        if rirb_sts & 0x01 != 0 {
            mmio_write8(mmio, RIRBCTL + 1, 0x01);
            log::info!("HDA: RIRBMEI cleared");
        }

        // Re-check CORBRUN / RIRBRUN
        let corb_ctl = mmio_read8(mmio, CORBCTL);
        if corb_ctl & 0x02 == 0 {
            let corb_sz = mmio_read8(mmio, CORBCTL + 2) & 0x03;
            mmio_write8(mmio, CORBCTL, 0x02);
            atomic::fence(atomic::Ordering::SeqCst);
            log::info!(
                "HDA: CORB restarted (CTL=0x{:02x} SZ={})",
                corb_ctl,
                corb_sz
            );
        }
        let rirb_ctl = mmio_read8(mmio, RIRBCTL);
        if rirb_ctl & 0x02 == 0 {
            let rirb_sz = mmio_read8(mmio, RIRBCTL + 2) & 0x03;
            mmio_write8(mmio, RIRBCTL, 0x02);
            atomic::fence(atomic::Ordering::SeqCst);
            log::info!(
                "HDA: RIRB restarted (CTL=0x{:02x} SZ={})",
                rirb_ctl,
                rirb_sz
            );
        }

        // Capture RIRBWP before writing CORBWP
        let rirb_mask = corb_n - 1; // RIRB uses same size as CORB
        let curr_rp = mmio_read16(mmio, RIRBWP) as usize & rirb_mask;
        let wp = mmio_read16(mmio, CORBWP) as usize & corb_mask;
        let next_wp = (wp + 1) & corb_mask;

        // Write CORB entry with fence
        core::ptr::write_volatile(corb.add(next_wp), cmd);
        atomic::fence(atomic::Ordering::SeqCst);
        mmio_write16(mmio, CORBWP, next_wp as u16);

        // Walk RIRB entries incrementally (RIRB size matches CORB size)
        let rirb_n: usize = corb_n;
        let mut rp = curr_rp;
        for _iter in 0..100_000 {
            let rirb_wp = mmio_read16(mmio, RIRBWP) as usize & rirb_mask;
            while rp != rirb_wp {
                rp = (rp + 1) & rirb_mask;
                let resp = core::ptr::read_volatile(rirb.add(rp));
                let unsol = (resp >> 63) & 1;
                if unsol == 0 {
                    let raw = (resp >> 32) as u32;
                    // Verbose log for GET_PARAM
                    if verb == verbs::GET_PARAM && payload <= 0x12 {
                        log::info!(
                            "HDA: verb OK c={} n={:#x} v={:#03x} → raw=0x{:08x}",
                            codec,
                            node,
                            verb,
                            raw
                        );
                    }
                    return raw;
                }
            }
            core::hint::spin_loop();
        }

        // Timeout — dump diagnostic regs
        let corb_ctl32 = mmio_read32(mmio, CORBCTL);
        let rirb_ctl32 = mmio_read32(mmio, RIRBCTL);
        let corb_wp = mmio_read16(mmio, CORBWP);
        let corb_rp = mmio_read16(mmio, CORBRP);
        let rirb_wp = mmio_read16(mmio, RIRBWP);
        log::warn!(
            "HDA: verb timeout c={} n={:#x} v={:#03x} p={:#x}",
            codec,
            node,
            verb,
            payload
        );
        log::warn!(
            "HDA:  CORB CTL=0x{:08x} WP=0x{:04x} RP=0x{:04x}  RIRB CTL=0x{:08x} WP=0x{:04x}",
            corb_ctl32,
            corb_wp,
            corb_rp,
            rirb_ctl32,
            rirb_wp
        );
        0xFFFF_FFFF
    }
}

// ── MMIO access helpers ──────────────────────────────────────────

#[inline]
unsafe fn mmio_read32(mmio: *mut u8, offset: usize) -> u32 {
    core::ptr::read_volatile(mmio.add(offset) as *const u32)
}

#[inline]
unsafe fn mmio_read16(mmio: *mut u8, offset: usize) -> u16 {
    core::ptr::read_volatile(mmio.add(offset) as *const u16)
}

#[inline]
unsafe fn mmio_read8(mmio: *mut u8, offset: usize) -> u8 {
    core::ptr::read_volatile(mmio.add(offset))
}

#[inline]
unsafe fn mmio_write32(mmio: *mut u8, offset: usize, val: u32) {
    core::ptr::write_volatile(mmio.add(offset) as *mut u32, val);
}

#[inline]
unsafe fn mmio_write16(mmio: *mut u8, offset: usize, val: u16) {
    core::ptr::write_volatile(mmio.add(offset) as *mut u16, val);
}

#[inline]
unsafe fn mmio_write8(mmio: *mut u8, offset: usize, val: u8) {
    core::ptr::write_volatile(mmio.add(offset), val);
}
