use x86_64::instructions::port::Port;

/// Generic port writer struct to reduce unsafe block repetition and improve type safety
pub struct PortWriter<T> {
    port: Port<T>,
}

impl<T> PortWriter<T> {
    pub fn new(port_addr: u16) -> Self {
        Self {
            port: Port::new(port_addr),
        }
    }

    pub unsafe fn write(&mut self, value: T)
    where
        T: x86_64::instructions::port::PortWrite,
    {
        unsafe {
            self.port.write(value);
        }
    }

    pub unsafe fn read(&mut self) -> T
    where
        T: x86_64::instructions::port::PortRead,
    {
        unsafe { self.port.read() }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RegisterConfig {
    pub index: u8,
    pub value: u8,
}

// Specialized VGA port operations
pub struct VgaPortOps {
    index_writer: PortWriter<u8>,
    data_writer: PortWriter<u8>,
}

impl VgaPortOps {
    pub fn new(index_port: u16, data_port: u16) -> Self {
        Self {
            index_writer: PortWriter::new(index_port),
            data_writer: PortWriter::new(data_port),
        }
    }

    pub fn write_register(&mut self, index: u8, value: u8) {
        unsafe {
            self.index_writer.write(index);
            self.data_writer.write(value);
        }
    }

    pub fn write_sequence(&mut self, configs: &[RegisterConfig]) {
        for reg in configs {
            self.write_register(reg.index, reg.value);
        }
    }
}

// VGA port addresses
pub struct VgaPorts;

impl VgaPorts {
    pub const MISC_OUTPUT: u16 = 0x3C2;
    pub const CRTC_INDEX: u16 = 0x3D4;
    pub const CRTC_DATA: u16 = 0x3D5;
    pub const STATUS: u16 = 0x3DA;
    pub const ATTRIBUTE_INDEX: u16 = 0x3C0;
    pub const DAC_INDEX: u16 = 0x3C8;
    pub const DAC_DATA: u16 = 0x3C9;
    pub const GRAPHICS_INDEX: u16 = 0x3CE;
    pub const GRAPHICS_DATA: u16 = 0x3CF;
    pub const SEQUENCER_INDEX: u16 = 0x3C4;
    pub const SEQUENCER_DATA: u16 = 0x3C5;
}

// Enhanced macro for writing port sequences with automatic port management
#[macro_export]
macro_rules! write_port_sequence {
    ($($config:expr, $index_port:expr, $data_port:expr);*$(;)?) => {{
        $(
            let mut vga_ports = $crate::VgaPortOps::new($index_port, $data_port);
            vga_ports.write_sequence($config);
        )*
    }};
}

// Simplified macro for single register writes
#[macro_export]
macro_rules! write_vga_register {
    ($index_port:expr, $data_port:expr, $index:expr, $data:expr) => {{
        let mut vga_ports = $crate::VgaPortOps::new($index_port, $data_port);
        vga_ports.write_register($index, $data);
    }};
}
