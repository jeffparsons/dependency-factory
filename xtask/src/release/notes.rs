use std::io::Write;
use std::path::Path;

use anyhow::Result;
use semver::Version;

use crate::changelog;
use crate::release::Paths;

/// Print the CHANGELOG body for `version` on stdout.
///
/// Used by the release workflow to populate the GitHub Release body.
pub fn run(version: &Version, root: &Path) -> Result<()> {
    let paths = Paths::new(root);
    let content = std::fs::read_to_string(paths.changelog())?;
    let body = changelog::extract_section(&content, version)?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(body.as_bytes())?;
    Ok(())
}
