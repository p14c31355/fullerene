//! Receive-ring, interrupt, and inbound protocol processing.

use alloc::vec::Vec;
use bonder::dhcp::DhcpMessageType;
use bonder::wifi::{self, Ssid};
use bonder::wpa::WpaState;
use core::fmt;

use crate::mmio;

use super::device::IwlWifiDevice;
use super::registers::*;
use super::types::*;

/// Bound a lost authentication/association response.  This is intentionally
/// longer than a normal management exchange but finite, so the UI cannot stay
/// in Authenticating forever when an AP or RX path drops the response.
const CONNECTION_WATCHDOG_TICKS: u32 = 4_000;

const RX_MPDU_RES_STATUS_CRC_OK: u32 = 1 << 0;
const RX_MPDU_RES_STATUS_OVERRUN_OK: u32 = 1 << 1;
const RX_MPDU_RES_STATUS_MIC_OK: u32 = 1 << 6;
const RX_MPDU_RES_STATUS_SEC_CCM_ENC: u32 = 2 << 8;
const RX_MPDU_RES_STATUS_SEC_ENC_MSK: u32 = 7 << 8;
const IEEE80211_CCMP_HDR_LEN: usize = 8;
const REPLY_TX_CMD: u8 = 0x1c;
const TX_STATUS_MSK: u16 = 0x00ff;
const TX_STATUS_SUCCESS: u16 = 0x01;
const TX_STATUS_DIRECT_DONE: u16 = 0x02;

fn active_management_tx_matches(
    frame_control: u8,
    state: IwlState,
    expected_sequence: Option<u16>,
    response_sequence: u16,
) -> bool {
    matches!(frame_control & 0xfc, 0xb0 | 0x00)
        && matches!(state, IwlState::AuthSent | IwlState::AssocSent)
        && expected_sequence == Some(response_sequence)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LegacyRxMpdu<'a> {
    frame: &'a [u8],
    status: u32,
    decrypted: bool,
    crypto_header_len: usize,
    crypto_trailer_len: usize,
}

/// Decode the gen1 `REPLY_RX_MPDU_CMD` payload used by 7265D firmware.
/// Linux reads the status word immediately after the `byte_count` bytes; it
/// is not part of the 802.11 frame and is not aligned separately.
fn decode_legacy_rx_mpdu(payload: &[u8]) -> Option<LegacyRxMpdu<'_>> {
    if payload.len() < 8 {
        return None;
    }
    let byte_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    let frame_end = 4usize.checked_add(byte_count)?;
    let status_end = frame_end.checked_add(4)?;
    if byte_count < 2 || status_end > payload.len() {
        return None;
    }
    let frame = &payload[4..frame_end];
    let status = u32::from_le_bytes(payload[frame_end..status_end].try_into().ok()?);

    // mac80211 ultimately rejects bad-FCS/FIFO frames outside monitor mode.
    if status & (RX_MPDU_RES_STATUS_CRC_OK | RX_MPDU_RES_STATUS_OVERRUN_OK)
        != RX_MPDU_RES_STATUS_CRC_OK | RX_MPDU_RES_STATUS_OVERRUN_OK
    {
        return None;
    }

    let protected = frame[1] & 0x40 != 0;
    if !protected || status & RX_MPDU_RES_STATUS_SEC_ENC_MSK == 0 {
        return Some(LegacyRxMpdu {
            frame,
            status,
            decrypted: false,
            crypto_header_len: 0,
            crypto_trailer_len: 0,
        });
    }

    // This driver only negotiates CCMP. Linux v4.14 accepts a hardware-CCMP
    // result only when MIC_OK is set, marks it decrypted, and tells mac80211
    // that the still-present crypto header is eight bytes long.
    if status & RX_MPDU_RES_STATUS_SEC_ENC_MSK != RX_MPDU_RES_STATUS_SEC_CCM_ENC
        || status & RX_MPDU_RES_STATUS_MIC_OK == 0
    {
        return None;
    }
    Some(LegacyRxMpdu {
        frame,
        status,
        decrypted: true,
        crypto_header_len: IEEE80211_CCMP_HDR_LEN,
        crypto_trailer_len: 8,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LegacyTxResponse {
    frame_count: u8,
    failure_rts: u8,
    failure_frame: u8,
    initial_rate: u32,
    wireless_media_time: u16,
    frame_control: u16,
    status: u16,
}

/// Decode `iwl_mvm_tx_resp_v3`, the non-TFH response used by 7265D.
fn decode_legacy_tx_response(payload: &[u8]) -> Option<LegacyTxResponse> {
    if payload.len() < 40 {
        return None;
    }
    Some(LegacyTxResponse {
        frame_count: payload[0],
        failure_rts: payload[2],
        failure_frame: payload[3],
        initial_rate: u32::from_le_bytes(payload[4..8].try_into().ok()?),
        wireless_media_time: u16::from_le_bytes(payload[8..10].try_into().ok()?),
        frame_control: u16::from_le_bytes(payload[34..36].try_into().ok()?),
        status: u16::from_le_bytes(payload[36..38].try_into().ok()?),
    })
}

struct RxHexBytes<'a>(&'a [u8]);

impl fmt::Display for RxHexBytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index != 0 {
                f.write_str(" ")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Match a firmware command response, optionally requiring the exact q9
/// sequence that was placed in the host-command header. Runtime setup can
/// reuse an opcode while older responses are still present in RX, so opcode
/// and group alone are not a sufficient synchronous-command boundary.
const fn command_response_matches(
    packet_opcode: u8,
    packet_group: u8,
    packet_sequence: u16,
    expected_opcode: u8,
    expected_group: u8,
    expected_sequence: Option<u16>,
) -> bool {
    let group_matches = packet_group == expected_group
        || (expected_opcode == LegacyCmd::MccUpdate as u8
            && packet_opcode == LegacyCmd::MccUpdate as u8);
    let sequence_matches = match expected_sequence {
        Some(sequence) => packet_sequence == sequence,
        None => true,
    };
    packet_opcode == expected_opcode && group_matches && sequence_matches
}

impl IwlWifiDevice {
    /// Advance the management-frame connection watchdog before touching the
    /// device.  A PCIe/MMIO health failure can make the RX poll return early;
    /// the user-facing state must still leave `Authenticating`/`Associating`
    /// instead of remaining there forever.
    fn advance_connection_watchdog(&mut self) {
        let management_pending = matches!(self.iwl_state, IwlState::AuthSent | IwlState::AssocSent);
        let handshake_pending = self.wifi_conn.status == bonder::wifi::WifiStatus::Handshake;
        if management_pending || handshake_pending {
            self.connection_watchdog_ticks = self.connection_watchdog_ticks.saturating_add(1);
            if self.connection_watchdog_ticks > CONNECTION_WATCHDOG_TICKS {
                let phase = if handshake_pending {
                    "WPA2 handshake"
                } else if self.iwl_state == IwlState::AuthSent {
                    "authentication"
                } else {
                    "association"
                };
                if handshake_pending {
                    self.wpa_failed("WPA2 handshake timeout");
                } else if self.iwl_state == IwlState::AuthSent && self.advance_authentication_plan()
                {
                    // The bounded TX solver selected and submitted the next
                    // queue plan. Keep the public state in Authenticating.
                } else {
                    self.iwl_state = IwlState::Disconnected;
                    self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                    self.wifi_conn.error_msg = Some(alloc::format!("{} response timeout", phase));
                }
                self.connection_watchdog_ticks = 0;
                log::warn!("iwlwifi: {} response timeout", phase);
            }
        } else {
            self.connection_watchdog_ticks = 0;
        }
    }

    fn save_phy_db_notification(&mut self, payload: &[u8]) {
        if payload.len() < 4 {
            return;
        }
        let section_type = u16::from_le_bytes([payload[0], payload[1]]);
        let section_len = u16::from_le_bytes([payload[2], payload[3]]) as usize;
        let available = payload.len().saturating_sub(4);
        let length = core::cmp::min(section_len, available);
        if length == 0 {
            return;
        }
        self.phy_db_sections
            .push((section_type, payload[4..4 + length].to_vec()));
        log::debug!(
            "iwlwifi: init.phy_db section={} bytes={}",
            section_type,
            length,
        );
    }

    /// Wait for the RX ALIVE notification after the CSR ALIVE edge.
    ///
    /// On gen1 hardware the CSR bit only announces that firmware reached its
    /// boot interrupt. Linux completes the transition using REPLY_ALIVE from
    /// the pre-armed RX ring, which also supplies the scheduler SRAM base.
    /// Explicitly drain that notification before submitting the first host
    /// command or touching the TX scheduler.
    pub(super) fn wait_for_alive_rx(&mut self) -> Result<(), crate::DriverError> {
        const ALIVE_TIMEOUT_US: u64 = 500_000;
        const IWL_ALIVE_STATUS_OK: u16 = 0xcafe;

        let deadline_tsc = unsafe { core::arch::x86_64::_rdtsc() }
            .saturating_add(crate::timing::ticks_per_us().saturating_mul(ALIVE_TIMEOUT_US));
        loop {
            if let Some(payload) = self.poll_init_notification(
                LegacyCmd::ReplyAlive as u8,
                GroupId::Legacy as u8,
                None,
                deadline_tsc,
            )? {
                if payload.len() < 44 {
                    log::error!(
                        "iwlwifi: firmware ALIVE RX payload too short: {} bytes",
                        payload.len(),
                    );
                    return Err(crate::DriverError::Protocol);
                }
                let status = u16::from_le_bytes([payload[0], payload[1]]);
                if status != IWL_ALIVE_STATUS_OK {
                    log::error!("iwlwifi: firmware ALIVE RX rejected status={:#06x}", status,);
                    // The v3 ALIVE payload carries the authoritative LMAC
                    // error-table address even when firmware reports DEAD.
                    // `poll_init_notification()` records it before returning
                    // this packet, so preserve the assertion evidence before
                    // aborting initialization just as Linux does.
                    self.log_firmware_error_table("init.alive.dead");
                    return Err(crate::DriverError::Protocol);
                }
                log::info!(
                    "iwlwifi: firmware ALIVE RX consumed status={:#06x} scd_base={:#010x}",
                    status,
                    self.alive_scd_base_addr,
                );
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    /// Poll one command response while the INIT image is running.
    ///
    /// The normal service tick intentionally processes RX only in `Ready`
    /// state.  INIT commands therefore need a small private RX pump: NVM
    /// responses and `INIT_COMPLETE_NOTIF` arrive through the same FH RBD
    /// ring, but before the runtime image is installed.
    pub(super) fn poll_init_notification(
        &mut self,
        opcode: u8,
        group: u8,
        sequence: Option<u16>,
        deadline_tsc: u64,
    ) -> Result<Option<Vec<u8>>, crate::DriverError> {
        const FRAME_ALIGN: usize = 64;
        let now_tsc = unsafe { core::arch::x86_64::_rdtsc() };

        // CSR_INT is sticky on this generation.  A firmware assertion can
        // therefore stop the command scheduler while the TX descriptor still
        // advances normally.  Do this check before the deadline check so a
        // rejected command is reported as SW_ERR/HW_ERR, rather than as a
        // misleading missing response after the full timeout.
        let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
        if csr_int == !0 {
            log::error!(
                "iwlwifi: init.rx.error device_gone opcode=0x{:02x} group=0x{:02x}",
                opcode,
                group,
            );
            return Err(crate::DriverError::DeviceNotFound);
        }
        let mut firmware_error = false;
        let mut firmware_error_command = "init.rx.firmware_failure";
        if csr_int & (CSR_INT_BIT_SW_ERR | CSR_INT_BIT_HW_ERR) != 0 {
            let gp1 = self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0);
            let gp_driver = self.safe_read32(CSR_GP_DRIVER).unwrap_or(!0);
            let reset = self.safe_read32(CSR_RESET).unwrap_or(!0);
            let gp_cntrl = self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0);
            let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
            let rptr = self
                .read_prph(scd_queue_rdptr(self.command_queue()))
                .unwrap_or(!0);
            log::error!(
                "iwlwifi: init.rx.error firmware_failure opcode=0x{:02x} group=0x{:02x} CSR_INT={:#010x} FH_INT={:#010x} UCODE_GP1={:#010x} GP_DRIVER={:#010x} RESET={:#010x} GP_CNTRL={:#010x} SCD_RDPTR={:#010x}",
                opcode,
                group,
                csr_int,
                fh_int,
                gp1,
                gp_driver,
                reset,
                gp_cntrl,
                rptr,
            );
            self.fw_state = FwState::Error;
            // Continue draining the current RBD below.  Some firmware builds
            // emit REPLY_ERROR together with SW_ERR; returning here would
            // discard that command-specific reason.
            firmware_error = true;
        }

        // ALIVE arrives as a high-priority RX event (FH bit 30) together with
        // RX channel 0. Acknowledge the complete Linux gen1 RX mask; clearing
        // channel 0 alone leaves the aggregate interrupt asserted.
        if csr_int & (CSR_INT_BIT_FH_RX | CSR_INT_BIT_SW_RX | CSR_INT_BIT_RX_PERIODIC) != 0 {
            let fh_cause = self.safe_read32(CSR_FH_INT).unwrap_or(0);
            self.write_mmio32(CSR_FH_INT, fh_cause & CSR_FH_INT_RX_MASK);
            self.write_mmio32(
                CSR_INT,
                csr_int & (CSR_INT_BIT_FH_RX | CSR_INT_BIT_SW_RX | CSR_INT_BIT_RX_PERIODIC),
            );
        }

        let gp1 = self.safe_read32(CSR_UCODE_GP1).unwrap_or(0);
        if gp1 & CSR_UCODE_GP1_BIT_CMD_BLOCKED != 0 {
            log::error!(
                "iwlwifi: init.rx.error command_blocked opcode=0x{:02x} group=0x{:02x} UCODE_GP1={:#010x} CSR_INT={:#010x}",
                opcode,
                group,
                gp1,
                csr_int,
            );
            firmware_error_command = "init.rx.command_blocked";
            firmware_error = true;
        }

        if now_tsc.wrapping_sub(deadline_tsc) < (1u64 << 63) {
            let status = self.rx_status();
            let closed_rb =
                unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(status.closed_rb_num)) };
            let closed_fr =
                unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(status.closed_fr_num)) };
            let finished_rb =
                unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(status.finished_rb_num)) };
            let finished_fr =
                unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(status.finished_fr_num)) };
            log::warn!(
                "iwlwifi: init.rx.timeout opcode=0x{:02x} group=0x{:02x} closed_rb={} closed_fr={} finished_rb={} finished_fr={} rx_tail={} CSR_INT={:#010x} FH_INT={:#010x} RX_RDPTR={:#010x} RX_WPTR={:#010x}",
                opcode,
                group,
                closed_rb,
                closed_fr,
                finished_rb,
                finished_fr,
                self.rx_tail,
                self.safe_read32(CSR_INT).unwrap_or(!0),
                self.safe_read32(CSR_FH_INT).unwrap_or(!0),
                self.safe_read32(FH_RSCSR_CHNL0_RDPTR_REG).unwrap_or(!0),
                self.safe_read32(FH_RSCSR_CHNL0_RBDCB_WPTR_REG)
                    .unwrap_or(!0),
            );
            self.log_firmware_error_table("init.rx.timeout");
            return Err(if firmware_error {
                crate::DriverError::Protocol
            } else {
                crate::DriverError::TimedOut
            });
        }

        self.health
            .check()
            .map_err(|_| crate::DriverError::DeviceNotFound)?;

        mmio::cache_flush_range(
            self.rx_status() as *const RxDmaStatus as usize,
            core::mem::size_of::<RxDmaStatus>(),
        );
        self.rx_head = (self.rx_status().closed_rb_num as usize) & (RX_QUEUE_SIZE - 1);
        let mut matched = None;
        let mut processed = 0usize;

        while self.rx_tail != self.rx_head {
            processed += 1;
            let index = self.rx_tail;
            if index < self.rx_bufs.len() {
                let buf = &self.rx_bufs[index];
                let mut frame = alloc::vec![0; buf.len()];
                buf.read_into(&mut frame);
                let mut offset = 0usize;
                while offset + 8 <= frame.len() {
                    let len_n_flags = u32::from_le_bytes([
                        frame[offset],
                        frame[offset + 1],
                        frame[offset + 2],
                        frame[offset + 3],
                    ]);
                    let packet_len = (len_n_flags as usize & 0x3fff).saturating_add(4);
                    if packet_len < 8 || offset + packet_len > frame.len() {
                        break;
                    }
                    let packet = &frame[offset..offset + packet_len];
                    let payload = &packet[8..];
                    let packet_sequence = u16::from_le_bytes([packet[6], packet[7]]);
                    let payload_preview = &payload[..core::cmp::min(payload.len(), 32)];
                    if packet[4] == LegacyCmd::ReplyAlive as u8
                        && packet[5] == GroupId::Legacy as u8
                    {
                        self.record_alive_notification(payload);
                    }
                    log::debug!(
                        "iwlwifi: init.rx.packet opcode=0x{:02x} group=0x{:02x} len={} payload_len={} payload_preview={} expected_opcode=0x{:02x} expected_group=0x{:02x}",
                        packet[4],
                        packet[5],
                        packet_len,
                        payload.len(),
                        RxHexBytes(payload_preview),
                        opcode,
                        group,
                    );
                    if opcode == LegacyCmd::MccUpdate as u8 {
                        log::info!(
                            "iwlwifi: init.rx.mcc_packet opcode=0x{:02x} group=0x{:02x} len={} payload={}",
                            packet[4],
                            packet[5],
                            packet_len,
                            RxHexBytes(payload),
                        );
                    }
                    if packet[4] == LegacyCmd::CalibResNotifPhyDb as u8
                        && packet[5] == GroupId::Legacy as u8
                    {
                        self.save_phy_db_notification(&packet[8..]);
                    }
                    if packet[4] == LegacyCmd::TimeEventNotification as u8
                        && packet[5] == GroupId::Legacy as u8
                    {
                        if payload.len() >= 24 {
                            log::info!(
                                "iwlwifi: time_event.notification timestamp={:#010x} session_id={:#010x} unique_id={:#010x} id_and_color={:#010x} action={:#010x} status={:#010x} payload_hex={}",
                                u32::from_le_bytes([
                                    payload[0], payload[1], payload[2], payload[3]
                                ]),
                                u32::from_le_bytes([
                                    payload[4], payload[5], payload[6], payload[7]
                                ]),
                                u32::from_le_bytes([
                                    payload[8],
                                    payload[9],
                                    payload[10],
                                    payload[11]
                                ]),
                                u32::from_le_bytes([
                                    payload[12],
                                    payload[13],
                                    payload[14],
                                    payload[15]
                                ]),
                                u32::from_le_bytes([
                                    payload[16],
                                    payload[17],
                                    payload[18],
                                    payload[19]
                                ]),
                                u32::from_le_bytes([
                                    payload[20],
                                    payload[21],
                                    payload[22],
                                    payload[23]
                                ]),
                                RxHexBytes(payload),
                            );
                        } else {
                            log::warn!(
                                "iwlwifi: time_event.notification short payload_len={} payload_hex={}",
                                payload.len(),
                                RxHexBytes(payload),
                            );
                        }
                    }
                    // MCC_UPDATE is a legacy command, but older 7000-series
                    // firmware has emitted its response through either the
                    // legacy or long notification namespace. Match its
                    // opcode independently of the namespace; all other
                    // command responses remain strict.
                    if command_response_matches(
                        packet[4],
                        packet[5],
                        packet_sequence,
                        opcode,
                        group,
                        sequence,
                    ) {
                        matched = Some(payload.to_vec());
                    } else if packet[4] == LegacyCmd::ReplyError as u8 {
                        let error_type = payload
                            .get(0..4)
                            .and_then(|bytes| bytes.try_into().ok())
                            .map(u32::from_le_bytes);
                        let bad_cmd = payload.get(4).copied();
                        let bad_seq = payload
                            .get(6..8)
                            .and_then(|bytes| bytes.try_into().ok())
                            .map(u16::from_le_bytes);
                        let service = payload
                            .get(8..12)
                            .and_then(|bytes| bytes.try_into().ok())
                            .map(u32::from_le_bytes);
                        log::warn!(
                            "iwlwifi: INIT firmware command error while waiting for opcode=0x{:02x} group=0x{:02x} error_type={:?} bad_cmd={:?} bad_seq={:?} service={:?} payload_len={} payload={}",
                            opcode,
                            group,
                            error_type,
                            bad_cmd,
                            bad_seq,
                            service,
                            payload.len(),
                            RxHexBytes(payload_preview),
                        );
                        return Err(crate::DriverError::Protocol);
                    }
                    offset += packet_len.next_multiple_of(FRAME_ALIGN);
                }
            }
            self.rx_tail = (self.rx_tail + 1) % RX_QUEUE_SIZE;
            if matched.is_some() {
                break;
            }
        }

        self.restock_rx_buffers(processed);
        if firmware_error {
            // An assertion may enqueue a fresh ALIVE notification in the
            // same RX batch.  Record its error-table pointer first, then
            // read the table; doing this before draining the RBD would
            // incorrectly report pointer_missing.
            self.log_firmware_error_table(firmware_error_command);
            return Err(crate::DriverError::Protocol);
        }
        if let Some(payload) = matched {
            log::debug!(
                "iwlwifi: init.rx.match opcode=0x{:02x} group=0x{:02x} payload={}",
                opcode,
                group,
                payload.len(),
            );
            return Ok(Some(payload));
        }
        Ok(None)
    }

    fn process_rx_frame(
        &mut self,
        frame: &[u8],
        rx_decrypted: bool,
        crypto_header_len: usize,
        crypto_trailer_len: usize,
    ) {
        if frame.len() < 2 {
            return;
        }

        let frame_type = (frame[0] & 0x0C) >> 2;
        let subtype = (frame[0] >> 4) & 0x0F;
        match (frame_type, subtype) {
            (0, 5) | (0, 8) => {
                // Accept beacons both during an active scan and for a short
                // grace period after the scan-complete notification.  The
                // firmware can deliver the scan-complete notification before
                // the last few beacons reach the RX ring, and the old
                // `iwl_state == Scanning` guard silently discarded them.
                if self.iwl_state == IwlState::Scanning
                    || self.scan_pending
                    || self.scan_result_grace_ticks > 0
                {
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
                            self.wifi_conn.status = bonder::wifi::WifiStatus::Associating;
                            self.connection_watchdog_ticks = 0;
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
                            self.auth_tx_acknowledged = None;
                            if let Err(error) = self.send_raw_80211_frame(&assoc) {
                                self.iwl_state = IwlState::Disconnected;
                                self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                                self.wifi_conn.error_msg = Some(alloc::format!(
                                    "association frame transmission failed: {:?}",
                                    error
                                ));
                                log::warn!(
                                    "iwlwifi: failed to send association frame: {:?}",
                                    error
                                );
                            } else {
                                log::info!("iwlwifi: auth successful, associating");
                            }
                        } else {
                            self.iwl_state = IwlState::Disconnected;
                            self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                            self.connection_watchdog_ticks = 0;
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
                            let bssid = [
                                frame[10], frame[11], frame[12], frame[13], frame[14], frame[15],
                            ];
                            // The MAC context is kept unassociated until the
                            // AP accepts the association. Publish the AID
                            // now so firmware can correctly handle the
                            // subsequent protected handshake/data traffic.
                            let channel = self
                                .scan_results
                                .iter()
                                .find(|ap| ap.bssid == bssid)
                                .map(|ap| ap.channel)
                                .unwrap_or(1);
                            let ap_timing = self
                                .scan_results
                                .iter()
                                .find(|ap| ap.bssid == bssid)
                                .map(|ap| {
                                    (
                                        ap.beacon_interval,
                                        ap.dtim_period,
                                        ap.dtim_count,
                                        ap.beacon_timestamp,
                                        ap.device_timestamp,
                                    )
                                })
                                .unwrap_or((100, 0, 0, 0, 0));
                            let mac_context =
                                MacContextCmd::sta_for_bssid_on_channel(self.mac, bssid, channel)
                                    .associated_with_ap(
                                        aid,
                                        ap_timing.0,
                                        ap_timing.1,
                                        ap_timing.2,
                                        ap_timing.3,
                                        ap_timing.4,
                                    );
                            let mac_context_bytes = unsafe { super::as_bytes(&mac_context) };
                            if let Err(error) = self.send_hcmd(
                                LegacyCmd::MacContext as u8,
                                GroupId::Legacy as u8,
                                mac_context_bytes,
                            ) {
                                self.iwl_state = IwlState::Disconnected;
                                self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                                self.connection_watchdog_ticks = 0;
                                self.wifi_conn.error_msg = Some(alloc::format!(
                                    "associated MAC context setup failed: {:?}",
                                    error
                                ));
                                log::warn!(
                                    "iwlwifi: failed to publish associated MAC context: {:?}",
                                    error
                                );
                                return;
                            }

                            // Linux's AUTH -> ASSOC station transition also
                            // updates the firmware peer entry with the AID.
                            // Keep this behind MAC_CONTEXT on the same command
                            // queue; synchronously polling RX from inside this
                            // RX-buffer walk would re-enter ring accounting.
                            let associated_peer = AddStaCmdV7::associated_peer(0, 0, aid);
                            if let Err(error) = self.send_hcmd(
                                LegacyCmd::AddSta as u8,
                                GroupId::Legacy as u8,
                                unsafe { super::as_bytes(&associated_peer) },
                            ) {
                                self.iwl_state = IwlState::Disconnected;
                                self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                                self.connection_watchdog_ticks = 0;
                                self.wifi_conn.error_msg = Some(alloc::format!(
                                    "associated peer station setup failed: {:?}",
                                    error
                                ));
                                log::warn!(
                                    "iwlwifi: failed to publish associated peer station: {:?}",
                                    error
                                );
                                return;
                            }
                            self.iwl_state = IwlState::Connected;
                            self.connection_watchdog_ticks = 0;
                            self.wifi_conn.status = if self.wpa_required {
                                // Association is not an encrypted connection.
                                // Do not expose Connected or start DHCP until
                                // the 4-way handshake installs CCMP keys.
                                bonder::wifi::WifiStatus::Handshake
                            } else {
                                bonder::wifi::WifiStatus::Connected
                            };
                            self.wifi_conn.current_bssid = Some(bssid);
                            log::info!(
                                "iwlwifi: association accepted AID={} link_status={}",
                                aid & 0x3fff,
                                if self.wpa_required {
                                    "Handshake"
                                } else {
                                    "Connected"
                                },
                            );

                            if !self.wpa_required {
                                self.start_dhcp(aid);
                            }
                        } else {
                            self.iwl_state = IwlState::Disconnected;
                            self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
                            self.connection_watchdog_ticks = 0;
                            log::warn!("iwlwifi: assoc failed with status {}", status_code);
                        }
                    }
                }
            }
            (2, subtype) => {
                let header_len = if subtype & 0x08 != 0 { 26 } else { 24 };
                if frame.len() > header_len {
                    let protected = frame[1] & 0x40 != 0;
                    let llc_offset = header_len + if protected { crypto_header_len } else { 0 };
                    let payload_end = frame.len().saturating_sub(crypto_trailer_len);
                    if payload_end > llc_offset + 8 {
                        let ether_type =
                            u16::from_be_bytes([frame[llc_offset + 6], frame[llc_offset + 7]]);
                        let data = &frame[llc_offset + 8..payload_end];
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
                            // Before the handshake completes, discard every data
                            // frame. Afterward, require the authenticated CCMP
                            // result from the firmware status trailer; the header's
                            // Protected bit alone does not prove decryption/MIC.
                            if !self.wpa_keys_installed || !rx_decrypted {
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
                                            self.connection_watchdog_ticks = 0;
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
                                            self.connection_watchdog_ticks = 0;
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
                                                        log::info!(
                                                            "iwlwifi: network ready link_status=Connected ipv4={:?} gateway={:?} dns={:?}",
                                                            self.ip_address,
                                                            self.gateway,
                                                            self.dns_server,
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
        self.wpa_key_pending_sequences = [None; 2];
        self.pending_wpa_message4 = None;
        self.wifi_conn.status = bonder::wifi::WifiStatus::Error;
        self.wifi_conn.error_msg = Some(alloc::string::String::from(reason));
        log::warn!("iwlwifi: WPA2 handshake failed: {}", reason);
    }

    /// Service device interrupts and consume completed receive descriptors.
    pub fn tick(&mut self) {
        // Keep this before the PCIe/MMIO probe: a failed probe must not leave
        // the UI in an unbounded Authenticating or Associating state.
        self.advance_connection_watchdog();
        if let Err(error) = self.health.pre_mmio_access() {
            if self.scan_pending {
                // Do not silently turn a live-scan PCIe/link check failure
                // into a missing firmware notification.  `send_hcmd()` has
                // already accepted the scan request at this point, so this
                // is the only evidence that the RX service path was unable
                // to poll the device.  Reuse the watchdog counter to rate
                // limit the message; it is also the elapsed service-tick
                // count used by the scan watchdog below.
                if self.scan_channel == 0 || self.scan_channel % 256 == 0 {
                    log::warn!(
                        "iwlwifi: scan RX poll skipped by PCI health: error={} scan_ticks={}",
                        error,
                        self.scan_channel,
                    );
                }
                self.scan_channel = self.scan_channel.saturating_add(1);
            }
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
            self.read_prph(scd_queue_rdptr(self.command_queue()))
                .map(|value| value as usize)
        } else {
            None
        };
        let polled_data_tx_tail = if self.fw_state == FwState::Ready {
            self.read_prph(scd_queue_rdptr(self.traffic_queue()))
                .map(|value| value as usize)
        } else {
            None
        };
        let tx_tail_before_poll = self.tx_tail;
        if let Some(hardware_tail) = polled_tx_tail {
            self.update_tx_tail(hardware_tail);
            if self.tx_tail != tx_tail_before_poll {
                log::debug!(
                    "iwlwifi: TX completion progress scd_rptr={} tx_tail={} tx_head={}",
                    hardware_tail & (TX_QUEUE_SIZE - 1),
                    self.tx_tail & (TX_QUEUE_SIZE - 1),
                    self.tx_head & (TX_QUEUE_SIZE - 1),
                );
                self.process_tx_queue();
            }
        }
        let data_tx_tail_before_poll = self.tx_data_tail;
        if let Some(hardware_tail) = polled_data_tx_tail {
            self.update_data_tx_tail(hardware_tail);
            if self.tx_data_tail != data_tx_tail_before_poll {
                log::debug!(
                    "iwlwifi: TX data completion progress scd_rptr={} tx_data_tail={} tx_data_head={}",
                    hardware_tail & (TX_QUEUE_SIZE - 1),
                    self.tx_data_tail & (TX_QUEUE_SIZE - 1),
                    self.tx_data_head & (TX_QUEUE_SIZE - 1),
                );
                self.process_tx_queue();
            }
        }

        // The first management TX log is emitted immediately after ringing
        // q5, where an rptr of zero is expected.  Sample it again after the
        // scheduler has had time to fetch the descriptor.  q0 is explicitly
        // activated through SCD_EN_CTRL, but a DQA traffic queue relies on
        // SCD_GP_CTRL auto-active mode, so include both controls here.
        let management_pending = matches!(self.iwl_state, IwlState::AuthSent | IwlState::AssocSent);
        if management_pending
            && (self.connection_watchdog_ticks == 64 || self.connection_watchdog_ticks % 512 == 0)
        {
            let queue = self.traffic_queue();
            let scd_status = self.read_prph(scd_queue_status(queue)).unwrap_or(!0);
            let fifo = scd_status & 0x7;
            let scd_en = self.read_prph(SCD_EN_CTRL).unwrap_or(!0);
            let scd_gp = self.read_prph(SCD_GP_CTRL).unwrap_or(!0);
            // SCD SRAM state: for DQA queues the firmware sets up the SCD
            // context, queuechain, and aggr registers via SCD_QUEUE_CFG.
            // Read them back to diagnose why the scheduler is not fetching
            // q5 TFDs.  SCD_QUEUE_WRPTR confirms the doorbell reached the
            // scheduler.  Context word 1 holds WIN_SIZE (bits 0-6) and
            // FRAME_LIMIT (bits 16-22); a zero window prevents fetching.
            let scd_wrptr = self.read_prph(scd_queue_wrptr(queue)).unwrap_or(!0);
            let scd_rptr_hw = self.read_prph(scd_queue_rdptr(queue)).unwrap_or(!0);
            let scd_chain = self.read_prph(SCD_QUEUECHAIN_SEL).unwrap_or(!0);
            let scd_aggr = self.read_prph(SCD_AGGR_SEL).unwrap_or(!0);
            let scd_base = self.alive_scd_base_addr;
            let ctx0 = self
                .read_mem32(scd_base + scd_context_queue(queue))
                .unwrap_or(!0);
            let ctx1 = self
                .read_mem32(scd_base + scd_context_queue(queue) + 4)
                .unwrap_or(!0);
            let scd_txfact = self.read_prph(SCD_TXFACT).unwrap_or(!0);
            let fh_tx_trb = self.safe_read32(fh_tx_trb_channel(fifo)).unwrap_or(!0);
            log::info!(
                "iwlwifi: management TX scheduler poll tick={} phase={} queue={} sw_head={} sw_tail={} wrptr={:#010x} rptr={:#010x} status={:#010x} fifo={} scd_en={:#010x} scd_gp={:#010x} cbbc={:#010x} scd_txfact={:#010x} fh_tx_trb={:#010x} fifo_cfg={:#010x} fifo_credit={:#010x} fifo_buf={:#010x} tx_status={:#010x} tx_error={:#010x} gp_cntrl={:#010x} gp1={:#010x}",
                self.connection_watchdog_ticks,
                if self.iwl_state == IwlState::AuthSent {
                    "authentication"
                } else {
                    "association"
                },
                queue,
                self.tx_data_head & (TX_QUEUE_SIZE - 1),
                self.tx_data_tail & (TX_QUEUE_SIZE - 1),
                self.safe_read32(HBUS_TARG_WRPTR).unwrap_or(!0),
                polled_data_tx_tail.map(|tail| tail as u32).unwrap_or(!0),
                scd_status,
                fifo,
                scd_en,
                scd_gp,
                self.safe_read32(fh_mem_cbbc_queue(queue)).unwrap_or(!0),
                scd_txfact,
                fh_tx_trb,
                self.safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + fifo * (0x20 / 4))
                    .unwrap_or(!0),
                self.safe_read32(FH_TCSR_CHNL_TX_CREDIT_BASE + fifo * (0x20 / 4))
                    .unwrap_or(!0),
                self.safe_read32(FH_TCSR_CHNL_TX_BUF_STS_BASE + fifo * (0x20 / 4))
                    .unwrap_or(!0),
                self.safe_read32(FH_TSSR_TX_STATUS_REG).unwrap_or(!0),
                self.safe_read32(FH_TSSR_TX_ERROR_REG).unwrap_or(!0),
                self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0),
                self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0),
            );
            log::info!(
                "iwlwifi: SCD SRAM queue={} hw_wrptr={:#010x} hw_rdptr={:#010x} queuechain={:#010x} aggr_sel={:#010x} ctx0={:#010x} ctx1={:#010x} win_size={} frame_limit={}",
                queue,
                scd_wrptr & 0xff,
                scd_rptr_hw & 0xff,
                scd_chain,
                scd_aggr,
                ctx0,
                ctx1,
                ctx1 & 0x7f,
                (ctx1 >> 16) & 0x7f,
            );
            // Read the SCD translation table entry and TX status SRAM entry
            // for q5. The translation table maps RA/TID to queue; Linux
            // leaves it zero for a non-aggregate DQA management queue, so a
            // stale non-zero value would be suspicious. The TX status entry
            // is firmware-owned aggregation/reclaim state and may remain
            // zero before the first fetch/completion; zero is not itself a
            // queue-activation failure.
            let trans_tbl = self
                .read_mem32(scd_base + scd_trans_tbl_offset_queue(queue))
                .unwrap_or(!0);
            let tx_stts = self
                .read_mem32(scd_base + scd_tx_stts_queue_offset(queue))
                .unwrap_or(!0);
            log::info!(
                "iwlwifi: SCD SRAM q5 extra trans_tbl={:#010x} tx_stts={:#010x}",
                trans_tbl,
                tx_stts,
            );
        }

        let int_cause = match self.safe_read32(CSR_INT) {
            Some(value) => value,
            None => return,
        };
        if self.scan_pending && (self.scan_channel == 0 || self.scan_channel % 512 == 0) {
            let scd_rptr = self
                .read_prph(scd_queue_rdptr(self.command_queue()))
                .unwrap_or(!0);
            let scd_status = self
                .read_prph(scd_queue_status(self.command_queue()))
                .unwrap_or(!0);
            let tx_cfg = self
                .safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + SCD_QUEUE_STTS_FIFO_COMMAND * (0x20 / 4))
                .unwrap_or(!0);
            let tx_credit = self
                .safe_read32(FH_TCSR_CHNL_TX_CREDIT_BASE + SCD_QUEUE_STTS_FIFO_COMMAND * (0x20 / 4))
                .unwrap_or(!0);
            let tx_buf_sts = self
                .safe_read32(
                    FH_TCSR_CHNL_TX_BUF_STS_BASE + SCD_QUEUE_STTS_FIFO_COMMAND * (0x20 / 4),
                )
                .unwrap_or(!0);
            let rx_closed = self.rx_status().closed_rb_num;
            let rx_finished = self.rx_status().finished_rb_num;
            let rx_rptr = self.safe_read32(FH_RSCSR_CHNL0_RDPTR_REG).unwrap_or(!0);
            let rx_wptr = self
                .safe_read32(FH_RSCSR_CHNL0_RBDCB_WPTR_REG)
                .unwrap_or(!0);
            log::info!(
                "iwlwifi: scan poll tick={} CSR_INT={:#010x} FH_INT={:#010x} tx_head={} tx_tail={} scd_rptr={} scd_status={:#010x} cmd_fifo_cfg={:#010x} cmd_fifo_credit={:#010x} cmd_fifo_buf_sts={:#010x} tx_status={:#010x} tx_error={:#010x} rx_closed={} rx_finished={} rx_head={} rx_tail={} rx_rptr={:#010x} rx_wptr={:#010x}",
                self.scan_channel,
                int_cause,
                self.safe_read32(CSR_FH_INT).unwrap_or(!0),
                self.tx_head & (TX_QUEUE_SIZE - 1),
                self.tx_tail & (TX_QUEUE_SIZE - 1),
                scd_rptr,
                scd_status,
                tx_cfg,
                tx_credit,
                tx_buf_sts,
                self.safe_read32(FH_TSSR_TX_STATUS_REG).unwrap_or(!0),
                self.safe_read32(FH_TSSR_TX_ERROR_REG).unwrap_or(!0),
                rx_closed,
                rx_finished,
                self.rx_head,
                self.rx_tail,
                rx_rptr,
                rx_wptr,
            );
        }
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
                .safe_read32(FH_TCSR_CHNL_TX_CONFIG_BASE + SCD_QUEUE_STTS_FIFO_COMMAND * (0x20 / 4))
                .unwrap_or(!0);
            let csr_gp1 = self.safe_read32(CSR_UCODE_GP1).unwrap_or(!0);
            let csr_gp_driver = self.safe_read32(CSR_GP_DRIVER).unwrap_or(!0);
            let csr_reset = self.safe_read32(CSR_RESET).unwrap_or(!0);
            let csr_gp_cntrl = self.safe_read32(CSR_GP_CNTRL).unwrap_or(!0);
            let scd_rptr = self
                .read_prph(scd_queue_rdptr(self.command_queue()))
                .unwrap_or(!0);
            let scd_status = self
                .read_prph(scd_queue_status(self.command_queue()))
                .unwrap_or(!0);
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
            // CSR_INT is cleared using the upstream gen1 formula:
            // acknowledge the observed causes plus every currently masked
            // cause. Writing only int_cause leaves HW_ERR latched on this
            // hardware.
            self.write_mmio32(CSR_INT, int_cause | !int_mask);
            self.write_mmio32(CSR_INT_MASK, 0);
            self.fw_state = FwState::Error;
            self.scan_pending = false;
            self.iwl_state = IwlState::Disconnected;
            return;
        }
        if int_cause != 0 {
            self.write_mmio32(CSR_INT, int_cause);
            if int_cause & (CSR_INT_BIT_FH_RX | CSR_INT_BIT_SW_RX) != 0 {
                self.write_mmio32(CSR_FH_INT, fh_cause & CSR_FH_INT_RX_MASK);
            }
            if int_cause & CSR_INT_BIT_FH_TX != 0 {
                self.write_mmio32(CSR_FH_INT, fh_cause & CSR_FH_INT_TX_MASK);
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
                let previous_head = self.rx_head;
                let process_from = self.rx_tail;
                // closed_rb_num is the next RBD boundary: firmware has
                // filled entries [rx_tail, closed_rb_num). This matches the
                // Linux gen1_2 receive loop, which processes while read != r.
                self.rx_head = closed_rb;
                if closed_rb != previous_head {
                    log::info!(
                        "iwlwifi: RX DMA progress closed_rbd={} process_from={} process_until={}",
                        closed_rb,
                        process_from,
                        self.rx_head
                    );
                }
            }
            if int_cause & CSR_INT_BIT_FH_TX != 0 {
                // The pointer was polled above. Re-read only when an actual
                // FH_TX cause is present, since the interrupt and the SCD
                // pointer are independent on this generation.
                if let Some(hardware_tail) = self.read_prph(scd_queue_rdptr(self.command_queue())) {
                    self.update_tx_tail(hardware_tail as usize);
                }
                if let Some(hardware_tail) = self.read_prph(scd_queue_rdptr(self.traffic_queue())) {
                    self.update_data_tx_tail(hardware_tail as usize);
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
        let mut deferred_scan_complete = false;
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
                self.process_rx_buffer(&frame_data, &mut deferred_scan_complete);
            }

            self.rx_tail = (self.rx_tail + 1) % RX_QUEUE_SIZE;
        }

        if self.rx_tail != rx_tail_before {
            // The hardware write index is the next RBD made available and
            // must advance in groups of eight on this generation.
            let processed = (self.rx_tail + RX_QUEUE_SIZE - rx_tail_before) % RX_QUEUE_SIZE;
            self.restock_rx_buffers(processed);
        }

        if deferred_scan_complete && self.scan_pending {
            self.scan_pending = false;
            self.wifi_conn.finish_scan();
            self.iwl_state = IwlState::Disconnected;
            self.scan_result_grace_ticks = SCAN_RESULT_GRACE_TICKS;
            log::info!(
                "iwlwifi: scan completion notification received; accepting late RX beacons for {} ticks",
                SCAN_RESULT_GRACE_TICKS
            );
        }

        if self.scan_result_grace_ticks > 0 {
            self.scan_result_grace_ticks -= 1;
            if self.scan_result_grace_ticks == 0 {
                log::info!(
                    "iwlwifi: scan complete ({} APs found)",
                    self.scan_results.len()
                );
            }
        }

        if self.scan_pending {
            // Watchdog: allow up to 12 000 ticks for the firmware to complete
            // a passive scan and deliver beacons.  The previous 4 000-tick
            // limit could fire in under 4 seconds on hardware with a fast
            // APIC timer (1 tick ≈ 2.25 ms on this platform, but the period
            // is hardware-dependent), destroying beacons that arrived just
            // after the scan-complete notification or while the scan was
            // still in progress.  23 channels × 110 TU ≈ 2.6 s of dwell,
            // plus TX/RX latency, so 12 000 ticks (≈ 27 s) gives ample
            // headroom while still bounding a wedged firmware.
            self.scan_channel += 1;
            if self.scan_channel > 12_000 {
                self.scan_pending = false;
                self.wifi_conn.finish_scan();
                self.iwl_state = IwlState::Disconnected;
                self.scan_result_grace_ticks = SCAN_RESULT_GRACE_TICKS;
                let tx_rptr = self
                    .read_prph(scd_queue_rdptr(self.command_queue()))
                    .unwrap_or(!0);
                let scd_status = self
                    .read_prph(scd_queue_status(self.command_queue()))
                    .unwrap_or(!0);
                let csr_int = self.safe_read32(CSR_INT).unwrap_or(!0);
                let fh_int = self.safe_read32(CSR_FH_INT).unwrap_or(!0);
                let int_mask = self.safe_read32(CSR_INT_MASK).unwrap_or(!0);
                let tx_cfg = self
                    .safe_read32(
                        FH_TCSR_CHNL_TX_CONFIG_BASE + SCD_QUEUE_STTS_FIFO_COMMAND * (0x20 / 4),
                    )
                    .unwrap_or(!0);
                let rx_closed = self.rx_status().closed_rb_num;
                let rx_finished = self.rx_status().finished_rb_num;
                let rx_rptr = self.safe_read32(FH_RSCSR_CHNL0_RDPTR_REG).unwrap_or(!0);
                log::warn!(
                    "iwlwifi: scan watchdog expired without firmware completion notification ({} APs found): tx_head={} tx_tail={} scd_rptr={} scd_status={:#010x} CSR_INT={:#010x} CSR_INT_MASK={:#010x} FH_INT={:#010x} TX_CFG={:#010x} RX_CLOSED={} RX_FINISHED={} RX_RDPTR={:#010x} RX_HEAD={} RX_TAIL={}",
                    self.scan_results.len(),
                    self.tx_head & 0xff,
                    self.tx_tail & 0xff,
                    tx_rptr,
                    scd_status,
                    csr_int,
                    int_mask,
                    fh_int,
                    tx_cfg,
                    rx_closed,
                    rx_finished,
                    rx_rptr,
                    self.rx_head,
                    self.rx_tail,
                );
                log::info!(
                    "iwlwifi: scan watchdog ended; accepting late RX beacons for {} ticks",
                    SCAN_RESULT_GRACE_TICKS
                );
            }
        }
    }

    /// Decode a legacy iwlwifi RX buffer.
    ///
    /// The 7265 firmware packs **multiple** packets into a single 4 KB RX
    /// buffer (RB), 64-byte aligned.  Each beacon reception produces two
    /// back-to-back packets: `REPLY_RX_PHY_CMD (0xc0)` followed by
    /// `REPLY_RX_MPDU_CMD (0xc1)`.  Processing only the first packet (the
    /// PHY info) silently dropped every beacon, leaving scan results empty
    /// even though the scan completed successfully.
    ///
    /// We now iterate over every packet in the RB, matching the Linux kernel
    /// `iwl_pcie_rx_handle_rb` loop.  A `0x5555_0000` `len_n_flags` marks the
    /// end of valid packets.
    fn process_rx_buffer(&mut self, data: &[u8], deferred_scan_complete: &mut bool) {
        const FH_RSCSR_FRAME_ALIGN: usize = 64;
        const FH_RSCSR_FRAME_INVALID: u32 = 0x5555_0000;

        let mut offset = 0usize;
        while offset + 8 <= data.len() {
            let len_n_flags = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);

            if len_n_flags == FH_RSCSR_FRAME_INVALID {
                break;
            }

            // The low 14 bits give the payload length (excluding the 4-byte
            // len_n_flags itself).  Total packet size = payload + 4.
            let payload_len = (len_n_flags as usize) & 0x3FFF;
            let total_len = payload_len.checked_add(4).unwrap_or(0);
            if total_len < 8 || offset + total_len > data.len() {
                // Malformed — fall back to treating remaining data as a single
                // packet so the bounded beacon scan can still attempt a match.
                let remaining = &data[offset..];
                self.process_single_packet(remaining, deferred_scan_complete);
                break;
            }

            let packet = &data[offset..offset + total_len];
            self.process_single_packet(packet, deferred_scan_complete);

            // Advance to the next 64-byte-aligned packet boundary.
            offset += total_len.next_multiple_of(FH_RSCSR_FRAME_ALIGN);
        }
    }

    /// Process one packet extracted from an RX buffer.
    fn process_single_packet(&mut self, data: &[u8], deferred_scan_complete: &mut bool) {
        const REPLY_RX_PHY_CMD: u8 = 0xc0;

        if data.len() < 8 {
            return;
        }
        let packet_len = data.len();
        let command = data[4];
        let group = data[5];
        let sequence = u16::from_le_bytes([data[6], data[7]]);

        if command == LegacyCmd::AddStaKey as u8
            && group == GroupId::Legacy as u8
            && self
                .wpa_key_pending_sequences
                .iter()
                .any(|pending| *pending == Some(sequence))
        {
            let payload = &data[8..packet_len];
            let status = payload
                .get(..4)
                .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()));
            // ADD_STA_SUCCESS is reported in the low byte, as in
            // `send_hcmd_and_wait()` for ADD_STA.
            if status.map(|status| status & 0xff) != Some(1) {
                log::warn!(
                    "iwlwifi: ADD_STA_KEY failed sequence=0x{:04x} status={:?} payload={}",
                    sequence,
                    status,
                    payload.len(),
                );
                self.wpa_failed("firmware rejected a CCMP key");
                return;
            }
            for pending in &mut self.wpa_key_pending_sequences {
                if *pending == Some(sequence) {
                    *pending = None;
                }
            }
            log::info!("iwlwifi: ADD_STA_KEY accepted sequence=0x{:04x}", sequence,);
            return;
        }

        // Scan-complete notification.
        if command == LegacyCmd::ScanOffloadCompleteNotif as u8 {
            let payload = &data[8..packet_len];
            log::info!(
                "iwlwifi: firmware scan iteration complete cmd=0x{:02x} scanned_channels={} status={} bt_status={} last_channel={}",
                command,
                payload.first().copied().unwrap_or(0),
                payload.get(1).copied().unwrap_or(u8::MAX),
                payload.get(2).copied().unwrap_or(u8::MAX),
                payload.get(3).copied().unwrap_or(0),
            );
            if self.scan_pending {
                *deferred_scan_complete = true;
            }
            return;
        }

        if command == LegacyCmd::ScanCompleteUrgent as u8 {
            let payload = &data[8..packet_len];
            let elapsed = if payload.len() >= 8 {
                u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]])
            } else {
                u32::MAX
            };
            log::info!(
                "iwlwifi: firmware scan offload complete cmd=0x{:02x} status={} schedule_line={} iteration={} ebs_status={} elapsed_s={}",
                command,
                payload.get(2).copied().unwrap_or(u8::MAX),
                payload.first().copied().unwrap_or(u8::MAX),
                payload.get(1).copied().unwrap_or(u8::MAX),
                payload.get(3).copied().unwrap_or(u8::MAX),
                elapsed,
            );
            if self.scan_pending {
                *deferred_scan_complete = true;
            }
            return;
        }

        // REPLY_RX_PHY_CMD (0xc0) precedes every REPLY_RX_MPDU_CMD.  It
        // carries PHY metadata (RSSI, noise, rate, channel) and has no
        // 802.11 frame.  Store the channel for the subsequent MPDU — 5 GHz
        // beacons lack a DS Parameter Set IE so the channel can only be
        // obtained from this metadata.
        if command == REPLY_RX_PHY_CMD {
            let payload = &data[8..packet_len];
            // iwl_rx_phy_info: channel is at byte offset 22 (le16).
            if payload.len() >= 24 {
                self.last_rx_system_timestamp =
                    u32::from_le_bytes(payload[4..8].try_into().unwrap());
                let channel = u16::from_le_bytes([payload[22], payload[23]]);
                self.last_rx_phy_channel = channel;
            }
            return;
        }

        // 7265D uses the v3 (non-TFH) TX response. Record whether the AP
        // acknowledged authentication/association and how many firmware
        // retries were needed; this is the authoritative boundary between a
        // queue/descriptor problem and a missing over-the-air response.
        if command == REPLY_TX_CMD {
            let payload = &data[8..packet_len];
            if let Some(response) = decode_legacy_tx_response(payload) {
                let status = response.status & TX_STATUS_MSK;
                let acknowledged = status == TX_STATUS_SUCCESS || status == TX_STATUS_DIRECT_DONE;
                let frame_control = response.frame_control as u8;
                if active_management_tx_matches(
                    frame_control,
                    self.iwl_state,
                    self.auth_tx_sequence,
                    sequence,
                ) {
                    self.auth_tx_acknowledged = Some(acknowledged);
                    // A duplicate/late response must not affect a later
                    // authentication plan after this descriptor retires.
                    self.auth_tx_sequence = None;
                } else if matches!(frame_control & 0xfc, 0xb0 | 0x00)
                    && matches!(self.iwl_state, IwlState::AuthSent | IwlState::AssocSent)
                {
                    log::debug!(
                        "iwlwifi: ignoring stale management TX response seq=0x{:04x} expected={:?}",
                        sequence,
                        self.auth_tx_sequence,
                    );
                }
                log::info!(
                    "iwlwifi: TX response seq=0x{:04x} fc=0x{:04x} frames={} ack={} status=0x{:02x} retries={} rts_failures={} rate={:#010x} airtime_us={}",
                    sequence,
                    response.frame_control,
                    response.frame_count,
                    acknowledged,
                    status,
                    response.failure_frame,
                    response.failure_rts,
                    response.initial_rate,
                    response.wireless_media_time,
                );
            } else {
                log::warn!(
                    "iwlwifi: truncated legacy TX response seq=0x{:04x} payload={}",
                    sequence,
                    payload.len(),
                );
            }
            return;
        }

        // REPLY_RX_MPDU_CMD (0xc1) has iwl_rx_mpdu_res_start
        // (byte_count, assist) at payload offset 0, followed by the raw
        // 802.11 MPDU. The packet's 4-byte length header is not part of
        // the command payload.
        if command == LegacyCmd::ReplyRxMpduCmd as u8 && packet_len >= 12 {
            let payload = &data[8..packet_len];
            let byte_count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
            log::info!(
                "iwlwifi: RX MPDU notification group=0x{:02x} seq=0x{:04x} bytes={}",
                group,
                sequence,
                byte_count,
            );
            if let Some(mpdu) = decode_legacy_rx_mpdu(payload) {
                log::debug!(
                    "iwlwifi: RX MPDU status={:#010x} decrypted={} crypto_header={} crypto_trailer={}",
                    mpdu.status,
                    mpdu.decrypted,
                    mpdu.crypto_header_len,
                    mpdu.crypto_trailer_len,
                );
                if mpdu.frame.len() >= 24 {
                    self.process_rx_frame(
                        mpdu.frame,
                        mpdu.decrypted,
                        mpdu.crypto_header_len,
                        mpdu.crypto_trailer_len,
                    );
                }
            } else {
                log::warn!(
                    "iwlwifi: rejected malformed or unauthenticated RX MPDU bytes={} payload={}",
                    byte_count,
                    payload.len(),
                );
            }
            return;
        }

        // REPLY_ERROR payload layout is error_type:u32, cmd_id:u8,
        // reserved:u8, bad_cmd_seq:u16, error_service:u32.
        if command == LegacyCmd::ReplyError as u8 && packet_len >= 20 {
            let payload = &data[8..packet_len];
            let error_type = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let bad_cmd = payload[4];
            let bad_seq = u16::from_le_bytes([payload[6], payload[7]]);
            let service = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
            log::warn!(
                "iwlwifi: firmware command error type={:#010x} cmd=0x{:02x} seq=0x{:04x} service={:#010x}",
                error_type,
                bad_cmd,
                bad_seq,
                service,
            );
            if self
                .wpa_key_pending_sequences
                .iter()
                .any(|pending| *pending == Some(bad_seq))
            {
                self.wpa_failed("firmware rejected a CCMP key command");
            }
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
        if self.wpa_key_pending_sequences.iter().any(Option::is_some) {
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
        log::info!("iwlwifi: WPA2 4-way handshake complete link_status=Connected");
        self.start_dhcp(0);
    }
}

// ── Test injection methods ───────────────────────────────────────

#[cfg(test)]
impl IwlWifiDevice {
    /// Inject an 802.11 frame directly into the RX processing path, bypassing
    /// the DMA ring and MMIO.  This simulates a frame received from firmware.
    pub(super) fn inject_rx_frame(&mut self, frame: &[u8]) {
        self.process_rx_frame(frame, false, 0, 0);
    }

    /// Inject an 802.11 frame that the firmware has already CCMP-decrypted.
    /// Use this for data frames received after WPA keys are installed.
    pub(super) fn inject_rx_frame_decrypted(&mut self, frame: &[u8]) {
        self.process_rx_frame(frame, true, 0, 0);
    }

    /// Inject a raw firmware notification directly into packet processing.
    pub(super) fn inject_rx_notification(&mut self, notification: &[u8]) {
        let mut deferred_scan_complete = false;
        self.process_single_packet(notification, &mut deferred_scan_complete);
    }

    /// Force the deferred WPA key finalisation.  In normal operation this is
    /// called from `tick()` after the TX ring reports that the key commands
    /// have been consumed.  In tests, call `drain_tx()` first to advance
    /// `tx_tail`, then call this method.
    pub(super) fn finish_wpa_for_test(&mut self) {
        self.wpa_key_pending_sequences = [None; 2];
        self.finish_pending_wpa_keys();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_management_tx_response_does_not_match_new_plan() {
        assert!(active_management_tx_matches(
            0xb0,
            IwlState::AuthSent,
            Some(0x0501),
            0x0501,
        ));
        assert!(!active_management_tx_matches(
            0xb0,
            IwlState::AuthSent,
            Some(0x0501),
            0x0500,
        ));
        assert!(!active_management_tx_matches(
            0xb0,
            IwlState::Disconnected,
            Some(0x0501),
            0x0501,
        ));
    }

    fn legacy_mpdu_payload(frame: &[u8], status: u32) -> Vec<u8> {
        let mut payload = Vec::with_capacity(4 + frame.len() + 4);
        payload.extend_from_slice(&(frame.len() as u16).to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(frame);
        payload.extend_from_slice(&status.to_le_bytes());
        payload
    }

    #[test]
    fn legacy_ccmp_rx_uses_unaligned_trailing_status_and_keeps_eight_byte_iv() {
        let mut frame = [0u8; 35];
        frame[0] = 0x08;
        frame[1] = 0x42; // FromDS + Protected
        let status = RX_MPDU_RES_STATUS_CRC_OK
            | RX_MPDU_RES_STATUS_OVERRUN_OK
            | RX_MPDU_RES_STATUS_MIC_OK
            | RX_MPDU_RES_STATUS_SEC_CCM_ENC;
        let payload = legacy_mpdu_payload(&frame, status);

        let decoded = decode_legacy_rx_mpdu(&payload).expect("valid CCMP MPDU");

        assert_eq!(decoded.frame, frame);
        assert!(decoded.decrypted);
        assert_eq!(decoded.crypto_header_len, IEEE80211_CCMP_HDR_LEN);
        assert_eq!(decoded.crypto_trailer_len, 8);
    }

    #[test]
    fn legacy_tx_response_uses_linux_v3_fixed_offsets() {
        let mut payload = [0u8; 40];
        payload[0] = 1;
        payload[2] = 2;
        payload[3] = 3;
        payload[4..8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        payload[8..10].copy_from_slice(&77u16.to_le_bytes());
        payload[34..36].copy_from_slice(&0x00b0u16.to_le_bytes());
        payload[36..38].copy_from_slice(&TX_STATUS_SUCCESS.to_le_bytes());

        assert_eq!(
            decode_legacy_tx_response(&payload),
            Some(LegacyTxResponse {
                frame_count: 1,
                failure_rts: 2,
                failure_frame: 3,
                initial_rate: 0x1234_5678,
                wireless_media_time: 77,
                frame_control: 0x00b0,
                status: TX_STATUS_SUCCESS,
            })
        );
        assert!(decode_legacy_tx_response(&payload[..39]).is_none());
    }

    #[test]
    fn legacy_ccmp_rx_rejects_bad_mic_bad_fcs_and_missing_status() {
        let mut frame = [0u8; 32];
        frame[0] = 0x08;
        frame[1] = 0x42;
        let good_transport = RX_MPDU_RES_STATUS_CRC_OK | RX_MPDU_RES_STATUS_OVERRUN_OK;

        assert!(
            decode_legacy_rx_mpdu(&legacy_mpdu_payload(
                &frame,
                good_transport | RX_MPDU_RES_STATUS_SEC_CCM_ENC,
            ))
            .is_none()
        );
        assert!(
            decode_legacy_rx_mpdu(&legacy_mpdu_payload(
                &frame,
                RX_MPDU_RES_STATUS_OVERRUN_OK
                    | RX_MPDU_RES_STATUS_MIC_OK
                    | RX_MPDU_RES_STATUS_SEC_CCM_ENC,
            ))
            .is_none()
        );

        let mut truncated = legacy_mpdu_payload(
            &frame,
            good_transport | RX_MPDU_RES_STATUS_MIC_OK | RX_MPDU_RES_STATUS_SEC_CCM_ENC,
        );
        truncated.truncate(truncated.len() - 1);
        assert!(decode_legacy_rx_mpdu(&truncated).is_none());
    }

    #[test]
    fn decrypted_ccmp_data_strips_iv_and_mic_before_llc_delivery() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.wpa_required = true;
        device.wpa_keys_installed = true;
        let mut frame = Vec::new();
        frame.extend_from_slice(&[0x08, 0x42, 0, 0]); // data, FromDS, Protected
        frame.extend_from_slice(&device.mac);
        frame.extend_from_slice(&[0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);
        frame.extend_from_slice(&[0; 8]); // addr3 + sequence control
        frame.extend_from_slice(&[1, 0, 0, 0x20, 0, 0, 0, 0]); // CCMP IV
        frame.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x12, 0x34]);
        frame.push(0x7b);
        frame.extend_from_slice(&[0xa5; 8]); // CCMP MIC

        device.process_rx_frame(&frame, true, 8, 8);

        assert_eq!(device.rx_queue.pop_front().as_deref(), Some(&[0x7b][..]));
    }

    #[test]
    fn add_sta_key_reply_clears_only_the_matching_pending_sequence() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.wpa_key_pending_sequences = [Some(0x090a), Some(0x090b)];
        let mut reply = [0u8; 12];
        reply[4] = LegacyCmd::AddStaKey as u8;
        reply[5] = GroupId::Legacy as u8;
        reply[6..8].copy_from_slice(&0x090au16.to_le_bytes());
        reply[8..12].copy_from_slice(&1u32.to_le_bytes());

        device.inject_rx_notification(&reply);

        assert_eq!(device.wpa_key_pending_sequences, [None, Some(0x090b)]);
    }

    #[test]
    fn reply_error_for_a_pending_key_fails_the_handshake() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.wifi_conn.status = bonder::wifi::WifiStatus::Handshake;
        device.wpa_key_pending_sequences = [Some(0x090a), Some(0x090b)];
        let mut reply = [0u8; 20];
        reply[4] = LegacyCmd::ReplyError as u8;
        reply[5] = GroupId::Legacy as u8;
        reply[8..12].copy_from_slice(&1u32.to_le_bytes());
        reply[12] = LegacyCmd::AddStaKey as u8;
        reply[14..16].copy_from_slice(&0x090bu16.to_le_bytes());

        device.inject_rx_notification(&reply);

        assert_eq!(device.wifi_conn.status, bonder::wifi::WifiStatus::Error);
        assert_eq!(device.wpa_key_pending_sequences, [None; 2]);
    }

    #[test]
    fn handshake_watchdog_is_finite() {
        let mut device = IwlWifiDevice::new_for_test([0x02, 0, 0, 0, 0, 1]);
        device.iwl_state = IwlState::Connected;
        device.wifi_conn.status = bonder::wifi::WifiStatus::Handshake;

        for _ in 0..=CONNECTION_WATCHDOG_TICKS {
            device.advance_connection_watchdog();
        }

        assert_eq!(device.wifi_conn.status, bonder::wifi::WifiStatus::Error);
        assert_eq!(device.wpa.state, WpaState::Error);
    }

    #[test]
    fn runtime_hcmd_response_requires_the_submitted_q9_sequence() {
        let opcode = LegacyCmd::MacContext as u8;
        let group = GroupId::Legacy as u8;

        assert!(command_response_matches(
            opcode,
            group,
            0x0918,
            opcode,
            group,
            Some(0x0918),
        ));
        assert!(!command_response_matches(
            opcode,
            group,
            0x0917,
            opcode,
            group,
            Some(0x0918),
        ));
        assert!(!command_response_matches(
            LegacyCmd::AddSta as u8,
            group,
            0x0918,
            opcode,
            group,
            Some(0x0918),
        ));
        assert!(command_response_matches(
            opcode, group, 0x0917, opcode, group, None,
        ));
    }
}
