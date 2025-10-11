use x86_64::instructions::port::Port;

use super::ports::{RegisterConfig, VgaPorts};
use super::registers::{ATTRIBUTE_CONFIG, CRTC_CONFIG, GRAPHICS_CONFIG, SEQUENCER_CONFIG};

// Helper macros to reduce repetitive serial logging in init functions
macro_rules! log_step {
    ($msg:expr) => {
        crate::write_serial_bytes!(0x3F8, 0x3FD, $msg.as_bytes());
    };
}

// Enhanced setup function with better organization
pub fn setup_vga_mode_13h() {
    log_step!("VGA setup: Starting mode 13h initialization\n");
    setup_misc_output();
    setup_registers_from_configs();
    setup_attribute_controller();
    setup_palette();
    log_step!("VGA setup: Mode 13h initialization complete\n");
}

// Unified text mode initialization function
pub fn setup_vga_text_mode() {
    log_step!("VGA text mode setup: Starting\n");
    setup_misc_output();

    // Sequencer setup - reduced from separate function call
    let seq_configs = [
        (0x00, 0x03), (0x01, 0x00), (0x02, 0x03), (0x03, 0x00), (0x04, 0x02),
    ];
    write_vga_registers(VgaPorts::SEQUENCER_INDEX, VgaPorts::SEQUENCER_DATA, &seq_configs);

    // CRTC unlock
    write_vga_registers(VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA, &[(0x11, 0x0E)]);

    // CRTC main configuration
    let crtc_configs = [
        (0x00, 0x5F), (0x01, 0x4F), (0x02, 0x50), (0x03, 0x82), (0x04, 0x55),
        (0x05, 0x81), (0x06, 0xBF), (0x07, 0x1F), (0x08, 0x00), (0x09, 0x4F),
        (0x10, 0x9C), (0x11, 0x8E), (0x12, 0x8F), (0x13, 0x28), (0x14, 0x1F),
        (0x15, 0x96), (0x16, 0xB9), (0x17, 0xA3),
    ];
    write_vga_registers(VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA, &crtc_configs);

    // Graphics configuration
    let graphics_configs = [(0x05, 0x10), (0x06, 0x0E)];
    write_vga_registers(VgaPorts::GRAPHICS_INDEX, VgaPorts::GRAPHICS_DATA, &graphics_configs);

    // Attribute controller with inlined setup
    setup_vga_attributes();

    log_step!("VGA text mode setup: Complete\n");
}

// Legacy functions for backward compatibility
pub fn init_vga_graphics() {
    setup_vga_mode_13h();
}

pub fn init_vga_text_mode() {
    setup_vga_text_mode();
}

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
            (0x10, 0x01), // Mode control - text mode, 8-bit characters, blinking off
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

        attr_port.write(0x20); // Enable video output
    }
}
