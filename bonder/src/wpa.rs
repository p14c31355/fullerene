//! WPA/WPA2 supplicant implementation.
//!
//! Handles the 4-way handshake for WPA2-PSK (Personal mode).
//! Uses PBKDF2-SHA1 for passphrase-to-PMK derivation and
//! the 4-way handshake for PTK/GTK derivation.

use crate::wifi::Bssid;
use alloc::string::String;
use alloc::vec::Vec;

/// WPA state for a single connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WpaState {
    #[default]
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
    /// Length of the GTK extracted from the GTK KDE.  WPA2-CCMP uses 16.
    gtk_len: usize,
    pub gtk_key_index: u8,
}

impl Default for WpaSupplicant {
    fn default() -> Self {
        Self {
            state: WpaState::default(),
            ssid: String::new(),
            passphrase: String::new(),
            pmk: [0; 32],
            ptk: [0; 48],
            gtk: [0; 32],
            anonce: [0; 32],
            snonce: [0; 32],
            ap_bssid: [0; 6],
            client_mac: [0; 6],
            replay_counter: 0,
            mic_error: false,
            gtk_len: 0,
            gtk_key_index: 0,
        }
    }
}

impl WpaSupplicant {
    pub fn new() -> Self {
        Self::default()
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

        // The PMK is ready, but PTK derivation must wait for the AP's
        // ANonce in EAPOL-Key message 1.
        self.state = WpaState::WaitMsg1;
        self.replay_counter = 0;
        self.anonce = [0; 32];
        self.ptk = [0; 48];
        self.gtk = [0; 32];
        self.gtk_len = 0;
        self.gtk_key_index = 0;
    }

    /// Derive PMK = PBKDF2-SHA1(passphrase, ssid, 4096, 256)
    fn derive_pmk(&mut self) {
        let pass_bytes = self.passphrase.as_bytes();
        let ssid_bytes = self.ssid.as_bytes();

        // PBKDF2 implementation for WPA2-PSK.
        let mut output = [0u8; 32];

        // WPA2 PSK = PBKDF2(HMAC-SHA1, passphrase, ssid, 4096, 256)
        // HMAC-SHA1 produces 20 bytes per block, so we need 2 blocks for 32 bytes
        for (i, block) in (1u32..=2).enumerate() {
            let start = i * 20;
            let end = core::cmp::min(start + 20, 32);
            let len = end - start;

            // U_1 = HMAC-SHA1(P, S || INT(i))
            let mut salt = ssid_bytes.to_vec();
            salt.extend_from_slice(&block.to_be_bytes());

            let mut u = hmac_sha1(pass_bytes, &salt);

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
        let mut snonce = [0u8; 32];

        for (i, chunk) in snonce.chunks_mut(8).enumerate() {
            let mut val = 0u64;
            #[cfg(target_arch = "x86_64")]
            let success = unsafe {
                let cpuid = core::arch::x86_64::__cpuid(1);
                if (cpuid.ecx & (1 << 30)) != 0 {
                    core::arch::x86_64::_rdrand64_step(&mut val)
                } else {
                    0
                }
            };
            #[cfg(not(target_arch = "x86_64"))]
            let success = 0;
            if success == 0 {
                // Fallback to TSC if RDRAND is not supported or fails
                #[cfg(target_arch = "x86_64")]
                {
                    let tsc = unsafe { core::arch::x86_64::_rdtsc() };
                    val = tsc ^ (i as u64).wrapping_mul(0x9E3779B97F4A7C15);
                }
                #[cfg(not(target_arch = "x86_64"))]
                {
                    val = (i as u64).wrapping_mul(0x9E3779B97F4A7C15);
                }
            }

            // xorshift64* PRNG to whiten the sample
            let mut state = val;
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let whiten = state.wrapping_mul(0x2545F4914F6CDD1D);

            chunk.copy_from_slice(&whiten.to_le_bytes()[..chunk.len()]);
        }

        self.snonce = snonce;
    }

    /// Handle EAPOL-Key Message 1 (from AP).
    pub fn handle_message_1(&mut self, frame: &[u8]) -> Result<Vec<u8>, crate::NetError> {
        if self.state != WpaState::WaitMsg1 || frame.len() < 4 + 95 {
            return Err(crate::NetError::Protocol);
        }
        let body = &frame[4..];

        if body[0] != 0x02 {
            return Err(crate::NetError::Protocol);
        }
        let key_info = u16::from_be_bytes([body[1], body[2]]);
        // Message 1 is an un-MICed pairwise EAPOL-Key frame sent by the AP.
        if key_info & (KEY_INFO_KEY_TYPE | KEY_INFO_PAIRWISE | KEY_INFO_ACK)
            != (KEY_INFO_KEY_TYPE | KEY_INFO_PAIRWISE | KEY_INFO_ACK)
            || key_info & (KEY_INFO_MIC | KEY_INFO_INSTALL | KEY_INFO_ERROR) != 0
        {
            return Err(crate::NetError::Protocol);
        }

        // Extract ANonce (bytes 13-44 relative to key descriptor)
        let anonce_start = 13;
        if anonce_start + 32 > body.len() {
            return Err(crate::NetError::Protocol);
        }
        self.anonce
            .copy_from_slice(&body[anonce_start..anonce_start + 32]);
        if self.anonce == [0; 32] {
            return Err(crate::NetError::Protocol);
        }

        // PTK = PRF(PMK, ANonce, SNonce, AP-MAC, STA-MAC).  In particular,
        // this must happen after ANonce has been received; deriving it in
        // init() would permanently bind the session to an all-zero nonce.
        self.derive_ptk();

        // Update replay counter
        if body.len() >= 13 {
            let rc_bytes: [u8; 8] = [
                body[5], body[6], body[7], body[8], body[9], body[10], body[11], body[12],
            ];
            self.replay_counter = u64::from_be_bytes(rc_bytes);
        }

        // Message 2 has just been built; the next peer message is message 3.
        self.state = WpaState::WaitMsg3;

        // Build Message 2 (SNonce + MIC)
        let msg2 = self.build_message_2();
        Ok(msg2)
    }

    /// Build EAPOL-Key Message 2 (our response with SNonce).
    fn build_message_2(&self) -> Vec<u8> {
        let mut msg = Vec::new();

        // EAPOL header
        msg.push(0x03); // EAPOL Version (802.1X-2010)
        msg.push(0x03); // EAPOL Packet Type (EAPOL-Key)
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
    pub fn handle_message_3(&mut self, frame: &[u8]) -> Result<Vec<u8>, crate::NetError> {
        if self.state != WpaState::WaitMsg3 || frame.len() < 4 + 95 {
            return Err(crate::NetError::Protocol);
        }

        // Parse EAPOL length field and validate that 4 + length fits within frame
        let eapol_length = u16::from_be_bytes([frame[2], frame[3]]) as usize;
        let pdu_end = 4 + eapol_length;
        if pdu_end > frame.len() {
            return Err(crate::NetError::Protocol);
        }

        // Verify MIC: zero the MIC field in a copy, compute, compare
        let body = &frame[4..pdu_end];
        if body[0] != 0x02 {
            return Err(crate::NetError::Protocol);
        }
        let key_info = u16::from_be_bytes([body[1], body[2]]);
        if key_info
            & (KEY_INFO_KEY_TYPE
                | KEY_INFO_PAIRWISE
                | KEY_INFO_MIC
                | KEY_INFO_ACK
                | KEY_INFO_INSTALL)
            != (KEY_INFO_KEY_TYPE
                | KEY_INFO_PAIRWISE
                | KEY_INFO_MIC
                | KEY_INFO_ACK
                | KEY_INFO_INSTALL)
            || key_info & KEY_INFO_ERROR != 0
        {
            return Err(crate::NetError::Protocol);
        }

        let mic_field = &frame[81..97];
        let mut frame_copy = frame[..pdu_end].to_vec();
        frame_copy[81..97].fill(0);
        let computed_mic = compute_mic(&self.ptk, &frame_copy);

        // Use constant-time comparison and return immediately on mismatch
        let mut mismatch = 0u8;
        for i in 0..16 {
            mismatch |= computed_mic[i] ^ mic_field[i];
        }
        if mismatch != 0 {
            return Err(crate::NetError::AuthenticationFailed);
        }

        let message3_replay = u64::from_be_bytes([
            body[5], body[6], body[7], body[8], body[9], body[10], body[11], body[12],
        ]);
        if message3_replay <= self.replay_counter {
            return Err(crate::NetError::Protocol);
        }
        self.replay_counter = message3_replay;

        let key_data_len = u16::from_be_bytes([body[93], body[94]]);
        let key_data_end = 95 + key_data_len as usize;
        if body.len() < key_data_end {
            return Err(crate::NetError::Protocol);
        }

        // Extract key descriptor version from key_info (bits 0-2)
        let descriptor_version = key_info & 0x07;

        // For descriptor version 2, Key Data is encrypted with AES-Key-Wrap
        let key_data_plaintext: Vec<u8>;
        let key_data_slice = if descriptor_version == 2 {
            // Key Encryption Key (KEK) is bytes 16-31 of the PTK
            let kek: [u8; 16] = {
                let mut k = [0u8; 16];
                k.copy_from_slice(&self.ptk[16..32]);
                k
            };

            // Decrypt the Key Data using AES-Key-Unwrap (RFC 3394)
            let encrypted_data = &body[95..key_data_end];
            match aes_key_unwrap(&kek, encrypted_data) {
                Ok(plaintext) => {
                    key_data_plaintext = plaintext;
                    &key_data_plaintext[..]
                }
                Err(_) => return Err(crate::NetError::Protocol),
            }
        } else {
            // For other descriptor versions, Key Data is plaintext
            &body[95..key_data_end]
        };

        // Parse Key Data Descriptors to extract GTK
        let mut gtk_len = 0;
        let mut gtk_key_index = 0;
        let mut pos = 0;
        while pos + 2 <= key_data_slice.len() {
            let kde_type = key_data_slice[pos];
            let kde_len = key_data_slice[pos + 1] as usize;
            let kde_end = pos + 2 + kde_len;
            if kde_end > key_data_slice.len() {
                break;
            }
            // GTK KDE: type=0xDD, OUI=00-0F-AC, data_type=1
            if kde_type == 0xDD
                && kde_len >= 6
                && key_data_slice[pos + 2] == 0x00
                && key_data_slice[pos + 3] == 0x0F
                && key_data_slice[pos + 4] == 0xAC
                && key_data_slice[pos + 5] == 0x01
            {
                let gtk_data = &key_data_slice[pos + 6..kde_end];
                // gtk_data: key_id(1) + reserved(1) + gtk(N)
                if gtk_data.len() >= 2 {
                    gtk_len = core::cmp::min(gtk_data.len() - 2, 32);
                    self.gtk[..gtk_len].copy_from_slice(&gtk_data[2..2 + gtk_len]);
                    self.gtk_len = gtk_len;
                    gtk_key_index = gtk_data[0] & 0x03;
                    self.gtk_key_index = gtk_key_index;
                }
                break;
            }
            pos = kde_end;
        }

        if gtk_len < 16 {
            return Err(crate::NetError::Protocol);
        }
        self.state = WpaState::WaitMsg4;

        // Build Message 4 (ACK)
        Ok(self.build_message_4())
    }

    /// Build EAPOL-Key Message 4 (final ACK).
    fn build_message_4(&mut self) -> Vec<u8> {
        let mut msg = Vec::new();

        msg.push(0x03); // EAPOL Version (802.1X-2010)
        msg.push(0x03); // EAPOL Packet Type (EAPOL-Key)
        msg.extend_from_slice(&[0x00, 0x00]); // Length placeholder

        msg.push(0x02); // WPA2 Key Descriptor

        // Key Info
        let key_info = KEY_INFO_KEY_TYPE | KEY_INFO_MIC | KEY_INFO_SECURE | KEY_INFO_PAIRWISE;
        msg.extend_from_slice(&key_info.to_be_bytes());

        msg.extend_from_slice(&[0x00, 0x10]); // Key Length

        // Key Replay Counter
        // Message 4 uses the replay counter from Message 3 unchanged.
        msg.extend_from_slice(&self.replay_counter.to_be_bytes());

        msg.extend_from_slice(&[0u8; 32]); // Key Nonce (zero for msg4)

        msg.extend_from_slice(&[0u8; 16]); // Key IV
        msg.extend_from_slice(&[0u8; 8]); // Key RSC
        msg.extend_from_slice(&[0u8; 8]); // Key ID

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

        msg
    }

    /// Return the CCMP pairwise and group keys after Message 3 was verified.
    /// The caller must install both keys in the hardware before sending
    /// Message 4 and exposing the data path.
    pub fn key_material(&self) -> Option<([u8; 16], [u8; 16], u8)> {
        if !matches!(self.state, WpaState::WaitMsg4 | WpaState::Done) || self.gtk_len < 16 {
            return None;
        }
        let mut ptk = [0u8; 16];
        let mut gtk = [0u8; 16];
        ptk.copy_from_slice(&self.ptk[32..48]);
        gtk.copy_from_slice(&self.gtk[..16]);
        Some((ptk, gtk, self.gtk_key_index))
    }

    /// Mark the handshake complete only after the hardware accepted both
    /// CCMP keys and Message 4 has been queued.
    pub fn complete_handshake(&mut self) -> Result<(), crate::NetError> {
        if self.state != WpaState::WaitMsg4 {
            return Err(crate::NetError::Protocol);
        }
        self.state = WpaState::Done;
        Ok(())
    }

    /// Derive PTK from PMK, ANonce, SNonce, AP MAC, and client MAC.
    pub fn derive_ptk(&mut self) {
        // An all-zero ANonce is the sentinel used before EAPOL-Key message 1.
        // Refusing to derive here prevents accidental use of a predictable
        // PTK if a caller invokes this method at the wrong point in the flow.
        if self.anonce == [0; 32] {
            self.ptk = [0; 48];
            return;
        }
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
    if key.len() > block_size {
        // Key is longer than block: hash it first
        let hashed = sha1(key);
        k[..20].copy_from_slice(&hashed);
    } else {
        k[..key.len()].copy_from_slice(key);
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

/// AES-128 Key Unwrap algorithm (RFC 3394).
/// Unwraps (decrypts) data encrypted with AES Key Wrap.
fn aes_key_unwrap(kek: &[u8; 16], ciphertext: &[u8]) -> Result<Vec<u8>, ()> {
    // Ciphertext must be at least 24 bytes (8-byte IV + minimum 16-byte block)
    // and a multiple of 8 bytes
    if ciphertext.len() < 24 || ciphertext.len() % 8 != 0 {
        return Err(());
    }

    let n = (ciphertext.len() / 8) - 1;
    let mut r = Vec::with_capacity(n * 8);
    for i in 0..n {
        r.extend_from_slice(&ciphertext[(i + 1) * 8..(i + 2) * 8]);
    }

    let mut a = [0u8; 8];
    a.copy_from_slice(&ciphertext[0..8]);

    // Unwrap: 6 iterations in reverse
    for j in (0..6).rev() {
        for i in (0..n).rev() {
            let t = (n * j + i + 1) as u64;
            for k in 0..8 {
                a[7 - k] ^= ((t >> (k * 8)) & 0xFF) as u8;
            }

            let mut b = [0u8; 16];
            b[0..8].copy_from_slice(&a);
            b[8..16].copy_from_slice(&r[i * 8..(i + 1) * 8]);

            let decrypted = aes_decrypt_block(kek, &b);
            a.copy_from_slice(&decrypted[0..8]);
            r[i * 8..(i + 1) * 8].copy_from_slice(&decrypted[8..16]);
        }
    }

    // Verify the IV (default IV is 0xA6A6A6A6A6A6A6A6)
    let expected_iv = [0xA6u8; 8];
    if a != expected_iv {
        return Err(());
    }

    Ok(r)
}

/// AES-128 block decryption.
/// This is a minimal AES-128 implementation for WPA2 key unwrapping.
fn aes_decrypt_block(key: &[u8; 16], ciphertext: &[u8; 16]) -> [u8; 16] {
    // For a complete implementation, we would need full AES decrypt.
    // This is a placeholder that should be replaced with a proper AES library.
    // For now, we'll implement a basic version using the inverse AES operations.

    // Expand the key
    let round_keys = aes_key_expansion(key);

    // Copy ciphertext to state
    let mut state = [0u8; 16];
    state.copy_from_slice(ciphertext);

    // Initial round
    aes_add_round_key(&mut state, &round_keys[10]);

    // 9 main rounds (in reverse)
    for round in (1..10).rev() {
        aes_inv_shift_rows(&mut state);
        aes_inv_sub_bytes(&mut state);
        aes_add_round_key(&mut state, &round_keys[round]);
        aes_inv_mix_columns(&mut state);
    }

    // Final round
    aes_inv_shift_rows(&mut state);
    aes_inv_sub_bytes(&mut state);
    aes_add_round_key(&mut state, &round_keys[0]);

    state
}

/// AES key expansion for 128-bit keys.
fn aes_key_expansion(key: &[u8; 16]) -> [[u8; 16]; 11] {
    let mut round_keys = [[0u8; 16]; 11];
    round_keys[0].copy_from_slice(key);

    let rcon: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1B, 0x36];

    for i in 1..11 {
        let mut temp = [0u8; 4];
        temp.copy_from_slice(&round_keys[i - 1][12..16]);

        // RotWord
        temp.rotate_left(1);

        // SubWord
        for byte in &mut temp {
            *byte = AES_SBOX[*byte as usize];
        }

        // XOR with Rcon
        temp[0] ^= rcon[i - 1];

        // Generate new round key
        for j in 0..4 {
            round_keys[i][j] = round_keys[i - 1][j] ^ temp[j];
        }
        for j in 4..8 {
            round_keys[i][j] = round_keys[i - 1][j] ^ round_keys[i][j - 4];
        }
        for j in 8..12 {
            round_keys[i][j] = round_keys[i - 1][j] ^ round_keys[i][j - 4];
        }
        for j in 12..16 {
            round_keys[i][j] = round_keys[i - 1][j] ^ round_keys[i][j - 4];
        }
    }

    round_keys
}

fn aes_add_round_key(state: &mut [u8; 16], round_key: &[u8; 16]) {
    for i in 0..16 {
        state[i] ^= round_key[i];
    }
}

fn aes_inv_sub_bytes(state: &mut [u8; 16]) {
    for byte in state.iter_mut() {
        *byte = AES_INV_SBOX[*byte as usize];
    }
}

fn aes_inv_shift_rows(state: &mut [u8; 16]) {
    let tmp = *state;
    state[0] = tmp[0];
    state[1] = tmp[13];
    state[2] = tmp[10];
    state[3] = tmp[7];
    state[4] = tmp[4];
    state[5] = tmp[1];
    state[6] = tmp[14];
    state[7] = tmp[11];
    state[8] = tmp[8];
    state[9] = tmp[5];
    state[10] = tmp[2];
    state[11] = tmp[15];
    state[12] = tmp[12];
    state[13] = tmp[9];
    state[14] = tmp[6];
    state[15] = tmp[3];
}

fn aes_inv_mix_columns(state: &mut [u8; 16]) {
    fn gf_mul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0u8;
        for _ in 0..8 {
            if b & 1 != 0 {
                p ^= a;
            }
            let hi_bit_set = a & 0x80 != 0;
            a <<= 1;
            if hi_bit_set {
                a ^= 0x1B;
            }
            b >>= 1;
        }
        p
    }

    for i in 0..4 {
        let s0 = state[i * 4];
        let s1 = state[i * 4 + 1];
        let s2 = state[i * 4 + 2];
        let s3 = state[i * 4 + 3];

        state[i * 4] = gf_mul(s0, 0x0e) ^ gf_mul(s1, 0x0b) ^ gf_mul(s2, 0x0d) ^ gf_mul(s3, 0x09);
        state[i * 4 + 1] = gf_mul(s0, 0x09) ^ gf_mul(s1, 0x0e) ^ gf_mul(s2, 0x0b) ^ gf_mul(s3, 0x0d);
        state[i * 4 + 2] = gf_mul(s0, 0x0d) ^ gf_mul(s1, 0x09) ^ gf_mul(s2, 0x0e) ^ gf_mul(s3, 0x0b);
        state[i * 4 + 3] = gf_mul(s0, 0x0b) ^ gf_mul(s1, 0x0d) ^ gf_mul(s2, 0x09) ^ gf_mul(s3, 0x0e);
    }
}

const AES_SBOX: [u8; 256] = [
    0x63, 0x7C, 0x77, 0x7B, 0xF2, 0x6B, 0x6F, 0xC5, 0x30, 0x01, 0x67, 0x2B, 0xFE, 0xD7, 0xAB, 0x76,
    0xCA, 0x82, 0xC9, 0x7D, 0xFA, 0x59, 0x47, 0xF0, 0xAD, 0xD4, 0xA2, 0xAF, 0x9C, 0xA4, 0x72, 0xC0,
    0xB7, 0xFD, 0x93, 0x26, 0x36, 0x3F, 0xF7, 0xCC, 0x34, 0xA5, 0xE5, 0xF1, 0x71, 0xD8, 0x31, 0x15,
    0x04, 0xC7, 0x23, 0xC3, 0x18, 0x96, 0x05, 0x9A, 0x07, 0x12, 0x80, 0xE2, 0xEB, 0x27, 0xB2, 0x75,
    0x09, 0x83, 0x2C, 0x1A, 0x1B, 0x6E, 0x5A, 0xA0, 0x52, 0x3B, 0xD6, 0xB3, 0x29, 0xE3, 0x2F, 0x84,
    0x53, 0xD1, 0x00, 0xED, 0x20, 0xFC, 0xB1, 0x5B, 0x6A, 0xCB, 0xBE, 0x39, 0x4A, 0x4C, 0x58, 0xCF,
    0xD0, 0xEF, 0xAA, 0xFB, 0x43, 0x4D, 0x33, 0x85, 0x45, 0xF9, 0x02, 0x7F, 0x50, 0x3C, 0x9F, 0xA8,
    0x51, 0xA3, 0x40, 0x8F, 0x92, 0x9D, 0x38, 0xF5, 0xBC, 0xB6, 0xDA, 0x21, 0x10, 0xFF, 0xF3, 0xD2,
    0xCD, 0x0C, 0x13, 0xEC, 0x5F, 0x97, 0x44, 0x17, 0xC4, 0xA7, 0x7E, 0x3D, 0x64, 0x5D, 0x19, 0x73,
    0x60, 0x81, 0x4F, 0xDC, 0x22, 0x2A, 0x90, 0x88, 0x46, 0xEE, 0xB8, 0x14, 0xDE, 0x5E, 0x0B, 0xDB,
    0xE0, 0x32, 0x3A, 0x0A, 0x49, 0x06, 0x24, 0x5C, 0xC2, 0xD3, 0xAC, 0x62, 0x91, 0x95, 0xE4, 0x79,
    0xE7, 0xC8, 0x37, 0x6D, 0x8D, 0xD5, 0x4E, 0xA9, 0x6C, 0x56, 0xF4, 0xEA, 0x65, 0x7A, 0xAE, 0x08,
    0xBA, 0x78, 0x25, 0x2E, 0x1C, 0xA6, 0xB4, 0xC6, 0xE8, 0xDD, 0x74, 0x1F, 0x4B, 0xBD, 0x8B, 0x8A,
    0x70, 0x3E, 0xB5, 0x66, 0x48, 0x03, 0xF6, 0x0E, 0x61, 0x35, 0x57, 0xB9, 0x86, 0xC1, 0x1D, 0x9E,
    0xE1, 0xF8, 0x98, 0x11, 0x69, 0xD9, 0x8E, 0x94, 0x9B, 0x1E, 0x87, 0xE9, 0xCE, 0x55, 0x28, 0xDF,
    0x8C, 0xA1, 0x89, 0x0D, 0xBF, 0xE6, 0x42, 0x68, 0x41, 0x99, 0x2D, 0x0F, 0xB0, 0x54, 0xBB, 0x16,
];

const AES_INV_SBOX: [u8; 256] = [
    0x52, 0x09, 0x6A, 0xD5, 0x30, 0x36, 0xA5, 0x38, 0xBF, 0x40, 0xA3, 0x9E, 0x81, 0xF3, 0xD7, 0xFB,
    0x7C, 0xE3, 0x39, 0x82, 0x9B, 0x2F, 0xFF, 0x87, 0x34, 0x8E, 0x43, 0x44, 0xC4, 0xDE, 0xE9, 0xCB,
    0x54, 0x7B, 0x94, 0x32, 0xA6, 0xC2, 0x23, 0x3D, 0xEE, 0x4C, 0x95, 0x0B, 0x42, 0xFA, 0xC3, 0x4E,
    0x08, 0x2E, 0xA1, 0x66, 0x28, 0xD9, 0x24, 0xB2, 0x76, 0x5B, 0xA2, 0x49, 0x6D, 0x8B, 0xD1, 0x25,
    0x72, 0xF8, 0xF6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xD4, 0xA4, 0x5C, 0xCC, 0x5D, 0x65, 0xB6, 0x92,
    0x6C, 0x70, 0x48, 0x50, 0xFD, 0xED, 0xB9, 0xDA, 0x5E, 0x15, 0x46, 0x57, 0xA7, 0x8D, 0x9D, 0x84,
    0x90, 0xD8, 0xAB, 0x00, 0x8C, 0xBC, 0xD3, 0x0A, 0xF7, 0xE4, 0x58, 0x05, 0xB8, 0xB3, 0x45, 0x06,
    0xD0, 0x2C, 0x1E, 0x8F, 0xCA, 0x3F, 0x0F, 0x02, 0xC1, 0xAF, 0xBD, 0x03, 0x01, 0x13, 0x8A, 0x6B,
    0x3A, 0x91, 0x11, 0x41, 0x4F, 0x67, 0xDC, 0xEA, 0x97, 0xF2, 0xCF, 0xCE, 0xF0, 0xB4, 0xE6, 0x73,
    0x96, 0xAC, 0x74, 0x22, 0xE7, 0xAD, 0x35, 0x85, 0xE2, 0xF9, 0x37, 0xE8, 0x1C, 0x75, 0xDF, 0x6E,
    0x47, 0xF1, 0x1A, 0x71, 0x1D, 0x29, 0xC5, 0x89, 0x6F, 0xB7, 0x62, 0x0E, 0xAA, 0x18, 0xBE, 0x1B,
    0xFC, 0x56, 0x3E, 0x4B, 0xC6, 0xD2, 0x79, 0x20, 0x9A, 0xDB, 0xC0, 0xFE, 0x78, 0xCD, 0x5A, 0xF4,
    0x1F, 0xDD, 0xA8, 0x33, 0x88, 0x07, 0xC7, 0x31, 0xB1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xEC, 0x5F,
    0x60, 0x51, 0x7F, 0xA9, 0x19, 0xB5, 0x4A, 0x0D, 0x2D, 0xE5, 0x7A, 0x9F, 0x93, 0xC9, 0x9C, 0xEF,
    0xA0, 0xE0, 0x3B, 0x4D, 0xAE, 0x2A, 0xF5, 0xB0, 0xC8, 0xEB, 0xBB, 0x3C, 0x83, 0x53, 0x99, 0x61,
    0x17, 0x2B, 0x04, 0x7E, 0xBA, 0x77, 0xD6, 0x26, 0xE1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0C, 0x7D,
];

/// SHA-1 hash function (simplified implementation for WPA2).
/// This implements the SHA-1 algorithm as specified in FIPS 180-4.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

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

        for (t, word) in w.iter().enumerate() {
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
                .wrapping_add(*word);
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
    use alloc::vec;

    #[test]
    fn ptk_is_derived_only_after_message_one() {
        let mut supplicant = WpaSupplicant::new();
        supplicant.init(
            "correct horse battery staple",
            "TestAP",
            [1, 2, 3, 4, 5, 6],
            [6, 5, 4, 3, 2, 1],
        );

        assert_eq!(supplicant.state, WpaState::WaitMsg1);
        assert_eq!(supplicant.ptk, [0; 48]);

        let mut message1 = vec![0u8; 4 + 95];
        message1[4] = 0x02; // WPA2 key descriptor
        message1[5..7]
            .copy_from_slice(&(KEY_INFO_KEY_TYPE | KEY_INFO_PAIRWISE | KEY_INFO_ACK).to_be_bytes());
        message1[9..17].copy_from_slice(&1u64.to_be_bytes());
        message1[17..49].copy_from_slice(&[0xA5; 32]);

        let reply = supplicant.handle_message_1(&message1).unwrap();
        assert!(!reply.is_empty());
        assert_eq!(supplicant.state, WpaState::WaitMsg3);
        assert_ne!(supplicant.ptk, [0; 48]);
    }

    #[test]
    fn test_sha1_empty() {
        let result = sha1(b"");
        let expected = [
            0xDA, 0x39, 0xA3, 0xEE, 0x5E, 0x6B, 0x4B, 0x0D, 0x32, 0x55, 0xBF, 0xEF, 0x95, 0x60,
            0x18, 0x90, 0xAF, 0xD8, 0x07, 0x09,
        ];
        assert_eq!(result, expected);
    }

    #[test]
    fn test_sha1_abc() {
        let result = sha1(b"abc");
        let expected = [
            0xA9, 0x99, 0x3E, 0x36, 0x47, 0x06, 0x81, 0x6A, 0xBA, 0x3E, 0x25, 0x71, 0x78, 0x50,
            0xC2, 0x6C, 0x9C, 0xD0, 0xD8, 0x9D,
        ];
        assert_eq!(result, expected);
    }
}
