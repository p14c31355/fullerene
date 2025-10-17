//! System scheduler module for Fullerene OS
//!
//! This module provides the main kernel scheduler that orchestrates all system functionality,
//! including process scheduling, shell execution, and system-wide orchestration.

use x86_64::VirtAddr;


/// Main kernel scheduler loop - orchestrates all system functionality
pub fn scheduler_loop() -> ! {
    use x86_64::instructions::hlt;

    // Initialize scheduler by creating the shell process
    log::info!("Initializing shell process...");
    let shell_pid = crate::process::create_process(
        "shell_process",
        VirtAddr::new(shell_process_main as usize as u64),
    );
    log::info!("Created shell process with PID {}", shell_pid);

    // Set initial process as shell
    let _ = crate::process::unblock_process(shell_pid);

    // Main scheduler loop - continuously execute processes
    loop {
        // Yield to allow scheduler to run
        crate::syscall::kernel_syscall(22, 0, 0, 0); // Yield syscall

        // Allow graphics and I/O operations to process
        // This loop will be interrupted by timer and maintain process scheduling

        // Yield for short periods to avoid high CPU usage
        for _ in 0..100 {
            hlt();
        }
    }
}

/// Shell process main function
pub extern "C" fn shell_process_main() -> ! {
    log::info!("Shell process started");

    // Run the shell main loop
    crate::shell::shell_main();

    // If shell exits, terminate the process
    crate::process::terminate_process(
        crate::process::current_pid().unwrap(),
        0
    );

    // Should never reach here
    loop {
        x86_64::instructions::hlt();
    }
}
