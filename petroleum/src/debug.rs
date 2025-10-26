//! Debug utilities for stack unwinding and symbol resolution
//!
//! This module provides functionality for capturing stack backtraces
//! and resolving addresses to file names and line numbers.

use core::arch::asm;
use core::fmt::{self, Write};

/// A simple backtrace entry
#[derive(Debug, Clone, Copy, Default)]
pub struct BacktraceEntry {
    pub ip: u64,
    pub sp: u64,
    pub symbol: Option<&'static str>,
    pub file: Option<&'static str>,
    pub line: Option<u32>,
}

/// A basic backtrace collector
#[derive(Default)]
pub struct BacktraceCollector {
    entries: [BacktraceEntry; 32],
    count: usize,
}

impl BacktraceCollector {
    pub fn new() -> Self {
        Default::default()
    }

    /// Capture a basic stack backtrace using frame pointers
    ///
    /// This is a simplified implementation that works in no_std environments
    /// without full DWARF parsing. It relies on frame pointers being enabled.
    pub fn capture(&mut self) {
        let mut rbp: *const usize;
        unsafe {
            asm!("mov {}, rbp", out(reg) rbp);
        }

        let mut frame = rbp;
        let mut i = 0;

        while !frame.is_null() && i < self.entries.len() {
            unsafe {
                // Read return address from stack frame
                let return_addr = *frame.offset(1);
                let next_frame = *frame;

                if return_addr == 0 {
                    break;
                }

                self.entries[i] = BacktraceEntry {
                    ip: return_addr as u64,
                    sp: (frame as usize + 16) as u64, // Approximate SP
                    symbol: None, // TODO: Symbol resolution
                    file: None,   // TODO: File resolution
                    line: None,   // TODO: Line resolution
                };

                frame = next_frame as *const usize;
                i += 1;
            }
        }

        self.count = i;
    }

    /// Get the collected backtrace entries
    pub fn entries(&self) -> &[BacktraceEntry] {
        &self.entries[..self.count]
    }
}

/// Helper function to print a backtrace to serial
pub fn print_backtrace(writer: &mut impl Write) {
    let mut collector = BacktraceCollector::new();
    collector.capture();

    for (i, entry) in collector.entries().iter().enumerate() {
        if let Some(symbol) = entry.symbol {
            if let (Some(file), Some(line)) = (entry.file, entry.line) {
                writeln!(
                    writer,
                    "  [{}] {:#x} ({}) at {}:{}",
                    i, entry.ip, symbol, file, line
                )
                .ok();
            } else {
                writeln!(writer, "  [{}] {:#x} ({})", i, entry.ip, symbol).ok();
            }
        } else {
            writeln!(writer, "  [{}] {:#x}", i, entry.ip).ok();
        }
    }
}

/// Convert an address to human-readable format
/// This is a placeholder for more advanced symbol resolution
pub fn resolve_address(addr: u64) -> Option<(&'static str, &'static str, u32)> {
    // TODO: Implement proper symbol resolution using addr2line and DWARF
    // For now, return None - will be implemented when DWARF data is available
    None
}
