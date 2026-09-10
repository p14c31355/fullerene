//! Bounded AArch64 platform-device inventory.
//!
//! The Bramble USB gadget and DT-described platform devices share one bounded
//! inventory.  The USB VID/PID is decoded from the same descriptor that the
//! DWC3 EP0 path presents to the host, while platform rows retain their first
//! DT `reg` window as descriptive metadata.  This is still an inventory
//! boundary: no platform driver or arbitrary MMIO access is implied.

use fullerene_abi::{
    BlockDeviceInfo, DeviceCapabilityInfo, DeviceInfo, DeviceResourceInfo, device_capability,
    device_class, device_ioctl, device_resource,
};

#[cfg(fullerene_aarch64_bramble)]
use fullerene_abi::BlockRequest;

use super::{fdt, fs, task, ufs, user_memory};

const ERR_ADDRESS: u64 = (-(14i64)) as u64;
const ERR_BAD_FD: u64 = (-(9i64)) as u64;
const ERR_INVALID: u64 = (-(22i64)) as u64;
const ERR_IO: u64 = (-(5i64)) as u64;
const ERR_NAME_TOO_LONG: u64 = (-(36i64)) as u64;
const ERR_NO_DEVICE: u64 = (-(19i64)) as u64;
const ERR_NOT_SUPPORTED: u64 = (-(95i64)) as u64;
const MAX_DEVICE_BYTES: usize = 1 << 20;
const MAX_UFS_READ_BYTES: usize = 256 * 1024;
const MAX_DEVICES: usize = 8;
const MAX_IDENTIFIER: usize = 64;
const USB_DESCRIPTOR_VENDOR_OFFSET: usize = 8;
const USB_DESCRIPTOR_PRODUCT_OFFSET: usize = 10;
const EMPTY_RESOURCE: DeviceResourceInfo = DeviceResourceInfo {
    base: 0,
    size: 0,
    kind: 0,
    reserved: 0,
};

static mut DEVICES: [DeviceInfo; MAX_DEVICES] = [DeviceInfo {
    class: 0,
    device_id: 0,
    vendor_id: 0,
    product_id: 0,
}; MAX_DEVICES];
static mut DEVICE_COUNT: usize = 0;
static mut DEVICE_REFERENCES: [u16; MAX_DEVICES] = [0; MAX_DEVICES];
static mut DEVICE_RESOURCES: [DeviceResourceInfo; MAX_DEVICES] = [EMPTY_RESOURCE; MAX_DEVICES];

const DEVICE_ID_USB_GADGET: u32 = 1;
const DEVICE_ID_UFS: u32 = 2;
const DEVICE_ID_DISPLAY: u32 = 3;
const DEVICE_ID_SDMMC0: u32 = 4;
const DEVICE_ID_SDMMC1: u32 = 5;
const DEVICE_ID_QEMU_UART: u32 = 6;
const DEVICE_ID_TOUCH: u32 = 7;

// A user buffer cannot be used as a UFS DMA target: the UTP contract only
// covers the fixed, cache-maintained arena owned by the storage backend.
// Serialize the bounded copy-out scratch buffer with the early AArch64
// device inventory's simple locking model.
#[cfg(fullerene_aarch64_bramble)]
static UFS_READ_SCRATCH: spin::Mutex<[u8; MAX_UFS_READ_BYTES]> =
    spin::Mutex::new([0; MAX_UFS_READ_BYTES]);

/// Publish the platform identity after the early USB handoff has been
/// configured.  The descriptor is the source of truth for both fields:
/// `DEVICE_DESCRIPTOR[8..10]` is little-endian idVendor and
/// `[10..12]` is little-endian idProduct.
pub(crate) fn init(usb_ready: bool, dtb_address: Option<u64>) {
    unsafe {
        DEVICE_COUNT = 0;
        DEVICE_REFERENCES = [0; MAX_DEVICES];
        DEVICE_RESOURCES = [EMPTY_RESOURCE; MAX_DEVICES];
    }
    #[cfg(fullerene_aarch64_bramble)]
    {
        if !usb_ready {
            return;
        }
        let descriptor = super::usb_protocol::DEVICE_DESCRIPTOR;
        if descriptor.len() >= USB_DESCRIPTOR_PRODUCT_OFFSET + 2
            && descriptor[0] == 18
            && descriptor[1] == 1
        {
            let vendor_id = u16::from_le_bytes([
                descriptor[USB_DESCRIPTOR_VENDOR_OFFSET],
                descriptor[USB_DESCRIPTOR_VENDOR_OFFSET + 1],
            ]) as u32;
            let product_id = u16::from_le_bytes([
                descriptor[USB_DESCRIPTOR_PRODUCT_OFFSET],
                descriptor[USB_DESCRIPTOR_PRODUCT_OFFSET + 1],
            ]) as u32;
            let resource = dtb_address
                .and_then(|address| fdt::find_compatible(address, b"snps,dwc3"))
                .or_else(|| {
                    dtb_address
                        .and_then(|address| fdt::find_compatible(address, b"qcom,dwc-usb3-msm"))
                })
                .map(mmio_resource)
                .unwrap_or_default();
            unsafe {
                push_device(
                    DeviceInfo {
                        class: device_class::USB,
                        // The native DeviceInfo ABI has no USB bus/port field.
                        // Use the descriptor product as the stable local ID.
                        device_id: DEVICE_ID_USB_GADGET,
                        vendor_id,
                        product_id,
                    },
                    resource,
                );
            }
        }
    }
    if let Some(address) = dtb_address {
        // QEMU's PL011 is a useful resource-bearing smoke row.  It exercises
        // the same DT metadata path without pretending that the QEMU device
        // is one of Bramble's Qualcomm peripherals.
        add_platform_device(
            address,
            b"arm,pl011",
            device_class::OTHER,
            DEVICE_ID_QEMU_UART,
        );
        // These are identity-only platform rows.  The compatible/resource
        // match comes from the boot DTB; no MMIO access is implied until a
        // device-specific driver is implemented.
        if ufs::platform_ready() {
            add_platform_device(address, b"qcom,ufshc", device_class::STORAGE, DEVICE_ID_UFS);
        }
        // Bramble's active MDSS node is compatible with qcom,sde-kms.  Keep
        // the older qcom,mdss_mdp spelling as a source-compatible fallback;
        // matching only the latter silently omitted the real display node
        // after DTBO 17 was applied.
        if !add_platform_device(
            address,
            b"qcom,sde-kms",
            device_class::DISPLAY,
            DEVICE_ID_DISPLAY,
        ) {
            add_platform_device(
                address,
                b"qcom,mdss_mdp",
                device_class::DISPLAY,
                DEVICE_ID_DISPLAY,
            );
        }
        add_platform_device(
            address,
            b"qcom,sdhci-msm-v5",
            device_class::STORAGE,
            DEVICE_ID_SDMMC0,
        );
        if fdt::find_compatible_nth(address, b"qcom,sdhci-msm-v5", 1).is_some() {
            add_platform_device(
                address,
                b"qcom,sdhci-msm-v5",
                device_class::STORAGE,
                DEVICE_ID_SDMMC1,
            );
        }
        // DTBO 17 enables the FocalTech SPI touchscreen on stock Bramble.
        // Synaptics DSX remains a valid source-tree variant, but must not be
        // invented when the active boot DT selects the other controller.
        if !add_platform_device(address, b"st,fts", device_class::INPUT, DEVICE_ID_TOUCH) {
            add_platform_device(
                address,
                b"synaptics,dsx-i2c",
                device_class::INPUT,
                DEVICE_ID_TOUCH,
            );
        }
    }
}

fn mmio_resource(region: fdt::Region) -> DeviceResourceInfo {
    DeviceResourceInfo {
        base: region.base,
        size: region.size,
        kind: device_resource::MMIO,
        reserved: 0,
    }
}

fn push_device(device: DeviceInfo, resource: DeviceResourceInfo) {
    unsafe {
        if DEVICE_COUNT < MAX_DEVICES {
            let slot = DEVICE_COUNT;
            DEVICES[slot] = device;
            DEVICE_RESOURCES[slot] = resource;
            DEVICE_COUNT += 1;
        }
    }
}

fn add_platform_device(address: u64, compatible: &[u8], class: u32, device_id: u32) -> bool {
    if let Some(region) = fdt::find_compatible(address, compatible) {
        push_device(
            DeviceInfo {
                class,
                device_id,
                // Platform rows are not PCI/USB identities.  Keep these fields
                // zero rather than inventing a vendor/product pair.
                vendor_id: 0,
                product_id: 0,
            },
            mmio_resource(region),
        );
        true
    } else {
        false
    }
}

/// Copy the bounded platform inventory into an EL0 buffer and return the
/// total number of matching records, including records that did not fit.
pub(crate) fn enumerate(class: u64, buffer_address: u64, buffer_size: u64) -> u64 {
    let buffer_size = usize::try_from(buffer_size).unwrap_or(usize::MAX);
    if buffer_address == 0 || buffer_size == 0 || buffer_size > MAX_DEVICE_BYTES {
        return ERR_INVALID;
    }
    let requested_class = u32::try_from(class).unwrap_or(u32::MAX);
    let capacity = buffer_size / DeviceInfo::BYTE_SIZE;
    let mut matching = 0usize;
    let mut copied = 0usize;
    unsafe {
        for device in DEVICES[..DEVICE_COUNT].iter().copied() {
            if requested_class != device_class::ANY && requested_class != device.class {
                continue;
            }
            if copied < capacity {
                let bytes = device.to_ne_bytes();
                let address = buffer_address
                    .checked_add((copied * DeviceInfo::BYTE_SIZE) as u64)
                    .ok_or(())
                    .unwrap_or(0);
                if address == 0 || user_memory::copy_to_user(address, &bytes).is_err() {
                    return ERR_ADDRESS;
                }
                copied += 1;
            }
            matching += 1;
        }
    }
    matching as u64
}

pub(crate) fn retain(slot: u8) -> bool {
    unsafe {
        let slot = slot as usize;
        if slot >= DEVICE_COUNT {
            return false;
        }
        DEVICE_REFERENCES[slot] = DEVICE_REFERENCES[slot].saturating_add(1);
        true
    }
}

pub(crate) fn release(slot: u8) {
    unsafe {
        let slot = slot as usize;
        if slot < DEVICE_COUNT {
            DEVICE_REFERENCES[slot] = DEVICE_REFERENCES[slot].saturating_sub(1);
        }
    }
}

/// Open one inventory row by its stable `VID:PID` spelling or local numeric
/// id. This creates the same owner/generation-checked handle used by files
/// and other native resources.
pub(crate) fn open(identifier_address: u64) -> u64 {
    let Some(owner_pid) = task::resource_owner_pid() else {
        return ERR_BAD_FD;
    };
    let (identifier, length) = match copy_identifier(identifier_address) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let identifier = &identifier[..length];
    let slot =
        unsafe { (0..DEVICE_COUNT).find(|&slot| identifier_matches(identifier, DEVICES[slot])) };
    let Some(slot) = slot else {
        return ERR_NO_DEVICE;
    };
    fs::install_device_handle(owner_pid, slot as u8).unwrap_or_else(|error| error)
}

/// Expose native device operations. UFS is advertised only after the guarded
/// Bramble read-only probe has installed a geometry-bearing backend. Its
/// user-facing path copies through a kernel scratch buffer, so arbitrary EL0
/// memory is never handed to the UFS DMA engine. `GET_RESOURCES` remains
/// metadata-only and does not authorize direct MMIO access.
pub(crate) fn ioctl(handle: u64, command: u64, argument: u64) -> u64 {
    let Some(slot) = fs::device_slot(handle) else {
        return ERR_BAD_FD;
    };
    if !matches!(
        command,
        device_ioctl::GET_CAPABILITIES
            | device_ioctl::GET_RESOURCES
            | device_ioctl::GET_BLOCK_INFO
            | device_ioctl::READ_BLOCKS
            | device_ioctl::WRITE_BLOCKS
    ) {
        return ERR_NOT_SUPPORTED;
    }
    let info = unsafe {
        let devices = core::ptr::addr_of!(DEVICES);
        if (slot as usize) < DEVICE_COUNT {
            Some((*devices)[slot as usize])
        } else {
            None
        }
    };
    let Some(info) = info else {
        return ERR_BAD_FD;
    };
    if command == device_ioctl::GET_RESOURCES {
        let resource = unsafe {
            let resources = core::ptr::addr_of!(DEVICE_RESOURCES);
            if (slot as usize) < DEVICE_COUNT {
                Some((*resources)[slot as usize])
            } else {
                None
            }
        };
        let Some(resource) = resource else {
            return ERR_BAD_FD;
        };
        if resource.kind == 0 || resource.base == 0 || resource.size == 0 {
            return ERR_NOT_SUPPORTED;
        }
        return if user_memory::copy_to_user(argument, &resource.to_ne_bytes()).is_err() {
            ERR_ADDRESS
        } else {
            0
        };
    }
    if info.device_id == DEVICE_ID_UFS {
        match command {
            device_ioctl::GET_BLOCK_INFO => {
                let Some((sector_size, total_sectors)) = ufs::bramble_block_info() else {
                    return ERR_NO_DEVICE;
                };
                let block_info = BlockDeviceInfo {
                    sector_size,
                    reserved: 0,
                    total_sectors,
                };
                return if user_memory::copy_to_user(argument, &block_info.to_ne_bytes()).is_err() {
                    ERR_ADDRESS
                } else {
                    0
                };
            }
            device_ioctl::READ_BLOCKS => {
                let Some((sector_size, total_sectors)) = ufs::bramble_block_info() else {
                    return ERR_NO_DEVICE;
                };
                #[cfg(fullerene_aarch64_bramble)]
                {
                    return read_ufs_blocks(argument, sector_size, total_sectors);
                }
                #[cfg(not(fullerene_aarch64_bramble))]
                {
                    let _ = (argument, sector_size, total_sectors);
                    return ERR_NOT_SUPPORTED;
                }
            }
            device_ioctl::WRITE_BLOCKS => {
                // No WRITE(10), descriptor write, format, or partition write
                // is reachable through the first AArch64 UFS registration.
                return ERR_NOT_SUPPORTED;
            }
            _ => {}
        }
    }
    if command != device_ioctl::GET_CAPABILITIES {
        return ERR_NOT_SUPPORTED;
    }
    let capabilities = DeviceCapabilityInfo {
        class: info.class,
        reserved: 0,
        capabilities: if info.device_id == DEVICE_ID_UFS && ufs::bramble_block_info().is_some() {
            device_capability::BLOCK_INFO | device_capability::BLOCK_READ
        } else {
            0
        },
    };
    if user_memory::copy_to_user(argument, &capabilities.to_ne_bytes()).is_err() {
        ERR_ADDRESS
    } else {
        0
    }
}

#[cfg(fullerene_aarch64_bramble)]
fn read_ufs_blocks(argument: u64, sector_size: u32, total_sectors: u64) -> u64 {
    if argument == 0 || sector_size == 0 {
        return ERR_INVALID;
    }
    let mut request_bytes = [0u8; BlockRequest::BYTE_SIZE];
    if user_memory::copy_from_user(argument, &mut request_bytes).is_err() {
        return ERR_ADDRESS;
    }
    let request = BlockRequest::from_ne_bytes(request_bytes);
    if request.reserved != 0 || request.count == 0 || request.buffer_ptr == 0 {
        return ERR_INVALID;
    }
    let required = match (request.count as usize).checked_mul(sector_size as usize) {
        Some(required) if required <= MAX_UFS_READ_BYTES => required,
        _ => return ERR_INVALID,
    };
    if (request.buffer_len as usize) < required {
        return ERR_INVALID;
    }
    match request.lba.checked_add(request.count as u64) {
        Some(end) if end <= total_sectors => {}
        _ => return ERR_INVALID,
    }

    let mut scratch = UFS_READ_SCRATCH.lock();
    if ufs::read_bramble_blocks(request.lba, request.count, &mut scratch[..required]).is_err() {
        return ERR_IO;
    }
    if user_memory::copy_to_user(request.buffer_ptr, &scratch[..required]).is_err() {
        return ERR_ADDRESS;
    }
    required as u64
}

fn copy_identifier(address: u64) -> Result<([u8; MAX_IDENTIFIER], usize), u64> {
    if address == 0 {
        return Err(ERR_ADDRESS);
    }
    let mut bytes = [0u8; MAX_IDENTIFIER];
    for (offset, destination) in bytes.iter_mut().enumerate() {
        let mut byte = [0u8; 1];
        user_memory::copy_from_user(
            address.checked_add(offset as u64).ok_or(ERR_ADDRESS)?,
            &mut byte,
        )
        .map_err(|_| ERR_ADDRESS)?;
        if byte[0] == 0 {
            return Ok((bytes, offset));
        }
        *destination = byte[0];
    }
    Err(ERR_NAME_TOO_LONG)
}

fn identifier_matches(identifier: &[u8], device: DeviceInfo) -> bool {
    let identifier = identifier
        .strip_prefix(b"/dev/usb/")
        .or_else(|| identifier.strip_prefix(b"usb/"))
        .unwrap_or(identifier);
    if let Some(separator) = identifier.iter().position(|&byte| byte == b':') {
        return parse_hex(&identifier[..separator]) == Some(device.vendor_id)
            && parse_hex(&identifier[separator + 1..]) == Some(device.product_id);
    }
    parse_hex(identifier) == Some(device.device_id)
}

fn parse_hex(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let bytes = bytes
        .strip_prefix(b"0x")
        .or_else(|| bytes.strip_prefix(b"0X"))
        .unwrap_or(bytes);
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0u32;
    for &byte in bytes {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        value = value.checked_mul(16)?.checked_add(digit as u32)?;
    }
    Some(value)
}
