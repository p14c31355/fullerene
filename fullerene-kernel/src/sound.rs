//! Sound / Audio subsystem for Fullerene OS.
//!
//! ## Architecture
//!
//! - **PC Speaker** — simple PIT‑driven square‑wave beeper.
//! - **HDA** — Intel High Definition Audio, implemented in
//!   `nitrogen::hda`.  This module holds a global `HdaController`
//!   context and provides thin wrapper functions so existing
//!   callers (`badapple.rs`, `shell.rs`, etc.) continue to
//!   compile without changes.

use nitrogen::hda::controller::HdaDiagInfo;
use nitrogen::hda::dma::{DmaRegion, DMA_BUF_SIZE, BDL_ENTRIES};
use nitrogen::hda::HdaController;
use spin::Mutex;

// ── PC Speaker ───────────────────────────────────────────────────

pub fn pc_speaker_on(frequency_hz: u32) {
    if frequency_hz == 0 {
        pc_speaker_off();
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

pub fn pc_speaker_off() {
    unsafe {
        let t = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(t & !0x03);
    }
}

// ── Global HDA context ───────────────────────────────────────────

/// Global HDA controller instance.  `None` until `init()` succeeds.
static HDA_CTRL: Mutex<Option<HdaController>> = Mutex::new(None);

/// Cached HDA diagnostic info, preserved for shell inspection.
pub static HDA_DIAG: Mutex<HdaDiagInfo> = Mutex::new(HdaDiagInfo {
    gcap: 0,
    gcap64: false,
    corb_phys: 0,
    rirb_phys: 0,
    states_after_crst: 0,
    populated: false,
});

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
    // Size is pages * 4096; callers compute the actual buffer layout.
    Some(DmaRegion {
        phys,
        virt,
        size: pages * 4096,
    })
}

// ── Public API (mirrors old sound.rs function signatures) ────────

/// Probe PCI buses for an HDA controller and store the MMIO address.
/// Does NOT start CORB/RIRB or the DMA engine — that happens lazily
/// on first `hda_write_direct` / `hda_feed_samples` call.
pub fn init() {
    let phys_offset = petroleum::common::memory::get_physical_memory_offset() as u64;
    match HdaController::probe(phys_offset) {
        Some((bus, dev, func, bar0)) => {
            log::info!(
                "Sound: HDA at {:04x}:{:02x}.{}, BAR0=0x{:x}",
                bus, dev, func, bar0
            );
            let mmio = (bar0 + phys_offset) as *mut u8;
            let ctrl = HdaController::new(mmio, bar0);
            *HDA_CTRL.lock() = Some(ctrl);
        }
        None => log::info!("Sound: No HDA (PC speaker only)"),
    }
}

/// Returns `true` when an HDA controller was found during `init()`.
pub fn hda_available() -> bool {
    HDA_CTRL.lock().is_some()
}

/// Force a VM exit on QEMU/KVM so the device model advances HDA DMA state.
pub fn hda_tick() {
    HdaController::tick_vm_exit();
}

/// Lazy initialisation: bring up CORB/RIRB, enumerate codec, start DMA.
///
/// Initialisation is attempted exactly once.  Even if the first attempt
/// fails (e.g. GCAP invalid, no output streams, codec not found), we
/// mark it as done so that repeated `hda_write_direct` / `hda_feed_samples`
/// calls do not leak DMA pages on each retry.
fn hda_init() {
    use core::sync::atomic::{AtomicBool, Ordering};

    static INIT_ATTEMPTED: AtomicBool = AtomicBool::new(false);
    if INIT_ATTEMPTED.swap(true, Ordering::Relaxed) {
        return;
    }

    let mut guard = HDA_CTRL.lock();
    let ctrl = match guard.as_mut() {
        Some(c) => c,
        None => return,
    };
    if ctrl.is_ready() {
        return;
    }

    // Allocate CORB/RIRB DMA pages
    let Some(corb_region) = alloc_dma_region(1) else {
        return;
    };
    let Some(rirb_region) = alloc_dma_region(1) else {
        return;
    };

    // Allocate DMA buffer page(s)
    let dma_pages = (DMA_BUF_SIZE as usize + 4095) / 4096;
    let Some(dma_region) = alloc_dma_region(dma_pages) else {
        return;
    };

    // Initialise the controller (CORB/RIRB + codec + DMA engine)
    let init_ok = unsafe {
        ctrl.init(&corb_region, &rirb_region, &dma_region)
    };
    if !init_ok {
        log::error!("Sound: HDA controller init failed (is_ready={}, GCAP=0x{:x})",
                    ctrl.is_ready(), unsafe {
                        core::ptr::read_volatile(ctrl.mmio().add(0x0000) as *const u32)
                    });
        return;
    }

    // Populate diagnostic cache
    let mut diag = HDA_DIAG.lock();
    let gcap_raw = unsafe {
        let mmio = ctrl.mmio();
        core::ptr::read_volatile(mmio.add(0x0000) as *const u32)
    };
    diag.gcap = gcap_raw;
    diag.gcap64 = gcap_raw & 1 != 0;
    diag.corb_phys = corb_region.phys;
    diag.rirb_phys = rirb_region.phys;
    diag.populated = true;
}

/// Write PCM bytes at a specific offset into the DMA buffer
/// (used for pre‑fill).  Returns bytes written.
pub fn hda_write_direct(offset: u32, samples: &[u8]) -> usize {
    hda_init();
    let guard = HDA_CTRL.lock();
    match guard.as_ref() {
        Some(ctrl) if ctrl.is_ready() => ctrl.write_at(offset, samples),
        _ => 0,
    }
}

/// Reset LPIB tracking so DMA is assumed to start at half 0
/// (call after pre‑filling both halves).
pub fn hda_reset_prefill_tracking() {
    let guard = HDA_CTRL.lock();
    if let Some(ctrl) = guard.as_ref() {
        if ctrl.is_ready() {
            ctrl.reset_prefill_tracking();
        }
    }
}

/// Feed PCM samples into the DMA ring buffer.  Returns bytes written.
pub fn hda_feed_samples(samples: &[u8]) -> usize {
    hda_init();
    let guard = HDA_CTRL.lock();
    match guard.as_ref() {
        Some(ctrl) if ctrl.is_ready() => ctrl.feed_samples(samples),
        _ => 0,
    }
}

/// High‑level PCM feed with offset tracking.
/// Advances `*pcm_off` and returns bytes fed.
#[inline]
pub fn hda_feed_pcm(pcm: &[u8], pcm_off: &mut usize, pcm_total: usize, half: usize) -> usize {
    let off = *pcm_off;
    if off >= pcm_total {
        return 0;
    }
    let rem = pcm_total - off;
    let end = (off + rem.min(half)).min(pcm_total);
    let fed = hda_feed_samples(&pcm[off..end]);
    if fed > 0 {
        *pcm_off += fed;
    }
    fed
}

/// Poll for half‑buffer completion (no timeout — never blocks forever
/// due to internal TSC watchdog in `poll_delay`).
pub fn hda_poll() {
    let guard = HDA_CTRL.lock();
    if let Some(ctrl) = guard.as_ref() {
        if ctrl.is_ready() {
            // Use the old behaviour: ~100 ms at 3 GHz
            let deadline =
                unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(300_000_000);
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
    }
}

/// Poll with optional TSC timeout.  Returns `true` when data was
/// fed, `false` on timeout / not ready.
pub fn hda_poll_block(timeout_tsc: Option<u64>) -> bool {
    let guard = HDA_CTRL.lock();
    match guard.as_ref() {
        Some(ctrl) if ctrl.is_ready() => ctrl.poll(timeout_tsc),
        _ => false,
    }
}

/// TSC‑based delay with periodic HDA poll (used for silence drain).
pub fn hda_poll_delay(tsc_per_ms: u64, ms: u64) {
    let deadline =
        unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(tsc_per_ms.saturating_mul(ms));
    while unsafe { core::arch::x86_64::_rdtsc() } < deadline {
        hda_poll();
        core::hint::spin_loop();
    }
}

/// Return the total number of PCM bytes the HDA hardware has
/// consumed (played back) since the stream was started.
pub fn hda_playback_progress() -> Option<u64> {
    let guard = HDA_CTRL.lock();
    guard.as_ref().and_then(|c| c.playback_progress())
}

/// Feed silence into the HDA half‑buffer.
pub fn hda_feed_silence(half: usize) -> usize {
    let guard = HDA_CTRL.lock();
    match guard.as_ref() {
        Some(ctrl) if ctrl.is_ready() => ctrl.feed_silence(half),
        _ => 0,
    }
}