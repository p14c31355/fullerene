//! The ADB device-side dispatch - the aarch64-shared USB wire layer
//! (the v331 consolidation: the ADB plumbing was scattered in the
//! platform responders; both the ANE responder and any future bulk-ADB
//! gadget dispatch through this module).
//!
//! The framing lives in `crate::adb_wire` (the pure, host-tested
//! encoders). This module adds the device-side policy: the CNXN banner,
//! the shell services (crash/log/ver), the ring streaming, and the
//! anchor rules for the loader-glued RX prefixes.
//!
//! The transport is injected via [`AdbDeviceIo`]: the SoC port
//! implements the bulk send, the scratch buffers, the event tee, and
//! the watchdog kick. The dispatch itself touches no MMIO.

/// The transport contract the dispatch needs (the ANE responder
/// implements it over its EP1 statics; a DWC3 gadget implements it over
/// its request buffers).
pub trait AdbDeviceIo {
    /// One bulk-IN send (the EP1 arm; the pacing is the impl's business).
    fn bulk_send(&mut self, data: &[u8]);
    /// The ring event tee (the tag mirror).
    fn event(&mut self, tag: &[u8]);
    /// The watchdog kick (the long paced sends feed it).
    fn watchdog_kick(&mut self);
    /// The numbered log line.
    fn log(&mut self, tag: &str, value: u64);
    /// Read the durable ring's tail into `dst` (up to `max` bytes, the
    /// most recent first-aligned slice). Returns the bytes read.
    fn ring_tail_read(&self, dst: &mut [u8], max: usize) -> usize;
    /// Wait until the bulk-IN TX FIFO drains below the last arm's depth
    /// (the paced-send throttle; the SoC's EP FIFO state).
    fn drain_wait(&mut self);
}

/// prefix the loader's core state glued in front of it.
pub(crate) fn anchor_verb(raw: &[u8]) -> &[u8] {
    for i in 0..raw.len() {
        let s = &raw[i..];
        if s.starts_with(b"getvar:")
            || s.starts_with(b"oem ")
            || s.starts_with(b"reboot")
            || s.starts_with(b"download:")
            || s.starts_with(b"flash:")
            || s.starts_with(b"erase:")
        {
            return s;
        }
    }
    raw
}

/// Scan for the ADB header magic ("CNXN") so a frame glued behind stale
/// loader-session FIFO bytes still decodes. A false anchor is harmless: the
/// final magic check (`magic == command ^ 0xffff_ffff`) only passes on a
/// real header, and the length/checksum checks follow.
pub(crate) fn anchor_adb(raw: &[u8]) -> &[u8] {
    let mut i = 0;
    while i + 4 <= raw.len() {
        if &raw[i..i + 4] == b"CNXN" {
            return &raw[i..];
        }
        i += 1;
    }
    raw
}

/// "OKAY0x<hex>" with no leading zeros - the numeric getvar form the
/// client's strtoul(base 0) parser expects.
/// The same reply without the `0x` - what the fastboot host's own parsers
/// expect for the numeric getvars (`max-download-size`, `partition-size`).
/// They run strtoul(..., 16) over the value, which stops at the `x` and
/// reads zero, hence "couldn't parse max-download-size 'OKAY0xC00000'".
pub(crate) fn fmt_okay_hex_bare(value: u64, out: &mut [u8; 32]) -> usize {
    let n = fmt_okay_hex(value, out);
    let mut i = 6usize;
    while i + 1 < n {
        out[i] = out[i + 1];
        i += 1;
    }
    n - 1
}

pub(crate) fn fmt_okay_hex(value: u64, out: &mut [u8; 32]) -> usize {
    out[..6].copy_from_slice(b"OKAY0x");
    let mut i = 6usize;
    let mut started = false;
    let mut shift: i32 = 60;
    while shift >= 0 {
        let nib = ((value >> shift) & 0xF) as u8;
        if nib != 0 || started || shift == 0 {
            out[i] = if nib < 10 {
                b'0' + nib
            } else {
                b'A' + nib - 10
            };
            i += 1;
            started = true;
        }
        shift -= 4;
    }
    i
}

/// Dispatch scratch buffers. These live in .bss, not on the interrupt
/// stack: adb_dispatch runs from the timer IRQ, and the CNXN reply alone
/// needs 6 KiB (4 KiB out + 2 KiB banner) - more than the boot thread's
/// remaining stack at the stall depth, which is why the CNXN froze the
/// world between its entry marker and the magic check (v166). The dispatch
/// is inherently serialized (one conversation at a time), so sharing one
/// set of buffers is safe.
pub(crate) static mut ADB_OUT: [u8; 4096] = [0u8; 4096];
pub(crate) static mut ADB_BANNER: [u8; 2080] = [0u8; 2080];
pub(crate) static mut ADB_RUN: [u8; 8192] = [0u8; 8192];
pub(crate) static mut ADB_LOG_CHUNK: [u8; 2048] = [0u8; 2048];
pub(crate) static mut ADB_PAYLOAD: [u8; 2049] = [0u8; 2049];
pub(crate) static mut ADB_LOG_BUF: [u8; 512] = [0u8; 512];

/// Parse a hex offset ("1c00"); stops at the first non-hex byte.
pub(crate) fn parse_hex(bytes: &[u8]) -> usize {
    let mut v: usize = 0;
    for &b in bytes {
        let d = match b {
            b'0'..=b'9' => (b - b'0') as usize,
            b'a'..=b'f' => (b - b'a' + 10) as usize,
            b'A'..=b'F' => (b - b'A' + 10) as usize,
            _ => break,
        };
        v = v.saturating_mul(16) + d;
    }
    v
}

/// The used length of the main durable text block. The loader tramples the
/// header between boots, so an out-of-range size means an empty block (the
/// same guard the writer applies). ANE-only: the block itself lives in the
/// ANE's durable-log zone. Only the ANE USB path calls this function.

/// Minimal ADB wire responder: handshake plus enough of OPEN/write/close to
/// give a host `adb` a live device. The framing itself lives in
/// `crate::adb_wire` (pure, host-tested); this is only the plumbing that
/// drives the module's encoders onto the bulk endpoint.
// The dispatch scratch (the module owns it; the transport impls do not
// need to - the buffers are wire-format state, not SoC state).

pub(crate) fn adb_dispatch<T: AdbDeviceIo>(io: &mut T, pkt: &crate::adb_wire::Packet<'_>) {
    let out = unsafe { &mut *core::ptr::addr_of_mut!(ADB_OUT) };
    match pkt.command {
        crate::adb_wire::CNXN => {
            // Ring marker first: a photo (or the piggyback of a later boot)
            // proves whether this CNXN reached the ADB dispatch at all.
            crate::blackbox::text_puts("ADB: CNXN\n");
            io.event(b"ADB CNXN");
            // Announce version + max payload. The banner is what `adb
            // devices -l` shows, so it carries the device identity.
            //
            // v155 piggyback: the stale-boot responder reliably services the
            // FIRST conversation only (the CNXN); the follow-up OPEN dies
            // unanswered, which is why every automated ring pull has come
            // back 0 bytes. So the CNXN reply's payload itself carries the
            // banner plus the durable ring tail: the host splits at the
            // banner's NUL and writes the rest to disk in one conversation.
            let banner = unsafe { &mut *core::ptr::addr_of_mut!(ADB_BANNER) };
            io.event(b"BG");
            let head = b"device::fullerene-ane\0";
            banner[..head.len()].copy_from_slice(head);
            let mut blen = head.len();
            blen += io.ring_tail_read(&mut banner[blen..], 2048);
            io.event(b"RD");
            if let Some(n) = crate::adb_wire::cnxn_reply(
                crate::adb_wire::VERSION,
                crate::adb_wire::MAX_PAYLOAD as u32,
                &banner[..blen],
                &mut out[..],
            ) {
                io.event(b"RP");
                io.event(b"SS");
                // v175: send the reply as paced 512-byte chunks with the same
                // bounded drain wait as the xfer stream, instead of one big
                // transfer. Why: the first reply of a boot shares the TX FIFO
                // with the loader's stale leftovers ([stale][reply]); a single
                // n-byte transfer then delivers [stale][reply-head] and
                // truncates the rest (the host's frame parse rejects a header
                // whose payload is cut -> "cnxn reply: False frames: 0"),
                // while a stale-aware oversized transfer wedges EP1 (the v172
                // lesson). Chunking sidesteps both: every transfer stays
                // small AND the reply bytes end up contiguous right after the
                // stale prefix, which every host parser skips by design.
                let full = &out[..n];
                let mut off = 0usize;
                while off < full.len() {
                    let chunk = core::cmp::min(full.len() - off, 512);
                    io.bulk_send(&full[off..off + chunk]);
                    io.watchdog_kick();
                    // The paced-send drain wait is the transport's
                    // business (the EP1 TX FIFO depth is SoC state).
                    io.drain_wait();
                    off += chunk;
                }
                io.log("adb: cnxn sent", n as u64);
                // v156 cycle magic: the unmanned loop sends CNXN with arg0
                // 0xC0DE0001. The reply (with the ring) is already drained to
                // the wire above, so the deliberate panic that follows costs
                // nothing: the host keeps the ring and the device lands in
                // the loader's fastboot, where the engine flashes the next
                // image. One conversation now does collect + reset.
                //
                // v160: schedule the reset ~2 s out instead of panicking
                // immediately - the earlier shape reset the link before the
                // host's read URB could complete, so the reply was lost in
                // every attempt. The timer tick fires the delayed panic.
                if pkt.arg0 == 0xC0DE_0001 {
                    crate::blackbox::text_puts("ADB: MAGIC\n");
                    io.event(b"ADB MAGIC");
                    crate::blackbox::schedule_reset_at(
                        // v190: was 2 s. The uptime clock runs 1.3-18x real
                        // and the v189-fixed reply (up to 2093 bytes, sent
                        // in ~1-2 s paced chunks) needs a full host drain:
                        // the c17 attempt received only 496 of 2093 bytes
                        // before the reset cut the link.
                        // v191: 20 s still cut the c318 drain at 2544/3103
                        // bytes (the complete CNXN frame WAS there - stale
                        // 1008 + frame 2095). 50 s of uptime (~25-35 real
                        // seconds) clears the host's whole 20 s read window.
                        crate::timer::uptime_us().saturating_add(50_000_000),
                    );
                }
            }
        }
        crate::adb_wire::OPEN => {
            // OPEN's payload is the service string, NUL-terminated.
            let svc = pkt.payload;
            let svc = &svc[..svc.iter().position(|&b| b == 0).unwrap_or(svc.len())];
            let remote: u32 = 1;
            // One bulk send for the whole OKAY + WRTE + CLSE run: back-to-back
            // ep1_send calls arm over each other's pending transfers and wedge
            // the endpoint (same lesson as the xfer stream), and a byte stream
            // of concatenated frames is exactly what the ADB host parses.
            let run = unsafe { &mut *core::ptr::addr_of_mut!(ADB_RUN) };
            let log_chunk = unsafe { &mut *core::ptr::addr_of_mut!(ADB_LOG_CHUNK) };
            let mut at = 0usize;
            if let Some(n) =
                crate::adb_wire::encode(crate::adb_wire::OKAY, pkt.arg0, remote, &[], &mut out[..])
            {
                run[at..at + n].copy_from_slice(&out[..n]);
                at += n;
            }
            let text: &[u8] = match svc {
                b"shell:crash" => {
                    // The unmanned loop's reset trigger: the v63 panic chain
                    // lands in the loader's fastboot, where the armed engine
                    // flashes the next image. This is the ADB twin of the
                    // fastboot `oem crash` command.
                    panic!("adb crash: deliberate");
                }
                b"shell:log" => {
                    // Stream the durable ring's tail as one WRTE run: the most
                    // recent bytes are what a boot post-mortem needs, and a
                    // single send avoids the arm-over-pending wedge. 2 KiB is
                    // plenty for the last screens of log.
                    let base = crate::blackbox::TEXT_BLOCK_BASE;
                    let cap = crate::blackbox::TEXT_BLOCK_CAP as usize;
                    let raw =
                        unsafe { core::ptr::read_volatile((base + 4) as *const u32) } as usize;
                    let used = if raw < cap { raw } else { 0 };
                    let take = core::cmp::min(used, 2048);
                    let start = used - take;
                    for i in 0..take {
                        log_chunk[i] = unsafe {
                            core::ptr::read_volatile((base as usize + start + i) as *const u8)
                        };
                    }
                    &log_chunk[..take]
                }
                b"shell:ver" => b"fullerene-ane\r\n",
                _ => b"fullerene-ane adb up\r\n",
            };
            let payload = unsafe { &mut *core::ptr::addr_of_mut!(ADB_PAYLOAD) };
            if let Some(m) = crate::adb_wire::shell_v2_chunk(
                crate::adb_wire::SHELL_STDOUT,
                text,
                &mut payload[..],
            ) {
                if let Some(n) = crate::adb_wire::encode(
                    crate::adb_wire::WRTE,
                    remote,
                    pkt.arg0,
                    &payload[..m],
                    &mut out[..],
                ) {
                    run[at..at + n].copy_from_slice(&out[..n]);
                    at += n;
                }
            }
            if let Some(n) =
                crate::adb_wire::encode(crate::adb_wire::CLSE, remote, pkt.arg0, &[], &mut out[..])
            {
                run[at..at + n].copy_from_slice(&out[..n]);
                at += n;
            }
            if at > 0 {
                io.bulk_send(&run[..at]);
            }
            io.log("adb: open", svc.len() as u64);
        }
        crate::adb_wire::AUTH => {
            // No authentication is offered; older adb accepts silence here.
            io.log("adb: auth ignored", pkt.payload.len() as u64);
        }
        crate::adb_wire::WRTE | crate::adb_wire::CLSE | crate::adb_wire::OKAY => {
            io.log("adb: post-open frame", pkt.command as u64);
        }
        _ => {}
    }
}
