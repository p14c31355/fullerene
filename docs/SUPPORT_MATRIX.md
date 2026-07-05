# Fullerene Support Matrix

## Native Syscalls

| # | Name | Status | Notes |
|---|------|--------|-------|
| 0 | abi_version | ✅ Full | |
| 1 | exit | ✅ Full | |
| 2 | fork | ✅ Full | COW page tables |
| 3 | read | ✅ Full | |
| 4 | write | ✅ Full | |
| 5 | open | ✅ Full | Read-only only |
| 6 | close | ✅ Full | |
| 7 | wait | 🔶 Partial | Non-blocking |
| 20 | getpid | ✅ Full | |
| 21 | get_process_name | ✅ Full | |
| 22 | yield | ✅ Full | |
| 30 | map_memory | ✅ Full | |
| 31 | unmap_memory | ✅ Full | |
| 32 | protect_memory | ⚠️ Stub | |
| 40 | create_event | ✅ Full | |
| 41 | wait_event | ✅ Full | |
| 42 | signal_event | ✅ Full | |
| 43 | subscribe_event | ⚠️ Stub | |
| 50 | create_thread | ✅ Full | |
| 51 | join_thread | ✅ Full | |
| 52 | detach_thread | ✅ Full | |
| 53 | exit_thread | ✅ Full | |
| 60 | create_window | ✅ Full | |
| 61 | destroy_window | ✅ Full | |
| 62 | resize_window | ✅ Full | |
| 63 | present_window | ✅ Full | |
| 64 | get_window_event | ⚠️ Stub | |
| 70 | enumerate_devices | 🔶 Partial | PCI only |
| 71 | open_device | ⚠️ Stub | |
| 72 | device_ioctl | ❌ NotSupported | |
| 80 | channel_create | ✅ Full | |
| 81 | channel_send | ✅ Full | |
| 82 | channel_recv | ✅ Full | |
| 83 | pipe_create | ✅ Full | Uses user buffer |
| 90 | handle_transfer | ✅ Full | |
| 91 | handle_duplicate | ✅ Full | |
| 92 | handle_revoke | ✅ Full | |
| 100 | clock_gettime | ✅ Full | |
| 101 | timer_create | ✅ Full | |
| 102 | sleep | 🔶 Partial | |
| 103 | uptime | ✅ Full | |

## Linux Compat Syscalls (partial)

| Syscall | Status |
|---------|--------|
| read, write, open, close | ✅ |
| fork, getpid | ✅ |
| mount, umount2 | ❌ NotSupported |
| truncate, ftruncate | ❌ NotSupported |
| fsync, fdatasync | ❌ NotSupported |
| exit, exit_group | ✅ |
| mmap, munmap | ✅ |
| brk | ✅ |
| sigaction, sigreturn | ⚠️ Stub |
| ioctl | ⚠️ Partial |
| clock_gettime | ✅ |
| getdents64 | ✅ |
| stat | ⚠️ Partial |

## Filesystem

| Feature | MemFS | FAT32 | exFAT |
|---------|-------|-------|-------|
| Read | ✅ | ✅ | ✅ |
| Write | ✅ | ✅ | ✅ |
| Create | ✅ | ❌ | ❌ |
| Mkdir | ✅ | ❌ | ❌ |
| Unlink | ✅ | ❌ | ❌ |
| Symlink | ✅ | ❌ | ❌ |
| Large files (>4GB) | ✅ | ❌ | 🔶 |

## Drivers

| Driver | QEMU | Real HW | Status |
|--------|------|---------|--------|
| PS/2 Keyboard | ✅ | ✅ | Stable |
| PS/2 Mouse | ✅ | ✅ | Stable |
| UEFI Framebuffer | ✅ | ✅ | Stable |
| VGA Text Mode | ✅ | ✅ | Stable |
| AHCI | ✅ | 🔶 | Beta |
| NVMe | ✅ | ✅ | Beta |
| xHCI | ✅ | ✅ | Beta |
| eHCI | ✅ | ❌ | Alpha |
| RTSX | ❌ | ✅ | Alpha |
| IWL WiFi | ❌ | 🔶 | Alpha |
| VirtIO GPU | ✅ | ❌ | Beta |
| HDA Audio | ❌ | 🔶 | Alpha |
| IOMMU | ✅ | 🔶 | Alpha |

Legend: ✅ Full, 🔶 Partial, ⚠️ Stub, ❌ NotSupported
