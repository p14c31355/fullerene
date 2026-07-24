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
                                            if self.send_raw_80211_frame(&reply).is_err() {
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

        let int_cause = match self.safe_read32(CSR_INT) {
            Some(value) => value,
            None => return,
        };
        if int_cause != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }
            if int_cause & (1 << 18) != 0 {
                let raw_rx_head = match self.safe_read32(FH_RSCSR_CHNL0_RBDCB_RPTR_REG) {
                    Some(value) => value,
                    None => return,
                };
                self.rx_head = raw_rx_head as usize % RX_QUEUE_SIZE;
            }
            if int_cause & (1 << 15) != 0 {
                self.tx_tail = match self.safe_read32(FH_TX_CHNL0_WPTR) {
                    Some(value) => value,
                    None => return,
                } as usize;
                self.process_tx_queue();
            }
        }

        self.finish_pending_wpa_keys();

        mmio::cache_flush_range(
            self.rx_dma_ring.virt(),
            core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE,
        );
        while self.rx_tail != self.rx_head {
            let desc_idx = self.rx_tail;
            let (desc_len, rx_decrypted) = {
                let desc = self.rx_desc(desc_idx);
                (desc.len, desc.flags & RX_DESC_FLAG_DECRYPTED != 0)
            };
            if desc_len > 0 && desc_idx < self.rx_bufs.len() {
                let buf = &self.rx_bufs[desc_idx];
                let frame_len = (desc_len as usize).min(buf.len());
                let mut frame_data = alloc::vec![0; frame_len];
                buf.read_into(&mut frame_data);
                self.process_rx_frame(&frame_data, rx_decrypted);
            }

            let desc = self.rx_desc_mut(desc_idx);
            desc.len = MAX_FRAME_SIZE as u16;
            mmio::cache_flush(desc as *const RxDmaDesc as *const u8);
            self.rx_tail = (self.rx_tail + 1) % RX_QUEUE_SIZE;
        }

        if self.scan_pending {
            self.scan_channel += 1;
            if self.scan_channel > 13 {
                self.scan_pending = false;
                self.wifi_conn.finish_scan();
                self.iwl_state = IwlState::Disconnected;
                log::info!(
                    "iwlwifi: scan complete ({} APs found)",
                    self.scan_results.len()
                );
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
        if self.send_raw_80211_frame(&reply).is_err() || self.wpa.complete_handshake().is_err() {
            self.wpa_failed("could not complete WPA handshake");
            return;
        }
        self.wifi_conn.status = bonder::wifi::WifiStatus::Connected;
        self.start_dhcp(0);
    }
}
