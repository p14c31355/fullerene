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

/// The CRG window the UART's clock gate lives in. `/sysctrl@fff0a000`'s
/// sibling; the USB probe's clock step programs the same block at +0x40.
pub const CRG_BASE: usize = 0xfff3_5000;

/// Open the early console's clock gate before the first PL011 register touch.
///
/// The property lives on the clock node, not the uart node: the device's own
/// tree (fdt dump, `/clocks@0/crgctrl@0xfff35000/clk_uart0`) carries
/// `hisilicon,hi3xxx-clkgate = <0x00000020 0x00000400>` - register 0x20,
/// bit 10 - the vendor encoding the USB probe already uses for CRG+0x40, and
/// that clock node is what gates `/amba/uart@fdf02000` (reset-reg-base
/// 0xfff35000). A loader that ran a console normally leaves the gate open and
/// re-opening it is a no-op, but a closed gate turns the first `init_at`
/// register write into a bus access nobody answers, which is exactly how a
/// bring-up dies between mark 6 and mark 7 without leaving a trace. Paid once
/// per boot.
pub fn open_uart_clock_gate() {
    unsafe {
        let gate = CRG_BASE as *mut u32;
        let value = core::ptr::read_volatile(gate.add(0x20 / 4));
        core::ptr::write_volatile(gate.add(0x20 / 4), value | 0x400);
    }
}

/// The hi6250 MMIO aperture consumed by the early identity map's attribute
/// split (`mmu::init` via `mmu::is_mmio`).
///
/// Read off the device's own tree (`firmware/ane_dtb/fdt_properties.txt`):
/// every register-bearing peripheral node sits at or above 0xE0000000
/// (`@e8...` DSS/USB, `@fdf02000` UART, `@fff35000` CRG); the sub-0xE0000000
/// nodes are all `/reserved-memory` carveouts plus one DSP TCM window
/// (`/dsp@49000000`), which belong on the Normal side. `/memory@0` places
/// RAM at 0x00400000..0xE0000000 plus 0x1_0000_0000..0x1_2000_0000. The
/// whole top 512 MiB is therefore Device memory. No RAM range may appear
/// here: a 2 MiB identity block covering the UART as Normal memory makes
/// the first post-MMU console write touch device registers through a
/// cacheable mapping, and the boot parks between step marks 9 and a exactly
/// there (2026-10-04, text-console readout). The legacy shared list also
/// marked several plain RAM ranges as Device, giving the heap uncached,
/// unaligned-faulting semantics.
pub const MMIO_RANGES: &[(u64, u64)] = &[(0xE000_0000, 0xFFFF_FFFF)];
