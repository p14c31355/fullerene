use alloc::vec::Vec;

const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
const DMAR_SIG: &[u8; 4] = b"DMAR";

// Search ranges for RSDP (ACPI spec §5.2.5.1)
const EBDA_SEG_PTR: u64 = 0x0000_0000_0000_040E;
const EBDA_SEG_LEN: u64 = 0x0000_0000_0000_0400;
const BIOS_ROM_START: u64 = 0x0000_0000_000E_0000;
const BIOS_ROM_END: u64 = 0x0000_0000_000F_FFFF;

fn phys_to_virt(phys: u64) -> usize {
    let offset = crate::iommu::PHYS_TO_VIRT.load(core::sync::atomic::Ordering::Relaxed);
    // If the addition would overflow, the physical address is invalid
    // (>= 128 TB with the standard offset).  Return 0 so the caller
    // either gets a page fault (preferable) or finds corrupt data and
    // bails out — cannot hang the bus.
    phys.checked_add(offset).unwrap_or(0) as usize
}

fn read_u8(addr: usize) -> u8 {
    unsafe { *(addr as *const u8) }
}

fn read_u16(addr: usize) -> u16 {
    unsafe { core::ptr::read_unaligned(addr as *const u16) }
}

fn read_u32(addr: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(addr as *const u32) }
}

fn read_u64(addr: usize) -> u64 {
    unsafe { core::ptr::read_unaligned(addr as *const u64) }
}

fn checksum(data: &[u8]) -> bool {
    data.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

fn find_rsdp_in_range(start: u64, len: u64) -> Option<u64> {
    let mut addr = start;
    while addr < start + len {
        let virt = phys_to_virt(addr);
        if virt == 0 {
            addr += 16;
            continue;
        }
        let sig = unsafe { *(virt as *const [u8; 8]) };
        if &sig == RSDP_SIG {
            let cksum_len = if read_u8(virt + 15) >= 2 { 36 } else { 20 };
            let data = unsafe { core::slice::from_raw_parts(virt as *const u8, cksum_len) };
            if checksum(data) {
                return Some(addr);
            }
        }
        addr += 16;
    }
    None
}

pub fn find_rsdp() -> Option<u64> {
    // Try EBDA first
    let ebda_virt = phys_to_virt(EBDA_SEG_PTR);
    let ebda_ptr = read_u16(ebda_virt) as u64 * 16;
    if ebda_ptr > 0 {
        if let Some(addr) = find_rsdp_in_range(ebda_ptr, EBDA_SEG_LEN) {
            return Some(addr);
        }
    }
    // Fall back to BIOS ROM area
    find_rsdp_in_range(BIOS_ROM_START, BIOS_ROM_END - BIOS_ROM_START + 1)
}

pub fn find_rsdp_from_addr(addr: u64) -> bool {
    if addr == 0 {
        return false;
    }
    let virt = phys_to_virt(addr);
    if virt == 0 {
        return false;
    }
    let sig = unsafe { *(virt as *const [u8; 8]) };
    if &sig != RSDP_SIG {
        return false;
    }
    let cksum_len = if read_u8(virt + 15) >= 2 { 36 } else { 20 };
    let data = unsafe { core::slice::from_raw_parts(virt as *const u8, cksum_len) };
    checksum(data)
}

pub(crate) fn get_xsdt_address(rsdp_phys: u64) -> Option<u64> {
    let virt = phys_to_virt(rsdp_phys);
    if virt == 0 {
        return None;
    }
    let rev = read_u8(virt + 15);
    if rev >= 2 {
        let xsdt = read_u64(virt + 24);
        if xsdt != 0 { Some(xsdt) } else { None }
    } else {
        let rsdt = read_u32(virt + 16) as u64;
        if rsdt != 0 { Some(rsdt) } else { None }
    }
}

/// Returns `(primary_sdt, optional_rsdt_fallback)`.
/// On ACPI v2+ the primary is the XSDT; the RSDT (if non-zero) is returned as fallback.
/// On ACPI v1 the primary is the RSDT and no fallback is returned.
pub(crate) fn get_sdt_addresses(rsdp_phys: u64) -> Option<(u64, Option<u64>)> {
    let virt = phys_to_virt(rsdp_phys);
    if virt == 0 {
        return None;
    }
    let rev = read_u8(virt + 15);
    if rev >= 2 {
        let xsdt = read_u64(virt + 24);
        let rsdt = read_u32(virt + 16) as u64;
        if xsdt == 0 { return None; }
        Some((xsdt, if rsdt != 0 { Some(rsdt) } else { None }))
    } else {
        let rsdt = read_u32(virt + 16) as u64;
        if rsdt != 0 { Some((rsdt, None)) } else { None }
    }
}

pub(crate) fn find_table(sdt_phys: u64, signature: &[u8; 4]) -> Option<u64> {
    let virt = phys_to_virt(sdt_phys);
    if virt == 0 {
        return None;
    }

    let length = read_u32(virt + 4) as usize;
    let sdt_sig = unsafe { *(virt as *const [u8; 4]) };

    // Detect entry size: XSDT uses 8-byte entries, RSDT uses 4-byte entries
    let entry_size: usize;
    let min_length: usize;
    if &sdt_sig == b"XSDT" {
        entry_size = 8;
        min_length = 44;
    } else if &sdt_sig == b"RSDT" {
        entry_size = 4;
        min_length = 40;
    } else {
        return None;
    }

    if length < min_length || length > 128 * 1024 {
        return None;
    }
    let entry_count = (length - 36) / entry_size;
    for i in 0..entry_count {
        let entry_phys = if entry_size == 8 {
            read_u64(virt + 36 + i * entry_size)
        } else {
            read_u32(virt + 36 + i * entry_size) as u64
        };
        if entry_phys == 0 { continue; }
        let entry_virt = phys_to_virt(entry_phys);
        if entry_virt == 0 { continue; }
        let tbl_sig = unsafe { *(entry_virt as *const [u8; 4]) };
        if &tbl_sig == signature {
            return Some(entry_phys);
        }
    }
    None
}

fn read_table_len(phys: u64) -> usize {
    let virt = phys_to_virt(phys);
    if virt == 0 {
        return 0;
    }
    read_u32(virt + 4) as usize
}

// ── Public structures exposed for the engine ─────────────────────

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

pub(crate) fn parse_dmar_from_phys(dmar_phys: u64) -> Option<DmarInfo> {
    let dmar_virt = phys_to_virt(dmar_phys);
    if dmar_virt == 0 {
        return None;
    }
    let length = read_table_len(dmar_phys);
    // Sanity: DMAR table must be at least 48 bytes and at most 1MB
    if length < 48 || length > 1024 * 1024 {
        return None;
    }

    let host_address_width = read_u8(dmar_virt + 36);
    // flags at offset 37
    let mut drhd_units = Vec::new();
    let mut offset = 48; // header(48) + fixed fields

    while offset < length {
        if offset + 4 > length {
            return None;
        }
        let stype = read_u16(dmar_virt + offset);
        let slen = read_u16(dmar_virt + offset + 2) as usize;
        if slen < 4 || offset + slen > length {
            return None;
        }
        if stype == 0 {
            if slen < 16 {
                return None;
            }
            let flags = read_u8(dmar_virt + offset + 4);
            let segment = read_u16(dmar_virt + offset + 6);
            let phys_base = read_u64(dmar_virt + offset + 8);
            // Device scope starts at offset + 16 (after 16-byte DRHD header)
            let scope_offset = offset + 16;
            let scope_remaining = slen - 16;
            let (bus, path) = if scope_remaining >= 6 {
                let _scope_type = read_u8(dmar_virt + scope_offset);
                let scope_len = read_u8(dmar_virt + scope_offset + 1) as usize;
                if scope_len >= 6 && scope_len <= scope_remaining {
                    let bus = read_u8(dmar_virt + scope_offset + 5);
                    let mut path = Vec::new();
                    let mut p_off = scope_offset + 6;
                    while p_off + 1 < scope_offset + scope_len {
                        let dev = read_u8(dmar_virt + p_off);
                        let func = read_u8(dmar_virt + p_off + 1);
                        path.push((dev, func));
                        p_off += 2;
                    }
                    (bus, path)
                } else {
                    (0, Vec::new())
                }
            } else {
                (0, Vec::new())
            };
            drhd_units.push(DmarDrhd {
                flags,
                segment,
                phys_base,
                dev_scope_bus: bus,
                dev_scope_path: path,
            });
        }
        offset += slen;
    }

    Some(DmarInfo {
        host_address_width,
        drhd_units,
    })
}

pub fn parse_dmar(rsdp_phys: u64) -> Option<DmarInfo> {
    let xsdt_phys = get_xsdt_address(rsdp_phys)?;
    let dmar_phys = find_table(xsdt_phys, DMAR_SIG)?;
    parse_dmar_from_phys(dmar_phys)
}
