//! Sound / Audio subsystem for Fullerene OS.
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use nitrogen::pci::PciDevice;
use spin::Mutex;

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

const GCAP: usize = 0x0000;
const GCTL: usize = 0x0008;
const STATESTS: usize = 0x000E;
const INTCTL: usize = 0x0020;
const CORBLBASE: usize = 0x0040;
const CORBUBASE: usize = 0x0044;
const CORBWP: usize = 0x0048;
const CORBRP: usize = 0x004A;
const CORBCTL: usize = 0x004C;
const RIRBLBASE: usize = 0x0050;
const RIRBUBASE: usize = 0x0054;
const RIRBWP: usize = 0x0058;
const RIRBCTL: usize = 0x005C;
const SD_BASE: usize = 0x0080;
const SD_SIZE: usize = 0x0020;
const SD_CTL: usize = 0x00;
const SD_STS: usize = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0C;
const SD_FMT: usize = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;
const VERB_GET_PARAM: u32 = 0xF00;
const VERB_SET_FMT: u32 = 0x002;
const VERB_SET_AMP_GAIN_MUTE: u32 = 0x003;
const VERB_SET_PIN_CTL: u32 = 0x707;
const VERB_SET_STREAM: u32 = 0x706;
const VERB_SET_EAPD: u32 = 0x70C;
const PARAM_SUBORDINATE_COUNT: u8 = 0x04;
const PARAM_AUDIO_WIDGET_CAP: u8 = 0x09;
const PARAM_OUTPUT_AMP_CAP: u8 = 0x12;
const PARAM_PIN_CAP: u8 = 0x0C;
const WTYPE_AUDIO_OUTPUT: u32 = 0x0;
const WTYPE_PIN_COMPLEX: u32 = 0x4;
const WTYPE_AFG: u32 = 0x1;
const CORB_ENTRIES: usize = 256;
const RIRB_ENTRIES: usize = 256;

#[repr(C)]
struct BdlEntry {
    addr_lo: u32,
    addr_hi: u32,
    length: u32,
    flags: u32,
}
const DMA_BUF_SIZE: u32 = 32768;
const BDL_ENTRIES: u32 = 2;

static HDA_PHYS: Mutex<u64> = Mutex::new(0);
static HDA_READY: AtomicBool = AtomicBool::new(false);
static HDA_VIRT: Mutex<usize> = Mutex::new(0);
static HDA_DMA: Mutex<usize> = Mutex::new(0);
static HDA_AUDIO_OFF: Mutex<u32> = Mutex::new(0);
static HDA_AUDIO_SZ: Mutex<u32> = Mutex::new(0);
static HDA_HALF: Mutex<u32> = Mutex::new(0);
static HDA_SD: Mutex<usize> = Mutex::new(0);
static HDA_LAST_LPIB: AtomicU64 = AtomicU64::new(u64::MAX);
static HDA_CORB_V: Mutex<usize> = Mutex::new(0);
static HDA_RIRB_V: Mutex<usize> = Mutex::new(0);
static HDA_INIT_DONE: AtomicBool = AtomicBool::new(false);
/// Actual CORB entry count (derived from GCAP CORBSZCAP; 2, 16, or 256).
/// Used by `corb_send_verb` for circular‑buffer wrap.
static HDA_CORB_ENTRIES: Mutex<usize> = Mutex::new(256);

unsafe fn r32(m: *mut u8, o: usize) -> u32 {
    unsafe { core::ptr::read_volatile(m.add(o) as *const u32) }
}
unsafe fn w32(m: *mut u8, o: usize, v: u32) {
    unsafe { core::ptr::write_volatile(m.add(o) as *mut u32, v) }
}
unsafe fn r16(m: *mut u8, o: usize) -> u16 {
    unsafe { core::ptr::read_volatile(m.add(o) as *const u16) }
}
unsafe fn w16(m: *mut u8, o: usize, v: u16) {
    unsafe { core::ptr::write_volatile(m.add(o) as *mut u16, v) }
}
unsafe fn r8(m: *mut u8, o: usize) -> u8 {
    unsafe { core::ptr::read_volatile(m.add(o)) }
}
unsafe fn w8(m: *mut u8, o: usize, v: u8) {
    unsafe { core::ptr::write_volatile(m.add(o), v) }
}

fn alloc_dma_pages(pages: usize) -> Option<(u64, *mut u8)> {
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
    Some((phys, virt))
}

/// Probe for HDA controller across all PCI buses.
///
/// On real hardware (InsydeH2O) there are often two HDA controllers:
///
/// | BDF       | Vendor:Device | Role                      |
/// |-----------|---------------|---------------------------|
/// | 00:03.0   | 8086:160c     | Intel Display Audio (HDMI)|
/// | 00:1b.0   | 8086:9ca0     | Wildcat Point-LP (PCH)    |
///
/// The Display Audio controller appears first during bus scan (device 3 <
/// device 27), but it has no codec attached → STATESTS bit 0 is 0.  We
/// therefore enumerate **all** HDA controllers, log BAR0 / GCAP / STATESTS
/// for each, and prefer the one with a connected codec (STATESTS & 0x0001).
fn probe_hda() -> Option<(u8, u8, u8, u64)> {
    use nitrogen::pci::PciConfigSpace;
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;

    /// Check whether a PCI bus exists by probing device 0 function 0.
    fn bus_exists(bus: u8) -> bool {
        PciConfigSpace::read_config_word(bus, 0, 0, 0) != 0xFFFF
    }

    // Accumulate the "last seen" HDA as a fallback when no codec-connected
    // candidate is found.  Because we iterate device numbers in ascending
    // order, the last entry corresponds to the highest device number
    // (i.e. the PCH HDA rather than the CPU Display Audio).
    let mut fallback: Option<(u8, u8, u8, u64)> = None;

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
            let Some(bar0) = dev.read_bar(0) else {
                continue;
            };
            dev.enable_memory_access();
            let Some(mut cfg) = PciConfigSpace::read_from_device(bus, d, 0) else {
                continue;
            };
            cfg.command |= 0x0004;
            let v = (cfg.status as u32) << 16 | (cfg.command as u32);
            PciConfigSpace::write_config_dword(&mut cfg, bus, d, 0, 0x04, v);

            // ── Quick MMIO probe ──────────────────────────────────
            let mmio = (bar0 + off) as *mut u8;
            let gcap = unsafe { r32(mmio, GCAP) };
            let states = unsafe { r16(mmio, STATESTS) };

            log::info!(
                "Sound: HDA {:04x}:{:02x}.{} [{:#06x}:{:#06x}] BAR0=0x{:016x} GCAP=0x{:08x} STATESTS=0x{:04x}",
                bus, d, 0,
                dev.vendor_id, dev.device_id,
                bar0,
                gcap,
                states
            );

            // Codec #0 connected → this is the real audio controller.
            if states & 0x0001 != 0 {
                log::info!(
                    "Sound: selecting HDA {:04x}:{:02x}.{} (codec connected)",
                    bus, d, 0
                );
                return Some((bus, d, 0, bar0));
            }

            fallback = Some((bus, d, 0, bar0));
        }
    }

    if let Some(ref b) = fallback {
        log::info!(
            "Sound: falling back to HDA {:04x}:{:02x}.{} (no codec detected on any HDA)",
            b.0, b.1, b.2
        );
    }
    fallback
}

pub fn init() {
    match probe_hda() {
        Some((bus, dev, func, mmio)) => {
            log::info!(
                "Sound: HDA at {:04x}:{:02x}.{}, MMIO=0x{:x}",
                bus,
                dev,
                func,
                mmio
            );
            *HDA_PHYS.lock() = mmio;
        }
        None => log::info!("Sound: No HDA (PC speaker only)"),
    }
}

unsafe fn corb_send_verb(mmio: *mut u8, codec: u8, node: u8, verb: u32, payload: u16) -> u32 {
    let corb_v = *HDA_CORB_V.lock();
    let rirb_v = *HDA_RIRB_V.lock();
    if corb_v == 0 || rirb_v == 0 {
        return 0xFFFF_FFFF;
    }
    let corb_n = *HDA_CORB_ENTRIES.lock();
    let corb = corb_v as *mut u32;
    let rirb = rirb_v as *mut u64;

    // 4-bit verbs (e.g. VERB_SET_FMT=0x002, VERB_SET_AMP_GAIN_MUTE=0x003):
    //   Verb ID → bits [19:16], 16-bit payload → bits [15:0]
    // 12-bit verbs (e.g. VERB_GET_PARAM=0xF00):
    //   Verb ID → bits [19:8], 8-bit payload → bits [7:0]
    let cmd_val = if verb > 0xF {
        (verb << 8) | (payload as u32 & 0xFF)
    } else {
        (verb << 16) | (payload as u32 & 0xFFFF)
    };
    let cmd = ((codec as u32) << 28) | ((node as u32) << 20) | cmd_val;
    for _ in 0..1000 {
        let wp = unsafe { r16(mmio, CORBWP) } as usize;
        let rp = unsafe { r16(mmio, CORBRP) } as usize & 0xFF;
        if (wp + 1) % corb_n != rp {
            break;
        }
        core::hint::spin_loop();
    }
    let wp = unsafe { r16(mmio, CORBWP) } as usize;
    let next_wp = (wp + 1) % corb_n;
    unsafe { core::ptr::write_volatile(corb.add(next_wp), cmd) };
    unsafe { w16(mmio, CORBWP, next_wp as u16) };
    let rirb_wp_before = unsafe { r16(mmio, RIRBWP) } & 0xFF;
    for _ in 0..50_000 {
        let rirb_wp = unsafe { r16(mmio, RIRBWP) } & 0xFF;
        if rirb_wp != rirb_wp_before {
            let resp = unsafe { core::ptr::read_volatile(rirb.add(rirb_wp as usize)) };
            if (resp >> 63) & 1 == 0 {
                return (resp >> 32) as u32;
            }
        }
        core::hint::spin_loop();
    }
    log::warn!(
        "Sound: verb timeout c={} n={:#x} v={:#03x}",
        codec,
        node,
        verb
    );
    0xFFFF_FFFF
}

unsafe fn discover_codec(mmio: *mut u8, codec: u8) -> Option<(u8, u8)> {
    let sub = unsafe { corb_send_verb(mmio, codec, 0, VERB_GET_PARAM, PARAM_SUBORDINATE_COUNT as u16) };
    if sub == 0xFFFF_FFFF {
        return None;
    }
    let start = ((sub >> 16) & 0xFF) as u8;
    let count = (sub & 0xFF) as u8;
    if count == 0 {
        return None;
    }
    let end = start + count - 1;
    log::info!("Sound: root children {}-{}", start, end);
    let mut afg: Option<u8> = None;
    for n in start..=end {
        let cap = unsafe { corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_AUDIO_WIDGET_CAP as u16) };
        if cap == 0xFFFF_FFFF {
            continue;
        }
        if (cap >> 20) & 0xF == WTYPE_AFG {
            afg = Some(n);
            log::info!("Sound: AFG node {}", n);
            break;
        }
    }
    let afg = afg?;
    let sub = unsafe { corb_send_verb(mmio, codec, afg, VERB_GET_PARAM, PARAM_SUBORDINATE_COUNT as u16) };
    if sub == 0xFFFF_FFFF {
        return None;
    }
    let start = ((sub >> 16) & 0xFF) as u8;
    let count = (sub & 0xFF) as u8;
    if count == 0 {
        return None;
    }
    let end = start + count - 1;
    log::info!("Sound: AFG children {}-{}", start, end);
    let mut dac: Option<u8> = None;
    let mut pin: Option<u8> = None;
    for n in start..=end {
        let cap = unsafe { corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_AUDIO_WIDGET_CAP as u16) };
        if cap == 0xFFFF_FFFF {
            continue;
        }
        let t = (cap >> 20) & 0xF;
        if t == WTYPE_AUDIO_OUTPUT {
            // Prefer later DACs — on many codecs (e.g. ALC286) the
            // first DAC (0x02) is for headphones while a later one
            // (0x03) drives the internal speaker.
            dac = Some(n);
        }
        if t == WTYPE_PIN_COMPLEX {
            // Query pin capabilities to ensure this pin supports
            // output (bit 4 = OUT).  Skip input-only pins (e.g.
            // internal mic 0x12 on ALC286) so we don't mis-route
            // audio to a microphone.
            let pincap =
                unsafe { corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_PIN_CAP as u16) };
            if pincap != 0xFFFF_FFFF && (pincap & (1 << 4)) != 0 {
                pin = Some(n);
            } else {
                log::info!(
                    "Sound: pin 0x{:x} cap=0x{:08x} — skipping (no OUT)",
                    n, pincap
                );
            }
        }
    }
    match (dac, pin) {
        (Some(d), Some(p)) => Some((d, p)),
        _ => None,
    }
}

unsafe fn configure_codec(mmio: *mut u8, codec: u8, dac: u8, pin: u8, stream: u8) {
    let ac = unsafe { corb_send_verb(mmio, codec, dac, VERB_GET_PARAM, PARAM_OUTPUT_AMP_CAP as u16) };
    let steps = ac as u8 & 0x7F;
    let gain = if steps > 0 { steps / 2 } else { 0 };
    unsafe { corb_send_verb(mmio, codec, dac, VERB_SET_AMP_GAIN_MUTE, (0x70 | gain) as u16) };
    // 16-bit signed mono at 48000 Hz:
    // bit[7] = 0 → 48 kHz base, bits[6:4] = 1 → 16-bit container,
    // bit[3:0] = 0 → 1 channel
    unsafe { corb_send_verb(mmio, codec, dac, VERB_SET_FMT, 0x10u16) };
    unsafe { corb_send_verb(mmio, codec, dac, VERB_SET_STREAM, stream as u16) };
    let pa = unsafe { corb_send_verb(mmio, codec, pin, VERB_GET_PARAM, PARAM_OUTPUT_AMP_CAP as u16) };
    let psteps = pa as u8 & 0x7F;
    let pgain = if psteps > 0 { psteps / 2 } else { 0 };
    unsafe { corb_send_verb(mmio, codec, pin, VERB_SET_AMP_GAIN_MUTE, (0x70 | pgain) as u16) };
    // Query pin capabilities to check EAPD support (bit 16)
    let pin_cap = unsafe { corb_send_verb(mmio, codec, pin, VERB_GET_PARAM, PARAM_PIN_CAP as u16) };
    let eapd_capable = pin_cap != 0xFFFF_FFFF && (pin_cap >> 16) & 1 != 0;
    log::info!(
        "Sound: pin 0x{:x} cap=0x{:08x} eapd_capable={}",
        pin, pin_cap, eapd_capable
    );
    // Power up external amplifier BEFORE enabling pin output.
    // On many notebook codecs (ALC286 etc.) EAPD controls the
    // internal speaker amplifier power — without this the output
    // stays silent even though the stream and DMA are running.
    if eapd_capable {
        let eapd_res = unsafe { corb_send_verb(mmio, codec, pin, VERB_SET_EAPD, 0x02) };
        log::info!("Sound: SET_EAPD pin=0x{:x} result=0x{:08x}", pin, eapd_res);
    }
    // Enable pin output (0x40 = Output Enable only, matching Linux
    // behaviour for fixed-function speakers).  Do NOT set HP Enable
    // (bit 7) on speaker pins — it is semantically wrong and some
    // codecs may behave unexpectedly.
    let pin_ctl_res = unsafe { corb_send_verb(mmio, codec, pin, VERB_SET_PIN_CTL, 0x40u16) };
    log::info!(
        "Sound: SET_PIN_CTL pin=0x{:x} val=0x40 result=0x{:08x}",
        pin, pin_ctl_res
    );
    log::info!("Sound: codec done DAC=0x{:x} Pin=0x{:x}", dac, pin);
}

fn hda_init() {
    if HDA_INIT_DONE.load(Ordering::Acquire) {
        return;
    }
    let phys = *HDA_PHYS.lock();
    if phys == 0 {
        return;
    }
    let off = petroleum::common::memory::get_physical_memory_offset() as u64;
    let virt = (phys + off) as usize;
    *HDA_VIRT.lock() = virt;
    let gctest = unsafe { r32(virt as *mut u8, GCAP) };
    if gctest == 0 || gctest == 0xFFFF_FFFF {
        log::warn!("Sound: GCAP invalid");
        *HDA_PHYS.lock() = 0;
        return;
    }
    log::info!("Sound: GCAP=0x{:x}", gctest);
    let (iss, oss) = unsafe {
        let m = virt as *mut u8;
        w32(m, GCTL, 0);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
        w32(m, GCTL, 1);
        for _ in 0..20000 {
            if r32(m, GCTL) & 1 != 0 {
                break;
            }
        }
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
    if oss == 0 {
        log::warn!("Sound: no output streams");
        return;
    }
    *HDA_SD.lock() = SD_BASE + (iss as usize) * SD_SIZE;
    let Some((corb_phys, corb_virt)) = alloc_dma_pages(1) else {
        return;
    };
    *HDA_CORB_V.lock() = corb_virt as usize;
    let Some((rirb_phys, rirb_virt)) = alloc_dma_pages(1) else {
        return;
    };
    *HDA_RIRB_V.lock() = rirb_virt as usize;
    // ── CORB size encoding ────────────────────────────────────
    // GCAP bit 0 → 64-bit address support; bits 7:4 → CORBSZCAP
    // We request 256 entries → CORBSIZE = 10b (bits 9:8 of CORBCTL).
    // But first we must ensure the controller supports it; if not,
    // fall back to 2 entries (00b) or 16 entries (01b).
    let gcap = unsafe {
        r16(virt as *mut u8, GCAP + 2) as u32 | ((r16(virt as *mut u8, GCAP) as u32) << 16)
    };
    // Full 32-bit GCAP.  CORB size capability in bits 7:4.
    let corb_szcap = (gcap >> 4) & 0xF; // 0=2, 1=16, 2=256 entries
    // By default assume 256 entries.  If the controller does not
    // support that, fall back to 16 entries.
    let corb_sz: u32 = if corb_szcap >= 2 {
        2
    } else if corb_szcap >= 1 {
        1
    } else {
        0
    };
    let corb_sz_bits = corb_sz << 8; // CORBSIZE in bits 9:8
    // CORB entries count derived from size code
    let corb_n: usize = match corb_sz {
        0 => 2,
        1 => 16,
        _ => 256,
    };
    // RIRB uses the same size field (bits 9:8 of RIRBCTL);
    // the controller only supports a single size for both.
    let rirb_sz_bits = corb_sz_bits;
    // Store for corb_send_verb
    *HDA_CORB_ENTRIES.lock() = corb_n;

    unsafe {
        let m = virt as *mut u8;
        // Stop CORB/RIRB DMA engines before programming
        w32(m, CORBCTL, 0);
        w32(m, RIRBCTL, 0);
        w32(m, CORBLBASE, corb_phys as u32);
        w32(m, CORBUBASE, (corb_phys >> 32) as u32);
        // CORB Read Pointer Reset via bit 15, then clear RP/WP
        w16(m, CORBRP, 0x8000);
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        w16(m, CORBRP, 0);
        w16(m, CORBWP, 0);
        // Enable CORB DMA with the correct size
        w32(m, CORBCTL, 0x02 | corb_sz_bits);
        w32(m, RIRBLBASE, rirb_phys as u32);
        w32(m, RIRBUBASE, (rirb_phys >> 32) as u32);
        // RIRBWP reset: set bit 15 (RIRBRST) then clear
        w16(m, RIRBWP, 0x8000);
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        // Read back to confirm reset is released, then zero WP
        if r16(m, RIRBWP) & 0x8000 != 0 {
            w16(m, RIRBWP, 0);
        }
        // Enable RIRB DMA with the correct size
        w32(m, RIRBCTL, 0x02 | rirb_sz_bits);
        log::info!("Sound: CORB/RIRB enabled (size={} entries)", corb_n);
    }
    let codec_addr: u8 = 0;
    unsafe {
        if let Some((dac, pin)) = discover_codec(virt as *mut u8, codec_addr) {
            configure_codec(virt as *mut u8, codec_addr, dac, pin, 0);
        } else {
            log::warn!("Sound: no codec widgets");
        }
    }
    let dma_pages = (DMA_BUF_SIZE as usize + 4095) / 4096;
    let Some((dma_phys, dma_virt)) = alloc_dma_pages(dma_pages) else {
        return;
    };
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
        *bdl.add(0) = BdlEntry {
            addr_lo: audio_phys as u32,
            addr_hi: (audio_phys >> 32) as u32,
            length: half,
            flags: 0x01, // IOC — signal completion via BCIS
        };
        *bdl.add(1) = BdlEntry {
            addr_lo: (audio_phys + half as u64) as u32,
            addr_hi: ((audio_phys + half as u64) >> 32) as u32,
            length: half,
            flags: 0x01, // IOC
        };
    }
    unsafe {
        let m = virt as *mut u8;
        let sd = *HDA_SD.lock();
        // Stop any previous stream, then clear status
        w32(m, sd + SD_CTL, 0);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
        w8(m, sd + SD_STS, 0xFF); // clear all status bits (WC)
        // Reset stream
        w32(m, sd + SD_CTL, 0x01);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
        // Wait for reset to complete
        for _ in 0..50000 {
            if r32(m, sd + SD_CTL) & 0x01 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // Program format, BDL and stream settings
        w8(m, sd + SD_STS, 0xFF);
        // 48 kHz 16-bit mono:
        // bit7 BASE=0 (48kHz), bits6:4 BITS=1 (16-bit), bits3:0 CHAN=0 (1ch)
        w16(m, sd + SD_FMT, 0x0010);
        w32(m, sd + SD_CBL, audio_sz);
        w16(m, sd + SD_LVI, BDL_ENTRIES as u16 - 1);
        w32(m, sd + SD_BDPL, dma_phys as u32);
        w32(m, sd + SD_BDPU, (dma_phys >> 32) as u32);
        // Store fence: ensure BDL / DMA buffer writes are visible
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // Start stream: RUN (bit 1) + IOCE (bit 2) + STRIPE1 (bits 18:16)
        w32(m, sd + SD_CTL, (1u32 << 16) | 0x06);
        log::info!("Sound: stream started ({} B, fmt=0x0010)", audio_sz);
    }
    HDA_READY.store(true, Ordering::Release);
    HDA_INIT_DONE.store(true, Ordering::Release);
}

/// Force a VM exit on QEMU/KVM so the device model can advance
/// HDA DMA state.  We read the PIC master IMR (I/O port 0x21)
/// because I/O-port accesses always trap on KVM, whereas MMIO
/// reads from the HDA BAR may be satisfied directly via EPT
/// without any exit (depending on QEMU's memory region layout).
pub fn hda_tick() {
    unsafe {
        x86_64::instructions::port::PortReadOnly::<u8>::new(0x21).read();
    }
}

pub fn hda_available() -> bool {
    *HDA_PHYS.lock() != 0
}

pub fn hda_write_direct(offset: u32, samples: &[u8]) -> usize {
    hda_init();
    if !HDA_READY.load(Ordering::Acquire) {
        return 0;
    }
    let dma = *HDA_DMA.lock() as *mut u8;
    let off = *HDA_AUDIO_OFF.lock();
    let total = *HDA_AUDIO_SZ.lock() as usize;
    let max_len = total.saturating_sub(offset as usize);
    let n = samples.len().min(max_len);
    if n == 0 {
        return 0;
    }
    unsafe {
        let dst = dma.add((off + offset) as usize);
        core::ptr::copy_nonoverlapping(samples.as_ptr(), dst, n);
    }
    n
}

/// After pre‑filling both halves of the DMA buffer, reset the
/// LPIB tracking so `hda_feed_samples` knows DMA starts from
/// half 0 and won't overwrite pre‑filled data.
pub fn hda_reset_prefill_tracking() {
    // DMA starts at offset 0 → pretend we last observed it in
    // the first half so the gate blocks writes until it crosses
    // into the second half.
    HDA_LAST_LPIB.store(0, Ordering::Relaxed);
}

pub fn hda_feed_samples(samples: &[u8]) -> usize {
    hda_init();
    if !HDA_READY.load(Ordering::Acquire) {
        return 0;
    }
    let virt = *HDA_VIRT.lock();
    if virt == 0 {
        return 0;
    }
    let mmio = virt as *mut u8;
    let dma = *HDA_DMA.lock() as *mut u8;
    let off = *HDA_AUDIO_OFF.lock();
    let half = *HDA_HALF.lock();
    let sd = *HDA_SD.lock();

    // ── Determine safe write half from LPIB ─────────────────
    // LPIB normalised to [0, audio_sz) tells us which half DMA
    // is reading → we may write the *other* half.
    let lpib_raw = unsafe { r32(mmio, sd + SD_LPIB) };
    let lpib = lpib_raw.wrapping_rem(*HDA_AUDIO_SZ.lock());
    let write_off = if lpib < half { half } else { 0 };

    // BCIS (hardware IOC) provides a strong “half-done” signal.
    let sts = unsafe { r8(mmio, sd + SD_STS) };
    if sts & 0x04 != 0 {
        unsafe { w8(mmio, sd + SD_STS, 0x04); }
    }

    // ── Time‑based fallback guard ───────────────────────────
    // QEMU sometimes stalls HDA state updates inside tight
    // spin‑loops; a key‑press (IRQ) briefly unblocks it.
    // Compare raw LPIB against the last *raw* value so that a
    // monotonically‑increasing counter still triggers a write
    // every ~half bytes, preventing total stall.
    let last_raw = HDA_LAST_LPIB.load(Ordering::Relaxed) as u32;
    let delta = lpib_raw.wrapping_sub(last_raw);
    // Allow writing if raw LPIB advanced by at least half bytes
    // (hardware crossed the boundary even if normalised view
    // looks identical due to wrapping) OR if BCIS was observed.
    let crossed = delta >= half || (sts & 0x04) != 0;
    if !crossed {
        return 0;
    }

    // Record raw LPIB for next delta comparison.
    HDA_LAST_LPIB.store(lpib_raw as u64, Ordering::Relaxed);

    let write_max = half as usize;
    let n = samples.len().min(write_max);
    if n == 0 {
        return 0;
    }
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
    loop {
        if !HDA_READY.load(Ordering::Acquire) {
            return;
        }
        let virt = *HDA_VIRT.lock();
        if virt == 0 {
            return;
        }
        let mmio = virt as *mut u8;
        let sd = *HDA_SD.lock();
        // Read SD_STS first before calling hda_feed_samples
        let sts = unsafe { r8(mmio, sd + SD_STS) };
        if sts & 0x04 != 0 {
            break; // BCIS set → half-buffer complete
        }
        core::hint::spin_loop();
    }
}

/// Poll with optional TSC‑based timeout.  Returns `true` when data
/// was fed, `false` on timeout / not ready.
pub fn hda_poll_block(timeout_tsc: Option<u64>) -> bool {
    if !HDA_READY.load(Ordering::Acquire) {
        return false;
    }
    let virt = *HDA_VIRT.lock();
    if virt == 0 {
        return false;
    }
    let mmio = virt as *mut u8;
    let sd = *HDA_SD.lock();
    let deadline = match timeout_tsc {
        Some(d) => unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(d),
        None => u64::MAX,
    };
    loop {
        let sts = unsafe { r8(mmio, sd + SD_STS) };
        if sts & 0x04 != 0 {
            return true; // BCIS set
        }
        if timeout_tsc.is_some() && unsafe { core::arch::x86_64::_rdtsc() } >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
}

/// TSC‑based delay after HDA poll (used for silence drain).
pub fn hda_poll_delay(tsc_per_ms: u64, ms: u64) {
    let deadline =
        unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(tsc_per_ms.saturating_mul(ms));
    while unsafe { core::arch::x86_64::_rdtsc() } < deadline {
        hda_poll();
        core::hint::spin_loop();
    }
}

/// High‑level PCM feed: try to push `pcm[pcm_off..pcm_total]` into
/// the HDA half‑buffer.  Advances `*pcm_off`.  Returns immediately
/// if the destination half is not ready.
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

/// Return the total number of PCM bytes the HDA hardware has
/// consumed (played back) since the stream was started.
///
/// Reads the raw SD_LPIB register which the controller updates
/// in real time.  The returned value wraps at `audio_sz` bytes
/// (the DMA ring‑buffer size), but for frame‑sync purposes the
/// caller can track wraps using `pcm_fed` comparison.
pub fn hda_playback_progress() -> Option<u64> {
    if !HDA_READY.load(Ordering::Acquire) {
        return None;
    }
    let virt = *HDA_VIRT.lock();
    if virt == 0 {
        return None;
    }
    let sd = *HDA_SD.lock();
    let mmio = virt as *mut u8;
    let raw = unsafe { r32(mmio, sd + SD_LPIB) };
    Some(raw as u64)
}

/// Feed silence into the HDA half‑buffer.
pub fn hda_feed_silence(half: usize) -> usize {
    // Allocate a zeroed buffer on the stack sized to match the half buffer.
    // The maximum half size is derived from DMA_BUF_SIZE minus BDL overhead:
    // audio_sz = DMA_BUF_SIZE - bdl_sz = 32768 - 64 = 32704, half = audio_sz/2 = 16352.
    const MAX_SILENCE: usize = 16352;
    let buf = [0u8; MAX_SILENCE];
    hda_feed_samples(&buf[..half.min(MAX_SILENCE)])
}
