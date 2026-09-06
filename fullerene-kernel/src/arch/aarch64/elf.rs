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
const MAX_LOAD_SEGMENTS: usize = 4;
const MAX_IMAGE_PAGES: usize = 32;
const PT_LOAD: u32 = 1;
const EM_AARCH64: u16 = 183;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const PF_W: u32 = 2;
const PF_X: u32 = 1;
const PAGE_SIZE: u64 = 4096;
const USER_ADDRESS_LIMIT: u64 = 0x1_0000_0000;

#[derive(Clone, Copy)]
struct LoadSegment {
    flags: u32,
    file_offset: usize,
    virtual_address: u64,
    file_size: usize,
    memory_size: u64,
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
}

/// Load a bounded native ELF image into pages obtained from the early frame
/// allocator. Segment data is copied through the identity map, while the
/// user-visible mappings are installed by `mmu::map_user_page`.
pub(crate) fn load_image(
    space_id: usize,
    image: &[u8],
    frames: &mut PhysicalFrameAllocator,
) -> Option<LoadedImage> {
    let (entry, segments, segment_count) = parse_segments(image)?;
    let mut pages = [ImagePage::EMPTY; MAX_IMAGE_PAGES];
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

    for page in pages.iter_mut().take(page_count) {
        let Some(physical_address) = frames.next_frame() else {
            release_image_pages(frames, &pages, page_count);
            return None;
        };
        unsafe {
            core::ptr::write_bytes(physical_address as *mut u8, 0, PAGE_SIZE as usize);
        }
        page.physical_address = physical_address;
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
            let _ = mmu::release_user_space(space_id, frames);
            return None;
        }
    }

    Some(LoadedImage { entry, page_count })
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

fn parse_segments(image: &[u8]) -> Option<(u64, [LoadSegment; MAX_LOAD_SEGMENTS], usize)> {
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

    let entry = read_u64(image, 24)?;
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
    for index in 0..program_header_count {
        let offset = program_header_offset.checked_add(index * PROGRAM_HEADER_SIZE)?;
        if read_u32(image, offset)? != PT_LOAD {
            continue;
        }
        if segment_count == MAX_LOAD_SEGMENTS {
            return None;
        }
        let flags = read_u32(image, offset + 4)?;
        let file_offset = usize::try_from(read_u64(image, offset + 8)?).ok()?;
        let virtual_address = read_u64(image, offset + 16)?;
        let file_size = usize::try_from(read_u64(image, offset + 32)?).ok()?;
        let memory_size = read_u64(image, offset + 40)?;
        let segment_end = virtual_address.checked_add(memory_size)?;
        let file_end = file_offset.checked_add(file_size)?;
        if memory_size == 0
            || file_size > memory_size as usize
            || file_end > image.len()
            || segment_end > USER_ADDRESS_LIMIT
            || virtual_address >= USER_ADDRESS_LIMIT
            || (file_offset as u64 & (PAGE_SIZE - 1)) != (virtual_address & (PAGE_SIZE - 1))
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
    Some((entry, segments, segment_count))
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
