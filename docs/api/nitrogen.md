# Nitrogen — Public Trait API (v0.1)

> **Status: DRAFT — Subject to Freeze**
>
> The traits documented here constitute the v0.1 API surface.
> Internal implementation changes are permitted, but **signature changes to these traits affect all crates and are prohibited after the v0.1 freeze**.

---

## 1. DriverContext — Driver Runtime Context

`nitrogen::driver_context::DriverContext`

Runtime capabilities a driver receives from the kernel. Provides physical memory allocation, MMIO mapping, and DMA mapping.

```rust
pub trait DriverContext: Send + Sync {
    fn phys_to_virt(&self, phys: u64) -> usize;
    fn allocate_frame(&self) -> Result<u64, DriverContextError>;
    fn allocate_contiguous_frames(&self, count: usize) -> Result<u64, DriverContextError>;
    fn map_mmio_region(&self, phys: usize, virt: usize, size: usize) -> Result<(), DriverContextError>;
    fn map_page(&self, virt: usize, phys: usize, flags: PageFlags) -> Result<(), DriverContextError>;
    fn free_frame(&self, phys: u64);
    fn free_contiguous_frames(&self, phys: u64, count: usize);
    fn dma_map(&self, device_id: u16, phys: u64, size: usize) -> Result<u64, DriverContextError>;
    fn dma_unmap(&self, iova: u64, size: usize);
}
```

**Associated types**:

| Type | Role |
|---|---|
| `nitrogen::driver_context::DriverContextError` | OutOfMemory / MmioMappingFailed / InvalidArgument |
| `nitrogen::driver_context::PageFlags` | Three flags: writable / write_combining / executable |

**Implementation (kernel side)**: `fullerene-kernel/src/driver_context_impl.rs` — `KernelDriverContext`

---

## 2. StorageDriver — Block Storage

`nitrogen::driver_api::StorageDriver`

Block devices such as NVMe, AHCI, SATA, IDE, SD/MMC, USB mass storage.

```rust
pub trait StorageDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn read_blocks(&self, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), &'static str>;
    fn write_blocks(&self, lba: u64, count: usize, buf: &[u8]) -> Result<(), &'static str>;
    fn block_size(&self) -> u32;
    fn total_blocks(&self) -> u64;
}
```

---

## 3. NetworkDriver — Network Interface

`nitrogen::driver_api::NetworkDriver`

NICs such as Ethernet, Wi-Fi.

```rust
pub trait NetworkDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn send(&self, buf: &[u8]) -> Result<(), &'static str>;
    fn receive(&self, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn mac_address(&self) -> [u8; 6];
}
```

---

## 4. DisplayDriver — Display/GPU

`nitrogen::driver_api::DisplayDriver`

VGA-compatible, VirtIO-GPU, etc.

```rust
pub trait DisplayDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn framebuffer(&self) -> &[u8];
    fn resolution(&self) -> (usize, usize);
    fn stride(&self) -> usize;
    fn flush(&self);
}
```

---

## 5. AudioDriver — Audio

`nitrogen::driver_api::AudioDriver`

HDA, AC97, USB audio, etc.

```rust
pub trait AudioDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn play(&self, buf: &[u8]) -> Result<(), &'static str>;
}
```

---

## 6. UsbHostDriver — USB Host Controller

`nitrogen::driver_api::UsbHostDriver`

EHCI, XHCI, OHCI, UHCI.

```rust
pub trait UsbHostDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn poll(&self);
}
```

---

## 7. DriverBox — Type-Erased Driver Return Value

`nitrogen::driver_api::DriverBox`

An enum returned by a driver or plugin probe. DriverRegistry handles vendor/device, class/subclass, and fallback matching around the probe.

```rust
pub enum DriverBox {
    Storage(Box<dyn StorageDriver>),
    Network(Box<dyn NetworkDriver>),
    Display(Box<dyn DisplayDriver>),
    Audio(Box<dyn AudioDriver>),
    UsbHost(Box<dyn UsbHostDriver>),
    None,
}
```

---

## 8. Driver — Driver Lifecycle (added v0.1)

`nitrogen::driver_api::Driver`

Unifies driver instance creation and PCI matching into a single trait.

```rust
pub trait Driver: Send {
    /// PCI vendor/device pair this driver handles.
    /// Return `(0xFFFF, 0xFFFF)` for a fallback / catch‑all driver.
    fn pci_id(&self) -> (u16, u16);

    /// Probe a PCI device and return a type‑erased driver instance.
    fn probe(&self, ctx: &dyn DriverContext, device: &nitrogen::pci::PciDevice) -> DriverBox;
}
```

---

## 9. DriverRegistry — Driver Registry (added v0.1)

`nitrogen::driver_api::DriverRegistry`

Collects `Driver` instances and returns a matching driver for a PCI device.

```rust
pub struct DriverRegistry { /* ... */ }

impl DriverRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, name: &'static str, driver: Box<dyn Driver>);
    pub fn match_device(&self, ctx: &dyn DriverContext, device: &nitrogen::pci::PciDevice) -> DriverBox;
    pub fn iter(&self) -> impl Iterator<Item = &'static str>;
}
```

---

## Changelog

| Date | Change |
|---|---|
| 2026-07-13 | v0.1 initial — DriverContext, StorageDriver, NetworkDriver, DisplayDriver, AudioDriver, UsbHostDriver, DriverBox, Driver, DriverRegistry |
