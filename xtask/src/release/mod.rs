//! `cargo xtask release ...` subcommands.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use semver::Version;

mod check;
mod notes;
mod prepare;
mod publish;
mod verify_tag;

#[derive(Subcommand)]
pub enum ReleaseCmd {
    /// Bump versions, update CHANGELOG, refresh Cargo.lock, build & test.
    /// Leaves the working tree dirty so the user can review and commit.
    Prepare {
        /// New version, e.g. `0.1.1`.
        version: Version,
    },
    /// Pre-flight checks before tagging: clean tree, on a permitted branch,
    /// CI green for HEAD, CHANGELOG and manifest agree.
    Check,
    /// Verify the pushed tag matches the workspace version. Used by CI.
    VerifyTag,
    /// Publish `dependency-factory-derive` then `dependency-factory` to
    /// crates.io, in order, waiting for the registry to index the
    /// proc-macro between publishes. Used by CI.
    Publish,
    /// Print the CHANGELOG body for the given version on stdout. Used by
    /// CI to populate the GitHub Release body.
    Notes {
        /// Released version, e.g. `0.1.0`.
        version: Version,
    },
}

pub fn run(cmd: ReleaseCmd, root: &Path) -> Result<()> {
    match cmd {
        ReleaseCmd::Prepare { version } => prepare::run(&version, root),
        ReleaseCmd::Check => check::run(root),
        ReleaseCmd::VerifyTag => verify_tag::run(root),
        ReleaseCmd::Publish => publish::run(root),
        ReleaseCmd::Notes { version } => notes::run(&version, root),
    }
}

/// Paths that subcommands consult repeatedly. Computed once relative to the
/// workspace root so subcommands stay short.
pub(crate) struct Paths<'a> {
    pub root: &'a Path,
}

impl<'a> Paths<'a> {
    pub fn new(root: &'a Path) -> Self {
        Self { root }
    }

    pub fn root_manifest(&self) -> std::path::PathBuf {
        self.root.join("Cargo.toml")
    }

    pub fn lib_manifest(&self) -> std::path::PathBuf {
        self.root.join("dependency-factory").join("Cargo.toml")
    }

    pub fn changelog(&self) -> std::path::PathBuf {
        self.root.join("CHANGELOG.md")
    }
}

/// Run a command in the workspace root, streaming its output to the terminal.
/// Errors out if the command fails or the program isn't found.
pub(crate) fn run_cmd(root: &Path, prog: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(prog)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("spawning `{prog} {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`{prog} {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

/// Run a command and capture its stdout. Stderr is forwarded to the terminal.
pub(crate) fn capture(root: &Path, prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog)
        .args(args)
        .current_dir(root)
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("spawning `{prog} {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!("`{prog} {}` exited with {}", args.join(" "), out.status);
    }
    String::from_utf8(out.stdout).context("command stdout is not UTF-8")
}
