//! DWC3 device-mode encodings shared by the hardware path and its simulator.

pub const DCFG_SPEED_MASK: u32 = 7;
pub const DCFG_DEVADDR_MASK: u32 = 0x7f << 3;
pub const DCFG_HIGHSPEED: u32 = 0;
pub const DCFG_SUPERSPEED: u32 = 4;
pub const DCTL_RUN_STOP: u32 = 1 << 31;
pub const DSTS_CONNECTSPD_MASK: u32 = 7;
pub const DSTS_DEVCTRLHLT: u32 = 1 << 22;
pub const DSTS_DCNRD: u32 = 1 << 23;
pub const DSTS_SUPERSPEED: u32 = 4;

pub const DEVTEN_DISCONNECT: u32 = 1 << 0;
pub const DEVTEN_USB_RESET: u32 = 1 << 1;
pub const DEVTEN_CONNECT_DONE: u32 = 1 << 2;

pub const DEVICE_EVENT_KIND_SHIFT: u32 = 8;
pub const DEVICE_EVENT_KIND_MASK: u32 = 0x0f;
pub const DEVICE_EVENT_USB_RESET: u32 = 1;
pub const DEVICE_EVENT_CONNECT_DONE: u32 = 2;

pub const DEPCMD_CMDACT: u32 = 1 << 10;
pub const DEPCMD_HIPRI_FORCERM: u32 = 1 << 11;
pub const DEPCMD_PARAM_SHIFT: u32 = 16;
pub const DEPCMD_DEPSTARTCFG: u32 = 0x09;
pub const DEPCMD_ENDTRANSFER: u32 = 0x08;
pub const DEPCMD_STARTTRANSFER: u32 = 0x06;
pub const DEPCMD_SETTRANSFRESOURCE: u32 = 0x02;
pub const DEPCMD_SETEPCONFIG: u32 = 0x01;
pub const DEPCMD_ACTION_MODIFY: u32 = 2 << 30;

pub const DEPCFG_XFER_COMPLETE_EN: u32 = 1 << 8;
pub const DEPCFG_XFER_NOT_READY_EN: u32 = 1 << 10;
pub const DEPCFG_EP_NUMBER_SHIFT: u32 = 25;
pub const DEPCFG_EP_TYPE_CONTROL: u32 = 0;
pub const DEPCFG_MAX_PACKET_SHIFT: u32 = 3;

pub const TRB_HWO: u32 = 1 << 0;
pub const TRB_LST: u32 = 1 << 1;
pub const TRB_ISP_IMI: u32 = 1 << 10;
pub const TRB_IOC: u32 = 1 << 11;
pub const TRB_CONTROL_SETUP: u32 = 2 << 4;
pub const TRB_CONTROL_STATUS2: u32 = 3 << 4;
pub const TRB_CONTROL_STATUS3: u32 = 4 << 4;
pub const TRB_CONTROL_DATA: u32 = 5 << 4;

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
