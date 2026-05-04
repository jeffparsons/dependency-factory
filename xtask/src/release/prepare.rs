use std::path::Path;

use anyhow::{Result, bail};
use semver::Version;

use crate::changelog;
use crate::manifest;
use crate::release::{Paths, capture, run_cmd};

pub fn run(version: &Version, root: &Path) -> Result<()> {
    let paths = Paths::new(root);

    // 1. Sanity-check the requested version.
    let current = manifest::read_workspace_version(&paths.root_manifest())?;
    if version <= &current {
        bail!("new version {version} must be strictly greater than current {current}");
    }

    // 2. Bump the workspace version and the lib's path-dep pin.
    println!("Bumping workspace version to {version}...");
    manifest::set_workspace_version(&paths.root_manifest(), version)?;
    manifest::set_lib_pathdep_version(&paths.lib_manifest(), version)?;

    // 3. Rename the [Unreleased] section to the new version with today's date.
    println!("Renaming CHANGELOG.md [Unreleased] to [{version}]...");
    let date = today_utc(root)?;
    let content = std::fs::read_to_string(paths.changelog())?;
    let new_content = changelog::rename_unreleased(&content, version, &date)?;
    std::fs::write(paths.changelog(), new_content)?;

    // 4. Refresh the lockfile, then build and test.
    println!("Refreshing Cargo.lock and re-building...");
    run_cmd(root, "cargo", &["update", "-w"])?;
    run_cmd(root, "cargo", &["build", "--workspace", "--locked"])?;
    run_cmd(root, "cargo", &["test", "--workspace", "--locked"])?;

    // 5. Pre-flight publish checks. The lib can't run a real `--dry-run`
    //    pre-publish (its `=` pin on the derive crate can only be satisfied
    //    after the derive is actually on crates.io); `cargo package --list`
    //    is the closest thing that works without contacting the registry.
    println!("Running pre-flight publish checks...");
    run_cmd(
        root,
        "cargo",
        &[
            "publish",
            "-p",
            "dependency-factory-derive",
            "--dry-run",
            "--allow-dirty",
            "--locked",
        ],
    )?;
    run_cmd(
        root,
        "cargo",
        &[
            "package",
            "--list",
            "-p",
            "dependency-factory",
            "--allow-dirty",
        ],
    )?;

    // 6. Hand back to the user.
    println!();
    println!("Prepared release v{version}. Working tree is intentionally dirty.");
    println!("Suggested next steps:");
    println!("  git diff   # review the bump");
    println!(
        "  git add Cargo.toml Cargo.lock dependency-factory/Cargo.toml CHANGELOG.md \\\n    && git commit -m 'Release v{version}'"
    );
    println!("  git push                              # CI runs");
    println!("  gh run watch                          # wait until green");
    println!("  git tag -a v{version} -m 'Release v{version}'");
    println!("  git push origin v{version}             # release workflow runs");
    Ok(())
}

/// Today's UTC date in `YYYY-MM-DD`. Shells out rather than pulling a
/// date-handling crate into the dep tree.
fn today_utc(root: &Path) -> Result<String> {
    let s = capture(root, "date", &["-u", "+%Y-%m-%d"])?;
    Ok(s.trim().to_string())
}
