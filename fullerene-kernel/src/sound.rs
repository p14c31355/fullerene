//! Sound / Audio subsystem for Fullerene OS.
//!
//! Provides PC speaker beep and Intel HD Audio (HDA) streaming playback.
//!
//! ```text
//! PC Speaker (PIT mode 3 → square wave)
//! HDA controller (PCI class 0x04, subclass 0x03)
//!   → SD configured via MMIO registers
//!   → BDL with 2 entries → circular DMA buffer
//!   → LPIB polling for playback progress
//! ```

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use nitrogen::pci::PciDevice;
use spin::Mutex;

// ── PC Speaker ─────────────────────────────────────────────────

pub fn pc_speaker_on(frequency_hz: u32) {
    if frequency_hz == 0 { pc_speaker_off(); return; }
    let divisor = (1_193_182u32 / frequency_hz).min(65535) as u16;
    unsafe {
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x43).write(0xB6);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write(divisor as u8);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write((divisor>>8) as u8);
        let t = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(t|0x03);
    }
}

pub fn pc_speaker_off() {
    unsafe {
        let t = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(t&!0x03);
    }
}

// ── HDA Registers ──────────────────────────────────────────────

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
const SD_CTL: usize  = 0x00;
const SD_STS: usize  = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize  = 0x08;
const SD_LVI: usize  = 0x0C;
const SD_FMT: usize  = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;

#[repr(C)]
struct BdlEntry { addr_lo: u32, addr_hi: u32, length: u32, flags: u32 }

const DMA_BUF_SIZE: u32 = 32768;  // 32 KB → ~340 ms at 48kHz
const BDL_ENTRIES: u32 = 2;

static HDA_PHYS: Mutex<u64> = Mutex::new(0);
static HDA_READY: AtomicBool = AtomicBool::new(false);
static HDA_VIRT: Mutex<usize> = Mutex::new(0);
static HDA_DMA: Mutex<usize> = Mutex::new(0);
static HDA_AUDIO_OFF: Mutex<u32> = Mutex::new(0);
static HDA_AUDIO_SZ: Mutex<u32> = Mutex::new(0);
static HDA_HALF: Mutex<u32> = Mutex::new(0);
static HDA_SD: Mutex<usize> = Mutex::new(0);
/// Last LPIB value we've written past (used to avoid double‑write)
static HDA_LAST_LPIB: AtomicU64 = AtomicU64::new(u64::MAX);

unsafe fn r32(m: *mut u8, o: usize) -> u32 { core::ptr::read_volatile(m.add(o) as *const u32) }
unsafe fn w32(m: *mut u8, o: usize, v: u32) { core::ptr::write_volatile(m.add(o) as *mut u32, v); }
unsafe fn r16(m: *mut u8, o: usize) -> u16 { core::ptr::read_volatile(m.add(o) as *const u16) }
unsafe fn w16(m: *mut u8, o: usize, v: u16) { core::ptr::write_volatile(m.add(o) as *mut u16, v); }
unsafe fn r8(m: *mut u8, o: usize) -> u8 { core::ptr::read_volatile(m.add(o)) }
unsafe fn w8(m: *mut u8, o: usize, v: u8) { core::ptr::write_volatile(m.add(o), v); }

// ── Init ────────────────────────────────────────────────────────

pub fn init() {
    if let Some(h) = probe_hda() {
        log::info!("Sound: HDA at {:02x}:{:02x}.{}, MMIO=0x{:x}", h.bus, h.device, h.function, h.mmio);
        *HDA_PHYS.lock() = h.mmio;
    } else {
        log::info!("Sound: No HDA (PC speaker only)");
    }
}

struct HdaInfo { bus: u8, device: u8, function: u8, mmio: u64 }

fn probe_hda() -> Option<HdaInfo> {
    for d in 0..=31u8 {
        let Some(dev) = PciDevice::new(0, d, 0) else { continue };
        if dev.class_code != 0x04 || dev.subclass != 0x03 { continue }
        let bar0 = dev.read_bar(0)?;
        dev.enable_memory_access();
        use nitrogen::pci::PciConfigSpace;
        let Some(mut cfg) = PciConfigSpace::read_from_device(0, d, 0) else { continue };
        cfg.command |= 0x0004; // bus mastering
        let v = (cfg.status as u32) << 16 | (cfg.command as u32);
        PciConfigSpace::write_config_dword(&mut cfg, 0, d, 0, 0x04, v);
        return Some(HdaInfo { bus:0, device:d, function:0, mmio:bar0 });
    }
    None
}

// ── Deferred init ───────────────────────────────────────────────

fn hda_init() {
    if HDA_READY.load(Ordering::Acquire) { return }
    let phys = *HDA_PHYS.lock();
    if phys == 0 { return }

    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let virt = (phys + off) as usize;
    *HDA_VIRT.lock() = virt;

    // Sanity
    let gctest = unsafe { r32(virt as *mut u8, GCAP) };
    if gctest == 0 || gctest == 0xFFFF_FFFF {
        log::warn!("Sound: HDA GCAP invalid, disabling");
        *HDA_PHYS.lock() = 0; return;
    }
    log::info!("Sound: HDA GCAP=0x{:x}", gctest);

    // Reset
    unsafe {
        let m = virt as *mut u8;
        w32(m, GCTL, 0);
        for _ in 0..2000 { core::hint::spin_loop(); }
        w32(m, GCTL, 1);
        for _ in 0..20000 { if r32(m, GCTL)&1 != 0 { break } }
        w16(m, STATESTS, 0x000F);
        w32(m, INTCTL, 0);

        let gcap = r32(m, GCAP);
        let iss = ((gcap>>8)&0xF) as usize;
        let oss = ((gcap>>12)&0xF) as usize;
        log::info!("Sound: ISS={} OSS={}", iss, oss);
        if oss == 0 { log::warn!("Sound: no output streams"); return }
        *HDA_SD.lock() = SD_BASE + iss * SD_SIZE;
    }

    // DMA buffer
    let pages = (DMA_BUF_SIZE as usize + 4095) / 4096;
    let dma_phys = match petroleum::page_table::constants::get_frame_allocator_mut()
        .allocate_contiguous_frames(pages)
    { Ok(a) => a, Err(_) => { log::error!("Sound: DMA alloc fail"); return } };
    let dma_virt = (dma_phys + off) as *mut u8;
    unsafe { core::ptr::write_bytes(dma_virt, 0, DMA_BUF_SIZE as usize); }
    *HDA_DMA.lock() = dma_virt as usize;

    let bdl_sz = (core::mem::size_of::<BdlEntry>() * BDL_ENTRIES as usize) as u64;
    let audio_phys = dma_phys + bdl_sz;
    let audio_off = bdl_sz as u32;
    let audio_sz = DMA_BUF_SIZE - audio_off;
    let half = audio_sz / 2;
    *HDA_AUDIO_OFF.lock() = audio_off;
    *HDA_AUDIO_SZ.lock() = audio_sz;
    *HDA_HALF.lock() = half;

    unsafe {
        let bdl = dma_virt as *mut BdlEntry;
        (*bdl.add(0)) = BdlEntry { addr_lo:audio_phys as u32, addr_hi:(audio_phys>>32) as u32, length:half, flags:1 };
        (*bdl.add(1)) = BdlEntry { addr_lo:(audio_phys+half as u64) as u32, addr_hi:((audio_phys+half as u64)>>32) as u32, length:half, flags:1 };
    }

    // Configure SD
    unsafe {
        let m = virt as *mut u8;
        let sd = *HDA_SD.lock();

        // Reset stream
        let ctl = r8(m, sd + SD_CTL);
        w8(m, sd + SD_CTL, ctl & !0x02); // clear RUN
        w8(m, sd + SD_CTL, 0x01); // SRST
        for _ in 0..2000 { core::hint::spin_loop(); }
        w8(m, sd + SD_CTL, 0x00); // clear SRST
        for _ in 0..2000 { core::hint::spin_loop(); }
        w8(m, sd + SD_STS, 0xFF);

        // Format: 16-bit mono, 48 kHz base
        w16(m, sd + SD_FMT, (1u16<<4) | 0);
        w32(m, sd + SD_CBL, audio_sz);
        w16(m, sd + SD_LVI, BDL_ENTRIES as u16 - 1);
        w32(m, sd + SD_BDPL, dma_phys as u32);
        w32(m, sd + SD_BDPU, (dma_phys >> 32) as u32);

        // Stream tag = 1, RUN
        let ctl_val: u32 = (1u32 << 20) | 0x02; // tag=1, RUN
        w32(m, sd + SD_CTL, ctl_val);

        log::info!("Sound: HDA stream started ({} B audio)", audio_sz);
    }

    HDA_READY.store(true, Ordering::Release);
}

// ── Public API ──────────────────────────────────────────────────

pub fn hda_available() -> bool { *HDA_PHYS.lock() != 0 }

/// Feed PCM samples to HDA.  Non‑blocking.  Writes at most one
/// half‑buffer of data, then returns.  Call `hda_poll()` before
/// each call to wait until space is available.
pub fn hda_feed_samples(samples: &[u8]) -> usize {
    hda_init();
    if !HDA_READY.load(Ordering::Acquire) { return 0 }

    let virt = *HDA_VIRT.lock();
    if virt == 0 { return 0 }
    let mmio = virt as *mut u8;
    let dma = *HDA_DMA.lock() as *mut u8;
    let audio_off = *HDA_AUDIO_OFF.lock();
    let half = *HDA_HALF.lock();
    let sd = *HDA_SD.lock();

    let lpib = unsafe { r32(mmio, sd + SD_LPIB) };
    let last = HDA_LAST_LPIB.load(Ordering::Relaxed) as u32;

    // Determine which half DMA is currently reading.
    // We write to the OTHER half.
    let dma_in_first = lpib < half;
    let (write_off, write_max) = if dma_in_first { (half, half) } else { (0, half) };

    // Avoid writing to the same half twice: only write if
    // LPIB has moved since last write, or if no write has happened yet.
    if last == write_off { return 0 } // same half as last write, not yet consumed

    let to_write = samples.len().min(write_max as usize);
    if to_write == 0 { return 0 }

    unsafe {
        let dst = dma.add((audio_off + write_off) as usize);
        core::ptr::copy_nonoverlapping(samples.as_ptr(), dst, to_write);
        if to_write < write_max as usize {
            core::ptr::write_bytes(dst.add(to_write), 0, write_max as usize - to_write);
        }
    }

    // Mark which half we just wrote
    HDA_LAST_LPIB.store(write_off as u64, Ordering::Relaxed);
    to_write
}

/// Block until the DMA has consumed at least `bytes` worth
/// of audio data.  Call before `hda_feed_samples` if you need to
/// guarantee buffer space.
pub fn hda_poll() {
    if !HDA_READY.load(Ordering::Acquire) { return }
    let virt = *HDA_VIRT.lock();
    if virt == 0 { return }
    let mmio = virt as *mut u8;
    let half = *HDA_HALF.lock();
    let sd = *HDA_SD.lock();
    let last = HDA_LAST_LPIB.load(Ordering::Relaxed) as u32;

    // Spin until LPIB moves into the OTHER half
    loop {
        let lpib = unsafe { r32(mmio, sd + SD_LPIB) };
        let dma_in_first = lpib < half;
        let last_in_first = last < half;
        if dma_in_first != last_in_first { break } // DMA crossed the boundary
        core::hint::spin_loop();
    }
}