//! The ADB wire, device side.
//!
//! Why this exists: the gadget already advertises an ADB-class interface (class `0xff`, subclass
//! `0x42`, protocol `0x01`, one bulk IN/OUT pair - see `CONFIG_DESCRIPTOR` and the test that
//! asserts those three bytes), and it already has a bulk data path (`debug_transport.rs`, which
//! speaks the legacy `FDBG`/`FDRP` framing). What is missing is the ADB protocol itself, so a host
//! running `adb` - or this repository's own `flasks adb` client - would connect to an interface
//! that never answers `CNXN`.
//!
//! Scope, stated honestly: this module is the *framing*, the pure part. It is written and tested
//! now because it does not depend on the thing that is broken. Nothing here can be reached on the
//! device until control-endpoint enumeration completes - the host will not send an ADB packet to a
//! device it has not finished enumerating, and today it stops at `device descriptor read/64,
//! error -71`. So implementing ADB does not unblock "adb cannot read it" on its own; it is what
//! makes that phrase solve itself *once* enumeration works.
//!
//! Wire format (24-byte little-endian header, then the payload):
//!
//! ```text
//! offset  size  field
//!      0     4  command   (e.g. b"CNXN", b"OKAY", b"WRTE")
//!      4     4  arg0      (service-specific: local id, remote id, ...)
//!      8     4  arg1
//!     12     4  data_length
//!     16     4  data_checksum  (sum of the payload bytes)
//!     20     4  magic_magic   (command ^ 0xffff_ffff)
//!     24     n  payload
//! ```
//!
//! A decoder that ignores the magic would happily accept a `CNXN` that is not one, and the
//! checksum is what catches a payload that was truncated in the middle - both are checked here
//! rather than trusted.

pub const HEADER_BYTES: usize = 24;
pub const VERSION: u32 = 0x0100_0001;
pub const MAX_PAYLOAD: usize = 4096;

pub const CNXN: u32 = u32::from_le_bytes(*b"CNXN");
pub const AUTH: u32 = u32::from_le_bytes(*b"AUTH");
pub const OPEN: u32 = u32::from_le_bytes(*b"OPEN");
pub const OKAY: u32 = u32::from_le_bytes(*b"OKAY");
pub const CLSE: u32 = u32::from_le_bytes(*b"CLSE");
pub const WRTE: u32 = u32::from_le_bytes(*b"WRTE");

/// Shell protocol v2 stream ids, used when a host opens `shell,v2:...`.
pub const SHELL_STDOUT: u8 = 1;
pub const SHELL_EXIT: u8 = 3;

/// A decoded ADB packet header with the payload left in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    pub command: u32,
    pub arg0: u32,
    pub arg1: u32,
    pub payload: &'a [u8],
}

fn checksum(payload: &[u8]) -> u32 {
    payload
        .iter()
        .fold(0u32, |sum, byte| sum.wrapping_add(*byte as u32))
}

/// Why a decode failed. Named rather than a bare `None` so a run can tell a short read from a
/// corrupted header - the two have different causes and different fixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer bytes than a header, or fewer than the header claims.
    Short,
    /// `magic_magic` is not `command ^ 0xffff_ffff`.
    BadMagic,
    /// The payload does not sum to `data_checksum`.
    BadChecksum,
    /// `data_length` exceeds what this transport will accept.
    TooLarge,
}

/// Decode one packet. The payload slice borrows `input`, so callers must keep it alive.
pub fn decode(input: &[u8]) -> Result<Packet<'_>, DecodeError> {
    if input.len() < HEADER_BYTES {
        return Err(DecodeError::Short);
    }
    let word = |offset: usize| -> u32 {
        u32::from_le_bytes([
            input[offset],
            input[offset + 1],
            input[offset + 2],
            input[offset + 3],
        ])
    };
    let command = word(0);
    let arg0 = word(4);
    let arg1 = word(8);
    let length = word(12) as usize;
    let data_checksum = word(16);
    let magic = word(20);
    if magic != command ^ 0xffff_ffff {
        return Err(DecodeError::BadMagic);
    }
    if length > MAX_PAYLOAD {
        return Err(DecodeError::TooLarge);
    }
    if input.len() < HEADER_BYTES + length {
        return Err(DecodeError::Short);
    }
    let payload = &input[HEADER_BYTES..HEADER_BYTES + length];
    if checksum(payload) != data_checksum {
        return Err(DecodeError::BadChecksum);
    }
    Ok(Packet {
        command,
        arg0,
        arg1,
        payload,
    })
}

/// Encode one packet into `out`, returning the number of bytes written, or `None` when `out` is
/// too small. The caller owns the buffer because this runs on the bulk path, where allocating is
/// not an option.
pub fn encode(command: u32, arg0: u32, arg1: u32, payload: &[u8], out: &mut [u8]) -> Option<usize> {
    if payload.len() > MAX_PAYLOAD || out.len() < HEADER_BYTES + payload.len() {
        return None;
    }
    out[0..4].copy_from_slice(&command.to_le_bytes());
    out[4..8].copy_from_slice(&arg0.to_le_bytes());
    out[8..12].copy_from_slice(&arg1.to_le_bytes());
    out[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    out[16..20].copy_from_slice(&checksum(payload).to_le_bytes());
    out[20..24].copy_from_slice(&(command ^ 0xffff_ffff).to_le_bytes());
    out[HEADER_BYTES..HEADER_BYTES + payload.len()].copy_from_slice(payload);
    Some(HEADER_BYTES + payload.len())
}

/// The reply to a host's `CNXN`: the device announces its version and its banner.
///
/// The banner is where the black box belongs. `adb` prints it in `adb devices -l` output, so even
/// a host that only enumerates and connects gets a one-line summary of the last recorded boot
/// without asking for anything - which is the whole point of having a channel at all.
pub fn cnxn_reply(version: u32, max_payload: u32, banner: &[u8], out: &mut [u8]) -> Option<usize> {
    // The banner must be NUL-terminated on the wire, as the host's parser expects it.
    let mut terminated = [0u8; 512];
    if banner.len() + 1 > terminated.len() {
        return None;
    }
    terminated[..banner.len()].copy_from_slice(banner);
    encode(
        CNXN,
        version,
        max_payload,
        &terminated[..banner.len() + 1],
        out,
    )
}

/// The payload of a shell-v2 stdout chunk: a one-byte stream id, then the bytes.
pub fn shell_v2_chunk(stream: u8, bytes: &[u8], out: &mut [u8]) -> Option<usize> {
    if bytes.len() + 1 > out.len() {
        return None;
    }
    out[0] = stream;
    out[1..1 + bytes.len()].copy_from_slice(bytes);
    Some(1 + bytes.len())
}

#[cfg(test)]
mod tests {
    //! The framing only: these run with `cargo test -p fullerene-kernel` and they are the reason
    //! this module is worth writing before enumeration works - a protocol that cannot be exercised
    //! on the device can still be pinned down off it.

    use super::*;

    #[test]
    fn round_trip_preserves_every_field() {
        let payload = b"host::features=shell_v2";
        let mut out = [0u8; 128];
        let written = encode(CNXN, VERSION, 4096, payload, &mut out).expect("fits");
        assert_eq!(written, HEADER_BYTES + payload.len());
        let packet = decode(&out[..written]).expect("decodes");
        assert_eq!(packet.command, CNXN);
        assert_eq!(packet.arg0, VERSION);
        assert_eq!(packet.arg1, 4096);
        assert_eq!(packet.payload, payload);
    }

    #[test]
    fn the_magic_is_checked() {
        let mut out = [0u8; 64];
        let written = encode(CNXN, 1, 2, b"", &mut out).expect("fits");
        // Corrupt only the magic: without that check this would decode as a valid CNXN.
        out[20] ^= 0xff;
        assert_eq!(decode(&out[..written]), Err(DecodeError::BadMagic));
    }

    #[test]
    fn a_corrupted_payload_is_rejected() {
        let mut out = [0u8; 64];
        let written = encode(WRTE, 1, 2, b"abc", &mut out).expect("fits");
        out[HEADER_BYTES] = b'z';
        assert_eq!(decode(&out[..written]), Err(DecodeError::BadChecksum));
    }

    #[test]
    fn a_truncated_packet_is_short_not_accepted() {
        let mut out = [0u8; 64];
        let written = encode(WRTE, 1, 2, b"abcdef", &mut out).expect("fits");
        assert_eq!(decode(&out[..written - 1]), Err(DecodeError::Short));
        assert_eq!(decode(&out[..HEADER_BYTES - 1]), Err(DecodeError::Short));
    }

    #[test]
    fn an_oversized_claim_is_rejected_before_it_is_believed() {
        let mut out = [0u8; 64];
        let written = encode(WRTE, 1, 2, b"", &mut out).expect("fits");
        out[12..16].copy_from_slice(&(MAX_PAYLOAD as u32 + 1).to_le_bytes());
        out[16..20].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode(&out[..written]), Err(DecodeError::TooLarge));
    }

    #[test]
    fn the_cnxn_reply_carries_a_terminated_banner() {
        let mut out = [0u8; 640];
        let written = cnxn_reply(
            VERSION,
            4096,
            b"fullerene-blackbox: boot 7, 12 records",
            &mut out,
        )
        .expect("fits");
        let packet = decode(&out[..written]).expect("decodes");
        assert_eq!(packet.command, CNXN);
        assert_eq!(packet.arg0, VERSION);
        assert_eq!(
            *packet.payload.last().expect("non-empty"),
            0,
            "NUL terminated"
        );
        assert!(packet.payload.starts_with(b"fullerene-blackbox"));
    }

    #[test]
    fn a_shell_chunk_prefixes_the_stream_id() {
        let mut out = [0u8; 32];
        let written = shell_v2_chunk(SHELL_STDOUT, b"hello", &mut out).expect("fits");
        assert_eq!(&out[..written], b"\x01hello");
    }
}
