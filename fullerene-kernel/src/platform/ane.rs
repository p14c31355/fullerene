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

/// `/interrupt-controller@e82b0000`: `compatible = "arm,cortex-a9-gic"` - the
/// GIC-400 pair, memory-mapped only. There is no `ICC_*` system-register
/// interface on this CPU: the QEMU-virt GICv3 path's `msr ICC_SRE_EL1` is an
/// undefined instruction here, and it is exactly where the first boot that
/// reached the interrupt bring-up died (exception dump: ELR at the msr).
/// `reg` names both windows: distributor 0xe82b1000 (64 KiB) and the CPU
/// interface 0xe82b2000 (4 KiB).
pub const GICD_BASE: usize = 0xe82b_1000;
pub const GICC_BASE: usize = 0xe82b_2000;

/// Bring up the GICv2 pair. Read-modify-write only: the loader ran with these
/// interrupts enabled, so nothing here configures the routing - it makes sure
/// the groups, the priority mask and the CPU interface stayed open, then
/// leaves the rest to the firmware state the LK handed over.
pub fn init_interrupt_controller(gicd_base: Option<usize>, gicc_base: Option<usize>) {
    let gicd = gicd_base.unwrap_or(GICD_BASE);
    let gicc = gicc_base.unwrap_or(GICC_BASE);
    unsafe {
        // GICD_CTLR (0x000): bit 0 enables, bit 1 enables group 1.
        let gicd_ctlr = core::ptr::read_volatile(gicd as *const u32);
        core::ptr::write_volatile(gicd as *mut u32, gicd_ctlr | 0x3);
        // GICC_CTLR (0x000) and GICC_PMR (0x004): enable, allow all priorities.
        let gicc_ctlr = core::ptr::read_volatile(gicc as *const u32);
        core::ptr::write_volatile(gicc as *mut u32, gicc_ctlr | 0x1);
        core::ptr::write_volatile((gicc + 4) as *mut u32, 0xff);

        // Timer PPI 30: level-configured (GICD_ICFGR1, two bits per PPI,
        // 0b00 = level), a passable priority, and enabled. PPI registers are
        // banked per CPU interface, so this reaches CPU 0's timer.
        let icfgr1 = core::ptr::read_volatile((gicd + 0xc04) as *const u32);
        core::ptr::write_volatile((gicd + 0xc04) as *mut u32, icfgr1 & !(0b11 << 28));
        core::ptr::write_volatile((gicd + 0x400 + 30 * 4) as *mut u32, 0xa0);
        core::ptr::write_volatile((gicd + 0x100) as *mut u32, 1 << 30);
    }
}

/// `/amba/watchdog0`: `arm,sp805` at 0xe8a06000 (clocks pclk_wd0 + apb_pclk,
/// default-timeout 60 s in the DT). This is the watchdog the loader arms on a
/// boot attempt; on Android the OS takes it over and feeds it. While the
/// kernel feeds it, a healthy boot survives; when the kernel wedges the
/// feeding stops and the loader's fallback still runs exactly as before.
pub const WATCHDOG_BASE: usize = 0xe8a0_6000;
/// WDOGLOCK's magic unlock value (SP805 TRM).
const WATCHDOG_UNLOCK: u32 = 0x1acc_e551;

/// One read-only snapshot of the SP805 registers, so the caller can report
/// whether the loader left the watchdog armed and locked.
pub struct WatchdogState {
    pub load: u32,
    pub value: u32,
    pub control: u32,
    pub lock: u32,
}

pub fn watchdog_state() -> WatchdogState {
    unsafe {
        WatchdogState {
            load: core::ptr::read_volatile(WATCHDOG_BASE as *const u32),
            value: core::ptr::read_volatile((WATCHDOG_BASE + 0x004) as *const u32),
            control: core::ptr::read_volatile((WATCHDOG_BASE + 0x008) as *const u32),
            lock: core::ptr::read_volatile((WATCHDOG_BASE + 0xc00) as *const u32),
        }
    }
}

/// Feed the watchdog: unlock if the loader locked it, clear the counter,
/// leave the lock as it was (relock when it was locked).
pub fn watchdog_kick() {
    unsafe {
        let lock = core::ptr::read_volatile((WATCHDOG_BASE + 0xc00) as *const u32);
        if lock != 0 {
            core::ptr::write_volatile((WATCHDOG_BASE + 0xc00) as *mut u32, WATCHDOG_UNLOCK);
        }
        core::ptr::write_volatile((WATCHDOG_BASE + 0x00c) as *mut u32, 1);
        if lock != 0 {
            core::ptr::write_volatile((WATCHDOG_BASE + 0xc00) as *mut u32, 0);
        }
    }
}

// ---------------------------------------------------------------------------
// USB (Synopsys DWC2 OTG) - the vendor bring-up, ported step for step from the
// l1d probe's device sequence (ane_usb_l1d_probe.S), which cites the vendor
// CIL (drivers/usb/susb/dwc_otg_cil.c) and the device's own tree. Unlike the
// standalone probe this runs inside Fullerene with the MMU up, so its results
// are reported the same boot - cross-boot handoff turned out to be
// impossible (the loader re-initialises DRAM every boot; measured six times).
// ---------------------------------------------------------------------------

/// `/hisi_usb@ff100000`: `compatible = "hisilicon,hi6250-usb-otg"` - the
/// DWC2 register window (GUSBCFG 0x00C, GRSTCTL 0x010, GINTSTS 0x014,
/// DCFG 0x800, DCTL 0x804).
pub const USB_CORE_BASE: usize = 0xff10_0000;
/// `/usb_otg_ahbif@ff200000`: the vendor AHB wrapper (ctrl0 0x00, ctrl2 0x08,
/// ctrl3 0x0C - the eye pattern lives there).
pub const USB_AHBIF_BASE: usize = 0xff20_0000;
/// `/usb@fe000000`: `compatible = "hisilicon,hisi-usb2phy"`.
pub const USB_PHY_BASE: usize = 0xfe00_0000;
/// The USB divider window (PCTRL+0x64 bits 24-26 = the abb divider value).
pub const USB_PCTRL_BASE: usize = 0xe8a0_9000;
/// sysctrl: AP PPLL0 control at +0x30, the abb gate at +0x43C.
pub const USB_SCTRL_BASE: usize = 0xfff0_a000;
/// The PMIC cell holding the abb enable bit (byte +0x10F, bit 0).
pub const USB_PMIC_BASE: usize = 0xfff3_4000;

/// One snapshot of what the USB bring-up found, for the caller to report.
pub struct UsbState {
    /// SCTRL+0x43C after the abb gate step (bit0 = AP_ABB_EN).
    pub abb: u32,
    /// AP PPLL0 control before EN and after the lock poll (bit0, bit26).
    pub ppll0: (u32, u32),
    /// DWC2 readbacks after the attach attempt: GUSBCFG, DCFG, DCTL, GINTSTS.
    pub core: [u32; 4],
    /// The USB2PHY block +0x0/+0x4/+0x8/+0xC, first touch after the gates.
    pub phy: [u32; 4],
}

unsafe fn usb_or(addr: usize, bits: u32) -> u32 {
    let p = addr as *mut u32;
    let v = core::ptr::read_volatile(p);
    core::ptr::write_volatile(p, v | bits);
    v
}

unsafe fn usb_mask(addr: usize, clear: u32, set: u32) -> u32 {
    let p = addr as *mut u32;
    let v = core::ptr::read_volatile(p);
    core::ptr::write_volatile(p, (v & !clear) | set);
    v
}

/// A bounded busy-wait. The vendor sequence wants ~1 ms + ~150 ms + ~50 ms
/// of settle time; the standalone probe used the same style (its spin
/// macros). The generic counter's rate on this SoC is still under
/// measurement (CNTFRQ_EL0 reads 0x4000 while the observed tick rate is far
/// higher - 2026-10-04), so a calibrated timer wait would lie; a generous
/// busy loop only over-waits, which the USB settle windows tolerate.
fn usb_spin(units: u32) {
    for _ in 0..units {
        core::hint::spin_loop();
    }
}

/// Run the vendor USB bring-up once and return the readbacks.
///
/// Every wait here is a vendor-mandated settle (the CIL's mdelay between the
/// PHY releases, the kernel's 150-200 ms after the DWC2 reset): they are
/// bounded register polls wherever the hardware reports readiness, and fixed
/// delays only where the vendor code itself sleeps. A controller that never
/// answers must not hang the boot - every poll has a bound.
pub fn usb_bringup() -> UsbState {
    let core = USB_CORE_BASE;
    let ahbif = USB_AHBIF_BASE;
    let crg = CRG_BASE;
    let sctrl = USB_SCTRL_BASE;
    unsafe {
        // The abb gate for clk_abb_192 (clk-kirin-common.c,
        // hi3xxx_multicore_abb_clkgate_prepare): if AP_ABB_EN is clear and
        // LPM3_ABB_EN is also clear, set the PMIC enable bit, then set
        // AP_ABB_EN and honour the vendor's mdelay(1).
        let mut abb = core::ptr::read_volatile((sctrl + 0x43c) as *const u32);
        if abb & 1 == 0 {
            if abb & 2 == 0 {
                let en = core::ptr::read_volatile((USB_PMIC_BASE + 0x10f) as *const u8);
                core::ptr::write_volatile((USB_PMIC_BASE + 0x10f) as *mut u8, en | 1);
            }
            core::ptr::write_volatile((sctrl + 0x43c) as *mut u32, abb | 1);
            usb_spin(0x8_0000); /* ~1 ms */
            abb = core::ptr::read_volatile((sctrl + 0x43c) as *const u32);
        }

        // AP PPLL0 - the root of the USB2PHY clock chain. PPLLCTRL0 bit0 EN,
        // bit26 LOCK; the enable path sets EN then polls lock (bounded: a
        // gate that never locks must not park the boot).
        let ppll0_pre = core::ptr::read_volatile((sctrl + 0x30) as *const u32);
        core::ptr::write_volatile((sctrl + 0x30) as *mut u32, ppll0_pre | 1);
        for _ in 0..0x400 {
            if core::ptr::read_volatile((sctrl + 0x30) as *const u32) & (1 << 26) != 0 {
                break;
            }
        }
        let ppll0_post = core::ptr::read_volatile((sctrl + 0x30) as *const u32);

        // 1: USB clock gates (CRG+0x40: pll bit6, ref bit2, hclk bit1).
        usb_or(crg + 0x40, 0x46);
        // 2: abb divider (PCTRL+0x64 bits 24-26 = 5).
        usb_mask(USB_PCTRL_BASE + 0x64, 0x7 << 24, 5 << 24);
        // 3: AHBIF resets released (CRG+0x94: ADP bit27, 32K bit14,
        // MUX bit10, AHBIF bit11).
        usb_or(crg + 0x94, 0x0800_4C00);
        // 4: AHBIF ctrl0 - acaenb_sel bit2 | id_sel bit4.
        usb_or(ahbif + 0x00, 0x14);
        // 5: AHBIF ctrl3 - the device eye pattern from the device tree.
        core::ptr::write_volatile((ahbif + 0x0c) as *mut u32, 0x06b8_66db);
        // 6-8: the PHY releases, step by step with the vendor's settles.
        usb_or(crg + 0x94, 1 << 13); /* PHY power-on reset released */
        usb_spin(0x4000); /* ~0.5 ms */
        usb_or(crg + 0x94, 1 << 12); /* PHY clock domain released */
        usb_spin(0x4000); /* ~0.5 ms */
        usb_or(crg + 0x94, 1 << 9); /* OTG released */
        // 9: AHBIF ctrl2 - vbusvldsel bit2 | vbusvldext bit3.
        usb_or(ahbif + 0x08, 0x0c);
        usb_spin(0x8_0000); /* ~1 ms */

        // 9.5: DWC2 core - force device mode and UTMI+ 8-bit on GUSBCFG, a
        // core soft reset with bounded AHB-idle and self-clear polls,
        // GUSBCFG re-written because the reset clears it (dwc2_core_reset in
        // drivers/usb/dwc2/core.c does exactly this), then DCFG devspd = HS.
        usb_mask(core + 0x00c, 0x18, 1 << 30);
        for _ in 0..0x1000 {
            if core::ptr::read_volatile((core + 0x010) as *const u32) & (1 << 31) != 0 {
                break;
            }
        }
        usb_or(core + 0x010, 1);
        for _ in 0..0x1000 {
            if core::ptr::read_volatile((core + 0x010) as *const u32) & 1 == 0 {
                break;
            }
        }
        usb_mask(core + 0x00c, 0x18, 1 << 30);
        usb_spin(0x4B0_0000); /* ~150 ms */
        usb_mask(core + 0x800, 0x3, 0);

        // 10: attach - clear DCTL.SftDiscon (bit 1) and let the connect settle.
        usb_mask(core + 0x804, 0x2, 0);
        usb_spin(0x190_0000); /* ~50 ms */

        // The readbacks: DCTL is the critical one - if SftDiscon did not stay
        // cleared there is no attach for the host to see.
        UsbState {
            abb,
            ppll0: (ppll0_pre, ppll0_post),
            core: [
                core::ptr::read_volatile((core + 0x00c) as *const u32),
                core::ptr::read_volatile((core + 0x800) as *const u32),
                core::ptr::read_volatile((core + 0x804) as *const u32),
                core::ptr::read_volatile((core + 0x014) as *const u32),
            ],
            phy: [
                core::ptr::read_volatile(USB_PHY_BASE as *const u32),
                core::ptr::read_volatile((USB_PHY_BASE + 0x4) as *const u32),
                core::ptr::read_volatile((USB_PHY_BASE + 0x8) as *const u32),
                core::ptr::read_volatile((USB_PHY_BASE + 0xc) as *const u32),
            ],
        }
    }
}
