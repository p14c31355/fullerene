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

/// ACPI power-management registers needed to enter the S5 soft-off state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerInfo {
    pub pm1a_control: u32,
    pub pm1b_control: u32,
    pub pm1_control_len: u8,
    pub smi_command: u32,
    pub acpi_enable: u8,
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

    /// Parse the FADT/FACP fields used by the S5 soft-off transition.
    pub fn parse_power_info(&self) -> Option<PowerInfo> {
        let table_phys = self.find_table(b"FACP")?;
        let bytes = self.table_bytes(table_phys)?;
        if bytes.len() < 113 {
            return None;
        }

        let mut pm1a = u32::from_le_bytes(bytes[64..68].try_into().ok()?);
        let mut pm1b = u32::from_le_bytes(bytes[68..72].try_into().ok()?);

        if pm1a > u16::MAX as u32 || pm1b > u16::MAX as u32 {
            return None;
        }

        // Prefer the legacy I/O addresses, but accept the ACPI 2.0 extended
        // Generic Address Structures when firmware leaves the legacy fields
        // empty.  PM1a/PM1b control blocks are at offsets 172 and 184.
        if pm1a == 0 {
            pm1a = match gas_io_address(bytes, 172) {
                Some(address) if address <= u16::MAX as u64 => address as u32,
                Some(_) => return None,
                None => 0,
            };
        }
        if pm1b == 0 {
            pm1b = match gas_io_address(bytes, 184) {
                Some(address) if address <= u16::MAX as u64 => address as u32,
                Some(_) => return None,
                None => 0,
            };
        }

        let pm1_control_len = bytes[89];
        if pm1a == 0 || pm1_control_len < 2 {
            return None;
        }

        let smi_command = u32::from_le_bytes(bytes[48..52].try_into().ok()?);
        if smi_command > u16::MAX as u32 {
            return None;
        }

        Some(PowerInfo {
            pm1a_control: pm1a,
            pm1b_control: pm1b,
            pm1_control_len,
            smi_command,
            acpi_enable: bytes[52],
        })
    }

    /// Read the `_S5_` sleep-type package from the DSDT.
    pub fn parse_s5_sleep_types(&self) -> Option<(u16, u16)> {
        let fadt_phys = self.find_table(b"FACP")?;
        let fadt = self.table_bytes(fadt_phys)?;
        if fadt.len() < 44 {
            return None;
        }
        let dsdt_phys = if fadt.len() >= 148 {
            u64::from_le_bytes(fadt[140..148].try_into().ok()?)
        } else {
            0
        };
        let dsdt_phys = if dsdt_phys != 0 {
            dsdt_phys
        } else {
            u32::from_le_bytes(fadt[40..44].try_into().ok()?) as u64
        };
        let dsdt = self.table_bytes(dsdt_phys)?;
        parse_s5_from_aml(dsdt)
    }
}

fn gas_io_address(table: &[u8], offset: usize) -> Option<u64> {
    let gas = table.get(offset..offset + 12)?;
    // Generic Address Structure: AddressSpaceId 1 means System I/O.
    if gas[0] != 1 {
        return None;
    }
    Some(u64::from_le_bytes(gas[4..12].try_into().ok()?))
}

fn aml_integer(bytes: &[u8], offset: usize) -> Option<(u16, usize)> {
    match *bytes.get(offset)? {
        0x00 => Some((0, 1)), // ZeroOp
        0x01 => Some((1, 1)), // OneOp
        0x0A => Some((*bytes.get(offset + 1)? as u16, 2)),
        0x0B => Some((
            u16::from_le_bytes(bytes.get(offset + 1..offset + 3)?.try_into().ok()?),
            3,
        )),
        0x0C => Some((
            u16::try_from(u32::from_le_bytes(
                bytes.get(offset + 1..offset + 5)?.try_into().ok()?,
            ))
            .ok()?,
            5,
        )),
        0x0E => Some((
            u16::try_from(u64::from_le_bytes(
                bytes.get(offset + 1..offset + 9)?.try_into().ok()?,
            ))
            .ok()?,
            9,
        )),
        _ => None,
    }
}

fn parse_s5_from_aml(dsdt: &[u8]) -> Option<(u16, u16)> {
    let aml = dsdt.get(36..)?;
    for offset in 0..aml.len().saturating_sub(5) {
        if aml[offset] != 0x08 || &aml[offset + 1..offset + 5] != b"_S5_" {
            continue;
        }
        let package = offset + 5;
        if aml.get(package) != Some(&0x12) {
            continue;
        }
        // We only need to advance over the package-length encoding; the
        // package's first byte is the element count.
        let pkg_len_bytes = ((aml.get(package + 1)? >> 6) as usize) + 1;
        let elements = package + 1 + pkg_len_bytes;
        if *aml.get(elements)? < 2 {
            continue;
        }
        let (a, used) = aml_integer(aml, elements + 1)?;
        let (b, _) = aml_integer(aml, elements + 1 + used)?;
        if a > 7 || b > 7 {
            continue;
        }
        return Some((a, b));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_s5_from_aml;

    #[test]
    fn parses_s5_sleep_types_from_dsdt_aml() {
        let mut dsdt = alloc::vec![0u8; 36];
        dsdt.extend_from_slice(&[
            0x08, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x02, 0x0A, 0x05, 0x0A, 0x05,
        ]);
        assert_eq!(parse_s5_from_aml(&dsdt), Some((5, 5)));
    }

    #[test]
    fn parses_dword_and_qword_s5_values_that_fit_pm1() {
        let mut dsdt = alloc::vec![0u8; 36];
        dsdt.extend_from_slice(&[
            0x08, b'_', b'S', b'5', b'_', 0x12, 0x0E, 0x02, 0x0C, 0x05, 0, 0, 0, 0x0E, 0x05, 0, 0,
            0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(parse_s5_from_aml(&dsdt), Some((5, 5)));
    }

    #[test]
    fn rejects_s5_values_outside_pm1_sleep_type_range() {
        let mut dsdt = alloc::vec![0u8; 36];
        dsdt.extend_from_slice(&[
            0x08, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x02, 0x0A, 0x08, 0x0A, 0x05,
        ]);
        assert_eq!(parse_s5_from_aml(&dsdt), None);
    }
}
