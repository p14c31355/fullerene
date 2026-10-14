use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

pub const DEFAULT_BASE: usize = 0x0900_0000;

const UART_FR_TXFF: u32 = 1 << 5;
const UART_CR_UARTEN: u32 = 1 << 0;
const UART_CR_TXE: u32 = 1 << 8;
const UART_CR_RXE: u32 = 1 << 9;

const GENI_FORCE_DEFAULT_REG: usize = 0x20;
const GENI_OUTPUT_CTRL: usize = 0x24;
const SE_GENI_CGC_CTRL: usize = 0x28;
const SE_GENI_DMA_MODE_EN: usize = 0x258;
const SE_GENI_BYTE_GRAN: usize = 0x254;
const SE_GENI_TX_PACKING_CFG0: usize = 0x260;
const SE_GENI_TX_PACKING_CFG1: usize = 0x264;
const SE_GENI_RX_PACKING_CFG0: usize = 0x284;
const SE_GENI_RX_PACKING_CFG1: usize = 0x288;
const SE_GENI_M_CMD0: usize = 0x600;
const SE_GENI_M_CMD_CTRL: usize = 0x604;
const SE_GENI_M_IRQ_STATUS: usize = 0x610;
const SE_GENI_M_IRQ_EN: usize = 0x614;
const SE_GENI_M_IRQ_CLEAR: usize = 0x618;
const SE_GENI_S_CMD_CTRL: usize = 0x634;
const SE_GENI_S_IRQ_EN: usize = 0x644;
const SE_GENI_S_IRQ_CLEAR: usize = 0x648;
const SE_GENI_TX_TRANS_CFG: usize = 0x25c;
const SE_GENI_TX_WORD_LEN: usize = 0x268;
const SE_GENI_TX_STOP_BIT_LEN: usize = 0x26c;
const SE_GENI_TX_TRANS_LEN: usize = 0x270;
const SE_GENI_RX_TRANS_CFG: usize = 0x280;
const SE_GENI_RX_WORD_LEN: usize = 0x28c;
const SE_GENI_TX_FIFO: usize = 0x700;
const SE_GENI_TX_WATERMARK: usize = 0x80c;
const SE_GENI_RX_WATERMARK: usize = 0x810;
const SE_GENI_RX_RFR_WATERMARK: usize = 0x814;
const SE_GSI_EVENT_EN: usize = 0xe18;
const SE_DMA_GENERAL_CFG: usize = 0xe30;
const SE_DMA_TX_IRQ_CLEAR: usize = 0xc44;
const SE_DMA_RX_IRQ_CLEAR: usize = 0xd44;

const GENI_COMMON_M_IRQ_EN: u32 = 0x33c0_007e;
const GENI_COMMON_S_IRQ_EN: u32 = 0x0300_3e3e;
const M_CMD_ABORT: u32 = 1 << 1;
const M_CMD_DONE: u32 = 1;
const M_CMD_ABORT_IRQ: u32 = 1 << 5;
const M_TX_FIFO_WATERMARK: u32 = 1 << 30;
const S_CMD_ABORT: u32 = 1 << 1;
const S_CMD_DONE: u32 = 1;
const S_CMD_ABORT_IRQ: u32 = 1 << 5;
const TX_WATERMARK: u32 = 2;

static BASE: AtomicUsize = AtomicUsize::new(DEFAULT_BASE);
static BACKEND: AtomicU8 = AtomicU8::new(Backend::Pl011 as u8);

#[repr(u8)]
#[derive(Clone, Copy)]
enum Backend {
    Pl011 = 0,
    QcomGeni = 1,
}

/// Initialize a PL011 discovered from the platform DTB.
pub fn init_at(base: u64) {
    BACKEND.store(Backend::Pl011 as u8, Ordering::Relaxed);
    BASE.store(base as usize, Ordering::Relaxed);
    let uart_base = base as usize;
    unsafe {
        write_volatile((uart_base + 0x30) as *mut u32, 0);
        // QEMU virt supplies a 24 MHz PL011 clock.
        write_volatile((uart_base + 0x24) as *mut u32, 13);
        write_volatile((uart_base + 0x28) as *mut u32, 1);
        write_volatile((uart_base + 0x2c) as *mut u32, 0b11 << 5);
        write_volatile(
            (uart_base + 0x30) as *mut u32,
            UART_CR_UARTEN | UART_CR_TXE | UART_CR_RXE,
        );
    }
}

/// Initialize the Qualcomm GENI debug UART used by SM7250's early console.
///
/// The bootloader has already enabled the QUP clocks and loaded the UART
/// protocol firmware. This is the FIFO-mode subset of the Linux earlycon
/// setup: it deliberately avoids clocks, DMA, and interrupt-driven RX.
pub fn init_qcom_geni(base: u64) {
    BACKEND.store(Backend::QcomGeni as u8, Ordering::Relaxed);
    BASE.store(base as usize, Ordering::Relaxed);
    let base = base as usize;
    unsafe {
        poll_tx_done(base);
        abort_secondary_command(base);
        // Reset the FIFO-facing part of the serial engine while preserving
        // the bootloader-provided UART protocol firmware.
        write32(base + SE_GSI_EVENT_EN, 0);
        write32(base + SE_GENI_M_IRQ_CLEAR, u32::MAX);
        write32(base + SE_GENI_S_IRQ_CLEAR, u32::MAX);
        write32(base + SE_DMA_TX_IRQ_CLEAR, u32::MAX);
        write32(base + SE_DMA_RX_IRQ_CLEAR, u32::MAX);
        write32(
            base + SE_GENI_CGC_CTRL,
            read32(base + SE_GENI_CGC_CTRL) | 0x7f,
        );
        write32(
            base + SE_DMA_GENERAL_CFG,
            read32(base + SE_DMA_GENERAL_CFG) | 0x0f,
        );
        write32(base + GENI_OUTPUT_CTRL, 0x7f);
        write32(base + GENI_FORCE_DEFAULT_REG, 1);
        write32(
            base + SE_GENI_DMA_MODE_EN,
            read32(base + SE_GENI_DMA_MODE_EN) & !1,
        );
        write32(base + SE_GSI_EVENT_EN, 0);
        write32(base + SE_GENI_RX_WATERMARK, 8);
        write32(base + SE_GENI_RX_RFR_WATERMARK, 14);
        write32(base + SE_GENI_TX_WATERMARK, TX_WATERMARK);
        write32(
            base + SE_GENI_M_IRQ_EN,
            read32(base + SE_GENI_M_IRQ_EN) | GENI_COMMON_M_IRQ_EN,
        );
        write32(
            base + SE_GENI_S_IRQ_EN,
            read32(base + SE_GENI_S_IRQ_EN) | GENI_COMMON_S_IRQ_EN,
        );

        // One 8-bit word per FIFO command, LSB first. This is the same
        // packing produced by Linux's msm_geni_serial earlycon for
        // se_get_packing_config(8, 1, false): cfg0=0x0f, cfg1=0.
        write32(base + SE_GENI_TX_PACKING_CFG0, 0x0f);
        write32(base + SE_GENI_TX_PACKING_CFG1, 0);
        write32(base + SE_GENI_RX_PACKING_CFG0, 0x0f);
        write32(base + SE_GENI_RX_PACKING_CFG1, 0);
        write32(base + SE_GENI_BYTE_GRAN, 0);

        // 8N1, no parity, ignore CTS/RTS for the early console.
        write32(base + SE_GENI_TX_TRANS_CFG, 1 << 1);
        write32(base + 0x2a4, 0);
        write32(base + SE_GENI_RX_TRANS_CFG, 0);
        write32(base + 0x2a8, 0);
        write32(base + SE_GENI_TX_WORD_LEN, 8);
        write32(base + SE_GENI_RX_WORD_LEN, 8);
        write32(base + SE_GENI_TX_STOP_BIT_LEN, 0);

        // Configure FIFO mode and leave command completion enabled.
        write32(
            base + SE_GENI_M_IRQ_EN,
            read32(base + SE_GENI_M_IRQ_EN) | (1 << 0),
        );
    }
}

pub fn putc(byte: u8) {
    // Tee first, device second: the durable log must exist even when the UART
    // itself wedges (the failure this device keeps producing).
    crate::blackbox::text_byte(byte);
    // v174: every console byte also tends the loader watchdog. Narration is
    // the densest non-storage activity, and unlike the timer-IRQ feed this
    // works on boots where the timer never came up.
    //
    // v211: DISABLED on the ANE build. The launchd-entry stall's ELR sits
    // inside this function (historically 0x1BFD1A4/0x1BFD1C0) and the ONLY
    // ANE-side code after the ring tee is this SP805 read: on the ANE path
    // the hardware UART is already skipped ("a stalled bus access cannot
    // be bounded"), but the watchdog_tend() read/write at 0xe8a06000 was
    // not. The main thread freezes on its next console byte right at the
    // launchd entry (the ring's last line is "launchd: pre t=" with no
    // newline), while the timer IRQ keeps running - the IRQ's own SP805
    // feed is a write-only kick, so a read here is the one access class
    // that can stall. The SP805 is still fed by the timer IRQ (beats % 8),
    // the USB path and the storage loops (their own watchdog_tend calls).
    #[cfg(all(fullerene_aarch64_ane, not(fullerene_ane_no_putc_wd_tend)))]
    super::platform::ane::watchdog_tend();
    let uart_base = BASE.load(Ordering::Relaxed);
    match BACKEND.load(Ordering::Relaxed) {
        value if value == Backend::QcomGeni as u8 => unsafe {
            // Use the same FIFO-mode sequence as the Qualcomm early console:
            // arm the watermark, start one-byte TX, wait until FIFO space is
            // advertised, write the byte, then wait for command completion.
            write32(uart_base + SE_GENI_M_IRQ_CLEAR, M_CMD_DONE);
            write32(uart_base + SE_GENI_TX_WATERMARK, TX_WATERMARK);
            write32(uart_base + SE_GENI_TX_TRANS_LEN, 1);
            write32(uart_base + SE_GENI_M_CMD0, 1 << 27);
            let _ = poll_m_irq(uart_base, M_TX_FIFO_WATERMARK);
            write32(uart_base + SE_GENI_TX_FIFO, byte as u32);
            write32(uart_base + SE_GENI_M_IRQ_CLEAR, M_TX_FIFO_WATERMARK);
            poll_tx_done(uart_base);
        },
        _ => {
            // The ANE's PL011 window has no observable console attached, so
            // the hardware half is best-effort everywhere. On the ANE build
            // it is skipped entirely: a stalled bus access (an unclocked
            // block - the v39 dwmmc lesson) cannot be bounded by a spin
            // count, and the ring tee above already holds the byte.
            // Evidence trail: the launchd-entry stall's ELR sits inside this
            // function (0x1BFD1A4/0x1BFD1C0) and the v140 bounded-spin fix
            // did not cure it (v143 photo), which points at the MMIO access
            // itself rather than the wait.
            #[cfg(not(fullerene_aarch64_ane))]
            unsafe {
                let mut spins = 0u32;
                while read_volatile((uart_base + 0x18) as *const u32) & UART_FR_TXFF != 0 {
                    spins += 1;
                    if spins > 200_000 {
                        break;
                    }
                    core::hint::spin_loop();
                }
                write_volatile(uart_base as *mut u32, byte as u32);
            }
            #[cfg(fullerene_aarch64_ane)]
            let _ = uart_base;
        }
    }
}

unsafe fn poll_m_irq(base: usize, bit: u32) -> bool {
    unsafe {
        for _ in 0..1_000_000 {
            if read32(base + SE_GENI_M_IRQ_STATUS) & bit != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}

unsafe fn poll_register_clear(base: usize, offset: usize, bit: u32) -> bool {
    unsafe {
        for _ in 0..1_000_000 {
            if read32(base + offset) & bit == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}

unsafe fn abort_secondary_command(base: usize) {
    unsafe {
        write32(base + SE_GENI_S_CMD_CTRL, S_CMD_ABORT);
        let _ = poll_register_clear(base, SE_GENI_S_CMD_CTRL, S_CMD_ABORT);
        write32(base + SE_GENI_S_IRQ_CLEAR, S_CMD_DONE | S_CMD_ABORT_IRQ);
        write32(base + GENI_FORCE_DEFAULT_REG, 1);
    }
}

unsafe fn poll_tx_done(base: usize) {
    unsafe {
        if poll_m_irq(base, M_CMD_DONE) {
            write32(base + SE_GENI_M_IRQ_CLEAR, M_CMD_DONE);
            return;
        }

        // A stuck command must not wedge every later panic/diagnostic print.
        write32(base + SE_GENI_M_CMD_CTRL, M_CMD_ABORT);
        let _ = poll_m_irq(base, M_CMD_ABORT_IRQ);
        write32(base + SE_GENI_M_IRQ_CLEAR, M_CMD_ABORT_IRQ);
    }
}

unsafe fn read32(address: usize) -> u32 {
    unsafe { read_volatile(address as *const u32) }
}

unsafe fn write32(address: usize, value: u32) {
    unsafe { write_volatile(address as *mut u32, value) };
}

/// Read the flag register (base+0x18) for diagnostics: bit 5 = TXFF (FIFO
/// full - the bit the putc spin waits on), bit 3 = BUSY. The launchd-entry
/// stall investigation samples this before and after a short write burst to
/// tell "FIFO was already full before launchd" from "the burst filled it and
/// the drain is dead".
pub fn fr_state() -> u64 {
    let uart_base = BASE.load(Ordering::Relaxed);
    unsafe { read_volatile((uart_base + 0x18) as *const u32) as u64 }
}

pub fn puts(message: &str) {
    for byte in message.bytes() {
        if byte == b'\n' {
            putc(b'\r');
        }
        putc(byte);
    }
}

pub fn put_hex(label: &str, value: u64) {
    puts(label);
    put_hex_value(value);
}

/// The label + hex value + newline - the common diagnostic line shape.
pub fn put_hexln(label: &str, value: u64) {
    put_hex(label, value);
    puts("\n");
}

pub fn put_hex_value(value: u64) {
    puts("0x");
    for shift in (0..16).rev() {
        let nibble = ((value >> (shift * 4)) & 0xf) as u8;
        putc(match nibble {
            0..=9 => b'0' + nibble,
            _ => b'a' + (nibble - 10),
        });
    }
    putc(b'\n');
}

/// Eight hex digits and no newline: the compact single-line form the ANE
/// one-photo status report uses for 32-bit registers, where two values plus
/// labels must fit one 45-character console line.
pub fn put_hex32(label: &str, value: u32) {
    puts(label);
    for shift in (0..8).rev() {
        let nibble = ((value >> (shift * 4)) & 0xf) as u8;
        putc(match nibble {
            0..=9 => b'0' + nibble,
            _ => b'a' + (nibble - 10),
        });
    }
}
