//! Graphics and VGA macros for Fullerene OS

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
                let cell = $buffer.get_char_at(row, col);
                $buffer.set_char_at(row - 1, col, cell);
            }
        }
        for col in 0..$width {
            $buffer.set_char_at($height - 1, col, $blank);
        }
    };
}

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

#[macro_export]
macro_rules! vga_stat_display {
    ($vga_buffer:expr, $stats:expr, $current_tick:expr, $interval_ticks:expr, $start_row:expr, $($display_line:tt)*) => {{
        static LAST_DISPLAY_TICK: spin::Mutex<u64> = spin::Mutex::new(0);
        petroleum::check_periodic!(LAST_DISPLAY_TICK, $interval_ticks, $current_tick, {
            petroleum::vga_stat_display_impl!($vga_buffer, $start_row, $($display_line)*);
        });
    }};
}

#[macro_export]
macro_rules! vga_stat_display_impl {
    ($vga_buffer:expr, $start_row:expr, $($display_line:tt)*) => {{
        let lock = $vga_buffer.lock();
        if let Some(ref mut vga_writer) = *lock {
            let blank_char = ScreenChar {
                ascii_character: b' ',
                color_code: ColorCode::new(Color::Black, Color::Black),
            };
            petroleum::clear_line_range!(vga_writer, $start_row, $start_row + 3, 0, 80, blank_char);
            vga_writer.set_position($start_row, 0);
            use core::fmt::Write;
            vga_writer.set_color_code(ColorCode::new(Color::Cyan, Color::Black));
            $(
                vga_stat_line!(vga_writer, $display_line);
            )*
            vga_writer.update_cursor();
        }
    }};
}

#[macro_export]
macro_rules! vga_stat_line {
    ($vga_writer:expr, $row:expr, $format:expr, $($args:expr),*) => {{
        (*$vga_writer).set_position($row, 0);
        let _ = write!(*$vga_writer, $format, $($args),* );
    }};
}

#[macro_export]
macro_rules! init_vga_palette_registers {
    () => {{
        for i in 0u8..16u8 {
            $crate::hardware::ports::write_vga_attribute_register(i, i);
        }
    }};
}

#[macro_export]
macro_rules! set_vga_attribute_registers {
    ($($index:expr => $value:expr),* $(,)?) => {{
        $(
            $crate::hardware::ports::write_vga_attribute_register($index, $value);
        )*
    }};
}

#[macro_export]
macro_rules! enable_vga_video {
    () => {{
        // Use defined constants for readability and maintainability
        $crate::port_read_u8!($crate::hardware::ports::HardwarePorts::STATUS);
        $crate::port_write!(
            $crate::hardware::ports::HardwarePorts::ATTRIBUTE_INDEX,
            0x20u8
        );
    }};
}

#[macro_export]
macro_rules! vga_write_registers {
    ($configs:expr, $index_port:expr, $data_port:expr) => {
        let mut ops = $crate::hardware::ports::VgaPortOps::new($index_port, $data_port);
        ops.write_sequence($configs);
    };
}

#[macro_export]
macro_rules! init_vga_text_mode_3 {
    () => {{
        // Write misc register
        $crate::port_write!($crate::hardware::ports::HardwarePorts::MISC_OUTPUT, 0x67u8);

        // Sequencer, CRTC, Graphics registers using helper macro
        $crate::vga_write_registers!(
            $crate::graphics::registers::SEQUENCER_TEXT_CONFIG,
            $crate::hardware::ports::HardwarePorts::SEQUENCER_INDEX,
            $crate::hardware::ports::HardwarePorts::SEQUENCER_DATA
        );
        $crate::vga_write_registers!(
            $crate::graphics::registers::CRTC_TEXT_CONFIG,
            $crate::hardware::ports::HardwarePorts::CRTC_INDEX,
            $crate::hardware::ports::HardwarePorts::CRTC_DATA
        );
        $crate::vga_write_registers!(
            $crate::graphics::registers::GRAPHICS_TEXT_CONFIG,
            $crate::hardware::ports::HardwarePorts::GRAPHICS_INDEX,
            $crate::hardware::ports::HardwarePorts::GRAPHICS_DATA
        );

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

#[macro_export]
macro_rules! update_vga_cursor {
    ($pos:expr) => {{
        $crate::port_write!(
            $crate::hardware::ports::HardwarePorts::CRTC_INDEX,
            $crate::hardware::ports::HardwarePorts::CURSOR_POS_LOW_REG
        );
        $crate::port_write!(
            $crate::hardware::ports::HardwarePorts::CRTC_DATA,
            (($pos & 0xFFusize) as u8)
        );
        $crate::port_write!(
            $crate::hardware::ports::HardwarePorts::CRTC_INDEX,
            $crate::hardware::ports::HardwarePorts::CURSOR_POS_HIGH_REG
        );
        $crate::port_write!(
            $crate::hardware::ports::HardwarePorts::CRTC_DATA,
            ((($pos >> 8) & 0xFFusize) as u8)
        );
    }};
}

#[macro_export]
macro_rules! display_vga_stats_lines {
    ($vga_writer:expr, $($row:expr, $format:expr, $($args:expr),*);*) => {
        $(
            {
                (*$vga_writer).set_position($row, 0);
                let _ = write!(*$vga_writer, $format, $($args),* );
            }
        )*
    };
}

#[macro_export]
macro_rules! vga_write_lines {
    ($writer:expr, $($line:expr);* $(;)?) => {{
        $(
            $writer.write_string($line);
        )*
    }};
}

#[macro_export]
macro_rules! display_stats_on_available_display {
    ($stats:expr, $current_tick:expr, $interval_ticks:expr, $vga_buffer:expr) => {{
        static LAST_DISPLAY_TICK: spin::Mutex<u64> = spin::Mutex::new(0);

        petroleum::check_periodic!(LAST_DISPLAY_TICK, $interval_ticks, $current_tick, {
            let mut lock = $vga_buffer.lock();
            if let Some(ref mut writer) = *lock {

                // Clear bottom rows for system info display
                let blank_char = petroleum::ScreenChar {
                    ascii_character: b' ',
                    color_code: petroleum::ColorCode::new(
                        petroleum::Color::Black,
                        petroleum::Color::Black,
                    ),
                };

                // Set position to bottom left for system info
                (*writer).set_position(22, 0);
                use core::fmt::Write;

                (*writer).set_color_code(petroleum::ColorCode::new(
                    petroleum::Color::Cyan,
                    petroleum::Color::Black,
                ));

                // Clear the status lines first
                petroleum::clear_line_range(&mut *writer, 23, 26, 0, 80, blank_char);

                // Display system info on bottom rows using macro to reduce repetition
                petroleum::display_vga_stats_lines!(writer,
                    23, "Processes: {}/{}", $stats.active_processes, $stats.total_processes;
                    24, "Memory: {} KB", $stats.memory_used / 1024;
                    25, "Tick: {}", $stats.uptime_ticks
                );
                (*writer).update_cursor();
            }
        });
    }};
}

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
            for col in 0..self.get_width() {
                self.set_char_at(row, col, blank_char);
            }
        }

        fn clear_screen(&mut self) {
            self.$row_pos = 0;
            self.$col_pos = 0;
            let blank_char = ScreenChar {
                ascii_character: b' ',
                color_code: ColorCode(0),
            };
            for row in 0..self.get_height() {
                for col in 0..self.get_width() {
                    self.set_char_at(row, col, blank_char);
                }
            }
        }

        fn scroll_up(&mut self) {
            let blank = ScreenChar {
                ascii_character: b' ',
                color_code: self.$color_field,
            };
            for row in 1..$height {
                for col in 0..$width {
                    self.$buffer_field[row - 1][col] = self.$buffer_field[row][col];
                }
            }
            for col in 0..$width {
                self.$buffer_field[$height - 1][col] = blank;
            }
        }
    };
}

#[macro_export]
macro_rules! impl_vga_buffer {
    ($struct_name:ident, $height:ident, $width:ident) => {
        pub struct $struct_name {
            buffer: &'static mut [[ScreenChar; $width]; $height],
            column_position: usize,
            row_position: usize,
            color_code: ColorCode,
        }

        impl $struct_name {
            pub fn new(vga_address: usize) -> $struct_name {
                $struct_name {
                    buffer: unsafe { &mut *(vga_address as *mut _) },
                    column_position: 0,
                    row_position: 0,
                    color_code: ColorCode::new(Color::Green, Color::Black),
                }
            }

            pub fn set_color_code(&mut self, color_code: ColorCode) {
                self.color_code = color_code;
            }

            pub fn update_cursor(&self) {
                let pos = self.row_position * $width + self.column_position;
                update_vga_cursor!(pos);
            }
        }

        impl core::fmt::Write for $struct_name {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                self.write_string(s);
                Ok(())
            }
        }

        unsafe impl Send for $struct_name {}
        unsafe impl Sync for $struct_name {}

        impl TextBufferOperations for $struct_name {
            impl_text_buffer_operations!(
                $struct_name,
                buffer,
                row_position,
                column_position,
                color_code,
                $height,
                $width
            );
        }
    };
}

#[macro_export]
macro_rules! impl_mock_vga_buffer {
    ($struct_name:ident, $height:ident, $width:ident) => {
        struct $struct_name {
            buffer: Vec<ScreenChar>,
            column_position: usize,
            row_position: usize,
            color_code: ColorCode,
            height: usize,
            width: usize,
        }

        impl TextBufferOperations for $struct_name {
            fn get_width(&self) -> usize {
                self.width
            }

            fn get_height(&self) -> usize {
                self.height
            }

            fn get_color_code(&self) -> ColorCode {
                self.color_code
            }

            fn get_position(&self) -> (usize, usize) {
                (self.row_position, self.column_position)
            }

            fn set_position(&mut self, row: usize, col: usize) {
                self.row_position = row;
                self.column_position = col;
            }

            fn set_char_at(&mut self, row: usize, col: usize, chr: ScreenChar) {
                if row < self.height && col < self.width {
                    let index = row * self.width + col;
                    self.buffer[index] = chr;
                }
            }

            fn get_char_at(&self, row: usize, col: usize) -> ScreenChar {
                if row < self.height && col < self.width {
                    let index = row * self.width + col;
                    self.buffer[index]
                } else {
                    ScreenChar {
                        ascii_character: 0,
                        color_code: self.color_code,
                    }
                }
            }

            fn scroll_up(&mut self) {
                let blank_char = ScreenChar {
                    ascii_character: b' ',
                    color_code: self.color_code,
                };
                for row in 1..self.height {
                    for col in 0..self.width {
                        let index = row * self.width + col;
                        let next_index = (row - 1) * self.width + col;
                        self.buffer[next_index] = self.buffer[index];
                    }
                }
                for col in 0..self.width {
                    let index = (self.height - 1) * self.width + col;
                    self.buffer[index] = blank_char;
                }
            }
        }

        impl $struct_name {
            fn new(width: usize, height: usize) -> Self {
                $struct_name {
                    buffer: vec![
                        ScreenChar {
                            ascii_character: b' ',
                            color_code: ColorCode::new(Color::White, Color::Black),
                        };
                        width * height
                    ],
                    column_position: 0,
                    row_position: 0,
                    color_code: ColorCode::new(Color::White, Color::Black),
                    height,
                    width,
                }
            }

            fn get_char(&self, row: usize, col: usize) -> Option<ScreenChar> {
                if row < self.height && col < self.width {
                    let index = row * self.width + col;
                    Some(self.buffer[index])
                } else {
                    None
                }
            }
        }
    };
}

#[macro_export]
macro_rules! create_vga_singleton {
    ($name:ident, $buffer_ty:ty) => {
        pub static $name: Mutex<Option<$buffer_ty>> = Mutex::new(None);
    };
}
