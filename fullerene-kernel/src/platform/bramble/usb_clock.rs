use super::{
    ClockProvider, ClockResource, UsbBusVote, set_usb_resource_state, usb_clock_plan, usb_resources,
};

const GCC_BRANCH_CLK_OFF: u32 = 1 << 31;

unsafe fn wait_for_branch_state(address: *mut u32, enabled: bool) -> bool {
    unsafe {
        for _ in 0..500_000u32 {
            let value = core::ptr::read_volatile(address);
            let on = value & 1 != 0;
            let off = value & GCC_BRANCH_CLK_OFF != 0;
            if on == enabled && off == !enabled {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

/// Switch the GCC branch resources required by the Qualcomm glue. This is
/// intentionally platform-owned: DWC3 should not need to know GCC offsets or
/// which of the six controller clocks are present in the DT.
pub unsafe fn enable_usb_clock_branches() -> bool {
    let resources = usb_resources();
    let mut ok = true;
    for clock in resources.controller_clocks {
        if clock.provider != ClockProvider::Gcc {
            continue;
        }
        let address = (resources.gcc_base + clock.branch_offset) as *mut u32;
        let value = unsafe { core::ptr::read_volatile(address) } | 1;
        unsafe { core::ptr::write_volatile(address, value) };
        ok &= unsafe { wait_for_branch_state(address, true) };
    }
    for clock in resources.qmp_clocks {
        if clock.provider != ClockProvider::Gcc {
            continue;
        }
        let address = (resources.gcc_base + clock.branch_offset) as *mut u32;
        let value = unsafe { core::ptr::read_volatile(address) } | 1;
        unsafe { core::ptr::write_volatile(address, value) };
        ok &= unsafe { wait_for_branch_state(address, true) };
    }
    if ok {
        set_usb_resource_state(|state| state.clock_branches_enabled = true);
    }
    ok
}

/// Re-enable only the four clocks consumed by the Lito QMP USB+DP PHY.
///
/// The Android PHY driver prepares these in `ref_clk_src`, `com_aux_clk`,
/// `aux_clk`, then `pipe_clk` order.  A SuperSpeed Fastboot handoff must
/// restore that ownership before touching QMP registers, but must not retune
/// the live DWC3 core/UTMI clocks as a side effect of a PHY-only operation.
pub unsafe fn enable_usb_qmp_clock_branches() -> bool {
    unsafe {
        let resources = usb_resources();
        let Some(aux) = resources
            .qmp_clocks
            .iter()
            .find(|clock| clock.name == "aux")
        else {
            return false;
        };
        if aux.provider != ClockProvider::Gcc
            || aux.source_offset == 0
            || !configure_rcg(aux.source_offset, 0, 0)
        {
            return false;
        }

        let mut ok = true;
        for name in ["com_aux", "aux", "pipe"] {
            let Some(clock) = resources.qmp_clocks.iter().find(|clock| clock.name == name) else {
                return false;
            };
            if clock.provider != ClockProvider::Gcc {
                return false;
            }
            let address = (resources.gcc_base + clock.branch_offset) as *mut u32;
            let value = core::ptr::read_volatile(address) | 1;
            core::ptr::write_volatile(address, value);
            ok &= wait_for_branch_state(address, true);
        }
        ok
    }
}

/// Gate the USB-specific GCC branches after the controller has stopped and
/// the interconnect vote has been dropped.  XO is shared with the rest of the
/// SoC and is intentionally left enabled; Linux's clock framework applies the
/// same ownership distinction to the USB clock handles.
pub unsafe fn disable_usb_clock_branches() -> bool {
    let resources = usb_resources();
    let mut ok = true;
    for clock in resources.controller_clocks {
        if clock.provider != ClockProvider::Gcc || clock.name == "xo" {
            continue;
        }
        let address = (resources.gcc_base + clock.branch_offset) as *mut u32;
        let value = unsafe { core::ptr::read_volatile(address) } & !1;
        unsafe { core::ptr::write_volatile(address, value) };
        ok &= unsafe { wait_for_branch_state(address, false) };
    }
    for clock in resources.qmp_clocks {
        if clock.provider != ClockProvider::Gcc {
            continue;
        }
        let address = (resources.gcc_base + clock.branch_offset) as *mut u32;
        let value = unsafe { core::ptr::read_volatile(address) } & !1;
        unsafe { core::ptr::write_volatile(address, value) };
        ok &= unsafe { wait_for_branch_state(address, false) };
    }
    if ok {
        set_usb_resource_state(|state| state.clock_branches_enabled = false);
    }
    ok
}

const GCC_CMD_UPDATE: u32 = 1 << 0;
const GCC_CFG_SRC_DIV_MASK: u32 = 0xff;
const GCC_CFG_SRC_SEL_MASK: u32 = 0x7 << 8;

/// Raw GCC state used by the Bramble USB handoff diagnostics. Keeping this
/// in the platform clock layer makes the register offsets DT/resource-owned,
/// while the DWC3 driver can retain the values beside GUSB2PHYCFG snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbClockRegisterState {
    /// RCG configuration words for the controller core and mock UTMI source.
    pub core_source_config: u32,
    pub utmi_source_config: u32,
    /// Branch status words in the six-clock DT order:
    /// core, iface, bus_aggr, utmi, sleep, xo.
    pub controller_branches: [u32; 6],
    /// QMP PHY branch status words in DT order: aux, pipe, ref, com_aux.
    /// The RPMh ref entry has no GCC branch and is reported as zero.
    pub qmp_branches: [u32; 4],
}

#[inline]
unsafe fn gcc_reg(offset: usize) -> *mut u32 {
    (usb_resources().gcc_base + offset) as *mut u32
}

/// Program one Qualcomm RCG2 clock source and commit the change.
///
/// The Lito GCC driver describes the USB master clock as parent 1 divided by
/// 8 and the mock UTMI clock as the BI_TCXO parent with encoded divider 0. Keeping this in the
/// platform layer prevents the DWC3 driver from depending on GCC register
/// layout.
unsafe fn configure_rcg(cmd_offset: usize, parent: u32, divider: u32) -> bool {
    unsafe {
        let cfg = gcc_reg(cmd_offset + 0x4);
        let mut value = core::ptr::read_volatile(cfg);
        value &= !(GCC_CFG_SRC_DIV_MASK | GCC_CFG_SRC_SEL_MASK);
        value |= divider & GCC_CFG_SRC_DIV_MASK;
        value |= (parent << 8) & GCC_CFG_SRC_SEL_MASK;
        core::ptr::write_volatile(cfg, value);
        let _ = core::ptr::read_volatile(cfg);

        let cmd = gcc_reg(cmd_offset);
        let value = core::ptr::read_volatile(cmd) | GCC_CMD_UPDATE;
        core::ptr::write_volatile(cmd, value);
        let _ = core::ptr::read_volatile(cmd);

        for _ in 0..500_000u32 {
            if core::ptr::read_volatile(cmd) & GCC_CMD_UPDATE == 0 {
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

/// Select the rates required by the Lito USB glue before its branch clocks
/// are enabled. The values mirror `qcom,core-clk-rate = 133333333` and
/// `qcom,core-clk-rate-hs = 66666667`'s source tables.
pub unsafe fn configure_usb_clocks(vote: UsbBusVote) -> bool {
    let plan = usb_clock_plan(vote);
    if !plan.branches_enabled {
        return true;
    }
    if !unsafe { configure_usb_controller_clocks(vote) } {
        return false;
    }
    unsafe {
        let resources = usb_resources();
        // The QMP AUX and COM_AUX branches share the 19.2 MHz
        // gcc_usb3_prim_phy_aux_clk_src.  It is a separate RCG from the
        // DWC3 core/UTMI sources and must be selected before the PHY reset is
        // released on a cold platform start.
        if let Some(aux) = resources
            .qmp_clocks
            .iter()
            .find(|clock| clock.name == "aux")
        {
            if aux.source_offset == 0 || !configure_rcg(aux.source_offset, 0, 0) {
                return false;
            }
        }
        true
    }
}

/// Select only the DWC3 controller clock sources, leaving the already-live
/// QMP AUX source untouched.  This is used by the no-core SuperSpeed handoff
/// differential after QMP init: it tests whether the DWC3 MMIO domain was
/// left behind by Fastboot without rewriting a running PHY clock.
pub unsafe fn configure_usb_controller_clocks(vote: UsbBusVote) -> bool {
    let plan = usb_clock_plan(vote);
    if !plan.branches_enabled {
        return true;
    }
    unsafe {
        let resources = usb_resources();
        let core = resources.controller_clocks[0];
        let utmi = resources.controller_clocks[3];
        // gcc_usb30_prim_master_clk_src.
        if core.source_offset == 0
            || !configure_rcg(core.source_offset, plan.core_parent, plan.core_divider)
        {
            return false;
        }
        // gcc_usb30_prim_mock_utmi_clk_src.
        if utmi.source_offset == 0
            || !configure_rcg(utmi.source_offset, plan.utmi_parent, plan.utmi_divider)
        {
            return false;
        }
        true
    }
}

/// Read the GCC source and branch state without changing ownership or clock
/// programming. This is deliberately separate from `configure_usb_clocks`:
/// a failed enumeration must be diagnosable even when another A/B should not
/// retune a live Fastboot clock domain.
pub unsafe fn read_usb_clock_register_state() -> UsbClockRegisterState {
    unsafe {
        let resources = usb_resources();
        let core = resources.controller_clocks[0];
        let utmi = resources.controller_clocks[3];
        let source_config = |clock: ClockResource| {
            if clock.source_offset == 0 {
                0
            } else {
                core::ptr::read_volatile(
                    (resources.gcc_base + clock.source_offset + 0x4) as *const u32,
                )
            }
        };
        let mut controller_branches = [0; 6];
        for (index, clock) in resources.controller_clocks.iter().enumerate() {
            controller_branches[index] =
                core::ptr::read_volatile((resources.gcc_base + clock.branch_offset) as *const u32);
        }
        let mut qmp_branches = [0; 4];
        for (index, clock) in resources.qmp_clocks.iter().enumerate() {
            if clock.provider == ClockProvider::Gcc && clock.branch_offset != 0 {
                qmp_branches[index] = core::ptr::read_volatile(
                    (resources.gcc_base + clock.branch_offset) as *const u32,
                );
            }
        }
        UsbClockRegisterState {
            core_source_config: source_config(core),
            utmi_source_config: source_config(utmi),
            controller_branches,
            qmp_branches,
        }
    }
}

/// Bring up only the mock UTMI branch feeding the USB2 datapath.
///
/// The Bramble Lito GCC table exposes `gcc_usb30_prim_mock_utmi_clk_src` at
/// 19.2 MHz (`BI_TCXO / 1`), and `dwc3-msm.c` requests that rate explicitly.
/// An SS-only Fastboot session may leave this GCC branch gated, so program the
/// source and raise that one branch only; the core/iface/QMP branches are left
/// as firmware configured them.  The old 60 MHz path is retained only behind
/// an explicit negative-control build flag.
pub unsafe fn enable_usb2_utmi_clock() -> bool {
    unsafe {
        let resources = usb_resources();
        let utmi = resources.controller_clocks[3];
        if utmi.name != "utmi" || utmi.provider != ClockProvider::Gcc {
            return false;
        }
        // 60 MHz is not an official Bramble-Lito rate; keep it only for
        // reproducing the earlier invalid-source experiments.  The legacy
        // 19.2 MHz flag remains accepted for build-script compatibility.
        let (parent, divider) = if option_env!("FULLERENE_USB_UTMI_60MHZ").is_some()
            && option_env!("FULLERENE_USB_UTMI_19_2MHZ").is_none()
        {
            (6u32, 5u32)
        } else {
            (0u32, 0u32)
        };
        if utmi.source_offset != 0 && !configure_rcg(utmi.source_offset, parent, divider) {
            return false;
        }
        let address = (resources.gcc_base + utmi.branch_offset) as *mut u32;
        let value = core::ptr::read_volatile(address) | 1;
        core::ptr::write_volatile(address, value);
        wait_for_branch_state(address, true)
    }
}

/// Re-enable the non-UTMI controller branches in the same order used by the
/// Android msm resume path: iface, core, sleep, then utmi.  The UTMI source
/// and branch are handled by `enable_usb2_utmi_clock()` so its rate A/B stays
/// isolated; this helper tests only whether an SS-only Fastboot session left
/// one of the other controller branches gated across the handoff.
pub unsafe fn rearm_usb2_android_clock_branches() -> bool {
    unsafe {
        let resources = usb_resources();
        let mut ok = true;
        for name in ["iface", "core", "sleep"] {
            let Some(clock) = resources
                .controller_clocks
                .iter()
                .find(|clock| clock.name == name)
            else {
                return false;
            };
            if clock.provider != ClockProvider::Gcc {
                return false;
            }
            let address = (resources.gcc_base + clock.branch_offset) as *mut u32;
            let value = core::ptr::read_volatile(address) | 1;
            core::ptr::write_volatile(address, value);
            ok &= wait_for_branch_state(address, true);
        }
        ok
    }
}

/// Re-enable all DWC3 controller branches without touching the QMP PHY
/// branches.  The order follows the six controller clocks in the Bramble DT,
/// minus the shared XO clock.  This is intentionally separate from the USB2
/// resume helper: the SuperSpeed no-core A/B must include `bus_aggr` and must
/// be able to restore the controller domain while leaving QMP state intact.
pub unsafe fn rearm_usb_controller_clock_branches() -> bool {
    unsafe {
        let resources = usb_resources();
        let mut ok = true;
        for name in ["core", "iface", "bus_aggr", "utmi", "sleep"] {
            let Some(clock) = resources
                .controller_clocks
                .iter()
                .find(|clock| clock.name == name)
            else {
                return false;
            };
            if clock.provider != ClockProvider::Gcc {
                return false;
            }
            let address = (resources.gcc_base + clock.branch_offset) as *mut u32;
            let value = core::ptr::read_volatile(address) | 1;
            core::ptr::write_volatile(address, value);
            ok &= wait_for_branch_state(address, true);
        }
        ok
    }
}

/// Mirror the Android msm controller block-reset boundary for a handoff A/B.
/// The downstream driver disables the four DWC3 link clocks, asserts the GCC
/// core reset, waits for the reset to settle, then deasserts the reset and
/// enables the clocks in iface/core/sleep/utmi order before a 10 ms settle
/// window. This is intentionally separate from the DWC3 device CSFTRST and
/// from the external QUSB2 PHY reset.
pub unsafe fn android_controller_block_reset() -> bool {
    unsafe {
        let resources = usb_resources();
        let mut ok = true;
        for name in ["utmi", "sleep", "core", "iface"] {
            let Some(clock) = resources
                .controller_clocks
                .iter()
                .find(|clock| clock.name == name)
            else {
                return false;
            };
            if clock.provider != ClockProvider::Gcc {
                return false;
            }
            let address = gcc_reg(clock.branch_offset);
            let value = core::ptr::read_volatile(address) & !1;
            core::ptr::write_volatile(address, value);
            ok &= wait_for_branch_state(address, false);
        }

        let reset = resources.resets[0];
        if reset.name != "core_reset" {
            return false;
        }
        let reset_address = gcc_reg(reset.offset);
        let asserted = core::ptr::read_volatile(reset_address) | 1;
        core::ptr::write_volatile(reset_address, asserted);
        ok &= core::ptr::read_volatile(reset_address) & 1 != 0;
        crate::timer::delay_us(1_000);
        core::ptr::write_volatile(reset_address, asserted & !1);
        ok &= core::ptr::read_volatile(reset_address) & 1 == 0;
        crate::timer::delay_us(1);

        for name in ["iface", "core", "sleep", "utmi"] {
            let clock = resources
                .controller_clocks
                .iter()
                .find(|clock| clock.name == name)
                .unwrap();
            let address = gcc_reg(clock.branch_offset);
            let value = core::ptr::read_volatile(address) | 1;
            core::ptr::write_volatile(address, value);
            ok &= wait_for_branch_state(address, true);
        }
        crate::timer::delay_us(10_000);
        ok
    }
}

/// Mirror the `core_reset = false` part of Android msm's
/// `dwc3_msm_block_reset()`: reset the DBM, enable its two FIFO-address/size
/// masks, and set QSCRATCH `DBM_EN`. This is separate from the GCC link-clock
/// reset above; the Bramble DT does not describe a DBM node because Android
/// derives the DBM base from the Qualcomm wrapper/DWC3 resource base
/// (`+ 0xf8000`). QSCRATCH is a different window at wrapper `+ 0xf8800`, so
/// it must not be used as the DBM base.
pub unsafe fn android_dbm_reset_and_enable() -> bool {
    unsafe {
        const DBM_BASE_OFFSET: usize = 0xf8000;
        const DBM_DATA_FIFO_ADDR_EN: usize = 0x0200;
        const DBM_SOFT_RESET: usize = 0x020c;
        const DBM_DATA_FIFO_SIZE_EN: usize = 0x0204;
        const DBM_SFT_RST_MASK: u32 = 1 << 31;
        const DBM_ENABLE: u32 = 1 << 1;

        let resources = usb_resources();
        let qscratch_base = resources.qscratch_base;
        let Some(dbm_base) = resources.dwc3_base.checked_add(DBM_BASE_OFFSET) else {
            return false;
        };
        let reset_address = (dbm_base + DBM_SOFT_RESET) as *mut u32;
        let saved = core::ptr::read_volatile(reset_address);
        if saved == u32::MAX {
            return false;
        }
        core::ptr::write_volatile(reset_address, saved | DBM_SFT_RST_MASK);
        // qpr1's dbm.c reaches this register through iowrite32(), whose
        // AArch64 implementation performs __iowmb() before the store.
        core::arch::asm!("dsb st", options(nostack, preserves_flags));
        // The Android driver does not gate the sequence on DBM reset
        // readback; keep the read as an ordering/readout point only.
        let _ = core::ptr::read_volatile(reset_address);
        crate::timer::delay_us(1_000);
        let released = core::ptr::read_volatile(reset_address) & !DBM_SFT_RST_MASK;
        core::ptr::write_volatile(reset_address, released);
        core::arch::asm!("dsb st", options(nostack, preserves_flags));
        let _ = core::ptr::read_volatile(reset_address);

        let general_address = (qscratch_base + 0x08) as *mut u32;
        let general = core::ptr::read_volatile(general_address) | DBM_ENABLE;
        core::ptr::write_volatile(general_address, general);
        core::arch::asm!("dsb st", options(nostack, preserves_flags));
        // Match dwc3_msm_write_reg_field(): the readback confirms ordering,
        // but a write-only/masked status bit must not abort the source path.
        let _ = core::ptr::read_volatile(general_address);
        core::ptr::write_volatile((dbm_base + DBM_DATA_FIFO_ADDR_EN) as *mut u32, 0xff);
        core::arch::asm!("dsb st", options(nostack, preserves_flags));
        core::ptr::write_volatile((dbm_base + DBM_DATA_FIFO_SIZE_EN) as *mut u32, 0xff);
        core::arch::asm!("dsb st", options(nostack, preserves_flags));
        let _ = core::ptr::read_volatile((dbm_base + DBM_DATA_FIFO_SIZE_EN) as *const u32);
        true
    }
}
