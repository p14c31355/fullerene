//! Host-command and transmit-ring handling for [`IwlWifiDevice`].

use crate::mmio;
use alloc::vec::Vec;

use super::device::IwlWifiDevice;
use super::registers::*;
use super::types::*;

impl IwlWifiDevice {
    /// Queue WPA2-PSK CCMP pairwise and group key installation commands.
    ///
    /// iwlwifi performs CCMP in the NIC/firmware.  Keeping the keys only in
    /// the supplicant is not sufficient: raw data frames must never leave the
    /// device until these two ADD_STA_KEY commands have been consumed by
    /// firmware.
    ///
    /// IMPORTANT: This function only queues the commands asynchronously.  The
    /// returned TX-ring position must be retained and checked from the device
    /// tick before enabling the protected data path.
    pub(super) fn install_wpa_keys(
        &mut self,
        ptk: [u8; 16],
        gtk: [u8; 16],
        gtk_key_index: u8,
    ) -> Result<usize, crate::DriverError> {
        const STA_KEY_FLG_CCM: u16 = 2;
        const STA_KEY_FLG_KEYID_POS: u16 = 8;
        const STA_KEY_MULTICAST: u16 = 1 << 14;

        let mut pairwise = AddStaKeyCmd {
            // The AP is the first peer station in this minimal STA mode.
            sta_id: 0,
            key_offset: 0,
            key_flags: STA_KEY_FLG_CCM,
            key: [0; 32],
            rx_security_seq: [0; 16],
        };
        pairwise.key[..16].copy_from_slice(&ptk);

        let mut group = AddStaKeyCmd {
            sta_id: 0,
            key_offset: 1,
            key_flags: STA_KEY_FLG_CCM
                | STA_KEY_MULTICAST
                | ((gtk_key_index as u16 & 0x03) << STA_KEY_FLG_KEYID_POS),
            key: [0; 32],
            rx_security_seq: [0; 16],
        };
        group.key[..16].copy_from_slice(&gtk);

        let pairwise_bytes = unsafe {
            core::slice::from_raw_parts(
                &pairwise as *const AddStaKeyCmd as *const u8,
                core::mem::size_of::<AddStaKeyCmd>(),
            )
        };
        let group_bytes = unsafe {
            core::slice::from_raw_parts(
                &group as *const AddStaKeyCmd as *const u8,
                core::mem::size_of::<AddStaKeyCmd>(),
            )
        };

        self.send_hcmd(
            LegacyCmd::AddStaKey as u8,
            GroupId::Legacy as u8,
            pairwise_bytes,
        )?;
        self.send_hcmd(
            LegacyCmd::AddStaKey as u8,
            GroupId::Legacy as u8,
            group_bytes,
        )?;
        Ok(self.tx_head)
    }

    pub(super) fn send_hcmd(
        &mut self,
        opcode: u8,
        group: u8,
        data: &[u8],
    ) -> Result<(), crate::DriverError> {
        let total_len = core::mem::size_of::<HcmdHeader>() + data.len();
        if total_len > MAX_FRAME_SIZE {
            return Err(crate::DriverError::InvalidArgument);
        }

        self.health
            .pre_mmio_access()
            .map_err(|_| crate::DriverError::DeviceNotFound)?;
        let hcmd_header = HcmdHeader {
            opcode,
            group_id: group,
            length: data.len() as u16,
            flags: 0,
            reserved: 0,
        };

        let used = self.tx_head.wrapping_sub(self.tx_tail);
        if used >= TX_QUEUE_SIZE {
            return Err(crate::DriverError::Busy);
        }
        let desc_idx = self.tx_head % TX_QUEUE_SIZE;
        let desc_ptr = self.tx_dma_ring.virt() as *mut TxDmaDesc;
        let cmd_buf = &mut self.tx_bufs[desc_idx];
        let header_bytes = unsafe {
            core::slice::from_raw_parts(
                &hcmd_header as *const HcmdHeader as *const u8,
                core::mem::size_of::<HcmdHeader>(),
            )
        };
        let mut full_data = alloc::vec::Vec::with_capacity(total_len);
        full_data.extend_from_slice(header_bytes);
        full_data.extend_from_slice(data);
        cmd_buf.write_from(&full_data);

        let dma_addr = cmd_buf.dma_iova();
        let desc = unsafe { &mut *desc_ptr.add(desc_idx) };
        desc.addr_lo = dma_addr as u32;
        desc.addr_hi = (dma_addr >> 32) as u32;
        desc.len = total_len as u16;
        desc.flags = 0;
        mmio::cache_flush(desc as *const TxDmaDesc as *const u8);

        self.tx_head = self.tx_head.wrapping_add(1);
        mmio::write_barrier();
        unsafe {
            core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
        }
        mmio::write_barrier();
        Ok(())
    }

    pub fn send_init_commands(&mut self) -> Result<(), crate::DriverError> {
        let ant_cfg: [u8; 8] = [0x03, 0x03, 0, 0, 0, 0, 0, 0];
        self.send_hcmd(
            LegacyCmd::TxAntConfig as u8,
            GroupId::Legacy as u8,
            &ant_cfg,
        )?;
        log::info!("iwlwifi: TX antenna config sent");

        let mut rxon = [0u8; 36];
        rxon[0] = 0x42;
        rxon[1] = 0x00;
        rxon[12..18].copy_from_slice(&self.mac);
        rxon[22] = 100;
        self.send_hcmd(LegacyCmd::Rxon as u8, GroupId::Legacy as u8, &rxon)?;
        log::info!("iwlwifi: RXON config sent");

        self.fw_state = FwState::Ready;
        log::info!("iwlwifi: init commands complete, device operational");
        Ok(())
    }

    /// Send a complete IPv4 packet in an 802.11 data frame with LLC/SNAP.
    ///
    /// Callers must provide the IPv4 header as well as its payload.  In
    /// particular, a bare DHCP packet must go through `send_dhcp_payload`.
    pub fn send_ip_payload(&mut self, payload: &[u8]) -> Result<(), crate::DriverError> {
        if payload.len() < 20
            || payload[0] >> 4 != 4
            || (payload[0] & 0x0f) < 5
            || (payload[0] & 0x0f) as usize * 4 > payload.len()
        {
            return Err(crate::DriverError::InvalidArgument);
        }
        let ihl = (payload[0] & 0x0f) as usize * 4;
        let total_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
        if total_len < ihl || total_len > payload.len() {
            return Err(crate::DriverError::InvalidArgument);
        }
        let protected = self.wpa_keys_installed;
        self.send_data_frame(0x0800, &payload[..total_len], protected)
    }

    /// Encapsulate a DHCP packet in IPv4/UDP/LLC and send it.
    pub fn send_dhcp_payload(&mut self, payload: &[u8]) -> Result<(), crate::DriverError> {
        let udp_len = 8usize
            .checked_add(payload.len())
            .ok_or(crate::DriverError::InvalidArgument)?;
        let ip_len = 20usize
            .checked_add(udp_len)
            .ok_or(crate::DriverError::InvalidArgument)?;
        if ip_len > u16::MAX as usize {
            return Err(crate::DriverError::InvalidArgument);
        }

        let mut packet = Vec::with_capacity(ip_len);
        packet.extend_from_slice(&[
            0x45,
            0x00, // IPv4, IHL=5, DSCP/ECN
            (ip_len >> 8) as u8,
            ip_len as u8,
            0x00,
            0x00, // identification
            0x00,
            0x00, // flags/fragment offset
            64,   // TTL
            17,   // UDP
            0x00,
            0x00, // checksum placeholder
            0x00,
            0x00,
            0x00,
            0x00, // source 0.0.0.0
            0xff,
            0xff,
            0xff,
            0xff, // destination 255.255.255.255
        ]);
        let checksum = ipv4_checksum(&packet[..20]);
        packet[10..12].copy_from_slice(&checksum.to_be_bytes());

        packet.extend_from_slice(&[
            0x00,
            0x44, // source port 68
            0x00,
            0x43, // destination port 67
            (udp_len >> 8) as u8,
            udp_len as u8,
            0x00,
            0x00, // UDP checksum is optional for IPv4 DHCP
        ]);
        packet.extend_from_slice(payload);
        let protected = self.wpa_keys_installed;
        self.send_data_frame(0x0800, &packet, protected)
    }

    /// Wrap an EAPOL-Key PDU in the 802.11 data and LLC/SNAP headers required
    /// on the air.  EAPOL itself is intentionally unprotected during the
    /// four-way handshake; only ordinary data frames require CCMP keys.
    pub(super) fn send_eapol_frame(&mut self, pdu: &[u8]) -> Result<(), crate::DriverError> {
        if pdu.len() < 4 || pdu[1] != 3 {
            return Err(crate::DriverError::InvalidArgument);
        }
        let declared_len = u16::from_be_bytes([pdu[2], pdu[3]]) as usize;
        if declared_len < 95 || 4 + declared_len > pdu.len() {
            return Err(crate::DriverError::InvalidArgument);
        }
        let frame = self.build_data_frame(0x888E, pdu, false)?;
        self.send_raw_80211_frame(&frame)
    }

    fn send_data_frame(
        &mut self,
        ether_type: u16,
        payload: &[u8],
        protected: bool,
    ) -> Result<(), crate::DriverError> {
        let frame = self.build_data_frame(ether_type, payload, protected)?;
        self.send_raw_80211_frame(&frame)
    }

    fn build_data_frame(
        &self,
        ether_type: u16,
        payload: &[u8],
        protected: bool,
    ) -> Result<Vec<u8>, crate::DriverError> {
        let bssid = self
            .wifi_conn
            .current_bssid
            .ok_or(crate::DriverError::NotReady)?;
        let frame_len = 24usize
            .checked_add(8)
            .and_then(|len| len.checked_add(payload.len()))
            .ok_or(crate::DriverError::InvalidArgument)?;
        if frame_len > MAX_FRAME_SIZE {
            return Err(crate::DriverError::InvalidArgument);
        }

        let mut frame = Vec::with_capacity(frame_len);

        // Frame control: data + ToDS.  EAPOL is unprotected; ordinary data
        // callers pass protected=true only after CCMP key activation.
        let protected_bit = if protected { 0x40 } else { 0x00 };
        frame.push(0x08);
        frame.push(0x01 | protected_bit);
        // Duration
        frame.extend_from_slice(&[0x00, 0x00]);
        // Addr1: BSSID (destination = AP)
        frame.extend_from_slice(&bssid);
        // Addr2: source (client MAC)
        frame.extend_from_slice(&self.mac);
        // Addr3: BSSID
        frame.extend_from_slice(&bssid);
        // Sequence control
        frame.extend_from_slice(&[0x00, 0x00]);

        // LLC/SNAP header.
        frame.extend_from_slice(&[
            0xAA,
            0xAA,
            0x03, // LLC header
            0x00,
            0x00,
            0x00, // SNAP OUI
            (ether_type >> 8) as u8,
            ether_type as u8,
        ]);

        // Append the IP payload
        frame.extend_from_slice(payload);

        Ok(frame)
    }

    pub fn send_raw_80211_frame(&mut self, frame: &[u8]) -> Result<(), crate::DriverError> {
        // Validate that we have a proper 802.11 frame.  EAPOL-Key PDUs must
        // already be wrapped by send_eapol_frame; bare payloads are rejected.
        if frame.len() < 2 {
            return Err(crate::DriverError::InvalidArgument);
        }

        // Identify frame type based on well-known 802.11 patterns
        let frame_control = frame[0];
        let frame_type = (frame_control & 0x0C) >> 2;

        let is_80211_management = frame.len() >= 24
            && frame_type == 0 // Management frame type
            && matches!(frame[0] & 0xFC, 0x00 | 0xB0 | 0xC0); // assoc, auth, deauth subtypes
        let is_80211_data = frame_type == 2; // Data frame type
        let is_80211_eapol = is_80211_data
            && frame.len() >= 32
            && frame[24..32] == [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x88, 0x8E];

        // EAPOL data frames and management frames are intentionally allowed
        // before the handshake. Other data frames are not: an unprotected
        // frame is a silent plaintext fallback and must be rejected.
        if self.wpa_required {
            if is_80211_data {
                let protected_bit = (frame[1] & 0x40) != 0;
                if is_80211_eapol {
                    if protected_bit {
                        return Err(crate::DriverError::NotReady);
                    }
                } else if !self.wpa_keys_installed || !protected_bit {
                    // For WPA-protected associations, data frames must not be
                    // transmitted until keys are installed, and must carry the
                    // Protected bit.  There is no plaintext fallback.
                    return Err(crate::DriverError::NotReady);
                }
            } else if !is_80211_management {
                // Reject frames that are neither management nor data.
                // This prevents bare IP/UDP payloads from being misclassified
                // and sent without proper 802.11 encapsulation.
                return Err(crate::DriverError::NotSupported);
            }
        }

        self.tx_queue.push_back(frame.to_vec());
        self.process_tx_queue();
        Ok(())
    }

    pub(super) fn process_tx_queue(&mut self) {
        if self.health.pre_mmio_access().is_err() {
            return;
        }

        while let Some(tx_frame) = self.tx_queue.front() {
            if tx_frame.len() > MAX_FRAME_SIZE {
                self.tx_queue.pop_front();
                continue;
            }
            if self.tx_head.wrapping_sub(self.tx_tail) >= TX_QUEUE_SIZE {
                break;
            }

            let tx_frame = self.tx_queue.pop_front().unwrap();
            let desc_idx = self.tx_head % TX_QUEUE_SIZE;
            let desc_ptr = self.tx_dma_ring.virt() as *mut TxDmaDesc;
            let buf = &mut self.tx_bufs[desc_idx];
            buf.write_from(&tx_frame);

            let dma_addr = buf.dma_iova();
            let desc = unsafe { &mut *desc_ptr.add(desc_idx) };
            desc.addr_lo = dma_addr as u32;
            desc.addr_hi = (dma_addr >> 32) as u32;
            desc.len = tx_frame.len() as u16;
            desc.flags = 0;
            mmio::cache_flush(desc as *const TxDmaDesc as *const u8);

            self.tx_head = self.tx_head.wrapping_add(1);
            mmio::write_barrier();
            unsafe {
                core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
            }
            mmio::write_barrier();
        }
    }

    /// Return whether the monotonic hardware TX tail has reached or passed the
    /// command sequence's end position.
    pub(super) fn tx_tail_reached(&self, target: usize) -> bool {
        (self.tx_tail.wrapping_sub(target) as isize) >= 0
    }

    /// Extend the hardware's ring index into the host's monotonic TX-tail
    /// counter.  This keeps queue accounting and completion checks correct
    /// across ring wraparound.
    pub(super) fn update_tx_tail(&mut self, hardware_tail: usize) {
        let hardware_tail = hardware_tail % TX_QUEUE_SIZE;
        let current_tail = self.tx_tail % TX_QUEUE_SIZE;
        let advance = (hardware_tail + TX_QUEUE_SIZE - current_tail) % TX_QUEUE_SIZE;
        let outstanding = self.tx_head.wrapping_sub(self.tx_tail);
        if advance > outstanding {
            // A backwards jump is not valid progress.  Leave the monotonic
            // counter unchanged so a reset/stale register cannot activate
            // WPA keys prematurely.
            return;
        }
        self.tx_tail = self.tx_tail.wrapping_add(advance);
    }
}

fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in header.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
