use fastboot_protocol::nusb::{self as protocol_nusb, DeviceInfo, NusbFastBoot};
use nusb::{
    descriptors::TransferType,
    transfer::{Buffer, In, Out},
    transfer::{Bulk, Direction},
};
use std::{fmt::Display, fs, io, path::Path, time::Duration};
use tokio::runtime::Builder;

const EXPECTED_PRODUCT: &str = "bramble";

pub fn run_device() -> io::Result<()> {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(other)?
        .block_on(async {
            let devices = protocol_nusb::devices().await.map_err(other)?;
            let devices: Vec<_> = devices.collect();
            if devices.is_empty() {
                return Err(other("no USB Fastboot device found"));
            }
            for info in &devices {
                print_device(info);
                print_vars(info, &["product", "serialno", "version"]).await;
            }
            Ok(())
        })
}

pub fn run_boot(image: &Path) -> io::Result<()> {
    let image = fs::read(image)?;
    let image_size =
        u32::try_from(image.len()).map_err(|_| other("Fastboot image is larger than 4 GiB"))?;
    Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(other)?
        .block_on(async move {
            let info = single_device().await?;
            print_device(&info);

            let mut fastboot = NusbFastBoot::from_info(&info).await.map_err(other)?;
            let product = fastboot.get_var("product").await.map_err(other)?;
            if product.trim() != EXPECTED_PRODUCT {
                return Err(other(format!(
                    "refusing to boot: expected product {EXPECTED_PRODUCT}, found {}",
                    product.trim()
                )));
            }
            println!("product: {}", product.trim());
            match fastboot.get_var("unlocked").await {
                Ok(value) => {
                    let value = value.trim();
                    println!("unlocked: {value}");
                    if value.eq_ignore_ascii_case("no") {
                        return Err(other(
                            "refusing to boot: the device bootloader reports unlocked=no",
                        ));
                    }
                }
                Err(error) => println!("unlocked: unavailable ({error})"),
            }

            println!("downloading: {} bytes", image.len());
            let mut download = fastboot.download(image_size).await.map_err(other)?;
            download.extend_from_slice(&image).await.map_err(other)?;
            download.finish().await.map_err(other)?;
            drop(fastboot);

            send_boot_command(&info).await
        })
}

async fn single_device() -> io::Result<DeviceInfo> {
    let devices = protocol_nusb::devices().await.map_err(other)?;
    let devices: Vec<_> = devices.collect();
    match devices.len() {
        0 => Err(other("no USB Fastboot device found")),
        1 => Ok(devices.into_iter().next().unwrap()),
        count => Err(other(format!(
            "refusing to choose between {count} USB Fastboot devices"
        ))),
    }
}

async fn print_vars(info: &DeviceInfo, names: &[&str]) {
    match NusbFastBoot::from_info(info).await {
        Ok(mut fastboot) => {
            for name in names {
                match fastboot.get_var(name).await {
                    Ok(value) => println!("{name}: {}", value.trim()),
                    Err(error) => println!("{name}: unavailable ({error})"),
                }
            }
        }
        Err(error) => {
            for name in names {
                println!("{name}: unavailable ({error})");
            }
        }
    }
}

fn print_device(info: &DeviceInfo) {
    println!(
        "fastboot: bus={} address={} manufacturer={} product={}",
        info.bus_id(),
        info.device_address(),
        info.manufacturer_string().unwrap_or_default(),
        info.product_string().unwrap_or_default(),
    );
}

async fn send_boot_command(info: &DeviceInfo) -> io::Result<()> {
    let interface_number = NusbFastBoot::find_fastboot_interface(info)
        .ok_or_else(|| other("Fastboot interface disappeared before boot command"))?;
    let device = info.open().await.map_err(other)?;
    let interface = device
        .claim_interface(interface_number)
        .await
        .map_err(other)?;
    let (out_address, in_address, max_in) = find_bulk_endpoints(&interface)?;
    let mut ep_out = interface
        .endpoint::<Bulk, Out>(out_address)
        .map_err(other)?;
    let mut ep_in = interface.endpoint::<Bulk, In>(in_address).map_err(other)?;

    ep_out.submit(b"boot".to_vec().into());
    ep_out.next_complete().await.into_result().map_err(other)?;

    // Fastboot bootloaders commonly detach immediately after accepting boot.
    // Give a response a short window for diagnostics, but do not require it
    // after the command has been transferred successfully.
    enum BootResponse {
        Accepted(String),
        Failed(String),
        Disconnected(io::Error),
    }

    let response = tokio::time::timeout(Duration::from_millis(750), async {
        loop {
            ep_in.submit(Buffer::new(max_in));
            let bytes = match ep_in.next_complete().await.into_result() {
                Ok(bytes) => bytes,
                Err(error) => return Ok(BootResponse::Disconnected(other(error))),
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if text.starts_with("INFO") || text.starts_with("TEXT") {
                println!("Fastboot: {}", text.trim());
                continue;
            }
            if text.starts_with("FAIL") {
                return Ok(BootResponse::Failed(text));
            }
            if text.starts_with("OKAY") {
                return Ok(BootResponse::Accepted(text));
            }
            return Err(other(format!("unexpected Fastboot boot response: {text}")));
        }
    })
    .await;
    match response {
        Ok(Ok(BootResponse::Accepted(text))) => {
            println!("Fastboot boot accepted: {}", text.trim());
        }
        Ok(Ok(BootResponse::Failed(text))) => {
            return Err(other(format!("Fastboot boot failed: {text}")));
        }
        Ok(Ok(BootResponse::Disconnected(error))) => {
            println!("Fastboot boot command sent; device disconnected ({error})");
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => println!("Fastboot boot command sent; waiting for device reboot"),
    }
    Ok(())
}

fn find_bulk_endpoints(interface: &nusb::Interface) -> io::Result<(u8, u8, usize)> {
    let descriptor = interface
        .descriptors()
        .find(|alternate| {
            alternate
                .endpoints()
                .any(|endpoint| endpoint.transfer_type() == TransferType::Bulk)
        })
        .ok_or_else(|| other("Fastboot interface has no bulk endpoints"))?;
    let out = descriptor
        .endpoints()
        .find(|endpoint| {
            endpoint.transfer_type() == TransferType::Bulk && endpoint.direction() == Direction::Out
        })
        .ok_or_else(|| other("Fastboot interface has no bulk OUT endpoint"))?;
    let input = descriptor
        .endpoints()
        .find(|endpoint| {
            endpoint.transfer_type() == TransferType::Bulk && endpoint.direction() == Direction::In
        })
        .ok_or_else(|| other("Fastboot interface has no bulk IN endpoint"))?;
    Ok((
        out.address(),
        input.address(),
        input.max_packet_size() as usize,
    ))
}

fn other(error: impl Display) -> io::Error {
    io::Error::other(error.to_string())
}
