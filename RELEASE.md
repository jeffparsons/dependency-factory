# Release process

Releases are driven by `cargo xtask release`. The tool *is* the source of
truth — see `cargo xtask release --help` for the authoritative list of
subcommands. This document is a brief overview plus recovery guidance for
the rare situations the tool can't help with.

`dependency-factory` and `dependency-factory-derive` are released in
lockstep at the same version.

## Typical flow

1. **`cargo xtask release prepare X.Y.Z`** — bumps the workspace version,
   bumps the lib's path-dep pin, renames `## [Unreleased]` to
   `## [X.Y.Z] - <today>` in `CHANGELOG.md`, refreshes the lockfile,
   builds, tests, and runs pre-flight publish dry-runs. Leaves the
   working tree dirty.
2. **Review and commit.** `git diff`, then commit with subject
   `Release vX.Y.Z`.
3. **Push and wait for CI.** `git push`, then `gh run watch` until green.
   You can also use `cargo xtask release check` to confirm everything is
   in shape (clean tree, on a permitted branch, CI green for HEAD,
   manifest and changelog agree).
4. **Tag.** `git tag -a vX.Y.Z -m "Release vX.Y.Z"` and `git push origin
   vX.Y.Z`.
5. **CI publishes.** The tag push triggers
   `.github/workflows/release.yml`, which calls
   `cargo xtask release verify-tag`, re-runs the CI suite, calls
   `cargo xtask release publish` (derive then lib in order, with a
   crates.io indexing wait between), and creates a GitHub Release whose
   body is `cargo xtask release notes X.Y.Z`.

## Recovery

- **Before tag push** (anywhere in steps 1–3): revert the version-bump
  commit. Nothing has been published.
- **After tag push, workflow failed before publish:** fix the issue on
  the branch, force-update the tag (`git tag -fa vX.Y.Z -m "..." HEAD &&
  git push --force origin vX.Y.Z`), and the workflow runs again.
- **After tag push, derive published but lib failed:** the lib publish
  step is re-runnable. Investigate, fix if needed, and re-run the
  workflow from the failed step (`gh run rerun <id> --failed`).
- **After both published:** crates.io is append-only. Bump the patch
  version and ship a fix release.

## Post-1.0: cutting a release branch

Pre-1.0 we always release from `main` and never cut release branches.
The following only applies once we're at 1.0+ and a patch needs to ship
on an older minor.

- `git switch -c release/x.y vX.Y.0` (branch off the existing tag).
- `git push -u origin release/x.y`.
- Cherry-pick the fix(es) from main onto the new branch.
- Add the changelog entry on the release branch (Keep a Changelog
  format, same as on main).
- Forward-port the changelog entry to main alongside (or instead of, if
  the fix already landed there) the code change, so main accumulates
  all release entries in chronological order.
- Run the typical flow from step 1 with the patch version, on the
  `release/x.y` branch.

## One-time setup

These should already be in place before the first release.

- A `CARGO_REGISTRY_TOKEN` repository secret with publish rights to both
  crates on crates.io. Mint at <https://crates.io/me>; add at
  `Settings → Secrets and variables → Actions → New repository secret`.
- Both crates exist on crates.io. The very first release publishes them;
  subsequent releases update them.
