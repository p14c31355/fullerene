//! Multiple APIC Description Table (MADT) processor topology parsing.

use alloc::vec::Vec;

const SDT_HEADER_LEN: usize = 36;
const MADT_FIXED_LEN: usize = SDT_HEADER_LEN + 8;
const ENTRY_LOCAL_APIC: u8 = 0;
const ENTRY_LOCAL_X2APIC: u8 = 9;
const CPU_ENABLED: u32 = 1;
const CPU_ONLINE_CAPABLE: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Processor {
    pub processor_uid: u32,
    pub apic_id: u32,
    pub enabled: bool,
    pub online_capable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MadtInfo {
    pub local_apic_address: u32,
    pub processors: Vec<Processor>,
}

pub fn parse(bytes: &[u8]) -> Option<MadtInfo> {
    if bytes.len() < MADT_FIXED_LEN || bytes.get(..4) != Some(b"APIC") {
        return None;
    }
    let local_apic_address = u32::from_le_bytes(bytes[36..40].try_into().ok()?);
    let mut processors = Vec::new();
    let mut offset = MADT_FIXED_LEN;
    while offset < bytes.len() {
        let entry_type = *bytes.get(offset)?;
        let entry_len = *bytes.get(offset + 1)? as usize;
        if entry_len < 2 || offset.checked_add(entry_len)? > bytes.len() {
            return None;
        }
        let entry = &bytes[offset..offset + entry_len];
        match entry_type {
            ENTRY_LOCAL_APIC if entry_len >= 8 => {
                let flags = u32::from_le_bytes(entry[4..8].try_into().ok()?);
                processors.push(Processor {
                    processor_uid: entry[2] as u32,
                    apic_id: entry[3] as u32,
                    enabled: flags & CPU_ENABLED != 0,
                    online_capable: flags & CPU_ONLINE_CAPABLE != 0,
                });
            }
            ENTRY_LOCAL_X2APIC if entry_len >= 16 => {
                let apic_id = u32::from_le_bytes(entry[4..8].try_into().ok()?);
                let flags = u32::from_le_bytes(entry[8..12].try_into().ok()?);
                let processor_uid = u32::from_le_bytes(entry[12..16].try_into().ok()?);
                processors.push(Processor {
                    processor_uid,
                    apic_id,
                    enabled: flags & CPU_ENABLED != 0,
                    online_capable: flags & CPU_ONLINE_CAPABLE != 0,
                });
            }
            _ => {}
        }
        offset += entry_len;
    }
    processors.sort_by_key(|processor| processor.apic_id);
    processors.dedup_by_key(|processor| processor.apic_id);
    Some(MadtInfo {
        local_apic_address,
        processors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_apic_and_x2apic_entries() {
        let mut madt = alloc::vec![0u8; MADT_FIXED_LEN];
        madt[..4].copy_from_slice(b"APIC");
        madt[36..40].copy_from_slice(&0xfee0_0000u32.to_le_bytes());
        madt.extend_from_slice(&[0, 8, 1, 2, 1, 0, 0, 0]);
        madt.extend_from_slice(&[9, 16, 0, 0, 7, 0, 0, 0, 3, 0, 0, 0, 42, 0, 0, 0]);

        let info = parse(&madt).unwrap();
        assert_eq!(info.local_apic_address, 0xfee0_0000);
        assert_eq!(
            info.processors,
            alloc::vec![
                Processor {
                    processor_uid: 1,
                    apic_id: 2,
                    enabled: true,
                    online_capable: false,
                },
                Processor {
                    processor_uid: 42,
                    apic_id: 7,
                    enabled: true,
                    online_capable: true,
                },
            ]
        );
    }

    #[test]
    fn rejects_truncated_entries() {
        let mut madt = alloc::vec![0u8; MADT_FIXED_LEN];
        madt[..4].copy_from_slice(b"APIC");
        madt.extend_from_slice(&[0, 8, 1]);
        assert!(parse(&madt).is_none());
    }
}
