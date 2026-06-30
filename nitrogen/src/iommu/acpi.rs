use alloc::vec::Vec;

const RSDP_SIG: &[u8; 8] = b"RSD PTR ";
const DMAR_SIG: &[u8; 4] = b"DMAR";

// Search ranges for RSDP (ACPI spec §5.2.5.1)
const EBDA_SEG: u64 = 0x0000_0000_0000_0400;
const EBDA_SEG_LEN: u64 = 0x0000_0000_0000_0400;
const BIOS_ROM_START: u64 = 0x0000_0000_000E_0000;
const BIOS_ROM_END: u64 = 0x0000_0000_000F_FFFF;

const PHYSICAL_MEMORY_OFFSET: u64 = 0xFFFF_8000_0000_0000;

fn phys_to_virt(phys: u64) -> usize {
    (phys + PHYSICAL_MEMORY_OFFSET) as usize
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
    let ebda_virt = phys_to_virt(EBDA_SEG);
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
    let sig = unsafe { *(virt as *const [u8; 8]) };
    if &sig != RSDP_SIG {
        return false;
    }
    let cksum_len = if read_u8(virt + 15) >= 2 { 36 } else { 20 };
    let data = unsafe { core::slice::from_raw_parts(virt as *const u8, cksum_len) };
    checksum(data)
}

pub fn get_xsdt_address(rsdp_phys: u64) -> Option<u64> {
    let virt = phys_to_virt(rsdp_phys);
    let rev = read_u8(virt + 15);
    if rev >= 2 {
        let xsdt = read_u64(virt + 24);
        if xsdt != 0 { Some(xsdt) } else { None }
    } else {
        let rsdt = read_u32(virt + 16) as u64;
        if rsdt != 0 { Some(rsdt) } else { None }
    }
}

fn find_table(xsdt_phys: u64, signature: &[u8; 4]) -> Option<u64> {
    let virt = phys_to_virt(xsdt_phys);
    let length = read_u32(virt + 4) as usize;
    let entry_count = (length - 36) / 8;
    for i in 0..entry_count {
        let entry_phys = read_u64(virt + 36 + i * 8);
        if entry_phys == 0 { continue; }
        let entry_virt = phys_to_virt(entry_phys);
        let sig = unsafe { *(entry_virt as *const [u8; 4]) };
        if &sig == signature {
            return Some(entry_phys);
        }
    }
    None
}

fn read_table_len(phys: u64) -> usize {
    read_u32(phys_to_virt(phys) + 4) as usize
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

pub fn parse_dmar(rsdp_phys: u64) -> Option<DmarInfo> {
    let xsdt_phys = get_xsdt_address(rsdp_phys)?;
    let dmar_phys = find_table(xsdt_phys, DMAR_SIG)?;
    let dmar_virt = phys_to_virt(dmar_phys);
    let length = read_table_len(dmar_phys);

    let host_address_width = read_u8(dmar_virt + 36);
    // flags at offset 37
    let mut drhd_units = Vec::new();
    let mut offset = 48; // header(48) + fixed fields

    while offset < length {
        let stype = read_u16(dmar_virt + offset);
        let slen = read_u16(dmar_virt + offset + 2) as usize;
        if stype == 0 {
            let flags = read_u8(dmar_virt + offset + 4);
            let segment = read_u16(dmar_virt + offset + 6);
            // bit 0 of flags: INCLUDE_PCI_ALL
            // Device scope starts at offset + 8
            let scope_offset = offset + 8;
            let scope_remaining = slen - 8;
            // Parse first device scope to find IOMMU PCI location
            let (bus, path) = if scope_remaining >= 6 {
                // Device scope structure:
                // 0: type (1=PCI endpoint, 2=PCI sub-hierarchy)
                // 1: length
                // 2-3: reserved
                // 4: enumeration ID
                // 5: bus
                // 6+: path[] = {dev, func} pairs
                let _scope_type = read_u8(dmar_virt + scope_offset);
                let scope_len = read_u8(dmar_virt + scope_offset + 1) as usize;
                let bus = read_u8(dmar_virt + scope_offset + 5);
                let mut path = Vec::new();
                let mut p_off = scope_offset + 6;
                while p_off < scope_offset + scope_len {
                    let dev = read_u8(dmar_virt + p_off);
                    let func = read_u8(dmar_virt + p_off + 1);
                    path.push((dev, func));
                    p_off += 2;
                }
                (bus, path)
            } else {
                (0, Vec::new())
            };
            drhd_units.push(DmarDrhd {
                flags,
                segment,
                phys_base: 0,
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
