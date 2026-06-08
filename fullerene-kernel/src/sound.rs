//! Sound / Audio subsystem for Fullerene OS.
//!
//! Provides PC speaker beep and Intel HD Audio (HDA) streaming playback.
//!
//! ```text
//! PC Speaker (PIT mode 3 → square wave)
//! HDA controller (PCI class 0x04, subclass 0x03)
//!   → CORB/RIRB for codec verb communication
//!   → Codec discovery & configuration (unmute, pin setup)
//!   → SD configured via MMIO registers
//!   → BDL with 2 entries → circular DMA buffer
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

const GCAP: usize      = 0x0000;
const GCTL: usize      = 0x0008;
const STATESTS: usize  = 0x000E;
const INTCTL: usize    = 0x0020;
const CORBLBASE: usize = 0x0040;
const CORBUBASE: usize = 0x0044;
const CORBWP: usize    = 0x0048;
const CORBRP: usize    = 0x004A;
const CORBCTL: usize   = 0x004C;
// CORBSIZE: r/o in QEMU, skip writes
const RIRBLBASE: usize = 0x0050;
const RIRBUBASE: usize = 0x0054;
const RIRBWP: usize    = 0x0058;
const RIRBCTL: usize   = 0x005C;
// RIRBSIZE: r/o in QEMU, skip writes

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

// ── Codec verbs (12-bit) ───────────────────────────────────────

const VERB_GET_PARAM: u32        = 0xF00;
const VERB_SET_FMT: u32          = 0x002;
const VERB_SET_AMP_GAIN_MUTE: u32 = 0x003;
const VERB_SET_PIN_CTL: u32      = 0x707;
const VERB_SET_STREAM: u32       = 0x706;
const VERB_SET_EAPD: u32         = 0x70C;

// Parameter IDs
const PARAM_SUBORDINATE_COUNT: u8 = 0x04;
const PARAM_AUDIO_WIDGET_CAP: u8  = 0x09;
const PARAM_OUTPUT_AMP_CAP: u8    = 0x12;
const PARAM_PIN_CAP: u8           = 0x0C;

// Widget types (bits [23:20])
const WTYPE_AUDIO_OUTPUT: u32 = 0x0;
const WTYPE_PIN_COMPLEX: u32  = 0x4;
const WTYPE_AFG: u32          = 0x1;

const CORB_ENTRIES: usize = 256;
const RIRB_ENTRIES: usize = 256;

// ── DMA ────────────────────────────────────────────────────────

#[repr(C)]
struct BdlEntry { addr_lo: u32, addr_hi: u32, length: u32, flags: u32 }

const DMA_BUF_SIZE: u32 = 32768;
const BDL_ENTRIES: u32 = 2;

// ── Static state ───────────────────────────────────────────────

static HDA_PHYS: Mutex<u64> = Mutex::new(0);
static HDA_READY: AtomicBool = AtomicBool::new(false);
static HDA_VIRT:  Mutex<usize> = Mutex::new(0);
static HDA_DMA:   Mutex<usize> = Mutex::new(0);
static HDA_AUDIO_OFF: Mutex<u32> = Mutex::new(0);
static HDA_AUDIO_SZ:  Mutex<u32> = Mutex::new(0);
static HDA_HALF: Mutex<u32> = Mutex::new(0);
static HDA_SD:   Mutex<usize> = Mutex::new(0);
static HDA_LAST_LPIB: AtomicU64 = AtomicU64::new(0);
static HDA_CORB_V: Mutex<usize> = Mutex::new(0);
static HDA_RIRB_V: Mutex<usize> = Mutex::new(0);
static HDA_INIT_DONE: AtomicBool = AtomicBool::new(false);

// ── MMIO helpers ───────────────────────────────────────────────

unsafe fn r32(m: *mut u8, o: usize) -> u32 { core::ptr::read_volatile(m.add(o) as *const u32) }
unsafe fn w32(m: *mut u8, o: usize, v: u32) { core::ptr::write_volatile(m.add(o) as *mut u32, v); }
unsafe fn r16(m: *mut u8, o: usize) -> u16 { core::ptr::read_volatile(m.add(o) as *const u16) }
unsafe fn w16(m: *mut u8, o: usize, v: u16) { core::ptr::write_volatile(m.add(o) as *mut u16, v); }
unsafe fn r8(m: *mut u8, o: usize) -> u8 { core::ptr::read_volatile(m.add(o)) }
unsafe fn w8(m: *mut u8, o: usize, v: u8) { core::ptr::write_volatile(m.add(o), v); }

// ── Alloc helper ───────────────────────────────────────────────

fn alloc_dma_pages(pages: usize) -> Option<(u64, *mut u8)> {
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let phys = match petroleum::page_table::constants::get_frame_allocator_mut()
        .allocate_contiguous_frames(pages)
    { Ok(a) => a, Err(_) => { log::error!("Sound: DMA alloc fail"); return None } };
    let virt = (phys + off) as *mut u8;
    unsafe { core::ptr::write_bytes(virt, 0, pages * 4096); }
    Some((phys, virt))
}

fn probe_hda() -> Option<(u8, u8, u8, u64)> {
    for d in 0..=31u8 {
        let Some(dev) = PciDevice::new(0, d, 0) else { continue };
        if dev.class_code != 0x04 || dev.subclass != 0x03 { continue }
        let bar0 = dev.read_bar(0)?;
        dev.enable_memory_access();
        use nitrogen::pci::PciConfigSpace;
        let Some(mut cfg) = PciConfigSpace::read_from_device(0, d, 0) else { continue };
        cfg.command |= 0x0004;
        let v = (cfg.status as u32) << 16 | (cfg.command as u32);
        PciConfigSpace::write_config_dword(&mut cfg, 0, d, 0, 0x04, v);
        return Some((0, d, 0, bar0));
    }
    None
}

pub fn init() {
    match probe_hda() {
        Some((bus, dev, func, mmio)) => {
            log::info!("Sound: HDA at {:#04x}:{:#04x}.{}, MMIO=0x{:x}", bus, dev, func, mmio);
            *HDA_PHYS.lock() = mmio;
        }
        None => log::info!("Sound: No HDA (PC speaker only)"),
    }
}

// ── CORB verb send ─────────────────────────────────────────────

/// Returns solicited 32-bit response, or 0xFFFF_FFFF on timeout.
unsafe fn corb_send_verb(mmio: *mut u8, codec: u8, node: u8, verb: u32, payload: u16) -> u32 {
    let corb_v = *HDA_CORB_V.lock();
    let rirb_v = *HDA_RIRB_V.lock();
    if corb_v == 0 || rirb_v == 0 { return 0xFFFF_FFFF; }

    let corb = corb_v as *mut u32;
    let rirb = rirb_v as *mut u64;

    // Verb: [31:28]=codec, [27:20]=node, [19:8]=verb, [7:0]=payload
    let cmd = ((codec as u32) << 28) | ((node as u32) << 20) | (verb << 8) | (payload as u32);

    // Wait if CORB is full (next WP would equal RP)
    for _ in 0..1000 {
        let wp = r16(mmio, CORBWP) as usize;
        let rp = r16(mmio, CORBRP) as usize & 0xFF;
        if (wp + 1) % CORB_ENTRIES != rp { break }
        core::hint::spin_loop();
    }

    // Write verb to CORB at (wp + 1) % CORB_ENTRIES
    let wp = r16(mmio, CORBWP) as usize;
    let next_wp = (wp + 1) % CORB_ENTRIES;
    core::ptr::write_volatile(corb.add(next_wp), cmd);
    w16(mmio, CORBWP, next_wp as u16);

    // Wait for RIRBWP to advance
    let rirb_wp_before = r16(mmio, RIRBWP) & 0xFF;
    for _ in 0..50_000 {
        let rirb_wp = r16(mmio, RIRBWP) & 0xFF;
        if rirb_wp != rirb_wp_before {
            let resp = core::ptr::read_volatile(rirb.add(rirb_wp as usize));
            if (resp >> 63) & 1 == 0 {
                // Solicited response: bits [63:32] are the response
                return (resp >> 32) as u32;
            }
            // Unsolicited response, keep polling
        }
        core::hint::spin_loop();
    }
    log::warn!("Sound: verb timeout c={} n={:#x} v={:#03x}", codec, node, verb);
    0xFFFF_FFFF
}

// ── Codec discovery ────────────────────────────────────────────

/// Find first DAC and Pin widgets under the AFG.
unsafe fn discover_codec(mmio: *mut u8, codec: u8) -> Option<(u8, u8)> {
    // 1. Root → AFG
    let sub = corb_send_verb(mmio, codec, 0, VERB_GET_PARAM, PARAM_SUBORDINATE_COUNT as u16);
    if sub == 0xFFFF_FFFF { return None; }
    let start = ((sub >> 16) & 0xFF) as u8;
    let count = (sub & 0xFF) as u8;
    if count == 0 { return None; }
    let end = start + count - 1;
    log::info!("Sound: root children {}-{}", start, end);

    let mut afg: Option<u8> = None;
    for n in start..=end {
        let cap = corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_AUDIO_WIDGET_CAP as u16);
        if cap == 0xFFFF_FFFF { continue }
        if (cap >> 20) & 0xF == WTYPE_AFG {
            afg = Some(n);
            log::info!("Sound: AFG node {}", n);
            break;
        }
    }
    let afg = afg?;

    // 2. AFG → widgets
    let sub = corb_send_verb(mmio, codec, afg, VERB_GET_PARAM, PARAM_SUBORDINATE_COUNT as u16);
    if sub == 0xFFFF_FFFF { return None; }
    let start = ((sub >> 16) & 0xFF) as u8;
    let count = (sub & 0xFF) as u8;
    if count == 0 { return None; }
    let end = start + count - 1;
    log::info!("Sound: AFG children {}-{}", start, end);

    let mut dac: Option<u8> = None;
    let mut pin: Option<u8> = None;
    for n in start..=end {
        let cap = corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_AUDIO_WIDGET_CAP as u16);
        if cap == 0xFFFF_FFFF { continue }
        let t = (cap >> 20) & 0xF;
        log::debug!("Sound: node {} type={}", n, t);
        if t == WTYPE_AUDIO_OUTPUT && dac.is_none() { dac = Some(n); }
        if t == WTYPE_PIN_COMPLEX && pin.is_none() { pin = Some(n); }
    }
    match (dac, pin) {
        (Some(d), Some(p)) => Some((d, p)),
        _ => None,
    }
}

// ── Codec config ───────────────────────────────────────────────

unsafe fn configure_codec(mmio: *mut u8, codec: u8, dac: u8, pin: u8, stream: u8) {
    // Unmute DAC amp
    let ac = corb_send_verb(mmio, codec, dac, VERB_GET_PARAM, PARAM_OUTPUT_AMP_CAP as u16);
    let steps = (ac & 0x7F) as u16;
    let gain = if steps > 0 { steps / 2 } else { 0 };
    corb_send_verb(mmio, codec, dac, VERB_SET_AMP_GAIN_MUTE,
        (1u16 << 13) | (1u16 << 12) | gain);
    // Set format: 44.1kHz / 2 = 22.05kHz, 16-bit, 1ch
    // bits[7]=1 (44.1kHz), bits[3:0]=1 (/2), bits[10:8]=1 (16-bit)
    corb_send_verb(mmio, codec, dac, VERB_SET_FMT, 0x0181);
    // Assign stream
    corb_send_verb(mmio, codec, dac, VERB_SET_STREAM, stream as u16);

    // Unmute pin amp
    let pa = corb_send_verb(mmio, codec, pin, VERB_GET_PARAM, PARAM_OUTPUT_AMP_CAP as u16);
    let ps = (pa & 0x7F) as u16;
    let pg = if ps > 0 { ps / 2 } else { 0 };
    corb_send_verb(mmio, codec, pin, VERB_SET_AMP_GAIN_MUTE,
        (1u16 << 13) | (1u16 << 12) | pg);
    // Pin output enable
    corb_send_verb(mmio, codec, pin, VERB_SET_PIN_CTL, (1u16 << 7) | (1u16 << 6));
    // EAPD
    let cap = corb_send_verb(mmio, codec, pin, VERB_GET_PARAM, PARAM_PIN_CAP as u16);
    if cap != 0xFFFF_FFFF && (cap >> 16) & 1 != 0 {
        corb_send_verb(mmio, codec, pin, VERB_SET_EAPD, 0x02);
    }
    log::info!("Sound: codec done DAC=0x{:x} Pin=0x{:x}", dac, pin);
}

// ── Main init ──────────────────────────────────────────────────

fn hda_init() {
    if HDA_INIT_DONE.load(Ordering::Acquire) { return }

    let phys = *HDA_PHYS.lock();
    if phys == 0 { return }

    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let virt = (phys + off) as usize;
    *HDA_VIRT.lock() = virt;

    let gctest = unsafe { r32(virt as *mut u8, GCAP) };
    if gctest == 0 || gctest == 0xFFFF_FFFF {
        log::warn!("Sound: GCAP invalid");
        *HDA_PHYS.lock() = 0; return;
    }
    log::info!("Sound: GCAP=0x{:x}", gctest);

    // Reset controller
    let (iss, oss) = unsafe {
        let m = virt as *mut u8;
        w32(m, GCTL, 0);
        for _ in 0..2000 { core::hint::spin_loop(); }
        w32(m, GCTL, 1);
        for _ in 0..20000 { if r32(m, GCTL) & 1 != 0 { break } }
        if r32(m, GCTL) & 1 == 0 {
            log::warn!("Sound: controller reset timeout");
            return;
        }
        w16(m, STATESTS, 0x000F);
        w32(m, INTCTL, 0);
        let gcap = r32(m, GCAP);
        ((gcap >> 8) & 0xF, (gcap >> 12) & 0xF)
    };
    log::info!("Sound: ISS={} OSS={}", iss, oss);
    if oss == 0 { log::warn!("Sound: no output streams"); return; }

    *HDA_SD.lock() = SD_BASE + (iss as usize) * SD_SIZE;

    // Alloc CORB (1 page = 4 KiB, enough for 256×4 = 1 KiB)
    let Some((corb_phys, corb_virt)) = alloc_dma_pages(1) else { return };
    *HDA_CORB_V.lock() = corb_virt as usize;

    // Alloc RIRB (1 page = 4 KiB, enough for 256×8 = 2 KiB)
    let Some((rirb_phys, rirb_virt)) = alloc_dma_pages(1) else { return };
    *HDA_RIRB_V.lock() = rirb_virt as usize;

    unsafe {
        let m = virt as *mut u8;

        // CORB: set base, reset read pointer, enable
        w32(m, CORBLBASE, corb_phys as u32);
        w32(m, CORBUBASE, (corb_phys >> 32) as u32);
        w16(m, CORBRP, 0x8000); // reset
        for _ in 0..200 { core::hint::spin_loop(); }
        w16(m, CORBRP, 0);
        w16(m, CORBWP, 0);
        w8(m, CORBCTL, 0x02); // DMA run
        for _ in 0..2000 { core::hint::spin_loop(); }

        // RIRB: set base, reset write pointer, enable
        w32(m, RIRBLBASE, rirb_phys as u32);
        w32(m, RIRBUBASE, (rirb_phys >> 32) as u32);
        w16(m, RIRBWP, 0x8000); // reset
        for _ in 0..200 { core::hint::spin_loop(); }
        w16(m, RIRBWP, 0);
        w8(m, RIRBCTL, 0x02); // DMA run
        for _ in 0..2000 { core::hint::spin_loop(); }
        log::info!("Sound: CORB/RIRB enabled");
    }

    // Codec discovery & config
    let codec_addr: u8 = 0;
    unsafe {
        if let Some((dac, pin)) = discover_codec(virt as *mut u8, codec_addr) {
            configure_codec(virt as *mut u8, codec_addr, dac, pin, 1);
        } else {
            log::warn!("Sound: no codec widgets — output may be silent");
        }
    }

    // Alloc DMA buffer
    let dma_pages = (DMA_BUF_SIZE as usize + 4095) / 4096;
    let Some((dma_phys, dma_virt)) = alloc_dma_pages(dma_pages) else { return };
    *HDA_DMA.lock() = dma_virt as usize;

    let bdl_sz = core::mem::size_of::<BdlEntry>() as u64 * BDL_ENTRIES as u64;
    let audio_phys = dma_phys + bdl_sz;
    let audio_off = bdl_sz as u32;
    let audio_sz = DMA_BUF_SIZE - audio_off;
    let half = audio_sz / 2;
    *HDA_AUDIO_OFF.lock() = audio_off;
    *HDA_AUDIO_SZ.lock() = audio_sz;
    *HDA_HALF.lock() = half;

    unsafe {
        let bdl = dma_virt as *mut BdlEntry;
        *bdl.add(0) = BdlEntry { addr_lo: audio_phys as u32, addr_hi: (audio_phys>>32) as u32, length: half, flags: 1 };
        *bdl.add(1) = BdlEntry { addr_lo: (audio_phys+half as u64) as u32, addr_hi: ((audio_phys+half as u64)>>32) as u32, length: half, flags: 1 };
    }

    unsafe {
        let m = virt as *mut u8;
        let sd = *HDA_SD.lock();
        let ctl = r8(m, sd + SD_CTL);
        w8(m, sd + SD_CTL, ctl & !0x02);
        w8(m, sd + SD_CTL, 0x01);
        for _ in 0..2000 { core::hint::spin_loop(); }
        w8(m, sd + SD_CTL, 0x00);
        for _ in 0..2000 { core::hint::spin_loop(); }
        w8(m, sd + SD_STS, 0xFF);
        // 22.05kHz = 44.1kHz base / 2, 16-bit, 1ch
        // bits[7]=1 (44.1kHz base), bits[3:0]=1 (÷2), bits[10:8]=1 (16-bit), bits[13:11]=0 (1ch)
        w16(m, sd + SD_FMT, 0x0181);
        w32(m, sd + SD_CBL, audio_sz);
        w16(m, sd + SD_LVI, BDL_ENTRIES as u16 - 1);
        w32(m, sd + SD_BDPL, dma_phys as u32);
        w32(m, sd + SD_BDPU, (dma_phys >> 32) as u32);
        w32(m, sd + SD_CTL, (1u32 << 20) | 0x02);
        log::info!("Sound: stream started ({} B)", audio_sz);
    }

    HDA_READY.store(true, Ordering::Release);
    HDA_INIT_DONE.store(true, Ordering::Release);
}

// ── Public API ─────────────────────────────────────────────────

pub fn hda_available() -> bool { *HDA_PHYS.lock() != 0 }

pub fn hda_feed_samples(samples: &[u8]) -> usize {
    hda_init();
    if !HDA_READY.load(Ordering::Acquire) { return 0 }
    let virt = *HDA_VIRT.lock();
    if virt == 0 { return 0 }
    let mmio = virt as *mut u8;
    let dma = *HDA_DMA.lock() as *mut u8;
    let off = *HDA_AUDIO_OFF.lock();
    let half = *HDA_HALF.lock();
    let sd = *HDA_SD.lock();

    // Read LPIB to determine which half DMA is currently consuming.
    let lpib = unsafe { r32(mmio, sd + SD_LPIB) };
    let dma_in_first = lpib < half;

    // Write to the half that DMA is NOT currently consuming.
    let write_off = if dma_in_first { half } else { 0 };
    let write_max = half as usize;

    let n = samples.len().min(write_max);
    if n == 0 { return 0 }

    unsafe {
        let dst = dma.add((off + write_off) as usize);
        core::ptr::copy_nonoverlapping(samples.as_ptr(), dst, n);
        if n < write_max {
            core::ptr::write_bytes(dst.add(n), 0, write_max - n);
        }
    }
    n
}

pub fn hda_poll() {
    if !HDA_READY.load(Ordering::Acquire) { return }
    let virt = *HDA_VIRT.lock();
    if virt == 0 { return }
    let mmio = virt as *mut u8;
    let half = *HDA_HALF.lock();
    let sd = *HDA_SD.lock();
    let last = HDA_LAST_LPIB.load(Ordering::Relaxed) as u32;
    loop {
        let lpib = unsafe { r32(mmio, sd + SD_LPIB) };
        let a = lpib < half;
        let b = last < half;
        if a != b { break }
        core::hint::spin_loop();
    }
}