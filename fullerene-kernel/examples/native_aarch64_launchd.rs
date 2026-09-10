#![no_std]
#![no_main]

//! Minimal AArch64 PID-1-shaped bootstrap payload.
//!
//! This is intentionally small, but it is a real user ELF: the kernel loads
//! it from the build artifact, maps its code/data segments, enters EL0, and
//! services its SVCs through the native AArch64 dispatcher. The eventual
//! launchd service table can grow behind this same ABI without changing the
//! kernel entry contract.

use core::arch::asm;

const ABI_QUERY: u64 = 0;
const EXIT: u64 = 1;
const FORK: u64 = 2;
const READ: u64 = 3;
const WRITE: u64 = 4;
const OPEN: u64 = 5;
const CLOSE: u64 = 6;
const WAIT: u64 = 7;
const MAP_MEMORY: u64 = 30;
const UNMAP_MEMORY: u64 = 31;
const PROTECT_MEMORY: u64 = 32;
const QUERY_MEMORY: u64 = 33;
const GET_PID: u64 = 20;
const GET_PROCESS_NAME: u64 = 21;
const YIELD: u64 = 22;
const SPAWN: u64 = 23;
const EXEC: u64 = 24;
const EXEC_PATH: u64 = 25;
const SHARED_BUFFER_CREATE: u64 = 34;
const SHARED_BUFFER_MAP: u64 = 35;
const SHARED_BUFFER_UNMAP: u64 = 36;
const CREATE_EVENT: u64 = 40;
const WAIT_EVENT: u64 = 41;
const SIGNAL_EVENT: u64 = 42;
const SUBSCRIBE_EVENT: u64 = 43;
const CREATE_THREAD: u64 = 50;
const JOIN_THREAD: u64 = 51;
const DETACH_THREAD: u64 = 52;
const EXIT_THREAD: u64 = 53;
const CREATE_TERMINAL: u64 = 65;
const CREATE_WINDOW: u64 = 60;
const DESTROY_WINDOW: u64 = 61;
const RESIZE_WINDOW: u64 = 62;
const PRESENT_WINDOW: u64 = 63;
const GET_WINDOW_EVENT: u64 = 64;
const ENUMERATE_DEVICES: u64 = 70;
const OPEN_DEVICE: u64 = 71;
const DEVICE_IOCTL: u64 = 72;
const DEVICE_GET_CAPABILITIES: u64 = 8;
const DEVICE_GET_RESOURCES: u64 = 12;
const DEVICE_RESOURCE_KIND_MMIO: u64 = 1;
const CLOCK_GETTIME: u64 = 100;
const TIMER_CREATE: u64 = 101;
const SLEEP: u64 = 102;
const UPTIME: u64 = 103;
const CHANNEL_CREATE: u64 = 80;
const CHANNEL_SEND: u64 = 81;
const CHANNEL_RECV: u64 = 82;
const PIPE_CREATE: u64 = 83;
const HANDLE_DUPLICATE: u64 = 91;
const HANDLE_TRANSFER: u64 = 90;
const HANDLE_REVOKE: u64 = 92;
const OPEN_PROCESS_CONTROL: u64 = 110;
const PROCESS_CONTROL_STOP: u64 = 111;
const PROCESS_CONTROL_STATUS: u64 = 112;
const PROCESS_CONTROL_REAP: u64 = 113;
const PROCESS_CONTROL_ASSIGN: u64 = 114;

static LAUNCH_MESSAGE: &[u8] = b"launchd: spawning child\n";
static REPEAT_MESSAGE: &[u8] = b"launchd: reusing child slot\n";
static EXEC_MESSAGE: &[u8] = b"launchd: exec child\n";
static EXEC_PATH_MESSAGE: &[u8] = b"launchd: exec path child\n";
static INHERIT_FD_MESSAGE: &[u8] = b"launchd: inherited fd child\n";
static DYNAMIC_MESSAGE: &[u8] = b"launchd: dynamic mapping\n";
static COW_CHILD_MESSAGE: &[u8] = b"launchd: cow child\n";
static FORK_MESSAGE: &[u8] = b"launchd: fork child\n";
static ORPHAN_REAP_MESSAGE: &[u8] = b"launchd: reaped adopted child\n";
static EVENT_MESSAGE: &[u8] = b"AArch64 event ok\n";
static SHARED_BUFFER_MESSAGE: &[u8] = b"AArch64 shared buffer ok\n";
static TIME_MESSAGE: &[u8] = b"AArch64 time ok\n";
static CHANNEL_MESSAGE: &[u8] = b"AArch64 channel ok\n";
static PIPE_MESSAGE: &[u8] = b"AArch64 pipe ok\n";
static PROCESS_CONTROL_MESSAGE: &[u8] = b"AArch64 process control ok\n";
static TIMER_MESSAGE: &[u8] = b"AArch64 timer ok\n";
static THREAD_MESSAGE: &[u8] = b"AArch64 thread ok\n";
static TERMINAL_TITLE: &[u8] = b"AArch64 terminal";
static TERMINAL_MESSAGE: &[u8] = b"AArch64 terminal ok\n";
static DEVICE_MESSAGE: &[u8] = b"AArch64 device inventory ok\n";
static WINDOW_MESSAGE: &[u8] = b"AArch64 window ok\n";
static DEVICE_IDENTIFIER: &[u8] = b"1234:0001\0";
static PLATFORM_IDENTIFIER_2: &[u8] = b"2\0";
static PLATFORM_IDENTIFIER_3: &[u8] = b"3\0";
static PLATFORM_IDENTIFIER_4: &[u8] = b"4\0";
static PLATFORM_IDENTIFIER_5: &[u8] = b"5\0";
static PLATFORM_IDENTIFIER_6: &[u8] = b"6\0";
static MOTD_PATH: &[u8] = b"/etc/motd\0";
static WRITE_TEST_PATH: &[u8] = b"/etc/write-test\0";
static WRITE_TEST_MESSAGE: &[u8] = b"AArch64 VFS write ok\n";
static CHILD_PATH: &[u8] = b"/bin/child\0";
static CHILD_NAME: &[u8] = b"child";
const CHILD_IMAGE_CAPACITY: u64 = 96 * 1024;

#[repr(C, align(16))]
struct ProcessName([u8; 16]);

static mut PROCESS_NAME: ProcessName = ProcessName([0; 16]);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _abi = unsafe { syscall(ABI_QUERY, 0, 0, 0, 0, 0, 0) };
    let _pid = unsafe { syscall(GET_PID, 0, 0, 0, 0, 0, 0) };
    let name = unsafe { core::ptr::addr_of_mut!(PROCESS_NAME.0) as u64 };
    let _name_len = unsafe { syscall(GET_PROCESS_NAME, name, 16, 0, 0, 0, 0) };
    let file = unsafe { syscall(OPEN, MOTD_PATH.as_ptr() as u64, 0, 0, 0, 0, 0) };
    if (file as i64) >= 0 {
        let mut file_buffer = [0u8; 64];
        let bytes = unsafe {
            syscall(
                READ,
                file,
                file_buffer.as_mut_ptr() as u64,
                file_buffer.len() as u64,
                0,
                0,
                0,
            )
        };
        if (bytes as i64) > 0 {
            let _ = unsafe { syscall(WRITE, 1, file_buffer.as_ptr() as u64, bytes, 0, 0, 0) };
        }
        let _ = unsafe { syscall(CLOSE, file, 0, 0, 0, 0, 0) };
    }
    let mapped = unsafe { syscall(MAP_MEMORY, 0, 4096, 3 << 16, 0, 0, 0) };
    if (mapped as i64) >= 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(
                DYNAMIC_MESSAGE.as_ptr(),
                mapped as *mut u8,
                DYNAMIC_MESSAGE.len(),
            );
        }
        let _ = unsafe { syscall(WRITE, 1, mapped, DYNAMIC_MESSAGE.len() as u64, 0, 0, 0) };
        let mut memory_info = [0u8; 64];
        let info = memory_info.as_mut_ptr() as u64;
        let _ = unsafe { syscall(QUERY_MEMORY, info, 64, 0, 0, 0, 0) };
    }
    let fork_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    if fork_pid == 0 {
        if (mapped as i64) >= 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    COW_CHILD_MESSAGE.as_ptr(),
                    mapped as *mut u8,
                    COW_CHILD_MESSAGE.len(),
                );
            }
            let _ = unsafe { syscall(WRITE, 1, mapped, COW_CHILD_MESSAGE.len() as u64, 0, 0, 0) };
        }
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                FORK_MESSAGE.as_ptr() as u64,
                FORK_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
        let _ = unsafe { syscall(EXIT, 0, 0, 0, 0, 0, 0) };
    } else if (fork_pid as i64) > 0 {
        let _ = unsafe { syscall(WAIT, fork_pid, 0, 0, 0, 0, 0) };
        if (mapped as i64) >= 0 {
            let _ = unsafe { syscall(WRITE, 1, mapped, DYNAMIC_MESSAGE.len() as u64, 0, 0, 0) };
            let _ = unsafe { syscall(PROTECT_MEMORY, mapped, 4096, 1, 0, 0, 0) };
        }
    }
    if (mapped as i64) >= 0 {
        let _ = unsafe { syscall(UNMAP_MEMORY, mapped, 4096, 0, 0, 0, 0) };
    }
    let _ = unsafe {
        syscall(
            WRITE,
            1,
            LAUNCH_MESSAGE.as_ptr() as u64,
            LAUNCH_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    reap_spawned_child(spawn_child_once());
    let _ = unsafe {
        syscall(
            WRITE,
            1,
            REPEAT_MESSAGE.as_ptr() as u64,
            REPEAT_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    reap_spawned_child(spawn_child_once());
    let _ = unsafe {
        syscall(
            WRITE,
            1,
            EXEC_MESSAGE.as_ptr() as u64,
            EXEC_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    reap_spawned_child(exec_child_once());
    let _ = unsafe {
        syscall(
            WRITE,
            1,
            EXEC_PATH_MESSAGE.as_ptr() as u64,
            EXEC_PATH_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    reap_spawned_child(exec_path_child_once());
    let _ = unsafe {
        syscall(
            WRITE,
            1,
            INHERIT_FD_MESSAGE.as_ptr() as u64,
            INHERIT_FD_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    reap_spawned_child(fork_inherited_file_once());
    shared_buffer_once();
    event_once();
    channel_once();
    pipe_once();
    process_control_once();
    timer_once();
    thread_once();
    terminal_once();
    device_once();
    window_once();
    time_once();
    write_file_once();
    let _ = unsafe { syscall(EXIT, 0, 0, 0, 0, 0, 0) };
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

#[unsafe(no_mangle)]
extern "C" fn thread_entry() -> ! {
    let _ = unsafe { syscall(EXIT_THREAD, 37, 0, 0, 0, 0, 0) };
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

fn thread_once() {
    let stack = unsafe { syscall(MAP_MEMORY, 0, 8192, 3 << 16, 0, 0, 0) };
    if (stack as i64) < 0 {
        return;
    }
    let handle = unsafe {
        syscall(
            CREATE_THREAD,
            thread_entry as *const () as u64,
            stack.saturating_add(8192 - 16),
            0,
            0,
            0,
            0,
        )
    };
    let joined = if (handle as i64) >= 0 {
        unsafe { syscall(JOIN_THREAD, handle, 0, 0, 0, 0, 0) }
    } else {
        handle
    };
    let closed = if (handle as i64) >= 0 {
        unsafe { syscall(CLOSE, handle, 0, 0, 0, 0, 0) }
    } else {
        0
    };
    let detached_handle = unsafe {
        syscall(
            CREATE_THREAD,
            thread_entry as *const () as u64,
            stack.saturating_add(8192 - 16),
            0,
            0,
            0,
            0,
        )
    };
    let detached = if (detached_handle as i64) >= 0 {
        unsafe { syscall(DETACH_THREAD, detached_handle, 0, 0, 0, 0, 0) }
    } else {
        detached_handle
    };
    let yielded = if (detached_handle as i64) >= 0 {
        unsafe { syscall(YIELD, 0, 0, 0, 0, 0, 0) }
    } else {
        detached_handle
    };
    let detached_closed = if (detached_handle as i64) >= 0 {
        unsafe { syscall(CLOSE, detached_handle, 0, 0, 0, 0, 0) }
    } else {
        0
    };
    let unmapped = unsafe { syscall(UNMAP_MEMORY, stack, 8192, 0, 0, 0, 0) };
    if joined == 37
        && closed == 0
        && detached == 0
        && yielded == 0
        && detached_closed == 0
        && unmapped == 0
    {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                THREAD_MESSAGE.as_ptr() as u64,
                THREAD_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn spawn_child_once() -> u64 {
    spawn_child_with_terminal(0)
}

fn spawn_child_with_terminal(terminal_handle: u64) -> u64 {
    let child_mapping = unsafe { syscall(MAP_MEMORY, 0, CHILD_IMAGE_CAPACITY, 3 << 16, 0, 0, 0) };
    if (child_mapping as i64) < 0 {
        return child_mapping;
    }
    let mut child_pid = (-(22i64)) as u64;
    let child_file = unsafe { syscall(OPEN, CHILD_PATH.as_ptr() as u64, 0, 0, 0, 0, 0) };
    if (child_file as i64) >= 0 {
        let mut offset = 0u64;
        let mut complete = true;
        while offset < CHILD_IMAGE_CAPACITY {
            let request = (CHILD_IMAGE_CAPACITY - offset).min(4096);
            let bytes = unsafe {
                syscall(
                    READ,
                    child_file,
                    child_mapping.saturating_add(offset),
                    request,
                    0,
                    0,
                    0,
                )
            };
            if (bytes as i64) < 0 {
                complete = false;
                break;
            }
            if bytes == 0 {
                break;
            }
            offset = offset.saturating_add(bytes);
        }
        let _ = unsafe { syscall(CLOSE, child_file, 0, 0, 0, 0, 0) };
        if complete && offset > 0 {
            child_pid = unsafe {
                syscall(
                    SPAWN,
                    child_mapping,
                    offset,
                    CHILD_NAME.as_ptr() as u64,
                    CHILD_NAME.len() as u64,
                    terminal_handle,
                    1,
                )
            };
        }
    }
    let _ = unsafe {
        syscall(
            UNMAP_MEMORY,
            child_mapping,
            CHILD_IMAGE_CAPACITY,
            0,
            0,
            0,
            0,
        )
    };
    child_pid
}

fn terminal_once() {
    let terminal = unsafe {
        syscall(
            CREATE_TERMINAL,
            TERMINAL_TITLE.as_ptr() as u64,
            TERMINAL_TITLE.len() as u64,
            0,
            0,
            0,
            0,
        )
    };
    if terminal == 0 || terminal & (1 << 62) == 0 {
        return;
    }
    let written = unsafe {
        syscall(
            WRITE,
            terminal,
            TERMINAL_MESSAGE.as_ptr() as u64,
            TERMINAL_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    let child = spawn_child_with_terminal(terminal);
    if child > 0 {
        reap_spawned_child(child);
    }
    let closed = unsafe { syscall(CLOSE, terminal, 0, 0, 0, 0, 0) };
    if written == TERMINAL_MESSAGE.len() as u64 && child > 0 && closed == 0 {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                TERMINAL_MESSAGE.as_ptr() as u64,
                TERMINAL_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn device_once() {
    let mapping = unsafe { syscall(MAP_MEMORY, 0, 4096, 3 << 16, 0, 0, 0) };
    if (mapping as i64) < 0 {
        return;
    }
    let count = unsafe { syscall(ENUMERATE_DEVICES, 0, mapping, 4096, 0, 0, 0) };
    let valid = if count == 0 {
        true
    } else if (count as i64) >= 0 {
        let rows = usize::try_from(count).unwrap_or(usize::MAX).min(4096 / 16);
        let mut usb_present = false;
        let mut platform_resource_id = 0u32;
        let mut platform_resource_class = 0u32;
        for index in 0..rows {
            let row = mapping.saturating_add((index * 16) as u64);
            let class = unsafe { core::ptr::read_volatile(row as *const u32) };
            let device_id =
                unsafe { core::ptr::read_volatile(row.saturating_add(4) as *const u32) };
            let vendor_id =
                unsafe { core::ptr::read_volatile(row.saturating_add(8) as *const u32) };
            let product_id =
                unsafe { core::ptr::read_volatile(row.saturating_add(12) as *const u32) };
            if class == 6 && device_id == 1 && vendor_id == 0x1234 && product_id == 1 {
                usb_present = true;
            }
            if (2..=6).contains(&device_id) {
                platform_resource_id = device_id;
                platform_resource_class = class;
            }
        }
        if !usb_present {
            if platform_resource_id == 0 {
                // A DT-only platform inventory is still valid when the USB
                // handoff was withheld and no resource-bearing smoke row is
                // present.
                true
            } else {
                let identifier = match platform_resource_id {
                    2 => PLATFORM_IDENTIFIER_2,
                    3 => PLATFORM_IDENTIFIER_3,
                    4 => PLATFORM_IDENTIFIER_4,
                    5 => PLATFORM_IDENTIFIER_5,
                    6 => PLATFORM_IDENTIFIER_6,
                    _ => &[][..],
                };
                let handle =
                    unsafe { syscall(OPEN_DEVICE, identifier.as_ptr() as u64, 0, 0, 0, 0, 0) };
                let capability_result = if handle & (1 << 62) != 0 {
                    unsafe {
                        syscall(
                            DEVICE_IOCTL,
                            handle,
                            DEVICE_GET_CAPABILITIES,
                            mapping,
                            0,
                            0,
                            0,
                        )
                    }
                } else {
                    (-(9i64)) as u64
                };
                let capability_class = unsafe { core::ptr::read_volatile(mapping as *const u32) };
                let capabilities =
                    unsafe { core::ptr::read_volatile(mapping.saturating_add(8) as *const u64) };
                let resource_result = if handle & (1 << 62) != 0 {
                    unsafe { syscall(DEVICE_IOCTL, handle, DEVICE_GET_RESOURCES, mapping, 0, 0, 0) }
                } else {
                    (-(9i64)) as u64
                };
                let resource_base = unsafe { core::ptr::read_volatile(mapping as *const u64) };
                let resource_size =
                    unsafe { core::ptr::read_volatile(mapping.saturating_add(8) as *const u64) };
                let resource_kind =
                    unsafe { core::ptr::read_volatile(mapping.saturating_add(16) as *const u32) };
                let duplicate = if handle & (1 << 62) != 0 {
                    unsafe { syscall(HANDLE_DUPLICATE, handle, 0, 0, 0, 0, 0) }
                } else {
                    0
                };
                let duplicate_closed = if duplicate & (1 << 62) != 0 {
                    unsafe { syscall(CLOSE, duplicate, 0, 0, 0, 0, 0) }
                } else {
                    0
                };
                let closed = if handle & (1 << 62) != 0 {
                    unsafe { syscall(CLOSE, handle, 0, 0, 0, 0, 0) }
                } else {
                    (-(9i64)) as u64
                };
                capability_result == 0
                    && capability_class == platform_resource_class
                    && capabilities == 0
                    && resource_result == 0
                    && resource_base != 0
                    && resource_size != 0
                    && resource_kind == DEVICE_RESOURCE_KIND_MMIO as u32
                    && duplicate & (1 << 62) != 0
                    && duplicate_closed == 0
                    && closed == 0
            }
        } else {
            let handle = unsafe {
                syscall(
                    OPEN_DEVICE,
                    DEVICE_IDENTIFIER.as_ptr() as u64,
                    0,
                    0,
                    0,
                    0,
                    0,
                )
            };
            let ioctl_result = if handle & (1 << 62) != 0 {
                unsafe {
                    syscall(
                        DEVICE_IOCTL,
                        handle,
                        DEVICE_GET_CAPABILITIES,
                        mapping,
                        0,
                        0,
                        0,
                    )
                }
            } else {
                (-(9i64)) as u64
            };
            let capability_class = unsafe { core::ptr::read_volatile(mapping as *const u32) };
            let capabilities =
                unsafe { core::ptr::read_volatile(mapping.saturating_add(8) as *const u64) };
            let duplicate = if handle & (1 << 62) != 0 {
                unsafe { syscall(HANDLE_DUPLICATE, handle, 0, 0, 0, 0, 0) }
            } else {
                0
            };
            let duplicate_closed = if duplicate & (1 << 62) != 0 {
                unsafe { syscall(CLOSE, duplicate, 0, 0, 0, 0, 0) }
            } else {
                0
            };
            let closed = if handle & (1 << 62) != 0 {
                unsafe { syscall(CLOSE, handle, 0, 0, 0, 0, 0) }
            } else {
                (-(9i64)) as u64
            };
            ioctl_result == 0
                && capability_class == 6
                && capabilities == 0
                && duplicate & (1 << 62) != 0
                && duplicate_closed == 0
                && closed == 0
        }
    } else {
        false
    };
    let unmapped = unsafe { syscall(UNMAP_MEMORY, mapping, 4096, 0, 0, 0, 0) };
    if valid && unmapped == 0 {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                DEVICE_MESSAGE.as_ptr() as u64,
                DEVICE_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn window_once() {
    let mapping = unsafe { syscall(MAP_MEMORY, 0, 4096, 3 << 16, 0, 0, 0) };
    if (mapping as i64) < 0 {
        return;
    }
    let handle = unsafe { syscall(CREATE_WINDOW, 10, 20, 64, 32, 0, 0) };
    let resized = if handle & (1 << 62) != 0 {
        unsafe { syscall(RESIZE_WINDOW, handle, 80, 40, 0, 0, 0) }
    } else {
        (-(9i64)) as u64
    };
    let presented = if handle & (1 << 62) != 0 {
        unsafe { syscall(PRESENT_WINDOW, handle, 0, 0, 0, 0, 0) }
    } else {
        (-(9i64)) as u64
    };
    let event_result = if handle & (1 << 62) != 0 {
        unsafe { syscall(GET_WINDOW_EVENT, handle, mapping, 128, 0, 0, 0) }
    } else {
        (-(9i64)) as u64
    };
    let kind = unsafe { core::ptr::read_volatile(mapping as *const u32) };
    let window_id = unsafe { core::ptr::read_volatile(mapping.saturating_add(8) as *const u64) };
    let width = unsafe { core::ptr::read_volatile(mapping.saturating_add(32) as *const u64) };
    let height = unsafe { core::ptr::read_volatile(mapping.saturating_add(40) as *const u64) };
    let duplicate = if handle & (1 << 62) != 0 {
        unsafe { syscall(HANDLE_DUPLICATE, handle, 0, 0, 0, 0, 0) }
    } else {
        0
    };
    let duplicate_closed = if duplicate & (1 << 62) != 0 {
        unsafe { syscall(CLOSE, duplicate, 0, 0, 0, 0, 0) }
    } else {
        0
    };
    let destroyed = if handle & (1 << 62) != 0 {
        unsafe { syscall(DESTROY_WINDOW, handle, 0, 0, 0, 0, 0) }
    } else {
        (-(9i64)) as u64
    };
    let closed = if handle & (1 << 62) != 0 {
        unsafe { syscall(CLOSE, handle, 0, 0, 0, 0, 0) }
    } else {
        (-(9i64)) as u64
    };
    let unmapped = unsafe { syscall(UNMAP_MEMORY, mapping, 4096, 0, 0, 0, 0) };
    if handle & (1 << 62) != 0
        && resized == 0
        && presented == 0
        && event_result == 0
        && kind == 1
        && window_id != 0
        && width == 80
        && height == 40
        && duplicate & (1 << 62) != 0
        && duplicate_closed == 0
        && destroyed == 0
        && closed == 0
        && unmapped == 0
    {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                WINDOW_MESSAGE.as_ptr() as u64,
                WINDOW_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn exec_child_once() -> u64 {
    let child_mapping = unsafe { syscall(MAP_MEMORY, 0, CHILD_IMAGE_CAPACITY, 3 << 16, 0, 0, 0) };
    if (child_mapping as i64) < 0 {
        return child_mapping;
    }
    let mut child_pid = (-(22i64)) as u64;
    let child_file = unsafe { syscall(OPEN, CHILD_PATH.as_ptr() as u64, 0, 0, 0, 0, 0) };
    if (child_file as i64) >= 0 {
        let mut offset = 0u64;
        let mut complete = true;
        while offset < CHILD_IMAGE_CAPACITY {
            let request = (CHILD_IMAGE_CAPACITY - offset).min(4096);
            let bytes = unsafe {
                syscall(
                    READ,
                    child_file,
                    child_mapping.saturating_add(offset),
                    request,
                    0,
                    0,
                    0,
                )
            };
            if (bytes as i64) < 0 {
                complete = false;
                break;
            }
            if bytes == 0 {
                break;
            }
            offset = offset.saturating_add(bytes);
        }
        let _ = unsafe { syscall(CLOSE, child_file, 0, 0, 0, 0, 0) };
        if complete && offset > 0 {
            let fork_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
            if fork_pid == 0 {
                let result = unsafe {
                    syscall(
                        EXEC,
                        child_mapping,
                        offset,
                        CHILD_NAME.as_ptr() as u64,
                        CHILD_NAME.len() as u64,
                        0,
                        0,
                    )
                };
                let _ = unsafe { syscall(EXIT, result, 0, 0, 0, 0, 0) };
            }
            child_pid = fork_pid;
        }
    }
    let _ = unsafe {
        syscall(
            UNMAP_MEMORY,
            child_mapping,
            CHILD_IMAGE_CAPACITY,
            0,
            0,
            0,
            0,
        )
    };
    child_pid
}

fn exec_path_child_once() -> u64 {
    let child_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    if child_pid == 0 {
        let argv = [CHILD_PATH.as_ptr() as u64, 0];
        let envp = [0u64; 1];
        let result = unsafe {
            syscall(
                EXEC_PATH,
                CHILD_PATH.as_ptr() as u64,
                argv.as_ptr() as u64,
                envp.as_ptr() as u64,
                0,
                0,
                0,
            )
        };
        let _ = unsafe { syscall(EXIT, result, 0, 0, 0, 0, 0) };
    }
    child_pid
}

fn fork_inherited_file_once() -> u64 {
    let file = unsafe { syscall(OPEN, MOTD_PATH.as_ptr() as u64, 0, 0, 0, 0, 0) };
    if (file as i64) < 0 {
        return file;
    }
    let child_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    if child_pid == 0 {
        let mut buffer = [0u8; 64];
        let bytes = unsafe {
            syscall(
                READ,
                file,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                0,
                0,
                0,
            )
        };
        if (bytes as i64) > 0 {
            let _ = unsafe { syscall(WRITE, 1, buffer.as_ptr() as u64, bytes, 0, 0, 0) };
        }
        let _ = unsafe { syscall(CLOSE, file, 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(EXIT, 0, 0, 0, 0, 0, 0) };
    }
    if (child_pid as i64) > 0 {
        let _ = unsafe { syscall(CLOSE, file, 0, 0, 0, 0, 0) };
    }
    child_pid
}

fn write_file_once() {
    let file = unsafe { syscall(OPEN, WRITE_TEST_PATH.as_ptr() as u64, 1, 0, 0, 0, 0) };
    if (file as i64) < 0 {
        return;
    }
    let written = unsafe {
        syscall(
            WRITE,
            file,
            WRITE_TEST_MESSAGE.as_ptr() as u64,
            WRITE_TEST_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    let _ = unsafe { syscall(CLOSE, file, 0, 0, 0, 0, 0) };
    if written != WRITE_TEST_MESSAGE.len() as u64 {
        return;
    }

    let file = unsafe { syscall(OPEN, WRITE_TEST_PATH.as_ptr() as u64, 0, 0, 0, 0, 0) };
    if (file as i64) < 0 {
        return;
    }
    let mut buffer = [0u8; 32];
    let bytes = unsafe {
        syscall(
            READ,
            file,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
            0,
            0,
        )
    };
    let _ = unsafe { syscall(CLOSE, file, 0, 0, 0, 0, 0) };
    if (bytes as i64) > 0 {
        let _ = unsafe { syscall(WRITE, 1, buffer.as_ptr() as u64, bytes, 0, 0, 0) };
    }
}

fn pipe_once() {
    let mut handles = [0u64; 2];
    let result = unsafe { syscall(PIPE_CREATE, handles.as_mut_ptr() as u64, 0, 0, 0, 0, 0) };
    if result != 0 {
        return;
    }
    let duplicate = unsafe { syscall(HANDLE_DUPLICATE, handles[1], 0, 0, 0, 0, 0) };
    if (duplicate as i64) < 0 {
        let _ = unsafe { syscall(CLOSE, handles[0], 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(CLOSE, handles[1], 0, 0, 0, 0, 0) };
        return;
    }
    let _ = unsafe { syscall(CLOSE, handles[1], 0, 0, 0, 0, 0) };
    let child_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    if child_pid == 0 {
        let _ = unsafe { syscall(CLOSE, handles[0], 0, 0, 0, 0, 0) };
        let written = unsafe {
            syscall(
                WRITE,
                duplicate,
                PIPE_MESSAGE.as_ptr() as u64,
                PIPE_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
        let _ = unsafe { syscall(CLOSE, duplicate, 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(EXIT, written, 0, 0, 0, 0, 0) };
    }
    if (child_pid as i64) <= 0 {
        let _ = unsafe { syscall(CLOSE, handles[0], 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(CLOSE, duplicate, 0, 0, 0, 0, 0) };
        return;
    }
    let _ = unsafe { syscall(CLOSE, duplicate, 0, 0, 0, 0, 0) };
    let child_status = unsafe { syscall(WAIT, child_pid, 0, 0, 0, 0, 0) };
    if child_status != PIPE_MESSAGE.len() as u64 {
        let _ = unsafe { syscall(CLOSE, handles[0], 0, 0, 0, 0, 0) };
        return;
    }
    let mut buffer = [0u8; 32];
    let bytes = unsafe {
        syscall(
            READ,
            handles[0],
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
            0,
            0,
        )
    };
    let _ = unsafe { syscall(CLOSE, handles[0], 0, 0, 0, 0, 0) };
    if (bytes as i64) > 0 {
        let _ = unsafe { syscall(WRITE, 1, buffer.as_ptr() as u64, bytes, 0, 0, 0) };
    }
}

fn event_once() {
    let event = unsafe { syscall(CREATE_EVENT, 0, 0, 0, 0, 0, 0) };
    let gate = unsafe { syscall(CREATE_EVENT, 0, 0, 0, 0, 0, 0) };
    let done = unsafe { syscall(CREATE_EVENT, 0, 0, 0, 0, 0, 0) };
    let mut pipe = [0u64; 2];
    let pipe_result = unsafe { syscall(PIPE_CREATE, pipe.as_mut_ptr() as u64, 0, 0, 0, 0, 0) };
    if (event as i64) < 0 || (gate as i64) < 0 || (done as i64) < 0 || pipe_result != 0 {
        for handle in [event, gate, done, pipe[0], pipe[1]] {
            if (handle as i64) >= 0 {
                let _ = unsafe { syscall(CLOSE, handle, 0, 0, 0, 0, 0) };
            }
        }
        return;
    }
    let subscribed = unsafe { syscall(SUBSCRIBE_EVENT, 1, event, 0, 0, 0, 0) };
    let child_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    if child_pid == 0 {
        // Close the inherited source capability before the parent moves its
        // copy here.  The gate keeps the child alive while the parent obtains
        // the newly allocated target handle and sends it over the pipe.
        let _ = unsafe { syscall(CLOSE, event, 0, 0, 0, 0, 0) };
        let ready = [1u8; 1];
        let _ = unsafe {
            syscall(
                WRITE,
                pipe[1],
                ready.as_ptr() as u64,
                ready.len() as u64,
                0,
                0,
                0,
            )
        };
        let gate_result = unsafe { syscall(WAIT_EVENT, gate, 1_000_000, 0, 0, 0, 0) };
        let mut transferred = 0u64;
        let received = unsafe {
            syscall(
                READ,
                pipe[0],
                (&mut transferred as *mut u64) as u64,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
                0,
            )
        };
        let signaled = if gate_result == 0 && received == core::mem::size_of::<u64>() as u64 {
            unsafe { syscall(SIGNAL_EVENT, transferred, 0, 0, 0, 0, 0) }
        } else {
            (-(22i64)) as u64
        };
        let _ = unsafe { syscall(SIGNAL_EVENT, done, 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(CLOSE, pipe[0], 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(CLOSE, pipe[1], 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(CLOSE, gate, 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(CLOSE, done, 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(EXIT, signaled, 0, 0, 0, 0, 0) };
    }
    if (child_pid as i64) <= 0 {
        for handle in [event, gate, done, pipe[0], pipe[1]] {
            let _ = unsafe { syscall(CLOSE, handle, 0, 0, 0, 0, 0) };
        }
        return;
    }

    let mut ready = [0u8; 1];
    let mut ready_bytes = 0u64;
    for _ in 0..4 {
        ready_bytes = unsafe {
            syscall(
                READ,
                pipe[0],
                ready.as_mut_ptr() as u64,
                ready.len() as u64,
                0,
                0,
                0,
            )
        };
        if ready_bytes == 1 {
            break;
        }
        let _ = unsafe { syscall(YIELD, 0, 0, 0, 0, 0, 0) };
    }
    let transferred = unsafe { syscall(HANDLE_TRANSFER, child_pid, event, 0, 0, 0, 0) };
    let sent = unsafe {
        syscall(
            WRITE,
            pipe[1],
            (&transferred as *const u64) as u64,
            core::mem::size_of::<u64>() as u64,
            0,
            0,
            0,
        )
    };
    let source_revoked = unsafe { syscall(CLOSE, event, 0, 0, 0, 0, 0) };
    let gate_signal = unsafe { syscall(SIGNAL_EVENT, gate, 0, 0, 0, 0, 0) };
    let waited = unsafe { syscall(WAIT_EVENT, done, 1_000_000, 0, 0, 0, 0) };
    let child_status = unsafe { syscall(WAIT, child_pid, 0, 0, 0, 0, 0) };
    for handle in [gate, done, pipe[0], pipe[1]] {
        let _ = unsafe { syscall(CLOSE, handle, 0, 0, 0, 0, 0) };
    }
    if subscribed == 0
        && ready_bytes == 1
        && (transferred as i64) >= 0
        && sent == core::mem::size_of::<u64>() as u64
        && (source_revoked as i64) < 0
        && gate_signal == 0
        && waited == 0
        && child_status == 0
    {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                EVENT_MESSAGE.as_ptr() as u64,
                EVENT_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn shared_buffer_once() {
    let buffer = unsafe { syscall(SHARED_BUFFER_CREATE, 4096, 0b111, 0, 0, 0, 0) };
    if (buffer as i64) < 0 {
        return;
    }
    let first = unsafe { syscall(SHARED_BUFFER_MAP, buffer, 0, 0b11, 0, 0, 0) };
    let second = unsafe { syscall(SHARED_BUFFER_MAP, buffer, 0, 0b11, 0, 0, 0) };
    if (first as i64) < 0 || (second as i64) < 0 {
        if (first as i64) >= 0 {
            let _ = unsafe { syscall(SHARED_BUFFER_UNMAP, buffer, first, 0, 0, 0, 0) };
        }
        if (second as i64) >= 0 {
            let _ = unsafe { syscall(SHARED_BUFFER_UNMAP, buffer, second, 0, 0, 0, 0) };
        }
        let _ = unsafe { syscall(HANDLE_REVOKE, buffer, 0, 0, 0, 0, 0) };
        return;
    }
    let value = 0x_a64f_u64;
    unsafe {
        core::ptr::write_volatile(first as *mut u64, value);
    }
    let observed = unsafe { core::ptr::read_volatile(second as *const u64) };
    let child_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    let fork_shared = if child_pid == 0 {
        unsafe { core::ptr::write_volatile(second as *mut u64, value.wrapping_add(1)) };
        let _ = unsafe { syscall(EXIT, 0, 0, 0, 0, 0, 0) };
        false
    } else if (child_pid as i64) > 0 {
        let child_status = unsafe { syscall(WAIT, child_pid, 0, 0, 0, 0, 0) };
        let after_fork = unsafe { core::ptr::read_volatile(first as *const u64) };
        child_status == 0 && after_fork == value.wrapping_add(1)
    } else {
        false
    };
    let first_unmapped = unsafe { syscall(SHARED_BUFFER_UNMAP, buffer, first, 0, 0, 0, 0) };
    let second_unmapped = unsafe { syscall(SHARED_BUFFER_UNMAP, buffer, second, 0, 0, 0, 0) };
    let revoked = unsafe { syscall(HANDLE_REVOKE, buffer, 0, 0, 0, 0, 0) };
    if observed == value
        && fork_shared
        && first_unmapped == 0
        && second_unmapped == 0
        && revoked == 0
    {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                SHARED_BUFFER_MESSAGE.as_ptr() as u64,
                SHARED_BUFFER_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn channel_once() {
    let channel = unsafe { syscall(CHANNEL_CREATE, 0, 0, 0, 0, 0, 0) };
    if (channel as i64) < 0 {
        return;
    }
    let duplicate = unsafe { syscall(HANDLE_DUPLICATE, channel, 0, 0, 0, 0, 0) };
    if (duplicate as i64) < 0 {
        let _ = unsafe { syscall(CLOSE, channel, 0, 0, 0, 0, 0) };
        return;
    }
    let sent = unsafe {
        syscall(
            CHANNEL_SEND,
            duplicate,
            CHANNEL_MESSAGE.as_ptr() as u64,
            CHANNEL_MESSAGE.len() as u64,
            0,
            0,
            0,
        )
    };
    let mut buffer = [0u8; 32];
    let received = unsafe {
        syscall(
            CHANNEL_RECV,
            channel,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
            0,
            0,
        )
    };
    let _ = unsafe { syscall(CLOSE, duplicate, 0, 0, 0, 0, 0) };
    let _ = unsafe { syscall(CLOSE, channel, 0, 0, 0, 0, 0) };
    if sent == CHANNEL_MESSAGE.len() as u64 && received == sent {
        let _ = unsafe { syscall(WRITE, 1, buffer.as_ptr() as u64, received, 0, 0, 0) };
    }
}

fn time_once() {
    let mut before = [0u8; 8];
    let uptime_before = unsafe { syscall(UPTIME, before.as_mut_ptr() as u64, 0, 0, 0, 0, 0) };
    let mut clock = [0u8; 16];
    let clock_result = unsafe { syscall(CLOCK_GETTIME, 0, clock.as_mut_ptr() as u64, 0, 0, 0, 0) };

    // Keep one runnable peer alive while PID 1 sleeps. This makes the probe
    // exercise the scheduler/timer wake path instead of the single-task
    // fallback used when there is nobody to switch to.
    let helper_pid = unsafe { syscall(FORK, 0, 0, 0, 0, 0, 0) };
    if helper_pid == 0 {
        loop {
            let _ = unsafe { syscall(YIELD, 0, 0, 0, 0, 0, 0) };
        }
    }
    if (helper_pid as i64) < 0 {
        return;
    }
    let control = unsafe { syscall(OPEN_PROCESS_CONTROL, helper_pid, 0, 0, 0, 0, 0) };
    if control & (1 << 63) == 0 {
        return;
    }
    let sleep_result = unsafe { syscall(SLEEP, 2_000, 0, 0, 0, 0, 0) };
    let mut after = [0u8; 8];
    let uptime_after = unsafe { syscall(UPTIME, after.as_mut_ptr() as u64, 0, 0, 0, 0, 0) };
    let stop_result = unsafe { syscall(PROCESS_CONTROL_STOP, control, 0, 0, 0, 0, 0) };
    let reap_result = unsafe { syscall(PROCESS_CONTROL_REAP, control, 0, 0, 0, 0, 0) };
    let _ = unsafe { syscall(CLOSE, control, 0, 0, 0, 0, 0) };
    if uptime_before == 0
        && clock_result == 0
        && sleep_result == 0
        && uptime_after == 0
        && stop_result == 0
        && reap_result == 0
        && before != after
        && clock.iter().any(|&byte| byte != 0)
    {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                TIME_MESSAGE.as_ptr() as u64,
                TIME_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn timer_once() {
    let event = unsafe { syscall(CREATE_EVENT, 0, 0, 0, 0, 0, 0) };
    if (event as i64) < 0 {
        return;
    }
    let mut uptime = [0u8; 8];
    let uptime_result = unsafe { syscall(UPTIME, uptime.as_mut_ptr() as u64, 0, 0, 0, 0, 0) };
    let now_us = u64::from_ne_bytes(uptime);
    let deadline_ns = now_us.saturating_add(5_000).saturating_mul(1_000);
    let timer = unsafe { syscall(TIMER_CREATE, 0, deadline_ns, event, 0, 0, 0) };
    let sleep_result = unsafe { syscall(SLEEP, 10_000, 0, 0, 0, 0, 0) };
    let wait_result = unsafe { syscall(WAIT_EVENT, event, 1_000_000, 0, 0, 0, 0) };
    let close_timer = unsafe { syscall(CLOSE, timer, 0, 0, 0, 0, 0) };
    let close_event = unsafe { syscall(CLOSE, event, 0, 0, 0, 0, 0) };
    if uptime_result == 0
        && (timer as i64) >= 0
        && sleep_result == 0
        && wait_result == 0
        && close_timer == 0
        && close_event == 0
    {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                TIMER_MESSAGE.as_ptr() as u64,
                TIMER_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn process_control_once() {
    let child_pid = spawn_child_once();
    if (child_pid as i64) <= 0 {
        return;
    }
    let control = unsafe { syscall(OPEN_PROCESS_CONTROL, child_pid, 0, 0, 0, 0, 0) };
    if control & (1 << 63) == 0 {
        return;
    }
    let duplicate = unsafe { syscall(HANDLE_DUPLICATE, control, 0, 0, 0, 0, 0) };
    let closed = unsafe { syscall(CLOSE, control, 0, 0, 0, 0, 0) };
    let closed_status = unsafe { syscall(PROCESS_CONTROL_STATUS, control, 0, 0, 0, 0, 0) };
    // Transfer to PID 1 exercises the same-process move path while keeping
    // the child alive long enough to validate the replacement token.
    let transferred = unsafe { syscall(HANDLE_TRANSFER, 1, duplicate, 0, 0, 0, 0) };
    let assigned = unsafe { syscall(PROCESS_CONTROL_ASSIGN, transferred, 1, 0, 0, 0, 0) };
    let stopped = unsafe { syscall(PROCESS_CONTROL_STOP, transferred, 37, 0, 0, 0, 0) };
    let state = unsafe { syscall(PROCESS_CONTROL_STATUS, transferred, 0, 0, 0, 0, 0) };
    let status = unsafe { syscall(PROCESS_CONTROL_REAP, transferred, 0, 0, 0, 0, 0) };
    let revoked = unsafe { syscall(HANDLE_REVOKE, transferred, 0, 0, 0, 0, 0) };
    let revoked_status = unsafe { syscall(PROCESS_CONTROL_STATUS, transferred, 0, 0, 0, 0, 0) };
    if duplicate & (1 << 63) != 0
        && closed == 0
        && (closed_status as i64) < 0
        && transferred & (1 << 63) != 0
        && assigned == 0
        && stopped == 0
        && state == 3
        && status == 37
        && revoked == 0
        && (revoked_status as i64) < 0
    {
        let _ = unsafe {
            syscall(
                WRITE,
                1,
                PROCESS_CONTROL_MESSAGE.as_ptr() as u64,
                PROCESS_CONTROL_MESSAGE.len() as u64,
                0,
                0,
                0,
            )
        };
    }
}

fn reap_spawned_child(child_pid: u64) {
    if (child_pid as i64) > 0 {
        let control = unsafe { syscall(OPEN_PROCESS_CONTROL, child_pid, 0, 0, 0, 0, 0) };
        let _ = unsafe { syscall(YIELD, 0, 0, 0, 0, 0, 0) };
        let _child_state = unsafe { syscall(PROCESS_CONTROL_STATUS, control, 0, 0, 0, 0, 0) };
        let _child_status = unsafe { syscall(PROCESS_CONTROL_REAP, control, 0, 0, 0, 0, 0) };
        let orphan_status = unsafe { syscall(WAIT, u64::MAX, 0, 0, 0, 0, 0) };
        if (orphan_status as i64) >= 0 {
            let _ = unsafe {
                syscall(
                    WRITE,
                    1,
                    ORPHAN_REAP_MESSAGE.as_ptr() as u64,
                    ORPHAN_REAP_MESSAGE.len() as u64,
                    0,
                    0,
                    0,
                )
            };
        }
    }
}

unsafe fn syscall(
    number: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> u64 {
    let mut result = arg0;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") result,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x4") arg4,
            in("x5") arg5,
            in("x8") number,
            options(nostack)
        );
    }
    result
}
