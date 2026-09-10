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
#[repr(C)]
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
    let mut states = [NodeState::new(); 16];
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => {
                let parent = states[depth - 1];
                states[depth] = NodeState {
                    address_cells: parent.child_address_cells,
                    size_cells: parent.child_size_cells,
                    child_address_cells: parent.child_address_cells,
                    child_size_cells: parent.child_size_cells,
                    ..NodeState::new()
                };
            }
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"#address-cells") && item.length >= 4 {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_address_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"#size-cells") && item.length >= 4
                {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_size_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"compatible") {
                    state.compatible = compatible_list_contains(item.value, item.length, target);
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, b"reg") {
                    state.regions = read_regions(
                        item.value,
                        item.length,
                        state.address_cells,
                        state.size_cells,
                    );
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.compatible {
                    if let Some(region) = state.regions.get(index).and_then(|region| *region) {
                        result = Some(region);
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

/// Collect enabled system-memory regions from the DTB's `memory` nodes.
///
/// Linux arm64 bootloaders describe usable DRAM with a node named
/// `memory@...` and/or `device_type = "memory"`; the `reg` cells inherit the
/// root bus `#address-cells`/`#size-cells`.  Keep this parser bounded and
/// allocation-free because it runs before the full allocator exists.  The
/// caller owns `out`, and the return value is the number of entries written.
pub fn find_memory_regions(address: u64, out: &mut [Region]) -> usize {
    let mut states = [MemoryNodeState::new(); 16];
    let mut count = 0usize;
    let _ = walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode {
                depth,
                name,
                name_end,
            } => {
                let parent = states[depth - 1];
                states[depth] = MemoryNodeState {
                    address_cells: parent.child_address_cells,
                    size_cells: parent.child_size_cells,
                    child_address_cells: parent.child_address_cells,
                    child_size_cells: parent.child_size_cells,
                    enabled: true,
                    is_memory_name: c_string_is_memory_node(name, name_end),
                    is_memory_type: false,
                    regions: [None; 8],
                };
            }
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"#address-cells") && item.length >= 4 {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_address_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"#size-cells") && item.length >= 4
                {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_size_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"device_type") {
                    state.is_memory_type = c_string_eq(item.value, item.value_end, b"memory");
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, b"reg") {
                    state.regions = read_regions_bounded(
                        item.value,
                        item.length,
                        state.address_cells,
                        state.size_cells,
                    );
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && (state.is_memory_name || state.is_memory_type) {
                    for region in state.regions.into_iter().flatten() {
                        if count >= out.len() {
                            return false;
                        }
                        out[count] = region;
                        count += 1;
                    }
                }
            }
        }
        true
    });
    count
}

/// Collect fixed `reg` ranges below the DT `/reserved-memory` container.
///
/// A physical DMA address is not safe merely because it falls inside a
/// `memory` node: Qualcomm firmware and remote processors retain several
/// sub-ranges as `no-map`/removed-dma-pool memory.  This bounded view is used
/// by early device bring-up to reject an arena that overlaps one of those
/// reservations before a controller can receive its address.
pub fn find_reserved_memory_regions(address: u64, out: &mut [Region]) -> usize {
    let mut states = [ReservedMemoryNodeState::new(); 16];
    let mut count = 0usize;
    let _ = walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode {
                depth,
                name,
                name_end,
            } => {
                let parent = states[depth - 1];
                states[depth] = ReservedMemoryNodeState {
                    address_cells: parent.child_address_cells,
                    size_cells: parent.child_size_cells,
                    child_address_cells: parent.child_address_cells,
                    child_size_cells: parent.child_size_cells,
                    enabled: true,
                    reserved_container: c_string_eq(name, name_end, b"reserved-memory"),
                    reserved_region: parent.reserved_container,
                    regions: [None; 8],
                };
            }
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"#address-cells") && item.length >= 4 {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_address_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"#size-cells") && item.length >= 4
                {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_size_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, b"reg") {
                    state.regions = read_regions_bounded(
                        item.value,
                        item.length,
                        state.address_cells,
                        state.size_cells,
                    );
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.reserved_region {
                    for region in state.regions.into_iter().flatten() {
                        if count >= out.len() {
                            return false;
                        }
                        out[count] = region;
                        count += 1;
                    }
                }
            }
        }
        true
    });
    count
}

#[derive(Clone, Copy)]
struct StructureProperty {
    name: *const u8,
    name_end: *const u8,
    value: *const u8,
    value_end: *const u8,
    length: usize,
}

#[derive(Clone, Copy)]
enum StructureEvent {
    BeginNode {
        depth: usize,
        name: *const u8,
        name_end: *const u8,
    },
    Property {
        depth: usize,
        property: StructureProperty,
    },
    EndNode {
        depth: usize,
    },
}

/// Walk the structure block once, keeping all token, pointer-bound, node-name,
/// and alignment checks in one place. The callback returns false to stop after
/// finding a value; malformed or unterminated trees return `None`.
fn walk_structure<F>(address: u64, mut visit: F) -> Option<()>
where
    F: FnMut(StructureEvent) -> bool,
{
    let header = inspect(address)?;
    let base = address as *const u8;
    let structure = unsafe { base.add(header.structure_offset as usize) };
    let strings = unsafe { base.add(header.strings_offset as usize) };
    let structure_end = unsafe { structure.add(header.structure_size as usize) };
    let strings_end = unsafe { strings.add(header.strings_size as usize) };
    let mut cursor = structure;
    let mut depth = 0usize;

    while (cursor as usize) < (structure_end as usize) {
        if (structure_end as usize) - (cursor as usize) < 4 {
            return None;
        }
        let token = read_be32(cursor, 0)?;
        cursor = unsafe { cursor.add(4) };
        match token {
            FDT_BEGIN_NODE => {
                if depth + 1 >= 16 {
                    return None;
                }
                depth += 1;
                let name = cursor;
                while (cursor as usize) < (structure_end as usize) && unsafe { *cursor } != 0 {
                    cursor = unsafe { cursor.add(1) };
                }
                if (cursor as usize) >= (structure_end as usize) {
                    return None;
                }
                cursor = align4_checked(unsafe { cursor.add(1) })?;
                if (cursor as usize) > (structure_end as usize) {
                    return None;
                }
                if !visit(StructureEvent::BeginNode {
                    depth,
                    name,
                    name_end: structure_end,
                }) {
                    return Some(());
                }
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return None;
                }
                if !visit(StructureEvent::EndNode { depth }) {
                    return Some(());
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
                cursor = align4_checked(value_end)?;
                if (cursor as usize) > (structure_end as usize) {
                    return None;
                }
                if !visit(StructureEvent::Property {
                    depth,
                    property: StructureProperty {
                        name,
                        name_end: strings_end,
                        value,
                        value_end,
                        length,
                    },
                }) {
                    return Some(());
                }
            }
            FDT_NOP => {}
            FDT_END => return None,
            _ => return None,
        }
    }
    None
}

/// Read one 32-bit big-endian property from the first enabled node matching
/// `target`. This is deliberately allocation-free and is used for the small
/// Qualcomm USB contract fields that are not encoded in `reg` (DMA pool,
/// clock rates, GSI count, and PM QoS latency). A zero-length property is
/// returned as `Some(0)` so the same primitive can also detect DT boolean
/// properties such as `qcom,gsi-disable-io-coherency`.
pub fn find_compatible_property_u32(
    address: u64,
    target: &[u8],
    property: &[u8],
    index: usize,
) -> Option<u32> {
    find_compatible_nth_property_u32(address, target, property, index, 0)
}

/// Hash the complete byte payload of a property on the first enabled node
/// matching `target`.  This is used for bounded DT string-list contracts:
/// length alone cannot distinguish two provider bindings with the same number
/// of names, while copying an unbounded string list would violate the early
/// boot parser's no-heap contract.
pub fn find_compatible_property_fnv1a(address: u64, target: &[u8], property: &[u8]) -> Option<u32> {
    let mut states = [TextPropertyNodeState::new(); 16];
    let mut matching_nodes = 0usize;
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => states[depth] = TextPropertyNodeState::new(),
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"compatible") {
                    state.compatible = compatible_list_contains(item.value, item.length, target);
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, property) {
                    state.property_hash = Some(fnv1a(item.value, item.length));
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.compatible {
                    let selected = matching_nodes == 0;
                    matching_nodes = matching_nodes.saturating_add(1);
                    if selected {
                        result = state.property_hash;
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

/// Identity and property observation for one enabled compatible node.
/// `ordinal` is zero-based among enabled nodes whose compatible list contains
/// the requested string. `reg_base` is the first address cell of that node's
/// first `reg` tuple, decoded with the same parent-cell rules as
/// `find_compatible_nth`. `cells` are the first six big-endian cells of the
/// requested property, without validation or pair packing.
#[derive(Clone, Copy)]
pub struct CompatiblePropertyObservation {
    pub ordinal: usize,
    pub reg_base: Option<u64>,
    pub property_present: bool,
    pub property_length: u32,
    pub cells: [Option<u32>; 6],
}

/// Observe the identity and raw property of the `node_index`th enabled node
/// whose compatible list contains `target`. This deliberately combines node
/// matching, `reg` decoding, property presence/length, and raw-cell capture
/// in one FDT walk: a property result cannot be accidentally attributed to a
/// different compatible node than the resource address.
pub fn find_compatible_node_property_observation(
    address: u64,
    target: &[u8],
    property: &[u8],
    node_index: usize,
) -> Option<CompatiblePropertyObservation> {
    let mut states = [NodePropertyObservationState::new(); 16];
    let mut matching_nodes = 0usize;
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => {
                let parent = states[depth - 1];
                states[depth] = NodePropertyObservationState {
                    address_cells: parent.child_address_cells,
                    size_cells: parent.child_size_cells,
                    child_address_cells: parent.child_address_cells,
                    child_size_cells: parent.child_size_cells,
                    ..NodePropertyObservationState::new()
                };
            }
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"#address-cells") && item.length >= 4 {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_address_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"#size-cells") && item.length >= 4
                {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_size_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"compatible") {
                    state.compatible = compatible_list_contains(item.value, item.length, target);
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, b"reg") {
                    state.regions = read_regions(
                        item.value,
                        item.length,
                        state.address_cells,
                        state.size_cells,
                    );
                } else if c_string_eq(item.name, item.name_end, property) {
                    state.property_present = true;
                    state.property_length = item.length.min(u32::MAX as usize) as u32;
                    for index in 0..state.cells.len() {
                        if let Some(offset) = index.checked_mul(4) {
                            if offset.checked_add(4).is_some_and(|end| end <= item.length) {
                                state.cells[index] = read_be32(item.value, offset as u32);
                            }
                        }
                    }
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.compatible {
                    let selected = matching_nodes == node_index;
                    let ordinal = matching_nodes;
                    matching_nodes = matching_nodes.saturating_add(1);
                    if selected {
                        result = Some(CompatiblePropertyObservation {
                            ordinal,
                            reg_base: state.regions[0].map(|region| region.base),
                            property_present: state.property_present,
                            property_length: state.property_length,
                            cells: state.cells,
                        });
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

/// Report whether a property exists on the `node_index`th enabled node whose
/// compatible list contains `target`, and its exact byte length. Keep this
/// compatibility wrapper for callers that do not need node identity; the
/// identity-aware implementation above is the authoritative walk.
pub fn find_compatible_property_observation(
    address: u64,
    target: &[u8],
    property: &[u8],
    node_index: usize,
) -> Option<(bool, u32)> {
    find_compatible_node_property_observation(address, target, property, node_index)
        .map(|observation| (observation.property_present, observation.property_length))
}

/// Read one 32-bit property from the `node_index`th enabled node whose
/// `compatible` list contains `target`. Qualcomm DTs commonly contain more
/// than one `qcom,qsmmu-v500` node (for example KGSL followed by Apps-SMMU),
/// so selecting the first compatible node is not sufficient for SMMU options.
pub fn find_compatible_nth_property_u32(
    address: u64,
    target: &[u8],
    property: &[u8],
    index: usize,
    node_index: usize,
) -> Option<u32> {
    let mut states = [PropertyNodeState::new(); 16];
    let mut matching_nodes = 0usize;
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => states[depth] = PropertyNodeState::new(),
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"phandle")
                    || c_string_eq(item.name, item.name_end, b"linux,phandle")
                {
                    if item.length >= 4 {
                        state.phandle = read_be32(item.value, 0);
                    }
                } else if c_string_eq(item.name, item.name_end, b"compatible") {
                    state.compatible = compatible_list_contains(item.value, item.length, target);
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, property) {
                    if item.length == 0 {
                        state.property_value = Some(0);
                    } else if let Some(offset) = index.checked_mul(4) {
                        if offset.checked_add(4).is_some_and(|end| end <= item.length) {
                            state.property_value = read_be32(item.value, offset as u32);
                        }
                    }
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.compatible {
                    let selected = matching_nodes == node_index;
                    matching_nodes = matching_nodes.saturating_add(1);
                    if selected {
                        result = state.property_value;
                        if result.is_some() {
                            return false;
                        }
                    }
                }
            }
        }
        true
    })?;
    result
}

/// Read one 32-bit property from the enabled node identified by a phandle.
/// This is used for provider capabilities such as
/// `qcom,use-3-lvl-tables`, where selecting the nth compatible node would
/// silently bind the consumer to the wrong SMMU if the DT node order changes.
pub fn find_phandle_property_u32(
    address: u64,
    target: u32,
    property: &[u8],
    index: usize,
) -> Option<u32> {
    let mut states = [PropertyNodeState::new(); 16];
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => states[depth] = PropertyNodeState::new(),
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"phandle")
                    || c_string_eq(item.name, item.name_end, b"linux,phandle")
                {
                    if item.length >= 4 {
                        state.phandle = read_be32(item.value, 0);
                    }
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, property) {
                    if item.length == 0 {
                        state.property_value = Some(0);
                    } else if let Some(offset) = index.checked_mul(4) {
                        if offset.checked_add(4).is_some_and(|end| end <= item.length) {
                            state.property_value = read_be32(item.value, offset as u32);
                        }
                    }
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.phandle == Some(target) {
                    result = state.property_value;
                    if result.is_some() {
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

/// Read a 32-bit property from an enabled node identified by its full DT
/// node name (for example `qcom,typec@1500`).  SPMI child nodes deliberately
/// do not carry a `compatible` property in the Android PMIC DT; their
/// interrupt specifiers therefore have to be discovered by node name.
pub fn find_named_property_u32(
    address: u64,
    node_name: &[u8],
    property: &[u8],
    index: usize,
) -> Option<u32> {
    let mut states = [PropertyNodeState::new(); 16];
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode {
                depth,
                name,
                name_end,
            } => {
                states[depth] = PropertyNodeState {
                    name_matches: c_string_eq(name, name_end, node_name),
                    ..PropertyNodeState::new()
                };
            }
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, property) {
                    if item.length == 0 {
                        state.property_value = Some(0);
                    } else if let Some(offset) = index.checked_mul(4) {
                        if offset.checked_add(4).is_some_and(|end| end <= item.length) {
                            state.property_value = read_be32(item.value, offset as u32);
                        }
                    }
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.name_matches {
                    result = state.property_value;
                    if result.is_some() {
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

/// A small, allocation-free copy of a DT string.  This is used for supply
/// phandles: a PHY's `*-supply` property contains only a phandle, while the
/// RPMh regulator resource name is determined by the referenced regulator
/// node's `regulator-name`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StringValue {
    pub bytes: [u8; 48],
    pub len: usize,
}

fn string_value(value: *const u8, length: usize) -> Option<StringValue> {
    if length == 0 || length > 48 {
        return None;
    }
    let mut string = StringValue {
        bytes: [0; 48],
        len: length.saturating_sub(1),
    };
    for offset in 0..length {
        string.bytes[offset] = unsafe { *value.add(offset) };
    }
    Some(string)
}

/// Resolve one phandle-valued property on the first compatible node and
/// return a string property from the referenced enabled node. The helper
/// deliberately requires an exact phandle match and a bounded string copy; a
/// missing/ambiguous supply is reported to the platform layer instead of
/// turning into a guessed PMIC resource.
pub fn find_phandle_property_string(
    address: u64,
    source_node: &[u8],
    property: &[u8],
    index: usize,
    target_property: &[u8],
) -> Option<StringValue> {
    let target = find_compatible_property_u32(address, source_node, property, index)?;
    let mut states = [PhandleNodeState::new(); 16];
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => states[depth] = PhandleNodeState::new(),
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, b"phandle")
                    || c_string_eq(item.name, item.name_end, b"linux,phandle")
                {
                    if item.length >= 4 {
                        state.phandle = read_be32(item.value, 0);
                    }
                } else if c_string_eq(item.name, item.name_end, target_property) {
                    state.value = string_value(item.value, item.length);
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.phandle == Some(target) {
                    result = state.value;
                    if result.is_some() {
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

/// Resolve a property from the provider node that directly contains a
/// phandle-targeted child. Qualcomm RPMh regulators put the consumer-visible
/// `regulator-name` on the child and the Command DB resource name on the
/// parent, so resolving only the child would permit a regulator-name/resource
/// mismatch to pass the early platform contract.
pub fn find_phandle_parent_property_string(
    address: u64,
    target: u32,
    target_property: &[u8],
) -> Option<StringValue> {
    let mut states = [PhandleNodeState::new(); 16];
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => {
                states[depth] = PhandleNodeState::new();
            }
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"phandle")
                    || c_string_eq(item.name, item.name_end, b"linux,phandle")
                {
                    if item.length >= 4 {
                        state.phandle = read_be32(item.value, 0);
                    }
                } else if c_string_eq(item.name, item.name_end, b"status") {
                    state.enabled = !c_string_eq(item.value, item.value_end, b"disabled");
                } else if c_string_eq(item.name, item.name_end, target_property) {
                    state.value = string_value(item.value, item.length);
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                if state.enabled && state.phandle == Some(target) && depth > 1 {
                    result = states[depth - 1].value;
                    if result.is_some() {
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

/// Resolve one phandle-valued property on the first compatible node and
/// return the first `reg` tuple from the referenced node. Qualcomm glue
/// resources such as `USB3_GDSC-supply` are represented this way: the
/// consumer node does not carry the GDSC MMIO address itself.
pub fn find_phandle_property_region(
    address: u64,
    source_node: &[u8],
    property: &[u8],
) -> Option<Region> {
    let target = find_compatible_property_u32(address, source_node, property, 0)?;
    find_phandle_region(address, target)
}

/// Return the first `reg` tuple of the enabled node carrying `target` as its
/// `phandle`/`linux,phandle`.  This is the non-ambiguous counterpart to a
/// compatible-string lookup: Qualcomm DTs contain multiple instances of
/// providers such as `qcom,qsmmu-v500`.
pub fn find_phandle_region(address: u64, target: u32) -> Option<Region> {
    let mut states = [PhandleRegionNodeState::new(); 16];
    let mut result = None;
    walk_structure(address, |event| {
        match event {
            StructureEvent::BeginNode { depth, .. } => {
                let parent = states[depth - 1];
                states[depth] = PhandleRegionNodeState {
                    address_cells: parent.child_address_cells,
                    size_cells: parent.child_size_cells,
                    child_address_cells: parent.child_address_cells,
                    child_size_cells: parent.child_size_cells,
                    ..PhandleRegionNodeState::new()
                };
            }
            StructureEvent::Property {
                depth,
                property: item,
            } => {
                let state = &mut states[depth];
                if c_string_eq(item.name, item.name_end, b"phandle")
                    || c_string_eq(item.name, item.name_end, b"linux,phandle")
                {
                    if item.length >= 4 {
                        state.phandle = read_be32(item.value, 0);
                    }
                } else if c_string_eq(item.name, item.name_end, b"#address-cells")
                    && item.length >= 4
                {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_address_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"#size-cells") && item.length >= 4
                {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_size_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"reg") {
                    state.regions = read_regions(
                        item.value,
                        item.length,
                        state.address_cells,
                        state.size_cells,
                    );
                }
            }
            StructureEvent::EndNode { depth } => {
                let state = states[depth];
                // A phandle explicitly referenced by a consumer is the
                // authoritative resource identity, even when a shared SoC
                // include marks the provider disabled.
                if state.phandle == Some(target) {
                    result = state.regions[0];
                    if result.is_some() {
                        return false;
                    }
                }
            }
        }
        true
    })?;
    result
}

fn read_be32(base: *const u8, offset: u32) -> Option<u32> {
    let value = unsafe { read_volatile(base.add(offset as usize) as *const u32) };
    Some(u32::from_be(value))
}

#[derive(Clone, Copy)]
struct NodeState {
    address_cells: u8,
    size_cells: u8,
    child_address_cells: u8,
    child_size_cells: u8,
    compatible: bool,
    enabled: bool,
    regions: [Option<Region>; 2],
}

#[derive(Clone, Copy)]
struct MemoryNodeState {
    address_cells: u8,
    size_cells: u8,
    child_address_cells: u8,
    child_size_cells: u8,
    enabled: bool,
    is_memory_name: bool,
    is_memory_type: bool,
    regions: [Option<Region>; 8],
}

#[derive(Clone, Copy)]
struct ReservedMemoryNodeState {
    address_cells: u8,
    size_cells: u8,
    child_address_cells: u8,
    child_size_cells: u8,
    enabled: bool,
    reserved_container: bool,
    reserved_region: bool,
    regions: [Option<Region>; 8],
}

impl ReservedMemoryNodeState {
    const fn new() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
            child_address_cells: 2,
            child_size_cells: 1,
            enabled: true,
            reserved_container: false,
            reserved_region: false,
            regions: [None; 8],
        }
    }
}

#[derive(Clone, Copy)]
struct PropertyNodeState {
    compatible: bool,
    name_matches: bool,
    enabled: bool,
    property_value: Option<u32>,
    phandle: Option<u32>,
    /// Whether the tracked property name was seen on this node at all,
    /// independent of its length. The observation walk uses this to report
    /// "absent" separately from "present but shorter than the read".
    property_present: bool,
    /// Exact byte length of the tracked property when it was seen.
    property_length: u32,
}

#[derive(Clone, Copy)]
struct TextPropertyNodeState {
    compatible: bool,
    enabled: bool,
    property_hash: Option<u32>,
}

impl TextPropertyNodeState {
    const fn new() -> Self {
        Self {
            compatible: false,
            enabled: true,
            property_hash: None,
        }
    }
}

#[derive(Clone, Copy)]
struct NodePropertyObservationState {
    address_cells: u8,
    size_cells: u8,
    child_address_cells: u8,
    child_size_cells: u8,
    compatible: bool,
    enabled: bool,
    regions: [Option<Region>; 2],
    property_present: bool,
    property_length: u32,
    cells: [Option<u32>; 6],
}

impl NodePropertyObservationState {
    const fn new() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
            child_address_cells: 2,
            child_size_cells: 1,
            compatible: false,
            enabled: true,
            regions: [None; 2],
            property_present: false,
            property_length: 0,
            cells: [None; 6],
        }
    }
}

#[derive(Clone, Copy)]
struct PhandleNodeState {
    enabled: bool,
    phandle: Option<u32>,
    value: Option<StringValue>,
}

#[derive(Clone, Copy)]
struct PhandleRegionNodeState {
    address_cells: u8,
    size_cells: u8,
    child_address_cells: u8,
    child_size_cells: u8,
    phandle: Option<u32>,
    regions: [Option<Region>; 2],
}

impl PhandleRegionNodeState {
    const fn new() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
            child_address_cells: 2,
            child_size_cells: 1,
            phandle: None,
            regions: [None; 2],
        }
    }
}

impl PhandleNodeState {
    const fn new() -> Self {
        Self {
            enabled: true,
            phandle: None,
            value: None,
        }
    }
}

impl PropertyNodeState {
    const fn new() -> Self {
        Self {
            compatible: false,
            name_matches: false,
            enabled: true,
            property_value: None,
            phandle: None,
            property_present: false,
            property_length: 0,
        }
    }
}

impl NodeState {
    const fn new() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
            child_address_cells: 2,
            child_size_cells: 1,
            compatible: false,
            enabled: true,
            regions: [None; 2],
        }
    }
}

impl MemoryNodeState {
    const fn new() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
            child_address_cells: 2,
            child_size_cells: 1,
            enabled: true,
            is_memory_name: false,
            is_memory_type: false,
            regions: [None; 8],
        }
    }
}

fn align4_checked(pointer: *const u8) -> Option<*const u8> {
    Some(((pointer as usize).checked_add(3)? & !3) as *const u8)
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

fn c_string_is_memory_node(pointer: *const u8, end: *const u8) -> bool {
    if (pointer as usize) >= end as usize {
        return false;
    }
    let mut index = 0usize;
    while index < b"memory".len() {
        if (pointer as usize) + index >= end as usize
            || unsafe { *pointer.add(index) } != b"memory"[index]
        {
            return false;
        }
        index += 1;
    }
    let next = (pointer as usize).saturating_add(index);
    next < end as usize && matches!(unsafe { *pointer.add(index) }, 0 | b'@')
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

fn fnv1a(pointer: *const u8, length: usize) -> u32 {
    let mut hash = 0x811c9dc5u32;
    let mut index = 0usize;
    while index < length {
        hash ^= unsafe { *pointer.add(index) } as u32;
        hash = hash.wrapping_mul(0x01000193);
        index += 1;
    }
    hash
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

fn read_regions_bounded(
    pointer: *const u8,
    length: usize,
    address_cells: u8,
    size_cells: u8,
) -> [Option<Region>; 8] {
    let mut regions = [None; 8];
    if !matches!((address_cells, size_cells), (1..=2, 1..=2)) {
        return regions;
    }
    let tuple_size = (address_cells as usize + size_cells as usize) * 4;
    if tuple_size == 0 {
        return regions;
    }
    for (index, region) in regions.iter_mut().enumerate() {
        let Some(offset) = index.checked_mul(tuple_size) else {
            break;
        };
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

#[cfg(test)]
mod tests {
    use super::{Region, find_memory_regions, inspect};
    use alloc::vec::Vec;

    fn be32(value: u32, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn pad4(out: &mut Vec<u8>) {
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    fn property(out: &mut Vec<u8>, name: u32, value: &[u8]) {
        be32(3, out);
        be32(value.len() as u32, out);
        be32(name, out);
        out.extend_from_slice(value);
        pad4(out);
    }

    fn memory_dtb(disabled: bool) -> Vec<u8> {
        // The builder intentionally uses a 2-cell address and 2-cell size so
        // the test exercises the same 64-bit cell path used by Bramble.
        let strings = b"#address-cells\0#size-cells\0device_type\0reg\0status\0";
        let address_cells = 0u32;
        let size_cells = 15u32;
        let device_type = 27u32;
        let reg = 39u32;
        let mut structure = Vec::new();
        be32(1, &mut structure);
        structure.push(0);
        pad4(&mut structure);
        property(&mut structure, address_cells, &2u32.to_be_bytes());
        property(&mut structure, size_cells, &2u32.to_be_bytes());
        be32(1, &mut structure);
        structure.extend_from_slice(b"memory@80000000\0");
        pad4(&mut structure);
        property(&mut structure, device_type, b"memory\0");
        let reg_value = [0x0000_0000u32, 0x8000_0000, 0x0000_0000, 0x4000_0000];
        let mut reg_bytes = Vec::new();
        for cell in reg_value {
            be32(cell, &mut reg_bytes);
        }
        property(&mut structure, reg, &reg_bytes);
        if disabled {
            let status = 43u32;
            property(&mut structure, status, b"disabled\0");
        }
        be32(2, &mut structure);
        be32(2, &mut structure);
        be32(9, &mut structure);

        let structure_offset = 40u32;
        let strings_offset = structure_offset + structure.len() as u32;
        let total_size = strings_offset + strings.len() as u32;
        let mut dtb = Vec::new();
        be32(0xd00d_feed, &mut dtb);
        be32(total_size, &mut dtb);
        be32(structure_offset, &mut dtb);
        be32(strings_offset, &mut dtb);
        be32(0, &mut dtb); // memory reservation block offset
        be32(17, &mut dtb);
        be32(16, &mut dtb);
        be32(0, &mut dtb); // boot CPU
        be32(strings.len() as u32, &mut dtb);
        be32(structure.len() as u32, &mut dtb);
        dtb.extend_from_slice(&structure);
        dtb.extend_from_slice(strings);
        dtb
    }

    fn reserved_memory_dtb(disabled: bool) -> Vec<u8> {
        let strings = b"#address-cells\0#size-cells\0reg\0status\0";
        let address_cells = 0u32;
        let size_cells = 15u32;
        let reg = 27u32;
        let status = 31u32;
        let mut structure = Vec::new();
        be32(1, &mut structure);
        structure.push(0);
        pad4(&mut structure);
        property(&mut structure, address_cells, &2u32.to_be_bytes());
        property(&mut structure, size_cells, &2u32.to_be_bytes());
        be32(1, &mut structure);
        structure.extend_from_slice(b"reserved-memory\0");
        pad4(&mut structure);
        property(&mut structure, address_cells, &2u32.to_be_bytes());
        property(&mut structure, size_cells, &2u32.to_be_bytes());
        be32(1, &mut structure);
        structure.extend_from_slice(b"modem@8c000000\0");
        pad4(&mut structure);
        let reg_value = [0x0000_0000u32, 0x8c00_0000, 0x0000_0000, 0x0100_0000];
        let mut reg_bytes = Vec::new();
        for cell in reg_value {
            be32(cell, &mut reg_bytes);
        }
        property(&mut structure, reg, &reg_bytes);
        if disabled {
            property(&mut structure, status, b"disabled\0");
        }
        be32(2, &mut structure);
        be32(2, &mut structure);
        be32(2, &mut structure);
        be32(9, &mut structure);

        let structure_offset = 40u32;
        let strings_offset = structure_offset + structure.len() as u32;
        let total_size = strings_offset + strings.len() as u32;
        let mut dtb = Vec::new();
        be32(0xd00d_feed, &mut dtb);
        be32(total_size, &mut dtb);
        be32(structure_offset, &mut dtb);
        be32(strings_offset, &mut dtb);
        be32(0, &mut dtb);
        be32(17, &mut dtb);
        be32(16, &mut dtb);
        be32(0, &mut dtb);
        be32(strings.len() as u32, &mut dtb);
        be32(structure.len() as u32, &mut dtb);
        dtb.extend_from_slice(&structure);
        dtb.extend_from_slice(strings);
        dtb
    }

    #[test]
    fn memory_node_with_64_bit_cells_is_collected() {
        let dtb = memory_dtb(false);
        assert!(inspect(dtb.as_ptr() as u64).is_some());
        let mut regions = [Region { base: 0, size: 0 }; 2];
        assert_eq!(find_memory_regions(dtb.as_ptr() as u64, &mut regions), 1);
        assert_eq!(regions[0].base, 0x8000_0000);
        assert_eq!(regions[0].size, 0x4000_0000);
    }

    #[test]
    fn disabled_memory_node_is_ignored() {
        let dtb = memory_dtb(true);
        let mut regions = [Region { base: 0, size: 0 }; 2];
        assert_eq!(find_memory_regions(dtb.as_ptr() as u64, &mut regions), 0);
    }

    #[test]
    fn fixed_reserved_memory_region_is_collected() {
        let dtb = reserved_memory_dtb(false);
        let mut regions = [Region { base: 0, size: 0 }; 2];
        assert_eq!(
            super::find_reserved_memory_regions(dtb.as_ptr() as u64, &mut regions),
            1
        );
        assert_eq!(regions[0].base, 0x8c00_0000);
        assert_eq!(regions[0].size, 0x0100_0000);
    }

    #[test]
    fn disabled_reserved_memory_region_is_ignored() {
        let dtb = reserved_memory_dtb(true);
        let mut regions = [Region { base: 0, size: 0 }; 2];
        assert_eq!(
            super::find_reserved_memory_regions(dtb.as_ptr() as u64, &mut regions),
            0
        );
    }
}
