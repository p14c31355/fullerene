#![no_std]
#![allow(unsafe_op_in_unsafe_fn)]

use core::ptr;

pub const VDSO_USER_BASE: u64 = 0x7000_0000_0000;
pub const VDSO_META_SIZE: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub struct VdsoEntry {
    pub name: &'static str,
    pub virt_addr: u64,
    pub phys_addr: u64,
}

pub const VDSO_BUFFER_SIZE: usize = 32768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    TooLarge,
}

pub fn build(buf: &mut [u8; VDSO_BUFFER_SIZE], entries: &[VdsoEntry]) -> Result<usize, BuildError> {
    let count = entries.len();
    let phnum = 1 + count;
    let names_total = entries.iter().try_fold(0usize, |acc, e| acc.checked_add(e.name.len() + 1)).ok_or(BuildError::TooLarge)?;

    let phoff: u64 = 64;
    let ph_entry_size: u64 = 56;
    let ph_end = phoff + phnum as u64 * ph_entry_size;

    let symtab_off = ph_end as usize;
    let strtab_off = symtab_off + (1 + count) * 24;
    // Section headers go AFTER the string table
    let shoff = strtab_off + names_total;
    let shnum = 2;
    let total_size = shoff + shnum * 56;

    if total_size > VDSO_BUFFER_SIZE {
        return Err(BuildError::TooLarge);
    }

    unsafe fn put_u64(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u64) {
        unsafe {
            let p = buf.as_mut_ptr().add(off) as *mut u64;
            ptr::write_unaligned(p, val.to_le());
        }
    }
    unsafe fn put_u32(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u32) {
        unsafe {
            let p = buf.as_mut_ptr().add(off) as *mut u32;
            ptr::write_unaligned(p, val.to_le());
        }
    }
    unsafe fn put_u16(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u16) {
        unsafe {
            let p = buf.as_mut_ptr().add(off) as *mut u16;
            ptr::write_unaligned(p, val.to_le());
        }
    }
    unsafe fn put_u8(buf: &mut [u8; VDSO_BUFFER_SIZE], off: usize, val: u8) {
        unsafe {
            *buf.get_unchecked_mut(off) = val;
        }
    }

    unsafe {
        buf[0..4].copy_from_slice(b"\x7fELF");
        put_u8(buf, 4, 2);
        put_u8(buf, 5, 1);
        put_u8(buf, 6, 1);
        put_u8(buf, 7, 0);

        put_u16(buf, 16, 3);
        put_u16(buf, 18, 62);
        put_u32(buf, 20, 1);
        put_u64(buf, 24, 0);
        put_u64(buf, 32, phoff);
        put_u64(buf, 40, shoff as u64);
        put_u32(buf, 48, 0);
        put_u16(buf, 52, 64);
        put_u16(buf, 54, 56);
        put_u16(buf, 56, phnum as u16);
        put_u16(buf, 58, 64);
        put_u16(buf, 60, shnum as u16);
        put_u16(buf, 62, 1);

        let mut ph_off = phoff as usize;

        put_u32(buf, ph_off, 1);
        ph_off += 4;
        put_u32(buf, ph_off, 6);
        ph_off += 4;
        put_u64(buf, ph_off, 0);
        ph_off += 8;
        put_u64(buf, ph_off, VDSO_USER_BASE);
        ph_off += 8;
        put_u64(buf, ph_off, 0);
        ph_off += 8;
        put_u64(buf, ph_off, 4096);
        ph_off += 8;
        put_u64(buf, ph_off, 4096);
        ph_off += 8;
        put_u64(buf, ph_off, 4096);
        ph_off += 8;

        for (i, entry) in entries.iter().enumerate() {
            let slot_base = VDSO_USER_BASE + VDSO_META_SIZE as u64 + (i as u64) * 4096;

            put_u32(buf, ph_off, 1);
            ph_off += 4;
            put_u32(buf, ph_off, 5);
            ph_off += 4;
            put_u64(buf, ph_off, 0);
            ph_off += 8;
            put_u64(buf, ph_off, slot_base);
            ph_off += 8;
            put_u64(buf, ph_off, entry.phys_addr);
            ph_off += 8;
            put_u64(buf, ph_off, 4096);
            ph_off += 8;
            put_u64(buf, ph_off, 4096);
            ph_off += 8;
            put_u64(buf, ph_off, 4096);
            ph_off += 8;
        }

        let mut sym_off = symtab_off;
        put_u32(buf, sym_off, 0);
        put_u8(buf, sym_off + 4, 0);
        put_u8(buf, sym_off + 5, 0);
        put_u16(buf, sym_off + 6, 0);
        put_u64(buf, sym_off + 8, 0);
        put_u64(buf, sym_off + 16, 0);
        sym_off += 24;

        let mut str_off = strtab_off;
        for (i, entry) in entries.iter().enumerate() {
            let name_off = (str_off - strtab_off) as u32;
            put_u32(buf, sym_off, name_off);
            put_u8(buf, sym_off + 4, 0x12);
            put_u8(buf, sym_off + 5, 0);
            put_u16(buf, sym_off + 6, 0);
            put_u64(buf, sym_off + 8, slot_vaddr(i));
            put_u64(buf, sym_off + 16, 0);
            sym_off += 24;

            let name_bytes = entry.name.as_bytes();
            let mut si = 0;
            while si < name_bytes.len() {
                *buf.get_unchecked_mut(str_off) = name_bytes[si];
                str_off += 1;
                si += 1;
            }
            *buf.get_unchecked_mut(str_off) = 0;
            str_off += 1;
        }

        let mut sh_off = shoff;
        put_u32(buf, sh_off, str_off as u32 - strtab_off as u32 + 1);
        put_u32(buf, sh_off + 4, 2);
        put_u64(buf, sh_off + 8, 0);
        put_u64(buf, sh_off + 16, 0);
        put_u64(buf, sh_off + 24, symtab_off as u64);
        put_u64(buf, sh_off + 32, (1 + count) as u64 * 24);
        put_u32(buf, sh_off + 40, 2);
        put_u32(buf, sh_off + 44, 1);
        put_u64(buf, sh_off + 48, 24);
        sh_off += 56;

        put_u32(buf, sh_off, 0);
        put_u32(buf, sh_off + 4, 3);
        put_u64(buf, sh_off + 8, 0);
        put_u64(buf, sh_off + 16, 0);
        put_u64(buf, sh_off + 24, strtab_off as u64);
        put_u64(buf, sh_off + 32, (str_off - strtab_off) as u64);
        put_u64(buf, sh_off + 40, 0);
        put_u64(buf, sh_off + 48, 0);
    }

    Ok(total_size)
}

pub fn slot_vaddr(index: usize) -> u64 {
    VDSO_USER_BASE + VDSO_META_SIZE as u64 + (index as u64) * 4096
}
