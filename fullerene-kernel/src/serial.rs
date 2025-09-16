use x86_64::instructions::port::Port;
use spin::Mutex;

pub struct SerialPort {
    data: Port<u8>,
    irq_enable: Port<u8>,
    fifo_ctrl: Port<u8>,
    line_ctrl: Port<u8>,
    modem_ctrl: Port<u8>,
    line_status: Port<u8>,
}

impl SerialPort {
    pub const fn new() -> SerialPort {
        SerialPort {
            data: Port::new(0x3F8),
            irq_enable: Port::new(0x3F9),
            fifo_ctrl: Port::new(0x3FA),
            line_ctrl: Port::new(0x3FB),
            modem_ctrl: Port::new(0x3FC),
            line_status: Port::new(0x3FD),
        }
    }

    pub fn init(&mut self) {
        unsafe {
            self.line_ctrl.write(0x80); // Enable DLAB
            self.data.write(0x03);      // Baud rate divisor low byte (38400 bps)
            self.irq_enable.write(0x00); 
            self.line_ctrl.write(0x03); // 8 bits, no parity, one stop bit
            self.fifo_ctrl.write(0xC7); // Enable FIFO, clear, 14-byte threshold
            self.modem_ctrl.write(0x0B); // IRQs enabled, OUT2
        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        unsafe {
            while (self.line_status.read() & 0x20) == 0 {}
            self.data.write(byte);
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for b in s.bytes() {
            self.write_byte(b);
        }
    }
}

static SERIAL1: Mutex<SerialPort> = Mutex::new(SerialPort::new());

pub fn serial_init() {
    SERIAL1.lock().init();
}

pub fn serial_log(s: &str) {
    SERIAL1.lock().write_string(s);
}
