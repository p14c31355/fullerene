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
                if c_string_eq(item.name, item.name_end, b"#address-cells")
                    && item.length >= 4
                {
                    if let Some(value) = read_be32(item.value, 0) {
                        state.child_address_cells = value as u8;
                    }
                } else if c_string_eq(item.name, item.name_end, b"#size-cells")
                    && item.length >= 4
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
                    if item.length == 0 || item.length > 48 {
                        state.value = None;
                    } else {
                        let mut string = StringValue {
                            bytes: [0; 48],
                            len: item.length.saturating_sub(1),
                        };
                        for offset in 0..item.length {
                            string.bytes[offset] = unsafe { *item.value.add(offset) };
                        }
                        state.value = Some(string);
                    }
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
