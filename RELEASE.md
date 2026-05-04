# Release process

This file is the source of truth for cutting a release. Every step has a
verifiable end-state, so you can stop and resume at any time without losing
track of where you are.

`dependency-factory` and `dependency-factory-derive` are released in
lockstep at the same version.

## Pre-flight

- [ ] CI is green on the latest commit of the branch you'll release from
      (`gh run list --branch <branch> --limit 1`).
- [ ] Working tree clean (`git status`).
- [ ] You're on the right branch:
    - new minor (or any pre-1.0): `main`
    - patch on an older line (post-1.0 only): `release/x.y`

## 1. Pick the version

- Look up the latest published version: `cargo search dependency-factory`.
- Decide the new version per semver. Pre-1.0, breaking changes bump the
  minor; non-breaking changes bump the patch.

## 2. Update version + changelog

- [ ] Bump `[workspace.package].version` in the root `Cargo.toml`.
- [ ] Bump the path-dep version in `dependency-factory/Cargo.toml`
      (`dependency-factory-derive = { version = "=X.Y.Z", ... }`).
- [ ] Edit `CHANGELOG.md`: rename `## [Unreleased]` to
      `## [X.Y.Z] - YYYY-MM-DD`, then open a new empty `## [Unreleased]`
      section above it.
- [ ] `cargo update -w` (refresh `Cargo.lock`).
- [ ] `cargo build --workspace --locked && cargo test --workspace --locked`.
- [ ] `cargo publish -p dependency-factory-derive --dry-run --locked --allow-dirty`.
- [ ] `cargo package --list -p dependency-factory --allow-dirty` — confirms
      the lib's manifest is valid and shows the file list that will be
      uploaded. (A real `cargo publish --dry-run -p dependency-factory`
      cannot run pre-flight because the lib's `=X.Y.Z` pin on the derive
      crate can only be satisfied after the derive is actually published
      to crates.io. The release workflow handles that ordering.)

End-state check: `git diff` shows exactly the version bump in two
`Cargo.toml` files, the changelog rename, and the `Cargo.lock` refresh.

## 3. Commit, tag, push

- [ ] Commit: `Release vX.Y.Z`.
- [ ] Push the branch: `git push`.
- [ ] Wait for CI: `gh run watch`.
- [ ] Tag the release commit: `git tag -a vX.Y.Z -m "Release vX.Y.Z"`.
- [ ] Push the tag: `git push origin vX.Y.Z`.

End-state check: `git ls-remote --tags origin | grep vX.Y.Z` returns the
tag.

## 4. Watch the release workflow

The tag push triggers `.github/workflows/release.yml`, which re-runs CI on
the tagged commit, verifies the tag matches the manifest version,
publishes `dependency-factory-derive` to crates.io, waits for crates.io to
index it, then publishes `dependency-factory`, then creates a GitHub
Release whose body is the changelog section for this version.

- [ ] `gh run watch` until the `release` workflow completes successfully.
- [ ] `cargo search dependency-factory` shows the new version.
- [ ] Check the GitHub Releases page — the new release is present with the
      changelog excerpt as its body.

## 5. Recovery

- **Before tag push (anywhere in step 1–3):** revert the version-bump
  commit. Nothing has been published.
- **After tag push, workflow failed before publish:** fix the issue on the
  branch, force-update the tag (`git tag -fa vX.Y.Z -m "..." HEAD &&
  git push --force origin vX.Y.Z`), and the workflow runs again.
- **After tag push, derive published but lib failed:** the lib publish
  step is re-runnable. Investigate, fix if needed, and re-run the
  workflow from the failed step (`gh run rerun <id> --failed`).
- **After both published:** crates.io is append-only. Bump the patch
  version and ship a fix release.

## Post-1.0: cutting a release branch

Pre-1.0 we always release from `main` and never cut release branches. The
following only applies once we're at 1.0+ and a patch needs to ship on an
older minor.

- [ ] `git switch -c release/x.y vX.Y.0` (branch off the existing tag).
- [ ] `git push -u origin release/x.y`.
- [ ] Cherry-pick the fix(es) from main onto the new branch.
- [ ] Add the changelog entry on the release branch (Keep a Changelog
      format, same as on main).
- [ ] Forward-port the changelog entry to main (own PR or part of the
      fix's PR), so main accumulates all release entries in chronological
      order.
- [ ] Run this checklist from "Pick the version" with the patch version,
      using `release/x.y` as the branch.

## One-time setup

These are recorded here so the requirement is visible, but they should
already be in place before the first release.

- [ ] A `CARGO_REGISTRY_TOKEN` repository secret with publish rights to
      both crates on crates.io. Mint at <https://crates.io/me>; add at
      `Settings → Secrets and variables → Actions → New repository secret`.
- [ ] Both crates exist on crates.io. The very first release publishes
      them; subsequent releases update them.
