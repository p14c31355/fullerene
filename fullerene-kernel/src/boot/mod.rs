//! Boot module containing UEFI and BIOS entry points and boot-specific logic

// Submodules for boot functionality
pub mod bios_entry;
pub mod uefi_entry;

// Macro for common print pattern in kernel/shell code
#[macro_export]
macro_rules! kernel_print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        if let Some(writer) = $crate::graphics::WRITER_UEFI.get() {
            let mut writer = writer.lock();
            let _ = write!(writer, $($arg)*);
        }
    }};
}

#[macro_export]
macro_rules! kernel_println {
    () => ($crate::kernel_print!("\n"));
    ($($arg:tt)*) => ($crate::kernel_print!("{}\n", format_args!($($arg)*)));
}
