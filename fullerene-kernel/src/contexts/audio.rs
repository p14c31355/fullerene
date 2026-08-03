//! AudioContext — HDA controller + PC speaker.
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use nitrogen::hda::HdaController;
use nitrogen::hda::controller::HdaDiagInfo;
use nitrogen::hda::dma::{DMA_BUF_SIZE, DmaRegion};

unsafe impl Send for AudioContext {}
unsafe impl Sync for AudioContext {}

pub struct AudioContext {
    pub hda: Option<HdaController>,
    pub diag: HdaDiagInfo,
    init_done: bool,
    corb: Option<DmaRegion>,
    rirb: Option<DmaRegion>,
    dma: Option<DmaRegion>,
    playback: Option<PlaybackState>,
}

struct PlaybackState {
    request_id: u64,
    pcm: Vec<u8>,
    half_size: usize,
    half_count: usize,
    completed: usize,
    next_half: usize,
    initial_progress: u64,
    progressed: bool,
    deadline_tsc: u64,
}

enum AudioRequest {
    PlayPcm {
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        pcm: Vec<u8>,
    },
}

struct AudioCompletion {
    request_id: u64,
    success: bool,
}

static AUDIO_SQ: spin::Mutex<VecDeque<(u64, AudioRequest)>> = spin::Mutex::new(VecDeque::new());
static AUDIO_CQ: spin::Mutex<VecDeque<AudioCompletion>> = spin::Mutex::new(VecDeque::new());
static NEXT_AUDIO_REQUEST: AtomicU64 = AtomicU64::new(1);

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
            playback: None,
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
        let ctrl = match self.hda.as_mut() {
            Some(c) => c,
            None => {
                self.init_done = true;
                return;
            }
        };
        if ctrl.is_ready() {
            self.init_done = true;
            return;
        }
        let Some(corb) = alloc_dma(1) else { return };
        let Some(rirb) = alloc_dma(1) else {
            free_dma(corb);
            return;
        };
        let Some(dma) = alloc_dma((DMA_BUF_SIZE as usize + 4095) / 4096) else {
            free_dma(rirb);
            free_dma(corb);
            return;
        };
        if !unsafe { ctrl.init(&corb, &rirb, &dma) } {
            log::error!("Sound: HDA init failed");
            free_dma(dma);
            free_dma(rirb);
            free_dma(corb);
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
        self.init_done = true;
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
            if c.poll(Some(0))
                || unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start) >= 300_000_000
            {
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

    /// Start a PCM request without waiting for the DMA stream to finish.
    fn start_pcm_async(
        &mut self,
        request_id: u64,
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        pcm: Vec<u8>,
    ) -> bool {
        if self.playback.is_some()
            || sample_rate != 48_000
            || channels != 1
            || bits_per_sample != 16
            || pcm.is_empty()
        {
            return false;
        }

        self.lazy_init();
        let Some(controller) = self.hda.as_ref().filter(|c| c.is_ready()) else {
            log::warn!("Sound: asynchronous playback requested before HDA became ready");
            return false;
        };
        let half_size = controller.dma().half_size() as usize;
        if half_size == 0 {
            return false;
        }
        let half_count = core::cmp::max(2, pcm.len().div_ceil(half_size));

        for slot in 0..2 {
            let start = (slot * half_size).min(pcm.len());
            let end = (start + half_size).min(pcm.len());
            let offset = (slot * half_size) as u32;
            let written = controller.write_at(offset, &pcm[start..end]);
            if written != end - start {
                log::warn!(
                    "Sound: asynchronous HDA prefill short write {} / {}",
                    written,
                    end - start
                );
                return false;
            }
            if written < half_size {
                controller.clear_at(offset + written as u32, half_size - written);
            }
        }
        controller.reset_prefill_tracking();
        if !controller.start_stream() {
            log::warn!("Sound: asynchronous HDA stream failed to start");
            return false;
        }

        let initial_progress = controller.playback_progress().unwrap_or(0);
        let timeout_tsc = solvent::get_tsc_per_ms().max(1).saturating_mul(500);
        self.playback = Some(PlaybackState {
            request_id,
            pcm,
            half_size,
            half_count,
            completed: 0,
            next_half: 2,
            initial_progress,
            progressed: false,
            deadline_tsc: unsafe { core::arch::x86_64::_rdtsc() }.saturating_add(timeout_tsc),
        });
        true
    }

    /// Advance one DMA boundary without spinning. Returns a completion when
    /// the current request has finished or timed out.
    fn poll_async_playback(&mut self) -> Option<(u64, bool)> {
        let state = self.playback.as_mut()?;
        let now = unsafe { core::arch::x86_64::_rdtsc() };
        let Some(controller) = self.hda.as_ref().filter(|c| c.is_ready()) else {
            let request_id = state.request_id;
            self.playback = None;
            return Some((request_id, false));
        };
        if now >= state.deadline_tsc {
            let request_id = state.request_id;
            let (ctl, sts, lpib) = controller.debug_stream_status();
            log::warn!(
                "Sound: asynchronous HDA playback timed out (CTL=0x{:08x} STS=0x{:02x} LPIB={})",
                ctl,
                sts,
                lpib
            );
            self.playback = None;
            return Some((request_id, false));
        }
        if !controller.poll(Some(0)) {
            return None;
        }

        let _ = controller.feed_samples(&[]);
        state.progressed |= controller
            .playback_progress()
            .is_some_and(|progress| progress != state.initial_progress);
        if state.next_half < state.half_count {
            let start = state.next_half * state.half_size;
            let end = (start + state.half_size).min(state.pcm.len());
            let offset = ((state.next_half % 2) * state.half_size) as u32;
            let written = controller.write_at(offset, &state.pcm[start..end]);
            if written != end - start {
                let request_id = state.request_id;
                self.playback = None;
                return Some((request_id, false));
            }
            if written < state.half_size {
                controller.clear_at(offset + written as u32, state.half_size - written);
            }
            state.next_half += 1;
        }
        state.completed += 1;
        if state.completed < state.half_count {
            return None;
        }

        let request_id = state.request_id;
        let success = state.progressed;
        if !success {
            log::warn!("Sound: asynchronous HDA playback completed without DMA progress");
        }
        self.playback = None;
        Some((request_id, success))
    }

    /// Play a complete 48 kHz, mono, signed 16-bit PCM stream.
    ///
    /// HDA is configured for the format used by the Fullerene startup WAV.
    /// The stream is double-buffered, so this method pre-fills both halves
    /// and then advances the DMA ring one half at a time while the caller
    /// (the synchronous WASM runtime) waits for playback to finish.
    pub fn play_pcm(
        &mut self,
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        pcm: &[u8],
    ) -> bool {
        if sample_rate != 48_000 || channels != 1 || bits_per_sample != 16 || pcm.is_empty() {
            log::warn!(
                "Sound: unsupported PCM format rate={} channels={} bits={} bytes={}",
                sample_rate,
                channels,
                bits_per_sample,
                pcm.len()
            );
            return false;
        }

        self.lazy_init();
        let Some(controller) = self.hda.as_ref().filter(|c| c.is_ready()) else {
            log::warn!("Sound: HDA playback requested before controller became ready");
            return false;
        };

        let half_size = controller.dma().half_size() as usize;
        if half_size == 0 {
            return false;
        }
        let half_count = core::cmp::max(2, pcm.len().div_ceil(half_size));

        // The stream starts as soon as HDA is initialised. Fill both DMA
        // halves before waiting for the first boundary; this prevents the
        // first audible buffer from containing stale or uninitialised data.
        for slot in 0..2 {
            let start = (slot * half_size).min(pcm.len());
            let end = (start + half_size).min(pcm.len());
            let offset = (slot * half_size) as u32;
            let written = controller.write_at(offset, &pcm[start..end]);
            if written != end - start {
                log::warn!(
                    "Sound: HDA prefill short write {} / {}",
                    written,
                    end - start
                );
                return false;
            }
            if written < half_size {
                controller.clear_at(offset + written as u32, half_size - written);
            }
        }
        controller.reset_prefill_tracking();
        if !controller.start_stream() {
            log::warn!("Sound: HDA stream failed to start after PCM prefill");
            return false;
        }

        let tsc_per_ms = solvent::get_tsc_per_ms().max(1);
        let timeout_tsc = tsc_per_ms.saturating_mul(500);
        let initial_progress = controller.playback_progress().unwrap_or(0);
        let mut progressed = false;
        let mut completed = 0usize;
        let mut next_half = 2usize;
        while completed < half_count {
            if !controller.poll(Some(timeout_tsc)) {
                let (ctl, sts, lpib) = controller.debug_stream_status();
                log::warn!(
                    "Sound: HDA playback timed out at half {} (CTL=0x{:08x} STS=0x{:02x} LPIB={})",
                    completed,
                    ctl,
                    sts,
                    lpib
                );
                return false;
            }

            // A zero-length feed acknowledges BCIS and advances the driver's
            // LPIB tracking without copying data into the DMA buffer.
            let _ = controller.feed_samples(&[]);
            progressed |= controller
                .playback_progress()
                .is_some_and(|progress| progress != initial_progress);
            if next_half < half_count {
                let start = next_half * half_size;
                let end = (start + half_size).min(pcm.len());
                let offset = ((next_half % 2) * half_size) as u32;
                let written = controller.write_at(offset, &pcm[start..end]);
                if written != end - start {
                    log::warn!(
                        "Sound: HDA playback short write {} / {}",
                        written,
                        end - start
                    );
                    return false;
                }
                if written < half_size {
                    controller.clear_at(offset + written as u32, half_size - written);
                }
                next_half += 1;
            }
            completed += 1;
        }

        let (stream_ctl, stream_status, lpib) = controller.debug_stream_status();
        if !progressed {
            log::warn!(
                "Sound: HDA reported completion without DMA progress (CTL=0x{:08x} STS=0x{:02x} LPIB={})",
                stream_ctl,
                stream_status,
                lpib
            );
            return false;
        }
        log::info!(
            "Sound: startup PCM playback complete ({} bytes) CTL=0x{:08x} STS=0x{:02x} LPIB={}",
            pcm.len(),
            stream_ctl,
            stream_status,
            lpib
        );
        true
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
    let phys = unsafe { petroleum::page_table::constants::get_frame_allocator_mut() }
        .allocate_contiguous_frames(pages)
        .ok()?;
    let virt = (phys + off) as *mut u8;
    unsafe {
        core::ptr::write_bytes(virt, 0, pages * 4096);
    }
    nitrogen::metrics::dma_allocated(pages * 4096);
    Some(DmaRegion {
        phys,
        virt,
        size: pages * 4096,
    })
}

fn free_dma(region: DmaRegion) {
    let pages = region.size / 4096 + usize::from(region.size % 4096 != 0);
    petroleum::page_table::constants::with_frame_allocator(|allocator| {
        allocator.free_contiguous_frames(region.phys, pages);
    });
    nitrogen::metrics::dma_released(region.size);
}

static AUDIO_CTX: spin::Mutex<Option<AudioContext>> = spin::Mutex::new(None);
pub fn init_audio() {
    let mut c = AudioContext::new();
    c.probe();
    *AUDIO_CTX.lock() = Some(c);
}
pub fn get_audio() -> &'static spin::Mutex<Option<AudioContext>> {
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

/// Queue an owned PCM playback request. The caller returns immediately; the
/// scheduler owns hardware submission and completion delivery.
pub fn enqueue_play_pcm(
    sample_rate: u32,
    channels: u8,
    bits_per_sample: u8,
    pcm: &[u8],
) -> Option<u64> {
    if pcm.is_empty() || pcm.len() > 8 * 1024 * 1024 {
        return None;
    }
    let mut queue = AUDIO_SQ.lock();
    if queue.len() >= 8 {
        log::warn!("Sound: playback request SQ full; dropping request");
        return None;
    }
    let request_id = NEXT_AUDIO_REQUEST.fetch_add(1, Ordering::Relaxed);
    queue.push_back((
        request_id,
        AudioRequest::PlayPcm {
            sample_rate,
            channels,
            bits_per_sample,
            pcm: pcm.to_vec(),
        },
    ));
    Some(request_id)
}

/// Submit queued audio requests from the scheduler context.
pub fn process_audio_submission_queue(budget: usize) {
    for _ in 0..budget {
        let Some((request_id, request)) = AUDIO_SQ.lock().pop_front() else {
            break;
        };
        let success = match request {
            AudioRequest::PlayPcm {
                sample_rate,
                channels,
                bits_per_sample,
                pcm,
            } => super::kernel::with_kernel_mut(|kernel| {
                kernel.audio.start_pcm_async(
                    request_id,
                    sample_rate,
                    channels,
                    bits_per_sample,
                    pcm,
                )
            })
            .unwrap_or(false),
        };
        if !success {
            AUDIO_CQ.lock().push_back(AudioCompletion {
                request_id,
                success: false,
            });
        }
    }
}

/// Advance the active HDA stream once and publish completed requests.
pub fn poll_audio_playback() {
    if let Some(completion) =
        super::kernel::with_kernel_mut(|kernel| kernel.audio.poll_async_playback()).flatten()
    {
        AUDIO_CQ.lock().push_back(AudioCompletion {
            request_id: completion.0,
            success: completion.1,
        });
    }
}

/// Consume audio CQs in scheduler context. Playback has no userspace waiter
/// yet, so completion is currently observable through the kernel log.
pub fn consume_audio_completion_queue(budget: usize) {
    for _ in 0..budget {
        let Some(completion) = AUDIO_CQ.lock().pop_front() else {
            break;
        };
        if completion.success {
            log::debug!(
                "Sound: playback request {} completed",
                completion.request_id
            );
        } else {
            log::warn!("Sound: playback request {} failed", completion.request_id);
        }
    }
}
