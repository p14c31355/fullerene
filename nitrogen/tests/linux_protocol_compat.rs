//! Linux/xHCI and Linux/iwlwifi protocol compatibility tests.
//!
//! These are deliberately ordinary integration tests. They do not consume a
//! boot log and do not need special arguments or hardware: the Linux 4.9
//! wire format is the oracle, and a driver regression makes `cargo test` fail.

use std::mem::{offset_of, size_of};
use std::slice;

use nitrogen::iwlwifi::registers::{
    CSR_FH_INT_BIT_HI_PRIOR, CSR_FH_INT_BIT_RX_CHNL0, CSR_FH_INT_BIT_RX_CHNL1, CSR_FH_INT_RX_MASK,
    CSR_MAC_SHADOW_REG_CTRL_ENABLE, FH_MEM_CBBC_0_15_LOWER_BOUND, FH_MEM_CBBC_16_19_LOWER_BOUND,
    FH_MEM_CBBC_20_31_LOWER_BOUND, IWL_AUX_QUEUE, IWL_CMD_QUEUE, IWL_NUM_OF_QUEUES,
    SCD_CONTEXT_MEM_LOWER_BOUND, SCD_TRANS_TBL_MEM_UPPER_BOUND, TX_AUX_TFD_RING_OFFSET,
    TX_DMA_ALLOCATION_BYTES, TX_KEEP_WARM_BYTES, TX_KEEP_WARM_OFFSET, TX_QUEUE_SIZE,
    TX_SCD_BC_BYTES, TX_SCD_BC_OFFSET, TX_TFD_RING_BYTES, fh_mem_cbbc_queue,
    scd_trans_tbl_offset_queue,
};
use nitrogen::iwlwifi::types::{
    AddStaCmdV7, BtCoexConfigCmd, MacContextCmd, MccUpdateCmdV1, MccUpdateCmdV2,
    ScanChannelCfgLmac, ScanConfigV1, ScanRequestCmd, ScdTxqCfgCmdV1,
};
use nitrogen::usb::UsbSetupPacket;
use nitrogen::usb::xhci::ring::{trb_flag, trb_type};
use nitrogen::usb::xhci::transfer::linux_control_transfer_trbs;

fn bytes<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

#[test]
fn linux_7000_scheduler_uses_31_queue_geometry() {
    assert_eq!(IWL_NUM_OF_QUEUES, 31);
    assert_eq!(CSR_MAC_SHADOW_REG_CTRL_ENABLE, 0x800f_ffff);
    assert_eq!(scd_trans_tbl_offset_queue(0), 0x7e0);
    assert_eq!(scd_trans_tbl_offset_queue(31), 0x81c);
    assert_eq!(SCD_CONTEXT_MEM_LOWER_BOUND, 0x600);
    assert_eq!(SCD_TRANS_TBL_MEM_UPPER_BOUND, 0x81c);
    assert_eq!(
        (SCD_TRANS_TBL_MEM_UPPER_BOUND - SCD_CONTEXT_MEM_LOWER_BOUND) / 4,
        135
    );
}

#[test]
fn linux_legacy_cbbc_register_windows_cover_all_31_queues() {
    assert_eq!(fh_mem_cbbc_queue(0), FH_MEM_CBBC_0_15_LOWER_BOUND);
    assert_eq!(fh_mem_cbbc_queue(15), FH_MEM_CBBC_0_15_LOWER_BOUND + 15);
    assert_eq!(fh_mem_cbbc_queue(16), FH_MEM_CBBC_16_19_LOWER_BOUND);
    assert_eq!(fh_mem_cbbc_queue(19), FH_MEM_CBBC_16_19_LOWER_BOUND + 3);
    assert_eq!(fh_mem_cbbc_queue(20), FH_MEM_CBBC_20_31_LOWER_BOUND);
    assert_eq!(fh_mem_cbbc_queue(30), FH_MEM_CBBC_20_31_LOWER_BOUND + 10);
}

#[test]
fn linux_gen1_rx_interrupt_mask_includes_high_priority_alive() {
    assert_eq!(CSR_FH_INT_BIT_RX_CHNL0, 1 << 16);
    assert_eq!(CSR_FH_INT_BIT_RX_CHNL1, 1 << 17);
    assert_eq!(CSR_FH_INT_BIT_HI_PRIOR, 1 << 30);
    assert_eq!(CSR_FH_INT_RX_MASK, 0x4003_0000);
}

#[test]
fn linux_api29_bt_init_command_enables_the_v1_module_bits() {
    let command = BtCoexConfigCmd::network_default();
    assert_eq!(size_of::<BtCoexConfigCmd>(), 8);
    assert_eq!(bytes(&command), &[1, 0, 0, 0, 0x15, 0, 0, 0]);
}

#[test]
fn linux_v49_aux_station_payload_is_wire_compatible() {
    assert_eq!(IWL_CMD_QUEUE, 9);
    assert_eq!(IWL_AUX_QUEUE, 11);
    assert_eq!(size_of::<AddStaCmdV7>(), 44);

    // Linux v4.9 iwl_mvm_add_aux_sta(): MAC_INDEX_AUX is 4, while the
    // internal station-table entry is allocated as sta_id 1. The queue mask
    // must advertise q11 before ADD_STA is sent.
    let payload = AddStaCmdV7::aux(4, 1);
    let actual = bytes(&payload);
    let mut expected = [0u8; 44];
    expected[2..4].copy_from_slice(&0xffffu16.to_le_bytes());
    expected[4..8].copy_from_slice(&4u32.to_le_bytes());
    expected[16] = 1;
    expected[40..44].copy_from_slice(&(1u32 << 11).to_le_bytes());
    assert_eq!(actual, expected);
}

#[test]
fn linux_v49_aux_queue_config_is_sent_before_station_add() {
    assert_eq!(size_of::<ScdTxqCfgCmdV1>(), 12);
    let payload = ScdTxqCfgCmdV1::aux(1);
    let actual = bytes(&payload);
    // token=0, owner sta=1, tid=15, q11, enable, non-aggregate,
    // multicast FIFO=5, window=64, ssn=0, reserved=0.
    assert_eq!(actual, &[0, 1, 15, 11, 1, 0, 5, 64, 0, 0, 0, 0]);
}

#[test]
fn linux_v49_scan_commands_use_the_aux_station_id() {
    let mac = [0x02, 0, 0, 0, 0, 1];

    let config = ScanConfigV1::new(mac, 1);
    let config_bytes = bytes(&config);
    // SCAN_CFG_CMD API v1 places bcast_sta_id after the six-byte MAC address.
    assert_eq!(config_bytes[28..34], mac);
    assert_eq!(config_bytes[34], 1);

    let request = ScanRequestCmd::new(mac, 1);
    let request_bytes = bytes(&request);
    let channels_offset = offset_of!(ScanRequestCmd, channels);
    let probe_offset = offset_of!(ScanRequestCmd, probe_req);
    // Linux allocates one channel slot for each value in the firmware TLV
    // (40 on the 7265), then fills only the requested 23 channels. The probe
    // request therefore follows all 40 slots, including a zero tail.
    assert_eq!(
        probe_offset - channels_offset,
        40 * size_of::<ScanChannelCfgLmac>()
    );
    assert_eq!(size_of::<ScanRequestCmd>(), 1772);
    assert!(
        request_bytes[channels_offset + 23 * size_of::<ScanChannelCfgLmac>()..probe_offset]
            .iter()
            .all(|byte| *byte == 0)
    );
    // The two Linux LMAC scan TX command entries are both bound to sta_id 1.
    assert_eq!(request_bytes[40], 1);
    assert_eq!(request_bytes[52], 1);
    // FullereneOS uses the supplied wildcard probe request for an active
    // scan; the LMAC PASSIVE flag must remain clear.
    // Linux does not set ITER_COMPLETE for regular unassociated scans.
    let scan_flags = u32::from_le_bytes(request_bytes[12..16].try_into().unwrap());
    assert_ne!(scan_flags & (1 << 0), 0); // PASS_ALL
    assert_eq!(scan_flags & (1 << 1), 0); // not PASSIVE
    assert_ne!(scan_flags & (1 << 7), 0); // EXTENDED_DWELL

    let probe = &request_bytes[offset_of!(ScanRequestCmd, probe_req)..];
    assert_eq!(u16::from_le_bytes(probe[4..6].try_into().unwrap()), 26);
    assert_eq!(u16::from_le_bytes(probe[6..8].try_into().unwrap()), 10);
    assert_eq!(u16::from_le_bytes(probe[8..10].try_into().unwrap()), 36);
    assert_eq!(u16::from_le_bytes(probe[10..12].try_into().unwrap()), 6);
    assert_eq!(u16::from_le_bytes(probe[12..14].try_into().unwrap()), 42);
    assert_eq!(u16::from_le_bytes(probe[14..16].try_into().unwrap()), 6);
    assert_eq!(
        &probe[16 + 26..16 + 36],
        &[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]
    );
    assert_eq!(&probe[16 + 42..16 + 48], &[50, 4, 0xb0, 0x48, 0x60, 0x6c]);
}

#[test]
fn linux_scan_config_uses_the_7265_firmware_channel_array_size() {
    let config = ScanConfigV1::new([0x02, 0, 0, 0, 0, 1], 1);
    let config_bytes = bytes(&config);
    // The 7265 TLV advertises 40 SCAN_CONFIG channel slots. Linux sends the
    // full fixed-size array and encodes 39 populated channels in the flags.
    assert_eq!(size_of::<ScanConfigV1>(), 36 + 40);
    let flags = u32::from_le_bytes(config_bytes[0..4].try_into().unwrap());
    assert_eq!(flags >> 26, 39);
    assert_eq!(&config_bytes[24..28], &[10, 110, 44, 90]);
    assert_eq!(config_bytes[36 + 39], 0);
}

#[test]
fn linux_mcc_update_api_versions_use_the_advertised_wire_layouts() {
    let v1 = MccUpdateCmdV1 {
        mcc: u16::from_be_bytes(*b"ZZ"),
        source_id: 0,
        reserved: 0,
    };
    assert_eq!(bytes(&v1), &[0x5a, 0x5a, 0, 0]);

    let v2 = MccUpdateCmdV2 {
        mcc: u16::from_be_bytes(*b"ZZ"),
        source_id: 0,
        reserved: 0,
        key: 0,
        reserved2: [0; 20],
    };
    assert_eq!(size_of::<MccUpdateCmdV2>(), 28);
    assert_eq!(&bytes(&v2)[..4], &[0x5a, 0x5a, 0, 0]);
    assert!(bytes(&v2)[4..].iter().all(|byte| *byte == 0));
}

#[test]
fn linux_v49_xhci_control_in_td_has_correct_stage_contract() {
    let setup = UsbSetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 8,
    };
    let td = linux_control_transfer_trbs(&setup, 0x1234_5000, trb_flag::CYCLE);

    assert_eq!(td.setup.trb_type(), trb_type::SETUP_STAGE);
    assert_eq!(td.setup.params, [0x80, 0x06, 0, 1, 0, 0, 8, 0]);
    assert_ne!(td.setup.flags & trb_flag::IDT, 0);
    // Linux 4.9 does not set TRB_CHAIN for control SETUP/DATA TRBs; the
    // control TD is delimited by the STATUS TRB.
    assert_eq!(td.setup.flags & trb_flag::CHAIN, 0);
    assert_eq!((td.setup.flags >> 16) & 0x3, 3); // TRB_DATA_IN

    let data = td.data.expect("IN control transfer needs DATA stage");
    assert_eq!(data.trb_type(), trb_type::DATA_STAGE);
    assert_eq!(data.params, 0x1234_5000u64.to_le_bytes());
    assert_ne!(data.flags & trb_flag::DIR_IN, 0);
    assert_ne!(data.flags & trb_flag::ISP, 0);
    assert_eq!(data.flags & trb_flag::CHAIN, 0);

    assert_eq!(td.status.trb_type(), trb_type::STATUS_STAGE);
    assert_eq!(td.status.flags & trb_flag::DIR_IN, 0); // IN data => OUT status
    assert_ne!(td.status.flags & trb_flag::IOC, 0);
}

#[test]
fn linux_v49_xhci_control_out_and_no_data_status_directions() {
    let out = UsbSetupPacket {
        bm_request_type: 0,
        b_request: 9,
        w_value: 1,
        w_index: 0,
        w_length: 8,
    };
    let out_td = linux_control_transfer_trbs(&out, 0x2000, trb_flag::CYCLE);
    assert_eq!((out_td.setup.flags >> 16) & 0x3, 2); // TRB_DATA_OUT
    assert_eq!(out_td.data.unwrap().flags & trb_flag::DIR_IN, 0);
    assert_ne!(out_td.status.flags & trb_flag::DIR_IN, 0);

    let no_data = UsbSetupPacket {
        bm_request_type: 0,
        b_request: 5,
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    let no_data_td = linux_control_transfer_trbs(&no_data, 0, trb_flag::CYCLE);
    assert_eq!((no_data_td.setup.flags >> 16) & 0x3, 0); // TRB_NO_DATA
    assert!(no_data_td.data.is_none());
    assert_ne!(no_data_td.status.flags & trb_flag::DIR_IN, 0);
}

#[test]
fn tx_dma_allocation_covers_every_region() {
    // Linux allocates 256 legacy 128-byte TFDs per scheduler queue.
    assert_eq!(TX_TFD_RING_BYTES, 128 * TX_QUEUE_SIZE);
    assert_eq!(TX_AUX_TFD_RING_OFFSET, TX_TFD_RING_BYTES);
    assert!(TX_KEEP_WARM_OFFSET >= TX_AUX_TFD_RING_OFFSET + TX_TFD_RING_BYTES);
    assert!(TX_SCD_BC_OFFSET >= TX_KEEP_WARM_OFFSET + TX_KEEP_WARM_BYTES);
    assert!(TX_SCD_BC_OFFSET + TX_SCD_BC_BYTES <= TX_DMA_ALLOCATION_BYTES);
}

#[test]
fn mac_context_payload_matches_api_v1_fixed_offsets() {
    assert_eq!(size_of::<MacContextCmd>(), 148);
    let payload = MacContextCmd::sta([0x94, 0x65, 0x9c, 0x44, 0x73, 0xd4]);
    let actual = bytes(&payload);
    assert_eq!(&actual[24..30], &[0xff; 6]);
    assert_eq!(&actual[60..62], &15u16.to_le_bytes());
    assert_eq!(&actual[62..64], &1023u16.to_le_bytes());
    assert_eq!(actual[64], 2);
    assert_eq!(&actual[68..70], &15u16.to_le_bytes());
    assert_eq!(&actual[70..72], &1023u16.to_le_bytes());
    assert_eq!(&actual[120..124], &0x028f_5c28u32.to_le_bytes());
}
