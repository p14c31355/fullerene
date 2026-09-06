# FullereneOS USB permissions

The current host reports `1234:0001` as `root:root` because the installed
Android udev rules only mark Google `18d1` devices as ADB-capable. The rule in
`70-fullerene-adb.rules` covers the temporary FullereneOS identity as well as
the stock Pixel 4a (5G) identity.

Install it only when preparing a deliberate physical bring-up:

```sh
sudo install -m 0644 70-fullerene-adb.rules /etc/udev/rules.d/70-fullerene-adb.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=usb
```

This repository does not install or reload the rule automatically. Confirm the
phone's intended USB mode before any subsequent bootloader operation.

The installed rule assigns both Fullerene and stock Google identities to
plugdev and adds uaccess, so a member of the plugdev group can open the USB
device after the rules are reloaded.
