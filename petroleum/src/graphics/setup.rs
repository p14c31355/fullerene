use super::ports::{PortWriter, VgaPortOps, VgaPorts};
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

// Detect VGA hardware type (attempt to identify Cirrus VGA vs standard VGA)
pub fn detect_vga_hardware_type() -> &'static str {
    // Read PCI configuration space method - requires MMIO or PCI access
    // For now, we rely on empirical detection or fallback to generic VGA

    // Try to read VGA registers and determine type
    unsafe {
        // Check graphics controller register (index 0x0F) for identification
        let mut index_writer = PortWriter::<u8>::new(VgaPorts::GRAPHICS_INDEX);
        let mut data_reader = PortWriter::<u8>::new(VgaPorts::GRAPHICS_DATA);

        index_writer.write_safe(0x0F);
        let id_value = data_reader.read_safe();

        // Cirrus Logic VGA chips often have specific signatures
        // This is a simple heuristic - not comprehensive
        match id_value {
            0xBC | 0xBD | 0xBE | 0xBF => {
                log_step!("Detected Cirrus VGA hardware\n");
                "cirrus"
            }
            0x00 | 0x01 => {
                log_step!("Detected basic VGA hardware\n");
                "basic"
            }
            _ => {
                log_step!("Detected unknown VGA hardware, assuming basic\n");
                "basic"
            }
        }
    }
}

// Unified text mode initialization function
pub fn setup_vga_text_mode() {
    log_step!("VGA text mode setup: Starting\n");

    // Detect hardware before setup
    let _vga_type = detect_vga_hardware_type();

    setup_misc_output();

    // Sequencer, CRTC, and Graphics setup using centralized macros
    use super::ports::RegisterConfig;
    let seq_configs: [RegisterConfig; 5] = [
        RegisterConfig {
            index: 0x00,
            value: 0x03,
        },
        RegisterConfig {
            index: 0x01,
            value: 0x00,
        },
        RegisterConfig {
            index: 0x02,
            value: 0x03,
        },
        RegisterConfig {
            index: 0x03,
            value: 0x00,
        },
        RegisterConfig {
            index: 0x04,
            value: 0x02,
        },
    ];

    let crtc_unlock = [RegisterConfig {
        index: 0x11,
        value: 0x0E,
    }];
    let crtc_configs: [RegisterConfig; 18] = [
        RegisterConfig {
            index: 0x00,
            value: 0x5F,
        },
        RegisterConfig {
            index: 0x01,
            value: 0x4F,
        },
        RegisterConfig {
            index: 0x02,
            value: 0x50,
        },
        RegisterConfig {
            index: 0x03,
            value: 0x82,
        },
        RegisterConfig {
            index: 0x04,
            value: 0x55,
        },
        RegisterConfig {
            index: 0x05,
            value: 0x81,
        },
        RegisterConfig {
            index: 0x06,
            value: 0xBF,
        },
        RegisterConfig {
            index: 0x07,
            value: 0x1F,
        },
        RegisterConfig {
            index: 0x08,
            value: 0x00,
        },
        RegisterConfig {
            index: 0x09,
            value: 0x4F,
        },
        RegisterConfig {
            index: 0x10,
            value: 0x9C,
        },
        RegisterConfig {
            index: 0x11,
            value: 0x8E,
        },
        RegisterConfig {
            index: 0x12,
            value: 0x8F,
        },
        RegisterConfig {
            index: 0x13,
            value: 0x28,
        },
        RegisterConfig {
            index: 0x14,
            value: 0x1F,
        },
        RegisterConfig {
            index: 0x15,
            value: 0x96,
        },
        RegisterConfig {
            index: 0x16,
            value: 0xB9,
        },
        RegisterConfig {
            index: 0x17,
            value: 0xA3,
        },
    ];
    let graphics_configs: [RegisterConfig; 2] = [
        RegisterConfig {
            index: 0x05,
            value: 0x10,
        },
        RegisterConfig {
            index: 0x06,
            value: 0x0E,
        },
    ];

    write_port_sequence!(
        &seq_configs, VgaPorts::SEQUENCER_INDEX, VgaPorts::SEQUENCER_DATA;
        &crtc_unlock, VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA;
        &crtc_configs, VgaPorts::CRTC_INDEX, VgaPorts::CRTC_DATA;
        &graphics_configs, VgaPorts::GRAPHICS_INDEX, VgaPorts::GRAPHICS_DATA
    );

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
    let mut misc_writer = PortWriter::new(VgaPorts::MISC_OUTPUT);
    misc_writer.write_safe(0x63u8); // Value for enabling VGA in 320x200x256 mode
}

// Makro is already defined in ports.rs
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
    let mut status_reader = PortWriter::<u8>::new(VgaPorts::STATUS);
    let mut attr_ops = VgaPortOps::new(VgaPorts::ATTRIBUTE_INDEX, VgaPorts::ATTRIBUTE_INDEX);

    let _: u8 = status_reader.read_safe(); // Reset flip-flop

    for reg in ATTRIBUTE_CONFIG {
        attr_ops.write_register(reg.index, reg.value);
    }

    PortWriter::<u8>::new(VgaPorts::ATTRIBUTE_INDEX).write_safe(0x20u8); // Enable video output
}

/// Configures the VGA Attribute Controller registers.
pub fn setup_attribute_controller() {
    write_attribute_registers();
}

/// Sets up a simple grayscale palette for the 256-color mode.
pub fn setup_palette() {
    let mut dac_index_writer = PortWriter::<u8>::new(VgaPorts::DAC_INDEX);
    let mut dac_data_writer = PortWriter::<u8>::new(VgaPorts::DAC_DATA);

    dac_index_writer.write_safe(0x00u8); // Start at color index 0

    for i in 0..256 {
        let val = (i * 63 / 255) as u8;
        for _ in 0..3 {
            dac_data_writer.write_safe(val); // RGB
        }
    }
}

// Helper function to write multiple registers to a port pair (for VGA setup)
pub fn write_vga_registers(index_port: u16, data_port: u16, configs: &[(u8, u8)]) {
    let mut index_writer = PortWriter::new(index_port);
    let mut data_writer = PortWriter::new(data_port);
    for &(index, value) in configs {
        index_writer.write_safe(index);
        data_writer.write_safe(value);
    }
}

// Helper function to set VGA attribute controller registers
pub fn setup_vga_attributes() {
    let mut status_reader = PortWriter::<u8>::new(VgaPorts::STATUS);
    let mut attr_ops = VgaPortOps::new(VgaPorts::ATTRIBUTE_INDEX, VgaPorts::ATTRIBUTE_INDEX);

    let _: u8 = status_reader.read_safe(); // Reset flip-flop

    // Attribute registers configuration
    let attr_configs = [
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
    for (reg_index, reg_value) in attr_configs {
        attr_ops.write_register(reg_index, reg_value);
    }

    PortWriter::<u8>::new(VgaPorts::ATTRIBUTE_INDEX).write_safe(0x20u8); // Enable video output
}
