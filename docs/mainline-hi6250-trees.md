# Public kernel trees for Kirin 659 (hi6250) — survey findings (2026-10-20)

Surveyed by subagent against GitHub + pmOS wiki. Conclusion first: **no
public tree contains an `anne` (ANE-LX2J) board DTS** — the board file
ships only in the phone's DT partition (we already have it:
`p20-root/firmware/ane_dtb/fdt.dtb`). Everything else must be assembled
from these sources.

## Mainline effort (the tree to contribute to / steal from)

- `hi6250-mainline/linux` (branch 7.0-develop, tracks torvalds 7.0):
  the most complete public mainline effort. `arch/arm64/boot/dts/hisilicon/hi6250.dtsi`
  already has PSCI/GIC/timer/PMU, the hi6250-{ao,pmu,sys,crg}ctrl
  blocks, clocks, mailbox, reset, gpio25, pinctrl-single, PL011
  uart0-6, dwmmc1 (SD) + dwmmc2 (SDIO), and **full USB**: usb_ahbif
  syscon, the custom PHY (`drivers/phy/hisilicon/phy-hi6250-usb.c`,
  compatible `hisilicon,hi6250-usb-phy`), and
  `usb@ff100000` compatible `"hisilicon,hi6220-usb","snps,dwc2"`
  `dr_mode=peripheral` with explicit g-fifo sizes. The dwc2 node is
  disabled in the dtsi and enabled (`&usb status=ok`) in
  `hi6250-huawei-agassi2.dts` (MediaPad T5, same SoC). **No anne dts;
  ours would be `hi6250-huawei-anne.dts`.**
- `tildeknown/linux-hi6250` (branches 7.0-usb / 7.0-dwmmc /
  7.0-reboot): second mainline tree, same dtsi + PHY driver, authored
  by Tildeguy 2026. The 7.0-usb branch is where USB/PHY work lands
  before the merge. pmOS wiki mainlining table: USB phy = Y, USB
  gadget = written-untested ("no regulators"), I2C / reset / PMIC /
  GPU / UART / DMA / eMMC / SD / SDIO still N/P.
- `torvalds/linux`: no hi6250 at all (only hikey960/970, poplar,
  hikey6220, hip0x). Upstream `drivers/usb/dwc2/params.c` does carry
  `dwc2_set_his_params` for `"hisilicon,hi6220-usb"` — reusable
  directly.
- `hi6250_defconfig` (in hi6250-mainline) is the DWC2-gadget recipe:
  `CONFIG_USB_DWC2=y`, `CONFIG_USB_DWC2_PERIPHERAL=y`,
  `CONFIG_USB_CONFIGFS=m` with RNDIS/NCM, `CONFIG_PHY_HI6250_USB=y`,
  `CONFIG_NOP_USB_XCEIV=y`, `CONFIG_EXTCON_USB_GPIO=y`.

## Downstream (anne-specific, for hardware truth)

- `hisi-oss/android_kernel_huawei_hi6250-9` (4.9 EMUI9) — the tree
  LineageOS 17-20 for anne builds from (device repos:
  `android_device_huawei_anne` + `...hi6250-9-common`). Our local
  `p20-root/hi6250-kernel` is the -8 variant, same layout.
- `Abdelhay-Ali/android_kernel_huawei_hi6250_KernelSU` (4.9.148,
  anne-targeted, "Fix Wifi issues" changelog) — diff target when the
  hi1102a driver misbehaves on anne.
- `utkustnr/android_kernel_huawei_hi6250` ("Phenix", weekly CI, tested
  on ANE-LX1 with Lineage 20) — most actively maintained; cherry-pick
  source for EMUI9-era anne fixes.
- `LineageOS/android_kernel_huawei_hi6250` (lineage-17.1, EMUI8 4.4
  base) — history only.

## What this means for FullereneOS

1. The SoC-level bring-up order is already proven by hi6250-mainline:
   clocks → pinctrl → uart → dwmmc → USB PHY → dwc2 gadget. Our ANE
   driver work matches it register-for-register so far (CRG bits,
   DesignWare I2C, dwmmc).
2. USB: when we need host-role or a standard gadget stack, the
   `"hisilicon,hi6220-usb"` dwc2 param table plus `phy-hi6250-usb` are
   the reference; our fastboot responder currently hand-rolls DWC2,
   which is fine for bring-up but should eventually share the PHY
   power sequence with that driver.
3. No anne DTS exists publicly, so our `fdt.dtb` extraction (already
   done) is the only board-level source in existence outside Huawei.
   Keeping `docs/` notes tied to blob 277 (ANE_LX2J_VH) is the right
   anchor.

See `docs/hi1102-bfgx-bringup-notes.md` for the connectivity side
(hi1102 power/SDIO/UART sequences, also from this survey batch).
