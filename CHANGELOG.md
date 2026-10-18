# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `DependencyFactory` and `DependencyFactoryHandle`: type-keyed
  dependency-injection container with `Arc`-shared resources, lazy
  auto-building, and a cheap `Weak`-backed handle for avoiding `Arc` cycles
  that would prevent ever destroying the factory.
- `Singleton` trait: one-instance-per-type resources, recursively buildable.
- `Query` trait: keyed resources where the key value parameterises the build.
- Per-thread cycle detection across `Singleton` and `Query` resolution, with
  `BuildError` chains that name each frame and surface a `CycleError`
  for downcasting when a cycle is detected.
- `#[derive(Singleton)]` proc macro with a `#[factory(query = key_fn)]`
  field attribute for opting individual fields into `Query`-keyed resolution.
