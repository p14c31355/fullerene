use super::ports::{HardwarePorts, PortWriter, VgaPortOps};
use super::registers::{
    ATTRIBUTE_CONFIG, ATTRIBUTE_TEXT_CONFIG, CRTC_CONFIG, CRTC_TEXT_CONFIG, GRAPHICS_CONFIG,
    GRAPHICS_TEXT_CONFIG, SEQUENCER_CONFIG, SEQUENCER_TEXT_CONFIG,
};

/// Macro to reduce repetitive RGB value writing in palette setup
#[macro_export]
macro_rules! write_rgb_value {
    ($writer:expr, $value:expr) => {
        for _ in 0..3 {
            $writer.write_safe($value);
        }
    };
}

// Helper macros to reduce repetitive serial logging in init functions
macro_rules! log_step {
    ($msg:expr) => {
        crate::write_serial_bytes!(0x3F8, 0x3FD, $msg.as_bytes());
    };
}

// Macro to reduce repetitive PCI vendor ID checks in device detection
macro_rules! pci_vendor_check_loop {
    ($vendor_id:expr, $bus_start:expr, $bus_end:expr, $dev_start:expr, $dev_end:expr, $found_msg:expr) => {
        for bus in $bus_start..$bus_end {
            for device in $dev_start..$dev_end {
                let test_vendor = crate::bare_metal_pci::pci_config_read_word(bus, device, 0, 0x00);
                if test_vendor == $vendor_id {
                    crate::serial::_print(format_args!($found_msg, bus, device));
                    return true;
                }
            }
        }
    };
}

// Macro to reduce repetitive cursor register settings in text mode
macro_rules! set_cursor_registers {
    ($start:expr, $end:expr) => {
        write_vga_register!(
            HardwarePorts::CRTC_INDEX,
            HardwarePorts::CRTC_DATA,
            0x0A,
            $start
        );
        write_vga_register!(
            HardwarePorts::CRTC_INDEX,
            HardwarePorts::CRTC_DATA,
            0x0B,
            $end
        );
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

// Cirrus VGA specific initialization for better compatibility
pub fn setup_cirrus_vga_mode() {
    log_step!("Cirrus VGA: Starting Cirrus-specific initialization\n");

    // First set up standard VGA mode 13h
    setup_vga_mode_13h();

    // Cirrus-specific register setup for better graphics mode support
    // Cirrus Logic 5446/5480 specific registers
    let mut index_writer = PortWriter::<u8>::new(0x3C4); // Sequencer index
    let mut data_writer = PortWriter::<u8>::new(0x3C5); // Sequencer data

    // Enable extended memory and better graphics support
    index_writer.write_safe(0x06u8); // Unlock Cirrus registers
    data_writer.write_safe(0x12u8);

    // Set up Cirrus-specific graphics registers for better desktop display
    index_writer.write_safe(0x1Eu8); // Extended mode register
    data_writer.write_safe(0x01u8); // Enable extended memory

    log_step!("Cirrus VGA: Cirrus-specific initialization complete\n");
}

// VGA device detection and initialization
pub fn detect_and_init_vga_graphics() {
    log_step!("VGA Detection: Starting VGA device detection\n");

    // Check if we have a Cirrus VGA device by checking PCI
    if detect_cirrus_vga() {
        log_step!("VGA Detection: Cirrus VGA device detected, initializing\n");
        setup_cirrus_vga_mode();
    } else {
        log_step!("VGA Detection: Standard VGA device detected, using standard mode\n");
        setup_vga_mode_13h();
    }
}

// Detect Cirrus VGA device via PCI
pub fn detect_cirrus_vga() -> bool {
    log_step!("VGA Detection: Checking for Cirrus VGA device\n");

    // Check PCI configuration for Cirrus device (vendor ID: 0x1013, device ID: various)
    // Bus 0, Device 2, Function 0 is typically where VGA devices are located
    if crate::bare_metal_pci::pci_config_read_word(0, 2, 0, 0x00) == 0x1013 {
        log_step!("VGA Detection: Cirrus VGA device found via PCI\n");
        return true;
    }

    // Also check other common locations using macro to reduce code
    pci_vendor_check_loop!(
        0x1013,
        0,
        2,
        0,
        32,
        "VGA Detection: Cirrus VGA device found at bus:device = {}:{}\n"
    );

    log_step!("VGA Detection: No Cirrus VGA device found, using standard VGA\n");
    false
}

// Unified text mode initialization function
pub fn setup_vga_text_mode() {
    log_step!("VGA text mode setup: Starting\n");

    setup_misc_output();

    // Unlock CRTC registers
    let crtc_unlock = super::ports::RegisterConfig {
        index: 0x11,
        value: 0x0E,
    };

    write_port_sequence!(
        SEQUENCER_TEXT_CONFIG, HardwarePorts::SEQUENCER_INDEX, HardwarePorts::SEQUENCER_DATA;
        &[crtc_unlock], HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA;
        CRTC_TEXT_CONFIG, HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA;
        GRAPHICS_TEXT_CONFIG, HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA
    );

    // Set cursor registers using macro to reduce lines
    set_cursor_registers!(0x0E, 0x0F);

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
    let mut misc_writer = PortWriter::new(HardwarePorts::MISC_OUTPUT);
    misc_writer.write_safe(0x63u8); // Value for enabling VGA in 320x200x256 mode
}

// Makro is already defined in ports.rs
/// Configures the VGA registers using the new macro
pub fn setup_registers_from_configs() {
    write_port_sequence!(
        SEQUENCER_CONFIG, HardwarePorts::SEQUENCER_INDEX, HardwarePorts::SEQUENCER_DATA;
        CRTC_CONFIG, HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA;
        GRAPHICS_CONFIG, HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA
    );
}

/// Helper function to write to attribute registers with special sequence
pub fn write_attribute_registers() {
    let mut status_reader = PortWriter::<u8>::new(HardwarePorts::STATUS);
    let mut attr_ops = VgaPortOps::new(
        HardwarePorts::ATTRIBUTE_INDEX,
        HardwarePorts::ATTRIBUTE_INDEX,
    );

    let _: u8 = status_reader.read_safe(); // Reset flip-flop

    for reg in ATTRIBUTE_CONFIG {
        attr_ops.write_register(reg.index, reg.value);
    }

    PortWriter::<u8>::new(HardwarePorts::ATTRIBUTE_INDEX).write_safe(0x20u8); // Enable video output
}

/// Configures the VGA Attribute Controller registers.
pub fn setup_attribute_controller() {
    write_attribute_registers();
}

/// Sets up a simple grayscale palette for the 256-color mode.
pub fn setup_palette() {
    let mut dac_index_writer = PortWriter::<u8>::new(HardwarePorts::DAC_INDEX);
    let mut dac_data_writer = PortWriter::<u8>::new(HardwarePorts::DAC_DATA);

    dac_index_writer.write_safe(0x00u8); // Start at color index 0

    for i in 0..256 {
        let val = (i * 63 / 255) as u8;
        write_rgb_value!(dac_data_writer, val);
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
    let mut status_reader = PortWriter::<u8>::new(HardwarePorts::STATUS);
    let mut attr_ops = VgaPortOps::new(
        HardwarePorts::ATTRIBUTE_INDEX,
        HardwarePorts::ATTRIBUTE_INDEX,
    );

    let _: u8 = status_reader.read_safe(); // Reset flip-flop

    // Write text mode attribute settings
    for reg in ATTRIBUTE_TEXT_CONFIG {
        attr_ops.write_register(reg.index, reg.value);
    }

    PortWriter::<u8>::new(HardwarePorts::ATTRIBUTE_INDEX).write_safe(0x20u8); // Enable video output
}
