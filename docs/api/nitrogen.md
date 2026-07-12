# Nitrogen — Public Trait API (v0.1)

> **Status: DRAFT — 凍結予定**
>
> この文書に記載されたtraitはバージョン v0.1 のAPIサーフェスを構成する。
> 内部実装の変更は許可されるが、これらのtraitの**シグネチャ変更は全クレートに影響するため、v0.1 凍結後は変更禁止**。

---

## 1. DriverContext — ドライバ実行時コンテキスト

`nitrogen::driver_context::DriverContext`

ドライバがカーネルから受け取る実行時ケイパビリティ。物理メモリ割当、MMIOマッピング、DMAマッピングを提供する。

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

**関連型**:

| 型 | 役割 |
|---|---|
| `nitrogen::driver_context::DriverContextError` | OutOfMemory / MmioMappingFailed / InvalidArgument |
| `nitrogen::driver_context::PageFlags` | writable / write_combining / executable の3フラグ |

**実装 (kernel側)**: `fullerene-kernel/src/driver_context_impl.rs` — `KernelDriverContext`

---

## 2. StorageDriver — ブロックストレージ

`nitrogen::driver_api::StorageDriver`

NVMe, AHCI, SATA, IDE, SD/MMC, USB mass storage 等のブロックデバイス。

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

## 3. NetworkDriver — ネットワークインターフェース

`nitrogen::driver_api::NetworkDriver`

Ethernet, Wi-Fi 等のNIC。

```rust
pub trait NetworkDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn send(&self, buf: &[u8]) -> Result<(), &'static str>;
    fn receive(&self, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn mac_address(&self) -> [u8; 6];
}
```

---

## 4. DisplayDriver — ディスプレイ/GPU

`nitrogen::driver_api::DisplayDriver`

VGA-compatible, VirtIO-GPU 等。

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

## 5. AudioDriver — オーディオ

`nitrogen::driver_api::AudioDriver`

HDA, AC97, USB audio 等。

```rust
pub trait AudioDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn play(&self, buf: &[u8]) -> Result<(), &'static str>;
}
```

---

## 6. UsbHostDriver — USBホストコントローラ

`nitrogen::driver_api::UsbHostDriver`

EHCI, XHCI, OHCI, UHCI。

```rust
pub trait UsbHostDriver: Send {
    fn init(&mut self) -> Result<(), &'static str>;
    fn poll(&self);
}
```

---

## 7. DriverBox — 型消去されたドライバ戻り値

`nitrogen::driver_api::DriverBox`

PCIデバイスの(class, subclass) マッチング結果として返される enum。

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

## 8. Driver — ドライバライフサイクル (v0.1 追加)

`nitrogen::driver_api::Driver`

ドライバインスタンスの生成とPCIマッチングを一つのtraitに統一する。

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

## 9. DriverRegistry — ドライバ登録簿 (v0.1 追加)

`nitrogen::driver_api::DriverRegistry`

`Driver` インスタンスを集め、PCIデバイスにマッチするドライバを返す。

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

## 変更履歴

| 日付 | 変更 |
|---|---|
| 2026-07-13 | v0.1 初版 — DriverContext, StorageDriver, NetworkDriver, DisplayDriver, AudioDriver, UsbHostDriver, DriverBox, Driver, DriverRegistry |
