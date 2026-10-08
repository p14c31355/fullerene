//! Reusable I²C register-device contracts.
//!
//! Platform adapters implement the bus operations.  Device drivers must not
//! know the controller's MMIO layout, clock gates, or reset registers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum I2cError {
    NotReady,
    AddressNack,
    DataNack,
    ArbitrationLost,
    Timeout,
    BusFault,
    BufferTooSmall,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterAddressWidth {
    OneByte,
    TwoBytesBigEndian,
}

/// Minimal synchronous contract used by small register-oriented devices.
pub trait I2cBus {
    fn write_read(&mut self, address: u8, register: &[u8], read: &mut [u8])
    -> Result<(), I2cError>;

    fn write(&mut self, address: u8, payload: &[u8]) -> Result<(), I2cError>;
}

/// Register-device helper that keeps address framing out of individual
/// drivers.  The caller supplies the controller-owned bus implementation.
pub struct RegisterDevice<'a, B: I2cBus> {
    bus: &'a mut B,
    address: u8,
    width: RegisterAddressWidth,
}

impl<'a, B: I2cBus> RegisterDevice<'a, B> {
    pub fn new(bus: &'a mut B, address: u8, width: RegisterAddressWidth) -> Self {
        Self {
            bus,
            address,
            width,
        }
    }

    pub fn read(&mut self, register: u16, output: &mut [u8]) -> Result<(), I2cError> {
        let frame = self.register_frame(register);
        self.bus
            .write_read(self.address, &frame[..self.address_width()], output)
    }

    pub fn write(&mut self, register: u16, payload: &[u8]) -> Result<(), I2cError> {
        let frame = self.register_frame(register);
        let mut packet = [0u8; 258];
        let frame_len = self.address_width();
        let total = frame_len + payload.len();
        if total > packet.len() {
            return Err(I2cError::BufferTooSmall);
        }
        packet[..frame_len].copy_from_slice(&frame[..frame_len]);
        packet[frame_len..total].copy_from_slice(payload);
        self.bus.write(self.address, &packet[..total])
    }

    fn register_frame(&self, register: u16) -> [u8; 2] {
        match self.width {
            RegisterAddressWidth::OneByte => [register as u8, 0],
            RegisterAddressWidth::TwoBytesBigEndian => register.to_be_bytes(),
        }
    }

    fn address_width(&self) -> usize {
        match self.width {
            RegisterAddressWidth::OneByte => 1,
            RegisterAddressWidth::TwoBytesBigEndian => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBus {
        write_read_register: [u8; 2],
        write_packet: [u8; 4],
        write_len: usize,
    }

    impl I2cBus for MockBus {
        fn write_read(
            &mut self,
            _address: u8,
            register: &[u8],
            read: &mut [u8],
        ) -> Result<(), I2cError> {
            self.write_read_register = [0; 2];
            self.write_read_register[..register.len()].copy_from_slice(register);
            read.fill(0x5a);
            Ok(())
        }

        fn write(&mut self, _address: u8, payload: &[u8]) -> Result<(), I2cError> {
            self.write_len = payload.len();
            self.write_packet[..payload.len()].copy_from_slice(payload);
            Ok(())
        }
    }

    #[test]
    fn frames_goodix_registers_big_endian() {
        let mut bus = MockBus {
            write_read_register: [0; 2],
            write_packet: [0; 4],
            write_len: 0,
        };
        let mut device =
            RegisterDevice::new(&mut bus, 0x38, RegisterAddressWidth::TwoBytesBigEndian);
        let mut data = [0; 2];
        device.read(0x814e, &mut data).unwrap();
        assert_eq!(bus.write_read_register, [0x81, 0x4e]);
        assert_eq!(data, [0x5a, 0x5a]);
        device.write(0x8041, &[1, 2]).unwrap();
        assert_eq!(bus.write_len, 4);
        assert_eq!(bus.write_packet, [0x80, 0x41, 1, 2]);
    }

    #[test]
    fn frames_one_byte_registers_without_a_padding_byte() {
        let mut bus = MockBus {
            write_read_register: [0; 2],
            write_packet: [0; 4],
            write_len: 0,
        };
        let mut device = RegisterDevice::new(&mut bus, 0x50, RegisterAddressWidth::OneByte);
        let mut data = [0; 1];
        device.read(0x12, &mut data).unwrap();
        assert_eq!(bus.write_read_register, [0x12, 0]);
        device.write(0x34, &[0xaa]).unwrap();
        assert_eq!(bus.write_len, 2);
        assert_eq!(&bus.write_packet[..2], &[0x34, 0xaa]);
    }
}
