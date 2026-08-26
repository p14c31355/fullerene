//! Deterministic EP0 and DWC3-device-mode test for QEMU virt.

use super::{
    uart,
    usb_dwc3_sim::Dwc3DeviceModel,
    usb_protocol::{ControlAction, Ep0Simulator},
    usb_regs::{
        DEVICE_EVENT_CONNECT_DONE, DEVICE_EVENT_USB_RESET, EP_EVENT_TRANSFER_COMPLETE,
        EP_EVENT_XFER_NOT_READY, EP_XFER_NOT_READY_STATUS, device_event_kind, endpoint_event_kind,
        endpoint_event_status, endpoint_from_event,
    },
};

fn expect_action(actual: ControlAction, expected: ControlAction, name: &str) -> bool {
    if actual == expected {
        report_pass(name)
    } else {
        report_fail(name)
    }
}

fn expect_model_queue(ok: bool, name: &str) -> bool {
    expect_action(
        if ok {
            ControlAction::Setup
        } else {
            ControlAction::Stall
        },
        ControlAction::Setup,
        name,
    )
}

fn expect_device_event(model: &mut Dwc3DeviceModel, kind: u32, name: &str) -> bool {
    if !model.inject_device_event(kind) {
        return report_fail(name);
    }
    let Some(raw) = model.pop_event() else {
        return report_fail(name);
    };
    if device_event_kind(raw) == kind {
        report_pass(name)
    } else {
        report_fail(name)
    }
}

fn expect_transfer_complete(model: &mut Dwc3DeviceModel, endpoint: u32, name: &str) -> bool {
    if !model.inject_transfer_complete(endpoint) {
        return report_fail(name);
    }
    let Some(raw) = model.pop_event() else {
        return report_fail(name);
    };
    if endpoint_from_event(raw) == endpoint
        && endpoint_event_kind(raw) == EP_EVENT_TRANSFER_COMPLETE
    {
        report_pass(name)
    } else {
        uart::put_hex("qemu usb sim: bad transfer event=", raw as u64);
        report_fail(name)
    }
}

fn expect_setup(model: &mut Dwc3DeviceModel, packet: [u8; 8], name: &str) -> bool {
    if !model.receive_setup(packet) || model.setup_packet() != packet {
        return report_fail(name);
    }
    let Some(raw) = model.pop_event() else {
        return report_fail(name);
    };
    if endpoint_from_event(raw) == 0 && endpoint_event_kind(raw) == EP_EVENT_TRANSFER_COMPLETE {
        report_pass(name)
    } else {
        report_fail(name)
    }
}

fn expect_not_ready(model: &mut Dwc3DeviceModel, endpoint: u32, name: &str) -> bool {
    if !model.inject_xfer_not_ready(endpoint) {
        return report_fail(name);
    }
    let Some(raw) = model.pop_event() else {
        return report_fail(name);
    };
    if endpoint_from_event(raw) == endpoint
        && endpoint_event_kind(raw) == EP_EVENT_XFER_NOT_READY
        && endpoint_event_status(raw) == EP_XFER_NOT_READY_STATUS
    {
        report_pass(name)
    } else {
        uart::put_hex("qemu usb sim: bad not-ready event=", raw as u64);
        report_fail(name)
    }
}

pub fn run() -> bool {
    uart::puts("qemu usb sim: begin\n");
    let mut ep0 = Ep0Simulator::new();
    let mut dwc3 = Dwc3DeviceModel::new();
    let mut response = [0u8; 512];
    let mut passed = true;

    passed &= expect_model_queue(
        dwc3.configure_usb2_control_endpoints(),
        "DWC3 endpoint configuration",
    );
    passed &= expect_model_queue(dwc3.queue_setup([0; 8]), "DWC3 SETUP TRB queue");
    passed &= expect_device_event(&mut dwc3, DEVICE_EVENT_USB_RESET, "USB reset event");
    passed &= expect_device_event(&mut dwc3, DEVICE_EVENT_CONNECT_DONE, "connect-done event");

    let device_setup = [0x80, 6, 0, 1, 0, 0, 64, 0];
    passed &= expect_setup(&mut dwc3, device_setup, "device SETUP received");
    passed &= expect_action(
        ep0.on_setup(device_setup, &mut response),
        ControlAction::DataIn(18),
        "GET_DESCRIPTOR device",
    );
    passed &= expect_model_queue(dwc3.queue_data_in(18), "DWC3 DATA IN TRB queue");
    passed &= expect_transfer_complete(&mut dwc3, 1, "DATA IN complete");
    passed &= expect_action(
        ep0.on_transfer_complete(),
        ControlAction::StatusOut,
        "device data complete",
    );
    passed &= expect_model_queue(dwc3.queue_status(0, true), "DWC3 STATUS OUT TRB queue");
    passed &= expect_not_ready(&mut dwc3, 0, "STATUS OUT not-ready event");
    passed &= expect_transfer_complete(&mut dwc3, 0, "STATUS OUT complete");
    passed &= expect_action(
        ep0.on_transfer_complete(),
        ControlAction::Setup,
        "device status complete",
    );
    passed &= expect_model_queue(dwc3.queue_setup([0; 8]), "device EP0 rearm");

    let config_setup = [0x80, 6, 0, 2, 0, 0, 255, 0];
    passed &= expect_setup(&mut dwc3, config_setup, "config SETUP received");
    passed &= expect_action(
        ep0.on_setup(config_setup, &mut response),
        ControlAction::DataIn(18),
        "GET_DESCRIPTOR config",
    );
    passed &= expect_model_queue(dwc3.queue_data_in(18), "DWC3 config DATA IN TRB queue");
    passed &= expect_transfer_complete(&mut dwc3, 1, "config DATA IN complete");
    passed &= expect_action(
        ep0.on_transfer_complete(),
        ControlAction::StatusOut,
        "config data complete",
    );
    passed &= expect_model_queue(
        dwc3.queue_status(0, true),
        "DWC3 config STATUS OUT TRB queue",
    );
    passed &= expect_not_ready(&mut dwc3, 0, "config STATUS OUT not-ready event");
    passed &= expect_transfer_complete(&mut dwc3, 0, "config STATUS OUT complete");
    passed &= expect_action(
        ep0.on_transfer_complete(),
        ControlAction::Setup,
        "config status complete",
    );
    passed &= expect_model_queue(dwc3.queue_setup([0; 8]), "config EP0 rearm");

    let set_address = [0, 5, 7, 0, 0, 0, 0, 0];
    passed &= expect_setup(&mut dwc3, set_address, "SET_ADDRESS SETUP received");
    passed &= expect_action(
        ep0.on_setup(set_address, &mut response),
        ControlAction::StatusIn,
        "SET_ADDRESS queue",
    );
    passed &= expect_model_queue(
        dwc3.queue_status(1, false),
        "DWC3 SET_ADDRESS STATUS IN TRB queue",
    );
    passed &= expect_transfer_complete(&mut dwc3, 1, "SET_ADDRESS status complete");
    passed &= expect_action(
        ep0.on_transfer_complete(),
        ControlAction::Setup,
        "SET_ADDRESS complete",
    );
    passed &= expect_model_queue(dwc3.queue_setup([0; 8]), "SET_ADDRESS EP0 rearm");

    let set_configuration = [0, 9, 1, 0, 0, 0, 0, 0];
    passed &= expect_setup(
        &mut dwc3,
        set_configuration,
        "SET_CONFIGURATION SETUP received",
    );
    passed &= expect_action(
        ep0.on_setup(set_configuration, &mut response),
        ControlAction::StatusIn,
        "SET_CONFIGURATION queue",
    );
    passed &= expect_model_queue(
        dwc3.queue_status(1, false),
        "DWC3 SET_CONFIGURATION STATUS IN TRB queue",
    );
    passed &= expect_transfer_complete(&mut dwc3, 1, "SET_CONFIGURATION status complete");
    passed &= expect_action(
        ep0.on_transfer_complete(),
        ControlAction::Setup,
        "SET_CONFIGURATION complete",
    );
    passed &= expect_model_queue(dwc3.queue_setup([0; 8]), "SET_CONFIGURATION EP0 rearm");

    let final_state = ep0.address() == 7
        && ep0.configured()
        && dwc3.endpoint_active(0)
        && !dwc3.endpoint_active(1);
    passed &= expect_action(
        if final_state {
            ControlAction::Setup
        } else {
            ControlAction::Stall
        },
        ControlAction::Setup,
        "final EP0/controller state",
    );

    uart::puts(if passed {
        "qemu usb sim: PASS\n"
    } else {
        "qemu usb sim: FAIL\n"
    });
    passed
}

fn report_pass(name: &str) -> bool {
    uart::puts("qemu usb sim: ");
    uart::puts(name);
    uart::puts(" PASS\n");
    true
}

fn report_fail(name: &str) -> bool {
    uart::puts("qemu usb sim: ");
    uart::puts(name);
    uart::puts(" FAIL\n");
    false
}
