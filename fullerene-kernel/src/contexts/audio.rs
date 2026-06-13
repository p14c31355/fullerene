//! AudioContext — HDA controller, codec, stream, mixer, PC speaker.
//!
//! Aggregates:
//! - `ControllerContext` — HDA bus mastering, CORB/RIRB, DMA engine
//! - `CodecContext`      — ALC286 / generic codec state
//! - `StreamContext`     — input/output stream descriptors
//! - `MixerContext`       — volume, muting, routing

use nitrogen::hda::HdaController;
use nitrogen::hda::controller::HdaDiagInfo;
use nitrogen::hda::dma::{DMA_BUF_SIZE, DmaRegion};
use spin::Mutex;

// ── Sub-contexts ──────────────────────────────────────────────

/// HDA controller-level state (bus mastering, CORB/RIRB, DMA engine).
pub struct ControllerContext {
    pub hda: Option<HdaController>,
    pub diag: HdaDiagInfo,
}

impl ControllerContext {
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
        }
    }

    pub fn is_ready(&self) -> bool {
        self.hda.as_ref().is_some_and(|c| c.is_ready())
    }
}

/// Audio codec context (ALC286 / generic).
#[derive(Debug, Clone, Copy)]
pub struct CodecContext {
    /// Codec vendor/device IDs populated after probe.
    pub vendor_id: u32,
    pub device_id: u32,
    /// Number of nodes discovered.
    pub node_count: u32,
    /// Whether codec probing has completed.
    pub probed: bool,
}

impl CodecContext {
    pub const fn new() -> Self {
        Self {
            vendor_id: 0,
            device_id: 0,
            node_count: 0,
            probed: false,
        }
    }
}

/// HDA stream context (input/output descriptors).
#[derive(Debug, Clone, Copy)]
pub struct StreamContext {
    /// Number of configured output streams.
    pub output_streams: u8,
    /// Number of configured input streams.
    pub input_streams: u8,
    /// Whether any stream is active.
    pub active: bool,
}

impl StreamContext {
    pub const fn new() -> Self {
        Self {
            output_streams: 0,
            input_streams: 0,
            active: false,
        }
    }
}

/// Mixer context (volume, muting, routing).
#[derive(Debug, Clone, Copy)]
pub struct MixerContext {
    /// Master volume (0-100).
    pub master_volume: u8,
    /// PCM volume (0-100).
    pub pcm_volume: u8,
    /// Whether output is muted.
    pub muted: bool,
}

impl MixerContext {
    pub const fn new() -> Self {
        Self {
            master_volume: 100,
            pcm_volume: 100,
            muted: false,
        }
    }
}

// ── Aggregate AudioContext ────────────────────────────────────

unsafe impl Send for AudioContext {}
unsafe impl Sync for AudioContext {}

pub struct AudioContext {
    // Sub-contexts (new)
    pub controller: ControllerContext,
    pub codec: CodecContext,
    pub stream: StreamContext,
    pub mixer: MixerContext,

    // ── retained for backward compat ──────────────────────────
    pub hda: Option<HdaController>,
    pub diag: HdaDiagInfo,
    init_done: bool,
    corb: Option<DmaRegion>,
    rirb: Option<DmaRegion>,
    dma: Option<DmaRegion>,
}

impl AudioContext {
    pub const fn new() -> Self {
        Self {
            controller: ControllerContext::new(),
            codec: CodecContext::new(),
            stream: StreamContext::new(),
            mixer: MixerContext::new(),
            hda: None,
            diag: HdaDiagInfo {
                gcap: 0,
                gcap64: false,
                corb_phys: 0,
                rirb_phys: 0,
                states_after_crst: 0,
                populated: false,
            },
            init_done: false,
            corb: None,
            rirb: None,
            dma: None,
        }
    }

    pub fn probe(&mut self) {
        let off = petroleum::common::memory::get_physical_memory_offset() as u64;
        if let Some((bus, dev, func, bar0)) = HdaController::probe(off) {
            let mmio = (bar0 + off) as *mut u8;
            self.hda = Some(HdaController::new(mmio, bar0));
            self.stream.output_streams = 1;
            self.stream.input_streams = 0;
            self.codec.probed = true;
            log::info!(
                "Sound: HDA at {:04x}:{:02x}.{}, BAR0=0x{:x}",
                bus,
                dev,
                func,
                bar0
            );
        } else {
            log::info!("Sound: No HDA (PC speaker only)");
        }
    }

    pub fn hda_available(&self) -> bool {
        self.hda.is_some()
    }
    pub fn hda_ready(&self) -> bool {
        self.controller.is_ready()
    }

    pub fn lazy_init(&mut self) {
        if self.init_done {
            return;
        }
        self.init_done = true;
        let ctrl = match self.hda.as_mut() {
            Some(c) => c,
            None => return,
        };
        if ctrl.is_ready() {
            return;
        }
        let Some(corb) = alloc_dma(1) else { return };
        let Some(rirb) = alloc_dma(1) else { return };
        let Some(dma) = alloc_dma((DMA_BUF_SIZE as usize + 4095) / 4096) else {
            return;
        };
        if !unsafe { ctrl.init(&corb, &rirb, &dma) } {
            log::error!("Sound: HDA init failed");
            return;
        }
        let gcap = unsafe { core::ptr::read_volatile(ctrl.mmio().add(0x0000) as *const u32) };
        let diag = HdaDiagInfo {
            gcap,
            gcap64: gcap & 1 != 0,
            corb_phys: corb.phys,
            rirb_phys: rirb.phys,
            states_after_crst: 0,
            populated: true,
        };
        self.diag = diag;
        self.controller.diag = diag;
        self.corb = Some(corb);
        self.rirb = Some(rirb);
        self.dma = Some(dma);
    }

    pub fn write_samples(&mut self, offset: u32, samples: &[u8]) -> usize {
        self.lazy_init();
        match self.hda.as_ref() {
            Some(c) if c.is_ready() => c.write_at(offset, samples),
            _ => 0,
        }
    }
    pub fn feed_samples(&mut self, samples: &[u8]) -> usize {
        self.lazy_init();
        match self.hda.as_ref() {
            Some(c) if c.is_ready() => c.feed_samples(samples),
            _ => 0,
        }
    }
    pub fn feed_silence(&self, half: usize) -> usize {
        match self.hda.as_ref() {
            Some(c) if c.is_ready() => c.feed_silence(half),
            _ => 0,
        }
    }
    pub fn poll(&self) {
        let Some(c) = self.hda.as_ref() else { return };
        if !c.is_ready() {
            return;
        }
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        loop {
            if c.poll(Some(0)) || unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 300_000_000 {
                return;
            }
            core::hint::spin_loop();
        }
    }
    pub fn poll_block(&self, timeout: Option<u64>) -> bool {
        self.hda
            .as_ref()
            .filter(|c| c.is_ready())
            .is_some_and(|c| c.poll(timeout))
    }
    pub fn poll_delay(&self, tsc_per_ms: u64, ms: u64) {
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        let duration = tsc_per_ms.saturating_mul(ms);
        while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) < duration {
            self.poll_block(Some(0));
            core::hint::spin_loop();
        }
    }
    pub fn playback_progress(&self) -> Option<u64> {
        self.hda.as_ref().and_then(|c| c.playback_progress())
    }
    pub fn reset_prefill_tracking(&self) {
        if let Some(c) = self.hda.as_ref() {
            if c.is_ready() {
                c.reset_prefill_tracking();
            }
        }
    }

    pub fn pc_speaker_on(freq_hz: u32) {
        if freq_hz == 0 {
            Self::pc_speaker_off();
            return;
        }
        let d = (1_193_182u32 / freq_hz).min(65535) as u16;
        unsafe {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x43).write(0xB6);
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write(d as u8);
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write((d >> 8) as u8);
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61)
                .write(x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read() | 0x03);
        }
    }
    pub fn pc_speaker_off() {
        unsafe {
            x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61)
                .write(x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read() & !0x03);
        }
    }
}

fn alloc_dma(pages: usize) -> Option<DmaRegion> {
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let phys = petroleum::page_table::constants::get_frame_allocator_mut()
        .allocate_contiguous_frames(pages)
        .ok()?;
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

static AUDIO_CTX: Mutex<Option<AudioContext>> = Mutex::new(None);
pub fn init_audio() {
    let mut c = AudioContext::new();
    c.probe();
    *AUDIO_CTX.lock() = Some(c);
}
pub fn get_audio() -> &'static Mutex<Option<AudioContext>> {
    &AUDIO_CTX
}
pub fn with_audio_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut AudioContext) -> R,
{
    AUDIO_CTX.lock().as_mut().map(f)
}
pub fn with_audio<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&AudioContext) -> R,
{
    AUDIO_CTX.lock().as_ref().map(f)
}