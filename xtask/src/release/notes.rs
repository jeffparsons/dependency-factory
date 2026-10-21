use std::io::Write;
use std::path::Path;

use anyhow::Result;
use semver::Version;

use crate::changelog;
use crate::release::Paths;

/// Print the CHANGELOG body for `version` on stdout, or a placeholder for
/// releases up to 0.1.0, which have no notes.
///
/// Used by the release workflow to populate the GitHub Release body.
pub fn run(version: &Version, root: &Path) -> Result<()> {
    let body = if changelog::has_section(version) {
        let paths = Paths::new(root);
        let content = std::fs::read_to_string(paths.changelog())?;
        changelog::extract_section(&content, version)?
    } else {
        "No release notes: the changelog starts after 0.1.0.\n".to_string()
    };
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(body.as_bytes())?;
    Ok(())
}
