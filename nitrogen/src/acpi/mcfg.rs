//! MCFG (PCI Express Memory-mapped Configuration) table parser.
//!
//! Parses the ACPI MCFG table to discover the ECAM (Enhanced
//! Configuration Access Mechanism) base address, which is needed
//! to access extended PCIe config space (offsets ≥ 0x100).

use crate::acpi;

/// A single ECAM base-address entry in the MCFG table.
#[derive(Debug, Clone, Copy)]
pub struct McfgEntry {
    /// PCI segment group number (almost always 0).
    pub segment: u16,
    /// First bus covered by this ECAM region.
    pub start_bus: u8,
    /// Last bus covered by this ECAM region (inclusive).
    pub end_bus: u8,
    /// Physical base address of the ECAM MMIO window.
    pub base_address: u64,
}

/// Parse the MCFG table and return the first (segment 0) ECAM entry.
///
/// On almost all consumer hardware there is exactly one entry
/// covering segment 0, buses 0..255.  Returns `None` if:
/// - The MCFG table is not present
/// - No entry covers segment 0
pub fn parse_mcfg(rsdp_phys: u64) -> Option<McfgEntry> {
    let table_phys = acpi::find_table(rsdp_phys, b"MCFG")?;
    let bytes = acpi::get_table_bytes(table_phys)?;

    // MCFG header: 36-byte SDT header + 8 bytes reserved = 44 bytes,
    // then entries of 16 bytes each.
    let total_len = bytes.len();
    // MCFG table: 44-byte header + N × 16-byte entries.
    if total_len < 44 || (total_len - 44) % 16 != 0 {
        return None;
    }

    let p = bytes.as_ptr();
    let mut offset = 44usize;

    while offset + 16 <= total_len {
        let base_addr = unsafe { core::ptr::read_unaligned(p.add(offset) as *const u64) };
        let segment = unsafe { core::ptr::read_unaligned(p.add(offset + 8) as *const u16) };
        let start_bus = unsafe { core::ptr::read_unaligned(p.add(offset + 10) as *const u8) };
        let end_bus = unsafe { core::ptr::read_unaligned(p.add(offset + 11) as *const u8) };

        // Return the first entry for segment 0 (the only one on
        // typical consumer hardware).
        if segment == 0 {
            return Some(McfgEntry {
                segment,
                start_bus,
                end_bus,
                base_address: base_addr,
            });
        }
        offset += 16;
    }

    None
}
