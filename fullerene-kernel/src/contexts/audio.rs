//! AudioContext — Sound / Audio subsystem context.
//!
//! Consolidates:
//! - HDA controller state (`crate::sound::HDA_CTRL`)
//! - HDA diagnostic info (`crate::sound::HDA_DIAG`)
//! - DMA region allocation
//! - PC speaker control
//!
//! # Design
//!
//! Instead of a global `HDA_CTRL` wrapped in `Mutex<Option<...>>`
//! with static init guards, this context owns the controller and
//! provides methods that encapsulate the lazy-init pattern.
//!
//! ```rust,ignore
//! audio.write_samples(pcm_data, offset);
//! audio.poll();
//! audio.feed_silence(256);
//! ```

use nitrogen::hda::controller::HdaDiagInfo;
use nitrogen::hda::dma::{DmaRegion, DMA_BUF_SIZE};
use nitrogen::hda::HdaController;
use spin::Mutex;

// Safety: AudioContext is only accessed via Mutex (see AUDIO_CONTEXT global).
// The raw MMIO pointer inside HdaController is never sent between threads.
unsafe impl Send for AudioContext {}
unsafe impl Sync for AudioContext {}

/// Audio subsystem context.
pub struct AudioContext {
    /// HDA controller, if detected.
    pub hda: Option<HdaController>,

    /// Cached HDA diagnostic info for shell inspection.
    pub diag: HdaDiagInfo,

    /// Whether lazy HDA initialisation has been attempted.
    hda_init_attempted: bool,

    /// CORB DMA region (held to keep pages alive).
    corb_region: Option<DmaRegion>,

    /// RIRB DMA region.
    rirb_region: Option<DmaRegion>,

    /// Output stream DMA region.
    dma_region: Option<DmaRegion>,
}

impl AudioContext {
    /// Create an empty audio context.
    pub const fn new() -> Self {
        Self {
            hda: None,
            diag: HdaDiagInfo {
                gcap: 0,
                gcap64: false,
                corb_phys: 0,
                rirb_phys: 0,
                states_after_crst: 0,
                populated: false,
            },
            hda_init_attempted: false,
            corb_region: None,
            rirb_region: None,
            dma_region: None,
        }
    }

    /// Probe PCI for an HDA controller and store the MMIO address.
    /// Does NOT start CORB/RIRB or DMA — that happens lazily.
    pub fn probe(&mut self) {
        let phys_offset = petroleum::common::memory::get_physical_memory_offset() as u64;
        match HdaController::probe(phys_offset) {
            Some((bus, dev, func, bar0)) => {
                log::info!(
                    "Sound: HDA at {:04x}:{:02x}.{}, BAR0=0x{:x}",
                    bus,
                    dev,
                    func,
                    bar0
                );
                let mmio = (bar0 + phys_offset) as *mut u8;
                let ctrl = HdaController::new(mmio, bar0);
                self.hda = Some(ctrl);
            }
            None => log::info!("Sound: No HDA (PC speaker only)"),
        }
    }

    /// Returns `true` when an HDA controller was found.
    pub fn hda_available(&self) -> bool {
        self.hda.is_some()
    }

    /// Returns `true` when the HDA controller is fully initialised
    /// and ready to accept audio data.
    pub fn hda_ready(&self) -> bool {
        self.hda.as_ref().is_some_and(|c| c.is_ready())
    }

    /// Lazy initialisation: bring up CORB/RIRB, enumerate codec, start DMA.
    pub fn hda_lazy_init(&mut self) {
        if self.hda_init_attempted {
            return;
        }
        self.hda_init_attempted = true;

        let ctrl = match self.hda.as_mut() {
            Some(c) => c,
            None => return,
        };
        if ctrl.is_ready() {
            return;
        }

        // Allocate CORB/RIRB DMA pages
        let Some(corb) = alloc_dma_region(1) else {
            log::error!("Sound: CORB alloc fail");
            return;
        };
        let Some(rirb) = alloc_dma_region(1) else {
            log::error!("Sound: RIRB alloc fail");
            return;
        };
        let dma_pages = (DMA_BUF_SIZE as usize + 4095) / 4096;
        let Some(dma) = alloc_dma_region(dma_pages) else {
            log::error!("Sound: DMA alloc fail");
            return;
        };

        let init_ok = unsafe { ctrl.init(&corb, &rirb, &dma) };
        if !init_ok {
            log::error!(
                "Sound: HDA controller init failed (is_ready={}, GCAP=0x{:x})",
                ctrl.is_ready(),
                unsafe { core::ptr::read_volatile(ctrl.mmio().add(0x0000) as *const u32) }
            );
            return;
        }

        // Populate diagnostic cache
        let gcap_raw =
            unsafe { core::ptr::read_volatile(ctrl.mmio().add(0x0000) as *const u32) };
        self.diag = HdaDiagInfo {
            gcap: gcap_raw,
            gcap64: gcap_raw & 1 != 0,
            corb_phys: corb.phys,
            rirb_phys: rirb.phys,
            states_after_crst: 0,
            populated: true,
        };

        self.corb_region = Some(corb);
        self.rirb_region = Some(rirb);
        self.dma_region = Some(dma);
    }

    /// Write PCM bytes at a specific offset into the DMA buffer.
    pub fn write_samples(&mut self, offset: u32, samples: &[u8]) -> usize {
        self.hda_lazy_init();
        match self.hda.as_ref() {
            Some(ctrl) if ctrl.is_ready() => ctrl.write_at(offset, samples),
            _ => 0,
        }
    }

    /// Feed PCM samples into the DMA ring buffer.  Returns bytes written.
    pub fn feed_samples(&mut self, samples: &[u8]) -> usize {
        self.hda_lazy_init();
        match self.hda.as_ref() {
            Some(ctrl) if ctrl.is_ready() => ctrl.feed_samples(samples),
            _ => 0,
        }
    }

    /// Feed silence into the HDA half-buffer.
    pub fn feed_silence(&mut self, half: usize) -> usize {
        match self.hda.as_ref() {
            Some(ctrl) if ctrl.is_ready() => ctrl.feed_silence(half),
            _ => 0,
        }
    }

    /// Poll for half-buffer completion.  Times out after ~100 ms at 3 GHz.
    pub fn poll(&self) {
        let Some(ctrl) = self.hda.as_ref() else {
            return;
        };
        if !ctrl.is_ready() {
            return;
        }
        let deadline = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(300_000_000);
        loop {
            if ctrl.poll(None) {
                return;
            }
            if unsafe { core::arch::x86_64::_rdtsc() } >= deadline {
                return;
            }
            core::hint::spin_loop();
        }
    }

    /// Poll with optional TSC timeout.  Returns `true` when data was fed.
    pub fn poll_block(&self, timeout_tsc: Option<u64>) -> bool {
        match self.hda.as_ref() {
            Some(ctrl) if ctrl.is_ready() => ctrl.poll(timeout_tsc),
            _ => false,
        }
    }

    /// TSC‑based delay with periodic HDA poll (used for silence drain).
    pub fn poll_delay(&self, tsc_per_ms: u64, ms: u64) {
        let deadline =
            unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(tsc_per_ms.saturating_mul(ms));
        while unsafe { core::arch::x86_64::_rdtsc() } < deadline {
            self.poll();
            core::hint::spin_loop();
        }
    }

    /// Return total PCM bytes consumed (played back) since stream start.
    pub fn playback_progress(&self) -> Option<u64> {
        self.hda.as_ref().and_then(|c| c.playback_progress())
    }

    /// Reset LPIB tracking so DMA is assumed to start at half 0.
    pub fn reset_prefill_tracking(&self) {
        if let Some(ctrl) = self.hda.as_ref() {
            if ctrl.is_ready() {
                ctrl.reset_prefill_tracking();
            }
        }
    }

    // ── PC Speaker ─────────────────────────────────────────────

    /// Turn the PC speaker on at the given frequency (Hz). 0 = off.
    pub fn pc_speaker_on(frequency_hz: u32) {
        if frequency_hz == 0 {
            Self::pc_speaker_off();
            return;
        }
        let divisor = (1_193_182u32 / frequency_hz).min(65535) as u16;
        unsafe {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x43).write(0xB6);
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write(divisor as u8);
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write((divisor >> 8) as u8);
            let t = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(t | 0x03);
        }
    }

    /// Turn the PC speaker off.
    pub fn pc_speaker_off() {
        unsafe {
            let t = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(t & !0x03);
        }
    }
}

/// Allocate contiguous physical DMA pages and return a `DmaRegion`.
fn alloc_dma_region(pages: usize) -> Option<DmaRegion> {
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let phys = match petroleum::page_table::constants::get_frame_allocator_mut()
        .allocate_contiguous_frames(pages)
    {
        Ok(a) => a,
        Err(_) => {
            log::error!("Sound: DMA alloc fail");
            return None;
        }
    };
    let virt = (phys + off) as *mut u8;
    unsafe {
        core::ptr::write_bytes(virt, 0, pages * 4096);
    }
    Some(DmaRegion {
        phys,
        virt,
        size: pages * 4096,
    })
}

/// Global audio context.
static AUDIO_CONTEXT: Mutex<Option<AudioContext>> = Mutex::new(None);

/// Initialise the global audio context.
pub fn init_audio_context() {
    let mut ctx = AudioContext::new();
    ctx.probe();
    *AUDIO_CONTEXT.lock() = Some(ctx);
}

/// Get a reference to the global audio context.
pub fn get_audio() -> &'static Mutex<Option<AudioContext>> {
    &AUDIO_CONTEXT
}

/// Convenience: execute a closure with a mutable reference.
pub fn with_audio_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut AudioContext) -> R,
{
    AUDIO_CONTEXT.lock().as_mut().map(f)
}

/// Convenience: execute a closure with a shared reference.
pub fn with_audio<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&AudioContext) -> R,
{
    AUDIO_CONTEXT.lock().as_ref().map(f)
}