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

/// The active ANE DTB exposes this as `/amba/i2c@FFD73000`, a Synopsys
/// DesignWare master with `bus-id = 2`, IRQ 120, and a 19.2 MHz input clock.
/// These constants describe the verified resource only; no I2C register is
/// touched until the clock/reset/pinctrl ownership sequence is implemented.
pub const I2C2_BASE: usize = 0xffd7_3000;
pub const I2C2_IRQ: u32 = 120;
pub const I2C2_INPUT_CLOCK_HZ: u32 = 19_200_000;
pub const I2C2_CLOCK_PARENT_PHANDLE: u32 = 0x12f;
pub const I2C2_CLOCK_DIV: u32 = 1;
pub const I2C2_CLOCK_MULT: u32 = 1;
pub const I2C2_APB_CLOCK_PHANDLE: u32 = 0x28;
pub const I2C2_APB_CLOCK_HZ: u32 = 20_000_000;

/// The stock `ts_kit` bus number is 0.  In the active ANE DTB that maps to
/// `/amba/i2c@FFD71000`, not to I2C2.  The controller is currently disabled;
/// these constants are inventory only and deliberately have no accessors.
pub const TOUCH_I2C0_BASE: usize = 0xffd7_1000;
pub const TOUCH_I2C0_BUS_ID: u32 = 0;
pub const TOUCH_I2C0_STATUS_ENABLED: bool = false;
pub const TOUCH_I2C0_IRQ: u32 = 118;
pub const TOUCH_I2C0_ADDRESS: u8 = 0x38;
pub const TOUCH_RESET_GPIO: u32 = 145;
pub const TOUCH_IRQ_GPIO: u32 = 224;
pub const TOUCH_RESET_GPIO_PROVIDER: usize = 0xe8a1_d000;
pub const TOUCH_IRQ_GPIO_PROVIDER: usize = 0xfff1_d000;
pub const TOUCH_RESET_ACTIVE_LOW: bool = true;
pub const TOUCH_RESET_ASSERT_TO_RELEASE_MS: u32 = 2;
pub const TOUCH_RESET_SETTLE_MS: u32 = 8;
pub const TOUCH_IRQ_SYNC_LOW_MS: u32 = 50;
pub const TOUCH_IRQ_FALLING_EDGE: bool = true;
pub const TOUCH_VDD_REGULATOR_PHANDLE: u32 = 0x5f;
pub const TOUCH_VDD_REGULATOR_CTRL: u8 = 0x25;
pub const TOUCH_VDD_UV: u32 = 3_000_000;
pub const TOUCH_IO_REGULATOR_PHANDLE: u32 = 0x60;
pub const TOUCH_IO_REGULATOR_CTRL: u8 = 0x19;
pub const TOUCH_IO_UV: u32 = 1_800_000;
pub const TOUCH_REGULATORS: [super::common::regulator::RegulatorDescriptor; 2] = [
    super::common::regulator::RegulatorDescriptor::hisi_ldo(
        TOUCH_VDD_REGULATOR_PHANDLE,
        TOUCH_VDD_REGULATOR_CTRL,
        0x4e,
        0x07,
        TOUCH_VDD_UV,
        20_000,
        120,
    ),
    super::common::regulator::RegulatorDescriptor::hisi_ldo(
        TOUCH_IO_REGULATOR_PHANDLE,
        TOUCH_IO_REGULATOR_CTRL,
        0x42,
        0x07,
        TOUCH_IO_UV,
        20_000,
        120,
    ),
];
pub const TOUCH_I2C0_RESET_BIT: u32 = 3;
pub const TOUCH_I2C0_DELAY_OFF_US: u32 = 300;
pub const TOUCH_I2C0_RESET_BASE: usize = 0xFFD7E000;
pub const TOUCH_I2C0_RESET_ENABLE_OFFSET: usize = 0x20;
pub const TOUCH_I2C0_RESET_DISABLE_OFFSET: usize = 0x24;
pub const TOUCH_I2C0_RESET_STATUS_OFFSET: usize = 0x28;
pub const TOUCH_I2C0_INPUT_CLOCK_HZ: u32 = 19_200_000;
/// Standard DesignWare observation point for the disabled touch controller.
/// Calling this requires an independently verified clock/MMIO ownership state.
pub unsafe fn touch_i2c0_controller_snapshot() -> super::common::designware_i2c::ControllerSnapshot
{
    super::common::designware_i2c::snapshot(TOUCH_I2C0_BASE)
}
pub const TOUCH_I2C0_CLOCK_PHANDLE: u32 = 0x48;
pub const TOUCH_I2C0_CLOCK_PARENT_PHANDLE: u32 = 0x12f;
pub const TOUCH_I2C0_APB_CLOCK_PHANDLE: u32 = 0x28;
pub const TOUCH_I2C0_APB_CLOCK_HZ: u32 = 20_000_000;
pub const TOUCH_I2C0_MUX_BASE: usize = 0xfff1_1000;
pub const TOUCH_I2C0_CFG_BASE: usize = 0xfff1_1800;
pub const TOUCH_I2C0_SCL_MUX_OFFSET: usize = 0x14;
pub const TOUCH_I2C0_SDA_MUX_OFFSET: usize = 0x18;
pub const TOUCH_I2C0_SCL_CFG_OFFSET: usize = 0x1c;
pub const TOUCH_I2C0_SDA_CFG_OFFSET: usize = 0x20;
pub const TOUCH_I2C0_PIN_MUX_FUNCTION: u32 = 1;
pub const TOUCH_I2C0_PIN_CFG_FUNCTION: u32 = 0;
pub const TOUCH_I2C0_SCL_MUX_PHANDLE: u32 = 0x49;
pub const TOUCH_I2C0_SDA_MUX_PHANDLE: u32 = 0x4a;
pub const TOUCH_I2C0_SCL_CFG_PHANDLE: u32 = 0x4b;
pub const TOUCH_I2C0_SDA_CFG_PHANDLE: u32 = 0x4c;
pub const TOUCH_I2C0_PINCTRL_FUNC_MUX_VALUE: u32 = 1;
pub const TOUCH_I2C0_PINCTRL_FUNC_CFG_VALUE: u32 = 0;

/// Read-only clock description for the disabled stock touch bus.
pub const TOUCH_I2C0_CLOCKS: [super::common::clock::ClockDescriptor; 2] = [
    super::common::clock::ClockDescriptor::fixed_factor(
        TOUCH_I2C0_CLOCK_PHANDLE,
        TOUCH_I2C0_CLOCK_PARENT_PHANDLE,
        TOUCH_I2C0_INPUT_CLOCK_HZ,
        1,
        1,
    ),
    super::common::clock::ClockDescriptor::fixed(
        TOUCH_I2C0_APB_CLOCK_PHANDLE,
        TOUCH_I2C0_APB_CLOCK_HZ,
    ),
];

/// Active DTB power sources for the ANE battery path.  The coulomb counter
/// is enabled at the PMIC, while the charger alternatives are selected by
/// board firmware; no disabled charger node is activated by this metadata.
pub const BATTERY_COMPATIBLE: &[u8] = b"hisilicon,hi6555v100_coul";
pub const BATTERY_IRQ: u32 = 9;
pub const BATTERY_DESIGN_CAPACITY_MAH: u32 = 3000;
pub const CHARGER_I2C_BUS_BASE: usize = 0xfdf0_d000;
pub const CHARGER_I2C_ADDRESS: u8 = 0x6b;
pub const CHARGER_COMPATIBLE: &[u8] = b"huawei,hi6523_charger";

/// Read-only clock contract for the vendor DesignWare adapter.  The
/// descriptor documents the two DT clocks without claiming ownership or
/// performing a provider write.
pub const I2C2_CLOCKS: [super::common::clock::ClockDescriptor; 2] = [
    super::common::clock::ClockDescriptor::fixed_factor(
        0x53,
        I2C2_CLOCK_PARENT_PHANDLE,
        I2C2_INPUT_CLOCK_HZ,
        I2C2_CLOCK_DIV,
        I2C2_CLOCK_MULT,
    ),
    super::common::clock::ClockDescriptor::fixed(I2C2_APB_CLOCK_PHANDLE, I2C2_APB_CLOCK_HZ),
];
pub const I2C2_RESET_BASE: usize = 0xffd7_e000;
pub const I2C2_RESET_ENABLE: usize = 0x20;
pub const I2C2_RESET_DISABLE: usize = 0x24;
pub const I2C2_RESET_STATUS: usize = 0x28;
pub const I2C2_RESET_BIT: u32 = 5;

/// I2C2's active DT pinctrl states resolve to GPIO187/188.  The mux function
/// selects function 1 at offsets 0x24/0x28; the electrical configuration lives
/// in the companion window at 0x2c/0x30.  Idle only changes the mux back to
/// GPIO (0), while the electrical configuration remains the DT value 0.
pub const I2C2_MUX_BASE: usize = 0xfff1_1000;
pub const I2C2_CFG_BASE: usize = 0xfff1_1800;
pub const I2C2_SCL_MUX_OFFSET: usize = 0x24;
pub const I2C2_SDA_MUX_OFFSET: usize = 0x28;
pub const I2C2_SCL_CFG_OFFSET: usize = 0x2c;
pub const I2C2_SDA_CFG_OFFSET: usize = 0x30;
pub const I2C2_ACTIVE_MUX_VALUE: u32 = 1;
pub const I2C2_IDLE_MUX_VALUE: u32 = 0;
pub const I2C2_CFG_VALUE: u32 = 0;

/// Select the verified I2C2 pinctrl state. This is intentionally not called
/// from early bring-up yet: the caller must first establish clock ownership
/// and choose whether the bus is being handed to a live controller.
pub unsafe fn i2c2_select_pins(active: bool) {
    let mux = if active {
        I2C2_ACTIVE_MUX_VALUE
    } else {
        I2C2_IDLE_MUX_VALUE
    };
    core::ptr::write_volatile((I2C2_MUX_BASE + I2C2_SCL_MUX_OFFSET) as *mut u32, mux);
    core::ptr::write_volatile((I2C2_MUX_BASE + I2C2_SDA_MUX_OFFSET) as *mut u32, mux);
    core::ptr::write_volatile(
        (I2C2_CFG_BASE + I2C2_SCL_CFG_OFFSET) as *mut u32,
        I2C2_CFG_VALUE,
    );
    core::ptr::write_volatile(
        (I2C2_CFG_BASE + I2C2_SDA_CFG_OFFSET) as *mut u32,
        I2C2_CFG_VALUE,
    );
}

/// Reset I2C2 using the exact reset-controller tuple from the active DTB.
/// Returns true only when reset asserted and then deasserted according to the
/// two immediate status samples. A false result is inconclusive or failed;
/// callers must not continue into controller MMIO in that case.
pub unsafe fn i2c2_reset_controller() -> bool {
    let bit = 1u32 << I2C2_RESET_BIT;
    let reset = I2C2_RESET_BASE as *mut u32;
    core::ptr::write_volatile(reset.add(I2C2_RESET_ENABLE / 4), bit);
    let asserted = core::ptr::read_volatile(reset.add(I2C2_RESET_STATUS / 4)) & bit;
    core::ptr::write_volatile(reset.add(I2C2_RESET_DISABLE / 4), bit);
    let remaining = core::ptr::read_volatile(reset.add(I2C2_RESET_STATUS / 4)) & bit;
    asserted != 0 && remaining == 0
}

/// DesignWare register offsets used by the vendor HiSilicon adapter. Keep
/// these in the reusable backend rather than scattering literals through an
/// ANE touchscreen driver.
pub mod designware_i2c {
    pub const CON: usize = 0x000;
    pub const TAR: usize = 0x004;
    pub const DATA_CMD: usize = 0x010;
    pub const ENABLE: usize = 0x06c;
    pub const STATUS: usize = 0x070;
    pub const TXFLR: usize = 0x074;
    pub const RXFLR: usize = 0x078;
    pub const TX_ABRT_SOURCE: usize = 0x080;
    pub const COMP_PARAM_1: usize = 0x0f4;
}

/// Read-only state sampled from the DesignWare block.  This is deliberately a
/// snapshot rather than an initialization routine: clock, reset, and pinctrl
/// ownership still belong to firmware until a caller establishes otherwise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct I2c2ControllerSnapshot {
    pub status: u32,
    pub enable: u32,
    pub tx_fifo_level: u32,
    pub rx_fifo_level: u32,
    pub tx_abort_source: u32,
    pub component_parameters: u32,
}

impl I2c2ControllerSnapshot {
    /// A nonzero component-parameter register is the minimum evidence that
    /// the read reached a DesignWare instance rather than an unmapped value.
    pub const fn identifies_designware(self) -> bool {
        self.component_parameters != 0 && self.tx_fifo_level <= 0x100 && self.rx_fifo_level <= 0x100
    }
}

/// Sample only documented DesignWare registers.  The caller must invoke this
/// only after proving that the DT clock/reset ownership is available; a
/// disabled peripheral can still produce a platform-specific bus stall.
pub unsafe fn i2c2_controller_snapshot() -> I2c2ControllerSnapshot {
    let base = I2C2_BASE as *const u32;
    let read = |offset: usize| core::ptr::read_volatile(base.add(offset / 4));
    I2c2ControllerSnapshot {
        status: read(designware_i2c::STATUS),
        enable: read(designware_i2c::ENABLE),
        tx_fifo_level: read(designware_i2c::TXFLR),
        rx_fifo_level: read(designware_i2c::RXFLR),
        tx_abort_source: read(designware_i2c::TX_ABRT_SOURCE),
        component_parameters: read(designware_i2c::COMP_PARAM_1),
    }
}

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
        // v270: bit 0 = EnableGrp0, bit 1 = EnableGrp1. The loader may leave
        // the SPIs in group 1; enabling only group 0 would then leave every
        // SPI undelivered (v267/v268: the enabled DWC2 SPI never fired). Set
        // both so the group assignment the loader made cannot silently mute
        // an enabled line.
        core::ptr::write_volatile(gicc as *mut u32, gicc_ctlr | 0x3);
        core::ptr::write_volatile((gicc + 4) as *mut u32, 0xff);

        // Timer PPI 30: level-configured (GICD_ICFGR1, two bits per PPI,
        // 0b00 = level), a passable priority, and enabled. PPI registers are
        // banked per CPU interface, so this reaches CPU 0's timer.
        let icfgr1 = core::ptr::read_volatile((gicd + 0xc04) as *const u32);
        core::ptr::write_volatile((gicd + 0xc04) as *mut u32, icfgr1 & !(0b11 << 28));
        core::ptr::write_volatile((gicd + 0x400 + 30 * 4) as *mut u32, 0xa0);
        core::ptr::write_volatile((gicd + 0x100) as *mut u32, 1 << 30);
    }
    // v264 experiment: route the DWC2 SPI as an event source. Off by default;
    // the baseline boot keeps the loader's routing untouched.
    if option_env!("FULLERENE_ANE_USB_SPI").is_some() {
        enable_usb_spi();
    }
}

/// Enable the DWC2 SPI (159) at the distributor: level-high (the DTB's type 4),
/// a passable priority, targeted at CPU 0, then unmasked. Everything here is
/// read-modify-write over the same registers the timer setup touches, so the
/// PPI configuration above is preserved.
pub fn enable_usb_spi() {
    let gicd = GICD_BASE;
    let spi = USB_DWC2_SPI as usize;
    unsafe {
        // ICFGR: two bits per interrupt, 0b00 = level.
        let icfgr = gicd + 0xc00 + (spi / 16) * 4;
        let cfg_shift = (spi % 16) * 2;
        let v = core::ptr::read_volatile(icfgr as *const u32);
        core::ptr::write_volatile(icfgr as *mut u32, v & !(0b11u32 << cfg_shift));
        // IPRIORITYR: one byte per interrupt.
        let ipri = gicd + 0x400 + (spi / 4) * 4;
        let byte_shift = (spi % 4) * 8;
        let v = core::ptr::read_volatile(ipri as *const u32);
        core::ptr::write_volatile(
            ipri as *mut u32,
            (v & !(0xffu32 << byte_shift)) | (0xa0u32 << byte_shift),
        );
        // ITARGETSR: one byte per interrupt; CPU 0 = bit 0.
        let itarget = gicd + 0x800 + (spi / 4) * 4;
        let v = core::ptr::read_volatile(itarget as *const u32);
        core::ptr::write_volatile(
            itarget as *mut u32,
            (v & !(0xffu32 << byte_shift)) | (0x01u32 << byte_shift),
        );
        // ISENABLER4 (SPI 159 -> word 4, bit 31).
        core::ptr::write_volatile(
            (gicd + 0x100 + (spi / 32) * 4) as *mut u32,
            1u32 << (spi % 32),
        );
        // v271/v272 MEASURED BLOCKER: the DWC2 SPI is owned by the secure
        // world. The readback (GS= in the IQ line) shows grp=0 with en=1 and
        // tgt=1, and this write does NOT stick, which places us in the
        // non-secure view with security extensions enabled: GICD_CTLR bit 1
        // (EnableGrp1A) also reads back 0 after |= 0x3, and GICD_IGROUPR is
        // RAZ/WI for secure-owned interrupts, so EL1 cannot move SPI 159 into
        // group 1. A group-0 line belongs to the secure group, which the
        // non-secure CPU interface never signals (the timer PPI is group 1,
        // which is why it works). Hence S=0 with an enabled, targeted,
        // permanently silent line: the event-driven path cannot be built on
        // this SPI without a secure-world change. The write is kept as the
        // experiment; the readback is the proof.
        let igroupr = gicd + 0x080 + (spi / 32) * 4;
        let v = core::ptr::read_volatile(igroupr as *const u32);
        core::ptr::write_volatile(igroupr as *mut u32, v | (1u32 << (spi % 32)));
    }
}

/// v277: the responder duty window. The fastboot/USB handoff is the only boot
/// phase that needs the timer IRQ to service DWC2 - the boot blocks in the
/// predesktop-config wait until the host configures the device, and the pump
/// is what answers the descriptor reads. Every later phase (storage probe,
/// desktop, launchd) wants the boot thread's CPU and touches no DWC2, yet the
/// pump used to run forever, holding the CPU exactly where the boot thread
/// needed it. v275's pump-less boot proved the point by reaching code every
/// pumped boot stopped short of. This flag lets the boot close the window
/// once the handoff is done.
pub static USB_PUMP_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Whether the timer IRQ may service the responder this tick.
pub fn usb_pump_on() -> bool {
    USB_PUMP_ON.load(core::sync::atomic::Ordering::Relaxed)
}

/// Open (`true`) or close (`false`) the responder duty window.
pub fn usb_pump_window(enabled: bool) {
    USB_PUMP_ON.store(enabled, core::sync::atomic::Ordering::Relaxed);
}

/// v270: the DWC2 SPI's routing state, read back from the distributor and CPU
/// interface so one collection answers "why doesn't this line fire": the
/// group assignment, the enable bit, the target CPU and both interface
/// enables, all after the writes above.
pub fn usb_spi_gic_state() -> (u32, u32, u32, u32, u32) {
    let gicd = GICD_BASE;
    let gicc = GICC_BASE;
    let spi = USB_DWC2_SPI as usize;
    unsafe {
        let gicd_ctlr = core::ptr::read_volatile(gicd as *const u32);
        let gicc_ctlr = core::ptr::read_volatile(gicc as *const u32);
        let igroupr = core::ptr::read_volatile((gicd + 0x080 + (spi / 32) * 4) as *const u32);
        let isenabler = core::ptr::read_volatile((gicd + 0x100 + (spi / 32) * 4) as *const u32);
        let itargetsr = core::ptr::read_volatile((gicd + 0x800 + (spi / 4) * 4) as *const u32);
        (
            gicd_ctlr,
            gicc_ctlr,
            (igroupr >> (spi % 32)) & 1,
            (isenabler >> (spi % 32)) & 1,
            (itargetsr >> ((spi % 4) * 8)) & 0xff,
        )
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

/// Feed the watchdog: write the unlock key (unconditionally - see below),
/// then clear/reload the counter. The watchdog is left unlocked; we own it
/// for the rest of this boot.
pub fn watchdog_kick() {
    unsafe {
        // v177: unlock UNCONDITIONALLY before feeding. The lock register
        // read is not trustworthy on this bus (the DTXFSTS lesson), and a
        // kick that skips the unlock while the core is really locked is a
        // silent no-op - the watchdog then expires no matter how often we
        // "feed" it. Writing the key when already unlocked is harmless.
        //
        // v185: back to the proven v182 shape - two plain writes, NO inline
        // readbacks. The c30 boot (v182, 30+ min with kicks landing) used
        // this exact shape combined with the %16 instrument reads; the
        // v183 inline readbacks were a suspect in the v184 boot's failed
        // enumeration, and the %16 reads already provide the flush effect.
        core::ptr::write_volatile((WATCHDOG_BASE + 0xc00) as *mut u32, WATCHDOG_UNLOCK);
        core::ptr::write_volatile((WATCHDOG_BASE + 0x00c) as *mut u32, 1);
    }
}

/// Kept for the on-screen evidence: the last lock readback request. The
/// instrument now reads WDOGLOCK live through watchdog_state() instead of
/// piggybacking on the kick, so this is simply the live value's cache.
static mut WD_LOCK_RB: u32 = 0xffff_ffff;

pub fn watchdog_lock_rb() -> u32 {
    unsafe { core::ptr::addr_of!(WD_LOCK_RB).read() }
}

/// Store the live lock value for the instrument (callers pass what
/// watchdog_state() read).
pub fn set_watchdog_lock_rb(v: u32) {
    unsafe { core::ptr::addr_of_mut!(WD_LOCK_RB).write(v) }
}

/// The loader's SP805 LOAD value, captured on first use. The countdown
/// reloads from it on every kick, so half of it is a safe "running low"
/// threshold without hardcoding the loader's window.
static mut WD_LOAD: u32 = 0;

/// Call counter for the unconditional periodic kick.
static mut WD_CALLS: u32 = 0;

/// Feed the loader watchdog only when its countdown is running low.
/// Unlike the timer-IRQ feed (exceptions.rs `beats % 8`), this works from
/// any context - including boots where the timer never came up, and long
/// storage grinds where no console output happens. Call it from loops that
/// can spin for seconds (eMMC command/data waits, console narration).
pub fn watchdog_tend() {
    unsafe {
        let mut load = core::ptr::addr_of!(WD_LOAD).read();
        if load == 0 {
            load = core::ptr::read_volatile(WATCHDOG_BASE as *const u32);
            core::ptr::addr_of_mut!(WD_LOAD).write(load);
        }
        let value = core::ptr::read_volatile((WATCHDOG_BASE + 0x004) as *const u32);
        let half = if load / 2 > 0x2000 { load / 2 } else { 0x2000 };
        // v176: belt and suspenders. The SP805's registers are not fully
        // trustworthy on this bus (DTXFSTS taught that lesson), so also
        // kick unconditionally every 64th call even when the value read
        // looks healthy. Bounded cost (4 MMIO ops) and it guarantees a
        // feed during any sustained activity, value read or not.
        let n = core::ptr::addr_of!(WD_CALLS).read().wrapping_add(1);
        core::ptr::addr_of_mut!(WD_CALLS).write(n);
        if value < half || n % 64 == 0 {
            watchdog_kick();
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
/// The DWC2 interrupt line, from the active DTB:
/// `/hisi_usb@ff100000  interrupts = <0x00 0x9f 0x04>` = SPI 159, level-high.
/// The core asserts it whenever a masked GINTSTS event is pending. Leaving it
/// masked is exactly why the responder is timer-polled today - and that poll's
/// DWC2 MMIO reads are the 0.5-3.6 s stalls the v260 meter measured. Enabling
/// the line turns the responder event-driven and moves the MMIO work out of
/// the periodic timer path.
pub const USB_DWC2_SPI: u32 = 0x9f;
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

        // 10: v197 - keep the device soft-disconnected. This step used to
        // clear DCTL.SftDiscon and let the host enumerate immediately,
        // while the responder arm (ane_usb_dev::init), the timer-IRQ pump
        // and the storage bring-up were still starting. The host's first
        // descriptor reads then raced the bring-up: timeouts (-110) and
        // port resets landing mid-answer left stale words in the EP0
        // TxFIFO, so every later answer trailed one request behind
        // ("Љ" strings, -71/-75 storms) on roughly half the boots. The
        // attach now happens explicitly through usb_attach(), after the
        // storage bring-up, so the host's fresh enumeration meets an
        // armed responder.
        usb_or(core + 0x804, 0x2);
        usb_spin(0x190_0000); /* ~50 ms */

        // The readbacks: DCTL is the critical one - with the deferred
        // attach, SftDiscon must READ SET here (the host sees no device
        // until usb_attach()).
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

/// v197: the deferred attach - clears DCTL.SftDiscon (bit 1) so the host
/// sees the connect only after the responder is armed (ane_usb_dev::init)
/// and the storage bring-up has finished. The old flow attached during
/// usb_bringup() itself, and the host's enumeration raced the bring-up:
/// roughly half of the boots came up garbled ("Љ" descriptor shift) or
/// never enumerated (-110/-71/-75). Call once, from the boot path, after
/// the eMMC probe.
pub fn usb_attach() {
    // v209: the prepare_attach (EP0 cleanup + FIFO flush) experiment is
    // REVERTED for now: the v208 boot never attached at all (the host
    // saw no device for 4+ minutes) - the same signature as the v26-era
    // "GRSTCTL write wedged the core" before the first bus reset. Do not
    // touch GRSTCTL before the host's first reset (the v206 flushes at
    // handle_reset remain, and those are proven).
    unsafe {
        usb_mask(USB_CORE_BASE + 0x804, 0x2, 0);
    }
    usb_spin(0x190_0000); /* ~50 ms connect settle */
}

/// Re-enumerate the gadget after long pre-userspace work has finished.
///
/// The initial attach happens before the desktop renderer.  If the host gives
/// up while that renderer is monopolizing the boot thread, merely servicing
/// the existing gadget state is not enough: the pull-up must be dropped and
/// raised so the host starts a fresh descriptor exchange.
pub fn usb_reenumerate() {
    // v311: 0x1900 iterations (~2.6k). The old 0x190_0000 settle parked the
    // boot thread for 24+ min when the core never retired the DCTL write
    // (PC 0x5C17AC measured across v306-v309 long-lived boots). 2.6k
    // iterations still covers the vendor's settle intent on the observed
    // core speed while bounding the worst case to milliseconds.
    unsafe {
        usb_or(USB_CORE_BASE + 0x804, 0x2);
    }
    usb_spin(0x1900); /* bounded disconnect settle */
    unsafe {
        usb_mask(USB_CORE_BASE + 0x804, 0x2, 0);
    }
    usb_spin(0x1900); /* bounded connect settle */
}
