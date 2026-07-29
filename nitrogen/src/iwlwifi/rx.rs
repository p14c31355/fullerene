//! Receive-ring, interrupt, and inbound protocol processing.

use bonder::dhcp::DhcpMessageType;
use bonder::wifi::{self, Ssid};
use bonder::wpa::WpaState;

use crate::mmio;

use super::device::IwlWifiDevice;
use super::registers::*;
use super::types::*;

impl IwlWifiDevice {
    fn process_rx_frame(&mut self, frame: &[u8], rx_decrypted: bool) {
        if frame.len() < 2 {
            return;
        }

        let frame_type = (frame[0] & 0x0C) >> 2;
        let subtype = (frame[0] >> 4) & 0x0F;
        match (frame_type, subtype) {
            (0, 5) | (0, 8) => {
                if self.iwl_state == IwlState::Scanning {
                    self.process_scan_result(frame);
                }
            }
            (0, 11) => {
                if self.iwl_state == IwlState::AuthSent || self.iwl_state == IwlState::Scanning {
                    let body_offset = 24;
                    if frame.len() >= body_offset + 6 {
                        let status_code =
                            u16::from_le_bytes([frame[body_offset + 4], frame[body_offset + 5]]);
                        if status_code == 0 {
                            self.iwl_state = IwlState::AssocSent;
                            let bssid = [
                                frame[10], frame[11], frame[12], frame[13], frame[14], frame[15],
                            ];
                            let ap_ssid = self
                                .wifi_conn
                                .current_ssid
                                .clone()
                                .unwrap_or(Ssid::new(b""));
                            let assoc = wifi::build_assoc_request_with_security(
                                bssid,
                                self.mac,
                                &ap_ssid,
                                self.wpa_required,
                            );
                            let _ = self.send_raw_80211_frame(&assoc);
                            log::info!("iwlwifi: auth successful, associating");
                        } else {
                            self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                            log::warn!("iwlwifi: auth failed with status {}", status_code);
                        }
                    }
                }
            }
            (0, 1) => {
                if self.iwl_state == IwlState::AssocSent {
                    let body_offset = 24;
                    if frame.len() >= body_offset + 6 {
                        let status_code =
                            u16::from_le_bytes([frame[body_offset + 2], frame[body_offset + 3]]);
                        if status_code == 0 {
                            let aid = u16::from_le_bytes([
                                frame[body_offset + 4],
                                frame[body_offset + 5],
                            ]);
                            self.iwl_state = IwlState::Connected;
                            self.wifi_conn.status = if self.wpa_required {
                                // Association is not an encrypted connection.
                                // Do not expose Connected or start DHCP until
                                // the 4-way handshake installs CCMP keys.
                                bonder::wifi::WifiStatus::Handshake
                            } else {
                                bonder::wifi::WifiStatus::Connected
                            };
                            self.wifi_conn.current_bssid = Some([
                                frame[10], frame[11], frame[12], frame[13], frame[14], frame[15],
                            ]);

                            if !self.wpa_required {
                                self.start_dhcp(aid);
                            }
                        } else {
                            self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                            log::warn!("iwlwifi: assoc failed with status {}", status_code);
                        }
                    }
                }
            }
            (2, subtype) => {
                let header_len = if subtype & 0x08 != 0 { 26 } else { 24 };
                if frame.len() > header_len {
                    let llc_offset = header_len;
                    if frame.len() > llc_offset + 8 {
                        let ether_type =
                            u16::from_be_bytes([frame[llc_offset + 6], frame[llc_offset + 7]]);
                        let data = &frame[llc_offset + 8..];
                        if self.wpa_required && ether_type == 0x888E && frame.len() >= 16 {
                            let from_ap = self
                                .wifi_conn
                                .current_bssid
                                .map(|bssid| frame[10..16] == bssid)
                                .unwrap_or(false);
                            let to_us = frame[4..10] == self.mac;
                            if !from_ap || !to_us {
                                return;
                            }
                        }
                        if self.wpa_required && ether_type != 0x888E {
                            let protected = frame[1] & 0x40 != 0;
                            // Before the handshake completes, discard every data
                            // frame.  Afterward, require either the Protected bit
                            // or an explicit firmware decryption/authentication
                            // status; this handles firmware that clears Protected
                            // after decrypting without allowing plaintext fallback.
                            if !self.wpa_keys_installed || (!protected && !rx_decrypted) {
                                return;
                            }
                        }
                        match ether_type {
                            0x888E => {
                                if !self.wpa_required {
                                    return;
                                }

                                match self.wpa.state {
                                    WpaState::WaitMsg1 => {
                                        if let Ok(reply) = self.wpa.handle_message_1(data) {
                                            if self.send_eapol_frame(&reply).is_err() {
                                                self.wpa_failed("could not send EAPOL message 2");
                                            }
                                        } else {
                                            self.wpa_failed("invalid EAPOL message 1");
                                        }
                                    }
                                    WpaState::WaitMsg3 => match self.wpa.handle_message_3(data) {
                                        Ok(reply) => {
                                            let Some((ptk, gtk, gtk_key_index)) =
                                                self.wpa.key_material()
                                            else {
                                                self.wpa_failed("EAPOL message 3 had no GTK");
                                                return;
                                            };
                                            let key_command_end = match self.install_wpa_keys(
                                                ptk,
                                                gtk,
                                                gtk_key_index,
                                            ) {
                                                Ok(end) => end,
                                                Err(_) => {
                                                    self.wpa_failed(
                                                        "could not queue the CCMP keys",
                                                    );
                                                    return;
                                                }
                                            };
                                            // Message 4 and all data traffic remain
                                            // blocked until the hardware advances its
                                            // TX tail over both key commands.
                                            self.wpa_key_command_end = Some(key_command_end);
                                            self.pending_wpa_message4 = Some(reply);
                                        }
                                        Err(_) => {
                                            self.wpa_failed("EAPOL message 3 authentication failed")
                                        }
                                    },
                                    _ => {}
                                }
                            }
                            0x0800 => {
                                let dhcp_handled = if data.len() >= 28 {
                                    let ihl = (data[0] & 0x0F) as usize * 4;
                                    if ihl >= 20 && data[9] == 17 && data.len() >= ihl + 8 {
                                        let dst_port =
                                            u16::from_be_bytes([data[ihl + 2], data[ihl + 3]]);
                                        if dst_port == 68 {
                                            if let Some(ref mut dhcp) = self.dhcp {
                                                let dhcp_data = &data[ihl + 8..];
                                                if let Ok(msg_type) = dhcp.parse_response(dhcp_data)
                                                {
                                                    log::info!(
                                                        "iwlwifi: DHCP {} received",
                                                        msg_type as u8
                                                    );
                                                    if msg_type == DhcpMessageType::Offer {
                                                        let request = dhcp.build_request(
                                                            dhcp.lease.ip_address,
                                                            dhcp.lease.server_id,
                                                        );
                                                        if let Err(error) =
                                                            self.send_dhcp_payload(&request)
                                                        {
                                                            self.wifi_conn.status =
                                                                bonder::wifi::WifiStatus::Error;
                                                            self.wifi_conn.error_msg = Some(
                                                                alloc::string::String::from(
                                                                    "DHCP request transmission failed",
                                                                ),
                                                            );
                                                            log::warn!(
                                                                "iwlwifi: failed to send DHCP request: {:?}",
                                                                error
                                                            );
                                                        }
                                                    } else if msg_type == DhcpMessageType::Ack {
                                                        self.ip_address = dhcp.lease.ip_address;
                                                        self.subnet_mask = dhcp.lease.subnet_mask;
                                                        self.gateway = dhcp.lease.router;
                                                        self.dns_server = dhcp.lease.dns_server;
                                                        log::info!(
                                                            "iwlwifi: IP address assigned: {:?}",
                                                            self.ip_address
                                                        );
                                                    }
                                                    true
                                                } else {
                                                    false
                                                }
                                            } else {
                                                false
                                            }
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                };
                                if !dhcp_handled {
                                    self.rx_queue.push_back(data.to_vec());
                                }
                            }
                            _ => self.rx_queue.push_back(data.to_vec()),
                        }
                    }
                }
            }
            (0, 10) | (0, 12) => {
                self.wifi_conn.status = bonder::wifi::WifiStatus::Disconnected;
                self.iwl_state = IwlState::Disconnected;
                log::warn!("iwlwifi: disconnected by AP");
            }
            _ => {}
        }
    }

    fn start_dhcp(&mut self, aid: u16) {
        self.dhcp = Some(bonder::dhcp::DhcpClient::new(self.mac));
        let discover = self
            .dhcp
            .as_mut()
            .expect("DHCP client was just initialized")
            .build_discover();
        log::info!("iwlwifi: associated (AID={}), sending DHCP discover", aid);
        if let Err(e) = self.send_dhcp_payload(&discover) {
            match e {
                crate::DriverError::InvalidArgument | crate::DriverError::NotSupported => {
                    self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                    self.wifi_conn.error_msg = Some(alloc::string::String::from(
                        "DHCP packet encapsulation failed",
                    ));
                    log::error!("iwlwifi: DHCP packet encapsulation failed");
                }
                crate::DriverError::NotReady => {
                    self.wpa_failed("cannot send DHCP before WPA keys installed");
                }
                _ => {
                    log::error!("iwlwifi: failed to send DHCP discover: {:?}", e);
                }
            }
        }
    }

    fn wpa_failed(&mut self, reason: &str) {
        self.wpa.state = WpaState::Error;
        self.wpa_keys_installed = false;
        self.wpa_key_command_end = None;
        self.pending_wpa_message4 = None;
        self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
        self.wifi_conn.error_msg = Some(alloc::string::String::from(reason));
        log::warn!("iwlwifi: WPA2 handshake failed: {}", reason);
    }

    /// Service device interrupts and consume completed receive descriptors.
    pub fn tick(&mut self) {
        if self.health.pre_mmio_access().is_err() {
            return;
        }
        // A hardware error requires a full device restart. Do not keep
        // polling the latched CSR_INT value after the transport has entered
        // the terminal error state; otherwise one uncleared cause floods the
        // kernel log on every scheduler tick.
        if self.fw_state == FwState::Error {
            return;
        }

        // CSR_INT is an interrupt cause register, not a reliable completion
        // poll result. On this legacy transport a completion can be reflected
        // in the scheduler/RBD pointers without presenting a cause bit to the
        // host (for example while the interrupt is coalesced or masked). Poll
        // both shared-memory pointers so queue accounting does not remain
        // stuck at the last interrupt.
        let polled_tx_tail = if self.fw_state == FwState::Ready {
            self.read_prph(SCD_QUEUE_RDPTR_CMD)
                .map(|value| value as usize)
        } else {
            None
        };
        let tx_tail_before_poll = self.tx_tail;
        if let Some(hardware_tail) = polled_tx_tail {
            self.update_tx_tail(hardware_tail);
            if self.tx_tail != tx_tail_before_poll {
                log::info!(
                    "iwlwifi: TX completion progress scd_rptr={} tx_tail={} tx_head={}",
                    hardware_tail & (TX_QUEUE_SIZE - 1),
                    self.tx_tail & (TX_QUEUE_SIZE - 1),
                    self.tx_head & (TX_QUEUE_SIZE - 1),
                );
                self.process_tx_queue();
            }
        }

        let int_cause = match self.safe_read32(CSR_INT) {
            Some(value) => value,
            None => return,
        };
        // Read FH_INT before acknowledging CSR_INT.  Once the aggregate
        // interrupt is acknowledged, the per-channel error cause may no
        // longer be observable.  HW_ERR is fatal for this transport; leave a
        // complete snapshot in the log instead of allowing the scan watchdog
        // to obscure the first DMA failure.
        let fh_cause = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
        if int_cause & CSR_INT_BIT_HW_ERR != 0 {
            let int_mask = self.safe_read32(CSR_INT_MASK).unwrap_or(0);
            let tx_status = self.safe_read32(FH_TSSR_TX_STATUS_REG).unwrap_or(!0);
            let tx_error = self.safe_read32(FH_TSSR_TX_ERROR_REG).unwrap_or(!0);
            let tx_trb = self.safe_read32(FH_TX_TRB_CHNL0).unwrap_or(!0);
            let tx_cfg = self
                .safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + IWL_CMD_QUEUE * (0x20 / 4))
                .unwrap_or(!0);
            let csr_gp1 = self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0);
            let csr_gp_driver = self.safe_read32(CSR_GP_DRIVER).unwrap_or(!0);
            let csr_reset = self.safe_read32(CSR_RESET).unwrap_or(!0);
            let csr_gp_cntrl = self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0);
            let scd_rptr = self.read_prph(SCD_QUEUE_RDPTR_CMD).unwrap_or(!0);
            let scd_status = self.read_prph(SCD_QUEUE_STATUS_CMD).unwrap_or(!0);
            log::error!(
                "iwlwifi: FH hardware error: CSR_INT={:#010x} CSR_INT_MASK={:#010x} FH_INT={:#010x} UCODE_GP1={:#010x} GP_DRIVER={:#010x} RESET={:#010x} GP_CNTRL={:#010x} TSSR_STATUS={:#010x} TSSR_ERROR={:#010x} TX_TRB={:#010x} TX_CFG={:#010x} SCD_RDPTR={} SCD_STATUS={:#010x}",
                int_cause,
                int_mask,
                fh_cause,
                csr_gp1,
                csr_gp_driver,
                csr_reset,
                csr_gp_cntrl,
                tx_status,
                tx_error,
                tx_trb,
                tx_cfg,
                scd_rptr,
                scd_status,
            );
            unsafe {
                // CSR_INT is cleared using the upstream gen1 formula:
                // acknowledge the observed causes plus every currently
                // masked cause. Writing only int_cause leaves HW_ERR latched
                // on this hardware.
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause | !int_mask);
                core::ptr::write_volatile(self.mmio.add(CSR_INT_MASK as usize), 0);
            }
            self.fw_state = FwState::Error;
            self.scan_pending = false;
            self.iwl_state = IwlState::Disconnected;
            return;
        }
        if int_cause != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }
            if int_cause & (CSR_INT_BIT_FH_RX | CSR_INT_BIT_SW_RX) != 0 {
                unsafe {
                    core::ptr::write_volatile(
                        self.mmio.add(CSR_FH_INT as usize),
                        fh_cause & CSR_FH_INT_RX_MASK,
                    );
                }
            }
            if int_cause & CSR_INT_BIT_FH_TX != 0 {
                unsafe {
                    core::ptr::write_volatile(
                        self.mmio.add(CSR_FH_INT as usize),
                        fh_cause & CSR_FH_INT_TX_MASK,
                    );
                }
            }
            // CSR_INT reports the aggregate FH RX cause at bit 31. The
            // per-channel bits live in CSR_FH_INT; bit 18 is not the host RX
            // interrupt and caused received scan frames to be ignored.
            if int_cause & (CSR_INT_BIT_FH_RX | CSR_INT_BIT_SW_RX) != 0 {
                // Gen1 hardware reports the completed RBD in host memory;
                // the old MMIO read-pointer offsets used here previously are
                // not the legacy RX status registers.
                mmio::cache_flush_range(
                    self.rx_status() as *const RxDmaStatus as usize,
                    core::mem::size_of::<RxDmaStatus>(),
                );
                let closed_rb = (self.rx_status().closed_rb_num as usize) & (RX_QUEUE_SIZE - 1);
                // closed_rb_num is the next RBD boundary: firmware has
                // filled entries [rx_tail, closed_rb_num). This matches the
                // Linux gen1_2 receive loop, which processes while read != r.
                self.rx_head = closed_rb;
                log::info!(
                    "iwlwifi: RX DMA progress closed_rbd={} process_until={}",
                    closed_rb,
                    self.rx_head
                );
            }
            if int_cause & CSR_INT_BIT_FH_TX != 0 {
                // The pointer was polled above. Re-read only when an actual
                // FH_TX cause is present, since the interrupt and the SCD
                // pointer are independent on this generation.
                if let Some(hardware_tail) = self.read_prph(SCD_QUEUE_RDPTR_CMD) {
                    self.update_tx_tail(hardware_tail as usize);
                }
                self.process_tx_queue();
            }
        }

        // Also consume RX buffers when the shared status advanced without a
        // corresponding CSR_INT bit. This is the RX equivalent of the TX
        // pointer polling above.
        if self.fw_state == FwState::Ready {
            mmio::cache_flush_range(
                self.rx_status() as *const RxDmaStatus as usize,
                core::mem::size_of::<RxDmaStatus>(),
            );
            let closed_rb = (self.rx_status().closed_rb_num as usize) & (RX_QUEUE_SIZE - 1);
            if closed_rb != self.rx_head {
                self.rx_head = closed_rb;
                log::info!(
                    "iwlwifi: RX DMA progress polled closed_rbd={} process_until={}",
                    closed_rb,
                    self.rx_head
                );
            }
        }

        self.finish_pending_wpa_keys();

        let rx_tail_before = self.rx_tail;
        mmio::cache_flush_range(
            self.rx_dma_ring.virt(),
            core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE,
        );
        while self.rx_tail != self.rx_head {
            let desc_idx = self.rx_tail;
            if desc_idx < self.rx_bufs.len() {
                let buf = &self.rx_bufs[desc_idx];
                // Legacy FH does not put a length in the RBD. The firmware
                // packet header contains the useful length; inspect the full
                // 4K RB and let the packet decoder locate the 802.11 body.
                let frame_len = buf.len();
                let mut frame_data = alloc::vec![0; frame_len];
                buf.read_into(&mut frame_data);
                self.process_rx_buffer(&frame_data);
            }

            self.rx_tail = (self.rx_tail + 1) % RX_QUEUE_SIZE;
        }

        if self.rx_tail != rx_tail_before {
            // The hardware write index is the next RBD made available and
            // must advance in groups of eight on this generation.
            unsafe {
                core::ptr::write_volatile(
                    self.mmio.add(FH_RSCSR_CHNL0_RBDCB_WPTR_REG as usize),
                    (self.rx_tail as u32) & !7,
                );
            }
            mmio::write_barrier();
        }

        if self.scan_pending {
            self.scan_channel += 1;
            if self.scan_channel > 4000 {
                self.scan_pending = false;
                self.wifi_conn.finish_scan();
                self.iwl_state = IwlState::Disconnected;
                let tx_rptr = self.read_prph(SCD_QUEUE_RDPTR_CMD).unwrap_or(!0);
                let scd_status = self.read_prph(SCD_QUEUE_STATUS_CMD).unwrap_or(!0);
                let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
                let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
                let int_mask = self.safe_read32(CSR_INT_MASK).unwrap_or(!0);
                let tx_cfg = self
                    .safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + IWL_CMD_QUEUE * (0x20 / 4))
                    .unwrap_or(!0);
                log::warn!(
                    "iwlwifi: scan watchdog expired without firmware completion notification ({} APs found): tx_head={} tx_tail={} scd_rptr={} scd_status={:#010x} CSR_INT={:#010x} CSR_INT_MASK={:#010x} FH_INT={:#010x} TX_CFG={:#010x}",
                    self.scan_results.len(),
                    self.tx_head & 0xff,
                    self.tx_tail & 0xff,
                    tx_rptr,
                    scd_status,
                    csr_int,
                    int_mask,
                    fh_int,
                    tx_cfg,
                );
                log::info!(
                    "iwlwifi: scan complete by host watchdog ({} APs found)",
                    self.scan_results.len()
                );
            }
        }
    }

    /// Decode a legacy iwlwifi RX buffer. Runtime notifications carry an
    /// iwl_rx_packet header before their payload. The first four bytes are
    /// len_n_flags; the four-byte command header follows it. Some firmware
    /// paths expose the 802.11 MPDU directly, so retain a bounded fallback.
    fn process_rx_buffer(&mut self, data: &[u8]) {
        // FH_RSCSR_FRAME_SIZE_MSK covers the low 14 bits of len_n_flags. The
        // packet length includes len_n_flags and the command header.
        let packet_len = if data.len() >= 8 {
            let reported =
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize & 0x3fff;
            if reported >= 8 && reported <= data.len() {
                reported
            } else {
                data.len()
            }
        } else {
            data.len()
        };

        if packet_len >= 8 {
            let command = data[4];
            let group = data[5];
            let sequence = u16::from_le_bytes([data[6], data[7]]);
            if command == 0xe7 || command == 0x6d {
                let payload = &data[8..packet_len];
                log::info!(
                    "iwlwifi: firmware scan complete notification cmd=0x{:02x} status={} scanned_channels={}",
                    command,
                    payload.get(1).copied().unwrap_or(0),
                    payload.first().copied().unwrap_or(0),
                );
                if self.scan_pending {
                    self.scan_pending = false;
                    self.wifi_conn.finish_scan();
                    self.iwl_state = IwlState::Disconnected;
                    log::info!(
                        "iwlwifi: scan complete ({} APs found)",
                        self.scan_results.len()
                    );
                }
                return;
            }

            // REPLY_RX_MPDU_CMD (0xc1) has iwl_rx_mpdu_res_start
            // (byte_count, assist) at payload offset 0, followed by the raw
            // 802.11 MPDU. The packet's 4-byte length header is not part of
            // the command payload.
            if command == 0xc1 && packet_len >= 12 {
                let payload = &data[8..packet_len];
                let byte_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                let frame_end = 12usize.saturating_add(byte_count).min(packet_len);
                log::info!(
                    "iwlwifi: RX MPDU notification group=0x{:02x} seq=0x{:04x} bytes={} frame_bytes={}",
                    group,
                    sequence,
                    byte_count,
                    frame_end.saturating_sub(12)
                );
                if frame_end >= 36 {
                    self.process_rx_frame(&data[12..frame_end], false);
                }
                return;
            }

            // REPLY_ERROR payload layout is error_type:u32, cmd_id:u8,
            // reserved:u8, bad_cmd_seq:u16, error_service:u32.
            if command == LegacyCmd::ReplyError as u8 && packet_len >= 20 {
                let payload = &data[8..packet_len];
                let error_type =
                    u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                let bad_cmd = payload[4];
                let bad_seq = u16::from_le_bytes([payload[6], payload[7]]);
                let service =
                    u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
                log::warn!(
                    "iwlwifi: firmware command error type={:#010x} cmd=0x{:02x} seq=0x{:04x} service={:#010x}",
                    error_type,
                    bad_cmd,
                    bad_seq,
                    service,
                );
                return;
            }

            log::info!(
                "iwlwifi: RX firmware notification cmd=0x{:02x} group=0x{:02x} seq=0x{:04x} packet_len={}",
                command,
                group,
                sequence,
                packet_len,
            );
        }

        let scan_end = core::cmp::min(packet_len.saturating_sub(24), 128);
        for offset in 0..=scan_end {
            let fc = data[offset];
            let frame_type = (fc & 0x0C) >> 2;
            let subtype = (fc >> 4) & 0x0F;
            if frame_type == 0 && (subtype == 5 || subtype == 8) {
                let candidate = &data[offset..];
                if candidate.len() >= 24 {
                    self.process_rx_frame(candidate, false);
                    return;
                }
            }
        }
    }

    /// Complete the WPA transition only after the key commands have left the
    /// host TX ring.  Message 4 is deliberately deferred so a peer cannot
    /// start encrypted data exchange while the NIC is still unarmed.
    fn finish_pending_wpa_keys(&mut self) {
        let Some(command_end) = self.wpa_key_command_end else {
            return;
        };
        if !self.tx_tail_reached(command_end) {
            return;
        }

        self.wpa_key_command_end = None;
        self.wpa_keys_installed = true;
        let Some(reply) = self.pending_wpa_message4.take() else {
            self.wpa_failed("WPA Message 4 was lost before key activation");
            return;
        };
        if self.send_eapol_frame(&reply).is_err() || self.wpa.complete_handshake().is_err() {
            self.wpa_failed("could not complete WPA handshake");
            return;
        }
        self.wifi_conn.status = bonder::wifi::WifiStatus::Connected;
        self.start_dhcp(0);
    }
}
