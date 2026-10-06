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

const CFG_DESC: [u8; 32] = [
    // configuration
    9, 0x02, 32, 0x00, 1, 1, 0, 0x80, 50, // bus powered, 100 mA
    // interface: fastboot (class 0xff, subclass 0x42, protocol 0x03)
    9, 0x04, 0, 0, 2, 0xff, 0x42, 0x03, 0, // endpoint IN, bulk, 512
    7, 0x05, 0x81, 0x02, 0x00, 0x02, 0, // endpoint OUT, bulk, 512
    7, 0x05, 0x01, 0x02, 0x00, 0x02, 0,
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
/// The paced `oem log` stream: one INFO line per service pass.
static mut LOG_ACTIVE: bool = false;
static mut LOG_CURSOR: usize = 0;
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
        }
    }
}

fn rd(off: usize) -> u32 {
    super::common::mmio::rd32(USB_CORE_BASE, off)
}

fn wr(off: usize, value: u32) {
    super::common::mmio::wr32(USB_CORE_BASE, off, value);
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

/// Queue an IN data phase on EP0, then push the bytes into the FIFO.
///
/// Order matters (gadget.c `s3c_hsotg_write_fifo`): the transfer size and
/// EPENA are programmed first, only then does the payload enter the FIFO.
fn ep0_send(data: &[u8]) {
    let n = data.len();
    unsafe {
        TRANSACTION_BUSY = true;
        IN_WAS_DATA = n > 0;
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
        }
        0x05 => {
            // SET_ADDRESS
            let addr = (wvalue as u32) & 0x7f;
            wr(DCFG, (rd(DCFG) & !DCFG_DEVADDR_MASK) | (addr << 4));
            ep0_zlp_in();
        }
        0x09 => {
            // SET_CONFIGURATION
            unsafe { CONFIGURED = wvalue != 0 };
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

/// Reset handling: back to address 0 and re-arm the SETUP receive.
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
                log("ud: setupbcnt=", bcnt as u64);
                // Answer the moment the bytes exist. The vendor computes its
                // answer around the SETUP interrupt (dwc_otg handle_ep0 ->
                // pcd_setup, fed by the RX path's stored packet); doing it
                // here keeps no dependency on any later status word.
                if unsafe { !SETUP_HANDLED } {
                    unsafe { SETUP_HANDLED = true };
                    handle_setup(log);
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

/// Send `data` on the EP1 bulk IN endpoint (the fastboot response path).
fn ep1_send(data: &[u8]) {
    let n = data.len();
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
            let free = ((rd(DTXFSTS1) & 0xffff) as usize) * 4;
            let depth = ((rd(DPTXFSIZ1) >> 16) as usize) * 4;
            let stale = depth.saturating_sub(free);
            if stale > 0 && stale < 0x7_0000 {
                padded += stale;
            }
        }
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
    let mut i = 0;
    while i < n {
        let mut word = 0u32;
        for j in 0..4 {
            if i + j < n {
                word |= (data[i + j] as u32) << (8 * j);
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
/// ANE's durable-log zone (`blackbox::TEXT_BLOCK_*` is cfg'd the same way).
#[cfg(fullerene_aarch64_ane)]
fn main_log_len() -> usize {
    let raw = unsafe { ptr::read_volatile((crate::blackbox::TEXT_BLOCK_BASE + 4) as *const u32) }
        as usize;
    let cap = crate::blackbox::TEXT_BLOCK_CAP as usize;
    if raw < cap { raw } else { 0 }
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
    if cmd == b"oem log" || cmd == b"oem log full" || cmd.starts_with(b"oem log main") {
        // The durable log as INFO lines, one per service pass. Plain
        // `oem log` streams the last stretch of the fixed copy; `oem log
        // full` the whole fixed copy (7 KiB - the boot's first ~200 lines
        // fill it on this platform). `oem log main [hexoff]` streams the
        // main text block (cap 0xE000) where the later sections (usb /
        // sd / emmc) actually live.
        let mut main = false;
        let mut written = crate::blackbox::fixed_log_len();
        let mut start = 0usize;
        if cmd.starts_with(b"oem log main") {
            main = true;
            written = main_log_len();
            let rest = &cmd[b"oem log main".len()..];
            let off = parse_hex(rest.strip_prefix(b" ").unwrap_or(rest));
            start = off.min(written);
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

/// One service pass over the device interrupts. Cheap enough for the wait
/// loop's polling cadence (the wait loop already wakes on the timer IRQ).
pub fn service(log: fn(&str, u64)) {
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
    if sts != 0 {
        if sts & GINT_USBRST != 0 {
            wr(GINTSTS, GINT_USBRST);
            handle_reset(log);
        }
        if sts & GINT_ENUMDONE != 0 {
            wr(GINTSTS, GINT_ENUMDONE);
            log("ud: enumdone=", ((rd(DSTS) >> 1) & 3) as u64);
        }
        if sts & (GINT_RXFLVL | GINT_OEPINT | GINT_IEPINT) != 0 {
            pop_rx(log);
            service_ep0(log);
            service_ep1(log);
        }
    }
    // The paced `oem log` stream ticks once per service pass, so the host's
    // reads never coalesce into unusable chunks.
    #[cfg(fullerene_aarch64_ane)]
    fb_log_step();
    // Re-arm the setup receive whenever no control transfer is in flight;
    // its previous arm was consumed by the SETUP that started the transfer.
    if unsafe { !SETUP_ARMED && !TRANSACTION_BUSY } {
        arm_setup_receive();
    }
    // The core wants SOF acknowledged when unmasked; it is masked off, but
    // clear it defensively so the summary never sticks.
    if rd(GINTSTS) & GINT_SOF != 0 {
        wr(GINTSTS, GINT_SOF);
    }
}
