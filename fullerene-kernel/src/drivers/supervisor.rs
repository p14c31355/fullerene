//! Failure supervision for process-isolated drivers.
//!
//! Most drivers currently live in the kernel and therefore have no process to
//! terminate.  A process-backed driver can bind its PID to a PCI function;
//! fatal hardware failures then remove that binding and terminate the driver
//! only after the device driver has completed its resource cleanup.

use alloc::vec::Vec;
use nitrogen::DriverError;
use nitrogen::pci::PciDevice;
use spin::Mutex;

use crate::process::ProcessId;

#[derive(Debug, Clone, Copy)]
struct Binding {
    bus: u8,
    device: u8,
    function: u8,
    pid: ProcessId,
}

static BINDINGS: Mutex<Vec<Binding>> = Mutex::new(Vec::new());

/// Bind a process-backed driver to a PCI function.
pub fn bind_driver_process(device: &PciDevice, pid: ProcessId) {
    let mut bindings = BINDINGS.lock();
    bindings.retain(|binding| {
        binding.bus != device.bus
            || binding.device != device.device
            || binding.function != device.function
    });
    bindings.push(Binding {
        bus: device.bus,
        device: device.device,
        function: device.function,
        pid,
    });
}

/// Remove a process binding without terminating the process.
pub fn unbind_driver_process(device: &PciDevice) {
    BINDINGS.lock().retain(|binding| {
        binding.bus != device.bus
            || binding.device != device.device
            || binding.function != device.function
    });
}

/// Kill the registered driver process after its hardware cleanup has finished.
///
/// This function deliberately does not fall back to the current process: the
/// current process may only be an ioctl caller, not the driver owner.
pub fn kill_failed_driver(device: &PciDevice, error: DriverError) {
    let pid = {
        let mut bindings = BINDINGS.lock();
        bindings
            .iter()
            .position(|binding| {
                binding.bus == device.bus
                    && binding.device == device.device
                    && binding.function == device.function
            })
            .map(|index| bindings.swap_remove(index).pid)
    };

    if let Some(pid) = pid {
        log::error!(
            "DriverSupervisor: killing driver pid={} for {:02x}:{:02x}.{} after {}",
            pid.0,
            device.bus,
            device.device,
            device.function,
            error,
        );
        crate::process::terminate_process(pid, 128 + 6);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> PciDevice {
        PciDevice {
            bus: 0,
            device: 1,
            function: 0,
            handle: 0,
            vendor_id: 0x8086,
            device_id: 0x5845,
            class_code: 0x01,
            subclass: 0x08,
            prog_if: 0,
            header_type: 0,
        }
    }

    #[test]
    fn binding_is_replaced_and_unbound_without_touching_other_devices() {
        let first = device();
        let second = PciDevice { device: 2, ..first };
        bind_driver_process(&first, ProcessId(11));
        bind_driver_process(&first, ProcessId(12));
        bind_driver_process(&second, ProcessId(13));
        unbind_driver_process(&first);

        let bindings = BINDINGS.lock();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].pid, ProcessId(13));
        drop(bindings);
        unbind_driver_process(&second);
    }
}
