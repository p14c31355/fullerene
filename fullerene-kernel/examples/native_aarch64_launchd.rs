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
const PIPE_CREATE: u64 = 83;
const HANDLE_DUPLICATE: u64 = 91;
const OPEN_PROCESS_CONTROL: u64 = 110;
const PROCESS_CONTROL_STATUS: u64 = 112;
const PROCESS_CONTROL_REAP: u64 = 113;

static LAUNCH_MESSAGE: &[u8] = b"launchd: spawning child\n";
static REPEAT_MESSAGE: &[u8] = b"launchd: reusing child slot\n";
static EXEC_MESSAGE: &[u8] = b"launchd: exec child\n";
static EXEC_PATH_MESSAGE: &[u8] = b"launchd: exec path child\n";
static INHERIT_FD_MESSAGE: &[u8] = b"launchd: inherited fd child\n";
static DYNAMIC_MESSAGE: &[u8] = b"launchd: dynamic mapping\n";
static COW_CHILD_MESSAGE: &[u8] = b"launchd: cow child\n";
static FORK_MESSAGE: &[u8] = b"launchd: fork child\n";
static ORPHAN_REAP_MESSAGE: &[u8] = b"launchd: reaped adopted child\n";
static PIPE_MESSAGE: &[u8] = b"AArch64 pipe ok\n";
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
    pipe_once();
    write_file_once();
    let _ = unsafe { syscall(EXIT, 0, 0, 0, 0, 0, 0) };
    loop {
        unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
    }
}

fn spawn_child_once() -> u64 {
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
                    0,
                    0,
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
