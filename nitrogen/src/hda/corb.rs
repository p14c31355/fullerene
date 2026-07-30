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
const RINTCNT: usize = 0x005A;
const RIRBCTL: usize = 0x005C;
const RIRBSTS: usize = 0x005D;

/// Default CORB / RIRB entry counts.
pub const CORB_ENTRIES: usize = 256;
pub const RIRB_ENTRIES: usize = 256;

/// HDA verb identifiers.
pub mod verbs {
    // ── 4‑bit verbs (payload in lower 16 bits) ──
    pub const SET_FMT: u32 = 0x002;
    pub const SET_AMP_GAIN_MUTE: u32 = 0x003;
    pub const SET_CVT_CHAN_COUNT: u32 = 0x72D;
    pub const SET_PIN_CTL: u32 = 0x707;
    pub const SET_STREAM: u32 = 0x706;
    pub const SET_EAPD: u32 = 0x70C;
    pub const SET_CONNECTION_SELECT: u32 = 0x701;
    pub const SET_PROC_COEF: u32 = 0x400;
    pub const SET_COEF_INDEX: u32 = 0x500;

    // ── 12‑bit verbs (normally payload in lower 8 bits) ──
    pub const GET_PARAM: u32 = 0xF00;
    pub const GET_CONNECTION_LIST_ENTRY: u32 = 0xF02;
    pub const GET_PIN_SENSE: u32 = 0xF09;
    pub const GET_AMP_GAIN_MUTE: u32 = 0x00B;
    pub const GET_POWER_STATE: u32 = 0xF05;
    pub const GET_CONFIG_DEFAULT: u32 = 0xF1C;
    pub const GET_SUBSYSTEM_ID: u32 = 0xF20;
    pub const GET_PIN_CTL: u32 = 0xF07;
    pub const GET_EAPD: u32 = 0xF0C;
    pub const GET_PROC_COEF: u32 = 0xD00;
    pub const SET_POWER_STATE: u32 = 0x705;
}

/// Parameter IDs for `VERB_GET_PARAM`.
pub mod params {
    pub const VENDOR_ID: u16 = 0x00;
    pub const REVISION_ID: u16 = 0x02;
    pub const SUBORDINATE_COUNT: u16 = 0x04;
    pub const FUNCTION_GROUP_TYPE: u16 = 0x05;
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
        unsafe {
            // Determine CORBSIZE code (0 = 2, 1 = 16, 2 = 256).
            // CORBSIZE[1:0] is the selected size; CORBSZCAP[7:4] is
            // read-only capability information.
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
            // Short settling delay after reset assertion — CORBRPRST is
            // self-clearing per HDA spec §4.4.1.
            crate::timing::delay_us(50);
            mmio_write16(mmio, CORBRP, 0);
            mmio_write16(mmio, CORBWP, 0);

            // Program CORB size (CORBSIZE bits [1:0]), preserving the
            // read-only capability nibble in CORBSZCAP[7:4].
            // Then enable CORB DMA (CORBRUN = bit 1).
            let corb_szcap = mmio_read8(mmio, CORBCTL + 2) & 0xF0;
            mmio_write8(mmio, CORBCTL + 2, corb_sz_code | corb_szcap);
            mmio_write8(mmio, CORBCTL, 0x02);

            // Program RIRB base
            mmio_write32(mmio, RIRBLBASE, rirb_phys as u32);
            mmio_write32(mmio, RIRBUBASE, (rirb_phys >> 32) as u32);

            // Reset RIRB write pointer
            mmio_write16(mmio, RIRBWP, 0x8000);
            // Short settling delay after RIRBWP reset assertion (self-clearing).
            crate::timing::delay_us(50);
            if mmio_read16(mmio, RIRBWP) & 0x8000 != 0 {
                mmio_write16(mmio, RIRBWP, 0);
            }

            // RINTCNT is not optional: zero means that the controller has
            // already reached its response-count threshold.  QEMU (and
            // some Intel controllers) consequently drops every solicited
            // response until the host programs a non-zero count.  Use a
            // generously sized polling batch: this driver polls RIRBWP and
            // does not use the RIRB interrupt, while QEMU keeps its internal
            // response counter latched until the interrupt status is
            // acknowledged.  A 255-response batch covers codec enumeration
            // without making every verb depend on that interrupt handshake.
            mmio_write16(mmio, RINTCNT, 0x00FF);
            // Clear stale response/overrun status before enabling RIRB DMA.
            mmio_write8(mmio, RIRBSTS, 0x05);

            // Program RIRB size and enable RIRB DMA
            let rirb_szcap = mmio_read8(mmio, RIRBCTL + 2) & 0xF0;
            mmio_write8(mmio, RIRBCTL + 2, corb_sz_code | rirb_szcap);
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

            // Short settling delay for Intel PCH controllers (50 ms).
            crate::timing::delay_us(50_000);
            true
        }
    }

    /// Force a VM exit on QEMU/KVM by reading the PIC master IMR
    /// (I/O port 0x21).  I/O port accesses always trap on KVM,
    /// unlike MMIO reads which may be satisfied via EPT.
    #[inline]
    fn tick_vm_exit() {
        unsafe {
            x86_64::instructions::port::PortReadOnly::<u8>::new(0x21).read();
        }
    }

    /// Send a verb to the codec and return the 32‑bit solicited response,
    /// or `0xFFFF_FFFF` on timeout.
    ///
    /// Uses CORB/RIRB DMA.  On KVM, guest‑RAM writes to the CORB buffer
    /// may be cached by EPT, so we flush the cache line before advancing
    /// CORBWP, and also force VM exits so QEMU can see the update.
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
        unsafe {
            let corb = self.corb_virt;
            let rirb = self.rirb_virt;
            if corb.is_null() || rirb.is_null() {
                return 0xFFFF_FFFF;
            }
            let corb_n = self.corb_entries;

            // Encode the verb command word
            let cmd_val = if verb > 0xF {
                // The 12-bit verb form uses the low 16 bits for the
                // parameter when the verb's low byte is zero (for example
                // SET_PROC_COEF=0x400).  Keeping all 16 bits is required by
                // Realtek's indexed coefficient registers.
                (verb << 8) | (payload as u32 & 0xFFFF)
            } else {
                (verb << 16) | (payload as u32 & 0xFFFF)
            };
            let cmd = ((codec as u32) << 28) | ((node as u32) << 20) | cmd_val;

            // CORBWP/CORBRP are entry offsets in Dword units. RIRBWP is
            // an entry offset in two-Dword units. They are not byte
            // addresses, so the register values index the rings directly.
            let corb_mask = (corb_n - 1) as u16;
            let rirb_mask = (corb_n - 1) as u16;

            // Wait for space in CORB (1 ms deadline).
            let has_space = crate::timing::poll_timeout_us(1_000, || {
                let wp = (mmio_read16(mmio, CORBWP) & corb_mask) as usize;
                let rp = (mmio_read16(mmio, CORBRP) & corb_mask) as usize;
                if (wp + 1) % corb_n != rp {
                    Some(true)
                } else {
                    None
                }
            })
            .unwrap_or(false);
            if !has_space {
                return 0xFFFF_FFFF;
            }

            // Capture RIRBWP before writing CORBWP
            let curr_rp = (mmio_read16(mmio, RIRBWP) & rirb_mask) as usize;
            let wp = (mmio_read16(mmio, CORBWP) & corb_mask) as usize;
            let next_wp = (wp + 1) % corb_n;

            // Write CORB entry
            core::ptr::write_volatile(corb.add(next_wp), cmd);
            atomic::fence(atomic::Ordering::SeqCst);

            // On KVM with EPT, the write above went into guest RAM which
            // is cached.  Force a VM exit so QEMU gets a chance to see
            // the updated CORB entry before we advance CORBWP.
            Self::tick_vm_exit();

            mmio_write16(mmio, CORBWP, next_wp as u16);

            // Walk RIRB entries incrementally. A codec response should be
            // available within a few milliseconds; keep enumeration from
            // blocking the boot path when a controller advertises a codec
            // that does not actually answer verbs (for example, a partial
            // virtual HDA device).
            let rirb_n: usize = corb_n;
            let rirb_entry_mask = rirb_n - 1;
            let mut rp = curr_rp;
            let verb_result = crate::timing::poll_timeout_us(5_000, || {
                Self::tick_vm_exit();
                let rirb_wp = (mmio_read16(mmio, RIRBWP) & rirb_mask) as usize;
                while rp != rirb_wp {
                    rp = (rp + 1) & rirb_entry_mask;
                    let resp = core::ptr::read_volatile(rirb.add(rp));
                    let unsol = (resp >> 63) & 1;
                    if unsol == 0 {
                        // RIRB entries store the codec response in the
                        // lower dword. The upper dword is response metadata
                        // (codec address, unsolicited-response bit, etc.).
                        let raw = resp as u32;
                        if verb == verbs::GET_PARAM && payload <= 0x12 {
                            log::info!(
                                "HDA: corb verb OK c={} n={:#x} v={:#03x} → raw=0x{:08x}",
                                codec,
                                node,
                                verb,
                                raw
                            );
                        }
                        // Keep stale response/overrun status from leaking into
                        // a subsequent polling batch.  The normal batch
                        // threshold is deliberately large because this path
                        // polls RIRBWP instead of servicing RIRB interrupts.
                        mmio_write8(mmio, RIRBSTS, 0x05);
                        return Some(raw);
                    }
                }
                None
            });
            let result = verb_result.unwrap_or(0xFFFF_FFFF);
            if result == 0xFFFF_FFFF {
                // Also release a possible overrun/response-count latch on a
                // timeout so a later recovery or probe is not wedged behind
                // stale RIRB status.
                mmio_write8(mmio, RIRBSTS, 0x05);
            }
            result
        }
    }
}

crate::make_mmio_helpers!();
