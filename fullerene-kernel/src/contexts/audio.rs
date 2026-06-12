//! AudioContext — HDA controller + PC speaker. Replaces crate::sound globals.
use nitrogen::hda::HdaController;
use nitrogen::hda::controller::HdaDiagInfo;
use nitrogen::hda::dma::{DMA_BUF_SIZE, DmaRegion};
use spin::Mutex;

unsafe impl Send for AudioContext {}
unsafe impl Sync for AudioContext {}

pub struct AudioContext {
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
        self.hda.as_ref().is_some_and(|c| c.is_ready())
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
        self.diag = HdaDiagInfo {
            gcap,
            gcap64: gcap & 1 != 0,
            corb_phys: corb.phys,
            rirb_phys: rirb.phys,
            states_after_crst: 0,
            populated: true,
        };
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
        let dl = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(300_000_000);
        loop {
            if c.poll(None) || unsafe { core::arch::x86_64::_rdtsc() } >= dl {
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
        let dl =
            unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(tsc_per_ms.saturating_mul(ms));
        while unsafe { core::arch::x86_64::_rdtsc() } < dl {
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
