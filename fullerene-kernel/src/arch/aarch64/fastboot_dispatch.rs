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
            if let VarReply::Static(reply) = entry.reply {
                io.bulk_send(reply);
                return true;
            }
        }
    }
    io.bulk_send(b"FAILunknown variable");
    true
}
