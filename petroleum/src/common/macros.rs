//! Macro definitions for common patterns across Fullerene OS

use alloc::boxed::Box;

/// Unified macros to reduce repetitions across the file

#[macro_export]
macro_rules! bitmap_chunk_bit {
    ($frame:expr) => {{
        let chunk_index = $frame / 64;
        let bit_index = $frame % 64;
        (chunk_index, bit_index)
    }};
}



#[macro_export]
macro_rules! map_range_with_log_macro {
    ($($tt:tt)*) => {
        map_with_log_macro!($($tt)*)
    };
}

#[macro_export]
macro_rules! bit_ops {
    (set_field, $field:expr, $mask:expr, $shift:expr, $value:expr) => {
        $field = ($field & !($mask << $shift)) | (($value as u32 & $mask) << $shift);
    };
    (set_bool_bit, $field:expr, $bit:expr, $value:expr) => {
        if $value {
            $field |= 1 << $bit;
        } else {
            $field &= !(1 << $bit);
        }
    };
    (bitmap_set_free, $bitmap:expr, $frame:expr) => {
        if let Some(ref mut bitmap) = $bitmap {
            let (chunk_index, bit_index) = bitmap_chunk_bit!($frame);
            if chunk_index < bitmap.len() {
                bitmap[chunk_index] &= !(1 << bit_index);
            }
        }
    };
    (bitmap_set_used, $bitmap:expr, $frame:expr) => {
        if let Some(ref mut bitmap) = $bitmap {
            let (chunk_index, bit_index) = bitmap_chunk_bit!($frame);
            if chunk_index < bitmap.len() {
                bitmap[chunk_index] |= 1 << bit_index;
            }
        }
    };
    (bitmap_is_free, $bitmap:expr, $frame:expr) => {{
        if let Some(ref bitmap) = $bitmap {
            let (chunk_index, bit_index) = bitmap_chunk_bit!($frame);
            if chunk_index < bitmap.len() {
                (bitmap[chunk_index] & (1 << bit_index)) == 0
            } else {
                false
            }
        } else {
            false
        }
    }};
}

#[macro_export]
macro_rules! buffer_ops {
    (clear_line_range, $buffer:expr, $start_row:expr, $end_row:expr, $col_start:expr, $col_end:expr, $blank_char:expr) => {{
        for row in $start_row..$end_row {
            for col in $col_start..$col_end {
                $buffer.set_char_at(row, col, $blank_char);
            }
        }
    }};
    (clear_buffer, $buffer:expr, $height:expr, $width:expr, $value:expr) => {
        for row in 0..$height {
            for col in 0..$width {
                $buffer.set_char_at(row, col, $value);
            }
        }
    };
    (scroll_char_buffer_up, $buffer:expr, $height:expr, $width:expr, $blank:expr) => {
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

#[macro_export]
macro_rules! unified_logging {
    (mem_debug, $($args:tt)*) => {
        $crate::mem_debug!($($args)*);
    };
    (serial_str, $msg:literal) => {
        $crate::serial::debug_print_str_to_com1($msg);
    };
    (serial_hex, $value:expr) => {
        $crate::serial::debug_print_hex($value);
    };
    (verbose_print, literal, $msg:literal) => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!($msg);
        } else {
            $crate::serial::_print(format_args!("{}\n", $msg));
        }
    };
    (verbose_print, args, $($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!("{}", format_args!($($arg)*));
        } else {
            $crate::serial::_print(format_args!("{}\n", format_args!($($arg)*)));
        }
    };
}

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





#[macro_export]
macro_rules! map_pages {
    ($mapper:expr, $allocator:expr, $phys_base:expr, $virt_calc:expr, $num_pages:expr, $flags:expr, $behavior:tt) => {{
        use x86_64::{
            PhysAddr, VirtAddr,
            structures::paging::{Page, PhysFrame, Size4KiB, mapper::MapToError},
        };
        for i in 0..$num_pages {
            let phys_addr = $phys_base + i * 4096;
            let virt_addr = $virt_calc + i * 4096;
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt_addr));
            let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys_addr));
            unsafe {
                match $mapper.map_to(page, frame, $flags, $allocator) {
                    Ok(flush) => flush.flush(),
                    Err(MapToError::PageAlreadyMapped(_)) => {},
                    Err(e) => match $behavior {
                        "continue" => continue,
                        "panic" => panic!("Mapping error: {:?}", e),
                        _ => {},
                    }
                }
            }
        }
    }};
}

/// Non-allocating debug log macro for early boot code
///
/// Supports limited formatting for numeric types without heap allocation.
/// Use during early boot before heap initialization.
///
/// # Examples
/// ```
/// debug_log_no_alloc!("Starting initialization");
/// debug_log_no_alloc!(42);  // Just print a value
/// debug_log_no_alloc!("Log with values: ", 42, 0x1234);
/// debug_log_no_alloc!("Heap: Status: ", "Success");  // Literal + string concatenation
/// ```
#[macro_export]
macro_rules! debug_log_no_alloc {
    ($msg:literal) => {{
        $crate::write_serial_bytes!(0x3F8, 0x3FD, concat!($msg, "\n").as_bytes());
    }};
    ($value:expr) => {{
        $crate::serial::debug_print_hex($value);
        $crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    }};
    ($msg:literal, $($value:expr),* $(,)?) => {{
        $crate::write_serial_bytes!(0x3F8, 0x3FD, $msg.as_bytes());
        $(
            $crate::serial::debug_print_hex($value);
        )*
        $crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
    }};
    ($prefix:literal, $string_var:expr) => {{
        $crate::write_serial_bytes!(0x3F8, 0x3FD, $prefix.as_bytes());
        $crate::serial::debug_print_str_to_com1($string_var);
        $crate::write_serial_bytes!(0x3F8, 0x3FD, b"\n");
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

/// Macro for modifying contents protected by a Mutex lock
#[macro_export]
macro_rules! lock_and_modify {
    ($lock:expr, $var:ident, $code:block) => {{
        let mut $var = $lock.lock();
        $code
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

/// Unified logging macro for scheduler operations to reduce repetition
///
/// # Examples
/// ```
/// scheduler_log!("Starting process scheduling");
/// scheduler_log!("Found process at index {}", index);
/// ```
#[macro_export]
macro_rules! scheduler_log {
    ($msg:literal) => {
        log::info!("Scheduler: {}", $msg);
    };
    ($msg:literal, $($arg:tt)*) => {
        log::info!("Scheduler: {}", format_args!($msg, $($arg)*));
    };
}

/// Macro for bootloader initialization steps with consistent logging and error handling
/// Reduces boilerplate in bootloader code while providing debug output
///
/// # Examples
/// ```
/// init_boot_step!("Initializing heap", || init_heap(service));
/// init_boot_step!("Loading kernel", || load_kernel());
/// ```
#[macro_export]
macro_rules! init_boot_step {
    ($step_name:expr, $init_fn:expr) => {{
        $crate::println!($step_name);
        $crate::serial::_print(format_args!("{} \n", $step_name));
        $init_fn.expect(concat!("Bootloader initialization failed at: ", $step_name))
    }};
}

/// Helper for init_step in sequences
#[macro_export]
macro_rules! init_step {
    ($name:expr, $closure:expr) => {
        (
            $name,
            Box::new($closure) as Box<dyn Fn() -> Result<(), &'static str>>,
        )
    };
}

/// Macro for reading unaligned data from memory with offset
#[macro_export]
macro_rules! read_unaligned {
    ($ptr:expr, $offset:expr, $ty:ty) => {
        unsafe { core::ptr::read_unaligned(($ptr as *const u8).add($offset) as *const $ty) }
    };
}



/// Enhanced memory debugging macro that supports formatted output with mixed strings and values
///
/// # Examples
/// ```
/// mem_debug!("Memory descriptor: ");
/// mem_debug!(descriptor.physical_start);
/// mem_debug!("type=", descriptor.type_, ", pages=", descriptor.number_of_pages);
/// ```
#[macro_export]
macro_rules! mem_debug {
    () => {};
    ($value:expr, $($rest:tt)*) => {
        $crate::serial::debug_print_hex($value);
        $crate::mem_debug!($($rest)*);
    };
    ($value:expr) => {
        $crate::serial::debug_print_hex($value);
    };
    ($msg:literal, $($rest:tt)*) => {
        $crate::serial::debug_print_str_to_com1($msg);
        $crate::mem_debug!($($rest)*);
    };
    ($msg:literal) => {
        $crate::serial::debug_print_str_to_com1($msg);
    };
}

/// Debug print macro that handles mixed string and hex value output line by line
///
/// # Examples
/// ```
/// debug_print!("Total map size: ");
/// debug_print!(total_map_size);
/// debug_print!(", config size: ");
/// debug_print!(config_size);
/// debug_print!("\n");
/// ```
#[macro_export]
macro_rules! debug_print {
    ($msg:literal) => {
        $crate::serial::debug_print_str_to_com1($msg);
    };
    ($value:expr) => {
        $crate::serial::debug_print_hex($value);
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
        if $current_tick - *last >= $interval {
            *last = $current_tick;
            $block;
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

/// Macro for defining health check functions with consistent logging and thresholds
/// Reduces boilerplate in system monitoring by providing a unified interface
#[macro_export]
macro_rules! health_check {
    ($fn_name:ident, $threshold_expr:expr, $log_level:ident, $msg:expr, $body:block) => {
        fn $fn_name() {
            if $threshold_expr {
                log::$log_level!($msg);
                $body
            }
        }
    };
}

/// Macro for drawing filled rectangles on framebuffer writers
/// Reduces repetition in desktop rendering operations
#[macro_export]
macro_rules! draw_filled_rect {
    ($writer:expr, $x:expr, $y:expr, $w:expr, $h:expr, $color:expr) => {{
        for y_coord in ($y as i32)..(($y as i32) + ($h as i32)) {
            for x_coord in ($x as i32)..(($x as i32) + ($w as i32)) {
                $writer.put_pixel(x_coord as u32, y_coord as u32, $color);
            }
        }
    }};
}

/// Macro for periodic VGA stat display to reduce code duplication in scheduler
/// Automatically handles cursor positioning and line clearing
#[macro_export]
macro_rules! vga_stat_display {
    ($vga_buffer:expr, $stats:expr, $current_tick:expr, $interval_ticks:expr, $start_row:expr, $($display_line:tt)*) => {{
        static LAST_DISPLAY_TICK: spin::Mutex<u64> = spin::Mutex::new(0);
        petroleum::check_periodic!(LAST_DISPLAY_TICK, $interval_ticks, $current_tick, {
            petroleum::vga_stat_display_impl!($vga_buffer, $start_row, $($display_line)*);
        });
    }};
}

/// Helper macro for implementing VGA stat display logic
#[macro_export]
macro_rules! vga_stat_display_impl {
    ($vga_buffer:expr, $start_row:expr, $($display_line:tt)*) => {{
        if let Some(vga_buffer) = $vga_buffer.get() {
            let mut vga_writer = vga_buffer.lock();
            let blank_char = ScreenChar {
                ascii_character: b' ',
                color_code: ColorCode::new(Color::Black, Color::Black),
            };
            petroleum::clear_line_range!(vga_writer, $start_row, $start_row + 3, 0, 80, blank_char);
            vga_writer.set_position($start_row, 0);
            use core::fmt::Write;
            vga_writer.set_color_code(ColorCode::new(Color::Cyan, Color::Black));
            $(
                vga_stat_display_line!(vga_writer, $display_line);
            )*
            vga_writer.update_cursor();
        }
    }};
}

/// Helper macro for vga_stat_display to process each display line
#[macro_export]
macro_rules! vga_stat_display_line {
    ($vga_writer:expr, ($row:expr, $format:expr, $($args:tt)*)) => {{
        $vga_writer.set_position($row, 0);
        let _ = write!($vga_writer, $format, $($args)*);
    }};
}



/// Unified periodic stat logging to filesystem with auto-file creation
/// Reduces duplication between different stat logging functions
#[macro_export]
macro_rules! periodic_fs_log {
    ($filename:expr, $interval_ticks:expr, $current_tick:expr, $($stats_expr:tt)*) => {{
        static LAST_LOG_TICK: spin::Mutex<u64> = spin::Mutex::new(0);

        petroleum::check_periodic!(LAST_LOG_TICK, $interval_ticks, $current_tick, {
            let log_content = alloc::format!($($stats_expr)*);
            log::info!("{}", log_content);
        });
    }};
}

/// Macro for common system maintenance task scheduling
/// Wraps multiple functions with periodic execution
#[macro_export]
macro_rules! maintenance_tasks {
    ($current_tick:expr, $(($interval:expr, $fn_call:expr)),* $(,)?) => {
        $(
            petroleum::periodic_task!($current_tick, $interval, {
                $fn_call
            });
        )*
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

////////////// Old macros removed to use unified bit_ops! and buffer_ops! above

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

/// Macro for volatile memory operations
#[macro_export]
macro_rules! volatile_ops {
    (read, $addr:expr, $ty:ty) => {
        unsafe { core::ptr::read_volatile($addr as *const $ty) }
    };
    (write, $addr:expr, $val:expr) => {
        unsafe { core::ptr::write_volatile($addr as *mut _, $val) }
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

/// Helper macros for common attributes during bitmap manipulation
/// Reduces line count for repetitive bitmap operation
#[macro_export]
macro_rules! init_vga_palette_registers {
    () => {{
        for i: u8 in 0u8..16u8 {
            $crate::graphics::ports::write_vga_attribute_register(i, i);
        }
    }};
}

/// Macro for setting multiple VGA attribute registers with arbitrary index => value pairs
/// Reduces line count for attribute register initialization
#[macro_export]
macro_rules! set_vga_attribute_registers {
    ($($index:expr => $value:expr),* $(,)?) => {{
        $(
            $crate::graphics::ports::write_vga_attribute_register($index, $value);
        )*
    }};
}

/// Macro for enabling VGA video output after attribute controller setup
/// Reduces boilerplate for video enable sequence
#[macro_export]
macro_rules! enable_vga_video {
    () => {{
        $crate::port_read_u8!(0x3DA);
        $crate::port_write!(0x3C0u16, 0x20u8);
    }};
}

/// Macro for VGA mode 3 (80x25 text mode) setup sequence
/// Reduces repetition in VGA initialization code across crates
#[macro_export]
macro_rules! init_vga_text_mode_3 {
    () => {{
        // Write misc register
        $crate::port_write!($crate::graphics::ports::HardwarePorts::MISC_OUTPUT, 0x67u8);

        // Sequencer registers
        let sequencer_configs = $crate::graphics::registers::SEQUENCER_TEXT_CONFIG;
        let mut sequencer_ops = $crate::graphics::ports::VgaPortOps::new(
            $crate::graphics::ports::HardwarePorts::SEQUENCER_INDEX,
            $crate::graphics::ports::HardwarePorts::SEQUENCER_DATA,
        );
        sequencer_ops.write_sequence(sequencer_configs);

        // CRTC registers for 80x25 text mode
        let crtc_configs = $crate::graphics::registers::CRTC_TEXT_CONFIG;
        let mut crtc_ops = $crate::graphics::ports::VgaPortOps::new(
            $crate::graphics::ports::HardwarePorts::CRTC_INDEX,
            $crate::graphics::ports::HardwarePorts::CRTC_DATA,
        );
        crtc_ops.write_sequence(crtc_configs);

        // Graphics controller
        let graphics_configs = $crate::graphics::registers::GRAPHICS_TEXT_CONFIG;
        let mut graphics_ops = $crate::graphics::ports::VgaPortOps::new(
            $crate::graphics::ports::HardwarePorts::GRAPHICS_INDEX,
            $crate::graphics::ports::HardwarePorts::GRAPHICS_DATA,
        );
        graphics_ops.write_sequence(graphics_configs);

        // Attribute controller
        $crate::init_vga_palette_registers!();
        $crate::set_vga_attribute_registers!(
            0x10 => 0x0C,
            0x11 => 0x00,
            0x12 => 0x0F,
            0x13 => 0x08,
            0x14 => 0x00
        );

        // Enable video output
        $crate::enable_vga_video!();
    }};
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

/// Macro for checking UEFI status and returning error if not success
#[macro_export]
macro_rules! check_uefi_status {
    ($status:expr, $log_msg:expr, $err:expr) => {
        if EfiStatus::from($status) != EfiStatus::Success {
            log::error!($log_msg);
            return Err($err);
        }
    };
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

/// Consolidated logging macro for page table operations
#[macro_export]
macro_rules! log_page_table_op {
    ($operation:literal) => {
        mem_debug!($operation, "\n");
    };
    ($operation:literal, $msg:literal, $addr:expr) => {
        mem_debug!($operation, $msg, " addr=", $addr, "\n");
    };
    ($operation:literal, $phys:expr, $virt:expr, $pages:expr) => {
        mem_debug!(
            $operation,
            " phys=0x",
            $phys,
            " virt=0x",
            $virt,
            " pages=",
            $pages,
            "\n"
        );
    };
    ($stage:literal, $phys:expr, $virt:expr, $pages:expr) => {
        mem_debug!(
            "Memory mapping stage=",
            $stage,
            " phys=0x",
            $phys,
            " virt=0x",
            $virt,
            " pages=",
            $pages,
            "\n"
        );
    };
    ($operation:literal, $msg:literal) => {
        mem_debug!($operation, $msg, "\n");
    };
}

/// Memory descriptor processing macro
#[macro_export]
macro_rules! process_memory_descriptors_safely {
    ($descriptors:expr, $processor:expr) => {{
        for descriptor in $descriptors.iter() {
            if $crate::page_table::efi_memory::is_valid_memory_descriptor(descriptor)
                && descriptor.is_memory_available()
            {
                let start_frame = (descriptor.get_physical_start() / 4096) as usize;
                let end_frame = start_frame.saturating_add(descriptor.get_page_count() as usize);

                if start_frame < end_frame {
                    $processor(descriptor, start_frame, end_frame);
                }
            }
        }
    }};
}

/// Consolidated validation logging macro
#[macro_export]
macro_rules! debug_log_validate_macro {
    ($field:expr, $value:expr) => {
        mem_debug!($field, " validated: ", $value, "\n");
    };
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

/// Page table flags constants macro
#[macro_export]
macro_rules! page_flags_const {
    (READ_WRITE_NO_EXEC) => {
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
    };
    (READ_ONLY) => {
        PageTableFlags::PRESENT
    };
    (READ_WRITE) => {
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
    };
    (READ_EXECUTE) => {
        PageTableFlags::PRESENT
    };
}

/// Integrated identity mapping macro
#[macro_export]
macro_rules! map_identity_range_macro {
    ($mapper:expr, $frame_allocator:expr, $start_addr:expr, $pages:expr, $flags:expr) => {{
        unsafe {
            map_identity_range($mapper, $frame_allocator, $start_addr, $pages, $flags)
        }
    }};
}



//// Identity mapping with detailed logging
#[macro_export]
macro_rules! identity_map_range_with_log_macro {
    ($mapper:expr, $frame_allocator:expr, $start_addr:expr, $num_pages:expr, $flags:expr) => {{
        log_page_table_op!("Identity mapping start", $start_addr, $start_addr, $num_pages);
        let result =
            map_identity_range_macro!($mapper, $frame_allocator, $start_addr, $num_pages, $flags);
        if result.is_ok() {
            log_page_table_op!("Identity mapping complete", $start_addr, $start_addr, $num_pages);
        }
        result
    }};
}

/// Map to higher half with logging
#[macro_export]
macro_rules! map_to_higher_half_with_log_macro {
    ($mapper:expr, $frame_allocator:expr, $phys_offset:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {{
        let virt_start = $phys_offset.as_u64() + $phys_start;
        log_page_table_op!(
            "Higher half mapping start",
            $phys_start,
            virt_start,
            $num_pages
        );
        map_range_with_log_macro!(
            $mapper,
            $frame_allocator,
            $phys_start,
            virt_start,
            $num_pages,
            $flags
        );
        log_page_table_op!(
            "Higher half mapping complete",
            $phys_start,
            virt_start,
            $num_pages
        );
        Ok::<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>>(())
    }};
}

/// Consolidated memory mapping with log support
#[macro_export]
macro_rules! map_with_log_macro {
    ($mapper:expr, $allocator:expr, $phys:expr, $virt:expr, $pages:expr, $flags:expr) => {{
        log_page_table_op!("Mapping", $phys, $virt, $pages);
        for i in 0..$pages {
            let phys_addr = $phys + i * 4096;
            let virt_addr = $virt + i * 4096;
            map_with_offset!($mapper, $allocator, phys_addr, virt_addr, $flags);
        }
        Ok::<(), x86_64::structures::paging::mapper::MapToError<x86_64::structures::paging::Size4KiB>>(())
    }};
}

/// Flush TLB and verify the flush was successful
#[macro_export]
macro_rules! flush_tlb_and_verify {
    () => {{
        use x86_64::instructions::tlb;
        use x86_64::registers::control::{Cr3, Cr3Flags};
        tlb::flush_all();
        // Verify by reading CR3 to force a TLB reload
        let (frame, flags): (x86_64::structures::paging::PhysFrame<x86_64::structures::paging::Size4KiB>, Cr3Flags) = Cr3::read();
        unsafe { Cr3::write(frame, flags) };
    }};
}

pub struct InitSequence<'a> {
    steps: &'a [(&'static str, Box<dyn Fn() -> Result<(), &'static str>>)],
}

/// Calculate offset address in loops (phys_addr + i * 4096)
#[macro_export]
macro_rules! calc_offset_addr {
    ($base:expr, $i:expr) => {
        $base + ($i * 4096)
    };
}

/// Create page and frame from virtual and physical addresses
#[macro_export]
macro_rules! create_page_and_frame {
    ($virt_addr:expr, $phys_addr:expr) => {{
        use x86_64::{
            PhysAddr, VirtAddr,
            structures::paging::{Page, PhysFrame, Size4KiB},
        };
        let virt = VirtAddr::new($virt_addr);
        let phys = PhysAddr::new($phys_addr);
        let page = Page::<Size4KiB>::containing_address(virt);
        let frame = PhysFrame::<Size4KiB>::containing_address(phys);
        (page, frame)
    }};
}

/// Map and flush operation in one call
#[macro_export]
macro_rules! map_and_flush {
    ($mapper:expr, $page:expr, $frame:expr, $flags:expr, $allocator:expr) => {{
        unsafe {
            $mapper
                .map_to($page, $frame, $flags, $allocator)
                .expect("Failed to map page")
                .flush();
        }
    }};
}

/// Map a physical address to virtual address with offset
#[macro_export]
macro_rules! map_with_offset {
    ($mapper:expr, $allocator:expr, $phys_addr:expr, $virt_addr:expr, $flags:expr) => {{
        let (page, frame) = create_page_and_frame!($virt_addr, $phys_addr);
        map_and_flush!($mapper, page, frame, $flags, $allocator);
    }};
}

/// Enhanced memory descriptor logging for common patterns
#[macro_export]
macro_rules! log_memory_descriptor {
    ($desc:expr, $i:expr) => {
        crate::mem_debug!("Memory descriptor ", $i);
        crate::mem_debug!(
            ": type=",
            $desc.type_ as usize,
            ", phys_start=0x",
            $desc.physical_start as usize
        );
        crate::mem_debug!(
            ", virt_start=",
            $desc.virtual_start as usize,
            ", pages=",
            $desc.number_of_pages as usize
        );
        crate::mem_debug!("\n");
    };
}

/// Identity mapping range with error handling
#[macro_export]
macro_rules! map_identity_range_checked {
    ($mapper:expr, $allocator:expr, $phys_start:expr, $num_pages:expr, $flags:expr) => {{
        for i in 0..$num_pages {
            let addr = calc_offset_addr!($phys_start, i);
            let (page, frame) = create_page_and_frame!(addr, addr);
            match unsafe { $mapper.map_to(page, frame, $flags, $allocator) } {
                Ok(flush) => flush.flush(),
                Err(x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_)) => {
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }};
}

/// Macro for mapping a range of pages using map_page
#[macro_export]
macro_rules! map_page_range {
    ($mapper:expr, $allocator:expr, $base_virt:expr, $base_phys:expr, $num_pages:expr, $flags:expr) => {{
        for i in 0..$num_pages {
            let phys_addr = $base_phys + (i * 4096);
            let virt_addr = $base_virt + (i * 4096);
            $mapper.map_page(virt_addr, phys_addr, $flags, $allocator)?;
        }
    }};
}

/// Macro for unmapping a range of pages using unmap_page
#[macro_export]
macro_rules! unmap_page_range {
    ($mapper:expr, $base_virt:expr, $num_pages:expr) => {{
        for i in 0..$num_pages {
            let vaddr = $base_virt + (i * 4096);
            $mapper.unmap_page(vaddr)?;
        }
    }};
}

/// Calculate kernel size from ELF headers
#[macro_export]
macro_rules! calculate_kernel_pages {
    ($size:expr) => {
        ($size.div_ceil(4096))
    };
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
            crate::serial::serial_log(format_args!("About to init {}\n", name));
            if let Err(e) = init_fn() {
                crate::serial::serial_log(format_args!("Init {} failed: {}\n", name, e));
                panic!("{}", e);
            }
            crate::serial::serial_log(format_args!("{} init done\n", name));
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

/// Unified PCI configuration space read macro to reduce separate function duplication
/// Supports reading 8, 16, or 32-bit values based on the size parameter
///
/// # Examples
/// ```
/// let vendor_id = pci_config_read!(bus, device, function, 0x00, 16);
/// let class_code = pci_config_read!(bus, device, function, 0x0B, 8);
/// ```
#[macro_export]
macro_rules! pci_config_read {
    ($bus:expr, $device:expr, $function:expr, $reg:expr, 32) => {
        $crate::bare_metal_pci::pci_config_read_dword($bus, $device, $function, $reg)
    };
    ($bus:expr, $device:expr, $function:expr, $reg:expr, 16) => {
        $crate::bare_metal_pci::pci_config_read_word($bus, $device, $function, $reg)
    };
    ($bus:expr, $device:expr, $function:expr, $reg:expr, 8) => {
        $crate::bare_metal_pci::pci_config_read_byte($bus, $device, $function, $reg)
    };
}

/// Macro for displaying multiple lines of statistics on VGA buffer to reduce repetitive position/write calls
/// Automatically positions cursor and writes formatted text for each line
///
/// # Examples
/// ```
/// display_vga_stats_lines!(vga_writer,
///     23, "Processes: {}/{}", active, total;
///     24, "Memory: {} KB", mem_kb;
///     25, "Uptime: {}", uptime
/// );
/// ```
#[macro_export]
macro_rules! display_vga_stats_lines {
    ($vga_writer:expr, $($row:expr, $format:expr, $($args:expr),*);*) => {
        $(
            $crate::vga_stat_line!($vga_writer, $row, $format, $($args),*)
        )*
    };
}

/// Helper macro for a single stat line display
#[macro_export]
macro_rules! vga_stat_line {
    ($vga_writer:expr, $row:expr, $format:expr, $($args:expr),*) => {{
        $vga_writer.set_position($row, 0);
        let _ = write!($vga_writer, $format, $($args),*);
    }};
}

/// Macro for subsystem initialization with consistent logging and error handling
/// Reduces boilerplate in component startup code
///
/// # Examples
/// ```
/// init_with_log!("Scheduler", || scheduler.init());
/// init_with_log!("Memory Manager", || mem_manager.init());
/// ```
#[macro_export]
macro_rules! init_with_log {
    ($name:expr, $init_fn:expr) => {{
        debug_log!(concat!($name, " initializing"));
        match $init_fn() {
            Ok(_) => debug_log!(concat!($name, " initialized successfully")),
            Err(e) => {
                error_log!(concat!($name, " initialization failed: {:?}"), e);
                panic!("{} initialization failed", $name);
            }
        }
    }};
}

/// Macro for unified error checking with optional logging
/// Reduces repetitive if condition with return Err pattern
///
/// # Examples
/// ```
/// ensure!(ptr.is_some(), SystemError::InvalidArgument, "Pointer is null");
/// ensure_with_log!(value > 0, "Value must be positive", SystemError::InvalidArgument);
/// ```
#[macro_export]
macro_rules! ensure {
    ($condition:expr, $error_ty:ty, $error_val:expr) => {
        if !$condition {
            return Err(<$error_ty>::$error_val as $error_ty);
        }
    };
    ($condition:expr, $error_ty:ty, $error_val:expr, $msg:expr) => {
        if !$condition {
            error_log!($msg);
            return Err(<$error_ty>::$error_val as $error_ty);
        }
    };
}

/// Macro for displaying system stats on VGA display with periodic checks
/// Reduces code for stat display functionality
///
/// # Examples
/// ```
/// display_stats_on_available_display!(stats, current_tick, interval_ticks, vga_buffer);
/// ```
#[macro_export]
macro_rules! display_stats_on_available_display {
    ($stats:expr, $current_tick:expr, $interval_ticks:expr, $vga_buffer:expr) => {{
        static LAST_DISPLAY_TICK: spin::Mutex<u64> = spin::Mutex::new(0);

        petroleum::check_periodic!(LAST_DISPLAY_TICK, $interval_ticks, $current_tick, {
            if let Some(vga_buffer_ref) = $vga_buffer.get() {
                let mut writer = vga_buffer_ref.lock();

                // Clear bottom rows for system info display
                let blank_char = petroleum::ScreenChar {
                    ascii_character: b' ',
                    color_code: petroleum::ColorCode::new(
                        petroleum::Color::Black,
                        petroleum::Color::Black,
                    ),
                };

                // Set position to bottom left for system info
                writer.set_position(22, 0);
                use core::fmt::Write;

                writer.set_color_code(petroleum::ColorCode::new(
                    petroleum::Color::Cyan,
                    petroleum::Color::Black,
                ));

                // Clear the status lines first
                petroleum::clear_line_range!(writer, 23, 26, 0, 80, blank_char);

                // Display system info on bottom rows using macro to reduce repetition
                petroleum::display_vga_stats_lines!(writer,
                    23, "Processes: {}/{}", $stats.active_processes, $stats.total_processes;
                    24, "Memory: {} KB", $stats.memory_used / 1024;
                    25, "Tick: {}", $stats.uptime_ticks
                );
                writer.update_cursor();
            }
        });
    }};
}

/// Macro for defining simple shell command functions that print a message and return 0
/// Reduces boilerplate in command implementations
///
/// # Examples
/// ```
/// simple_command_fn!(uname_command, "Fullerene OS 0.1.0 x86_64\n");
/// simple_command_fn!(ps_command, "Process list not implemented\n", 1); // With exit code
/// ```
#[macro_export]
macro_rules! simple_command_fn {
    ($fn_name:ident, $message:literal) => {
        fn $fn_name(_args: &[&str]) -> i32 {
            petroleum::print!($message);
            0
        }
    };
    ($fn_name:ident, $message:literal, $exit_code:expr) => {
        fn $fn_name(_args: &[&str]) -> i32 {
            petroleum::print!($message);
            $exit_code
        }
    };
}

/// Unified print macros using the log crate for consistent logging across all crates
/// Fallback to serial if logger not initialized yet
#[macro_export]
macro_rules! println {
    () => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!("");
        } else {
            $crate::serial::_print(format_args!("\n"));
        }
    };
    ($($arg:tt)*) => {
        if $crate::common::logging::is_logger_initialized() {
            log::info!("{}", format_args!($($arg)*));
        } else {
            $crate::serial::_print(format_args!("{}\n", format_args!($($arg)*)));
        }
    };
}

/// Unified print macro - same as print! for consistency
#[macro_export]
macro_rules! print {
    () => {
        $crate::println!();
    };
    ($($arg:tt)*) => {
        $crate::println!($($arg)*);
    };
}

/// Macro for implementing TextBufferOperations trait for buffer-like structs
/// Generates all 7 required methods using the buffer, dimensions, and other fields
///
/// # Examples
/// ```
/// impl_text_buffer_operations!(VgaBuffer,
///     buffer, row_position, column_position, color_code,
///     BUFFER_HEIGHT, BUFFER_WIDTH
/// );
/// ```
#[macro_export]
macro_rules! impl_text_buffer_operations {
    ($struct_name:ident, $buffer_field:ident, $row_pos:ident, $col_pos:ident, $color_field:ident, $height:ident, $width:ident) => {
        fn get_width(&self) -> usize {
            $width
        }

        fn get_height(&self) -> usize {
            $height
        }

        fn get_color_code(&self) -> ColorCode {
            self.$color_field
        }

        fn get_position(&self) -> (usize, usize) {
            (self.$row_pos, self.$col_pos)
        }

        fn set_position(&mut self, row: usize, col: usize) {
            self.$row_pos = row;
            self.$col_pos = col;
        }

        fn set_char_at(&mut self, row: usize, col: usize, chr: ScreenChar) {
            if row < $height && col < $width {
                self.$buffer_field[row][col] = chr;
            }
        }

        fn get_char_at(&self, row: usize, col: usize) -> ScreenChar {
            if row < $height && col < $width {
                self.$buffer_field[row][col]
            } else {
                ScreenChar {
                    ascii_character: 0,
                    color_code: self.$color_field,
                }
            }
        }

        #[inline]
        fn write_byte(&mut self, byte: u8) {
            handle_write_byte!(self, byte, { self.new_line() }, {
                if self.$col_pos >= $width {
                    self.new_line();
                }
                if self.$row_pos >= $height {
                    self.scroll_up();
                    self.$row_pos = $height - 1;
                }
                let screen_char = ScreenChar {
                    ascii_character: byte,
                    color_code: self.$color_field,
                };
                self.$buffer_field[self.$row_pos][self.$col_pos] = screen_char;
                self.$col_pos += 1;
            });
        }

        fn new_line(&mut self) {
            self.$col_pos = 0;
            if self.$row_pos < $height - 1 {
                self.$row_pos += 1;
            } else {
                self.scroll_up();
            }
        }

        fn clear_row(&mut self, row: usize) {
            let blank_char = ScreenChar {
                ascii_character: b' ',
                color_code: self.$color_field,
            };
            petroleum::clear_line_range!(self, row, row + 1, 0, self.get_width(), blank_char);
        }

        fn clear_screen(&mut self) {
            self.$row_pos = 0;
            self.$col_pos = 0;
            let blank_char = ScreenChar {
                ascii_character: b' ',
                color_code: ColorCode(0),
            };
            petroleum::clear_buffer!(self, self.get_height(), self.get_width(), blank_char);
        }

        fn scroll_up(&mut self) {
            for row in 1..$height {
                for col in 0..$width {
                    self.$buffer_field[row - 1][col] = self.$buffer_field[row][col];
                }
            }
            for col in 0..$width {
                self.$buffer_field[$height - 1][col] = ScreenChar {
                    ascii_character: b' ',
                    color_code: self.$color_field
                };
            }
        }
    };
}

/// Macro for defining extension trait getter/setter methods
/// Reduces boilerplate for simple property accessors
///
/// # Examples
/// ```
/// impl_getter_setter!(ColorCode, foreground, u4);
/// impl_getter_setter!(ColorCode, background, u4);
/// ```
#[macro_export]
macro_rules! impl_getter_setter {
    ($struct:ident, $field:ident, $type:ty) => {
        #[inline]
        pub fn $field(&self) -> $type {
            self.$field
        }

        #[inline]
        pub fn set_$field(&mut self, value: $type) {
            self.$field = value;
        }
    };
}
