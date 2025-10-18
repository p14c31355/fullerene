/// Generic port writer struct to reduce unsafe block repetition and improve type safety
/// Centralizes port operations to minimize unsafe code usage
pub struct PortWriter<T> {
    port: x86_64::instructions::port::Port<T>,
}

impl<T> PortWriter<T> {
    pub fn new(port_addr: u16) -> Self {
        Self {
            port: x86_64::instructions::port::Port::new(port_addr),
        }
    }

    /// Safe wrapper for port writes - requires T: PortWrite trait
    pub fn write_safe(&mut self, value: T)
    where
        T: Copy + x86_64::instructions::port::PortWrite,
    {
        unsafe {
            self.port.write(value);
        }
    }

    /// Safe wrapper for port reads - requires T: PortRead trait
    pub fn read_safe(&mut self) -> T
    where
        T: x86_64::instructions::port::PortRead,
    {
        unsafe { self.port.read() }
    }
}

/// Generic helper for processing I/O port sequences with automatic type safety
pub trait PortOperations {
    fn write_multiple<T: Copy + x86_64::instructions::port::PortWrite>(
        &mut self,
        configs: &[(u8, T)],
    );
    fn write_sequence_u8(&mut self, index_port: u16, data_port: u16, configs: &[(u8, u8)]);
}

impl PortOperations for () {
    fn write_multiple<T: Copy + x86_64::instructions::port::PortWrite>(
        &mut self,
        _configs: &[(u8, T)],
    ) {
        // Global implementation for sequence operations
    }

    fn write_sequence_u8(&mut self, index_port: u16, data_port: u16, configs: &[(u8, u8)]) {
        let mut idx_writer = PortWriter::new(index_port);
        let mut data_writer = PortWriter::new(data_port);
        for &(index, value) in configs {
            idx_writer.write_safe(index);
            data_writer.write_safe(value);
        }
    }
}

/// Macro to safely write to a port with automatic type deduction
#[macro_export]
macro_rules! port_write {
    ($port_addr:expr, $value:expr) => {{
        let mut writer = $crate::PortWriter::new($port_addr);
        writer.write_safe($value);
    }};
}

/// Macro to safely read from a port with automatic type deduction
#[macro_export]
macro_rules! port_read_u8 {
    ($port_addr:expr) => {{
        let mut reader = $crate::PortWriter::new($port_addr);
        reader.read_safe()
    }};
}

#[macro_export]
macro_rules! port_read {
    ($port_addr:expr) => {
        port_read_u8!($port_addr)
    };
}

/// Generic port sequence writer trait
pub trait PortSequenceWriter<T> {
    fn write_sequence(&mut self, values: &[T]);
}

impl<T: Copy + x86_64::instructions::port::PortWrite> PortSequenceWriter<T> for x86_64::instructions::port::Port<T> {
    fn write_sequence(&mut self, values: &[T]) {
        for &value in values {
            unsafe { self.write(value) };
        }
    }
}

/// Generic MSR (Model-Specific Register) operations wrapper
pub struct MsrHelper {
    index: u32,
}

impl MsrHelper {
    pub fn new(index: u32) -> Self {
        Self { index }
    }

    pub fn read(&self) -> u64 {
        unsafe { x86_64::registers::model_specific::Msr::new(self.index).read() }
    }

    pub fn write(&self, value: u64) {
        unsafe { x86_64::registers::model_specific::Msr::new(self.index).write(value) }
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
        self.index_writer.write_safe(index);
        self.data_writer.write_safe(value);
    }

    pub fn write_sequence(&mut self, configs: &[RegisterConfig]) {
        for reg in configs {
            self.write_register(reg.index, reg.value);
        }
    }
}

// Hardware port addresses - renamed from VgaPorts to HardwarePorts for generality
pub struct HardwarePorts;

impl HardwarePorts {
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

    // PCI Configuration Space ports
    pub const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
    pub const PCI_CONFIG_DATA: u16 = 0xCFC;

    // VGA CRTC register indices
    pub const CURSOR_POS_LOW_REG: u8 = 0x0F;
    pub const CURSOR_POS_HIGH_REG: u8 = 0x0E;
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
