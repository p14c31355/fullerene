# Driver SQ/CQ audit

Date: 2026-08-05

The audit distinguishes two boundaries:

1. The hardware queue boundary (DMA descriptors/rings owned by a device).
2. The kernel-to-driver boundary (owned request payload in a submission queue,
   followed by an owned completion entry).

The second boundary is required for work that can be requested by the kernel,
services, or protocol code. Controller setup and interrupt acknowledgement are
lifecycle operations, not device requests, and do not need an SQ/CQ pair.

| subsystem | hardware path | kernel/service path | result |
|---|---|---|---|
| HDA/audio | HDA CORB/RIRB plus cyclic stream DMA | typed audio SQ/CQ in `contexts/audio.rs` | compliant |
| AHCI | command list, command table, and device-to-host FIS | generic storage SQ/CQ in `drivers/registry.rs` | compliant |
| NVMe | admin and I/O SQ/CQ | generic storage SQ/CQ in `drivers/registry.rs` | compliant |
| RTSX/SD | controller command/data engine (no general host ring) | generic storage SQ/CQ wraps every block request | compliant |
| xHCI | command, transfer, and event rings | USB mass-storage requests use generic storage SQ/CQ | compliant |
| EHCI | async queue heads and qTD completion status | USB mass-storage requests use generic storage SQ/CQ | compliant |
| VirtIO | avail/used virtqueue | display is explicitly out of this task's scope | compliant / excluded |
| iwlwifi control | firmware TX/RX rings and command completion state | typed Wi-Fi SQ/CQ | compliant |
| iwlwifi data TX | firmware TX ring | `NetDevice::send_frame` enqueues an owned `DataTx` request; the scheduler submits it and consumes its CQ | compliant |
| iwlwifi data RX | firmware RX ring, polled by the driver and drained into its receive queue | `NetDevice::poll_frame` consumes that driver-owned receive buffer directly; no separate `DataRx` SQ/CQ is created | compliant |
| PIC / IOAPIC | interrupt delivery and EOI/acknowledgement | no request/response device operation | not applicable |
| IOMMU | page tables and invalidation transactions | mapping is a kernel memory capability operation | not applicable |

PS/2 input and framebuffer update paths are intentionally excluded. The
remaining entries either have a hardware SQ/CQ-equivalent or are infrastructure
whose operation is not a request that can be submitted and completed.

## Invariants

- A service-facing request owns all data crossing the SQ boundary.
- Firmware/MMIO access happens only in the scheduler's device phase.
- CQ consumption is separate from SQ execution.
- The synchronous `NetDevice` method is only a compatibility adapter: it
  reports whether the frame was accepted into the bounded SQ, not whether the
  hardware has already transmitted it.
- Legacy synchronous block and ioctl APIs remain adapters over the generic
  storage SQ/CQ until their ABI can return request handles.
