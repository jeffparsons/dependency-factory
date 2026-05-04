//! `CHANGELOG.md` mutations and queries.
//!
//! The format is [Keep a Changelog](https://keepachangelog.com), which we treat
//! as a small fixed grammar: section headings are exactly `## [<TAG>]` (the
//! [Unreleased] section) or `## [<VERSION>] - <DATE>` (a released section). The
//! body of a section is everything between two consecutive section headings.

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;

const UNRELEASED_HEADING: &str = "## [Unreleased]";

/// Rename the `## [Unreleased]` section to a versioned heading and insert a
/// fresh empty `## [Unreleased]` above it. Returns the new file content.
pub fn rename_unreleased(content: &str, version: &Version, date: &str) -> Result<String> {
    let unreleased_idx = content
        .find(UNRELEASED_HEADING)
        .ok_or_else(|| anyhow!("CHANGELOG.md is missing a `{UNRELEASED_HEADING}` section"))?;

    // Anchor the match to the start of a line: either at file start or after `\n`.
    if unreleased_idx != 0 && content.as_bytes()[unreleased_idx - 1] != b'\n' {
        bail!("`{UNRELEASED_HEADING}` is not at the start of a line");
    }

    // Split: everything up to (and including) the heading line, and everything after.
    let line_end = content[unreleased_idx..]
        .find('\n')
        .map(|n| unreleased_idx + n)
        .unwrap_or(content.len());

    let prefix = &content[..unreleased_idx];
    let suffix = &content[line_end..];

    let new_heading = format!("## [{version}] - {date}");
    let mut out = String::with_capacity(content.len() + 64);
    out.push_str(prefix);
    out.push_str(UNRELEASED_HEADING);
    out.push_str("\n\n");
    out.push_str(&new_heading);
    out.push_str(suffix);
    Ok(out)
}

/// Extract the body of the `## [<version>] - <date>` section.
///
/// Returns the lines between the section's heading and the next `## [` heading
/// (or end of file). The heading itself is not included. Trailing blank lines
/// are stripped so the GitHub Release body doesn't render with extra padding.
pub fn extract_section(content: &str, version: &Version) -> Result<String> {
    let needle = format!("## [{version}]");
    let mut lines = content.lines();
    let mut found = false;
    let mut body = String::new();

    for line in &mut lines {
        if !found {
            if line.starts_with(&needle) {
                found = true;
            }
            continue;
        }
        if line.starts_with("## [") {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }

    if !found {
        bail!("CHANGELOG.md has no section for version {version}");
    }

    // Trim leading and trailing blank lines.
    let trimmed = body.trim_matches('\n');
    Ok(format!("{trimmed}\n"))
}

/// Read the latest released-version heading (i.e. the most recent
/// `## [X.Y.Z] - DATE` section, ignoring `## [Unreleased]`).
pub fn latest_released_version(content: &str) -> Result<Option<Version>> {
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("## [") else {
            continue;
        };
        let Some(end) = rest.find(']') else { continue };
        let tag = &rest[..end];
        if tag == "Unreleased" {
            continue;
        }
        return Ok(Some(Version::parse(tag).with_context(|| {
            format!("changelog heading `## [{tag}]` is not a semver")
        })?));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# Changelog

Some preamble.

## [Unreleased]

### Added
- A thing.
- Another thing.

## [0.1.0] - 2026-04-15

### Added
- The very first release.
";

    #[test]
    fn rename_unreleased_swaps_heading_and_inserts_fresh_section() {
        let v = Version::parse("0.2.0").unwrap();
        let out = rename_unreleased(FIXTURE, &v, "2026-05-04").unwrap();

        // The fresh [Unreleased] is above the renamed section.
        let unreleased = out.find("## [Unreleased]").unwrap();
        let renamed = out.find("## [0.2.0] - 2026-05-04").unwrap();
        assert!(unreleased < renamed);

        // The body that was under [Unreleased] is now under the renamed section.
        assert!(out.contains("## [0.2.0] - 2026-05-04\n\n### Added\n- A thing."));

        // The previous release is untouched.
        assert!(out.contains("## [0.1.0] - 2026-04-15"));
    }

    #[test]
    fn rename_unreleased_errors_when_section_missing() {
        let bad = "# Changelog\n\n(nothing)\n";
        let v = Version::parse("0.2.0").unwrap();
        let err = rename_unreleased(bad, &v, "2026-05-04").unwrap_err();
        assert!(err.to_string().contains("[Unreleased]"));
    }

    #[test]
    fn extract_section_returns_body_only() {
        let v = Version::parse("0.1.0").unwrap();
        let body = extract_section(FIXTURE, &v).unwrap();
        // Heading is excluded; leading/trailing blank lines are trimmed so
        // the GitHub Release body doesn't render with extra padding.
        assert_eq!(body, "### Added\n- The very first release.\n");
    }

    #[test]
    fn extract_section_stops_at_next_heading() {
        let content = "\
## [0.2.0] - 2026-05-04

### Added
- New thing.

## [0.1.0] - 2026-04-15

### Added
- Old thing.
";
        let v = Version::parse("0.2.0").unwrap();
        let body = extract_section(content, &v).unwrap();
        assert!(body.contains("New thing"));
        assert!(!body.contains("Old thing"));
    }

    #[test]
    fn extract_section_errors_when_version_absent() {
        let v = Version::parse("9.9.9").unwrap();
        let err = extract_section(FIXTURE, &v).unwrap_err();
        assert!(err.to_string().contains("9.9.9"));
    }

    #[test]
    fn latest_released_version_skips_unreleased() {
        let v = latest_released_version(FIXTURE).unwrap();
        assert_eq!(v, Some(Version::parse("0.1.0").unwrap()));
    }

    #[test]
    fn latest_released_version_returns_none_when_no_releases() {
        let content = "## [Unreleased]\n\n### Added\n- Stuff\n";
        let v = latest_released_version(content).unwrap();
        assert_eq!(v, None);
    }
}
