// fullerene/flasks/src/main.rs
use clap::Parser;
use isobemak::{BiosBootInfo, BootInfo, IsoImage, IsoImageFile, UefiBootInfo, build_iso};
use std::{env, io, path::PathBuf, process::Command};

use env_logger;

#[derive(Parser)]
struct Args {
    /// Use VirtualBox instead of QEMU for virtualization
    #[arg(long, default_value = "false")]
    virtualbox: bool,

    /// VirtualBox VM name (default: fullerene-vm)
    #[arg(long, default_value = "fullerene-vm")]
    vm_name: String,

    /// IDE controller name (default: IDE Controller)
    #[arg(long, default_value = "IDE Controller")]
    controller: String,

    /// Start VM in GUI mode instead of headless (useful for debugging)
    #[arg(long, default_value = "true")]
    gui: bool,
}

fn main() -> io::Result<()> {
    // Initialize env_logger - it will respect RUST_LOG environment variable for filtering
    env_logger::init();
    let args = Args::parse();
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Failed to get workspace root")
        .to_path_buf();

    if args.virtualbox {
        run_virtualbox(&args, &workspace_root)?;
    } else {
        run_qemu(&workspace_root)?;
    }
    Ok(())
}

fn create_iso_and_setup(
    workspace_root: &PathBuf,
) -> io::Result<(PathBuf, PathBuf, PathBuf, tempfile::NamedTempFile)> {
    // --- 1. Build fullerene-kernel (no_std) ---
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "+nightly",
            "build",
            "-q",
            "-Zbuild-std=core,alloc",
            "--package",
            "fullerene-kernel",
            "--target",
            "x86_64-unknown-uefi",
            "--profile",
            "dev",
        ])
        .status()?;
    if !status.success() {
        return Err(io::Error::other("fullerene-kernel build failed"));
    }

    let target_dir = workspace_root
        .join("target")
        .join("x86_64-unknown-uefi")
        .join("debug");
    let kernel_path = target_dir.join("fullerene-kernel.efi");
    // Copy kernel to bellows/src for embedding
    std::fs::copy(&kernel_path, "bellows/src/kernel.bin")?;

    // --- 2. Build bellows (no_std) ---
    // For BIOS mode, skip bellows
    let bellows_path = target_dir.join("bellows.efi");
    if bellows_path.exists() {
        // Use existing
    } else {
        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args([
                "+nightly",
                "build",
                "-q",
                "-Zbuild-std=core,alloc",
                "--features",
                "debug_loader",
                "--package",
                "bellows",
                "--target",
                "x86_64-unknown-uefi",
                "--profile",
                "dev",
            ])
            .status()?;
        if !status.success() {
            return Err(io::Error::other("bellows build failed"));
        }
    }

    // --- 3. Create ISO using isobemak ---
    let iso_path = workspace_root.join("fullerene.iso");

    // Create dummy file for BIOS boot catalog path required by BiosBootInfo struct
    let dummy_boot_catalog_path = workspace_root.join("dummy_boot_catalog.bin");
    std::fs::File::create(&dummy_boot_catalog_path)?;

    let image = IsoImage {
        files: vec![
            IsoImageFile {
                source: kernel_path.clone(),
                destination: "EFI\\BOOT\\KERNEL.EFI".to_string(),
            },
            IsoImageFile {
                source: bellows_path.clone(),
                destination: "EFI\\BOOT\\BOOTX64.EFI".to_string(),
            },
        ],
        boot_info: BootInfo {
            bios_boot: Some(BiosBootInfo {
                boot_catalog: dummy_boot_catalog_path.clone(),
                boot_image: bellows_path.clone(),
                destination_in_iso: "EFI\\BOOT\\BOOTX64.EFI".to_string(),
            }),
            uefi_boot: Some(UefiBootInfo {
                boot_image: bellows_path.clone(),
                kernel_image: kernel_path.clone(),
                destination_in_iso: "EFI\\BOOT\\BOOTX64.EFI".to_string(),
            }),
        },
    };
    build_iso(&iso_path, &image, true)?; // Set to true for isohybrid UEFI boot

    let ovmf_fd_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_CODE.fd");
    let ovmf_vars_fd_original_path = workspace_root
        .join("flasks")
        .join("ovmf")
        .join("RELEASEX64_OVMF_VARS.fd");

    // Create a temporary file for OVMF_VARS.fd to ensure a clean state each run
    let mut temp_ovmf_vars_fd = tempfile::NamedTempFile::new()?;
    std::io::copy(
        &mut std::fs::File::open(&ovmf_vars_fd_original_path)?,
        temp_ovmf_vars_fd.as_file_mut(),
    )?;
    let ovmf_vars_fd_path = temp_ovmf_vars_fd.path().to_path_buf();

    Ok((iso_path, ovmf_fd_path, ovmf_vars_fd_path, temp_ovmf_vars_fd))
}

// Constants to replace magic numbers
const VM_SHUTDOWN_POLL_ATTEMPTS: u32 = 20;
const VM_SHUTDOWN_POLL_INTERVAL_MS: u64 = 1000;
const VM_POWER_OFF_POLL_ATTEMPTS: u32 = 200;
const VM_POWER_OFF_POLL_INTERVAL_S: u64 = 1;

fn run_virtualbox(args: &Args, workspace_root: &PathBuf) -> io::Result<()> {
    log::info!("Starting VirtualBox...");
    let (iso_path, _ovmf_fd_path, _ovmf_vars_fd_path, _dummy) =
        create_iso_and_setup(&workspace_root)?;

    ensure_vm_exists(&args.vm_name)?;
    power_off_vm(&args.vm_name)?;
    configure_vm_settings(&args.vm_name)?;
    configure_serial_port(&args.vm_name)?;
    attach_iso_and_start_vm(&args, &iso_path)?;

    Ok(())
}

struct VmSetting<'a> {
    args: &'a [&'a str],
    failure_msg: &'a str,
    success_msg: Option<&'a str>,
}

fn configure_vm_settings(vm_name: &str) -> io::Result<()> {
    log::info!("Configuring VM settings for '{}'...", vm_name);

    let settings = &[
        VmSetting {
            args: &["--memory", "4096"],
            failure_msg: "Failed to set VM memory.",
            success_msg: None,
        },
        VmSetting {
            args: &["--vram", "128"],
            failure_msg: "Failed to set VM video memory.",
            success_msg: None,
        },
        VmSetting {
            args: &["--acpi", "on"],
            failure_msg: "Failed to enable ACPI.",
            success_msg: None,
        },
        VmSetting {
            args: &["--nic1", "nat"],
            failure_msg: "Failed to configure network NAT.",
            success_msg: None,
        },
        VmSetting {
            args: &["--cpus", "1"],
            failure_msg: "Failed to set CPU count.",
            success_msg: None,
        },
        VmSetting {
            args: &["--chipset", "ich9"],
            failure_msg: "Failed to set chipset.",
            success_msg: None,
        },
        VmSetting {
            args: &["--firmware", "efi"],
            failure_msg: "Failed to set firmware to EFI.",
            success_msg: Some("Firmware set to EFI for UEFI boot."),
        },
        VmSetting {
            args: &["--hwvirtex", "off"],
            failure_msg: "Failed to disable hardware virtualization. This may cause issues in nested VM environments.",
            success_msg: Some("Hardware virtualization disabled for compatibility with nested VMs"),
        },
        VmSetting {
            args: &["--nested-paging", "off"],
            failure_msg: "Failed to disable nested paging.",
            success_msg: None,
        },
        VmSetting {
            args: &["--large-pages", "off"],
            failure_msg: "Failed to disable large pages.",
            success_msg: None,
        },
        VmSetting {
            args: &["--nested-hw-virt", "off"],
            failure_msg: "Failed to disable nested hardware virtualization.",
            success_msg: None,
        },
    ];

    for setting in settings {
        let mut command = Command::new("VBoxManage");
        command.arg("modifyvm").arg(vm_name).args(setting.args);

        let status = command.status()?;
        if !status.success() {
            log::warn!("{}", setting.failure_msg);
            return Err(io::Error::new(io::ErrorKind::Other, setting.failure_msg));
        }

        if let Some(msg) = setting.success_msg {
            log::info!("{}", msg);
        }
    }

    Ok(())
}

fn ensure_vm_exists(vm_name: &str) -> io::Result<()> {
    log::info!("Checking if VM '{}' exists...", vm_name);
    let check_vm_cmd = Command::new("VBoxManage")
        .args(["showvminfo", vm_name])
        .output();

    match check_vm_cmd {
        Ok(output) if output.status.success() => {
            log::info!("Found VM: {}", vm_name);
            Ok(())
        }
        _ => Err(io::Error::other(format!(
            "VirtualBox VM '{}' does not exist. Create it first with:\nVBoxManage createvm --name \"{}\" --ostype \"Other\" --register\nThen add storage controller and attach a hard disk.",
            vm_name, vm_name
        ))),
    }
}

fn power_off_vm(vm_name: &str) -> io::Result<()> {
    log::info!("Powering off VM if running...");

    // Check if VM is currently running
    let initial_state = get_vm_state(vm_name)?;

    // If VM is already powered off, do nothing
    if let Some("poweroff") = initial_state.as_deref() {
        log::info!("VM '{}' is already powered off.", vm_name);
        return Ok(());
    }

    // If VM is saved, discard the saved state
    if let Some("saved") = initial_state.as_deref() {
        log::info!(
            "VM '{}' is in saved state, discarding saved state...",
            vm_name
        );
        let discard_status = Command::new("VBoxManage")
            .args(["discardstate", vm_name])
            .status()?;
        if !discard_status.success() {
            return Err(io::Error::other(format!(
                "`VBoxManage discardstate` failed for saved VM state."
            )));
        }
        // After discarding, VM should be powered off
        log::info!("Saved state discarded.");
        return Ok(());
    }

    let mut vm_powered_off = false;

    // Try graceful shutdown first
    let acpi_result = Command::new("VBoxManage")
        .args(["controlvm", vm_name, "acpipowerbutton"])
        .output();

    if let Err(e) = &acpi_result {
        log::warn!(
            "Failed to send ACPI power button signal: {}. This may be ignored if the VM was not running.",
            e
        );
    } else if !acpi_result.as_ref().unwrap().status.success() {
        let stderr = String::from_utf8_lossy(&acpi_result.as_ref().unwrap().stderr);
        if stderr.contains("not currently running") {
            log::info!(
                "VM '{}' is not currently running, so it is already powered off.",
                vm_name
            );
            return Ok(());
        } else {
            log::warn!("ACPI power button signal failed: {}", stderr);
        }
    }

    // Poll VM state
    for _ in 0..VM_SHUTDOWN_POLL_ATTEMPTS {
        std::thread::sleep(std::time::Duration::from_millis(
            VM_SHUTDOWN_POLL_INTERVAL_MS,
        ));
        let state = get_vm_state(vm_name)?;

        if let Some("poweroff") = state.as_deref() {
            vm_powered_off = true;
            break;
        }
    }

    // If graceful shutdown didn't work, force power off
    if !vm_powered_off {
        let status = Command::new("VBoxManage")
            .args(["controlvm", vm_name, "poweroff"])
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "`VBoxManage poweroff` failed with status: {}",
                status
            )));
        }

        // Brief wait for force power off
        std::thread::sleep(std::time::Duration::from_millis(
            VM_SHUTDOWN_POLL_INTERVAL_MS,
        ));
    }

    Ok(())
}

fn configure_serial_port(vm_name: &str) -> io::Result<()> {
    log::info!("Configuring serial port for VM '{}'...", vm_name);

    // Configure serial port 1 (COM1) to redirect output to TCP server
    // Enable UART1 at COM1 (0x3f8) with 4 IRQs
    let uart_status = Command::new("VBoxManage")
        .args(["modifyvm", vm_name, "--uart1", "0x3f8", "4"])
        .status()?;

    if !uart_status.success() {
        log::warn!("Failed to configure UART1 port.");
        return Ok(());
    }

    // Set UART1 mode to tcp server for serial output
    let serial_status = Command::new("VBoxManage")
        .args(["modifyvm", vm_name, "--uartmode1", "tcpserver", "6000"])
        .status()?;

    if !serial_status.success() {
        log::warn!("Failed to configure serial port mode. Serial logging may not work.");
        // Don't return error, as this might not be fatal
    } else {
        log::info!("Serial output will be accessible on TCP port 6000");
    }

    Ok(())
}

fn attach_iso_and_start_vm(args: &Args, iso_path: &PathBuf) -> io::Result<()> {
    // Use default firmware (UEFI) for serial console output like QEMU

    // Attach ISO
    log::info!("Attaching ISO to VM...");
    let attach_status = Command::new("VBoxManage")
        .args([
            "storageattach",
            &args.vm_name,
            "--storagectl",
            &args.controller,
            "--port",
            "0",
            "--device",
            "0",
            "--type",
            "dvddrive",
            "--medium",
            &iso_path.to_string_lossy(),
        ])
        .status()?;

    if !attach_status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Failed to attach ISO to VirtualBox VM",
        ));
    }

    // Start VM in headless or GUI mode
    let start_type = if args.gui { "gui" } else { "headless" };
    log::info!("Starting VM in {} mode...", start_type);
    let status = Command::new("VBoxManage")
        .args(["startvm", &args.vm_name, "--type", start_type])
        .status()?;

    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to start VM in {} mode", start_type),
        ));
    }

    // Wait a moment for VM to start and serial output to begin
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Start nc to connect to serial TCP server and display output in realtime
    log::info!("Streaming serial output...");
    let nc_status = Command::new("nc").args(["localhost", "6000"]).status();

    match nc_status {
        Ok(status) if status.success() => {
            log::info!("Serial output streaming completed.");
        }
        _ => {
            log::info!("Warning: Failed to stream serial output, but VM may still be running.");
        }
    }

    // Wait for the VM to shut down by polling state
    log::info!("Waiting for VM to power off...");
    let mut consecutive_failures = 0;
    const MAX_CONSECUTIVE_FAILURES: u32 = 5;
    let mut is_powered_off = false;

    for _ in 0..VM_POWER_OFF_POLL_ATTEMPTS {
        std::thread::sleep(std::time::Duration::from_secs(VM_POWER_OFF_POLL_INTERVAL_S));
        match get_vm_state(&args.vm_name) {
            Ok(state) => {
                consecutive_failures = 0; // Reset on success
                if let Some("poweroff") = state.as_deref() {
                    log::info!("VM is confirmed to be powered off.");
                    is_powered_off = true;
                    break;
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                log::warn!(
                    "Failed to check VM state (attempt {}/{}), error: {}, continuing...",
                    consecutive_failures,
                    MAX_CONSECUTIVE_FAILURES,
                    e
                );
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    log::error!("Aborting wait: Failed to check VM state after multiple attempts.");
                    break;
                }
            }
        }
    }

    if !is_powered_off {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Timed out waiting for VM to power off. It might still be running.",
        ));
    }

    Ok(())
}

// Helper function to get the VM state
fn get_vm_state(vm_name: &str) -> io::Result<Option<String>> {
    let output = Command::new("VBoxManage")
        .args(["showvminfo", vm_name, "--machinereadable"])
        .output()?;

    if output.status.success() {
        let stdout = std::str::from_utf8(&output.stdout)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let state = stdout
            .lines()
            .find(|line| line.starts_with("VMState="))
            .and_then(|line| line.strip_prefix("VMState=\""))
            .and_then(|s| s.strip_suffix('"'))
            .map(|s| s.to_string());
        Ok(state)
    } else {
        Ok(None)
    }
}

fn run_qemu(workspace_root: &PathBuf) -> io::Result<()> {
    log::info!("Starting QEMU...");
    let (iso_path, ovmf_fd_path, ovmf_vars_fd_path, temp_ovmf_vars_fd) =
        create_iso_and_setup(&workspace_root)?;

    // --- 4. Run QEMU with the created ISO ---

    let ovmf_fd_drive = format!(
        "if=pflash,format=raw,unit=0,readonly=on,file={}",
        ovmf_fd_path.display()
    );
    let ovmf_vars_fd_drive = format!(
        "if=pflash,format=raw,unit=1,file={}",
        ovmf_vars_fd_path.display()
    );

    let iso_path_str = iso_path.to_str().expect("ISO path should be valid UTF-8");

    let mut qemu_cmd = Command::new("qemu-system-x86_64");
    qemu_cmd.args([
        "-m",
        "4G",
        "-cpu",
        "qemu64,+smap,-invtsc",
        "-smp",
        "1",
        "-M",
        "q35",
        "-vga",
        "cirrus",
        "-display",
        "gtk,gl=off,window-close=on,zoom-to-fit=on",
        "-serial",
        "stdio",
        "-accel",
        "tcg,thread=single",
        "-d",
        "guest_errors,unimp",
        "-D",
        "qemu_log.txt",
        "-monitor",
        "none",
        "-drive",
        &ovmf_fd_drive,
        "-drive",
        &ovmf_vars_fd_drive,
        "-drive",
        &format!("file={},media=cdrom,if=ide,format=raw", iso_path_str),
        "-no-reboot",
        "-no-shutdown",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-rtc",
        "base=utc",
        "-boot",
        "menu=on,order=d",
        "-nodefaults",
    ]);
    // Keep the temporary file alive until QEMU exits
    let _temp_ovmf_vars_fd_holder = temp_ovmf_vars_fd;
    // LD_PRELOAD is a workaround for specific QEMU/libpthread versions.
    // It can be overridden by setting the FULLERENE_QEMU_LD_PRELOAD environment variable.
    let ld_preload_path = env::var("FULLERENE_QEMU_LD_PRELOAD").unwrap_or_else(|_| {
        flasks::find_libpthread().expect("libpthread.so.0 not found in common locations")
    });
    qemu_cmd.env("LD_PRELOAD", ld_preload_path);
    let qemu_status = qemu_cmd.status()?;

    if !qemu_status.success() {
        return Err(io::Error::other("QEMU execution failed"));
    }

    Ok(())
}
