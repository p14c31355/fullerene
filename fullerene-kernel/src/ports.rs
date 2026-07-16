//! Catalog and launcher for large third-party/application ports.
//!
//! Fullerene does not redistribute third-party binaries. A port is installed
//! from a user-provided ELF image after its runtime contract is selected.

use alloc::string::String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortRuntime {
    Native,
    Linux,
}

impl PortRuntime {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Linux => "linux",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortSpec {
    pub name: &'static str,
    pub runtime: PortRuntime,
    pub description: &'static str,
    pub purpose: &'static str,
}

pub const CATALOG: &[PortSpec] = &[
    PortSpec {
        name: "freedoom",
        runtime: PortRuntime::Linux,
        description: "FREEDOOM engine and game data",
        purpose: "game",
    },
    PortSpec {
        name: "fullerene-present",
        runtime: PortRuntime::Native,
        description: "Self-hosted presentation application",
        purpose: "presentation",
    },
    PortSpec {
        name: "netsurf",
        runtime: PortRuntime::Linux,
        description: "NetSurf web browser",
        purpose: "browser",
    },
    PortSpec {
        name: "plasma-session",
        runtime: PortRuntime::Linux,
        description: "KDE Plasma desktop session",
        purpose: "desktop-environment",
    },
    PortSpec {
        name: "xfce-session",
        runtime: PortRuntime::Linux,
        description: "Xfce desktop session",
        purpose: "desktop-environment",
    },
    PortSpec {
        name: "vscodium",
        runtime: PortRuntime::Linux,
        description: "VSCodium self-hosted coding environment",
        purpose: "editor",
    },
    PortSpec {
        name: "cargo",
        runtime: PortRuntime::Linux,
        description: "Cargo build driver for self-hosted builds",
        purpose: "build-tool",
    },
    PortSpec {
        name: "rustc",
        runtime: PortRuntime::Linux,
        description: "Rust compiler for self-hosted builds",
        purpose: "build-tool",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortError {
    UnknownPort,
    FileSystem(genome::FsError),
    InvalidExecutable,
    LaunchFailed,
}

impl From<genome::FsError> for PortError {
    fn from(error: genome::FsError) -> Self {
        Self::FileSystem(error)
    }
}

pub fn find(name: &str) -> Option<&'static PortSpec> {
    CATALOG.iter().find(|port| port.name == name)
}

pub fn install(name: &str, source_path: &str) -> Result<(), PortError> {
    let spec = find(name).ok_or(PortError::UnknownPort)?;
    let image = crate::fs::read_entire_file(source_path)?;
    validate_elf(&image)?;
    crate::fs::install_package_with_runtime(
        spec.name,
        "1.0.0",
        spec.description,
        spec.runtime.as_str(),
        &image,
    )?;
    Ok(())
}

pub fn launch(name: &str) -> Result<u64, PortError> {
    let package = crate::fs::list_packages()?
        .into_iter()
        .find(|package| package.name == name)
        .ok_or(PortError::FileSystem(genome::FsError::FileNotFound))?;
    let path = alloc::format!("/packages/{}/{}", package.name, package.binary);
    match package.runtime.as_str() {
        "native" => {
            let image = crate::fs::read_entire_file(&path)?;
            validate_elf(&image)?;
            let label: &'static str = alloc::boxed::Box::leak(package.name.into_boxed_str());
            crate::loader::load_program(&image, label)
                .map(|pid| pid.0)
                .map_err(|_| PortError::LaunchFailed)
        }
        "linux" => crate::linux::launch::launch_linux_binary(&path)
            .map(|pid| pid.0)
            .map_err(|_| PortError::LaunchFailed),
        _ => Err(PortError::LaunchFailed),
    }
}

pub fn catalog_text() -> String {
    use core::fmt::Write;
    let mut output = String::from("NAME                RUNTIME  PURPOSE\n");
    output.push_str("------------------  -------  -------------------\n");
    for port in CATALOG {
        let _ = writeln!(
            output,
            "{:<18}  {:<7}  {}",
            port.name,
            port.runtime.as_str(),
            port.purpose
        );
    }
    output
}

fn validate_elf(image: &[u8]) -> Result<(), PortError> {
    const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
    if image.len() < 64 || image.get(..4) != Some(ELF_MAGIC) || image.get(4) != Some(&2) {
        return Err(PortError::InvalidExecutable);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stretch_catalog_has_unique_names_and_required_ports() {
        for (index, port) in CATALOG.iter().enumerate() {
            assert!(CATALOG[..index].iter().all(|other| other.name != port.name));
        }
        for required in [
            "freedoom",
            "fullerene-present",
            "netsurf",
            "plasma-session",
            "vscodium",
            "cargo",
            "rustc",
        ] {
            assert!(find(required).is_some(), "missing port {required}");
        }
    }

    #[test]
    fn rejects_non_elf_port_images() {
        assert_eq!(
            validate_elf(b"not an elf"),
            Err(PortError::InvalidExecutable)
        );
    }
}
