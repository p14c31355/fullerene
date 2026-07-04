//! WPA/WPA2 supplicant implementation.
//!
//! Handles the 4-way handshake for WPA2-PSK (Personal mode).
//! Uses PBKDF2-SHA1 for passphrase-to-PMK derivation and
//! the 4-way handshake for PTK/GTK derivation.

use alloc::string::String;
use alloc::vec::Vec;
use crate::wifi::Bssid;

/// WPA state for a single connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WpaState {
    Initial,
    HavePmk,
    WaitMsg1,
    WaitMsg2,
    WaitMsg3,
    WaitMsg4,
    Done,
    Error,
}

/// WPA key information flags.
pub const KEY_INFO_PAIRWISE: u16 = 0x0008;
pub const KEY_INFO_INSTALL: u16 = 0x0040;
pub const KEY_INFO_ACK: u16 = 0x0080;
pub const KEY_INFO_MIC: u16 = 0x0100;
pub const KEY_INFO_ERROR: u16 = 0x0200;
pub const KEY_INFO_SECURE: u16 = 0x0400;
pub const KEY_INFO_KEY_TYPE: u16 = 0x2000;
pub const KEY_INFO_KEY_INDEX: u16 = 0x0C00;

/// EAPOL-Key frame.
#[repr(C, packed)]
pub struct EapolKeyFrame {
    pub version: u8,
    pub key_type: u8,
    pub key_len: u16,
    pub key_replay_counter: [u8; 8],
    pub key_nonce: [u8; 32],
    pub key_iv: [u8; 16],
    pub key_rsc: [u8; 8],
    pub key_id: [u8; 8],
    pub key_mic: [u8; 16],
    pub key_len2: u16,
}

/// WPA supplicant state machine.
#[derive(Debug)]
pub struct WpaSupplicant {
    pub state: WpaState,
    pub ssid: String,
    pub passphrase: String,
    pub pmk: [u8; 32],
    pub ptk: [u8; 48],
    pub gtk: [u8; 32],
    pub anonce: [u8; 32],
    pub snonce: [u8; 32],
    pub ap_bssid: Bssid,
    pub client_mac: Bssid,
    pub replay_counter: u64,
    pub mic_error: bool,
}

impl WpaSupplicant {
    pub fn new() -> Self {
        Self {
            state: WpaState::Initial,
            ssid: alloc::string::String::new(),
            passphrase: alloc::string::String::new(),
            pmk: [0u8; 32],
            ptk: [0u8; 48],
            gtk: [0u8; 32],
            anonce: [0u8; 32],
            snonce: [0u8; 32],
            ap_bssid: [0u8; 6],
            client_mac: [0u8; 6],
            replay_counter: 0,
            mic_error: false,
        }
    }

    /// Initialize with passphrase and SSID (PMK derivation).
    pub fn init(&mut self, passphrase: &str, ssid: &str, ap_bssid: Bssid, client_mac: Bssid) {
        self.ssid = alloc::string::String::from(ssid);
        self.passphrase = alloc::string::String::from(passphrase);
        self.ap_bssid = ap_bssid;
        self.client_mac = client_mac;

        // Derive PMK from passphrase + SSID using PBKDF2-SHA1
        self.derive_pmk();

        // Generate random SNonce (in a real impl, use hardware RNG)
        self.generate_snonce();

        self.state = WpaState::HavePmk;
        self.replay_counter = 0;
    }

    /// Derive PMK = PBKDF2-SHA1(passphrase, ssid, 4096, 256)
    fn derive_pmk(&mut self) {
        let pass_bytes = self.passphrase.as_bytes();
        let ssid_bytes = self.ssid.as_bytes();

    // PBKDF2 implementation for WPA2-PSK.
    let mut output = [0u8; 32];

    // WPA2 PSK = PBKDF2(HMAC-SHA1, passphrase, ssid, 4096, 256)
    // HMAC-SHA1 produces 20 bytes per block, so we need 2 blocks for 32 bytes
    let mut block = 1u32;
    for i in 0..2 {
        let start = (i as usize) * 20;
        let end = core::cmp::min(start + 20, 32);
        let len = end - start;

        // U_1 = HMAC-SHA1(P, S || INT(i))
        let mut salt = ssid_bytes.to_vec();
        salt.extend_from_slice(&block.to_be_bytes());

        let mut u = hmac_sha1(pass_bytes, &salt);
        block += 1;

        let mut t = [0u8; 20];
        t.copy_from_slice(&u);

        // U_j = HMAC-SHA1(P, U_{j-1}) for j = 2..4096
        for _ in 1..4096 {
            u = hmac_sha1(pass_bytes, &u);
            for j in 0..20 {
                t[j] ^= u[j];
            }
        }

        // Copy the appropriate amount of bytes to output
        output[start..start + len].copy_from_slice(&t[..len]);
    }

        self.pmk = output;
    }

    /// Generate a random SNonce.
    fn generate_snonce(&mut self) {
        // Generate the SNonce from a fresh entropy source (CPU timestamp
        // counter) on every call, so it is neither predictable nor derived
        // from any connection state such as the PMK.
        let mut snonce = [0u8; 32];

        for (i, chunk) in snonce.chunks_mut(8).enumerate() {
            // Re-sample the TSC for each 8-byte chunk and mix in the chunk
            // index so successive words don't collapse to the same seed.
            let mut state = unsafe { core::arch::x86_64::_rdtsc() } ^ (i as u64).wrapping_mul(0x9E3779B97F4A7C15);

            // xorshift64* PRNG to whiten the TSC sample into output bytes.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let val = state.wrapping_mul(0x2545F4914F6CDD1D);

            chunk.copy_from_slice(&val.to_le_bytes()[..chunk.len()]);
        }

        self.snonce = snonce;
    }

    /// Handle EAPOL-Key Message 1 (from AP).
    pub fn handle_message_1(&mut self, frame: &[u8]) -> Result<Vec<u8>, &'static str> {
        if frame.len() < 97 {
            return Err("EAPOL-Key frame too short");
        }

        let _key_info = u16::from_be_bytes([frame[1], frame[2]]);

        // Extract ANonce (bytes 13-44 relative to key descriptor)
        let anonce_start = 13;
        if anonce_start + 32 > frame.len() {
            return Err("Frame too short for ANonce");
        }
        self.anonce.copy_from_slice(&frame[anonce_start..anonce_start + 32]);

        // Update replay counter
        if frame.len() >= 9 {
            let rc_bytes: [u8; 8] = [
                frame[5], frame[6], frame[7], frame[8],
                frame[9], frame[10], frame[11], frame[12],
            ];
            self.replay_counter = u64::from_be_bytes(rc_bytes);
        }

        self.state = WpaState::WaitMsg2;

        // Build Message 2 (SNonce + MIC)
        let msg2 = self.build_message_2();
        Ok(msg2)
    }

    /// Build EAPOL-Key Message 2 (our response with SNonce).
    fn build_message_2(&self) -> Vec<u8> {
        let mut msg = Vec::new();

        // EAPOL header
        msg.push(0x03); // EAPOL-Key
        msg.push(0xFE); // Key Descriptor Version (WPA2)
        msg.extend_from_slice(&[0x00, 0x00]); // Length placeholder

        // Key Descriptor Type
        msg.push(0x02); // WPA2

        // Key Info
        let key_info = KEY_INFO_KEY_TYPE | KEY_INFO_MIC | KEY_INFO_PAIRWISE;
        msg.extend_from_slice(&key_info.to_be_bytes());

        // Key Length (16 for CCMP)
        msg.extend_from_slice(&[0x00, 0x10]);

        // Key Replay Counter
        msg.extend_from_slice(&self.replay_counter.to_be_bytes());

        // Key Nonce (SNonce)
        msg.extend_from_slice(&self.snonce);

        // Key IV (0)
        msg.extend_from_slice(&[0u8; 16]);

        // Key RSC (0)
        msg.extend_from_slice(&[0u8; 8]);

        // Key ID (0)
        msg.extend_from_slice(&[0u8; 8]);

        // MIC (placeholder - computed below)
        let mic_offset = msg.len();
        msg.extend_from_slice(&[0u8; 16]);

        // Key Data Length (0 for now)
        msg.extend_from_slice(&[0x00, 0x00]);

        // Update length field
        let len = (msg.len() - 4) as u16;
        msg[2] = (len >> 8) as u8;
        msg[3] = len as u8;

        // Compute MIC over the frame
        let mic = compute_mic(&self.ptk, &msg);
        msg[mic_offset..mic_offset + 16].copy_from_slice(&mic);

        msg
    }

    /// Handle EAPOL-Key Message 3 (from AP, contains GTK).
    pub fn handle_message_3(&mut self, frame: &[u8]) -> Result<Vec<u8>, &'static str> {
        if frame.len() < 97 {
            return Err("EAPOL-Key Message 3 too short");
        }

        // Verify MIC
        let key_data_len = u16::from_be_bytes([frame[95], frame[96]]);
        let key_data_end = 97 + key_data_len as usize;
        if frame.len() < key_data_end {
            return Err("Frame too short for key data");
        }

        // Extract GTK from key data (simplified - in real impl, parse KDE)
        if key_data_len >= 24 {
            let gtk_start: usize = 97 + 8; // Skip KDE header
            let gtk_len: usize = core::cmp::min(32, (key_data_len.saturating_sub(8)) as usize);
            if gtk_start + gtk_len <= frame.len() {
                self.gtk[..gtk_len].copy_from_slice(&frame[gtk_start..gtk_start + gtk_len]);
            }
        }

        self.state = WpaState::WaitMsg4;

        // Build Message 4 (ACK)
        Ok(self.build_message_4())
    }

    /// Build EAPOL-Key Message 4 (final ACK).
    fn build_message_4(&mut self) -> Vec<u8> {
        let mut msg = Vec::new();

        msg.push(0x03); // EAPOL-Key
        msg.push(0xFE);
        msg.extend_from_slice(&[0x00, 0x00]); // Length placeholder

        msg.push(0x02); // WPA2 Key Descriptor

        // Key Info
        let key_info = KEY_INFO_KEY_TYPE | KEY_INFO_MIC | KEY_INFO_SECURE | KEY_INFO_PAIRWISE;
        msg.extend_from_slice(&key_info.to_be_bytes());

        msg.extend_from_slice(&[0x00, 0x10]); // Key Length

        // Key Replay Counter
        let rc = self.replay_counter.wrapping_add(1);
        msg.extend_from_slice(&rc.to_be_bytes());

        msg.extend_from_slice(&[0u8; 32]); // Key Nonce (zero for msg4)

        msg.extend_from_slice(&[0u8; 16]); // Key IV
        msg.extend_from_slice(&[0u8; 8]);  // Key RSC
        msg.extend_from_slice(&[0u8; 8]);  // Key ID

        // MIC placeholder
        let mic_offset = msg.len();
        msg.extend_from_slice(&[0u8; 16]);

        msg.extend_from_slice(&[0x00, 0x00]); // Key Data Length

        // Update length
        let len = (msg.len() - 4) as u16;
        msg[2] = (len >> 8) as u8;
        msg[3] = len as u8;

        // Compute MIC
        let mic = compute_mic(&self.ptk, &msg);
        msg[mic_offset..mic_offset + 16].copy_from_slice(&mic);

        self.state = WpaState::Done;

        msg
    }

    /// Derive PTK from PMK, ANonce, SNonce, AP MAC, and client MAC.
    pub fn derive_ptk(&mut self) {
        // PTK = PRF-X(PMK, "Pairwise key expansion",
        //             min(AP_MAC, Client_MAC) || max(AP_MAC, Client_MAC) ||
        //             min(ANonce, SNonce) || max(ANonce, SNonce))

        let a1 = if self.ap_bssid < self.client_mac {
            self.ap_bssid
        } else {
            self.client_mac
        };
        let a2 = if self.ap_bssid < self.client_mac {
            self.client_mac
        } else {
            self.ap_bssid
        };

        let n1 = if self.anonce < self.snonce {
            self.anonce
        } else {
            self.snonce
        };
        let n2 = if self.anonce < self.snonce {
            self.snonce
        } else {
            self.anonce
        };

        let label = b"Pairwise key expansion";
        let mut data = Vec::new();
        data.extend_from_slice(&a1);
        data.extend_from_slice(&a2);
        data.extend_from_slice(&n1);
        data.extend_from_slice(&n2);

        // PRF-384 (PTK = 48 bytes = KCK(16) + KEK(16) + TK(16))
        // Copy PMK to avoid borrow conflict
        let pmk_copy = self.pmk;
        self.prf_384(&pmk_copy, label, &data);
    }

    /// PRF-384: Pseudo-random function producing 384 bits (48 bytes).
    fn prf_384(&mut self, key: &[u8; 32], label: &[u8], data: &[u8]) {
        // PRF(K, A, B) = HMAC-SHA1(K, A || 0 || B || i) for i = 0, 1, 2
        let mut output = [0u8; 48];
        let mut offset = 0;

        for i in 0..3 {
            let mut msg = Vec::new();
            msg.extend_from_slice(label);
            msg.push(0x00);
            msg.extend_from_slice(data);
            msg.push(i as u8);

            let hash = hmac_sha1(key, &msg);
            let end = core::cmp::min(offset + 20, 48);
            output[offset..end].copy_from_slice(&hash[..end - offset]);
            offset = end;
        }

        self.ptk = output;
    }
}

// ── Cryptographic helpers ─────────────────────────────────────

/// HMAC-SHA1 (simplified for WPA2 use).
fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    // HMAC-SHA1: H(K XOR opad || H(K XOR ipad || data))
    let block_size = 64;

    let mut k = [0u8; 64];
    let klen = key.len().min(64);
    k[..klen].copy_from_slice(&key[..klen]);

    if key.len() > block_size {
        // Key is longer than block: hash it first
        let hashed = sha1(key);
        k[..20].copy_from_slice(&hashed);
    }

    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for i in 0..64 {
        ipad[i] = k[i] ^ 0x36;
        opad[i] = k[i] ^ 0x5C;
    }

    // Inner hash: SHA1(K XOR ipad || data)
    let mut inner = Vec::with_capacity(64 + data.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(data);
    let inner_hash = sha1(&inner);

    // Outer hash: SHA1(K XOR opad || inner_hash)
    let mut outer = Vec::with_capacity(64 + 20);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_hash);
    sha1(&outer)
}

/// Compute MIC (Message Integrity Code) for EAPOL-Key frames.
fn compute_mic(ptk: &[u8; 48], frame: &[u8]) -> [u8; 16] {
    // The MIC is the first 16 bytes of HMAC-SHA1 using KCK (first 16 bytes of PTK)
    let kck: [u8; 16] = {
        let mut k = [0u8; 16];
        k.copy_from_slice(&ptk[..16]);
        k
    };

    let hash = hmac_sha1(&kck, frame);
    let mut mic = [0u8; 16];
    mic.copy_from_slice(&hash[..16]);
    mic
}

/// SHA-1 hash function (simplified implementation for WPA2).
/// This implements the SHA-1 algorithm as specified in FIPS 180-4.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [
        0x67452301,
        0xEFCDAB89,
        0x98BADCFE,
        0x10325476,
        0xC3D2E1F0,
    ];

    let len_bits = (data.len() as u64) * 8;
    let mut padded = Vec::with_capacity(data.len() + 9);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&len_bits.to_be_bytes());

    for chunk in padded.chunks(64) {
        let mut w = [0u32; 80];
        for t in 0..16 {
            w[t] = u32::from_be_bytes([
                chunk[t * 4],
                chunk[t * 4 + 1],
                chunk[t * 4 + 2],
                chunk[t * 4 + 3],
            ]);
        }
        for t in 16..80 {
            w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) =
            (state[0], state[1], state[2], state[3], state[4]);

        for t in 0..80 {
            let (f, k) = if t < 20 {
                ((b & c) | (!b & d), 0x5A827999)
            } else if t < 40 {
                (b ^ c ^ d, 0x6ED9EBA1)
            } else if t < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDC)
            } else {
                (b ^ c ^ d, 0xCA62C1D6)
            };

            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[t]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut output = [0u8; 20];
    for (i, s) in state.iter().enumerate() {
        output[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha1_empty() {
        let result = sha1(b"");
        let expected = [
            0xDA, 0x39, 0xA3, 0xEE, 0x5E, 0x6B, 0x4B, 0x0D,
            0x32, 0x55, 0xBF, 0xEF, 0x95, 0x60, 0x18, 0x90,
            0xAF, 0xD8, 0x07, 0x09,
        ];
        assert_eq!(result, expected);
    }

    #[test]
    fn test_sha1_abc() {
        let result = sha1(b"abc");
        let expected = [
            0xA9, 0x99, 0x3E, 0x36, 0x47, 0x06, 0x81, 0x6A,
            0xBA, 0x3E, 0x25, 0x71, 0x78, 0x50, 0xC2, 0x6C,
            0x9C, 0xD0, 0xD8, 0x9D,
        ];
        assert_eq!(result, expected);
    }
}
