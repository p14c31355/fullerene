//! ESP32 Bellows handoff.

#[unsafe(no_mangle)]
pub extern "C" fn esp32_bellows_start() -> ! {
    fullerene_kernel::arch::xtensa::esp32::rust_entry()
}
