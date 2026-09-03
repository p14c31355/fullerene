//! DWC3 device-mode encodings shared by the hardware path and its simulator.

pub const DCFG_SPEED_MASK: u32 = 7;
pub const DCFG_DEVADDR_MASK: u32 = 0x7f << 3;
// DWC3 DCFG.SPEED encoding from the qpr1 dwc3 core header.
pub const DCFG_FULLSPEED: u32 = 1;
pub const DCFG_HIGHSPEED: u32 = 0;
pub const DCFG_SUPERSPEED: u32 = 4;
// DWC3 device-speed encoding used by the qpr1 gadget driver for USB low speed.
pub const DCFG_LOWSPEED: u32 = 2;
pub const DCTL_RUN_STOP: u32 = 1 << 31;
pub const DSTS_CONNECTSPD_MASK: u32 = 7;
pub const DSTS_DEVCTRLHLT: u32 = 1 << 22;
// DWC3_DSTS_DCNRD is bit 29. Bit 23 is COREIDLE; treating it as
// controller-not-ready makes a post-reset handoff race the DWC3 device state
// machine and can issue EP0 commands before the core accepts them.
pub const DSTS_DCNRD: u32 = 1 << 29;
pub const DSTS_SUPERSPEED: u32 = 4;

pub const GEVNTCOUNT_MASK: u32 = 0xfffc;
pub const GEVNTCOUNT_EHB: u32 = 1 << 31;
pub const GEVNTSIZ_INTMASK: u32 = 1 << 31;
pub const GEVNTSIZ_SIZE_MASK: u32 = 0xffff;

pub const DEVTEN_DISCONNECT: u32 = 1 << 0;
pub const DEVTEN_USB_RESET: u32 = 1 << 1;
pub const DEVTEN_CONNECT_DONE: u32 = 1 << 2;
pub const DEVTEN_LINK_STATUS_CHANGE: u32 = 1 << 3;
pub const DEVTEN_WAKEUP: u32 = 1 << 4;
pub const DEVTEN_HIBERNATION_REQUEST: u32 = 1 << 5;
pub const DEVTEN_SUSPEND: u32 = 1 << 6;
// Linux enables these diagnostic/controller events in
// dwc3_gadget_enable_irq(); they are useful even when the early path polls
// the event ring because the records remain available to the retained trace.
pub const DEVTEN_ERRATIC_ERROR: u32 = 1 << 9;
pub const DEVTEN_CMD_COMPLETE: u32 = 1 << 10;
pub const DEVTEN_OVERFLOW: u32 = 1 << 11;

pub const DEVICE_EVENT_KIND_SHIFT: u32 = 8;
pub const DEVICE_EVENT_KIND_MASK: u32 = 0x0f;
pub const DEVICE_EVENT_USB_RESET: u32 = 1;
pub const DEVICE_EVENT_CONNECT_DONE: u32 = 2;
pub const DEVICE_EVENT_LINK_STATUS_CHANGE: u32 = 3;
pub const DEVICE_EVENT_WAKEUP: u32 = 4;
pub const DEVICE_EVENT_HIBERNATION_REQUEST: u32 = 5;
pub const DEVICE_EVENT_SUSPEND: u32 = 6;
pub const DEVICE_EVENT_ERRATIC_ERROR: u32 = 9;
pub const DEVICE_EVENT_CMD_COMPLETE: u32 = 10;
pub const DEVICE_EVENT_OVERFLOW: u32 = 11;

pub const DEPCMD_CMDACT: u32 = 1 << 10;
pub const DEPCMD_CMDIOC: u32 = 1 << 8;
pub const DEPCMD_HIPRI_FORCERM: u32 = 1 << 11;
pub const DEPCMD_PARAM_SHIFT: u32 = 16;
pub const DEPCMD_DEPSTARTCFG: u32 = 0x09;
pub const DEPCMD_ENDTRANSFER: u32 = 0x08;
pub const DEPCMD_UPDATETRANSFER: u32 = 0x07;
pub const DEPCMD_STARTTRANSFER: u32 = 0x06;
pub const DEPCMD_CLEARSTALL: u32 = 0x05;
pub const DEPCMD_SETSTALL: u32 = 0x04;
pub const DEPCMD_SETTRANSFRESOURCE: u32 = 0x02;
pub const DEPCMD_SETEPCONFIG: u32 = 0x01;
pub const DEPCMD_ACTION_MODIFY: u32 = 2 << 30;

pub const DEPCFG_XFER_COMPLETE_EN: u32 = 1 << 8;
pub const DEPCFG_XFER_IN_PROGRESS_EN: u32 = 1 << 9;
pub const DEPCFG_XFER_NOT_READY_EN: u32 = 1 << 10;
/// Interrupter number in DEPCMD_SETEPCONFIG.P1. GSI endpoints use event
/// buffers 1..3; ordinary gadget endpoints keep the default interrupter.
pub const DEPCFG_INT_NUM_SHIFT: u32 = 0;
pub const DEPCFG_EP_NUMBER_SHIFT: u32 = 25;
pub const DEPCFG_EP_TYPE_CONTROL: u32 = 0;
// DWC3_DEPCFG_EP_TYPE(n) stores the USB endpoint type in bits 2:1.
pub const DEPCFG_EP_TYPE_BULK: u32 = 2 << 1;
pub const DEPCFG_MAX_PACKET_SHIFT: u32 = 3;
pub const DEPCFG_FIFO_NUMBER_SHIFT: u32 = 17;
pub const DEPCFG_BURST_SIZE_SHIFT: u32 = 22;

pub const TRB_HWO: u32 = 1 << 0;
pub const TRB_LST: u32 = 1 << 1;
pub const TRB_CHN: u32 = 1 << 2;
pub const TRB_CSP: u32 = 1 << 3;
pub const TRB_ISP_IMI: u32 = 1 << 10;
pub const TRB_IOC: u32 = 1 << 11;
pub const TRB_CONTROL_SETUP: u32 = 2 << 4;
pub const TRB_NORMAL: u32 = 1 << 4;
pub const TRB_CONTROL_STATUS2: u32 = 3 << 4;
pub const TRB_CONTROL_STATUS3: u32 = 4 << 4;
pub const TRB_CONTROL_DATA: u32 = 5 << 4;
pub const TRB_LINK: u32 = 8 << 4;

pub const EP_EVENT_TRANSFER_COMPLETE: u32 = 1;
pub const EP_EVENT_XFER_NOT_READY: u32 = 3;
pub const EP_XFER_NOT_READY_STATUS: u32 = 2;

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trb {
    pub bpl: u32,
    pub bph: u32,
    pub size: u32,
    pub ctrl: u32,
}

pub const fn device_event(kind: u32) -> u32 {
    1 | (kind << DEVICE_EVENT_KIND_SHIFT)
}

pub const fn endpoint_event(endpoint: u32, event: u32, status: u32) -> u32 {
    (endpoint << 1) | (event << 6) | (status << 12)
}

pub const fn endpoint_from_event(raw: u32) -> u32 {
    (raw >> 1) & 0x1f
}

pub const fn endpoint_event_kind(raw: u32) -> u32 {
    (raw >> 6) & 0xf
}

pub const fn endpoint_event_status(raw: u32) -> u32 {
    (raw >> 12) & 0xf
}

pub const fn device_event_kind(raw: u32) -> u32 {
    (raw >> DEVICE_EVENT_KIND_SHIFT) & DEVICE_EVENT_KIND_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwc3_status_bits_match_upstream_layout() {
        assert_eq!(DSTS_DEVCTRLHLT, 1 << 22);
        assert_eq!(DSTS_DCNRD, 1 << 29);
        assert_ne!(DSTS_DCNRD, 1 << 23); // COREIDLE, not DCNRD
    }

    #[test]
    fn gadget_controller_event_enable_bits_match_upstream_layout() {
        assert_eq!(DEVTEN_ERRATIC_ERROR, 1 << 9);
        assert_eq!(DEVTEN_CMD_COMPLETE, 1 << 10);
        assert_eq!(DEVTEN_OVERFLOW, 1 << 11);
    }

    #[test]
    fn event_buffer_mask_and_count_bits_match_upstream_layout() {
        assert_eq!(GEVNTCOUNT_MASK, 0xfffc);
        assert_eq!(GEVNTCOUNT_EHB, 1 << 31);
        assert_eq!(GEVNTSIZ_INTMASK, 1 << 31);
        assert_eq!(GEVNTSIZ_SIZE_MASK, 0xffff);
    }

    #[test]
    fn device_and_endpoint_events_keep_their_hardware_bit_layout() {
        assert_eq!(device_event_kind(device_event(DEVICE_EVENT_USB_RESET)), 1);
        let raw = endpoint_event(1, EP_EVENT_XFER_NOT_READY, EP_XFER_NOT_READY_STATUS);
        assert_eq!(endpoint_from_event(raw), 1);
        assert_eq!(endpoint_event_kind(raw), EP_EVENT_XFER_NOT_READY);
        assert_eq!(endpoint_event_status(raw), EP_XFER_NOT_READY_STATUS);
    }
}
