#[repr(C, packed)]
pub struct ImageDosHeader {
    pub e_magic: u16,
    pub _pad: [u8; 58],
    pub e_lfanew: i32,
}

#[repr(C, packed)]
pub struct ImageFileHeader {
    pub _machine: u16,
    pub number_of_sections: u16,
    pub _time_date_stamp: u32,
    pub _pointer_to_symbol_table: u32,
    pub _number_of_symbols: u32,
    pub size_of_optional_header: u16,
    pub _characteristics: u16,
}

#[repr(C, packed)]
pub struct ImageDataDirectory {
    pub virtual_address: u32,
    pub size: u32,
}

#[repr(C, packed)]
pub struct ImageOptionalHeader64 {
    pub _magic: u16,
    pub _major_linker_version: u8,
    pub _minor_linker_version: u8,
    pub _size_of_code: u32,
    pub _size_of_initialized_data: u32,
    pub _size_of_uninitialized_data: u32,
    pub address_of_entry_point: u32,
    pub _base_of_code: u32,
    pub image_base: u64,
    pub _section_alignment: u32,
    pub _file_alignment: u32,
    pub _major_operating_system_version: u16,
    pub _minor_operating_system_version: u16,
    pub _major_image_version: u16,
    pub _minor_image_version: u16,
    pub _major_subsystem_version: u16,
    pub _minor_subsystem_version: u16,
    pub _win32_version_value: u32,
    pub size_of_image: u32,
    pub _size_of_headers: u32,
    pub _checksum: u32,
    pub _subsystem: u16,
    pub _dll_characteristics: u16,
    pub size_of_stack_reserve: u64,
    pub size_of_stack_commit: u64,
    pub size_of_heap_reserve: u64,
    pub size_of_heap_commit: u64,
    pub _loader_flags: u32,
    pub number_of_rva_and_sizes: u32,
    pub data_directory: [ImageDataDirectory; 16],
}

#[repr(C, packed)]
pub struct ImageNtHeaders64 {
    pub _signature: u32,
    pub _file_header: ImageFileHeader,
    pub optional_header: ImageOptionalHeader64,
}

#[repr(C, packed)]
pub struct ImageSectionHeader {
    pub _name: [u8; 8],
    pub _virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub _pointer_to_relocations: u32,
    pub _pointer_to_linenumbers: u32,
    pub _number_of_relocations: u16,
    pub _number_of_linenumbers: u16,
    pub _characteristics: u32,
}

#[repr(C, packed)]
pub struct ImageBaseRelocation {
    pub virtual_address: u32,
    pub size_of_block: u32,
}

#[repr(u16)]
pub enum ImageRelBasedType {
    Absolute = 0,
    High = 1,
    Low = 2,
    HighLow = 3,
    HighAdj = 4,
    MachineSpecific1 = 5,
    Reserved = 6,
    Dir64 = 10,
}
