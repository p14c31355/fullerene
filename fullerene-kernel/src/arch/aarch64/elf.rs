//! Bounded AArch64 ELF loading for the first user-address-space boundary.
//!
//! The generic loader is tied to the x86_64 page-table and process manager.
//! This module owns the architecture-facing part needed to load a small
//! native image: validated ELF64 headers, up to four `PT_LOAD` segments, and
//! a fixed page list. The eventual process manager can replace the fixed
//! arrays without changing the segment-copy or entry-point contract.

use super::{allocator::PhysicalFrameAllocator, mmu};

const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const MAX_LOAD_SEGMENTS: usize = 8;
const MAX_IMAGE_PAGES: usize = 4096;
pub(crate) const MAX_INTERPRETER_PATH: usize = 128;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const EM_AARCH64: u16 = 183;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const PF_W: u32 = 2;
const PF_X: u32 = 1;
const PAGE_SIZE: u64 = 4096;
const USER_ADDRESS_LIMIT: u64 = 0x1_0000_0000;
const ET_DYN_LOAD_BASE: u64 = 0x4040_0000;
// Keep the Android interpreter below the main executable and reserve a
// separate main-image base so the two ET_DYN images cannot overlap.
pub(crate) const INTERPRETER_LOAD_BASE: u64 = 0x4000_0000;
pub(crate) const MAIN_EXECUTABLE_LOAD_BASE: u64 = 0x4160_0000;
const R_AARCH64_RELATIVE: u32 = 1027;
const DT_NULL: i64 = 0;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_RELAENT: i64 = 9;
const DT_RELR: i64 = 36;
const DT_RELRSZ: i64 = 35;
const DT_RELRENT: i64 = 37;

#[derive(Clone, Copy)]
struct LoadSegment {
    flags: u32,
    file_offset: usize,
    virtual_address: u64,
    file_size: usize,
    memory_size: u64,
}

#[derive(Clone, Copy)]
struct DynamicTable {
    file_offset: usize,
    file_size: usize,
}

#[derive(Clone, Copy)]
struct ParsedImage {
    entry: u64,
    segments: [LoadSegment; MAX_LOAD_SEGMENTS],
    segment_count: usize,
    dynamic: Option<DynamicTable>,
    load_bias: u64,
    phdr: u64,
    phent: u64,
    phnum: u64,
    interpreter_path: [u8; MAX_INTERPRETER_PATH],
    interpreter_path_len: usize,
}

impl LoadSegment {
    const EMPTY: Self = Self {
        flags: 0,
        file_offset: 0,
        virtual_address: 0,
        file_size: 0,
        memory_size: 0,
    };
}

#[derive(Clone, Copy)]
struct ImagePage {
    virtual_address: u64,
    physical_address: u64,
    writable: bool,
    executable: bool,
}

impl ImagePage {
    const EMPTY: Self = Self {
        virtual_address: 0,
        physical_address: 0,
        writable: false,
        executable: false,
    };
}

pub(crate) struct LoadedImage {
    pub(crate) entry: u64,
    pub(crate) page_count: usize,
    pub(crate) phdr: u64,
    pub(crate) phent: u64,
    pub(crate) phnum: u64,
    pub(crate) load_bias: u64,
    pub(crate) interpreter_path: [u8; MAX_INTERPRETER_PATH],
    pub(crate) interpreter_path_len: usize,
}

// The first AArch64 loader is single-core and performs one load at a time.
// Keep the descriptor scratch outside the EL1 call stack so a larger image
// does not consume the bootstrap stack. The mapped pages themselves remain
// owned by the process address space and are not backed by this array.
static mut IMAGE_PAGES: [ImagePage; MAX_IMAGE_PAGES] = [ImagePage::EMPTY; MAX_IMAGE_PAGES];

/// Load a bounded native ELF image into pages obtained from the early frame
/// allocator. Segment data is copied through the identity map, while the
/// user-visible mappings are installed by `mmu::map_user_page`.
pub(crate) fn load_image(
    space_id: usize,
    image: &[u8],
    frames: &mut PhysicalFrameAllocator,
) -> Option<LoadedImage> {
    let initial = parse_segments(image, None)?;
    let load_bias = if initial.interpreter_path_len != 0 {
        MAIN_EXECUTABLE_LOAD_BASE
    } else {
        initial.load_bias
    };
    load_image_at(
        space_id,
        image,
        frames,
        load_bias,
        initial.interpreter_path_len == 0,
    )
}

/// Load an ELF image at an explicit ET_DYN base.
///
/// The Android kernel contract leaves relocation processing to the dynamic
/// linker for both the main executable and `PT_INTERP`. `apply_relocations`
/// therefore remains an explicit choice: static-PIE images use the bounded
/// in-kernel relative relocator, while dynamically linked images are handed
/// to the Android linker unchanged.
pub(crate) fn load_image_at(
    space_id: usize,
    image: &[u8],
    frames: &mut PhysicalFrameAllocator,
    load_bias: u64,
    relocate: bool,
) -> Option<LoadedImage> {
    let parsed = parse_segments(image, Some(load_bias))?;
    load_parsed_image(space_id, image, frames, parsed, relocate)
}

fn load_parsed_image(
    space_id: usize,
    image: &[u8],
    frames: &mut PhysicalFrameAllocator,
    parsed: ParsedImage,
    relocate: bool,
) -> Option<LoadedImage> {
    let ParsedImage {
        entry,
        segments,
        segment_count,
        dynamic,
        load_bias,
        phdr,
        phent,
        phnum,
        interpreter_path,
        interpreter_path_len,
    } = parsed;
    let pages = unsafe { &mut *core::ptr::addr_of_mut!(IMAGE_PAGES) };
    for page in pages.iter_mut() {
        *page = ImagePage::EMPTY;
    }
    let mut page_count = 0usize;

    for segment in segments.iter().take(segment_count) {
        let segment_end = segment.virtual_address.checked_add(segment.memory_size)?;
        let first_page = segment.virtual_address & !(PAGE_SIZE - 1);
        let last_page = align_up(segment_end)?;
        let mut virtual_address = first_page;
        while virtual_address < last_page {
            let existing = pages[..page_count]
                .iter()
                .position(|page| page.virtual_address == virtual_address);
            if let Some(index) = existing {
                pages[index].executable |= segment.flags & PF_X != 0;
                pages[index].writable |= segment.flags & PF_W != 0;
            } else {
                if page_count == MAX_IMAGE_PAGES {
                    return None;
                }
                pages[page_count] = ImagePage {
                    virtual_address,
                    physical_address: 0,
                    writable: segment.flags & PF_W != 0,
                    executable: segment.flags & PF_X != 0,
                };
                page_count += 1;
            }
            virtual_address = virtual_address.checked_add(PAGE_SIZE)?;
        }
    }

    if page_count == 0 {
        return None;
    }

    for index in 0..page_count {
        let Some(physical_address) = frames.next_frame() else {
            release_image_pages(frames, &pages, page_count);
            return None;
        };
        unsafe {
            core::ptr::write_bytes(physical_address as *mut u8, 0, PAGE_SIZE as usize);
        }
        pages[index].physical_address = physical_address;
    }

    for segment in segments.iter().take(segment_count) {
        let mut copied = 0usize;
        while copied < segment.file_size {
            let Some(virtual_address) = segment.virtual_address.checked_add(copied as u64) else {
                release_image_pages(frames, &pages, page_count);
                return None;
            };
            let page_address = virtual_address & !(PAGE_SIZE - 1);
            let Some(page) = pages[..page_count]
                .iter()
                .find(|page| page.virtual_address == page_address)
            else {
                release_image_pages(frames, &pages, page_count);
                return None;
            };
            let page_offset = (virtual_address - page_address) as usize;
            let page_remaining = PAGE_SIZE as usize - page_offset;
            let chunk_size = page_remaining.min(segment.file_size - copied);
            let Some(source_offset) = segment.file_offset.checked_add(copied) else {
                release_image_pages(frames, &pages, page_count);
                return None;
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    image.as_ptr().add(source_offset),
                    (page.physical_address as *mut u8).add(page_offset),
                    chunk_size,
                );
            }
            copied += chunk_size;
        }
    }

    if relocate {
        if let Some(dynamic) = dynamic {
            if !apply_relocations(
                image,
                &segments[..segment_count],
                dynamic,
                pages,
                page_count,
                load_bias,
            ) {
                release_image_pages(frames, pages, page_count);
                return None;
            }
        }
    }

    for page in pages.iter().take(page_count).filter(|page| page.executable) {
        mmu::sync_code(page.physical_address);
    }

    // Publish the permissions only after the identity-map writes are done:
    // an EL1 write to a user-RO code page is correctly rejected by hardware.
    for (mapped_count, page) in pages.iter().take(page_count).enumerate() {
        if !mmu::map_user_page(
            space_id,
            page.virtual_address,
            page.physical_address,
            true,
            page.writable,
            page.executable,
        ) {
            for remaining in pages
                .iter()
                .skip(mapped_count)
                .take(page_count - mapped_count)
            {
                let _ = frames.release_frame(remaining.physical_address);
            }
            if mmu::active_user_space() == space_id {
                for mapped in pages.iter().take(mapped_count) {
                    if let Some(physical) = mmu::unmap_user_page(space_id, mapped.virtual_address) {
                        let _ = frames.release_frame(physical);
                    }
                }
            } else {
                let _ = mmu::release_user_space(space_id, frames, &[]);
            }
            return None;
        }
    }

    Some(LoadedImage {
        entry,
        page_count,
        phdr,
        phent,
        phnum,
        load_bias,
        interpreter_path,
        interpreter_path_len,
    })
}

fn release_image_pages(
    frames: &mut PhysicalFrameAllocator,
    pages: &[ImagePage; MAX_IMAGE_PAGES],
    page_count: usize,
) {
    for page in pages.iter().take(page_count) {
        if page.physical_address != 0 {
            let _ = frames.release_frame(page.physical_address);
        }
    }
}

fn parse_segments(image: &[u8], requested_load_bias: Option<u64>) -> Option<ParsedImage> {
    if image.len() < ELF_HEADER_SIZE
        || image.get(0..4)? != b"\x7fELF"
        || image.get(4).copied()? != 2
        || image.get(5).copied()? != 1
        || image.get(6).copied()? != 1
        || !matches!(read_u16(image, 16)?, ET_EXEC | ET_DYN)
        || read_u16(image, 18)? != EM_AARCH64
        || read_u16(image, 52)? as usize != ELF_HEADER_SIZE
        || read_u16(image, 54)? as usize != PROGRAM_HEADER_SIZE
    {
        return None;
    }

    let image_type = read_u16(image, 16)?;
    let load_bias = match image_type {
        ET_DYN => requested_load_bias.unwrap_or(ET_DYN_LOAD_BASE),
        ET_EXEC => {
            if requested_load_bias.unwrap_or(0) != 0 {
                return None;
            }
            0
        }
        _ => return None,
    };
    let entry = read_u64(image, 24)?.checked_add(load_bias)?;
    let program_header_offset = usize::try_from(read_u64(image, 32)?).ok()?;
    let program_header_count = read_u16(image, 56)? as usize;
    if program_header_count == 0 || program_header_count > 32 {
        return None;
    }
    let header_end = program_header_offset
        .checked_add(program_header_count.checked_mul(PROGRAM_HEADER_SIZE)?)?;
    if header_end > image.len() {
        return None;
    }

    let mut segments = [LoadSegment::EMPTY; MAX_LOAD_SEGMENTS];
    let mut segment_count = 0usize;
    let mut dynamic = None;
    let mut interpreter_path = [0u8; MAX_INTERPRETER_PATH];
    let mut interpreter_path_len = 0usize;
    for index in 0..program_header_count {
        let offset = program_header_offset.checked_add(index * PROGRAM_HEADER_SIZE)?;
        let program_type = read_u32(image, offset)?;
        if program_type == PT_DYNAMIC {
            if dynamic.is_some() {
                return None;
            }
            let file_offset = usize::try_from(read_u64(image, offset + 8)?).ok()?;
            let file_size = usize::try_from(read_u64(image, offset + 32)?).ok()?;
            let file_end = file_offset.checked_add(file_size)?;
            if file_size == 0 || file_end > image.len() {
                return None;
            }
            dynamic = Some(DynamicTable {
                file_offset,
                file_size,
            });
            continue;
        }
        if program_type == PT_INTERP {
            if interpreter_path_len != 0 {
                return None;
            }
            let file_offset = usize::try_from(read_u64(image, offset + 8)?).ok()?;
            let file_size = usize::try_from(read_u64(image, offset + 32)?).ok()?;
            let file_end = file_offset.checked_add(file_size)?;
            if file_size < 2 || file_size > MAX_INTERPRETER_PATH || file_end > image.len() {
                return None;
            }
            let bytes = image.get(file_offset..file_end)?;
            let nul = bytes.iter().position(|byte| *byte == 0)?;
            if nul == 0 || nul + 1 != bytes.len() {
                return None;
            }
            interpreter_path[..nul].copy_from_slice(&bytes[..nul]);
            interpreter_path_len = nul;
            continue;
        }
        if program_type != PT_LOAD {
            continue;
        }
        if segment_count == MAX_LOAD_SEGMENTS {
            return None;
        }
        let flags = read_u32(image, offset + 4)?;
        let file_offset = usize::try_from(read_u64(image, offset + 8)?).ok()?;
        let raw_virtual_address = read_u64(image, offset + 16)?;
        let virtual_address = raw_virtual_address.checked_add(load_bias)?;
        let file_size = usize::try_from(read_u64(image, offset + 32)?).ok()?;
        let memory_size = read_u64(image, offset + 40)?;
        let segment_end = virtual_address.checked_add(memory_size)?;
        let file_end = file_offset.checked_add(file_size)?;
        if memory_size == 0
            || file_size > memory_size as usize
            || file_end > image.len()
            || segment_end > USER_ADDRESS_LIMIT
            || virtual_address >= USER_ADDRESS_LIMIT
            || (file_offset as u64 & (PAGE_SIZE - 1)) != (raw_virtual_address & (PAGE_SIZE - 1))
        {
            return None;
        }
        segments[segment_count] = LoadSegment {
            flags,
            file_offset,
            virtual_address,
            file_size,
            memory_size,
        };
        segment_count += 1;
    }

    if segment_count == 0
        || !segments.iter().take(segment_count).any(|segment| {
            entry >= segment.virtual_address
                && entry < segment.virtual_address + segment.memory_size
        })
    {
        return None;
    }
    let phdr_size =
        u64::from(PROGRAM_HEADER_SIZE as u16).checked_mul(program_header_count as u64)?;
    let phdr_file_end = read_u64(image, 32)?.checked_add(phdr_size)?;
    let phdr = segments
        .iter()
        .take(segment_count)
        .find_map(|segment| {
            let file_end = (segment.file_offset as u64).checked_add(
                // `segment.virtual_address` already includes the load bias,
                // but its file offset remains the original ELF offset.
                segment.file_size as u64,
            )?;
            let program_header_offset = read_u64(image, 32)?;
            (program_header_offset >= segment.file_offset as u64 && phdr_file_end <= file_end).then(
                || segment.virtual_address + (program_header_offset - segment.file_offset as u64),
            )
        })
        .unwrap_or(0);
    Some(ParsedImage {
        entry,
        segments,
        segment_count,
        dynamic,
        load_bias,
        phdr,
        phent: PROGRAM_HEADER_SIZE as u64,
        phnum: program_header_count as u64,
        interpreter_path,
        interpreter_path_len,
    })
}

/// Apply the relocation forms emitted by static AArch64 PIE linkers.
///
/// This intentionally handles only `R_AARCH64_RELATIVE` in RELA and RELR
/// tables. A dynamic symbol resolver is outside the early user boundary;
/// rejecting all other relocation types is safer than entering an image with
/// partially relocated pointers.
fn apply_relocations(
    image: &[u8],
    segments: &[LoadSegment],
    dynamic: DynamicTable,
    pages: &[ImagePage; MAX_IMAGE_PAGES],
    page_count: usize,
    load_bias: u64,
) -> bool {
    let Some(dynamic_end) = dynamic.file_offset.checked_add(dynamic.file_size) else {
        return false;
    };
    let Some(dynamic_bytes) = image.get(dynamic.file_offset..dynamic_end) else {
        return false;
    };
    if dynamic_bytes.len() % 16 != 0 {
        return false;
    }
    let mut rela_address = 0u64;
    let mut rela_size = 0usize;
    let mut rela_entry_size = 24usize;
    let mut relr_address = 0u64;
    let mut relr_size = 0usize;
    let mut relr_entry_size = 8usize;

    let mut offset = 0usize;
    while offset
        .checked_add(16)
        .is_some_and(|end| end <= dynamic_bytes.len())
    {
        let tag = i64::from_ne_bytes(
            dynamic_bytes[offset..offset + 8]
                .try_into()
                .expect("dynamic tag has fixed width"),
        );
        let value = u64::from_ne_bytes(
            dynamic_bytes[offset + 8..offset + 16]
                .try_into()
                .expect("dynamic value has fixed width"),
        );
        offset += 16;
        match tag {
            DT_NULL => break,
            DT_RELA => rela_address = value,
            DT_RELASZ => rela_size = usize::try_from(value).ok().unwrap_or(usize::MAX),
            DT_RELAENT => rela_entry_size = usize::try_from(value).ok().unwrap_or(0),
            DT_RELR => relr_address = value,
            DT_RELRSZ => relr_size = usize::try_from(value).ok().unwrap_or(usize::MAX),
            DT_RELRENT => relr_entry_size = usize::try_from(value).ok().unwrap_or(0),
            _ => {}
        }
    }
    if rela_size != 0 {
        if rela_address == 0 || rela_entry_size != 24 {
            return false;
        }
        let Some(rela_virtual_address) = rela_address.checked_add(load_bias) else {
            return false;
        };
        let Some(rela_offset) = file_offset_for_virtual(rela_virtual_address, segments) else {
            return false;
        };
        let Some(rela_end) = rela_offset.checked_add(rela_size) else {
            return false;
        };
        let Some(rela_bytes) = image.get(rela_offset..rela_end) else {
            return false;
        };
        if rela_bytes.len() % rela_entry_size != 0 {
            return false;
        }
        for entry in rela_bytes.chunks_exact(rela_entry_size) {
            let target = u64::from_le_bytes(entry[0..8].try_into().unwrap());
            let info = u64::from_le_bytes(entry[8..16].try_into().unwrap());
            let addend = i64::from_le_bytes(entry[16..24].try_into().unwrap());
            if (info & 0xffff_ffff) as u32 != R_AARCH64_RELATIVE {
                return false;
            }
            let Some(value) = addend_to_absolute(target, addend, load_bias) else {
                return false;
            };
            if !write_mapped_u64(value.0, value.1, pages, page_count) {
                return false;
            }
        }
    }

    if relr_size != 0 {
        if relr_address == 0 || relr_entry_size != 8 {
            return false;
        }
        let Some(relr_virtual_address) = relr_address.checked_add(load_bias) else {
            return false;
        };
        let Some(relr_offset) = file_offset_for_virtual(relr_virtual_address, segments) else {
            return false;
        };
        let Some(relr_end) = relr_offset.checked_add(relr_size) else {
            return false;
        };
        let Some(relr_bytes) = image.get(relr_offset..relr_end) else {
            return false;
        };
        if relr_bytes.len() % relr_entry_size != 0 {
            return false;
        }
        let mut next_address = None;
        for entry in relr_bytes.chunks_exact(relr_entry_size) {
            let encoded = u64::from_le_bytes(entry.try_into().unwrap());
            if encoded & 1 == 0 {
                let Some(address) = encoded.checked_add(load_bias) else {
                    return false;
                };
                if !apply_relr_at(address, pages, page_count, load_bias) {
                    return false;
                }
                next_address = address.checked_add(8);
            } else {
                let Some(mut address) = next_address else {
                    return false;
                };
                for bit in 1..64 {
                    if encoded & (1u64 << bit) != 0
                        && !apply_relr_at(address, pages, page_count, load_bias)
                    {
                        return false;
                    }
                    address = match address.checked_add(8) {
                        Some(address) => address,
                        None => return false,
                    };
                }
                next_address = Some(address);
            }
        }
    }
    true
}

fn file_offset_for_virtual(address: u64, segments: &[LoadSegment]) -> Option<usize> {
    segments.iter().find_map(|segment| {
        let file_end = segment
            .virtual_address
            .checked_add(segment.file_size as u64)?;
        if address < segment.virtual_address || address >= file_end {
            return None;
        }
        segment
            .file_offset
            .checked_add((address - segment.virtual_address) as usize)
    })
}

fn addend_to_absolute(target: u64, addend: i64, load_bias: u64) -> Option<(u64, u64)> {
    let target = target.checked_add(load_bias)?;
    let value = if addend >= 0 {
        load_bias.checked_add(addend as u64)?
    } else {
        load_bias.checked_sub(addend.unsigned_abs())?
    };
    Some((target, value))
}

fn apply_relr_at(
    address: u64,
    pages: &[ImagePage; MAX_IMAGE_PAGES],
    page_count: usize,
    load_bias: u64,
) -> bool {
    let Some(current) = read_mapped_u64(address, pages, page_count) else {
        return false;
    };
    write_mapped_u64(address, current.wrapping_add(load_bias), pages, page_count)
}

fn read_mapped_u64(
    address: u64,
    pages: &[ImagePage; MAX_IMAGE_PAGES],
    page_count: usize,
) -> Option<u64> {
    if address & 7 != 0 {
        return None;
    }
    let page_address = address & !(PAGE_SIZE - 1);
    let page = pages
        .iter()
        .take(page_count)
        .find(|page| page.virtual_address == page_address)?;
    let offset = usize::try_from(address - page_address).ok()?;
    (offset + 8 <= PAGE_SIZE as usize).then(|| unsafe {
        core::ptr::read_unaligned((page.physical_address + offset as u64) as *const u64)
    })
}

fn write_mapped_u64(
    address: u64,
    value: u64,
    pages: &[ImagePage; MAX_IMAGE_PAGES],
    page_count: usize,
) -> bool {
    if address & 7 != 0 {
        return false;
    }
    let page_address = address & !(PAGE_SIZE - 1);
    let Some(page) = pages
        .iter()
        .take(page_count)
        .find(|page| page.virtual_address == page_address)
    else {
        return false;
    };
    let Ok(offset) = usize::try_from(address - page_address) else {
        return false;
    };
    if offset + 8 > PAGE_SIZE as usize {
        return false;
    }
    unsafe {
        core::ptr::write_unaligned((page.physical_address + offset as u64) as *mut u64, value);
    }
    true
}

/// Map and clear one user stack page through the same bounded MMU boundary.
pub(crate) fn map_zeroed_user_page(
    space_id: usize,
    virtual_address: u64,
    physical_page: u64,
) -> bool {
    if !mmu::map_user_page(space_id, virtual_address, physical_page, true, true, false) {
        return false;
    }
    unsafe {
        core::ptr::write_bytes(physical_page as *mut u8, 0, PAGE_SIZE as usize);
    }
    true
}

fn align_up(value: u64) -> Option<u64> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let value = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let value = bytes.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}
