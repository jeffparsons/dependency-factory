use std::path::Path;

use anyhow::Result;
use semver::Version;

use crate::changelog;
use crate::manifest;
use crate::release::{Paths, capture};

/// Pre-flight checks before tagging.
///
/// Runs every check independently, accumulating issues so the user gets the
/// full list in a single pass instead of having to re-run after fixing each
/// problem. Exits non-zero if any issue was found.
pub fn run(root: &Path) -> Result<()> {
    let paths = Paths::new(root);
    let mut issues: Vec<String> = Vec::new();

    let manifest_version = manifest::read_workspace_version(&paths.root_manifest())?;

    // Tree clean.
    let porcelain = capture(root, "git", &["status", "--porcelain"])?;
    if !porcelain.trim().is_empty() {
        issues.push(format!(
            "working tree is dirty:\n{}",
            indent(porcelain.trim())
        ));
    }

    // Branch is `main` (pre-1.0) or `release/x.y` (post-1.0).
    let branch = capture(root, "git", &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if !is_permitted_branch(&branch, &manifest_version) {
        issues.push(format!(
            "on branch `{branch}`; release branches are `main` (pre-1.0) or `release/x.y` (post-1.0)"
        ));
    }

    // CI is green for HEAD.
    match latest_ci_conclusion(root, &branch) {
        Ok(Some(ref conclusion)) if conclusion == "success" => {}
        Ok(Some(other)) => issues.push(format!("latest CI run on `{branch}` was `{other}`")),
        Ok(None) => issues.push(format!("no CI runs found on branch `{branch}`")),
        Err(e) => issues.push(format!("could not query CI status: {e}")),
    }

    // CHANGELOG and manifest agree on the latest released version.
    let changelog_content = std::fs::read_to_string(paths.changelog())?;
    match changelog::latest_released_version(&changelog_content)? {
        Some(ref ch) if ch == &manifest_version => {}
        Some(ref ch) => issues.push(format!(
            "manifest version is {manifest_version} but the latest CHANGELOG section is [{ch}] — \
             did you forget to rename `[Unreleased]`?"
        )),
        None => issues.push(format!(
            "manifest version is {manifest_version} but CHANGELOG has no released sections — \
             did you run `cargo xtask release prepare`?"
        )),
    }

    // Report.
    if issues.is_empty() {
        println!("Ready to tag v{manifest_version}.");
        Ok(())
    } else {
        eprintln!("Not ready to release. {} issue(s):", issues.len());
        for (n, issue) in issues.iter().enumerate() {
            eprintln!("  {}. {issue}", n + 1);
        }
        std::process::exit(1);
    }
}

fn is_permitted_branch(branch: &str, version: &Version) -> bool {
    if branch == "main" {
        return true;
    }
    if version.major == 0 {
        return false;
    }
    let Some(rest) = branch.strip_prefix("release/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.len() != 2 {
        return false;
    }
    let Ok(major): Result<u64, _> = parts[0].parse() else {
        return false;
    };
    let Ok(minor): Result<u64, _> = parts[1].parse() else {
        return false;
    };
    major == version.major && minor == version.minor
}

fn latest_ci_conclusion(root: &Path, branch: &str) -> Result<Option<String>> {
    let out = capture(
        root,
        "gh",
        &[
            "run",
            "list",
            "--workflow",
            "ci.yml",
            "--branch",
            branch,
            "--limit",
            "1",
            "--json",
            "conclusion",
            "-q",
            ".[0].conclusion",
        ],
    )?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("       {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn main_is_always_permitted() {
        assert!(is_permitted_branch("main", &v("0.1.0")));
        assert!(is_permitted_branch("main", &v("1.4.2")));
    }

    #[test]
    fn release_branch_only_permitted_post_1_for_matching_minor() {
        // pre-1.0: release branches not allowed.
        assert!(!is_permitted_branch("release/0.1", &v("0.1.5")));
        // post-1.0: only matching minor.
        assert!(is_permitted_branch("release/1.4", &v("1.4.2")));
        assert!(!is_permitted_branch("release/1.3", &v("1.4.2")));
        assert!(!is_permitted_branch("release/2.0", &v("1.4.2")));
    }

    #[test]
    fn other_branches_rejected() {
        assert!(!is_permitted_branch("feature/foo", &v("0.1.0")));
        assert!(!is_permitted_branch("release/garbage", &v("1.0.0")));
        assert!(!is_permitted_branch("release/1", &v("1.0.0")));
    }
}
