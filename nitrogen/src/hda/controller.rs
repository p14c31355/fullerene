//! `HdaController` — the main context struct for an HDA PCI device.
//!
//! This struct owns all HDA state that was previously scattered across
//! global `Mutex<...>` statics in `sound.rs`.  The caller creates one
//! instance per HDA controller and passes the necessary DMA regions in.

use crate::hda::codec::CodecGraph;
use crate::hda::corb::CorbEngine;
use crate::hda::dma::{DmaEngine, DmaRegion};
use crate::hda::route::RouteFinder;
use crate::pci::{PciConfigSpace, PciDevice};
use core::sync::atomic::{AtomicBool, Ordering};

// ── HDA MMIO register offsets ────────────────────────────────────
const GCAP: usize = 0x0000;
const STATESTS: usize = 0x000E;
const INTCTL: usize = 0x0020;
const SD_BASE: usize = 0x0080;
const SD_SIZE: usize = 0x0020;

/// A fully‑initialised HDA controller context.
pub struct HdaController {
    /// MMIO virtual address (BAR0 + phys‑offset).
    mmio: *mut u8,
    /// Physical BAR0 address (unused, kept for future diagnostics).
    _phys: u64,
    /// CORB/RIRB verb engine.
    corb: CorbEngine,
    /// DMA playback engine.
    dma: DmaEngine,
    /// GCAP value (cached).
    gcap: u32,
    /// Whether the controller has been fully initialised.
    ready: AtomicBool,
}

// SAFETY: In the kernel, MMIO and DMA buffer pointers are globally
// accessible across all CPUs (single address space).  The `Mutex`
// in `sound.rs` provides mutual exclusion for mutable operations.
unsafe impl Send for HdaController {}
unsafe impl Sync for HdaController {}

/// Diagnostic information about the HDA controller, preserved across
/// initialisation calls for external inspection (shell, logging).
#[derive(Clone, Copy, Debug)]
pub struct HdaDiagInfo {
    pub gcap: u32,
    pub gcap64: bool,
    pub corb_phys: u64,
    pub rirb_phys: u64,
    pub states_after_crst: u16,
    pub populated: bool,
}

impl HdaController {
    /// Probe all PCI buses for an HDA controller (class 0x04, subclass 0x03).
    ///
    /// Returns `(bus, dev, func, BAR0)` for the preferred controller —
    /// the one with a codec connected (STATESTS bit 0 set).  Falls back
    /// to the last HDA seen if no codec is detected.
    ///
    /// `phys_offset` is the physical‑memory offset (virtual = phys + offset).
    pub fn probe(phys_offset: u64) -> Option<(u8, u8, u8, u64)> {
        /// Check whether a PCI bus exists.
        fn bus_exists(bus: u8) -> bool {
            PciConfigSpace::read_config_word(bus, 0, 0, 0) != 0xFFFF
        }

        /// Identify Intel integrated-GPU HDMI audio controllers.
        ///
        /// These controllers expose only digital display-port codecs and
        /// cannot drive an analog speaker.  When a PCH analog controller is
        /// also present we must prefer it; otherwise playback silently routes
        /// to a port nothing is listening on.  The list below is the set of
        /// Intel HDMI audio PCI device IDs observed across Broadwell, Haswell,
        /// Skylake/Kaby Lake, and Coffee Lake platforms.
        fn is_intel_hdmi_audio(vendor_id: u16, device_id: u16) -> bool {
            if vendor_id != 0x8086 {
                return false;
            }
            matches!(
                device_id,
                0x0a0c
                    | 0x0c0c
                    | 0x0d0c
                    | 0x160c
                    | 0x190c
                    | 0x1c0c
                    | 0x1d0c
                    | 0x1e0c
                    | 0x1f0c
                    | 0x280c
                    | 0x281c
                    | 0x9c70
                    | 0x9d70
                    | 0xa170
                    | 0x490d
                    | 0x4b55
                    | 0x5c70
                    | 0x7af0
                    | 0x51c8
                    | 0x51cb
                    | 0x54c8
                    | 0x7a70
            )
        }

        let mut analog_candidate: Option<(u8, u8, u8, u64)> = None;
        let mut hdmi_candidate: Option<(u8, u8, u8, u64)> = None;
        let mut analog_codec_seen = false;
        let mut hdmi_codec_seen = false;

        for bus in 0..=255u8 {
            if bus > 0 && !bus_exists(bus) {
                continue;
            }
            for d in 0..=31u8 {
                let Some(dev) = PciDevice::new(bus, d, 0) else {
                    continue;
                };
                if dev.class_code != 0x04 || dev.subclass != 0x03 {
                    continue;
                }
                let is_hdmi = is_intel_hdmi_audio(dev.vendor_id, dev.device_id);
                let kind_label = if is_hdmi { "HDMI" } else { "analog" };
                let Some(bar0) = dev.read_bar(0) else {
                    continue;
                };
                dev.enable_memory_access();
                let Some(mut cfg) = PciConfigSpace::read_from_device(bus, d, 0) else {
                    continue;
                };
                // Ensure Memory Space + Bus Mastering
                cfg.command |= 0x0006;
                let v = (cfg.status as u32) << 16 | (cfg.command as u32);
                PciConfigSpace::write_config_dword(&mut cfg, bus, d, 0, 0x04, v);
                let cmd_after = PciConfigSpace::read_config_word(bus, d, 0, 4);

                // Quick MMIO probe
                let mmio = (bar0 + phys_offset) as *mut u8;
                let gcap = unsafe { core::ptr::read_volatile(mmio.add(GCAP) as *const u32) };
                let states = unsafe { core::ptr::read_volatile(mmio.add(STATESTS) as *const u16) };

                log::info!(
                    "HDA({}): {:04x}:{:02x}.{} [{:#06x}:{:#06x}] BAR0=0x{:016x} GCAP=0x{:08x} STATESTS=0x{:04x} CMD=0x{:04x}",
                    kind_label,
                    bus,
                    d,
                    0,
                    dev.vendor_id,
                    dev.device_id,
                    bar0,
                    gcap,
                    states,
                    cmd_after,
                );

                let codec_present = states & 0x0001 != 0;
                if codec_present {
                    log::info!(
                        "HDA({}): codec connected at {:04x}:{:02x}.{}",
                        kind_label,
                        bus,
                        d,
                        0
                    );
                }

                // Prefer the analog (PCH) controller over the GPU HDMI
                // controller.  STATESTS is unreliable before CRST, so do not
                // gate selection on codec presence: the analog codec may only
                // appear after the controller is reset in `init`.  HDMI is
                // tracked only as a fallback so that display-audio-only
                // systems still produce sound through their monitor.
                if is_hdmi {
                    if hdmi_candidate.is_none() {
                        hdmi_candidate = Some((bus, d, 0, bar0));
                        if codec_present {
                            hdmi_codec_seen = true;
                        }
                    }
                } else {
                    // Keep the last analog controller (PCH codecs are usually
                    // at higher device numbers than the GPU HDMI audio).
                    analog_candidate = Some((bus, d, 0, bar0));
                    if codec_present {
                        analog_codec_seen = true;
                    }
                }
            }
        }

        if let Some(ref b) = analog_candidate {
            log::info!(
                "HDA(analog): selecting {:04x}:{:02x}.{} (codec_present={})",
                b.0,
                b.1,
                b.2,
                analog_codec_seen
            );
            return analog_candidate;
        }
        if let Some(ref b) = hdmi_candidate {
            log::info!(
                "HDA(HDMI): selecting {:04x}:{:02x}.{} (codec_present={}) — no analog controller found",
                b.0,
                b.1,
                b.2,
                hdmi_codec_seen
            );
            return hdmi_candidate;
        }
        None
    }

    /// Create a new `HdaController` in the uninitialised state.
    ///
    /// After creation, call `init()` to bring up CORB/RIRB, enumerate
    /// the codec, configure routing, and start the DMA engine.
    pub fn new(mmio: *mut u8, phys: u64) -> Self {
        Self {
            mmio,
            _phys: phys,
            corb: CorbEngine::new(core::ptr::null_mut(), core::ptr::null_mut(), 256),
            dma: DmaEngine::new(0),
            gcap: 0,
            ready: AtomicBool::new(false),
        }
    }

    /// Full controller initialisation.
    ///
    /// This method:
    /// 1. Reads GCAP / STATESTS, caches diagnostics.
    /// 2. Initialises CORB/RIRB via `corb_region` / `rirb_region`.
    /// 3. Enumerates the codec graph and finds the DAC→Pin route.
    /// 4. Configures the codec and starts the DMA engine.
    ///
    /// # Safety
    ///
    /// All `DmaRegion` pointers must point to valid, physically‑
    /// contiguous, zeroed memory.  `mmio` must be valid.
    pub unsafe fn init(
        &mut self,
        corb_region: &DmaRegion,
        rirb_region: &DmaRegion,
        dma_region: &DmaRegion,
    ) -> bool {
        if self.ready.load(Ordering::Acquire) {
            return true;
        }

        let mmio = self.mmio;

        // Validate GCAP
        let gcap = unsafe { mmio_read32(mmio, GCAP) };
        if gcap == 0 || gcap == 0xFFFF_FFFF {
            log::warn!("HDA: GCAP invalid (0x{:08x})", gcap);
            return false;
        }
        self.gcap = gcap;
        log::info!("HDA: GCAP=0x{:x}", gcap);

        let gcap64 = gcap & 1 != 0;
        let iss = (gcap >> 8) & 0xF;
        let oss = (gcap >> 12) & 0xF;

        // ── Controller reset via GCTL (offset 0x08) ──────────────
        // The HDA specification requires a CRST cycle before the
        // codec becomes accessible on the link.  Without this, CORB
        // verbs time out (RIRBWP never advances).
        //   1) Write 0 → wait for CRST bit to read back as 0
        //   2) Write 1 → wait for CRST bit to read back as 1
        //   3) Wait for codec to signal presence on the link (STATESTS)
        const GCTL: usize = 0x0008;
        unsafe { mmio_write32(mmio, GCTL, 0) };
        if crate::timing::wait_timeout_us(50_000, || unsafe { mmio_read32(mmio, GCTL) } & 1 == 0)
            .is_err()
        {
            log::warn!("HDA: GCTL CRST=0 timeout");
            return false;
        }
        unsafe { mmio_write32(mmio, GCTL, 1) };
        if crate::timing::wait_timeout_us(50_000, || unsafe { mmio_read32(mmio, GCTL) } & 1 != 0)
            .is_err()
        {
            log::warn!("HDA: GCTL CRST=1 timeout");
            return false;
        }
        log::info!("HDA: GCTL CRST cycle complete");

        // After CRST, wait for the codec to signal presence on the
        // link.  Real codecs can take up to 1 ms.  On KVM each MMIO
        // read causes a VM exit, giving QEMU a chance to advance the
        // device model state.
        if crate::timing::wait_timeout_us(200_000, || {
            let sts = unsafe { mmio_read16(mmio, STATESTS) };
            let ready = sts & 0x0001 != 0;
            if !ready {
                // Force VM exit so QEMU/KVM can bring the codec online
                HdaController::tick_vm_exit();
            }
            ready
        })
        .is_err()
        {
            log::warn!(
                "HDA: codec not detected after CRST (STATESTS=0x{:04x})",
                unsafe { mmio_read16(mmio, STATESTS) }
            );
            // Do not abort — some codecs appear only after CORB init
        } else {
            log::info!("HDA: codec presence detected after CRST");
        }

        // Capture STATESTS before clearing
        let sts_pre_clear = unsafe { mmio_read16(mmio, STATESTS) };
        unsafe { mmio_write16(mmio, STATESTS, 0x000F) };
        unsafe { mmio_write32(mmio, INTCTL, 0) };

        log::info!(
            "HDA: STATESTS = 0x{:04x} (SDIN0={} SDIN1={} SDIN2={} SDIN3={})",
            sts_pre_clear,
            if sts_pre_clear & 0x0001 != 0 {
                1u8
            } else {
                0u8
            },
            if sts_pre_clear & 0x0002 != 0 {
                1u8
            } else {
                0u8
            },
            if sts_pre_clear & 0x0004 != 0 {
                1u8
            } else {
                0u8
            },
            if sts_pre_clear & 0x0008 != 0 {
                1u8
            } else {
                0u8
            },
        );
        log::info!("HDA: ISS={} OSS={} 64bit={}", iss, oss, gcap64);

        if oss == 0 {
            log::warn!("HDA: no output streams");
            return false;
        }

        // Determine CORB size from CORBSIZE register (offset 0x4E)
        // Read CORBSZCAP field (bits [7:4]) which indicates supported sizes
        let corbsize_reg = unsafe { mmio_read8(mmio, 0x004E) };
        // CORBSZCAP is the upper nibble; CORBSIZE is the lower two bits.
        let corb_szcap = (corbsize_reg >> 4) & 0x0F;
        let corb_entries: usize = if corb_szcap & 0x4 != 0 {
            256
        } else if corb_szcap & 0x2 != 0 {
            16
        } else if corb_szcap & 0x1 != 0 {
            2
        } else {
            log::warn!(
                "HDA: CORBSZCAP invalid (0x{:x}), defaulting to 2",
                corb_szcap
            );
            2
        };

        // Init CORB/RIRB
        if unsafe { !CorbEngine::init(mmio, corb_region, rirb_region, corb_entries) } {
            return false;
        }

        // Set up the CorbEngine instance
        self.corb = CorbEngine::new(
            corb_region.virt as *mut u32,
            rirb_region.virt as *mut u64,
            corb_entries,
        );

        // Enumerate codec
        let codec_addr: u8 = 0;
        let graph = unsafe { CodecGraph::enumerate(mmio, &self.corb, codec_addr) };
        log::info!("HDA: codec graph enumerated");

        // Find route
        let stream_tag: u8 = 1;
        let route_found = if let Some((dac, pin)) =
            unsafe { RouteFinder::find_speaker_route(mmio, &self.corb, &graph) }
        {
            log::info!("HDA: route DAC=0x{:x} → Pin=0x{:x}", dac, pin);
            unsafe { RouteFinder::configure_route(mmio, &self.corb, &graph, dac, pin, stream_tag) };
            true
        } else {
            log::warn!("HDA: no speaker route found");
            false
        };

        // Without a connected DAC/pin route the stream cannot produce a
        // boundary interrupt. Do not start DMA in that case: callers would
        // otherwise wait forever for playback progress.
        if !route_found {
            log::warn!("HDA: no speaker route found — dumping codec inventory for diagnosis");
            unsafe { crate::hda::diagnostics::dump_codec_inventory(mmio, &self.corb, &graph) };
            return false;
        }

        // Init DMA engine
        let sd_offset = SD_BASE + (iss as usize) * SD_SIZE;
        self.dma = DmaEngine::new(sd_offset);
        unsafe { self.dma.init(mmio, dma_region, stream_tag) };

        self.ready.store(true, Ordering::Release);
        log::info!("HDA: controller ready");
        true
    }

    /// Whether the controller is fully initialised.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// Get the MMIO base pointer.
    pub fn mmio(&self) -> *mut u8 {
        self.mmio
    }

    /// Get the CORB engine.
    pub fn corb(&self) -> &CorbEngine {
        &self.corb
    }

    /// Get the DMA engine.
    pub fn dma(&self) -> &DmaEngine {
        &self.dma
    }

    /// Mutable access to the DMA engine.
    pub fn dma_mut(&mut self) -> &mut DmaEngine {
        &mut self.dma
    }

    /// Force a VM exit on QEMU/KVM so the device model advances HDA
    /// DMA state.  Reads the PIC master IMR (port 0x21) — I/O port
    /// accesses always trap on KVM, unlike MMIO reads which may be
    /// satisfied via EPT.
    pub fn tick_vm_exit() {
        unsafe {
            x86_64::instructions::port::PortReadOnly::<u8>::new(0x21).read();
        }
    }

    /// Convenience: feed PCM samples into the DMA engine.
    /// Returns bytes written.
    pub fn feed_samples(&self, samples: &[u8]) -> usize {
        // Safety: self.mmio is valid if the controller was initialised
        unsafe { self.dma.feed_samples(self.mmio, samples) }
    }

    /// Convenience: write PCM at a specific DMA buffer offset.
    pub fn write_at(&self, offset: u32, samples: &[u8]) -> usize {
        self.dma.write_at(offset, samples)
    }

    /// Clear a region of the DMA buffer without allocating a temporary
    /// silence buffer. Used to pad the final playback half.
    pub fn clear_at(&self, offset: u32, len: usize) -> usize {
        self.dma.clear_at(offset, len)
    }

    /// Convenience: reset LPIB prefill tracking.
    pub fn reset_prefill_tracking(&self) {
        self.dma.reset_prefill_tracking();
    }

    /// Convenience: poll for half‑buffer completion.
    pub fn poll(&self, timeout_tsc: Option<u64>) -> bool {
        // Safety: self.mmio is valid
        unsafe { self.dma.poll(self.mmio, timeout_tsc) }
    }

    /// Convenience: poll delay with periodic DMA poll.
    pub fn poll_delay(&self, tsc_per_ms: u64, ms: u64) {
        // Safety: self.mmio is valid
        unsafe { self.dma.poll_delay(self.mmio, tsc_per_ms, ms) }
    }

    /// Convenience: feed silence.
    pub fn feed_silence(&self, half: usize) -> usize {
        // Safety: self.mmio is valid if the controller was initialised
        unsafe { self.dma.feed_silence(self.mmio, half) }
    }

    /// Convenience: playback progress in bytes.
    pub fn playback_progress(&self) -> Option<u64> {
        // Safety: self.mmio is valid
        unsafe { self.dma.playback_progress_bytes(self.mmio) }
    }

    /// Read stream state for diagnostics.
    pub fn debug_stream_status(&self) -> (u32, u8, u32) {
        // Safety: self.mmio is valid
        unsafe { self.dma.debug_status(self.mmio) }
    }
}

crate::make_mmio_helpers!();
