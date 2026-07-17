use crate::acpi;
use crate::acpi::{find_rsdp, find_table, get_table_bytes};

/// Parsed MCFG entry (PCI ECAM configuration).
#[derive(Debug, Clone, Copy)]
pub struct McfgEntry {
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
    pub base_address: u64,
}

/// Standalone ACPI table manager.
///
/// Wraps RSDP discovery and table parsing behind a single struct,
/// so callers don't have to pass `rsdp_phys` everywhere.
pub struct AcpiManager {
    rsdp_phys: u64,
}

impl AcpiManager {
    /// Initialise by discovering the RSDP.
    ///
    /// Tries the given address first (from UEFI / boot context), then
    /// falls back to a legacy EBDA / BIOS ROM scan.
    pub fn init(hint_rsdp: u64) -> Option<Self> {
        let rsdp = if hint_rsdp != 0 {
            if acpi::find_rsdp_from_addr(hint_rsdp) {
                hint_rsdp
            } else {
                0
            }
        } else {
            0
        };
        let rsdp = if rsdp != 0 { rsdp } else { find_rsdp()? };
        Some(Self { rsdp_phys: rsdp })
    }

    pub fn rsdp(&self) -> u64 {
        self.rsdp_phys
    }

    /// Find an ACPI table by signature (e.g. `b"FADT"`, `b"MADT"`).
    pub fn find_table(&self, signature: &[u8; 4]) -> Option<u64> {
        find_table(self.rsdp_phys, signature)
    }

    /// Return the bytes of a table at the given physical address.
    pub fn table_bytes(&self, phys: u64) -> Option<&'static [u8]> {
        get_table_bytes(phys)
    }

    /// Parse the MCFG table and return the first (segment 0) ECAM entry.
    pub fn parse_mcfg(&self) -> Option<McfgEntry> {
        let table_phys = self.find_table(b"MCFG")?;
        let bytes = self.table_bytes(table_phys)?;

        let total_len = bytes.len();
        if total_len < 44 {
            return None;
        }
        let p = bytes.as_ptr();
        let mut offset = 44usize;
        while offset + 16 <= total_len {
            let base_addr = unsafe { core::ptr::read_unaligned(p.add(offset) as *const u64) };
            let segment = unsafe { core::ptr::read_unaligned(p.add(offset + 8) as *const u16) };
            let start_bus = unsafe { core::ptr::read_unaligned(p.add(offset + 10) as *const u8) };
            let end_bus = unsafe { core::ptr::read_unaligned(p.add(offset + 11) as *const u8) };
            if segment == 0 {
                return Some(McfgEntry {
                    segment,
                    start_bus,
                    end_bus,
                    base_address: base_addr,
                });
            }
            log::debug!(
                "MCFG: skipping segment {} (bus {}-{})",
                segment,
                start_bus,
                end_bus
            );
            offset += 16;
        }
        None
    }

    /// Parse enabled/online-capable processors from the MADT.
    pub fn parse_madt(&self) -> Option<crate::acpi::madt::MadtInfo> {
        let table_phys = self.find_table(b"APIC")?;
        crate::acpi::madt::parse(self.table_bytes(table_phys)?)
    }
}
