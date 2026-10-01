//! HiSilicon hi6250 - Huawei P20 Lite (ANE-LX2J).
//!
//! Every address here was read from the device itself rather than from a
//! datasheet or a datasheet-derived DTS: the device tree dumped from the
//! running system (`/sys/firmware/fdt`, 475,816 bytes) together with
//! `/proc/iomem`. That tree is what the LK hands over in x0, and every
//! runtime resource is still taken from it. These constants exist only for
//! what must work before the tree can be parsed.

/// `/amba/uart@fdf02000`: `compatible = "arm,pl011"`, `status = "ok"`,
/// clock `clk_uart0_fac`. Of the three PL011s in the tree this is the one
/// marked enabled (`uart@fdf00000` is disabled; `uart@fdf03000`'s status is
/// not "ok"), so it is the early console - driven by the PL011 path
/// (`uart::init_at`), not the Qualcomm GENI one.
///
/// Note for bring-up: this is a compiled-in first write. The console rate is
/// programmed for a 24 MHz reference clock (the QEMU virt value), which need
/// not match this SoC's; nothing is listening on the other end either, so the
/// value of this write is that it exercises the bus, not that it prints.
pub const UART_BASE: usize = 0xfdf0_2000;
