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
- [ ] Introduction of dirty rect
- [ ] Partial Redraw
- [ ] Deprecation of Fullscreen Redraw
- [ ] Compositor Responsibilities Reorganization
- [ ] Framebuffer Copy Optimization
- [ ] Draw Call Profiling

## Layer System
- [ ] Cursor-Specific Layer
- [ ] Window Layer Separation
- [ ] Overlay Layer
- [ ] Z-order Management
- [ ] Invalidate Range Management
## Visual Improvements
- [ ] Title Bar
- [ ] Semi-transparent Cursor
- [ ] Shadow Drawing
- [ ] UI Padding Adjustment
- [ ] Theme Color Support
- [ ] FPS/debug Overlay

---
# Window System
## Window Management
- [ ] Window Drag
- [ ] Window Focus
- [ ] Active Window Highlight
- [ ] Multiple Windows
- [ ] Window Close Function
- [ ] Resizing Function
## Desktop
- [ ] Desktop Layer
- [ ] Taskbar
- [ ] System Menu
- [ ] Clock Widget
- [ ] Mouse Right-Click Menu

---
# Input System
## Mouse
- [x] Mouse Cursor Display
- [x] Mouse Movement
- [ ] Left Click
- [ ] Right-click
- [ ] Drag event
- [ ] Double-click
- [ ] Wheel support
## Keyboard
- [ ] Modifier key support
- [ ] Key repeat
- [ ] Keymap abstraction
- [ ] Japanese layout support
## Event Architecture
- [ ] Event queue
- [ ] Event dispatcher
- [ ] Window event routing
- [ ] Timer event
- [ ] Input abstraction

---
# Terminal / Shell
## Shell Core
- [x] Command input
- [x] Basic builtin command
- [ ] Command history
- [ ] TAB completion
- [ ] Pipe
- [ ] Standard input/output abstraction
## Terminal Rendering
- [ ] Caret blink localization
- [ ] Scroll support
- [ ] ANSI escape sequence
- [ ] Color display
- [ ] selection/copy
- [x] Terminal buffer
## Commands
- [ ] ls
- [ ] cat
- [ ] pwd
- [ ] meminfo
- [ ] dmesg
- [ ] ps
- [ ] clear improvement
---
# Font System
## Future Font Pipeline
- [ ] PSF loader
- [ ] BDF importer
- [ ] build.rs font compiler
- [ ] Unicode Foundation
- [ ] fallback font
---
# Kernel / Runtime
## Tasking
- [ ] scheduler
- [ ] cooperative multitasking
- [ ] preemptive multitasking
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
- [ ] logging subsystem
- [x] serial logger
- [ ] kernel tracing

---

# Drivers

## Graphics
- [ ] VirtIO-GPU stabilization
- [ ] double buffering
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