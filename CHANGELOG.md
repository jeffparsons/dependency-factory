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
- Cycle detection across `Singleton` and `Query` resolution, with
  `BuildError` chains of `Frame`s that name each value type and, for a
  keyed resource, its key, and surface a `CycleError` for downcasting
  when a cycle is detected.
- `#[derive(Singleton)]` proc macro with a `#[factory(query = key_fn)]`
  field attribute for opting individual fields into `Query`-keyed resolution.
- Indexes: `Transaction::index::<T>(arc)` adds an `Arc<T>` (usually a
  trait object) to the registry's index for `T`, and `indexed::<T>()` on
  `Registry`, `Transaction`, `Builder`, `DependencyFactory` and
  `DependencyFactoryHandle` lists it in insertion order. Index changes are
  published with the transaction's batch, and `replace` removes the
  previous value from every index it was in.
- `Singleton::register` and `Query::register` hooks, run once right after
  a built value is cached, for adding it to its indexes or registering it
  with other resources. `#[derive(Singleton)]` generates the hook from a
  struct-level `#[factory(index(dyn Trait, ...))]` attribute.
