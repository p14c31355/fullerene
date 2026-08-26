/// Qualcomm SM7250 / Pixel 4a 5G (bramble) early-boot addresses.
///
/// The DTB remains authoritative at boot. These constants document the
/// addresses used by the SM7250 device tree for the first bring-up.
pub const UART_BASE: usize = 0x0098_8000;
pub const GICD_BASE: usize = 0x17a0_0000;
pub const GICR_BASE: usize = 0x17a6_0000;
/// Android's Lito DT routes the primary DWC3 device event interrupt here.
pub const USB_DWC3_IRQ: u32 = 240;

/// GCC register block and USB3 power-domain registers from the Lito DT.
pub const GCC_BASE: usize = 0x0010_0000;
/// Unlike GCC branch registers, the GDSC node is exposed at an absolute SoC
/// address in the DT (`reg = <0x10f004 0x4>`), not as an offset from GCC_BASE.
pub const USB30_PRIM_GDSC: usize = 0x10f004;
const GDSC_PWR_ON: u32 = 1 << 31;
const GDSC_HW_CONTROL: u32 = 1 << 1;
const GDSC_SW_OVERRIDE: u32 = 1 << 2;
const GDSC_SW_COLLAPSE: u32 = 1 << 0;
const GDSC_WAIT_MASK: u32 = (0xf << 20) | (0xf << 16) | (0xf << 12);
const GDSC_WAIT_VALUE: u32 = (0x2 << 20) | (0x8 << 16) | (0x2 << 12);

// Lito's SPMI arbiter.  The Pixel DT exposes the five resources below under
// qcom,spmi@c440000; the arbiter is the only path from the Apps CPU to the
// PM8150B Type-C block.
const SPMI_CORE: usize = 0x0c44_0000;
const SPMI_CHANNELS: usize = 0x0c60_0000;
const SPMI_OBSERVER: usize = 0x0e60_0000;
const SPMI_CONFIG: usize = 0x0c40_a000;
const SPMI_VERSION: usize = 0x0000;
const SPMI_APID_MAP_V5: usize = 0x0900;
const SPMI_MAPPING_TABLE: usize = 0x0b00;
const SPMI_OWNERSHIP_TABLE: usize = 0x0700;
const SPMI_STATUS: usize = 0x08;
const SPMI_WDATA0: usize = 0x10;
const SPMI_RDATA0: usize = 0x18;
const SPMI_STATUS_DONE: u32 = 1 << 0;
const SPMI_STATUS_FAILURE: u32 = 1 << 1;
const SPMI_STATUS_DENIED: u32 = 1 << 2;
const SPMI_STATUS_DROPPED: u32 = 1 << 3;
const SPMI_OP_EXT_WRITEL: u32 = 0;
const SPMI_OP_EXT_READL: u32 = 1;
const SPMI_EE: usize = 0;
const PM8150B_SID: u8 = 2;
const PM8150B_TYPEC_PPID: u16 = ((PM8150B_SID as u16) << 8) | 0x15;
const PM8150B_TYPEC_BASE: u16 = 0x1500;
const TYPEC_MISC_STATUS: u16 = PM8150B_TYPEC_BASE + 0x0b;
const TYPEC_MODE_CFG: u16 = PM8150B_TYPEC_BASE + 0x44;
const TYPEC_CC_ATTACHED: u8 = 1 << 0;
const TYPEC_CC_ORIENTATION: u8 = 1 << 1;
const TYPEC_DISABLE_CMD: u8 = 1 << 0;
const TYPEC_EN_SNK_ONLY: u8 = 1 << 1;
const TYPEC_EN_SRC_ONLY: u8 = 1 << 2;

#[derive(Clone, Copy)]
pub struct TypecState {
    pub arbiter_version: u32,
    pub misc_status: u8,
    pub mode: u8,
    pub orientation_reverse: bool,
    pub sink_mode_written: bool,
}

#[inline]
unsafe fn spmi_reg(base: usize, offset: usize) -> *mut u32 {
    (base + offset) as *mut u32
}

#[inline]
unsafe fn spmi_read(base: usize, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile(spmi_reg(base, offset)) }
}

#[inline]
unsafe fn spmi_write(base: usize, offset: usize, value: u32) {
    unsafe { core::ptr::write_volatile(spmi_reg(base, offset), value) };
    let _ = unsafe { spmi_read(base, offset) };
}

fn find_typec_apid(version: u32) -> Option<(usize, bool)> {
    unsafe {
        if version >= 0x5000_0000 {
            // v5 has a flat APID -> PPID table.  Multiple APIDs can refer to
            // one peripheral; prefer one owned by execution environment 0 so
            // that a write cannot be silently rejected by the arbiter.
            let mut fallback = None;
            for apid in 0..512usize {
                let entry = spmi_read(SPMI_CORE, SPMI_APID_MAP_V5 + apid * 4);
                if ((entry >> 8) & 0x0fff) as u16 != PM8150B_TYPEC_PPID {
                    continue;
                }
                let owner = spmi_read(SPMI_CONFIG, SPMI_OWNERSHIP_TABLE + apid * 4) & 0x7;
                if owner == SPMI_EE as u32 {
                    return Some((apid, true));
                }
                fallback = Some((apid, false));
            }
            return fallback;
        }

        // v2/v3 use the binary mapping tree in the configuration block and
        // the APID table at core+0x800.  This is the same lookup used by the
        // upstream SPMI driver, bounded to the arbiter's 16-bit tree depth.
        let mut index = 0usize;
        for _ in 0..16 {
            let entry = spmi_read(SPMI_CONFIG, SPMI_MAPPING_TABLE + index * 4);
            let bit = ((entry >> 18) & 0xf) as u16;
            let one = (PM8150B_TYPEC_PPID & (1 << bit)) != 0;
            let flag = if one {
                (entry >> 8) & 1
            } else {
                (entry >> 17) & 1
            };
            let result = if one {
                entry & 0xff
            } else {
                (entry >> 9) & 0xff
            };
            if flag != 0 {
                index = result as usize;
                continue;
            }
            return Some((result as usize, true));
        }
    }
    None
}

fn spmi_channel_offset(version: u32, apid: usize, observer: bool) -> usize {
    if version >= 0x5000_0000 {
        if observer {
            0x10000 * SPMI_EE + 0x80 * apid
        } else {
            0x10000 * apid
        }
    } else {
        0x1000 * SPMI_EE + 0x8000 * apid
    }
}

unsafe fn spmi_transfer(
    version: u32,
    apid: usize,
    address: u16,
    value: &mut u8,
    write: bool,
) -> bool {
    let observer = !write;
    let offset = spmi_channel_offset(version, apid, observer);
    let base = if observer {
        SPMI_OBSERVER
    } else {
        SPMI_CHANNELS
    };
    let command = ((if write {
        SPMI_OP_EXT_WRITEL
    } else {
        SPMI_OP_EXT_READL
    }) << 27)
        | (((address & 0xff) as u32) << 4);

    unsafe {
        if write {
            spmi_write(SPMI_CHANNELS, offset + SPMI_WDATA0, *value as u32);
        }
        spmi_write(base, offset, command);
        for _ in 0..1_000_000u32 {
            let status = spmi_read(base, offset + SPMI_STATUS);
            if status & SPMI_STATUS_DONE != 0 {
                if status & (SPMI_STATUS_FAILURE | SPMI_STATUS_DENIED | SPMI_STATUS_DROPPED) != 0 {
                    return false;
                }
                if !write {
                    *value = spmi_read(SPMI_OBSERVER, offset + SPMI_RDATA0) as u8;
                }
                return true;
            }
            core::arch::asm!("nop", options(nomem, nostack, preserves_flags));
        }
    }
    false
}

/// Read PM8150B Type-C state and select sink-only mode for a host-connected
/// phone.  This is intentionally a small, synchronous handoff operation: it
/// does not install the PMIC interrupt controller or pretend to replace the
/// full Linux Type-C state machine.
pub unsafe fn prepare_usb_device_role() -> Option<TypecState> {
    let version = unsafe { spmi_read(SPMI_CORE, SPMI_VERSION) };
    if version == 0 || version == u32::MAX {
        return None;
    }
    let (apid, writable) = find_typec_apid(version)?;
    let mut misc_status = 0u8;
    if !unsafe { spmi_transfer(version, apid, TYPEC_MISC_STATUS, &mut misc_status, false) } {
        return None;
    }
    let mut mode = 0u8;
    if !unsafe { spmi_transfer(version, apid, TYPEC_MODE_CFG, &mut mode, false) } {
        return None;
    }

    let mut sink_mode_written = false;
    // The USB cable is attached to a host during `fastboot boot`; the phone
    // must therefore remain a sink and expose a USB device, not source VBUS.
    // Preserve unrelated PMIC bits and only replace the source/sink selection.
    // During a `fastboot boot` handoff the PMIC can report CC detached for a
    // short interval while the bootloader is tearing down its gadget.  The
    // cable is nevertheless the boot transport that brought us here, so do
    // not discard the device-role request solely because that transient bit
    // is clear.  Reassert sink-only whenever the current mode is not already
    // an unambiguous sink configuration.
    if writable {
        let requested = (mode & !(TYPEC_EN_SNK_ONLY | TYPEC_EN_SRC_ONLY)) | TYPEC_EN_SNK_ONLY;
        // The upstream Qualcomm PMIC Type-C driver forces the state machine
        // through DISABLE before selecting a new power role. A same-value
        // write is not sufficient after Fastboot has torn down its gadget:
        // the PMIC can retain sink-only in the register while its attach
        // evaluation remains stopped.
        let mut disable = TYPEC_DISABLE_CMD;
        let disabled = unsafe { spmi_transfer(version, apid, TYPEC_MODE_CFG, &mut disable, true) };
        let mut new_mode = requested;
        sink_mode_written = disabled
            && unsafe { spmi_transfer(version, apid, TYPEC_MODE_CFG, &mut new_mode, true) };
        if sink_mode_written {
            mode = requested;
        }
    }
    Some(TypecState {
        arbiter_version: version,
        misc_status,
        mode,
        orientation_reverse: misc_status & TYPEC_CC_ORIENTATION != 0,
        sink_mode_written,
    })
}

/// Enable the USB3 GDSC using the same software-controlled sequence as the
/// Qualcomm GDSC regulator driver. The parent RPMh supplies are intentionally
/// not touched here: those rails are controlled by secure firmware and are
/// already enabled by the Pixel boot chain for a temporary boot image.
pub unsafe fn enable_usb30_gdsc() -> bool {
    let address = USB30_PRIM_GDSC as *mut u32;
    let mut value = unsafe { core::ptr::read_volatile(address) };
    value &= !(GDSC_HW_CONTROL | GDSC_SW_OVERRIDE | GDSC_WAIT_MASK);
    value |= GDSC_WAIT_VALUE;
    unsafe { core::ptr::write_volatile(address, value) };
    let _ = unsafe { core::ptr::read_volatile(address) };

    value &= !GDSC_SW_COLLAPSE;
    unsafe { core::ptr::write_volatile(address, value) };
    let _ = unsafe { core::ptr::read_volatile(address) };

    for _ in 0..1_000_000u32 {
        if unsafe { core::ptr::read_volatile(address) } & GDSC_PWR_ON != 0 {
            return true;
        }
        unsafe { core::arch::asm!("nop", options(nomem, nostack, preserves_flags)) };
    }
    false
}

const GCC_CMD_UPDATE: u32 = 1 << 0;
const GCC_CFG_SRC_DIV_MASK: u32 = 0xff;
const GCC_CFG_SRC_SEL_MASK: u32 = 0x7 << 8;

#[inline]
unsafe fn gcc_reg(offset: usize) -> *mut u32 {
    (GCC_BASE + offset) as *mut u32
}

/// Program one Qualcomm RCG2 clock source and commit the change.
///
/// The Lito GCC driver describes the USB master clock as parent 1 divided by
/// 8 and the mock UTMI clock as parent 0 with no divider. Keeping this in the platform
/// layer prevents the DWC3 driver from depending on GCC register layout.
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
pub unsafe fn configure_usb_clocks() -> bool {
    unsafe {
        // gcc_usb30_prim_master_clk_src.
        if !configure_rcg(0xf020, 1, 8) {
            return false;
        }
        // gcc_usb30_prim_mock_utmi_clk_src.
        configure_rcg(0xf038, 0, 0)
    }
}

pub fn init_interrupt_controller(gicd_base: Option<usize>, gicr_base: Option<usize>) {
    super::gicv3::init(
        gicd_base.unwrap_or(GICD_BASE),
        gicr_base.unwrap_or(GICR_BASE),
        Some(USB_DWC3_IRQ),
    );
}
