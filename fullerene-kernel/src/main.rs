#![no_std]
#![no_main]

extern crate alloc;

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
