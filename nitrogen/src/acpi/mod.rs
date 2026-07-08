pub mod dmar;

use core::sync::atomic::AtomicU64;

pub use dmar::parse_dmar;

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    _reserved: [u8; 3],
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

static PHYS_TO_VIRT: AtomicU64 = AtomicU64::new(0);

pub fn set_phys_to_virt_offset(offset: u64) {
    PHYS_TO_VIRT.store(offset, core::sync::atomic::Ordering::Relaxed);
}

fn phys_to_virt<T>(phys: u64) -> *const T {
    let offset = PHYS_TO_VIRT.load(core::sync::atomic::Ordering::Relaxed);
    phys.checked_add(offset).map(|v| v as *const T).unwrap_or(core::ptr::null())
}

fn checksum(data: &[u8]) -> bool {
    data.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

const EBDA_SEG_PTR: u64 = 0x0000_0000_0000_040E;
const EBDA_SEG_LEN: u64 = 0x0000_0000_0000_0400;
const BIOS_ROM_START: u64 = 0x0000_0000_000E_0000;
const BIOS_ROM_END: u64 = 0x0000_0000_000F_FFFF;

fn find_rsdp_at(phys: u64) -> Option<u64> {
    use core::ptr::addr_of;
    let ptr = phys_to_virt::<Rsdp>(phys);
    if ptr.is_null() {
        return None;
    }
    let sig = unsafe { core::ptr::read_unaligned(addr_of!((*ptr).signature)) };
    if &sig != b"RSD PTR " {
        return None;
    }
    let rev = unsafe { core::ptr::read_unaligned(addr_of!((*ptr).revision)) };
    let cksum_len = if rev >= 2 { 36 } else { 20 };
    let data = unsafe { core::slice::from_raw_parts(ptr as *const u8, cksum_len) };
    if checksum(data) { Some(phys) } else { None }
}

fn find_rsdp_in_range(start: u64, len: u64) -> Option<u64> {
    let mut addr = start;
    while addr < start + len {
        if find_rsdp_at(addr).is_some() {
            return Some(addr);
        }
        addr += 16;
    }
    None
}

pub fn find_rsdp() -> Option<u64> {
    let ebda_ptr = unsafe {
        let p = phys_to_virt::<u16>(EBDA_SEG_PTR);
        if p.is_null() { 0 } else { *p as u64 * 16 }
    };
    if ebda_ptr > 0 {
        if let Some(addr) = find_rsdp_in_range(ebda_ptr, EBDA_SEG_LEN) {
            return Some(addr);
        }
    }
    find_rsdp_in_range(BIOS_ROM_START, BIOS_ROM_END - BIOS_ROM_START + 1)
}

pub fn find_rsdp_from_addr(addr: u64) -> bool {
    find_rsdp_at(addr).is_some()
}

pub fn find_table(rsdp_phys: u64, signature: &[u8; 4]) -> Option<u64> {
    use core::ptr::addr_of;

    if find_rsdp_at(rsdp_phys).is_none() {
        return None;
    }
    let rsdp_ptr = phys_to_virt::<Rsdp>(rsdp_phys);
    if rsdp_ptr.is_null() {
        return None;
    }

    let rev = unsafe { core::ptr::read_unaligned(addr_of!((*rsdp_ptr).revision)) };
    let (sdt_phys, entry_size): (u64, usize) = if rev >= 2 {
        let xsdt = unsafe { core::ptr::read_unaligned(addr_of!((*rsdp_ptr).xsdt_address)) };
        if xsdt != 0 { (xsdt, 8) } else { return None; }
    } else {
        let rsdt = unsafe { core::ptr::read_unaligned(addr_of!((*rsdp_ptr).rsdt_address)) } as u64;
        if rsdt != 0 { (rsdt, 4) } else { return None; }
    };

    let sdt_ptr = phys_to_virt::<SdtHeader>(sdt_phys);
    if sdt_ptr.is_null() {
        return None;
    }
    let length = unsafe { core::ptr::read_unaligned(addr_of!((*sdt_ptr).length)) };

    if length > 128 * 1024 || length < (36 + entry_size) as u32 {
        return None;
    }

    let sdt_bytes = unsafe { core::slice::from_raw_parts(sdt_ptr as *const u8, length as usize) };
    if !checksum(sdt_bytes) {
        return None;
    }

    let entry_count = (length as usize - 36) / entry_size;
    let entries_virt = sdt_ptr as usize + 36;

    for i in 0..entry_count {
        let entry_phys = if entry_size == 8 {
            unsafe { core::ptr::read_unaligned((entries_virt as *const u64).add(i)) }
        } else {
            unsafe { core::ptr::read_unaligned((entries_virt as *const u32).add(i)) as u64 }
        };
        if entry_phys == 0 {
            continue;
        }
        let tbl = phys_to_virt::<[u8; 4]>(entry_phys);
        if tbl.is_null() {
            continue;
        }
        let sig = unsafe { core::ptr::read_unaligned(tbl) };
        if &sig == signature {
            return Some(entry_phys);
        }
    }
    None
}

pub fn get_table_bytes(phys: u64) -> Option<&'static [u8]> {
    use core::ptr::addr_of;
    let sdt = phys_to_virt::<SdtHeader>(phys);
    if sdt.is_null() {
        return None;
    }
    let length = unsafe { core::ptr::read_unaligned(addr_of!((*sdt).length)) };
    if length > 128 * 1024 || length < 36 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(sdt as *const u8, length as usize) };
    if !checksum(bytes) {
        return None;
    }
    Some(bytes)
}
