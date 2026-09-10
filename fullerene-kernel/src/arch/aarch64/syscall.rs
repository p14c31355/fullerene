//! AArch64 native syscall entry for the first shared-runtime boundary.
//!
//! The generic x86_64 dispatcher cannot be reused here: it is coupled to the
//! x86 scheduler, user-copy implementation, and `syscall` instruction ABI.
//! Keep this small dispatcher on the typed AArch64 trap frame until those
//! services are moved behind architecture-neutral traits.

use fullerene_abi::SyscallNumber;

use super::{
    allocator, devices, exceptions::Aarch64TrapFrame, fs, linux, task, timer, uart, user_memory,
    window,
};

const ERR_NOT_SUPPORTED: u64 = (-(95i64)) as u64;
const ERR_ADDRESS: u64 = (-(14i64)) as u64;
const ERR_INVALID: u64 = (-(22i64)) as u64;
const ERR_OVERFLOW: u64 = (-(75i64)) as u64;
const MAX_WRITE: usize = 4096;
const MAX_SPAWN_IMAGE: usize = 4 * 1024 * 1024;
const MAX_TASK_NAME: usize = 16;
const MAX_EXEC_ARGUMENTS: usize = 8;
const MAX_EXEC_STRING: usize = 128;
const STACK_ADDRESS: u64 = 0x47ff_0000;
const PAGE_SIZE: u64 = 4096;

#[derive(Clone, Copy)]
struct ExecString {
    bytes: [u8; MAX_EXEC_STRING],
    length: usize,
}

impl ExecString {
    const EMPTY: Self = Self {
        bytes: [0; MAX_EXEC_STRING],
        length: 0,
    };
}

// The first AArch64 allocator is intentionally a bump allocator, so syscall
// staging must not leak one heap allocation per spawn/exec. The scheduler is
// single-core in this bounded port; a later SMP process layer can replace this
// with per-CPU or owned process buffers.
static mut IMAGE_STAGING: [u8; MAX_SPAWN_IMAGE] = [0; MAX_SPAWN_IMAGE];

/// Dispatch one user-origin SVC and leave its return value in x0.
pub(super) fn dispatch(frame: &mut Aarch64TrapFrame) -> bool {
    if task::current_personality() == task::AbiPersonality::LinuxAarch64 {
        return linux::dispatch(frame);
    }
    let number = frame.x[8];
    let result = match SyscallNumber::try_from(number) {
        Ok(SyscallNumber::AbiQuery) if frame.x[0] == 0 && frame.x[1] == 0 => {
            fullerene_abi::AbiVersion::CURRENT.pack()
        }
        Ok(SyscallNumber::MapMemory) => syscall_map_memory(frame),
        Ok(SyscallNumber::UnmapMemory) => syscall_unmap_memory(frame),
        Ok(SyscallNumber::ProtectMemory) => syscall_protect_memory(frame),
        Ok(SyscallNumber::QueryMemory) => syscall_query_memory(frame),
        Ok(SyscallNumber::SharedBufferCreate) => fs::shared_buffer_create(frame.x[0], frame.x[1]),
        Ok(SyscallNumber::SharedBufferMap) => {
            fs::shared_buffer_map(frame.x[0], frame.x[1], frame.x[2])
        }
        Ok(SyscallNumber::SharedBufferUnmap) => fs::shared_buffer_unmap(frame.x[0], frame.x[1]),
        Ok(SyscallNumber::Fork) => syscall_fork(frame),
        Ok(SyscallNumber::Read) => fs::read(frame.x[0], frame.x[1], frame.x[2]),
        Ok(SyscallNumber::Open) => fs::open(frame.x[0], frame.x[1], frame.x[2]),
        Ok(SyscallNumber::Close) => {
            if task::is_process_control_handle(frame.x[0]) {
                task::close_process_control(frame.x[0]).unwrap_or_else(|error| error)
            } else {
                fs::close(frame.x[0])
            }
        }
        Ok(SyscallNumber::ChannelCreate) => fs::channel_create(frame.x[0]),
        Ok(SyscallNumber::ChannelSend) => fs::channel_send(frame.x[0], frame.x[1], frame.x[2]),
        Ok(SyscallNumber::ChannelRecv) => fs::channel_recv(frame.x[0], frame.x[1], frame.x[2]),
        Ok(SyscallNumber::PipeCreate) => fs::pipe_create(frame.x[0]),
        Ok(SyscallNumber::HandleDuplicate) => {
            if task::is_process_control_handle(frame.x[0]) {
                task::duplicate_process_control(frame.x[0]).unwrap_or_else(|error| error)
            } else {
                fs::duplicate(frame.x[0])
            }
        }
        Ok(SyscallNumber::HandleTransfer) => {
            if task::is_process_control_handle(frame.x[1]) {
                task::transfer_process_control(frame.x[0], frame.x[1]).unwrap_or_else(|error| error)
            } else {
                fs::transfer(frame.x[0], frame.x[1])
            }
        }
        Ok(SyscallNumber::HandleRevoke) => {
            if task::is_process_control_handle(frame.x[0]) {
                task::revoke_process_control(frame.x[0]).unwrap_or_else(|error| error)
            } else {
                fs::revoke(frame.x[0])
            }
        }
        Ok(SyscallNumber::CreateEvent) => fs::event_create(frame.x[0]),
        Ok(SyscallNumber::SignalEvent) => fs::event_signal(frame.x[0]),
        Ok(SyscallNumber::SubscribeEvent) => fs::event_subscribe(frame.x[0], frame.x[1]),
        Ok(SyscallNumber::WaitEvent) => return fs::event_wait(frame.x[0], frame.x[1], frame),
        Ok(SyscallNumber::Wait) => return syscall_wait(frame),
        Ok(SyscallNumber::CreateThread) => {
            task::create_thread(frame, frame.x[0], frame.x[1]).unwrap_or_else(|error| error)
        }
        Ok(SyscallNumber::JoinThread) => return fs::thread_join(frame.x[0], frame),
        Ok(SyscallNumber::DetachThread) => fs::thread_detach(frame.x[0]),
        Ok(SyscallNumber::OpenProcessControl) => {
            task::open_process_control(frame.x[0]).unwrap_or_else(|error| error)
        }
        Ok(SyscallNumber::ProcessControlStop) => {
            task::process_control_stop(frame.x[0], frame.x[1] as u32 as u64)
                .unwrap_or_else(|error| error)
        }
        Ok(SyscallNumber::ProcessControlStatus) => {
            task::process_control_status(frame.x[0]).unwrap_or_else(|error| error)
        }
        Ok(SyscallNumber::ProcessControlReap) => syscall_process_control_reap(frame),
        Ok(SyscallNumber::ProcessControlAssign) => {
            task::process_control_assign(frame.x[0], frame.x[1]).unwrap_or_else(|error| error)
        }
        Ok(SyscallNumber::GetPid) => task::current_pid().unwrap_or(ERR_NOT_SUPPORTED),
        Ok(SyscallNumber::GetParentPid) => task::current_parent_pid().unwrap_or(ERR_NOT_SUPPORTED),
        Ok(SyscallNumber::GetProcessName) => {
            let mut name = [0u8; 16];
            let name_length = task::current_name(&mut name);
            let requested = usize::try_from(frame.x[1]).unwrap_or(usize::MAX);
            let copy_length = name_length.min(requested);
            if user_memory::copy_to_user(frame.x[0], &name[..copy_length]).is_err() {
                ERR_ADDRESS
            } else {
                copy_length as u64
            }
        }
        Ok(SyscallNumber::Write) => syscall_write(frame),
        Ok(SyscallNumber::Spawn) => syscall_spawn(frame),
        Ok(SyscallNumber::Exec) => syscall_exec(frame),
        Ok(SyscallNumber::ExecPath) => syscall_exec_path(frame),
        Ok(SyscallNumber::CreateTerminal) => fs::create_terminal(frame.x[0], frame.x[1]),
        Ok(SyscallNumber::EnumerateDevices) => {
            devices::enumerate(frame.x[0], frame.x[1], frame.x[2])
        }
        Ok(SyscallNumber::OpenDevice) => devices::open(frame.x[0]),
        Ok(SyscallNumber::DeviceIoctl) => devices::ioctl(frame.x[0], frame.x[1], frame.x[2]),
        Ok(SyscallNumber::CreateWindow) => window::create(
            frame.x[0] as i32,
            frame.x[1] as i32,
            frame.x[2] as u32,
            frame.x[3] as u32,
            frame.x[4],
        ),
        Ok(SyscallNumber::DestroyWindow) => window::destroy(frame.x[0]),
        Ok(SyscallNumber::ResizeWindow) => {
            window::resize(frame.x[0], frame.x[1] as u32, frame.x[2] as u32)
        }
        Ok(SyscallNumber::PresentWindow) => window::present(frame.x[0]),
        Ok(SyscallNumber::GetWindowEvent) => window::get_event(frame.x[0], frame.x[1], frame.x[2]),
        Ok(SyscallNumber::Yield) => return task::yield_syscall(frame),
        Ok(SyscallNumber::ClockGetTime) => syscall_clock_gettime(frame),
        Ok(SyscallNumber::TimerCreate) => fs::timer_create(frame.x[0], frame.x[1], frame.x[2]),
        Ok(SyscallNumber::Sleep) => return syscall_sleep(frame),
        Ok(SyscallNumber::Uptime) => syscall_uptime(frame),
        Ok(SyscallNumber::Exit) => return task::exit_syscall(frame, frame.x[0]),
        Ok(SyscallNumber::ExitThread) => return task::exit_syscall(frame, frame.x[0]),
        Ok(_) | Err(()) => ERR_NOT_SUPPORTED,
    };

    frame.x[0] = result;
    uart::put_hex("aarch64 syscall nr=", number);
    uart::put_hex("aarch64 syscall ret=", result);
    true
}

fn syscall_clock_gettime(frame: &Aarch64TrapFrame) -> u64 {
    let clock_id = frame.x[0];
    let destination = frame.x[1];
    if destination == 0 || !matches!(clock_id, 0 | 1) {
        return ERR_INVALID;
    }
    let microseconds = if clock_id == 0 { timer::uptime_us() } else { 0 };
    let value = fullerene_abi::TimeSpec {
        seconds: microseconds / 1_000_000,
        nanoseconds: (microseconds % 1_000_000) * 1_000,
    };
    if user_memory::copy_to_user(destination, &value.to_ne_bytes()).is_err() {
        ERR_ADDRESS
    } else {
        0
    }
}

fn syscall_sleep(frame: &mut Aarch64TrapFrame) -> bool {
    let duration_us = frame.x[0];
    if duration_us == 0 {
        frame.x[0] = 0;
        return true;
    }

    // Preserve progress if a platform has not enabled its interrupt
    // controller yet; once IRQs are live, park the current task and let the
    // architectural timer wake it. A single runnable task also uses the
    // bounded fallback because blocking it would leave no task to switch to.
    if !timer::irq_ready() || !task::can_block_sleep() {
        timer::delay_us(duration_us);
        fs::fire_timers(timer::uptime_us().saturating_mul(1_000));
        frame.x[0] = 0;
        return true;
    }
    task::sleep_syscall(frame, duration_us)
}

fn syscall_uptime(frame: &Aarch64TrapFrame) -> u64 {
    let destination = frame.x[0];
    if destination == 0 {
        return ERR_INVALID;
    }
    let value = timer::uptime_us().to_ne_bytes();
    if user_memory::copy_to_user(destination, &value).is_err() {
        ERR_ADDRESS
    } else {
        0
    }
}

fn syscall_map_memory(frame: &Aarch64TrapFrame) -> u64 {
    match allocator::with_global(|frames| {
        task::map_memory(frames, frame.x[0], frame.x[1], frame.x[2])
    }) {
        Some(Ok(address)) => address,
        Some(Err(error)) => error,
        None => ERR_NOT_SUPPORTED,
    }
}

fn syscall_unmap_memory(frame: &Aarch64TrapFrame) -> u64 {
    match allocator::with_global(|frames| task::unmap_memory(frames, frame.x[0], frame.x[1])) {
        Some(Ok(result)) => result,
        Some(Err(error)) => error,
        None => ERR_NOT_SUPPORTED,
    }
}

fn syscall_protect_memory(frame: &Aarch64TrapFrame) -> u64 {
    task::protect_memory(frame.x[0], frame.x[1], frame.x[2]).unwrap_or_else(|error| error)
}

fn syscall_query_memory(frame: &Aarch64TrapFrame) -> u64 {
    let info = fullerene_abi::MemoryInfo {
        page_size: 4096,
        ..fullerene_abi::MemoryInfo::default()
    };
    let bytes = info.to_ne_bytes();
    let requested = usize::try_from(frame.x[1]).unwrap_or(usize::MAX);
    if requested < fullerene_abi::MemoryInfo::MIN_BYTE_SIZE
        || user_memory::copy_to_user(frame.x[0], &bytes).is_err()
    {
        ERR_ADDRESS
    } else {
        bytes.len() as u64
    }
}

fn syscall_fork(frame: &mut Aarch64TrapFrame) -> u64 {
    match allocator::with_global(|frames| task::fork(frames, frame)) {
        Some(Ok(pid)) => pid,
        Some(Err(error)) => error,
        None => ERR_NOT_SUPPORTED,
    }
}

fn syscall_wait(frame: &mut Aarch64TrapFrame) -> bool {
    let pid = frame.x[0];
    match allocator::with_global(|frames| task::wait_syscall(frames, frame, pid)) {
        Some(completed) => completed,
        None => {
            frame.x[0] = ERR_NOT_SUPPORTED;
            true
        }
    }
}

fn syscall_process_control_reap(frame: &Aarch64TrapFrame) -> u64 {
    match allocator::with_global(|frames| task::process_control_reap(frames, frame.x[0])) {
        Some(Ok(status)) => status,
        Some(Err(error)) => error,
        None => ERR_NOT_SUPPORTED,
    }
}

fn syscall_write(frame: &Aarch64TrapFrame) -> u64 {
    if !matches!(frame.x[0], 1 | 2) {
        return fs::write(frame.x[0], frame.x[1], frame.x[2]);
    }
    let length = usize::try_from(frame.x[2]).unwrap_or(usize::MAX);
    if length > MAX_WRITE {
        return ERR_OVERFLOW;
    }
    let mut buffer = [0u8; 128];
    let mut offset = 0usize;
    while offset < length {
        let chunk = (length - offset).min(buffer.len());
        let address = match frame.x[1].checked_add(offset as u64) {
            Some(address) => address,
            None => return ERR_ADDRESS,
        };
        if user_memory::copy_from_user(address, &mut buffer[..chunk]).is_err() {
            return ERR_ADDRESS;
        }
        for byte in &buffer[..chunk] {
            uart::putc(*byte);
        }
        offset += chunk;
    }
    length as u64
}

fn syscall_spawn(frame: &Aarch64TrapFrame) -> u64 {
    let image_length = usize::try_from(frame.x[1]).unwrap_or(usize::MAX);
    let name_length = usize::try_from(frame.x[3]).unwrap_or(usize::MAX);
    if image_length == 0 || image_length > MAX_SPAWN_IMAGE || name_length == 0 {
        return ERR_INVALID;
    }
    if name_length > MAX_TASK_NAME {
        return ERR_OVERFLOW;
    }

    let image = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(IMAGE_STAGING).cast::<u8>(),
            image_length,
        )
    };
    if user_memory::copy_from_user(frame.x[0], image).is_err() {
        return ERR_ADDRESS;
    }
    let mut name = [0u8; MAX_TASK_NAME];
    if user_memory::copy_from_user(frame.x[2], &mut name[..name_length]).is_err() {
        return ERR_ADDRESS;
    }

    match allocator::with_global(|frames| {
        task::spawn(frames, image, &name[..name_length], frame.x[6], frame.x[4])
    }) {
        Some(Ok(pid)) => pid,
        Some(Err(error)) => error,
        None => ERR_NOT_SUPPORTED,
    }
}

fn syscall_exec(frame: &mut Aarch64TrapFrame) -> u64 {
    let image_length = usize::try_from(frame.x[1]).unwrap_or(usize::MAX);
    let name_length = usize::try_from(frame.x[3]).unwrap_or(usize::MAX);
    if image_length == 0 || image_length > MAX_SPAWN_IMAGE || name_length == 0 {
        return ERR_INVALID;
    }
    if name_length > MAX_TASK_NAME {
        return ERR_OVERFLOW;
    }

    let image = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(IMAGE_STAGING).cast::<u8>(),
            image_length,
        )
    };
    if user_memory::copy_from_user(frame.x[0], image).is_err() {
        return ERR_ADDRESS;
    }
    let mut name = [0u8; MAX_TASK_NAME];
    if user_memory::copy_from_user(frame.x[2], &mut name[..name_length]).is_err() {
        return ERR_ADDRESS;
    }

    match allocator::with_global(|frames| task::exec(frames, frame, image, &name[..name_length])) {
        Some(Ok(_)) => 0,
        Some(Err(error)) => error,
        None => ERR_NOT_SUPPORTED,
    }
}

fn syscall_exec_path(frame: &mut Aarch64TrapFrame) -> u64 {
    let mut argv = [ExecString::EMPTY; MAX_EXEC_ARGUMENTS];
    let mut envp = [ExecString::EMPTY; MAX_EXEC_ARGUMENTS];
    let argc = match copy_user_vector(frame.x[1], &mut argv) {
        Ok(count) => count,
        Err(error) => return error,
    };
    let envc = match copy_user_vector(frame.x[2], &mut envp) {
        Ok(count) => count,
        Err(error) => return error,
    };
    let image = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(IMAGE_STAGING).cast::<u8>(),
            MAX_SPAWN_IMAGE,
        )
    };
    let (image_length, name, name_length) = match fs::read_path(frame.x[0], image) {
        Ok(result) => result,
        Err(error) => return error,
    };
    if image_length == 0 {
        return ERR_INVALID;
    }
    let replaced = match allocator::with_global(|frames| {
        task::exec(frames, frame, &image[..image_length], &name[..name_length])
    }) {
        Some(Ok(_)) => true,
        Some(Err(error)) => return error,
        None => return ERR_NOT_SUPPORTED,
    };
    if replaced {
        let stack = match install_exec_stack(frame, &argv[..argc], &envp[..envc]) {
            Ok(stack) => stack,
            Err(error) => {
                let _ = task::exit_syscall(frame, error);
                return error;
            }
        };
        uart::put_hex("aarch64 exec argc=", argc as u64);
        uart::put_hex("aarch64 exec envc=", envc as u64);
        uart::put_hex("aarch64 exec sp=", stack);
    }
    0
}

/// Linux `execve`/`execveat` entry for the AArch64 Linux personality.
///
/// It deliberately reuses the same checked pathname/VFS staging and atomic
/// address-space replacement as the native `EXEC_PATH` syscall, then writes
/// the Linux initial stack shape in the new address space. The loader still
/// accepts only the bounded AArch64 ELF subset, so dynamically linked Android
/// images remain gated by the interpreter/DSO work documented in the Linux
/// boundary module.
pub(super) fn linux_exec_path(
    path_address: u64,
    argv_address: u64,
    envp_address: u64,
    frame: &mut Aarch64TrapFrame,
) -> u64 {
    let mut argv = [ExecString::EMPTY; MAX_EXEC_ARGUMENTS];
    let mut envp = [ExecString::EMPTY; MAX_EXEC_ARGUMENTS];
    let argc = match copy_user_vector(argv_address, &mut argv) {
        Ok(count) if count != 0 => count,
        Ok(_) => return ERR_INVALID,
        Err(error) => return error,
    };
    let envc = match copy_user_vector(envp_address, &mut envp) {
        Ok(count) => count,
        Err(error) => return error,
    };
    let image = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(IMAGE_STAGING).cast::<u8>(),
            MAX_SPAWN_IMAGE,
        )
    };
    let (image_length, name, name_length) = match fs::read_path(path_address, image) {
        Ok(result) => result,
        Err(error) => return error,
    };
    if image_length == 0 {
        return ERR_INVALID;
    }
    let image_info = match allocator::with_global(|frames| {
        task::exec(frames, frame, &image[..image_length], &name[..name_length])
    }) {
        Some(Ok(info)) => info,
        Some(Err(error)) => return error,
        None => return ERR_NOT_SUPPORTED,
    };
    if let Err(error) = install_linux_exec_stack(frame, &argv[..argc], &envp[..envc], &image_info) {
        let _ = task::exit_syscall(frame, error);
        return error;
    }
    let result = fs::linux_close_on_exec();
    if (result as i64) < 0 {
        return result;
    }
    0
}

fn install_linux_exec_stack(
    frame: &mut Aarch64TrapFrame,
    argv: &[ExecString],
    envp: &[ExecString],
    image: &task::ExecImageInfo,
) -> Result<(), u64> {
    let mut cursor = STACK_ADDRESS + PAGE_SIZE;
    let mut argv_addresses = [0u64; MAX_EXEC_ARGUMENTS];
    let mut env_addresses = [0u64; MAX_EXEC_ARGUMENTS];
    for (index, value) in argv.iter().enumerate().rev() {
        cursor = cursor
            .checked_sub((value.length + 1) as u64)
            .ok_or(ERR_OVERFLOW)?;
        user_memory::copy_to_user(cursor, &value.bytes[..value.length]).map_err(|_| ERR_ADDRESS)?;
        user_memory::copy_to_user(cursor + value.length as u64, &[0]).map_err(|_| ERR_ADDRESS)?;
        argv_addresses[index] = cursor;
    }
    for (index, value) in envp.iter().enumerate().rev() {
        cursor = cursor
            .checked_sub((value.length + 1) as u64)
            .ok_or(ERR_OVERFLOW)?;
        user_memory::copy_to_user(cursor, &value.bytes[..value.length]).map_err(|_| ERR_ADDRESS)?;
        user_memory::copy_to_user(cursor + value.length as u64, &[0]).map_err(|_| ERR_ADDRESS)?;
        env_addresses[index] = cursor;
    }
    cursor &= !15;
    let mut words = [0u64; 64];
    let mut word_count = 0usize;
    let mut push = |value: u64| {
        words[word_count] = value;
        word_count += 1;
    };
    push(argv.len() as u64);
    for address in argv_addresses.iter().take(argv.len()).copied() {
        push(address);
    }
    push(0);
    for address in env_addresses.iter().take(envp.len()).copied() {
        push(address);
    }
    push(0);
    push(7); // AT_BASE
    push(image.interpreter_base.unwrap_or(0));
    push(3); // AT_PHDR
    push(image.phdr);
    push(4); // AT_PHENT
    push(image.phent);
    push(5); // AT_PHNUM
    push(image.phnum);
    push(6); // AT_PAGESZ
    push(PAGE_SIZE);
    push(9); // AT_ENTRY
    push(image.entry);
    push(23); // AT_SECURE
    push(0);
    push(31); // AT_EXECFN
    push(argv_addresses[0]);
    push(0); // AT_NULL
    push(0);
    let bytes = word_count
        .checked_mul(core::mem::size_of::<u64>())
        .ok_or(ERR_OVERFLOW)?;
    let stack_pointer = cursor.checked_sub(bytes as u64).ok_or(ERR_OVERFLOW)? & !15;
    let stack_bytes = unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes) };
    user_memory::copy_to_user(stack_pointer, stack_bytes).map_err(|_| ERR_ADDRESS)?;
    if !task::set_current_stack(frame, stack_pointer) {
        return Err(ERR_NOT_SUPPORTED);
    }
    Ok(())
}

fn copy_user_vector(
    address: u64,
    destination: &mut [ExecString; MAX_EXEC_ARGUMENTS],
) -> Result<usize, u64> {
    if address == 0 {
        return Ok(0);
    }
    for index in 0..MAX_EXEC_ARGUMENTS {
        let pointer_address = address
            .checked_add((index * core::mem::size_of::<u64>()) as u64)
            .ok_or(ERR_ADDRESS)?;
        let mut raw = [0u8; core::mem::size_of::<u64>()];
        user_memory::copy_from_user(pointer_address, &mut raw).map_err(|_| ERR_ADDRESS)?;
        let string_address = u64::from_ne_bytes(raw);
        if string_address == 0 {
            return Ok(index);
        }
        let length = copy_user_string(string_address, &mut destination[index].bytes)?;
        destination[index].length = length;
    }
    Err(ERR_OVERFLOW)
}

fn copy_user_string(address: u64, destination: &mut [u8; MAX_EXEC_STRING]) -> Result<usize, u64> {
    if address == 0 {
        return Err(ERR_ADDRESS);
    }
    for offset in 0..MAX_EXEC_STRING {
        let mut byte = [0u8; 1];
        user_memory::copy_from_user(
            address.checked_add(offset as u64).ok_or(ERR_ADDRESS)?,
            &mut byte,
        )
        .map_err(|_| ERR_ADDRESS)?;
        if byte[0] == 0 {
            return Ok(offset);
        }
        destination[offset] = byte[0];
    }
    Err(ERR_OVERFLOW)
}

fn install_exec_stack(
    frame: &mut Aarch64TrapFrame,
    argv: &[ExecString],
    envp: &[ExecString],
) -> Result<u64, u64> {
    let mut cursor = STACK_ADDRESS + PAGE_SIZE - 16;
    let mut argv_addresses = [0u64; MAX_EXEC_ARGUMENTS];
    let mut env_addresses = [0u64; MAX_EXEC_ARGUMENTS];

    for (index, value) in argv.iter().enumerate().rev() {
        cursor = cursor
            .checked_sub((value.length + 1) as u64)
            .ok_or(ERR_OVERFLOW)?;
        user_memory::copy_to_user(cursor, &value.bytes[..value.length]).map_err(|_| ERR_ADDRESS)?;
        user_memory::copy_to_user(cursor + value.length as u64, &[0]).map_err(|_| ERR_ADDRESS)?;
        argv_addresses[index] = cursor;
    }
    for (index, value) in envp.iter().enumerate().rev() {
        cursor = cursor
            .checked_sub((value.length + 1) as u64)
            .ok_or(ERR_OVERFLOW)?;
        user_memory::copy_to_user(cursor, &value.bytes[..value.length]).map_err(|_| ERR_ADDRESS)?;
        user_memory::copy_to_user(cursor + value.length as u64, &[0]).map_err(|_| ERR_ADDRESS)?;
        env_addresses[index] = cursor;
    }

    cursor &= !15;
    let word_count = 1 + argv.len() + 1 + envp.len() + 1;
    let stack_bytes = (word_count * core::mem::size_of::<u64>()) as u64;
    let stack_pointer = cursor.checked_sub(stack_bytes).ok_or(ERR_OVERFLOW)? & !15;
    if stack_pointer < STACK_ADDRESS {
        return Err(ERR_OVERFLOW);
    }

    let mut words = [0u8; (1 + (MAX_EXEC_ARGUMENTS + 1) + (MAX_EXEC_ARGUMENTS + 1)) * 8];
    let mut word_index = 0usize;
    write_stack_word(&mut words, &mut word_index, argv.len() as u64);
    for address in argv_addresses.iter().take(argv.len()).copied() {
        write_stack_word(&mut words, &mut word_index, address);
    }
    write_stack_word(&mut words, &mut word_index, 0);
    for address in env_addresses.iter().take(envp.len()).copied() {
        write_stack_word(&mut words, &mut word_index, address);
    }
    write_stack_word(&mut words, &mut word_index, 0);
    user_memory::copy_to_user(stack_pointer, &words[..word_index * 8]).map_err(|_| ERR_ADDRESS)?;
    if !task::set_current_stack(frame, stack_pointer) {
        return Err(ERR_NOT_SUPPORTED);
    }
    Ok(stack_pointer)
}

fn write_stack_word(buffer: &mut [u8], index: &mut usize, value: u64) {
    let start = *index * core::mem::size_of::<u64>();
    buffer[start..start + 8].copy_from_slice(&value.to_ne_bytes());
    *index += 1;
}
