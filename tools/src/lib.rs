use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

pub type ToolResult<T = ()> = Result<T, Box<dyn Error>>;

#[derive(Debug, Deserialize)]
struct DependencyPolicy {
    allow: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct SupportMatrix {
    title: String,
    section: Vec<SupportSection>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct SupportSection {
    title: String,
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

pub fn workspace_root() -> PathBuf {
    // Try CARGO_WORKSPACE_DIR first (Cargo sets this at runtime).
    if let Some(dir) = env::var_os("CARGO_WORKSPACE_DIR") {
        return PathBuf::from(dir);
    }
    // Fall back to parent of CARGO_MANIFEST_DIR (compile-time).
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut candidate = Some(manifest);
    while let Some(dir) = candidate {
        if dir.join("Cargo.toml").exists() {
            if let Ok(content) = fs::read_to_string(dir.join("Cargo.toml")) {
                if content.contains("[workspace]") {
                    return dir.to_path_buf();
                }
            }
        }
        candidate = dir.parent();
    }
    panic!(
        "tools crate cannot find the workspace root from {}",
        manifest.display()
    );
}

pub fn check_dependency_duplicates(root: &Path) -> ToolResult {
    let policy_path = root.join("dependency-duplicates.toml");
    let policy: DependencyPolicy =
        toml::from_str(&fs::read_to_string(&policy_path)?).map_err(|error| {
            format!(
                "failed to parse dependency policy {}: {error}",
                policy_path.display()
            )
        })?;

    let metadata = load_cargo_metadata(root)?;
    let versions = collect_versions(&metadata);
    let duplicates = duplicate_versions(&versions);
    let failures = validate_duplicate_policy(&duplicates, &policy.allow);

    if !failures.is_empty() {
        eprintln!("Duplicate dependency policy failed:");
        for failure in failures {
            eprintln!("  - {failure}");
        }
        return Err("unreviewed dependency duplicates found".into());
    }

    let approved = duplicates
        .iter()
        .map(|(name, versions)| format!("{name}=[{}]", join_versions(versions)))
        .collect::<Vec<_>>()
        .join(", ");
    println!("Duplicate dependency policy satisfied: {approved}");
    Ok(())
}

fn load_cargo_metadata(root: &Path) -> ToolResult<CargoMetadata> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cargo metadata failed:\n{stderr}").into());
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("failed to parse cargo metadata: {error}").into())
}

fn collect_versions(metadata: &CargoMetadata) -> BTreeMap<String, BTreeSet<String>> {
    let mut versions = BTreeMap::<String, BTreeSet<String>>::new();
    for package in &metadata.packages {
        versions
            .entry(package.name.clone())
            .or_default()
            .insert(package.version.clone());
    }
    versions
}

fn duplicate_versions(
    versions: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    versions
        .iter()
        .filter(|(_, versions)| versions.len() > 1)
        .map(|(name, versions)| (name.clone(), versions.clone()))
        .collect()
}

fn validate_duplicate_policy(
    duplicates: &BTreeMap<String, BTreeSet<String>>,
    allowed: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut failures = Vec::new();

    for (name, found) in duplicates {
        let expected = allowed
            .get(name)
            .map(|versions| versions.iter().cloned().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        if *found != expected {
            let allowed_text = if expected.is_empty() {
                String::from("(none)")
            } else {
                join_versions(&expected)
            };
            failures.push(format!(
                "{name}: found {}; allowed {allowed_text}",
                join_versions(found)
            ));
        }
    }

    let stale = allowed
        .keys()
        .filter(|name| !duplicates.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    if !stale.is_empty() {
        failures.push(format!(
            "stale allow-list entries (remove after dependency convergence): {}",
            stale.join(", ")
        ));
    }

    failures
}

fn join_versions(versions: &BTreeSet<String>) -> String {
    versions.iter().cloned().collect::<Vec<_>>().join(", ")
}

pub fn generate_support_matrix(root: &Path, check: bool) -> ToolResult {
    let source_path = root.join("support").join("matrix.toml");
    let output_path = root.join("docs").join("SUPPORT_MATRIX.md");
    let matrix: SupportMatrix =
        toml::from_str(&fs::read_to_string(&source_path)?).map_err(|error| {
            format!(
                "failed to parse support matrix {}: {error}",
                source_path.display()
            )
        })?;
    let generated = render_support_matrix(&matrix)?;

    if check {
        let current = fs::read_to_string(&output_path)?;
        if current != generated {
            return Err(format!(
                "{} is stale; run `cargo run -p fullerene-tools --bin \
                 generate-support-matrix`",
                output_path.display()
            )
            .into());
        }
    } else {
        fs::write(&output_path, generated)?;
    }

    Ok(())
}

pub fn render_support_matrix(matrix: &SupportMatrix) -> ToolResult<String> {
    let mut lines = vec![
        format!("# {}", matrix.title),
        String::new(),
        String::from(
            "<!-- Generated by cargo run -p fullerene-tools --bin \
             generate-support-matrix; edit support/matrix.toml. -->",
        ),
        String::new(),
    ];

    for section in &matrix.section {
        if section
            .rows
            .iter()
            .any(|row| row.len() != section.columns.len())
        {
            return Err(format!("{}: row width does not match columns", section.title).into());
        }

        lines.push(format!("## {}", section.title));
        lines.push(String::new());
        lines.push(format!("| {} |", section.columns.join(" | ")));
        lines.push(format!(
            "|{}|",
            std::iter::repeat_n("---", section.columns.len())
                .collect::<Vec<_>>()
                .join("|")
        ));
        for row in &section.rows {
            lines.push(format!(
                "| {} |",
                row.iter()
                    .map(|cell| render_cell(cell))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        lines.push(String::new());
    }

    lines.push(String::from(
        "Status values are generated from the same TOML data checked by CI.",
    ));
    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn render_cell(value: &str) -> String {
    let rendered = match value {
        "Full" => "✅ Full",
        "Partial" => "🟡 Partial",
        "Stub" => "🧩 Stub",
        "NotSupported" => "❌ Not supported",
        value => value,
    };
    rendered.replace('|', r"\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| String::from(*value)).collect()
    }

    #[test]
    fn duplicate_policy_rejects_unapproved_and_stale_entries() {
        let duplicates = BTreeMap::from([
            (String::from("approved"), versions(&["1", "2"])),
            (String::from("new"), versions(&["3", "4"])),
        ]);
        let allowed = BTreeMap::from([
            (
                String::from("approved"),
                vec![String::from("2"), String::from("1")],
            ),
            (
                String::from("stale"),
                vec![String::from("5"), String::from("6")],
            ),
        ]);

        let failures = validate_duplicate_policy(&duplicates, &allowed);
        assert_eq!(failures.len(), 2);
        assert!(failures[0].starts_with("new: found 3, 4; allowed"));
        assert!(failures[1].contains("stale allow-list entries"));
    }

    #[test]
    fn support_matrix_rendering_maps_status_and_escapes_pipes() {
        let matrix = SupportMatrix {
            title: String::from("Matrix"),
            section: vec![SupportSection {
                title: String::from("Section"),
                columns: vec![String::from("Feature"), String::from("Status")],
                rows: vec![vec![String::from("A|B"), String::from("Full")]],
            }],
        };

        let rendered = render_support_matrix(&matrix).unwrap();
        assert!(rendered.contains("| A\\|B | ✅ Full |"));
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn support_matrix_rejects_rows_with_the_wrong_width() {
        let matrix = SupportMatrix {
            title: String::from("Matrix"),
            section: vec![SupportSection {
                title: String::from("Broken"),
                columns: vec![String::from("A"), String::from("B")],
                rows: vec![vec![String::from("only one")]],
            }],
        };

        assert!(render_support_matrix(&matrix).is_err());
    }
}
