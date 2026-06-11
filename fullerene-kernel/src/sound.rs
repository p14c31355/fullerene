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
const VERB_SET_CONNECTION_SELECT: u32 = 0x701;
const VERB_GET_CONNECTION_LIST_ENTRY: u32 = 0xF02;
const VERB_GET_PIN_SENSE: u32 = 0xF09;
const VERB_GET_AMP_GAIN_MUTE: u32 = 0x00B;
const VERB_GET_POWER_STATE: u32 = 0xF05;
const VERB_GET_CONFIG_DEFAULT: u32 = 0xF1C;
const VERB_GET_SUBSYSTEM_ID: u32 = 0xF20;
const PARAM_VENDOR_ID: u16 = 0x00;
const PARAM_REVISION_ID: u16 = 0x02;
const PARAM_SUBORDINATE_COUNT: u16 = 0x04;
const PARAM_AUDIO_WIDGET_CAP: u16 = 0x09;
const PARAM_PCM: u16 = 0x0A;
const PARAM_STREAM: u16 = 0x0B;
const PARAM_PIN_CAP: u16 = 0x0C;
const PARAM_INPUT_AMP_CAP: u16 = 0x0D;
const PARAM_OUTPUT_AMP_CAP: u16 = 0x12;
const PARAM_CONNECTION_LIST_LEN: u16 = 0x0E;
const PARAM_POWER_STATE: u16 = 0x0F;
const WTYPE_AUDIO_OUTPUT: u32 = 0x0;
const WTYPE_AUDIO_INPUT: u32 = 0x1;
const WTYPE_AUDIO_MIXER: u32 = 0x2;
const WTYPE_AUDIO_SELECTOR: u32 = 0x3;
const WTYPE_PIN_COMPLEX: u32 = 0x4;
const WTYPE_POWER_WIDGET: u32 = 0x5;
const WTYPE_VOLUME_KNOB: u32 = 0x6;
const WTYPE_BEEP_GENERATOR: u32 = 0x7;
const WTYPE_VENDOR_DEFINED: u32 = 0xF;
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

macro_rules! mmio {
    (r32 $m:expr, $o:expr) => {
        unsafe { core::ptr::read_volatile($m.add($o) as *const u32) }
    };
    (w32 $m:expr, $o:expr, $v:expr) => {
        unsafe { core::ptr::write_volatile($m.add($o) as *mut u32, $v) }
    };
    (r16 $m:expr, $o:expr) => {
        unsafe { core::ptr::read_volatile($m.add($o) as *const u16) }
    };
    (w16 $m:expr, $o:expr, $v:expr) => {
        unsafe { core::ptr::write_volatile($m.add($o) as *mut u16, $v) }
    };
    (r8 $m:expr, $o:expr) => {
        unsafe { core::ptr::read_volatile($m.add($o)) }
    };
    (w8 $m:expr, $o:expr, $v:expr) => {
        unsafe { core::ptr::write_volatile($m.add($o), $v) }
    };
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
            // Ensure both Memory Space (bit 1) AND Bus Mastering (bit 2)
            // are set.  CORB/RIRB DMA requires Bus Mastering; without it
            // all verbs timeout on real hardware (QEMU is lenient).
            cfg.command |= 0x0006;
            let v = (cfg.status as u32) << 16 | (cfg.command as u32);
            PciConfigSpace::write_config_dword(&mut cfg, bus, d, 0, 0x04, v);
            // Re-read PCI command to confirm it stuck
            let cmd_after =
                PciConfigSpace::read_config_word(bus, d, 0, 4);

            // ── Quick MMIO probe ──────────────────────────────────
            let mmio = (bar0 + off) as *mut u8;
            let gcap = unsafe { mmio!(r32 mmio, GCAP) };
            let states = unsafe { mmio!(r16 mmio, STATESTS) };

            log::info!(
                "Sound: HDA {:04x}:{:02x}.{} [{:#06x}:{:#06x}] BAR0=0x{:016x} GCAP=0x{:08x} STATESTS=0x{:04x} CMD=0x{:04x}",
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

            // Codec #0 connected → this is the real audio controller.
            if states & 0x0001 != 0 {
                log::info!(
                    "Sound: selecting HDA {:04x}:{:02x}.{} (codec connected)",
                    bus,
                    d,
                    0
                );
                return Some((bus, d, 0, bar0));
            }

            fallback = Some((bus, d, 0, bar0));
        }
    }

    if let Some(ref b) = fallback {
        log::info!(
            "Sound: falling back to HDA {:04x}:{:02x}.{} (no codec detected on any HDA)",
            b.0,
            b.1,
            b.2
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
        let wp = unsafe { mmio!(r16 mmio, CORBWP) } as usize;
        let rp = unsafe { mmio!(r16 mmio, CORBRP) } as usize & 0xFF;
        if (wp + 1) % corb_n != rp {
            break;
        }
        core::hint::spin_loop();
    }
    // ── Ensure CORB/RIRB DMA engines are running ─────────────────
    // Real hardware may have encountered a previous memory error
    // (CORBMEI / RIRBMEI) that stopped the DMA engines.  We must
    // clear the MEI bits (WC) and re-enable the engines before
    // sending a new verb.
    let corb_ctl = unsafe { mmio!(r32 mmio, CORBCTL) };
    if corb_ctl & 0x0100 != 0 {
        // CORBMEI set — clear it (WC) and restart the CORB engine
        unsafe { mmio!(w32 mmio, CORBCTL, corb_ctl | 0x0100) };
        log::info!("Sound: CORBMEI cleared, restarting CORB DMA");
    }
    let rirb_ctl_check = unsafe { mmio!(r32 mmio, RIRBCTL) };
    if rirb_ctl_check & 0x0100 != 0 {
        // RIRBMEI set — clear it (WC) and restart the RIRB engine
        unsafe { mmio!(w32 mmio, RIRBCTL, rirb_ctl_check | 0x0100) };
        log::info!("Sound: RIRBMEI cleared, restarting RIRB DMA");
    }
    // Re-read to confirm we are running
    let corb_ctl2 = unsafe { mmio!(r32 mmio, CORBCTL) };
    let rirb_ctl2 = unsafe { mmio!(r32 mmio, RIRBCTL) };
    if corb_ctl2 & 0x0002 == 0 || corb_ctl2 & 0x0100 != 0 {
        // Not running or MEI still set — attempt full restart
        // HDA spec §4.8: write size+MEI+run in one go
        let corb_sz = (corb_ctl2 >> 16) & 0x03;
        unsafe {
            mmio!(w32 mmio, CORBCTL, 0x0102 | (corb_sz << 16));
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        log::info!("Sound: CORB restart: CTL=0x{:08x}", corb_ctl2);
    }
    if rirb_ctl2 & 0x0002 == 0 || rirb_ctl2 & 0x0100 != 0 {
        let rirb_sz = (rirb_ctl2 >> 16) & 0x03;
        unsafe {
            mmio!(w32 mmio, RIRBCTL, 0x0102 | (rirb_sz << 16));
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        log::info!("Sound: RIRB restart: CTL=0x{:08x}", rirb_ctl2);
    }

    let wp = unsafe { mmio!(r16 mmio, CORBWP) } as usize;
    let next_wp = (wp + 1) % corb_n;
    // Write the CORB entry, then issue a full memory fence.
    // Without mfence, the write may still be in the CPU store buffer
    // when the HDA controller reads it via DMA → memory error → CORBMEI
    unsafe { core::ptr::write_volatile(corb.add(next_wp), cmd) };
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    unsafe { mmio!(w16 mmio, CORBWP, next_wp as u16) };
    let rirb_wp_before = unsafe { mmio!(r16 mmio, RIRBWP) } & 0xFF;
    for _ in 0..50_000 {
        let rirb_wp = unsafe { mmio!(r16 mmio, RIRBWP) } & 0xFF;
        if rirb_wp != rirb_wp_before {
            let resp = unsafe { core::ptr::read_volatile(rirb.add(rirb_wp as usize)) };
            let unsol = (resp >> 63) & 1;
            if unsol == 0 {
                let raw = (resp >> 32) as u32;
                // Log first successful verb response for debugging
                // (only for the initial discovery probes)
                if verb == VERB_GET_PARAM && payload <= 0x12 {
                    log::info!(
                        "Sound: verb OK c={} n={:#x} v={:#03x} → raw=0x{:08x}",
                        codec, node, verb, raw
                    );
                }
                return raw;
            }
        }
        core::hint::spin_loop();
    }
    // Dump CORB/RIRB register state for timeout diagnosis
    let corb_ctl = unsafe { mmio!(r32 mmio, CORBCTL) };
    let rirb_ctl = unsafe { mmio!(r32 mmio, RIRBCTL) };
    let corb_wp = unsafe { mmio!(r16 mmio, CORBWP) };
    let corb_rp = unsafe { mmio!(r16 mmio, CORBRP) };
    let rirb_wp = unsafe { mmio!(r16 mmio, RIRBWP) };
    log::warn!(
        "Sound: verb timeout c={} n={:#x} v={:#03x} p={:#x}",
        codec,
        node,
        verb,
        payload
    );
    log::warn!(
        "Sound:  CORB CTL=0x{:08x} WP=0x{:04x} RP=0x{:04x}  RIRB CTL=0x{:08x} WP=0x{:04x}",
        corb_ctl, corb_wp, corb_rp, rirb_ctl, rirb_wp
    );
    0xFFFF_FFFF
}

/// Widget type name for logging.
fn widget_type_name(wtype: u32) -> &'static str {
    match wtype {
        WTYPE_AUDIO_OUTPUT => "AudioOut",
        WTYPE_AUDIO_INPUT => "AudioIn",
        WTYPE_AUDIO_MIXER => "Mixer",
        WTYPE_AUDIO_SELECTOR => "Selector",
        WTYPE_PIN_COMPLEX => "Pin",
        WTYPE_POWER_WIDGET => "Power",
        WTYPE_VOLUME_KNOB => "VolumeKnob",
        WTYPE_BEEP_GENERATOR => "BeepGen",
        WTYPE_VENDOR_DEFINED => "VendorDef",
        _ => "Unknown",
    }
}

/// Dump all pin capabilities as a human-readable flag string.
fn pin_cap_str(pincap: u32) -> &'static str {
    // Allocate a static buffer per invocation (single‑threaded kernel).
    // We use 6 static string slices to cover all bits we care about.
    static mut PIN_CAP_BUF: [u8; 128] = [0; 128];
    let buf = unsafe { &mut PIN_CAP_BUF[..] };
    let mut pos = 0;
    macro_rules! push {
        ($s:expr) => {
            let s = $s;
            let len = s.len();
            if pos + len < buf.len() {
                buf[pos..pos + len].copy_from_slice(s.as_bytes());
                pos += len;
            }
        };
    }
    if pincap & (1 << 0) != 0 { push!("ImpSense "); }
    if pincap & (1 << 1) != 0 { push!("TrigReq "); }
    if pincap & (1 << 2) != 0 { push!("PresDet "); }
    if pincap & (1 << 4) != 0 { push!("OUT "); }
    if pincap & (1 << 5) != 0 { push!("IN "); }
    if pincap & (1 << 6) != 0 { push!("Balanced "); }
    if pincap & (1 << 7) != 0 { push!("HP-Drv "); }
    if pincap & (1 << 8) != 0 { push!("Vref "); }
    if pincap & (1 << 16) != 0 { push!("EAPD "); }
    if pincap & (1 << 24) != 0 { push!("DP "); }
    if pincap & (1 << 25) != 0 { push!("HDMI "); }
    if pos == 0 {
        push!("(none)");
    }
    buf[pos] = 0;
    unsafe { core::str::from_utf8_unchecked(&buf[..pos]) }
}

/// Decode Pin Default Configuration word (verb F1C response).
fn pin_default_str(cfg: u32) -> &'static str {
    static mut PD_BUF: [u8; 256] = [0; 256];
    let buf = unsafe { &mut PD_BUF[..] };
    let location = (cfg >> 24) & 0x3F;
    let device = (cfg >> 20) & 0xF;
    let conn_type = (cfg >> 16) & 0xF;
    let color = (cfg >> 12) & 0xF;
    let misc = (cfg >> 8) & 0xF;
    let def_assoc = (cfg >> 4) & 0xF;
    let sequence = cfg & 0xF;
    let device_name = match device {
        0x0 => "LineOut", 0x1 => "Speaker", 0x2 => "HPOut",
        0x3 => "CD", 0x4 => "SPDIFOut", 0x5 => "DigitalOther",
        0x6 => "ModemLine", 0x7 => "ModemHandset", 0x8 => "LineIn",
        0x9 => "AUX", 0xA => "MicIn", 0xB => "Telephony",
        0xC => "SPDIFIn", 0xD => "DigitalOtherIn", 0xE => "ReservedE",
        0xF => "Other",
        _ => "?",
    };
    let conn_name = match conn_type {
        0x0 => "Unknown", 0x1 => "1/8\"", 0x2 => "1/4\"",
        0x3 => "ATAPI", 0x4 => "RCA", 0x5 => "Optical",
        0x6 => "OtherDigital", 0x7 => "OtherAnalog", 0x8 => "DIN",
        0x9 => "XLR", 0xF => "Other",
        _ => "?",
    };
    let is_connected = def_assoc != 0xF;
    // Use core::fmt::Write on a fixed buffer
    unsafe {
        let buf_slice = &mut buf[..];
        let mut w = WriteToBuf { buf: buf_slice, pos: 0 };
        use core::fmt::Write;
        let _ = write!(w,
            "{}(dev={}, color={:#x}, conn={}, loc={:#x}, misc={:#x}, seq={})",
            if is_connected { "" } else { "UNCONNECTED " },
            device_name,
            color,
            conn_name,
            location,
            misc,
            sequence,
        );
        let len = w.pos;
        buf[len] = 0;
        core::str::from_utf8_unchecked(&buf[..len])
    }
}

struct WriteToBuf<'a> {
    buf: &'a mut [u8],
    pos: usize,
}
impl<'a> core::fmt::Write for WriteToBuf<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let end = self.pos + bytes.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }
}

/// Dump a comprehensive inventory of every widget node reachable from
/// the AFG.  This includes:
///
/// - Codec vendor / device / revision IDs
/// - Per‑widget: node id, wcaps, type, connection list, pin caps & default,
///   amp caps (input + output), current amp state, current power state,
///   current pin control, EAPD state.
///
/// The output is intentionally verbose so that real‑hardware silent‑output
/// bugs (wrong DAC→Pin routing, muted amp, powered‑down EAPD, etc.) can be
/// diagnosed from the serial log alone.
unsafe fn dump_codec_inventory(mmio: *mut u8, codec: u8) {
    let vid = unsafe { corb_send_verb(mmio, codec, 0, VERB_GET_PARAM, PARAM_VENDOR_ID) };
    let rid = unsafe { corb_send_verb(mmio, codec, 0, VERB_GET_PARAM, PARAM_REVISION_ID) };
    let sub = unsafe {
        corb_send_verb(mmio, codec, 0, VERB_GET_PARAM, PARAM_SUBORDINATE_COUNT)
    };
    log::info!(
        "Sound: === CODEC INVENTORY (codec={}) ===",
        codec
    );
    log::info!(
        "Sound:  Vendor=0x{:08x} Rev=0x{:08x} SubCnt=0x{:08x}",
        vid, rid, sub
    );
    if sub == 0xFFFF_FFFF {
        log::warn!("Sound:  Cannot read subordinate count — aborting inventory");
        return;
    }
    let start_root = ((sub >> 16) & 0xFF) as u8;
    let count_root = (sub & 0xFF) as u8;
    if count_root == 0 {
        log::warn!("Sound:  Root has no subordinate nodes");
        return;
    }
    let end_root = start_root + count_root - 1;

    // Find AFG
    let mut afg: Option<u8> = None;
    for n in start_root..=end_root {
        let wc = unsafe { corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_AUDIO_WIDGET_CAP) };
        if wc == 0xFFFF_FFFF { continue; }
        let t = (wc >> 20) & 0xF;
        log::info!(
            "Sound:  Root node=0x{:02x} wcaps=0x{:08x} type={}({})",
            n, wc, widget_type_name(t), t
        );
        if t == WTYPE_AFG {
            afg = Some(n);
        }
    }
    let afg = match afg {
        Some(a) => a,
        None => {
            log::warn!("Sound:  No AFG found — aborting inventory");
            return;
        }
    };

    // AFG subordinate nodes
    let sub2 = unsafe { corb_send_verb(mmio, codec, afg, VERB_GET_PARAM, PARAM_SUBORDINATE_COUNT) };
    if sub2 == 0xFFFF_FFFF {
        log::warn!("Sound:  Cannot read AFG subordinate count");
        return;
    }
    let start_afg = ((sub2 >> 16) & 0xFF) as u8;
    let count_afg = (sub2 & 0xFF) as u8;
    if count_afg == 0 {
        log::warn!("Sound:  AFG has no subordinate nodes");
        return;
    }
    let end_afg = start_afg + count_afg - 1;

    log::info!("Sound:  AFG nodes {}-{}:", start_afg, end_afg);
    for n in start_afg..=end_afg {
        let wc = unsafe { corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_AUDIO_WIDGET_CAP) };
        if wc == 0xFFFF_FFFF {
            log::info!("Sound:  node=0x{:02x} *** NO RESPONSE ***", n);
            continue;
        }
        let t = (wc >> 20) & 0xF;
        log::info!(
            "Sound:  ┌─ node=0x{:02x} wcaps=0x{:08x} type={}({})",
            n, wc, widget_type_name(t), t
        );

        // ── Common: connection list ─────────────────────────
        let con_len = unsafe {
            let r = corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_CONNECTION_LIST_LEN);
            if r == 0xFFFF_FFFF { 0 } else { r & 0x7F }
        };
        if con_len > 0 {
            log::info!("Sound:  │ connections ({}):", con_len);
            for ci in 0..con_len.min(16) {
                let chunk = (ci / 4) * 4;
                let r = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_CONNECTION_LIST_ENTRY, chunk as u16)
                };
                if r == 0xFFFF_FFFF { continue; }
                let shift = (ci % 4) * 8;
                let src = ((r >> shift) & 0x7F) as u8;
                // Distinguish range vs. single entry (bit 7 of entry set → range)
                let is_range = (r >> shift) & 0x80 != 0;
                log::info!(
                    "Sound:  │   [{}] → 0x{:02x}{}",
                    ci,
                    src,
                    if is_range { " (range)" } else { "" }
                );
            }
        }

        // ── Common: current power state ─────────────────────
        let ps = unsafe { corb_send_verb(mmio, codec, n, VERB_GET_POWER_STATE, 0) };
        if ps != 0xFFFF_FFFF {
            log::info!("Sound:  │ PowerState=0x{:08x}", ps);
        }

        // ── Type‑specific parameters ─────────────────────────
        match t {
            WTYPE_AUDIO_OUTPUT | WTYPE_AUDIO_INPUT | WTYPE_AUDIO_MIXER | WTYPE_AUDIO_SELECTOR => {
                // Output amp cap (DACs, mixers, selectors, pins)
                let oac = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_OUTPUT_AMP_CAP)
                };
                if oac != 0xFFFF_FFFF {
                    let mute_capable = (oac >> 31) & 1;
                    let step_size = (oac >> 16) & 0x7F;
                    let num_steps = (oac >> 8) & 0x7F;
                    let offset = oac & 0x7F;
                    log::info!(
                        "Sound:  │ OutAmpCap=0x{:08x} mute={} stepSize={} nSteps={} offset={}",
                        oac, mute_capable, step_size, num_steps, offset
                    );
                }

                // Input amp cap (mixers, selectors, ADC)
                if t == WTYPE_AUDIO_MIXER || t == WTYPE_AUDIO_SELECTOR || t == WTYPE_AUDIO_INPUT {
                    let iac = unsafe {
                        corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_INPUT_AMP_CAP)
                    };
                    if iac != 0xFFFF_FFFF {
                        log::info!(
                            "Sound:  │ InAmpCap=0x{:08x} mute={} stepSize={} nSteps={} offset={}",
                            iac,
                            (iac >> 31) & 1,
                            (iac >> 16) & 0x7F,
                            (iac >> 8) & 0x7F,
                            iac & 0x7F
                        );
                    }
                }

                // Current amp state (read back)
                // Output amp: get left + right
                let amp_out = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_AMP_GAIN_MUTE, 0x8000)
                };
                if amp_out != 0xFFFF_FFFF {
                    let muted = (amp_out >> 7) & 1;
                    let gain = amp_out & 0x7F;
                    log::info!(
                        "Sound:  │ CurOutAmp=0x{:04x} mute={} gain={}",
                        amp_out, muted, gain
                    );
                }
                // Input amp for mixers: read index 0
                if t == WTYPE_AUDIO_MIXER || t == WTYPE_AUDIO_SELECTOR {
                    for inp_idx in 0..con_len.min(4) {
                        let amp_in = unsafe {
                            corb_send_verb(
                                mmio, codec, n, VERB_GET_AMP_GAIN_MUTE,
                                (inp_idx as u16) << 8,
                            )
                        };
                        if amp_in != 0xFFFF_FFFF {
                            let muted = (amp_in >> 7) & 1;
                            let gain = amp_in & 0x7F;
                            log::info!(
                                "Sound:  │ CurInAmp[{}]=0x{:04x} mute={} gain={}",
                                inp_idx, amp_in, muted, gain
                            );
                        }
                    }
                }

                // PCM / stream format support
                let pcm = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_PCM)
                };
                if pcm != 0xFFFF_FFFF {
                    log::info!("Sound:  │ PCM=0x{:08x}", pcm);
                }
                let stream = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_STREAM)
                };
                if stream != 0xFFFF_FFFF {
                    log::info!("Sound:  │ Stream=0x{:08x}", stream);
                }
            }
            WTYPE_PIN_COMPLEX => {
                let pincap = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_PARAM, PARAM_PIN_CAP)
                };
                if pincap != 0xFFFF_FFFF {
                    log::info!(
                        "Sound:  │ PinCap=0x{:08x} [{}]",
                        pincap,
                        pin_cap_str(pincap)
                    );
                }

                let pin_def = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_CONFIG_DEFAULT, 0)
                };
                if pin_def != 0xFFFF_FFFF {
                    log::info!(
                        "Sound:  │ PinDefault=0x{:08x} → {}",
                        pin_def,
                        pin_default_str(pin_def)
                    );
                }

                // Current pin control value
                let pin_ctl = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_PIN_CTL, 0)
                };
                if pin_ctl != 0xFFFF_FFFF {
                    let out = pin_ctl & (1 << 6) != 0;
                    let hp = pin_ctl & (1 << 7) != 0;
                    let in_en = pin_ctl & (1 << 5) != 0;
                    let vref = (pin_ctl >> 8) & 0xFF;
                    let eapd_raw = (pin_ctl >> 16) & 0xFF;
                    log::info!(
                        "Sound:  │ CurPinCtl=0x{:02x} OUT={} HP={} IN={} VRef=0x{:02x} EAPD=0x{:02x}",
                        pin_ctl, out, hp, in_en, vref, eapd_raw
                    );
                }

                // EAPD current state (if supported)
                if pincap != 0xFFFF_FFFF && (pincap >> 16) & 1 != 0 {
                    let eapd_state = unsafe {
                        corb_send_verb(mmio, codec, n, VERB_GET_EAPD, 0)
                    };
                    if eapd_state != 0xFFFF_FFFF {
                        log::info!(
                            "Sound:  │ CurEAPD=0x{:02x}",
                            eapd_state & 0xFF
                        );
                    }
                }

                // Pin sense
                let sense = unsafe {
                    corb_send_verb(mmio, codec, n, VERB_GET_PIN_SENSE, 0)
                };
                if sense != 0xFFFF_FFFF {
                    let present = (sense >> 31) & 1;
                    log::info!(
                        "Sound:  │ PinSense=0x{:08x} present={}",
                        sense, present
                    );
                }
            }
            _ => {}
        }
        log::info!("Sound:  └─ end node=0x{:02x}", n);
    }

    // ── Subsystem ID (SSID) ─────────────────────────────────
    let ssid = unsafe { corb_send_verb(mmio, codec, 0, VERB_GET_SUBSYSTEM_ID, 0) };
    if ssid != 0xFFFF_FFFF {
        log::info!(
            "Sound:  SubsystemID: {:04x}:{:04x}",
            (ssid >> 16) & 0xFFFF,
            ssid & 0xFFFF
        );
    }

    log::info!("Sound: === END CODEC INVENTORY ===");
}

/// Read the current pin control value via verb F07.
/// payload: bit 7=LR (0=drive left/right same), bit 6=OutputEn, bit 5=InputEn,
/// bits 3:0=VRef.
const VERB_GET_PIN_CTL: u32 = 0xF07;

/// Read the current EAPD state via verb F0D.
const VERB_GET_EAPD: u32 = 0xF0D;

unsafe fn discover_codec(mmio: *mut u8, codec: u8) -> Option<(u8, u8)> {
    // ── Dump full codec inventory before discovery ──────────
    unsafe { dump_codec_inventory(mmio, codec) };

    let sub = unsafe {
        corb_send_verb(
            mmio,
            codec,
            0,
            VERB_GET_PARAM,
            PARAM_SUBORDINATE_COUNT as u16,
        )
    };
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
        let cap = unsafe {
            corb_send_verb(
                mmio,
                codec,
                n,
                VERB_GET_PARAM,
                PARAM_AUDIO_WIDGET_CAP as u16,
            )
        };
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
    let sub = unsafe {
        corb_send_verb(
            mmio,
            codec,
            afg,
            VERB_GET_PARAM,
            PARAM_SUBORDINATE_COUNT as u16,
        )
    };
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
        let cap = unsafe {
            corb_send_verb(
                mmio,
                codec,
                n,
                VERB_GET_PARAM,
                PARAM_AUDIO_WIDGET_CAP as u16,
            )
        };
        if cap == 0xFFFF_FFFF {
            continue;
        }
        let t = (cap >> 20) & 0xF;
        if t == WTYPE_AUDIO_OUTPUT {
            // Skip digital converters (SPDIF) — bit 9 of wcaps.
            // 0x02/0x03 on ALC286 are analog (wcaps 0x41d, bit9=0);
            // 0x06 is digital (wcaps 0x611, bit9=1) and must be
            // skipped so audio goes through the analog speaker path.
            if (cap >> 9) & 1 != 0 {
                log::info!("Sound: skipping digital DAC 0x{:x}", n);
                continue;
            }
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
                // Prefer pins with EAPD (amplifier power control, bit 16)
                // — these are typically internal speakers on notebook
                // codecs (e.g. ALC286 pin 0x14).  An EAPD-capable pin
                // always wins over a non-EAPD pin, otherwise last seen
                // wins (which favours higher node numbers, i.e. jacks).
                // Read pin default configuration (verb F1C) to check
                // if this pin is actually connected (DefAssociation != 0xf).
                // Pins 0x17/0x1a on ALC286 have EAPD but are unconnected
                // (DefAssociation=0xf); selecting them would send audio to
                // a dead output.
                let pin_default = unsafe {
                    corb_send_verb(mmio, codec, n, 0xF1C, 0)
                };
                let is_connected = pin_default != 0xFFFF_FFFF
                    && ((pin_default >> 8) & 0xF) != 0xF;
                if (pincap >> 16) & 1 != 0 && is_connected {
                    pin = Some(n); // connected EAPD pin always wins
                } else if pin.is_none()
                    || pin.map_or(false, |p| {
                        let pc = unsafe {
                            corb_send_verb(mmio, codec, p, VERB_GET_PARAM, PARAM_PIN_CAP as u16)
                        };
                        (pc >> 16) & 1 == 0
                    })
                {
                    pin = Some(n);
                }
            } else {
                log::info!(
                    "Sound: pin 0x{:x} cap=0x{:08x} — skipping (no OUT)",
                    n,
                    pincap
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
    let ac = unsafe {
        corb_send_verb(
            mmio,
            codec,
            dac,
            VERB_GET_PARAM,
            PARAM_OUTPUT_AMP_CAP as u16,
        )
    };
    // amp_cap response: bits[31]=mute, [22:16]=stepsize, [14:8]=nsteps, [6:0]=offset
    let offset = (ac & 0x7F) as u8;
    let nsteps = ((ac >> 8) & 0x7F) as u8;
    let gain = if nsteps > 0 { offset } else { 0 };
    log::info!(
        "Sound: DAC 0x{:x} amp cap=0x{:08x} offset={} nsteps={} gain={}",
        dac, ac, offset, nsteps, gain
    );
    unsafe {
        // VERB_SET_AMP_GAIN_MUTE payload:
        //   bit15=SetOut(1 for output amp), bit13=SetLeft, bit12=SetRight,
        //   bit7=Mute(0=unmute), bits[6:0]=Gain.
        // 0xB000 = SetOut + SetLeft + SetRight.
        corb_send_verb(
            mmio,
            codec,
            dac,
            VERB_SET_AMP_GAIN_MUTE,
            0xB000u16 | gain as u16,
        )
    };
    // 16-bit signed mono at 48000 Hz:
    // bit[7] = 0 → 48 kHz base, bits[6:4] = 1 → 16-bit container,
    // bit[3:0] = 0 → 1 channel
    unsafe { corb_send_verb(mmio, codec, dac, VERB_SET_FMT, 0x10u16) };
    unsafe { corb_send_verb(mmio, codec, dac, VERB_SET_STREAM, stream as u16) };
    let pa = unsafe {
        corb_send_verb(
            mmio,
            codec,
            pin,
            VERB_GET_PARAM,
            PARAM_OUTPUT_AMP_CAP as u16,
        )
    };
    let p_offset = (pa & 0x7F) as u8;
    let p_nsteps = ((pa >> 8) & 0x7F) as u8;
    let pgain = if p_nsteps > 0 { p_offset } else { 0 };
    log::info!(
        "Sound: Pin 0x{:x} amp cap=0x{:08x} offset={} nsteps={} pgain={}",
        pin, pa, p_offset, p_nsteps, pgain
    );
    unsafe {
        // Pin output amp → SetOut(bit15) + SetLeft(bit13) + SetRight(bit12)
        corb_send_verb(
            mmio,
            codec,
            pin,
            VERB_SET_AMP_GAIN_MUTE,
            0xB000u16 | pgain as u16,
        )
    };
    // ── Route DAC → Pin through correct mixer ─────────────────
    // Many HDA codecs (ALC286 etc.) have multiple mixer widgets
    // between DACs and pin complexes.  The pin may default to a
    // mixer that connects to a different DAC (e.g. headphone DAC
    // 0x02 → mixer 0x0c → speaker pin 0x14).  We need to select
    // the mixer that contains our chosen DAC in its connection
    // list so the audio actually reaches the pin.
    let pin_con_count = unsafe {
        let r = corb_send_verb(mmio, codec, pin, VERB_GET_PARAM, 0x0Eu16 /* connection list len */);
        if r == 0xFFFF_FFFF { 0 } else { r & 0x7F }
    };
    if pin_con_count > 0 && pin_con_count != 0xFFFF_FFFF {
        // Iterate the pin's connection list entries; look for a
        // mixer that includes our DAC as an input.
        'pin_con: for con_idx in 0..pin_con_count.min(16) {
            // HDA spec §7.1.2: Get Connection List Entry offset must be a
            // multiple of 4.  Each 32-bit response packs up to four 8-bit
            // connection entries at byte positions [7:0], [15:8], [23:16],
            // [31:24].
            let chunk_idx = (con_idx / 4) * 4;
            let resp = unsafe {
                corb_send_verb(mmio, codec, pin, VERB_GET_CONNECTION_LIST_ENTRY, chunk_idx as u16)
            };
            if resp == 0xFFFF_FFFF {
                continue;
            }
            let shift = (con_idx % 4) * 8;
            let con_node = ((resp >> shift) & 0x7F) as u8;
            // Check if this connection node is a mixer
            let con_wcap = unsafe {
                corb_send_verb(
                    mmio,
                    codec,
                    con_node,
                    VERB_GET_PARAM,
                    PARAM_AUDIO_WIDGET_CAP as u16,
                )
            };
            if con_wcap == 0xFFFF_FFFF {
                continue;
            }
            let con_type = (con_wcap >> 20) & 0xF;
            if con_type != WTYPE_AUDIO_MIXER {
                // Direct connection from DAC to pin — no mixer
                // needed; keep current selection (or set to this
                // index if it matches our DAC).
                if con_node == dac {
                    let r = unsafe {
                        corb_send_verb(mmio, codec, pin, VERB_SET_CONNECTION_SELECT, con_idx as u16)
                    };
                    log::info!(
                        "Sound: SET_CONN pin=0x{:x} → DAC 0x{:x} (direct) result=0x{:08x}",
                        pin,
                        con_node,
                        r
                    );
                }
                continue;
            }
            // This is a mixer — check its own connection list for
            // our DAC.
            let mix_con_count = unsafe {
                let r = corb_send_verb(
                    mmio,
                    codec,
                    con_node,
                    VERB_GET_PARAM,
                    0x0Eu16, /* connection list len */
                );
                if r == 0xFFFF_FFFF { 0 } else { r & 0x7F }
            };
            for mix_ci in 0..mix_con_count.min(16) {
                let mix_chunk_idx = (mix_ci / 4) * 4;
                let mix_resp = unsafe {
                    corb_send_verb(
                        mmio,
                        codec,
                        con_node,
                        VERB_GET_CONNECTION_LIST_ENTRY,
                        mix_chunk_idx as u16,
                    )
                };
                if mix_resp == 0xFFFF_FFFF {
                    continue;
                }
                let mix_shift = (mix_ci % 4) * 8;
                let mix_src = ((mix_resp >> mix_shift) & 0x7F) as u8;
                if mix_src == dac {
                    // Found mixer that has our DAC as input →
                    // select this mixer on the pin and unmute
                    // the mixer input for our DAC.
                    let r = unsafe {
                        corb_send_verb(mmio, codec, pin, VERB_SET_CONNECTION_SELECT, con_idx as u16)
                    };
                    log::info!(
                        "Sound: SET_CONN pin=0x{:x} → mixer 0x{:x} (DAC 0x{:x}) result=0x{:08x}",
                        pin,
                        con_node,
                        dac,
                        r
                    );
                    // Unmute the mixer input for the DAC channel.
                    // VERB_SET_AMP_GAIN_MUTE on a mixer selects
                    // input index via bits [12:8], gain [7:0].
                    // bit15=0 (input amp), bit14=reserved (must be 0),
                    // bit13=SetLeft, bit12=SetRight, bits[11:8]=Index,
                    // bit7=Mute(0=unmute), bits[6:0]=Gain.
                    // 0x3000 → SetLeft + SetRight, index=0, gain=0, unmute.
                    // Mixer input amp: SetLeft(bit13) + SetRight(bit12) + Index(bits[11:8])
                    let unmute_payload = 0x3000u16 | ((mix_ci as u16) << 8);
                    let r2 = unsafe {
                        corb_send_verb(
                            mmio,
                            codec,
                            con_node,
                            VERB_SET_AMP_GAIN_MUTE,
                            unmute_payload,
                        )
                    };
                    log::info!(
                        "Sound: UNMUTE mixer 0x{:x} input {} result=0x{:08x}",
                        con_node,
                        mix_ci,
                        r2
                    );
                    break 'pin_con;
                }
            }
        }
    }

    // Query pin capabilities to check EAPD support (bit 16)
    let pin_cap = unsafe { corb_send_verb(mmio, codec, pin, VERB_GET_PARAM, PARAM_PIN_CAP as u16) };
    let eapd_capable = pin_cap != 0xFFFF_FFFF && (pin_cap >> 16) & 1 != 0;
    log::info!(
        "Sound: pin 0x{:x} cap=0x{:08x} eapd_capable={}",
        pin,
        pin_cap,
        eapd_capable
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
        pin,
        pin_ctl_res
    );
    log::info!("Sound: codec done DAC=0x{:x} Pin=0x{:x}", dac, pin);
}

/// Cached HDA diagnostic info, preserved across `hda_init` calls.
pub static HDA_DIAG: Mutex<HdaDiagInfo> = Mutex::new(HdaDiagInfo {
    gcap: 0,
    gcap64: false,
    corb_phys: 0,
    rirb_phys: 0,
    states_after_crst: 0,
    populated: false,
});

pub struct HdaDiagInfo {
    pub gcap: u32,
    pub gcap64: bool,
    pub corb_phys: u64,
    pub rirb_phys: u64,
    pub states_after_crst: u16,
    pub populated: bool,
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
    let gctest = unsafe { mmio!(r32 virt as *mut u8, GCAP) };
    if gctest == 0 || gctest == 0xFFFF_FFFF {
        log::warn!("Sound: GCAP invalid");
        *HDA_PHYS.lock() = 0;
        return;
    }
    log::info!("Sound: GCAP=0x{:x}", gctest);
    let (iss, oss, gcap_full, sts_post_crst) = unsafe {
        let m = virt as *mut u8;
        // ── Skip full controller reset (CRST cycle) ────────────────
        // The controller was already in a working state when we probed
        // it (STATESTS showed a connected codec).  Performing a full
        // CRST cycle on many Realtek + Intel PCH combos causes the
        // codec to enter reset and never come back (STATESTS → 0),
        // even after > 1 s wait.  We therefore leave the controller
        // as-is and only clear the status bits + disable interrupts.
        mmio!(w16 m, STATESTS, 0x000F);
        mmio!(w32 m, INTCTL, 0);
        let sts_after = mmio!(r16 m, STATESTS);
        log::info!(
            "Sound: STATESTS (no CRST) = 0x{:04x} (SDIN0={} SDIN1={} SDIN2={} SDIN3={})",
            sts_after,
            if sts_after & 0x0001 != 0 { 1u8 } else { 0u8 },
            if sts_after & 0x0002 != 0 { 1u8 } else { 0u8 },
            if sts_after & 0x0004 != 0 { 1u8 } else { 0u8 },
            if sts_after & 0x0008 != 0 { 1u8 } else { 0u8 },
        );
        let gcap_full = mmio!(r32 m, GCAP);
        let iss = (gcap_full >> 8) & 0xF;
        let oss = (gcap_full >> 12) & 0xF;
        (iss, oss, gcap_full, sts_after)
    };
    log::info!("Sound: ISS={} OSS={}", iss, oss);
    // ── Populate diagnostic cache ──────────────────────────────
    {
        let mut diag = HDA_DIAG.lock();
        diag.gcap = gcap_full;
        diag.gcap64 = gcap_full & 1 != 0;
        diag.states_after_crst = sts_post_crst;
        diag.populated = true;
    }
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
        mmio!(r16 virt as *mut u8, GCAP + 2) as u32
            | ((mmio!(r16 virt as *mut u8, GCAP) as u32) << 16)
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
        mmio!(w32 m, CORBCTL, 0);
        mmio!(w32 m, RIRBCTL, 0);
        // ── Update diagnostic cache with DMA addresses ─────────
        {
            let mut diag = HDA_DIAG.lock();
            diag.corb_phys = corb_phys;
            diag.rirb_phys = rirb_phys;
        }
        log::info!(
            "Sound: CORB phys=0x{:016x} RIRB phys=0x{:016x}",
            corb_phys, rirb_phys
        );
        // Check 64-bit support: GCAP bit 0
        let gcap64 = mmio!(r32 m, GCAP) & 1;
        log::info!("Sound: HDA 64-bit support (GCAP bit0)={}", gcap64);
        mmio!(w32 m, CORBLBASE, corb_phys as u32);
        mmio!(w32 m, CORBUBASE, (corb_phys >> 32) as u32);
        // CORB Read Pointer Reset via bit 15, then clear RP/WP
        mmio!(w16 m, CORBRP, 0x8000);
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        mmio!(w16 m, CORBRP, 0);
        mmio!(w16 m, CORBWP, 0);
        // Enable CORB DMA with the correct size
        mmio!(w32 m, CORBCTL, 0x02 | corb_sz_bits);
        mmio!(w32 m, RIRBLBASE, rirb_phys as u32);
        mmio!(w32 m, RIRBUBASE, (rirb_phys >> 32) as u32);
        // RIRBWP reset: set bit 15 (RIRBRST) then clear
        mmio!(w16 m, RIRBWP, 0x8000);
        for _ in 0..200 {
            core::hint::spin_loop();
        }
        // Read back to confirm reset is released, then zero WP
        if mmio!(r16 m, RIRBWP) & 0x8000 != 0 {
            mmio!(w16 m, RIRBWP, 0);
        }
        // Enable RIRB DMA with the correct size
        mmio!(w32 m, RIRBCTL, 0x02 | rirb_sz_bits);
        // ── Verify CORB/RIRB register state after enable ──────────
        let corb_ctl = mmio!(r32 m, CORBCTL);
        let rirb_ctl = mmio!(r32 m, RIRBCTL);
        let corb_rp = mmio!(r16 m, CORBRP);
        let corb_wp_after = mmio!(r16 m, CORBWP);
        let rirb_wp_after = mmio!(r16 m, RIRBWP);
        log::info!(
            "Sound: CORB CTL=0x{:08x} RP=0x{:04x} WP=0x{:04x}  RIRB CTL=0x{:08x} WP=0x{:04x}",
            corb_ctl, corb_rp, corb_wp_after, rirb_ctl, rirb_wp_after
        );
        log::info!("Sound: CORB/RIRB enabled (size={} entries)", corb_n);
        // ── Short delay after CORB/RIRB enable ────────────────────
        // Some Intel PCH HDA controllers require a brief settling
        // period after DMA enable before the first verb is accepted.
        for _ in 0..50000 {
            core::hint::spin_loop();
        }
    }
    let codec_addr: u8 = 0;
    unsafe {
        if let Some((dac, pin)) = discover_codec(virt as *mut u8, codec_addr) {
            // Stream tag 1 — HDA stream IDs are 1‑based; 0 means "no stream"
            // and causes real hardware to silently ignore the DMA engine.
            configure_codec(virt as *mut u8, codec_addr, dac, pin, 1);
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
        mmio!(w32 m, sd + SD_CTL, 0);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
        mmio!(w8 m, sd + SD_STS, 0xFF); // clear all status bits (WC)
        // Reset stream
        mmio!(w32 m, sd + SD_CTL, 0x01);
        for _ in 0..2000 {
            core::hint::spin_loop();
        }
        // Wait for reset to complete
        for _ in 0..50000 {
            if mmio!(r32 m, sd + SD_CTL) & 0x01 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        // Program format, BDL and stream settings
        mmio!(w8 m, sd + SD_STS, 0xFF);
        // 48 kHz 16-bit mono:
        // bit7 BASE=0 (48kHz), bits6:4 BITS=1 (16-bit), bits3:0 CHAN=0 (1ch)
        mmio!(w16 m, sd + SD_FMT, 0x0010);
        mmio!(w32 m, sd + SD_CBL, audio_sz);
        mmio!(w16 m, sd + SD_LVI, BDL_ENTRIES as u16 - 1);
        mmio!(w32 m, sd + SD_BDPL, dma_phys as u32);
        mmio!(w32 m, sd + SD_BDPU, (dma_phys >> 32) as u32);
        // Store fence: ensure BDL / DMA buffer writes are visible
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // Start stream: RUN (bit 1) + IOCE (bit 2) + STRIPE1 (bits 18:16)
        mmio!(w32 m, sd + SD_CTL, (1u32 << 16) | 0x06);
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
    let lpib_raw = unsafe { mmio!(r32 mmio, sd + SD_LPIB) };
    let lpib = lpib_raw.wrapping_rem(*HDA_AUDIO_SZ.lock());
    let write_off = if lpib < half { half } else { 0 };

    // BCIS (hardware IOC) provides a strong "half‑done" signal.
    let sts = unsafe { mmio!(r8 mmio, sd + SD_STS) };
    if sts & 0x04 != 0 {
        unsafe {
            mmio!(w8 mmio, sd + SD_STS, 0x04);
        }
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

/// Poll for HDA half‑buffer completion.
///
/// Never blocks indefinitely: an internal TSC watchdog (~100 ms at
/// 3 GHz) forces a return so the caller doesn't hang if the DMA
/// engine stalls (common on real hardware when the stream has been
/// stopped or the codec is not producing BCIS interrupts).
pub fn hda_poll() {
    let deadline =
        unsafe { core::arch::x86_64::_rdtsc() }.wrapping_add(300_000_000); // ~100 ms at 3 GHz
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
        let sts = unsafe { mmio!(r8 mmio, sd + SD_STS) };
        if sts & 0x04 != 0 {
            break; // BCIS set → half-buffer complete
        }
        if unsafe { core::arch::x86_64::_rdtsc() } >= deadline {
            return; // timeout — don't hang forever
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
        let sts = unsafe { mmio!(r8 mmio, sd + SD_STS) };
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
    let raw = unsafe { mmio!(r32 mmio, sd + SD_LPIB) };
    Some(raw as u64)
}

/// Feed silence into the HDA half‑buffer.
///
/// Uses a static zeroed buffer to avoid allocating 16 KiB on the kernel
/// stack, which could trigger a stack overflow (kernel stacks are often
/// limited to 8–16 KiB total).
pub fn hda_feed_silence(half: usize) -> usize {
    const MAX_SILENCE: usize = 16368;
    static SILENCE_BUF: [u8; MAX_SILENCE] = [0; MAX_SILENCE];
    hda_feed_samples(&SILENCE_BUF[..half.min(MAX_SILENCE)])
}