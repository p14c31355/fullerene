//! Sound / Audio subsystem for Fullerene OS.
//!
//! Provides PC speaker beep and Intel HD Audio (HDA) streaming playback.
//!
//! # Architecture
//!
//! ```text
//! PC Speaker (PIT mode 3 → square wave)
//! HDA controller (PCI class 0x04, subclass 0x03)
//!   → SD1 (first output stream) configured via MMIO registers
//!   → BDL with 2 entries → circular DMA buffer
//!   → LPIB polling for playback progress
//! ```

use core::sync::atomic::{AtomicBool, Ordering};
use nitrogen::pci::PciDevice;
use spin::Mutex;

// ── PC Speaker ─────────────────────────────────────────────────

pub fn pc_speaker_beep(frequency_hz: u32, duration_ms: u32) {
    if frequency_hz == 0 {
        return;
    }
    pc_speaker_on(frequency_hz);
    unsafe {
        for _ in 0..duration_ms * 1000 {
            core::hint::spin_loop();
        }
    }
    pc_speaker_off();
}

pub fn pc_speaker_on(frequency_hz: u32) {
    if frequency_hz == 0 {
        pc_speaker_off();
        return;
    }
    unsafe {
        let divisor = (1_193_182u32 / frequency_hz).min(65535) as u16;
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x43).write(0xB6);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write(divisor as u8);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write((divisor >> 8) as u8);
        let tmp = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(tmp | 0x03);
    }
}

pub fn pc_speaker_off() {
    unsafe {
        let tmp = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(tmp & !0x03);
    }
}

// ── HDA MMIO Register offsets ───────────────────────────────────

const GCAP: usize     = 0x0000;
const GCTL: usize     = 0x0008;
const STATESTS: usize = 0x000E;
const INTCTL: usize   = 0x0020;
const CORBLBASE: usize = 0x0040;
const CORBUBASE: usize = 0x0044;
const CORBWP: usize   = 0x0048;
const CORBRP: usize   = 0x004A;
const CORBCTL: usize  = 0x004C;
const CORBSIZE: usize = 0x004E;
const RIRBLBASE: usize = 0x0050;
const RIRBUBASE: usize = 0x0054;
const RIRBWP: usize   = 0x0058;
const RIRBCTL: usize  = 0x005C;
const RIRBSIZE: usize = 0x005E;

const SD_BASE: usize = 0x0080;
const SD_SIZE: usize = 0x0020;
const SD0_CTL: usize  = SD_BASE + 0x00;
const SD0_STS: usize  = SD_BASE + 0x03;
const SD0_LPIB: usize = SD_BASE + 0x04;
const SD0_CBL: usize  = SD_BASE + 0x08;
const SD0_LVI: usize  = SD_BASE + 0x0C;
const SD0_FMT: usize  = SD_BASE + 0x12;
const SD0_BDPL: usize = SD_BASE + 0x18;
const SD0_BDPU: usize = SD_BASE + 0x1C;

// SD1 = second stream (we use first output stream)
const SD1_CTL: usize  = SD_BASE + SD_SIZE + 0x00;
const SD1_STS: usize  = SD_BASE + SD_SIZE + 0x03;
const SD1_LPIB: usize = SD_BASE + SD_SIZE + 0x04;
const SD1_CBL: usize  = SD_BASE + SD_SIZE + 0x08;
const SD1_LVI: usize  = SD_BASE + SD_SIZE + 0x0C;
const SD1_FMT: usize  = SD_BASE + SD_SIZE + 0x12;
const SD1_BDPL: usize = SD_BASE + SD_SIZE + 0x18;
const SD1_BDPU: usize = SD_BASE + SD_SIZE + 0x1C;

// ── BDL entry ───────────────────────────────────────────────────

#[repr(C)]
struct BdlEntry {
    addr_lo: u32,
    addr_hi: u32,
    length: u32,
    flags: u32, // bit 0: IOC (interrupt on completion)
}

const DMA_BUF_SIZE: u32 = 16384; // 16 KB circular DMA buffer (~370 ms at 22 kHz mono)
const BDL_ENTRIES: u32 = 2;      // ring with 2 entries

// ── Global HDA state ────────────────────────────────────────────

static HDA_MMIO_BASE: Mutex<usize> = Mutex::new(0);
static HDA_READY: AtomicBool = AtomicBool::new(false);
/// Physical address of DMA buffer
static HDA_DMA_PHYS: Mutex<u64> = Mutex::new(0);
/// Virtual address of DMA buffer (stored as usize for Sync)
static HDA_DMA_VIRT: Mutex<usize> = Mutex::new(0);

// ── MMIO helpers ────────────────────────────────────────────────

unsafe fn read32(mmio: *mut u8, offset: usize) -> u32 {
    core::ptr::read_volatile(mmio.add(offset) as *const u32)
}

unsafe fn write32(mmio: *mut u8, offset: usize, value: u32) {
    core::ptr::write_volatile(mmio.add(offset) as *mut u32, value);
}

unsafe fn read16(mmio: *mut u8, offset: usize) -> u16 {
    core::ptr::read_volatile(mmio.add(offset) as *const u16)
}

unsafe fn write16(mmio: *mut u8, offset: usize, value: u16) {
    core::ptr::write_volatile(mmio.add(offset) as *mut u16, value);
}

unsafe fn read8(mmio: *mut u8, offset: usize) -> u8 {
    core::ptr::read_volatile(mmio.add(offset))
}

unsafe fn write8(mmio: *mut u8, offset: usize, value: u8) {
    core::ptr::write_volatile(mmio.add(offset), value);
}

// ── Init ────────────────────────────────────────────────────────

pub fn init() {
    let hda = match probe_hda() {
        Some(h) => h,
        None => {
            log::info!("Sound: No HDA controller (PC speaker only)");
            return;
        }
    };

    log::info!("Sound: HDA at {:02x}:{:02x}.{}, MMIO=0x{:x}",
        hda.bus, hda.device, hda.function, hda.mmio_base);

    // Map BAR0 (16K)
    let hda_virt = 0xFFFF8000_80000000usize;
    {
        let mut mm = crate::memory_management::get_memory_manager().lock();
        let mm = mm.as_mut().expect("MemoryManager not initialised");
        let flags = x86_64::structures::paging::PageTableFlags::NO_CACHE
            | x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE
            | x86_64::structures::paging::PageTableFlags::NO_EXECUTE;
        let phys = hda.mmio_base as usize;
        for i in 0..4 {
            if mm.safe_map_page(hda_virt + i * 4096, phys + i * 4096, flags).is_err() {
                log::error!("Sound: HDA MMIO map page {} failed", i);
                return;
            }
        }
    }
    *HDA_MMIO_BASE.lock() = hda_virt;

    // Reset controller
    unsafe {
        let mmio = hda_virt as *mut u8;

        write32(mmio, GCTL, 0); // clear CRST
        for _ in 0..1000 { core::hint::spin_loop(); }

        write32(mmio, GCTL, 1); // set CRST
        for _ in 0..10000 {
            if read32(mmio, GCTL) & 1 != 0 { break; }
        }

        write16(mmio, STATESTS, 0x000F); // clear state-change bits
        write32(mmio, INTCTL, 0);

        let gcap = read32(mmio, GCAP);
        log::info!("Sound: HDA GCAP=0x{:x}", gcap);
    }

    // Allocate DMA buffer (physically contiguous)
    let dma_pages = (DMA_BUF_SIZE as usize + 4095) / 4096;
    let dma_phys = {
        let fa = petroleum::page_table::constants::get_frame_allocator_mut();
        match fa.allocate_contiguous_frames(dma_pages) {
            Ok(addr) => addr,
            Err(_) => {
                log::error!("Sound: HDA DMA buffer allocation failed");
                return;
            }
        }
    };
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let dma_virt = (dma_phys + off) as *mut u8;

    // Clear DMA buffer
    unsafe { core::ptr::write_bytes(dma_virt, 0, DMA_BUF_SIZE as usize); }

    *HDA_DMA_PHYS.lock() = dma_phys;
    *HDA_DMA_VIRT.lock() = dma_virt as usize;

    // Place BDL at the start of DMA buffer; audio follows immediately
    let bdl_phys = dma_phys;
    let bdl_virt = dma_virt;
    let bdl_entry_size = core::mem::size_of::<BdlEntry>() as u64;
    let bdl_total = bdl_entry_size * BDL_ENTRIES as u64;
    let audio_phys = dma_phys + bdl_total;
    let audio_offset = bdl_total as u32;
    let audio_size = DMA_BUF_SIZE - audio_offset;
    let half = audio_size / 2;

    unsafe {
        let bdl = bdl_virt as *mut BdlEntry;
        (*bdl.add(0)).addr_lo = audio_phys as u32;
        (*bdl.add(0)).addr_hi = (audio_phys >> 32) as u32;
        (*bdl.add(0)).length = half;
        (*bdl.add(0)).flags = 1; // IOC

        (*bdl.add(1)).addr_lo = (audio_phys + half as u64) as u32;
        (*bdl.add(1)).addr_hi = ((audio_phys + half as u64) >> 32) as u32;
        (*bdl.add(1)).length = half;
        (*bdl.add(1)).flags = 1; // IOC
    }

    // Configure SD1 (first output stream)
    unsafe {
        let mmio = hda_virt as *mut u8;

        // Stop and reset SD1
        write8(mmio, SD1_CTL, 0); // clear RUN
        write8(mmio, SD1_CTL, 1); // set SRST
        for _ in 0..1000 { core::hint::spin_loop(); }
        write8(mmio, SD1_CTL, 0); // clear SRST
        for _ in 0..1000 { core::hint::spin_loop(); }

        // Clear stream status
        write8(mmio, SD1_STS, 0xFF);

        // Stream format: 16-bit, mono
        let fmt: u16 = (1u16 << 4) | 0;
        write16(mmio, SD1_FMT, fmt);

        // Cyclic Buffer Length
        write32(mmio, SD1_CBL, audio_size);

        // Last Valid Index = 1
        write16(mmio, SD1_LVI, BDL_ENTRIES as u16 - 1);

        // BDL pointer
        write32(mmio, SD1_BDPL, bdl_phys as u32);
        write32(mmio, SD1_BDPU, (bdl_phys >> 32) as u32);

        // Start stream: set RUN (bit 1)
        write8(mmio, SD1_CTL, 2);

        log::info!("Sound: HDA stream SD1 started ({} byte buffer)", audio_size);
    }

    HDA_READY.store(true, Ordering::Release);
    log::info!("Sound: HDA ready for playback");
}

// ── HDA Controller probe ────────────────────────────────────────

pub struct HdaController {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub mmio_base: u64,
}

fn probe_hda() -> Option<HdaController> {
    for bus in 0..=0u8 {
        for dev in 0..=31u8 {
            if let Some(device) = PciDevice::new(bus, dev, 0) {
                if device.class_code == 0x04 && device.subclass == 0x03 {
                    if let Some(bar0) = device.read_bar(0) {
                        return Some(HdaController {
                            bus,
                            device: dev,
                            function: 0,
                            mmio_base: bar0,
                        });
                    }
                }
            }
        }
    }
    None
}

// ═══════════════════════════════════════════════════════════════
// PCM playback API
// ═══════════════════════════════════════════════════════════════

/// Stream a PCM buffer via HDA, blocking until all data has been
/// consumed by the DMA engine.
///
/// `pcm_data`: raw s16le mono samples at 22 050 Hz.
pub fn hda_play_pcm(pcm_data: &[u8]) {
    if !HDA_READY.load(Ordering::Acquire) {
        return;
    }

    let mmio_base = *HDA_MMIO_BASE.lock();
    if mmio_base == 0 {
        return;
    }

    let dma_virt = *HDA_DMA_VIRT.lock() as *mut u8;
    if dma_virt.is_null() {
        return;
    }

    // Offset where audio starts in DMA buffer (after BDL entries)
    let audio_offset = (core::mem::size_of::<BdlEntry>() * BDL_ENTRIES as usize) as u32;
    let audio_size = DMA_BUF_SIZE - audio_offset;
    let half = audio_size / 2;

    let mmio = mmio_base as *mut u8;
    let mut src_offset: usize = 0;
    let total = pcm_data.len();

    while src_offset < total {
        let lpib = unsafe { read32(mmio, SD1_LPIB) };

        let (write_offset, write_max) = if lpib < half {
            (half, half)
        } else {
            (0, half)
        };

        let chunk = write_max as usize;
        let remaining = total - src_offset;
        let to_copy = chunk.min(remaining);

        if to_copy == 0 {
            break;
        }

        unsafe {
            let dst = dma_virt.add((audio_offset + write_offset) as usize);
            let src = pcm_data.as_ptr().add(src_offset);
            core::ptr::copy_nonoverlapping(src, dst, to_copy);
            if to_copy < chunk {
                core::ptr::write_bytes(dst.add(to_copy), 0, chunk - to_copy);
            }
        }

        src_offset += to_copy;

        // Wait for DMA to move past the half we just filled
        loop {
            let new_lpib = unsafe { read32(mmio, SD1_LPIB) };
            let in_first = new_lpib < half;
            let filled_second = write_offset >= half;
            if filled_second && in_first {
                break;
            }
            if !filled_second && !in_first {
                break;
            }
            core::hint::spin_loop();
        }
    }

    // Drain silence
    unsafe {
        let dst = dma_virt.add(audio_offset as usize);
        core::ptr::write_bytes(dst, 0, audio_size as usize);
    }
    for _ in 0..10000000 {
        core::hint::spin_loop();
    }
}

/// Check if HDA is available.
pub fn hda_available() -> bool {
    HDA_READY.load(Ordering::Acquire)
}

/// Feed raw PCM bytes into the HDA DMA ring buffer.
///
/// Returns the number of bytes actually queued (may be 0 if buffer is full).
/// This function is non‑blocking and should be called periodically
/// (e.g. once per video frame) to keep the DMA fed.
pub fn hda_feed_samples(samples: &[u8]) -> usize {
    if !HDA_READY.load(Ordering::Acquire) {
        return 0;
    }
    let mmio_base = *HDA_MMIO_BASE.lock();
    if mmio_base == 0 {
        return 0;
    }
    let dma_virt = *HDA_DMA_VIRT.lock() as *mut u8;
    if dma_virt.is_null() {
        return 0;
    }

    let audio_offset = (core::mem::size_of::<BdlEntry>() * BDL_ENTRIES as usize) as u32;
    let audio_size = DMA_BUF_SIZE - audio_offset;
    let half = audio_size / 2;

    let mmio = mmio_base as *mut u8;
    let lpib = unsafe { read32(mmio, SD1_LPIB) };

    // LPIB is relative to the start of the cyclic buffer (audio region).
    // Determine which half is free:
    let (write_offset, write_max) = if lpib < half {
        (half, half)
    } else {
        (0, half)
    };

    let to_copy = samples.len().min(write_max as usize);
    if to_copy == 0 {
        return 0;
    }

    unsafe {
        let dst = dma_virt.add((audio_offset + write_offset) as usize);
        core::ptr::copy_nonoverlapping(samples.as_ptr(), dst, to_copy);
        if to_copy < write_max as usize {
            core::ptr::write_bytes(dst.add(to_copy), 0, write_max as usize - to_copy);
        }
    }

    to_copy
}
