//! Sound / Audio subsystem for Fullerene OS.
//!
//! Provides basic PC speaker beep and an HDA (Intel High Definition Audio)
//! probe framework.  Full audio playback (BADAPPLE) requires a working
//! HDA stream with buffer queueing, which is scaffolded here.
//!
//! # Architecture
//!
//! ```
//! PC Speaker (PIT mode 3 → square wave)
//!     ↓
//! HDA probe (PCI class 0x04, subclass 0x03)
//!     → CORB/RIRB setup
//!     → Stream DMA
//! ```
//!
//! # BADAPPLE Goal
//!
//! The BADAPPLE audio playback target needs:
//! 1. HDA codec enumeration via CORB/RIRB
//! 2. Stream descriptor with DMA buffer
//! 3. PCM samples fed at 44.1 kHz (or 48 kHz) in 16-bit stereo
//! 4. Frame-synchronised output (vsync + audio timing)
//!
//! This module provides the probe + beep foundations; the HDA stream
//! driver is the next step toward BADAPPLE.

use nitrogen::pci::PciDevice;

/// Play a simple beep using the PC speaker (PIT channel 2).
///
/// # Safety
///
/// Direct I/O port writes.  Caller must ensure single-threaded access.
pub fn pc_speaker_beep(frequency_hz: u32, duration_ms: u32) {
    if frequency_hz == 0 {
        return;
    }

    unsafe {
        // PIT frequency divisor
        let divisor = (1_193_182u32 / frequency_hz).min(65535) as u16;

        // Set PIT channel 2 to mode 3 (square wave generator)
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x43).write(0xB6);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write(divisor as u8);
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x42).write((divisor >> 8) as u8);

        // Enable PC speaker (bit 0 and bit 1 of port 0x61)
        let tmp = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(tmp | 0x03);

        // Wait for the duration
        for _ in 0..duration_ms * 1000 {
            core::hint::spin_loop();
        }

        // Disable PC speaker
        let tmp2 = x86_64::instructions::port::PortReadOnly::<u8>::new(0x61).read();
        x86_64::instructions::port::PortWriteOnly::<u8>::new(0x61).write(tmp2 & !0x03);
    }
}

/// HDA controller info.
#[derive(Debug, Clone)]
pub struct HdaController {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub mmio_base: u64,
}

/// Probe the PCI bus for an HDA (Intel HD Audio) controller.
///
/// Returns the controller info if found, or `None` if no HDA present.
pub fn probe_hda() -> Option<HdaController> {
    // Re-scanning would consume; for simplicity, use a direct scan
    // through the PCI device abstraction.
    for bus in 0..=0u8 {
        for dev in 0..=31u8 {
            if let Some(device) = PciDevice::new(bus, dev, 0) {
                // HD Audio: class 0x04, subclass 0x03
                if device.class_code == 0x04 && device.subclass == 0x03 {
                    if let Some(bar0) = device.read_bar(0) {
                        log::info!(
                            "HDA controller found at {:02x}:{:02x}.{:x}, MMIO=0x{:x}",
                            bus, dev, 0, bar0
                        );
                        return Some(HdaController {
                            bus,
                            device: dev,
                            function: 0,
                            mmio_base: bar0,
                        });
                    }
                }
            }
        }
    }
    None
}

/// Initialize the sound subsystem.
///
/// Currently probes for HDA and logs the result.
pub fn init() {
    match probe_hda() {
        Some(hda) => {
            log::info!("Sound: HDA controller at 0x{:x}", hda.mmio_base);
        }
        None => {
            log::info!("Sound: No HDA controller found (PC speaker only)");
        }
    }
}