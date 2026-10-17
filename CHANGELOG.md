# Changelog

All notable changes to this project are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Storage is a public `Registry`, a thread-safe type-keyed map with no
  knowledge of building. `DependencyFactory` is one client of its registry,
  reachable via `factory.registry()` or `handle.registry()`, and commits
  what it builds with `get_or_insert_with`, so a value already present
  always wins over a built one. `DependencyFactory::with_registry` wraps a
  registry populated by hand.
- `insert` and `insert_for` move from the factory and handle to `Registry`,
  and no longer overwrite: they return `OccupiedError` with the rejected
  value if the slot is filled. `replace`/`replace_for` overwrite
  explicitly, and `get_or_insert_with`/`get_or_insert_with_for` fill a slot
  only when empty.
- New `Key` trait pairs a key type with its value type. `Query: Key` keeps
  only `build`; `Query::Output` is now `Key::Value`. `get_for` takes the
  key by reference.
- `build` and `build_for` on the factory and handle are renamed
  `get_or_build` and `get_or_build_for`, since they return a cached value
  when there is one. The trait methods `Singleton::build` and
  `Query::build`, which always construct, keep their names.

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
