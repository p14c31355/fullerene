pub mod descriptor;
pub mod processor;
pub mod validator;

pub use descriptor::*;
pub use processor::*;
pub use validator::*;

use core::fmt::Write;
use crate::common::EfiMemoryType;

/// Converts EfiMemoryType to a human-readable string.
fn efi_memory_type_to_str(type_: u32) -> &'static str {
    match type_ {
        0 => "EfiReservedMemoryType",
        1 => "EfiLoaderCode",
        2 => "EfiLoaderData",
        3 => "EfiBootServicesCode",
        4 => "EfiBootServicesData",
        5 => "EfiRuntimeServicesCode",
        6 => "EfiRuntimeServicesData",
        7 => "EfiConventionalMemory",
        _ => "Unknown",
    }
}

/// Dumps the UEFI memory map to the provided writer.
pub fn dump_memory_map(descriptors: &[MemoryMapDescriptor], writer: &mut impl Write) {
    writeln!(writer, "--- UEFI Memory Map Dump ---").ok();
    writeln!(writer, "{:<20} {:<18} {:<12} {:<12}", "Type", "Physical Start", "Pages", "Size (Bytes)").ok();
    writeln!(writer, "---------------------------------------------------------------------------").ok();

    let mut max_phys_addr = 0u64;

    for (i, desc) in descriptors.iter().enumerate() {
        let start = desc.physical_start();
        let pages = desc.number_of_pages();
        let size = pages * 4096;
        let end = start + size;
        let type_val = desc.type_();

        writeln!(
            writer,
            "[{:<2}] {:<18} {:#018x} {:<12} {:#x}",
            i,
            efi_memory_type_to_str(type_val),
            start,
            pages,
            size
        ).ok();

        if end > max_phys_addr {
            max_phys_addr = end;
        }
    }

    writeln!(writer, "---------------------------------------------------------------------------").ok();
    writeln!(writer, "Max Physical Address: {:#x}", max_phys_addr).ok();
    writeln!(writer, "--- End of Memory Map Dump ---\n").ok();
}
