#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

// Module declarations
pub mod boot;
pub mod context_switch;
pub mod fs;
pub mod gdt;
pub mod graphics;
pub mod hardware;
pub mod heap;
pub mod init;
pub mod interrupts;
pub mod keyboard;
pub mod loader;
pub mod memory;
pub mod memory_management;
pub mod process;
pub mod scheduler;
pub mod shell;
pub mod syscall;
pub mod vga;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    use petroleum::println;
    let mut writer = petroleum::serial::SERIAL_PORT_WRITER.lock();
    let _ = write!(writer, "Kernel Panic: {}\n", info);
    println!("Kernel Panic: {}", info);
    loop {}
}
