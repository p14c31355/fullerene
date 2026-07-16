use super::registers::{
    ATTRIBUTE_CONFIG, ATTRIBUTE_TEXT_CONFIG, CRTC_CONFIG, CRTC_TEXT_CONFIG, GRAPHICS_CONFIG,
    GRAPHICS_TEXT_CONFIG, SEQUENCER_CONFIG, SEQUENCER_TEXT_CONFIG,
};
use crate::io::{HardwarePorts, PortWriter, VgaPortOps};
use crate::write_port_sequence;

/// Write RGB triples for palette setup (DRY helper).
fn write_rgb(writer: &mut PortWriter<u8>, val: u8) {
    for _ in 0..3 {
        writer.write_safe(val);
    }
}

/// Write a register pair to an index/data port pair.
fn write_reg(index_port: u16, data_port: u16, index: u8, value: u8) {
    PortWriter::new(index_port).write_safe(index);
    PortWriter::new(data_port).write_safe(value);
}

/// Reset the attribute controller flip-flop and return VgaPortOps.
fn attr_ops() -> VgaPortOps {
    let _ = PortWriter::<u8>::new(HardwarePorts::STATUS).read_safe();
    VgaPortOps::new(
        HardwarePorts::ATTRIBUTE_INDEX,
        HardwarePorts::ATTRIBUTE_INDEX,
    )
}

/// Write attribute registers and enable video output.
fn write_attr(config: &[crate::io::RegisterConfig]) {
    let mut ops = attr_ops();
    for reg in config {
        ops.write_register(reg.index, reg.value);
    }
    PortWriter::<u8>::new(HardwarePorts::ATTRIBUTE_INDEX).write_safe(0x20u8);
}

fn log_serial(msg: &str) {
    crate::write_serial_bytes(0x3F8, 0x3FD, msg.as_bytes());
}

// ── Public API ──────────────────────────────────────────────────────

pub fn setup_vga_mode_13h() {
    log_serial("VGA setup: Starting mode 13h initialization\n");
    PortWriter::new(HardwarePorts::MISC_OUTPUT).write_safe(0x63u8);
    write_port_sequence!(
        SEQUENCER_CONFIG, HardwarePorts::SEQUENCER_INDEX, HardwarePorts::SEQUENCER_DATA;
        CRTC_CONFIG, HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA;
        GRAPHICS_CONFIG, HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA
    );
    write_attr(ATTRIBUTE_CONFIG);
    setup_palette();
    log_serial("VGA setup: Mode 13h initialization complete\n");
}

pub fn setup_cirrus_vga_mode() {
    log_serial("Cirrus VGA: Starting Cirrus-specific initialization\n");
    setup_vga_mode_13h();
    let mut idx = PortWriter::<u8>::new(0x3C4);
    let mut dat = PortWriter::<u8>::new(0x3C5);
    idx.write_safe(0x06);
    dat.write_safe(0x12);
    idx.write_safe(0x1E);
    dat.write_safe(0x01);
    log_serial("Cirrus VGA: Cirrus-specific initialization complete\n");
}

pub fn detect_and_init_vga_graphics() {
    log_serial("VGA Detection: Starting VGA device detection\n");
    if detect_cirrus_vga() {
        log_serial("VGA Detection: Cirrus VGA device detected, initializing\n");
        setup_cirrus_vga_mode();
    } else {
        log_serial("VGA Detection: Standard VGA device detected, using standard mode\n");
        setup_vga_mode_13h();
    }
}

pub fn detect_cirrus_vga() -> bool {
    const CIRRUS_VID: u16 = 0x1013;
    log_serial("VGA Detection: Checking for Cirrus VGA device\n");
    if crate::bare_metal_pci::pci_config_read_word(0, 2, 0, 0x00) == CIRRUS_VID {
        log_serial("VGA Detection: Cirrus VGA device found via PCI\n");
        return true;
    }
    for bus in 0..2u8 {
        for device in 0..32u8 {
            if crate::bare_metal_pci::pci_config_read_word(bus, device, 0, 0x00) == CIRRUS_VID {
                crate::serial::serial_log(format_args!(
                    "VGA Detection: Cirrus VGA device found at bus:device = {}:{}\n",
                    bus, device
                ));
                return true;
            }
        }
    }
    log_serial("VGA Detection: No Cirrus VGA device found, using standard VGA\n");
    false
}

pub fn setup_vga_text_mode() {
    log_serial("VGA text mode setup: Starting\n");
    PortWriter::new(HardwarePorts::MISC_OUTPUT).write_safe(0x63u8);
    write_port_sequence!(
        SEQUENCER_TEXT_CONFIG, HardwarePorts::SEQUENCER_INDEX, HardwarePorts::SEQUENCER_DATA;
        &[crate::io::RegisterConfig { index: 0x11, value: 0x0E }], HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA;
        CRTC_TEXT_CONFIG, HardwarePorts::CRTC_INDEX, HardwarePorts::CRTC_DATA;
        GRAPHICS_TEXT_CONFIG, HardwarePorts::GRAPHICS_INDEX, HardwarePorts::GRAPHICS_DATA
    );
    // Cursor registers
    write_reg(
        HardwarePorts::CRTC_INDEX,
        HardwarePorts::CRTC_DATA,
        0x0A,
        0x0E,
    );
    write_reg(
        HardwarePorts::CRTC_INDEX,
        HardwarePorts::CRTC_DATA,
        0x0B,
        0x0F,
    );
    write_attr(ATTRIBUTE_TEXT_CONFIG);
    log_serial("VGA text mode setup: Complete\n");
}

pub fn init_vga_graphics() {
    setup_vga_mode_13h();
}
pub fn init_vga_text_mode() {
    setup_vga_text_mode();
}

pub fn setup_palette() {
    let mut dac_idx = PortWriter::<u8>::new(HardwarePorts::DAC_INDEX);
    let mut dac_dat = PortWriter::<u8>::new(HardwarePorts::DAC_DATA);
    dac_idx.write_safe(0x00);
    for i in 0..256 {
        write_rgb(&mut dac_dat, (i * 63 / 255) as u8);
    }
}

pub fn write_vga_registers(index_port: u16, data_port: u16, configs: &[(u8, u8)]) {
    for &(index, value) in configs {
        write_reg(index_port, data_port, index, value);
    }
}

pub fn setup_vga_attributes() {
    write_attr(ATTRIBUTE_TEXT_CONFIG);
}
