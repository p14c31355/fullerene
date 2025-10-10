use x86_64::instructions::port::Port;

use super::ports::VgaPorts;
use super::registers::{ATTRIBUTE_CONFIG, CRTC_CONFIG, GRAPHICS_CONFIG, SEQUENCER_CONFIG};

/// Configures the Miscellaneous Output Register.
pub fn setup_misc_output() {
    unsafe {
        let mut misc_output_port = Port::new(VgaPorts::MISC_OUTPUT);
        misc_output_port.write(0x63u8); // Value for enabling VGA in 320x200x256 mode
    }
}

/// Configures the VGA registers using the new macro
pub fn setup_registers_from_configs() {
    write_port_sequence!(
        SEQUENCER_CONFIG, VgaPorts::SEQUENCER_INDEX, VgaPorts::SEQUENCER_DATA;
        CRTC_CONFIG, VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA;
        GRAPHICS_CONFIG, VgaPorts::GRAPHICS_INDEX, VgaPorts::GRAPHICS_DATA
    );
}

/// Helper function to write to attribute registers with special sequence
pub fn write_attribute_registers() {
    unsafe {
        let mut status_port = Port::<u8>::new(VgaPorts::STATUS);
        let mut index_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);
        let mut data_port = Port::<u8>::new(VgaPorts::ATTRIBUTE_INDEX);

        let _ = status_port.read(); // Reset flip-flop

        for reg in ATTRIBUTE_CONFIG {
            index_port.write(reg.index);
            data_port.write(reg.value);
        }

        index_port.write(0x20); // Enable video output
    }
}

/// Configures the VGA Attribute Controller registers.
pub fn setup_attribute_controller() {
    write_attribute_registers();
}

/// Sets up a simple grayscale palette for the 256-color mode.
pub fn setup_palette() {
    unsafe {
        let mut dac_index_port = Port::<u8>::new(VgaPorts::DAC_INDEX);
        dac_index_port.write(0x00); // Start at color index 0

        let mut dac_data_port = Port::<u8>::new(VgaPorts::DAC_DATA);
        for i in 0..256 {
            let val = (i * 63 / 255) as u8;
            for _ in 0..3 {
                // RGB
                dac_data_port.write(val);
            }
        }
    }
}

// Helper function to write multiple registers to a port pair (for VGA setup)
pub fn write_vga_registers(index_port: u16, data_port: u16, configs: &[(u8, u8)]) {
    unsafe {
        let mut idx_port = Port::new(index_port);
        let mut dat_port = Port::new(data_port);
        for &(index, value) in configs {
            idx_port.write(index);
            dat_port.write(value);
        }
    }
}

// Helper function to set VGA attribute controller registers
pub fn setup_vga_attributes() {
    unsafe {
        // Reset flip-flop first
        Port::<u8>::new(VgaPorts::STATUS).read();

        let mut attr_port = Port::new(VgaPorts::ATTRIBUTE_INDEX);
        // Attribute registers configuration
        let attr_configs: [(u8, u8); 21] = [
            (0x00, 0x00),
            (0x01, 0x01),
            (0x02, 0x02),
            (0x03, 0x03),
            (0x04, 0x04),
            (0x05, 0x05),
            (0x06, 0x06),
            (0x07, 0x07),
            (0x08, 0x08),
            (0x09, 0x09),
            (0x0A, 0x0A),
            (0x0B, 0x0B),
            (0x0C, 0x0C),
            (0x0D, 0x0D),
            (0x0E, 0x0E),
            (0x0F, 0x0F), // Palette setup
            (0x10, 0x41), // Mode control - enable 8-bit color, graphics mode, blinking on
            (0x11, 0x00), // Overscan
            (0x12, 0x0F), // Plane enable
            (0x13, 0x00), // Pixel padding
            (0x14, 0x00), // Color select
        ];

        // Write each index/data pair for attribute registers
        for &(reg_index, reg_value) in &attr_configs {
            attr_port.write(reg_index);
            attr_port.write(reg_value);
        }

        // Enable video output
        attr_port.write(0x20);
    }
}

// Initializes VGA text mode.
/// Sets up VGA for standard 80x25 text mode.
pub fn init_vga_text_mode() {
    unsafe {
        // Debug: Log start of VGA text mode setup
        crate::write_serial_bytes!(0x3F8, 0x3FD, b"VGA text mode setup: Starting misc output\n");

        // Misc output register - enable VGA, use color mode, low page, sync polarities
        Port::<u8>::new(VgaPorts::MISC_OUTPUT).write(0x63);

        // Sequencer registers for text mode
        let seq_configs = [
            (0x00, 0x03), // Synchronous reset - acquire access for CPU to video memory
            (0x01, 0x00), // Clocking mode - 25 MHz dot clock, 9 dots per character
            (0x02, 0x03), // Map mask register - enable writes to all planes
            (0x03, 0x00), // Character map select - select character set A and B
            (0x04, 0x02), // Memory mode - extended memory, sequential addressing, text mode
        ];
        write_vga_registers(
            VgaPorts::SEQUENCER_INDEX,
            VgaPorts::SEQUENCER_DATA,
            &seq_configs,
        );

        // Unlock CRTC registers - disable write protection on CRTC registers 0-7
        write_vga_registers(VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA, &[(0x11, 0x0E)]);

        // CRTC registers - standard values for 80x25 text mode
        let crtc_configs = [
            (0x00, 0x5F), // Horizontal total (79 + 1)
            (0x01, 0x4F), // Horizontal display end (79)
            (0x02, 0x50), // Start horizontal blanking (79 + 1)
            (0x03, 0x82), // End horizontal blanking (15 chars wide, compatibility mode)
            (0x04, 0x55), // Start horizontal sync pulse (after 85 chars)
            (0x05, 0x81), // End horizontal sync pulse (5 chars wide)
            (0x06, 0xBF), // Vertical total (400 - 1 = 399)
            (0x07, 0x1F), // Overflow register - all bits low
            (0x08, 0x00), // Preset row scan
            (0x09, 0x4F), // Maximum scan line (8 pixel high chars + 1 space)
            (0x10, 0x9C), // Start vertical sync pulse (after 412 lines)
            (0x11, 0x8E), // End vertical sync pulse (2 lines wide, write protection disabled)
            (0x12, 0x8F), // Vertical display end (400 - 1 = 399)
            (0x13, 0x28), // Offset register - 40 bytes per scan line (80 chars * 2 bytes/char)
            (0x14, 0x1F), // Underline location - scan line 0, move to D0 on overflow
            (0x15, 0x96), // Start vertical blanking (after 400 + 12 lines)
            (0x16, 0xB9), // End vertical blanking (25 lines blank)
            (0x17, 0xA3), // CRTC mode control - byte mode, enable video, every other line
        ];
        write_vga_registers(VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA, &crtc_configs);

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"VGA text mode setup: CRTC done\n");

        // Graphics registers for text mode
        let graphics_configs = [
            (0x05, 0x10), // Graphics mode register - read mode 0, write mode 0
            (0x06, 0x0E), // Miscellaneous register - text mode, 0xB8000 base, alpha disabled
        ];
        write_vga_registers(
            VgaPorts::GRAPHICS_INDEX,
            VgaPorts::GRAPHICS_DATA,
            &graphics_configs,
        );

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"VGA text mode setup: Graphics done\n");

        // Attribute controller setup
        setup_vga_attributes();

        crate::write_serial_bytes!(0x3F8, 0x3FD, b"VGA text mode setup: Attributes done\n");
    }
}

// Initializes VGA graphics mode 13h (320x200, 256 colors).
/// This function configures the VGA controller registers to switch to the specified
/// graphics mode. It is a complex process involving multiple sets of registers.
/// The initialization is broken down into smaller helper functions for clarity.
pub fn init_vga_graphics() {
    setup_misc_output();
    setup_registers_from_configs(); // Consolidated setup for sequencer, crtc, graphics
    setup_attribute_controller();
    setup_palette();
}
