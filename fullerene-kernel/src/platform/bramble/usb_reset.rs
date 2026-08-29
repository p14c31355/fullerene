use super::{set_usb_resource_state, usb_resources};

/// Assert and release every reset exposed by the Lito USB node. The caller
/// controls the surrounding power-domain/clock ordering; this function only
/// performs the DT-described reset resources and reports readback failures.
pub unsafe fn reset_usb_blocks(super_speed: bool) -> bool {
    let resources = usb_resources();
    let mut ok = true;
    for (index, reset) in resources.resets.iter().enumerate() {
        if !super_speed && index >= 2 {
            continue;
        }
        let address = (resources.gcc_base + reset.offset) as *mut u32;
        let asserted = unsafe { core::ptr::read_volatile(address) } | 1;
        unsafe { core::ptr::write_volatile(address, asserted) };
        ok &= unsafe { core::ptr::read_volatile(address) & 1 != 0 };
        for _ in 0..250_000 {
            unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
        }
        unsafe { core::ptr::write_volatile(address, asserted & !1) };
        ok &= unsafe { core::ptr::read_volatile(address) & 1 == 0 };
    }
    if ok {
        let mask = if super_speed { 0x0f } else { 0x03 };
        set_usb_resource_state(|state| state.reset_released_mask |= mask);
    }
    ok
}

/// Pulse the USB2 (femto) PHY block reset and return it to its running state.
///
/// The lito femto PHY's `phy_reset` is the `GCC_QUSB2PHY_PRIM_BCR` line.  An
/// SS-only fastboot session never deasserts it, so the PHY core logic (PLL,
/// UTMI interface, register file) can stay held in reset while the D+/D- IO
/// state machine still answers the host reset autonomously.  The 4.19
/// phy-core deasserts the reset before `snps_hsphy_init`; the handoff
/// reproduces that boundary here.  Only the USB2 PHY line is pulsed; the
/// shared SSUSB core and QMP reset lines are left as firmware left them.
pub unsafe fn pulse_usb2_phy_reset() -> bool {
    unsafe {
        let resources = usb_resources();
        let reset = resources.resets[1];
        if reset.name != "qusb2phy_reset" {
            return false;
        }
        let address = (resources.gcc_base + reset.offset) as *mut u32;
        let asserted = core::ptr::read_volatile(address) | 1;
        core::ptr::write_volatile(address, asserted);
        for _ in 0..250_000u32 {
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        core::ptr::write_volatile(address, asserted & !1);
        for _ in 0..250_000u32 {
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
        core::ptr::read_volatile(address) & 1 == 0
    }
}
