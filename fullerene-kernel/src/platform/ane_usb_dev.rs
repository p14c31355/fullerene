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

use super::ane::USB_CORE_BASE;

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
const EPCTL_STALL: u32 = 1 << 21;
const EPCTL_USBACTEP: u32 = 1 << 15;

// ---- DXEPINT bits ----------------------------------------------------------

const EPINT_XFERCOMPL: u32 = 1 << 0;
const EPINT_SETUP: u32 = 1 << 3;

// ---- GRXSTSP fields (hw.h) -------------------------------------------------

const RXSTS_EPNUM_MASK: u32 = 0xf;
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
    64,                  // bMaxPacketSize0
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
/// True from the moment a SETUP was answered until its control transfer
/// finishes (the data phase plus the status phase). The auto re-arm of the
/// SETUP receive must not fire mid-transaction.
static mut TRANSACTION_BUSY: bool = false;
/// True when the last queued IN phase carried data (so a status OUT still
/// follows) - false when it was itself the zero-length status (no follow-up).
static mut IN_WAS_DATA: bool = false;

fn rd(off: usize) -> u32 {
    unsafe { ptr::read_volatile((USB_CORE_BASE + off) as *const u32) }
}

fn wr(off: usize, value: u32) {
    unsafe { ptr::write_volatile((USB_CORE_BASE + off) as *mut u32, value) }
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
    unsafe { SETUP_ARMED = true };
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
    wr(DOEPCTL0, rd(DOEPCTL0) | EPCTL_EPENA | EPCTL_CNAK | EPCTL_USBACTEP);
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
    wr(DIEPCTL0, rd(DIEPCTL0) | EPCTL_EPENA | EPCTL_CNAK | EPCTL_USBACTEP);
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
    wr(DCFG, rd(DCFG) & !DCFG_DEVADDR_MASK);
    // Clear any stale endpoint interrupts before re-arming.
    wr(DOEPINT0, 0xffff_ffff);
    wr(DIEPINT0, 0xffff_ffff);
    arm_setup_receive();
}

/// Pop and act on every RX FIFO status entry (the packet stream).
fn pop_rx(log: fn(&str, u64)) {
    while rd(GINTSTS) & GINT_RXFLVL != 0 {
        let s = rd(GRXSTSP);
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
                }
                log("ud: setupbcnt=", bcnt as u64);
            }
            PKTSTS_SETUPDONE => {
                handle_setup(log);
            }
            PKTSTS_OUTRX | PKTSTS_OUTDONE => {
                // Data/status OUT bytes: drain so the FIFO does not wedge.
                let mut n = bcnt;
                while n > 0 {
                    let _ = fifo_pop_word();
                    n = n.saturating_sub(4);
                }
            }
            _ => {}
        }
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
            unsafe { TRANSACTION_BUSY = false };
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
    if doep & EPINT_SETUP != 0 {
        wr(DOEPINT0, EPINT_SETUP);
        // The NAK is cleared only now so the core ACKs the setup it just
        // stored (dwc_otg: "cnak will be set after setup interrupt").
        wr(DOEPCTL0, rd(DOEPCTL0) | EPCTL_CNAK);
    }
    if doep & EPINT_XFERCOMPL != 0 {
        wr(DOEPINT0, EPINT_XFERCOMPL);
        // The status OUT completed: the control transfer is done.
        unsafe { TRANSACTION_BUSY = false };
        log("ud: out done=", 1);
    }
    if doep & !(EPINT_XFERCOMPL | EPINT_SETUP) != 0 {
        wr(DOEPINT0, doep);
    }
}

/// Bring the device side up: interrupt masks, EP0 setup arming.
pub fn init(log: fn(&str, u64)) {
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
    wr(GINTMSK, GINT_USBRST | GINT_ENUMDONE | GINT_IEPINT | GINT_OEPINT | GINT_RXFLVL);

    // Clear any latched interrupts from the LK's session.
    wr(GINTSTS, rd(GINTSTS));
    wr(DOEPINT0, 0xffff_ffff);
    wr(DIEPINT0, 0xffff_ffff);

    // EP0: 64-byte packets and the first SETUP receive armed.
    wr(DOEPCTL0, rd(DOEPCTL0) | EPCTL_USBACTEP);
    wr(DIEPCTL0, rd(DIEPCTL0) | EPCTL_USBACTEP);
    arm_setup_receive();
    log("ud: armed=", 1);
}

/// One service pass over the device interrupts. Cheap enough for the wait
/// loop's polling cadence (the wait loop already wakes on the timer IRQ).
pub fn service(log: fn(&str, u64)) {
    let sts = rd(GINTSTS)
        & (GINT_USBRST | GINT_ENUMDONE | GINT_RXFLVL | GINT_IEPINT | GINT_OEPINT);
    if sts == 0 {
        return;
    }
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
    }
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
