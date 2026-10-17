# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Assembly is now a `&mut` phase followed by a `freeze`. `insert`, `build`
  and their `_for` variants take `&mut DependencyFactory`, and `Singleton::
  build` and `Query::build` (including `#[factory(query = ...)]` key
  functions) receive `&mut DependencyFactory` instead of a handle. Each
  key is therefore built at most once, with no internal locking. The new
  `DependencyFactory::freeze` returns a `FrozenFactory`: `Send + Sync`,
  read-only (`get`/`get_for`), and the owner that handles resolve against.
- `DependencyFactoryHandle` is read-only. It can be taken during assembly
  and stored in a resource, but resolves only once the factory is frozen;
  before that its lookups return `BuildError::factory_not_frozen`.

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
