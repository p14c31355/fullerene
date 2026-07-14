use alloc::vec::Vec;
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

/// Parsed DMAR entry (VT-d DMA remapping).
#[derive(Debug, Clone)]
pub struct DmarDrhd {
    pub flags: u8,
    pub segment: u16,
    pub phys_base: u64,
    pub dev_scope_bus: u8,
    pub dev_scope_path: Vec<(u8, u8)>,
}

#[derive(Debug, Clone)]
pub struct DmarInfo {
    pub host_address_width: u8,
    pub drhd_units: Vec<DmarDrhd>,
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
            if acpi::find_rsdp_from_addr(hint_rsdp) { hint_rsdp } else { 0 }
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
            if segment == 0 {
                return Some(McfgEntry { segment, start_bus, end_bus, base_address: base_addr });
            }
            offset += 16;
        }
        None
    }

    /// Parse the DMAR table and return VT-d information.
    pub fn parse_dmar(&self) -> Option<DmarInfo> {
        let bytes = self.table_bytes(self.find_table(b"DMAR")?)?;
        let total_len = bytes.len();
        if total_len < 48 {
            return None;
        }
        let p = bytes.as_ptr();
        let raw_width_byte = unsafe { core::ptr::read_unaligned(p.add(36) as *const u8) };
        let host_address_width = match raw_width_byte.checked_add(1) {
            Some(w) if raw_width_byte != 0xFF => w,
            _ => return None, // Invalid: 0xFF would overflow or produce invalid width
        };

        let mut drhd_units = Vec::new();
        let mut offset = 48usize;
        while offset + 4 <= total_len {
            let stype = unsafe { core::ptr::read_unaligned(p.add(offset) as *const u16) };
            let slen = unsafe { core::ptr::read_unaligned(p.add(offset + 2) as *const u16) } as usize;
            if slen < 4 || offset + slen > total_len {
                return None;
            }
            if stype == 0 && slen >= 16 {
                let flags = unsafe { core::ptr::read_unaligned(p.add(offset + 4) as *const u8) };
                let segment = unsafe { core::ptr::read_unaligned(p.add(offset + 6) as *const u16) };
                let phys_base = unsafe { core::ptr::read_unaligned(p.add(offset + 8) as *const u64) };
                let mut bus = 0u8;
                let mut path = Vec::new();
                let scope_off = offset + 16;
                if scope_off + 6 <= offset + slen {
                    let scope_len = unsafe { core::ptr::read_unaligned(p.add(scope_off + 1) as *const u8) } as usize;
                    if scope_len >= 6 && scope_off + scope_len <= offset + slen {
                        bus = unsafe { core::ptr::read_unaligned(p.add(scope_off + 5) as *const u8) };
                        let mut po = scope_off + 6;
                        while po + 2 <= scope_off + scope_len {
                            let dev = unsafe { core::ptr::read_unaligned(p.add(po) as *const u8) };
                            let func = unsafe { core::ptr::read_unaligned(p.add(po + 1) as *const u8) };
                            path.push((dev, func));
                            po += 2;
                        }
                    }
                }
                drhd_units.push(DmarDrhd { flags, segment, phys_base, dev_scope_bus: bus, dev_scope_path: path });
            }
            offset += slen;
        }
        Some(DmarInfo { host_address_width, drhd_units })
    }
}
