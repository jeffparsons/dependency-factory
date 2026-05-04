//! `Cargo.toml` mutations and queries, formatting-preserving via `toml_edit`.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use semver::Version;
use toml_edit::{DocumentMut, value};

/// The lib's path-dep version pin. Bumped in lockstep with the workspace
/// version so `dependency-factory X.Y.Z` always resolves
/// `dependency-factory-derive` at exactly the same version.
const LIB_PATHDEP_KEY: &str = "dependency-factory-derive";

/// Read `[workspace.package].version` from the root `Cargo.toml`.
pub fn read_workspace_version(root_manifest: &Path) -> Result<Version> {
    let raw = std::fs::read_to_string(root_manifest)
        .with_context(|| format!("reading {}", root_manifest.display()))?;
    let doc: DocumentMut = raw.parse().context("parsing root Cargo.toml")?;
    let v = doc
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("[workspace.package].version is missing or not a string"))?;
    Version::parse(v).with_context(|| format!("[workspace.package].version `{v}` is not semver"))
}

/// Set `[workspace.package].version` in the root `Cargo.toml`.
pub fn set_workspace_version(root_manifest: &Path, new: &Version) -> Result<()> {
    let raw = std::fs::read_to_string(root_manifest)
        .with_context(|| format!("reading {}", root_manifest.display()))?;
    let mut doc: DocumentMut = raw.parse().context("parsing root Cargo.toml")?;
    let pkg = doc
        .get_mut("workspace")
        .and_then(|w| w.get_mut("package"))
        .ok_or_else(|| anyhow!("[workspace.package] is missing from root Cargo.toml"))?;
    pkg["version"] = value(new.to_string());
    std::fs::write(root_manifest, doc.to_string())
        .with_context(|| format!("writing {}", root_manifest.display()))?;
    Ok(())
}

/// Set the lib's `dependency-factory-derive` path-dep version to `=X.Y.Z`.
///
/// This is the second half of a release version bump: the workspace
/// version drives the published versions, but the lib's manifest also pins
/// the derive crate to the exact same version with `=`, so the published
/// `dependency-factory X.Y.Z` always pulls `dependency-factory-derive X.Y.Z`.
pub fn set_lib_pathdep_version(lib_manifest: &Path, new: &Version) -> Result<()> {
    let raw = std::fs::read_to_string(lib_manifest)
        .with_context(|| format!("reading {}", lib_manifest.display()))?;
    let mut doc: DocumentMut = raw.parse().context("parsing lib Cargo.toml")?;
    let dep = doc
        .get_mut("dependencies")
        .and_then(|t| t.get_mut(LIB_PATHDEP_KEY))
        .ok_or_else(|| anyhow!("lib Cargo.toml has no `[dependencies].{LIB_PATHDEP_KEY}` entry"))?;
    let table = dep
        .as_inline_table_mut()
        .ok_or_else(|| anyhow!("`{LIB_PATHDEP_KEY}` dep is not an inline table"))?;
    table.insert("version", format!("={new}").into());
    std::fs::write(lib_manifest, doc.to_string())
        .with_context(|| format!("writing {}", lib_manifest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .prefix(name)
            .suffix(".toml")
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    const ROOT: &str = "\
[workspace]
resolver = \"3\"
members = [\"a\", \"b\"]

[workspace.package]
# Inline comment.
version = \"0.1.0\"
edition = \"2024\"
";

    const LIB: &str = "\
[package]
name = \"dependency-factory\"
version.workspace = true

[dependencies]
dependency-factory-derive = { version = \"=0.1.0\", path = \"../dependency-factory-derive\", optional = true }
other-dep = \"1\"
";

    #[test]
    fn read_workspace_version_returns_parsed_version() {
        let f = write_temp("root", ROOT);
        let v = read_workspace_version(f.path()).unwrap();
        assert_eq!(v, Version::parse("0.1.0").unwrap());
    }

    #[test]
    fn set_workspace_version_preserves_comments_and_other_fields() {
        let f = write_temp("root", ROOT);
        set_workspace_version(f.path(), &Version::parse("0.2.0").unwrap()).unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert!(after.contains("# Inline comment."));
        assert!(after.contains("version = \"0.2.0\""));
        assert!(after.contains("edition = \"2024\""));
        assert!(after.contains("members = [\"a\", \"b\"]"));
    }

    #[test]
    fn set_lib_pathdep_version_pins_with_equals() {
        let f = write_temp("lib", LIB);
        set_lib_pathdep_version(f.path(), &Version::parse("0.2.0").unwrap()).unwrap();
        let after = std::fs::read_to_string(f.path()).unwrap();
        assert!(after.contains("version = \"=0.2.0\""));
        // The path and optional fields are preserved alongside the bumped version.
        assert!(after.contains("path = \"../dependency-factory-derive\""));
        assert!(after.contains("optional = true"));
        // Other deps are untouched.
        assert!(after.contains("other-dep = \"1\""));
    }
}
