# Hi1102 BFGX power-on & firmware download — primary-source findings (2026-10-20)

Primary sources:
- Kernel: `yuioto/android_kernel_huawei_art-al00x` (Kirin710, 4.14, `drivers/connectivity/hi11xx/hi1102a/`)
  and `rcstar6696/android_kernel_huawei_hi3660` (4.9, `drivers/connectivity/hisi/`) — same driver family as the
  built-in `[wifi]`/`[plat]` modules in ANE-LX2J's stock 4.4 kernel.
- Device tree: the stock `dts_stock.bin` partition image pulled from the device (`p20-root/firmware/dts_stock.bin`,
  HSDT container of 278 gzipped DTBs; ANE_LX2J_VH = blob 277 parsed below).
- Local blobs: `p20-root/firmware_blobs/{bfgx_cfg,bfgx_and_wifi_cfg,wifi_cfg,init.connectivity.hi1102.rc}`.

## Which firmware is which
- `cr4_asic.bin` = WCPU bootloader, pushed by host over SDIO to device address **0x70000** (wifi_cfg / bfgx_and_wifi_cfg).
- `CPU_RAM_SCHED.bin` → **0x300E4000**, `CPU_RAM_WBS.bin` → **0x30168000** = BCPU (BT/FM/GNSS) patch images,
  pushed over SDIO per `bfgx_cfg` when BT/GNSS is opened; cal data at 0x30170000.
- The **BCM43455** files (`fw_bcm43455_hw.bin`, `nvram43455_hw.txt`, `BCM4345C0…hcd`) belong to the *other*
  chiptype branch (`ro.connectivity.chiptype=bcm`, `init.connectivity.bcm43xx.rc` path). On a hi1102
  (`ro.connectivity.sub_chiptype=hi1102`) the WLAN CPU runs HiSi firmware (cr4_asic → Total.wdk-class image);
  host-side driver code for WiFi is entirely HiSi (`oal_*/hmac/wal`). BCM blobs are on the vendor partition for
  union-image support but should be **unused** on this unit — host only pushes cfg scripts; nothing loads
  BCM43455 on the hi1102 path. Confidence: high for driver code path, medium for "blob unused".

## Transport
All patch download is over **SDIO func1** (IDs: VEND 0x22d1/0x12d1, PROD 0x5347/0x5348), FIFO reg 0x00,
xfer-count 0x0c, device→host msg regs 0x28/0x2b, host→device msg reg 0x24. The cfg file itself is a script of
text commands; each command/ack travels through the SDIO patch path (`hcc_bus_patch_write`/`patch_read`):
commands: `VERSION`, `PARA_WRITEM=<type>,<addr>,<val>`, `ADDR_FILE_FILES=<n>,<addr>,<path>`,
`PARA_JUMP`, `PARA_CALI_BFGX_DATA`, `SHUTDOWN_WIFI`, `SHUTDOWN_BFGX`, `QUIT`;
acks: `WRITEM OK`, `FILES OK`, `JUMP OK`, `QUIT OK`, `READY`.

## Power GPIOs (ANE_LX2J_VH DTB, blob 277)
- `/hi1102` compat `hisilicon,hi1102`:
  - `hi1102,gpio_power_on` = **<&gpio 3 0> on pl061 @0xe8a0f000** (8-gpio block, pinctrl pins `gpio090/091/092`,
    offsets 0x15c/0x160/0x164 func, 0x168/0x16c/0x170 cfg in pinmux@e896c000/e896c800).
  - `hi1102,gpio_xldo_pinmux` = `<&gpio 3 0> @0xe8a16000`, `xldo_gpio_level=1`.
  - `huawei,pmu_clk32b` = `pmu_clk32kb` (32 kHz sleep clock).
- `/hisi_wifi`: `hi1102,gpio_wlan_wakeup_host` = **<&gpio 5 0> @0xfff0f000** (AO gpio4, input, irq).
- `/hisi_bfgx`: `hi1102,gpio_bfgn_wakeup_host` = **<&gpio 2 0> @0xfff0f000**; `hi1102,uart_port` = **/dev/ttyAMA4**
  (note: ueventd on the running unit chmods /dev/ttyAMA3 — several other blob variants use ttyAMA3; verify on HW).
- Power-on pulse (`power_state_change`): **low 10 ms → high 30 ms** (hisi tree; hi1102A tree uses 10/20 ms).
  hi1102 has ONE power gpio (one rail for both WLAN and BFGX cores); hi1102A splits
  `power_on`/`wlan_power_on`/`bfgx_power_on` — all three names exist in the new tree; this board uses the
  single-gpio `hisilicon,hi1102` node.

## Boot sequence (from driver source)
1. `plat_init` via `echo init > /sys/hisys/boot/plat`; wifi via `/sys/hisys/boot/wifi` (postmarketOS-compatible).
2. WLAN: `wlan_power_on` → board gpio power-on → SDIO (re)enum (`HCC_BUS_POWER_UP`, `oal_sdio_probe` with the
   IDs above) → `HCC_BUS_POWER_PATCH_LOAD_PREPARE` → download `bfgx_and_wifi_cfg` (first boot:
   cr4_asic + both CPU RAMs + reg pokes + `SHUTDOWN_BFGX`) → `HCC_BUS_POWER_PATCH_LAUCH` (wait device-ready,
   up to 15 s).
   - If only WiFi: `wifi_cfg` (cr4_asic only).
3. BT/GNSS open (`/dev/hwbt`, `hwgnss`): `bfg_power_set` → power gpio (if whole chip was down: re-power +
   re-download WLAN) → `open_tty_drv` (open uart, install ldisc) → `firmware_download_function(BFGX_CFG)`
   (CPU_RAM images over SDIO) → wait BCPU boot-ok (1 s) → `SYS_CFG_NOTIFY_WIFI_OPEN` over UART.
4. UART data path: custom line discipline **N_HW_BFG** (35; or `NR_LDISCS-1` when `_PRE_NON_OCTTY`, which is set
   in `hi1102a_comm_defconfig` — octty is unnecessary on 4.14 builds; on the 4.4 stock kernel octty opens
   /dev/ttyAMA3 and sets ldisc/baud from sysfs `install`, `dev_name`, `baud_rate`, `flow_cntrl`).
   - Frame: `0x7E | type | len_lo | len_hi | payload | 0x7E` (sys packets len=6; type 0 = SYS).
   - Sys commands: OPEN_BT=0, CLOSE_BT=1, OPEN_GNSS=2, OPEN_FM=4, OPEN_NFC=6, OPEN_IR=8, OPEN_WIFI=0xA…
   - Baud: wake 115200 no-flowctl (host writes 0x00 bytes to wake sleeping device), working **4 000 000** 8N1
     with CRTSCTS for hi1102A (5 000 000 on hi1102); device may request 2↔6 M switch via SYS_INF msgs.
5. WiFi data path is SDIO HCC (not UART).

## Files saved
- `tmp/hi1102_research/` — extracted driver sources (board*, plat_pm*, plat_firmware*, oal_sdio_host*, …),
  `merge_kirin710_defconfig`, `hi1102a_comm_defconfig`, `FINDINGS.json`, and a parser for the stock dts partition.
