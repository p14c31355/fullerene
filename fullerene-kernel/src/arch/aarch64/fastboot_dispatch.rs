//! The fastboot verb dispatch - the aarch64-shared wire layer (the v331
//! consolidation, the same pattern as `super::adb_dispatch`).
//!
//! The command surface splits:
//! - **The variable table** (`getvar:*`): the static identity replies
//!   (version, secure, ...) registered per SoC, plus one computed
//!   callback (the partition-size lookup - storage-specific).
//! - **The action verbs** (download/flash/erase/reboot/oem *): the
//!   SoC-side handlers stay registered as callbacks; the shared layer
//!   owns the reply shapes (OKAY/FAIL/data) so the wire contract is
//!   written once.
//!
//! The transport is injected via [`FastbootIo`] - the dispatch touches
//! no MMIO, exactly like [`crate::adb_dispatch::AdbDeviceIo`].

/// The variable reply: a static string or a computed callback.
pub enum VarReply {
    /// The static OKAY reply (the identity strings).
    Static(&'static [u8]),
    /// Computed: partition-size and friends. The closure writes the
    /// reply into `out` and returns its length.
    Computed(fn(name: &[u8], out: &mut [u8]) -> usize),
}

/// One registered variable: the name (without the `getvar:` prefix).
pub struct VarEntry {
    pub name: &'static [u8],
    pub reply: VarReply,
}

/// The fastboot transport + the SoC hooks (the same injection pattern
/// as `AdbDeviceIo`).
pub trait FastbootIo {
    /// One bulk-IN send (the EP1 arm).
    fn bulk_send(&mut self, data: &[u8]);
    /// The numbered log line.
    fn log(&mut self, tag: &str, value: u64);
    /// The ring event tee.
    fn event(&mut self, tag: &[u8]);
}

/// Reply `OKAY<value>` for a registered variable, or the FAIL fallback.
/// Returns true when the verb was consumed.
pub fn getvar_dispatch(
    io: &mut dyn FastbootIo,
    vars: &[VarEntry],
    name: &[u8],
    out: &mut [u8],
) -> bool {
    // partition-size:<name> needs a computed reply, so it leaves the
    // static-string match below (the client asks this before flashing;
    // a FAIL makes it warn "partition size: 0" but continue).
    if let Some(part) = name.strip_prefix(b"partition-size:") {
        for entry in vars {
            if let VarReply::Computed(f) = entry.reply {
                if entry.name == b"partition-size" {
                    let n = f(part, out);
                    io.bulk_send(&out[..n]);
                    return true;
                }
            }
        }
        io.bulk_send(b"FAILunknown partition");
        return true;
    }
    for entry in vars {
        if entry.name == name {
            match entry.reply {
                VarReply::Static(reply) => io.bulk_send(reply),
                // A computed variable never takes arguments: it reports
                // the board's own state (`emmc` reports the clock the
                // boot settled on).
                VarReply::Computed(f) => {
                    let n = f(name, out);
                    io.bulk_send(&out[..n]);
                }
            }
            return true;
        }
    }
    io.bulk_send(b"FAILunknown variable");
    true
}

/// The USB string descriptor builder: a length-prefixed UTF-16LE
/// descriptor for `text`, written into `out`. Returns the used length.
/// (Replaces the hand-unrolled per-string arrays - the v331 golf.)
pub const fn string_descriptor(text: &[u8], out: &mut [u8]) -> usize {
    // bLength = 2 + 2*chars, capped to the buffer.
    let n = if text.len() * 2 + 2 > out.len() {
        (out.len() - 2) / 2
    } else {
        text.len()
    };
    out[0] = (2 + 2 * n) as u8;
    out[1] = 0x03; // STRING
    let mut i = 0;
    while i < n {
        out[2 + i * 2] = text[i];
        out[3 + i * 2] = 0;
        i += 1;
    }
    2 + 2 * n
}

/// The fastboot-class descriptor set: the DEVICE + CONFIGURATION tables
/// the host's enumeration reads. The identity (the VID:PID + the
/// strings) is per-device; the shapes are the USB-IF standard fastboot
/// class (class 0xFF / subclass 0x42 / protocol 0x03, the bulk pair).
/// `protocol` is 0x03 (fastboot) or 0x01 (the ADB-first builds).
pub const fn device_descriptor(vid: u16, pid: u16) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[0] = 18;
    d[1] = 0x01; // DEVICE
    d[2] = 0x00;
    d[3] = 0x02; // bcdUSB 2.00
    d[4] = 0x00;
    d[5] = 0x00;
    d[6] = 0x00; // class/subclass/protocol at the device level
    d[7] = 64; // bMaxPacketSize0
    d[8] = (vid & 0xff) as u8;
    d[9] = (vid >> 8) as u8;
    d[10] = (pid & 0xff) as u8;
    d[11] = (pid >> 8) as u8;
    d[12] = 0x00;
    d[13] = 0x01; // bcdDevice 1.00
    d[14] = 1; // iManufacturer
    d[15] = 2; // iProduct
    d[16] = 3; // iSerialNumber
    d[17] = 1; // bNumConfigurations
    d
}

pub const fn config_descriptor(protocol: u8) -> [u8; 32] {
    let mut c = [0u8; 32];
    // configuration
    c[0] = 9;
    c[1] = 0x02;
    c[2] = 32;
    c[3] = 0x00;
    c[4] = 1; // bNumInterfaces
    c[5] = 1; // bConfigurationValue
    c[6] = 0; // iConfiguration
    c[7] = 0x80; // bmAttributes: bus powered
    c[8] = 50; // bMaxPower: 100 mA
    // interface: fastboot class
    c[9] = 9;
    c[10] = 0x04;
    c[11] = 0; // bInterfaceNumber
    c[12] = 0; // bAlternateSetting
    c[13] = 2; // bNumEndpoints
    c[14] = 0xff; // class
    c[15] = 0x42; // subclass
    c[16] = protocol; // 0x03 fastboot / 0x01 ADB
    c[17] = 0; // iInterface
    // endpoint IN, bulk, 512
    c[18] = 7;
    c[19] = 0x05;
    c[20] = 0x81;
    c[21] = 0x02;
    c[22] = 0x00;
    c[23] = 0x02;
    c[24] = 0;
    // endpoint OUT, bulk, 512
    c[25] = 7;
    c[26] = 0x05;
    c[27] = 0x01;
    c[28] = 0x02;
    c[29] = 0x00;
    c[30] = 0x02;
    c[31] = 0;
    c
}

use crate::adb_dispatch::{ADB_LOG_BUF, anchor_adb, anchor_verb};
use crate::fastboot_session as fb;
use crate::platform::ane_usb_dev::{
    ADB_PENDING, ADB_PENDING_CAP, ADB_PENDING_LEN, ADB_PENDING_READY, DPTXFSIZ1, DTXFSTS1,
    EP1_CONVERSATIONS, LOG_END, LOG_LINES, LOG_MAIN, XFER_END, XFER_FEED, XFER_ON, XFER_WIN_END,
    adb_dispatch_io, fb_getvar_io, fb_schedule_reboot, rd, xfer_pump,
};
use core::ptr;

/// The fastboot command interpreter: one call per completed EP1 OUT
/// transfer. Every reply is derived from the command just read, so a
/// retry is always safe.
pub fn poll(log: fn(&str, u64)) {
    // A download in flight owns EP1 OUT: skip command parsing until its
    // payload is complete, then answer it once and resume.
    match fb::dl_state() {
        1 => return,
        2 => {
            let len = fb::dl_len();
            fb::dl_reset();
            let (w0, wl) = unsafe {
                let base = fb::dl_data().as_ptr();
                let mut first = [0u8; 4];
                let mut last = [0u8; 4];
                for i in 0..4 {
                    if i < len {
                        first[i] = base.add(i).read();
                    }
                    if len >= 4 {
                        last[i] = base.add(len - 4 + i).read();
                    }
                }
                (u32::from_le_bytes(first), u32::from_le_bytes(last))
            };
            log("fb: dl_len=", len as u64);
            log("fb: dl_w0=", w0 as u64);
            crate::platform::ane_usb_dev::reply_ok(log, "fb: dl_wl=", wl as u64);
            return;
        }
        _ => {}
    }
    let len = fb::cmd_len();
    if len == 0 {
        return;
    }
    // A command arriving while `oem log` streams means the client moved on
    // (or a new session started); stop the stream so it cannot flood an
    // endpoint nobody reads. The oem-log handler re-activates it below.
    unsafe {
        core::ptr::addr_of_mut!(crate::platform::ane_usb_dev::LOG_ACTIVE).write(false);
    }
    // ADB wire path: a host `adb` speaks the binary 24-byte framing whose
    // header contains NUL bytes, so this check runs on the RAW transfer
    // length before the fastboot NUL-trim below can cut it short. Text
    // commands fail the frame magic instantly and fall through untouched.
    if len >= crate::adb_wire::HEADER_BYTES {
        let raw = fb::cmd();
        // The RX FIFO can carry loader-session leftovers in front of our
        // frame (the same glued-prefix defect the text path anchors around).
        // Decoding the raw buffer made every CNXN fail the magic check and
        // slide into the fastboot fallback. Anchor on the ADB header magic
        // first; a false anchor cannot pass the trailing magic check.
        let anchored = anchor_adb(raw);
        match crate::adb_wire::decode(anchored) {
            Ok(pkt) => {
                if anchored.len() != raw.len() {
                    crate::platform::ane_fb::event(b"DEC ANCH");
                }
                crate::platform::ane_fb::event(b"DEC OK");
                // v325: adb_dispatch is the pump's deep path (the CNXN ring
                // copy + chunked send) - running it from the timer IRQ
                // collided with the boot thread's exception frame (the
                // v320-v323 corrupted ELR). From IRQ context, copy the raw
                // anchored frame into ADB_PENDING and let the next NON-IRQ
                // service pass (wait loop / SVC boundary) dispatch it.
                if crate::blackbox::in_irq_context() {
                    // v326: copy the ANCHORED frame (the stale prefix must
                    // not ride along - the deferred decode would fail its
                    // magic check and drop the reply).
                    let skip = anchored.as_ptr() as usize - raw.as_ptr() as usize;
                    unsafe {
                        let src = fb::cmd().as_ptr().add(skip);
                        let dst = core::ptr::addr_of_mut!(ADB_PENDING) as *mut u8;
                        let n = anchored.len().min(ADB_PENDING_CAP);
                        core::ptr::copy_nonoverlapping(src, dst, n);
                        core::ptr::addr_of_mut!(ADB_PENDING_LEN).write(n);
                        core::ptr::addr_of_mut!(ADB_PENDING_READY).write(true);
                    }
                    crate::platform::ane_fb::event(b"ADB DEFER");
                } else {
                    adb_dispatch_io(&pkt);
                }
                // v307: a decoded ADB frame is a live conversation - the
                // v306 quiet detector must stand down from here on.
                EP1_CONVERSATIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                return;
            }
            Err(_) => {
                // Hex clue for the photo ledger: raw length + the first four
                // bytes (an intact header at offset 0 reads "434E584E").
                let mut ev = [b' '; 18];
                ev[0] = b'N';
                ev[1] = b'G';
                ev[2] = b' ';
                let hexd = |v: u8| -> u8 { if v < 10 { b'0' + v } else { b'A' + v - 10 } };
                ev[3] = hexd(((len >> 4) & 0xf) as u8);
                ev[4] = hexd((len & 0xf) as u8);
                ev[5] = b' ';
                let mut i = 0;
                while i < 4 {
                    let b = if i < len { raw[i] } else { 0 };
                    ev[6 + i * 2] = hexd(b >> 4);
                    ev[7 + i * 2] = hexd(b & 0xf);
                    i += 1;
                }
                crate::platform::ane_fb::event(&ev);
            }
        }
    }
    // The command ends at the transfer boundary; when the wire carried a
    // NUL too, everything past the first NUL is padding/junk and ignored.
    let mut end = len;
    unsafe {
        let buf = fb::cmd().as_ptr();
        for i in 0..len {
            if buf.add(i).read() == 0 {
                end = i;
                break;
            }
        }
    }
    if end == 0 {
        return; // nothing but padding
    }
    let cmd = &fb::cmd()[..end];
    // The core can carry one stale transfer over from the loader's session;
    // anchor past any glued prefix so the dispatch still matches.
    let cmd = anchor_verb(cmd);
    // v336c: the length is consumed HERE - for real this time. v331's
    // comment claimed it, but the only write sat on the ADB-pending path,
    // so a fastboot text command stayed "received" for every later service
    // pass and was re-dispatched over and over. The client read a stale
    // reply (a `getvar` FAIL arrived for a later `oem log`) and every
    // variable looked unknown. `cmd` is a slice over the session buffer and the next
    // RX rewrites that buffer, so zeroing the length now cannot cut this
    // pass short; the host also waits for the reply before sending again,
    // so no partial transfer can be dropped here.
    unsafe {
        fb::clear_cmd();
    }
    unsafe {
        crate::platform::ane_usb_dev::DBG_FB_DISPATCH =
            crate::platform::ane_usb_dev::DBG_FB_DISPATCH.wrapping_add(1);
        crate::platform::ane_usb_dev::DBG_CMD_HEAD = 0;
        for &b in cmd.iter().take(8) {
            crate::platform::ane_usb_dev::DBG_CMD_HEAD =
                (crate::platform::ane_usb_dev::DBG_CMD_HEAD << 8) | b as u64;
        }
    }
    // v307: a dispatched bulk command is a live conversation. The v306 quiet
    // detector must stand down from here on.
    EP1_CONVERSATIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    log("fb: cmd len=", len as u64);
    log("fb: cmd8=", unsafe {
        core::ptr::addr_of!(crate::platform::ane_usb_dev::DBG_CMD_HEAD).read()
    });

    // The whole `oem log` family streams the ANE's durable text block, whose
    // addresses are ANE-only (blackbox::TEXT_BLOCK_*). Builds for another
    // platform compile this branch out and fall through to the unknown-
    // command reply below.
    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem log"
        || cmd == b"oem log full"
        || cmd == b"oem log raw"
        || cmd.starts_with(b"oem log main")
    {
        // The durable log as INFO lines, one per service pass. Plain
        // `oem log` streams the last stretch of the fixed copy; `oem log
        // full` the whole fixed copy (7 KiB - the boot's first ~200 lines
        // fill it on this platform). `oem log main [hexoff]` streams the
        // main text block (cap 0xE000) where the later sections (usb /
        // sd / emmc) actually live. `oem log raw` streams the WHOLE
        // 0xE000 ring, both sides of `raw` - after the ring wraps, the
        // pre-wrap content lives in [raw..0xE000) and the plain `main`
        // stream cannot reach it.
        let mut main = false;
        let mut written = crate::blackbox::fixed_log_len();
        let mut start = 0usize;
        if cmd.starts_with(b"oem log main") {
            main = true;
            // v331: the same computation main_log_len performed (the
            // durable main block's used length).
            let raw =
                unsafe { ptr::read_volatile((crate::blackbox::TEXT_BLOCK_BASE + 4) as *const u32) }
                    as usize;
            written = if raw < crate::blackbox::TEXT_BLOCK_CAP as usize {
                raw
            } else {
                0
            };
            let rest = &cmd[b"oem log main".len()..];
            let off = crate::adb_dispatch::parse_hex(rest.strip_prefix(b" ").unwrap_or(rest));
            start = off.min(written);
        } else if cmd == b"oem log raw" {
            main = true;
            written = crate::blackbox::TEXT_BLOCK_CAP as usize;
            start = 0;
        } else if cmd == b"oem log" {
            // Back up ~12 lines from the end of this boot's content: the
            // host reads at its own pace, so the default dump is kept short
            // and `oem log full` exists for everything.
            let mut pos = written;
            let mut seen = 0u32;
            let base =
                (crate::blackbox::TEXT_BLOCK_BASE + crate::blackbox::FIXED_LOG_OFF) as *const u8;
            while pos > 0 && seen < 12 {
                pos -= 1;
                let b = unsafe { ptr::read_volatile(base.add(pos)) };
                if b == 0 || b == b'\n' {
                    seen += 1;
                }
            }
            start = pos;
        }
        unsafe {
            core::ptr::addr_of_mut!(crate::platform::ane_usb_dev::LOG_ACTIVE).write(true);
            core::ptr::addr_of_mut!(crate::platform::ane_usb_dev::LOG_CURSOR).write(start);
            core::ptr::addr_of_mut!(LOG_LINES).write(0);
            core::ptr::addr_of_mut!(LOG_END).write(written);
            core::ptr::addr_of_mut!(LOG_MAIN).write(main);
            fb::clear_cmd();
        }
        return;
    }
    if cmd == b"reboot"
        || cmd == b"reboot-bootloader"
        || cmd == b"reboot-fastboot"
        || cmd == b"reboot-recovery"
    {
        // Reply first, then hand the loader its boot mode and reset: a
        // bootloader reboot lands in the loader's fastboot with no key
        // combo (the loader reads the PMIC reason register at its start).
        crate::platform::ane_usb_dev::ep1_send(b"OKAY");
        fb_schedule_reboot(cmd, log);
        return;
    }

    if let Some(rest) = cmd.strip_prefix(b"download:") {
        let size = crate::adb_dispatch::parse_hex(rest) as usize;
        if size == 0 || size > fb::DOWNLOAD_MAX {
            crate::platform::ane_usb_dev::ep1_send(b"FAILbad download size");
        } else {
            unsafe {
                fb::dl_begin(size as usize, crate::timer::uptime_us());
            }
            // The fastboot protocol wants "DATA" + the size as eight
            // uppercase hex digits, then the raw payload follows.
            let mut reply = [b'0'; 12];
            reply[..4].copy_from_slice(b"DATA");
            for i in 0..8 {
                let nib = ((size >> (28 - 4 * i)) & 0xF) as u8;
                reply[4 + i] = if nib < 10 {
                    b'0' + nib
                } else {
                    b'A' + nib - 10
                };
            }
            crate::platform::ane_usb_dev::ep1_send(&reply);
            log("fb: dl_arm=", size as u64);
        }
        return;
    }

    if let Some(part) = cmd.strip_prefix(b"flash:") {
        let dl_len = fb::dl_len();
        if dl_len == 0 || fb::dl_state() != 0 {
            crate::platform::ane_usb_dev::ep1_send(b"FAILno download in progress");
        } else {
            let mut nm = 0u64;
            for &b in part.iter().take(8) {
                nm = (nm << 8) | b as u64;
            }
            log("fb: flash_name=", nm);
            match crate::platform::ane_emmc::find_partition(part, log) {
                None => crate::platform::ane_usb_dev::ep1_send(b"FAILpartition not found"),
                Some((start, _last)) => {
                    log("fb: flash_lba=", start as u64);
                    let data = fb::dl_data();
                    let (ok_s, bad_s) = crate::platform::ane_emmc::write_sectors(start, data, log);
                    log("fb: flash_ok=", ok_s);
                    log("fb: flash_bad=", bad_s);
                    if bad_s == 0 {
                        crate::platform::ane_usb_dev::ep1_send(b"OKAY");
                    } else {
                        // v336j: carry the counts and the first failed
                        // chunk's status words to the host - the panel and
                        // the boot log block are both out of reach here.
                        let mut m = [0u8; 48];
                        m[..7].copy_from_slice(b"FAILwr ");
                        let mut o = 7usize;
                        let mut t = [0u8; 32];
                        for v in [
                            ok_s,
                            bad_s,
                            crate::platform::ane_emmc::LAST_WR
                                .load(core::sync::atomic::Ordering::Relaxed)
                                as u64,
                        ] {
                            let k = crate::adb_dispatch::fmt_okay_hex(v, &mut t);
                            let n = k - 6;
                            m[o..o + n].copy_from_slice(&t[6..k]);
                            o += n;
                            m[o] = b',';
                            o += 1;
                        }
                        crate::platform::ane_usb_dev::ep1_send(&m[..o - 1]);
                    }
                }
            }
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem coldreboot" {
        // The software cold reboot: HRST_REG0 = AP_S_COLDBOOT (0x00) -
        // the value the bootloader sees on a true fresh-from-off
        // power-on (downstream hi6250-reboot.c, verified on the Kirin
        // 659 family) - then the proven LPM3 NMI reset. The BL's BFM
        // reads 0x00 as a clean power-on: no failure-count escalation,
        // no eRecovery routing, and the boot-fail chain-of-custody
        // restarts. The USB-PHY-warm-state question this targets: if
        // the enumeration failures track the reset reason rather than
        // the physical rails, this lane recovers the link without the
        // physical power hold.
        log("fb: cold_req=", 1);
        crate::platform::ane_usb_dev::ep1_send(b"OKAY");
        unsafe {
            ptr::write_volatile(
                crate::platform::ane_usb_dev::PMIC_REASON_ADDR as *mut u8,
                0x00,
            );
        }
        let back = unsafe {
            ptr::read_volatile(crate::platform::ane_usb_dev::PMIC_REASON_ADDR as *const u8)
        };
        log("fb: cold_hrst=", back as u64);
        crate::platform::ane_usb_dev::wdog_drain_and_reset();
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem crash" {
        // Deliberate panic for the BFM/escalation test: exercises the v63
        // panic -> PMIC reset -> loader path on demand. The bootup-keypoint
        // write has already stamped this boot, so a crash here must NOT
        // compound into the loader's failure count.
        log("fb: crash_req=", 1);
        panic!("oem crash: deliberate");
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem sof" || cmd == b"oem sof restart" {
        // `oem sof`: the host's 1 ms frame against our counter. Two numbers
        // answer the 10x question by division: frames seen, and the counter
        // span they cover. `restart` begins a fresh window.
        if cmd == b"oem sof restart" {
            crate::platform::ane_usb_dev::sof_restart();
        }
        let (n, c0, now) = crate::platform::ane_usb_dev::sof_measure();
        let span = now.wrapping_sub(c0);
        const HEX: [u8; 16] = *b"0123456789ABCDEF";
        // Two sixteen-digit fields, one after the other: 4 + 16 + 16.
        let mut line = [b' '; 36];
        line[..4].copy_from_slice(b"sof ");
        let mut put = |at: usize, v: u64| {
            for i in 0..16 {
                line[at + i] = HEX[((v >> (60 - i * 4)) & 0xF) as usize];
            }
        };
        put(4, n); // frames
        put(20, span); // counter ticks across those frames
        crate::platform::ane_usb_dev::fb_send_info(&line[..]);
        log("fb: sof_n=", n);
        log("fb: sof_span=", span);
        let mut ok = [0u8; 32];
        let l = crate::adb_dispatch::fmt_okay_hex(n, &mut ok);
        crate::platform::ane_usb_dev::ep1_send(&ok[..l]);
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if let Some(arg) = cmd.strip_prefix(b"oem bus ") {
        // `oem bus 1|4|8`: EXT_CSD[183] plus the host's HOST_CONTROL1, with a
        // read-back that reverts both if the new width does not work. The
        // card is left at the width the read-back proved, not the one asked
        // for.
        let bits = crate::adb_dispatch::parse_hex(arg.trim_ascii()) as u8;
        log("fb: bus_req=", bits as u64);
        crate::platform::ane_usb_dev::fb_send_info(b"bus width");
        match crate::platform::ane_emmc::bus_width_set(bits, log) {
            Ok(v) => {
                // Eight lines change what each rung is worth, so re-climb.
                crate::platform::ane_emmc::clock_probe(log);
                let mut line = [0u8; 32];
                let n = crate::adb_dispatch::fmt_okay_hex(v as u64, &mut line);
                crate::platform::ane_usb_dev::ep1_send(&line[..n]);
            }
            Err(code) => {
                log("fb: bus_fail=", code as u64);
                let mut line = [b'0'; 14];
                line[..6].copy_from_slice(b"FAILbw");
                const HEX: [u8; 16] = *b"0123456789ABCDEF";
                for i in 0..8 {
                    line[6 + i] = HEX[((code >> (28 - i * 4)) & 0xF) as usize];
                }
                crate::platform::ane_usb_dev::ep1_send(&line[..]);
            }
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem hs" || cmd == b"oem hs on" || cmd == b"oem hs off" {
        // `oem hs on|off`: ask the card to switch to high-speed timing and
        // then re-run the clock ladder. CMD6 writes one EXT_CSD byte, so
        // the switch is persistent - `off` puts it back. Both directions
        // read the table before and after, so the answer is the card's.
        let on = cmd != b"oem hs off";
        log("fb: hs_req=", on as u64);
        crate::platform::ane_usb_dev::fb_send_info(b"hs switch");
        match crate::platform::ane_emmc::switch_hs(on, log) {
            Ok(v) => {
                // A timing switch only widens the ladder; climbing it is what
                // turns that into bytes per second, so do both here.
                // The envelope moved in one direction or the other: lower the
                // clock before climbing, so a card told to leave high speed is
                // never driven above default-speed timing.
                if v == 1 {
                    crate::platform::ane_emmc::clock_probe(log);
                } else {
                    crate::platform::ane_emmc::clock_to_base(log);
                    crate::platform::ane_emmc::clock_probe(log);
                }
                let mut line = [0u8; 32];
                let n = crate::adb_dispatch::fmt_okay_hex(v as u64, &mut line);
                crate::platform::ane_usb_dev::ep1_send(&line[..n]);
            }
            Err(code) => {
                log("fb: hs_fail=", code as u64);
                // `FAILhs` then eight hex digits: the stage is the high
                // byte so the reply names which step refused.
                let mut line = [b'0'; 14];
                line[..6].copy_from_slice(b"FAILhs");
                const HEX: [u8; 16] = *b"0123456789ABCDEF";
                for i in 0..8 {
                    line[6 + i] = HEX[((code >> (28 - i * 4)) & 0xF) as usize];
                }
                crate::platform::ane_usb_dev::ep1_send(&line[..]);
            }
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem accel" {
        // `oem accel`: identify, configure, and read one sample. The writes
        // only happen after WHO_AM_I said LIS3DH.
        const HEX: [u8; 16] = *b"0123456789ABCDEF";
        let id = unsafe { crate::platform::ane_accel::start() }.unwrap_or(0);
        let mut line = [b'0'; 36];
        line[..6].copy_from_slice(b"accel ");
        line[6] = HEX[(id >> 4) as usize];
        line[7] = HEX[(id & 0xF) as usize];
        crate::platform::ane_usb_dev::fb_send_info(&line[..8]);
        match unsafe { crate::platform::ane_accel::read() } {
            Some((x, y, z)) => {
                let packed = (x as u16 as u64) << 32 | (y as u16 as u64) << 16 | z as u16 as u64;
                let mut ok = [0u8; 32];
                let n = crate::adb_dispatch::fmt_okay_hex(packed, &mut ok);
                crate::platform::ane_usb_dev::ep1_send(&ok[..n]);
            }
            None => crate::platform::ane_usb_dev::ep1_send(b"FAILaccel"),
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem sensors" {
        // `oem sensors`: one INFO line per candidate in the DTS table, with
        // the id byte that came back. A part that reads its expected id is
        // the part that is fitted; the rest are the other board variants.
        crate::platform::ane_usb_dev::fb_send_info(b"sensors");
        for (name, addr, reg, want) in crate::platform::ane_sensors::CANDIDATES {
            let got = crate::platform::ane_sensors::read_id(addr, reg);
            let mut line = [b' '; 40];
            let n = name.len().min(12);
            line[..n].copy_from_slice(&name.as_bytes()[..n]);
            let mut at = n;
            line[at] = b' ';
            at += 1;
            const HEX: [u8; 16] = *b"0123456789ABCDEF";
            line[at] = HEX[((addr >> 4) & 0xF) as usize];
            line[at + 1] = HEX[(addr & 0xF) as usize];
            line[at + 2] = b'/';
            match got {
                Some(v) => {
                    line[at + 3] = HEX[(v >> 4) as usize];
                    line[at + 4] = HEX[(v & 0xF) as usize];
                    line[at + 5] = if v == want { b'*' } else { b'.' };
                    at += 6;
                }
                None => {
                    line[at + 3] = b'-';
                    line[at + 4] = b'-';
                    at += 5;
                }
            }
            crate::platform::ane_usb_dev::fb_send_info(&line[..at]);
        }
        let mask = crate::platform::ane_sensors::identify();
        log("fb: sensors=", mask as u64);
        let mut ok = [0u8; 32];
        let l = crate::adb_dispatch::fmt_okay_hex(mask as u64, &mut ok);
        crate::platform::ane_usb_dev::ep1_send(&ok[..l]);
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem keys" {
        // `oem keys`: the three key banks, read-only. Press a key between two
        // calls and the byte that changed is the pin, without trusting a
        // board variant's DTS.
        let (a, b, c) = crate::platform::ane_keys::scan();
        const HEX: [u8; 16] = *b"0123456789ABCDEF";
        let mut line = [b' '; 25];
        line[..5].copy_from_slice(b"keys ");
        for (i, v) in [a, b, c].iter().enumerate() {
            let at = 5 + i * 6;
            line[at] = HEX[(24 + i) as usize / 10];
            line[at + 1] = b'0' + ((24 + i) % 10) as u8;
            line[at + 2] = b'=';
            line[at + 3] = HEX[(v >> 4) as usize];
            line[at + 4] = HEX[(v & 0xF) as usize];
            line[at + 5] = b' ';
        }
        crate::platform::ane_usb_dev::fb_send_info(&line[..]);
        log(
            "fb: keys=",
            ((a as u64) << 16) | ((b as u64) << 8) | c as u64,
        );
        crate::platform::ane_usb_dev::ep1_send(b"OKAYkeys");
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if let Some(arg) = cmd.strip_prefix(b"oem i2cbus ") {
        // `oem i2cbus <0|2|4>`: address scan of a bus that is already clocked.
        // Reads one byte from register 0 of every 7-bit address and reports
        // the ones that acknowledge - the topology the next driver needs,
        // without a datasheet. Nothing is written anywhere.
        let bus = crate::adb_dispatch::parse_hex(arg.trim_ascii()) as u8;
        let Some(base) = crate::platform::ane_i2c::bus_base(bus) else {
            crate::platform::ane_usb_dev::ep1_send(b"FAILno such bus");
            return;
        };
        log("fb: i2cbus=", bus as u64);
        crate::platform::ane_usb_dev::fb_send_info(b"i2c scan");
        let mut found = 0u32;
        let mut line = [b' '; 72];
        line[..5].copy_from_slice(b"bus ");
        const HEX: [u8; 16] = *b"0123456789ABCDEF";
        line[4] = HEX[(bus & 0xF) as usize];
        let mut at = 6;
        for addr in 0x08u16..=0x77 {
            if unsafe { crate::platform::ane_i2c::probe_at(base, addr) } {
                if at + 3 > line.len() {
                    crate::platform::ane_usb_dev::fb_send_info(&line[..at]);
                    at = 6;
                    line[..6].copy_from_slice(b"bus ? ");
                    line[4] = HEX[(bus & 0xF) as usize];
                }
                line[at] = HEX[((addr >> 4) & 0xF) as usize];
                line[at + 1] = HEX[(addr & 0xF) as usize];
                line[at + 2] = b' ';
                at += 3;
                found += 1;
            }
        }
        if at > 6 {
            crate::platform::ane_usb_dev::fb_send_info(&line[..at]);
        }
        log("fb: i2cbus_n=", found as u64);
        let mut ok = [0u8; 32];
        let l = crate::adb_dispatch::fmt_okay_hex(found as u64, &mut ok);
        crate::platform::ane_usb_dev::ep1_send(&ok[..l]);
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    let i2c_call = cmd
        .strip_prefix(b"oem i2c ")
        .map(|a| (0u8, a))
        .or_else(|| cmd.strip_prefix(b"oem i2c4 ").map(|a| (4u8, a)));
    if let Some((hint, args)) = i2c_call {
        // `oem i2c <bus> <addr> <from> <count>` reads any clocked bus;
        // `oem i2c4 <addr> <from> <count>` is the same call with the bus
        // fixed, kept because it is what the runbooks already say.
        //
        // Read-only: an unknown register map is not a hazard when nothing is
        // written, and dumping is how the map gets learned (hi6522 at 0x6B,
        // fsa9685 at 0x25, the sensors on i2c0).
        let (bus, args) = if hint == 0 {
            let mut it = args.splitn(2, |&b| b == b' ');
            let b = it
                .next()
                .map(crate::adb_dispatch::parse_hex)
                .unwrap_or(0xff) as u8;
            (b, it.next().unwrap_or(b""))
        } else {
            (hint, args)
        };
        let Some(base) = crate::platform::ane_i2c::bus_base(bus) else {
            crate::platform::ane_usb_dev::ep1_send(b"FAILno such bus");
            return;
        };
        let mut it = args.split(|&b| b == b' ');
        let addr = it.next().map(crate::adb_dispatch::parse_hex).unwrap_or(0) as u16;
        let from = it.next().map(crate::adb_dispatch::parse_hex).unwrap_or(0) as u8;
        let count = (it.next().map(crate::adb_dispatch::parse_hex).unwrap_or(0) as usize).min(16);
        if count == 0 {
            crate::platform::ane_usb_dev::ep1_send(b"FAILbad range");
            return;
        }
        log(
            "fb: i2c4_req=",
            ((addr as u64) << 16) | ((from as u64) << 8) | count as u64,
        );
        let mut bytes = [0u8; 16];
        // Only i2c4 has to be brought up by us; i2c0 and i2c2 are clocked
        // by the loader (the touch bus and the PMIC's, which works).
        let live = if bus == 4 {
            crate::platform::ane_usb_dev::fb_send_info(b"i2c4 bring-up");
            unsafe { crate::platform::ane_i2c::bring_up(log) }
        } else {
            true
        };
        if !live {
            // The reset never released: touching the controller now is the
            // unclocked-peripheral read that stalls this fabric.
            crate::platform::ane_usb_dev::fb_send_info(b"i2c4 reset stuck");
            crate::platform::ane_usb_dev::ep1_send(b"FAILi2c reset");
            return;
        }
        let r = unsafe {
            crate::platform::ane_i2c::read_regs_at(base, addr, from, &mut bytes[..count])
        };
        match r {
            Ok(()) => {
                let mut line = [b' '; 72];
                line[..3].copy_from_slice(b"i2c");
                line[3] = b'0' + (bus % 10);
                line[4] = b' ';
                let mut at = 5;
                const HEX: [u8; 16] = *b"0123456789ABCDEF";
                at += {
                    line[at] = HEX[(addr >> 4) as usize & 0xF];
                    line[at + 1] = HEX[(addr & 0xF) as usize];
                    2
                };
                line[at] = b' ';
                at += 1;
                at += {
                    line[at] = HEX[(from >> 4) as usize];
                    line[at + 1] = HEX[(from & 0xF) as usize];
                    2
                };
                line[at] = b':';
                at += 1;
                line[at] = b' ';
                at += 1;
                let n = crate::platform::ane_i2c::hex_row(&mut line, at, &bytes[..count]);
                crate::platform::ane_usb_dev::fb_send_info(&line[..n]);
                crate::platform::ane_usb_dev::ep1_send(b"OKAY");
            }
            Err(e) => {
                let mut line = [b' '; 40];
                line[..3].copy_from_slice(b"i2c");
                line[3] = b'0' + (bus % 10);
                line[4] = b' ';
                line[5] = b'!';
                const HEX: [u8; 16] = *b"0123456789ABCDEF";
                let mut i = 6;
                for sh in [28u32, 24, 20, 16, 12, 8, 4, 0] {
                    line[i] = HEX[((e >> sh) & 0xF) as usize];
                    i += 1;
                }
                crate::platform::ane_usb_dev::fb_send_info(&line[..i]);
                crate::platform::ane_usb_dev::ep1_send(b"FAILi2c");
            }
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if let Some(args) = cmd.strip_prefix(b"oem scan ") {
        // `oem scan <lba-hex> <count-hex>`: compact non-zero-word scan.
        let mut lba = 0u32;
        let mut count = 0u32;
        let mut it = args.split(|&b| b == b' ');
        if let Some(a) = it.next() {
            lba = crate::adb_dispatch::parse_hex(a) as u32;
        }
        if let Some(b) = it.next() {
            count = crate::adb_dispatch::parse_hex(b) as u32;
        }
        if count == 0 || count > 256 {
            crate::platform::ane_usb_dev::ep1_send(b"FAILbad range");
        } else {
            log("fb: scan_req=", ((lba as u64) << 32) | count as u64);
            crate::platform::ane_emmc::scan_sectors(lba, count, log);
            crate::platform::ane_usb_dev::ep1_send(b"OKAYscan");
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd.starts_with(b"oem ringsave") {
        // On-demand full-ring persistence from the command context (see
        // ane_emmc::persist_all for why it must not run from the IRQ).
        let (ok, bad) = crate::platform::ane_emmc::persist_all(log);
        log("em: rs_ok=", ok);
        log("em: rs_bad=", bad);
        crate::platform::ane_usb_dev::ep1_send(b"OKAYsaved");
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if let Some(args) = cmd.strip_prefix(b"oem xfer ") {
        // `oem xfer r <addr-hex> <len-hex>`    - raw RAM bytes
        // `oem xfer m <lba-hex> <sectors-hex>` - raw eMMC sectors
        // Reply `OKAY<16-hex-byte-count>` then stream the payload as raw
        // bulk packets. The host tool reads them directly, which drains tens
        // of KiB per second instead of one INFO line per service pass.
        let mut kind = 0u8;
        let mut arg = 0u64;
        let mut len = 0u64;
        let mut it = args.split(|&b| b == b' ');
        if let Some(k) = it.next() {
            kind = k.first().copied().unwrap_or(0);
            if let Some(a) = it.next() {
                arg = crate::adb_dispatch::parse_hex(a) as u64;
            }
            if let Some(l) = it.next() {
                len = crate::adb_dispatch::parse_hex(l) as u64;
            }
        }
        let total: u64 = if kind == b'r' as u8 { len } else { len * 512 };
        if total == 0 || total > 0x40_0000 || (kind != b'r' as u8 && kind != b'm' as u8) {
            crate::platform::ane_usb_dev::ep1_send(b"FAILbad xfer");
        } else {
            let mut hdr = [0u8; 20];
            hdr[..4].copy_from_slice(b"OKAY");
            let hexd = b"0123456789abcdef";
            let mut v = total;
            for i in (4..20).rev() {
                hdr[i] = hexd[(v & 0xf) as usize];
                v >>= 4;
            }
            crate::platform::ane_usb_dev::ep1_send(&hdr);
            if kind == b'r' as u8 {
                // v220: vendor-pattern pump stream. Arm bounded windows and
                // feed <=512 B chunks from service() (a >512 B request into
                // the non-periodic FIFO is the documented wedge). The pump
                // feeds the first chunks immediately; the timer-IRQ passes
                // carry the rest while the host reads.
                unsafe {
                    core::ptr::addr_of_mut!(XFER_FEED).write(arg as usize);
                    core::ptr::addr_of_mut!(XFER_END).write((arg + total) as usize);
                    core::ptr::addr_of_mut!(XFER_WIN_END).write(arg as usize);
                    core::ptr::addr_of_mut!(XFER_ON).write(true);
                }
                xfer_pump();
            } else {
                let mut remain = total;
                let buf = unsafe { &mut *core::ptr::addr_of_mut!(ADB_LOG_BUF) };
                while remain > 0 {
                    let chunk = core::cmp::min(remain, 512) as usize;
                    let lba = arg + (total - remain) / 512;
                    if !crate::platform::ane_emmc::read_sectors_bytes(
                        lba as u32,
                        1,
                        &mut buf[..],
                        log,
                    ) {
                        break;
                    }
                    crate::platform::ane_usb_dev::ep1_send(&buf[..chunk]);
                    // Keep the loader watchdog fed while the stream holds the CPU.
                    crate::platform::ane::watchdog_kick();
                    // Wait for the FIFO to drain (the host consumed this chunk's
                    // transfer) before arming the next one: arming over a pending
                    // transfer wedges the endpoint (a 112-chunk burst died with
                    // host Errno 5). Bounded, so a stalled host cannot hang the
                    // handler forever.
                    let depth_words = ((rd(DPTXFSIZ1) >> 16) & 0xffff) as usize;
                    let mut spins = 0u32;
                    while (((rd(DTXFSTS1) & 0xffff) as usize) < depth_words) && spins < 0x20_0000 {
                        spins += 1;
                    }
                    remain -= chunk as u64;
                }
            }
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if let Some(args) = cmd.strip_prefix(b"oem dump ") {
        // `oem dump <lba-hex> <count-hex>`: read sectors and log them as
        // "em: dd=" hex lines (2 words per line). BFM forensics: misc @
        // 0x7C000, bootfail_info @ 0x1E8000, oeminfo @ 0x8000.
        let mut lba: u32 = 0;
        let mut count: u32 = 0;
        let mut it = args.split(|&b| b == b' ');
        if let Some(a) = it.next() {
            lba = crate::adb_dispatch::parse_hex(a) as u32;
        }
        if let Some(b) = it.next() {
            count = crate::adb_dispatch::parse_hex(b) as u32;
        }
        if count == 0 || count > 16 {
            crate::platform::ane_usb_dev::ep1_send(b"FAILbad range");
        } else {
            log("fb: dump_req=", ((lba as u64) << 32) | count as u64);
            crate::platform::ane_emmc::dump_sectors(lba, count, log);
            crate::platform::ane_usb_dev::ep1_send(b"OKAYdump");
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd == b"oem udmount" {
        // On-demand userdata mount: crash-prone on the real 20 GB F2FS
        // (the v102 boot-time attempt crash-looped), so it never runs in
        // the boot path - a crash here self-limits, the next boot starts
        // clean and does not remount.
        log("fb: udm1=", crate::timer::uptime_us());
        crate::platform::ane_emmc::emmc_userdata_mount(log);
        log("fb: udm2=", crate::timer::uptime_us());
        crate::platform::ane_usb_dev::ep1_send(b"OKAYudm");
        return;
    }

    if cmd == b"oem touch" {
        // One plain eMMC sector read from the USB command context (LBA 0):
        // isolates the post-boot read path from the GPT logic.
        log("fb: ot1=", crate::timer::uptime_us());
        let mut t = [0u32; 128];
        let (ok, sts, err) = crate::platform::ane_emmc::sdhci_read_probe(0, &mut t, log);
        log("fb: ot2=", crate::timer::uptime_us());
        log("fb: ot_ok=", ok as u64);
        log("fb: ot_sts=", sts as u64);
        log("fb: ot_err=", err as u64);
        log("fb: ot_w0=", t[0] as u64);
        crate::platform::ane_usb_dev::ep1_send(b"OKAYtouch");
        return;
    }

    if cmd == b"oem ps1" {
        log("fb: ps1=", crate::timer::uptime_us());
        crate::platform::ane_usb_dev::ep1_send(b"OKAYps1");
        return;
    }

    if cmd == b"oem ps2" {
        log("fb: ps2a=", crate::timer::uptime_us());
        let found = crate::platform::ane_emmc::find_partition(b"kernel", log);
        log("fb: ps2b=", crate::timer::uptime_us());
        let mut gv = [0u8; 32];
        let n = crate::adb_dispatch::fmt_okay_hex(found.map(|f| f.0 as u64).unwrap_or(0), &mut gv);
        crate::platform::ane_usb_dev::ep1_send(&gv[..n]);
        return;
    }

    if cmd == b"oem ps3" {
        log("fb: ps3a=", crate::timer::uptime_us());
        let mut gv = [0u8; 32];
        let n = crate::adb_dispatch::fmt_okay_hex(0xDEAD_BEEF, &mut gv);
        log("fb: ps3b=", crate::timer::uptime_us());
        crate::platform::ane_usb_dev::ep1_send(&gv[..n]);
        return;
    }

    if cmd == b"oem ps4" {
        // The full branch logic, but the read is one sector only.
        log("fb: ps4a=", crate::timer::uptime_us());
        let mut t = [0u32; 128];
        let (ok, _sts, _err) = crate::platform::ane_emmc::sdhci_read_probe(0, &mut t, log);
        log("fb: ps4b=", crate::timer::uptime_us());
        let mut gv = [0u8; 32];
        let n = crate::adb_dispatch::fmt_okay_hex(ok as u64, &mut gv);
        crate::platform::ane_usb_dev::ep1_send(&gv[..n]);
        return;
    }

    if let Some(part) = cmd.strip_prefix(b"erase:") {
        let mut nm = 0u64;
        for &b in part.iter().take(8) {
            nm = (nm << 8) | b as u64;
        }
        log("fb: erase_name=", nm);
        match crate::platform::ane_emmc::find_partition(part, log) {
            None => crate::platform::ane_usb_dev::ep1_send(b"FAILpartition not found"),
            Some((first, last)) => {
                log("fb: erase_fr=", ((first as u64) << 32) | last as u64);
                let (ok, err) = crate::platform::ane_emmc::erase_sectors(first, last, log);
                log("fb: erase_ok=", ok as u64);
                log("fb: erase_err=", err as u64);
                if ok {
                    crate::platform::ane_usb_dev::ep1_send(b"OKAY");
                } else {
                    crate::platform::ane_usb_dev::ep1_send(b"FAILerase error");
                }
            }
        }
        return;
    }

    let mut reply: &[u8] = b"FAILunknown command";
    if let Some(name) = cmd.strip_prefix(b"getvar:") {
        // v331 golf: the variable replies dispatch through the shared
        // fastboot_dispatch table (the identity strings + the computed
        // partition-size lookup registered below). The computed path
        // sends directly and returns.
        if name == b"max-download-size" {
            reply = b"OKAYC00000";
        } else {
            fb_getvar_io(name);
            return;
        }
    }
    crate::platform::ane_usb_dev::ep1_send(reply);
}
