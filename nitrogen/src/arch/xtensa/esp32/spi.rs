//! Hardware-SPI (HSPI / SPI2) transport for the ESP32 LCD.
//!
//! This replaced a bit-banged shifter. Every register offset and bit position
//! below is taken from ESP-IDF (v5.3) rather than guessed:
//!
//! - `soc/esp32/include/soc/reg_base.h`: `DR_REG_SPI2_BASE = 0x3ff64000`,
//!   `DR_REG_DPORT_BASE = 0x3ff00000`.
//! - `soc/esp32/include/soc/spi_reg.h`: register offsets and field positions.
//! - `soc/esp32/include/soc/dport_reg.h`: `DPORT_PERIP_CLK_EN_REG` (0xc0),
//!   `DPORT_SPI2_CLK_EN` (bit 6).
//! - `hal/esp32/include/hal/spi_ll.h`: the transaction shape
//!   (`usr_mosi_dbitlen = bitlen - 1`, W0..W15 staging with a 4-byte
//!   little-endian copy per register, poll `SPI_CMD.USR` back to zero) and the
//!   clock-divider search this module ports.
//! - `soc/esp32/include/soc/io_mux_reg.h`: GPIO13/14 take HSPI on MCU_SEL 1.
//!
//! Two properties matter beyond raw speed:
//!
//! 1. The data phase is staged into the peripheral and shifted out by
//!    hardware, so the CPU is free between chunks.
//! 2. `write_data_paced` hands control back to the caller between chunks, so
//!    the input path keeps running while a large transfer is in flight. That
//!    is the mechanism that keeps a display flush from stalling touch.
//!
//! The chip-select line stays a plain GPIO: the peripheral's CS0 signal is
//! disabled (`SPI_PIN.SPI_CS0_DIS`) and the pad is never switched to
//! `FUNC_MTDO_HSPICS0`, which leaves command/data framing under software
//! control through the DC line.

use super::{board::BoardProfile, gpio};

const HSPI_BASE: usize = 0x3ff6_4000;
const HSPI_CMD: usize = HSPI_BASE + 0x00;
const HSPI_CTRL: usize = HSPI_BASE + 0x08;
const HSPI_CLOCK: usize = HSPI_BASE + 0x18;
const HSPI_USER: usize = HSPI_BASE + 0x1c;
const HSPI_MOSI_DLEN: usize = HSPI_BASE + 0x28;
const HSPI_PIN: usize = HSPI_BASE + 0x34;
const HSPI_SLAVE: usize = HSPI_BASE + 0x38;
const HSPI_W0: usize = HSPI_BASE + 0x80;

/// DPORT peripheral clock-enable register (`DPORT_PERIP_CLK_EN_REG`).
const DPORT_PERIP_CLK_EN: usize = 0x3ff0_00c0;
/// `DPORT_SPI2_CLK_EN` (bit 6).
const DPORT_SPI2_CLK_EN: u32 = 1 << 6;

/// `SPI_CMD.SPI_USR` (bit 18): set by software to start, cleared by hardware
/// when the transaction completes.
const SPI_USR: u32 = 1 << 18;
/// `SPI_USER.SPI_USR_MOSI` (bit 27): the transaction has a MOSI phase.
const SPI_USR_MOSI: u32 = 1 << 27;
/// `SPI_PIN` bits.
const SPI_CS0_DIS: u32 = 1 << 0;
const SPI_CK_IDLE_EDGE: u32 = 1 << 29;
/// `SPI_CTRL.SPI_WR_BIT_ORDER` (bit 26). The reference header is explicit:
/// "In command address write-data (MOSI) phases 1: LSB first 0: MSB first",
/// and its reset value is 0. The panel needs MSB first, so this bit must stay
/// clear - setting it reverses every bit of every byte and the controller
/// never sees a valid command, which presents as a lit but blank panel.
const SPI_WR_BIT_ORDER: u32 = 1 << 26;
/// `SPI_CLOCK` fields.
const SPI_CLK_EQU_SYSCLK: u32 = 1 << 31;
const SPI_CLKDIV_PRE_S: u32 = 18;
const SPI_CLKCNT_N_S: u32 = 12;
const SPI_CLKCNT_H_S: u32 = 6;

/// The APB clock feeding the SPI peripheral on this board.
pub const APB_CLOCK_HZ: u32 = 80_000_000;

/// Bytes staged per hardware transaction: the CPU-side buffer is W0..W15.
pub const HSPI_CHUNK_BYTES: usize = 64;

/// The starting clock. The ILI9341 write cycle is 66 ns (about 15 MHz), so
/// this deliberately stays under it: the panel sits on jumper wires, and signal
/// integrity on loose wiring matters more than the last few megahertz. It is
/// still roughly ten times the bit-banged path. Raise it only with the panel
/// showing a correct image at this rate first.
pub const DEFAULT_SPI_HZ: u32 = 8_000_000;

/// Bound on the wait for `SPI_CMD.USR` to clear. A 64-byte transfer at the
/// slowest clock this module can produce is a few microseconds; this is far
/// above that and exists so a wedged peripheral fails loudly instead of
/// hanging the cooperative scheduler forever.
const TRANSACTION_TIMEOUT_SPINS: u32 = 2_000_000;

pub struct SpiController {
    sclk: u8,
    mosi: u8,
    dc: u8,
    cs: u8,
    rst: u8,
    hz: u32,
}

impl SpiController {
    pub const fn new() -> Self {
        Self {
            sclk: 0,
            mosi: 0,
            dc: 0,
            cs: 0,
            rst: 0,
            hz: DEFAULT_SPI_HZ,
        }
    }

    pub const fn clock_hz(&self) -> u32 {
        self.hz
    }

    pub fn configure(&mut self, profile: &BoardProfile) -> Result<(), SpiError> {
        self.configure_at(profile, DEFAULT_SPI_HZ)
    }

    /// Bring up HSPI with the board profile's pins.
    pub fn configure_at(&mut self, profile: &BoardProfile, hz: u32) -> Result<(), SpiError> {
        if profile.lcd.spi_host != 2 {
            // The board profile names the peripheral; only SPI2/HSPI is wired
            // for direct IO_MUX routing on these pins.
            return Err(SpiError::UnsupportedHost);
        }
        self.sclk = profile.lcd.sclk;
        self.mosi = profile.lcd.mosi;
        self.dc = profile.lcd.dc;
        self.cs = profile.lcd.cs;
        self.rst = profile.lcd.rst;
        self.hz = hz;

        unsafe {
            // Enable the SPI2 peripheral clock before touching its registers.
            let enable = core::ptr::read_volatile(DPORT_PERIP_CLK_EN as *const u32);
            core::ptr::write_volatile(DPORT_PERIP_CLK_EN as *mut u32, enable | DPORT_SPI2_CLK_EN);
        }

        // SCLK and MOSI are routed to the peripheral by the IO_MUX function
        // select (GPIO14 = HSPICLK on MCU_SEL 1, GPIO13 = HSPID on MCU_SEL 1),
        // which is the shortest path and needs no GPIO-matrix entry.
        gpio::enable_output_function(self.sclk, 1);
        gpio::enable_output_function(self.mosi, 1);
        // CS, DC and RST stay software GPIOs.
        gpio::enable_output(self.cs);
        gpio::enable_output(self.dc);
        gpio::enable_output(self.rst);
        gpio::set_output_high(self.cs);
        gpio::set_output_low(self.dc);
        gpio::set_output_high(self.rst);
        // Mode 0: the clock idles low and the first data bit is valid before
        // the first rising edge.
        let pin = SPI_CS0_DIS;
        unsafe {
            core::ptr::write_volatile(HSPI_PIN as *mut u32, pin & !SPI_CK_IDLE_EDGE);
            // Master mode: SLV_SLAVE_MODE clear.
            let slave = core::ptr::read_volatile(HSPI_SLAVE as *const u32);
            core::ptr::write_volatile(HSPI_SLAVE as *mut u32, slave & !(1 << 30));
            // MSB first on write, and no command/address/dummy phases: the
            // transaction is a bare MOSI burst. Every other USER bit stays
            // clear (no duplex, no SIO, CPHA = 0 for mode 0, no CS hold/setup
            // because software owns the chip select).
            let ctrl = core::ptr::read_volatile(HSPI_CTRL as *const u32);
            core::ptr::write_volatile(HSPI_CTRL as *mut u32, ctrl & !SPI_WR_BIT_ORDER);
            core::ptr::write_volatile(HSPI_USER as *mut u32, SPI_USR_MOSI);
            core::ptr::write_volatile(
                HSPI_CLOCK as *mut u32,
                clock_divider_register(APB_CLOCK_HZ, hz, 128),
            );
        }
        Ok(())
    }

    pub fn select(&self) {
        gpio::set_output_low(self.cs);
    }

    pub fn deselect(&self) {
        gpio::set_output_high(self.cs);
    }

    /// Shift one byte out with the data/command line low.
    pub fn write_command(&mut self, command: u8) -> Result<(), SpiError> {
        self.select();
        gpio::set_output_low(self.dc);
        let result = self.transact(&[command]);
        gpio::set_output_high(self.dc);
        self.deselect();
        result
    }

    /// Shift a buffer out with the data/command line high.
    pub fn write_data(&mut self, data: &[u8]) -> Result<(), SpiError> {
        self.select();
        gpio::set_output_high(self.dc);
        let result = self.write_chunks(data, None);
        self.deselect();
        result
    }

    /// Shift a buffer out, calling `pace` between chunks.
    ///
    /// The caller uses this to keep input polling alive during a large
    /// transfer: a chunk is 64 bytes, so at 20 MHz the callback runs roughly
    /// every 26 us and a whole 320x240 frame never runs unattended for more
    /// than one chunk.
    pub fn write_data_paced(
        &mut self,
        data: &[u8],
        pace: &mut dyn FnMut(),
    ) -> Result<(), SpiError> {
        self.select();
        gpio::set_output_high(self.dc);
        let result = self.write_chunks(data, Some(pace));
        self.deselect();
        result
    }

    pub fn transfer(&mut self, data: &[u8]) -> Result<(), SpiError> {
        self.write_data(data)
    }

    fn write_chunks(
        &mut self,
        data: &[u8],
        mut pace: Option<&mut dyn FnMut()>,
    ) -> Result<(), SpiError> {
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + HSPI_CHUNK_BYTES).min(data.len());
            self.transact(&data[offset..end])?;
            offset = end;
            if let Some(callback) = pace.as_mut() {
                callback();
            }
        }
        Ok(())
    }

    /// Stage up to 64 bytes and run one peripheral transaction.
    fn transact(&mut self, bytes: &[u8]) -> Result<(), SpiError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > HSPI_CHUNK_BYTES {
            return Err(SpiError::TooLong);
        }
        unsafe {
            // Stage four bytes per W register with a little-endian load, which
            // is what `spi_ll_write_buffer` does; the peripheral then shifts
            // the bytes out in memory order.
            let mut word_index = 0;
            while word_index * 4 < bytes.len() {
                let mut word = 0u32;
                let mut lane = 0;
                while lane < 4 && word_index * 4 + lane < bytes.len() {
                    word |= u32::from(bytes[word_index * 4 + lane]) << (8 * lane);
                    lane += 1;
                }
                core::ptr::write_volatile((HSPI_W0 + word_index * 4) as *mut u32, word);
                word_index += 1;
            }
            // `spi_ll_set_mosi_bitlen`: the register holds bitlen - 1.
            core::ptr::write_volatile(
                HSPI_MOSI_DLEN as *mut u32,
                (bytes.len() as u32) * 8 - 1,
            );
            core::ptr::write_volatile(HSPI_CMD as *mut u32, SPI_USR);
        }

        let mut spins = 0u32;
        while unsafe { core::ptr::read_volatile(HSPI_CMD as *const u32) } & SPI_USR != 0 {
            spins += 1;
            if spins > TRANSACTION_TIMEOUT_SPINS {
                return Err(SpiError::Timeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }
}

/// The `SPI_CLOCK` value for the requested frequency.
///
/// The arithmetic itself lives in [`crate::spi_clock`], where it is covered by
/// host tests; this wrapper supplies the ESP32 field positions.
pub fn clock_divider_register(bus_hz: u32, hz: u32, duty_cycle: u32) -> u32 {
    crate::spi_clock::solve(
        bus_hz,
        hz,
        duty_cycle,
        SPI_CLKDIV_PRE_S,
        SPI_CLKCNT_N_S,
        SPI_CLKCNT_H_S,
        SPI_CLK_EQU_SYSCLK,
    )
    .register
}

/// The frequency the register value actually produces.
pub fn clock_frequency_hz(bus_hz: u32, register: u32) -> u32 {
    if register & SPI_CLK_EQU_SYSCLK != 0 {
        return bus_hz;
    }
    let prescale = ((register >> SPI_CLKDIV_PRE_S) & 0x1fff) + 1;
    let divider = ((register >> SPI_CLKCNT_N_S) & 0x3f) + 1;
    bus_hz / (prescale * divider)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpiError {
    Busy,
    Timeout,
    TooLong,
    UnsupportedHost,
}
