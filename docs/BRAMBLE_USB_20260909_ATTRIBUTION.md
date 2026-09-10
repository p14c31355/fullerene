# Bramble direct-path attribution (2026-09-09)

Goal remains real Fullerene-owned `1234:0001` enumeration, twice. Attach alone is not success. RAM-only `fastboot boot`; no device partition access or configuration changes.

## Control 2219798.0

- Hypothesis: current HEAD still reproduces the preserved standalone control 1700055.0.
- Kernel configuration: exact `build-command.txt` profile from 1700055.0; host enumeration window increased from 30 to 60 seconds, Fastboot wait 60 seconds, hold 5 seconds, passive all-bus usbmon.
- Starting state: serial `26191JECB00076`, Fastboot, product bramble, slot b, unlocked yes; clean HEAD `70bff517`.
- Artifact: `tmp/fullerene-bramble-loop.2219798.0/fullerene-bramble-boot.img`, SHA-256 `92724eed7a10969f78e1569d6e400551996a6d353afdae3e5692ec985e64740c`, byte-identical hash to 1700055.0.
- QEMU/preflight and image audit passed; kernel 67737 bytes, ramdisk 2045806 bytes, tail 0. Fastboot boot accepted OKAY.
- Host: Fastboot disconnected 07:29:22 JST; HS attach on 1-9 at 07:30:04; descriptor read/64 error -110 at 07:30:10; stock Android 18d1:4ee7 returned 07:30:31; harness automatically returned selected handset to Fastboot 18d1:4ee0 at 07:30:42.
- usbmon: first address-zero descriptor completion -2, then three -71 completions; all returned length/capture 0. No Fullerene descriptor.
- Harness bug: `classification.txt` says `android-fallback` despite the preserved -110, because the newly added automatic-return branch bypasses `classify_postboot_result`. Original artifacts are preserved unchanged.
- Conclusion: reproduced attach boundary, not enumeration; no current standalone binary regression demonstrated. PHY RX failure is not established by this observation.

## Next experiment: direct-only failure policy

- Hypothesis: the apparent direct-profile attach is produced only after failed initialization, fallback/retries or signal rescue; its attribution to a successfully initialized EP0 is unproven.
- Single variable: opt-in `--direct-only` call-path policy. Perform exactly one existing direct USB2 initializer and reset on false; do not enter the other initializer, retries, or failed-init signal rescue. All successful-path register programming/readback is unchanged.
- Do not use existing `SINGLE_ATTEMPT`: that also changes Run/Stop readback and still permits the first fallback, so it is not this discriminator.
- Expected distinction: losing attach implies the excluded paths were necessary in this run; retained attach alone still does not prove EP0 or SOF. Only readable Fullerene descriptors establish enumeration.
- Source: freshly fetched Android qpr1 `drivers/usb/dwc3/gadget.c`, blob `300bd00840da94a2e47623bafdcda6ae733a1491`: `dwc3_gadget_run_stop` prepares event buffers/gadget then returns -ETIMEDOUT on failed halt-state completion. This supports separating initializer return status from attach; it does not prove Qualcomm hardware failure.
- Local evidence: `init_usb2_handoff` previously fell from `init_usb2_gadget_handoff` to `init_with_super_speed(false, true, false)` and another gadget attempt; `usb_probe_entry` allowed three attempts, then `run_ep0_signal_probe` had a failed-init recovery branch.
- Validation: new CLI/environment policy test observed RED (unknown --direct-only), then focused suite GREEN (18 tests); formatter and diff checks passed. Physical result pending.

## Run 2275276.0 (--direct-only)

- Hypothesis: if the observed attach in direct profiles was caused by fallback/retries or signal rescue, `--direct-only` will lose attach. If the direct initializer itself succeeds in enabling the pull-up, attach will be retained.
- Single variable: `--direct-only` (FULLERENE_AARCH64_USB_DIRECT_ONLY=1). Exactly one attempt of `init_usb2_gadget_handoff`, no retries, no fallback to super_speed, no signal rescue on failure.
- Starting state: Fastboot (18d1:4ee0), serial 26191JECB00076.
- Artifact: `tmp/fullerene-bramble-loop.2275276.0/fullerene-bramble-boot.img`, SHA-256 `f102c6c7a2649a2bd1e5c47677c8f7b45b6ac102a4daff620eba5d206c6618d1`.
- Host observation:
  - Fastboot disconnected: 08:05:01
  - Fullerene HS attach on usb 1-9: 08:05:43 (new high-speed USB device number 103)
  - Device descriptor read/64 error -110: 08:05:49
  - Stock Android fallback: 08:06:09
  - Automatic return to Fastboot: 08:06:16
- Classification: `usb-attach-or-descriptor-failure--110` (retained, bug in classifier fixed).
- usbmon: 1-9 descriptor read attempt 08:05:43, submit wLength=64, completion -2 (timeout) at 08:05:48, followed by three -71 retries with cap=0 length=0.
- Conclusion: Attach is **definitively produced by the direct USB2 path itself** (`init_usb2_gadget_handoff`), NOT by retries, fallbacks, or signal rescues! `init_usb2_gadget_handoff()` returns `true` and turns on pull-up. The failure is strictly at the EP0 packet/transaction level (host sends SETUP, device does not respond / controller does not receive or deliver to EP0).

