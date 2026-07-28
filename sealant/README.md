# sealant

`sealant` is a `no_std` memory-access boundary for Fullerene. It converts a
raw address into a checked capability only after validating:

- nullness and alignment;
- checked address arithmetic and region bounds;
- read/write permissions; and
- the semantic kind of the region.

The capability types are intentionally separate:

- `RamPtr` and `RamWritePtr` are for ordinary RAM;
- `MmioPtr` and `MmioWritePtr` expose volatile operations only;
- `UserPtr` and `UserPtrMut` expose explicit copy operations;
- `DmaPtr` and `DmaWritePtr` require the caller to perform platform DMA
  synchronization; and
- `PhysPtr<T>` is only a typed physical-address token and has no dereference
  operation.

## Safety boundary

The crate does not claim that range checking proves that memory is mapped,
initialized, has valid pointer provenance, remains mapped, or is free from
concurrent access. Those facts belong to the owner of the address space or
device. Consequently, raw region construction and operations which access
untyped memory are `unsafe` and document their additional requirements.

Mutable RAM access is additionally created through `ExclusiveRamRegion` and
`CheckedMut::with_mut`, so the mutable reference cannot escape the capability's
borrow.

```rust
use sealant::RamRegion;

fn example() {
    let values = [10_u32, 20, 30];
    let region = RamRegion::from_slice(&values).unwrap();
    let value = region.check_read(values.as_ptr().wrapping_add(1)).unwrap();
    // The range and alignment are checked; reading remains an explicit audit point.
    let value = unsafe { value.read() };
    assert_eq!(value, 20);
}
```
