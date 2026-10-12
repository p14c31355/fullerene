//! The ANE's DWC2 device responder: EP0 control transfers and the
//! descriptors that present a fastboot-compatible interface.
//!
//! Why this module exists: the handoff bring-up (`platform::ane::usb_bringup`)
//! gets the core into device mode and attached - the host already enumerates
//! a high-speed device - but nothing answers the control endpoint, so every
//! descriptor read ends in -110 (device descriptor read/64, error -110,
//! measured across every v9..v11 run). The LK's own fastboot proves the same
//! core can serve a full enumeration; this module ports that half in Rust so
//! the boot that brings the link up also speaks to the host without a
//! bootloader round-trip.
//!
//! Primary sources (vendor tree, kept local):
//!   - drivers/usb/dwc2/hw.h                registers and bit fields
//!   - drivers/usb/dwc2/gadget.c            EP0 flow: setup arming, the
//!                                          zero-length status stages, the
//!                                          TxFIFO push order
//!   - drivers/usb/susb/dwc_otg_pcd_intr.c  the SETUP/status packet POP path
//!     (DWC_DSTS_SETUP_UPDT = read the 8 setup bytes, DWC_DSTS_SETUP_COMP =
//!      the setup stage finished, then the data/status state machine)
//!
//! Staging: this version serves the standard requests (the enumeration);
//! the fastboot bulk protocol is the next stage.

use core::ptr;

use super::ane::{USB_CORE_BASE, WATCHDOG_BASE};

// ---- registers (hw.h) -------------------------------------------------------

const GAHBCFG: usize = 0x008;
const GSNPSID: usize = 0x040; // core release, for the supcnt programming rule
const GINTSTS: usize = 0x014;
const GINTMSK: usize = 0x018;
const GRXSTSP: usize = 0x020;
const DCFG: usize = 0x800;
const DCTL: usize = 0x804;
const DSTS: usize = 0x808;
const DIEPMSK: usize = 0x810;
const DOEPMSK: usize = 0x814;
const DAINT: usize = 0x818;
const DAINTMSK: usize = 0x81c;
const DIEPCTL0: usize = 0x900;
const DIEPINT0: usize = 0x908;
const DIEPTSIZ0: usize = 0x910;
const DOEPCTL0: usize = 0xb00;
const DOEPINT0: usize = 0xb08;
const DOEPTSIZ0: usize = 0xb10;
/// Endpoint FIFO access window: EPFIFO(n) = 0x1000 + n*0x1000 (hw.h).
const EPFIFO0: usize = 0x1000;
/// Endpoint 1 - the fastboot bulk pair. Registers follow the DWC2 stride
/// (hw.h: DIEPCTL(n) = 0x900 + n*0x20, DOEPCTL(n) = 0xB00 + n*0x20).
const DIEPCTL1: usize = 0x920;
const DIEPINT1: usize = 0x928;
const DIEPTSIZ1: usize = 0x930;
const DOEPCTL1: usize = 0xB20;
const DOEPINT1: usize = 0xB28;
const DOEPTSIZ1: usize = 0xB30;
/// Per-endpoint TX FIFO size map (hw.h DPTXFSIZN(1) = 0x104); read at boot
/// to learn which TX FIFO slot this core assigned to EP1.
const DPTXFSIZ1: usize = 0x104;
/// EP1-IN's TX FIFO fill level in words (hw.h DTXFSTS(n) = 0x918 + n*0x20);
/// a nonzero reading at boot is the loader's stale reply word.
const DTXFSTS1: usize = 0x938;
/// The PMIC reset-reason register (reg 0x18B, byte access at reg << 2). The
/// loader reads this at its next start; BOOTLOADER=0x01 / RECOVERY=0x02 /
/// COLDBOOT=0x10 (mntn_public_interface.h, same table the stock kernel uses
/// in hisi_pm_system_reset -> set_reboot_reason).
const PMIC_REASON_ADDR: usize = 0xfff3_4000 + (0x18B << 2);
/// DPTXFSIZN(i) for the dedicated TX FIFOs (i >= 1; hw.h).
fn dptxfsiz_n(i: u32) -> usize {
    0x104 + (i as usize - 1) * 4
}
/// Shared non-periodic TX FIFO (start [15:0], depth in words [31:16]).
const GNPTXFSIZ: usize = 0x028;
/// RX FIFO depth in words [15:0].
const GRXFSIZ: usize = 0x024;
/// EP1's FIFO window: EPFIFO(1) = 0x1000 + 1*0x1000. The data window is
/// per end-point number regardless of the TX FIFO slot (gadget.c:468).
const EPFIFO1: usize = 0x2000;
/// Bulk max packet size on this high-speed link.
const BULK_MPS: u32 = 512;

// ---- GRSTCTL (FIFO flushes) -------------------------------------------------

const GRSTCTL: usize = 0x010;
const GRSTCTL_RXFFLSH: u32 = 1 << 4;
const GRSTCTL_TXFFLSH: u32 = 1 << 5;
/// TXFNUM = 0x10 means "all TX FIFOs" (gadget.c flushes use it).
const GRSTCTL_TXFNUM_ALL: u32 = 0x10 << 6;
/// GRSTCTL bit 31: AHB idle (1 = idle). Flushes must wait for this.
const GRSTCTL_AHBIDLE: u32 = 1 << 31;

// ---- GINTSTS / GINTMSK bits ------------------------------------------------

const GINT_SOF: u32 = 1 << 3;
const GINT_RXFLVL: u32 = 1 << 4;
const GINT_USBRST: u32 = 1 << 12;
const GINT_ENUMDONE: u32 = 1 << 13;
const GINT_IEPINT: u32 = 1 << 18;
const GINT_OEPINT: u32 = 1 << 19;

// ---- DXEPCTL bits ----------------------------------------------------------

const EPCTL_EPENA: u32 = 1 << 31;
const EPCTL_EPDIS: u32 = 1 << 30;
const EPCTL_CNAK: u32 = 1 << 26;
const EPCTL_SNAK: u32 = 1 << 27;
/// Transfer-type field [19:18]: reads back as CONTROL unless set, and the
/// core stalls every transaction on a CONTROL-typed bulk endpoint.
const EPCTL_EPTYPE_MASK: u32 = 0x3 << 18;
const EPCTL_EPTYPE_BULK: u32 = 0x2 << 18;
/// MPS field [10:0] (hw.h: DXEPCTL_MPS_MASK).
const EPCTL_MPS_MASK: u32 = 0x7ff;
const EPCTL_STALL: u32 = 1 << 21;
/// Select the DATA0 PID on (re)enable (gadget.c: `DXEPCTL_SETD0PID`).
const EPCTL_SETD0PID: u32 = 1 << 28;
/// TX FIFO number field [25:22] of DIEPCTLn.
const EPCTL_TXFNUM_MASK: u32 = 0xf << 22;
const EPCTL_USBACTEP: u32 = 1 << 15;

// ---- DXEPINT bits ----------------------------------------------------------

const EPINT_XFERCOMPL: u32 = 1 << 0;
const EPINT_SETUP: u32 = 1 << 3;

// ---- GRXSTSP fields (hw.h) -------------------------------------------------

const RXSTS_EPNUM_MASK: u32 = 0xf;
const RXSTS_EPNUM_SHIFT: u32 = 0;
const RXSTS_BCNT_SHIFT: u32 = 4;
const RXSTS_BCNT_MASK: u32 = 0x7ff << RXSTS_BCNT_SHIFT;
const RXSTS_PKTSTS_SHIFT: u32 = 17;
const RXSTS_PKTSTS_MASK: u32 = 0xf << RXSTS_PKTSTS_SHIFT;
const PKTSTS_OUTRX: u32 = 2;
const PKTSTS_OUTDONE: u32 = 3;
const PKTSTS_SETUPDONE: u32 = 4;
const PKTSTS_SETUPRX: u32 = 6;

// ---- DCFG fields -----------------------------------------------------------

const DCFG_DEVADDR_MASK: u32 = 0x7f << 4;

// ---- the descriptor set -----------------------------------------------------
//
// The interface presents class 0xFF / subclass 0x42 / protocol 0x03 - the
// fastboot protocol class - with the pair of bulk endpoints the protocol
// uses. The VID:PID mirrors the LK's fastboot ids (18d1:d00d, measured from
// the host journal) so the host tooling finds it without new rules.
//
// ANE_ADB_PROTOCOL flips the protocol byte to 0x01 (the ADB class) so a host
// `adb` claims the interface and sends its CNXN over EP1, where adb_dispatch
// answers. Measured open question: whether the fastboot client still matches
// the interface with 0x01 - our own tools (pyusb/xfer/adbtest) do not care.

const DEV_DESC: [u8; 18] = [
    18, 0x01, // bLength, DEVICE
    0x00, 0x02, // bcdUSB 2.00
    0x00, 0x00, 0x00, // class, subclass, protocol
    64,   // bMaxPacketSize0
    0xd1, 0x18, // idVendor 0x18d1
    0x0d, 0xd0, // idProduct 0xd00d
    0x00, 0x01, // bcdDevice 1.00
    1, 2, 3, // iManufacturer, iProduct, iSerialNumber
    1, // bNumConfigurations
];

#[cfg(fullerene_ane_adb_protocol)]
const IFACE_PROTOCOL: u8 = 0x01;
#[cfg(not(fullerene_ane_adb_protocol))]
const IFACE_PROTOCOL: u8 = 0x03;

const CFG_DESC: [u8; 32] = [
    // configuration
    9,
    0x02,
    32,
    0x00,
    1,
    1,
    0,
    0x80,
    50, // bus powered, 100 mA
    // interface: fastboot (class 0xff, subclass 0x42, protocol 0x03)
    9,
    0x04,
    0,
    0,
    2,
    0xff,
    0x42,
    IFACE_PROTOCOL,
    0, // endpoint IN, bulk, 512
    7,
    0x05,
    0x81,
    0x02,
    0x00,
    0x02,
    0, // endpoint OUT, bulk, 512
    7,
    0x05,
    0x01,
    0x02,
    0x00,
    0x02,
    0,
];

const STR_LANG: [u8; 4] = [4, 0x03, 0x09, 0x04];
/// "Fullerene" in UTF-16LE, length-prefixed.
const STR_MFR: [u8; 20] = [
    20, 0x03, b'F', 0, b'u', 0, b'l', 0, b'l', 0, b'e', 0, b'r', 0, b'e', 0, b'n', 0, b'e', 0,
];
/// "ANE fastboot" in UTF-16LE.
const STR_PROD: [u8; 26] = [
    26, 0x03, b'A', 0, b'N', 0, b'E', 0, b' ', 0, b'f', 0, b'a', 0, b's', 0, b't', 0, b'b', 0,
    b'o', 0, b'o', 0, b't', 0,
];
/// "SCV7N18927000473" (this handset's serial) in UTF-16LE.
const STR_SER: [u8; 34] = [
    34, 0x03, b'S', 0, b'C', 0, b'V', 0, b'7', 0, b'N', 0, b'1', 0, b'8', 0, b'9', 0, b'2', 0,
    b'7', 0, b'0', 0, b'0', 0, b'0', 0, b'4', 0, b'7', 0, b'3', 0,
];

// ---- state ------------------------------------------------------------------

/// This boot's SETUP packet (read from the FIFO in the RX pop path).
static mut SETUP_BUF: [u8; 8] = [0; 8];
/// True once the host has issued SET_CONFIGURATION with a non-zero value.
static mut CONFIGURED: bool = false;
/// v205: sticky "the host once configured us with the REAL config (1)".
/// The self-heal must never fire on a boot that ever reached config 1 -
/// the host's driver-rebind dance (SET_CONFIG(0) then (1) again) opens a
/// `config_value == 0` window minutes into a perfectly healthy boot, and
/// the v203 gate mis-fired there (fresh re-enumeration at +2 min broke a
/// collection with ENODEV).
static mut CONFIG_WAS_1: bool = false;
/// v202: the last SET_CONFIGURATION value. 1 = the real configuration;
/// anything else (2+, from a garbled bConfigurationValue) or 0 (never
/// configured) marks an enumeration the self-heal retry should redo.
static mut CONFIG_VALUE: u8 = 0;
/// True while EP0 is armed to receive a SETUP; the service pass re-arms it
/// whenever the core consumed the previous one (the DWC2 disables the OUT
/// endpoint after each setup stage, measured on the vendor stack).
static mut SETUP_ARMED: bool = false;
/// The setup-handling handshake. Answer as soon as this setup's 8 bytes
/// arrived in the RX stream (PKTSTS_SETUPRX): that entry is the reliable
/// trigger. Handling on the RX PKTSTS_SETUPDONE status word instead was
/// measured (2026-10-06) to lag a whole request when the core retires its
/// setup slot late - every control answer came back as the previous
/// request's, which is exactly the broken enumeration of v49/v52.
static mut SETUP_BYTES_READY: bool = false;
/// v198: true only between an RX-pop storing THIS setup's bytes and its
/// answering (or a bus-reset wipe). The old inline/belt answering could
/// fire on BYTES_READY while SETUP_BUF still held the PREVIOUS setup's
/// bytes (this core can surface the DOEPINT0.SETUP latch before the RX
/// entry is readable), which served the previous request's answer to the
/// new request - the observed one-request-behind "Љ" corruption. The
/// answering site is now single and gated on this flag.
static mut SETUP_BYTES_FRESH: bool = false;
static mut SETUP_HANDLED: bool = false;
/// True from the moment a SETUP was answered until its control transfer
/// finishes (the data phase plus the status phase). The auto re-arm of the
/// SETUP receive must not fire mid-transaction.
static mut TRANSACTION_BUSY: bool = false;
/// True when the last queued IN phase carried data (so a status OUT still
/// follows) - false when it was itself the zero-length status (no follow-up).
static mut IN_WAS_DATA: bool = false;
/// The fastboot command line being received on the EP1 bulk OUT endpoint.
const FB_CMD_MAX: usize = 128;
static mut FB_CMD: [u8; FB_CMD_MAX] = [0; FB_CMD_MAX];
static mut FB_CMD_LEN: usize = 0;

/// Fastboot `download:` staging. The payload lands here whole (the kernel
/// image is ~11.4 MB, so the area is 12 MiB of .bss) before any write path
/// consumes it. DL_STATE: 0 idle, 1 receiving, 2 complete-and-unreplied.
const DOWNLOAD_MAX: usize = 12 << 20;
static mut DOWNLOAD_BUF: [u8; DOWNLOAD_MAX] = [0; DOWNLOAD_MAX];
static mut DL_STATE: u8 = 0;
static mut DL_REMAIN: usize = 0;
static mut DL_LEN: usize = 0;
/// Last progress timestamp (uptime_us) for the download timeout in service().
static mut DL_T0: u64 = 0;
/// One-time per boot: the first EP1 reply sweeps the core's stale TX FIFO.
static mut SWEPT_FIRST_REPLY: bool = false;

/// v165: byte count of the most recent EP1 IN reply, surfaced to the tick
/// instrument (S4) together with the live FIFO free-word count. A multi-
/// packet reply that wedges in the core shows as a free count stuck below
/// depth; a healthy transfer drains back to depth as the host reads.
static LAST_SEND_N: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// (last reply byte count << 32) | (FIFO free words << 16): read every tick
/// by the S4 instrument.
pub fn last_send_state() -> u64 {
    let n = LAST_SEND_N.load(core::sync::atomic::Ordering::Relaxed) as u64;
    let free = (unsafe { rd(DTXFSTS1) } & 0xffff) as u64;
    (n << 32) | (free << 16)
}
/// The paced `oem log` stream: one INFO line per service pass.
static mut LOG_ACTIVE: bool = false;
static mut LOG_CURSOR: usize = 0;

// ---- v220: the `oem xfer` bulk stream (vendor-pattern pump) ----------------
// The vendor gadget.c (drivers/usb/dwc2/gadget.c, s3c_hsotg_write_fifo)
// writes AT MOST 512 bytes per call into the non-periodic TX FIFO and says
// why: "requests of >512 cause the endpoint to get stuck with a fragment of
// the end of the transfer in it" (gadget.c:401-407). Transfers are armed
// once (PKTCNT = ceil(size/MPS)) and the remaining bytes are fed from the
// IN-complete path as FIFO space frees - so this stream keeps cursors and
// is advanced from service() every pass: arm a bounded window, feed <=512 B
// chunks while DTXFSTS advertises room, re-arm only after the previous
// window is fully fed and EPENA has cleared (a DIEPTSIZ write while armed
// is silently dropped - the EP0 lesson).

/// Stream on/off.
static mut XFER_ON: bool = false;
/// Feed cursor: the next byte to hand the FIFO.
static mut XFER_FEED: usize = 0;
/// End of the whole command's range.
static mut XFER_END: usize = 0;
/// End of the currently armed window (a new arm happens when FEED reaches it).
static mut XFER_WIN_END: usize = 0;

/// Advance the bulk xfer stream (statics above). Safe to call every pass.
pub fn xfer_pump() {
    if !unsafe { core::ptr::addr_of!(XFER_ON).read() } {
        return;
    }
    // 1. Arm the next window when the current one is fully fed. Skip while
    //    EPENA is set (the in-flight window has not completed); the caller's
    //    next pass retries - never write DIEPTSIZ while armed.
    let feed = unsafe { core::ptr::addr_of!(XFER_FEED).read() };
    let end = unsafe { core::ptr::addr_of!(XFER_END).read() };
    let win_end = unsafe { core::ptr::addr_of!(XFER_WIN_END).read() };
    if feed >= win_end && feed < end && (rd(DIEPCTL1) & EPCTL_EPENA) == 0 {
        let win = core::cmp::min(end - feed, 0x1_0000);
        let pktcnt = ((win + BULK_MPS as usize - 1) / BULK_MPS as usize) as u32;
        wr(
            DIEPTSIZ1,
            (1 << 29) | (pktcnt << 19) | ((win & 0x7ffff) as u32),
        );
        wr(
            DIEPCTL1,
            (rd(DIEPCTL1) & !EPCTL_SNAK) | EPCTL_EPENA | EPCTL_CNAK | EPCTL_USBACTEP,
        );
        unsafe {
            core::ptr::addr_of_mut!(XFER_WIN_END).write(feed + win);
        }
    }
    // 2. Feed <=512 B chunks while the FIFO advertises room (bounded per
    //    pass so the pump cannot monopolise the IRQ). v221: DTXFSTS is not
    //    fully trustworthy on this bus - a stale LOW reading would starve
    //    the wire and hang the host's read (measured: header out, payload
    //    stalled). Hedge with a second read after a short spin and take
    //    the larger; a persistent zero marks "XF: STALL" for forensics.
    let mut k = 0u32;
    while k < 16 {
        let feed = unsafe { core::ptr::addr_of!(XFER_FEED).read() };
        let win_end = unsafe { core::ptr::addr_of!(XFER_WIN_END).read() };
        if feed >= win_end {
            break;
        }
        let mut free = (rd(DTXFSTS1) & 0xffff) as usize * 4;
        if free < 512 {
            for _ in 0..64 {
                core::hint::spin_loop();
            }
            let free2 = (rd(DTXFSTS1) & 0xffff) as usize * 4;
            if free2 > free {
                free = free2;
            }
        }
        if free < 512 {
            crate::blackbox::text_puts("XF: STALL\n");
            break;
        }
        let chunk = core::cmp::min(win_end - feed, 512);
        let mut i = 0;
        while i < chunk {
            let mut word = 0u32;
            for j in 0..4 {
                if i + j < chunk {
                    word |= (unsafe { ptr::read_volatile((feed + i + j) as *const u8) } as u32)
                        << (8 * j);
                }
            }
            unsafe {
                ptr::write_volatile((USB_CORE_BASE + EPFIFO1) as *mut u32, word);
            }
            i += 4;
        }
        unsafe {
            core::ptr::addr_of_mut!(XFER_FEED).write(feed + chunk);
        }
        k += 1;
    }
    // 3. All bytes fed: the wire drains the remainder by itself.
    let feed = unsafe { core::ptr::addr_of!(XFER_FEED).read() };
    let end = unsafe { core::ptr::addr_of!(XFER_END).read() };
    if feed >= end {
        unsafe {
            core::ptr::addr_of_mut!(XFER_ON).write(false);
        }
        crate::blackbox::text_puts("XF: DONE\n");
    }
}
static mut LOG_LINES: u32 = 0;
/// The frozen end of the stream window (this boot's log length when the
/// command arrived). Re-reading it live would make the stream chase the
/// boot's own ongoing log writes and never end.
static mut LOG_END: usize = 0;
/// True when the stream reads the main durable text block (TEXT_BLOCK_BASE
/// + 12, cap 0xE000) instead of the 7 KiB fixed copy. The fixed copy is
/// filled by ~the boot's first 200 lines on this platform (the loader's
/// early phases are chatty), so `oem log full` alone cannot reach the
/// USB/SD/eMMC sections - `oem log main [hexoff]` streams the whole block.
static mut LOG_MAIN: bool = false;
/// True when no EP1-IN transfer is in flight. The log stream sends one line
/// per completed transfer; arming the next while the previous was still in
/// flight was silently multi-second slow.
static mut EP1_IN_IDLE: bool = true;
/// Real-time pacing state for the stream (32.768 kHz ticks, >= 1 ms/line).
static mut LOG_WD_LAST: u32 = 0;
static mut LOG_WD_ACC: u32 = 0;

// ---- bring-up diagnostics (rendered live by ane_fb::draw_fb_diag) ----------

static mut DBG_RX_ENTRIES: u32 = 0;
static mut DBG_EP1_BYTES: u32 = 0;
static mut DBG_FB_DISPATCH: u32 = 0;
static mut DBG_FB_REPLY: u32 = 0;
static mut DBG_LAST_RXSTS: u32 = 0;
static mut DBG_CMD_HEAD: u64 = 0;
/// The EP registers as the loader left them (captured before init writes).
static mut LK_SNAP: [u32; 6] = [0; 6];
/// The dedicated-FIFO scan result at init (informational; 0 = none found).
static mut EP1_TX_IDX: u32 = 0;
/// v206: how many times ep0_send found EPENA still set (bounded wait
/// exhausted). A nonzero value in the UD8-era diag means the stale-TSIZ
/// class was live on this boot.
static mut DBG_EPENA_STUCK: u32 = 0;

/// v302: how many times the EP0 TX FIFO flush ran (DBG_FFLUSH_STUCK = the
/// flush self-clear never completed). The RUN counter rising while STRINGS
/// come back clean on the host is the proof the flush fixes the one-behind.
static mut DBG_FFLUSH_RUN: u32 = 0;
static mut DBG_FFLUSH_STUCK: u32 = 0;

/// A live snapshot of the fastboot responder, for the on-screen strip.
pub struct FbDiag {
    pub dptxfsiz1: u32,
    pub dtxfsts1: u32,
    pub gnptxfsiz: u32,
    pub grxfsiz: u32,
    pub tx_idx: u32,
    pub lk_diepctl1: u32,
    pub lk_doepctl1: u32,
    pub diepctl1: u32,
    pub doepctl1: u32,
    pub fb_len: u32,
    pub fb_disp: u32,
    pub fb_reply: u32,
    pub rx_entries: u32,
    pub ep1_bytes: u32,
    pub last_rxsts: u32,
    pub cmd_head: u64,
    pub gintsts: u32,
    pub epint: u32,
    /// v302: EP0 TX FIFO flush counter / stuck counter (the one-behind fix).
    pub fflush: u32,
    /// v302: served strings + (stale-size class evidence) epena-stuck count.
    pub strs: u32,
}

/// Take the snapshot (plain register reads; safe every loop pass).
pub fn diag() -> FbDiag {
    unsafe {
        let snap: &[u32; 6] = &*core::ptr::addr_of!(LK_SNAP);
        FbDiag {
            dptxfsiz1: rd(DPTXFSIZ1),
            dtxfsts1: rd(DTXFSTS1),
            gnptxfsiz: rd(GNPTXFSIZ),
            grxfsiz: rd(GRXFSIZ),
            tx_idx: core::ptr::addr_of!(EP1_TX_IDX).read(),
            lk_diepctl1: snap[0],
            lk_doepctl1: snap[1],
            diepctl1: rd(DIEPCTL1),
            doepctl1: rd(DOEPCTL1),
            fb_len: core::ptr::addr_of!(FB_CMD_LEN).read() as u32,
            fb_disp: core::ptr::addr_of!(DBG_FB_DISPATCH).read(),
            fb_reply: core::ptr::addr_of!(DBG_FB_REPLY).read(),
            rx_entries: core::ptr::addr_of!(DBG_RX_ENTRIES).read(),
            ep1_bytes: core::ptr::addr_of!(DBG_EP1_BYTES).read(),
            last_rxsts: core::ptr::addr_of!(DBG_LAST_RXSTS).read(),
            cmd_head: core::ptr::addr_of!(DBG_CMD_HEAD).read(),
            gintsts: rd(GINTSTS),
            epint: (rd(DIEPINT1) << 16) | (rd(DOEPINT1) & 0xffff),
            fflush: (core::ptr::addr_of!(DBG_FFLUSH_RUN).read() & 0xffff)
                | (core::ptr::addr_of!(DBG_FFLUSH_STUCK).read() << 16),
            strs: strings_served() | (core::ptr::addr_of!(DBG_EPENA_STUCK).read() << 16),
        }
    }
}

fn rd(off: usize) -> u32 {
    super::common::mmio::rd32(USB_CORE_BASE, off)
}

fn wr(off: usize, value: u32) {
    super::common::mmio::wr32(USB_CORE_BASE, off, value);
}

/// Read-only DCTL evidence for the attach/desktop race investigation.
pub fn dctl_snapshot() -> u32 {
    rd(DCTL)
}

/// Read 32-bit words out of an endpoint FIFO window.
fn fifo_pop_word() -> u32 {
    rd(EPFIFO0)
}

/// Push one 32-bit word into the endpoint 0 FIFO.
fn fifo_push_word(word: u32) {
    wr(EPFIFO0, word)
}

/// Arm EP0 to receive the next SETUP packet.
///
/// Vendor values (dwc_otg_pcd_intr.c `ep0_out_start`): 3 back-to-back setup
/// packets, one packet, transfer size 8*3 = 24. CNAK is deliberately *not*
/// set here - the vendor clears the NAK after the setup interrupt instead.
fn arm_setup_receive() {
    wr(DOEPTSIZ0, (3 << 29) | (1 << 19) | 24);
    wr(DOEPCTL0, rd(DOEPCTL0) | EPCTL_EPENA | EPCTL_USBACTEP);
    unsafe {
        SETUP_ARMED = true;
        SETUP_BYTES_READY = false;
        SETUP_BYTES_FRESH = false;
        SETUP_HANDLED = false;
    }
}

/// Send the zero-length IN (the status stage for an OUT data phase).
fn ep0_zlp_in() {
    ep0_send(&[]);
}

/// Receive the zero-length OUT (the status stage for an IN data phase).
///
/// Vendor values (dwc_otg_cil.c `dwc_otg_ep0_start_transfer`, OUT branch):
/// xfersize = maxpacket (64), pktcnt = 1, supcnt = 3 on cores at or above
/// 3.00a. The xfersize=0 / supcnt=1 shape this used to write made the core
/// mis-consume the following packets, which derailed the enumeration right
/// at the first config descriptor read (host journal: -71).
fn ep0_status_out() {
    wr(DOEPTSIZ0, (3 << 29) | (1 << 19) | 64);
    wr(
        DOEPCTL0,
        rd(DOEPCTL0) | EPCTL_EPENA | EPCTL_CNAK | EPCTL_USBACTEP,
    );
}

/// v302: flush the EP0 TX FIFO. The vendor driver flushes an endpoint's TX
/// FIFO when its transfer is torn down (gadget.c:1990
/// `s3c_hsotg_txfifo_flush(hsotg, hs_ep->fifo_index)` inside the EPDISBLD
/// branch) - and our EP0 never does. A response that was armed but never
/// fully sent (the host's early status stage, a bus reset mid-answer, an
/// aborted control transfer) leaves stale packets in the FIFO, and the next
/// EPENA then transmits the OLD response first: the one-behind rotation
/// measured on hardware (str1 -> serial, str2 -> manufacturer, dev-desc ->
/// langid; GET_STATUS/LANG always EOVERFLOW because a stale 18/20-byte
/// payload follows the short packet).
///
/// Wait for AHB idle first (the flush is ignored while the bus is busy),
/// then flush EP0's FIFO ONLY (TXFNUM=0). Flushing all numbers here would
/// also drain EP1's bulk FIFO mid-command - the shared-window reads of
/// EPFIFO1 would then race the command stream.
fn ep0_flush_tx_fifo() {
    let mut spins = 0u32;
    while (rd(GRSTCTL) & GRSTCTL_AHBIDLE) == 0 && spins < 0x4000 {
        spins += 1;
    }
    // TXFNUM(0) = EP0's non-periodic TX FIFO (hw.h GRSTCTL_TXFNUM_SHIFT=6).
    wr(GRSTCTL, (0 << 6) | GRSTCTL_TXFFLSH);
    let mut s2 = 0u32;
    while (rd(GRSTCTL) & GRSTCTL_TXFFLSH) != 0 && s2 < 0x4000 {
        s2 += 1;
    }
    unsafe {
        if s2 >= 0x4000 {
            DBG_FFLUSH_STUCK = DBG_FFLUSH_STUCK.wrapping_add(1);
        }
        DBG_FFLUSH_RUN = DBG_FFLUSH_RUN.wrapping_add(1);
    }
}

/// Queue an IN data phase on EP0, then push the bytes into the FIFO.
///
/// Order matters (gadget.c `s3c_hsotg_write_fifo`): the transfer size and
/// EPENA are programmed first, only then does the payload enter the FIFO.
fn ep0_send(data: &[u8]) {
    let n = data.len();
    unsafe {
        // v304 A/B VERDICT: skipping the flush while TRANSACTION_BUSY killed
        // the boot outright (attach with ERR110-only responses, never
        // answered; recovered via the LK window). The unconditional flush is
        // the proven shape - keep it, and record the failed variant here so
        // it is not retried.
        TRANSACTION_BUSY = true;
        IN_WAS_DATA = n > 0;
    }
    // v302: drain any stale response from the EP0 TX FIFO BEFORE arming the
    // new one (see ep0_flush_tx_fifo - the one-behind root cause).
    ep0_flush_tx_fifo();
    // v206: never write TSIZ while a transfer is still armed. While EPENA
    // reads 1 the core ignores TSIZ/EPENA writes - the new xfersize is
    // silently dropped and the OLD size drives the packet (stale-size
    // responses, a leading edge of the "one request behind" class).
    // v210: the wait is SHORT (0x400): a healthy completion clears EPENA
    // in microseconds, and the old 0x2_0000 bound cost SECONDS of slow-bus
    // MMIO reads - long enough to be the -110 timeout itself when the
    // endpoint was genuinely stuck. A stuck EPENA is force-disabled
    // immediately instead.
    let mut spins = 0u32;
    while (rd(DIEPCTL0) & EPCTL_EPENA) != 0 && spins < 0x400 {
        spins += 1;
    }
    if spins >= 0x400 {
        let c = rd(DIEPCTL0) | EPCTL_EPENA | EPCTL_EPDIS;
        wr(DIEPCTL0, c);
        let mut s2 = 0u32;
        while (rd(DIEPCTL0) & EPCTL_EPENA) != 0 && s2 < 0x400 {
            s2 += 1;
        }
        crate::blackbox::text_puts("WD: EP0 EPENA\n");
        unsafe {
            DBG_EPENA_STUCK = DBG_EPENA_STUCK.wrapping_add(1);
        }
    }
    wr(DIEPTSIZ0, (1 << 19) | ((n & 0x7f) as u32));
    wr(
        DIEPCTL0,
        rd(DIEPCTL0) | EPCTL_EPENA | EPCTL_CNAK | EPCTL_USBACTEP,
    );
    let mut i = 0;
    while i < n {
        let mut word = 0u32;
        for j in 0..4 {
            if i + j < n {
                word |= (data[i + j] as u32) << (8 * j);
            }
        }
        fifo_push_word(word);
        i += 4;
    }
}

/// Stall EP0 (both directions) until the next SETUP clears it.
fn stall_ep0() {
    let c = rd(DIEPCTL0) | EPCTL_EPENA | EPCTL_EPDIS | EPCTL_STALL;
    wr(DIEPCTL0, c);
    let c = rd(DOEPCTL0) | EPCTL_EPENA | EPCTL_EPDIS | EPCTL_STALL;
    wr(DOEPCTL0, c);
}

/// Handle one standard request from the setup packet (bRequest in `SETUP_BUF`).
fn handle_setup(log: fn(&str, u64)) {
    let bmrt = unsafe { SETUP_BUF[0] };
    let breq = unsafe { SETUP_BUF[1] };
    let wvalue = unsafe { u16::from_le_bytes([SETUP_BUF[2], SETUP_BUF[3]]) };
    let wlength = unsafe { u16::from_le_bytes([SETUP_BUF[6], SETUP_BUF[7]]) };
    let kind = bmrt & 0x60;

    // v198 forensic: one bounded ring line per answered request.
    trace_request(breq, wvalue, wlength);
    log("ud: req=", ((kind as u64) << 8) | breq as u64);
    log("ud: wv=", wvalue as u64);

    if kind != 0 {
        // Class/vendor requests are next stage; refuse cleanly for now.
        stall_ep0();
        return;
    }

    match breq {
        0x06 => {
            // GET_DESCRIPTOR
            let dtype = (wvalue >> 8) as u8;
            let index = (wvalue & 0xff) as u8;
            let desc: &[u8] = match dtype {
                1 => &DEV_DESC,
                2 => &CFG_DESC,
                3 => match index {
                    0 => &STR_LANG,
                    1 => &STR_MFR,
                    2 => &STR_PROD,
                    3 => &STR_SER,
                    _ => {
                        stall_ep0();
                        return;
                    }
                },
                _ => {
                    stall_ep0();
                    return;
                }
            };
            let n = core::cmp::min(wlength as usize, desc.len());
            ep0_send(&desc[..n]);
            if dtype == 3 && n > 0 {
                // v300: self-heal counter. The host reads at least the langid
                // and the product string on every clean enumeration. Count
                // served string transfers so the watchdog can tell a garbled
                // boot (config-1 reached but strings never served) from a
                // healthy one, and cycle D+ on a garbled boot.
                STRINGS_SERVED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
        0x05 => {
            // SET_ADDRESS
            let addr = (wvalue as u32) & 0x7f;
            ADDRESS_SEEN.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            wr(DCFG, (rd(DCFG) & !DCFG_DEVADDR_MASK) | (addr << 4));
            ep0_zlp_in();
        }
        0x09 => {
            // SET_CONFIGURATION
            unsafe {
                CONFIGURED = wvalue != 0;
                CONFIG_VALUE = (wvalue & 0xff) as u8;
                // 8e9f11cf semantics: latch CONFIG_WAS_1 at SET_CONFIGURATION
                // (the regression-boundary diff named this the one remaining
                // functional EP0 delta after the duplicate-answer guards).
                if wvalue == 1 {
                    CONFIG_WAS_1 = true;
                }
                // Do not mark the boot healthy merely because the SETUP
                // request reached us.  A host-aborted control transfer can
                // leave SET_CONFIGURATION(1) processed locally while its
                // status stage never completes.  The timer-side recovery
                // must remain armed until the host verifies the state with
                // GET_CONFIGURATION below.
            }
            if wvalue != 0 {
                // Activate the bulk pair only now, like a gadget driver's
                // set_config: before this the host never touches EP1, and a
                // bus reset may have left it disabled.
                init_ep1(log);
            }
            ep0_zlp_in();
        }
        0x00 => {
            // GET_STATUS (device)
            ep0_send(&[0, 0]);
        }
        0x08 => {
            // GET_CONFIGURATION
            let v = if unsafe { CONFIGURED } { 1u8 } else { 0u8 };
            if v == 1 {
                unsafe {
                    CONFIG_WAS_1 = true;
                }
            }
            ep0_send(&[v]);
        }
        0x0a => {
            // GET_INTERFACE
            ep0_send(&[0]);
        }
        0x0b => {
            // SET_INTERFACE
            ep0_zlp_in();
        }
        _ => stall_ep0(),
    }
}

/// Reset handling: back to address 0, void the EP0 state machine, re-arm.
fn handle_reset(log: fn(&str, u64)) {
    log("ud: usbreset=", 1);
    // A bus reset voids any fastboot session: drop a half-received
    // download so commands flow again. (A stray `download:` whose client
    // bailed used to eat every later command until the next reboot.)
    unsafe {
        core::ptr::addr_of_mut!(DL_STATE).write(0);
        core::ptr::addr_of_mut!(DL_REMAIN).write(0);
        core::ptr::addr_of_mut!(DL_LEN).write(0);
        core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
    }
    // v159: a bus reset also voids the control-transfer state machine. If
    // the boot thread froze mid-enumeration, TRANSACTION_BUSY could stay
    // latched forever, which blocks the SETUP re-arm gate in service() and
    // leaves the device unable to answer descriptor reads (the host then
    // retries port resets forever: -110 / -62 storms). The vendor's reset
    // path resets its pcd state the same way; clear every latch here.
    unsafe {
        core::ptr::addr_of_mut!(TRANSACTION_BUSY).write(false);
        core::ptr::addr_of_mut!(SETUP_HANDLED).write(false);
        core::ptr::addr_of_mut!(SETUP_BYTES_READY).write(false);
        core::ptr::addr_of_mut!(SETUP_BYTES_FRESH).write(false);
        core::ptr::addr_of_mut!(SETUP_ARMED).write(false);
    }
    // Ring marker (the pump's log callback is a no-op, so speak directly):
    // photo-visible proof that port resets are reaching this handler.
    crate::blackbox::text_puts("UD: RST\n");
    super::ane_fb::event(b"UD RST");
    // v206: flush the FIFOs at every bus reset. Residue words from an
    // aborted/retried control transfer survived the D+-only reconnects
    // (the self-heal's redraws kept drawing the same bad hand), and a
    // stale leading word is exactly the [09 02 20 00 | 09 02 20 00 | 01]
    // config-header signature read from /sys. The vendor's reset path
    // starts from a clean FIFO; ours did not. Wait for AHB idle, flush
    // Tx (all numbers) then Rx, bounded so a stuck flush cannot hang.
    {
        let mut spins = 0u32;
        while (rd(GRSTCTL) & GRSTCTL_AHBIDLE) == 0 && spins < 0x2_0000 {
            spins += 1;
        }
        wr(GRSTCTL, GRSTCTL_TXFNUM_ALL | GRSTCTL_TXFFLSH);
        let mut spins = 0u32;
        while (rd(GRSTCTL) & GRSTCTL_TXFFLSH) != 0 && spins < 0x2_0000 {
            spins += 1;
        }
        wr(GRSTCTL, GRSTCTL_RXFFLSH);
        let mut spins = 0u32;
        while (rd(GRSTCTL) & GRSTCTL_RXFFLSH) != 0 && spins < 0x2_0000 {
            spins += 1;
        }
    }
    crate::blackbox::text_puts("UD: FFLUSH\n");
    wr(DCFG, rd(DCFG) & !DCFG_DEVADDR_MASK);
    // Clear any stale endpoint interrupts before re-arming.
    wr(DOEPINT0, 0xffff_ffff);
    wr(DIEPINT0, 0xffff_ffff);
    arm_setup_receive();
    init_ep1(log);
}

/// Pop and act on every RX FIFO status entry (the packet stream).
fn pop_rx(log: fn(&str, u64)) {
    while rd(GINTSTS) & GINT_RXFLVL != 0 {
        let s = rd(GRXSTSP);
        unsafe {
            DBG_RX_ENTRIES = DBG_RX_ENTRIES.wrapping_add(1);
            DBG_LAST_RXSTS = s;
        }
        let pktsts = (s & RXSTS_PKTSTS_MASK) >> RXSTS_PKTSTS_SHIFT;
        let bcnt = ((s & RXSTS_BCNT_MASK) >> RXSTS_BCNT_SHIFT) as usize;
        match pktsts {
            PKTSTS_SETUPRX => {
                // The 8 setup bytes live in the FIFO (2 words).
                let w0 = fifo_pop_word();
                let w1 = fifo_pop_word();
                let bytes = w0.to_le_bytes();
                let bytes2 = w1.to_le_bytes();
                unsafe {
                    SETUP_BUF[..4].copy_from_slice(&bytes);
                    SETUP_BUF[4..].copy_from_slice(&bytes2);
                    SETUP_ARMED = false;
                    SETUP_BYTES_READY = true;
                }
                SETUP_SEEN.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                log("ud: setupbcnt=", bcnt as u64);
                // Answer the moment the bytes exist. v200: the v198 single-
                // site experiment (answer only from service()'s tail, gated
                // on a per-setup FRESH flag) produced 2/2 hard enumeration
                // failures on hardware, so the proven v197 shape is back.
                if unsafe { !SETUP_HANDLED } {
                    unsafe { SETUP_HANDLED = true };
                    // v204: own the setup handshake when we answer. Consume
                    // the DOEPINT0.SETUP latch and CNAK here, so the belt in
                    // service_ep0 cannot re-answer the SAME setup when a
                    // stale XFERCOMPL clears HANDLED later in the same pass -
                    // that double push staged two copies of one response in
                    // the EP0 FIFO, and the surplus surfaced as the next
                    // read's truncated/corrupted data ([09 02 20 00 | 09 02
                    // 20 00 | 01] in the host's descriptor dump).
                    // Delay the SETUP-latch clear + CNAK until the response
                    // has actually been staged (handle_setup did its EP0 IN
                    // arm + FIFO push). Clearing the latch BEFORE the
                    // response write lets the core retire the setup slot
                    // while the IN FIFO push is still running on the slow
                    // bus, which is the residual one-behind class.
                    // (v204 order moved here; verified by trace q= lines.)
                    handle_setup(log);
                    wr(DOEPINT0, EPINT_SETUP);
                    wr(DOEPCTL0, rd(DOEPCTL0) | EPCTL_CNAK);
                    // The answer for THIS setup is staged in the EP0 IN
                    // FIFO. Clear the bytes-present latch now: the SETUP
                    // interrupt for this same setup can arrive in a LATER
                    // service pass, after the status stage has cleared
                    // SETUP_HANDLED; with the latch still set, the belt in
                    // service_ep0 re-answered the same setup and staged a
                    // second copy of the payload (the misframed 9/13-byte
                    // config reads and shifted string slots on host).
                    unsafe { SETUP_BYTES_READY = false };
                }
            }
            PKTSTS_SETUPDONE => {
                // Deliberately no handling here: the DONE word can trail a
                // whole request on this handoff (the core retires its setup
                // slot late), which is how every control answer ended up
                // one request behind. See the SETUPRX case above.
            }
            PKTSTS_OUTRX | PKTSTS_OUTDONE => {
                let ep = ((s & RXSTS_EPNUM_MASK) >> RXSTS_EPNUM_SHIFT) as usize;
                if ep == 1 && pktsts == PKTSTS_OUTDONE {
                    // The OUT transfer boundary is the command terminator:
                    // the real fastboot client sends no NUL byte after the
                    // command, so dispatch here (not on a NUL).
                    super::ane_fb::event(b"EP1 DN");
                    fb_poll(log);
                } else if ep == 1 {
                    // The fastboot command stream on EP1 OUT. The RX FIFO is
                    // shared with EP0, so the pop window is the same 0x1000.
                    // Take only the bytes the count reports: the FIFO pads
                    // the final word. Dispatch happens at the transfer
                    // boundary (OUTDONE / XFERCOMPL), not here.
                    let mut n = bcnt;
                    while n > 0 {
                        let w = fifo_pop_word();
                        let b = w.to_le_bytes();
                        let take = if n >= 4 { 4 } else { n };
                        unsafe {
                            for &byte in b.iter().take(take) {
                                let remain = core::ptr::addr_of!(DL_REMAIN).read();
                                if remain > 0 {
                                    // A download payload owns EP1 OUT until
                                    // its last byte; the trailing padding of
                                    // the final word is dropped.
                                    let len = core::ptr::addr_of!(DL_LEN).read();
                                    if len < DOWNLOAD_MAX {
                                        let buf = core::ptr::addr_of_mut!(DOWNLOAD_BUF) as *mut u8;
                                        buf.add(len).write(byte);
                                        core::ptr::addr_of_mut!(DL_LEN).write(len + 1);
                                    }
                                    core::ptr::addr_of_mut!(DL_T0).write(crate::timer::uptime_us());
                                    let next = remain - 1;
                                    core::ptr::addr_of_mut!(DL_REMAIN).write(next);
                                    if next == 0 {
                                        core::ptr::addr_of_mut!(DL_STATE).write(2);
                                    }
                                } else {
                                    let len = core::ptr::addr_of!(FB_CMD_LEN).read();
                                    if len < FB_CMD_MAX {
                                        let buf = core::ptr::addr_of_mut!(FB_CMD) as *mut u8;
                                        buf.add(len).write(byte);
                                        core::ptr::addr_of_mut!(FB_CMD_LEN).write(len + 1);
                                        DBG_EP1_BYTES = DBG_EP1_BYTES.wrapping_add(1);
                                    }
                                }
                            }
                        }
                        n = n.saturating_sub(4);
                    }
                } else {
                    // EP0 data/status OUT bytes: drain so the FIFO does not wedge.
                    let mut n = bcnt;
                    while n > 0 {
                        let _ = fifo_pop_word();
                        n = n.saturating_sub(4);
                    }
                }
            }
            _ => {}
        }
    }
}

/// v184: EP1 diagnostics for the live screen instruments: the IN-endpoint
/// control register (EPENA bit 31 tells whether a transfer is armed or
/// stuck) and the TX FIFO free-words status. Photographed while host reads
/// fail with EIO, this pins down which side of the endpoint is wedged.
///
/// v186: reading the DWC2 registers before the controller is up turns out
/// to disturb enumeration (v184/v185 boots never enumerated; v182 with no
/// such reads lived 30 min). Gate all such reads behind USB_ENUMERATED.
static mut USB_ENUMERATED: bool = false;

pub fn usb_enumerated() -> bool {
    unsafe { core::ptr::addr_of!(USB_ENUMERATED).read() }
}

/// v199: the tick count when the boot attached (0 = not yet). The timer-IRQ
/// sentinel panics a boot whose enumeration never gets configured.
pub static ATTACH_BEAT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// True once the kernel has raised the pull-up after responder/storage
/// bring-up. This is intentionally independent of SETUP_SEEN: a physically
/// connected host may lose the first descriptor transaction before any SETUP
/// reaches the responder, but that boot still needs the autonomous retry and
/// loader-reset path.
pub static ATTACHED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn mark_attached() {
    ATTACHED.store(true, core::sync::atomic::Ordering::Relaxed);
}

pub fn usb_attached() -> bool {
    ATTACHED.load(core::sync::atomic::Ordering::Relaxed)
}

/// v203: total SETUP packets received this boot. The self-heal uses this as
/// "the host has talked to us" evidence - no attach-beat series dependency.
pub static SETUP_SEEN: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn setups_seen() -> u32 {
    SETUP_SEEN.load(core::sync::atomic::Ordering::Relaxed)
}

/// v300: string-descriptor transfers served this boot. A clean enumeration
/// always reads the langid + product strings; a garbled boot reaches
/// config-1 with zero served strings (the one-behind state mangles exactly
/// the string stage). The watchdog uses this to auto-cycle D+ on a garbled
/// boot - the host never speaks to it again otherwise.
/// v302: the host has PROVEN the enumeration reached (and survived) the
/// string stage. The garble detector's old premise (`strings_served()==0`
/// on a garbled boot) is false on real hardware - the corruption rotates
/// strings between slots, it does not drop them, so the counter counts up
/// on garbled boots too and the detector never fired. This flag is the
/// corrected premise: set when SET_ADDRESS arrives (the one request that
/// only a healthy descriptor exchange precedes) and NEVER on a garbled
/// boot - the rotation makes the host's next GET_DESCRIPTOR(String) read a
/// stale payload, and a host that sees a mangled identity never proceeds
/// to SET_CONFIGURATION... except the measured garbled boots DID reach
/// config-1. So the honest invariant is weaker: SET_ADDRESS seen implies
/// EP0 sequencing works at all; the watchdog then only guards the
/// `strings_served()==0` case, which remains real for the pre-string
/// deaths (config-1 via host cache). Kept as a marker for the FB line.
pub static ADDRESS_SEEN: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

pub fn address_seen() -> u32 {
    ADDRESS_SEEN.load(core::sync::atomic::Ordering::Relaxed)
}

pub static STRINGS_SERVED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn strings_served() -> u32 {
    STRINGS_SERVED.load(core::sync::atomic::Ordering::Relaxed)
}

/// v199: whether the host completed SET_CONFIGURATION for this boot.
pub fn usb_configured() -> bool {
    unsafe { core::ptr::addr_of!(CONFIGURED).read() }
}

/// v202: the SET_CONFIGURATION value seen last (1 = real config).
pub fn usb_config_value() -> u8 {
    unsafe { core::ptr::addr_of!(CONFIG_VALUE).read() }
}

/// v205: true once the host configured us with value 1 (healthy boot).
pub fn usb_config_was_1() -> bool {
    unsafe { core::ptr::addr_of!(CONFIG_WAS_1).read() }
}

/// v207: clean the EP0 machinery right before the deferred attach. The
/// first enumeration's answers inherit whatever endpoint state an earlier
/// era left behind (EPENA latched -> the first TSIZ writes get dropped ->
/// the -110/-71 first-read dance seen on every boot's first attempt).
/// Called from usb_attach() while the device is still soft-disconnected,
/// so nothing can be in flight.
pub fn prepare_attach() {
    // Hard-clear any armed transfer on EP0 (both directions).
    let c = rd(DIEPCTL0);
    if c & EPCTL_EPENA != 0 {
        wr(DIEPCTL0, c | EPCTL_EPENA | EPCTL_EPDIS);
        let mut s = 0u32;
        while (rd(DIEPCTL0) & EPCTL_EPENA) != 0 && s < 0x2_0000 {
            s += 1;
        }
    }
    let c = rd(DOEPCTL0);
    if c & EPCTL_EPENA != 0 {
        wr(DOEPCTL0, c | EPCTL_EPENA | EPCTL_EPDIS);
        let mut s = 0u32;
        while (rd(DOEPCTL0) & EPCTL_EPENA) != 0 && s < 0x2_0000 {
            s += 1;
        }
    }
    // Flush the FIFOs - with the device off the bus this is the safest
    // moment for it.
    {
        let mut s = 0u32;
        while (rd(GRSTCTL) & GRSTCTL_AHBIDLE) == 0 && s < 0x2_0000 {
            s += 1;
        }
        wr(GRSTCTL, GRSTCTL_TXFNUM_ALL | GRSTCTL_TXFFLSH);
        let mut s = 0u32;
        while (rd(GRSTCTL) & GRSTCTL_TXFFLSH) != 0 && s < 0x2_0000 {
            s += 1;
        }
        wr(GRSTCTL, GRSTCTL_RXFFLSH);
        let mut s = 0u32;
        while (rd(GRSTCTL) & GRSTCTL_RXFFLSH) != 0 && s < 0x2_0000 {
            s += 1;
        }
    }
    // Clear stale endpoint interrupts, reset the transfer latches and
    // re-arm the setup receiver so the first SETUP after attach lands in
    // a primed machine.
    wr(DOEPINT0, 0xffff_ffff);
    wr(DIEPINT0, 0xffff_ffff);
    unsafe {
        core::ptr::addr_of_mut!(TRANSACTION_BUSY).write(false);
        core::ptr::addr_of_mut!(SETUP_HANDLED).write(false);
        core::ptr::addr_of_mut!(SETUP_BYTES_READY).write(false);
        core::ptr::addr_of_mut!(SETUP_BYTES_FRESH).write(false);
        core::ptr::addr_of_mut!(SETUP_ARMED).write(false);
    }
    arm_setup_receive();
    crate::blackbox::text_puts("UD: PREP\n");
}

/// v202 self-heal: drop the D+ pull-up so the host sees a disconnect.
pub fn usb_reconnect_begin() {
    wr(DCTL, rd(DCTL) | 0x2);
}

/// v202 self-heal: re-attach; the host enumerates afresh.
pub fn usb_reconnect_end() {
    wr(DCTL, rd(DCTL) & !0x2);
}

pub fn ep1_diag() -> u64 {
    ((rd(DIEPCTL1) as u64) << 32) | (rd(DTXFSTS1) as u64)
}

/// Send `data` on the EP1 bulk IN endpoint (the fastboot response path).
fn ep1_send(data: &[u8]) {
    // v217: shared implementation - the pointer form lets the `oem xfer r`
    // stream arm ONE transfer per window straight from the source RAM (no
    // intermediate buffer). The v218 windowed fill makes long transfers safe.
    let _ = ep1_send_ptr(data.as_ptr() as usize, data.len());
}

/// v217: send `n` bytes straight from `src` (raw address). Returns the
/// bytes actually handed to the FIFO (v218: capped per call - see below).
fn ep1_send_ptr(src: usize, n: usize) -> usize {
    // v218: decide how much this call transfers BEFORE arming. Small sends
    // (<= one packet: every control-style reply) keep the proven blind
    // path. Larger sends read DTXFSTS ONCE, capped at 12 KiB (< the
    // measured 16 KiB depth): the register is not trustworthy on this bus
    // (the v176 note), and a fill loop that RE-reads and keeps writing can
    // push into a full FIFO - the AHB write then stalls forever (measured
    // on v217's 128 KiB arm: the header went out, then a 4-minute freeze).
    // A single read cannot overrun by more than one stale headroom
    // reading, and every caller arms exactly what this returns.
    let take = if n <= 512 {
        n
    } else {
        let free_words = (rd(DTXFSTS1) & 0xffff) as usize;
        core::cmp::min(n, core::cmp::min(free_words * 4, 0x3000))
    };
    let n = take;
    // Send the padded word count so no stale bytes stay in the FIFO (a
    // leftover word used to surface as junk on a later reply). The host
    // reads C strings, so the trailing NULs are harmless.
    let mut padded = (n + 3) & !3;
    // One-time per boot: the core's stale TX-FIFO content (the loader's or a
    // previous kernel's unsent stream, ~1.4 KB measured) sits AHEAD of our
    // reply - the core drains the FIFO head-first - so extend this first
    // transfer to sweep stale + reply in one go. Safe here, unlike the v32
    // init-time attempt: a session is live, this is the same arm+push path
    // every reply uses, and nothing else writes the FIFO between the count
    // read and the arm, so the size is exact. DTXFSTS counts the FIFO's
    // FREE words (vendor gadget.c:378-381 names it `can_write`), so the
    // fill is depth minus free; asking for more than is present once wedged
    // the endpoint (the transfer waits forever for bytes that never come).
    unsafe {
        if !core::ptr::addr_of!(SWEPT_FIRST_REPLY).read() {
            core::ptr::addr_of_mut!(SWEPT_FIRST_REPLY).write(true);
            super::ane_fb::event(b"SENT1");
            let free = ((rd(DTXFSTS1) & 0xffff) as usize) * 4;
            let depth = ((rd(DPTXFSIZ1) >> 16) as usize) * 4;
            let stale = depth.saturating_sub(free);
            // v161 marker: the sweep's inputs. If the reply never reaches
            // the host, this line (photo-read from the panel tail, or the
            // next piggyback) separates an inaccurate free-read from a
            // truly stale FIFO.
            crate::blackbox::text_puts("TX0:");
            ring_hex8(free as u32);
            crate::blackbox::text_byte(b',');
            ring_hex8(depth as u32);
            crate::blackbox::text_byte(b',');
            ring_hex8(stale as u32);
            crate::blackbox::text_puts(" S:");
            ring_hex8(padded as u32);
            crate::blackbox::text_byte(b'\n');
            // v172: do NOT extend the transfer by `stale`. The FIFO really
            // does hold loader leftovers ahead of our reply, but a transfer
            // sized beyond what is present wedges EP1 forever (the comment
            // above predicted it: "asking for more than is present once
            // wedged the endpoint"). Observed consequence: the host's very
            // first read after a boot returns EIO - the first reply of
            // every boot was the casualty, which is exactly why the CNXN
            // ring piggyback and the `oem xfer` streams never arrived.
            // Every host parser already scans for its own header ("OKAY",
            // the ADB magic) and skips any stale prefix, so the stale bytes
            // can simply flow out ahead of the payload on the first reply.
        }
    }
    // v216: never arm on top of an active EP1 transfer. The stream callers
    // re-enter per chunk, and arming over a pending one drops the new arm
    // (the same class as the EP0 EPENA issue). Bounded.
    let mut w = 0u32;
    while (rd(DIEPCTL1) & EPCTL_EPENA) != 0 && w < 0x2_0000 {
        w += 1;
    }
    // DIEPTSIZ, exactly as the vendor gadget.c programs it (primary source:
    // hi6250 gadget.c:584-614): MC(1) for every non-EP0 IN endpoint,
    // PKTCNT = ceil(xfersize / MPS) - packet count, not 1! - and the byte
    // count. The old constant PKTCNT=1 was only correct for replies up to
    // one packet; anything larger ended the transfer after 512 bytes.
    let pktcnt = ((padded + BULK_MPS as usize - 1) / BULK_MPS as usize).max(1) as u32;
    wr(
        DIEPTSIZ1,
        (1 << 29) | (pktcnt << 19) | ((padded & 0x7ffff) as u32),
    );
    wr(
        DIEPCTL1,
        (rd(DIEPCTL1) & !EPCTL_SNAK) | EPCTL_EPENA | EPCTL_CNAK | EPCTL_USBACTEP,
    );
    // v216: gate every FIFO write on the advertised free space. The old
    // blind fill wrote all `n` bytes regardless of room; replies larger
    // than the TX FIFO's depth overran it and wedged EP1 forever (the v172
    // lesson - the oem-xfer streams were the casualty: the CNXN piggyback
    // fit, so collections lived while every big read died). DTXFSTS1
    // counts free words (the vendor gadget.c "can_write"). Bounded: a FIFO
    // that never drains truncates the reply instead of hanging the IRQ.
    // v218: fill exactly the window decided above - no re-reads, no loop.
    let mut i = 0;
    while i < n {
        let mut word = 0u32;
        for j in 0..4 {
            if i + j < n {
                word |=
                    (unsafe { ptr::read_volatile((src + i + j) as *const u8) } as u32) << (8 * j);
            }
        }
        unsafe {
            ptr::write_volatile((USB_CORE_BASE + EPFIFO1) as *mut u32, word);
        }
        i += 4;
    }
    unsafe {
        DBG_FB_REPLY = DBG_FB_REPLY.wrapping_add(1);
    }
    // v165: expose this reply's real transfer size to the S4 tick instrument.
    LAST_SEND_N.store(padded as u32, core::sync::atomic::Ordering::Relaxed);
    // v218: the caller must know exactly what this arm covers.
    take
}

/// Trim a command to its first known fastboot verb, skipping any stale
/// prefix the loader's core state glued in front of it.
fn anchor_verb(raw: &[u8]) -> &[u8] {
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
fn anchor_adb(raw: &[u8]) -> &[u8] {
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
fn fmt_okay_hex(value: u64, out: &mut [u8; 32]) -> usize {
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
static mut ADB_OUT: [u8; 4096] = [0u8; 4096];
static mut ADB_BANNER: [u8; 2080] = [0u8; 2080];
static mut ADB_RUN: [u8; 8192] = [0u8; 8192];
static mut ADB_LOG_CHUNK: [u8; 2048] = [0u8; 2048];
static mut ADB_PAYLOAD: [u8; 2049] = [0u8; 2049];
static mut ADB_LOG_BUF: [u8; 512] = [0u8; 512];

/// Parse a hex offset ("1c00"); stops at the first non-hex byte.
fn parse_hex(bytes: &[u8]) -> usize {
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
fn main_log_len() -> usize {
    let raw = unsafe { ptr::read_volatile((crate::blackbox::TEXT_BLOCK_BASE + 4) as *const u32) }
        as usize;
    let cap = crate::blackbox::TEXT_BLOCK_CAP as usize;
    if raw < cap { raw } else { 0 }
}

/// Minimal ADB wire responder: handshake plus enough of OPEN/write/close to
/// give a host `adb` a live device. The framing itself lives in
/// `crate::adb_wire` (pure, host-tested); this is only the plumbing that
/// drives the module's encoders onto the bulk endpoint.
fn adb_dispatch(pkt: &crate::adb_wire::Packet<'_>, log: fn(&str, u64)) {
    let out = unsafe { &mut *core::ptr::addr_of_mut!(ADB_OUT) };
    match pkt.command {
        crate::adb_wire::CNXN => {
            // Ring marker first: a photo (or the piggyback of a later boot)
            // proves whether this CNXN reached the ADB dispatch at all.
            crate::blackbox::text_puts("ADB: CNXN\n");
            super::ane_fb::event(b"ADB CNXN");
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
            super::ane_fb::event(b"BG");
            let head = b"device::fullerene-ane\0";
            banner[..head.len()].copy_from_slice(head);
            let mut blen = head.len();
            let used = main_log_len();
            let take = core::cmp::min(used, 2048);
            let start = used - take;
            for i in 0..take {
                banner[blen + i] = unsafe {
                    ptr::read_volatile(
                        (crate::blackbox::TEXT_BLOCK_BASE as usize + start + i) as *const u8,
                    )
                };
            }
            blen += take;
            super::ane_fb::event(b"RD");
            if let Some(n) = crate::adb_wire::cnxn_reply(
                crate::adb_wire::VERSION,
                crate::adb_wire::MAX_PAYLOAD as u32,
                &banner[..blen],
                &mut out[..],
            ) {
                super::ane_fb::event(b"RP");
                super::ane_fb::event(b"SS");
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
                    ep1_send(&full[off..off + chunk]);
                    super::ane::watchdog_kick();
                    let depth_words = ((rd(DPTXFSIZ1) >> 16) & 0xffff) as usize;
                    let mut spins = 0u32;
                    while (((rd(DTXFSTS1) & 0xffff) as usize) < depth_words) && spins < 0x20_0000 {
                        spins += 1;
                    }
                    off += chunk;
                }
                log("adb: cnxn sent", n as u64);
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
                    super::ane_fb::event(b"ADB MAGIC");
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
                ep1_send(&run[..at]);
            }
            log("adb: open", svc.len() as u64);
        }
        crate::adb_wire::AUTH => {
            // No authentication is offered; older adb accepts silence here.
            log("adb: auth ignored", pkt.payload.len() as u64);
        }
        crate::adb_wire::WRTE | crate::adb_wire::CLSE | crate::adb_wire::OKAY => {
            log("adb: post-open frame", pkt.command as u64);
        }
        _ => {}
    }
}

/// Fastboot protocol: run when bytes arrived on EP1 OUT. The host's command
/// lines are NUL-terminated, and the responses are the plain "OKAY"/"FAIL"
/// strings the host's parser reads back NUL-terminated.
fn fb_poll(log: fn(&str, u64)) {
    // A download in flight owns EP1 OUT: skip command parsing until its
    // payload is complete, then answer it once and resume.
    match unsafe { core::ptr::addr_of!(DL_STATE).read() } {
        1 => return,
        2 => {
            let len = unsafe { core::ptr::addr_of!(DL_LEN).read() };
            unsafe { core::ptr::addr_of_mut!(DL_STATE).write(0) };
            let (w0, wl) = unsafe {
                let base = core::ptr::addr_of!(DOWNLOAD_BUF) as *const u8;
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
            log("fb: dl_wl=", wl as u64);
            ep1_send(b"OKAY");
            return;
        }
        _ => {}
    }
    let len = unsafe { core::ptr::addr_of!(FB_CMD_LEN).read() };
    if len == 0 {
        return;
    }
    // A command arriving while `oem log` streams means the client moved on
    // (or a new session started); stop the stream so it cannot flood an
    // endpoint nobody reads. The oem-log handler re-activates it below.
    unsafe {
        core::ptr::addr_of_mut!(LOG_ACTIVE).write(false);
    }
    // ADB wire path: a host `adb` speaks the binary 24-byte framing whose
    // header contains NUL bytes, so this check runs on the RAW transfer
    // length before the fastboot NUL-trim below can cut it short. Text
    // commands fail the frame magic instantly and fall through untouched.
    if len >= crate::adb_wire::HEADER_BYTES {
        let raw =
            unsafe { core::slice::from_raw_parts(core::ptr::addr_of!(FB_CMD) as *const u8, len) };
        // The RX FIFO can carry loader-session leftovers in front of our
        // frame (the same glued-prefix defect the text path anchors around).
        // Decoding the raw buffer made every CNXN fail the magic check and
        // slide into the fastboot fallback. Anchor on the ADB header magic
        // first; a false anchor cannot pass the trailing magic check.
        let anchored = anchor_adb(raw);
        match crate::adb_wire::decode(anchored) {
            Ok(pkt) => {
                if anchored.len() != raw.len() {
                    super::ane_fb::event(b"DEC ANCH");
                }
                super::ane_fb::event(b"DEC OK");
                adb_dispatch(&pkt, log);
                unsafe {
                    core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
                }
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
                super::ane_fb::event(&ev);
            }
        }
    }
    // The command ends at the transfer boundary; when the wire carried a
    // NUL too, everything past the first NUL is padding/junk and ignored.
    let mut end = len;
    unsafe {
        let buf = core::ptr::addr_of!(FB_CMD) as *const u8;
        for i in 0..len {
            if buf.add(i).read() == 0 {
                end = i;
                break;
            }
        }
    }
    if end == 0 {
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return; // nothing but padding
    }
    let cmd = unsafe { core::slice::from_raw_parts(core::ptr::addr_of!(FB_CMD) as *const u8, end) };
    // The core can carry one stale transfer over from the loader's session;
    // anchor past any glued prefix so the dispatch still matches.
    let cmd = anchor_verb(cmd);
    unsafe {
        DBG_FB_DISPATCH = DBG_FB_DISPATCH.wrapping_add(1);
        DBG_CMD_HEAD = 0;
        for &b in cmd.iter().take(8) {
            DBG_CMD_HEAD = (DBG_CMD_HEAD << 8) | b as u64;
        }
    }
    log("fb: cmd len=", len as u64);
    log("fb: cmd8=", unsafe {
        core::ptr::addr_of!(DBG_CMD_HEAD).read()
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
            written = main_log_len();
            let rest = &cmd[b"oem log main".len()..];
            let off = parse_hex(rest.strip_prefix(b" ").unwrap_or(rest));
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
            core::ptr::addr_of_mut!(LOG_ACTIVE).write(true);
            core::ptr::addr_of_mut!(LOG_CURSOR).write(start);
            core::ptr::addr_of_mut!(LOG_LINES).write(0);
            core::ptr::addr_of_mut!(LOG_END).write(written);
            core::ptr::addr_of_mut!(LOG_MAIN).write(main);
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
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
        ep1_send(b"OKAY");
        fb_schedule_reboot(cmd, log);
        return;
    }

    if let Some(rest) = cmd.strip_prefix(b"download:") {
        let size = parse_hex(rest) as usize;
        if size == 0 || size > DOWNLOAD_MAX {
            ep1_send(b"FAILbad download size");
        } else {
            unsafe {
                core::ptr::addr_of_mut!(DL_REMAIN).write(size);
                core::ptr::addr_of_mut!(DL_LEN).write(0);
                core::ptr::addr_of_mut!(DL_STATE).write(1);
                core::ptr::addr_of_mut!(DL_T0).write(crate::timer::uptime_us());
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
            ep1_send(&reply);
            log("fb: dl_arm=", size as u64);
        }
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    if let Some(part) = cmd.strip_prefix(b"flash:") {
        let dl_len = unsafe { core::ptr::addr_of!(DL_LEN).read() };
        if dl_len == 0 || unsafe { core::ptr::addr_of!(DL_STATE).read() } != 0 {
            ep1_send(b"FAILno download in progress");
        } else {
            let mut nm = 0u64;
            for &b in part.iter().take(8) {
                nm = (nm << 8) | b as u64;
            }
            log("fb: flash_name=", nm);
            match super::ane_emmc::find_partition(part, log) {
                None => ep1_send(b"FAILpartition not found"),
                Some((start, _last)) => {
                    log("fb: flash_lba=", start as u64);
                    let data = unsafe {
                        core::slice::from_raw_parts(
                            core::ptr::addr_of!(DOWNLOAD_BUF) as *const u8,
                            dl_len,
                        )
                    };
                    let (ok_s, bad_s) = super::ane_emmc::write_sectors(start, data, log);
                    log("fb: flash_ok=", ok_s);
                    log("fb: flash_bad=", bad_s);
                    if bad_s == 0 {
                        ep1_send(b"OKAY");
                    } else {
                        ep1_send(b"FAILwrite error");
                    }
                }
            }
        }
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
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
    if let Some(args) = cmd.strip_prefix(b"oem scan ") {
        // `oem scan <lba-hex> <count-hex>`: compact non-zero-word scan.
        let mut lba = 0u32;
        let mut count = 0u32;
        let mut it = args.split(|&b| b == b' ');
        if let Some(a) = it.next() {
            lba = parse_hex(a) as u32;
        }
        if let Some(b) = it.next() {
            count = parse_hex(b) as u32;
        }
        if count == 0 || count > 256 {
            ep1_send(b"FAILbad range");
        } else {
            log("fb: scan_req=", ((lba as u64) << 32) | count as u64);
            super::ane_emmc::scan_sectors(lba, count, log);
            ep1_send(b"OKAYscan");
        }
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    #[cfg(fullerene_aarch64_ane)]
    if cmd.starts_with(b"oem ringsave") {
        // On-demand full-ring persistence from the command context (see
        // ane_emmc::persist_all for why it must not run from the IRQ).
        let (ok, bad) = super::ane_emmc::persist_all(log);
        log("em: rs_ok=", ok);
        log("em: rs_bad=", bad);
        ep1_send(b"OKAYsaved");
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
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
                arg = parse_hex(a) as u64;
            }
            if let Some(l) = it.next() {
                len = parse_hex(l) as u64;
            }
        }
        let total: u64 = if kind == b'r' as u8 { len } else { len * 512 };
        if total == 0 || total > 0x40_0000 || (kind != b'r' as u8 && kind != b'm' as u8) {
            ep1_send(b"FAILbad xfer");
        } else {
            let mut hdr = [0u8; 20];
            hdr[..4].copy_from_slice(b"OKAY");
            let hexd = b"0123456789abcdef";
            let mut v = total;
            for i in (4..20).rev() {
                hdr[i] = hexd[(v & 0xf) as usize];
                v >>= 4;
            }
            ep1_send(&hdr);
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
                    if !super::ane_emmc::read_sectors_bytes(lba as u32, 1, &mut buf[..], log) {
                        break;
                    }
                    ep1_send(&buf[..chunk]);
                    // Keep the loader watchdog fed while the stream holds the CPU.
                    super::ane::watchdog_kick();
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
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
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
            lba = parse_hex(a) as u32;
        }
        if let Some(b) = it.next() {
            count = parse_hex(b) as u32;
        }
        if count == 0 || count > 16 {
            ep1_send(b"FAILbad range");
        } else {
            log("fb: dump_req=", ((lba as u64) << 32) | count as u64);
            super::ane_emmc::dump_sectors(lba, count, log);
            ep1_send(b"OKAYdump");
        }
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
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
        super::ane_emmc::emmc_userdata_mount(log);
        log("fb: udm2=", crate::timer::uptime_us());
        ep1_send(b"OKAYudm");
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    if cmd == b"oem touch" {
        // One plain eMMC sector read from the USB command context (LBA 0):
        // isolates the post-boot read path from the GPT logic.
        log("fb: ot1=", crate::timer::uptime_us());
        let mut t = [0u32; 128];
        let (ok, sts, err) = super::ane_emmc::sdhci_read_probe(0, &mut t, log);
        log("fb: ot2=", crate::timer::uptime_us());
        log("fb: ot_ok=", ok as u64);
        log("fb: ot_sts=", sts as u64);
        log("fb: ot_err=", err as u64);
        log("fb: ot_w0=", t[0] as u64);
        ep1_send(b"OKAYtouch");
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    if cmd == b"oem ps1" {
        log("fb: ps1=", crate::timer::uptime_us());
        ep1_send(b"OKAYps1");
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    if cmd == b"oem ps2" {
        log("fb: ps2a=", crate::timer::uptime_us());
        let found = super::ane_emmc::find_partition(b"kernel", log);
        log("fb: ps2b=", crate::timer::uptime_us());
        let mut gv = [0u8; 32];
        let n = fmt_okay_hex(found.map(|f| f.0 as u64).unwrap_or(0), &mut gv);
        ep1_send(&gv[..n]);
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    if cmd == b"oem ps3" {
        log("fb: ps3a=", crate::timer::uptime_us());
        let mut gv = [0u8; 32];
        let n = fmt_okay_hex(0xDEAD_BEEF, &mut gv);
        log("fb: ps3b=", crate::timer::uptime_us());
        ep1_send(&gv[..n]);
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    if cmd == b"oem ps4" {
        // The full branch logic, but the read is one sector only.
        log("fb: ps4a=", crate::timer::uptime_us());
        let mut t = [0u32; 128];
        let (ok, _sts, _err) = super::ane_emmc::sdhci_read_probe(0, &mut t, log);
        log("fb: ps4b=", crate::timer::uptime_us());
        let mut gv = [0u8; 32];
        let n = fmt_okay_hex(ok as u64, &mut gv);
        ep1_send(&gv[..n]);
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    if let Some(part) = cmd.strip_prefix(b"erase:") {
        let mut nm = 0u64;
        for &b in part.iter().take(8) {
            nm = (nm << 8) | b as u64;
        }
        log("fb: erase_name=", nm);
        match super::ane_emmc::find_partition(part, log) {
            None => ep1_send(b"FAILpartition not found"),
            Some((first, last)) => {
                log("fb: erase_fr=", ((first as u64) << 32) | last as u64);
                let (ok, err) = super::ane_emmc::erase_sectors(first, last, log);
                log("fb: erase_ok=", ok as u64);
                log("fb: erase_err=", err as u64);
                if ok {
                    ep1_send(b"OKAY");
                } else {
                    ep1_send(b"FAILerase error");
                }
            }
        }
        unsafe {
            core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
        }
        return;
    }

    let mut reply: &[u8] = b"FAILunknown command";
    if let Some(name) = cmd.strip_prefix(b"getvar:") {
        // partition-size:<name> needs a computed reply, so it leaves the
        // static-string match below (the client asks this before flashing;
        // a FAIL makes it warn "partition size: 0" but continue).
        if let Some(part) = name.strip_prefix(b"partition-size:") {
            log("fb: ps1=", crate::timer::uptime_us());
            let mut gv = [0u8; 32];
            if let Some((first, last)) = super::ane_emmc::find_partition(part, log) {
                log("fb: ps2=", crate::timer::uptime_us());
                let n = fmt_okay_hex(((last - first + 1) as u64) * 512, &mut gv);
                log("fb: ps3=", crate::timer::uptime_us());
                ep1_send(&gv[..n]);
                log("fb: ps4=", crate::timer::uptime_us());
            } else {
                ep1_send(b"FAILunknown partition");
            }
            unsafe {
                core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
            }
            return;
        }
        reply = match name {
            b"version" => b"OKAY0.4",
            b"product" => b"OKAYANE-LX2J",
            b"serialno" => b"OKAYSCV7N18927000473",
            b"secure" => b"OKAYno",
            b"unlocked" => b"OKAYyes",
            // The client parses numeric getvars with strtoul base 0, so a
            // bare hex string fails ("couldn't parse max-download-size").
            b"max-download-size" => b"OKAY0xC00000",
            _ => b"FAILunknown variable",
        };
    }
    ep1_send(reply);
    unsafe {
        core::ptr::addr_of_mut!(FB_CMD_LEN).write(0);
    }
}

/// Advance the paced `oem log` stream by one line (called every service
/// pass, so the host's reads never coalesce into unusable chunks).
#[cfg(fullerene_aarch64_ane)]
fn fb_log_step() {
    if unsafe { !core::ptr::addr_of!(LOG_ACTIVE).read() } {
        return;
    }
    // Pace to the READER: send the next line only when the unsent backlog is
    // under one line's worth. A live client drains as we go (the stream
    // runs at its pace); an abandoned dump stalls after ~a line instead of
    // piling the whole stream into the FIFO, where it would ride later
    // sessions' replies. (DTXFSTS reports FREE words; fill = depth - free.)
    let free = ((rd(DTXFSTS1) & 0xffff) as usize) * 4;
    let depth = ((rd(DPTXFSIZ1) >> 16) as usize) * 4;
    if depth.saturating_sub(free) > 256 {
        return;
    }
    // One transfer at a time: wait for the previous line's completion event
    // before arming the next (see EP1_IN_IDLE).
    if !unsafe { core::ptr::addr_of!(EP1_IN_IDLE).read() } {
        return;
    }
    // Backlog-only pacing: one line in flight at a time paces the stream to
    // the client's own read rate. (An earlier time-based pace used the
    // watchdog counter as its clock, but that counter's real rate is far
    // below its 32.768 kHz spec on this board, which silently throttled the
    // stream to seconds per line.)
    // Only this boot's bytes, frozen at command time: re-reading the length
    // live would chase the boot's own ongoing log writes forever.
    let main = unsafe { core::ptr::addr_of!(LOG_MAIN).read() };
    let written = unsafe { core::ptr::addr_of!(LOG_END).read() };
    let written = if main {
        written.min(crate::blackbox::TEXT_BLOCK_CAP as usize)
    } else {
        written.min(0x1c00)
    };
    let base_off = if main {
        12u64
    } else {
        crate::blackbox::FIXED_LOG_OFF
    };
    let base = (crate::blackbox::TEXT_BLOCK_BASE + base_off) as *const u8;
    let lines = unsafe { core::ptr::addr_of!(LOG_LINES).read() };
    if lines >= if main { 4000 } else { 500 } {
        ep1_send(b"OKAYdone");
        unsafe {
            core::ptr::addr_of_mut!(LOG_ACTIVE).write(false);
        }
        return;
    }
    let mut cursor = unsafe { core::ptr::addr_of!(LOG_CURSOR).read() };
    // Skip separators between lines.
    while cursor < written {
        let b = unsafe { base.add(cursor).read() };
        if b == 0 || b == b'\n' || b == b'\r' {
            cursor += 1;
        } else {
            break;
        }
    }
    let mut line = [0u8; 48];
    let mut n = 0usize;
    while cursor < written && n < line.len() {
        let b = unsafe { base.add(cursor).read() };
        if b == 0 || b == b'\n' || b == b'\r' {
            break;
        }
        line[n] = b;
        n += 1;
        cursor += 1;
    }
    // Two consecutive NULs mark the end of the written area.
    let end = cursor < written
        && unsafe { base.add(cursor).read() } == 0
        && cursor + 1 < written
        && unsafe { base.add(cursor + 1).read() } == 0;
    unsafe {
        core::ptr::addr_of_mut!(LOG_CURSOR).write(cursor);
        core::ptr::addr_of_mut!(LOG_LINES).write(lines + 1);
    }
    if n > 0 {
        unsafe {
            core::ptr::addr_of_mut!(EP1_IN_IDLE).write(false);
        }
        fb_send_info(&line[..n]);
    }
    if end || cursor >= written {
        ep1_send(b"OKAYdone");
        unsafe {
            core::ptr::addr_of_mut!(LOG_ACTIVE).write(false);
        }
    }
}

/// Panic-path reset: the same proven chain as `reboot-bootloader` (PMIC
/// reason -> LPM3 NMI -> SP805 fallback), so a crashing kernel lands in
/// the loader's fastboot instead of parking until the watchdog fires.
/// Parking is what feeds the loader's boot-fail monitor its failed-boot
/// count, and after enough of those the loader falls back to rescue
/// (eRecovery). Landing in fastboot instead lets the always-armed window
/// catcher reflash and continue with no key combo. Pure MMIO; callable
/// from the half-dead world. Never returns.
pub fn panic_reset_to_bootloader() -> ! {
    // BOOTLOADER=0x01: the loader enters fastboot on its next start.
    unsafe {
        ptr::write_volatile(PMIC_REASON_ADDR as *mut u8, 0x01);
    }
    // Same bounded settle as the fastboot path (counts only decreasing
    // stretches of WDOGVALUE so the keep-alive cannot fake elapsed time).
    let mut prev = wdog_r(0x004);
    let mut elapsed = 0u32;
    let mut spins = 0u32;
    while elapsed < 10000 && spins < 50_000_000 {
        let v = wdog_r(0x004);
        if v < prev {
            elapsed += prev - v;
        }
        prev = v;
        spins += 1;
    }
    // LPM3 NMI pulse (SCTRL + 0x510 bit 2) - the always-on MCU performs
    // the reset the loader's state machine expects.
    unsafe {
        let r = 0xfff0_a510usize as *mut u32;
        let v = ptr::read_volatile(r);
        ptr::write_volatile(r, v | (1 << 2));
        ptr::write_volatile(r, v & !(1 << 2));
    }
    // SP805 fallback in ~0.5 s if the LPM3 does not step in; the reason
    // is latched either way.
    unsafe {
        if wdog_r(0xc00) != 0 {
            wdog_w(0xc00, 0x1acc_e551);
        }
        wdog_w(0x00c, 1);
        wdog_w(0x000, 0x4000);
        wdog_w(0x008, 3);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Hand the loader its next boot mode, give the OKAY time to drain, then
/// reset the SoC through the SP805 the loader already shares with us.
fn fb_schedule_reboot(cmd: &[u8], log: fn(&str, u64)) {
    // BOOTLOADER=0x01 / RECOVERY=0x02 / COLDBOOT=0x10 - the same 8-bit codes
    // `reboot_reason_map` in the stock tree maps restart strings to.
    let reason: u8 = if cmd == b"reboot-recovery" {
        0x02
    } else if cmd == b"reboot" {
        0x10
    } else {
        0x01
    };
    // The PMIC cell: one byte at 0xFFF34000 + (reg << 2).
    unsafe {
        ptr::write_volatile(PMIC_REASON_ADDR as *mut u8, reason);
    }
    // Read it back for the durable log (visible through `oem log`).
    let back = unsafe { ptr::read_volatile(PMIC_REASON_ADDR as *const u8) };
    log("fb: hrst=", back as u64);
    // Drain grace (~200 ms at 32.768 kHz): accumulate only decreasing
    // stretches of WDOGVALUE so the keep-alive reloads cannot fake elapsed
    // time. Bounded so a surprise costs a bounded spin, never a hang.
    let mut prev = wdog_r(0x004);
    let mut elapsed = 0u32;
    let mut spins = 0u32;
    while elapsed < 10000 && spins < 50_000_000 {
        let v = wdog_r(0x004);
        if v < prev {
            elapsed += prev - v;
        }
        prev = v;
        spins += 1;
    }
    // The full stock chain also pulses the LPM3 NMI (SCTRL + 0x510, bit 2):
    // the always-on MCU performs the reset the loader's state machine
    // expects. v28 without it was ignored (the loader cold-booted instead).
    unsafe {
        let r = 0xfff0_a510usize as *mut u32;
        let v = ptr::read_volatile(r);
        ptr::write_volatile(r, v | (1 << 2));
        ptr::write_volatile(r, v & !(1 << 2));
    }
    // Fallback reset in ~0.5 s if the LPM3 does not step in; the reason is
    // latched either way.
    unsafe {
        if wdog_r(0xc00) != 0 {
            wdog_w(0xc00, 0x1acc_e551);
        }
        wdog_w(0x00c, 1);
        wdog_w(0x000, 0x4000);
        wdog_w(0x008, 3);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn wdog_r(off: usize) -> u32 {
    unsafe { ptr::read_volatile((WATCHDOG_BASE + off) as *const u32) }
}

fn wdog_w(off: usize, value: u32) {
    unsafe { ptr::write_volatile((WATCHDOG_BASE + off) as *mut u32, value) }
}

/// One INFO response line (the host prints it verbatim).
fn fb_send_info(text: &[u8]) {
    let mut buf = [0u8; 56];
    buf[..4].copy_from_slice(b"INFO");
    let n = core::cmp::min(text.len(), buf.len() - 4);
    buf[4..4 + n].copy_from_slice(&text[..n]);
    ep1_send(&buf[..4 + n]);
}

/// Bring up (or re-arm after a bus reset) the EP1 fastboot bulk pair.
///
/// No FIFO flush here: writing GRSTCTL while the core sits in the loader's
/// handoff state wedged it (v26 produced no enumeration at all). The
/// loader's stale reply word - a stray "OKAY" prefixed to this boot's first
/// response - is cosmetic and tolerable.
fn init_ep1(log: fn(&str, u64)) {
    // The loader served this same bulk pair all night, so its DIEPCTL1 is
    // the ground truth for the core's per-EP state: keep its TX FIFO slot
    // (bits 25:22) and log the FIFO sizes for the panel strip.
    let cur = rd(DIEPCTL1);
    let lk_txnum = (cur >> 22) & 0xf;
    let mut scan = 0u32;
    let mut best = u32::MAX;
    for i in 1..16u32 {
        let depth = (rd(dptxfsiz_n(i)) >> 16) * 4;
        if depth >= BULK_MPS && depth < best {
            best = depth;
            scan = i;
        }
    }
    unsafe {
        EP1_TX_IDX = scan;
    }
    log("ud: dptxfsiz1=", rd(DPTXFSIZ1) as u64);
    log("ud: txnum=", lk_txnum as u64);
    log("ud: scanslot=", scan as u64);
    // The type field reads back as CONTROL (0b00) unless set, and the core
    // stalls every transaction on a CONTROL-typed bulk endpoint. Match
    // s3c_hsotg_ep_enable (gadget.c:2626-2667): EPTYPE_BULK + MPS + USBACTEP
    // + SETD0PID + the loader's FIFO slot, then SNAK until armed.
    let in_ctl = (cur & !(EPCTL_EPTYPE_MASK | EPCTL_MPS_MASK | EPCTL_TXFNUM_MASK))
        | EPCTL_EPTYPE_BULK
        | BULK_MPS
        | (lk_txnum << 22)
        | EPCTL_USBACTEP
        | EPCTL_SETD0PID
        | EPCTL_SNAK;
    wr(DIEPCTL1, in_ctl);
    let out_ctl = (rd(DOEPCTL1) & !(EPCTL_EPTYPE_MASK | EPCTL_MPS_MASK | EPCTL_TXFNUM_MASK))
        | EPCTL_EPTYPE_BULK
        | BULK_MPS
        | EPCTL_USBACTEP
        | EPCTL_SETD0PID
        | EPCTL_SNAK;
    wr(DOEPCTL1, out_ctl);
    // Arm the first receive: size register first, then CNAK + EPENA.
    wr(DOEPTSIZ1, (1 << 19) | BULK_MPS);
    wr(DOEPCTL1, rd(DOEPCTL1) | EPCTL_CNAK | EPCTL_EPENA);
    // Latch EP1's interrupts too: DAINT bits 1 (EP1 IN) and 17 (EP1 OUT).
    wr(DAINTMSK, (1 << 0) | (1 << 1) | (1 << 16) | (1 << 17));
    log("ud: ep1ctl=", rd(DOEPCTL1) as u64);
}

/// The EP1 bulk interrupt service: dispatch finished commands and re-arm
/// the OUT receive.
fn service_ep1(log: fn(&str, u64)) {
    let diep = rd(DIEPINT1);
    if diep & EPINT_XFERCOMPL != 0 {
        wr(DIEPINT1, EPINT_XFERCOMPL);
        // The line just went out; the stream may send the next one.
        unsafe {
            core::ptr::addr_of_mut!(EP1_IN_IDLE).write(true);
        }
        // v222: the IN completion is the vendor's refill moment (gadget.c
        // feeds the remainder of an armed transfer from its completion
        // path); advance the bulk stream here too.
        #[cfg(fullerene_aarch64_ane)]
        xfer_pump();
    }
    let doep = rd(DOEPINT1);
    if doep & EPINT_XFERCOMPL != 0 {
        wr(DOEPINT1, EPINT_XFERCOMPL);
        // The OUT transfer completed: the fastboot command's end.
        fb_poll(log);
        // Ready for the next command chunk.
        wr(DOEPTSIZ1, (1 << 19) | BULK_MPS);
        wr(
            DOEPCTL1,
            rd(DOEPCTL1) | EPCTL_CNAK | EPCTL_EPENA | EPCTL_USBACTEP,
        );
    }
}

/// The endpoint-0 interrupt service (after the RX stream).
fn service_ep0(log: fn(&str, u64)) {
    let diep = rd(DIEPINT0);
    if diep & EPINT_XFERCOMPL != 0 {
        wr(DIEPINT0, EPINT_XFERCOMPL);
        if unsafe { IN_WAS_DATA } {
            // A data phase finished: the host's status OUT comes next.
            ep0_status_out();
            log("ud: in data done=", 1);
        } else {
            // This IN was itself the zero-length status: transfer done.
            unsafe {
                TRANSACTION_BUSY = false;
                SETUP_HANDLED = false;
            }
            log("ud: in zlp done=", 1);
        }
    }
    if diep & EPINT_SETUP != 0 {
        wr(DIEPINT0, EPINT_SETUP);
    }
    if diep & !(EPINT_XFERCOMPL | EPINT_SETUP) != 0 {
        wr(DIEPINT0, diep);
    }
    let doep = rd(DOEPINT0);
    if doep & EPINT_XFERCOMPL != 0 {
        wr(DOEPINT0, EPINT_XFERCOMPL);
        // The status OUT completed: the control transfer is done. Open the
        // next setup's handling window BEFORE the SETUP branch below: when
        // a new setup's interrupt latches in the same service pass as this
        // completion, the handler must see the fresh window, or the setup
        // bit would be consumed once and never answered.
        unsafe {
            TRANSACTION_BUSY = false;
            SETUP_HANDLED = false;
        }
        log("ud: out done=", 1);
    }
    if doep & EPINT_SETUP != 0 {
        wr(DOEPINT0, EPINT_SETUP);
        // The NAK is cleared only now so the core ACKs the setup it just
        // stored (dwc_otg: "cnak will be set after setup interrupt").
        wr(DOEPCTL0, rd(DOEPCTL0) | EPCTL_CNAK);
        // Belt for an interrupt-first ordering: if this setup's bytes are
        // already in (the RX pop normally answers first and marks HANDLED),
        // answer now. Never with stale bytes - the guard needs them present.
        if unsafe { SETUP_BYTES_READY && !SETUP_HANDLED } {
            unsafe { SETUP_HANDLED = true };
            handle_setup(log);
        }
    }
    if doep & !(EPINT_XFERCOMPL | EPINT_SETUP) != 0 {
        wr(DOEPINT0, doep);
    }
}

/// Bring the device side up: interrupt masks, EP0 setup arming.
pub fn init(log: fn(&str, u64)) {
    // The loader hands off a warm USB/DRAM image. Do not let the previous
    // session's SET_CONFIGURATION state suppress the first boot's recovery
    // gate when the handoff region was not zeroed by the loader.
    unsafe {
        core::ptr::addr_of_mut!(CONFIGURED).write(false);
        core::ptr::addr_of_mut!(CONFIG_VALUE).write(0);
        core::ptr::addr_of_mut!(CONFIG_WAS_1).write(false);
    }
    // Capture the loader's EP registers before any of our writes: its own
    // fastboot served the same bulk pair, so the snapshot (shown by the
    // panel strip) reveals the FIFO layout it used.
    unsafe {
        let snap = core::ptr::addr_of_mut!(LK_SNAP) as *mut u32;
        snap.add(0).write(rd(DIEPCTL1));
        snap.add(1).write(rd(DOEPCTL1));
        snap.add(2).write(rd(DPTXFSIZ1));
        snap.add(3).write(rd(GNPTXFSIZ));
        snap.add(4).write(rd(GRXFSIZ));
        snap.add(5).write(rd(DAINTMSK));
    }
    // Keep the LK's PHY/core basics (they already attached). Only the
    // device-side interrupt plumbing and the EP0 arm are ours.
    log("ud: init core=", USB_CORE_BASE as u64);
    log("ud: snpsid=", rd(GSNPSID) as u64);
    log("ud: dcfg=", rd(DCFG) as u64);
    log("ud: dctl=", rd(DCTL) as u64);

    // Interrupt masks: per-endpoint then the global summary.
    wr(DAINTMSK, (1 << 0) | (1 << 16)); // EP0 IN and OUT
    wr(DOEPMSK, EPINT_XFERCOMPL | EPINT_SETUP | (1 << 1) | (1 << 2));
    wr(DIEPMSK, EPINT_XFERCOMPL | (1 << 1) | (1 << 2) | (1 << 3));
    wr(
        GINTMSK,
        GINT_USBRST | GINT_ENUMDONE | GINT_IEPINT | GINT_OEPINT | GINT_RXFLVL,
    );

    // v264: the controller's GLOBAL interrupt output (GAHBCFG.GINT, bit 0).
    // The per-endpoint/GINTMSK masks are inert to the GIC while this bit is
    // clear - the loader-owned value is not assumed. Only the event-driven
    // build touches it, so the baseline stays byte-for-byte on the proven
    // polling shape.
    if option_env!("FULLERENE_ANE_USB_SPI").is_some() {
        wr(GAHBCFG, rd(GAHBCFG) | 1);
        log("ud: gahbcfg=", rd(GAHBCFG) as u64);
    }

    // Clear any latched interrupts from the LK's session.
    wr(GINTSTS, rd(GINTSTS));
    wr(DOEPINT0, 0xffff_ffff);
    wr(DIEPINT0, 0xffff_ffff);

    // EP0: 64-byte packets and the first SETUP receive armed.
    wr(DOEPCTL0, rd(DOEPCTL0) | EPCTL_USBACTEP);
    wr(DIEPCTL0, rd(DIEPCTL0) | EPCTL_USBACTEP);
    arm_setup_receive();
    // EP1: the fastboot bulk pair (commands in, responses out).
    init_ep1(log);
    // Note: an init-time sweep of the stale TX FIFO (v32: arming a
    // sacrificial transfer sized from DTXFSTS1) wedged the core - the
    // loader's inherited state must not be re-armed before a session
    // exists. The stale bytes are consumed host-side instead: the flash
    // chain spends one sacrificial pyusb exchange after boot, so the first
    // real client sees clean replies.
    log("ud: armed=", 1);
}

/// Write one 8-digit uppercase hex value into the durable ring (markers).
fn ring_hex8(mut v: u32) {
    let mut buf = [b'0'; 8];
    for i in (0..8).rev() {
        let d = (v & 0xf) as u8;
        buf[i] = if d < 10 { b'0' + d } else { b'A' + d - 10 };
        v >>= 4;
    }
    for b in buf {
        crate::blackbox::text_byte(b);
    }
}

/// v198 forensic: one short ring line per answered control request, from
/// any context (bounded by the request count; ~20 bytes each). Format:
/// `ud: q=BBWWWWWW LLLL` = bRequest, wValue, wLength. Readable from the
/// EP1 ring pull on clean boots and from the eMMC slice persistence on
/// garbled ones.
fn trace_request(breq: u8, wvalue: u16, wlength: u16) {
    crate::blackbox::text_puts("ud: q=");
    ring_hex8(((breq as u32) << 24) | ((wvalue as u32) << 8));
    crate::blackbox::text_byte(b' ');
    ring_hex8(wlength as u32);
    crate::blackbox::text_byte(b'\n');
}

/// Durable-ring EP0 state dump (v159): GINTSTS, DOEPCTL0, the latch nibble
/// (ARMED | BUSY | HANDLED | BYTES_READY) and DOEPTSIZ0 as compact hex,
/// emitted by the service pass every ~2 s so a photo names a wedged state
/// without any USB access. Runs on whichever context services; its ring
/// bytes are excluded from the progress heartbeat because it speaks only
/// from IRQ context in the stall case.
fn ep0_state_line() {
    // v256b: the UD8 phases named ring-write phases as the saturating ones.
    // Time the actual write funnel: t before/after one text_byte, worst-case
    // kept across passes, packed into the line's trailing field.
    let t0 = crate::timer::uptime_us();
    fn hex8(mut v: u32) {
        let mut buf = [b'0'; 8];
        for i in (0..8).rev() {
            let d = (v & 0xf) as u8;
            buf[i] = if d < 10 { b'0' + d } else { b'A' + d - 10 };
            v >>= 4;
        }
        for b in buf {
            crate::blackbox::text_byte(b);
        }
    }
    crate::blackbox::text_puts("UD8:");
    hex8(rd(GINTSTS));
    crate::blackbox::text_byte(b',');
    hex8(rd(DOEPCTL0));
    crate::blackbox::text_byte(b',');
    let packed = (unsafe { core::ptr::addr_of!(SETUP_ARMED).read() } as u32)
        | ((unsafe { core::ptr::addr_of!(TRANSACTION_BUSY).read() } as u32) << 1)
        | ((unsafe { core::ptr::addr_of!(SETUP_HANDLED).read() } as u32) << 2)
        | ((unsafe { core::ptr::addr_of!(SETUP_BYTES_READY).read() } as u32) << 3);
    hex8(packed);
    crate::blackbox::text_byte(b',');
    hex8(rd(DOEPTSIZ0));
    // v252b: worst service-phase costs, two halves. hi = p1<<48|p2<<32|p3<<16|p4
    // (entry, reset, enumdone, pop_rx); lo = p5<<48|p6<<32|p7<<16
    // (ep0, ep1, tail). 16 bits each, microseconds.
    let (hi, lo) = pass_cost();
    crate::blackbox::text_puts(" PC=");
    hex8(hi);
    crate::blackbox::text_byte(b',');
    hex8(lo);
    // v256b: worst single text_byte duration seen since boot (16 bits, us).
    let t1 = crate::timer::uptime_us();
    crate::blackbox::text_byte(b'X');
    let t2 = crate::timer::uptime_us();
    let dt = (t2 - t1).min(0xffff) as u32;
    {
        static WORST_TB: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        let prev = WORST_TB.load(core::sync::atomic::Ordering::Relaxed);
        if dt > prev {
            WORST_TB.store(dt, core::sync::atomic::Ordering::Relaxed);
        }
        crate::blackbox::text_puts(" TB=");
        hex8(WORST_TB.load(core::sync::atomic::Ordering::Relaxed));
        let _ = t0;
    }
    crate::blackbox::text_puts("\n");
}

/// One service pass over the device interrupts. Cheap enough for the wait
/// loop's polling cadence (the wait loop already wakes on the timer IRQ).
pub fn service(log: fn(&str, u64)) {
    use core::sync::atomic::{AtomicBool, Ordering};
    // Reentrancy guard (v158): the timer-IRQ pump now runs every ~16 ms, so
    // it can fire while the boot thread is inside its own service() call.
    // The pass is a state machine over shared endpoint arming, so a nested
    // pass would corrupt it. One pass at a time, on whichever context wins.
    static IN_SERVICE: AtomicBool = AtomicBool::new(false);
    if IN_SERVICE.swap(true, Ordering::Acquire) {
        return;
    }
    // A `download:` that stops receiving (client died mid-transfer) must
    // not eat every later command: abort it after ~15 s without a byte.
    if unsafe { core::ptr::addr_of!(DL_STATE).read() } == 1 {
        let now = crate::timer::uptime_us();
        let t0 = unsafe { core::ptr::addr_of!(DL_T0).read() };
        if now.saturating_sub(t0) > 15_000_000 {
            unsafe {
                core::ptr::addr_of_mut!(DL_STATE).write(0);
                core::ptr::addr_of_mut!(DL_REMAIN).write(0);
                core::ptr::addr_of_mut!(DL_LEN).write(0);
            }
            log("fb: dl_timeout=", 1);
        }
    }
    let sts = rd(GINTSTS) & (GINT_USBRST | GINT_ENUMDONE | GINT_RXFLVL | GINT_IEPINT | GINT_OEPINT);
    pass_enter(1);
    if sts != 0 {
        if sts & GINT_USBRST != 0 {
            wr(GINTSTS, GINT_USBRST);
            handle_reset(log);
        }
        pass_enter(2);
        if sts & GINT_ENUMDONE != 0 {
            wr(GINTSTS, GINT_ENUMDONE);
            log("ud: enumdone=", ((rd(DSTS) >> 1) & 3) as u64);
            // v186: from here on the USB block is up and safe to poll -
            // the DS instrument reads its registers only after this.
            unsafe {
                core::ptr::addr_of_mut!(USB_ENUMERATED).write(true);
            }
        }
        pass_enter(3);
        if sts & (GINT_RXFLVL | GINT_OEPINT | GINT_IEPINT) != 0 {
            pop_rx(log);
            pass_enter(4);
            service_ep0(log);
            pass_enter(5);
            service_ep1(log);
            pass_enter(6);
        }
    }
    // v252b: phase 1 now covers entry..GINTSTS; 2 = USBRST/handle_reset;
    // 3 = ENUMDONE; 4 = pop_rx; 5 = ep0; 6 = ep1; 7 = rearm..exit.
    pass_enter(7);
    // v220: advance the bulk xfer stream every pass (the vendor pattern:
    // the remaining bytes of an armed transfer are fed as FIFO space frees).
    #[cfg(fullerene_aarch64_ane)]
    xfer_pump();
    // The paced `oem log` stream ticks once per service pass, so the host's
    // reads never coalesce into unusable chunks.
    #[cfg(fullerene_aarch64_ane)]
    fb_log_step();
    // Re-arm the setup receive whenever no control transfer is in flight;
    // its previous arm was consumed by the SETUP that started the transfer.
    // v259 REVERTED: gating the re-arm on the SETUP_SEEN delta broke
    // enumeration (2/2 boots garbled "Љ" 27-byte descriptors) - the re-arm
    // one pump pass late leaves EP0 OUT NAKed across the host's next control
    // read. The every-pass re-arm is part of the proven v197 shape; keep it
    // and let the v257b worst-phase meter decide whether it is actually
    // expensive before touching it again.
    if unsafe { !SETUP_ARMED && !TRANSACTION_BUSY } {
        arm_setup_receive();
    }
    // The core wants SOF acknowledged when unmasked; it is masked off, but
    // clear it defensively so the summary never sticks.
    if rd(GINTSTS) & GINT_SOF != 0 {
        wr(GINTSTS, GINT_SOF);
    }
    // v159 EP0-state visibility: one ring line per ~2 s of service passes.
    // v191: the v162 every-tick pump multiplied the pass rate ~100x, so the
    // old 128 gate flooded the durable ring with UD8 lines (the first ring
    // recovery showed the tail almost entirely UD8 + newline padding, washing
    // out the boot log). 8192 restores the "rare diagnostic" intent.
    {
        use core::sync::atomic::{AtomicU32, Ordering};
        static PASSES: AtomicU32 = AtomicU32::new(0);
        if PASSES.fetch_add(1, Ordering::Relaxed) % 1024 == 0 {
            ep0_state_line();
        }
    }
    // v252: pass-cost attribution. The stage meter named the pump window
    // (S3..K, W=K 2.8 s) as the IRQ's cost center; this splits the cost
    // inside service() into pop/ep0/ep1/pump-tail so the next collection
    // names the exact callee instead of a blind A/B.
    {
        use core::sync::atomic::{AtomicU32, Ordering};
        let now = crate::timer::uptime_us();
        let t0 = PASS_T0.swap(now, Ordering::Relaxed);
        let phase = PASS_PHASE.load(Ordering::Relaxed);
        if phase != 0 {
            let slot = (phase - 1) as usize % PASS_US.len();
            let dt = now.saturating_sub(t0).min(u32::MAX as u64) as u32;
            let cur = PASS_US[slot].load(Ordering::Relaxed);
            if dt > cur {
                PASS_US[slot].store(dt, Ordering::Relaxed);
            }
        }
        PASS_PHASE.store(0, Ordering::Relaxed);
    }
    IN_SERVICE.store(false, Ordering::Release);
}

/// v252: worst-cost-per-service-phase. v252b phases: 1 = entry+GINTSTS read,
/// 2 = USBRST/handle_reset, 3 = ENUMDONE, 4 = pop_rx, 5 = service_ep0,
/// 6 = service_ep1, 7 = rearm/xfer/log tail. Read back by the UD8 line.
static PASS_US: [core::sync::atomic::AtomicU32; 8] =
    [const { core::sync::atomic::AtomicU32::new(0) }; 8];
static PASS_PHASE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static PASS_T0: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn pass_enter(phase: u32) {
    use core::sync::atomic::Ordering;
    let now = crate::timer::uptime_us();
    let t0 = PASS_T0.swap(now, Ordering::Relaxed);
    let prev = PASS_PHASE.load(Ordering::Relaxed);
    if prev != 0 {
        let slot = (prev - 1) as usize % PASS_US.len();
        let dt = now.saturating_sub(t0).min(u32::MAX as u64) as u32;
        let cur = PASS_US[slot].load(Ordering::Relaxed);
        if dt > cur {
            PASS_US[slot].store(dt, Ordering::Relaxed);
        }
    }
    PASS_PHASE.store(phase, Ordering::Relaxed);
}

/// v257b: the u16 packing clamped the real magnitudes (p3/p4/p7 all hit
/// 0xffff). Return instead the WORST phase packed as (letter << 24) |
/// 24-bit us; the lo half is reserved 0.
pub fn pass_cost() -> (u32, u32) {
    use core::sync::atomic::Ordering;
    let g = |i: usize| PASS_US[i].load(Ordering::Relaxed);
    let mut worst_i = 0usize;
    let mut worst_us = 0u32;
    for i in 0..7 {
        let v = g(i);
        if v > worst_us {
            worst_us = v;
            worst_i = i;
        }
    }
    let packed: u32 = (((b'A' + worst_i as u8) as u32) << 24) | (worst_us & 0x00ff_ffff);
    (packed, 0)
}
