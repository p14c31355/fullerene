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
