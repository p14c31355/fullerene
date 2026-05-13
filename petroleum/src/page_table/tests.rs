//! Integration tests for page table types.

use super::types::*;
use super::Pte;

#[test]
fn test_canonical_address() {
    let addr = CanonicalVirtAddr::new(0).unwrap();
    assert_eq!(addr.as_u64(), 0);

    let addr = CanonicalVirtAddr::new(0x0000_7FFF_FFFF_FFFF).unwrap();
    assert_eq!(addr.as_u64(), 0x0000_7FFF_FFFF_FFFF);

    let addr = CanonicalVirtAddr::new(0xFFFF_8000_0000_0000).unwrap();
    assert_eq!(addr.as_u64(), 0xFFFF_8000_0000_0000);

    assert!(CanonicalVirtAddr::new(0x0000_8000_0000_0000).is_none());
    assert!(CanonicalVirtAddr::new(0xFFFF_7FFF_FFFF_FFFF).is_none());
}

#[test]
fn test_page_table_entry() {
    let entry = Pte::new(Flags::PRESENT | Flags::WRITABLE);
    assert!(entry.is_present());
    assert!(!entry.is_huge());
    assert!(!entry.is_unused());

    let unused = Pte::new(0);
    assert!(unused.is_unused());
    assert!(!unused.is_present());
}

#[test]
fn test_page_table_create() {
    let table = super::types::PageTable::new();
    assert!(table.is_empty());
    assert_eq!(table.used_count(), 0);
}

#[test]
fn test_phys_frame() {
    let frame = PhysFrame::from_start_address(0x1000).unwrap();
    assert_eq!(frame.start_address(), 0x1000);
    assert!(PhysFrame::from_start_address(0x1001).is_none());
}

#[test]
fn test_page_indices() {
    let addr = CanonicalVirtAddr::new(0x0000_0080_0000_0000).unwrap();
    assert_eq!(addr.p4_index(), 1);
    assert_eq!(addr.p3_index(), 0);
    assert_eq!(addr.p2_index(), 0);
    assert_eq!(addr.p1_index(), 0);
}

#[test]
fn test_alignment() {
    use super::types::{align_up, align_down, is_aligned};
    assert_eq!(align_up(5, 4), 8);
    assert_eq!(align_down(7, 4), 4);
    assert!(is_aligned(4096, 4096));
    assert!(!is_aligned(4097, 4096));
}

/// Safe CR3 read — wrapper around the raw assembly.
pub fn safe_cr3_read() -> u64 {
    super::raw::utils::read_cr3()
}

#[test]
fn test_cr3_read() {
    let _cr3 = safe_cr3_read();
}