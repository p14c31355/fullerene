use alloc::string::String;
use alloc::vec;

use crate::map_handle;
use petroleum::common::memory::UserSlice;

use super::interface::{SyscallError, SyscallResult, copy_user_string};
use super::process::{alloc_handle, check_handle_permission, with_handle_mut};
use super::types::*;
use crate::contexts::kernel;

pub(crate) fn syscall_enumerate_devices(
    class: u64,
    buf: *mut u8,
    buf_size: usize,
) -> SyscallResult {
    if buf.is_null() || buf_size == 0 || buf_size > (1 << 20) {
        return Err(SyscallError::InvalidArgument);
    }
    petroleum::validate_user_buffer(buf as usize, buf_size, false)?;

    let slice = UserSlice::new(buf, buf_size, true).map_err(|_| SyscallError::InvalidArgument)?;

    let mut records = vec::Vec::new();
    let _ = kernel::with_kernel(|k| {
        for dev in k.pci.devices.iter() {
            let device_class = pci_device_class(dev);
            if class as u32 != fullerene_abi::device_class::ANY && class as u32 != device_class {
                continue;
            }
            records.push(fullerene_abi::DeviceInfo {
                class: device_class,
                device_id: ((dev.bus as u32) << 16)
                    | ((dev.device as u32) << 8)
                    | dev.function as u32,
                vendor_id: dev.vendor_id as u32,
                product_id: dev.device_id as u32,
            });
        }
    });
    if class as u32 == fullerene_abi::device_class::ANY
        || class as u32 == fullerene_abi::device_class::STORAGE
    {
        for name in crate::devfs::list_block_device_names() {
            records.push(fullerene_abi::DeviceInfo {
                class: fullerene_abi::device_class::STORAGE,
                device_id: stable_device_id(&name),
                vendor_id: 0,
                product_id: 0,
            });
        }
    }
    let count = records.len();
    let mut kernel_buf = vec![0u8; buf_size];
    for (index, record) in records
        .iter()
        .take(buf_size / fullerene_abi::DeviceInfo::BYTE_SIZE)
        .enumerate()
    {
        let offset = index * fullerene_abi::DeviceInfo::BYTE_SIZE;
        kernel_buf[offset..offset + fullerene_abi::DeviceInfo::BYTE_SIZE]
            .copy_from_slice(&record.to_ne_bytes());
    }

    unsafe { slice.copy_to_user(&kernel_buf) }.map_err(|_| SyscallError::InvalidArgument)?;
    Ok(count as u64)
}

pub(crate) fn syscall_open_device(device_id: *const u8) -> SyscallResult {
    let id_str = unsafe { copy_user_string(device_id, 128)? };
    if id_str.is_empty() {
        return Err(SyscallError::InvalidArgument);
    }

    let normalized = id_str.trim_start_matches("/dev/");
    if let Some(device) =
        kernel::with_kernel(|k| find_device(normalized, k.pci.devices())).flatten()
    {
        return alloc_handle(KernelObject::Device(DeviceState {
            pci: Some(device),
            name: None,
        }));
    }
    if crate::devfs::block_device_exists(normalized) {
        return alloc_handle(KernelObject::Device(DeviceState {
            pci: None,
            name: Some(String::from(normalized)),
        }));
    }
    // `enumerate_devices` represents registered block devices with a stable
    // numeric ID because the legacy DeviceInfo record has no name field.
    // Resolve that ID here so enumeration followed by open is a complete
    // round-trip even for callers that do not know the `/dev` spelling.
    if let Some(id) = parse_hex_u32(normalized) {
        if let Some(name) = crate::devfs::list_block_device_names()
            .into_iter()
            .find(|name| stable_device_id(name) == id)
        {
            return alloc_handle(KernelObject::Device(DeviceState {
                pci: None,
                name: Some(name),
            }));
        }
    }
    Err(SyscallError::NoSuchDevice)
}

pub(crate) fn syscall_device_ioctl(handle: u64, cmd: u64, arg: u64) -> SyscallResult {
    let h = Handle::from_raw(handle);
    match cmd {
        fullerene_abi::device_ioctl::GET_PCI_INFO => {
            check_handle_permission(h, HandlePerms::READ)?;
            with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                let pci = device.pci.as_ref().ok_or(SyscallError::NotSupported)?;
                let info = fullerene_abi::PciDeviceInfo {
                    bus: pci.bus,
                    device: pci.device,
                    function: pci.function,
                    reserved: 0,
                    vendor_id: pci.vendor_id,
                    product_id: pci.device_id,
                    class_code: pci.class_code,
                    subclass: pci.subclass,
                    prog_if: pci.prog_if,
                    header_type: pci.header_type,
                    reserved_tail: [0; 4],
                };
                copy_ioctl_out(arg, &info.to_ne_bytes())
            })
        }
        fullerene_abi::device_ioctl::READ_PCI_CONFIG => {
            check_handle_permission(h, HandlePerms::READ)?;
            with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                let pci = device.pci.as_ref().ok_or(SyscallError::NotSupported)?;
                let mut request = read_config_request(arg)?;
                let value = read_pci_config(pci, &request)?;
                request.value = value;
                copy_ioctl_out(arg, &request.to_ne_bytes())
            })
        }
        fullerene_abi::device_ioctl::WRITE_PCI_CONFIG => {
            check_handle_permission(h, HandlePerms::WRITE)?;
            with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                let pci = device.pci.as_ref().ok_or(SyscallError::NotSupported)?;
                let request = read_config_request(arg)?;
                if !is_safe_pci_config_write(request.offset, request.width) {
                    return Err(SyscallError::InvalidArgument);
                }
                write_pci_config(pci, &request)
            })
        }
        fullerene_abi::device_ioctl::INITIALIZE_NVME => {
            check_handle_permission(h, HandlePerms::WRITE)?;
            let device = with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                let pci = device.pci.as_ref().ok_or(SyscallError::NotSupported)?;
                if pci.class_code != 0x01 || pci.subclass != 0x08 {
                    return Err(SyscallError::NotSupported);
                }
                Ok(pci.clone())
            })?;
            #[cfg(not(nitrogen_no_storage))]
            {
                let index = crate::drivers::registry::initialize_nvme(device)
                    .map_err(SyscallError::from)?;
                Ok(index as u64)
            }
            #[cfg(nitrogen_no_storage)]
            {
                let _ = device;
                Err(SyscallError::NotSupported)
            }
        }
        fullerene_abi::device_ioctl::INITIALIZE_AHCI => {
            check_handle_permission(h, HandlePerms::WRITE)?;
            let device = with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                let pci = device.pci.as_ref().ok_or(SyscallError::NotSupported)?;
                if pci.class_code != 0x01 || pci.subclass != 0x06 {
                    return Err(SyscallError::NotSupported);
                }
                Ok(pci.clone())
            })?;
            #[cfg(not(nitrogen_no_storage))]
            {
                let index = crate::drivers::registry::initialize_ahci(device)
                    .map_err(SyscallError::from)?;
                Ok(index as u64)
            }
            #[cfg(nitrogen_no_storage)]
            {
                let _ = device;
                Err(SyscallError::NotSupported)
            }
        }
        fullerene_abi::device_ioctl::GET_CAPABILITIES => {
            check_handle_permission(h, HandlePerms::READ)?;
            with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                let info = fullerene_abi::DeviceCapabilityInfo {
                    class: device_class(device),
                    reserved: 0,
                    capabilities: device_capabilities(device),
                };
                copy_ioctl_out(arg, &info.to_ne_bytes())
            })
        }
        fullerene_abi::device_ioctl::GET_BLOCK_INFO => {
            check_handle_permission(h, HandlePerms::READ)?;
            let name = with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                device.name.clone().ok_or(SyscallError::NotSupported)
            })?;
            let (sector_size, total_sectors) =
                crate::devfs::block_device_info(&name).ok_or(SyscallError::NoSuchDevice)?;
            let info = fullerene_abi::BlockDeviceInfo {
                sector_size,
                reserved: 0,
                total_sectors,
            };
            copy_ioctl_out(arg, &info.to_ne_bytes())
        }
        fullerene_abi::device_ioctl::READ_BLOCKS | fullerene_abi::device_ioctl::WRITE_BLOCKS => {
            let write = cmd == fullerene_abi::device_ioctl::WRITE_BLOCKS;
            check_handle_permission(
                h,
                if write {
                    HandlePerms::WRITE
                } else {
                    HandlePerms::READ
                },
            )?;
            let request = read_block_request(arg)?;
            let name = with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                device.name.clone().ok_or(SyscallError::NotSupported)
            })?;
            let (sector_size, _) =
                crate::devfs::block_device_info(&name).ok_or(SyscallError::NoSuchDevice)?;
            let required = (sector_size as usize)
                .checked_mul(request.count as usize)
                .ok_or(SyscallError::Overflow)?;
            if request.count == 0 || (request.buffer_len as usize) < required {
                return Err(SyscallError::InvalidArgument);
            }
            let slice = UserSlice::new(request.buffer_ptr as *mut u8, required, !write)
                .map_err(|_| SyscallError::AddressFault)?;
            let mut buffer = vec![0u8; required];
            if write {
                unsafe { slice.copy_from_user(&mut buffer) }
                    .map_err(|_| SyscallError::AddressFault)?;
                crate::devfs::write_block_device(&name, request.lba, request.count, &buffer)
                    .map_err(SyscallError::from)?;
            } else {
                crate::devfs::read_block_device(&name, request.lba, request.count, &mut buffer)
                    .map_err(SyscallError::from)?;
                unsafe { slice.copy_to_user(&buffer) }.map_err(|_| SyscallError::AddressFault)?;
            }
            Ok(required as u64)
        }
        fullerene_abi::device_ioctl::READ_MMIO => {
            check_handle_permission(h, HandlePerms::READ)?;
            let request = read_mmio_request(arg, false)?;
            let device = with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                Ok(device.pci.clone().ok_or(SyscallError::NotSupported)?)
            })?;
            #[cfg(not(nitrogen_no_storage))]
            {
                let value = crate::drivers::registry::request_mmio(
                    device,
                    request.bar,
                    request.offset,
                    request.width,
                    false,
                    request.value,
                )
                .map_err(SyscallError::from)?;
                let response = fullerene_abi::MmioRequest { value, ..request };
                copy_ioctl_out(arg, &response.to_ne_bytes())
            }
            #[cfg(nitrogen_no_storage)]
            {
                let _ = request;
                let _ = device;
                Err(SyscallError::NotSupported)
            }
        }
        fullerene_abi::device_ioctl::WRITE_MMIO => {
            check_handle_permission(h, HandlePerms::WRITE)?;
            let request = read_mmio_request(arg, true)?;
            let device = with_handle_mut(h, |obj| {
                let device = map_handle!(obj, Device, state);
                Ok(device.pci.clone().ok_or(SyscallError::NotSupported)?)
            })?;
            #[cfg(not(nitrogen_no_storage))]
            {
                crate::drivers::registry::request_mmio(
                    device,
                    request.bar,
                    request.offset,
                    request.width,
                    true,
                    request.value,
                )
                .map_err(SyscallError::from)?;
                Ok(0)
            }
            #[cfg(nitrogen_no_storage)]
            {
                let _ = request;
                let _ = device;
                Err(SyscallError::NotSupported)
            }
        }
        _ => Err(SyscallError::NotSupported),
    }
}

fn copy_ioctl_out<const N: usize>(arg: u64, bytes: &[u8; N]) -> SyscallResult {
    let slice =
        UserSlice::new(arg as *mut u8, N, true).map_err(|_| SyscallError::InvalidArgument)?;
    unsafe { slice.copy_to_user(bytes) }.map_err(|_| SyscallError::InvalidArgument)?;
    Ok(0)
}

fn read_config_request(arg: u64) -> Result<fullerene_abi::PciConfigRequest, SyscallError> {
    let slice = UserSlice::new(
        arg as *mut u8,
        fullerene_abi::PciConfigRequest::BYTE_SIZE,
        false,
    )
    .map_err(|_| SyscallError::InvalidArgument)?;
    let mut bytes = [0u8; fullerene_abi::PciConfigRequest::BYTE_SIZE];
    unsafe { slice.copy_from_user(&mut bytes) }.map_err(|_| SyscallError::InvalidArgument)?;
    let request = fullerene_abi::PciConfigRequest::from_ne_bytes(bytes);
    if request.reserved != 0
        || !matches!(request.width, 1 | 2 | 4)
        || request.offset as usize >= 0x100
        || (request.width == 2 && request.offset % 2 != 0)
        || (request.width == 4 && request.offset % 4 != 0)
        || request.offset as usize + request.width as usize > 0x100
    {
        return Err(SyscallError::InvalidArgument);
    }
    Ok(request)
}

fn read_mmio_request(arg: u64, write: bool) -> Result<fullerene_abi::MmioRequest, SyscallError> {
    let slice = UserSlice::new(arg as *mut u8, fullerene_abi::MmioRequest::BYTE_SIZE, false)
        .map_err(|_| SyscallError::InvalidArgument)?;
    let mut bytes = [0u8; fullerene_abi::MmioRequest::BYTE_SIZE];
    unsafe { slice.copy_from_user(&mut bytes) }.map_err(|_| SyscallError::InvalidArgument)?;
    let request = fullerene_abi::MmioRequest::from_ne_bytes(bytes);
    if request.reserved != 0
        || !matches!(request.width, 1 | 2 | 4 | 8)
        || request.offset % request.width as u32 != 0
        || (write
            && request.width < 8
            && request.value > ((1u64 << (request.width as u32 * 8)) - 1))
    {
        return Err(SyscallError::InvalidArgument);
    }
    Ok(request)
}

fn read_pci_config(
    device: &nitrogen::pci::PciDevice,
    request: &fullerene_abi::PciConfigRequest,
) -> Result<u32, SyscallError> {
    let value = match request.width {
        1 => nitrogen::pci::PciConfigSpace::read_config_byte(
            device.bus,
            device.device,
            device.function,
            request.offset as u8,
        ) as u32,
        2 => nitrogen::pci::PciConfigSpace::read_config_word(
            device.bus,
            device.device,
            device.function,
            request.offset as u8,
        ) as u32,
        4 => nitrogen::pci::PciConfigSpace::read_config_dword(
            device.bus,
            device.device,
            device.function,
            request.offset as u8,
        ),
        _ => return Err(SyscallError::InvalidArgument),
    };
    // All-ones is the absent-device sentinel only for the vendor/device ID
    // dword. BAR probes and other configuration registers may legally return
    // 0xFFFF_FFFF.
    if value == u32::MAX && request.width == 4 && request.offset == 0 {
        return Err(SyscallError::Io);
    }
    Ok(value)
}

/// Restrict user writes to the harmless cache-line/latency bytes. Command,
/// BAR, expansion-ROM, interrupt, and capability registers can change address
/// decoding or DMA ownership and are never writable through this ioctl.
fn is_safe_pci_config_write(offset: u16, width: u8) -> bool {
    matches!((offset, width), (0x0C, 1) | (0x0D, 1))
}

fn write_pci_config(
    device: &nitrogen::pci::PciDevice,
    request: &fullerene_abi::PciConfigRequest,
) -> SyscallResult {
    match request.width {
        1 => {
            if request.value > u8::MAX as u32 {
                return Err(SyscallError::InvalidArgument);
            }
            let aligned = request.offset & !3;
            let old = nitrogen::pci::PciConfigSpace::read_config_dword(
                device.bus,
                device.device,
                device.function,
                aligned as u8,
            );
            let shift = (request.offset & 3) * 8;
            let value = (old & !(0xFF << shift)) | (request.value << shift);
            nitrogen::pci::PciConfigSpace::write_config_dword_raw(
                device.bus,
                device.device,
                device.function,
                aligned as u8,
                value,
            );
        }
        2 => {
            if request.value > u16::MAX as u32 {
                return Err(SyscallError::InvalidArgument);
            }
            nitrogen::pci::PciConfigSpace::write_config_word_raw(
                device.bus,
                device.device,
                device.function,
                request.offset as u8,
                request.value as u16,
            );
        }
        4 => nitrogen::pci::PciConfigSpace::write_config_dword_raw(
            device.bus,
            device.device,
            device.function,
            request.offset as u8,
            request.value,
        ),
        _ => return Err(SyscallError::InvalidArgument),
    }
    Ok(0)
}

fn pci_device_class(device: &nitrogen::pci::PciDevice) -> u32 {
    match (device.class_code, device.subclass) {
        (0x01, _) => fullerene_abi::device_class::STORAGE,
        (0x02, _) => fullerene_abi::device_class::NETWORK,
        (0x03, _) => fullerene_abi::device_class::DISPLAY,
        (0x04, _) => fullerene_abi::device_class::AUDIO,
        (0x0C, 0x03) => fullerene_abi::device_class::USB,
        (0x0C, _) => fullerene_abi::device_class::INPUT,
        _ => fullerene_abi::device_class::OTHER,
    }
}

fn stable_device_id(name: &str) -> u32 {
    let mut hash = 0x811C9DC5u32;
    for byte in name.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

fn device_class(device: &DeviceState) -> u32 {
    device
        .pci
        .as_ref()
        .map_or(fullerene_abi::device_class::STORAGE, pci_device_class)
}

fn device_capabilities(device: &DeviceState) -> u64 {
    if device.name.is_some() {
        return fullerene_abi::device_capability::BLOCK_INFO
            | fullerene_abi::device_capability::BLOCK_READ
            | fullerene_abi::device_capability::BLOCK_WRITE;
    }
    let Some(pci) = device.pci.as_ref() else {
        return 0;
    };
    let mut capabilities = fullerene_abi::device_capability::PCI_CONFIG_READ
        | fullerene_abi::device_capability::PCI_CONFIG_WRITE;
    match (pci.class_code, pci.subclass) {
        (0x01, 0x08) => {
            capabilities |= fullerene_abi::device_capability::NVME_INITIALIZE
                | fullerene_abi::device_capability::MMIO_READ
                | fullerene_abi::device_capability::MMIO_WRITE;
        }
        (0x01, 0x06) => {
            capabilities |= fullerene_abi::device_capability::AHCI_INITIALIZE;
        }
        _ => {}
    }
    capabilities
}

fn read_block_request(arg: u64) -> Result<fullerene_abi::BlockRequest, SyscallError> {
    let slice = UserSlice::new(
        arg as *mut u8,
        fullerene_abi::BlockRequest::BYTE_SIZE,
        false,
    )
    .map_err(|_| SyscallError::InvalidArgument)?;
    let mut bytes = [0u8; fullerene_abi::BlockRequest::BYTE_SIZE];
    unsafe { slice.copy_from_user(&mut bytes) }.map_err(|_| SyscallError::InvalidArgument)?;
    let request = fullerene_abi::BlockRequest::from_ne_bytes(bytes);
    if request.reserved != 0 || request.count == 0 || request.buffer_ptr == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    Ok(request)
}

fn device_id_matches(id: &str, device: &nitrogen::pci::PciDevice) -> bool {
    let id = id.strip_prefix("pci:").unwrap_or(id);
    let fields: alloc::vec::Vec<&str> = id.split([':', '.', '/']).collect();
    match fields.as_slice() {
        [bus, dev, function] => {
            parse_hex(bus) == Some(device.bus)
                && parse_hex(dev) == Some(device.device)
                && parse_hex(function) == Some(device.function)
        }
        [_, bus, dev, function] => {
            // This four-field form accepts the conventional domain-qualified
            // PCI spelling (domain:bus:device.function). The raw numeric BDF
            // fallback below is a separate ioctl byte-per-field encoding, not
            // the packed BDF used by DriverContext::dma_map.
            parse_hex(bus) == Some(device.bus)
                && parse_hex(dev) == Some(device.device)
                && parse_hex(function) == Some(device.function)
        }
        [vendor, product] => {
            parse_hex_u16(vendor) == Some(device.vendor_id)
                && parse_hex_u16(product) == Some(device.device_id)
        }
        // Raw hexadecimal BDFs use the ioctl identifier's byte-per-field
        // encoding (bus << 16 | device << 8 | function). This is distinct
        // from DriverContext::dma_map's packed PCI BDF representation.
        _ => parse_hex_u32(id).is_some_and(|bdf| {
            ((bdf >> 16) as u8) == device.bus
                && ((bdf >> 8) as u8) == device.device
                && (bdf as u8) == device.function
        }),
    }
}

fn find_device(id: &str, devices: &[nitrogen::pci::PciDevice]) -> Option<nitrogen::pci::PciDevice> {
    let normalized = id.strip_prefix("pci:").unwrap_or(id);
    if let Some(index) = normalized
        .strip_prefix("nvme")
        .and_then(|value| value.parse::<usize>().ok())
    {
        return devices
            .iter()
            .filter(|device| device.class_code == 0x01 && device.subclass == 0x08)
            .nth(index)
            .cloned();
    }
    if let Some(index) = normalized
        .strip_prefix("ahci")
        .and_then(|value| value.parse::<usize>().ok())
    {
        return devices
            .iter()
            .filter(|device| device.class_code == 0x01 && device.subclass == 0x06)
            .nth(index)
            .cloned();
    }
    devices
        .iter()
        .find(|device| device_id_matches(normalized, device))
        .cloned()
}

fn parse_hex(value: &str) -> Option<u8> {
    parse_hex_u16(value).and_then(|value| u8::try_from(value).ok())
}

fn parse_hex_u16(value: &str) -> Option<u16> {
    u16::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16).ok()
}

fn parse_hex_u32(value: &str) -> Option<u32> {
    u32::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::find_device;
    use nitrogen::pci::PciDevice;

    fn pci(bus: u8, device: u8, function: u8, class_code: u8, subclass: u8) -> PciDevice {
        PciDevice {
            bus,
            device,
            function,
            handle: 0,
            vendor_id: 0x8086,
            device_id: 0x5845,
            class_code,
            subclass,
            prog_if: 0,
            header_type: 0,
        }
    }

    #[test]
    fn open_device_resolves_nvme_stable_names() {
        let devices = [
            pci(0, 1, 0, 0x03, 0x00),
            pci(2, 3, 0, 0x01, 0x08),
            pci(4, 5, 1, 0x01, 0x08),
        ];
        assert_eq!(find_device("nvme0", &devices).map(|d| d.bus), Some(2));
        assert_eq!(find_device("nvme1", &devices).map(|d| d.bus), Some(4));
    }

    #[test]
    fn open_device_resolves_ahci_stable_names() {
        let devices = [pci(2, 3, 0, 0x01, 0x06), pci(4, 5, 1, 0x01, 0x06)];
        assert_eq!(find_device("ahci0", &devices).map(|d| d.bus), Some(2));
        assert_eq!(find_device("ahci1", &devices).map(|d| d.bus), Some(4));
    }

    #[test]
    fn open_device_resolves_bdf_and_vendor_product() {
        let devices = [pci(2, 3, 1, 0x01, 0x08)];
        assert!(find_device("02:03.1", &devices).is_some());
        assert!(find_device("8086:5845", &devices).is_some());
        assert!(find_device("02:04.0", &devices).is_none());
    }
}
