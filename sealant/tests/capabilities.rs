use sealant::{
    ExclusiveRamRegion, MmioRegion, Permissions, PointerError, RamRegion, RegionKind, UserRegion,
};

#[test]
fn secure_zero_erases_secret_bytes() {
    let mut secret = *b"private material";
    sealant::secure_zero(&mut secret);
    assert_eq!(secret, [0; 16]);
}

#[test]
fn read_capability_checks_bounds_and_alignment() {
    let values = [10_u32, 20, 30];
    let region = RamRegion::from_slice(&values).unwrap();

    let value = region.check_read(values.as_ptr().wrapping_add(1)).unwrap();
    assert_eq!(unsafe { value.read() }, 20);
    assert!(matches!(
        region.check_read(values.as_ptr().wrapping_add(3)),
        Err(PointerError::OutOfBounds)
    ));
    assert!(matches!(
        region.check_read(core::ptr::null::<u32>()),
        Err(PointerError::Null)
    ));

    let unaligned = values.as_ptr() as *const u8;
    assert!(matches!(
        region.check_read(unaligned.wrapping_add(1) as *const u32),
        Err(PointerError::Unaligned)
    ));
}

#[test]
fn exclusive_region_confines_mutable_access_to_closure() {
    let mut values = [1_u32, 2, 3];
    let ptr = values.as_mut_ptr().wrapping_add(1);
    let mut region = ExclusiveRamRegion::from_mut_slice(&mut values).unwrap();
    {
        let mut checked = region.check_mut(ptr).unwrap();
        checked.with_mut(|value| *value = 42);
    }
    assert_eq!(values, [1, 42, 3]);
}

#[test]
fn arithmetic_overflow_is_rejected() {
    let region = unsafe {
        sealant::MemoryRegion::from_raw_parts(usize::MAX, 1, Permissions::READ, RegionKind::Ram)
    };
    assert!(matches!(region, Err(sealant::RegionError::AddressOverflow)));

    let bytes = [0_u8; 1];
    let region = RamRegion::from_slice(&bytes).unwrap();
    assert!(matches!(
        region.check_slice::<u16>(bytes.as_ptr() as *const u16, usize::MAX),
        Err(PointerError::LengthOverflow)
    ));
}

#[test]
fn permissions_and_region_kinds_are_explicit() {
    let values = [1_u8, 2, 3];
    let read_only = RamRegion::from_slice(&values).unwrap();
    assert!(matches!(
        read_only.check_write(values.as_ptr() as *mut u8),
        Err(PointerError::PermissionDenied)
    ));

    let mmio = unsafe {
        MmioRegion::from_raw_parts(values.as_ptr() as usize, values.len(), Permissions::READ)
            .unwrap()
    };
    assert!(mmio.check_read(values.as_ptr()).is_ok());

    let user = unsafe {
        UserRegion::from_raw_parts(values.as_ptr() as usize, values.len(), Permissions::READ)
            .unwrap()
    };
    assert!(user.check_read(values.as_ptr()).is_ok());
    assert_eq!(RegionKind::Mmio, mmio.region().kind());
}

#[test]
fn physical_addresses_do_not_expose_dereference_operations() {
    let physical = sealant::PhysicalAddress::new(0x1234);
    let typed = sealant::PhysPtr::<u32>::new(physical);
    assert_eq!(typed.address().as_usize(), 0x1234);
    assert_eq!(typed.cast::<u8>().address(), physical);
}
