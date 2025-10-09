use x86_64::instructions::port::Port;

use super::ports::{VgaPorts};
use super::registers::{ATTRIBUTE_CONFIG, CRTC_CONFIG, GRAPHICS_CONFIG, SEQUENCER_CONFIG};

// Helper function to write a palette value in grayscale
pub fn write_palette_grayscale(val: u8) {
    unsafe {
        let mut dac_data_port: Port<u8> = Port::new(VgaPorts::DAC_DATA);
        for _ in 0..3 {
            // RGB
            dac_data_port.write(val);
        }
    }
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
    }

    for i in 0..256 {
        let val = (i * 63 / 255) as u8;
        write_palette_grayscale(val);
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
