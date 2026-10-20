
# Public kernel trees for kirin659 'anne' (P20 Lite) — survey (2026-10-20)

## Downstream (Android) trees
- hisi-oss/android_kernel_huawei_hi6250-8 (kernel 4.4, EMUI 8; local at /home/placeless/dev/p20-root/hi6250-kernel, lineage-20 branch checked out)
  * No board-level 'anne' dts anywhere. Board dts ship in the phone's DT partition; tree only has SoC-level auto-generate/config/hi6250 (udp_*) dts + lcdkit ane/ane_lx3 panel dtsis.
  * SoC connectivity dtsi (hisi_connectivity.dtsi): hi1102 node, power gpio4.3, wlan wake gpio26.5, bfgx wake gpio26.2, bfgx UART /dev/ttyAMA3, firmware_type_num=1.
  * WiFi/BT driver: drivers/connectivity/hi11xx/hi1102a (CONFIG_HI1102_PLATFORM=y, CONFIG_HI1102_WIFI=y -> built-in obj-y plat/wifi for product 1102a; module option obj-m exists but off).
  * Firmware load path: drivers/connectivity/hi11xx/hi1102a/platform/pm/plat_firmware.c reads cfg files from /vendor/firmware/{bfgx_and_wifi_cfg,wifi_cfg,bfgx_cfg,ram_reg_test_cfg} (kernel_read, no request_firmware); patch .bin files under /system/vendor/firmware (BCPU_ROM.bin etc in board.c debug paths); firmware pushed over SDIO (wifi) and UART ttyAMA3 (bfgx/bt/gps/nfc).
  * Userspace fw blobs: hisi-oss/android_device_huawei_hi6250-8-common proprietary-files.txt: vendor/firmware/bfgx_and_wifi_cfg, bfgx_cfg, wifi_cfg + gps.hi1102 libs.
  * USB: drivers/usb/susb (Huawei dwc_otg fork, CONFIG_HI6250_USB=y, HISI_USB_CONFIGFS) NOT mainline dwc2.
- hisi-oss/android_kernel_huawei_hi6250-9 (kernel 4.9, EMUI 9; lineage-20 branch) — same structure; hi1102a driver at drivers/connectivity/hi11xx/hi1102a with same /vendor/firmware cfg paths; this is the tree behind LineageOS 17-20 for anne (device: hisi-oss/android_device_huawei_anne + hi6250-9-common).
- Abdelhay-Ali/android_kernel_huawei_hi6250_KernelSU (4.9.148, anne-specific KernelSU builds; wifi fix commits Apr 2023)
- utkustnr/android_kernel_huawei_hi6250 (phenix kernel, KSU, EMUI9 base, active into 2025)
- LineageOS/android_kernel_huawei_hi6250 (lineage-17.1, stale 2020)
- No public tree contains the anne board dts (anne/ANE-L*).

## Mainline trees (the interesting ones)
- hi6250-mainline/linux (branch 7.0-develop, Linux 7.0-based, active Sep 2026): SoC dtsi hi6250.dtsi with PSCI, GIC, ARM timer, PMU, clocks (hi6250-{ao,pmu,sys,crg}ctrl), stub clock, mailbox, hisi3660-reset, gpio25 pl061, pinctrl-single pmx0-3, PL011 uarts 0-6, dwmmc1 SD + dwmmc2 SDIO, hi6250 USB: usb_ahbif syscon + phy-hi6250-usb (custom driver drivers/phy/hisilicon/phy-hi6250-usb.c) + usb@ff100000 compatible "hisilicon,hi6220-usb","snps,dwc2" dr_mode=peripheral with fifo sizes; agassi2 dts enables &usb ok.
  defconfig hi6250_defconfig: CONFIG_USB_DWC2=y, CONFIG_USB_DWC2_PERIPHERAL=y, USB_CONFIGFS=m (+NCM/RNDIS/F_FS), PHY_HI6250_USB=y, PHY_HI6220_USB=y, NOP_USB_XCEIV=y, EXTCON_USB_GPIO=y.
  Mainline dwc2 platform driver matches "hisilicon,hi6220-usb" (dwc2_set_his_params in drivers/usb/dwc2/params.c upstream).
  Board dts: hi6250-huawei-figo.dts (P Smart), hi6250-huawei-agassi2.dts (MediaPad T5) — boots mainline w/ simplefb; NO anne dts yet.
- tildeknown/linux-hi6250 (experimental forks of same; branches 7.0-usb/7.0-dwmmc/7.0-reboot, hi6250-next): same dtsi + custom phy-hi6250-usb.c (Tildeguy 2026), dwc2 node present; pmos wiki: boots, no storage drivers yet.
- torvalds/linux: zero hi6250/kirin659 support (hisilicon Makefile has only hikey/hip0x).

## hi1102 (Hi1102A) mainline status
- No mainline or out-of-tree mainline-targeted hi110x/hsak wifi driver found anywhere (no GitHub repo; not in torvalds). The vendor driver lives only in downstream trees as built-in (CONFIG_HI1102_PLATFORM/HI1102_WIFI=y) with Huawei-HCE userspace (start_connectivity_hisi, wpa_supplicant_hisi.conf).
- FW loading: driver reads /vendor/firmware/*cfg text command files via kernel_read and pushes patch over SDIO/UART; needs vendor blobs from device (proprietary_vendor_huawei_anne / hi6250-9-common).
- pmos/Nura: anne boots downstream 4.9.148 pmOS port (USB networking works); mainline anne = none. Closest kirin659 mainline refs: figo/agassi2 (same SoC, booted mainline 7.0).
