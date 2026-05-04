use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

mod changelog;
mod manifest;
mod release;

#[derive(Parser)]
#[command(version, about = "Repo-specific dev tooling for dependency-factory.")]
struct Cli {
    #[command(subcommand)]
    cmd: TopCmd,
}

#[derive(Subcommand)]
enum TopCmd {
    /// Release management.
    Release {
        #[command(subcommand)]
        cmd: release::ReleaseCmd,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let root = workspace_root();
    match cli.cmd {
        TopCmd::Release { cmd } => release::run(cmd, &root),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest is one level below workspace root")
        .to_path_buf()
}
