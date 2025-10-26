//! Debug utilities for stack unwinding and symbol resolution
//!
//! This module provides functionality for capturing stack backtraces
//! and resolving addresses to file names and line numbers.

use core::arch::asm;
use core::fmt::Write;

/// Validate if an address is safe to dereference
/// This is a basic check for stack frame pointers to prevent double faults
/// during page fault handling when stacks might be corrupted.
fn is_address_valid(addr: u64) -> bool {
    // Basic checks: not within null page and reasonably aligned.
    // The null pointer case is handled by the caller's loop.
    addr >= 0x1000 && (addr as usize) % core::mem::size_of::<usize>() == 0
}

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
            // Validate pointers before dereferencing to prevent double faults during page handling
            if !is_address_valid(frame as u64) ||
               !is_address_valid((frame as usize).wrapping_add(core::mem::size_of::<usize>()) as u64) {
                break;
            }

            unsafe {
                // Read return address from stack frame
                let return_addr = *frame.offset(1);
                let next_frame_ptr = *frame as *const usize;

                if next_frame_ptr <= frame {
                    // Stack grows downwards, so the next frame pointer should be at a higher address.
                    // If not, the stack is likely corrupted or we've reached the end of the call chain.
                    break;
                }

                if return_addr == 0 {
                    break;
                }

                if let Some((symbol, file, line)) = resolve_address(return_addr as u64) {
                    self.entries[i] = BacktraceEntry {
                        ip: return_addr as u64,
                        sp: (frame as usize + 16) as u64,
                        symbol: Some(symbol),
                        file: Some(file),
                        line: Some(line),
                    };
                } else {
                    self.entries[i] = BacktraceEntry {
                        ip: return_addr as u64,
                        sp: (frame as usize + 16) as u64,
                        symbol: None,
                        file: None,
                        line: None,
                    };
                }

                frame = next_frame_ptr;
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
pub fn resolve_address(addr: u64) -> Option<(&'static str, &'static str, u32)> {
    // Basic implementation without DWARF - classify addresses by known regions
    // For proper DWARF resolution, Dwarf data would be parsed when available

    if addr >= 0x100000 && addr < 0x200000 {
        Some(("kernel code", "unknown", 0))
    } else if addr >= 0xFFFF_8000_0000_0000 && addr < 0xFFFF_C000_0000_0000 {
        Some(("kernel heap", "unknown", 0))
    } else {
        Some(("unknown", "unknown", 0))
    }
}
