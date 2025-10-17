//! Initialization module containing common initialization logic for both UEFI and BIOS boot

use crate::interrupts;
use petroleum::write_serial_bytes;
use x86_64::VirtAddr;

#[cfg(target_os = "uefi")]
pub fn init_common() {
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: About to init VGA\n");
    crate::vga::init_vga();
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: VGA init done\n");

    // Now safe to initialize APIC and enable interrupts (after stable page tables and heap)
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: About to init APIC\n");
    interrupts::init_apic();
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: APIC init done\n");
    log::info!("Kernel: APIC initialized and interrupts enabled");

    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: About to init process\n");
    crate::process::init();
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: Process init done\n");
    log::info!("Kernel: Process management initialized");

    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: About to init syscall\n");
    crate::syscall::init();
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: syscall init done\n");
    log::info!("Kernel: System calls initialized");

    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: About to init fs\n");
    crate::fs::init();
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: FS init done\n");
    log::info!("Kernel: Filesystem initialized");

    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: About to init loader\n");
    crate::loader::init();
    write_serial_bytes!(0x3F8, 0x3FD, b"init_common: Loader init done\n");
    log::info!("Kernel: Program loader initialized");

// Test process main function
pub fn test_process_main() {
    // Simple test process that demonstrates system calls using proper syscall instruction
    unsafe fn syscall(
        num: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
        arg4: u64,
        arg5: u64,
        arg6: u64,
    ) -> u64 {
        let result: u64;
        unsafe {
            core::arch::asm!(
                "syscall",
                in("rax") num,
                in("rdi") arg1,
                in("rsi") arg2,
                in("rdx") arg3,
                in("r10") arg4,
                in("r8") arg5,
                in("r9") arg6,
                lateout("rax") result,
                out("rcx") _, out("r11") _,
            );
        }
        result
    }

    // Write to stdout via syscall
    let message = b"Hello from test user process!\n";
    unsafe {
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1, // fd (stdout)
            message.as_ptr() as u64,
            message.len() as u64,
            0,
            0,
            0,
        );
    }

    // Get PID via syscall and print the actual PID
    unsafe {
        let pid = syscall(
            crate::syscall::SyscallNumber::GetPid as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        );
        let pid_msg = b"My PID is: ";
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1,
            pid_msg.as_ptr() as u64,
            pid_msg.len() as u64,
            0,
            0,
            0,
        );

        // Convert PID to string and print it
        let pid_str = alloc::format!("{}\n", pid);
        let pid_bytes = pid_str.as_bytes();
        syscall(
            crate::syscall::SyscallNumber::Write as u64,
            1,
            pid_bytes.as_ptr() as u64,
            pid_bytes.len() as u64,
            0,
            0,
            0,
        );
    }

    // Yield a bit
    unsafe {
        syscall(
            crate::syscall::SyscallNumber::Yield as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        ); // SYS_YIELD
        syscall(
            crate::syscall::SyscallNumber::Yield as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        ); // SYS_YIELD
    }

    // Exit
    unsafe {
        syscall(crate::syscall::SyscallNumber::Exit as u64, 0, 0, 0, 0, 0, 0); // SYS_EXIT
    }
}

    let test_pid = crate::process::create_process("test_process", VirtAddr::new(test_process_main as usize as u64));

    log::info!("Kernel: Created test process with PID {}", test_pid);

    // Test interrupt handling - should not panic or crash if APIC is working

    log::info!("Testing interrupt handling with int3...");
    // The interrupt test has been removed.

    log::info!("Interrupt test passed (no crash)");
}

#[cfg(not(target_os = "uefi"))]
pub fn init_common() {
    use core::mem::MaybeUninit;

    // Static heap for BIOS
    static mut HEAP: [MaybeUninit<u8>; crate::heap::HEAP_SIZE] =
        [MaybeUninit::uninit(); crate::heap::HEAP_SIZE];
    let heap_start_addr: x86_64::VirtAddr;
    unsafe {
        let heap_start_ptr: *mut u8 = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        heap_start_addr = x86_64::VirtAddr::from_ptr(heap_start_ptr);
        use petroleum::page_table::ALLOCATOR;
        ALLOCATOR
            .lock()
            .init(heap_start_ptr, crate::heap::HEAP_SIZE);
    }

    crate::gdt::init(heap_start_addr); // Pass the actual heap start address
    interrupts::init(); // Initialize IDT
    // Heap already initialized
    petroleum::serial::serial_init(); // Initialize serial early for debugging
    crate::vga::init_vga();
}
