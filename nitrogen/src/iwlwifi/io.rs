//! HCMD interface, scanning, connection management, TX/RX processing,
//! periodic tick, and trait implementations for IwlWifiDevice.

use alloc::boxed::Box;
use alloc::vec::Vec;

use bonder::{NetDevice, NetError};
use bonder::wifi::{self, Ssid, AccessPoint};
use bonder::wpa::WpaState;
use bonder::dhcp::DhcpMessageType;

use crate::mmio;

use super::regs::*;
use super::types::*;
use super::device::IwlWifiDevice;

// ── HCMD interface ────────────────

impl IwlWifiDevice {
    pub(super) fn send_hcmd(&mut self, opcode: u8, group: u8, data: &[u8]) -> Result<(), &'static str> {
        let total_len = core::mem::size_of::<HcmdHeader>() + data.len();
        if total_len > MAX_FRAME_SIZE {
            return Err("HCMD too large");
        }

        self.health.pre_mmio_access().map_err(|_| "device not accessible")?;

        let hcmd_header = HcmdHeader {
            opcode,
            group_id: group,
            length: data.len() as u16,
            flags: 0,
            reserved: 0,
        };

        let used = self.tx_head.wrapping_sub(self.tx_tail);
        if used >= TX_QUEUE_SIZE {
            return Err("TX ring full");
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

        let desc_addr = desc as *const TxDmaDesc as *const u8;
        mmio::cache_flush(desc_addr);

        self.tx_head = self.tx_head.wrapping_add(1);

        mmio::write_barrier();
        unsafe {
            core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
        }
        mmio::write_barrier();

        Ok(())
    }

    pub fn send_init_commands(&mut self) -> Result<(), &'static str> {
        let ant_cfg: [u8; 8] = [0x03, 0x03, 0, 0, 0, 0, 0, 0];
        self.send_hcmd(LegacyCmd::TxAntConfig as u8, GroupId::Legacy as u8, &ant_cfg)
            .map_err(|_| "TX antenna config failed")?;
        log::info!("iwlwifi: TX antenna config sent");

        let mut rxon = [0u8; 36];
        rxon[0] = 0x42;
        rxon[1] = 0x00;
        let mac = self.mac;
        rxon[12..18].copy_from_slice(&mac);
        rxon[22] = 100;
        rxon[23] = 0;
        self.send_hcmd(LegacyCmd::Rxon as u8, GroupId::Legacy as u8, &rxon)
            .map_err(|_| "RXON config failed")?;
        log::info!("iwlwifi: RXON config sent");

        log::info!("iwlwifi: init commands complete, device operational");
        Ok(())
    }
}

// ── Scanning ──────────────────────

impl IwlWifiDevice {
    pub fn start_scan(&mut self) -> Result<(), &'static str> {
        if self.fw_state != FwState::Ready {
            return Err("Firmware not ready");
        }

        self.wifi_conn.start_scan();
        self.scan_results.clear();
        self.scan_channel = 1;
        self.scan_pending = true;
        self.iwl_state = IwlState::Scanning;

        let scan_cmd = ScanRequestCmd {
            beacon_interval: 100,
            flags: 0,
            num_channels: 4,
            reserved: [0u8; 3],
            channels: [
                ScanChannel { channel: 1, tx_power: 0, reserved: 0 },
                ScanChannel { channel: 6, tx_power: 0, reserved: 0 },
                ScanChannel { channel: 11, tx_power: 0, reserved: 0 },
                ScanChannel { channel: 36, tx_power: 0, reserved: 0 },
            ],
        };

        let cmd_data = unsafe {
            core::slice::from_raw_parts(
                &scan_cmd as *const ScanRequestCmd as *const u8,
                core::mem::size_of::<ScanRequestCmd>(),
            )
        };

        self.send_hcmd(LegacyCmd::ScanRequest as u8, GroupId::Legacy as u8, cmd_data)?;

        log::info!("iwlwifi: scan started");
        Ok(())
    }

    fn process_scan_result(&mut self, frame: &[u8]) {
        if let Some(beacon) = wifi::parse_beacon(frame) {
            let ssid = beacon.ssid.clone().unwrap_or(Ssid::new(b""));
            if ssid.is_empty() {
                return;
            }

            let security = wifi::security_from_beacon(
                beacon.capability,
                beacon.rsn.as_ref(),
            );

            let ap = AccessPoint {
                ssid,
                bssid: beacon.header.addr2,
                channel: beacon.ds_channel.unwrap_or(0),
                rssi: -50,
                security,
                beacon_interval: beacon.beacon_interval,
            };

            self.wifi_conn.add_scan_result(ap.clone());
            self.scan_results.push(ap);
        }
    }
}

// ── Connection management ─────────

impl IwlWifiDevice {
    pub fn connect(&mut self, ssid: &Ssid, password: Option<&str>) -> Result<(), &'static str> {
        if self.fw_state != FwState::Ready {
            return Err("Firmware not ready");
        }

        let ap = match self.scan_results.iter().find(|a| a.ssid == *ssid) {
            Some(a) => a.clone(),
            None => return Err("AP not found in scan results"),
        };

        self.wifi_conn.connect(ssid, password);

        if password.is_some() {
            self.wpa.init(
                password.unwrap(),
                ssid.as_str(),
                ap.bssid,
                self.mac,
            );
            self.wpa.derive_ptk();
        }

        self.iwl_state = IwlState::AuthSent;

        let auth_frame = wifi::build_auth_frame(ap.bssid, self.mac, 1);
        let _ = self.send_raw_80211_frame(&auth_frame);

        log::info!(
            "iwlwifi: authenticating with {} ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x})",
            ssid,
            ap.bssid[0], ap.bssid[1], ap.bssid[2],
            ap.bssid[3], ap.bssid[4], ap.bssid[5],
        );

        Ok(())
    }

    pub fn disconnect(&mut self) {
        if let Some(bssid) = self.wifi_conn.current_bssid {
            let deauth = wifi::build_deauth(bssid, self.mac, 3);
            let _ = self.send_raw_80211_frame(&deauth);
        }

        if let Some(ref mut dhcp) = self.dhcp {
            let release = dhcp.build_release();
            let _ = self.send_raw_80211_frame(&release);
        }
        self.dhcp = None;

        self.wifi_conn.disconnect();
        self.iwl_state = IwlState::Disconnected;

        log::info!("iwlwifi: disconnected");
    }

    pub fn send_raw_80211_frame(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        self.tx_queue.push_back(frame.to_vec());
        self.process_tx_queue();
        Ok(())
    }
}

// ── TX/RX processing ──────────────

impl IwlWifiDevice {
    fn process_tx_queue(&mut self) {
        if self.health.pre_mmio_access().is_err() {
            return;
        }

        while let Some(tx_frame) = self.tx_queue.front() {
            if tx_frame.len() > MAX_FRAME_SIZE {
                self.tx_queue.pop_front();
                continue;
            }
            let used = self.tx_head.wrapping_sub(self.tx_tail);
            if used >= TX_QUEUE_SIZE {
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

            let desc_addr = desc as *const TxDmaDesc as *const u8;
            mmio::cache_flush(desc_addr);

            self.tx_head = self.tx_head.wrapping_add(1);

            mmio::write_barrier();
            unsafe {
                core::ptr::write_volatile(self.mmio.add(0x0BC / 4), self.tx_head as u32);
            }
            mmio::write_barrier();
        }
    }

    fn process_rx_frame(&mut self, frame: &[u8]) {
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
                if self.iwl_state == IwlState::AuthSent
                    || self.iwl_state == IwlState::Scanning
                {
                    let body_offset = 24;
                    if frame.len() >= body_offset + 6 {
                        let status_code = u16::from_le_bytes([
                            frame[body_offset + 4],
                            frame[body_offset + 5],
                        ]);
                        if status_code == 0 {
                            self.iwl_state = IwlState::AssocSent;
                            let bssid = [
                                frame[10], frame[11], frame[12],
                                frame[13], frame[14], frame[15],
                            ];
                            let ap_ssid = self.wifi_conn.current_ssid.clone()
                                .unwrap_or(Ssid::new(b""));
                            let assoc = wifi::build_assoc_request(
                                bssid, self.mac, &ap_ssid,
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
                        let status_code = u16::from_le_bytes([
                            frame[body_offset + 2],
                            frame[body_offset + 3],
                        ]);
                        if status_code == 0 {
                            let aid = u16::from_le_bytes([
                                frame[body_offset + 4],
                                frame[body_offset + 5],
                            ]);
                            self.iwl_state = IwlState::Connected;
                            self.wifi_conn.status = bonder::wifi::WifiStatus::Connected;
                            self.wifi_conn.current_bssid = Some([
                                frame[10], frame[11], frame[12],
                                frame[13], frame[14], frame[15],
                            ]);

                            self.dhcp = Some(bonder::dhcp::DhcpClient::new(self.mac));
                            if let Some(ref mut dhcp) = self.dhcp {
                                let discover = dhcp.build_discover();
                                log::info!(
                                    "iwlwifi: associated (AID={}), sending DHCP discover", aid
                                );
                                let _ = self.send_raw_80211_frame(&discover);
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
                        let ether_type = u16::from_be_bytes([
                            frame[llc_offset + 6],
                            frame[llc_offset + 7],
                        ]);
                        let data = &frame[llc_offset + 8..];
                        match ether_type {
                            0x888E => {
                                if self.wpa.state == WpaState::WaitMsg1 {
                                    if let Ok(reply) = self.wpa.handle_message_1(data) {
                                        let _ = self.send_raw_80211_frame(&reply);
                                    }
                                } else if self.wpa.state == WpaState::WaitMsg3 {
                                    if let Ok(reply) = self.wpa.handle_message_3(data) {
                                        let _ = self.send_raw_80211_frame(&reply);
                                    }
                                }
                            }
                            0x0800 => {
                                let dhcp_handled = if data.len() >= 20 + 8 {
                                    let ip_ver_ihl = data[0];
                                    let ihl = (ip_ver_ihl & 0x0F) as usize * 4;
                                    let protocol = data[9];
                                    if protocol == 17 && data.len() >= ihl + 8 {
                                        let dst_port = u16::from_be_bytes([data[ihl + 2], data[ihl + 3]]);
                                        if dst_port == 68 {
                                            if let Some(ref mut dhcp) = self.dhcp {
                                                let dhcp_data = &data[ihl + 8..];
                                                if let Ok(msg_type) = dhcp.parse_response(dhcp_data) {
                                                    log::info!("iwlwifi: DHCP {} received", msg_type as u8);
                                                    if msg_type == DhcpMessageType::Offer {
                                                        let req = dhcp.build_request(dhcp.lease.ip_address, dhcp.lease.server_id);
                                                        let _ = self.send_raw_80211_frame(&req);
                                                    } else if msg_type == DhcpMessageType::Ack {
                                                        self.ip_address = dhcp.lease.ip_address;
                                                        self.subnet_mask = dhcp.lease.subnet_mask;
                                                        self.gateway = dhcp.lease.router;
                                                        self.dns_server = dhcp.lease.dns_server;
                                                        log::info!("iwlwifi: IP address assigned: {:?}", self.ip_address);
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
                            _ => {
                                self.rx_queue.push_back(data.to_vec());
                            }
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

    pub fn tick(&mut self) {
        if self.health.pre_mmio_access().is_err() {
            return;
        }

        let int_cause = match self.safe_read32(CSR_INT) {
            Some(v) => v,
            None => return,
        };
        if int_cause != 0 {
            unsafe {
                core::ptr::write_volatile(self.mmio.add(CSR_INT as usize), int_cause);
            }

            if (int_cause & (1 << 18)) != 0 {
                let raw_rx_head = match self.safe_read32(FH_RSCSR_CHNL0_RBDCB_RPTR_REG) {
                    Some(v) => v,
                    None => return,
                };
                self.rx_head = (raw_rx_head as usize) % RX_QUEUE_SIZE;
            }
            if (int_cause & (1 << 15)) != 0 {
                self.tx_tail = match self.safe_read32(FH_TX_CHNL0_WPTR) {
                    Some(v) => v,
                    None => return,
                } as usize;
                self.process_tx_queue();
            }
        }

        mmio::cache_flush_range(self.rx_dma_ring.virt(), core::mem::size_of::<RxDmaDesc>() * RX_QUEUE_SIZE);
        while self.rx_tail != self.rx_head {
            let desc_idx = self.rx_tail;
            let desc = self.rx_desc(desc_idx);
            if desc.len > 0 && desc_idx < self.rx_bufs.len() {
                let buf = &self.rx_bufs[desc_idx];
                let frame_len = (desc.len as usize).min(buf.len());
                let mut frame_data = alloc::vec![0; frame_len];
                buf.read_into(&mut frame_data);
                self.process_rx_frame(&frame_data);
            }

            let desc_mut = self.rx_desc_mut(desc_idx);
            desc_mut.len = MAX_FRAME_SIZE as u16;
            mmio::cache_flush(desc_mut as *const RxDmaDesc as *const u8);

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

    pub fn access_points(&self) -> &[AccessPoint] {
        &self.scan_results
    }

    pub fn wifi_status(&self) -> &bonder::wifi::WifiConnection {
        &self.wifi_conn
    }

    pub fn is_network_ready(&self) -> bool {
        self.wifi_conn.is_connected() && self.ip_address != [0u8; 4]
    }
}

// ── NetDevice implementation ──────

impl NetDevice for IwlWifiDevice {
    fn send_frame(&mut self, frame: &[u8]) -> Result<(), NetError> {
        if self.fw_state != FwState::Ready {
            return Err(NetError::NotInitialized);
        }

        if frame.len() > MAX_FRAME_SIZE {
            return Err(NetError::FrameTooLarge);
        }

        let _ = self.send_raw_80211_frame(frame);

        Ok(())
    }

    fn poll_frame(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        if self.fw_state != FwState::Ready {
            return Ok(None);
        }

        if let Some(rx_data) = self.rx_queue.pop_front() {
            if rx_data.len() > buf.len() {
                return Err(NetError::BufferTooSmall);
            }
            let len = rx_data.len();
            buf[..len].copy_from_slice(&rx_data);
            return Ok(Some(len));
        }

        Ok(None)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }
}

// ── WifiDriver trait implementation ─

impl crate::wifi::WifiDriver for IwlWifiDevice {
    fn create(
        ctx: &'static dyn crate::DriverContext,
        mmio_base: *mut u32,
        hw_rev: u32,
        device: crate::pci::PciDevice,
    ) -> Option<Box<dyn crate::wifi::WifiDriver>> {
        Self::init_from_mmio(ctx, mmio_base, hw_rev, device)
            .map(|dev| Box::new(dev) as Box<dyn crate::wifi::WifiDriver>)
    }

    fn tick(&mut self) {
        self.tick();
    }

    fn get_status(&self) -> bonder::wifi::WifiStatus {
        self.wifi_conn.status
    }

    fn start_scan(&mut self) -> bool {
        self.start_scan().is_ok()
    }

    fn get_scan_results(&self) -> Vec<AccessPoint> {
        self.scan_results.clone()
    }

    fn connect(&mut self, ssid: &Ssid, psk: Option<&str>) -> bool {
        self.connect(ssid, psk).is_ok()
    }

    fn disconnect(&mut self) {
        self.disconnect();
    }

    fn device_available(&self) -> bool {
        self.fw_state == FwState::Ready
    }

    fn connected_ssid(&self) -> Option<&Ssid> {
        self.wifi_conn.current_ssid.as_ref()
    }

    fn ip_address(&self) -> [u8; 4] {
        self.ip_address
    }

    fn load_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        IwlWifiDevice::load_firmware(self, fw_data)
    }

    fn start_firmware(&mut self, fw_data: &[u8]) -> Result<(), &'static str> {
        IwlWifiDevice::start_firmware(self, fw_data)
    }

    fn check_alive_nonblocking(&mut self, start_tsc: u64) -> Result<bool, &'static str> {
        IwlWifiDevice::check_alive_nonblocking(self, start_tsc)
    }

    fn send_init_commands(&mut self) -> Result<(), &'static str> {
        IwlWifiDevice::send_init_commands(self)
    }
}

// ── Free constructor ──────────────

pub fn try_create_iwl(
    ctx: &'static dyn crate::DriverContext,
    mmio: *mut u32,
    hw_rev: u32,
    device: crate::pci::PciDevice,
) -> Option<Box<dyn crate::wifi::WifiDriver>> {
    IwlWifiDevice::init_from_mmio(ctx, mmio, hw_rev, device)
        .map(|dev| Box::new(dev) as Box<dyn crate::wifi::WifiDriver>)
}
