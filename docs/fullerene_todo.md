# Fullerene TODO

## Boot Experience

### Boot Splash / Boot Screen
- [ ] Fullerene Logo Display
- [ ] Boot Message Display
- [ ] Resolution, CPU, and Memory Initialization Status Display
- [ ] Progress Indicator
- [ ] Boot Screen Fallback on Panic
- [ ] Fade Transition from Boot Screen to Desktop

### Branding
- [ ] Fullerene Color Palette Definition
- [ ] Logo Design Finalization
- [ ] Wallpaper Addition
- [ ] System Font Organization

---

# Drivers

## Graphics
- [ ] hardware cursor
- [ ] vsync

## Storage
- [ ] block cache

## Filesystem
- [ ] FAT32
- [ ] initramfs

## USB
- [ ] USB HID
- [ ] keyboard hotplug
- [ ] mouse hotplug

---

# Userspace

## Process Model
- [ ] ELF loader
- [ ] process abstraction
- [ ] Syscall layer
- [ ] Userspace memory isolation
## Applications
- [ ] Settings app
- [ ] Task monitor
- [ ] File browser
- [ ] Log viewer

---
# Developer Experience
## Build / Tooling
- [ ] Build time measurement
- [ ] QEMU launch helper
- [ ] Debug feature flags
- [ ] CI
- [ ] Nightly regression boot
## Documentation
- [ ] Graphics.md
- [ ] Memory.md
- [ ] Boot.md
- [ ] Driver model documentation

---
# Stretch Goals
- [ ] Network stack
- [ ] Audio output
- [ ] Wayland-style compositor
- [ ] SMP
- [ ] Rust userspace SDK
- [ ] Package manager
- [ ] Self-hosted build