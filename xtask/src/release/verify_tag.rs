use std::path::Path;

use anyhow::{Context, Result, bail};
use semver::Version;

use crate::manifest;
use crate::release::Paths;

/// Compare `$GITHUB_REF_NAME` (with leading `v` stripped) to the workspace
/// version. Used as the first step of the release workflow to catch a tag
/// that was pushed against the wrong commit, or against a manifest that
/// hasn't been bumped yet.
pub fn run(root: &Path) -> Result<()> {
    let paths = Paths::new(root);

    let ref_name = std::env::var("GITHUB_REF_NAME")
        .context("GITHUB_REF_NAME is not set; this command is intended for CI")?;
    let tag_version = ref_name
        .strip_prefix('v')
        .ok_or_else(|| anyhow::anyhow!("tag `{ref_name}` does not start with `v`"))?;
    let tag_version = Version::parse(tag_version)
        .with_context(|| format!("tag `{ref_name}` is not `v<semver>`"))?;

    let manifest_version = manifest::read_workspace_version(&paths.root_manifest())?;

    if tag_version != manifest_version {
        bail!(
            "tag `{ref_name}` does not match workspace version `{manifest_version}` — \
             refusing to publish"
        );
    }

    println!("Tag `{ref_name}` matches workspace version `{manifest_version}`.");
    Ok(())
}
