use std::{fs, io, path::Path};

pub const GENERATED_RULES: &str = include_str!(env!("FULLERENE_UDEV_RULES"));

/// Write the Rust-generated udev artifact to a deployment or packaging path.
pub fn write_rules(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, GENERATED_RULES)
}

pub fn print_rules() {
    print!("{GENERATED_RULES}");
}

#[cfg(test)]
mod tests {
    use super::GENERATED_RULES;

    #[test]
    fn generated_rules_cover_both_bramble_identities() {
        assert!(GENERATED_RULES.contains("ATTR{idVendor}==\"1234\""));
        assert!(GENERATED_RULES.contains("ATTR{idProduct}==\"0001\""));
        assert!(GENERATED_RULES.contains("ATTR{idVendor}==\"18d1\""));
        assert!(GENERATED_RULES.contains("ATTR{idProduct}==\"4ee7\""));
        assert!(GENERATED_RULES.ends_with('\n'));
    }
}
