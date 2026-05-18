# VirtIO-GPU Driver: Architectural Review and Debugging Roadmap

## 1. Executive Summary
The current implementation of the VirtIO-GPU driver in `petroleum` reaches a functional state in the initialization sequence but fails to trigger command processing by the device. This document outlines the proposed path to verify the driver's adherence to the VirtIO 1.0+ specification and identify potential environment-level misconfigurations.

## 2. Specification Compliance Review
We need to audit our implementation of the following VirtIO 1.0+ requirements:

- [ ] **Queue Memory Alignment:** Verify if the descriptor table, avail ring, and used ring meet the mandatory alignment requirements (typically page-aligned).
- [ ] **Notification Mechanism:** Re-validate the notification trigger. Are we correctly using the `queue_notify_off` and `notify_off_multiplier` provided by the device?
- [ ] **Memory Barriers:** Ensure `SeqCst` and `Release` memory barriers are correctly placed around the `avail` index updates and `notify` writes to prevent CPU/compiler reordering.
- [ ] **Endianness:** Confirm all multibyte fields in the command structures and ring buffers are written in the correct endianness (VirtIO uses Little Endian).
- [ ] **Interrupt Handling:** If the device relies on IRQs for completion signaling, our driver currently uses polling (`wait_used`). This must be revisited to ensure the device actually triggers an interrupt even if we are polling.

## 3. Environment & Configuration Re-evaluation
The issue might stem from the QEMU configuration or the virtual PCI environment:

- [ ] **MSI-X/Interrupt Injection:** The current `MSI-X` enablement test was inconclusive. We need to verify if the virtual machine is correctly routing interrupts from the GPU.
- [ ] **BAR Mapping:** Verify the MMIO mapping of the GPU's BARs. Are we accessing the correct memory regions, or is there an offset discrepancy?
- [ ] **Device Capabilities:** Perform a full dump of the device's PCI capabilities to ensure we are correctly interpreting the `VIRTIO_PCI_CAP_...` pointers.

## 4. Proposed Debugging Steps
1.  **Isolation Testing:** Create a minimal, self-contained test that performs a single `VIRTIO_GPU_CMD_GET_CAPSET_INFO` command to isolate the issue from complex graphics rendering.
2.  **PCI Register Sanity Check:** Write a tool to read back all VirtIO registers after configuration to confirm that the values we wrote (queue addresses, enable bit) are what the device actually stores.
3.  **Trace Analysis:** Use QEMU tracepoints (if available in the environment) to monitor the VirtIO GPU device's reaction to our memory writes.

## 5. Next Steps
- [ ] Conduct the Specification Compliance Review.
- [ ] Implement a more robust register verification check.
- [ ] If issues persist, evaluate if the current implementation of Type 5 PCI Access is fully compatible with the device.
