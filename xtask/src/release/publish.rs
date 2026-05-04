use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use semver::Version;

use crate::manifest;
use crate::release::{Paths, capture, run_cmd};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const POLL_TIMEOUT: Duration = Duration::from_secs(120);

/// Publish the proc-macro then the lib, in order, waiting for crates.io to
/// index the proc-macro between the two.
pub fn run(root: &Path) -> Result<()> {
    let paths = Paths::new(root);
    let version = manifest::read_workspace_version(&paths.root_manifest())?;

    println!("Publishing dependency-factory-derive {version}...");
    run_cmd(
        root,
        "cargo",
        &["publish", "-p", "dependency-factory-derive", "--locked"],
    )?;

    println!("Waiting for crates.io to index dependency-factory-derive {version}...");
    wait_for_indexed(root, "dependency-factory-derive", &version)?;

    println!("Publishing dependency-factory {version}...");
    run_cmd(
        root,
        "cargo",
        &["publish", "-p", "dependency-factory", "--locked"],
    )?;

    println!("Both crates published at {version}.");
    Ok(())
}

/// Poll crates.io until `<crate_name> <version>` shows up in the API, or we
/// hit `POLL_TIMEOUT`.
fn wait_for_indexed(root: &Path, crate_name: &str, version: &Version) -> Result<()> {
    let url = format!("https://crates.io/api/v1/crates/{crate_name}/versions");
    let needle = format!("\"num\":\"{version}\"");
    let deadline = Instant::now() + POLL_TIMEOUT;

    loop {
        let probe = capture(
            root,
            "curl",
            &["-fsSL", "-A", "dependency-factory-xtask/release", &url],
        );
        match probe {
            Ok(body) if body.contains(&needle) => return Ok(()),
            Ok(_) | Err(_) => {
                // Either not yet indexed, or transient curl failure. Both
                // are retryable; we'll loop until the deadline.
            }
        }
        if Instant::now() > deadline {
            bail!(
                "timed out after {:?} waiting for crates.io to index {crate_name} {version}",
                POLL_TIMEOUT
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
