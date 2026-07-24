//! Host-command and transmit-ring handling for [`IwlWifiDevice`].

use crate::mmio;

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
    /// IMPORTANT: This function only queues the commands asynchronously.
    /// The caller must NOT set wpa_keys_installed=true until firmware has
    /// consumed both commands from the TX ring. Use process_tx_queue() and
    /// verify tx_tail advancement before marking keys as installed.
    pub(super) fn install_wpa_keys(
        &mut self,
        ptk: [u8; 16],
        gtk: [u8; 16],
        gtk_key_index: u8,
    ) -> Result<(), crate::DriverError> {
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
        Ok(())
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

    /// Encapsulate a UDP/IP payload into an 802.11 data frame with LLC/SNAP header
    /// and send it. This function ensures DHCP and other IP traffic is properly
    /// encapsulated before transmission.
    pub fn send_ip_payload(&mut self, payload: &[u8]) -> Result<(), crate::DriverError> {
        let bssid = self.wifi_conn.current_bssid.ok_or(crate::DriverError::NotReady)?;

        let mut frame = Vec::new();

        // Frame control: type=data(2), subtype=data(0), Protected bit if WPA keys installed
        let protected_bit = if self.wpa_keys_installed { 0x40 } else { 0x00 };
        frame.push(0x08);
        frame.push(protected_bit);
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

        // LLC/SNAP header for IP (EtherType 0x0800)
        frame.extend_from_slice(&[
            0xAA, 0xAA, 0x03, // LLC header
            0x00, 0x00, 0x00, // SNAP OUI
            0x08, 0x00,       // EtherType: IPv4
        ]);

        // Append the IP payload
        frame.extend_from_slice(payload);

        // Send the encapsulated frame
        self.send_raw_80211_frame(&frame)
    }

    pub fn send_raw_80211_frame(&mut self, frame: &[u8]) -> Result<(), crate::DriverError> {
        // Validate that we have a proper 802.11 frame or EAPOL packet.
        // Reject bare payloads that need encapsulation.
        if frame.len() < 2 {
            return Err(crate::DriverError::InvalidArgument);
        }

        // Identify frame type based on well-known 802.11 patterns
        let frame_control = frame[0];
        let frame_type = (frame_control & 0x0C) >> 2;

        let is_eapol = frame.len() >= 4
            && frame[0] <= 3  // EAPOL version
            && frame[1] == 3; // EAPOL-Key packet type
        let is_80211_management = frame.len() >= 24
            && frame_type == 0 // Management frame type
            && matches!(frame[0] & 0xFC, 0x00 | 0xB0 | 0xC0); // assoc, auth, deauth subtypes
        let is_80211_data = frame_type == 2; // Data frame type

        // EAPOL and management frames are intentionally allowed before the
        // handshake. Data frames are not: an unprotected frame is a silent
        // plaintext fallback and must be rejected by the driver.
        if self.wpa_required {
            if is_80211_data {
                // For WPA-protected associations, data frames must have the
                // Protected bit set OR keys must not yet be installed (for
                // frames sent before handshake completion).
                let protected_bit = (frame[1] & 0x40) != 0;
                if !protected_bit && self.wpa_keys_installed {
                    return Err(crate::DriverError::NotReady);
                }
            } else if !is_eapol && !is_80211_management {
                // Reject frames that are neither EAPOL, management, nor data.
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
}
