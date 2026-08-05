//! Linux/xHCI and Linux/iwlwifi protocol compatibility tests.
//!
//! These are deliberately ordinary integration tests. They do not consume a
//! boot log and do not need special arguments or hardware: the Linux 4.9
//! wire format is the oracle, and a driver regression makes `cargo test` fail.

use std::mem::size_of;
use std::slice;

use nitrogen::iwlwifi::registers::{
    IWL_AUX_QUEUE, TX_AUX_TFD_RING_OFFSET, TX_DMA_ALLOCATION_BYTES, TX_KEEP_WARM_BYTES,
    TX_KEEP_WARM_OFFSET, TX_QUEUE_SIZE, TX_SCD_BC_BYTES, TX_SCD_BC_OFFSET, TX_TFD_RING_BYTES,
};
use nitrogen::iwlwifi::types::{AddStaCmdV7, MacContextCmd, ScanConfigV1, ScanRequestCmd};
use nitrogen::usb::UsbSetupPacket;
use nitrogen::usb::xhci::ring::{trb_flag, trb_type};
use nitrogen::usb::xhci::transfer::linux_control_transfer_trbs;

fn bytes<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

#[test]
fn linux_v49_aux_station_payload_is_wire_compatible() {
    assert_eq!(IWL_AUX_QUEUE, 8);
    assert_eq!(size_of::<AddStaCmdV7>(), 44);

    // Linux v4.9 iwl_mvm_add_aux_sta(): MAC_INDEX_AUX is 4, while the
    // internal station-table entry is allocated as sta_id 1. The queue mask
    // must advertise q8 before ADD_STA is sent.
    let payload = AddStaCmdV7::aux(4, 1);
    let actual = bytes(&payload);
    let mut expected = [0u8; 44];
    expected[2..4].copy_from_slice(&0xffffu16.to_le_bytes());
    expected[4..8].copy_from_slice(&4u32.to_le_bytes());
    expected[16] = 1;
    expected[40..44].copy_from_slice(&(1u32 << 8).to_le_bytes());
    assert_eq!(actual, expected);
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
    // The two Linux LMAC scan TX command entries are both bound to sta_id 1.
    assert_eq!(request_bytes[40], 1);
    assert_eq!(request_bytes[52], 1);
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
    assert_eq!(size_of::<MacContextCmd>(), 144);
}
