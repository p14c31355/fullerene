//! DMA engine and buffer management for HDA output streams.
//!
//! Manages:
//! - BDL (Buffer Descriptor List) with two half-buffer entries
//! - Stream descriptor register programming (SD_BASE → SD_BDPU)
//! - Half-buffer write/poll/feed logic

use core::sync::atomic::{self, AtomicBool, AtomicU64, Ordering};

// ── Stream descriptor register offsets (relative to SD base) ─────
const SD_CTL: usize = 0x00;
const SD_STS: usize = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0C;
const SD_FMT: usize = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;

/// A contiguous DMA‑capable memory region.
///
/// The caller (kernel) supplies this after allocating physical
/// pages.  Nitrogen never owns the allocator.
#[derive(Clone, Copy)]
pub struct DmaRegion {
    /// Physical base address.
    pub phys: u64,
    /// Virtual address (physical + offset).
    pub virt: *mut u8,
    /// Size in bytes.
    pub size: usize,
}

/// Default audio DMA buffer size (32 KiB).
pub const DMA_BUF_SIZE: u32 = 32768;

/// Number of BDL entries (2 → double‑buffered half‑buffer scheme).
pub const BDL_ENTRIES: u32 = 2;

/// A single Buffer Descriptor List entry.
#[repr(C)]
pub struct BdlEntry {
    pub addr_lo: u32,
    pub addr_hi: u32,
    pub length: u32,
    pub flags: u32,
}

/// DMA playback engine for a single HDA output stream.
pub struct DmaEngine {
    /// Stream descriptor base offset within MMIO (SD_BASE + idx * SD_SIZE).
    sd_offset: usize,
    /// DMA buffer virtual address.
    dma_virt: *mut u8,
    /// Offset within the DMA page where audio data begins (past BDL).
    audio_off: u32,
    /// Total audio buffer size (bytes).
    audio_size: u32,
    /// Half buffer size (bytes).
    half_size: u32,
    /// Last raw LPIB value seen (for cross‑boundary detection).
    last_lpib: AtomicU64,
    /// Whether the stream is actively running.
    ready: AtomicBool,
}

impl DmaEngine {
    /// Create an uninitialised DMA engine.
    pub const fn new(sd_offset: usize) -> Self {
        Self {
            sd_offset,
            dma_virt: core::ptr::null_mut(),
            audio_off: 0,
            audio_size: 0,
            half_size: 0,
            last_lpib: AtomicU64::new(u64::MAX),
            ready: AtomicBool::new(false),
        }
    }

    /// Program the stream descriptor and BDL, then start the DMA engine.
    ///
    /// `dma_region` is a contiguous block containing both the BDL (at
    /// offset 0) and the audio buffer (immediately after the BDL).
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base pointer.  `dma_region` must
    /// point to valid, zeroed, physically‑contiguous memory of at
    /// least `DMA_BUF_SIZE + BDL_ENTRIES * sizeof(BdlEntry)` bytes.
    pub unsafe fn init(&mut self, mmio: *mut u8, dma_region: &DmaRegion, stream_tag: u8) {
        let bdl_sz = (core::mem::size_of::<BdlEntry>() as u32) * BDL_ENTRIES;
        let audio_phys = dma_region.phys + bdl_sz as u64;
        let audio_off = bdl_sz;
        let audio_size = DMA_BUF_SIZE - audio_off;
        let half = audio_size / 2;

        // Write BDL entries (two half‑buffer segments with IOC)
        unsafe {
            // SAFETY: dma_region.virt points to valid, zeroed DMA memory
            let bdl = dma_region.virt as *mut BdlEntry;
            bdl.add(0).write_volatile(BdlEntry {
                addr_lo: audio_phys as u32,
                addr_hi: (audio_phys >> 32) as u32,
                length: half,
                flags: 0x01, // IOC
            });
            bdl.add(1).write_volatile(BdlEntry {
                addr_lo: (audio_phys + half as u64) as u32,
                addr_hi: ((audio_phys + half as u64) >> 32) as u32,
                length: half,
                flags: 0x01, // IOC
            });
        }

        let sd = self.sd_offset;

        unsafe {
            // SAFETY: mmio is valid HDA MMIO base, sd offset within valid range
            // Stop and reset stream
            mmio_write32(mmio, sd + SD_CTL, 0);
            // Short settling delay (50 μs) to let the hardware quiesce.
            crate::timing::delay_us(50);
            mmio_write8(mmio, sd + SD_STS, 0xFF); // clear all status bits (WC)

            // SRST handshake: set SRST and poll until it reads back as 1
            mmio_write32(mmio, sd + SD_CTL, 0x01); // SRST
            let srst_ok = crate::timing::wait_timeout_us(50_000, || {
                mmio_read32(mmio, sd + SD_CTL) & 0x01 != 0
            })
            .is_ok();

            // Clear SRST and poll until it reads back as 0
            mmio_write32(mmio, sd + SD_CTL, 0);
            let srst_clr_ok = crate::timing::wait_timeout_us(50_000, || {
                mmio_read32(mmio, sd + SD_CTL) & 0x01 == 0
            })
            .is_ok();

            if !srst_ok || !srst_clr_ok {
                log::warn!("HDA: SRST handshake timed out on stream {} (tag={})", sd, stream_tag);
                return;
            }

            // Clear status again, program format / BDL / stream params
            mmio_write8(mmio, sd + SD_STS, 0xFF);
            // 48 kHz, 16-bit, 1 channel: bit7 BASE=0, bits6:4 BITS=1, bits3:0 CHAN=0
            mmio_write16(mmio, sd + SD_FMT, 0x0010);
            mmio_write32(mmio, sd + SD_CBL, audio_size);
            mmio_write16(mmio, sd + SD_LVI, (BDL_ENTRIES - 1) as u16);
            mmio_write32(mmio, sd + SD_BDPL, dma_region.phys as u32);
            mmio_write32(mmio, sd + SD_BDPU, (dma_region.phys >> 32) as u32);
        }

        // Store fence: ensure BDL / DMA buffer writes are visible
        atomic::fence(atomic::Ordering::SeqCst);

        unsafe {
            // SAFETY: Start stream after fence
            // Start stream: RUN (bit 1) + IOCE (bit 2) + STRIPE1 (bits 18:16) + STREAMTAG (bits 23:20)
            mmio_write32(
                mmio,
                sd + SD_CTL,
                ((stream_tag as u32) << 20) | (1u32 << 16) | 0x06,
            );
        }

        log::info!("HDA: stream started ({} B, fmt=0x0010)", audio_size);

        self.dma_virt = dma_region.virt;
        self.audio_off = audio_off;
        self.audio_size = audio_size;
        self.half_size = half;
        self.last_lpib.store(0, Ordering::Relaxed);
        self.ready.store(true, Ordering::Release);
    }

    /// Whether the DMA engine is running.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Total audio buffer size (bytes).
    pub fn audio_size(&self) -> u32 {
        self.audio_size
    }

    /// Half buffer size (bytes).
    pub fn half_size(&self) -> u32 {
        self.half_size
    }

    /// Write PCM bytes at a specific offset into the DMA buffer
    /// (used for pre‑fill).  Returns bytes written.
    pub fn write_at(&self, offset: u32, samples: &[u8]) -> usize {
        if self.dma_virt.is_null() {
            return 0;
        }
        let total = self.audio_size as usize;
        let max_len = total.saturating_sub(offset as usize);
        let n = samples.len().min(max_len);
        if n == 0 {
            return 0;
        }
        unsafe {
            let dst = self.dma_virt.add((self.audio_off + offset) as usize);
            core::ptr::copy_nonoverlapping(samples.as_ptr(), dst, n);
        }
        n
    }

    /// Reset LPIB tracking so DMA is assumed to start from half 0
    /// (call after pre‑filling both halves).
    pub fn reset_prefill_tracking(&self) {
        self.last_lpib.store(0, Ordering::Relaxed);
    }

    /// Feed PCM samples into the currently‑safe half of the DMA
    /// ring buffer.  Returns the number of bytes written.
    ///
    /// Uses LPIB + BCIS to determine which half is safe to write.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base pointer.
    pub unsafe fn feed_samples(&self, mmio: *mut u8, samples: &[u8]) -> usize {
        unsafe {
            if !self.ready.load(Ordering::Acquire) {
                return 0;
            }

            let sd = self.sd_offset;
            let half = self.half_size;

            // Read LPIB and determine safe half
            let lpib_raw = mmio_read32(mmio, sd + SD_LPIB);
            let lpib = lpib_raw.wrapping_rem(self.audio_size);
            let write_off: u32 = if lpib < half { half } else { 0 };

            // Check BCIS (hardware IOC)
            let sts = mmio_read8(mmio, sd + SD_STS);
            if sts & 0x04 != 0 {
                mmio_write8(mmio, sd + SD_STS, 0x04);
            }

            // Time‑based fallback: detect LPIB advance ≥ half bytes
            let last_raw = self.last_lpib.load(Ordering::Relaxed) as u32;
            let delta = lpib_raw.wrapping_sub(last_raw);
            let crossed = delta >= half || (sts & 0x04) != 0;
            if !crossed {
                return 0;
            }

            self.last_lpib.store(lpib_raw as u64, Ordering::Relaxed);

            let write_max = half as usize;
            let n = samples.len().min(write_max);
            if n == 0 {
                return 0;
            }

            let dst = self.dma_virt.add((self.audio_off + write_off) as usize);
            core::ptr::copy_nonoverlapping(samples.as_ptr(), dst, n);
            // Zero the remainder so stale data doesn't repeat
            if n < write_max {
                core::ptr::write_bytes(dst.add(n), 0, write_max - n);
            }
            n
        }
    }

    /// Read the current LPIB (Link Position In Buffer) register.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base pointer.
    pub unsafe fn playback_progress_bytes(&self, mmio: *mut u8) -> Option<u64> {
        unsafe {
            if !self.ready.load(Ordering::Acquire) {
                return None;
            }
            let sd = self.sd_offset;
            let raw = mmio_read32(mmio, sd + SD_LPIB);
            Some(raw as u64)
        }
    }

    /// Poll for BCIS (half‑buffer completion) with optional TSC timeout.
    ///
    /// Returns `true` when BCIS was observed, `false` on timeout /
    /// not ready.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base pointer.
    pub unsafe fn poll(&self, mmio: *mut u8, timeout_tsc: Option<u64>) -> bool {
        unsafe {
            if !self.ready.load(Ordering::Acquire) {
                return false;
            }
            let sd = self.sd_offset;
            let deadline = match timeout_tsc {
                Some(d) => core::arch::x86_64::_rdtsc().wrapping_add(d),
                None => u64::MAX,
            };
            loop {
                let sts = mmio_read8(mmio, sd + SD_STS);
                if sts & 0x04 != 0 {
                    return true;
                }
                if timeout_tsc.is_some() && core::arch::x86_64::_rdtsc() >= deadline {
                    return false;
                }
                core::hint::spin_loop();
            }
        }
    }

    /// TSC‑based delay with periodic DMA poll (used for silence drain).
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base pointer.
    pub unsafe fn poll_delay(&self, mmio: *mut u8, tsc_per_ms: u64, ms: u64) {
        unsafe {
            let deadline = core::arch::x86_64::_rdtsc().wrapping_add(tsc_per_ms.saturating_mul(ms));
            while core::arch::x86_64::_rdtsc() < deadline {
                self.poll(mmio, None);
                core::hint::spin_loop();
            }
        }
    }

    /// Feed silence into the DMA half‑buffer.
    /// Uses a static zeroed buffer to avoid large stack allocations.
    ///
    /// # Safety
    ///
    /// `mmio` must be a valid MMIO base pointer.
    pub unsafe fn feed_silence(&self, mmio: *mut u8, half: usize) -> usize {
        const MAX_SILENCE: usize = 16368;
        static SILENCE_BUF: [u8; MAX_SILENCE] = [0; MAX_SILENCE];
        // SAFETY: Caller guarantees mmio is valid
        unsafe { self.feed_samples(mmio, &SILENCE_BUF[..half.min(MAX_SILENCE)]) }
    }
}

crate::make_mmio_helpers!();
