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

# Graphics / Compositor

## Rendering Architecture
- [x] Introduction of dirty rect
- [x] Partial Redraw
- [x] Deprecation of Fullscreen Redraw
- [x] Compositor Responsibilities Reorganization
- [x] Framebuffer Copy Optimization
- [x] Draw Call Profiling

## Layer System
- [x] Cursor-Specific Layer
- [x] Window Layer Separation
- [x] Overlay Layer
- [x] Z-order Management
- [x] Invalidate Range Management
## Visual Improvements
- [x] Title Bar
- [x] Semi-transparent Cursor
- [x] Shadow Drawing
- [x] UI Padding Adjustment
- [x] Theme Color Support
- [x] FPS/debug Overlay

---
# Window System
## Window Management
- [x] Window Drag
- [x] Window Focus
- [x] Active Window Highlight
- [x] Multiple Windows
- [x] Window Close Function
- [x] Resizing Function
## Desktop
- [x] Desktop Layer
- [x] Taskbar
- [x] System Menu
- [x] Clock Widget
- [x] Mouse Right-Click Menu

---
# Input System
## Mouse
- [x] Mouse Cursor Display
- [x] Mouse Movement
- [x] Left Click
- [x] Right-click
- [x] Drag event
- [x] Double-click
- [x] Wheel support
## Keyboard
- [x] Modifier key support
- [x] Key repeat
- [x] Keymap abstraction
- [x] Japanese layout support
## Event Architecture
- [x] Event queue
- [x] Event dispatcher
- [x] Window event routing
- [x] Timer event
- [x] Input abstraction

---
# Terminal / Shell
## Shell Core
- [x] Command input
- [x] Basic builtin command
- [x] Command history
- [x] TAB completion
- [x] Pipe
- [x] Standard input/output abstraction
## Terminal Rendering
- [x] Caret blink localization
- [x] Scroll support
- [x] ANSI escape sequence
- [x] Color display
- [x] selection/copy
- [x] Terminal buffer
## Commands
- [x] ls
- [x] cat
- [x] pwd
- [x] meminfo
- [x] dmesg
- [x] ps
- [x] clear improvement
- [x] version / hexdump commands
---
# Font System
## Future Font Pipeline
- [x] PSF loader
- [ ] BDF importer
- [ ] build.rs font compiler
- [x] Unicode Foundation
- [x] fallback font
---
# Kernel / Runtime
## Tasking
- [x] scheduler
- [ ] cooperative multitasking
- [x] preemptive multitasking
- [ ] async runtime
- [x] timer subsystem
## Memory
- [ ] heap statistics
- [ ] slab allocator
- [ ] virtual memory
- [ ] userspace address space
- [ ] page fault handler

## Diagnostics
- [ ] panic screen
- [ ] stack trace
- [x] logging subsystem
- [x] serial logger
- [ ] kernel tracing

---

# Drivers

## Graphics
- [ ] VirtIO-GPU stabilization
- [x] double buffering
- [ ] hardware cursor
- [ ] vsync

## Storage
- [ ] AHCI
- [ ] NVMe
- [ ] block cache

## Filesystem
- [ ] VFS
- [ ] tmpfs
- [ ] FAT32
- [ ] initramfs

## USB
- [ ] USB HID
- [ ] keyboard hotplug
- [ ] mouse hotplug

---

#Userspace

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
- [ ] Architecture.md
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