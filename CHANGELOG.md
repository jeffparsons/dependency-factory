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
- Best-effort cycle detection across `Singleton` and `Query` resolution,
  within a thread and across threads, with `BuildError` chains that name
  each frame and surface a `CycleError` for downcasting when a cycle is
  detected. Cycles whose edges are all `build` calls are reported; a cycle
  routed through a lock, channel or thread inside `build` is not visible
  to the factory and deadlocks.
- At-most-once building per key: concurrent requests for a key whose build
  is in progress wait for that build instead of running their own, and
  retry if it fails.
- `#[derive(Singleton)]` proc macro with a `#[factory(query = key_fn)]`
  field attribute for opting individual fields into `Query`-keyed resolution.
