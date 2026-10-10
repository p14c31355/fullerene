# The USB re-enumeration harness — ANE-LX2J status and plan (2026-10-02, midday)

## Where things stand

- The handset sits in its recovery screen, the loader's own fallback after the
  last USB probe run.
- One press (Volume Down + Power) lets the waiting script do everything else:
  restore the stock kernel, boot Android, keep the screen awake, and flash the
  **L1 USB probe**. From then on a run costs a single press, because the
  recovery path still needs physical buttons (see below).

## What today established

1. **Why the first USB probes were silent.** Writing only DCTL.SftDiscon
   (the attach bit) leaves the host blind - twice measured, zero kern.log
   events. The loader folds the USB clocks and resets away before jumping to
   the kernel.
2. **The full bring-up sequence, from vendor source.** `init_usb_otg_phy_hi6250()`
   in `drivers/usb/susb/dwc_otg_hi6250.c`:
   the three CRG+0x40 clock gates -> PCTRL+0x64 (abb) -> four reset releases
   in CRG+0x94 -> AHBIF ctrl0/ctrl3 (eye pattern) -> PHY POR -> PHY -> OTG
   releases -> AHBIF ctrl2 -> the DCTL attach.
   Implemented as `ane_usb_l1_probe.S`, **verified in QEMU** (the check caught
   one `movk` bit-assembly mistake before any device cycle).
3. **The L1 probe halts instead of looping**, so a successful attach stays
   visible on the host for inspection. A hung boot falls back into the
   loader's recovery after ~6-7 minutes - a known, recorded behaviour.

## A hard constraint worth repeating

- **Android's userspace consumes pstore at boot.** "Restore stock, boot
  Android, read" always returns empty. Readouts must come from (a) a probe
  reading the zone while it runs, or (b) a channel that never involves Android
  - the USB re-enumeration work is exactly (b).

## Button-free cycles (open, stated honestly)

Neither route is finished:

1. **Calling the loader's own function from the payload** - the method (derive
   the runtime address from VBAR, call the loader's reboot-to-bootloader path)
   is sound, but every string-xref search (adrp+add, 64-bit pointers, 32-bit
   offsets) came back empty: the flat-disassembly addressing model does not
   describe this image. Needs a different lead.
2. **Writing the PMIC reason register directly** - register and value are known
   (`HRST_REG0 = base+0x18B`, `BOOTLOADER = 0x01`), but the transport is SPMI
   and `hisi_pmic_reg_read/write` has no implementation in the source tree.

Until one of these lands, each run costs one button press. Once L1 passes,
**observation** becomes fully automatic even so.

## Next (automatic once the device is reachable)

1. Flash the L1 probe (`ane_usb_l1_run.sh`).
2. Attach observed -> enumerate on the host -> L2 (descriptor responses).
3. No attach -> investigate the PHY registers (0xfe000000) and the abb PLL.
4. Port the same sequence to Fullerene in Rust (`platform/ane.rs` + a usb
   module; note the multi-root rule: new top-level modules need `#[path] mod`
   declarations in every crate root).
