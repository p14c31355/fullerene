use super::{ClockProvider, UsbBusVote, set_usb_resource_state, usb_clock_plan, usb_resources};

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

#[inline]
unsafe fn gcc_reg(offset: usize) -> *mut u32 {
    (usb_resources().gcc_base + offset) as *mut u32
}

/// Program one Qualcomm RCG2 clock source and commit the change.
///
/// The Lito GCC driver describes the USB master clock as parent 1 divided by
/// 8 and the mock UTMI clock as parent 6 divided by 5. Keeping this in the
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

/// Bring up only the mock UTMI branch feeding the USB2 datapath.
///
/// The 4.19 msm driver pins `utmi_clk` to 19.2 MHz (BI_TCXO) at probe time
/// and its resume path enables it after `core_clk`.  An SS-only fastboot
/// session never raises `gcc_usb30_prim_mock_utmi_clk`, the `utmi_clk` spec
/// of the dwc3 node, which leaves the core's USB2 link domain unable to
/// reach U0.  This programs the RCG source and raises that one branch only;
/// the core/iface/QMP branches are left exactly as firmware left them.
pub unsafe fn enable_usb2_utmi_clock() -> bool {
    unsafe {
        let resources = usb_resources();
        let utmi = resources.controller_clocks[3];
        if utmi.name != "utmi" || utmi.provider != ClockProvider::Gcc {
            return false;
        }
        // The 4.19 driver pins UTMI to 19.2 MHz at probe, but the platform
        // clock plan and Linux parent table define the hardware's 60 MHz
        // mode as GPLL0_OUT_EVEN divided by 5. The experiment flag selects
        // that mode to test whether an SS-only Fastboot session left the
        // USB2 link domain expecting it.
        let (parent, divider) = if option_env!("FULLERENE_USB_UTMI_60MHZ").is_some() {
            (6u32, 5u32)
        } else {
            (0u32, 1u32)
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
