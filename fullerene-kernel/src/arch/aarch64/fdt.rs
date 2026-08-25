use core::ptr::read_volatile;

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_HEADER_SIZE: u32 = 40;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

#[derive(Clone, Copy)]
pub struct Header {
    pub address: u64,
    pub total_size: u32,
    pub structure_offset: u32,
    pub strings_offset: u32,
    pub structure_size: u32,
    pub strings_size: u32,
    pub version: u32,
}

#[derive(Clone, Copy)]
pub struct Region {
    pub base: u64,
    pub size: u64,
}

/// Read the small, fixed-size DTB header passed in x0 by QEMU's AArch64
/// `-kernel` loader. The full tree parser belongs in the platform layer; this
/// arch-level check gives early boot a useful handoff diagnostic first.
pub fn inspect(address: u64) -> Option<Header> {
    if address == 0 {
        return None;
    }

    let base = address as *const u8;
    let magic = read_be32(base, 0)?;
    let total_size = read_be32(base, 4)?;
    if magic != FDT_MAGIC || total_size < FDT_HEADER_SIZE {
        return None;
    }
    let structure_offset = read_be32(base, 8)?;
    let strings_offset = read_be32(base, 12)?;
    let version = read_be32(base, 20)?;
    let strings_size = read_be32(base, 32)?;
    let structure_size = read_be32(base, 36)?;

    let structure_end = structure_offset.checked_add(structure_size)?;
    let strings_end = strings_offset.checked_add(strings_size)?;
    if structure_offset < FDT_HEADER_SIZE
        || strings_offset < FDT_HEADER_SIZE
        || structure_end > total_size
        || strings_end > total_size
        || version < 16
    {
        return None;
    }

    Some(Header {
        address,
        total_size,
        structure_offset,
        strings_offset,
        structure_size,
        strings_size,
        version,
    })
}

/// Find the first DT node whose `compatible` property contains `target` and
/// decode its first `reg` tuple. This intentionally supports only the 1- and
/// 2-cell address/size forms needed during early platform discovery.
pub fn find_compatible(address: u64, target: &[u8]) -> Option<Region> {
    find_compatible_nth(address, target, 0)
}

/// Find the nth reg tuple from the first enabled compatible node.
/// GICv3 nodes use tuple 0 for the distributor and tuple 1 for the
/// redistributor, so early platform discovery needs both without knowing the
/// SoC's hard-coded addresses.
pub fn find_compatible_nth(address: u64, target: &[u8], index: usize) -> Option<Region> {
    let header = inspect(address)?;
    let base = address as *const u8;
    let structure = unsafe { base.add(header.structure_offset as usize) };
    let strings = unsafe { base.add(header.strings_offset as usize) };
    let structure_end = unsafe { structure.add(header.structure_size as usize) };
    let strings_end = unsafe { strings.add(header.strings_size as usize) };
    let mut cursor = structure;
    let mut depth = 0usize;
    let mut states = [NodeState::new(); 16];

    while (cursor as usize) < (structure_end as usize) {
        if (structure_end as usize) - (cursor as usize) < 4 {
            return None;
        }
        let token = read_be32(cursor, 0)?;
        cursor = unsafe { cursor.add(4) };
        match token {
            FDT_BEGIN_NODE => {
                if depth + 1 >= states.len() {
                    return None;
                }
                depth += 1;
                let parent = states[depth - 1];
                states[depth] = NodeState {
                    address_cells: parent.address_cells,
                    size_cells: parent.size_cells,
                    ..NodeState::new()
                };
                while (cursor as usize) < (structure_end as usize) && unsafe { *cursor } != 0 {
                    cursor = unsafe { cursor.add(1) };
                }
                if (cursor as usize) >= (structure_end as usize) {
                    return None;
                }
                cursor = unsafe { cursor.add(1) };
                cursor = align4(cursor);
                if (cursor as usize) > (structure_end as usize) {
                    return None;
                }
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return None;
                }
                if states[depth].enabled && states[depth].compatible {
                    if let Some(region) =
                        states[depth].regions.get(index).and_then(|region| *region)
                    {
                        return Some(region);
                    }
                }
                depth = depth.saturating_sub(1);
            }
            FDT_PROP => {
                if (structure_end as usize) - (cursor as usize) < 8 {
                    return None;
                }
                let length = read_be32(cursor, 0)? as usize;
                let name_offset = read_be32(unsafe { cursor.add(4) }, 0)?;
                let value = unsafe { cursor.add(8) };
                let value_end = (value as usize).checked_add(length)? as *const u8;
                if (value_end as usize) > (structure_end as usize)
                    || name_offset >= header.strings_size
                {
                    return None;
                }
                let name = unsafe { strings.add(name_offset as usize) };
                let state = &mut states[depth];
                if c_string_eq(name, strings_end, b"#address-cells") && length >= 4 {
                    state.address_cells = read_be32(value, 0)? as u8;
                } else if c_string_eq(name, strings_end, b"#size-cells") && length >= 4 {
                    state.size_cells = read_be32(value, 0)? as u8;
                } else if c_string_eq(name, strings_end, b"compatible") {
                    state.compatible = compatible_list_contains(value, length, target);
                } else if c_string_eq(name, strings_end, b"status") {
                    state.enabled = !c_string_eq(value, value_end, b"disabled");
                } else if c_string_eq(name, strings_end, b"reg") {
                    state.regions =
                        read_regions(value, length, state.address_cells, state.size_cells);
                }
                cursor = align4(value_end);
                if (cursor as usize) > (structure_end as usize) {
                    return None;
                }
            }
            FDT_NOP => {}
            FDT_END => return None,
            _ => return None,
        }
    }
    None
}

fn read_be32(base: *const u8, offset: u32) -> Option<u32> {
    let value = unsafe { read_volatile(base.add(offset as usize) as *const u32) };
    Some(u32::from_be(value))
}

#[derive(Clone, Copy)]
struct NodeState {
    address_cells: u8,
    size_cells: u8,
    compatible: bool,
    enabled: bool,
    regions: [Option<Region>; 2],
}

impl NodeState {
    const fn new() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
            compatible: false,
            enabled: true,
            regions: [None; 2],
        }
    }
}

fn align4(pointer: *const u8) -> *const u8 {
    (((pointer as usize) + 3) & !3) as *const u8
}

fn c_string_eq(pointer: *const u8, end: *const u8, target: &[u8]) -> bool {
    let mut index = 0;
    while index < target.len() {
        if (pointer as usize) + index >= (end as usize) {
            return false;
        }
        let byte = unsafe { *pointer.add(index) };
        if byte != target[index] {
            return false;
        }
        index += 1;
    }
    (pointer as usize) + target.len() < (end as usize) && unsafe { *pointer.add(target.len()) == 0 }
}

fn compatible_list_contains(pointer: *const u8, length: usize, target: &[u8]) -> bool {
    let mut offset = 0;
    while offset < length {
        let remaining =
            unsafe { core::slice::from_raw_parts(pointer.add(offset), length - offset) };
        let end = remaining
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(remaining.len());
        if remaining.get(..end) == Some(target) {
            return true;
        }
        offset += end + 1;
    }
    false
}

fn read_region(
    pointer: *const u8,
    length: usize,
    address_cells: u8,
    size_cells: u8,
) -> Option<Region> {
    if !matches!((address_cells, size_cells), (1..=2, 1..=2)) {
        return None;
    }
    let cells = address_cells as usize + size_cells as usize;
    if length < cells * 4 {
        return None;
    }
    let base = read_cells(pointer, address_cells as usize)?;
    let size = read_cells(
        unsafe { pointer.add(address_cells as usize * 4) },
        size_cells as usize,
    )?;
    Some(Region { base, size })
}

fn read_regions(
    pointer: *const u8,
    length: usize,
    address_cells: u8,
    size_cells: u8,
) -> [Option<Region>; 2] {
    let mut regions = [None; 2];
    if !matches!((address_cells, size_cells), (1..=2, 1..=2)) {
        return regions;
    }
    let tuple_size = (address_cells as usize + size_cells as usize) * 4;
    for (index, region) in regions.iter_mut().enumerate() {
        let offset = index * tuple_size;
        if offset + tuple_size > length {
            break;
        }
        *region = read_region(
            unsafe { pointer.add(offset) },
            tuple_size,
            address_cells,
            size_cells,
        );
    }
    regions
}

fn read_cells(pointer: *const u8, count: usize) -> Option<u64> {
    let mut value = 0u64;
    for index in 0..count {
        value = (value << 32) | read_be32(unsafe { pointer.add(index * 4) }, 0)? as u64;
    }
    Some(value)
}
