//! Macro definitions for common patterns across Fullerene OS

use alloc::boxed::Box;

/// Macro for reduce code duplication in command arrays
#[macro_export]
macro_rules! command_args {
    () => {
        &[]
    };
    ($($arg:expr),* $(,)?) => {
        &[$($arg.to_string()),*]
    };
}

/// Enhanced delegate call macro using generic patterns
#[macro_export]
macro_rules! debug_mem_descriptor {
    ($i:expr, $desc:expr) => {{
        mem_debug!("Memory descriptor ");
        $crate::serial::debug_print_hex($i);
        mem_debug!(", type=");
        $crate::serial::debug_print_hex($desc.type_ as usize);
        mem_debug!(", phys_start=");
        $crate::serial::debug_print_hex($desc.physical_start as usize);
        mem_debug!(", virt_start=");
        $crate::serial::debug_print_hex($desc.virtual_start as usize);
        mem_debug!(", pages=");
        $crate::serial::debug_print_hex($desc.number_of_pages as usize);
        mem_debug!("\n");
    }};
}

/// Macro for loop-based page mapping with simplified syntax
#[macro_export]
macro_rules! map_pages_loop {
    ($mapper:expr, $allocator:expr, $base_phys:expr, $base_virt:expr, $num_pages:expr, $flags:expr) => {{
        use x86_64::{PhysAddr, VirtAddr, structures::paging::{Page, PhysFrame, Size4KiB}};
        macro_rules! map_page_with_flush {
            ($map:expr, $pg:expr, $frm:expr, $flgs:expr, $alloc:expr) => {{
                unsafe {
                    $map
                        .map_to($pg, $frm, $flgs, $alloc)
                        .expect("Failed to map page")
                        .flush();
                }
            }};
        }
        for i in 0..$num_pages {
            let phys_addr = PhysAddr::new($base_phys + i * 4096);
            let virt_addr = VirtAddr::new($base_virt + i * 4096);
            let page = Page::<Size4KiB>::containing_address(virt_addr);
            let frame = PhysFrame::<Size4KiB>::containing_address(phys_addr);
            map_page_with_flush!($mapper, page, frame, $flags, $allocator);
        }
    }};
}

/// Macro for creating and mapping pages at higher-half offset
#[macro_export]
macro_rules! map_to_higher_half {
    ($mapper:expr, $allocator:expr, $phys_addr:expr, $num_pages:expr, $flags:expr, $offset:expr) => {{
        use x86_64::VirtAddr;
        let virt_base = $offset + $phys_addr;
        map_pages_loop!($mapper, $allocator, $phys_addr, virt_base, $num_pages, $flags);
    }};
}

/// Macro to reduce repetitive serial debug strings
#[macro_export]
macro_rules! debug_log {
    ($msg:literal) => {{
        unsafe { $crate::write_serial_bytes(0x3F8, 0x3FD, concat!($msg, "\n").as_bytes()); }
    }};
    ($fmt:expr, $($arg:tt)*) => {{
        use alloc::string::ToString;
        let msg = alloc::format!(concat!($fmt, "\n"), $($arg)*);
        unsafe { $crate::write_serial_bytes(0x3F8, 0x3FD, msg.as_bytes()); }
    }};
}

/// Macro for physical to virtual address conversion
#[macro_export]
macro_rules! phys_to_virt {
    ($phys:expr, $offset:expr) => {{
        x86_64::VirtAddr::new($offset.as_u64() + $phys)
    }};
}

/// Macro for checking memory initialization in methods
#[macro_export]
macro_rules! ensure_initialized {
    ($self:expr) => {
        if !$self.initialized {
            return Err($crate::common::logging::SystemError::InternalError);
        }
    };
}

/// Macro for flushing TLB and checking CR3
#[macro_export]
macro_rules! flush_tlb_and_verify {
    () => {{
        x86_64::instructions::tlb::flush_all();
        let (cr3_after, _) = x86_64::registers::control::Cr3::read();
        debug_log!("CR3 verified: {:#x}", cr3_after.start_address().as_u64());
    }};
}

/// Macro for common initialization patterns with cleanup
#[macro_export]
macro_rules! init_with_cleanup {
    ($name:expr, $init:block, $cleanup:block) => {{
        $crate::serial::serial_log(format_args!("Initializing {}\n", $name));
        $init;
        $crate::serial::serial_log(format_args!("{} initialized successfully\n", $name));
        // Store cleanup for later if needed - would be part of an RAII pattern
        || $cleanup
    }};
}

/// Macro for modifying contents protected by a Mutex lock
#[macro_export]
macro_rules! lock_and_modify {
    ($lock:expr, $var:ident, $code:block) => {{
        let mut $var = $lock.lock();
        $code
    }};
}

/// Macro for logging errors with context
#[macro_export]
macro_rules! log_error {
    ($error:expr, $context:expr) => {{
        log::error!("{}: {}", *$error as u64, $context);
    }};
}

/// Macro for reading contents protected by a Mutex lock (returns a copy/clone)
#[macro_export]
macro_rules! lock_and_read {
    ($lock:expr, $var:ident, $val:expr) => {{
        let $var = $lock.lock();
        $val
    }};
}

/// Initialize a component and log the result
///
/// # Examples
/// ```
/// let mut component = SomeComponent::new();
/// init_component!(component, "ComponentName");
/// ```
#[macro_export]
macro_rules! init_component {
    ($component:expr, $name:expr) => {{
        match $component.init() {
            Ok(()) => {
                log::info!(concat!($name, " initialized successfully"));
                Ok(())
            }
            Err(e) => {
                log::error!("Failed to initialize {}: {:?}", $name, e);
                Err(e)
            }
        }
    }};
}

/// Ensure a condition is true, otherwise log an error and return it
///
/// # Examples
/// ```
/// ensure!(ptr.is_some(), SystemError::InvalidArgument);
/// ```
#[macro_export]
macro_rules! ensure {
    ($condition:expr, $error:expr) => {
        if !$condition {
            $crate::log_error!($error, stringify!($condition));
            return Err(*$error);
        }
    };
}

/// Ensure a condition is true with a custom error message
///
/// # Examples
/// ```
/// ensure_with_msg!(ptr.is_some(), SystemError::InvalidArgument, "Pointer must not be null");
/// ```
#[macro_export]
macro_rules! ensure_with_msg {
    ($condition:expr, $error:expr, $msg:expr) => {
        if !$condition {
            $crate::log_error!($error, $msg);
            return Err(*$error);
        }
    };
}

/// Convert an option to a result with error logging
///
/// # Examples
/// ```
/// let value = option_to_result!(some_option, SystemError::NotFound);
/// ```
#[macro_export]
macro_rules! option_to_result {
    ($option:expr, $error:expr) => {
        match $option {
            Some(value) => Ok(value),
            None => {
                $crate::log_error!($error, "Option was None");
                Err(*$error)
            }
        }
    };
}

/// Execute an expression and log if it fails
///
/// # Examples
/// ```
/// let result = try_or_log!(some_fallible_operation(), "Operation failed");
/// ```
#[macro_export]
macro_rules! try_or_log {
    ($expr:expr, $context:expr) => {
        match $expr {
            Ok(value) => value,
            Err(e) => {
                $crate::log_error!(e, $context);
                return Err(e);
            }
        }
    };
}

/// Create a static string slice for use in logging
///
/// # Examples
/// ```
/// const COMPONENT_NAME: &str = static_str!("MemoryManager");
/// ```
#[macro_export]
macro_rules! static_str {
    ($s:expr) => {{
        const S: &str = $s;
        S
    }};
}

/// Memory debugging macro that prints strings and values to serial output
///
/// # Examples
/// ```
/// mem_debug!("Memory descriptor: ");
/// mem_debug!(descriptor.physical_start);
/// ```
#[macro_export]
macro_rules! mem_debug {
    ($msg:literal) => {
        $crate::serial::debug_print_str_to_com1($msg);
    };
    ($value:expr) => {
        $crate::serial::debug_print_hex($value as usize);
    };
}

/// Macro for periodic task execution that checks if enough time has passed since last execution
/// Takes a mutable last tick variable, interval, current tick, and block to execute
///
/// # Examples
/// ```
/// static LAST_CHECK: spin::Mutex<u64> = spin::Mutex::new(0);
/// check_periodic!(LAST_CHECK, 1000, current_tick, {
///     perform_hourly_task();
/// });
/// ```
#[macro_export]
macro_rules! check_periodic {
    ($last_tick:expr, $interval:expr, $current_tick:expr, $block:block) => {{
        let mut last = $last_tick.lock();
        let elapsed = $current_tick - *last;
        if elapsed >= $interval {
            $block;
            *last = $current_tick;
        }
    }};
}

/// Macro for simple periodic task execution based on tick intervals
/// Executes block every 'interval' ticks
///
/// # Examples
/// ```
/// periodic_task!(current_tick, 3000, {
///     log_system_stats();
/// });
/// ```
#[macro_export]
macro_rules! periodic_task {
    ($current_tick:expr, $interval:expr, $block:block) => {
        if $current_tick % $interval == 0 {
            $block;
        }
    };
}

/// Unified print macros using the log crate for consistent logging across all crates
/// Uses log::info! for println! and serial output for print!
#[macro_export]
macro_rules! println {
    () => {
        log::info!("");
    };
    ($($arg:tt)*) => {
        log::info!("{}", format_args!($($arg)*));
    };
}

/// Unified print macro using serial output for direct serial logging
#[macro_export]
macro_rules! print {
    () => {
        $crate::serial::_print(format_args!(""));
    };
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*));
    };
}

/// Enhanced logging macro for common patterns throughout the codebase
/// Provides consistent prefixes and formatting
#[macro_export]
macro_rules! log {
    ($prefix:literal) => {
        $crate::serial::_print(format_args!(concat!($prefix, "\n")));
    };
    ($prefix:literal, $msg:expr) => {
        $crate::serial::_print(format_args!(concat!($prefix, ": {}\n"), $msg));
    };
    ($prefix:literal, $format:expr, $($args:tt)*) => {
        $crate::serial::_print(format_args!(concat!($prefix, ": ", $format, "\n"), $($args)*));
    };
}

/// Common logging macros (note: some may be defined in serial.rs)
#[macro_export]
macro_rules! info_log {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!("[INFO] {}\n", format_args!($($arg)*)));
    };
}

#[macro_export]
macro_rules! error_log {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!("[ERROR] {}\n", format_args!($($arg)*)));
    };
}

#[macro_export]
macro_rules! warn_log {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!("[WARN] {}\n", format_args!($($arg)*)));
    };
}

/// PCI operation helper macros to reduce repetition in PCI handling
#[macro_export]
macro_rules! pci_read_bars {
    ($pci_io_ref:expr, $protocol_ptr:expr, $buf:expr, $count:expr, $offset:expr) => {{
        ($pci_io_ref.pci_read)(
            $protocol_ptr,
            2, // Dword width
            $offset,
            $count,
            $buf.as_mut_ptr() as *mut core::ffi::c_void,
        )
    }};
}

/// Safely extract BAR value and check if memory-mapped
#[macro_export]
macro_rules! extract_bar_info {
    ($bars:expr, $bar_index:expr) => {{
        let bar = $bars[$bar_index] & 0xFFFFFFF0; // Mask off lower 4 bits
        let bar_type = $bars[$bar_index] & 0xF;
        let is_memory = (bar_type & 0x1) == 0;
        (bar, bar_type, is_memory)
    }};
}

/// Macro for framebuffer detection patterns
#[macro_export]
macro_rules! test_framebuffer_mode {
    ($addr:expr, $width:expr, $height:expr, $bpp:expr, $stride:expr) => {{
        let fb_size = ($height * $stride * $bpp / 8) as u64;
        if crate::graphics_alternatives::probe_framebuffer_access($addr, fb_size) {
            info_log!(
                "Detected valid framebuffer: {}x{} @ {:#x}",
                $width,
                $height,
                $addr
            );
            Some($crate::common::FullereneFramebufferConfig {
                address: $addr,
                width: $width,
                height: $height,
                pixel_format:
                    $crate::common::EfiGraphicsPixelFormat::PixelRedGreenBlueReserved8BitPerColor,
                bpp: $bpp,
                stride: $stride,
            })
        } else {
            warn_log!("Framebuffer mode {}x{} invalid", $width, $height);
            None
        }
    }};
}

/// Macro to set a bit field in a u32 word, reducing line count for repetitive bit operations
#[macro_export]
macro_rules! bit_field_set {
    ($field:expr, $mask:expr, $shift:expr, $value:expr) => {
        $field = ($field & !($mask << $shift)) | (($value as u32 & $mask) << $shift);
    };
}

/// Macro to set or clear a single bit based on bool value
#[macro_export]
macro_rules! set_bool_bit {
    ($field:expr, $bit:expr, $value:expr) => {
        if $value {
            $field |= 1 << $bit;
        } else {
            $field &= !(1 << $bit);
        }
    };
}

/// Macro to clear a 2D buffer with a value for trait-based buffers, reducing nested loop code
#[macro_export]
macro_rules! clear_buffer {
    ($buffer:expr, $height:expr, $width:expr, $value:expr) => {
        for row in 0..$height {
            for col in 0..$width {
                $buffer.set_char_at(row, col, $value);
            }
        }
    };
}

/// Macro to scroll up a 2D buffer for trait-based buffers, reducing loop code
#[macro_export]
macro_rules! scroll_buffer_up {
    ($buffer:expr, $height:expr, $width:expr, $blank:expr) => {
        for row in 1..$height {
            for col in 0..$width {
                let chr = $buffer.get_char_at(row, col);
                $buffer.set_char_at(row - 1, col, chr);
            }
        }
        for col in 0..$width {
            $buffer.set_char_at($height - 1, col, $blank);
        }
    };
}

/// Command definition macro to reduce repetitive command array initialization scatter
///
/// # Examples
/// ```
/// define_commands!(CommandEntry,
///     ("help", "Show help", help_fn),
///     ("exit", "Exit", exit_fn)
/// )
/// ```
#[macro_export]
macro_rules! define_commands {
    ($entry_ty:ident, $(($name:expr, $desc:expr, $func:expr)),* $(,)?) => {
        &[
            $(
                $entry_ty {
                    name: $name,
                    description: $desc,
                    function: $func,
                }
            ),*
        ]
    };
}

/// Macro for volatile memory read operations
#[macro_export]
macro_rules! volatile_read {
    ($addr:expr, $ty:ty) => {
        unsafe { core::ptr::read_volatile($addr as *const $ty) }
    };
}

/// Macro for volatile memory write operations
#[macro_export]
macro_rules! volatile_write {
    ($addr:expr, $value:expr) => {{ unsafe { core::ptr::write_volatile($addr, $value) } }};
}

/// Macro for safe buffer index access with bounds checking
#[macro_export]
macro_rules! safe_buffer_access {
    ($buffer:expr, $index:expr, $default:expr) => {
        if $index < $buffer.len() {
            &$buffer[$index]
        } else {
            &$default
        }
    };
}

/// Macro for scrolling up a 2D character buffer (generic version)
#[macro_export]
macro_rules! scroll_char_buffer_up {
    ($buffer:expr, $height:expr, $width:expr, $blank:expr) => {
        for row in 1..$height {
            for col in 0..$width {
                $buffer[row - 1][col] = $buffer[row][col];
            }
        }
        for col in 0..$width {
            $buffer[$height - 1][col] = $blank;
        }
    };
}

/// Macro for generic text buffer operations in write_byte
#[macro_export]
macro_rules! handle_write_byte {
    ($self:expr, $byte:expr, $newline:block, $write_char:block) => {
        match $byte {
            b'\n' => $newline,
            byte => $write_char,
        }
    };
}

/// Macro to reduce boilerplate in error conversion implementations
/// Converts an error type to SystemError using a mapping closure
#[macro_export]
macro_rules! impl_error_from {
    ($src:ty, $dst:ty, $map_fn:expr) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                ($map_fn)(error)
            }
        }
    };
}

/// Compact error conversion macro for common patterns where variants map directly
#[macro_export]
macro_rules! error_variant_map {
    ($src:ty, $dst:ty, $pat:pat => $result:expr) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                match error {
                    $pat => $result,
                }
            }
        }
    };
}

/// Macro for chained error conversions
#[macro_export]
macro_rules! error_chain {
    ($src:ty, $dst:ty, $( $pat:pat => $result:expr ),* $(,)?) => {
        impl From<$src> for $dst {
            fn from(error: $src) -> Self {
                match error {
                    $(
                        $pat => $result,
                    )*
                }
            }
        }
    };
}

/// Macro for simple module initialization with logging
#[macro_export]
macro_rules! declare_init {
    ($mod_name:expr) => {{
        $crate::serial::serial_log(format_args!("{} initialized\n", $mod_name));
    }};
}

/// Macro for initialization steps/done with serial logging
#[macro_export]
macro_rules! init_log {
    ($msg:literal) => {{
        let msg = concat!($msg, "\n");
        $crate::write_serial_bytes!(0x3F8, 0x3FD, msg.as_bytes());
    }};
    ($fmt:expr $(, $($arg:tt)*)?) => {{
        $crate::serial::serial_log(format_args!(concat!($fmt, "\n") $(, $($arg)*)?));
    }};
}

/// Macro to update VGA cursor position by writing to ports
#[macro_export]
macro_rules! update_vga_cursor {
    ($pos:expr) => {{
        port_write!(
            $crate::graphics::ports::HardwarePorts::CRTC_INDEX,
            $crate::graphics::ports::HardwarePorts::CURSOR_POS_LOW_REG
        );
        port_write!(
            $crate::graphics::ports::HardwarePorts::CRTC_DATA,
            (($pos & 0xFFusize) as u8)
        );
        port_write!(
            $crate::graphics::ports::HardwarePorts::CRTC_INDEX,
            $crate::graphics::ports::HardwarePorts::CURSOR_POS_HIGH_REG
        );
        port_write!(
            $crate::graphics::ports::HardwarePorts::CRTC_DATA,
            ((($pos >> 8) & 0xFFusize) as u8)
        );
    }};
}

/// CPU pause instruction for busy-waiting
#[macro_export]
macro_rules! pause {
    () => {
        unsafe {
            core::arch::asm!("pause", options(nomem, nostack, preserves_flags));
        }
    };
}

/// CPU halt instruction (use with caution, can hang)
#[macro_export]
macro_rules! halt {
    () => {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    };
}

pub struct InitSequence<'a> {
    steps: &'a [(&'static str, Box<dyn Fn() -> Result<(), &'static str>>)],
}

/// Macro for getting memory statistics in a single line
#[macro_export]
macro_rules! get_memory_stats {
    () => {{
        let allocator = $crate::page_table::ALLOCATOR.lock();
        let used = allocator.used();
        let total = allocator.size();
        let free = total.saturating_sub(used);
        (used, total, free)
    }};
}

impl<'a> InitSequence<'a> {
    pub fn new(steps: &'a [(&'static str, Box<dyn Fn() -> Result<(), &'static str>>)]) -> Self {
        Self { steps }
    }

    pub fn run(&self) {
        for (name, init_fn) in self.steps {
            init_log!("About to init {}", name);
            if let Err(e) = init_fn() {
                init_log!("Init {} failed: {}", name, e);
                panic!("{}", e);
            }
            init_log!("{} init done", name);
        }
    }
}

/// Macro for creating FullereneFramebufferConfig structs to reduce boilerplate
#[macro_export]
macro_rules! create_framebuffer_config {
    ($address:expr, $width:expr, $height:expr, $pixel_format:expr, $bpp:expr, $stride:expr) => {
        $crate::common::FullereneFramebufferConfig {
            address: $address,
            width: $width,
            height: $height,
            pixel_format: $pixel_format,
            bpp: $bpp,
            stride: $stride,
        }
    };
}
